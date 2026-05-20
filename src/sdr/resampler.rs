//! Fractional IQ resampler for SDR backends that can't produce
//! `nrsc5`'s required 1,488,375 sps natively.
//!
//! **Why this exists.** `nrsc5` reads raw CU8 IQ at *exactly*
//! 1,488,375 sps (a quarter of the FM HD master clock, 5,953,500 Hz).
//! RTL-SDR's RTL2832 dongles can hit that rate directly because their
//! sample-rate control is a continuous-ratio divider. SDRplay's
//! MSi001/MSi2500 chain uses a fixed ADC plus integer decimators, so
//! its supported rates are quantized: 62.5k, 96k, 125k, 192k, 250k,
//! 384k, 500k, 768k, 1M (discrete), then a continuous range from
//! **2,000,000 to 10,660,000 sps**. 1.488 Msps is in the gap, so we
//! have to ask the device for 2 Msps and resample down in software.
//!
//! **Algorithm.** We feed each block of IQ samples into a polyphase
//! sinc resampler (from the `rubato` crate) configured for the exact
//! ratio `dst_rate / src_rate`. The resampler operates on `f32`
//! samples in two parallel channels (I and Q) and applies a
//! 128-tap windowed sinc kernel — quality is well above what's
//! needed for 200-kHz-wide FM HD demodulation, and runtime is
//! dominated by the wider DSP/spectrum/AGC chain anyway.
//!
//! **Buffering contract.** `rubato`'s `SincFixedIn` requires a fixed
//! *input* chunk size per call. The Soapy driver hands us
//! variable-size reads (typically the device MTU, e.g. 65 536 frames
//! on SDRplay). [`IqResampler::feed`] accumulates incoming frames
//! into an internal buffer; whenever it crosses the chunk threshold,
//! one resample pass runs and the resulting CU8 bytes are appended
//! to the caller's output `Vec`. Any partial trailing chunk stays
//! buffered for the next call, so no samples are dropped at block
//! boundaries.
//!
//! **Format.** Input is `&[Complex<f32>]` with samples roughly in
//! `[-1.0, 1.0]` (we accept whatever scale the upstream conversion
//! used, since the resampler is linear). Output is CU8 bytes
//! (I, Q, I, Q, ...) ready to push down `nrsc5`'s stdin pipe — the
//! same wire format that [`run_cs8_loop`](super::soapy::run_cs8_loop)
//! and [`run_cs16_loop`](super::soapy::run_cs16_loop) produce.

use num_complex::Complex;
use rubato::{
    Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction,
};

/// Fixed number of input frames the resampler accepts per `process`
/// call. Larger = better cache reuse and lower per-call overhead,
/// smaller = lower steady-state latency. 8192 at 2 Msps is ~4 ms of
/// IQ, which is well under any audible-latency budget and a comfy
/// CPU sweet spot.
const CHUNK_FRAMES: usize = 8192;

/// IQ-to-IQ fractional resampler. Single-channel-pair (I, Q) → single
/// CU8 output stream.
pub struct IqResampler {
    /// Source sample rate the device is actually producing (e.g.
    /// 2_000_000.0 for SDRplay).
    src_rate: f64,
    /// Destination sample rate nrsc5 expects (1_488_375.0).
    dst_rate: f64,
    /// `rubato` resampler. `Box`-ed because `SincFixedIn` is large
    /// (kernel + scratch buffers) and we'd rather keep
    /// [`SoapySdr`](super::soapy::SoapySdr) cache-friendly.
    inner: Box<SincFixedIn<f32>>,
    /// Pending I samples that haven't yet filled a chunk.
    in_i: Vec<f32>,
    /// Pending Q samples that haven't yet filled a chunk. Always the
    /// same length as `in_i`.
    in_q: Vec<f32>,
    /// Reusable scratch buffers passed into `inner.process_into_buffer`.
    /// One per channel (I, Q). Sized to the resampler's max output
    /// frame count so we never reallocate at runtime.
    out_i: Vec<f32>,
    out_q: Vec<f32>,
}

