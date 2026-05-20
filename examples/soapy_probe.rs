//! `soapy_probe` — Phase 1.5 smoke test for the `SoapySdr` backend.
//!
//! Enumerates every device libSoapySDR can see, opens one, captures a
//! few seconds of CU8 IQ to a file, and prints a sanity summary. The
//! goal is to confirm three things before any UI work:
//!
//!   1. `soapysdr` linked against the bundled libSoapySDR successfully.
//!   2. `SOAPY_SDR_PLUGIN_PATH` finds the bundled module DLLs and the
//!      target device enumerates.
//!   3. The CS8/CS16 → CU8 conversion produces IQ that's spectrally
//!      equivalent to what `RtlSdr` (the existing librtlsdr binding)
//!      writes — diff the output against `iq_capture` from `scripts\`
//!      with the same frequency / duration / gain.
//!
//! Usage (from repo root, after `scripts\install-soapysdr-msys2.ps1`):
//!
//! ```
//! cargo run --example soapy_probe -- --driver=rtlsdr
//! cargo run --example soapy_probe -- --driver=rtlsdr --args=rtl_tcp=127.0.0.1:1234
//! cargo run --example soapy_probe -- --driver=sdrplay --freq=97.1 --duration=10
//! ```
//!
//! Output file path defaults to `target\soapy-probe-<driver>.cu8`. With
//! the default settings (1.488 MS/s × 10 s × 2 bytes/sample) the file
//! should be ~29.76 MB.

use std::env;
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::time::Instant;

use nrsc5_studio::sdr::{Sdr, SdrConfig, SoapySdr, StreamControl};

const SAMPLE_RATE_SPS: u32 = 1_488_375;

fn main() {
    let args = ProbeArgs::parse();

    println!("=== SoapySdr smoke test ===");
    println!();
    println!("Enumerating devices (this may take 1–2 seconds)...");
    let devices = SoapySdr::enumerate_devices();
    if devices.is_empty() {
        println!("  (no devices found)");
        println!();
        println!("Things to check:");
        println!("  * Is your SDR connected and powered?");
        println!("  * Are the right WinUSB drivers installed (Zadig for RTL-SDR)?");
        println!("  * Are the Soapy module DLLs in bin\\SoapySDR\\modules0.8\\?");
        println!("  * Is SOAPY_SDR_PLUGIN_PATH pointing at the modules dir?");
        std::process::exit(1);
    }
    for (i, d) in devices.iter().enumerate() {
        println!(
            "  [{i}] driver={:<12} label={:<40} args={}",
            d.driver, d.label, d.device_args
        );
    }
    println!();

    // Build the Soapy args string. If the user supplied `--args=X`, prepend
    // `driver=Y,` so they don't have to repeat the driver key. Otherwise
    // use the driver key alone, which opens the first device of that type.
    let open_args = if args.extra_args.is_empty() {
        format!("driver={}", args.driver)
    } else {
        format!("driver={},{}", args.driver, args.extra_args)
    };

    println!("Opening device: {open_args}");
    let sdr = match SoapySdr::open(&open_args) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("  ERROR: {e}");
            std::process::exit(2);
        }
    };
    println!("  driver: {}", sdr.driver());
    println!("  label:  {}", sdr.label());
    println!("  gain elements: {:?}", sdr.gain_element_names());
    println!();

    let cfg = SdrConfig {
        center_freq_hz: (args.freq_mhz * 1_000_000.0) as u32,
        sample_rate_sps: SAMPLE_RATE_SPS,
        ppm_correction: 0,
        direct_sampling: 0,
        initial_gain_tenths: Some(args.gain_tenths),
    };
    println!(
        "Configuring: freq={:.3} MHz, rate={} sps, gain={:.1} dB",
        args.freq_mhz,
        SAMPLE_RATE_SPS,
        args.gain_tenths as f32 / 10.0
    );
    if let Err(e) = sdr.configure(&cfg) {
        eprintln!("  ERROR: {e}");
        std::process::exit(3);
    }

    // Output file lives under target/ alongside the other capture
    // artifacts (smoke.cu8, synthetic-10s.cu8 etc).
    let out_path = match args.output {
        Some(p) => p,
        None => {
            let mut p = PathBuf::from("target");
            std::fs::create_dir_all(&p).ok();
            p.push(format!("soapy-probe-{}.cu8", args.driver));
            p
        }
    };
    println!("Output: {}", out_path.display());

    let mut file = match File::create(&out_path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("  ERROR creating {}: {e}", out_path.display());
            std::process::exit(4);
        }
    };

    // The run_stream callback writes bytes to the file and stops when
    // we've collected `duration_sec` worth (or the user Ctrl-C's, but
    // we don't install a signal handler here — Ctrl-C just kills the
    // whole process, dropping the SDR mid-stream which is fine).
    let expected_bytes = SAMPLE_RATE_SPS as usize * 2 * args.duration_sec as usize;
    let mut bytes_written: usize = 0;
    let start = Instant::now();

    println!(
        "Capturing {duration} s (~{expected_mb:.2} MB)...",
        duration = args.duration_sec,
        expected_mb = expected_bytes as f64 / 1_048_576.0
    );

    let result = sdr.run_stream(&mut |buf| {
        if file.write_all(buf).is_err() {
            return StreamControl::Stop;
        }
        bytes_written += buf.len();
        if bytes_written >= expected_bytes {
            StreamControl::Stop
        } else {
            StreamControl::Continue
        }
    });

    let elapsed = start.elapsed();
    println!();
    match result {
        Ok(()) => println!("Stream ended cleanly."),
        Err(e) => println!("Stream ended with error: {e}"),
    }

    let actual_mb = bytes_written as f64 / 1_048_576.0;
    let expected_mb = expected_bytes as f64 / 1_048_576.0;
    let drift_pct = ((actual_mb - expected_mb) / expected_mb) * 100.0;
    println!();
    println!("=== Summary ===");
    println!("  Captured:  {bytes_written} bytes  ({actual_mb:.3} MB)");
    println!("  Expected:  {expected_bytes} bytes ({expected_mb:.3} MB)");
    println!("  Drift:     {drift_pct:+.2}%");
    println!("  Elapsed:   {:.3} s", elapsed.as_secs_f64());
    println!(
        "  Effective: {:.3} MS/s",
        (bytes_written as f64 / 2.0) / elapsed.as_secs_f64() / 1_000_000.0
    );
    println!();
    println!("File saved to {}", out_path.display());
    println!();
    println!("Next: compare spectrally against an iq_capture reference.");
    println!("      Same freq/duration via scripts\\iq_capture.rs should");
    println!("      produce equivalent IQ distribution (not byte-equal —");
    println!("      gain quantization may differ — but spectrally identical).");
}

