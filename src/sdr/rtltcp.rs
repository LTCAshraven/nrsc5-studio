//! Native rtl_tcp backend (`v0.5.0`).
//!
//! Speaks the rtl_tcp wire protocol directly so the in-process piped
//! pipeline can consume IQ from a remote `rtl_tcp` server without
//! routing through SoapySDR (which would require SoapyRemote +
//! SoapyRTLSDR running on the remote machine — a different server).
//!
//! ### Wire protocol summary
//!
//! On connect, the server sends a 12-byte **dongle info** header:
//!
//! ```text
//! offset 0..4   = ASCII "RTL0" magic
//! offset 4..8   = u32 BE tuner type (1=E4000, 5=R820T, ...)
//! offset 8..12  = u32 BE tuner gain count
//! ```
//!
//! After the header, the server streams unsigned-8 IQ pairs (CU8) at
//! whatever sample rate the client last requested. Client commands are
//! 5-byte big-endian frames:
//!
//! ```text
//! [opcode u8][param u32 BE]
//! ```
//!
//! Opcodes we use:
//!
//! | opcode | name                      | param                                 |
//! |--------|---------------------------|----------------------------------------|
//! | 0x01   | set center freq           | hz                                     |
//! | 0x02   | set sample rate           | sps                                    |
//! | 0x03   | set tuner gain mode       | 0 = auto, 1 = manual                   |
//! | 0x04   | set tuner gain (tenths)   | tenths of dB (e.g. 197 = 19.7 dB)      |
//! | 0x05   | set frequency correction  | ppm                                    |
//! | 0x08   | set AGC mode              | 0 = off, 1 = on (RTL2832 demod AGC)   |
//! | 0x0d   | set tuner gain by index   | index into the tuner's gain table      |
//!
//! The `RtlTcpSdr` impl wires those calls into the [`Sdr`](super::Sdr)
//! trait so the rest of the app doesn't care which backend is feeding it.
//!
//! ### Threading model
//!
//! * One `TcpStream` is shared between the worker thread (calling
//!   [`Sdr::run_stream`]) and any control thread that issues
//!   `set_center_freq_hz` / `set_tuner_gain_tenths` etc.
//! * The read half is held by the worker; the write half is wrapped
//!   in a `Mutex` so concurrent control writes serialize cleanly.
//! * `cancel_stream` flips an `AtomicBool` and shuts down the read
//!   side of the socket so a blocked `read` returns immediately.

use std::io::{Read, Write};
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use super::{GainElement, Sdr, SdrConfig, SdrError, StreamControl};

/// Tenths-of-dB gain table for the R820T(2) tuner — the only tuner
/// users connect with against an rtl_tcp server in practice. Matches
/// the values librtlsdr exposes via `rtlsdr_get_tuner_gains()` and the
/// table baked into [`super::profile::R820T_GAINS_TENTHS`].
///
/// rtl_tcp itself doesn't ship the table to the client; the server
/// uses it implicitly when the client picks a gain via opcode `0x0d`
/// ("set gain by index"). We send the absolute tenths via opcode
/// `0x04` instead and let the server snap to its own table, but we
/// still surface this same table to [`Sdr::gain_table_tenths`] so the
/// closed-loop AGC has something realistic to walk.
const R820T_GAIN_TABLE_TENTHS: &[i32] = super::profile::R820T_GAINS_TENTHS;

/// Default socket timeout for reads. Long enough that we never trip on
/// a transient pause in the IQ stream, short enough that a dead server
/// or a `cancel_stream` is observable within a reasonable window.
const READ_TIMEOUT: Duration = Duration::from_secs(5);

/// Connection timeout for the initial TCP handshake. Kept short so a
/// misconfigured host doesn't freeze the GUI thread (where `open` is
/// called synchronously from the Start button).
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

/// Read timeout for the 12-byte dongle-info header. A real rtl_tcp
/// server sends it within the first packet; if 2 s elapse with no
/// bytes, the peer is almost certainly not an rtl_tcp server (e.g.
/// user typed the wrong port and hit an unrelated service like ssh).
/// Bounding this prevents the indefinite hang that previously made
/// the app appear frozen until Linux's Force Quit dialog appeared.
const HANDSHAKE_READ_TIMEOUT: Duration = Duration::from_secs(2);

