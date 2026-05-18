//! iq_capture — Spike 1 live RTL-SDR producer (cu8 → stdout).
//!
//! Sibling tool to `iq_replay.rs`: opens an RTL-SDR via `librtlsdr.dll`,
//! tunes it, and streams raw cu8 samples to stdout. Intended to be piped
//! into `nrsc5.exe -r -` to prove the v0.2.0 architecture (we own the
//! radio; nrsc5 becomes a decode-only sink fed via stdin).
//!
//! Build (no Cargo manifest — same pattern as `iq_replay.rs`; the
//! workspace `rust-toolchain.toml` pins gnullvm but `rustc` doesn't read
//! `.cargo/config.toml`, so the bundled llvm-mingw `bin/` needs to be on
//! PATH for the linker to resolve):
//!
//!     $env:PATH = "$pwd\.toolchains\llvm-mingw-20260505-ucrt-x86_64\bin;$env:PATH"
//!     rustc -O scripts\iq_capture.rs -o target\iq_capture.exe
//!
//! Run (Spike 1 acceptance pipe — the canonical use case):
//!
//!     target\iq_capture.exe --freq 97.1 | bin\nrsc5.exe -r - 0 0
//!
//! Args:
//!     --freq MHZ      center frequency in MHz   (default 97.1)
//!     --rate SPS      sample rate in Hz         (default 1488375)
//!     --gain auto|N   tuner gain in dB or auto  (default auto)
//!     --bytes N       stop after N bytes (default: until EPIPE/Ctrl-C)
//!     --device IDX    librtlsdr device index    (default 0)
//!     --ppm N         frequency correction      (default 0)
//!
//! Cancellation model — canonical librtlsdr pattern (see rtl_sdr.c):
//! the worker callback itself calls `rtlsdr_cancel_async` whenever it
//! decides to stop (broken pipe, byte-count exceeded, Ctrl-C flag set).
//! `cancel_async` is documented as safe from any thread. After the call,
//! the next iteration of `read_async`'s event loop tears down the
//! transfers and the function returns. No detached threads, no cross-
//! thread race on the device handle.
//!
//! Windows-only by design; librtlsdr.dll is loaded via `LoadLibraryW`
//! directly so this stays a std-only standalone tool (no libloading
//! dependency, no workspace coupling).

#![cfg(windows)]
#![allow(non_snake_case)]

use std::env;
use std::ffi::c_void;
use std::io::{self, Write};
use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use std::sync::OnceLock;
use std::time::Instant;

// === Win32 FFI (kernel32) ================================================

#[link(name = "kernel32")]
unsafe extern "system" {
    fn LoadLibraryW(lpLibFileName: *const u16) -> *mut c_void;
    fn FreeLibrary(hLibModule: *mut c_void) -> i32;
    fn GetProcAddress(hModule: *mut c_void, lpProcName: *const u8) -> *mut c_void;
    fn SetConsoleCtrlHandler(
        HandlerRoutine: Option<extern "system" fn(u32) -> i32>,
        Add: i32,
    ) -> i32;
}

const CTRL_C_EVENT: u32 = 0;
const CTRL_BREAK_EVENT: u32 = 1;
const CTRL_CLOSE_EVENT: u32 = 2;

// === librtlsdr function-pointer types ====================================

