//! Analog-FM fallback demodulator (stereo-capable) for the
//! feature-gated analog path.
//!
//! Receive chain (all from the shared cu8 I/Q bus at 1.488 Msps):
//!
//! 1. Channel-select low-pass + decimate to isolate the FM signal.
//! 2. Phase-difference discriminator to recover the MPX baseband from
//!    the FM carrier. The discriminator runs at the ~248 ksps
//!    intermediate rate, so its output holds the *entire* multiplex —
//!    the 0–15 kHz L+R sum, the 19 kHz stereo pilot, the 23–53 kHz
//!    L−R difference (DSB-SC on 38 kHz), and 57 kHz RDS.
//! 3. Stereo decode: a PLL locks the 19 kHz pilot and synthesizes the
//!    38 kHz subcarrier reference; coherently multiplying the MPX by it
//!    recovers L−R. A pilot-strength blend fades L−R toward zero as the
//!    pilot weakens, so stereo degrades continuously to clean mono.
//! 4. Audio low-pass + decimate the L+R and L−R paths, matrix into
//!    L/R, 75 µs de-emphasis per channel, and resample to playback rate.

use std::collections::VecDeque;
use std::f32::consts::PI;

const DEFAULT_SAMPLE_RATE_HZ: u32 = 44_100;
const FM_DEEMPHASIS_US: f32 = 75.0;
const SDR_SAMPLE_RATE_HZ: u32 = 1_488_375;
const RDS_BAUD_RATE_BPS: f32 = 1_187.5;
/// One-pole coefficient for the RDS complex baseband low-pass
/// (~2.4 kHz cutoff at the ~248 ksps MPX rate).
const RDS_BASEBAND_ALPHA: f32 = 0.06;
/// BPSK Costas-loop step size for nulling residual subcarrier phase.
const RDS_COSTAS_MU: f32 = 0.001;
/// AGC averaging coefficient. Normalizes the RDS baseband to ~unit
/// amplitude so the Costas and timing loops see a predictable error
/// scale regardless of signal strength.
const RDS_AGC_ALPHA: f32 = 0.0005;
/// Gardner symbol-timing-recovery loop gains (proportional + integral).
const RDS_TIMING_ALPHA: f32 = 0.01;
const RDS_TIMING_BETA: f32 = 0.0002;

/// RDS (26,16) shortened cyclic code generator polynomial
/// g(x) = x^10 + x^8 + x^7 + x^5 + x^4 + x^3 + 1.
const RDS_POLY: u32 = 0x5B9;

/// Block offset words. Under `rds_syndrome`, an error-free block's
/// syndrome equals its offset word.
const RDS_OFFSET_A: u16 = 0x0FC;
const RDS_OFFSET_B: u16 = 0x198;
const RDS_OFFSET_C: u16 = 0x168;
const RDS_OFFSET_CP: u16 = 0x350;
const RDS_OFFSET_D: u16 = 0x1B4;

/// Channel-select decimation: 1.488 Msps → ~248 ksps.
const CHANNEL_DECIM: usize = 6;
const CHANNEL_CUTOFF_HZ: f32 = 100_000.0;
const CHANNEL_TAPS: usize = 47;

/// Audio decimation: ~248 ksps → ~62 ksps after band-limiting to 15 kHz.
/// 15 kHz (not 16) plus a longer kernel pushes the 19 kHz pilot deep
/// into the stopband so it can't leak into the L+R sum or the
/// recovered L−R difference.
const AUDIO_DECIM: usize = 4;
const AUDIO_CUTOFF_HZ: f32 = 15_000.0;
const AUDIO_TAPS: usize = 63;

/// Stereo pilot tone frequency (broadcast FM standard).
const PILOT_FREQ_HZ: f32 = 19_000.0;

/// Quality factor of the 19 kHz pilot bandpass ahead of the PLL.
const PILOT_BANDPASS_Q: f32 = 12.0;

/// Pilot PLL loop bandwidth (Hz) and damping. Narrow enough to reject
/// program material bleeding into the 19 kHz bin, wide enough to pull
/// in SDR/broadcaster frequency error within a few milliseconds.
const PILOT_LOOP_BW_HZ: f32 = 250.0;
const PILOT_LOOP_DAMPING: f32 = 0.707;

/// Phase-detector gain. Compensates for the small pilot amplitude in
/// the normalized MPX (~0.09 full-scale) so the loop-filter gains act
/// on a roughly unit-scale error. Tuned empirically.
const PILOT_PD_GAIN: f32 = 20.0;

/// EMA coefficient for the coherent pilot phasor used as the stereo
/// blend confidence (~4 ms time constant at the MPX rate).
const PILOT_PHASOR_EMA: f32 = 0.001;

/// Coherent-pilot magnitude thresholds for the stereo blend. Below
/// `LO` the output is fully mono (L−R muted); above `HI` it is full
/// separation; in between it blends linearly. Tuned against the
/// normalized MPX; refine by ear against a known-stereo station.
const STEREO_BLEND_LO: f32 = 0.006;
const STEREO_BLEND_HI: f32 = 0.020;

/// Broadcast FM peak deviation. Used to normalize the discriminator
/// output so full deviation maps to about unity before output scaling.
const FM_PEAK_DEVIATION_HZ: f32 = 75_000.0;

/// Final output headroom against i16 full scale.
const OUTPUT_GAIN: f32 = 0.8;

/// Make-up gain applied to the normalized discriminator output. Full
/// deviation maps to ±1.0, but typical program material averages well
/// below peak deviation, so a modest boost brings the perceived
/// loudness in line with the HD path. Loud peaks are protected by the
/// clamp at output.
const MAKEUP_GAIN: f32 = 0.95;

/// Low-level squelch threshold for the recovered audio. The fallback
/// path should stay silent rather than output noise floor when the FM
/// signal is too weak to be useful.
const AUDIO_SQUELCH_THRESHOLD: f32 = 0.008;

/// Design a Hamming-windowed-sinc low-pass FIR.
fn lowpass_kernel(num_taps: usize, cutoff_norm: f32) -> Vec<f32> {
    let m = (num_taps - 1) as f32;
    let mut taps = vec![0.0f32; num_taps];
    let mut sum = 0.0f32;
    for (n, tap) in taps.iter_mut().enumerate() {
        let x = n as f32 - m / 2.0;
        let sinc = if x.abs() < 1e-6 {
            2.0 * cutoff_norm
        } else {
            (2.0 * PI * cutoff_norm * x).sin() / (PI * x)
        };
        let window = 0.54 - 0.46 * (2.0 * PI * n as f32 / m).cos();
        *tap = sinc * window;
        sum += *tap;
    }
    if sum.abs() > 1e-12 {
        for tap in taps.iter_mut() {
            *tap /= sum;
        }
    }
    taps
}

