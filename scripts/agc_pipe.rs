//! agc_pipe — Spike 2 closed-loop AGC proof-of-concept.
//!
//! Fork of `iq_capture.rs` that owns *both* sides of the pipe:
//!
//!   RTL-SDR ──► librtlsdr ──► [worker thread]              [stderr reader thread]
//!                                  │                              │
//!                                  ▼                              │
//!                       Ctx { nrsc5_stdin: ChildStdin }           │
//!                                  │                              │
//!                                  ▼                              │
//!                              nrsc5.exe -l 1 -r - 0              │
//!                                  │                              │
//!                                  └─ stderr ───────────►─────────┘
//!                                                                 ▼
//!                                                       parse + push NrscEvent
//!                                                                 ▼
//!                                                          mpsc::Receiver
//!                                                                 ▼
//!                                                        [main thread = AGC]
//!                                                                 │
//!                                                                 ▼
//!                                              api.set_tuner_gain(dev, tenths)
//!
//! Spike-2 acceptance set lives in `/memories/session/spike2-plan.md`.
//!
//! Build (same toolchain dance as iq_capture):
//!
//!     $env:PATH = "$pwd\.toolchains\llvm-mingw-20260505-ucrt-x86_64\bin;$env:PATH"
//!     rustc -O scripts\agc_pipe.rs -o target\agc_pipe.exe
//!
//! Run:
//!
//!     target\agc_pipe.exe --freq 97.1 --initial-gain 20
//!
//! Args:
//!     --freq MHZ                 center frequency in MHz   (default 97.1)
//!     --rate SPS                 sample rate in Hz         (default 1488375)
//!     --initial-gain DB          first gain to try in dB   (default 20.7)
//!     --mer-target DB            convergence threshold     (default 12.0)
//!     --probe-period-ms MS       gap between gain probes   (default 5000)
//!     --bail-after-changes N     give up after N futile probes (default 15)
//!     --device IDX               librtlsdr device index    (default 0)
//!     --ppm N                    frequency correction      (default 0)
//!     --nrsc5 PATH               path to nrsc5.exe         (default bin\nrsc5.exe)
//!     --log PATH                 AGC decision log          (default target\spike2-agc.log)
//!     --max-seconds N            stop after N s of runtime (default: until Ctrl-C)
//!
//! Cancellation model: same canonical pattern as iq_capture. Ctrl-C trips
//! SHOULD_STOP; the librtlsdr callback notices on its next invocation and
//! calls cancel_async from inside the worker thread. The AGC loop sees
//! the stderr-reader's mpsc Sender drop when nrsc5 exits, and the worker
//! thread's join handle lets main wait for full teardown before closing
//! the device.

#![cfg(windows)]
#![allow(non_snake_case)]

use std::collections::BTreeMap;
use std::env;
use std::ffi::c_void;
use std::fs::File;
use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, ExitCode, Stdio};
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::OnceLock;
use std::thread;
use std::time::{Duration, Instant};

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
    u32,
    u32,
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
    #[allow(dead_code)]
    set_agc_mode: FnSetAgcMode,
    cancel_async: FnCancelAsync,
    read_async: FnReadAsync,
    _module: *mut c_void,
}

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

    let module = unsafe { LoadLibraryW(wide.as_ptr()) };
    if module.is_null() {
        return Err(format!("LoadLibraryW failed for {}", path.display()));
    }

    fn resolve<T: Copy>(module: *mut c_void, name: &[u8]) -> Result<T, String> {
        debug_assert!(name.ends_with(b"\0"));
        let p = unsafe { GetProcAddress(module, name.as_ptr()) };
        if p.is_null() {
            let pretty = std::str::from_utf8(&name[..name.len() - 1]).unwrap_or("?");
            return Err(format!("missing symbol: {}", pretty));
        }
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
            unsafe {
                FreeLibrary(module);
            }
            Err(e)
        }
    }
}

// === Globals =============================================================

static API: OnceLock<Api> = OnceLock::new();
static DEVICE: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static SHOULD_STOP: AtomicBool = AtomicBool::new(false);

extern "system" fn ctrl_handler(ctrl_type: u32) -> i32 {
    match ctrl_type {
        CTRL_C_EVENT | CTRL_BREAK_EVENT | CTRL_CLOSE_EVENT => {
            SHOULD_STOP.store(true, Ordering::SeqCst);
            1
        }
        _ => 0,
    }
}