type FnOpen = unsafe extern "C" fn(*mut *mut c_void, u32) -> i32;
type FnClose = unsafe extern "C" fn(*mut c_void) -> i32;
type FnSetCenterFreq = unsafe extern "C" fn(*mut c_void, u32) -> i32;
type FnSetSampleRate = unsafe extern "C" fn(*mut c_void, u32) -> i32;
type FnSetTunerGainMode = unsafe extern "C" fn(*mut c_void, i32) -> i32;
type FnSetTunerGain = unsafe extern "C" fn(*mut c_void, i32) -> i32;
type FnSetFreqCorrection = unsafe extern "C" fn(*mut c_void, i32) -> i32;
type FnResetBuffer = unsafe extern "C" fn(*mut c_void) -> i32;
type FnSetDirectSampling = unsafe extern "C" fn(*mut c_void, i32) -> i32;
type FnSetAgcMode = unsafe extern "C" fn(*mut c_void, i32) -> i32;
type FnCancelAsync = unsafe extern "C" fn(*mut c_void) -> i32;
type FnAsyncCallback = extern "C" fn(*mut u8, u32, *mut c_void);
type FnReadAsync = unsafe extern "C" fn(
    *mut c_void,
    FnAsyncCallback,
    *mut c_void,
    u32, // buf_num — 0 = librtlsdr default (15)
    u32, // buf_len — 0 = librtlsdr default (262144)
) -> i32;

struct Api {
    open: FnOpen,
    close: FnClose,
    set_center_freq: FnSetCenterFreq,
    set_sample_rate: FnSetSampleRate,
    set_tuner_gain_mode: FnSetTunerGainMode,
    set_tuner_gain: FnSetTunerGain,
    set_freq_correction: FnSetFreqCorrection,
    reset_buffer: FnResetBuffer,
    set_direct_sampling: FnSetDirectSampling,
    set_agc_mode: FnSetAgcMode,
    cancel_async: FnCancelAsync,
    read_async: FnReadAsync,
    // Kept-alive HMODULE so the function pointers above stay valid.
    _module: *mut c_void,
}

// SAFETY: `Api` only holds raw function pointers and one HMODULE handle.
// Function pointers are inherently `Send + Sync`; the module handle is
// process-wide. librtlsdr uses libusb's internal locking for thread
// safety.
unsafe impl Send for Api {}
unsafe impl Sync for Api {}

// === Loader ==============================================================

fn find_librtlsdr() -> Option<PathBuf> {
    const DLL: &str = "librtlsdr.dll";
    if let Ok(cwd) = env::current_dir() {
        for cand in [cwd.join("bin").join(DLL), cwd.join(DLL)] {
            if cand.exists() {
                return Some(cand);
            }
        }
    }
    if let Ok(exe) = env::current_exe() {
        if let Some(dir) = exe.parent() {
            for cand in [dir.join("bin").join(DLL), dir.join(DLL)] {
                if cand.exists() {
                    return Some(cand);
                }
            }
        }
    }
    None
}

fn load_api() -> Result<Api, String> {
    let path = find_librtlsdr()
        .ok_or_else(|| "could not locate librtlsdr.dll under ./bin/ or .".to_string())?;
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    // SAFETY: well-formed nul-terminated UTF-16 path; LoadLibraryW returns
    // null on failure, which we check before transmuting symbols.
    let module = unsafe { LoadLibraryW(wide.as_ptr()) };
    if module.is_null() {
        return Err(format!("LoadLibraryW failed for {}", path.display()));
    }

    fn resolve<T: Copy>(module: *mut c_void, name: &[u8]) -> Result<T, String> {
        debug_assert!(name.ends_with(b"\0"));
        // SAFETY: `name` is nul-terminated; we check the result for null.
        let p = unsafe { GetProcAddress(module, name.as_ptr()) };
        if p.is_null() {
            let pretty = std::str::from_utf8(&name[..name.len() - 1]).unwrap_or("?");
            return Err(format!("missing symbol: {}", pretty));
        }
        // SAFETY: each caller specifies `T` as a function-pointer type
        // matching the documented librtlsdr ABI for `name`.
        Ok(unsafe { std::mem::transmute_copy::<*mut c_void, T>(&p) })
    }

    let try_load = || -> Result<Api, String> {
        Ok(Api {
            open: resolve::<FnOpen>(module, b"rtlsdr_open\0")?,
            close: resolve::<FnClose>(module, b"rtlsdr_close\0")?,
            set_center_freq: resolve::<FnSetCenterFreq>(module, b"rtlsdr_set_center_freq\0")?,
            set_sample_rate: resolve::<FnSetSampleRate>(module, b"rtlsdr_set_sample_rate\0")?,
            set_tuner_gain_mode: resolve::<FnSetTunerGainMode>(
                module,
                b"rtlsdr_set_tuner_gain_mode\0",
            )?,
            set_tuner_gain: resolve::<FnSetTunerGain>(module, b"rtlsdr_set_tuner_gain\0")?,
            set_freq_correction: resolve::<FnSetFreqCorrection>(
                module,
                b"rtlsdr_set_freq_correction\0",
            )?,
            reset_buffer: resolve::<FnResetBuffer>(module, b"rtlsdr_reset_buffer\0")?,
            set_direct_sampling: resolve::<FnSetDirectSampling>(
                module,
                b"rtlsdr_set_direct_sampling\0",
            )?,
            set_agc_mode: resolve::<FnSetAgcMode>(module, b"rtlsdr_set_agc_mode\0")?,
            cancel_async: resolve::<FnCancelAsync>(module, b"rtlsdr_cancel_async\0")?,
            read_async: resolve::<FnReadAsync>(module, b"rtlsdr_read_async\0")?,
            _module: module,
        })
    };

    match try_load() {
        Ok(api) => Ok(api),
        Err(e) => {
            // SAFETY: `module` is the value we just got from `LoadLibraryW`;
            // no `Api` was constructed so nothing else references it.
            unsafe {
                FreeLibrary(module);
            }
            Err(e)
        }
    }
}

