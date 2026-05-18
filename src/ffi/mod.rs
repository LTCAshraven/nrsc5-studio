use crossbeam_channel::{unbounded, Receiver, Sender};
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use thiserror::Error;

use crate::config::GainMode;
use crate::dsp::{AgcConfig, AgcController, AgcSnapshot};
use crate::sdr::{Sdr, SdrConfig, SdrError, StreamControl};

// -- Events -----------------------------------------------------------

#[derive(Debug, Clone)]
pub enum NrscEvent {
    LostDevice,
    Sync,
    LostSync,
    Mer { lower: f32, upper: f32 },
    Ber { cber: f32 },
    /// Emitted when "Audio bit rate:" first appears, indicating audio is
    /// flowing.  nrsc5.exe plays audio itself via libao so we do not
    /// capture PCM data.
    AudioStarted {
        #[allow(dead_code)] // surfaced for future per-program plumbing
        program: u32,
    },
    Metadata {
        #[allow(dead_code)] // surfaced for future per-program plumbing
        program: u32,
        title: String,
        artist: String,
        album: String,
        genre: String,
    },
    /// LOT file received. `lot` is the LOT ID, `name` is the filename
    /// written to the AAS directory (e.g. "42_cover.jpg").
    LotFile {
        lot: String,
        name: String,
    },
    /// XHDR event — param 0 = cover art, param 1 = station logo.
    Xhdr {
        param: u32,
        lot: String,
    },
    StationName(String),
    /// Per-program short station name, e.g. (1, "The Eagle") for HD1.
    /// `number` is the 1-indexed program (matches the wire format).
    SigServiceAudio {
        number: u32,
        name: String,
    },
    EmergencyAlert,
    HereImage,
    Agc { gain_db: f32 },
    /// Closed-loop AGC controller applied a new tuner gain. Emitted
    /// from the AGC driver thread immediately after the
    /// `Sdr::set_tuner_gain_tenths` call returns. UI uses this to
    /// freshen the "last changed" timestamp on the gain readout.
    AgcDecision {
        tenths: i32,
        reason: String,
    },
}

impl NrscEvent {
    pub fn label(&self) -> &'static str {
        match self {
            Self::LostDevice => "lost-device",
            Self::Sync => "sync",
            Self::LostSync => "lost-sync",
            Self::Mer { .. } => "mer",
            Self::Ber { .. } => "ber",
            Self::AudioStarted { .. } => "audio-started",
            Self::Metadata { .. } => "metadata",
            Self::LotFile { .. } => "lot",
            Self::Xhdr { .. } => "xhdr",
            Self::StationName(_) => "station-name",
            Self::SigServiceAudio { .. } => "sig-service-audio",
            Self::EmergencyAlert => "emergency-alert",
            Self::HereImage => "here-image",
            Self::Agc { .. } => "agc",
            Self::AgcDecision { .. } => "agc-decision",
        }
    }
}

// -- Errors -----------------------------------------------------------