/// One open rtl_tcp connection.
pub struct RtlTcpSdr {
    /// `host:port` we connected to — used in error messages.
    addr: String,
    /// Read side of the TCP socket. Owned exclusively by the
    /// `run_stream` worker; control threads never touch it.
    /// Wrapped in `Mutex<Option<_>>` so `run_stream` can `take()` it
    /// once and `cancel_stream` can `shutdown()` the underlying socket
    /// through the still-cloned handle inside `tx`.
    rx: Mutex<Option<TcpStream>>,
    /// Write side of the TCP socket (a clone of the same underlying
    /// socket). All control commands go through this mutex so
    /// concurrent writes don't interleave 5-byte frames.
    tx: Mutex<TcpStream>,
    /// Set by `cancel_stream`; read by the `run_stream` loop on each
    /// iteration so a blocked read can be unblocked via a socket
    /// `shutdown` and the loop will then exit cleanly.
    stop_flag: AtomicBool,
    /// Snapshot of the dongle-info header read at connect time.
    /// Surfaced via `gain_table_tenths` (synthesized for R820T) and
    /// available for future diagnostics; tuner_type currently informs
    /// only the gain-table choice.
    dongle: DongleInfo,
}

/// 12-byte dongle-info header sent once by `rtl_tcp` on connect.
#[derive(Debug, Clone, Copy)]
pub struct DongleInfo {
    /// Tuner type as reported by `rtlsdr_get_tuner_type()` on the
    /// server side. `5` = R820T / R820T2, which is what every
    /// commodity RTL-SDR Blog dongle ships with.
    pub tuner_type: u32,
    /// Number of discrete gain steps the server's tuner exposes.
    pub gain_count: u32,
}

impl DongleInfo {
    /// Parse the 12-byte dongle-info header.
    ///
    /// Returns [`SdrError::RtlTcpBadMagic`] if the first 4 bytes
    /// aren't ASCII `RTL0` (catches the case where the user pointed
    /// us at the wrong service — e.g. a SoapySDRServer port — and
    /// we'd otherwise interpret arbitrary bytes as a tuner type).
    pub fn parse(bytes: &[u8; 12], addr: &str) -> Result<Self, SdrError> {
        let magic: [u8; 4] = [bytes[0], bytes[1], bytes[2], bytes[3]];
        if &magic != b"RTL0" {
            return Err(SdrError::RtlTcpBadMagic {
                addr: addr.to_string(),
                got: magic,
            });
        }
        let tuner_type = u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
        let gain_count = u32::from_be_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
        Ok(Self {
            tuner_type,
            gain_count,
        })
    }
}

/// Opcodes used in the 5-byte command frames we send to the server.
mod op {
    pub const SET_FREQ: u8 = 0x01;
    pub const SET_SAMPLE_RATE: u8 = 0x02;
    pub const SET_GAIN_MODE: u8 = 0x03;
    pub const SET_TUNER_GAIN: u8 = 0x04;
    pub const SET_FREQ_CORRECTION: u8 = 0x05;
    pub const SET_AGC_MODE: u8 = 0x08;
}

/// Encode one rtl_tcp control frame: `[opcode][param BE]`.
fn encode_cmd(opcode: u8, param: u32) -> [u8; 5] {
    let p = param.to_be_bytes();
    [opcode, p[0], p[1], p[2], p[3]]
}