// === Callback ctx ========================================================
//
// The librtlsdr worker thread invokes `rtlsdr_cb` with each USB transfer.
// We write the bytes directly into nrsc5's stdin. If nrsc5 dies, the
// write returns BrokenPipe and we cancel the async loop.

struct Ctx {
    bytes_written: u64,
    broken_pipe: bool,
    cancelled: bool,
    nrsc5_stdin: ChildStdin,
}

extern "C" fn rtlsdr_cb(buf: *mut u8, len: u32, ctx_raw: *mut c_void) {
    if ctx_raw.is_null() || buf.is_null() || len == 0 {
        return;
    }
    let ctx = unsafe { &mut *(ctx_raw as *mut Ctx) };

    if ctx.cancelled {
        return;
    }

    let cancel_now = if ctx.broken_pipe || SHOULD_STOP.load(Ordering::Acquire) {
        true
    } else {
        let slice = unsafe { std::slice::from_raw_parts(buf, len as usize) };
        match ctx.nrsc5_stdin.write_all(slice) {
            Ok(()) => {
                ctx.bytes_written += len as u64;
                false
            }
            Err(e) => {
                ctx.broken_pipe = true;
                if e.kind() != io::ErrorKind::BrokenPipe {
                    eprintln!("agc_pipe: nrsc5 stdin write failed: {}", e);
                }
                true
            }
        }
    };

    if cancel_now {
        ctx.cancelled = true;
        if let Some(api) = API.get() {
            let dev = DEVICE.load(Ordering::SeqCst);
            if !dev.is_null() {
                unsafe {
                    (api.cancel_async)(dev);
                }
            }
        }
    }
}

// === NRSC-5 events =======================================================

#[derive(Debug, Clone)]
enum NrscEvent {
    Synchronized,
    LostSync,
    Mer { lower: f32, upper: f32 },
    Ber(f32),
    StationName(String),
    Title(String),
    /// Any line that didn't match a structured event; kept so the AGC
    /// log can echo raw nrsc5 chatter for forensics.
    #[allow(dead_code)]
    Raw(String),
    /// nrsc5's stderr pipe closed (process exited).
    StderrClosed,
}

fn parse_nrsc5_line(line: &str) -> Option<NrscEvent> {
    // nrsc5 v3.1.0 lines all start with "HH:MM:SS " — skip it.
    let body = match line.get(9..) {
        Some(s) if line.len() >= 9 && line.as_bytes().get(2) == Some(&b':') => s,
        _ => line,
    };

    if body.starts_with("Synchronized") {
        return Some(NrscEvent::Synchronized);
    }
    if body.starts_with("Lost sync") || body.starts_with("Lost synchronization") {
        return Some(NrscEvent::LostSync);
    }
    if let Some(rest) = body.strip_prefix("MER: ") {
        // "12.1 dB (lower), 11.2 dB (upper)"
        let (l_part, rest) = rest.split_once(" dB (lower), ")?;
        let (u_part, _) = rest.split_once(" dB (upper)")?;
        let lower = l_part.trim().parse::<f32>().ok()?;
        let upper = u_part.trim().parse::<f32>().ok()?;
        return Some(NrscEvent::Mer { lower, upper });
    }
    if let Some(rest) = body.strip_prefix("BER: ") {
        // "0.000186, avg: ..."
        let first = rest.split(',').next()?.trim();
        let v = first.parse::<f32>().ok()?;
        return Some(NrscEvent::Ber(v));
    }
    if let Some(rest) = body.strip_prefix("Station name: ") {
        return Some(NrscEvent::StationName(rest.trim().to_string()));
    }
    if let Some(rest) = body.strip_prefix("Title: ") {
        return Some(NrscEvent::Title(rest.trim().to_string()));
    }
    None
}

// === R820T gain table ====================================================
//
// 29 discrete tuner gain steps in tenths-of-dB, ordered ascending.
// Source: librtlsdr's tuner_r82xx.c `r82xx_gain_steps` plus its hardcoded
// `r82xx_lna_gain_steps` cumulative sums. Verified against the values
// `rtlsdr_get_tuner_gains` returns from the bundled DLL.
//
// v0.2.0 will query the live table via rtlsdr_get_tuner_gains, but this
// is the canonical R820T set and matches our DLL.

const R820T_GAINS_TENTHS: &[i32] = &[
    0, 9, 14, 27, 37, 77, 87, 125, 144, 157, 166, 197, 207, 229, 254, 280, 297, 328, 338, 364,
    372, 386, 402, 421, 434, 439, 445, 480, 496,
];