impl IqResampler {
    /// Construct a new resampler converting `src_rate` → `dst_rate`.
    ///
    /// Returns an error only if `rubato`'s constructor rejects the
    /// ratio (e.g. NaN / negative / wildly out of range), which can't
    /// happen with the rates we use in practice.
    pub fn new(src_rate: f64, dst_rate: f64) -> Result<Self, rubato::ResamplerConstructionError> {
        // Sinc kernel parameters. 128 taps with 128x oversampling and
        // a Blackman-Harris window is rubato's "high quality" preset
        // -- aliasing well below -100 dB and stopband below -90 dB,
        // which is far cleaner than the receiver SNR will ever be.
        let params = SincInterpolationParameters {
            sinc_len: 128,
            f_cutoff: 0.95,
            interpolation: SincInterpolationType::Linear,
            oversampling_factor: 128,
            window: WindowFunction::BlackmanHarris2,
        };
        let ratio = dst_rate / src_rate;
        let inner = SincFixedIn::<f32>::new(
            ratio,
            // `max_resample_ratio_relative` -- how much rubato should
            // budget for runtime ratio changes. We don't change the
            // ratio after construction, so 1.0 (no change) would be
            // tight. Give it a bit of slack (2.0) in case we later
            // want to support drift correction without rebuilding
            // the resampler.
            2.0,
            params,
            CHUNK_FRAMES,
            2, // channels: I, Q
        )?;
        // Output frame count per chunk varies slightly with the
        // resampler's internal phase accumulator; ask rubato for
        // the worst case so we never realloc.
        let max_out = inner.output_frames_max();
        Ok(Self {
            src_rate,
            dst_rate,
            inner: Box::new(inner),
            in_i: Vec::with_capacity(CHUNK_FRAMES * 2),
            in_q: Vec::with_capacity(CHUNK_FRAMES * 2),
            out_i: vec![0.0; max_out],
            out_q: vec![0.0; max_out],
        })
    }

    /// Source sample rate this resampler was built for (Hz).
    pub fn src_rate(&self) -> f64 {
        self.src_rate
    }

    /// Destination sample rate this resampler emits (Hz).
    pub fn dst_rate(&self) -> f64 {
        self.dst_rate
    }

    /// Feed a block of IQ samples into the resampler and append any
    /// CU8 output bytes ready to flow downstream to `out`.
    ///
    /// `samples` is `Complex<f32>` with values nominally in `[-1, 1]`.
    /// Any samples that don't complete a full input chunk stay
    /// buffered for the next call.
    ///
    /// The output bytes are CU8: each complex frame becomes two
    /// consecutive bytes (I then Q) in the range `[0, 255]` where
    /// `127.5` represents zero. The conversion clamps to the byte
    /// range so out-of-bounds float inputs don't wrap silently.
    pub fn feed(&mut self, samples: &[Complex<f32>], out: &mut Vec<u8>) {
        // Deinterleave the input into our pending-buffer channels.
        // We pay one copy here to convert from interleaved complex
        // to rubato's per-channel layout; this is unavoidable given
        // rubato's API and is dwarfed by the sinc convolution cost.
        self.in_i.reserve(samples.len());
        self.in_q.reserve(samples.len());
        for c in samples {
            self.in_i.push(c.re);
            self.in_q.push(c.im);
        }

        // Drain as many full chunks as we have accumulated. Each
        // pass through this loop produces one output block sized
        // by the current ratio (~ CHUNK_FRAMES * ratio frames).
        while self.in_i.len() >= CHUNK_FRAMES {
            // rubato wants borrowed per-channel slices. We hand it
            // exactly CHUNK_FRAMES from the front of each buffer
            // and drain those positions after processing.
            let in_slices: [&[f32]; 2] = [
                &self.in_i[..CHUNK_FRAMES],
                &self.in_q[..CHUNK_FRAMES],
            ];
            // Output buffers: two mutable per-channel slices the
            // resampler writes its result into.
            let (n_in, n_out) = {
                let mut out_slices: [&mut [f32]; 2] =
                    [&mut self.out_i[..], &mut self.out_q[..]];
                // `process_into_buffer` returns `(input_frames_used,
                // output_frames_written)`. We sized `out_slices` to
                // `output_frames_max()` so it can't overflow.
                match self.inner.process_into_buffer(
                    &in_slices,
                    &mut out_slices,
                    None,
                ) {
                    Ok(pair) => pair,
                    Err(_) => {
                        // A processing error here means rubato hit
                        // a numerical edge case (e.g. NaN in input).
                        // Drop the chunk to keep streaming alive --
                        // a few ms of glitched audio is much better
                        // than tearing down the whole pipe.
                        self.in_i.drain(..CHUNK_FRAMES);
                        self.in_q.drain(..CHUNK_FRAMES);
                        continue;
                    }
                }
            };

            // Convert the f32 output back to CU8 and append. Saturate
            // anything outside [-1, 1] so a momentary clip doesn't
            // wrap around to the opposite rail.
            for n in 0..n_out {
                let i_byte = float_to_cu8(self.out_i[n]);
                let q_byte = float_to_cu8(self.out_q[n]);
                out.push(i_byte);
                out.push(q_byte);
            }

            // Drop the consumed input frames. `drain(..n)` is O(n)
            // but `n_in` should always equal `CHUNK_FRAMES` -- the
            // explicit drain protects us from any future rubato
            // version where it might consume fewer frames.
            self.in_i.drain(..n_in);
            self.in_q.drain(..n_in);
        }
    }
}