#[derive(Debug, Error)]
pub enum Nrsc5Error {
    #[error("nrsc5.exe not found at any known location")]
    ExeNotFound,
    #[error("failed to spawn nrsc5 process: {0}")]
    Spawn(std::io::Error),
    #[error("SDR backend error: {0}")]
    Sdr(#[from] SdrError),
}

// -- Process Backend --------------------------------------------------

/// Which `start*` path was used last. Remembered so [`Nrsc5Process::retune`]
/// can restart the same backend without the caller having to plumb its
/// mode selection through every retune call site.
#[derive(Debug, Clone)]
enum LastStartMode {
    Usb,
    Piped,
    RtlTcp { host: String, port: u16 },
}

pub struct Nrsc5Process {
    child: Option<Child>,
    stderr_thread: Option<JoinHandle<()>>,
    /// I/Q pump thread for the piped-SDR path. `Some` only while a
    /// piped Start is active; cleared by `stop`. Owns `nrsc5.exe`'s
    /// `ChildStdin` directly so dropping the thread closes the pipe
    /// (sending EOF to nrsc5).
    iq_thread: Option<JoinHandle<()>>,
    /// SDR backend for the active piped stream. `Some` between
    /// `start_piped` and `stop`; `None` otherwise.
    ///
    /// The modern `librtlsdr.dll` (osmocom ≥ 2022-01) handles
    /// `rtlsdr_close` after `rtlsdr_cancel_async` cleanly, so we
    /// open fresh on every Start and close fully on every Stop —
    /// the LED on the dongle goes off, the USB device is released,
    /// and the next Start (or a switch to USB / rtl_tcp mode) gets
    /// a clean handle. Older bundled DLLs crashed on this path; see
    /// the project's Spike 1/2 notes for the historical workaround
    /// that this refactor replaces.
    sdr: Option<Arc<dyn Sdr>>,
    last_mode: Option<LastStartMode>,
    /// Optional FFT tap fed by the piped-SDR I/Q thread. When set, every
    /// USB transfer is also handed to the tap (which throttles its own
    /// work). The Spectrum dock panel reads through a shared clone of
    /// this same handle.
    spectrum_tap: Option<crate::dsp::SpectrumTap>,
    /// Closed-loop AGC controller. `Some` only while a piped Start is
    /// active. Shared between the stderr-parser thread (which feeds it
    /// MER events via `on_event`) and the dedicated AGC driver thread
    /// (which calls `tick` periodically and applies any returned
    /// `AgcAction` via the SDR Arc). The UI reads `snapshot()` for the
    /// gain readout in the Signal panel.
    agc: Option<Arc<Mutex<AgcController>>>,
    /// Driver thread that periodically `tick`s the AGC controller and
    /// applies gain changes mid-stream. Joined in `stop`.
    agc_thread: Option<JoinHandle<()>>,
    /// Stop flag for the AGC driver thread. Set in `stop` before
    /// joining.
    agc_stop: Option<Arc<AtomicBool>>,
    /// Gain mode in effect for the currently-running (or most recent)
    /// piped stream. Preserved across `stop()` so `retune` can reuse it
    /// without the caller having to plumb it back through. `None` until
    /// the first piped Start.
    last_gain_mode: Option<GainMode>,
    /// Manual gain in tenths of dB that was applied (or would be applied
    /// in `GainMode::Manual`) for the current/last piped stream.
    /// Preserved across `stop()` for the same reason as `last_gain_mode`.
    last_manual_gain_tenths: Option<i32>,
    tx: Sender<NrscEvent>,
    rx: Receiver<NrscEvent>,
    exe_path: PathBuf,
    aas_dir: PathBuf,
}

impl Nrsc5Process {
    pub fn new() -> Result<Self, Nrsc5Error> {
        let exe_path = find_nrsc5_exe().ok_or(Nrsc5Error::ExeNotFound)?;
        let (tx, rx) = unbounded();
        let aas_dir = crate::paths::aas_temp_dir();
        let _ = std::fs::create_dir_all(&aas_dir);
        Ok(Self {
            child: None,
            stderr_thread: None,
            iq_thread: None,
            sdr: None,
            last_mode: None,
            spectrum_tap: None,
            agc: None,
            agc_thread: None,
            agc_stop: None,
            last_gain_mode: None,
            last_manual_gain_tenths: None,
            tx,
            rx,
            exe_path,
            aas_dir,
        })
    }

    /// Install a spectrum tap that will be fed raw I/Q bytes alongside
    /// `nrsc5.exe` whenever a piped stream is active. Call once at app
    /// startup; the same tap clone can be retained on the GUI side and
    /// read on every paint.
    pub fn set_spectrum_tap(&mut self, tap: crate::dsp::SpectrumTap) {
        self.spectrum_tap = Some(tap);
    }

    /// Read-only snapshot of the closed-loop AGC controller for the UI.
    /// Returns `None` when no piped stream is active (USB / rtl_tcp
    /// backends don't run our AGC \u2014 nrsc5 owns the dongle there).
    /// Cheap to call every frame.
    pub fn agc_snapshot(&self) -> Option<AgcSnapshot> {
        self.agc.as_ref().and_then(|h| h.lock().ok().map(|g| g.snapshot()))
    }