fn nearest_gain_idx(target_tenths: i32) -> usize {
    let mut best = 0usize;
    let mut best_diff = i32::MAX;
    for (i, &g) in R820T_GAINS_TENTHS.iter().enumerate() {
        let d = (g - target_tenths).abs();
        if d < best_diff {
            best_diff = d;
            best = i;
        }
    }
    best
}

// === AGC controller ======================================================

struct Agc {
    /// Index into R820T_GAINS_TENTHS for the gain currently applied.
    gain_idx: usize,
    /// +1 to step up, -1 to step down. Bounces on table edges.
    last_dir: i32,
    /// Lowpassed `min(mer_lower, mer_upper)`. None until first MER seen.
    ema_mer_min: Option<f32>,
    /// Best EMA we've ever seen (used to detect "did the last step help?").
    best_mer_seen: f32,
    /// Best gain index that produced `best_mer_seen`.
    best_gain_idx: usize,
    /// Probes that did not improve `best_mer_seen` since last reset.
    probes_without_improvement: u32,
    /// Wall time of last gain change (or AGC start). Used for settle hold.
    last_change_at: Instant,
    /// Wall time we last observed a Synchronized event.
    #[allow(dead_code)]
    last_sync_at: Option<Instant>,
    /// Have we ever seen Synchronized?
    has_ever_synced: bool,
    /// Once true, AGC stops adjusting gain.
    settled: bool,
    /// Once true, AGC permanently gives up.
    bailed_out: bool,
    /// Total probes executed (for logging).
    probes_done: u32,
    /// Per-index best EMA seen during this run. Indices in this map are
    /// "explored" — the prober will not return to them. NEG_INFINITY means
    /// we tried the gain but never saw an MER reading (typically: no sync).
    explored: BTreeMap<usize, f32>,

    // Tunables
    mer_target: f32,
    probe_period: Duration,
    bail_after_changes: u32,

    // Log sink (decisions only — separate from nrsc5's own stderr).
    log: BufWriter<File>,
    log_t0: Instant,
}

impl Agc {
    fn new(
        initial_tenths: i32,
        mer_target: f32,
        probe_period: Duration,
        bail_after_changes: u32,
        log_path: &Path,
    ) -> io::Result<Self> {
        let log = BufWriter::new(File::create(log_path)?);
        let gain_idx = nearest_gain_idx(initial_tenths);
        Ok(Agc {
            gain_idx,
            last_dir: -1, // start by walking DOWN; a too-hot gain is the
                          // failure mode that ruins HD reception the most,
                          // and walking down from a working signal can't lose sync.
            ema_mer_min: None,
            best_mer_seen: f32::NEG_INFINITY,
            best_gain_idx: gain_idx,
            probes_without_improvement: 0,
            last_change_at: Instant::now(),
            last_sync_at: None,
            has_ever_synced: false,
            settled: false,
            bailed_out: false,
            probes_done: 0,
            mer_target,
            probe_period,
            bail_after_changes,
            log,
            log_t0: Instant::now(),
            explored: BTreeMap::new(),
        })
    }

    fn log_line(&mut self, msg: &str) {
        let t = self.log_t0.elapsed().as_secs_f32();
        let line = format!("[{:7.2}s gain={:>4.1}dB] {}\n", t, self.current_db(), msg);
        let _ = self.log.write_all(line.as_bytes());
        let _ = self.log.flush();
        eprint!("agc: {}", line);
    }

    fn current_db(&self) -> f32 {
        R820T_GAINS_TENTHS[self.gain_idx] as f32 / 10.0
    }

    fn on_event(&mut self, ev: &NrscEvent) {
        match ev {
            NrscEvent::Synchronized => {
                self.has_ever_synced = true;
                self.last_sync_at = Some(Instant::now());
                self.log_line("nrsc5: Synchronized");
            }
            NrscEvent::LostSync => {
                self.log_line("nrsc5: Lost sync");
                // Treat sync loss as evidence the last gain change was
                // wrong — revert direction and step back immediately
                // (subject to probe-period hold).
            }
            NrscEvent::Mer { lower, upper } => {
                let m = lower.min(*upper);
                self.ema_mer_min = Some(match self.ema_mer_min {
                    Some(prev) => 0.6 * prev + 0.4 * m,
                    None => m,
                });
                self.log_line(&format!(
                    "nrsc5: MER L={:.1} U={:.1} dB (ema_min={:.2})",
                    lower, upper, self.ema_mer_min.unwrap()
                ));
            }
            NrscEvent::Ber(b) => {
                self.log_line(&format!("nrsc5: BER {:.2e}", b));
            }
            NrscEvent::StationName(s) => self.log_line(&format!("nrsc5: Station name: {}", s)),
            NrscEvent::Title(s) => self.log_line(&format!("nrsc5: Title: {}", s)),
            NrscEvent::Raw(_) => {}
            NrscEvent::StderrClosed => self.log_line("nrsc5: stderr closed (process exited)"),
        }
    }