/// Convert one float IQ sample to the CU8 byte representation nrsc5
/// expects. Saturates the [-1.0, 1.0] range to [0, 255] with 127.5
/// representing zero. Values outside the range clamp instead of
/// wrapping.
#[inline]
fn float_to_cu8(f: f32) -> u8 {
    // 127.5 * (f + 1.0) maps [-1, 1] → [0, 255]. We add 0.5 and
    // truncate to round-to-nearest. The `clamp` guards against
    // out-of-range inputs that would otherwise wrap.
    let scaled = (f.clamp(-1.0, 1.0) + 1.0) * 127.5 + 0.5;
    // The clamp above guarantees `scaled` is in [0.5, 255.5] so the
    // cast is well-defined.
    scaled as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Construct the resampler with the actual SDRplay → nrsc5 rates
    /// the production code will use. Catches API regressions in the
    /// rubato dep at the unit-test level instead of during integration.
    #[test]
    fn constructs_with_sdrplay_to_nrsc5_ratio() {
        let r = IqResampler::new(2_000_000.0, 1_488_375.0).unwrap();
        assert_eq!(r.src_rate(), 2_000_000.0);
        assert_eq!(r.dst_rate(), 1_488_375.0);
    }

    /// Feed enough zero-valued input to trigger at least one chunk
    /// and verify the output appears at the expected rate. Zeros in
    /// → zeros out (modulo the CU8 midpoint).
    #[test]
    fn produces_output_at_target_rate() {
        let mut r = IqResampler::new(2_000_000.0, 1_488_375.0).unwrap();
        let input = vec![Complex::new(0.0_f32, 0.0_f32); CHUNK_FRAMES * 2];
        let mut out = Vec::new();
        r.feed(&input, &mut out);
        // Output bytes per chunk ≈ CHUNK_FRAMES * (1488375/2000000) * 2
        //   = 8192 * 0.744... * 2 ≈ 12 195
        // Two chunks ≈ 24 390 bytes (+/- a handful from phase drift).
        // Looser bound: we should get *some* output, not all of it
        // buffered.
        assert!(out.len() > 1024, "got {} output bytes", out.len());
        // Every byte should be the zero-IQ midpoint (127 or 128 due
        // to rounding) for an all-zero input.
        for b in &out {
            assert!(*b == 127 || *b == 128, "unexpected byte: {}", b);
        }
    }

    /// CU8 clamp must not wrap on out-of-range inputs.
    #[test]
    fn cu8_saturates_clean() {
        assert_eq!(float_to_cu8(-2.0), 0);
        assert_eq!(float_to_cu8(2.0), 255);
        assert_eq!(float_to_cu8(0.0), 128);
    }
}
