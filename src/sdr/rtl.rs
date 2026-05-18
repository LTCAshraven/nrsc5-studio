//! [`RtlSdr`] — librtlsdr-backed [`Sdr`](super::Sdr) implementation.
//!
//! Loads `librtlsdr.dll` via `libloading` (same crate, same DLL location
//! pattern as [`sdr_detect`](crate::sdr_detect)). The threading and
//! cancellation pattern is the canonical librtlsdr one validated in
//! Spike 1 / Spike 2:
//!
//! * `rtlsdr_read_async` is called from one thread and blocks until
//!   cancelled.
//! * Inside the librtlsdr callback we check a stop flag; on stop we call
//!   `rtlsdr_cancel_async` *from inside the callback*. This avoids the
//!   detached-timer-thread anti-pattern that earlier spikes tripped over.
//! * `rtlsdr_set_tuner_gain` is callable from a control thread while the
//!   worker is blocked in `read_async`. Verified on the bundled DLL.
//! * `rtlsdr_close` is skipped if `read_async` returned a non-zero error
//!   code — calling close after an error access-violates inside the DLL.

use libloading::Library;
use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use std::sync::Mutex;

use super::{Sdr, SdrConfig, SdrError, StreamControl};

// === Hardcoded gain table (R820T2 / R828D) ==================================
//
// 29 discrete tuner gain steps in tenths of dB, ascending. Source:
// librtlsdr's `tuner_r82xx.c`; verified against `rtlsdr_get_tuner_gains`
// from our bundled DLL. Exported so callers (e.g. AGC code) can avoid a
// round-trip through `gain_table_tenths()` when they know the device.
//
// **TODO(v0.2.0+):** swap this constant for a live query via
// `rtlsdr_get_tuner_gains` once we're confident the bundled DLL exposes
// it. Until then this is the canonical R820T set.
pub const R820T_GAINS_TENTHS: &[i32] = &[
    0, 9, 14, 27, 37, 77, 87, 125, 144, 157, 166, 197, 207, 229, 254, 280, 297, 328, 338, 364,
    372, 386, 402, 421, 434, 439, 445, 480, 496,
];

// === librtlsdr function-pointer types ======================================
//
// `unsafe extern "C"` because libusb DLLs use the cdecl calling convention
// (not stdcall) on Windows. Cross-checked against the librtlsdr public
// header in `res/nrsc5.h` and rtl-sdr.h.

#[allow(non_camel_case_types)]
type rtlsdr_read_async_cb_t = extern "C" fn(buf: *mut u8, len: u32, ctx: *mut c_void);

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
type FnReadAsync =
    unsafe extern "C" fn(*mut c_void, rtlsdr_read_async_cb_t, *mut c_void, u32, u32) -> i32;

/// Function pointers resolved from `librtlsdr.dll`. The `Library` is kept
/// alive (never accessed after construction) so the function pointers
/// stay valid for the lifetime of the [`RtlSdr`] struct.
struct Api {
    _lib: Library,
    open: FnOpen,
    close: FnClose,
    set_center_freq: FnSetCenterFreq,
    set_sample_rate: FnSetSampleRate,
    set_tuner_gain_mode: FnSetTunerGainMode,
    set_tuner_gain: FnSetTunerGain,
    set_freq_correction: FnSetFreqCorrection,
    reset_buffer: FnResetBuffer,
    set_direct_sampling: FnSetDirectSampling,
    #[allow(dead_code)] // surfaced for future "force hardware AGC" path
    set_agc_mode: FnSetAgcMode,
    cancel_async: FnCancelAsync,
    read_async: FnReadAsync,
}

// SAFETY: `libloading::Library` is `Send + Sync` on Windows, and raw fn
// pointers are trivially `Send + Sync`. librtlsdr's USB control transfers
// are serialized inside libusb so concurrent calls from multiple threads
// are safe (validated by Spike 2 — mid-stream `set_tuner_gain` from a
// non-worker thread while the worker blocks in `read_async`).
unsafe impl Send for Api {}
unsafe impl Sync for Api {}

fn find_librtlsdr() -> Option<PathBuf> {
    const DLL: &str = "librtlsdr.dll";
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let p = dir.join("bin").join(DLL);
            if p.exists() {
                return Some(p);
            }
            let p = dir.join(DLL);
            if p.exists() {
                return Some(p);
            }
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        let p = cwd.join("bin").join(DLL);
        if p.exists() {
            return Some(p);
        }
        let p = cwd.join(DLL);
        if p.exists() {
            return Some(p);
        }
    }
    None
}