    /// Gain mode in effect for the currently-running (or most recent)
    /// piped stream. `None` until the first piped Start. The UI uses
    /// this to detect whether the user's desired `config.gain_mode`
    /// differs from what's actually running (in which case a
    /// "restart to apply" hint is shown).
    pub fn active_gain_mode(&self) -> Option<GainMode> {
        self.last_gain_mode
    }

    /// Manual gain (tenths of dB) in effect for the current/last piped
    /// stream. Mirrors `active_gain_mode` and lets the UI detect a
    /// pending change to `manual_gain_tenths` while streaming.
    pub fn active_manual_gain_tenths(&self) -> Option<i32> {
        self.last_manual_gain_tenths
    }

    pub fn events(&self) -> &Receiver<NrscEvent> {
        &self.rx
    }

    pub fn version(&self) -> String {
        format!("nrsc5 process ({})", self.exe_path.display())
    }

    pub fn aas_dir(&self) -> &std::path::Path {
        &self.aas_dir
    }

    /// PID of the running nrsc5 process, or `None` if not running.
    pub fn pid(&self) -> Option<u32> {
        self.child.as_ref().map(|c| c.id())
    }

    /// Start the nrsc5 process.
    ///
    /// `frequency_mhz` -- FM frequency (e.g. 101.1)
    /// `program`        -- 0-indexed HD program number (0 = HD1)
    /// `device_index`   -- RTL-SDR device index (usually 0)
    pub fn start(
        &mut self,
        frequency_mhz: f32,
        program: u32,
        device_index: u32,
    ) -> Result<(), Nrsc5Error> {
        self.stop();
        while self.rx.try_recv().is_ok() {}

        let mut cmd = Command::new(&self.exe_path);
        cmd.arg("-d").arg(device_index.to_string());
        cmd.arg("--dump-aas-files").arg(&self.aas_dir);
        cmd.arg(format!("{:.1}", frequency_mhz));
        cmd.arg(program.to_string());

        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::null());
        cmd.stderr(Stdio::piped());

        #[cfg(target_os = "windows")]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
        }

        let mut child = cmd.spawn().map_err(Nrsc5Error::Spawn)?;
        let stderr = child.stderr.take().expect("stderr was piped");
        let tx = self.tx.clone();
        let stderr_thread = std::thread::spawn(move || {
            parse_stderr(stderr, tx, program, None);
        });