// === Globals for the Ctrl-C handler ======================================
//
// The callback itself is what calls `cancel_async` — Ctrl-C just trips a
// flag the callback checks on its next invocation. This keeps cancellation
// strictly inside librtlsdr's own worker thread, matching the canonical
// rtl_sdr.c pattern.

static API: OnceLock<Api> = OnceLock::new();
static DEVICE: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static SHOULD_STOP: AtomicBool = AtomicBool::new(false);

extern "system" fn ctrl_handler(ctrl_type: u32) -> i32 {
    match ctrl_type {
        CTRL_C_EVENT | CTRL_BREAK_EVENT | CTRL_CLOSE_EVENT => {
            // Trip the flag; the callback picks it up and cancels from
            // inside the worker thread on its next invocation. Returning 1
            // tells Windows we handled the event (don't terminate).
            SHOULD_STOP.store(true, Ordering::SeqCst);
            1
        }
        _ => 0,
    }
}

// === Callback ctx ========================================================

struct Ctx {
    bytes_written: u64,
    max_bytes: Option<u64>,
    broken_pipe: bool,
    cancelled: bool,
    stdout: io::Stdout,
}

extern "C" fn rtlsdr_cb(buf: *mut u8, len: u32, ctx_raw: *mut c_void) {
    if ctx_raw.is_null() || buf.is_null() || len == 0 {
        return;
    }
    // SAFETY: `ctx_raw` is the `Box::into_raw` pointer set up by main
    // before calling `rtlsdr_read_async`. librtlsdr invokes this callback
    // serially from a single worker thread (no internal aliasing); the
    // main thread is blocked in `read_async` for the duration. The Box is
    // reclaimed only after `read_async` returns and the worker is done.
    let ctx = unsafe { &mut *(ctx_raw as *mut Ctx) };

    // If we've already cancelled, drain remaining transfers silently — the
    // worker is winding down and will exit after these.
    if ctx.cancelled {
        return;
    }

    let cancel_now = if ctx.broken_pipe || SHOULD_STOP.load(Ordering::Acquire) {
        true
    } else {
        // SAFETY: librtlsdr guarantees `buf` is valid for `len` bytes for
        // the duration of this callback.
        let slice = unsafe { std::slice::from_raw_parts(buf, len as usize) };
        let mut handle = ctx.stdout.lock();
        match handle.write_all(slice) {
            Ok(()) => {
                ctx.bytes_written += len as u64;
                matches!(ctx.max_bytes, Some(limit) if ctx.bytes_written >= limit)
            }
            Err(e) => {
                ctx.broken_pipe = true;
                if e.kind() != io::ErrorKind::BrokenPipe {
                    eprintln!("iq_capture: stdout write failed: {}", e);
                }
                true
            }
        }
    };

    if cancel_now {
        // Canonical librtlsdr pattern: callback itself triggers
        // cancellation. Only the first call actually invokes the C func;
        // subsequent in-flight callbacks see `ctx.cancelled` and return.
        ctx.cancelled = true;
        if let Some(api) = API.get() {
            let dev = DEVICE.load(Ordering::SeqCst);
            if !dev.is_null() {
                // SAFETY: `dev` is the handle from `rtlsdr_open`, still
                // valid because main is blocked in `read_async`.
                unsafe {
                    (api.cancel_async)(dev);
                }
            }
        }
    }
}