impl RtlTcpSdr {
    /// Connect to `host:port`, read the dongle-info header, and return
    /// a ready-to-`configure` SDR.
    ///
    /// Resolves `host` via DNS so users can enter hostnames
    /// (`localhost`, `mybox.local`, IPv6 literals) as well as raw
    /// IPv4 addresses. Sets bounded timeouts for both the TCP
    /// handshake and the dongle-info header read so a missing or
    /// non-rtl_tcp peer fails fast instead of hanging the caller.
    /// After the handshake, the longer steady-state `READ_TIMEOUT`
    /// is installed so the `run_stream` loop can poll its stop_flag
    /// if the server pauses.
    pub fn open(host: &str, port: u16) -> Result<Self, SdrError> {
        let addr = format!("{host}:{port}");

        // Resolve via DNS so hostnames work. `to_socket_addrs` may
        // return multiple addrs (IPv4 + IPv6); try each in order and
        // return the first successful connect. If all fail, surface
        // the last error.
        let sock_addrs: Vec<_> = addr
            .to_socket_addrs()
            .map_err(|e| SdrError::RtlTcpConnect {
                addr: addr.clone(),
                reason: format!("resolving address: {e}"),
            })?
            .collect();
        if sock_addrs.is_empty() {
            return Err(SdrError::RtlTcpConnect {
                addr: addr.clone(),
                reason: "address resolved to no socket addresses".to_string(),
            });
        }

        let mut last_err: Option<std::io::Error> = None;
        let mut stream: Option<TcpStream> = None;
        for sa in &sock_addrs {
            match TcpStream::connect_timeout(sa, CONNECT_TIMEOUT) {
                Ok(s) => {
                    stream = Some(s);
                    break;
                }
                Err(e) => last_err = Some(e),
            }
        }
        let mut stream = stream.ok_or_else(|| SdrError::RtlTcpConnect {
            addr: addr.clone(),
            reason: last_err
                .map(|e| e.to_string())
                .unwrap_or_else(|| "unknown connect error".to_string()),
        })?;

        // Install the short handshake read timeout BEFORE reading the
        // header so a peer that accepts the connection but never
        // sends bytes (wrong service, half-broken rtl_tcp, etc.)
        // can't hang the GUI thread.
        stream
            .set_read_timeout(Some(HANDSHAKE_READ_TIMEOUT))
            .map_err(|e| SdrError::RtlTcpIo {
                addr: addr.clone(),
                reason: format!("set_read_timeout (handshake): {e}"),
            })?;

        // Read the 12-byte dongle-info header. `read_exact` will
        // surface a timeout as `WouldBlock` / `TimedOut` if the peer
        // is silent; we map that to the same RtlTcpIo variant so the
        // user sees a clear failure instead of an indefinite freeze.
        let mut header = [0u8; 12];
        stream
            .read_exact(&mut header)
            .map_err(|e| SdrError::RtlTcpIo {
                addr: addr.clone(),
                reason: format!("reading dongle-info header: {e}"),
            })?;
        let dongle = DongleInfo::parse(&header, &addr)?;

        // Switch to the steady-state read timeout for the rest of the
        // session. Long enough that transient IQ-stream pauses don't
        // trip it; short enough that cancel_stream is observable.
        stream
            .set_read_timeout(Some(READ_TIMEOUT))
            .map_err(|e| SdrError::RtlTcpIo {
                addr: addr.clone(),
                reason: format!("set_read_timeout: {e}"),
            })?;

        // Clone the handle so the worker (read) and any control thread
        // (write) can both hold one. `try_clone` shares the underlying
        // OS socket so `shutdown()` on either clone unblocks reads on
        // the other.
        let tx_clone = stream
            .try_clone()
            .map_err(|e| SdrError::RtlTcpIo {
                addr: addr.clone(),
                reason: format!("try_clone for control half: {e}"),
            })?;

        Ok(Self {
            addr,
            rx: Mutex::new(Some(stream)),
            tx: Mutex::new(tx_clone),
            stop_flag: AtomicBool::new(false),
            dongle,
        })
    }

    /// Best-effort: send one 5-byte command frame.
    fn send_cmd(&self, opcode: u8, param: u32) -> Result<(), SdrError> {
        let frame = encode_cmd(opcode, param);
        let mut tx = self.tx.lock().map_err(|_| SdrError::RtlTcpIo {
            addr: self.addr.clone(),
            reason: "tx mutex poisoned".to_string(),
        })?;
        tx.write_all(&frame).map_err(|e| SdrError::RtlTcpIo {
            addr: self.addr.clone(),
            reason: format!("send_cmd(op=0x{opcode:02x}): {e}"),
        })?;
        tx.flush().ok();
        Ok(())
    }

    /// Server address (`host:port`) this connection was opened against.
    /// Exposed so the FFI layer can log which remote a session targets.
    pub fn addr(&self) -> &str {
        &self.addr
    }

    /// Cached dongle-info header read at connect time.
    pub fn dongle_info(&self) -> DongleInfo {
        self.dongle
    }
}