        self.child = Some(child);
        self.stderr_thread = Some(stderr_thread);
        self.last_mode = Some(LastStartMode::Usb);
        Ok(())
    }

    /// Start via rtl_tcp.
    pub fn start_rtltcp(
        &mut self,
        frequency_mhz: f32,
        program: u32,
        host: &str,
        port: u16,
    ) -> Result<(), Nrsc5Error> {
        self.stop();
        while self.rx.try_recv().is_ok() {}

        let mut cmd = Command::new(&self.exe_path);
        cmd.arg("-H").arg(format!("{}:{}", host, port));
        cmd.arg("--dump-aas-files").arg(&self.aas_dir);
        cmd.arg(format!("{:.1}", frequency_mhz));
        cmd.arg(program.to_string());

        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::null());
        cmd.stderr(Stdio::piped());

        #[cfg(target_os = "windows")]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x08000000);
        }

        let mut child = cmd.spawn().map_err(Nrsc5Error::Spawn)?;
        let stderr = child.stderr.take().expect("stderr was piped");
        let tx = self.tx.clone();
        let stderr_thread = std::thread::spawn(move || {
            parse_stderr(stderr, tx, program, None);
        });

        self.child = Some(child);
        self.stderr_thread = Some(stderr_thread);
        self.last_mode = Some(LastStartMode::RtlTcp {
            host: host.to_string(),
            port,
        });
        Ok(())
    }

    /// Start with the SDR driven in-process: open the device, retune,
    /// and spawn `nrsc5.exe -r -` with our I/Q pump feeding its stdin.
    ///
    /// This is the v0.2.0 "piped" path that unblocks the waterfall and
    /// the in-process AGC. Selected by `config.use_piped_sdr` in
    /// `config.toml`. The SDR is opened fresh on each Start and closed
    /// fully on each Stop (the modern librtlsdr.dll handles this
    /// cleanly).
    pub fn start_piped(
        &mut self,
        frequency_mhz: f32,
        program: u32,
        device_index: u32,
        gain_mode: GainMode,
        manual_gain_tenths: i32,
    ) -> Result<(), Nrsc5Error> {
        self.stop();
        while self.rx.try_recv().is_ok() {}

        // Open + configure a fresh SDR for this stream. The initial
        // gain depends on which mode we're operating in:
        //
        //   * `Auto`        — leave gain alone here; the AGC controller
        //                    constructed below will set the starting
        //                    value via its own `initial_action`.
        //   * `Manual`      — force manual gain mode at the user-chosen
        //                    value. Snapping happens inside the SDR.
        //   * `HardwareAgc` — leave gain alone so the R820T2's hardware
        //                    AGC stays in charge (librtlsdr's default).
        let initial_gain_tenths = match gain_mode {
            GainMode::Auto => None,
            GainMode::Manual => Some(manual_gain_tenths),
            GainMode::HardwareAgc => None,
        };
        let rtl = crate::sdr::RtlSdr::open(device_index)?;
        rtl.configure(&SdrConfig {
            center_freq_hz: (frequency_mhz * 1_000_000.0) as u32,
            sample_rate_sps: 1_488_375,
            ppm_correction: 0,
            direct_sampling: 0,
            initial_gain_tenths,
        })?;
        let sdr: Arc<dyn Sdr> = Arc::new(rtl);

        let mut cmd = Command::new(&self.exe_path);
        // -r -  : read raw I/Q from stdin.
        // -l 1  : librtlsdr-style log verbosity.
        //
        // In `-r -` mode nrsc5 v3.1.0 only accepts a SINGLE positional
        // (program); passing both `frequency program` makes it bail to
        // the usage banner. We tune the dongle ourselves via the SDR
        // config above, so the frequency on the CLI is unnecessary.
        let _ = frequency_mhz;
        cmd.arg("-l").arg("1");
        cmd.arg("-r").arg("-");
        cmd.arg("--dump-aas-files").arg(&self.aas_dir);
        cmd.arg(program.to_string());

        cmd.stdin(Stdio::piped());
        cmd.stdout(Stdio::null());
        cmd.stderr(Stdio::piped());

        #[cfg(target_os = "windows")]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
        }

        let mut child = cmd.spawn().map_err(Nrsc5Error::Spawn)?;
        let mut child_stdin = child.stdin.take().expect("stdin was piped");
        let stderr = child.stderr.take().expect("stderr was piped");

        // ----- AGC controller (only in `Auto` mode) -----------------
        // Build the controller, apply its initial gain to the SDR, and
        // wrap in `Arc<Mutex<_>>` so the stderr-parser thread (tee) and
        // the AGC driver thread (tick + apply) can share it. In
        // `Manual` / `HardwareAgc` we leave these `None` and skip the
        // driver thread entirely — the dongle's gain is set once by
        // `configure` above and never touched again for this stream.
        let (agc, agc_stderr_handle) = if gain_mode == GainMode::Auto {
            let agc_ctrl = AgcController::new(
                sdr.gain_table_tenths(),
                AgcConfig::default(),
            );
            let agc = Arc::new(Mutex::new(agc_ctrl));
            let initial = agc
                .lock()
                .expect("AGC mutex poisoned at startup")
                .initial_action();
            let _ = sdr.set_tuner_gain_tenths(initial.new_tenths);
            let _ = self
                .tx
                .send(NrscEvent::AgcDecision {
                    tenths: initial.new_tenths,
                    reason: initial.reason,
                });
            let stderr_handle = Arc::clone(&agc);
            (Some(agc), Some(stderr_handle))
        } else {
            // Surface the chosen mode on the status line so the user
            // can see what's running without checking config.toml.
            let label = match gain_mode {
                GainMode::Manual => format!(
                    "manual gain: {:.1} dB",
                    manual_gain_tenths as f32 / 10.0
                ),
                GainMode::HardwareAgc => "hardware AGC".to_string(),
                GainMode::Auto => unreachable!(),
            };
            let _ = self.tx.send(NrscEvent::AgcDecision {
                tenths: manual_gain_tenths,
                reason: label,
            });
            (None, None)
        };

        let stderr_tx = self.tx.clone();
        let stderr_thread = std::thread::spawn(move || {
            parse_stderr(stderr, stderr_tx, program, agc_stderr_handle);
        });

        // I/Q pump. Owns the SDR (via Arc clone) and `child_stdin`
        // directly, so dropping the thread closes the pipe and sends
        // EOF to nrsc5. The callback returns `Stop` on the first write
        // error (BrokenPipe = nrsc5 exited), which makes `run_stream`
        // return cleanly.
        let sdr_for_thread: Arc<dyn Sdr> = Arc::clone(&sdr);
        let evt_tx = self.tx.clone();
        // Optional FFT tap clone for the Spectrum panel. `None` when the
        // GUI side hasn't installed one (e.g. headless test builds).
        let spectrum_tap = self.spectrum_tap.clone();
        if let Some(tap) = spectrum_tap.as_ref() {
            tap.set_center_freq_hz((frequency_mhz as f64) * 1_000_000.0);
        }
        let iq_thread = std::thread::spawn(move || {
            let mut write_err_seen = false;
            let run_res = sdr_for_thread.run_stream(&mut |bytes| {
                // Spectrum tap first — it's cheap (and internally
                // throttled) and we want the panel to keep updating
                // even if the nrsc5 stdin write below blocks briefly.
                if let Some(tap) = spectrum_tap.as_ref() {
                    tap.feed(bytes);
                }
                if write_err_seen {
                    return StreamControl::Stop;
                }
                match child_stdin.write_all(bytes) {
                    Ok(()) => StreamControl::Continue,
                    Err(_) => {
                        write_err_seen = true;
                        StreamControl::Stop
                    }
                }
            });
            // Drop child_stdin explicitly so nrsc5 gets EOF the moment
            // we leave the stream loop (rather than at thread teardown).
            drop(child_stdin);
            // `run_stream` returns Err on real backend failure (e.g.
            // USB unplugged). A user-initiated Stop trips the cancel
            // flag, which the rtl backend translates to Ok per
            // `stop_flag` discriminator in `src/sdr/rtl.rs`. A clean
            // BrokenPipe-driven exit (write_err_seen) also returns Ok.
            if run_res.is_err() {
                let _ = evt_tx.send(NrscEvent::LostDevice);
            }
        });

        self.child = Some(child);
        self.stderr_thread = Some(stderr_thread);
        self.iq_thread = Some(iq_thread);
        self.sdr = Some(sdr);
        self.last_mode = Some(LastStartMode::Piped);
        self.last_gain_mode = Some(gain_mode);
        self.last_manual_gain_tenths = Some(manual_gain_tenths);

        // ----- AGC driver thread (only when AGC is active) ----------
        // Ticks the controller every ~500 ms and applies any gain
        // change it asks for via the shared SDR Arc. Sends an
        // `AgcDecision` event so the UI's "last changed" timestamp
        // matches the moment of the real FFI call. Skipped entirely in
        // `Manual` / `HardwareAgc` modes — there's no controller to
        // tick and no decisions to apply.
        if let Some(agc) = agc {
            let agc_for_driver = Arc::clone(&agc);
            let sdr_for_agc: Arc<dyn Sdr> =
                Arc::clone(self.sdr.as_ref().expect("sdr just set"));
            let agc_stop = Arc::new(AtomicBool::new(false));
            let agc_stop_for_driver = Arc::clone(&agc_stop);
            let agc_tx = self.tx.clone();
            let agc_thread = std::thread::spawn(move || {
                while !agc_stop_for_driver.load(Ordering::Relaxed) {
                    std::thread::sleep(std::time::Duration::from_millis(500));
                    if agc_stop_for_driver.load(Ordering::Relaxed) {
                        break;
                    }
                    // Lock briefly to extract any pending action;
                    // release the lock BEFORE the FFI call so the
                    // stderr-parser thread can keep feeding events
                    // without contention.
                    let action = match agc_for_driver.lock() {
                        Ok(mut ctrl) => ctrl.tick(),
                        Err(_) => break, // mutex poisoned — give up gracefully
                    };
                    if let Some(action) = action {
                        let _ = sdr_for_agc.set_tuner_gain_tenths(action.new_tenths);
                        let _ = agc_tx.send(NrscEvent::AgcDecision {
                            tenths: action.new_tenths,
                            reason: action.reason,
                        });
                    }
                }
            });

            self.agc = Some(agc);
            self.agc_thread = Some(agc_thread);
            self.agc_stop = Some(agc_stop);
        }
        Ok(())
    }

    /// Stop the active stream (regardless of mode).
    ///
    /// For piped mode this fully releases the SDR — the LED on the
    /// dongle goes off, the USB device is unclaimed, and the next
    /// Start (or a switch to USB / rtl_tcp) starts from scratch. For
    /// the legacy USB and rtl_tcp paths this is a no-op for the SDR
    /// state (nrsc5 owns it directly there).
    pub fn stop(&mut self) {
        // Signal the AGC driver thread to stop first — it borrows the
        // SDR Arc and we want it joined before we drop the SDR below.
        if let Some(flag) = self.agc_stop.as_ref() {
            flag.store(true, Ordering::Relaxed);
        }
        // Cancel the SDR stream first so the I/Q pump exits cleanly
        // and drops its `ChildStdin`, sending EOF to nrsc5. The cancel
        // call is idempotent and safe even when no stream is running.
        if let Some(sdr) = self.sdr.as_ref() {
            let _ = sdr.cancel_stream();
        }
        // Join the AGC driver thread now that its stop flag is set
        // and the stream is being torn down. Holds the SDR Arc clone;
        // must be joined before `self.sdr = None` below.
        if let Some(handle) = self.agc_thread.take() {
            let _ = handle.join();
        }
        // Join the I/Q pump first so its `ChildStdin` is released
        // before we try to wait on the child (avoids a deadlock where
        // nrsc5 is mid-write but stdin is still held open by our thread).
        if let Some(handle) = self.iq_thread.take() {
            let _ = handle.join();
        }
        // Kill the nrsc5 child as a belt-and-suspenders backstop in
        // case it didn't exit on its own from EOF.
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(handle) = self.stderr_thread.take() {
            let _ = handle.join();
        }
        // Drop the SDR last — all Arc clones (the one held by
        // iq_thread is already gone) are released, refcount hits zero,
        // and `RtlSdr::Drop` runs `rtlsdr_close`. Safe on the modern
        // osmocom librtlsdr.dll (≥ 2022-01).
        self.sdr = None;
        // Clear AGC handles last — all references (driver thread,
        // stderr-parser thread tee) are gone by this point.
        self.agc = None;
        self.agc_stop = None;
    }

    /// Retune: stop the active stream and restart in the same mode
    /// with the new frequency / program.
    pub fn retune(
        &mut self,
        frequency_mhz: f32,
        program: u32,
        device_index: u32,
    ) -> Result<(), Nrsc5Error> {
        // Capture the mode before `stop()` clears live state. The
        // `LastStartMode` is preserved across stop() so the caller
        // doesn't have to re-plumb mode selection.
        let mode = self.last_mode.clone();
        self.stop();
        // Small breather so any USB-side state has settled before the
        // next open. Matches the historical 500 ms delay used by the
        // legacy USB retune path.
        std::thread::sleep(std::time::Duration::from_millis(250));
        match mode {
            Some(LastStartMode::Piped) => {
                // Reuse the gain settings from the previous start. New
                // piped streams always come from the GUI which sources
                // these from `AppConfig`; retune doesn't need to know.
                let gain_mode = self.last_gain_mode.unwrap_or_default();
                let manual = self.last_manual_gain_tenths.unwrap_or(197);
                self.start_piped(frequency_mhz, program, device_index, gain_mode, manual)
            }
            Some(LastStartMode::RtlTcp { host, port }) => {
                self.start_rtltcp(frequency_mhz, program, &host, port)
            }
            Some(LastStartMode::Usb) | None => {
                self.start(frequency_mhz, program, device_index)
            }
        }
    }
}