// === Args ================================================================

#[derive(Debug)]
struct Args {
    freq_hz: u32,
    rate_sps: u32,
    gain: GainMode,
    max_bytes: Option<u64>,
    device_index: u32,
    ppm: i32,
}

#[derive(Debug)]
enum GainMode {
    Auto,
    /// Tenths of a dB, as `rtlsdr_set_tuner_gain` expects.
    Manual(i32),
}

const DEFAULT_FREQ_HZ: u32 = 97_100_000;
const DEFAULT_RATE_SPS: u32 = 1_488_375;

fn parse_args() -> Result<Args, String> {
    let mut out = Args {
        freq_hz: DEFAULT_FREQ_HZ,
        rate_sps: DEFAULT_RATE_SPS,
        gain: GainMode::Auto,
        max_bytes: None,
        device_index: 0,
        ppm: 0,
    };
    let mut iter = env::args().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--freq" => {
                let v: f64 = iter
                    .next()
                    .ok_or_else(|| "--freq expects MHZ".to_string())?
                    .parse()
                    .map_err(|_| "--freq: invalid number".to_string())?;
                out.freq_hz = (v * 1_000_000.0).round() as u32;
            }
            "--rate" => {
                out.rate_sps = iter
                    .next()
                    .ok_or_else(|| "--rate expects SPS".to_string())?
                    .parse()
                    .map_err(|_| "--rate: invalid u32".to_string())?;
            }
            "--gain" => {
                let v = iter
                    .next()
                    .ok_or_else(|| "--gain expects auto|N".to_string())?;
                if v.eq_ignore_ascii_case("auto") {
                    out.gain = GainMode::Auto;
                } else {
                    let db: f32 = v
                        .parse()
                        .map_err(|_| "--gain: invalid number (use auto or a dB value)".to_string())?;
                    out.gain = GainMode::Manual((db * 10.0).round() as i32);
                }
            }
            "--bytes" => {
                out.max_bytes = Some(
                    iter.next()
                        .ok_or_else(|| "--bytes expects N".to_string())?
                        .parse()
                        .map_err(|_| "--bytes: invalid u64".to_string())?,
                );
            }
            "--device" => {
                out.device_index = iter
                    .next()
                    .ok_or_else(|| "--device expects IDX".to_string())?
                    .parse()
                    .map_err(|_| "--device: invalid u32".to_string())?;
            }
            "--ppm" => {
                out.ppm = iter
                    .next()
                    .ok_or_else(|| "--ppm expects N".to_string())?
                    .parse()
                    .map_err(|_| "--ppm: invalid i32".to_string())?;
            }
            "-h" | "--help" => {
                print_help();
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument: {}", other)),
        }
    }
    Ok(out)
}

