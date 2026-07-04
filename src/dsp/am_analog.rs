//! Lightweight AM analog demodulator for fallback audio on AM tunes.
//!
//! Input: interleaved CS16 I/Q near `NRSC5_SAMPLE_RATE_CS16_AM`.
//! Output: stereo s16le frames at 44.1 kHz (mono duplicated to L/R).
//!
//! The chain is intentionally simple and robust for weak daytime AM:
//! 1) envelope detector (`sqrt(i^2 + q^2)`),
//! 2) DC blocker to remove the carrier baseline,
//! 3) audio low-pass and gentle AGC,
//! 4) fractional-rate resample to 44.1 kHz.

const AM_INPUT_RATE_HZ: f32 = 46_511.71875;
const AUDIO_OUTPUT_RATE_HZ: f32 = 44_100.0;
const OUTPUT_GAIN: f32 = 0.9;

const DC_BLOCK_R: f32 = 0.995;
const DEFAULT_IF_CUTOFF_HZ: f32 = 4_200.0;
const AUDIO_LPF_ALPHA: f32 = 0.18;
const AGC_ALPHA: f32 = 0.0008;
const AGC_TARGET: f32 = 0.28;
const AGC_MAX_GAIN: f32 = 10.0;
const AUDIO_SQUELCH: f32 = 0.002;

fn one_pole_alpha(cutoff_hz: f32, sample_rate_hz: f32) -> f32 {
    // Matched-z one-pole LPF coefficient: alpha = 1 - exp(-2*pi*fc/fs).
    let fc = cutoff_hz.max(10.0);
    let fs = sample_rate_hz.max(1.0);
    (1.0 - (-2.0 * std::f32::consts::PI * fc / fs).exp()).clamp(0.001, 0.999)
}

/// AM envelope demodulator state.
pub struct AmDemod {
    if_lpf_alpha: f32,
    iq_i_1: f32,
    iq_q_1: f32,
    iq_i_2: f32,
    iq_q_2: f32,
    prev_env: f32,
    dc_state: f32,
    lpf_state: f32,
    agc_level: f32,
    phase: f32,
    step: f32,
}

impl Default for AmDemod {
    fn default() -> Self {
        Self::new()
    }
}

impl AmDemod {
    pub fn new() -> Self {
        Self::new_with_if_cutoff_hz(DEFAULT_IF_CUTOFF_HZ)
    }

    pub fn new_with_if_cutoff_hz(if_cutoff_hz: f32) -> Self {
        Self {
            if_lpf_alpha: one_pole_alpha(if_cutoff_hz, AM_INPUT_RATE_HZ),
            iq_i_1: 0.0,
            iq_q_1: 0.0,
            iq_i_2: 0.0,
            iq_q_2: 0.0,
            prev_env: 0.0,
            dc_state: 0.0,
            lpf_state: 0.0,
            agc_level: 0.0,
            phase: 0.0,
            step: AUDIO_OUTPUT_RATE_HZ / AM_INPUT_RATE_HZ,
        }
    }

    /// Push one complex sample and optionally emit one stereo frame.
    pub fn push_complex(&mut self, i: f32, q: f32) -> Option<[i16; 2]> {
        // Pre-detect complex LPF to suppress adjacent-channel carriers.
        self.iq_i_1 += self.if_lpf_alpha * (i - self.iq_i_1);
        self.iq_q_1 += self.if_lpf_alpha * (q - self.iq_q_1);
        self.iq_i_2 += self.if_lpf_alpha * (self.iq_i_1 - self.iq_i_2);
        self.iq_q_2 += self.if_lpf_alpha * (self.iq_q_1 - self.iq_q_2);

        let env = (self.iq_i_2 * self.iq_i_2 + self.iq_q_2 * self.iq_q_2).sqrt();

        // One-pole DC blocker: y[n] = x[n] - x[n-1] + r*y[n-1].
        let mut audio = env - self.prev_env + DC_BLOCK_R * self.dc_state;
        self.prev_env = env;
        self.dc_state = audio;

        // Audio-band low-pass to tame detector hash.
        self.lpf_state += AUDIO_LPF_ALPHA * (audio - self.lpf_state);
        audio = self.lpf_state;

        // Slow AGC keeps perceived loudness in range as RF level drifts.
        self.agc_level += AGC_ALPHA * (audio.abs() - self.agc_level);
        let gain = (AGC_TARGET / self.agc_level.max(1e-3)).min(AGC_MAX_GAIN);
        audio *= gain * OUTPUT_GAIN;

        if audio.abs() < AUDIO_SQUELCH {
            audio = 0.0;
        }

        self.phase += self.step;
        if self.phase < 1.0 {
            return None;
        }
        self.phase -= 1.0;

        let s = (audio.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
        Some([s, s])
    }

    /// Demodulate a CS16 I/Q chunk and append stereo output frames.
    pub fn push_cs16_block(&mut self, samples: &[i16], out: &mut Vec<i16>) {
        for pair in samples.chunks_exact(2) {
            let i = pair[0] as f32 / i16::MAX as f32;
            let q = pair[1] as f32 / i16::MAX as f32;
            if let Some([l, r]) = self.push_complex(i, q) {
                out.push(l);
                out.push(r);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emits_audio_for_constant_carrier_with_tone() {
        let mut demod = AmDemod::new();
        let mut out = Vec::new();

        // Carrier with a small 1 kHz amplitude modulation.
        let fs = AM_INPUT_RATE_HZ;
        let tone_hz = 1_000.0f32;
        let mut phase = 0.0f32;
        let dphi = 2.0 * std::f32::consts::PI * tone_hz / fs;
        for _ in 0..100_000 {
            let m = 1.0 + 0.3 * phase.sin();
            phase += dphi;
            let i = (m * 0.7 * i16::MAX as f32) as i16;
            let q = 0i16;
            demod.push_cs16_block(&[i, q], &mut out);
        }

        assert!(out.len() > 1_000);
        let peak = out.iter().map(|s| s.abs() as i32).max().unwrap_or(0);
        assert!(peak > 200);
    }
}