fn load_api() -> Result<Api, SdrError> {
    let path = find_librtlsdr().ok_or(SdrError::LibraryNotFound)?;
    // SAFETY: load with `LOAD_WITH_ALTERED_SEARCH_PATH` (0x8) so the
    // DLL's own directory is the search base for its dependencies.
    // Modern `librtlsdr.dll` builds dynamically link against
    // `libusb-1.0.dll` (rather than statically as the old bundled DLL
    // did), and without this flag Windows looks for libusb in the
    // calling process's directory, not `bin/`.
    let lib = unsafe {
        #[cfg(target_os = "windows")]
        {
            const LOAD_WITH_ALTERED_SEARCH_PATH: u32 = 0x0000_0008;
            libloading::os::windows::Library::load_with_flags(
                &path,
                LOAD_WITH_ALTERED_SEARCH_PATH,
            )
            .map(Library::from)
        }
        #[cfg(not(target_os = "windows"))]
        {
            Library::new(&path)
        }
    }
    .map_err(|e| SdrError::LoadFailed(format!("{}: {}", path.display(), e)))?;

    // Helper to resolve symbols. The libloading `Symbol` borrows from the
    // `Library`, so we dereference to copy the raw fn pointer out — once
    // copied, the fn pointer is valid for as long as `_lib` lives.
    macro_rules! resolve {
        ($lib:expr, $name:literal, $ty:ty) => {{
            // SAFETY: each librtlsdr export is a stable C function with the
            // documented signature, matching the type alias above.
            unsafe {
                let sym: libloading::Symbol<$ty> = $lib
                    .get(concat!($name, "\0").as_bytes())
                    .map_err(|_| SdrError::SymbolMissing($name))?;
                *sym
            }
        }};
    }

    let api = Api {
        open: resolve!(lib, "rtlsdr_open", FnOpen),
        close: resolve!(lib, "rtlsdr_close", FnClose),
        set_center_freq: resolve!(lib, "rtlsdr_set_center_freq", FnSetCenterFreq),
        set_sample_rate: resolve!(lib, "rtlsdr_set_sample_rate", FnSetSampleRate),
        set_tuner_gain_mode: resolve!(lib, "rtlsdr_set_tuner_gain_mode", FnSetTunerGainMode),
        set_tuner_gain: resolve!(lib, "rtlsdr_set_tuner_gain", FnSetTunerGain),
        set_freq_correction: resolve!(lib, "rtlsdr_set_freq_correction", FnSetFreqCorrection),
        reset_buffer: resolve!(lib, "rtlsdr_reset_buffer", FnResetBuffer),
        set_direct_sampling: resolve!(lib, "rtlsdr_set_direct_sampling", FnSetDirectSampling),
        set_agc_mode: resolve!(lib, "rtlsdr_set_agc_mode", FnSetAgcMode),
        cancel_async: resolve!(lib, "rtlsdr_cancel_async", FnCancelAsync),
        read_async: resolve!(lib, "rtlsdr_read_async", FnReadAsync),
        _lib: lib,
    };
    Ok(api)
}

// === Callback context ======================================================
//
// `rtlsdr_read_async` takes a `*mut c_void` ctx that's passed verbatim to
// each callback invocation. We point it at this struct, which holds the
// user's per-frame closure plus the bits the callback needs to cancel the
// stream cleanly without crossing a thread boundary.

struct CbCtx<'a> {
    /// User callback. `&mut dyn` is `?Sized` so we type-erase the closure
    /// while keeping a unique borrow for the lifetime of the stream.
    cb: &'a mut dyn FnMut(&[u8]) -> StreamControl,
    /// Shared with the outside world via [`RtlSdr::stop_flag`].
    /// `cancel_stream` flips this; the callback checks it on every entry.
    external_stop: &'a AtomicBool,
    /// Snapshot of the device + API pointers so the callback can self-
    /// cancel without touching `RtlSdr` state.
    api: &'a Api,
    dev: *mut c_void,
    /// Set once we've already called `cancel_async`, to avoid double-
    /// cancelling. The callback is invoked single-threaded so a plain
    /// bool is sufficient.
    self_cancelled: bool,
}