    /// Apply a new gain index, log the change, and start the settle timer.
    /// Resets the MER EMA — readings from the old gain shouldn't pollute
    /// the post-step assessment.
    fn apply_gain(&mut self, dev: *mut c_void, new_idx: usize, reason: &str) {
        let new_idx = new_idx.min(R820T_GAINS_TENTHS.len() - 1);
        let tenths = R820T_GAINS_TENTHS[new_idx];
        let old_db = self.current_db();
        let new_db = tenths as f32 / 10.0;
        self.gain_idx = new_idx;
        self.last_change_at = Instant::now();
        self.ema_mer_min = None; // fresh start; rebuilt from upcoming MER events
        if let Some(api) = API.get() {
            let r = unsafe { (api.set_tuner_gain)(dev, tenths) };
            self.log_line(&format!(
                ">>> gain {:.1} -> {:.1} dB (idx {}, {}) set_tuner_gain={}",
                old_db, new_db, new_idx, reason, r
            ));
        }
    }

    /// Called from the main loop every iteration. Decides whether to probe
    /// a new gain step based on elapsed time and current EMA.
    ///
    /// Strategy (explored-set hill-climbing):
    ///
    /// 1. After every probe period, record the EMA observed at the current
    ///    gain index into `explored`. Indices in `explored` will never be
    ///    revisited.
    /// 2. If the EMA met the MER target, declare SETTLED.
    /// 3. If we improved over the previous best, keep walking the same
    ///    direction. Otherwise, flip direction.
    /// 4. Find the next UNEXPLORED gain index in the chosen direction. If
    ///    that direction is exhausted, try the other. If both are
    ///    exhausted, settle at the best-known gain (or bail if best < 6 dB).
    /// 5. Bail after `bail_after_changes` consecutive non-improving probes,
    ///    restoring the best-known gain first.
    ///
    /// Crucially, because the prober uses an `explored` set, we cannot
    /// oscillate over the same two indices. Each probe visits a new gain.
    fn maybe_probe(&mut self, dev: *mut c_void) {
        if self.bailed_out || self.settled {
            return;
        }
        if self.last_change_at.elapsed() < self.probe_period {
            return; // settle hold
        }

        self.probes_done += 1;

        // --- 1. Record what we observed at the current gain. ---
        let current_ema = self.ema_mer_min;
        match current_ema {
            Some(e) => {
                let prev = self
                    .explored
                    .get(&self.gain_idx)
                    .copied()
                    .unwrap_or(f32::NEG_INFINITY);
                if e > prev {
                    self.explored.insert(self.gain_idx, e);
                }
                if e > self.best_mer_seen {
                    self.best_mer_seen = e;
                    self.best_gain_idx = self.gain_idx;
                    self.probes_without_improvement = 0;
                } else {
                    self.probes_without_improvement += 1;
                }
            }
            None => {
                // No MER reading at this gain (no sync or too early). Mark
                // explored with a sentinel so we don't waste future probes
                // on this idx.
                self.explored
                    .entry(self.gain_idx)
                    .or_insert(f32::NEG_INFINITY);
                self.probes_without_improvement += 1;
            }
        }

        // --- 2. Target hit? ---
        if let Some(e) = current_ema {
            if e >= self.mer_target {
                self.log_line(&format!(
                    "probe #{}: ema_mer_min {:.2} >= target {:.1} dB — SETTLED at gain {:.1} dB",
                    self.probes_done, e, self.mer_target, self.current_db()
                ));
                self.settled = true;
                return;
            }
        }

        // --- 3. Bail-out budget exhausted? ---
        if self.probes_without_improvement >= self.bail_after_changes {
            // Restore best-known gain before bailing.
            if self.best_gain_idx != self.gain_idx {
                self.apply_gain(
                    dev,
                    self.best_gain_idx,
                    "bail-out: restoring best-known gain",
                );
            }
            self.log_bail(&format!(
                "no improvement in {} probes (best {:.2} dB at {:.1} dB)",
                self.bail_after_changes,
                self.best_mer_seen,
                R820T_GAINS_TENTHS[self.best_gain_idx] as f32 / 10.0
            ));
            return;
        }

        // --- 4. Pick direction. ---
        //
        // If the latest probe improved best, keep walking the same way.
        // Else flip. (For no-sync probes, keep current direction; we're
        // searching blindly.)
        let preferred_dir = match current_ema {
            Some(e) if (e - self.best_mer_seen).abs() < 0.01 => self.last_dir, // we ARE the best
            Some(_) => -self.last_dir,
            None => self.last_dir,
        };

        // --- 5. Find next unexplored idx in preferred dir, then the other. ---
        let (next_idx, chosen_dir) = match self.next_unexplored(preferred_dir) {
            Some(idx) => (idx, preferred_dir),
            None => match self.next_unexplored(-preferred_dir) {
                Some(idx) => (idx, -preferred_dir),
                None => {
                    // Entire reachable table explored. Settle at best (if
                    // usable) or bail.
                    let usable = self.best_mer_seen >= 6.0;
                    if self.best_gain_idx != self.gain_idx {
                        let reason = if usable {
                            "all explored: returning to best"
                        } else {
                            "all explored: restoring best before bail"
                        };
                        self.apply_gain(dev, self.best_gain_idx, reason);
                    }
                    if usable {
                        self.log_line(&format!(
                            "probe #{}: every gain explored — SETTLED at best {:.1} dB (ema {:.2})",
                            self.probes_done,
                            R820T_GAINS_TENTHS[self.best_gain_idx] as f32 / 10.0,
                            self.best_mer_seen
                        ));
                        self.settled = true;
                    } else {
                        self.log_bail("every gain explored, no usable lock");
                    }
                    return;
                }
            },
        };
        self.last_dir = chosen_dir;

        // --- 6. Stability shortcut: if we've already probed both neighbours
        //        of best_gain_idx and best_mer_seen is decent, just go there
        //        and settle instead of wandering further afield. ---
        if self.probes_done >= 4 && self.best_mer_seen >= 6.0 {
            let bi = self.best_gain_idx;
            let max_i = R820T_GAINS_TENTHS.len() - 1;
            let left_done = bi == 0 || self.explored.contains_key(&(bi - 1));
            let right_done = bi == max_i || self.explored.contains_key(&(bi + 1));
            if left_done && right_done {
                if self.gain_idx != bi {
                    self.apply_gain(
                        dev,
                        bi,
                        "stability: best-known gain has both neighbours probed",
                    );
                }
                self.log_line(&format!(
                    "probe #{}: stability — SETTLED at best gain {:.1} dB (ema {:.2})",
                    self.probes_done,
                    R820T_GAINS_TENTHS[bi] as f32 / 10.0,
                    self.best_mer_seen
                ));
                self.settled = true;
                return;
            }
        }

        // --- 7. Probe next gain. ---
        let reason = match current_ema {
            Some(e) => format!(
                "ema={:.2} best={:.2} -> probing idx {} ({})",
                e,
                self.best_mer_seen,
                next_idx,
                if self.last_dir > 0 { "up" } else { "down" }
            ),
            None => format!(
                "no MER at this gain -> probing idx {} ({})",
                next_idx,
                if self.last_dir > 0 { "up" } else { "down" }
            ),
        };
        self.apply_gain(dev, next_idx, &reason);
    }

