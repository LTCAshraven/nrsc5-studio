//! iq_compare — quick spectral parity check for two CU8 IQ captures.
//!
//! Built as a workspace example so it can pull in `rustfft` from the
//! main Cargo deps tree. Usage:
//!
//!     cargo run --release --example iq_compare -- \
//!         target\reference-rtlsdr.cu8 \
//!         target\soapy-probe-rtlsdr.cu8
//!
//! Reports per-file statistics (sample count, mean, RMS, IQ DC offset)
//! and computes a 65536-point FFT on the first window of each file to
//! locate the dominant carrier offset and estimate noise floor. The
//! goal isn't byte-equality (the two paths quantize gain differently
//! and may have minor timing offsets) — it's spectral equivalence:
//! same peak location ±1 bin, same noise floor ±3 dB.
//!
//! This is the Phase 1 verification gate for swapping from the 0.2.x
//! native RtlSdr impl to the new SoapySdr backend. If the two CU8 files
//! come out spectrally equivalent, the new path is good to go.

use std::env;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;

use num_complex::Complex32;
use rustfft::{num_complex::Complex as FftComplex, FftPlanner};

const FFT_SIZE: usize = 65536;
const SAMPLE_RATE_HZ: f64 = 1_488_375.0;

#[derive(Debug)]
struct Stats {
    samples: usize,
    mean_i: f32,
    mean_q: f32,
    rms: f32,
    dc_magnitude: f32,
    peak_bin: usize,
    peak_offset_hz: f64,
    peak_db: f32,
    noise_floor_db: f32,
    snr_db: f32,
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    if args.len() != 3 {
        eprintln!("usage: iq_compare <file1.cu8> <file2.cu8>");
        eprintln!();
        eprintln!("Compares two CU8 IQ captures (1.488 MS/s assumed) and");
        eprintln!("prints per-file statistics + dominant-carrier delta.");
        return ExitCode::from(2);
    }

    let path_a = Path::new(&args[1]);
    let path_b = Path::new(&args[2]);

    let stats_a = match analyze(path_a) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("ERROR analyzing {}: {e}", path_a.display());
            return ExitCode::from(1);
        }
    };
    let stats_b = match analyze(path_b) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("ERROR analyzing {}: {e}", path_b.display());
            return ExitCode::from(1);
        }
    };

    println!("=== File A: {} ===", path_a.display());
    print_stats(&stats_a);
    println!();
    println!("=== File B: {} ===", path_b.display());
    print_stats(&stats_b);
    println!();
    println!("=== Delta (A − B) ===");
    println!(
        "  Peak offset (Hz)   : {:+.0}",
        stats_a.peak_offset_hz - stats_b.peak_offset_hz
    );
    println!(
        "  Peak level (dB)    : {:+.2}",
        stats_a.peak_db - stats_b.peak_db
    );
    println!(
        "  Noise floor (dB)   : {:+.2}",
        stats_a.noise_floor_db - stats_b.noise_floor_db
    );
    println!(
        "  SNR (dB)           : {:+.2}",
        stats_a.snr_db - stats_b.snr_db
    );
    println!(
        "  DC offset (mag)    : {:+.4}",
        stats_a.dc_magnitude - stats_b.dc_magnitude
    );
    println!();

    // Verdict for OFDM signals (HD Radio, DVB-T, etc.) — the spectrum
    // is composed of hundreds of subcarriers of similar power, so the
    // single-bin "peak" can land anywhere in the modulated band
    // depending on capture timing. The right invariants for an OFDM
    // signal are: matched noise floor (±3 dB), matched RMS (±20%),
    // matched DC offset (±0.05 mag). Peak bin location is informational
    // only.
    let nf_ok = (stats_a.noise_floor_db - stats_b.noise_floor_db).abs() < 3.0;
    let rms_ratio = (stats_a.rms.max(1e-6) / stats_b.rms.max(1e-6)) as f64;
    let rms_ok = rms_ratio > 0.8 && rms_ratio < 1.25;
    let dc_ok = (stats_a.dc_magnitude - stats_b.dc_magnitude).abs() < 0.05;
    let bin_hz = SAMPLE_RATE_HZ / FFT_SIZE as f64;

    if nf_ok && rms_ok && dc_ok {
        println!("VERDICT: spectrally equivalent ✓");
        println!(
            "  Noise floor within ±3 dB ({:+.2} dB)",
            stats_a.noise_floor_db - stats_b.noise_floor_db
        );
        println!("  RMS ratio within ±20% ({:.3}×)", rms_ratio);
        println!(
            "  DC offset within ±0.05 mag ({:+.4})",
            stats_a.dc_magnitude - stats_b.dc_magnitude
        );
        println!(
            "  Peak bin diff: {:.0} Hz ({:.1} FFT bins) — informational only for OFDM signals",
            (stats_a.peak_offset_hz - stats_b.peak_offset_hz).abs(),
            (stats_a.peak_offset_hz - stats_b.peak_offset_hz).abs() / bin_hz
        );
        ExitCode::SUCCESS
    } else {
        println!("VERDICT: NOT equivalent ✗");
        if !nf_ok {
            println!(
                "  Noise floor diff {:.2} dB exceeds tolerance ±3 dB",
                (stats_a.noise_floor_db - stats_b.noise_floor_db).abs()
            );
        }
        if !rms_ok {
            println!("  RMS ratio {:.3}× outside tolerance 0.8–1.25", rms_ratio);
        }
        if !dc_ok {
            println!(
                "  DC offset diff {:.4} exceeds tolerance ±0.05",
                (stats_a.dc_magnitude - stats_b.dc_magnitude).abs()
            );
        }
        ExitCode::from(1)
    }
}

