//! iq_replay — rate-paced cu8 I/Q file → stdout pipe.
//!
//! Reads a cu8 I/Q file and writes it to stdout at the specified sample
//! rate (default 1_488_000 sps — the HD Radio NRSC-5 OFDM rate). Uses
//! cumulative-target wall-clock pacing so a 60 s recording drains in
//! ~60 s regardless of chunk-timing jitter on Windows' ~15 ms scheduler.
//!
//! Intended use as the Phase-3 producer in the v0.2.0 Spike 0 plan
//! (`/memories/session/plan.md`):
//!
//!     target\iq_replay.exe target\spike0-iq.cu8 | bin\nrsc5.exe -r - 0
//!
//! Build (no Cargo manifest needed — keeps this out of the workspace
//! dependency graph so it stays a throwaway dev tool):
//!
//!     rustc -O scripts\iq_replay.rs -o target\iq_replay.exe
//!
//! This file is intentionally std-only and not a workspace member.

use std::env;
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::process::ExitCode;
use std::thread;
use std::time::{Duration, Instant};

// Win32 multimedia timer API. Calling `timeBeginPeriod(1)` requests a
// 1 ms scheduler resolution for the whole process, which is what every
// real-time audio app on Windows does. Without it, `thread::sleep` is
// quantized to ~15.6 ms (the default `USER_TIMER_MAXIMUM`).
//
// Must be balanced by a matching `timeEndPeriod(1)` on shutdown so we
// don't leave the system in high-resolution mode (battery cost on
// laptops). Declared inline rather than pulling a `windows-sys` crate
// to keep this throwaway tool std-only.
#[cfg(windows)]
#[link(name = "winmm")]
unsafe extern "system" {
    fn timeBeginPeriod(uPeriod: u32) -> u32;
    fn timeEndPeriod(uPeriod: u32) -> u32;
}

/// Read/write granularity. ~86 ms of cu8 at 1.488 MS/s — sized large
/// enough that Windows scheduler jitter (which can overshoot
/// `thread::sleep` by 5-15 ms even with `timeBeginPeriod(1)` active)
/// stays a small fraction of each per-chunk wait. Combined with the
/// hybrid sleep+spin in `sleep_until` and cumulative-target pacing,
/// this keeps the long-term drift comfortably under 1 %.
///
/// nrsc5's internal OFDM input buffer absorbs > 100 ms of jitter
/// cleanly, so 86 ms bursts don't cause downstream problems.
const CHUNK_BYTES: usize = 256 * 1024;

/// cu8 packs one byte each for I and Q.
const BYTES_PER_SAMPLE: u64 = 2;

/// Default sample rate. Matches the rate nrsc5's `-w` flag emits and
/// what `-r` expects on the other end.
const DEFAULT_RATE_HZ: u64 = 1_488_000;

fn main() -> ExitCode {
    // Request 1 ms scheduler resolution for the duration of this process.
    // Balanced by `timeEndPeriod(1)` below before every exit path.
    #[cfg(windows)]
    // SAFETY: winmm is a standard Windows system DLL; timeBeginPeriod is
    // a documented no-side-effect call that simply requests scheduler
    // resolution. Always paired with a matching `timeEndPeriod` below.
    unsafe {
        timeBeginPeriod(1);
    }
    let code = run();
    #[cfg(windows)]
    // SAFETY: see above; must always run regardless of success/failure to
    // restore the system's default timer resolution.
    unsafe {
        timeEndPeriod(1);
    }
    code
}