/// A decimating FIR over complex samples backed by a history buffer.
struct FirDecimator {
    taps: Vec<f32>,
    hist_i: VecDeque<f32>,
    hist_q: VecDeque<f32>,
    decim: usize,
    count: usize,
}

impl FirDecimator {
    fn new(taps: Vec<f32>, decim: usize) -> Self {
        let capacity = taps.len();
        Self {
            taps,
            hist_i: VecDeque::with_capacity(capacity),
            hist_q: VecDeque::with_capacity(capacity),
            decim,
            count: 0,
        }
    }

    fn push(&mut self, i: f32, q: f32) -> Option<(f32, f32)> {
        self.hist_i.push_back(i);
        self.hist_q.push_back(q);
        if self.hist_i.len() > self.taps.len() {
            self.hist_i.pop_front();
            self.hist_q.pop_front();
        }

        self.count += 1;
        if self.count < self.decim {
            return None;
        }
        self.count = 0;

        if self.hist_i.len() < self.taps.len() {
            return None;
        }

        let mut acc_i = 0.0f32;
        let mut acc_q = 0.0f32;
        for (tap, (hist_i, hist_q)) in self.taps.iter().zip(self.hist_i.iter().zip(self.hist_q.iter())) {
            acc_i += tap * hist_i;
            acc_q += tap * hist_q;
        }
        Some((acc_i, acc_q))
    }
}

/// Transposed direct-form-II biquad, used as the 19 kHz pilot bandpass
/// ahead of the stereo PLL.
struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: f32,
    z2: f32,
}

impl Biquad {
    /// RBJ bandpass biquad centered at `f0` with quality `q` at `fs`.
    fn bandpass(f0: f32, q: f32, fs: f32) -> Self {
        let w0 = 2.0 * PI * f0 / fs;
        let (sin_w0, cos_w0) = w0.sin_cos();
        let alpha = sin_w0 / (2.0 * q);
        let a0 = 1.0 + alpha;
        Self {
            b0: alpha / a0,
            b1: 0.0,
            b2: -alpha / a0,
            a1: -2.0 * cos_w0 / a0,
            a2: (1.0 - alpha) / a0,
            z1: 0.0,
            z2: 0.0,
        }
    }

    fn process(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }
}

/// Second-order PLL that locks the 19 kHz stereo pilot and synthesizes
/// the coherent 38 kHz subcarrier reference for L\u2212R demodulation. Also
/// tracks the coherent pilot magnitude, which drives the stereo blend.
struct PilotPll {
    phase: f32,
    freq: f32,
    omega0: f32,
    alpha: f32,
    beta: f32,
    bandpass: Biquad,
    /// Coherent pilot phasor (EMA-smoothed). Its magnitude is ~0 with
    /// no pilot and rises once the loop locks, giving a phase-agnostic
    /// stereo-strength estimate.
    phasor_i: f32,
    phasor_q: f32,
    /// Coherent 57 kHz (3rd pilot harmonic) reference for RDS demod,
    /// recomputed each `process` call from the current pilot phase.
    sub_cos: f32,
    sub_sin: f32,
}

impl PilotPll {
    fn new(fs: f32) -> Self {
        let omega0 = 2.0 * PI * PILOT_FREQ_HZ / fs;
        let wn = 2.0 * PI * PILOT_LOOP_BW_HZ / fs;
        Self {
            phase: 0.0,
            freq: 0.0,
            omega0,
            alpha: 2.0 * PILOT_LOOP_DAMPING * wn,
            beta: wn * wn,
            bandpass: Biquad::bandpass(PILOT_FREQ_HZ, PILOT_BANDPASS_Q, fs),
            phasor_i: 0.0,
            phasor_q: 0.0,
            sub_cos: 1.0,
            sub_sin: 0.0,
        }
    }

    /// Advance the loop by one MPX sample and return the coherent
    /// 38 kHz reference `sin(2\u00b7phase)` for L\u2212R demodulation.
    fn process(&mut self, mpx: f32) -> f32 {
        let pilot = self.bandpass.process(mpx);
        let (sin_p, cos_p) = self.phase.sin_cos();

        // Phase detector: pilot\u00b7cos(phase) \u2248 -\u00bd\u00b7sin(phase error).
        let err = PILOT_PD_GAIN * pilot * cos_p;
        self.freq += self.beta * err;
        self.phase += self.omega0 + self.freq + self.alpha * err;
        if self.phase >= 2.0 * PI {
            self.phase -= 2.0 * PI;
        } else if self.phase < 0.0 {
            self.phase += 2.0 * PI;
        }

        // Coherent phasor for the stereo-strength estimate.
        self.phasor_i += PILOT_PHASOR_EMA * (pilot * sin_p - self.phasor_i);
        self.phasor_q += PILOT_PHASOR_EMA * (pilot * cos_p - self.phasor_q);

        // Coherent 57 kHz reference = cos/sin of 3x the pilot phase
        // (triple-angle identities from the current sin/cos). RDS's
        // suppressed subcarrier is locked to this 3rd pilot harmonic.
        self.sub_cos = 4.0 * cos_p * cos_p * cos_p - 3.0 * cos_p;
        self.sub_sin = 3.0 * sin_p - 4.0 * sin_p * sin_p * sin_p;

        // sin(2\u00b7phase) = 2\u00b7sin(phase)\u00b7cos(phase).
        2.0 * sin_p * cos_p
    }

    /// Coherent 57 kHz (3rd pilot harmonic) reference `(cos, sin)` for
    /// RDS subcarrier demodulation, valid after the latest `process`.
    fn subcarrier(&self) -> (f32, f32) {
        (self.sub_cos, self.sub_sin)
    }

    /// Coherent pilot magnitude \u2014 the stereo-blend confidence input.
    fn pilot_magnitude(&self) -> f32 {
        (self.phasor_i * self.phasor_i + self.phasor_q * self.phasor_q).sqrt()
    }
}

#[derive(Clone, Copy)]
struct DeemphasisFilter {
    alpha: f32,
    state: f32,
}