impl Drop for Nrsc5Process {
    fn drop(&mut self) {
        self.stop();
    }
}

// -- Stderr Parser ----------------------------------------------------

fn parse_stderr<R: std::io::Read>(
    stderr: R,
    tx: Sender<NrscEvent>,
    program: u32,
    agc: Option<Arc<Mutex<AgcController>>>,
) {
    let reader = std::io::BufReader::new(stderr);
    let mut got_first_audio_bitrate = false;

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };

        // nrsc5 prefixes each line with "HH:MM:SS " (9 chars).
        let msg = if line.len() > 9 && line.as_bytes()[8] == b' ' {
            &line[9..]
        } else {
            &line
        };

        if let Some(evt) = parse_line(msg, program, &mut got_first_audio_bitrate) {
            // Tee MER/Sync events into the AGC controller (cheap;
            // controller filters internally to the variants it cares
            // about). Done before sending so the controller's state is
            // up-to-date by the time anyone observes the event.
            if let Some(handle) = agc.as_ref() {
                if let Ok(mut ctrl) = handle.lock() {
                    ctrl.on_event(&evt);
                }
            }
            if tx.send(evt).is_err() {
                break;
            }
        }
    }

    // NOTE: Intentionally do NOT emit `LostDevice` here. This loop
    // returns whenever nrsc5's stderr closes, which happens on every
    // clean `stop()` too — emitting LostDevice on every shutdown made
    // legitimate stops look like device-loss events in the GUI. Real
    // device failures still surface via the explicit "Lost device" /
    // "Open device failed." lines parsed inside the loop, and the
    // piped path's I/Q thread emits LostDevice on `run_stream` errors.
}