extern "C" fn rtlsdr_callback(buf: *mut u8, len: u32, ctx_raw: *mut c_void) {
    if ctx_raw.is_null() || buf.is_null() || len == 0 {
        return;
    }
    // SAFETY: `ctx_raw` is the same pointer we handed to `read_async` and
    // the callback runs on the worker thread for the duration of that
    // call. The `CbCtx` borrow is alive on the run_stream stack frame.
    let ctx = unsafe { &mut *(ctx_raw as *mut CbCtx) };

    if ctx.self_cancelled {
        return;
    }

    let stop_requested = ctx.external_stop.load(Ordering::Acquire);

    let user_decision = if stop_requested {
        StreamControl::Stop
    } else {
        // SAFETY: librtlsdr owns the buffer for the duration of the
        // callback. We hand the user a borrow that they MUST NOT retain
        // past the callback return.
        let slice = unsafe { std::slice::from_raw_parts(buf, len as usize) };
        (ctx.cb)(slice)
    };

    if matches!(user_decision, StreamControl::Stop) {
        ctx.self_cancelled = true;
        // SAFETY: dev was non-null at run_stream entry. Calling
        // cancel_async from inside the callback is the canonical
        // librtlsdr cancellation pattern (see rtl_sdr.c).
        unsafe {
            (ctx.api.cancel_async)(ctx.dev);
        }
    }
}

// === The backend ===========================================================

/// RTL-SDR backend, libloading-based.
pub struct RtlSdr {
    api: Api,
    /// Opened device handle. Null until [`open`](Self::open) succeeds;
    /// reset to null in `Drop` once we've decided whether to call `close`.
    dev: AtomicPtr<c_void>,
    /// Set by `cancel_stream`; read by the callback to trigger self-cancel.
    stop_flag: AtomicBool,
    /// `true` once `read_async` has returned a non-zero error code. The
    /// Drop impl uses this to skip `rtlsdr_close` (which access-violates
    /// in the bundled DLL when called after a failed read_async).
    read_async_errored: AtomicBool,
    /// Serializes `run_stream` so only one worker thread can drive the
    /// device's async pump at a time. The mutex is held for the entire
    /// duration of the stream; gain changes and cancel use atomics, not
    /// this mutex, so they don't block on the worker.
    stream_guard: Mutex<()>,
}

impl RtlSdr {
    /// Open device at the given index and resolve all librtlsdr symbols.
    /// Does NOT apply any configuration; call [`Sdr::configure`] next.
    pub fn open(device_index: u32) -> Result<Self, SdrError> {
        let api = load_api()?;
        let mut dev: *mut c_void = std::ptr::null_mut();
        // SAFETY: `rtlsdr_open` writes through the `*mut *mut c_void` arg.
        let r = unsafe { (api.open)(&mut dev as *mut _, device_index) };
        if r != 0 || dev.is_null() {
            return Err(SdrError::OpenFailed(device_index));
        }
        Ok(Self {
            api,
            dev: AtomicPtr::new(dev),
            stop_flag: AtomicBool::new(false),
            read_async_errored: AtomicBool::new(false),
            stream_guard: Mutex::new(()),
        })
    }

    fn dev_or_err(&self) -> Result<*mut c_void, SdrError> {
        let dev = self.dev.load(Ordering::Acquire);
        if dev.is_null() {
            Err(SdrError::NotOpen)
        } else {
            Ok(dev)
        }
    }

    fn call(&self, func: &'static str, code: i32) -> Result<(), SdrError> {
        if code == 0 {
            Ok(())
        } else {
            Err(SdrError::CallFailed { func, code })
        }
    }
}

impl Sdr for RtlSdr {
    fn configure(&self, cfg: &SdrConfig) -> Result<(), SdrError> {
        let dev = self.dev_or_err()?;
        // SAFETY: dev is a valid librtlsdr handle held by self. All calls
        // are documented thread-safe vs each other (libusb-internal lock).
        unsafe {
            self.call(
                "rtlsdr_set_sample_rate",
                (self.api.set_sample_rate)(dev, cfg.sample_rate_sps),
            )?;
            self.call(
                "rtlsdr_set_center_freq",
                (self.api.set_center_freq)(dev, cfg.center_freq_hz),
            )?;
            self.call(
                "rtlsdr_set_direct_sampling",
                (self.api.set_direct_sampling)(dev, cfg.direct_sampling),
            )?;
            // Disable librtlsdr's internal digital AGC — we drive the
            // tuner AGC ourselves from the app side.
            self.call(
                "rtlsdr_set_agc_mode",
                (self.api.set_agc_mode)(dev, 0),
            )?;
            if cfg.ppm_correction != 0 {
                self.call(
                    "rtlsdr_set_freq_correction",
                    (self.api.set_freq_correction)(dev, cfg.ppm_correction),
                )?;
            }
            if let Some(tenths) = cfg.initial_gain_tenths {
                self.call(
                    "rtlsdr_set_tuner_gain_mode",
                    (self.api.set_tuner_gain_mode)(dev, 1),
                )?;
                self.call(
                    "rtlsdr_set_tuner_gain",
                    (self.api.set_tuner_gain)(dev, tenths),
                )?;
            }
            self.call("rtlsdr_reset_buffer", (self.api.reset_buffer)(dev))?;
        }
        Ok(())
    }