impl DeemphasisFilter {
    fn new(sample_rate_hz: u32, tau_s: f32) -> Self {
        let alpha = (-1.0 / (sample_rate_hz as f32 * tau_s)).exp();
        Self { alpha, state: 0.0 }
    }

    fn push(&mut self, input: f32) -> f32 {
        self.state = (1.0 - self.alpha) * input + self.alpha * self.state;
        self.state
    }
}

struct OutputRateResampler {
    phase: f64,
    step: f64,
}

impl OutputRateResampler {
    fn new(input_rate_hz: u32, output_rate_hz: u32) -> Self {
        Self {
            phase: 0.0,
            step: output_rate_hz as f64 / input_rate_hz as f64,
        }
    }

    fn should_emit(&mut self) -> bool {
        self.phase += self.step;
        if self.phase >= 1.0 {
            self.phase -= 1.0;
            true
        } else {
            false
        }
    }
}

/// 10-bit syndrome of a 26-bit RDS block (remainder modulo the RDS
/// generator polynomial). For an error-free block the syndrome equals
/// the block's offset word.
fn rds_syndrome(block: u32) -> u16 {
    let mut reg: u32 = 0;
    for i in (0..26).rev() {
        reg = (reg << 1) | ((block >> i) & 1);
        if reg & (1 << 10) != 0 {
            reg ^= RDS_POLY;
        }
    }
    (reg & 0x3FF) as u16
}

/// Whether `syndrome` matches the expected offset word for a block at
/// `position` (0=A, 1=B, 2=C or C', 3=D) within a group.
fn rds_offset_matches(position: usize, syndrome: u16) -> bool {
    match position {
        0 => syndrome == RDS_OFFSET_A,
        1 => syndrome == RDS_OFFSET_B,
        2 => syndrome == RDS_OFFSET_C || syndrome == RDS_OFFSET_CP,
        3 => syndrome == RDS_OFFSET_D,
        _ => false,
    }
}

fn is_rds_printable(byte: u8) -> bool {
    byte == b' ' || byte.is_ascii_graphic()
}

fn ps_to_string(ps: &[u8; 8]) -> String {
    ps.iter().map(|&b| b as char).collect::<String>().trim_end().to_string()
}

/// Recovers one channel bit per two half-bit levels using the RDS
/// bi-phase (Manchester) rule: a high\u2192low mid-symbol transition is a 1,
/// low\u2192high is a 0.
struct ManchesterBitGen {
    have_first: bool,
    first: u8,
}

impl ManchesterBitGen {
    fn new() -> Self {
        Self { have_first: false, first: 0 }
    }

    fn push_half(&mut self, level: u8) -> Option<u8> {
        if !self.have_first {
            self.first = level;
            self.have_first = true;
            None
        } else {
            self.have_first = false;
            Some(if self.first == 1 && level == 0 { 1 } else { 0 })
        }
    }
}

/// A decoded RDS text field, emitted by [`RdsGroupDecoder`] when a
/// field has been fully confirmed. Program Service is the short 8-char
/// station name (group 0); RadioText is the longer scrolling message
/// (group 2, up to 64 chars) that carries song / artist / promo text.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RdsUpdate {
    ProgramService(String),
    RadioText(String),
}

/// Synchronizes to the 26-bit RDS block structure via offset-word
/// syndromes and extracts the Program Service name (group 0) and
/// RadioText (group 2). Only CRC-valid blocks are ever used, so corrupt
/// data cannot leak through as text.
struct RdsGroupDecoder {
    reg: u32,
    filled: usize,
    synced: bool,
    position: usize,
    bits_in_block: usize,
    blocks: [u16; 4],
    ps: [u8; 8],
    ps_valid: [bool; 8],
    /// Last character received at each PS position, used to require two
    /// consecutive identical receptions before a character is committed.
    ps_prev: [u8; 8],
    ps_prev_valid: [bool; 8],
    last_emitted: Option<[u8; 8]>,
    /// RadioText assembly buffer (group 2). Up to 64 characters for 2A,
    /// 32 for 2B. Same per-character double-confirmation as PS.
    rt: [u8; 64],
    rt_valid: [bool; 64],
    rt_prev: [u8; 64],
    rt_prev_valid: [bool; 64],
    /// Last observed Text A/B flag. A toggle means the station started a
    /// new message, so the buffer is cleared and reassembled from the
    /// next groups. `None` until the first group 2 arrives.
    rt_ab: Option<bool>,
    last_rt_emitted: Option<String>,
}

impl RdsGroupDecoder {
    fn new() -> Self {
        Self {
            reg: 0,
            filled: 0,
            synced: false,
            position: 0,
            bits_in_block: 0,
            blocks: [0; 4],
            ps: [b' '; 8],
            ps_valid: [false; 8],
            ps_prev: [b' '; 8],
            ps_prev_valid: [false; 8],
            last_emitted: None,
            rt: [b' '; 64],
            rt_valid: [false; 64],
            rt_prev: [b' '; 64],
            rt_prev_valid: [false; 64],
            rt_ab: None,
            last_rt_emitted: None,
        }
    }

    fn reset(&mut self) {
        *self = Self::new();
    }

    fn push_bit(&mut self, bit: u8) -> Option<RdsUpdate> {
        self.reg = ((self.reg << 1) | (bit as u32 & 1)) & 0x3FF_FFFF;
        if self.filled < 26 {
            self.filled += 1;
        }
        if self.filled < 26 {
            return None;
        }

        if !self.synced {
            // Hunt bit-by-bit for a valid A block to establish lock.
            if rds_syndrome(self.reg) == RDS_OFFSET_A {
                self.blocks[0] = (self.reg >> 10) as u16;
                self.synced = true;
                self.position = 1;
                self.bits_in_block = 0;
            }
            return None;
        }

        // Locked: validate one block every 26 bits.
        self.bits_in_block += 1;
        if self.bits_in_block < 26 {
            return None;
        }
        self.bits_in_block = 0;

        let syndrome = rds_syndrome(self.reg);
        if !rds_offset_matches(self.position, syndrome) {
            self.synced = false;
            self.position = 0;
            return None;
        }

        self.blocks[self.position] = (self.reg >> 10) as u16;
        if self.position == 3 {
            let result = self.finalize_group();
            self.position = 0; // next group starts again with an A block
            return result;
        }
        self.position += 1;
        None
    }

