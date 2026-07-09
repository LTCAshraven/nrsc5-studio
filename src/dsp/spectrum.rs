//! FFT tap that produces a live spectrum + scrolling waterfall buffer for
//! the Spectrum dock panel.
//!
//! ## Pipeline
//!
//! The piped-SDR I/Q thread (see [`crate::ffi::Nrsc5Process::start_piped`])
//! holds an `Arc<SpectrumTap>` and, on every USB transfer batch, calls
//! [`SpectrumTap::feed`] with the raw 8-bit unsigned I/Q bytes that are
//! also being fed to `nrsc5.exe`. `feed` is throttled internally so the
//! work it does is bounded regardless of how often it's called.
//!
//! Each accepted batch is:
//! 1. converted to centered `Complex<f32>` (sample `s = (b - 127.5) / 127.5`),
//! 2. multiplied by a precomputed Hann window,
//! 3. transformed by a cached rustfft plan,
//! 4. magnitude-squared and converted to dB,
//! 5. fft-shifted so 0 Hz lands at the center bin,
//! 6. stored as the "latest spectrum" and pushed as a new row into the
//!    rolling waterfall (a flat `u8` ring buffer where each cell is the
//!    bin's intensity normalized to a tunable dB floor / ceiling).
//!
//! The dock panel reads the shared state under the same `Arc<Mutex<…>>`
//! on every paint via [`SpectrumTap::snapshot`].
//!
//! ## Why no separate worker thread
//!
//! A 1024-point complex FFT is ~50 µs on a modern CPU. At the throttled
//! ~30 FFTs/sec target the panel needs, that's < 0.2 % of one core, well
//! below the cost of moving samples across a channel. Keeping the work on
//! the I/Q thread also means there is exactly one writer to the shared
//! state, so the mutex is uncontended in practice (the panel locks for
//! a few microseconds at paint time).

use std::sync::{Arc, Mutex};
use std::time::Instant;

use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};

/// FFT length. 1024 points at 1.488 Msps = ~1453 Hz/bin which is plenty
/// of resolution to make the HD digital sidebands visible without the
/// noise floor looking choppy.
pub const FFT_SIZE: usize = 1024;

/// Number of historical FFT rows kept for the waterfall. At ~30 fps this
/// is ~8.5 seconds of history on screen.
pub const WATERFALL_ROWS: usize = 256;

/// Target FFT cadence in frames-per-second.
const TARGET_FPS: f32 = 30.0;

/// Default EMA alpha for spectrum-line smoothing when enabled.
/// 1.0 = no smoothing, lower values smooth more aggressively.
pub const DEFAULT_SMOOTHING_ALPHA: f32 = 0.5;

/// dB floor mapped to waterfall intensity 0. Anything below this is
/// rendered as deep blue.
const DB_FLOOR: f32 = -80.0;

/// dB ceiling mapped to waterfall intensity 255. Anything above this
/// is rendered as bright red. RTL-SDR magnitudes typically peak around
/// -10 dBFS for a strong FM carrier, so a 60 dB dynamic range from
/// floor to ceiling keeps the carrier saturated red and the noise floor
/// in the blue range.
const DB_CEIL: f32 = -10.0;

/// Shared snapshot returned to the panel each paint. All fields share
/// the same generation counter so the panel can detect new data and
/// skip work when nothing has changed.
pub struct SpectrumSnapshot {
    /// Latest spectrum, in dBFS, fft-shifted so index 0 is the lowest
    /// (most-negative) frequency and `FFT_SIZE - 1` is the highest.
    pub spectrum_db: Vec<f32>,
    /// `WATERFALL_ROWS × FFT_SIZE` u8 intensity grid (row-major, oldest
    /// row first after walking forward from `waterfall_head`).
    pub waterfall: Vec<u8>,
    /// Index of the OLDEST row in `waterfall`. The newest row is at
    /// `(waterfall_head + WATERFALL_ROWS - 1) % WATERFALL_ROWS`.
    pub waterfall_head: usize,
    pub sample_rate_sps: f32,
    pub center_freq_hz: f64,
    /// Bumped every time a new FFT row is computed. Lets the panel skip
    /// texture re-uploads when nothing has changed since the last paint.
    pub generation: u64,
}

