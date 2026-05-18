//! Digital-signal processing helpers that sit alongside the audio pipeline.
//!
//! Currently exposes the [`spectrum`] module, which provides the FFT tap
//! consumed by the Spectrum / waterfall panel.

pub mod spectrum;

pub use spectrum::{SpectrumSnapshot, SpectrumTap, FFT_SIZE, WATERFALL_ROWS};