    fn finalize_group(&mut self) -> Option<RdsUpdate> {
        let block_b = self.blocks[1];
        // Bits 15..12 are the 4-bit group type; bit 11 is the version
        // (0 = A, 1 = B). Group 0 carries the Program Service name;
        // group 2 carries RadioText.
        match (block_b >> 12) & 0xF {
            0 => self.finalize_ps(block_b),
            2 => self.finalize_radiotext(block_b),
            _ => None,
        }
    }

    fn finalize_ps(&mut self, block_b: u16) -> Option<RdsUpdate> {
        let segment = (block_b & 0x03) as usize;
        let chars = self.blocks[3];
        let idx = segment * 2;
        let pair = [(chars >> 8) as u8, (chars & 0xFF) as u8];

        // Per-character double confirmation: a PS character is only
        // committed to the displayed name after it has been received
        // identically in two consecutive transmissions of the same
        // segment. This rejects the isolated single-character errors that
        // slip past the block CRC (undetected roughly 1 in 1024) and that
        // otherwise pollute the dynamic-PS scroll with garbled variants.
        for (offset, &c) in pair.iter().enumerate() {
            let p = idx + offset;
            if self.ps_prev_valid[p] && self.ps_prev[p] == c {
                self.ps[p] = c;
                self.ps_valid[p] = true;
            }
            self.ps_prev[p] = c;
            self.ps_prev_valid[p] = true;
        }

        if !self.ps_valid.iter().all(|&v| v) {
            return None;
        }
        if !self.ps.iter().all(|&b| is_rds_printable(b)) {
            return None;
        }

        let candidate = self.ps;
        if self.last_emitted != Some(candidate) {
            self.last_emitted = Some(candidate);
            let text = ps_to_string(&candidate);
            if !text.is_empty() {
                return Some(RdsUpdate::ProgramService(text));
            }
        }
        None
    }

    /// Accumulate one RadioText group (2A or 2B) into the reassembly
    /// buffer. Version A packs four characters per group (blocks C and
    /// D) at `addr * 4`; version B packs two (block D only) at
    /// `addr * 2` and repeats the PI code in block C. A Text A/B flag
    /// toggle signals a fresh message and clears the buffer. Characters
    /// carry the same two-reception confirmation as PS, and the message
    /// is emitted only once every position up to its terminator is
    /// confirmed.
    fn finalize_radiotext(&mut self, block_b: u16) -> Option<RdsUpdate> {
        let version_b = (block_b >> 11) & 1 == 1;
        let text_ab = (block_b >> 4) & 1 == 1;
        let addr = (block_b & 0x0F) as usize;

        // A/B flag toggle => the station began a new message. Clear the
        // buffer so stale characters from the previous message can't
        // bleed into the new one.
        if self.rt_ab != Some(text_ab) {
            self.rt = [b' '; 64];
            self.rt_valid = [false; 64];
            self.rt_prev = [b' '; 64];
            self.rt_prev_valid = [false; 64];
            self.rt_ab = Some(text_ab);
        }

        let mut chars = [0u8; 4];
        let (base, count) = if version_b {
            let d = self.blocks[3];
            chars[0] = (d >> 8) as u8;
            chars[1] = (d & 0xFF) as u8;
            (addr * 2, 2)
        } else {
            let c = self.blocks[2];
            let d = self.blocks[3];
            chars[0] = (c >> 8) as u8;
            chars[1] = (c & 0xFF) as u8;
            chars[2] = (d >> 8) as u8;
            chars[3] = (d & 0xFF) as u8;
            (addr * 4, 4)
        };

        for (offset, &c) in chars.iter().take(count).enumerate() {
            let p = base + offset;
            if p >= 64 {
                continue;
            }
            if self.rt_prev_valid[p] && self.rt_prev[p] == c {
                self.rt[p] = c;
                self.rt_valid[p] = true;
            }
            self.rt_prev[p] = c;
            self.rt_prev_valid[p] = true;
        }

        let max = if version_b { 32 } else { 64 };
        self.assemble_radiotext(max)
    }

    /// Assemble the confirmed RadioText buffer into a string if it is
    /// complete. RadioText ends at a carriage return (0x0D) or fills the
    /// whole `max`-character field. Returns a new value only when every
    /// character up to the terminator is confirmed and the assembled text
    /// differs from the last one emitted.
    fn assemble_radiotext(&mut self, max: usize) -> Option<RdsUpdate> {
        // Locate the terminating carriage return among confirmed
        // positions. Its own slot need only be confirmed as 0x0D.
        let mut end = None;
        for p in 0..max {
            if self.rt_valid[p] && self.rt[p] == 0x0D {
                end = Some(p);
                break;
            }
        }
        let len = match end {
            Some(e) => e,
            None => {
                // No terminator yet: only assemble once the whole field
                // is confirmed (space-padded fixed-length messages).
                if (0..max).all(|p| self.rt_valid[p]) {
                    max
                } else {
                    return None;
                }
            }
        };

        if !(0..len).all(|p| self.rt_valid[p] && is_rds_printable(self.rt[p])) {
            return None;
        }

        let text: String = self.rt[..len]
            .iter()
            .map(|&b| b as char)
            .collect::<String>()
            .trim_end()
            .to_string();
        if text.is_empty() {
            return None;
        }
        if self.last_rt_emitted.as_deref() != Some(text.as_str()) {
            self.last_rt_emitted = Some(text.clone());
            return Some(RdsUpdate::RadioText(text));
        }
        None
    }
}

/// Coherent RDS receiver: mixes the pilot-locked 57 kHz subcarrier down
/// to complex baseband, low-passes it, and phase-corrects it with a
/// BPSK Costas loop. The recovered in-phase data carries differentially
/// encoded bi-phase symbols at 1187.5 bps. Two Manchester pipelines run
/// at the two half-bit alignments so the bit-clock phase never has to be
/// guessed \u2014 whichever pipeline achieves block sync produces the text.
struct RdsChannel {
    i_lp1: f32,
    i_lp2: f32,
    q_lp1: f32,
    q_lp2: f32,
    agc: f32,
    costas_phase: f32,
    /// Nominal Gardner NCO increment (strobes per input sample); two
    /// strobes per chip.
    base_inc: f32,
    period_inc: f32,
    nco: f32,
    prev_in: f32,
    have_prev: bool,
    /// 0 = next strobe is a midpoint (TED only); 1 = on-time chip center.
    strobe_parity: u8,
    mid_val: f32,
    prev_ontime: f32,
    timing_freq: f32,
    total_chips: usize,
    gen_a: ManchesterBitGen,
    gen_b: ManchesterBitGen,
    diff_prev_a: u8,
    diff_prev_b: u8,
    group_a: RdsGroupDecoder,
    group_b: RdsGroupDecoder,
}