fn run() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 || args.len() > 3 {
        eprintln!(
            "usage: {} <input.cu8> [sample_rate_hz]\n   default sample_rate = {}",
            args.first().map(String::as_str).unwrap_or("iq_replay"),
            DEFAULT_RATE_HZ,
        );
        return ExitCode::from(2);
    }

    let path = &args[1];
    let rate_hz: u64 = match args.get(2) {
        Some(s) => match s.parse::<u64>() {
            Ok(v) if v > 0 => v,
            _ => {
                eprintln!("error: invalid sample_rate '{}'", s);
                return ExitCode::from(2);
            }
        },
        None => DEFAULT_RATE_HZ,
    };

    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("error: cannot open {}: {}", path, e);
            return ExitCode::from(1);
        }
    };

    let bytes_per_sec = rate_hz * BYTES_PER_SAMPLE;
    let mut reader = BufReader::with_capacity(CHUNK_BYTES * 4, file);
    let stdout = io::stdout().lock();
    // Match BufWriter capacity to chunk size so every write_all triggers a
    // single pipe write — keeps bytes flowing to nrsc5 at the same cadence
    // as our pacing decisions instead of being merged across chunks.
    let mut writer = BufWriter::with_capacity(CHUNK_BYTES, stdout);
    let mut buf = vec![0u8; CHUNK_BYTES];
    let mut bytes_written: u64 = 0;
    let start = Instant::now();

    loop {
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => {
                eprintln!("error: read failed: {}", e);
                return ExitCode::from(1);
            }
        };

        if let Err(e) = writer.write_all(&buf[..n]) {
            // EPIPE / broken-pipe = consumer hung up (nrsc5 got killed, the
            // shell closed, etc.). That's a normal exit path, not an error.
            if e.kind() == io::ErrorKind::BrokenPipe {
                return ExitCode::SUCCESS;
            }
            eprintln!("error: write failed: {}", e);
            return ExitCode::from(1);
        }
        bytes_written += n as u64;

        // Cumulative target: where wall-clock *should* be by now if we'd
        // been emitting at exactly `bytes_per_sec`. Sleeping against this
        // cumulative target means per-chunk jitter doesn't accumulate into
        // long-term drift (drift would underrun nrsc5's sample clock).
        let target_secs = bytes_written as f64 / bytes_per_sec as f64;
        let target = Duration::from_secs_f64(target_secs);
        sleep_until(start, target);
    }

    if let Err(e) = writer.flush() {
        if e.kind() != io::ErrorKind::BrokenPipe {
            eprintln!("error: flush failed: {}", e);
            return ExitCode::from(1);
        }
    }
    // Internal-time diagnostic to stderr. The expected time is what an
    // ideal sample-rate-perfect pacer would have taken; the actual is our
    // measured wall-clock from inside the program. The difference between
    // this and an external `Measure-Command` lap is process startup.
    let expected_s = bytes_written as f64 / bytes_per_sec as f64;
    let actual_s = start.elapsed().as_secs_f64();
    eprintln!(
        "iq_replay: wrote {} bytes in {:.3} s (expected {:.3} s, drift {:+.2}%)",
        bytes_written,
        actual_s,
        expected_s,
        (actual_s - expected_s) / expected_s * 100.0,
    );
    ExitCode::SUCCESS
}

/// Wait until `start.elapsed() >= target` with sub-millisecond accuracy.
///
/// Windows' `thread::sleep` overshoots by up to ~1 ms even at 1 ms timer
/// resolution, and on default 15.6 ms resolution it overshoots by up to
/// ~16 ms — either of which compounds into multi-percent pacing drift
/// over a one-minute capture. To avoid that, sleep for `remaining - 1 ms`
/// (letting the OS do the bulk of the wait at near-zero CPU), then
/// busy-spin the final < 1 ms. Worst-case spin CPU is ~46 chunks/sec
/// × ~1 ms = under 5 % during replay.
fn sleep_until(start: Instant, target: Duration) {
    loop {
        let now = start.elapsed();
        if now >= target {
            return;
        }
        let remaining = target - now;
        if remaining > Duration::from_millis(2) {
            thread::sleep(remaining - Duration::from_millis(1));
        } else {
            // Sub-2ms tail: spin for accuracy. `spin_loop` is a hint to
            // the CPU to back off slightly (hyperthread yield etc.) so
            // this isn't a hot busy-loop on every available core.
            std::hint::spin_loop();
        }
    }
}