fn print_help() {
    eprintln!(
        "iq_capture - Spike 1 RTL-SDR producer (cu8 -> stdout)\n\
        \n\
        Usage:\n\
        \x20   iq_capture [options]\n\
        \n\
        Options:\n\
        \x20   --freq MHZ      center frequency in MHz   (default 97.1)\n\
        \x20   --rate SPS      sample rate in Hz         (default 1488375)\n\
        \x20   --gain auto|N   tuner gain in dB or auto  (default auto)\n\
        \x20   --bytes N       stop after N bytes        (default: until EPIPE/Ctrl-C)\n\
        \x20   --device IDX    librtlsdr device index    (default 0)\n\
        \x20   --ppm N         frequency correction      (default 0)\n\
        \n\
        Pipe into nrsc5:\n\
        \x20   iq_capture --freq 97.1 --gain 15 | nrsc5.exe -r - 0\n\
        \n\
        Notes on --gain:\n\
        \x20   The R820T tuner in many RTL-SDR dongles will over-amplify a\n\
        \x20   strong nearby FM station with the default tuner-AGC ('auto'),\n\
        \x20   driving the ADC into hard clipping and preventing NRSC-5\n\
        \x20   OFDM sync. If nrsc5 emits no 'Synchronized' line, sweep\n\
        \x20   manual gains 0/5/10/15/20/25 and pick the one whose byte\n\
        \x20   distribution spans ~50-120 around 127 (i.e. no values near\n\
        \x20   0 or 255). Gain 15 dB worked well in close-range testing.\n"
    );
}