impl RdsChannel {
    fn new(sample_rate_hz: f32) -> Self {
        // Two timing strobes per chip (half-bit); chip rate = 2xbaud.
        let samples_per_chip = sample_rate_hz / (2.0 * RDS_BAUD_RATE_BPS);
        let base_inc = 1.0 / (samples_per_chip / 2.0);
        Self {
            i_lp1: 0.0,
            i_lp2: 0.0,
            q_lp1: 0.0,
            q_lp2: 0.0,
            agc: 0.0,
            costas_phase: 0.0,
            base_inc,
            period_inc: base_inc,
            nco: 0.0,
            prev_in: 0.0,
            have_prev: false,
            strobe_parity: 0,
            mid_val: 0.0,
            prev_ontime: 0.0,
            timing_freq: 0.0,
            total_chips: 0,
            gen_a: ManchesterBitGen::new(),
            gen_b: ManchesterBitGen::new(),
            diff_prev_a: 0,
            diff_prev_b: 0,
            group_a: RdsGroupDecoder::new(),
            group_b: RdsGroupDecoder::new(),
        }
    }

    fn reset(&mut self) {
        self.i_lp1 = 0.0;
        self.i_lp2 = 0.0;
        self.q_lp1 = 0.0;
        self.q_lp2 = 0.0;
        self.agc = 0.0;
        self.costas_phase = 0.0;
        self.period_inc = self.base_inc;
        self.nco = 0.0;
        self.prev_in = 0.0;
        self.have_prev = false;
        self.strobe_parity = 0;
        self.mid_val = 0.0;
        self.prev_ontime = 0.0;
        self.timing_freq = 0.0;
        self.total_chips = 0;
        self.gen_a = ManchesterBitGen::new();
        self.gen_b = ManchesterBitGen::new();
        self.diff_prev_a = 0;
        self.diff_prev_b = 0;
        self.group_a.reset();
        self.group_b.reset();
    }

    fn push(&mut self, mpx: f32, cos3: f32, sin3: f32) -> Option<RdsUpdate> {
        // Mix the 57 kHz subcarrier down to complex baseband and
        // low-pass to the ~2.4 kHz RDS data bandwidth (this also acts as
        // the matched filter for the 2375 Hz chip rate).
        let i_mix = mpx * cos3;
        let q_mix = mpx * sin3;
        self.i_lp1 += RDS_BASEBAND_ALPHA * (i_mix - self.i_lp1);
        self.i_lp2 += RDS_BASEBAND_ALPHA * (self.i_lp1 - self.i_lp2);
        self.q_lp1 += RDS_BASEBAND_ALPHA * (q_mix - self.q_lp1);
        self.q_lp2 += RDS_BASEBAND_ALPHA * (self.q_lp1 - self.q_lp2);
        let mut i = self.i_lp2;
        let mut q = self.q_lp2;

        // AGC: normalize to ~unit amplitude for predictable loop gains.
        let mag = (i * i + q * q).sqrt();
        self.agc += RDS_AGC_ALPHA * (mag - self.agc);
        if self.agc > 1e-6 {
            let gain = 1.0 / self.agc;
            i *= gain;
            q *= gain;
        }

        // BPSK Costas loop: drive residual carrier phase to zero so the
        // data lands on the in-phase axis.
        let (sin_c, cos_c) = self.costas_phase.sin_cos();
        let i_rot = i * cos_c + q * sin_c;
        let q_rot = -i * sin_c + q * cos_c;
        self.costas_phase += RDS_COSTAS_MU * i_rot.signum() * q_rot;
        if self.costas_phase > PI {
            self.costas_phase -= 2.0 * PI;
        } else if self.costas_phase < -PI {
            self.costas_phase += 2.0 * PI;
        }

        // Gardner timing recovery: one chip level per on-time strobe.
        let level = self.advance_timing(i_rot)?;

        let index = self.total_chips;
        self.total_chips = self.total_chips.wrapping_add(1);
        let mut result = None;
        // Pipeline A pairs chips (0,1),(2,3),...; pipeline B is offset by
        // one chip: (1,2),(3,4),... One alignment forms valid bits.
        if let Some(channel_bit) = self.gen_a.push_half(level) {
            let data = channel_bit ^ self.diff_prev_a; // differential decode
            self.diff_prev_a = channel_bit;
            if let Some(ps) = self.group_a.push_bit(data) {
                result = Some(ps);
            }
        }
        if index >= 1 {
            if let Some(channel_bit) = self.gen_b.push_half(level) {
                let data = channel_bit ^ self.diff_prev_b; // differential decode
                self.diff_prev_b = channel_bit;
                if let Some(ps) = self.group_b.push_bit(data) {
                    result = Some(ps);
                }
            }
        }
        result
    }

    /// Advance the Gardner timing NCO by one input sample. Strobes twice
    /// per chip; returns a chip level (0/1) at each on-time strobe (chip
    /// center). Midpoint strobes only drive the timing-error detector.
    fn advance_timing(&mut self, sample: f32) -> Option<u8> {
        if !self.have_prev {
            self.prev_in = sample;
            self.have_prev = true;
            return None;
        }
        self.nco += self.period_inc;
        let mut chip = None;
        if self.nco >= 1.0 {
            self.nco -= 1.0;
            // Linear-interpolate the strobe between the previous and
            // current input sample.
            let t = (self.nco / self.period_inc).clamp(0.0, 1.0);
            let strobe = sample * (1.0 - t) + self.prev_in * t;
            if self.strobe_parity == 0 {
                self.mid_val = strobe;
                self.strobe_parity = 1;
            } else {
                let ontime = strobe;
                // Gardner TED for real BPSK. Positive error nudges the
                // strobe rate to pull the on-time strobe onto the chip
                // center.
                let error = (ontime - self.prev_ontime) * self.mid_val;
                self.timing_freq = (self.timing_freq + RDS_TIMING_BETA * error)
                    .clamp(-self.base_inc * 0.05, self.base_inc * 0.05);
                self.period_inc = (self.base_inc + RDS_TIMING_ALPHA * error + self.timing_freq)
                    .clamp(self.base_inc * 0.95, self.base_inc * 1.05);
                self.prev_ontime = ontime;
                self.strobe_parity = 0;
                chip = Some(if ontime >= 0.0 { 1 } else { 0 });
            }
        }
        self.prev_in = sample;
        chip
    }
}