    fn gain_table_tenths(&self) -> &[i32] {
        R820T_GAINS_TENTHS
    }

    fn set_tuner_gain_tenths(&self, tenths: i32) -> Result<(), SdrError> {
        let dev = self.dev_or_err()?;
        // SAFETY: see configure(). Specifically safe to call mid-stream
        // (Spike 2 finding).
        let r = unsafe { (self.api.set_tuner_gain)(dev, tenths) };
        self.call("rtlsdr_set_tuner_gain", r)
    }

    fn set_center_freq_hz(&self, hz: u32) -> Result<(), SdrError> {
        let dev = self.dev_or_err()?;
        // SAFETY: see configure(). Retune-while-streaming is supported.
        let r = unsafe { (self.api.set_center_freq)(dev, hz) };
        self.call("rtlsdr_set_center_freq", r)
    }

    fn run_stream(
        &self,
        cb: &mut dyn FnMut(&[u8]) -> StreamControl,
    ) -> Result<(), SdrError> {
        let dev = self.dev_or_err()?;

        // Only one streamer at a time per device.
        let _guard = self
            .stream_guard
            .try_lock()
            .map_err(|_| SdrError::AlreadyStreaming)?;

        // Reset the stop flag from any prior aborted stream.
        self.stop_flag.store(false, Ordering::Release);
        self.read_async_errored.store(false, Ordering::Release);

        let mut ctx = CbCtx {
            cb,
            external_stop: &self.stop_flag,
            api: &self.api,
            dev,
            self_cancelled: false,
        };

        // SAFETY: we hold `ctx` on this stack frame for the entire
        // duration of `read_async`. The callback receives a raw pointer
        // to it and dereferences it on the worker thread — but the worker
        // thread IS this thread (librtlsdr invokes callbacks from inside
        // `read_async`'s libusb loop on the calling thread).
        //
        // `buf_num=0, buf_len=0` requests librtlsdr's defaults
        // (15 × 256 KiB = 3.75 MiB ring buffer).
        let r = unsafe {
            (self.api.read_async)(
                dev,
                rtlsdr_callback,
                &mut ctx as *mut CbCtx as *mut c_void,
                0,
                0,
            )
        };

        // Distinguish "user-initiated clean cancel" from "USB went sideways".
        //
        // librtlsdr's `read_async` can return a non-zero code (typically
        // `LIBUSB_ERROR_INTERRUPTED` / negative) even on a normal cancel,
        // depending on which iteration of `libusb_handle_events_timeout`
        // the cancel landed in. Treating *any* non-zero return as a USB
        // error meant the Spike 1 close-after-error workaround triggered
        // on every clean stop, leaving the dongle un-closed — which then
        // surfaced as "device in use" on the next `rtlsdr_open` from the
        // long-lived GUI process. Spike 1's actual repro (USB unplugged
        // mid-stream) is captured by `stop_requested == false && r != 0`.
        let stop_requested = self.stop_flag.load(Ordering::Acquire);
        if r != 0 && !stop_requested {
            self.read_async_errored.store(true, Ordering::Release);
            return Err(SdrError::CallFailed {
                func: "rtlsdr_read_async",
                code: r,
            });
        }
        Ok(())
    }

    fn cancel_stream(&self) -> Result<(), SdrError> {
        // Just trip the flag — the callback will see it and self-cancel
        // via `rtlsdr_cancel_async` from the worker thread. This is the
        // canonical pattern; cross-thread `cancel_async` also works but
        // is less defensively coded inside libusb.
        self.stop_flag.store(true, Ordering::Release);
        Ok(())
    }
}

impl Drop for RtlSdr {
    fn drop(&mut self) {
        let dev = self.dev.swap(std::ptr::null_mut(), Ordering::AcqRel);
        if dev.is_null() {
            return;
        }
        // Known bug in the bundled librtlsdr.dll: calling `rtlsdr_close`
        // after `read_async` returned a non-zero error code triggers an
        // access violation inside the DLL. Spike 1 documents the repro;
        // we skip close in that path and accept the (process-scoped) leak.
        if self.read_async_errored.load(Ordering::Acquire) {
            return;
        }
        // SAFETY: dev was a valid handle from rtlsdr_open and we have
        // exclusive access in Drop. read_async is not active (the stream
        // guard would still be held if it were, and Drop can't run while
        // any &self method is on the stack).
        unsafe {
            (self.api.close)(dev);
        }
    }
}
