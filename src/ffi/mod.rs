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
use crate::sdr::profile::DeviceProfile;
use crate::sdr::{IqBus, Sdr, SdrConfig, SdrError, StreamControl};

mod decoder;
use decoder::DecoderInstance;

// -- Events -----------------------------------------------------------

#[derive(Debug, Clone)]
pub enum NrscEvent {
    LostDevice,
    /// Backend stream failed; carries the underlying Soapy error text
    /// for diagnostics/UI status.
    LostDeviceDetail(String),
    /// The nrsc5.exe child process closed its stdout pipe. Emitted from
    /// the PCM pump on EOF / BrokenPipe — covers external `taskkill`,
    /// child crash, child clean exit, or our own `stop()` path. Handled
    /// idempotently in the app: if we still think we're streaming, tear
    /// down; if Stop already ran, ignore. Lets us detect a dead child
    /// without polling `child.try_wait()` on every GUI frame.
    ChildExited,
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
    /// Per-program audio bit rate from `Audio bit rate: 96.0 kbps …`.
    /// Emitted on every occurrence (not just the first —
    /// `AudioStarted` carries the one-shot "audio is alive"
    /// signal). `program` is 0-indexed and matches the program
    /// nrsc5 was launched with, so it always corresponds to the
    /// currently-decoded subchannel.
    AudioBitRate {
        program: u32,
        kbps: f32,
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
    /// `program` is the HD subchannel whose decoder produced this
    /// event — stamped by `parse_stderr` from the per-child context.
    /// Used by the multi-decoder routing layer to attribute album art
    /// and station logo updates to the correct `programs[]` slot.
    LotFile {
        program: u32,
        lot: String,
        name: String,
    },
    /// XHDR event — param 0 = cover art, param 1 = station logo.
    /// `program` is the HD subchannel whose decoder produced this
    /// event; same routing role as on `LotFile`.
    Xhdr {
        program: u32,
        param: u32,
        lot: String,
    },
    StationName(String),
    /// Long-form station identifier from `Slogan: …`. Sent by SIS
    /// every few seconds while synced; receivers display it alongside
    /// the call sign.
    Slogan(String),
    /// Free-text broadcaster message from `Message: …`. Used for
    /// promos, "now playing on HD2", etc. — distinct from `Alert:`.
    Message(String),
    /// Transmitter location from `Location: <lat>, <lon>, <alt> m`.
    /// `altitude_m` is height above mean sea level.
    Location {
        latitude: f64,
        longitude: f64,
        altitude_m: i32,
    },
    /// Country code + FCC facility ID from
    /// `Country code: US, FCC facility ID: 12345`.
    CountryFcc {
        country: String,
        facility_id: u32,
    },
    /// Per-program descriptor from
    /// `Audio program N: <MPS|SPSx>, type: <Music|Talk|…>, sound experience: <Mono|Stereo|…>`.
    /// `number` is 1-indexed to match the wire format (HD1..HD8).
    AudioProgram {
        number: u32,
        program_type: String,
        sound_experience: String,
    },
    /// Per-program short station name, e.g. (1, "The Eagle") for HD1.
    /// `number` is the 1-indexed program (matches the wire format).
    SigServiceAudio {
        number: u32,
        name: String,
    },
    /// Non-audio data service from `SIG Service: type=data number=N name=…`.
    /// Inner `Component: …` lines (mime, service_data_type) are not yet
    /// captured — added when the panel needs them.
    SigServiceData {
        number: u32,
        name: String,
    },
    /// Emergency alert text from `Alert: …`. Empty alerts are dropped.
    EmergencyAlert {
        text: String,
    },
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
            Self::LostDeviceDetail(_) => "lost-device-detail",
            Self::ChildExited => "child-exited",
            Self::Sync => "sync",
            Self::LostSync => "lost-sync",
            Self::Mer { .. } => "mer",
            Self::Ber { .. } => "ber",
            Self::AudioStarted { .. } => "audio-started",
            Self::AudioBitRate { .. } => "audio-bitrate",
            Self::Metadata { .. } => "metadata",
            Self::LotFile { .. } => "lot",
            Self::Xhdr { .. } => "xhdr",
            Self::StationName(_) => "station-name",
            Self::Slogan(_) => "slogan",
            Self::Message(_) => "message",
            Self::Location { .. } => "location",
            Self::CountryFcc { .. } => "country-fcc",
            Self::AudioProgram { .. } => "audio-program",
            Self::SigServiceAudio { .. } => "sig-service-audio",
            Self::SigServiceData { .. } => "sig-service-data",
            Self::EmergencyAlert { .. } => "emergency-alert",
            Self::HereImage => "here-image",
            Self::Agc { .. } => "agc",
            Self::AgcDecision { .. } => "agc-decision",
        }
    }
}

// -- Errors -----------------------------------------------------------

/// Hard cap on the number of concurrently-running decoders against
/// one shared SDR pipeline. Each decoder is a full nrsc5.exe child
/// process plus three pump threads, so the cost scales linearly;
/// eight covers every HD Radio station's advertised program count
/// (HD1–HD8) with a margin of safety. Default streaming behavior is
/// single-decoder; the user opts in to extras via the per-program
/// decode toggle in the HD grid.
pub const MAX_DECODERS: usize = 8;