pub struct FmDemod {
    channel: FirDecimator,
    /// L+R (mono sum) audio-band decimator.
    sum_fir: FirDecimator,
    /// L\u2212R (stereo difference) audio-band decimator, fed the coherently
    /// demodulated 38 kHz subcarrier product.
    diff_fir: FirDecimator,
    pll: PilotPll,
    deemphasis_l: DeemphasisFilter,
    deemphasis_r: DeemphasisFilter,
    output_resampler: OutputRateResampler,
    rds: RdsChannel,
    stereo_enabled: bool,
    rds_enabled: bool,
    rds_program_service: Option<String>,
    rds_radiotext: Option<String>,
    prev_i: f32,
    prev_q: f32,
    has_prev: bool,
    peak_dev_rad: f32,
}

impl FmDemod {
    pub fn new() -> Self {
        Self::with_sample_rate(DEFAULT_SAMPLE_RATE_HZ)
    }

    pub fn with_sample_rate(sample_rate_hz: u32) -> Self {
        let stage1_rate = SDR_SAMPLE_RATE_HZ as f32 / CHANNEL_DECIM as f32;
        let stage2_rate = stage1_rate / AUDIO_DECIM as f32;
        let channel_kernel = lowpass_kernel(CHANNEL_TAPS, CHANNEL_CUTOFF_HZ / SDR_SAMPLE_RATE_HZ as f32);
        let audio_kernel = lowpass_kernel(AUDIO_TAPS, AUDIO_CUTOFF_HZ / stage1_rate);

        Self {
            channel: FirDecimator::new(channel_kernel.clone(), CHANNEL_DECIM),
            sum_fir: FirDecimator::new(audio_kernel.clone(), AUDIO_DECIM),
            diff_fir: FirDecimator::new(audio_kernel, AUDIO_DECIM),
            pll: PilotPll::new(stage1_rate),
            deemphasis_l: DeemphasisFilter::new(stage2_rate as u32, FM_DEEMPHASIS_US * 1e-6),
            deemphasis_r: DeemphasisFilter::new(stage2_rate as u32, FM_DEEMPHASIS_US * 1e-6),
            output_resampler: OutputRateResampler::new(stage2_rate as u32, sample_rate_hz),
            rds: RdsChannel::new(stage1_rate),
            stereo_enabled: true,
            rds_enabled: true,
            rds_program_service: None,
            rds_radiotext: None,
            prev_i: 0.0,
            prev_q: 0.0,
            has_prev: false,
            peak_dev_rad: 2.0 * PI * FM_PEAK_DEVIATION_HZ / stage1_rate,
        }
    }

    pub fn set_stereo_enabled(&mut self, enabled: bool) {
        self.stereo_enabled = enabled;
    }

    pub fn set_rds_enabled(&mut self, enabled: bool) {
        self.rds_enabled = enabled;
        if !enabled {
            self.rds.reset();
            self.rds_program_service = None;
            self.rds_radiotext = None;
        }
    }

    pub fn push_complex(&mut self, i: f32, q: f32) -> Option<[i16; 2]> {
        let (fi, fq) = self.channel.push(i, q)?;

        let delta = if self.has_prev {
            phase_delta(self.prev_i, self.prev_q, fi, fq)
        } else {
            0.0
        };
        self.prev_i = fi;
        self.prev_q = fq;
        self.has_prev = true;

        // Standard FM discriminators recover the baseband from the
        // phase derivative of the complex carrier. Normalizing by the
        // per-sample radians at peak deviation maps full ±75 kHz
        // deviation to ±1.0. This is the full MPX (L+R, pilot, L−R,
        // RDS); the stereo decode and audio filtering follow.
        let mpx = delta / self.peak_dev_rad;

        // Advance the pilot PLL and get the coherent 38 kHz reference.
        // Runs every MPX sample so the loop stays locked; the L−R
        // product is decimated in lockstep with the L+R sum path.
        let ref38 = self.pll.process(mpx);
        if self.rds_enabled {
            let (cos3, sin3) = self.pll.subcarrier();
            match self.rds.push(mpx, cos3, sin3) {
                Some(RdsUpdate::ProgramService(ps)) => self.rds_program_service = Some(ps),
                Some(RdsUpdate::RadioText(rt)) => self.rds_radiotext = Some(rt),
                None => {}
            }
        }
        let sum_out = self.sum_fir.push(mpx, 0.0);
        // Coherent DSB-SC demod of the 38 kHz subcarrier. The ×2 undoes
        // the ½ from sin²; the low-pass in `diff_fir` removes the sum
        // and 2ω/3ω image terms, leaving the L−R difference.
        let diff_out = self.diff_fir.push(2.0 * mpx * ref38, 0.0);

        // Both decimators share decim=4 and are pushed once per call,
        // so they emit in lockstep: if the sum path yields a sample the
        // diff path does too.
        let (sum, _) = sum_out?;
        let (side_raw, _) = diff_out.expect("diff_fir emits in lockstep with sum_fir");

        // Stereo blend: fade L−R toward zero as the pilot weakens so
        // stereo degrades continuously to clean mono. With no pilot the
        // output collapses to `sum` on both channels — bit-identical to
        // the mono path.
        let side = if self.stereo_enabled {
            let mag = self.pll.pilot_magnitude();
            let stereo_gain = ((mag - STEREO_BLEND_LO) / (STEREO_BLEND_HI - STEREO_BLEND_LO))
                .clamp(0.0, 1.0);
            side_raw * stereo_gain
        } else {
            0.0
        };

        // The fallback path should stay silent rather than output noise
        // floor when the demodulator is not seeing enough signal, which
        // makes the receiver feel much more like a real analog receiver.
        let audio_level = (sum.abs() + side.abs()) * 0.5;
        let squelch_gain = if audio_level > AUDIO_SQUELCH_THRESHOLD {
            1.0
        } else {
            0.0
        };

        // Matrix: L = (L+R)+(L−R), R = (L+R)−(L−R). Both channels carry
        // a uniform 2× gain versus the true per-channel level (the same
        // 2× the mono sum already had), so loudness matches the mono
        // path and the output clamp handles peaks.
        let left = self.deemphasis_l.push((sum + side) * squelch_gain);
        let right = self.deemphasis_r.push((sum - side) * squelch_gain);

        if !self.output_resampler.should_emit() {
            return None;
        }

        let l = (soft_clip(left * MAKEUP_GAIN * OUTPUT_GAIN) * 32767.0)
            .clamp(-32768.0, 32767.0) as i16;
        let r = (soft_clip(right * MAKEUP_GAIN * OUTPUT_GAIN) * 32767.0)
            .clamp(-32768.0, 32767.0) as i16;
        Some([l, r])
    }

