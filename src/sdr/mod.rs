//! SDR backend abstraction.
//!
//! v0.2.0 inserts this app into the I/Q path so we can paint a waterfall
//! and drive closed-loop AGC. `trait Sdr` is the seam between "device that
//! produces I/Q" and the consumers (nrsc5's stdin via `src/ffi`, plus the
//! FFT/waterfall renderer).
//!
//! The first concrete backend is [`RtlSdr`] (this module's [`rtl`]
//! submodule), which dynamically loads `librtlsdr.dll` via `libloading`
//! — the same DLL we already ship for `nrsc5.exe`. Future backends
//! (`RtlTcp`, `SoapySdr`, …) will implement the same trait so the rest of
//! the app doesn't care which one is wired in.

pub mod soapy;
pub mod profile;
pub mod resampler;

pub use soapy::{DeviceInfo, SoapySdr};
pub use profile::{DeviceProfile, R820T_GAINS_TENTHS};

use thiserror::Error;

/// Errors any [`Sdr`] backend may surface to the caller. All variants are
/// intended to be user-visible (logged or shown in a status bar), so they
/// include enough context to be self-explanatory.
#[derive(Debug, Error)]
pub enum SdrError {
    #[error("librtlsdr.dll not found in bin/ or alongside the executable")]
    LibraryNotFound,
    #[error("librtlsdr.dll failed to load: {0}")]
    LoadFailed(String),
    #[error("librtlsdr is missing required symbol `{0}` (DLL ABI break?)")]
    SymbolMissing(&'static str),
    #[error("rtlsdr_open(index={0}) failed (device in use or missing?)")]
    OpenFailed(u32),
    #[error("librtlsdr call `{func}` returned error code {code}")]
    CallFailed { func: &'static str, code: i32 },
    #[error("operation requires an open device but the SDR is not configured")]
    NotOpen,
    #[error("stream is already running on this device")]
    AlreadyStreaming,
    // === SoapySDR backend (v0.3.0) ===
    #[error("SoapySDR could not open device `{args}`: {reason}")]
    OpenFailedArgs { args: String, reason: String },
    #[error("SoapySDR call `{func}` failed: {detail}")]
    SoapyCall {
        func: &'static str,
        detail: String,
    },
}

/// Return value of the per-frame callback in [`Sdr::run_stream`]. Returning
/// [`StreamControl::Stop`] signals the backend to cancel the stream; the
/// `run_stream` call will then unblock and return `Ok(())`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamControl {
    Continue,
    Stop,
}

/// One named gain stage exposed by a device. SoapySDR's mental model is
/// that every device has a stack of gain elements with semantic names
/// (`TUNER` for RTL-SDR, `IFGR`/`RFGR` for SDRplay, `LNA`/`VGA`/`AMP`
/// for HackRF), each with its own range and step. The SDR Settings
/// modal renders one widget per element; the AGC adapter (Phase 2.3)
/// drives exactly one of them, picked per-device via [`DeviceProfile`].
///
/// All units are dB. `step_db == 0.0` means continuous; non-zero means
/// the device snaps to that step internally (e.g. SDRplay's IFGR is
/// 1 dB steps; SDRplay's RFGR is an integer index that we still
/// surface as dB for UI consistency).
///
/// [`DeviceProfile`]: profile::DeviceProfile
#[derive(Debug, Clone)]
pub struct GainElement {
    /// Element name as Soapy reports it. Used as the key to
    /// [`Sdr::set_gain_element`] and for looking up the AGC target in
    /// the device profile.
    pub name: String,
    /// Inclusive minimum in dB.
    pub min_db: f64,
    /// Inclusive maximum in dB.
    pub max_db: f64,
    /// Granularity in dB. `0.0` = continuous (treat as 0.1 dB in UI).
    pub step_db: f64,
    /// Current value in dB at the moment `gain_elements` was called.
    /// The SDR Settings modal uses this to populate its initial widget
    /// state; the AGC adapter ignores it (it tracks its own state).
    pub current_db: f64,
}

/// One-shot configuration applied at the start of a stream. Values are
/// pulled from the existing tuning UI; the defaults below match what
/// `nrsc5` itself uses for FM HD Radio.
#[derive(Debug, Clone, Copy)]
pub struct SdrConfig {
    /// Center frequency in Hz (e.g. 97_100_000 for 97.1 MHz).
    pub center_freq_hz: u32,
    /// Sample rate in Hz. nrsc5 expects 1_488_375 sps for FM HD Radio.
    pub sample_rate_sps: u32,
    /// Frequency correction in parts-per-million. 0 = none.
    pub ppm_correction: i32,
    /// Direct-sampling mode. Always 0 for normal R820T2 tuner use; the
    /// only reason this is configurable is to defend against the dongle
    /// being left in I-ADC or Q-ADC mode by a previous app. See Spike 1
    /// findings in `/memories/session/spike1-plan.md`.
    pub direct_sampling: i32,
    /// Optional initial tuner gain in tenths of dB. `None` leaves the
    /// gain alone (caller will set it via [`Sdr::set_tuner_gain_tenths`]
    /// once they've inspected the gain table). When `Some`, the backend
    /// also forces manual gain mode.
    pub initial_gain_tenths: Option<i32>,
}

impl Default for SdrConfig {
    fn default() -> Self {
        Self {
            center_freq_hz: 97_100_000,
            sample_rate_sps: 1_488_375,
            ppm_correction: 0,
            direct_sampling: 0,
            initial_gain_tenths: None,
        }
    }
}

/// Abstract I/Q source. Implementors expose a small synchronous API plus
/// a blocking `run_stream` that pumps bytes into a user callback.
///
/// **Threading model:** callers typically run `run_stream` on a dedicated
/// worker thread (it blocks until the stream is cancelled). All other
/// methods may be called from any thread, including while `run_stream`
/// is running — that is the whole point of [`cancel_stream`] and
/// [`set_tuner_gain_tenths`] (the latter enables closed-loop AGC during
/// streaming). See Spike 2 findings for the librtlsdr-specific proof
/// that this is safe.
///
/// [`cancel_stream`]: Sdr::cancel_stream
/// [`set_tuner_gain_tenths`]: Sdr::set_tuner_gain_tenths
pub trait Sdr: Send + Sync {
    /// Apply the one-shot configuration. Called once before `run_stream`.
    fn configure(&self, cfg: &SdrConfig) -> Result<(), SdrError>;