#[derive(Debug, Error)]
pub enum Nrsc5Error {
    #[error("nrsc5.exe not found at any known location")]
    ExeNotFound,
    #[error("failed to spawn nrsc5 process: {0}")]
    Spawn(std::io::Error),
    #[error("SDR backend error: {0}")]
    Sdr(#[from] SdrError),
    /// `add_decoder` / `set_active_speaker` called before any
    /// `start_piped` succeeded. The shared SDR + IqBus pipeline
    /// must be running before per-program decoders can be added.
    #[error("no piped session is active (call start_piped first)")]
    NotStarted,
    /// `add_decoder(program)` called for a program that's already
    /// being decoded. Idempotent failure — nothing was changed.
    #[error("program {0} is already being decoded")]
    DecoderAlreadyActive(u32),
    /// `add_decoder` called when [`MAX_DECODERS`] are already
    /// running. Tear one down before adding another.
    #[error("decoder cap reached ({0} of {1})")]
    DecoderCapReached(usize, usize),
    /// `set_active_speaker(program)` referenced a program that
    /// isn't being decoded. The speaker selection is unchanged.
    #[error("no decoder is running for program {0}")]
    NoSuchDecoder(u32),
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
    /// Active decoders for the current piped session (and the single
    /// entry for the legacy USB / rtl_tcp paths). Empty between
    /// `stop()` and the next `start*` call. Phase 3 Chunk 3 turned
    /// this into a `Vec` so multiple HD programs can be decoded in
    /// parallel against the same SDR pipeline; the public
    /// `add_decoder` / `remove_decoder` / `set_active_speaker` API
    /// arrived in the same chunk. Capped at [`MAX_DECODERS`].
    decoders: Vec<DecoderInstance>,
    /// I/Q **source** pump thread for the piped-SDR path. `Some`
    /// only while a piped Start is active; cleared by `stop`. Runs
    /// `sdr.run_stream`, feeds the spectrum tap, and publishes each
    /// raw I/Q payload onto `iq_bus`. Phase 2 of the 0.4.0 audio-path
    /// refactor split the previous single-thread "read SDR + write to
    /// nrsc5 stdin" loop into producer (this thread) and one or more
    /// consumers (`stdin_thread` below; per-program decoders in
    /// Phase 3).
    iq_thread: Option<JoinHandle<()>>,
    /// Fan-out bus that carries raw I/Q payloads from `iq_thread`
    /// (the one producer) to the nrsc5 stdin pump (Phase 2's single
    /// consumer) and, in Phase 3, to per-program decoder instances.
    /// `Some` only while a piped Start is active; reset by `stop`
    /// (we build a fresh bus on every `start_piped` so there is no
    /// stale state to migrate across stream sessions).
    iq_bus: Option<Arc<IqBus>>,
    /// Optional clone-cheap audio sink installed by the app at
    /// startup (via `set_audio_sink`). When `Some`, `start_piped`
    /// will request PCM on stdout from `nrsc5.exe` (`-o -`) and feed
    /// it to this sink. When `None`, the piped path falls back to
    /// `Stdio::null()` for stdout (audio is silently discarded —
    /// useful for headless testing).
    audio_sink: Option<crate::audio::AudioSink>,
    /// Long-lived speaker-routing thread spawned when an audio sink
    /// is installed. Receives per-decoder PCM rings via its command
    /// channel and forwards the active program's samples into the
    /// cpal sink. `None` in headless builds where no sink is wired.
    /// Phase 3 Chunk 2 wires this in for the single-decoder case;
    /// Chunk 3 makes `Nrsc5Process` a true multiplexer on top of it.
    speaker_router: Option<crate::audio::SpeakerRouter>,
    /// Program number whose decoded PCM is currently routed to the
    /// speakers. `Some` between `start_piped` and `stop`; `None`
    /// otherwise. Kept in sync with the router's own `active` state
    /// by sending `SpeakerCmd::SetActive` whenever this changes.
    active_speaker: Option<u32>,
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
    /// SoapySDR args string used to open the current/last piped
    /// stream's SDR (e.g. `"driver=rtlsdr"` or
    /// `"driver=sdrplay,serial=00000001"`). Preserved across `stop()`
    /// so [`retune`](Self::retune) can re-open the same device without
    /// the caller having to plumb the config section through. `None`
    /// until the first piped Start.
    last_sdr_args: Option<String>,
    /// PPM correction applied to the current/last piped stream's SDR.
    /// Same lifecycle as `last_sdr_args`.
    last_ppm: Option<f64>,
    tx: Sender<NrscEvent>,
    rx: Receiver<NrscEvent>,
    exe_path: PathBuf,
    aas_dir: PathBuf,
}

/// Translate one closed-loop AGC controller decision into the
/// corresponding gain-element write on the live SDR, observing the
/// device profile's sign convention and the element's reported range.
///
/// The AGC controller speaks in "tenths of dB of overall gain"
/// (matches the legacy librtlsdr convention from v0.2.x). Each device
/// has a different physical knob the controller should drive: RTL-SDR
/// has a single straight-gain `TUNER`, SDRplay has a `IFGR` (gain
/// reduction — *lower* is more gain), HackRF has a stepped `LNA`.
/// [`DeviceProfile`] encodes the per-driver mapping; this function is
/// the single place that mapping is applied.
///
/// Clamping happens here, not in the profile, because the element's
/// actual `[min_db, max_db]` is queried per-device at run time and may
/// be narrower than the synthesized AGC tenths table suggests (e.g.
/// an SDRplay revision that limits IFGR to 24..59 instead of 20..59).
///
/// Returns the dB value actually written (post-clamp) so the caller
/// can log it; returns `None` if the device doesn't expose the
/// profile's target element at all (in which case we log a warning
/// and treat the AGC as a no-op for this device).
fn apply_agc_action(
    sdr: &Arc<dyn Sdr>,
    profile: &DeviceProfile,
    action: &crate::dsp::AgcAction,
) -> Option<f64> {
    let target = profile.agc_element;
    let desired_db = profile.agc_tenths_to_element_db(action.new_tenths);

    // Look up the element's actual range. We deliberately re-query
    // every action rather than caching: it's a cheap Soapy call (no
    // hardware I/O) and it means the adapter survives mid-stream
    // configuration changes (e.g. SDRplay switching IF mode).
    let elements = sdr.gain_elements();
    let element = match elements
        .iter()
        .find(|e| e.name.eq_ignore_ascii_case(target))
    {
        Some(e) => e,
        None => {
            // Compatibility fallback for SDRplay module variants that
            // expose IFGR/RFGR but not aggregate Gain.
            if profile.driver == "sdrplay" {
                if let Some(ifgr) = elements
                    .iter()
                    .find(|e| e.name.eq_ignore_ascii_case("IFGR"))
                {
                    let mapped = map_sdrplay_gain_to_ifgr(profile, desired_db, ifgr);
                    if let Err(e) = sdr.set_gain_element(&ifgr.name, mapped) {
                        eprintln!(
                            "[agc] sdrplay fallback set_gain_element({}={:.2}dB) failed: {}",
                            ifgr.name, mapped, e
                        );
                        return None;
                    }
                    return Some(mapped);
                }
            }

            // The profile points at an element this device doesn't
            // expose. Either a profile bug or a driver version that
            // renamed it. Log once and let the caller no-op.
            eprintln!(
                "[agc] driver={} doesn't expose element {} — AGC disabled \
                 for this device. Elements present: {:?}",
                profile.driver,
                target,
                elements.iter().map(|e| &e.name).collect::<Vec<_>>()
            );
            return None;
        }
    };

    let clamped = desired_db.clamp(element.min_db, element.max_db);
    if let Err(e) = sdr.set_gain_element(&element.name, clamped) {
        eprintln!(
            "[agc] set_gain_element({}={:.2}dB) failed: {}",
            element.name, clamped, e
        );
        return None;
    }
    Some(clamped)
}

fn map_sdrplay_gain_to_ifgr(
    profile: &DeviceProfile,
    desired_gain_db: f64,
    ifgr: &crate::sdr::GainElement,
) -> f64 {
    let table = profile.agc_tenths_table;
    let min_gain_db = table.first().copied().unwrap_or(200) as f64 / 10.0;
    let max_gain_db = table.last().copied().unwrap_or(480) as f64 / 10.0;
    let denom = (max_gain_db - min_gain_db).max(1e-9);
    let norm = ((desired_gain_db - min_gain_db) / denom).clamp(0.0, 1.0);

    // SDRplay IFGR is a reduction control: lower IFGR = more gain.
    // Invert the normalized gain request into the IFGR range.
    let ifgr_span = ifgr.max_db - ifgr.min_db;
    ifgr.max_db - (norm * ifgr_span)
}

impl Nrsc5Process {
    pub fn new() -> Result<Self, Nrsc5Error> {
        let exe_path = find_nrsc5_exe().ok_or(Nrsc5Error::ExeNotFound)?;
        let (tx, rx) = unbounded();
        let aas_dir = crate::paths::aas_temp_dir();
        let _ = std::fs::create_dir_all(&aas_dir);
        Ok(Self {
            decoders: Vec::new(),
            iq_thread: None,
            iq_bus: None,
            audio_sink: None,
            speaker_router: None,
            active_speaker: None,
            sdr: None,
            last_mode: None,
            spectrum_tap: None,
            agc: None,
            agc_thread: None,
            agc_stop: None,
            last_gain_mode: None,
            last_manual_gain_tenths: None,
            last_sdr_args: None,
            last_ppm: None,
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

    /// Install an audio sink that will receive PCM from `nrsc5.exe`
    /// (invoked with `-o -`) whenever a piped stream is active. Call
    /// once at app startup. When absent, the piped path runs with
    /// `Stdio::null()` on the child's stdout and produces no audio.
    /// Only the piped path emits PCM through this sink — the legacy
    /// `start()` (USB direct) and `start_rtltcp()` paths still let
    /// `nrsc5.exe` drive libao itself.
    ///
    /// Also spawns the long-lived `SpeakerRouter` thread that drains
    /// per-decoder PCM rings into this sink. If a sink was previously
    /// installed, the prior router is shut down before the new one is
    /// spawned so we never have two routers competing for the sink.
    pub fn set_audio_sink(&mut self, sink: crate::audio::AudioSink) {
        // Shut down any pre-existing router first; safe even when
        // mid-stream because the router only forwards samples — the
        // decoder rings stay alive on the live `DecoderInstance` and
        // the new router will pick them up via fresh `AddDecoder`
        // commands on the next `start_piped`.
        if let Some(mut prev) = self.speaker_router.take() {
            prev.shutdown();
        }
        self.speaker_router = Some(crate::audio::SpeakerRouter::spawn(sink.clone()));
        self.audio_sink = Some(sink);
    }

    // ---------------------------------------------------------------
    // Multi-decoder API (Phase 3 Chunk 3)
    //
    // `start_piped` brings up the shared SDR + IqBus + speaker router
    // and spawns the first decoder. Callers can then add up to
    // [`MAX_DECODERS`] additional decoders against the same SDR via
    // `add_decoder`, switch which one is audible via
    // `set_active_speaker`, and tear individual ones down via
    // `remove_decoder`. `stop()` always tears down everything.
    //
    // The legacy USB and rtl_tcp paths (`start` / `start_rtltcp`) are
    // single-decoder only; calling `add_decoder` against them returns
    // `Nrsc5Error::NotStarted` because there's no IqBus to subscribe
    // a new decoder to.
    // ---------------------------------------------------------------

    /// Programs currently being decoded, in spawn order. Cheap; the
    /// returned Vec is freshly allocated and owned by the caller. The
    /// GUI calls this every frame to drive the HD1-HD8 grid's
    /// "decoded" indicators.
    pub fn decoded_programs(&self) -> Vec<u32> {
        self.decoders.iter().map(|d| d.program).collect()
    }

    /// Whether `program` is currently being decoded against the
    /// shared SDR pipeline. O(N) over the small `decoders` vec
    /// (capped at [`MAX_DECODERS`]).
    pub fn is_decoding(&self, program: u32) -> bool {
        self.decoders.iter().any(|d| d.program == program)
    }

    /// Program whose decoded PCM is currently routed to the
    /// speakers, or `None` when no piped session is active.
    pub fn active_speaker(&self) -> Option<u32> {
        self.active_speaker
    }

    /// Spawn an additional decoder for `program` against the
    /// currently-running shared SDR pipeline. The decoder subscribes
    /// to the IqBus, gets its own PCM ring registered with the
    /// speaker router, and starts producing events on the shared
    /// `events()` channel just like the first decoder did.
    ///
    /// Does **not** change the active speaker — call
    /// `set_active_speaker(program)` afterwards to listen to it. The
    /// new decoder runs silently in the background until then; its
    /// PCM ring is drained-and-discarded by the router, which keeps
    /// CPU steady but doesn't ship samples to the cpal sink.
    ///
    /// Errors:
    /// * [`Nrsc5Error::NotStarted`] — no piped session is active.
    /// * [`Nrsc5Error::DecoderAlreadyActive`] — `program` is already
    ///   being decoded; idempotent (no state changed).
    /// * [`Nrsc5Error::DecoderCapReached`] — already at
    ///   [`MAX_DECODERS`]; tear one down first.
    /// * [`Nrsc5Error::Spawn`] — failed to spawn nrsc5.exe.
    pub fn add_decoder(&mut self, program: u32) -> Result<(), Nrsc5Error> {
        // Validate the shared pipeline is up and we have headroom.
        if self.iq_bus.is_none() {
            return Err(Nrsc5Error::NotStarted);
        }
        if self.is_decoding(program) {
            return Err(Nrsc5Error::DecoderAlreadyActive(program));
        }
        if self.decoders.len() >= MAX_DECODERS {
            return Err(Nrsc5Error::DecoderCapReached(
                self.decoders.len(),
                MAX_DECODERS,
            ));
        }

        // Build the nrsc5 child. Mirrors `start_piped`'s argv exactly
        // except this isn't the first decoder, so we don't reset the
        // cpal queue (would interrupt the currently-playing decoder).
        let have_sink = self.audio_sink.is_some();
        let mut cmd = Command::new(&self.exe_path);
        cmd.arg("-l").arg("1");
        cmd.arg("-r").arg("-");
        if have_sink {
            cmd.arg("-o").arg("-");
        }
        cmd.arg("--dump-aas-files").arg(&self.aas_dir);
        cmd.arg(program.to_string());
        cmd.stdin(Stdio::piped());
        if have_sink {
            cmd.stdout(Stdio::piped());
        } else {
            cmd.stdout(Stdio::null());
        }
        cmd.stderr(Stdio::piped());
        #[cfg(target_os = "windows")]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
        }

        let mut child = cmd.spawn().map_err(Nrsc5Error::Spawn)?;
        let mut child_stdin = child.stdin.take().expect("stdin was piped");
        let stderr = child.stderr.take().expect("stderr was piped");
        let child_stdout = if have_sink {
            Some(child.stdout.take().expect("stdout was piped when sink installed"))
        } else {
            None
        };

        // stderr pump. Additional decoders do NOT drive the AGC
        // controller — only the first decoder spawned by
        // `start_piped` gets that responsibility. See the
        // `agc_stderr_handle` plumbing in `start_piped`.
        let stderr_tx = self.tx.clone();
        let stderr_thread = std::thread::spawn(move || {
            parse_stderr(stderr, stderr_tx, program, None);
        });

        // I/Q stdin pump. Subscribes its own receiver on the shared
        // bus; the bus's prune-on-Disconnected logic in
        // `IqBus::publish` cleans up automatically when the receiver
        // drops at thread exit.
        let bus = self.iq_bus.as_ref().expect("iq_bus is_some, checked above");
        let stdin_rx = bus.subscribe(64);
        let stdin_thread = std::thread::spawn(move || {
            while let Ok(payload) = stdin_rx.recv() {
                if child_stdin.write_all(&payload).is_err() {
                    break;
                }
            }
            drop(child_stdin);
        });

        // PCM ring + pump (identical to start_piped's pcm_pump body).
        let pcm_ring: Option<Arc<crate::audio::PcmRing>> = match (child_stdout.is_some(), have_sink) {
            (true, true) => Some(Arc::new(crate::audio::PcmRing::new())),
            _ => None,
        };
        let pcm_thread = match (child_stdout, pcm_ring.clone()) {
            (Some(mut stdout), Some(ring)) => {
                let exit_tx = self.tx.clone();
                let ring_for_thread = Arc::clone(&ring);
                let handle = std::thread::spawn(move || {
                    use std::io::Read;
                    const BYTES_PER_READ: usize = 2048;
                    let mut byte_buf = [0u8; BYTES_PER_READ];
                    let mut sample_buf: Vec<i16> = Vec::with_capacity(BYTES_PER_READ / 2);
                    loop {
                        match stdout.read(&mut byte_buf) {
                            Ok(0) => break,
                            Ok(n) => {
                                let pair_count = n / 2;
                                sample_buf.clear();
                                sample_buf.reserve(pair_count);
                                for chunk in byte_buf[..pair_count * 2].chunks_exact(2) {
                                    sample_buf.push(i16::from_le_bytes([chunk[0], chunk[1]]));
                                }
                                ring_for_thread.push(&sample_buf);
                            }
                            Err(_) => break,
                        }
                    }
                    let _ = exit_tx.send(NrscEvent::ChildExited);
                });
                Some(handle)
            }
            _ => None,
        };

        // Register the new ring with the router. Do NOT auto-activate
        // it as speaker — caller decides via `set_active_speaker`.
        if let (Some(router), Some(ring)) =
            (self.speaker_router.as_ref(), pcm_ring.as_ref())
        {
            let _ = router.cmd_tx().send(crate::audio::SpeakerCmd::AddDecoder {
                program,
                ring: Arc::clone(ring),
            });
        }

        self.decoders.push(DecoderInstance {
            program,
            child,
            stderr_thread,
            stdin_thread: Some(stdin_thread),
            pcm_thread,
            pcm_ring,
        });
        Ok(())
    }

    /// Tear down the decoder for `program`. Idempotent — returns
    /// `false` when no decoder was running for that program.
    ///
    /// If the removed decoder was the active speaker, the speaker
    /// goes silent until the next `set_active_speaker` call. The
    /// shared SDR pipeline stays running; call `stop()` for a full
    /// teardown.
    pub fn remove_decoder(&mut self, program: u32) -> bool {
        let Some(idx) = self.decoders.iter().position(|d| d.program == program) else {
            return false;
        };
        let DecoderInstance {
            program,
            mut child,
            stderr_thread,
            stdin_thread,
            pcm_thread,
            pcm_ring,
        } = self.decoders.remove(idx);

        // Same teardown order as `stop()`: detach router → kill child
        // → join threads. The IqBus subscriber is owned by
        // `stdin_thread`; killing the child causes BrokenPipe on the
        // next write, which exits the loop and drops the Receiver.
        // The bus's `publish` prune logic removes the subscriber on
        // its next call.
        if let Some(router) = self.speaker_router.as_ref() {
            let _ = router
                .cmd_tx()
                .send(crate::audio::SpeakerCmd::RemoveDecoder(program));
        }
        if Some(program) == self.active_speaker {
            self.active_speaker = None;
        }
        let _ = pcm_ring;
        let _ = child.kill();
        let _ = child.wait();
        if let Some(handle) = stdin_thread {
            let _ = handle.join();
        }
        if let Some(handle) = pcm_thread {
            let _ = handle.join();
        }
        let _ = stderr_thread.join();
        true
    }

    /// Route `program`'s decoded PCM to the speakers. The previous
    /// active speaker (if any) stays decoding in the background —
    /// its ring is drained-and-discarded by the router. Returns
    /// `NoSuchDecoder` if `program` isn't currently being decoded.
    pub fn set_active_speaker(&mut self, program: u32) -> Result<(), Nrsc5Error> {
        if !self.is_decoding(program) {
            return Err(Nrsc5Error::NoSuchDecoder(program));
        }
        if let Some(router) = self.speaker_router.as_ref() {
            let _ = router
                .cmd_tx()
                .send(crate::audio::SpeakerCmd::SetActive(program));
        }
        // Clear the cpal queue so the transition is immediate
        // rather than playing through whatever 100-200 ms of the
        // previous program is still buffered.
        if let Some(sink) = self.audio_sink.as_ref() {
            sink.clear();
        }
        self.active_speaker = Some(program);
        Ok(())
    }

    /// Attach an Opus recorder tap to `program`'s PCM stream. The
    /// recorder receives a clone of every chunk the `SpeakerRouter`
    /// drains from `program`'s ring, independent of which subchannel
    /// is currently on the speakers — so the user can listen to HD2
    /// while recording HD1, or vice versa. Returns `NoSuchDecoder` if
    /// `program` isn't currently being decoded (a recorder against a
    /// dead decoder would just produce a zero-byte file). The tap is
    /// last-writer-wins: attaching twice to the same program drops
    /// the previous recorder cleanly (its forwarder sees the closed
    /// channel and flushes the file).
    pub fn attach_recorder(
        &mut self,
        program: u32,
        tap: crossbeam_channel::Sender<Vec<i16>>,
    ) -> Result<(), Nrsc5Error> {
        if !self.is_decoding(program) {
            return Err(Nrsc5Error::NoSuchDecoder(program));
        }
        if let Some(router) = self.speaker_router.as_ref() {
            let _ = router
                .cmd_tx()
                .send(crate::audio::SpeakerCmd::AttachRecorder { program, tap });
        }
        Ok(())
    }

    /// Detach the Opus recorder tap (if any) from `program`. After
    /// this returns the recorder thread sees a closed forwarder
    /// channel, sends itself a Stop, encodes any leftover frames,
    /// and writes the Ogg EOS page. Idempotent — safe to call when
    /// no tap is attached.
    pub fn detach_recorder(&mut self, program: u32) {
        if let Some(router) = self.speaker_router.as_ref() {
            let _ = router
                .cmd_tx()
                .send(crate::audio::SpeakerCmd::DetachRecorder(program));
        }
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

    /// Apply a manual per-element gain on the live SDR if a piped
    /// stream is currently running. No-op when there's no active SDR
    /// (the change still survives in config and is applied on the
    /// next Start). Returns `Ok(())` for the no-op case too — callers
    /// don't need to distinguish "no stream running" from "applied".
    pub fn set_sdr_gain_element(
        &self,
        element: &str,
        value_db: f64,
    ) -> Result<(), SdrError> {
        match self.sdr.as_ref() {
            Some(sdr) => sdr.set_gain_element(element, value_db),
            None => Ok(()),
        }
    }

    /// Apply a frequency-correction PPM nudge to the live SDR. Same
    /// no-op-when-idle semantics as `set_sdr_gain_element`. Some
    /// backends (SDRplay) silently ignore this — see their `Sdr`
    /// trait impl for details.
    pub fn set_sdr_freq_correction_ppm(&self, ppm: f64) -> Result<(), SdrError> {
        match self.sdr.as_ref() {
            Some(sdr) => sdr.set_frequency_correction_ppm(ppm),
            None => Ok(()),
        }
    }

    /// Snapshot the live SDR's reported gain elements. Returns an
    /// empty `Vec` when no stream is running — the SDR Settings modal
    /// then falls back to an idle open-and-close to populate its
    /// sliders.
    pub fn sdr_gain_elements(&self) -> Vec<crate::sdr::GainElement> {
        self.sdr
            .as_ref()
            .map(|s| s.gain_elements())
            .unwrap_or_default()
    }

    pub fn version(&self) -> String {
        format!("nrsc5 process ({})", self.exe_path.display())
    }

    pub fn aas_dir(&self) -> &std::path::Path {
        &self.aas_dir
    }

    /// PID of the running nrsc5 process, or `None` if not running.
    /// In Phase 3 Chunk 3+ this will return the active speaker's PID
    /// when multiple decoders are running; for now there's at most one.
    pub fn pid(&self) -> Option<u32> {
        // Multi-decoder: return the first running decoder's PID. The
        // GUI displays this in the status bar; with multi-decode
        // active, callers that want a specific program should iterate
        // `decoded_programs()` and look up each `Child::id` themselves.
        self.decoders.first().map(|d| d.child.id())
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

        self.decoders.push(DecoderInstance {
            program,
            child,
            stderr_thread,
            stdin_thread: None,
            pcm_thread: None,
            pcm_ring: None,
        });
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

        self.decoders.push(DecoderInstance {
            program,
            child,
            stderr_thread,
            stdin_thread: None,
            pcm_thread: None,
            pcm_ring: None,
        });
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
        sdr_args: &str,
        ppm_correction: f64,
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
        // Open the SDR via SoapySDR. The args string already encodes
        // `driver=` plus any per-device disambiguators (serial /
        // device index / soapy_remote URL etc.). One open path,
        // every supported device — the legacy `RtlSdr::open(idx)`
        // call that lived here in 0.2.x is gone.
        let soapy = crate::sdr::SoapySdr::open(sdr_args)?;
        // Apply config-driven PPM correction. Zero is the common case;
        // backends that don't expose runtime PPM return Ok(()) silently.
        let _ = soapy.set_frequency_correction_ppm(ppm_correction);
        soapy.configure(&SdrConfig {
            center_freq_hz: (frequency_mhz * 1_000_000.0) as u32,
            sample_rate_sps: 1_488_375,
            ppm_correction: 0,
            direct_sampling: 0,
            initial_gain_tenths,
        })?;
        let sdr: Arc<dyn Sdr> = Arc::new(soapy);

        let mut cmd = Command::new(&self.exe_path);
        // -r -  : read raw I/Q from stdin.
        // -o -  : emit decoded PCM (s16le 44.1 kHz stereo) on stdout
        //         when an audio sink is installed. v0.4.0 Phase 1
        //         refactor — the Rust app owns playback now instead
        //         of letting nrsc5 drive libao itself. When no sink
        //         is installed (headless tests), stdout is nulled
        //         and nrsc5 still plays its own audio via libao
        //         (legacy fallback; harmless in single-process tests).
        // -l 1  : librtlsdr-style log verbosity.
        //
        // In `-r -` mode nrsc5 v3.1.0 only accepts a SINGLE positional
        // (program); passing both `frequency program` makes it bail to
        // the usage banner. We tune the dongle ourselves via the SDR
        // config above, so the frequency on the CLI is unnecessary.
        let _ = frequency_mhz;
        cmd.arg("-l").arg("1");
        cmd.arg("-r").arg("-");
        let have_sink = self.audio_sink.is_some();
        if have_sink {
            cmd.arg("-o").arg("-");
        }
        cmd.arg("--dump-aas-files").arg(&self.aas_dir);
        cmd.arg(program.to_string());

        cmd.stdin(Stdio::piped());
        if have_sink {
            cmd.stdout(Stdio::piped());
        } else {
            cmd.stdout(Stdio::null());
        }
        cmd.stderr(Stdio::piped());

        #[cfg(target_os = "windows")]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
        }

        let mut child = cmd.spawn().map_err(Nrsc5Error::Spawn)?;
        let mut child_stdin = child.stdin.take().expect("stdin was piped");
        let stderr = child.stderr.take().expect("stderr was piped");
        // When an audio sink is installed, grab the child's stdout so
        // the pcm_pump thread (spawned just below) can read decoded
        // PCM and push it into the shared playback queue. When no
        // sink is installed we left stdout as `Stdio::null()`, so
        // `child.stdout` is `None` and there is no pump.
        let child_stdout = if have_sink {
            Some(child.stdout.take().expect("stdout was piped when sink installed"))
        } else {
            None
        };

        // ----- AGC controller (only in `Auto` mode) -----------------
        // Build the controller, apply its initial gain to the SDR, and
        // wrap in `Arc<Mutex<_>>` so the stderr-parser thread (tee) and
        // the AGC driver thread (tick + apply) can share it. In
        // `Manual` / `HardwareAgc` we leave these `None` and skip the
        // driver thread entirely — the dongle's gain is set once by
        // `configure` above and never touched again for this stream.
        //
        // The controller walks a per-device tenths-of-dB table sourced
        // from the device profile (NOT from `sdr.gain_table_tenths()`,
        // which is the legacy librtlsdr-specific accessor). The
        // adapter (`apply_agc_action`) translates each tenths value
        // into a `set_gain_element` call on the profile's target
        // element, observing the device's actual range.
        let profile = crate::sdr::profile::lookup(sdr.driver())
            .copied()
            .unwrap_or(crate::sdr::profile::RTLSDR);
        let (agc, agc_stderr_handle) = if gain_mode == GainMode::Auto {
            // Build the controller with the profile's per-driver start
            // gain. The global `AgcConfig::default()` aims at the RTL-SDR
            // sweet spot (19.7 dB); SDRplay and HackRF override it via
            // `default_agc_initial_tenths` so they land closer to their
            // own HD lock range on first tick. Each profile also
            // picks the initial search direction — RTL-SDR walks
            // DOWN from 19.7 dB (over-clip caution), SDRplay walks UP
            // from 39 dB (HD sweet spot is above the start, not below).
            let mut agc_cfg = AgcConfig::default();
            agc_cfg.initial_tenths = profile.default_agc_initial_tenths;
            agc_cfg.initial_direction = profile.default_agc_initial_direction;
            let agc_ctrl = AgcController::new(
                profile.agc_tenths_table,
                agc_cfg,
            );
            let agc = Arc::new(Mutex::new(agc_ctrl));
            let initial = agc
                .lock()
                .expect("AGC mutex poisoned at startup")
                .initial_action();
            let _ = apply_agc_action(&sdr, &profile, &initial);
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

        // I/Q fan-out bus (Phase 2 of the 0.4.0 audio-path refactor).
        // One producer (this method's SDR pump thread), one consumer
        // today (the stdin pump below). Phase 3 will subscribe one
        // additional consumer per HD program. Capacity 64 ≈ 100 ms of
        // buffer at 1.488 Msps CS16 in ~4 KB chunks — enough to
        // absorb consumer scheduling jitter without back-pressuring
        // the SDR.
        let bus = Arc::new(IqBus::new());
        let stdin_rx = bus.subscribe(64);

        // nrsc5 stdin pump. Subscribes to the bus, writes each
        // payload to the child's stdin. Exits when the bus
        // disconnects (SDR pump shutdown) or when stdin breaks (child
        // died). Drops `ChildStdin` on exit so nrsc5 gets EOF
        // immediately.
        let stdin_thread = std::thread::spawn(move || {
            while let Ok(payload) = stdin_rx.recv() {
                if child_stdin.write_all(&payload).is_err() {
                    // BrokenPipe: child is gone. The pcm_pump will
                    // independently observe stdout EOF and emit
                    // `ChildExited` to the app, which calls `stop()`.
                    break;
                }
            }
            drop(child_stdin);
        });

        // I/Q source pump. Runs `run_stream`, feeds the spectrum tap,
        // and publishes raw bytes onto the bus. On exit (clean
        // cancel, USB unplug, etc.) calls `bus.shutdown()` so every
        // subscriber (the stdin pump above, plus any Phase 3
        // decoders) sees `Disconnected` on `recv` and exits cleanly.
        let sdr_for_thread: Arc<dyn Sdr> = Arc::clone(&sdr);
        let bus_for_sdr = Arc::clone(&bus);
        let evt_tx = self.tx.clone();
        // Optional FFT tap clone for the Spectrum panel. `None` when the
        // GUI side hasn't installed one (e.g. headless test builds).
        let spectrum_tap = self.spectrum_tap.clone();
        if let Some(tap) = spectrum_tap.as_ref() {
            tap.set_center_freq_hz((frequency_mhz as f64) * 1_000_000.0);
        }
        let iq_thread = std::thread::spawn(move || {
            let run_res = sdr_for_thread.run_stream(&mut |bytes| {
                // Spectrum tap first — it's cheap (and internally
                // throttled) and we want the panel to keep updating
                // regardless of any consumer back-pressure on the bus.
                if let Some(tap) = spectrum_tap.as_ref() {
                    tap.feed(bytes);
                }
                // Non-blocking publish. Slow / dead subscribers are
                // handled inside the bus (drop payload on Full, prune
                // on Disconnected). The SDR pump itself never blocks.
                bus_for_sdr.publish(bytes);
                StreamControl::Continue
            });
            // Tear down every subscriber so the stdin pump (and any
            // Phase 3 decoders) wake from their blocking `recv` with
            // `Err(RecvError)` and exit. Idempotent; safe even if a
            // subscriber already pruned itself via BrokenPipe.
            bus_for_sdr.shutdown();
            // `run_stream` returns Err on real backend failure (e.g.
            // USB unplugged). A user-initiated Stop trips the cancel
            // flag, which the rtl backend translates to Ok per
            // `stop_flag` discriminator in `src/sdr/rtl.rs`.
            if let Err(e) = &run_res {
                // Surface the real Soapy error on stderr so a user
                // hitting "device lost" can see whether it was a
                // timeout, an overflow, an API-service disconnect,
                // etc. Cheap diagnostic; only fires on actual
                // backend failure, not on user Stop.
                eprintln!("[sdr] run_stream failed: {e}");
                let _ = evt_tx.send(NrscEvent::LostDevice);
                let _ = evt_tx.send(NrscEvent::LostDeviceDetail(e.to_string()));
            }
        });

        // ----- PCM pump (only when an audio sink is installed) -----
        // Reads interleaved s16 LE 44.1 kHz stereo from `nrsc5.exe`'s
        // stdout in ~10 ms chunks and pushes them into this decoder's
        // private `PcmRing`. The shared `SpeakerRouter` thread (spawned
        // in `set_audio_sink`) drains the ring on its own polling tick
        // and forwards samples to the cpal `AudioSink` only when this
        // decoder is the active speaker. Phase 3 Chunk 2 wires the
        // routing through one ring; Chunk 3 expands to N rings.
        //
        // Exits cleanly on EOF (child died / killed / closed stdout).
        // Allocation-free hot path: one fixed scratch buffer reused
        // for every read.
        //
        // On exit (any cause — EOF, BrokenPipe, real I/O error) the
        // thread emits a `ChildExited` event so the app can detect a
        // dead child without polling `Child::try_wait` every frame.
        let pcm_ring: Option<Arc<crate::audio::PcmRing>> =
            match (child_stdout.is_some(), self.audio_sink.is_some()) {
                (true, true) => Some(Arc::new(crate::audio::PcmRing::new())),
                _ => None,
            };
        let pcm_thread = match (child_stdout, self.audio_sink.clone(), pcm_ring.clone()) {
            (Some(mut stdout), Some(sink), Some(ring)) => {
                // Drop anything currently queued in the cpal sink so
                // the next Start doesn't replay stale audio from the
                // previous session. The router-owned per-decoder ring
                // is brand new (just allocated above), so it starts
                // empty by construction.
                sink.clear();
                let exit_tx = self.tx.clone();
                let ring_for_thread = Arc::clone(&ring);
                let handle = std::thread::spawn(move || {
                    use std::io::Read;
                    // 2048 bytes = 1024 s16 samples = 512 stereo
                    // frames ≈ 11.6 ms at 44.1 kHz. Small enough to
                    // keep wake-up latency low, big enough to keep
                    // syscall overhead negligible.
                    const BYTES_PER_READ: usize = 2048;
                    let mut byte_buf = [0u8; BYTES_PER_READ];
                    let mut sample_buf: Vec<i16> = Vec::with_capacity(BYTES_PER_READ / 2);
                    loop {
                        match stdout.read(&mut byte_buf) {
                            Ok(0) => break, // EOF — child closed stdout
                            Ok(n) => {
                                // Reinterpret the read bytes as s16 LE.
                                // We always read into the front of
                                // `byte_buf` so the partial-read case
                                // is just `n` bytes / 2 samples; an
                                // odd `n` byte is dropped on the
                                // floor (extremely unlikely with
                                // OS-level pipe semantics, but
                                // tolerated rather than panicked on).
                                let pair_count = n / 2;
                                sample_buf.clear();
                                sample_buf.reserve(pair_count);
                                for chunk in byte_buf[..pair_count * 2].chunks_exact(2) {
                                    sample_buf.push(i16::from_le_bytes([chunk[0], chunk[1]]));
                                }
                                ring_for_thread.push(&sample_buf);
                            }
                            Err(e) => {
                                // Genuine I/O error. Most commonly
                                // a BrokenPipe when the child was
                                // killed mid-write. Treated identically
                                // to EOF: bail out cleanly.
                                let _ = e;
                                break;
                            }
                        }
                    }
                    // Notify the app that the child closed its stdout.
                    // Idempotent on the receiving side — if `stop()`
                    // was the cause, the app's `is_streaming` flag has
                    // already been cleared and the event is ignored.
                    let _ = exit_tx.send(NrscEvent::ChildExited);
                });
                Some(handle)
            }
            _ => None,
        };

        // Hand the new ring to the router and make this decoder the
        // active speaker. Both commands are sent over the long-lived
        // command channel set up in `set_audio_sink`; the router
        // applies them on its next tick. Skipped when no audio sink
        // is installed (no router, no ring).
        if let (Some(router), Some(ring)) = (self.speaker_router.as_ref(), pcm_ring.as_ref()) {
            let tx = router.cmd_tx();
            let _ = tx.send(crate::audio::SpeakerCmd::AddDecoder {
                program,
                ring: Arc::clone(ring),
            });
            let _ = tx.send(crate::audio::SpeakerCmd::SetActive(program));
            self.active_speaker = Some(program);
        }

        self.decoders.push(DecoderInstance {
            program,
            child,
            stderr_thread,
            stdin_thread: Some(stdin_thread),
            pcm_thread,
            pcm_ring,
        });
        self.iq_thread = Some(iq_thread);
        self.iq_bus = Some(bus);
        self.sdr = Some(sdr);
        self.last_mode = Some(LastStartMode::Piped);
        self.last_gain_mode = Some(gain_mode);
        self.last_manual_gain_tenths = Some(manual_gain_tenths);
        self.last_sdr_args = Some(sdr_args.to_string());
        self.last_ppm = Some(ppm_correction);

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
            // Capture the profile by value (it's Copy) so the driver
            // thread doesn't need to borrow anything from the outer
            // scope. Tick rate and gain-element mapping are both
            // baked into this copy.
            let agc_profile = profile;
            let tick_ms = profile.agc_tick_ms;
            let agc_thread = std::thread::spawn(move || {
                // SDRplay is sensitive right after stream activation;
                // avoid immediate AGC writes in the first moment.
                let startup_grace_ms = if agc_profile.driver == "sdrplay" {
                    1500
                } else {
                    0
                };
                if startup_grace_ms > 0 {
                    std::thread::sleep(std::time::Duration::from_millis(startup_grace_ms));
                }
                while !agc_stop_for_driver.load(Ordering::Relaxed) {
                    std::thread::sleep(std::time::Duration::from_millis(tick_ms));
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
                        let _ = apply_agc_action(&sdr_for_agc, &agc_profile, &action);
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
        // Join the I/Q source pump first. `cancel_stream` above made
        // `run_stream` return; on exit the source thread already
        // called `bus.shutdown()`, which drops every subscriber's
        // `Sender` — so the stdin pump (joined next) and any Phase 3
        // decoders will wake from `recv` with `Err(RecvError)`.
        if let Some(handle) = self.iq_thread.take() {
            let _ = handle.join();
        }
        // Tear down every running decoder. For Chunk 3 there can be
        // more than one (the user may have opted in to extras via
        // `add_decoder`). The teardown order per decoder matches the
        // single-instance flow: detach from the speaker router →
        // join stdin pump → kill child → join pcm pump → join
        // stderr pump.
        //
        // The shared SDR pump above has already called
        // `bus.shutdown()`, so every decoder's stdin pump has
        // already woken from its blocking `recv` and exited. The
        // joins below are therefore fast (microseconds).
        let drained: Vec<DecoderInstance> = self.decoders.drain(..).collect();
        let any_decoder = !drained.is_empty();
        for decoder in drained {
            let DecoderInstance {
                program,
                mut child,
                stderr_thread,
                stdin_thread,
                pcm_thread,
                pcm_ring,
            } = decoder;
            // Detach this program's ring from the speaker router so
            // the router stops draining (and forwarding) samples
            // from a soon-to-be-dead child. Idempotent on the
            // router's side. The ring itself is dropped when
            // `pcm_ring` goes out of scope at the end of this block.
            if let Some(router) = self.speaker_router.as_ref() {
                let _ = router.cmd_tx().send(
                    crate::audio::SpeakerCmd::RemoveDecoder(program),
                );
            }
            if Some(program) == self.active_speaker {
                self.active_speaker = None;
            }
            let _ = pcm_ring;
            // Join the nrsc5 stdin pump. By this point its bus
            // receiver is disconnected (the source thread above
            // already called `bus.shutdown`), so it has exited its
            // `recv -> write_all` loop and dropped `ChildStdin`,
            // sending EOF to nrsc5. `None` on legacy USB / rtl_tcp.
            if let Some(handle) = stdin_thread {
                let _ = handle.join();
            }
            // Kill the nrsc5 child as a belt-and-suspenders backstop
            // in case it didn't exit on its own from EOF.
            let _ = child.kill();
            let _ = child.wait();
            // Now that the child is dead, its stdout has closed and
            // the pcm_pump's read loop has returned EOF. Joining is
            // fast. `None` when no audio sink is installed or on the
            // legacy USB / rtl_tcp paths.
            if let Some(handle) = pcm_thread {
                let _ = handle.join();
            }
            let _ = stderr_thread.join();
        }
        // Bus is single-use per stream — drop it so a stale
        // reference isn't carried into the next `start_piped`
        // (which builds a fresh bus).
        self.iq_bus = None;
        // Discard any PCM still sitting in the cpal queue so the
        // next Start doesn't replay a fraction of a second of stale
        // audio. The per-decoder rings are dropped above when their
        // `pcm_ring` Arcs go out of scope.
        if any_decoder {
            if let Some(sink) = self.audio_sink.as_ref() {
                sink.clear();
            }
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
                // Reuse the gain settings and SDR args from the
                // previous start. The args string was stashed by
                // start_piped; if it's missing for some reason (e.g.
                // synthetic call ordering in tests), fall back to a
                // bare RTL-SDR open.
                let gain_mode = self.last_gain_mode.unwrap_or_default();
                let manual = self.last_manual_gain_tenths.unwrap_or(197);
                let args = self
                    .last_sdr_args
                    .clone()
                    .unwrap_or_else(|| "driver=rtlsdr".to_string());
                let ppm = self.last_ppm.unwrap_or(0.0);
                self.start_piped(frequency_mhz, program, &args, ppm, gain_mode, manual)
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

        // "Audio bit rate:" is also surfaced as a recurring
        // `AudioBitRate` event so the Station Info panel can show a
        // live kbps readout for the currently-decoded program. The
        // one-shot `AudioStarted` event above is separate — it only
        // fires on the first occurrence to drive the "audio started
        // in Xs" status message.
        if let Some(rest) = msg.strip_prefix("Audio bit rate: ") {
            if let Some(kbps) = parse_audio_bitrate(rest) {
                if tx.send(NrscEvent::AudioBitRate { program, kbps }).is_err() {
                    break;
                }
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
        return parse_lot(rest, program);
    }

    // "XHDR: 0 BE4B7536 42"
    if let Some(rest) = msg.strip_prefix("XHDR: ") {
        return parse_xhdr(rest, program);
    }

    // "Station name: KROQ-FM"
    if let Some(rest) = msg.strip_prefix("Station name: ") {
        return Some(NrscEvent::StationName(rest.to_string()));
    }

    // "Slogan: Today's Hits"
    if let Some(rest) = msg.strip_prefix("Slogan: ") {
        if rest.is_empty() {
            return None;
        }
        return Some(NrscEvent::Slogan(rest.to_string()));
    }

    // "Message: Welcome to KEGL"
    if let Some(rest) = msg.strip_prefix("Message: ") {
        if rest.is_empty() {
            return None;
        }
        return Some(NrscEvent::Message(rest.to_string()));
    }

    // "Location: 39.123456, -76.987654, 100 m"
    if let Some(rest) = msg.strip_prefix("Location: ") {
        return parse_location(rest);
    }

    // "Country code: US, FCC facility ID: 12345"
    if let Some(rest) = msg.strip_prefix("Country code: ") {
        return parse_country_fcc(rest);
    }

    // "Audio program 1: MPS, type: Music, sound experience: Mono"
    if let Some(rest) = msg.strip_prefix("Audio program ") {
        return parse_audio_program(rest);
    }

    // "SIG Service: type=audio number=2 name=The EDGE"
    if let Some(rest) = msg.strip_prefix("SIG Service: type=audio number=") {
        return parse_sig_service_audio(rest);
    }

    // "SIG Service: type=data number=4 name=Album Art"
    if let Some(rest) = msg.strip_prefix("SIG Service: type=data number=") {
        return parse_sig_service_data(rest);
    }

    // "Alert: National emergency test"
    if let Some(rest) = msg.strip_prefix("Alert: ") {
        if rest.is_empty() {
            return None;
        }
        return Some(NrscEvent::EmergencyAlert {
            text: rest.to_string(),
        });
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

/// Pull the leading float out of an `Audio bit rate:` value. Accepts
/// both the bare form ("96.0 kbps") and the extended form nrsc5 emits
/// on later cycles ("96.00 kbps (96.13 average, 12.43 min, 99.18
/// max)"). Returns `None` if the first token isn't a parseable float.
fn parse_audio_bitrate(rest: &str) -> Option<f32> {
    rest.split_whitespace().next()?.parse::<f32>().ok()
}

fn parse_lot(rest: &str, program: u32) -> Option<NrscEvent> {
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
    Some(NrscEvent::LotFile { program, lot, name: filename })
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

fn parse_sig_service_data(rest: &str) -> Option<NrscEvent> {
    // Same wire shape as the audio variant: "<N> name=<Name>".
    let (num_part, name_part) = rest.split_once(" name=")?;
    let number = num_part.parse::<u32>().ok()?;
    let name = name_part.trim().to_string();
    if name.is_empty() {
        return None;
    }
    Some(NrscEvent::SigServiceData { number, name })
}

fn parse_location(rest: &str) -> Option<NrscEvent> {
    // "39.123456, -76.987654, 100 m"
    let mut parts = rest.split(", ");
    let latitude = parts.next()?.trim().parse::<f64>().ok()?;
    let longitude = parts.next()?.trim().parse::<f64>().ok()?;
    // Altitude segment is "<N> m" — take the leading token.
    let alt_part = parts.next()?.trim();
    let altitude_m = alt_part.split_whitespace().next()?.parse::<i32>().ok()?;
    Some(NrscEvent::Location {
        latitude,
        longitude,
        altitude_m,
    })
}

fn parse_country_fcc(rest: &str) -> Option<NrscEvent> {
    // "US, FCC facility ID: 12345"
    let (country_part, fcc_part) = rest.split_once(", FCC facility ID: ")?;
    let country = country_part.trim().to_string();
    if country.is_empty() {
        return None;
    }
    let facility_id = fcc_part.trim().parse::<u32>().ok()?;
    Some(NrscEvent::CountryFcc {
        country,
        facility_id,
    })
}

fn parse_audio_program(rest: &str) -> Option<NrscEvent> {
    // rest = "1: MPS, type: Music, sound experience: Mono"
    // The MPS/SPSx token is redundant with `number` (MPS=1, SPS1=2, …),
    // so we skip it. We do capture type + sound experience.
    let (num_part, after_num) = rest.split_once(": ")?;
    let number = num_part.parse::<u32>().ok()?;

    // after_num = "MPS, type: Music, sound experience: Mono"
    let (_program_id, after_id) = after_num.split_once(", type: ")?;
    // after_id = "Music, sound experience: Mono"
    let (program_type, sound_experience) =
        after_id.split_once(", sound experience: ")?;
    let program_type = program_type.trim().to_string();
    let sound_experience = sound_experience.trim().to_string();
    if program_type.is_empty() || sound_experience.is_empty() {
        return None;
    }
    Some(NrscEvent::AudioProgram {
        number,
        program_type,
        sound_experience,
    })
}

fn parse_xhdr(rest: &str, program: u32) -> Option<NrscEvent> {
    // "0 BE4B7536 42"
    let mut parts = rest.split_whitespace();
    let param = parts.next()?.parse::<u32>().ok()?;
    let _mime = parts.next()?; // skip mime hash
    let lot = parts.next()?.to_string();
    Some(NrscEvent::Xhdr { program, param, lot })
}

// -- Exe discovery ----------------------------------------------------

#[cfg(unix)]
fn is_executable(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.is_file()
        && std::fs::metadata(path)
            .map(|m| (m.permissions().mode() & 0o111) != 0)
            .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &std::path::Path) -> bool {
    path.is_file()
}

fn find_on_path(exe_name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(exe_name);
        if is_executable(&candidate) {
            return Some(candidate);
        }
    }
    None
}

fn find_nrsc5_exe() -> Option<PathBuf> {
    let exe_name = if cfg!(target_os = "windows") {
        "nrsc5.exe"
    } else {
        "nrsc5"
    };

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join("bin").join(exe_name);
            if is_executable(&candidate) {
                return Some(candidate);
            }
            let candidate = dir.join(exe_name);
            if is_executable(&candidate) {
                return Some(candidate);
            }
        }
    }

    if let Ok(cwd) = std::env::current_dir() {
        let candidate = cwd.join("bin").join(exe_name);
        if is_executable(&candidate) {
            return Some(candidate);
        }
        let candidate = cwd.join(exe_name);
        if is_executable(&candidate) {
            return Some(candidate);
        }
    }

    // Linux packaging often installs `nrsc5` into /usr/bin rather than
    // shipping it beside this app, so fall back to PATH lookup.
    find_on_path(exe_name)
}

// -- Tests ------------------------------------------------------------

#[cfg(test)]
mod tests {
    //! Format-lock tests for the SIS-related stderr parsers added in
    //! 0.3.5. Each test mirrors the literal line nrsc5 prints so a
    //! future upstream wording change fails loudly instead of silently
    //! dropping events.

    use super::*;

    fn parse(msg: &str) -> Option<NrscEvent> {
        let mut audio_seen = false;
        parse_line(msg, 0, &mut audio_seen)
    }

    #[test]
    fn parses_slogan() {
        match parse("Slogan: Today's Hits") {
            Some(NrscEvent::Slogan(s)) => assert_eq!(s, "Today's Hits"),
            other => panic!("expected Slogan, got {:?}", other),
        }
        assert!(parse("Slogan: ").is_none());
    }

    #[test]
    fn parses_message() {
        match parse("Message: Welcome to KEGL") {
            Some(NrscEvent::Message(s)) => assert_eq!(s, "Welcome to KEGL"),
            other => panic!("expected Message, got {:?}", other),
        }
    }

    #[test]
    fn parses_location() {
        match parse("Location: 39.123456, -76.987654, 100 m") {
            Some(NrscEvent::Location {
                latitude,
                longitude,
                altitude_m,
            }) => {
                assert!((latitude - 39.123456).abs() < 1e-6);
                assert!((longitude - -76.987654).abs() < 1e-6);
                assert_eq!(altitude_m, 100);
            }
            other => panic!("expected Location, got {:?}", other),
        }
        // Malformed altitude segment must not crash.
        assert!(parse("Location: 39.0, -76.0, garbage").is_none());
    }

    #[test]
    fn parses_country_fcc() {
        match parse("Country code: US, FCC facility ID: 12345") {
            Some(NrscEvent::CountryFcc {
                country,
                facility_id,
            }) => {
                assert_eq!(country, "US");
                assert_eq!(facility_id, 12345);
            }
            other => panic!("expected CountryFcc, got {:?}", other),
        }
    }

    #[test]
    fn parses_audio_program() {
        match parse("Audio program 1: MPS, type: Music, sound experience: Mono") {
            Some(NrscEvent::AudioProgram {
                number,
                program_type,
                sound_experience,
            }) => {
                assert_eq!(number, 1);
                assert_eq!(program_type, "Music");
                assert_eq!(sound_experience, "Mono");
            }
            other => panic!("expected AudioProgram, got {:?}", other),
        }
        // HD2 with SPS1 identifier still resolves to number=2.
        match parse("Audio program 2: SPS1, type: Talk, sound experience: Stereo") {
            Some(NrscEvent::AudioProgram { number, .. }) => assert_eq!(number, 2),
            other => panic!("expected AudioProgram, got {:?}", other),
        }
    }

    #[test]
    fn parses_sig_service_data() {
        match parse("SIG Service: type=data number=4 name=Album Art") {
            Some(NrscEvent::SigServiceData { number, name }) => {
                assert_eq!(number, 4);
                assert_eq!(name, "Album Art");
            }
            other => panic!("expected SigServiceData, got {:?}", other),
        }
    }

    #[test]
    fn parses_alert_with_text() {
        match parse("Alert: Severe thunderstorm warning") {
            Some(NrscEvent::EmergencyAlert { text }) => {
                assert_eq!(text, "Severe thunderstorm warning");
            }
            other => panic!("expected EmergencyAlert, got {:?}", other),
        }
        // Empty alert text is dropped (we never want a blank popup).
        assert!(parse("Alert: ").is_none());
    }

    #[test]
    fn parses_audio_bitrate_helper() {
        // Bare form (early cycles).
        assert_eq!(parse_audio_bitrate("96.0 kbps"), Some(96.0));
        // Extended form (later cycles, with stats trailer).
        assert_eq!(
            parse_audio_bitrate("96.00 kbps (96.13 average, 12.43 min, 99.18 max)"),
            Some(96.00)
        );
        // Integer form.
        assert_eq!(parse_audio_bitrate("24 kbps"), Some(24.0));
        // Garbage rejected, not a crash.
        assert!(parse_audio_bitrate("").is_none());
        assert!(parse_audio_bitrate("garbage kbps").is_none());
    }

    #[test]
    fn existing_parsers_still_work() {
        // Smoke check that pre-existing variants weren't broken by the
        // reshuffle. One representative per category.
        assert!(matches!(parse("Synchronized"), Some(NrscEvent::Sync)));
        assert!(matches!(
            parse("Station name: KROQ-FM"),
            Some(NrscEvent::StationName(_))
        ));
        assert!(matches!(
            parse("SIG Service: type=audio number=1 name=KEGL HD1"),
            Some(NrscEvent::SigServiceAudio { number: 1, .. })
        ));
    }
}