    pub fn take_rds_program_service(&mut self) -> Option<String> {
        self.rds_program_service.take()
    }

    pub fn take_rds_radiotext(&mut self) -> Option<String> {
        self.rds_radiotext.take()
    }
}

fn soft_clip(sample: f32) -> f32 {
    let x = sample.clamp(-1.0, 1.0);
    x / (1.0 + x.abs())
}

/// Instantaneous phase advance from `(prev_i, prev_q)` to `(i, q)`.
pub fn phase_delta(prev_i: f32, prev_q: f32, i: f32, q: f32) -> f32 {
    f32::atan2(prev_i * q - prev_q * i, prev_i * i + prev_q * q)
}

#[cfg(test)]
mod tests {
    use super::{phase_delta, DeemphasisFilter, OutputRateResampler};

    #[test]
    fn phase_delta_matches_a_known_rotation() {
        let prev_i = 1.0;
        let prev_q = 0.0;
        let i = 0.70710677;
        let q = 0.70710677;
        let delta = phase_delta(prev_i, prev_q, i, q);
        assert!((delta - 0.7853982).abs() < 1e-5);
    }

    #[test]
    fn phase_delta_is_negative_for_reverse_rotation() {
        let prev_i = 1.0;
        let prev_q = 0.0;
        let i = 0.70710677;
        let q = -0.70710677;
        let delta = phase_delta(prev_i, prev_q, i, q);
        assert!((delta + 0.7853982).abs() < 1e-5);
    }

    #[test]
    fn lowpass_kernel_has_unity_dc_gain() {
        let kernel = super::lowpass_kernel(super::CHANNEL_TAPS, 0.05);
        let sum: f32 = kernel.iter().sum();
        assert!((sum - 1.0).abs() < 1e-4);
    }

    #[test]
    fn fir_decimator_emits_on_boundaries_only() {
        let mut fir = super::FirDecimator::new(vec![0.25, 0.5, 0.25], 4);
        let mut emits = 0;
        for _ in 0..16 {
            if fir.push(1.0, 0.0).is_some() {
                emits += 1;
            }
        }
        assert_eq!(emits, 4);
    }

    #[test]
    fn deemphasis_filter_attenuates_a_step_response() {
        let mut filter = DeemphasisFilter::new(62_000, 75e-6);
        let first = filter.push(1000.0);
        let second = filter.push(0.0);
        assert!(first > 0.0);
        assert!(second.abs() < first.abs());
    }

    #[test]
    fn fm_demod_emits_audio_near_output_rate() {
        let mut demod = super::FmDemod::new();
        let mut emits = 0;
        for _ in 0..40_000 {
            if demod.push_complex(1.0, 0.0).is_some() {
                emits += 1;
            }
        }
        let expected = (40_000.0 * super::DEFAULT_SAMPLE_RATE_HZ as f64 / super::SDR_SAMPLE_RATE_HZ as f64).round() as i32;
        // Tolerance covers the multi-stage FIR warmup latency (the
        // channel + audio decimators each emit nothing until their
        // history fills), which sits the steady-state count a handful
        // of samples below the ideal ratio.
        assert!((emits as i32 - expected).abs() <= 24);
    }

    #[test]
    fn output_resampler_matches_target_rate_ratio() {
        let mut resampler = OutputRateResampler::new(1_488_375, 44_100);
        let mut emits = 0;
        for _ in 0..1_000 {
            if resampler.should_emit() {
                emits += 1;
            }
        }
        assert!((emits as i32 - 29).abs() <= 1);
    }

    #[test]
    fn rds_syndrome_equals_offset_word_for_encoded_blocks() {
        fn checkword(info: u16) -> u16 {
            let value = (info as u32) << 10;
            let mut reg: u32 = 0;
            for i in (0..26).rev() {
                reg = (reg << 1) | ((value >> i) & 1);
                if reg & (1 << 10) != 0 {
                    reg ^= super::RDS_POLY;
                }
            }
            (reg & 0x3FF) as u16
        }
        let offsets = [
            super::RDS_OFFSET_A,
            super::RDS_OFFSET_B,
            super::RDS_OFFSET_C,
            super::RDS_OFFSET_CP,
            super::RDS_OFFSET_D,
        ];
        for offset in offsets {
            for info in [0x0000u16, 0x1234, 0xABCD, 0xFFFF, 0x5A5A] {
                let block = ((info as u32) << 10) | ((checkword(info) ^ offset) as u32);
                assert_eq!(super::rds_syndrome(block), offset);
            }
        }
    }

    #[test]
    fn rds_group_decoder_extracts_program_service_from_group0() {
        fn checkword(info: u16) -> u16 {
            let value = (info as u32) << 10;
            let mut reg: u32 = 0;
            for i in (0..26).rev() {
                reg = (reg << 1) | ((value >> i) & 1);
                if reg & (1 << 10) != 0 {
                    reg ^= super::RDS_POLY;
                }
            }
            (reg & 0x3FF) as u16
        }
        fn push_block(dec: &mut super::RdsGroupDecoder, info: u16, offset: u16) -> Option<String> {
            let block = ((info as u32) << 10) | ((checkword(info) ^ offset) as u32);
            let mut out = None;
            for i in (0..26).rev() {
                let bit = ((block >> i) & 1) as u8;
                if let Some(super::RdsUpdate::ProgramService(ps)) = dec.push_bit(bit) {
                    out = Some(ps);
                }
            }
            out
        }

        let ps = b"TESTPS12";
        let mut dec = super::RdsGroupDecoder::new();
        let mut emitted = None;
        for _ in 0..3 {
            for seg in 0u16..4 {
                // Block A: PI code. Block B: group 0A with the PS segment
                // address in its low two bits. Block C: unused here.
                push_block(&mut dec, 0x1234, super::RDS_OFFSET_A);
                push_block(&mut dec, seg, super::RDS_OFFSET_B);
                push_block(&mut dec, 0x0000, super::RDS_OFFSET_C);
                let c0 = ps[(seg * 2) as usize] as u16;
                let c1 = ps[(seg * 2 + 1) as usize] as u16;
                if let Some(text) = push_block(&mut dec, (c0 << 8) | c1, super::RDS_OFFSET_D) {
                    emitted = Some(text);
                }
            }
        }
        assert_eq!(emitted.as_deref(), Some("TESTPS12"));
    }