impl Sdr for RtlTcpSdr {
    fn configure(&self, cfg: &SdrConfig) -> Result<(), SdrError> {
        // Order matches what the official `rtl_tcp` clients do:
        // sample rate first, then frequency, then optional PPM, then
        // gain. Each of the writes returns the underlying I/O error
        // wrapped in `RtlTcpIo` if the socket has gone away.
        self.send_cmd(op::SET_SAMPLE_RATE, cfg.sample_rate_sps)?;
        self.send_cmd(op::SET_FREQ, cfg.center_freq_hz)?;
        if cfg.ppm_correction != 0 {
            // The PPM opcode takes an unsigned u32 on the wire; rtl_tcp
            // reinterprets it as a signed int on its side. Cast through
            // i32 so negative ppm values round-trip correctly via two's
            // complement.
            self.send_cmd(op::SET_FREQ_CORRECTION, cfg.ppm_correction as u32)?;
        }
        // Disable the demod's hardware AGC unconditionally — same
        // policy as the Soapy backend. The closed-loop AGC needs a
        // stable manual gain to walk against.
        self.send_cmd(op::SET_AGC_MODE, 0)?;
        if let Some(tenths) = cfg.initial_gain_tenths {
            // Manual gain mode + absolute tenths. The server snaps to
            // its own gain table internally.
            self.send_cmd(op::SET_GAIN_MODE, 1)?;
            self.send_cmd(op::SET_TUNER_GAIN, tenths as u32)?;
        }
        // Antenna selection is meaningless on RTL-SDR (single input);
        // we accept and ignore the `cfg.antenna` field for parity with
        // the Soapy backend.
        Ok(())
    }

    fn gain_table_tenths(&self) -> &[i32] {
        // R820T(2) is the only tuner anyone runs rtl_tcp against in
        // practice. If we ever see another tuner type (E4000 etc.) we
        // can extend this match.
        R820T_GAIN_TABLE_TENTHS
    }

    fn set_tuner_gain_tenths(&self, tenths: i32) -> Result<(), SdrError> {
        // Make sure the server is in manual mode before pushing a
        // value; harmless on re-issue.
        self.send_cmd(op::SET_GAIN_MODE, 1)?;
        self.send_cmd(op::SET_TUNER_GAIN, tenths as u32)
    }