    /// First gain index in `dir` from `gain_idx` that is NOT in `explored`.
    fn next_unexplored(&self, dir: i32) -> Option<usize> {
        let n = R820T_GAINS_TENTHS.len() as i32;
        let mut i = self.gain_idx as i32 + dir;
        while i >= 0 && i < n {
            if !self.explored.contains_key(&(i as usize)) {
                return Some(i as usize);
            }
            i += dir;
        }
        None
    }

    fn log_bail(&mut self, msg: &str) {
        self.bailed_out = true;
        self.log_line(&format!("BAIL: {}", msg));
        self.log_line(&format!(
            "  final gain {:.1} dB (idx {}), best MER seen {:.2} dB at {:.1} dB",
            self.current_db(),
            self.gain_idx,
            self.best_mer_seen,
            R820T_GAINS_TENTHS[self.best_gain_idx] as f32 / 10.0,
        ));
    }
}

// === Args ================================================================

struct Args {
    freq_hz: u32,
    rate_sps: u32,
    initial_gain_tenths: i32,
    mer_target: f32,
    probe_period_ms: u64,
    bail_after_changes: u32,
    device_index: u32,
    ppm: i32,
    nrsc5_path: PathBuf,
    log_path: PathBuf,
    max_seconds: Option<u64>,
}