impl Default for SpectrumSnapshot {
    fn default() -> Self {
        Self {
            spectrum_db: vec![DB_FLOOR; FFT_SIZE],
            waterfall: vec![0u8; WATERFALL_ROWS * FFT_SIZE],
            waterfall_head: 0,
            sample_rate_sps: 1_488_375.0,
            center_freq_hz: 0.0,
            generation: 0,
        }
    }
}

/// Public clone-cheap handle. Wraps the shared inner state in an `Arc`.
#[derive(Clone)]
pub struct SpectrumTap {
    inner: Arc<Mutex<TapInner>>,
}

struct TapInner {
    fft: Arc<dyn Fft<f32>>,
    /// rustfft scratch (size returned by `get_inplace_scratch_len`).
    scratch: Vec<Complex<f32>>,
    /// Hann window of length `FFT_SIZE`.
    window: Vec<f32>,
    /// Staging buffer for the next FFT (size `FFT_SIZE`). Filled by
    /// `feed`; consumed on each accepted batch.
    samples: Vec<Complex<f32>>,
    samples_have: usize,

    /// Last fftshifted dB spectrum (length `FFT_SIZE`).
    spectrum_db: Vec<f32>,
    /// Rolling intensity grid (length `WATERFALL_ROWS * FFT_SIZE`).
    waterfall: Vec<u8>,
    /// Oldest row index in `waterfall`. Newest row is one slot before
    /// this (mod `WATERFALL_ROWS`). Initial value 0.
    waterfall_head: usize,

    sample_rate_sps: f32,
    center_freq_hz: f64,
    /// EMA alpha for spectrum-line smoothing. 1.0 = raw trace.
    smoothing_alpha: f32,

    last_fft_at: Option<Instant>,
    /// Minimum wall-clock spacing between accepted FFT batches.
    min_period: std::time::Duration,

    generation: u64,
}

impl SpectrumTap {
    pub fn new(sample_rate_sps: f32) -> Self {
        let mut planner = FftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(FFT_SIZE);
        let scratch_len = fft.get_inplace_scratch_len();

        // Hann window: w[n] = 0.5 * (1 - cos(2*pi*n / (N-1))).
        let mut window = vec![0.0f32; FFT_SIZE];
        let denom = (FFT_SIZE as f32) - 1.0;
        for (n, w) in window.iter_mut().enumerate() {
            let theta = 2.0 * std::f32::consts::PI * (n as f32) / denom;
            *w = 0.5 * (1.0 - theta.cos());
        }

        let inner = TapInner {
            fft,
            scratch: vec![Complex::default(); scratch_len],
            window,
            samples: vec![Complex::default(); FFT_SIZE],
            samples_have: 0,
            spectrum_db: vec![DB_FLOOR; FFT_SIZE],
            waterfall: vec![0u8; WATERFALL_ROWS * FFT_SIZE],
            waterfall_head: 0,
            sample_rate_sps,
            center_freq_hz: 0.0,
            smoothing_alpha: DEFAULT_SMOOTHING_ALPHA,
            last_fft_at: None,
            min_period: std::time::Duration::from_secs_f32(1.0 / TARGET_FPS),
            generation: 0,
        };

        Self {
            inner: Arc::new(Mutex::new(inner)),
        }
    }

    /// Update the tuned center frequency so the panel can render correct
    /// X-axis labels. Cheap; safe to call on every retune.
    pub fn set_center_freq_hz(&self, hz: f64) {
        if let Ok(mut g) = self.inner.lock() {
            g.center_freq_hz = hz;
        }
    }

    /// Set EMA alpha used for spectrum-line smoothing.
    /// Values are clamped to [0.1, 1.0]. 1.0 disables smoothing.
    pub fn set_smoothing_alpha(&self, alpha: f32) {
        if let Ok(mut g) = self.inner.lock() {
            g.smoothing_alpha = alpha.clamp(0.1, 1.0);
        }
    }