fn parse_line(msg: &str, program: u32, got_first_audio: &mut bool) -> Option<NrscEvent> {
    if msg == "Synchronized" {
        return Some(NrscEvent::Sync);
    }
    if msg == "Lost synchronization" {
        return Some(NrscEvent::LostSync);
    }
    if msg == "Lost device" || msg == "Open device failed." {
        return Some(NrscEvent::LostDevice);
    }

    // "MER: -5.3 dB (lower), -4.8 dB (upper)"
    if let Some(rest) = msg.strip_prefix("MER: ") {
        return parse_mer(rest);
    }

    // "BER: 0.000000, avg: 0.000000, min: 0.000000, max: 0.000000"
    if let Some(rest) = msg.strip_prefix("BER: ") {
        return parse_ber(rest);
    }

    // "Best gain: 39.6 dB, Peak amplitude: -17.2 dBFS"
    if let Some(rest) = msg.strip_prefix("Best gain: ") {
        return parse_gain(rest);
    }

    if let Some(rest) = msg.strip_prefix("Title: ") {
        return Some(NrscEvent::Metadata {
            program,
            title: rest.to_string(),
            artist: String::new(),
            album: String::new(),
            genre: String::new(),
        });
    }
    if let Some(rest) = msg.strip_prefix("Artist: ") {
        return Some(NrscEvent::Metadata {
            program,
            title: String::new(),
            artist: rest.to_string(),
            album: String::new(),
            genre: String::new(),
        });
    }
    if let Some(rest) = msg.strip_prefix("Album: ") {
        return Some(NrscEvent::Metadata {
            program,
            title: String::new(),
            artist: String::new(),
            album: rest.to_string(),
            genre: String::new(),
        });
    }
    if let Some(rest) = msg.strip_prefix("Genre: ") {
        return Some(NrscEvent::Metadata {
            program,
            title: String::new(),
            artist: String::new(),
            album: String::new(),
            genre: rest.to_string(),
        });
    }

    if msg.starts_with("Audio bit rate:") && !*got_first_audio {
        *got_first_audio = true;
        return Some(NrscEvent::AudioStarted { program });
    }

    // "LOT file: port=1001 lot=42 name=cover.jpg size=12345 mime=BE4B7536 ..."
    if let Some(rest) = msg.strip_prefix("LOT file: ") {
        return parse_lot(rest);
    }

    // "XHDR: 0 BE4B7536 42"
    if let Some(rest) = msg.strip_prefix("XHDR: ") {
        return parse_xhdr(rest);
    }

    // "Station name: KROQ-FM"
    if let Some(rest) = msg.strip_prefix("Station name: ") {
        return Some(NrscEvent::StationName(rest.to_string()));
    }

    // "SIG Service: type=audio number=2 name=The EDGE"
    if let Some(rest) = msg.strip_prefix("SIG Service: type=audio number=") {
        return parse_sig_service_audio(rest);
    }

    if msg.starts_with("Alert:") {
        return Some(NrscEvent::EmergencyAlert);
    }

    if msg.starts_with("HERE Image:") {
        return Some(NrscEvent::HereImage);
    }

    None
}

