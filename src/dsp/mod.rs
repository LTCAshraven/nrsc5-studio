//! Digital-signal processing helpers that sit alongside the audio pipeline.
//!
//! * [`spectrum`] — FFT tap consumed by the Spectrum / waterfall panel.
//! * [`agc`] — closed-loop AGC state machine driven by nrsc5's MER stream.

pub mod agc;
pub mod fm_analog;
pub mod spectrum;

pub use agc::{AgcAction, AgcConfig, AgcController, AgcSnapshot, AgcStatus, SearchPhase};
pub use fm_analog::FmDemod;
pub use spectrum::{SpectrumSnapshot, SpectrumTap, FFT_SIZE, WATERFALL_ROWS};