    /// Feed raw interleaved 8-bit unsigned I/Q from the RTL-SDR.
    ///
    /// Throttled internally: most calls become a single timestamp check
    /// and return without doing FFT work. When the throttle elapses,
    /// up to `2 * FFT_SIZE` bytes from the *end* of `bytes` are consumed
    /// (newer samples are visually more meaningful than older ones).
    pub fn feed(&self, bytes: &[u8]) {
        if bytes.len() < 2 * FFT_SIZE {
            // Need at least one FFT's worth of samples in a single batch.
            // The RTL-SDR delivers ~16 KiB per USB transfer at the rates
            // we use, so this branch is hit only on small partial reads
            // at stream tear-down.
            return;
        }

        let now = Instant::now();
        let Ok(mut g) = self.inner.lock() else { return };

        if let Some(last) = g.last_fft_at {
            if now.duration_since(last) < g.min_period {
                return;
            }
        }
        g.last_fft_at = Some(now);

        // Take the last `2 * FFT_SIZE` bytes — most recent samples.
        let start = bytes.len() - 2 * FFT_SIZE;
        let slice = &bytes[start..];

        // Convert centered float, apply window, store into `samples`.
        // RTL-SDR I/Q is unsigned 8-bit with zero at 127.5.
        for n in 0..FFT_SIZE {
            let i_b = slice[2 * n] as f32;
            let q_b = slice[2 * n + 1] as f32;
            let i = (i_b - 127.5) * (1.0 / 127.5);
            let q = (q_b - 127.5) * (1.0 / 127.5);
            let w = g.window[n];
            g.samples[n] = Complex::new(i * w, q * w);
        }

        // Compute FFT in place. The `scratch` is sized at construction.
        // We can't borrow `samples` and `scratch` together if `fft` is
        // also borrowed from `g`, so we temporarily move them out.
        let mut samples = std::mem::take(&mut g.samples);
        let mut scratch = std::mem::take(&mut g.scratch);
        let fft = Arc::clone(&g.fft);
        fft.process_with_scratch(&mut samples, &mut scratch);
        g.samples = samples;
        g.scratch = scratch;

        // Magnitude → dB → fftshift. Naturally-ordered FFT output bin 0
        // is DC; bin N/2 is Nyquist; we want DC in the middle of the
        // displayed spectrum, so the lower half (indices N/2..N) goes
        // first, then the upper half (indices 0..N/2).
        let norm = 1.0 / (FFT_SIZE as f32);
        let half = FFT_SIZE / 2;
        let waterfall_head = g.waterfall_head;
        // Write the newest row over the slot that currently holds the
        // OLDEST row, then advance head by one (the oldest is now one
        // slot newer than it was).
        let row_offset = waterfall_head * FFT_SIZE;
        for shifted in 0..FFT_SIZE {
            let natural = if shifted < half {
                shifted + half
            } else {
                shifted - half
            };
            let c = g.samples[natural];
            let power = (c.re * c.re + c.im * c.im) * (norm * norm);
            // 10*log10 with a tiny floor to keep -inf out of the
            // arithmetic when a bin is exactly zero.
            let db = 10.0 * (power + 1e-30).log10();

            // EMA smoothing for the drawn spectrum line only.
            // Waterfall keeps raw values so history stays faithful.
            let a = g.smoothing_alpha;
            let prev = g.spectrum_db[shifted];
            g.spectrum_db[shifted] = a * db + (1.0 - a) * prev;

            let clamped = db.clamp(DB_FLOOR, DB_CEIL);
            let norm01 = (clamped - DB_FLOOR) / (DB_CEIL - DB_FLOOR);
            g.waterfall[row_offset + shifted] = (norm01 * 255.0).round() as u8;
        }

        g.waterfall_head = (waterfall_head + 1) % WATERFALL_ROWS;
        g.generation = g.generation.wrapping_add(1);
        g.samples_have = 0;
    }

    /// Snapshot the current state into the caller-provided buffer.
    /// The buffer is resized as needed so the panel can reuse it across
    /// frames and avoid per-paint allocations.
    pub fn snapshot_into(&self, out: &mut SpectrumSnapshot) {
        let Ok(g) = self.inner.lock() else { return };
        if out.spectrum_db.len() != g.spectrum_db.len() {
            out.spectrum_db.resize(g.spectrum_db.len(), DB_FLOOR);
        }
        out.spectrum_db.copy_from_slice(&g.spectrum_db);
        if out.waterfall.len() != g.waterfall.len() {
            out.waterfall.resize(g.waterfall.len(), 0);
        }
        out.waterfall.copy_from_slice(&g.waterfall);
        out.waterfall_head = g.waterfall_head;
        out.sample_rate_sps = g.sample_rate_sps;
        out.center_freq_hz = g.center_freq_hz;
        out.generation = g.generation;
    }
}