fn parse_mer(rest: &str) -> Option<NrscEvent> {
    // Input: "-5.3 dB (lower), -4.8 dB (upper)"
    // Split on ", " to get ["MER: -5.3 dB (lower)", "-4.8 dB (upper)"]
    let (lower_part, upper_part) = rest.split_once("), ")?;
    // lower_part = "-5.3 dB (lower"  → take first token
    let lower = lower_part.split_whitespace().next()?.parse::<f32>().ok()?;
    // upper_part = "-4.8 dB (upper)" → take first token
    let upper = upper_part.split_whitespace().next()?.parse::<f32>().ok()?;
    Some(NrscEvent::Mer { lower, upper })
}

fn parse_ber(rest: &str) -> Option<NrscEvent> {
    let cber = rest.split(',').next()?.trim().parse::<f32>().ok()?;
    Some(NrscEvent::Ber { cber })
}

fn parse_gain(rest: &str) -> Option<NrscEvent> {
    let gain_str = rest.split_whitespace().next()?;
    let gain_db = gain_str.parse::<f32>().ok()?;
    Some(NrscEvent::Agc { gain_db })
}

fn parse_lot(rest: &str) -> Option<NrscEvent> {
    // "port=0802 lot=16502 name=KDGE HD2HD024076.jpg size=10115 mime=1E653E9C"
    // name= value may contain spaces, so we extract it between "name=" and " size=".
    let lot_start = rest.find("lot=")?;
    let lot_rest = &rest[lot_start + 4..];
    let lot = lot_rest.split_whitespace().next()?.to_string();

    let name_start = rest.find("name=")?;
    let name_rest = &rest[name_start + 5..];
    let name_end = name_rest.find(" size=")?;
    let name = name_rest[..name_end].to_string();

    // nrsc5 writes the file as "{lot}_{name}" in the aas directory.
    let filename = format!("{}_{}", lot, name);
    Some(NrscEvent::LotFile { lot, name: filename })
}