#[derive(Debug)]
struct ProbeArgs {
    driver: String,
    extra_args: String,
    freq_mhz: f64,
    gain_tenths: i32,
    duration_sec: u32,
    output: Option<PathBuf>,
}

impl ProbeArgs {
    fn parse() -> Self {
        let mut out = ProbeArgs {
            driver: "rtlsdr".to_string(),
            extra_args: String::new(),
            freq_mhz: 97.1,
            gain_tenths: 197,
            duration_sec: 10,
            output: None,
        };
        for raw in env::args().skip(1) {
            let arg = raw.as_str();
            if let Some(v) = arg.strip_prefix("--driver=") {
                out.driver = v.to_string();
            } else if let Some(v) = arg.strip_prefix("--args=") {
                out.extra_args = v.to_string();
            } else if let Some(v) = arg.strip_prefix("--freq=") {
                out.freq_mhz = v.parse().unwrap_or_else(|_| {
                    eprintln!("invalid --freq= value (expecting MHz, e.g. 97.1)");
                    std::process::exit(64);
                });
            } else if let Some(v) = arg.strip_prefix("--gain=") {
                let dbf: f64 = v.parse().unwrap_or_else(|_| {
                    eprintln!("invalid --gain= value (expecting dB, e.g. 19.7)");
                    std::process::exit(64);
                });
                out.gain_tenths = (dbf * 10.0).round() as i32;
            } else if let Some(v) = arg.strip_prefix("--duration=") {
                out.duration_sec = v.parse().unwrap_or_else(|_| {
                    eprintln!("invalid --duration= value (expecting seconds)");
                    std::process::exit(64);
                });
            } else if let Some(v) = arg.strip_prefix("--output=") {
                out.output = Some(PathBuf::from(v));
            } else if arg == "--help" || arg == "-h" {
                print_help();
                std::process::exit(0);
            } else {
                eprintln!("unknown arg: {arg}");
                eprintln!("(run with --help for usage)");
                std::process::exit(64);
            }
        }
        out
    }
}

fn print_help() {
    println!("soapy_probe — Phase 1.5 smoke test for the SoapySdr backend");
    println!();
    println!("USAGE:");
    println!("  cargo run --example soapy_probe -- [options]");
    println!();
    println!("OPTIONS:");
    println!("  --driver=<key>       Soapy driver key (default: rtlsdr)");
    println!("                       Examples: rtlsdr, sdrplay, airspy, hackrf, remote");
    println!("  --args=<extra>       Extra Soapy device args (e.g. rtl_tcp=127.0.0.1:1234)");
    println!("  --freq=<MHz>         Center frequency in MHz (default: 97.1)");
    println!("  --gain=<dB>          Tuner gain in dB (default: 19.7)");
    println!("  --duration=<sec>     Capture duration in seconds (default: 10)");
    println!("  --output=<path>      Output file (default: target\\soapy-probe-<driver>.cu8)");
    println!("  --help, -h           Show this help and exit");
}
