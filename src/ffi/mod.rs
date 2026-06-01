use crossbeam_channel::{unbounded, Receiver, Sender};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use thiserror::Error;

use crate::config::{GainMode, SdrTransport};
use crate::dsp::{AgcConfig, AgcController, AgcSnapshot, AgcStatus};
use crate::sdr::profile::DeviceProfile;
use crate::sdr::{GainCache, GainCacheEntry, GainCacheKey, IqBus, Sdr, SdrConfig, SdrError, StreamControl};

mod decoder;
use decoder::DecoderInstance;

// Phase 1: raw FFI bindings for libnrsc5. Consumed by `api` below.
#[allow(unused)]
pub(crate) mod nrsc5_sys;

// Phase 2: safe wrapper around `nrsc5_sys`. The only place in the
// crate (besides `nrsc5_sys` itself) where `unsafe` is allowed.
// Phase 3 cut `Nrsc5Process` over from the external `nrsc5.exe` child
// to the in-process `Nrsc5Session` defined here.
pub(crate) mod api;

use api::{Mode, Nrsc5ApiError, Nrsc5Session};

// -- Events -----------------------------------------------------------

#[derive(Debug, Clone)]
pub enum NrscEvent {
    LostDevice,
    /// Backend stream failed; carries the underlying Soapy error text
    /// for diagnostics/UI status.
    LostDeviceDetail(String),
    /// The nrsc5.exe child process closed its stdout pipe. Emitted from
    /// the PCM pump on EOF / BrokenPipe â€” covers external `taskkill`,
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
    /// Per-program audio bit rate from `Audio bit rate: 96.0 kbps â€¦`.
    /// Emitted on every occurrence (not just the first â€”
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
    /// event â€” stamped by `parse_stderr` from the per-child context.
    /// Used by the multi-decoder routing layer to attribute album art
    /// and station logo updates to the correct `programs[]` slot.
    LotFile {
        program: u32,
        lot: String,
        name: String,
    },
    /// XHDR event â€” param 0 = cover art, param 1 = station logo.
    /// `program` is the HD subchannel whose decoder produced this
    /// event; same routing role as on `LotFile`.
    Xhdr {
        program: u32,
        param: u32,
        lot: String,
    },
    StationName(String),
    /// Long-form station identifier from `Slogan: â€¦`. Sent by SIS
    /// every few seconds while synced; receivers display it alongside
    /// the call sign.
    Slogan(String),
    /// Free-text broadcaster message from `Message: â€¦`. Used for
    /// promos, "now playing on HD2", etc. â€” distinct from `Alert:`.
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
    /// `Audio program N: <MPS|SPSx>, type: <Music|Talk|â€¦>, sound experience: <Mono|Stereo|â€¦>`.
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
    /// Non-audio data service from `SIG Service: type=data number=N name=â€¦`.
    /// Inner `Component: â€¦` lines (mime, service_data_type) are not yet
    /// captured â€” added when the panel needs them.
    SigServiceData {
        number: u32,
        name: String,
    },
    /// Emergency alert text from `Alert: â€¦`. Empty alerts are dropped.
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
/// one shared SDR pipeline. Each decoder is one libnrsc5 session plus
/// its I/Q feeder thread, so the cost scales linearly; eight covers
/// every HD Radio station's advertised program count (HD1â€“HD8) with a
/// margin of safety. Default streaming behavior is single-decoder;
/// the user opts in to extras via the per-program decode toggle in
/// the HD grid.
pub const MAX_DECODERS: usize = 8;

#[derive(Debug, Error)]
pub enum Nrsc5Error {
    /// libnrsc5 returned a non-zero result from one of its API
    /// functions, or otherwise refused to initialize a session.
    /// Carries the underlying typed error from `super::api`.
    #[error("libnrsc5 error: {0}")]
    Api(#[from] Nrsc5ApiError),
    #[error("SDR backend error: {0}")]
    Sdr(#[from] SdrError),
    /// `add_decoder` / `set_active_speaker` called before any
    /// `start_piped` succeeded. The shared SDR + IqBus pipeline
    /// must be running before per-program decoders can be added.
    #[error("no piped session is active (call start_piped first)")]
    NotStarted,
    /// `add_decoder(program)` called for a program that's already
    /// being decoded. Idempotent failure â€” nothing was changed.
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

/// Marker for whether the most recent start drove the piped pipeline.
/// Kept as an `Option<LastStartMode>` field on [`Nrsc5Process`] so a
/// fresh process (or one that's only been stopped) can be retuned via
/// [`Nrsc5Process::retune`] without the caller having to track state.
/// The pre-0.5.0 `Usb` and `RtlTcp` variants were removed when the
/// legacy start paths were retired â€” the in-process piped pipeline is
/// now the only way Start runs.
#[derive(Debug, Clone)]
enum LastStartMode {
    Piped,
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
    /// `Stdio::null()` for stdout (audio is silently discarded â€”
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
    /// The modern `librtlsdr.dll` (osmocom â‰¥ 2022-01) handles
    /// `rtlsdr_close` after `rtlsdr_cancel_async` cleanly, so we
    /// open fresh on every Start and close fully on every Stop â€”
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
    /// Antenna name resolved (user choice or profile default) for the
    /// current/last piped stream. Preserved across `stop()` for the
    /// same reason as `last_sdr_args` so [`retune`](Self::retune) can
    /// reuse it. `None` until the first piped Start.
    last_antenna: Option<String>,
    /// Transport selection (local Soapy, SoapyRemote, native rtl_tcp)
    /// active for the current/last piped stream. Preserved across
    /// `stop()` so [`retune`](Self::retune) can re-open the same kind
    /// of backend. Defaults to `LocalSoapy` until the first piped
    /// Start has set it.
    last_transport: SdrTransport,
    /// Remote host:port for the current/last piped stream, when
    /// `last_transport` is a remote variant. `None` for `LocalSoapy`.
    /// Used by retune to rebuild the rtl_tcp / SoapyRemote connection
    /// without the caller having to re-supply the connection details.
    last_remote: Option<(String, u16)>,
    /// Frequency in MHz that was passed to the most recent
    /// `start_piped`. Preserved across `stop()` so `add_decoder`
    /// (which doesn't take a frequency arg â€” it inherits from the
    /// already-running SDR) can pass it through to libnrsc5's
    /// `set_frequency_hz`, keeping station-info events from
    /// secondary decoders reporting the right value. `None` until
    /// the first piped Start.
    last_frequency_mhz: Option<f32>,
    /// Per-frequency gain cache (Phase 3 of the v0.4.0 AGC overhaul).
    /// Loaded once at `Nrsc5Process::new` from
    /// [`crate::paths::gain_cache_path`]; survives across
    /// `start_piped` / `stop` / `retune` so warm tunes can short-circuit
    /// the AGC coarse search. Shared with the AGC driver thread via
    /// `Arc<Mutex<_>>` so it can record a fresh entry whenever AGC
    /// transitions to `Settled`. Wrapped in `Arc` so the field clone in
    /// thread-spawn paths is cheap.
    gain_cache: Arc<Mutex<GainCache>>,
    tx: Sender<NrscEvent>,
    rx: Receiver<NrscEvent>,
    aas_dir: PathBuf,
}

/// Append one line to the AGC trace log (Phase 2c). Best-effort:
/// open failure or write failure is silently ignored so the AGC
/// driver thread never blocks or panics on a disk hiccup. The file
/// is created on first call if missing; truncation happens via
/// [`agc_log_start`] at the top of each `start_piped` so each tune's
/// trace stands alone.
fn agc_log_append(line: &str) {
    let Some(path) = crate::paths::agc_trace_path() else {
        return;
    };
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(f, "{}", line);
    }
}

/// Truncate the AGC trace log and write a header for a new tune.
/// Called once at the top of `start_piped` (after gain cache lookup)
/// so the file always reflects the current run only â€” old runs are
/// overwritten by design. Best-effort: silently ignored on failure.
fn agc_log_start(header: &str) {
    let Some(path) = crate::paths::agc_trace_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&path)
    {
        let _ = writeln!(f, "{}", header);
    }
}

/// Translate one closed-loop AGC controller decision into the
/// corresponding gain-element write on the live SDR, observing the
/// device profile's sign convention and the element's reported range.
///
/// The AGC controller speaks in "tenths of dB of overall gain"
/// (matches the legacy librtlsdr convention from v0.2.x). Each device
/// has a different physical knob the controller should drive: RTL-SDR
/// has a single straight-gain `TUNER`, SDRplay has a `IFGR` (gain
/// reduction â€” *lower* is more gain), HackRF has a stepped `LNA`.
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
    let target = profile.agc_element;    let desired_db = profile.agc_tenths_to_element_db(action.new_tenths);

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
                "[agc] driver={} doesn't expose element {} â€” AGC disabled \
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
        let (tx, rx) = unbounded();
        let aas_dir = crate::paths::aas_temp_dir();
        let _ = std::fs::create_dir_all(&aas_dir);
        // Best-effort cache load. Missing / unreadable file yields an
        // empty cache; misbehavior is strictly a performance issue
        // (cold AGC search on next tune), never a correctness bug.
        let gain_cache = match crate::paths::gain_cache_path() {
            Some(p) => GainCache::load(&p),
            None => GainCache::new(),
        };
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
            last_antenna: None,
            last_transport: SdrTransport::LocalSoapy,
            last_remote: None,
            last_frequency_mhz: None,
            gain_cache: Arc::new(Mutex::new(gain_cache)),
            tx,
            rx,
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
    /// Only the piped path emits PCM through this sink â€” the legacy
    /// `start()` (USB direct) and `start_rtltcp()` paths still let
    /// `nrsc5.exe` drive libao itself.
    ///
    /// Also spawns the long-lived `SpeakerRouter` thread that drains
    /// per-decoder PCM rings into this sink. If a sink was previously
    /// installed, the prior router is shut down before the new one is
    /// spawned so we never have two routers competing for the sink.
    pub fn set_audio_sink(&mut self, sink: crate::audio::AudioSink) {
        // Shut down any pre-existing router first; safe even when
        // mid-stream because the router only forwards samples â€” the
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
    /// Does **not** change the active speaker â€” call
    /// `set_active_speaker(program)` afterwards to listen to it. The
    /// new decoder runs silently in the background until then; its
    /// PCM ring is drained-and-discarded by the router, which keeps
    /// CPU steady but doesn't ship samples to the cpal sink.
    ///
    /// Errors:
    /// * [`Nrsc5Error::NotStarted`] â€” no piped session is active.
    /// * [`Nrsc5Error::DecoderAlreadyActive`] â€” `program` is already
    ///   being decoded; idempotent (no state changed).
    /// * [`Nrsc5Error::DecoderCapReached`] â€” already at
    ///   [`MAX_DECODERS`]; tear one down first.
    /// * [`Nrsc5Error::Api`] â€” libnrsc5 refused to open or configure
    ///   the new session.
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

        // Inherit the frequency that `start_piped` was called with so
        // the secondary session's station-info events report the same
        // tune. Defaults to 0.0 in the rare case `start_piped` hasn't
        // been called (impossible given the `iq_bus.is_none()` check
        // above, but the field is `Option`-typed so handle it).
        let frequency_mhz = self.last_frequency_mhz.unwrap_or(0.0);

        // Additional decoders do NOT drive the AGC controller â€” only
        // the first decoder spawned by `start_piped` gets that
        // responsibility. Passing `None` skips the AGC tee in
        // `spawn_decoder`'s event callback.
        let bus = self
            .iq_bus
            .as_ref()
            .expect("iq_bus is_some, checked above")
            .clone();
        let decoder = self.spawn_decoder(program, &bus, frequency_mhz, None)?;

        // Register the new ring with the router. Do NOT auto-activate
        // it as speaker â€” caller decides via `set_active_speaker`.
        if let (Some(router), Some(ring)) =
            (self.speaker_router.as_ref(), decoder.pcm_ring.as_ref())
        {
            let _ = router.cmd_tx().send(crate::audio::SpeakerCmd::AddDecoder {
                program,
                ring: Arc::clone(ring),
            });
        }

        self.decoders.push(decoder);
        Ok(())
    }