    fn run_stream(
        &self,
        cb: &mut dyn FnMut(&[u8]) -> StreamControl,
    ) -> Result<(), SdrError> {
        let mut rx = match self.rx.lock() {
            Ok(mut guard) => guard.take().ok_or_else(|| SdrError::RtlTcpIo {
                addr: self.addr.clone(),
                reason: "stream already consumed (rx half taken)".to_string(),
            })?,
            Err(_) => {
                return Err(SdrError::RtlTcpIo {
                    addr: self.addr.clone(),
                    reason: "rx mutex poisoned".to_string(),
                })
            }
        };
        self.stop_flag.store(false, Ordering::Release);

        // 16 KiB matches the chunk size the Soapy CU8 path produces —
        // keeps downstream consumers (spectrum tap, nrsc5 stdin pump)
        // seeing similarly-sized payloads regardless of transport.
        let mut buf = vec![0u8; 16 * 1024];
        let mut result: Result<(), SdrError> = Ok(());

        while !self.stop_flag.load(Ordering::Acquire) {
            match rx.read(&mut buf) {
                Ok(0) => {
                    // Clean EOF — server closed the connection.
                    result = Err(SdrError::RtlTcpIo {
                        addr: self.addr.clone(),
                        reason: "remote closed the connection".to_string(),
                    });
                    break;
                }
                Ok(n) => {
                    // rtl_tcp emits CU8 natively, which is exactly the
                    // wire format nrsc5 expects on stdin — no
                    // conversion required.
                    if cb(&buf[..n]) == StreamControl::Stop {
                        break;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
                {
                    // Read timeout fired (5 s). Loop back and check
                    // the stop flag so a `cancel_stream` becomes
                    // visible even when the server has paused.
                    continue;
                }
                Err(e) => {
                    if self.stop_flag.load(Ordering::Acquire) {
                        // Socket was shut down by `cancel_stream` —
                        // exit cleanly rather than surfacing the
                        // "connection aborted" error to the caller.
                        break;
                    }
                    result = Err(SdrError::RtlTcpIo {
                        addr: self.addr.clone(),
                        reason: format!("read: {e}"),
                    });
                    break;
                }
            }
        }

        // Best-effort socket shutdown so the tx clone also sees EOF
        // on its next write — avoids dangling control commands queued
        // up after the stream stops.
        let _ = rx.shutdown(Shutdown::Both);
        result
    }

    fn cancel_stream(&self) -> Result<(), SdrError> {
        self.stop_flag.store(true, Ordering::Release);
        // Shut down the *write* clone — the OS socket is shared, so
        // any in-flight `read` on the rx half returns immediately.
        if let Ok(tx) = self.tx.lock() {
            let _ = tx.shutdown(Shutdown::Both);
        }
        Ok(())
    }

    fn set_center_freq_hz(&self, hz: u32) -> Result<(), SdrError> {
        self.send_cmd(op::SET_FREQ, hz)
    }

    fn gain_elements(&self) -> Vec<GainElement> {
        // Surface a single synthetic `TUNER` element so the AGC
        // adapter and the SDR Settings modal both have something to
        // bind to. Range and step match the R820T(2) table; the
        // current value isn't observable on rtl_tcp, so we report
        // the midpoint as a placeholder.
        let table = self.gain_table_tenths();
        let min_db = table.first().copied().unwrap_or(0) as f64 / 10.0;
        let max_db = table.last().copied().unwrap_or(496) as f64 / 10.0;
        vec![GainElement {
            name: "TUNER".to_string(),
            min_db,
            max_db,
            step_db: 0.1,
            current_db: (min_db + max_db) / 2.0,
        }]
    }

    fn set_gain_element(&self, name: &str, value_db: f64) -> Result<(), SdrError> {
        // Only the synthetic TUNER element is real on rtl_tcp; ignore
        // everything else so the AGC adapter's profile-driven element
        // names (`IFGR`, `LNA`, etc. for other backends) silently
        // no-op when applied to an rtl_tcp connection.
        if !name.eq_ignore_ascii_case("TUNER") {
            return Ok(());
        }
        let tenths = (value_db * 10.0).round() as i32;
        self.set_tuner_gain_tenths(tenths)
    }

    fn set_frequency_correction_ppm(&self, ppm: f64) -> Result<(), SdrError> {
        // PPM is only meaningful as an integer to rtl_tcp; round here
        // so the float-domain control surface in the GUI doesn't keep
        // re-sending the same value as noise nudges it.
        let value = ppm.round() as i32;
        self.send_cmd(op::SET_FREQ_CORRECTION, value as u32)
    }

    fn driver(&self) -> &str {
        // Route through the existing RTL-SDR AGC profile, gain table,
        // and per-frequency cache lookups. From the audio/AGC pipeline's
        // point of view this connection behaves identically to a local
        // RTL-SDR dongle, which is the whole point of the protocol.
        "rtlsdr"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_set_freq_be() {
        let frame = encode_cmd(op::SET_FREQ, 97_100_000);
        let expected = 97_100_000u32.to_be_bytes();
        assert_eq!(frame[0], op::SET_FREQ);
        assert_eq!(&frame[1..], &expected);
    }

    #[test]
    fn encodes_set_sample_rate_be() {
        let frame = encode_cmd(op::SET_SAMPLE_RATE, 1_488_375);
        let expected = 1_488_375u32.to_be_bytes();
        assert_eq!(frame[0], op::SET_SAMPLE_RATE);
        assert_eq!(&frame[1..], &expected);
    }

    #[test]
    fn encodes_set_tuner_gain_in_tenths() {
        // 19.7 dB = 197 tenths = 0x00_00_00_C5
        let frame = encode_cmd(op::SET_TUNER_GAIN, 197);
        assert_eq!(frame, [0x04, 0x00, 0x00, 0x00, 0xC5]);
    }

    #[test]
    fn parses_valid_dongle_info() {
        // RTL0, tuner type = 5 (R820T), gain count = 29
        let bytes = [
            b'R', b'T', b'L', b'0',
            0x00, 0x00, 0x00, 0x05,
            0x00, 0x00, 0x00, 0x1D,
        ];
        let info = DongleInfo::parse(&bytes, "127.0.0.1:1234").expect("valid header");
        assert_eq!(info.tuner_type, 5);
        assert_eq!(info.gain_count, 29);
    }

    #[test]
    fn rejects_bad_magic() {
        // First 4 bytes are garbage — typical of pointing at the wrong
        // service (e.g. a SoapySDRServer port that immediately closes).
        let bytes = [
            0xDE, 0xAD, 0xBE, 0xEF,
            0x00, 0x00, 0x00, 0x05,
            0x00, 0x00, 0x00, 0x1D,
        ];
        let err = DongleInfo::parse(&bytes, "127.0.0.1:1234").expect_err("bad magic");
        match err {
            SdrError::RtlTcpBadMagic { got, .. } => {
                assert_eq!(got, [0xDE, 0xAD, 0xBE, 0xEF]);
            }
            other => panic!("expected RtlTcpBadMagic, got {other:?}"),
        }
    }
}
