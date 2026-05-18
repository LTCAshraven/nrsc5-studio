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

pub mod rtl;

pub use rtl::{RtlSdr, R820T_GAINS_TENTHS};

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
}

/// Return value of the per-frame callback in [`Sdr::run_stream`]. Returning
/// [`StreamControl::Stop`] signals the backend to cancel the stream; the
/// `run_stream` call will then unblock and return `Ok(())`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamControl {
    Continue,
    Stop,
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
}