    /// Tear down the decoder for `program`. Idempotent â€” returns
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
            feeder_thread,
            shutdown_tx,
            pcm_ring,
        } = self.decoders.remove(idx);

        // Detach this program's ring from the speaker router so the
        // router stops draining a soon-to-be-dropped ring. Idempotent
        // on the router's side. The ring itself is dropped when
        // `pcm_ring` goes out of scope at the end of this block.
        if let Some(router) = self.speaker_router.as_ref() {
            let _ = router
                .cmd_tx()
                .send(crate::audio::SpeakerCmd::RemoveDecoder(program));
        }
        if Some(program) == self.active_speaker {
            self.active_speaker = None;
        }
        let _ = pcm_ring;

        // Drop the per-decoder shutdown sender to wake the feeder
        // thread's `select!`. The feeder breaks its loop, drops its
        // owned `Nrsc5Session` (which runs `nrsc5_stop` +
        // `nrsc5_close` to join libnrsc5's worker), and emits the
        // final `ChildExited` event. The join below is fast
        // (typically <10 ms â€” bounded by `nrsc5_close` worker join).
        drop(shutdown_tx);
        let _ = feeder_thread.join();
        true
    }

    /// Route `program`'s decoded PCM to the speakers. The previous
    /// active speaker (if any) stays decoding in the background â€”
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
    /// is currently on the speakers â€” so the user can listen to HD2
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
    /// and writes the Ogg EOS page. Idempotent â€” safe to call when
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

    /// Wipe the on-disk per-frequency gain cache (Phase 3 of the
    /// v0.4.0 AGC overhaul). Used by the Tools menu "Clear gain
    /// cache\u2026" entry. Failure to persist is non-fatal \u2014 the in-memory
    /// cache is already cleared and the next save will overwrite the
    /// stale file. Returns the number of entries that were dropped so
    /// the UI can show a confirmation snackbar.
    pub fn clear_gain_cache(&self) -> usize {
        let mut dropped = 0;
        if let Ok(mut cache) = self.gain_cache.lock() {
            dropped = cache.len();
            cache.clear();
            if let Some(path) = crate::paths::gain_cache_path() {
                cache.save(&path);
            }
        }
        dropped
    }

    /// Number of entries currently in the gain cache (fresh + stale).
    /// Surfaces in the Tools menu as a parenthetical so the user can
    /// see whether "Clear gain cacheâ€¦" would do anything.
    pub fn gain_cache_len(&self) -> usize {
        self.gain_cache.lock().map(|c| c.len()).unwrap_or(0)
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
    /// next Start). Returns `Ok(())` for the no-op case too â€” callers
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

    /// Hot-apply a manual gain (tenths of dB) to the live SDR. Uses
    /// the same per-device gain-mapping path as the closed-loop AGC
    /// (`apply_agc_action`) so the value gets routed through the
    /// device profile's sign-flip / offset and clamped to the
    /// element's actual range. No-op when no piped stream is
    /// running â€” callers can still safely poke this on Manual-mode
    /// slider drags while idle; the value will be picked up at the
    /// next Start via the persisted `manual_gain_tenths`.
    ///
    /// Also updates `last_manual_gain_tenths` so the Tuner panel's
    /// "(restart stream to apply)" hint stays in sync â€” without this,
    /// dragging the slider while streaming would leave the hint stuck
    /// on even after the value matches the live device.
    pub fn set_manual_gain_tenths(&mut self, tenths: i32) -> Result<(), SdrError> {
        let sdr = match self.sdr.as_ref() {
            Some(s) => s,
            None => {
                // No live SDR; just record the desired value so the
                // next start_piped sees the updated mirror.
                self.last_manual_gain_tenths = Some(tenths);
                return Ok(());
            }
        };
        let profile = crate::sdr::profile::lookup(sdr.driver())
            .copied()
            .unwrap_or(crate::sdr::profile::RTLSDR);
        let action = crate::dsp::AgcAction {
            new_idx: 0,
            new_tenths: tenths,
            reason: "manual slider".to_string(),
        };
        let _ = apply_agc_action(sdr, &profile, &action);
        self.last_manual_gain_tenths = Some(tenths);
        Ok(())
    }

    /// Apply a frequency-correction PPM nudge to the live SDR. Same
    /// no-op-when-idle semantics as `set_sdr_gain_element`. Some
    /// backends (SDRplay) silently ignore this â€” see their `Sdr`
    /// trait impl for details.
    pub fn set_sdr_freq_correction_ppm(&self, ppm: f64) -> Result<(), SdrError> {
        match self.sdr.as_ref() {
            Some(sdr) => sdr.set_frequency_correction_ppm(ppm),
            None => Ok(()),
        }
    }

    /// Snapshot the live SDR's reported gain elements. Returns an
    /// empty `Vec` when no stream is running â€” the SDR Settings modal
    /// then falls back to an idle open-and-close to populate its
    /// sliders.
    pub fn sdr_gain_elements(&self) -> Vec<crate::sdr::GainElement> {
        self.sdr
            .as_ref()
            .map(|s| s.gain_elements())
            .unwrap_or_default()
    }

    /// Names of every antenna input the live SDR exposes. Empty when
    /// no stream is running, or when the live device only has a single
    /// (unnamed) input â€” the Tuner panel uses `len() > 1` as the gate
    /// for showing its antenna dropdown.
    pub fn sdr_antennas(&self) -> Vec<String> {
        self.sdr
            .as_ref()
            .map(|s| s.antennas())
            .unwrap_or_default()
    }

    /// Currently selected antenna name on the live SDR. `None` when
    /// no stream is running or the device doesn't expose antenna
    /// selection. The Tuner panel uses this to pre-select the right
    /// entry in its dropdown.
    pub fn active_antenna(&self) -> Option<String> {
        // Prefer the live device's reported value; fall back to the
        // last-resolved antenna so the UI still shows something
        // sensible during the brief window between `start_piped`
        // returning and the user clicking Stop.
        self.sdr
            .as_ref()
            .and_then(|s| s.antenna())
            .or_else(|| self.last_antenna.clone())
    }

    /// Short status label for the top bar. Reports the in-process
    /// libnrsc5 library version (lazy-loads the DLL on first call;
    /// returns the empty string if the load fails â€” the GUI just
    /// shows "ready" without a version suffix in that case).
    pub fn version(&self) -> String {
        let v = Nrsc5Session::library_version();
        if v.is_empty() {
            "libnrsc5".to_string()
        } else {
            format!("libnrsc5 {v}")
        }
    }

    pub fn aas_dir(&self) -> &std::path::Path {
        &self.aas_dir
    }

    /// PID of the running nrsc5 process. Phase 3 cutover (0.5.0)
    /// retired the external `nrsc5.exe` child in favor of in-process
    /// `libnrsc5` calls, so there's no separate PID to report
    /// anymore. Always returns `None`; callers that previously
    /// displayed a PID in the status bar can fall back to
    /// `version()` for a "decoder is alive" indicator.
    pub fn pid(&self) -> Option<u32> {
        None
    }

    /// Internal helper: open one `Nrsc5Session`, install event +
    /// PCM callbacks, start it, and spawn the I/Q feeder thread that
    /// owns the session for its lifetime. Used by both `start_piped`
    /// (the first decoder) and `add_decoder` (additional decoders
    /// against the same shared IqBus). Returns the populated
    /// [`DecoderInstance`] ready to be pushed into `self.decoders`.
    ///
    /// `agc_handle` is the controller cloned by `start_piped` for
    /// the **first** decoder only â€” its event callback tees MER /
    /// Sync events into the controller. Additional decoders pass
    /// `None` (only the primary decoder drives AGC; secondary
    /// decoders' events would just duplicate the signal).
    ///
    /// The event callback runs on libnrsc5's worker thread, so it
    /// must be `Send + Sync + 'static`; this is enforced at the
    /// closure type. The PCM sink filters by program so a session
    /// that happens to decode multiple programs only delivers the
    /// expected subchannel's audio to this decoder's ring (matches
    /// the previous nrsc5.exe `-r - <program>` filter behavior).
    fn spawn_decoder(
        &self,
        program: u32,
        iq_bus: &Arc<IqBus>,
        frequency_mhz: f32,
        agc_handle: Option<Arc<Mutex<AgcController>>>,
    ) -> Result<DecoderInstance, Nrsc5Error> {
        // Build + configure the session. All configuration calls must
        // precede `start`; the session moves into the feeder thread
        // immediately after `start`, so this is the only chance.
        let mut session = Nrsc5Session::open_pipe()?;
        session.set_mode(Mode::Fm)?;
        session.set_frequency_hz(frequency_mhz * 1_000_000.0)?;

        // Event callback. Three jobs:
        //   1. Rewrite events that carry a stale `program` field
        //      (`LotFile`) so the per-decoder routing layer sees the
        //      right subchannel. libnrsc5's LOT events don't carry a
        //      program identifier; api.rs hardcodes 0 as a placeholder.
        //   2. Tee MER / Sync events into the AGC controller (only on
        //      the primary decoder).
        //   3. Forward the (possibly rewritten) event to the shared
        //      `self.tx` channel for the app's event-loop consumer.
        let event_tx = self.tx.clone();
        let agc_cb = agc_handle.clone();
        session.set_event_callback(move |ev| {
            // Rewrite events that need this decoder's program number.
            let ev = match ev {
                NrscEvent::LotFile { program: _, lot, name } => {
                    NrscEvent::LotFile { program, lot, name }
                }
                other => other,
            };
            // Tee into AGC. on_event filters internally to the event
            // variants it cares about, so passing everything is cheap.
            if let Some(handle) = agc_cb.as_ref() {
                if let Ok(mut ctrl) = handle.lock() {
                    ctrl.on_event(&ev);
                }
            }
            let _ = event_tx.send(ev);
        })?;

        // PCM sink. Allocates the per-decoder ring buffer and pushes
        // every decoded chunk into it (filtered by program â€” see
        // module doc on multi-program sessions). Also emits the
        // one-shot `AudioStarted` event on the first chunk so the
        // app's "audio started in Xs" status message fires at the
        // moment audio actually begins flowing rather than when
        // libnrsc5 first logs a bit-rate line.
        //
        // `audio_sink_installed` controls whether we allocate a ring
        // at all: in headless tests there's no sink to drain into, so
        // we skip both the ring and the PCM callback (libnrsc5 still
        // decodes audio internally but nothing observes it).
        let audio_sink_installed = self.audio_sink.is_some();
        let pcm_ring = if audio_sink_installed {
            Some(Arc::new(crate::audio::PcmRing::new()))
        } else {
            None
        };
        if let Some(ring) = pcm_ring.as_ref() {
            let ring_for_cb = Arc::clone(ring);
            let audio_started_flag = Arc::new(AtomicBool::new(false));
            let audio_started_tx = self.tx.clone();
            session.set_pcm_sink(move |pcm_program, samples| {
                // libnrsc5 decodes ALL programs present in the signal.
                // Filter to the one this decoder claims so multiple
                // decoders on the same SDR don't fight over rings.
                if pcm_program != program {
                    return;
                }
                if !audio_started_flag.swap(true, Ordering::Relaxed) {
                    let _ = audio_started_tx.send(NrscEvent::AudioStarted { program });
                }
                ring_for_cb.push(samples);
            })?;
        }

        // Start the worker thread inside libnrsc5. After this call
        // returns, the event + PCM callbacks may fire at any time
        // from libnrsc5's internal thread.
        session.start();

        // Subscribe to the shared bus before we hand the session off
        // to the feeder thread. Capacity 64 â‰ˆ 100 ms of buffer at
        // 1.488 Msps CS16 in ~4 KB chunks â€” enough to absorb consumer
        // scheduling jitter without back-pressuring the SDR.
        let bus_rx = iq_bus.subscribe(64);
        let (shutdown_tx, shutdown_rx) = crossbeam_channel::bounded::<()>(1);
        let child_exit_tx = self.tx.clone();
        let feeder_thread = std::thread::spawn(move || {
            // Take ownership of the session in this thread so the C
            // library's `pipe_samples_cu8` is only ever called from
            // one thread (matches the libnrsc5 single-driver
            // contract). On exit (any path), `session` drops here,
            // which runs `nrsc5_stop` + `nrsc5_close` to join the
            // worker thread before any captured callback state is
            // freed.
            let session = session;
            'pump: loop {
                crossbeam_channel::select! {
                    recv(bus_rx) -> msg => {
                        match msg {
                            Ok(payload) => {
                                if session.pipe_samples_cu8(&payload).is_err() {
                                    // libnrsc5 refused the chunk (e.g.
                                    // after an internal `nrsc5_stop`).
                                    // Treat it as terminal â€” the loop
                                    // exits and the session drops.
                                    break 'pump;
                                }
                            }
                            // Bus was shut down (`IqBus::shutdown` from
                            // the SDR pump on backend failure or our
                            // own `stop()` path).
                            Err(_) => break 'pump,
                        }
                    }
                    recv(shutdown_rx) -> _ => {
                        // Per-decoder remove: `shutdown_tx` was
                        // dropped from the outside (`remove_decoder`).
                        break 'pump;
                    }
                }
            }
            // Explicit drop so the order is obvious from the source:
            // session â†’ nrsc5_stop â†’ nrsc5_close â†’ free callback ctx.
            drop(session);
            // Notify the app that this decoder ended. The app's
            // `ChildExited` handler treats it as pipeline-fatal only
            // when no other decoders survive, so the `stop()` path
            // and `remove_decoder` both produce this event without
            // spurious "device lost" status changes.
            let _ = child_exit_tx.send(NrscEvent::ChildExited);
        });

        Ok(DecoderInstance {
            program,
            feeder_thread,
            shutdown_tx,
            pcm_ring,
        })
    }

    /// Start with the SDR driven in-process: open the device, retune,
    /// and bring up an in-process libnrsc5 session fed from our I/Q
    /// pump via the shared [`IqBus`].
    ///
    /// This is the v0.2.0 "piped" path that unblocks the waterfall and
    /// the in-process AGC. It is now the **only** Start path; the
    /// pre-0.3.0 USB-direct and pre-0.5.0 `nrsc5 -H` paths were retired
    /// when the explicit `sdr.transport` field landed in config. The
    /// SDR is opened fresh on each Start and closed fully on each Stop
    /// (the modern librtlsdr.dll handles this cleanly).
    pub fn start_piped(
        &mut self,
        frequency_mhz: f32,
        program: u32,
        transport: SdrTransport,
        sdr_args: &str,
        remote: Option<(&str, u16)>,
        ppm_correction: f64,
        gain_mode: GainMode,
        manual_gain_tenths: i32,
        antenna: Option<String>,
    ) -> Result<(), Nrsc5Error> {
        self.stop();
        while self.rx.try_recv().is_ok() {}

        // Open + configure a fresh SDR for this stream. The initial
        // gain depends on which mode we're operating in:
        //
        //   * `Auto`        â€” leave gain alone here; the AGC controller
        //                    constructed below will set the starting
        //                    value via its own `initial_action`.
        //   * `Manual`      â€” force manual gain mode at the user-chosen
        //                    value. Snapping happens inside the SDR.
        //   * `HardwareAgc` â€” leave gain alone so the R820T2's hardware
        //                    AGC stays in charge (librtlsdr's default).
        let initial_gain_tenths = match gain_mode {
            GainMode::Auto => None,
            GainMode::Manual => Some(manual_gain_tenths),
            GainMode::HardwareAgc => None,
        };
        // Pick the backend based on transport:
        //   * LocalSoapy / SoapyRemote â†’ open via SoapySDR using the
        //     composed args string. For SoapyRemote the args already
        //     encode `driver=remote,remote=<host>:<port>`.
        //   * RtlTcpRemote â†’ open a native TCP connection to an
        //     `rtl_tcp` server. Bypasses SoapySDR entirely so the
        //     remote machine doesn't need a SoapyRemote server.
        let sdr: Arc<dyn Sdr> = match transport {
            SdrTransport::LocalSoapy | SdrTransport::SoapyRemote => {
                let soapy = crate::sdr::SoapySdr::open(sdr_args)?;
                // Apply config-driven PPM correction. Zero is the common
                // case; backends that don't expose runtime PPM return
                // Ok(()) silently.
                let _ = soapy.set_frequency_correction_ppm(ppm_correction);
                soapy.configure(&SdrConfig {
                    center_freq_hz: (frequency_mhz * 1_000_000.0) as u32,
                    sample_rate_sps: 1_488_375,
                    ppm_correction: 0,
                    direct_sampling: 0,
                    initial_gain_tenths,
                    antenna: antenna.clone(),
                })?;
                Arc::new(soapy)
            }
            SdrTransport::RtlTcpRemote => {
                let (host, port) = remote.ok_or_else(|| {
                    Nrsc5Error::Sdr(crate::sdr::SdrError::RtlTcpConnect {
                        addr: "<unset>".to_string(),
                        reason: "transport=rtl_tcp_remote but no host/port supplied"
                            .to_string(),
                    })
                })?;
                let rtl = crate::sdr::RtlTcpSdr::open(host, port)?;
                rtl.configure(&SdrConfig {
                    center_freq_hz: (frequency_mhz * 1_000_000.0) as u32,
                    sample_rate_sps: 1_488_375,
                    // PPM is rounded to an integer inside the rtl_tcp
                    // backend; pass it via the field rather than a
                    // separate call so configure() handles the no-op
                    // case at zero.
                    ppm_correction: ppm_correction.round() as i32,
                    direct_sampling: 0,
                    initial_gain_tenths,
                    antenna: None,
                })?;
                Arc::new(rtl)
            }
        };

        // ----- AGC controller (only in `Auto` mode) -----------------
        // Build the controller, apply its initial gain to the SDR, and
        // wrap in `Arc<Mutex<_>>` so both the decoder's event callback
        // (tee for MER / Sync) and the AGC driver thread (tick + apply)
        // can share it. In `Manual` / `HardwareAgc` we leave these
        // `None` and skip the driver thread entirely â€” the dongle's
        // gain is set once by `configure` above and never touched again
        // for this stream.
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
        let (agc, agc_stderr_handle, cache_hit_logged) = if gain_mode == GainMode::Auto {
            // Build the controller with the profile's per-driver start
            // gain. The global `AgcConfig::default()` aims at the RTL-SDR
            // sweet spot (19.7 dB); SDRplay and HackRF override it via
            // `default_agc_initial_tenths` so they land closer to their
            // own HD lock range on first tick. Each profile also
            // picks the initial search direction â€” RTL-SDR walks
            // DOWN from 19.7 dB (over-clip caution), SDRplay walks UP
            // from 39 dB (HD sweet spot is above the start, not below).
            // v0.4.0 also wires the profile's coarse probe set into
            // the controller so the Coarse phase visits each family's
            // middle-biased sweet-spot points before falling into Â±1
            // Fine hill-climb around the winner.
            let mut agc_cfg = AgcConfig::default();
            agc_cfg.initial_tenths = profile.default_agc_initial_tenths;
            agc_cfg.initial_direction = profile.default_agc_initial_direction;
            agc_cfg.coarse_probe_tenths = profile.coarse_probe_tenths;
            // Phase 3 gain-cache lookup. The key is built from the
            // about-to-be-tuned freq + driver + active antenna + the
            // PPM correction in use. A hit overrides the profile's
            // default initial gain with the previously-settled value
            // and flips the controller into Fine-from-start so the
            // coarse search is skipped entirely (~3 s warm tune vs
            // ~10â€“15 s cold). The trust-but-verify floor is set 3 dB
            // below the previously-observed MER so a marginal
            // station doesn't get held to the production 18 dB
            // target it could never reach again.
            let cache_key = GainCacheKey::new(
                (frequency_mhz * 1_000_000.0) as u32,
                sdr.driver(),
                antenna.clone(),
                ppm_correction as f32,
            );
            // Phase 2c: open a fresh trace log for this tune so the
            // user can tail %LOCALAPPDATA%\nrsc5-studio\agc-trace.log
            // and watch the controller live regardless of how the exe
            // is launched (the GUI subsystem detaches stdio).
            agc_log_start(&format!(
                "[agc] === new tune: {:.1} MHz driver={:?} antenna={:?} ppm={:.2} ===",
                frequency_mhz, cache_key.driver, antenna, ppm_correction,
            ));
            let cache_hit_logged = match self.gain_cache.lock() {
                Ok(cache) => match cache.lookup(&cache_key) {
                    Some(entry) => {
                        agc_cfg.initial_tenths = entry.gain_tenths;
                        // Floor at 5 dB so a deeply-marginal cached
                        // entry doesn't make the verify pass settle
                        // on garbage.
                        agc_cfg.mer_target_db = (entry.best_mer_db - 3.0).max(5.0);
                        agc_cfg.seeded_from_cache = true;
                        let msg = format!(
                            "[agc] cache HIT for {:.1} MHz on {:?}: \
                             starting at {:.1} dB (cached MER {:.1}, \
                             verify floor {:.1})",
                            frequency_mhz,
                            cache_key.driver,
                            entry.gain_tenths as f32 / 10.0,
                            entry.best_mer_db,
                            agc_cfg.mer_target_db,
                        );
                        eprintln!("{}", msg);
                        agc_log_append(&msg);
                        true
                    }
                    None => {
                        let msg = format!(
                            "[agc] cache MISS for {:.1} MHz on {:?}: \
                             running fresh coarse-then-fine search",
                            frequency_mhz, cache_key.driver,
                        );
                        eprintln!("{}", msg);
                        agc_log_append(&msg);
                        false
                    }
                },
                Err(_) => false, // poisoned mutex â€” treat as cache miss
            };
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
            (Some(agc), Some(stderr_handle), cache_hit_logged)
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
            (None, None, false)
        };
        let _ = cache_hit_logged; // diagnostic side effect already emitted

        // I/Q fan-out bus. One producer (the SDR pump thread spawned
        // below), zero-N consumers (decoders subscribe inside
        // `spawn_decoder`). Built first so the decoder spawned next
        // can subscribe before the SDR thread starts publishing.
        let bus = Arc::new(IqBus::new());

        // Spawn the first decoder. Owns the libnrsc5 session + the
        // feeder thread that pumps I/Q from `bus` into it. The event
        // callback tees MER / Sync into the AGC controller (if any)
        // and forwards every translated event to `self.tx`. The PCM
        // callback pushes decoded samples into a fresh `PcmRing`
        // attached to the returned `DecoderInstance.pcm_ring`.
        let decoder = self.spawn_decoder(program, &bus, frequency_mhz, agc_stderr_handle.clone())?;
        let pcm_ring = decoder.pcm_ring.clone();

        // I/Q source pump. Runs `run_stream`, feeds the spectrum tap,
        // and publishes raw bytes onto the bus. On exit (clean
        // cancel, USB unplug, etc.) calls `bus.shutdown()` so every
        // subscriber (the feeder thread above, plus any extra
        // decoders added via `add_decoder`) sees `Disconnected` on
        // `recv` and exits cleanly.
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
                // Spectrum tap first â€” it's cheap (and internally
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
            // Tear down every subscriber so each decoder's feeder
            // thread wakes from its blocking `recv` with
            // `Err(RecvError)` and exits. Idempotent; safe even if a
            // subscriber already pruned itself.
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

        // Hand the new ring to the speaker router and make this
        // decoder the active speaker. Both commands are sent over
        // the long-lived command channel set up in `set_audio_sink`;
        // the router applies them on its next tick. Skipped when no
        // audio sink is installed (no router, no ring). Also drop
        // any audio currently queued in the cpal sink so the next
        // Start doesn't replay a fraction of a second of stale audio
        // from the previous session.
        if let (Some(router), Some(ring)) = (self.speaker_router.as_ref(), pcm_ring.as_ref()) {
            if let Some(sink) = self.audio_sink.as_ref() {
                sink.clear();
            }
            let tx = router.cmd_tx();
            let _ = tx.send(crate::audio::SpeakerCmd::AddDecoder {
                program,
                ring: Arc::clone(ring),
            });
            let _ = tx.send(crate::audio::SpeakerCmd::SetActive(program));
            self.active_speaker = Some(program);
        }

        self.decoders.push(decoder);
        self.iq_thread = Some(iq_thread);
        self.iq_bus = Some(bus);
        self.sdr = Some(sdr);
        self.last_mode = Some(LastStartMode::Piped);
        self.last_gain_mode = Some(gain_mode);
        self.last_manual_gain_tenths = Some(manual_gain_tenths);
        self.last_sdr_args = Some(sdr_args.to_string());
        self.last_ppm = Some(ppm_correction);
        self.last_antenna = antenna;
        self.last_transport = transport;
        self.last_remote = remote.map(|(h, p)| (h.to_string(), p));
        self.last_frequency_mhz = Some(frequency_mhz);

        // ----- AGC driver thread (only when AGC is active) ----------
        // Ticks the controller every ~500 ms and applies any gain
        // change it asks for via the shared SDR Arc. Sends an
        // `AgcDecision` event so the UI's "last changed" timestamp
        // matches the moment of the real FFI call. Skipped entirely in
        // `Manual` / `HardwareAgc` modes â€” there's no controller to
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
            // Phase 3: cache write-back. The driver thread watches for
            // the Probing -> Settled transition and records the
            // settled gain to disk under the same key that was looked
            // up in `start_piped` above. Clone everything the closure
            // needs by value so it stays `'static`.
            let cache_for_driver = Arc::clone(&self.gain_cache);
            let cache_key_for_driver = GainCacheKey::new(
                (frequency_mhz * 1_000_000.0) as u32,
                sdr_for_agc.driver(),
                self.last_antenna.clone(),
                ppm_correction as f32,
            );
            let cache_path_for_driver = crate::paths::gain_cache_path();
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
                // Track the previous-tick AGC status so we can detect
                // the exact moment the controller flips from Probing
                // (or any non-Settled) into Settled. Recording on the
                // transition (not every tick while Settled) means the
                // cache file gets written once per converged tune.
                let mut prev_status = AgcStatus::Probing;
                while !agc_stop_for_driver.load(Ordering::Relaxed) {
                    std::thread::sleep(std::time::Duration::from_millis(tick_ms));
                    if agc_stop_for_driver.load(Ordering::Relaxed) {
                        break;
                    }
                    // Lock briefly to extract any pending action +
                    // current status; release the lock BEFORE the
                    // FFI call so the stderr-parser thread can keep
                    // feeding events without contention.
                    let (action, snap) = match agc_for_driver.lock() {
                        Ok(mut ctrl) => {
                            let action = ctrl.tick();
                            let snap = ctrl.snapshot();
                            (action, snap)
                        }
                        Err(_) => break, // mutex poisoned â€” give up gracefully
                    };
                    if let Some(action) = action {
                        // Phase 2c: per-action trace. Mirrored to
                        // stderr (for the rare case the user got
                        // stdio attached) and to the AGC log file
                        // (the reliable channel â€” read with
                        // `Get-Content -Wait %LOCALAPPDATA%\nrsc5-studio\agc-trace.log`).
                        let best_str = snap
                            .best_mer
                            .map(|m| format!("{:.2}", m))
                            .unwrap_or_else(|| "n/a".to_string());
                        let line = format!(
                            "[agc] phase={:?} status={:?} probes={} \
                             gain={:.1}dB(idx {}) best={:.1}dB(idx {}, mer {}) \
                             :: {}",
                            snap.phase,
                            snap.status,
                            snap.probes_done,
                            action.new_tenths as f32 / 10.0,
                            action.new_idx,
                            snap.best_tenths as f32 / 10.0,
                            snap.best_idx,
                            best_str,
                            action.reason
                        );
                        eprintln!("{}", line);
                        agc_log_append(&line);
                        let _ = apply_agc_action(&sdr_for_agc, &agc_profile, &action);
                        let _ = agc_tx.send(NrscEvent::AgcDecision {
                            tenths: action.new_tenths,
                            reason: action.reason,
                        });
                    }
                    // Detect the Probing -> Settled edge and write
                    // the converged gain back to the cache. Skipped
                    // when the controller bailed (the entry from a
                    // previous successful tune stays valid). We
                    // require a finite `best_mer` so a controller
                    // that settled via the stability shortcut with
                    // no MER observation never poisons the cache.
                    if prev_status != AgcStatus::Settled
                        && snap.status == AgcStatus::Settled
                    {
                        if let Some(mer) = snap.best_mer {
                            let entry = GainCacheEntry {
                                gain_tenths: snap.current_tenths,
                                best_mer_db: mer,
                                recorded_at: std::time::SystemTime::now(),
                            };
                            if let Ok(mut cache) = cache_for_driver.lock() {
                                cache.record(cache_key_for_driver.clone(), entry);
                                if let Some(ref p) = cache_path_for_driver {
                                    cache.save(p);
                                }
                            }
                            let msg = format!(
                                "[agc] SETTLED at {:.1} dB (idx {}, best MER {:.2}); \
                                 cache write-back complete",
                                snap.current_tenths as f32 / 10.0,
                                snap.current_idx,
                                mer,
                            );
                            eprintln!("{}", msg);
                            agc_log_append(&msg);
                        } else {
                            let msg = format!(
                                "[agc] SETTLED at {:.1} dB (idx {}); no MER reading, \
                                 cache NOT written",
                                snap.current_tenths as f32 / 10.0,
                                snap.current_idx,
                            );
                            eprintln!("{}", msg);
                            agc_log_append(&msg);
                        }
                    } else if prev_status == AgcStatus::Probing
                        && snap.status == AgcStatus::Bailed
                    {
                        let msg = format!(
                            "[agc] BAILED â€” gain restored to {:.1} dB (idx {}, best MER {})",
                            snap.current_tenths as f32 / 10.0,
                            snap.current_idx,
                            snap.best_mer
                                .map(|m| format!("{:.2}", m))
                                .unwrap_or_else(|| "n/a".to_string())
                        );
                        eprintln!("{}", msg);
                        agc_log_append(&msg);
                    }
                    prev_status = snap.status;
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
    /// For piped mode this fully releases the SDR â€” the LED on the
    /// dongle goes off, the USB device is unclaimed, and the next
    /// Start (or a switch to USB / rtl_tcp) starts from scratch. For
    /// the legacy USB and rtl_tcp paths this is a no-op for the SDR
    /// state (nrsc5 owns it directly there).
    pub fn stop(&mut self) {
        // Signal the AGC driver thread to stop first â€” it borrows the
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
        // `Sender` â€” so the stdin pump (joined next) and any Phase 3
        // decoders will wake from `recv` with `Err(RecvError)`.
        if let Some(handle) = self.iq_thread.take() {
            let _ = handle.join();
        }
        // Tear down every running decoder. With multi-decode active
        // there can be more than one (the user may have opted in to
        // extras via `add_decoder`). The shared SDR pump above has
        // already called `bus.shutdown()`, so every decoder's feeder
        // thread has woken from its `select!` with `Err(RecvError)`
        // on the bus arm and is about to exit. Dropping
        // `shutdown_tx` here is idempotent (the bus path already
        // tripped the feeder); we do it anyway so the teardown is
        // robust to the rare case where the bus shutdown raced with
        // a manual `set_active_speaker` / similar.
        //
        // The session itself drops inside the feeder thread, which
        // runs `nrsc5_stop` + `nrsc5_close` to join libnrsc5's
        // worker before this `join()` returns.
        let drained: Vec<DecoderInstance> = self.decoders.drain(..).collect();
        let any_decoder = !drained.is_empty();
        for decoder in drained {
            let DecoderInstance {
                program,
                feeder_thread,
                shutdown_tx,
                pcm_ring,
            } = decoder;
            // Detach this program's ring from the speaker router so
            // the router stops draining samples from a soon-to-be-
            // dropped ring. Idempotent on the router's side. The
            // ring itself is dropped when `pcm_ring` goes out of
            // scope at the end of this block.
            if let Some(router) = self.speaker_router.as_ref() {
                let _ = router.cmd_tx().send(
                    crate::audio::SpeakerCmd::RemoveDecoder(program),
                );
            }
            if Some(program) == self.active_speaker {
                self.active_speaker = None;
            }
            let _ = pcm_ring;
            // Drop the per-decoder shutdown sender + join the feeder
            // thread. The bus is already shut down so the feeder is
            // either at or near exit; this just waits for the
            // `nrsc5_close` inside the feeder's `drop(session)` to
            // join the worker thread.
            drop(shutdown_tx);
            let _ = feeder_thread.join();
        }
        // Bus is single-use per stream â€” drop it so a stale
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
        // Drop the SDR last â€” all Arc clones (the one held by
        // iq_thread is already gone) are released, refcount hits zero,
        // and `RtlSdr::Drop` runs `rtlsdr_close`. Safe on the modern
        // osmocom librtlsdr.dll (â‰¥ 2022-01).
        self.sdr = None;
        // Clear AGC handles last â€” all references (driver thread,
        // decoder event-callback tee) are gone by this point.
        self.agc = None;
        self.agc_stop = None;
    }

    /// Retune: stop the active stream and restart the piped pipeline
    /// at the new frequency / program. Reuses the gain mode, SDR args,
    /// PPM, and antenna captured by the previous `start_piped` call so
    /// the caller doesn't have to re-plumb config through every retune
    /// site.
    ///
    /// If `start_piped` has never been called (no `last_mode`), this is
    /// a no-op that returns `Ok(())` â€” historically the caller would
    /// fall back to a USB-direct start here, but that legacy path was
    /// removed in the 0.5.0 transport cleanup. The GUI guarantees a
    /// `start_piped` precedes any retune.
    pub fn retune(
        &mut self,
        frequency_mhz: f32,
        program: u32,
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
                let antenna = self.last_antenna.clone();
                let transport = self.last_transport;
                let remote = self.last_remote.clone();
                self.start_piped(
                    frequency_mhz,
                    program,
                    transport,
                    &args,
                    remote.as_ref().map(|(h, p)| (h.as_str(), *p)),
                    ppm,
                    gain_mode,
                    manual,
                    antenna,
                )
            }
            None => Ok(()),
        }
    }
}

impl Drop for Nrsc5Process {
    fn drop(&mut self) {
        self.stop();
    }
}