    /// Discrete tuner gain steps in tenths of dB, ascending. For R820T2
    /// this is the 29-step table `[0, 9, 14, …, 480, 496]`. Callers MUST
    /// only pass values from this table to [`set_tuner_gain_tenths`];
    /// librtlsdr silently snaps off-table values to the nearest step and
    /// the closed-loop AGC needs to know exactly what was applied.
    fn gain_table_tenths(&self) -> &[i32];

    /// Set the tuner gain. `tenths` should be a value from
    /// [`gain_table_tenths`]. Safe to call mid-stream from any thread.
    ///
    /// [`gain_table_tenths`]: Sdr::gain_table_tenths
    fn set_tuner_gain_tenths(&self, tenths: i32) -> Result<(), SdrError>;

    /// Block this thread and pump I/Q bytes into `cb`. Returns when:
    ///
    /// * the callback returns [`StreamControl::Stop`], OR
    /// * another thread calls [`cancel_stream`], OR
    /// * the underlying SDR errors.
    ///
    /// The callback is invoked on the worker thread (the same thread
    /// that called `run_stream`) for each USB transfer (~16 KiB at the
    /// FM HD Radio sample rate, ~85 calls/s).
    ///
    /// [`cancel_stream`]: Sdr::cancel_stream
    fn run_stream(
        &self,
        cb: &mut dyn FnMut(&[u8]) -> StreamControl,
    ) -> Result<(), SdrError>;

    /// Request the in-flight `run_stream` (if any) to stop. Returns
    /// immediately; the worker thread will see the request on its next
    /// callback invocation and unblock the `run_stream` call.
    fn cancel_stream(&self) -> Result<(), SdrError>;

    /// Best-effort retune to a new center frequency mid-stream. Returns
    /// without error if the stream isn't running. Used by the GUI's
    /// retune flow once the SDR backend is wired into [`ffi`](crate::ffi).
    fn set_center_freq_hz(&self, hz: u32) -> Result<(), SdrError>;

    /// Snapshot of every named gain element this device exposes, with
    /// each element's range and current value in dB. The SDR Settings
    /// modal (Phase 3.3) renders one slider/combobox per entry; the
    /// AGC adapter (Phase 2.3) picks one element by name from the
    /// active [`DeviceProfile`].
    ///
    /// Returns an empty `Vec` for devices that don't expose named
    /// elements (none in the 0.3.0 supported set, but futureproof).
    /// Backends should treat this as a "best-effort, never errors"
    /// query — if a device refuses to enumerate, fall back to an empty
    /// Vec rather than panicking.
    fn gain_elements(&self) -> Vec<GainElement>;

    /// Set a specific named gain element in dB. The AGC adapter calls
    /// this via the profile's `agc_element` field; manual gain UI
    /// calls it for whatever the user is dragging. Devices that don't
    /// expose the named element should return `Ok(())` and silently
    /// ignore the call rather than erroring — keeps the AGC adapter
    /// from needing per-device branching.
    fn set_gain_element(&self, name: &str, value_db: f64) -> Result<(), SdrError>;

    /// Apply parts-per-million frequency correction. Zero is a no-op
    /// on every device. Some devices (SDRplay) don't expose a runtime
    /// PPM knob — implementors should return `Ok(())` in that case
    /// rather than erroring, so the config-driven apply path can stay
    /// uniform across backends.
    fn set_frequency_correction_ppm(&self, ppm: f64) -> Result<(), SdrError>;

    /// SoapySDR driver key for this device (`"rtlsdr"`, `"sdrplay"`,
    /// `"hackrf"`, ...). Used by the device-profile lookup so the
    /// AGC adapter can pick the right gain element, sign convention,
    /// and tick rate. Backends without a Soapy concept of driver
    /// should return a string that matches one of the known profiles
    /// — the legacy native [`RtlSdr`] returns `"rtlsdr"`, matching the
    /// `RTLSDR` profile so its AGC behavior is identical to Soapy's
    /// SoapyRTLSDR.
    fn driver(&self) -> &str;
}