// === Main ================================================================

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("iq_capture: {}", e);
            print_help();
            return ExitCode::from(2);
        }
    };

    eprintln!("iq_capture: loading librtlsdr.dll");
    let api = match load_api() {
        Ok(api) => api,
        Err(e) => {
            eprintln!("iq_capture: {}", e);
            return ExitCode::from(1);
        }
    };
    if API.set(api).is_err() {
        eprintln!("iq_capture: API already initialised (cannot happen)");
        return ExitCode::from(1);
    }
    let api = API.get().expect("just set");

    // --- Open device ---
    eprintln!("iq_capture: opening device {}", args.device_index);
    let mut dev: *mut c_void = std::ptr::null_mut();
    // SAFETY: valid out-pointer; `args.device_index` is just a u32.
    let r = unsafe { (api.open)(&mut dev as *mut _, args.device_index) };
    if r != 0 || dev.is_null() {
        eprintln!(
            "iq_capture: rtlsdr_open(index={}) failed: {}",
            args.device_index, r
        );
        return ExitCode::from(1);
    }
    DEVICE.store(dev, Ordering::SeqCst);

    // Install Ctrl-C handler now that the device is open.
    // SAFETY: kernel32 ABI; `Some(fn)` is a valid HandlerRoutine.
    unsafe {
        SetConsoleCtrlHandler(Some(ctrl_handler), 1);
    }

    // --- Configure tuner ---
    macro_rules! call {
        ($f:expr, $label:literal $(, $arg:expr)*) => {{
            // SAFETY: each call site passes arguments matching the
            // documented librtlsdr ABI; `dev` is valid for the duration.
            let r = unsafe { $f(dev $(, $arg)*) };
            if r != 0 {
                eprintln!("iq_capture: {} -> {}", $label, r);
            }
        }};
    }

    call!(api.set_sample_rate, "set_sample_rate", args.rate_sps);
    call!(api.set_center_freq, "set_center_freq", args.freq_hz);
    // Force normal IQ mode — if a previous run (ours or nrsc5's) left the
    // dongle in direct-sampling mode 1 (I-ADC) or 2 (Q-ADC), every other
    // byte would be near zero and NRSC-5 OFDM sync would never lock.
    call!(api.set_direct_sampling, "set_direct_sampling(0)", 0);
    // RTL2832U built-in digital AGC off; we use the tuner AGC instead
    // (set below). nrsc5 itself uses this combination.
    call!(api.set_agc_mode, "set_agc_mode(0)", 0);
    if args.ppm != 0 {
        call!(api.set_freq_correction, "set_freq_correction", args.ppm);
    }
    match args.gain {
        GainMode::Auto => {
            // mode = 0 → automatic tuner AGC.
            call!(api.set_tuner_gain_mode, "set_tuner_gain_mode(auto)", 0);
        }
        GainMode::Manual(tenths) => {
            call!(api.set_tuner_gain_mode, "set_tuner_gain_mode(manual)", 1);
            call!(api.set_tuner_gain, "set_tuner_gain", tenths);
        }
    }
    call!(api.reset_buffer, "reset_buffer");

    eprintln!(
        "iq_capture: streaming freq={:.4} MHz rate={} sps gain={} device={} bytes_limit={}",
        args.freq_hz as f64 / 1_000_000.0,
        args.rate_sps,
        match args.gain {
            GainMode::Auto => "auto".to_string(),
            GainMode::Manual(t) => format!("{:.1} dB", t as f32 / 10.0),
        },
        args.device_index,
        match args.max_bytes {
            Some(n) => n.to_string(),
            None => "none".to_string(),
        },
    );

    // Heap-allocate Ctx so its address is stable and we can drop it
    // explicitly AFTER the device is closed — eliminates any chance of a
    // tail callback reading freed stack memory.
    let ctx_box = Box::new(Ctx {
        bytes_written: 0,
        max_bytes: args.max_bytes,
        broken_pipe: false,
        cancelled: false,
        stdout: io::stdout(),
    });
    let ctx_ptr = Box::into_raw(ctx_box);

    let start = Instant::now();
    // SAFETY: `dev` is valid; `rtlsdr_cb` matches the documented callback
    // signature; `ctx_ptr` is a heap-allocated `Ctx` kept alive across
    // this call. buf_num=0 and buf_len=0 request librtlsdr's default
    // 15×256-KB ring.
    let r = unsafe {
        (api.read_async)(
            dev,
            rtlsdr_cb,
            ctx_ptr as *mut c_void,
            0,
            0,
        )
    };
    let elapsed = start.elapsed().as_secs_f64();
    eprintln!(
        "iq_capture: read_async returned ({}) after {:.2} s",
        r, elapsed
    );

    // Reclaim Ctx for stats. By contract, `read_async` only returns after
    // every transfer is in CANCELLED state and no more callbacks will
    // fire, so this is safe.
    // SAFETY: `ctx_ptr` was returned by `Box::into_raw` above; librtlsdr
    // is done with it once `read_async` has returned.
    let ctx = unsafe { Box::from_raw(ctx_ptr) };

    // Only call rtlsdr_close on a clean exit. The bundled librtlsdr.dll
    // (older 0.6-era build) reliably access-violates inside rtlsdr_close
    // when read_async returned a non-zero error code — internal state is
    // already partially torn down at that point. Skipping close in that
    // case is safe: the OS reclaims the USB handle on process exit, and
    // the next rtlsdr_open call resets the device.
    DEVICE.store(std::ptr::null_mut(), Ordering::SeqCst);
    if r == 0 {
        // SAFETY: `dev` is still the handle from `rtlsdr_open`; the worker
        // is fully stopped per librtlsdr's contract above.
        let close_r = unsafe { (api.close)(dev) };
        eprintln!("iq_capture: rtlsdr_close -> {}", close_r);
    } else {
        eprintln!(
            "iq_capture: skipping rtlsdr_close (read_async error {} \
             leaves librtlsdr in an unsafe-to-close state on this DLL)",
            r
        );
    }

    eprintln!(
        "iq_capture: stopped after {:.2} s | {} bytes ({:.3} MB/s) | broken_pipe={} stop_flag={}",
        elapsed,
        ctx.bytes_written,
        ctx.bytes_written as f64 / elapsed.max(0.001) / 1_000_000.0,
        ctx.broken_pipe,
        SHOULD_STOP.load(Ordering::SeqCst),
    );

    if ctx.broken_pipe || SHOULD_STOP.load(Ordering::SeqCst) {
        ExitCode::SUCCESS
    } else if r != 0 {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}