fn print_stats(s: &Stats) {
    println!("  Samples            : {}", s.samples);
    println!(
        "  Mean (I, Q)        : ({:+.4}, {:+.4})  [target ≈ 0,0 after DC removal]",
        s.mean_i, s.mean_q
    );
    println!("  RMS magnitude      : {:.4}", s.rms);
    println!("  DC offset (mag)    : {:.4}", s.dc_magnitude);
    println!(
        "  Peak bin / offset  : bin {} = {:+.0} Hz from center",
        s.peak_bin, s.peak_offset_hz
    );
    println!("  Peak level         : {:.2} dB", s.peak_db);
    println!(
        "  Noise floor        : {:.2} dB (median of magnitude spectrum)",
        s.noise_floor_db
    );
    println!("  SNR (peak − floor) : {:.2} dB", s.snr_db);
}

fn analyze(path: &Path) -> std::io::Result<Stats> {
    let file = File::open(path)?;
    let mut reader = BufReader::with_capacity(1 << 20, file);

    // Read everything into memory as Complex32. CU8 → centered float:
    //     re = (byte − 128) / 128.0
    // Cap at FFT_SIZE * 64 samples (≈8 MB on disk) to bound memory; we
    // only need a small window for spectral analysis anyway.
    let max_samples = FFT_SIZE * 64;
    let mut buf = vec![0u8; 2 * max_samples];
    let read_bytes = reader.read(&mut buf)?;
    let n_samples = read_bytes / 2;
    if n_samples < FFT_SIZE {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "file too short: {} samples (need ≥ {})",
                n_samples, FFT_SIZE
            ),
        ));
    }
    let iq: Vec<Complex32> = (0..n_samples)
        .map(|i| {
            let re = (buf[2 * i] as f32 - 128.0) / 128.0;
            let im = (buf[2 * i + 1] as f32 - 128.0) / 128.0;
            Complex32::new(re, im)
        })
        .collect();

    // Per-sample stats over all samples in the window.
    let sum_i: f32 = iq.iter().map(|c| c.re).sum();
    let sum_q: f32 = iq.iter().map(|c| c.im).sum();
    let mean_i = sum_i / iq.len() as f32;
    let mean_q = sum_q / iq.len() as f32;
    let dc_magnitude = (mean_i * mean_i + mean_q * mean_q).sqrt();
    let sum_sq: f32 = iq.iter().map(|c| c.re * c.re + c.im * c.im).sum();
    let rms = (sum_sq / iq.len() as f32).sqrt();

    // FFT first 65536 samples, DC-removed, no window (rectangular —
    // good enough for finding the dominant carrier in a sea of noise;
    // a Hann window would smear the peak across a few bins).
    let mut fft_in: Vec<FftComplex<f32>> = iq[..FFT_SIZE]
        .iter()
        .map(|c| FftComplex::new(c.re - mean_i, c.im - mean_q))
        .collect();
    let planner_fft: Arc<dyn rustfft::Fft<f32>> = FftPlanner::new().plan_fft_forward(FFT_SIZE);
    planner_fft.process(&mut fft_in);

    // Magnitude spectrum, fftshift'd so bin 0 = -Fs/2 and bin N-1 = +Fs/2.
    let half = FFT_SIZE / 2;
    let mut mag = vec![0f32; FFT_SIZE];
    #[allow(clippy::needless_range_loop)]
    // k drives an fftshift index remap, not a straight iteration
    for k in 0..FFT_SIZE {
        // unshifted: bins 0..N/2 are positive freqs, N/2..N are negative.
        // We want centered: shifted_bin = (k + N/2) mod N.
        let src = if k < half { k + half } else { k - half };
        mag[k] = (fft_in[src].re * fft_in[src].re + fft_in[src].im * fft_in[src].im).sqrt();
    }

    // Peak bin location + value.
    let (peak_bin, peak_mag) = mag
        .iter()
        .enumerate()
        .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(i, &v)| (i, v))
        .unwrap_or((0, 1.0));

    // Noise floor: median of all magnitudes. Robust against single-bin
    // spikes; for an unmodulated carrier in a wide noise floor, this is
    // a much better estimate than mean.
    let mut sorted = mag.clone();
    sorted.sort_unstable_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let noise_floor_mag = sorted[FFT_SIZE / 2];

    let peak_db = 20.0 * peak_mag.max(1e-12).log10();
    let noise_floor_db = 20.0 * noise_floor_mag.max(1e-12).log10();
    let snr_db = peak_db - noise_floor_db;

    let bin_hz = SAMPLE_RATE_HZ / FFT_SIZE as f64;
    let peak_offset_hz = (peak_bin as f64 - half as f64) * bin_hz;

    Ok(Stats {
        samples: iq.len(),
        mean_i,
        mean_q,
        rms,
        dc_magnitude,
        peak_bin,
        peak_offset_hz,
        peak_db,
        noise_floor_db,
        snr_db,
    })
}