const DEFAULT_FREQ_HZ: u32 = 97_100_000;
const DEFAULT_RATE_SPS: u32 = 1_488_375;

fn parse_args() -> Result<Args, String> {
    let mut out = Args {
        freq_hz: DEFAULT_FREQ_HZ,
        rate_sps: DEFAULT_RATE_SPS,
        initial_gain_tenths: 207,
        mer_target: 12.0,
        probe_period_ms: 5000,
        bail_after_changes: 15,
        device_index: 0,
        ppm: 0,
        nrsc5_path: PathBuf::from("bin\\nrsc5.exe"),
        log_path: PathBuf::from("target\\spike2-agc.log"),
        max_seconds: None,
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
            "--initial-gain" => {
                let v: f32 = iter
                    .next()
                    .ok_or_else(|| "--initial-gain expects DB".to_string())?
                    .parse()
                    .map_err(|_| "--initial-gain: invalid number".to_string())?;
                out.initial_gain_tenths = (v * 10.0).round() as i32;
            }
            "--mer-target" => {
                out.mer_target = iter
                    .next()
                    .ok_or_else(|| "--mer-target expects DB".to_string())?
                    .parse()
                    .map_err(|_| "--mer-target: invalid number".to_string())?;
            }
            "--probe-period-ms" => {
                out.probe_period_ms = iter
                    .next()
                    .ok_or_else(|| "--probe-period-ms expects MS".to_string())?
                    .parse()
                    .map_err(|_| "--probe-period-ms: invalid u64".to_string())?;
            }
            "--bail-after-changes" => {
                out.bail_after_changes = iter
                    .next()
                    .ok_or_else(|| "--bail-after-changes expects N".to_string())?
                    .parse()
                    .map_err(|_| "--bail-after-changes: invalid u32".to_string())?;
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
            "--nrsc5" => {
                out.nrsc5_path =
                    PathBuf::from(iter.next().ok_or_else(|| "--nrsc5 expects PATH".to_string())?);
            }
            "--log" => {
                out.log_path =
                    PathBuf::from(iter.next().ok_or_else(|| "--log expects PATH".to_string())?);
            }
            "--max-seconds" => {
                out.max_seconds = Some(
                    iter.next()
                        .ok_or_else(|| "--max-seconds expects N".to_string())?
                        .parse()
                        .map_err(|_| "--max-seconds: invalid u64".to_string())?,
                );
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
        "agc_pipe - Spike 2 closed-loop AGC proof of concept\n\
        \n\
        Usage:\n\
        \x20   agc_pipe [options]\n\
        \n\
        Options:\n\
        \x20   --freq MHZ                 center frequency (MHz)     (default 97.1)\n\
        \x20   --rate SPS                 sample rate (Hz)           (default 1488375)\n\
        \x20   --initial-gain DB          first gain to try (dB)     (default 20.7)\n\
        \x20   --mer-target DB            convergence MER (dB)       (default 12.0)\n\
        \x20   --probe-period-ms MS       settle window (ms)         (default 5000)\n\
        \x20   --bail-after-changes N     give up after N probes     (default 15)\n\
        \x20   --device IDX               librtlsdr device index     (default 0)\n\
        \x20   --ppm N                    frequency correction       (default 0)\n\
        \x20   --nrsc5 PATH               nrsc5.exe                  (default bin\\nrsc5.exe)\n\
        \x20   --log PATH                 AGC decision log           (default target\\spike2-agc.log)\n\
        \x20   --max-seconds N            stop after N s             (default: until Ctrl-C)\n"
    );
}

// === Helpers =============================================================

fn spawn_nrsc5(path: &Path) -> io::Result<Child> {
    // We pass freq=0 because nrsc5 reads from stdin (-r -); freq is ignored.
    // Program 0 = HD1. Log level 1 = info (enough for MER/BER/sync events).
    Command::new(path)
        .args(["-l", "1", "-r", "-", "0"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
}

// === Main ================================================================

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("agc_pipe: {}", e);
            print_help();
            return ExitCode::from(2);
        }
    };

    eprintln!("agc_pipe: loading librtlsdr.dll");
    let api = match load_api() {
        Ok(api) => api,
        Err(e) => {
            eprintln!("agc_pipe: {}", e);
            return ExitCode::from(1);
        }
    };
    if API.set(api).is_err() {
        eprintln!("agc_pipe: API already initialised (cannot happen)");
        return ExitCode::from(1);
    }
    let api = API.get().expect("just set");

    // --- Open device ---
    eprintln!("agc_pipe: opening device {}", args.device_index);
    let mut dev: *mut c_void = std::ptr::null_mut();
    let r = unsafe { (api.open)(&mut dev as *mut _, args.device_index) };
    if r != 0 || dev.is_null() {
        eprintln!(
            "agc_pipe: rtlsdr_open(index={}) failed: {}",
            args.device_index, r
        );
        return ExitCode::from(1);
    }
    DEVICE.store(dev, Ordering::SeqCst);
    unsafe {
        SetConsoleCtrlHandler(Some(ctrl_handler), 1);
    }

    macro_rules! call {
        ($f:expr, $label:literal $(, $arg:expr)*) => {{
            let r = unsafe { $f(dev $(, $arg)*) };
            if r != 0 {
                eprintln!("agc_pipe: {} -> {}", $label, r);
            }
        }};
    }

    call!(api.set_sample_rate, "set_sample_rate", args.rate_sps);
    call!(api.set_center_freq, "set_center_freq", args.freq_hz);
    call!(api.set_direct_sampling, "set_direct_sampling(0)", 0);
    call!(api.set_agc_mode, "set_agc_mode(0)", 0);
    if args.ppm != 0 {
        call!(api.set_freq_correction, "set_freq_correction", args.ppm);
    }

    // Manual gain — AGC controls it from here on.
    let initial_idx = nearest_gain_idx(args.initial_gain_tenths);
    let initial_tenths = R820T_GAINS_TENTHS[initial_idx];
    call!(api.set_tuner_gain_mode, "set_tuner_gain_mode(manual)", 1);
    call!(api.set_tuner_gain, "set_tuner_gain(initial)", initial_tenths);
    call!(api.reset_buffer, "reset_buffer");

    eprintln!(
        "agc_pipe: freq={:.4} MHz rate={} sps initial_gain={:.1} dB (idx {}/{}) device={}",
        args.freq_hz as f64 / 1_000_000.0,
        args.rate_sps,
        initial_tenths as f32 / 10.0,
        initial_idx,
        R820T_GAINS_TENTHS.len() - 1,
        args.device_index
    );

    // --- Spawn nrsc5 ---
    eprintln!("agc_pipe: spawning {}", args.nrsc5_path.display());
    let mut child = match spawn_nrsc5(&args.nrsc5_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!(
                "agc_pipe: failed to spawn nrsc5 ({}): {}",
                args.nrsc5_path.display(),
                e
            );
            return ExitCode::from(1);
        }
    };
    let nrsc5_stdin = child.stdin.take().expect("piped stdin");
    let nrsc5_stderr = child.stderr.take().expect("piped stderr");

    // --- AGC state ---
    let mut agc = match Agc::new(
        args.initial_gain_tenths,
        args.mer_target,
        Duration::from_millis(args.probe_period_ms),
        args.bail_after_changes,
        &args.log_path,
    ) {
        Ok(a) => a,
        Err(e) => {
            eprintln!(
                "agc_pipe: cannot open log file {}: {}",
                args.log_path.display(),
                e
            );
            // Best-effort cleanup of child.
            let _ = child.kill();
            return ExitCode::from(1);
        }
    };
    agc.log_line(&format!(
        "start freq={:.4} MHz initial_gain={:.1} dB mer_target={:.1} dB probe={}ms bail_after={}",
        args.freq_hz as f64 / 1_000_000.0,
        agc.current_db(),
        args.mer_target,
        args.probe_period_ms,
        args.bail_after_changes,
    ));

    // --- Spawn stderr reader thread ---
    let (ev_tx, ev_rx) = mpsc::channel::<NrscEvent>();
    let reader_tx = ev_tx.clone();
    let stderr_reader = thread::spawn(move || {
        let reader = BufReader::new(nrsc5_stderr);
        for line in reader.lines() {
            match line {
                Ok(l) => {
                    if let Some(ev) = parse_nrsc5_line(&l) {
                        let _ = reader_tx.send(ev);
                    } else {
                        let _ = reader_tx.send(NrscEvent::Raw(l));
                    }
                }
                Err(_) => break,
            }
        }
        let _ = reader_tx.send(NrscEvent::StderrClosed);
    });
    drop(ev_tx); // keep only reader_tx alive; reader drops it on EOF

    // --- Spawn librtlsdr worker thread ---
    // Heap-allocate Ctx so it survives across the read_async call. The
    // worker thread owns the Box (via raw pointer); main thread blocks
    // on the JoinHandle until read_async returns.
    let ctx_box = Box::new(Ctx {
        bytes_written: 0,
        broken_pipe: false,
        cancelled: false,
        nrsc5_stdin,
    });
    let ctx_ptr = Box::into_raw(ctx_box);
    let ctx_ptr_usize = ctx_ptr as usize; // Send-safe handoff
    let api_ref: &'static Api = api;
    let dev_usize = dev as usize;

    let worker = thread::spawn(move || {
        let dev = dev_usize as *mut c_void;
        let ctx_ptr = ctx_ptr_usize as *mut c_void;
        let start = Instant::now();
        let r = unsafe { (api_ref.read_async)(dev, rtlsdr_cb, ctx_ptr, 0, 0) };
        let elapsed = start.elapsed().as_secs_f64();
        (r, elapsed)
    });

    // --- AGC main loop ---
    let run_start = Instant::now();
    let max_run = args.max_seconds.map(Duration::from_secs);
    loop {
        if SHOULD_STOP.load(Ordering::Acquire) {
            agc.log_line("AGC: SHOULD_STOP set, exiting loop");
            break;
        }
        if let Some(limit) = max_run {
            if run_start.elapsed() >= limit {
                agc.log_line(&format!("AGC: max-seconds ({}) reached", limit.as_secs()));
                SHOULD_STOP.store(true, Ordering::SeqCst);
                break;
            }
        }
        match ev_rx.recv_timeout(Duration::from_millis(250)) {
            Ok(ev) => {
                if matches!(ev, NrscEvent::StderrClosed) {
                    agc.on_event(&ev);
                    agc.log_line("AGC: nrsc5 stderr closed, exiting loop");
                    SHOULD_STOP.store(true, Ordering::SeqCst);
                    break;
                }
                agc.on_event(&ev);
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                agc.log_line("AGC: event channel disconnected, exiting loop");
                SHOULD_STOP.store(true, Ordering::SeqCst);
                break;
            }
        }
        let dev_now = DEVICE.load(Ordering::SeqCst);
        if !dev_now.is_null() {
            agc.maybe_probe(dev_now);
        }
    }

    // --- Shutdown ---
    agc.log_line("AGC: waiting for worker thread (read_async to return) ...");
    let (r, elapsed) = worker.join().unwrap_or((-9999, 0.0));
    agc.log_line(&format!(
        "worker: read_async returned ({}) after {:.2} s",
        r, elapsed
    ));

    // Reclaim Ctx and drop nrsc5_stdin (sends EOF to nrsc5).
    let ctx = unsafe { Box::from_raw(ctx_ptr) };
    agc.log_line(&format!(
        "worker stats: {} bytes ({:.3} MB/s) broken_pipe={} cancelled={}",
        ctx.bytes_written,
        ctx.bytes_written as f64 / elapsed.max(0.001) / 1_000_000.0,
        ctx.broken_pipe,
        ctx.cancelled,
    ));
    drop(ctx); // closes nrsc5.stdin

    // Wait for nrsc5 to exit; give it a few seconds, then kill if needed.
    let wait_deadline = Instant::now() + Duration::from_secs(5);
    let nrsc5_status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) => {
                if Instant::now() >= wait_deadline {
                    agc.log_line("nrsc5: didn't exit after EOF in 5 s, killing");
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
                thread::sleep(Duration::from_millis(100));
            }
            Err(e) => {
                agc.log_line(&format!("nrsc5: try_wait failed: {}", e));
                break None;
            }
        }
    };
    if let Some(s) = nrsc5_status {
        agc.log_line(&format!("nrsc5: exited {}", s));
    }

    // Join stderr reader (should already be done).
    let _ = stderr_reader.join();

    // Close device only on clean return — same crash workaround as iq_capture.
    DEVICE.store(std::ptr::null_mut(), Ordering::SeqCst);
    if r == 0 {
        let close_r = unsafe { (api.close)(dev) };
        agc.log_line(&format!("rtlsdr_close -> {}", close_r));
    } else {
        agc.log_line(&format!(
            "skipping rtlsdr_close (read_async returned {})", r
        ));
    }

    agc.log_line(&format!(
        "DONE: settled={} bailed={} probes={} final_gain={:.1}dB best_mer={:.2}dB at {:.1}dB",
        agc.settled,
        agc.bailed_out,
        agc.probes_done,
        agc.current_db(),
        agc.best_mer_seen,
        R820T_GAINS_TENTHS[agc.best_gain_idx] as f32 / 10.0,
    ));

    ExitCode::SUCCESS
}