    #[test]
    fn rds_group_decoder_rejects_corrupt_blocks() {
        let mut dec = super::RdsGroupDecoder::new();
        // Feed random-ish bits that never form a CRC-valid A block; no PS
        // text must ever be emitted.
        let mut state: u32 = 0x1234_5678;
        for _ in 0..10_000 {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let bit = ((state >> 31) & 1) as u8;
            assert!(dec.push_bit(bit).is_none());
        }
    }

    #[test]
    fn rds_channel_decodes_synthesized_biphase_baseband() {
        fn checkword(info: u16) -> u16 {
            let value = (info as u32) << 10;
            let mut reg: u32 = 0;
            for i in (0..26).rev() {
                reg = (reg << 1) | ((value >> i) & 1);
                if reg & (1 << 10) != 0 {
                    reg ^= super::RDS_POLY;
                }
            }
            (reg & 0x3FF) as u16
        }
        fn push_block_bits(bits: &mut Vec<u8>, info: u16, offset: u16) {
            let block = ((info as u32) << 10) | ((checkword(info) ^ offset) as u32);
            for i in (0..26).rev() {
                bits.push(((block >> i) & 1) as u8);
            }
        }

        // Build the data-bit stream for repeated group-0A frames.
        let ps = b"TESTPS12";
        let mut data_bits: Vec<u8> = Vec::new();
        for _ in 0..8 {
            for seg in 0u16..4 {
                push_block_bits(&mut data_bits, 0x1234, super::RDS_OFFSET_A);
                push_block_bits(&mut data_bits, seg, super::RDS_OFFSET_B);
                push_block_bits(&mut data_bits, 0x0000, super::RDS_OFFSET_C);
                let c0 = ps[(seg * 2) as usize] as u16;
                let c1 = ps[(seg * 2 + 1) as usize] as u16;
                push_block_bits(&mut data_bits, (c0 << 8) | c1, super::RDS_OFFSET_D);
            }
        }

        // Differential encode (inverse of the decoder's XOR): C[n] = C[n-1] ^ D[n].
        let mut prev = 0u8;
        let mut chan_bits = Vec::with_capacity(data_bits.len());
        for &d in &data_bits {
            let c = prev ^ d;
            chan_bits.push(c);
            prev = c;
        }

        // Bi-phase (Manchester) encode: channel bit 1 -> chips (+,-),
        // 0 -> (-,+), matching the decoder's mid-symbol transition rule.
        let mut chips: Vec<f32> = Vec::with_capacity(chan_bits.len() * 2);
        for &c in &chan_bits {
            if c == 1 {
                chips.push(1.0);
                chips.push(-1.0);
            } else {
                chips.push(-1.0);
                chips.push(1.0);
            }
        }

        // Feed the chips as a baseband waveform at the true fractional
        // chip rate. A fixed 40deg carrier phase offset splits each chip
        // across the I/Q "mixer" inputs (cos3/sin3), forcing the Costas
        // loop to actually rotate the constellation back onto I -- a
        // pure real (sin3=0) feed would leave the loop untested.
        let fs = super::SDR_SAMPLE_RATE_HZ as f32 / super::CHANNEL_DECIM as f32;
        let samples_per_chip = fs / (2.0 * super::RDS_BAUD_RATE_BPS);
        let total = (chips.len() as f32 * samples_per_chip) as usize;
        let phase = 40.0f32.to_radians();
        let (sin3, cos3) = phase.sin_cos();
        let mut ch = super::RdsChannel::new(fs);
        let mut emitted = None;
        for n in 0..total {
            let idx = (n as f32 / samples_per_chip) as usize;
            if idx >= chips.len() {
                break;
            }
            if let Some(super::RdsUpdate::ProgramService(text)) =
                ch.push(chips[idx], cos3, sin3)
            {
                emitted = Some(text);
            }
        }
        assert_eq!(emitted.as_deref(), Some("TESTPS12"));
    }

    #[test]
    fn rds_group_decoder_assembles_radiotext_from_group2a() {
        fn checkword(info: u16) -> u16 {
            let value = (info as u32) << 10;
            let mut reg: u32 = 0;
            for i in (0..26).rev() {
                reg = (reg << 1) | ((value >> i) & 1);
                if reg & (1 << 10) != 0 {
                    reg ^= super::RDS_POLY;
                }
            }
            (reg & 0x3FF) as u16
        }
        fn push_block(
            dec: &mut super::RdsGroupDecoder,
            info: u16,
            offset: u16,
        ) -> Option<super::RdsUpdate> {
            let block = ((info as u32) << 10) | ((checkword(info) ^ offset) as u32);
            let mut out = None;
            for i in (0..26).rev() {
                if let Some(u) = dec.push_bit(((block >> i) & 1) as u8) {
                    out = Some(u);
                }
            }
            out
        }

        // 64-character RadioText field: the message, a 0x0D terminator,
        // then space padding. Assembly must stop at the terminator.
        let message = b"Now Playing: Song Title";
        let char_at = |p: usize| -> u8 {
            if p < message.len() {
                message[p]
            } else if p == message.len() {
                0x0D
            } else {
                b' '
            }
        };

        let mut dec = super::RdsGroupDecoder::new();
        let mut emitted = None;
        // Three passes: two to satisfy per-character double-confirmation,
        // a margin third to ensure the assembled string is emitted.
        for _ in 0..3 {
            for addr in 0u16..16 {
                push_block(&mut dec, 0x1234, super::RDS_OFFSET_A);
                // Group 2A (type=2, version A), Text A/B flag = 0, address.
                push_block(&mut dec, 0x2000 | addr, super::RDS_OFFSET_B);
                let base = (addr * 4) as usize;
                let c = ((char_at(base) as u16) << 8) | char_at(base + 1) as u16;
                push_block(&mut dec, c, super::RDS_OFFSET_C);
                let d = ((char_at(base + 2) as u16) << 8) | char_at(base + 3) as u16;
                if let Some(super::RdsUpdate::RadioText(t)) =
                    push_block(&mut dec, d, super::RDS_OFFSET_D)
                {
                    emitted = Some(t);
                }
            }
        }
        assert_eq!(emitted.as_deref(), Some("Now Playing: Song Title"));
    }

}