fn parse_sig_service_audio(rest: &str) -> Option<NrscEvent> {
    // rest = "2 name=The EDGE"
    let (num_part, name_part) = rest.split_once(" name=")?;
    let number = num_part.parse::<u32>().ok()?;
    let name = name_part.trim().to_string();
    if name.is_empty() {
        return None;
    }
    Some(NrscEvent::SigServiceAudio { number, name })
}

fn parse_xhdr(rest: &str) -> Option<NrscEvent> {
    // "0 BE4B7536 42"
    let mut parts = rest.split_whitespace();
    let param = parts.next()?.parse::<u32>().ok()?;
    let _mime = parts.next()?; // skip mime hash
    let lot = parts.next()?.to_string();
    Some(NrscEvent::Xhdr { param, lot })
}

// -- Exe discovery ----------------------------------------------------

fn find_nrsc5_exe() -> Option<PathBuf> {
    let exe_name = if cfg!(target_os = "windows") {
        "nrsc5.exe"
    } else {
        "nrsc5"
    };

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join("bin").join(exe_name);
            if candidate.exists() {
                return Some(candidate);
            }
            let candidate = dir.join(exe_name);
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }

    if let Ok(cwd) = std::env::current_dir() {
        let candidate = cwd.join("bin").join(exe_name);
        if candidate.exists() {
            return Some(candidate);
        }
        let candidate = cwd.join(exe_name);
        if candidate.exists() {
            return Some(candidate);
        }
    }

    None
}