//! Safe Rust wrapper around `libnrsc5` (Phase 2 of the libnrsc5
//! migration).
//!
//! This module is the **only** place outside [`super::nrsc5_sys`] that
//! is allowed to write `unsafe`. The rest of the crate consumes it via
//! the safe types exported here:
//!
//! * [`Nrsc5Session`] — RAII handle wrapping a `*mut nrsc5_t`. Drops
//!   call `nrsc5_stop` → `nrsc5_close` → reclaim the boxed callback
//!   context, in that order, so any in-flight callback completes
//!   before its environment is freed.
//! * [`Mode`] — `Fm` / `Am` selector for [`Nrsc5Session::set_mode`].
//! * [`Nrsc5ApiError`] — typed error returned by the fallible methods.
//!
//! # Callback model
//!
//! The C library invokes a single `void (*cb)(const nrsc5_event_t *,
//! void *opaque)` callback on its worker thread for **all** event
//! kinds. We split that wire-level callback into two cleanly-typed
//! Rust callbacks at the trampoline:
//!
//! * an *event* callback `Fn(NrscEvent)` for low-rate metadata,
//!   sync, MER/BER, station info, etc.;
//! * an optional *PCM* sink `Fn(u32, &[i16])` for high-rate decoded
//!   audio, so PCM doesn't have to thread through the metadata
//!   channel.
//!
//! Both are installed before [`Nrsc5Session::start`] and freed in
//! [`Drop`] only after `nrsc5_close` joins the worker thread.
//!
//! # Phase 2 status
//!
//! Nothing in the crate calls this module yet. It exists so the unsafe
//! surface area can be reviewed independently of the cutover. Phase 3
//! will retire `ffi::decoder::DecoderInstance` and route the existing
//! [`NrscEvent`] channel through this wrapper instead.

use std::cell::Cell;
use std::ffi::{c_char, c_int, c_void, CStr};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr;
use std::slice;

use thiserror::Error;

use super::nrsc5_sys as sys;
use super::NrscEvent;

// =====================================================================
// Public surface
// =====================================================================

/// Demodulation mode passed to [`Nrsc5Session::set_mode`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Fm,
    // Kept: the AM half of the nrsc5 mode mapping. The app only tunes
    // FM HD today, but this completes the FFI surface against
    // `NRSC5_MODE_AM` so adding AM HD support is a one-line UI change.
    #[allow(dead_code)]
    Am,
}

impl Mode {
    fn to_raw(self) -> i32 {
        match self {
            Mode::Fm => sys::NRSC5_MODE_FM,
            Mode::Am => sys::NRSC5_MODE_AM,
        }
    }
}

/// Errors returned by the safe wrapper. The variants that carry an
/// `i32` propagate the raw return code from libnrsc5 verbatim — see
/// upstream `nrsc5.h` for the meaning of non-zero values.
#[derive(Debug, Error)]
pub enum Nrsc5ApiError {
    #[error("nrsc5_open_pipe failed (rc={0})")]
    OpenFailed(i32),
    #[error("nrsc5_set_mode failed (rc={0})")]
    SetModeFailed(i32),
    #[error("nrsc5_set_frequency failed (rc={0})")]
    SetFrequencyFailed(i32),
    #[error("nrsc5_pipe_samples_cu8 failed (rc={0})")]
    PipeFailed(i32),
    #[error("nrsc5_pipe_samples_cs16 failed (rc={0})")]
    PipeCs16Failed(i32),
    /// `pipe_samples_cu8` received a slice larger than `u32::MAX`
    /// bytes. The C API takes a `uint32_t` length so we can't pass
    /// anything bigger in a single call; split the buffer caller-side.
    #[error("pipe sample chunk too large: {len} bytes (max {max})", max = u32::MAX)]
    PipeChunkTooLarge { len: usize },
    /// Tried to register an event callback when one was already
    /// installed on this session. Re-installation isn't supported
    /// because it would race with the C worker thread.
    #[error("event callback already registered on this session")]
    EventCallbackAlreadySet,
    /// Same as above for the PCM sink.
    #[error("PCM sink already registered on this session")]
    PcmSinkAlreadySet,
}

/// Owned handle to a running (or about-to-run) libnrsc5 pipe session.
///
/// Construct via [`Self::open_pipe`]; install callbacks via
/// [`Self::set_event_callback`] and/or [`Self::set_pcm_sink`]; then
/// drive the session with [`Self::start`] / [`Self::pipe_samples_cu8`]
/// / [`Self::stop`]. Dropping the session always tears down cleanly.
///
/// `Send` but not `Sync`: a single session must be driven from one
/// user thread. Internally libnrsc5 spawns its own worker thread for
/// callbacks; that's transparent to the caller.
pub struct Nrsc5Session {
    st: *mut sys::nrsc5_t,
    /// Boxed callback context; the raw pointer is what libnrsc5 sees
    /// via `nrsc5_set_callback`. `null` until callbacks are
    /// installed. Reclaimed in [`Drop`] **after** `nrsc5_close`
    /// returns, so the worker thread can't be in a callback when we
    /// free the box.
    ctx: *mut CallbackCtx,
}

// SAFETY: libnrsc5 allows callers to move a session between threads
// as long as only one thread drives it at a time. The internal
// worker thread the library spawns doesn't read any user-visible
// state on `Self` — it only sees the boxed CallbackCtx via the
// opaque pointer.
unsafe impl Send for Nrsc5Session {}

impl Nrsc5Session {
    /// Allocate a fresh pipe-mode session.
    pub fn open_pipe() -> Result<Self, Nrsc5ApiError> {
        let mut st: *mut sys::nrsc5_t = ptr::null_mut();
        // SAFETY: `&mut st` is a valid out-pointer. On success libnrsc5
        // writes a heap-allocated session pointer. On failure the rc is
        // non-zero and we propagate it.
        let rc = unsafe { sys::nrsc5_open_pipe(&mut st) };
        if rc != 0 || st.is_null() {
            return Err(Nrsc5ApiError::OpenFailed(rc));
        }
        Ok(Self {
            st,
            ctx: ptr::null_mut(),
        })
    }

    /// Return libnrsc5's version string (e.g. `"3.1.0"`). Static —
    /// no session required.
    pub fn library_version() -> String {
        let mut p: *const c_char = ptr::null();
        // SAFETY: the C API writes a pointer to a static string into
        // `*version`. We never free it.
        unsafe { sys::nrsc5_get_version(&mut p) };
        unsafe { cstr_to_string(p) }
    }

    /// Set FM or AM mode. Must be called before [`Self::start`].
    pub fn set_mode(&self, mode: Mode) -> Result<(), Nrsc5ApiError> {
        // SAFETY: `self.st` is a valid session pointer for as long as
        // `self` is alive.
        let rc = unsafe { sys::nrsc5_set_mode(self.st, mode.to_raw()) };
        if rc != 0 {
            Err(Nrsc5ApiError::SetModeFailed(rc))
        } else {
            Ok(())
        }
    }

    /// Set the metadata frequency (Hz). In pipe mode this does not
    /// tune any hardware — the Soapy layer owns the tuner. The value
    /// is reflected back via station-info events.
    pub fn set_frequency_hz(&self, hz: f32) -> Result<(), Nrsc5ApiError> {
        // SAFETY: as in `set_mode`.
        let rc = unsafe { sys::nrsc5_set_frequency(self.st, hz) };
        if rc != 0 {
            Err(Nrsc5ApiError::SetFrequencyFailed(rc))
        } else {
            Ok(())
        }
    }

    /// Install the metadata event callback. Called on libnrsc5's
    /// worker thread for every translated [`NrscEvent`] — sync /
    /// MER / BER / metadata / station info / AGC. PCM audio is
    /// **not** delivered here; install a [`set_pcm_sink`] for that.
    ///
    /// [`set_pcm_sink`]: Self::set_pcm_sink
    pub fn set_event_callback<F>(&mut self, cb: F) -> Result<(), Nrsc5ApiError>
    where
        F: Fn(NrscEvent) + Send + Sync + 'static,
    {
        self.install_callbacks(Some(Box::new(cb)), None)
    }

    /// Install a PCM audio sink. Called on libnrsc5's worker thread
    /// for every decoded audio buffer. `program` is the HD subchannel
    /// (0-based); `samples` is interleaved stereo `s16le` at
    /// 44.1 kHz. The slice is **borrowed for the duration of the
    /// callback only** — copy if it needs to outlive the call.
    pub fn set_pcm_sink<F>(&mut self, sink: F) -> Result<(), Nrsc5ApiError>
    where
        F: Fn(u32, &[i16]) + Send + Sync + 'static,
    {
        self.install_callbacks(None, Some(Box::new(sink)))
    }

    /// Shared core for `set_event_callback` and `set_pcm_sink`. We
    /// merge both callbacks into one [`CallbackCtx`] so libnrsc5 only
    /// ever holds one opaque pointer per session. On first install we
    /// also wire the trampoline via `nrsc5_set_callback`; on
    /// subsequent installs we only mutate the box (safe because we
    /// require this to happen before [`Self::start`], so no worker
    /// thread is reading it yet).
    fn install_callbacks(
        &mut self,
        event_cb: Option<EventCallback>,
        pcm_sink: Option<PcmSink>,
    ) -> Result<(), Nrsc5ApiError> {
        if self.ctx.is_null() {
            let ctx = Box::into_raw(Box::new(CallbackCtx {
                event_cb,
                pcm_sink,
                bitrate: BitrateAccum::default(),
            }));
            // SAFETY: `ctx` is freshly leaked from a Box, so the pointer
            // is valid; `trampoline` matches the C ABI declared by
            // `nrsc5_callback_t`.
            unsafe {
                sys::nrsc5_set_callback(self.st, Some(trampoline), ctx as *mut c_void);
            }
            self.ctx = ctx;
            return Ok(());
        }

        // SAFETY: we created the box, no other thread is reading it
        // yet (caller contract: install all callbacks before `start`).
        let ctx = unsafe { &mut *self.ctx };
        if let Some(cb) = event_cb {
            if ctx.event_cb.is_some() {
                return Err(Nrsc5ApiError::EventCallbackAlreadySet);
            }
            ctx.event_cb = Some(cb);
        }
        if let Some(sink) = pcm_sink {
            if ctx.pcm_sink.is_some() {
                return Err(Nrsc5ApiError::PcmSinkAlreadySet);
            }
            ctx.pcm_sink = Some(sink);
        }
        Ok(())
    }

    /// Start the worker. After this returns, callbacks may fire on
    /// the libnrsc5 worker thread until [`Self::stop`] is called.
    pub fn start(&self) {
        // SAFETY: `self.st` is a valid session pointer.
        unsafe { sys::nrsc5_start(self.st) };
    }

    /// Stop the worker and wait for it to become idle. Pending
    /// callbacks complete before this returns.
    // Kept: lifecycle pair to `start()` completing the safe FFI
    // wrapper. Session teardown currently routes through Drop/close,
    // so no production caller invokes this directly.
    #[allow(dead_code)]
    pub fn stop(&self) {
        // SAFETY: `self.st` is a valid session pointer.
        unsafe { sys::nrsc5_stop(self.st) };
    }

    /// Push a chunk of unsigned-8-bit complex I/Q samples
    /// (interleaved I, Q) at [`NRSC5_SAMPLE_RATE_CU8`] = 1.488375
    /// Msps. Must be called while the worker is running (between
    /// `start` and `stop`).
    ///
    /// [`NRSC5_SAMPLE_RATE_CU8`]: super::nrsc5_sys::NRSC5_SAMPLE_RATE_CU8
    pub fn pipe_samples_cu8(&self, samples: &[u8]) -> Result<(), Nrsc5ApiError> {
        let len: u32 = samples
            .len()
            .try_into()
            .map_err(|_| Nrsc5ApiError::PipeChunkTooLarge { len: samples.len() })?;
        // SAFETY: `samples` is a live borrow for the duration of the
        // call; `len` is its element count (also byte count, u8s).
        let rc = unsafe { sys::nrsc5_pipe_samples_cu8(self.st, samples.as_ptr(), len) };
        if rc != 0 {
            Err(Nrsc5ApiError::PipeFailed(rc))
        } else {
            Ok(())
        }
    }

    /// Push a chunk of signed-16-bit complex I/Q samples (interleaved I, Q).
    /// FM expects [`NRSC5_SAMPLE_RATE_CS16_FM`] and AM expects
    /// [`NRSC5_SAMPLE_RATE_CS16_AM`]. Must be called while the worker is
    /// running (between `start` and `stop`).
    pub fn pipe_samples_cs16(&self, samples: &[i16]) -> Result<(), Nrsc5ApiError> {
        let len: u32 = samples
            .len()
            .try_into()
            .map_err(|_| Nrsc5ApiError::PipeChunkTooLarge { len: samples.len() })?;
        let rc = unsafe { sys::nrsc5_pipe_samples_cs16(self.st, samples.as_ptr(), len) };
        if rc != 0 {
            Err(Nrsc5ApiError::PipeCs16Failed(rc))
        } else {
            Ok(())
        }
    }
}

impl Drop for Nrsc5Session {
    fn drop(&mut self) {
        if self.st.is_null() {
            return;
        }
        // Order matters:
        //   1. `nrsc5_stop` — request shutdown.
        //   2. `nrsc5_close` — joins the worker thread; on return no
        //      callbacks can fire any more.
        //   3. Free the boxed callback context — safe now because
        //      step 2 guarantees no in-flight reads.
        // SAFETY: pointer validity per the type invariants above.
        unsafe {
            sys::nrsc5_stop(self.st);
            sys::nrsc5_close(self.st);
            self.st = ptr::null_mut();
            if !self.ctx.is_null() {
                drop(Box::from_raw(self.ctx));
                self.ctx = ptr::null_mut();
            }
        }
    }
}

// =====================================================================
// Callback plumbing
// =====================================================================

type EventCallback = Box<dyn Fn(NrscEvent) + Send + Sync + 'static>;
type PcmSink = Box<dyn Fn(u32, &[i16]) + Send + Sync + 'static>;

/// Boxed and handed to libnrsc5 as `opaque`. Both callback fields are
/// optional so a caller can install only the sink they care about.
struct CallbackCtx {
    event_cb: Option<EventCallback>,
    pcm_sink: Option<PcmSink>,
    /// Per-program accumulator used to derive the decoded audio bit
    /// rate from the raw HDC packet stream. Mutated only on libnrsc5's
    /// single worker thread inside [`trampoline`], so `Cell` interior
    /// mutability is sound without locking.
    bitrate: BitrateAccum,
}

/// Number of CRC-valid HDC frames to average over before emitting a
/// bit-rate estimate. Matches the upstream nrsc5 CLI's window.
const BITRATE_FRAME_WINDOW: u32 = 32;

/// Per-program HDC accumulator that reproduces the nrsc5 CLI's
/// `Audio bit rate:` calculation on stock libnrsc5 (v3.2.0 has no
/// decoded-bit-rate event). For each complete HDC packet we add its
/// byte count, and for each CRC-valid packet we count a frame; every
/// [`BITRATE_FRAME_WINDOW`] valid frames we emit
///
/// ```text
/// kbps = bytes * 8 * SAMPLE_RATE_AUDIO / AUDIO_FRAME_SAMPLES / frames / 1000
/// ```
///
/// and reset that program's counters. Indexed by 0-based program;
/// libnrsc5 caps the program count at 8.
///
#[derive(Default)]
struct BitrateAccum {
    bytes: [Cell<u64>; 8],
    frames: [Cell<u32>; 8],
}

impl BitrateAccum {
    /// Feed one HDC packet. `bytes` is the packet size, `crc_ok` is
    /// `true` when the CRC-error flag is clear. Returns `Some(kbps)`
    /// once a full window of CRC-valid frames has accumulated (and
    /// resets that program's counters), otherwise `None`.
    fn push(&self, program: usize, bytes: usize, crc_ok: bool) -> Option<f32> {
        if program >= self.bytes.len() {
            return None;
        }
        // Every complete packet contributes its bytes (mirrors the
        // CLI, which sums packet size before the CRC check).
        let total_bytes = self.bytes[program].get() + bytes as u64;
        self.bytes[program].set(total_bytes);
        if !crc_ok {
            return None;
        }
        let frames = self.frames[program].get() + 1;
        if frames < BITRATE_FRAME_WINDOW {
            self.frames[program].set(frames);
            return None;
        }
        // Window complete — compute, then reset for the next window.
        let kbps = total_bytes as f64 * 8.0 * sys::NRSC5_SAMPLE_RATE_AUDIO as f64
            / sys::NRSC5_AUDIO_FRAME_SAMPLES as f64
            / frames as f64
            / 1000.0;
        self.bytes[program].set(0);
        self.frames[program].set(0);
        Some(kbps as f32)
    }
}

/// The single C-ABI entry point libnrsc5 invokes. Splits the wire-level
/// event into typed Rust callbacks and panic-isolates the closures —
/// unwinding into C is undefined behaviour.
unsafe extern "C" fn trampoline(evt: *const sys::nrsc5_event_t, opaque: *mut c_void) {
    // `AssertUnwindSafe` is OK here because nothing we touch survives
    // a panic — the &CallbackCtx is borrowed only for this call, the
    // raw event pointer is C-owned, and the closures themselves are
    // user code we are deliberately isolating.
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if evt.is_null() || opaque.is_null() {
            return;
        }
        // SAFETY: `opaque` is the same pointer we boxed in
        // `install_callbacks`; it remains valid until `Drop`, which
        // waits for `nrsc5_close` to return before freeing.
        let ctx = unsafe { &*(opaque as *const CallbackCtx) };
        // SAFETY: `evt` is non-null and points to a transient
        // `nrsc5_event_t` owned by the library for this call.
        let evt = unsafe { &*evt };

        match evt.event {
            sys::NRSC5_EVENT_AUDIO => {
                if let Some(sink) = ctx.pcm_sink.as_deref() {
                    // SAFETY: when tag == AUDIO, libnrsc5 guarantees
                    // the `audio` variant of the union is the live one.
                    let a = unsafe { &evt.payload.audio };
                    if !a.data.is_null() && a.count > 0 {
                        // SAFETY: `data` is valid for `count`
                        // contiguous i16 samples (per the C API contract)
                        // for the duration of the callback.
                        let samples = unsafe { slice::from_raw_parts(a.data, a.count) };
                        sink(a.program, samples);
                    }
                }
            }
            tag => {
                if let Some(cb) = ctx.event_cb.as_deref() {
                    // SAFETY: `translate_event` is only allowed to
                    // read the variant matching `tag`; matched per-arm
                    // inside the function.
                    let events = unsafe { translate_event(tag, &evt.payload, &ctx.bitrate) };
                    for ev in events {
                        cb(ev);
                    }
                }
            }
        }
    }));
}

/// Translate one C event into 0..N high-level [`NrscEvent`]s. Variants
/// that don't map to anything the rest of the crate consumes today
/// return an empty `Vec`; Phase 4 may surface more of them (richer
/// AGC, exciter info, leap-second, local time, ID3 comments).
///
/// # Safety
///
/// `tag` must be the live discriminant of `payload`. The caller in
/// [`trampoline`] obtains both fields from the same event struct so
/// this invariant is upheld.
unsafe fn translate_event(
    tag: u32,
    payload: &sys::nrsc5_event_payload,
    bitrate: &BitrateAccum,
) -> Vec<NrscEvent> {
    let mut out: Vec<NrscEvent> = Vec::new();
    match tag {
        sys::NRSC5_EVENT_LOST_DEVICE => out.push(NrscEvent::LostDevice),
        sys::NRSC5_EVENT_SYNC => {
            // v3.2.0 AM-mode supplementary indicators. libnrsc5 sets
            // all four to -1 in FM mode — emit `SyncAm` only when at
            // least one carries real data, so FM consumers stay clean.
            let s = unsafe { payload.sync };
            out.push(NrscEvent::Sync {
                freq_offset_hz: s.freq_offset,
                psmi: s.psmi,
            });
            if s.pli != -1 || s.hppi != -1 || s.aabi != -1 || s.rdbi != -1 {
                out.push(NrscEvent::SyncAm {
                    pli: s.pli,
                    hppi: s.hppi,
                    aabi: s.aabi,
                    rdbi: s.rdbi,
                });
            }
        }
        sys::NRSC5_EVENT_LOST_SYNC => out.push(NrscEvent::LostSync),
        sys::NRSC5_EVENT_MER => {
            // SAFETY: union access; tag matches.
            let m = unsafe { payload.mer };
            out.push(NrscEvent::Mer {
                lower: m.lower,
                upper: m.upper,
            });
        }
        sys::NRSC5_EVENT_BER => {
            let b = unsafe { payload.ber };
            out.push(NrscEvent::Ber { cber: b.cber });
        }
        sys::NRSC5_EVENT_ID3 => {
            let id3 = unsafe { payload.id3 };
            let title = unsafe { cstr_to_string(id3.title) };
            let artist = unsafe { cstr_to_string(id3.artist) };
            let album = unsafe { cstr_to_string(id3.album) };
            let genre = unsafe { cstr_to_string(id3.genre) };
            if !(title.is_empty() && artist.is_empty() && album.is_empty() && genre.is_empty()) {
                out.push(NrscEvent::Metadata {
                    program: id3.program,
                    title,
                    artist,
                    album,
                    genre,
                });
            }
            // xhdr is embedded; libnrsc5 sets `lot < 0` when absent.
            if id3.xhdr.lot >= 0 {
                out.push(NrscEvent::Xhdr {
                    program: id3.program,
                    mime: id3.xhdr.mime,
                    param: id3.xhdr.param as u32,
                    lot: id3.xhdr.lot.to_string(),
                });
            }
        }
        sys::NRSC5_EVENT_LOT => {
            let lot = unsafe { payload.lot };
            // Copy the payload bytes out before returning — libnrsc5
            // owns the buffer for the duration of this callback only.
            // A null pointer or zero size both mean "no payload" and
            // yield an empty Vec; the app layer will then skip the
            // disk write (an empty file would corrupt cover-art /
            // map processors that expect real image data).
            let data = if lot.data.is_null() || lot.size == 0 {
                Vec::new()
            } else {
                unsafe { slice::from_raw_parts(lot.data, lot.size as usize) }.to_vec()
            };
            let lot_component_mime = if lot.component.is_null() {
                None
            } else {
                // SAFETY: `lot.component` is non-null for this branch and
                // valid for the lifetime of this callback.
                let component = unsafe { &*lot.component };
                if component.type_ == sys::NRSC5_SIG_COMPONENT_DATA {
                    // SAFETY: union variant matches `type_`.
                    Some(unsafe { component.variant.data.mime })
                } else {
                    None
                }
            };
            // Phase 2 placeholder: program=0. The per-decoder event
            // callback in `Nrsc5Process::spawn_decoder` rewrites this
            // to the right subchannel; Phase 5 (multi-program decode)
            // will derive it from `lot.service` / `lot.component`.
            out.push(NrscEvent::LotFile {
                program: 0,
                lot: lot.lot.to_string(),
                name: unsafe { cstr_to_string(lot.name) },
                data,
                mime: lot.mime,
                lot_component_mime,
            });
        }
        sys::NRSC5_EVENT_SIS => {
            let s = unsafe { payload.sis };
            let name = unsafe { cstr_to_string(s.name) };
            if !name.is_empty() {
                out.push(NrscEvent::StationName(name));
            }
            let slogan = unsafe { cstr_to_string(s.slogan) };
            if !slogan.is_empty() {
                out.push(NrscEvent::Slogan(slogan));
            }
            let message = unsafe { cstr_to_string(s.message) };
            if !message.is_empty() {
                out.push(NrscEvent::Message(message));
            }
            let alert = unsafe { cstr_to_string(s.alert) };
            if !alert.is_empty() {
                out.push(NrscEvent::EmergencyAlert { text: alert });
            }
            let country = unsafe { cstr_to_string(s.country_code) };
            if s.fcc_facility_id > 0 || !country.is_empty() {
                let facility_id = if s.fcc_facility_id > 0 {
                    s.fcc_facility_id as u32
                } else {
                    0
                };
                out.push(NrscEvent::CountryFcc {
                    country,
                    facility_id,
                });
            }
            // libnrsc5 reports lat/lon as 0.0 when the SIS frame
            // doesn't carry a location; suppress that case.
            if s.latitude.abs() > f32::EPSILON || s.longitude.abs() > f32::EPSILON {
                out.push(NrscEvent::Location {
                    latitude: s.latitude as f64,
                    longitude: s.longitude as f64,
                    altitude_m: s.altitude,
                });
            }
            // Walk audio_services linked list → AudioProgram per node.
            let mut node = s.audio_services;
            while !node.is_null() {
                // SAFETY: `node` is non-null; lifetime is tied to the
                // event call, which is shorter than this closure.
                let asd = unsafe { &*node };
                out.push(NrscEvent::AudioProgram {
                    number: asd.program + 1,
                    program_type: program_type_name(asd.type_).to_string(),
                    sound_experience: sound_experience_name(asd.sound_exp).to_string(),
                });
                node = asd.next;
            }
        }
        sys::NRSC5_EVENT_SIG => {
            let sig = unsafe { payload.sig };
            let mut svc = sig.services;
            while !svc.is_null() {
                let s = unsafe { &*svc };
                let name = unsafe { cstr_to_string(s.name) };
                match s.type_ {
                    sys::NRSC5_SIG_SERVICE_AUDIO => out.push(NrscEvent::SigServiceAudio {
                        number: s.number as u32,
                        name,
                    }),
                    sys::NRSC5_SIG_SERVICE_DATA => out.push(NrscEvent::SigServiceData {
                        number: s.number as u32,
                        name,
                    }),
                    _ => {}
                }
                svc = s.next;
            }
        }
        sys::NRSC5_EVENT_STATION_NAME => {
            let n = unsafe { cstr_to_string(payload.station_name.name) };
            if !n.is_empty() {
                out.push(NrscEvent::StationName(n));
            }
        }
        sys::NRSC5_EVENT_STATION_SLOGAN => {
            let s = unsafe { cstr_to_string(payload.station_slogan.slogan) };
            if !s.is_empty() {
                out.push(NrscEvent::Slogan(s));
            }
        }
        sys::NRSC5_EVENT_STATION_MESSAGE => {
            let m = unsafe { cstr_to_string(payload.station_message.message) };
            if !m.is_empty() {
                out.push(NrscEvent::Message(m));
            }
        }
        sys::NRSC5_EVENT_STATION_LOCATION => {
            let l = unsafe { payload.station_location };
            out.push(NrscEvent::Location {
                latitude: l.latitude as f64,
                longitude: l.longitude as f64,
                altitude_m: l.altitude,
            });
        }
        sys::NRSC5_EVENT_STATION_ID => {
            let s = unsafe { payload.station_id };
            let country = unsafe { cstr_to_string(s.country_code) };
            let facility_id = if s.fcc_facility_id > 0 {
                s.fcc_facility_id as u32
            } else {
                0
            };
            out.push(NrscEvent::CountryFcc {
                country,
                facility_id,
            });
        }
        sys::NRSC5_EVENT_AUDIO_SERVICE_DESCRIPTOR => {
            let a = unsafe { payload.asd };
            out.push(NrscEvent::AudioProgram {
                number: a.program + 1,
                program_type: program_type_name(a.type_).to_string(),
                sound_experience: sound_experience_name(a.sound_exp).to_string(),
            });
        }
        sys::NRSC5_EVENT_EMERGENCY_ALERT => {
            let e = unsafe { payload.emergency_alert };
            let text = unsafe { cstr_to_string(e.message) };
            if !text.is_empty() {
                out.push(NrscEvent::EmergencyAlert { text });
            }
        }
        sys::NRSC5_EVENT_HERE_IMAGE => {
            let h = unsafe { payload.here_image };
            // Copy the HERE payload bytes out before returning; libnrsc5
            // owns the source buffer only for the callback duration.
            let data = if h.data.is_null() || h.size == 0 {
                Vec::new()
            } else {
                unsafe { slice::from_raw_parts(h.data, h.size as usize) }.to_vec()
            };
            out.push(NrscEvent::HereImage {
                image_type: h.image_type,
                seq: h.seq,
                n1: h.n1,
                n2: h.n2,
                latitude1: h.latitude1,
                longitude1: h.longitude1,
                latitude2: h.latitude2,
                longitude2: h.longitude2,
                has_time_utc: !h.time_utc.is_null(),
                name: unsafe { cstr_to_string(h.name) },
                size: h.size,
                data,
            });
        }
        sys::NRSC5_EVENT_AGC => {
            let a = unsafe { payload.agc };
            out.push(NrscEvent::Agc { gain_db: a.gain_db });
        }
        sys::NRSC5_EVENT_EXCITER_INFO => {
            let e = unsafe { payload.exciter_info };
            out.push(NrscEvent::ExciterInfo {
                manufacturer_id: unsafe { cstr_to_string(e.manufacturer_id) },
                core_version: e.core_version,
                core_status: e.core_status,
                manufacturer_version: e.manufacturer_version,
                manufacturer_status: e.manufacturer_status,
                importer_connected: e.importer_connected != 0,
            });
        }
        sys::NRSC5_EVENT_IMPORTER_INFO => {
            let i = unsafe { payload.importer_info };
            out.push(NrscEvent::ImporterInfo {
                manufacturer_id: unsafe { cstr_to_string(i.manufacturer_id) },
                core_version: i.core_version,
                core_status: i.core_status,
                manufacturer_version: i.manufacturer_version,
                manufacturer_status: i.manufacturer_status,
            });
        }
        sys::NRSC5_EVENT_LEAP_SECOND_OFFSET => {
            let l = unsafe { payload.leap_second_offset };
            out.push(NrscEvent::LeapSecondOffset {
                pending_offset: l.pending_offset,
                current_offset: l.current_offset,
                pending_alfn: l.pending_alfn,
            });
        }
        sys::NRSC5_EVENT_LOCAL_TIME => {
            let t = unsafe { payload.local_time };
            // dst_schedule is 0..=2; clamp defensively for the u8 cast.
            let dst_schedule = t.dst_schedule.clamp(0, u8::MAX as c_int) as u8;
            out.push(NrscEvent::LocalTime {
                utc_offset_minutes: t.utc_offset,
                dst_regional: t.dst_regional != 0,
                dst_local: t.dst_local != 0,
                dst_schedule,
            });
        }
        sys::NRSC5_EVENT_HDC => {
            // Stock libnrsc5 (v3.2.0) has no decoded-bit-rate event, so
            // derive it here from the raw HDC packet stream exactly like
            // the nrsc5 CLI: sum packet bytes, count CRC-valid frames,
            // and emit a per-program estimate every
            // `BITRATE_FRAME_WINDOW` valid frames.
            // SAFETY: union access; tag matches.
            let h = unsafe { payload.hdc };
            let crc_ok = h.flags & sys::NRSC5_PKT_FLAGS_CRC_ERROR == 0;
            if let Some(kbps) = bitrate.push(h.program as usize, h.count, crc_ok) {
                out.push(NrscEvent::AudioBitRate {
                    program: h.program,
                    kbps,
                });
            }
        }
        // Not surfaced:
        //   IQ, STREAM, PACKET — internal / raw.
        //   AUDIO — handled by the PCM sink path in `trampoline`.
        //   AUDIO_SERVICE (codec/blend/latency) — no consumer today;
        //     bit rate is computed differently (Phase 3 may add a
        //     periodic rate estimator on the PCM sink path).
        //   DATA_SERVICE_DESCRIPTOR — SigServiceData covers it.
        //   LOT_HEADER, LOT_FRAGMENT — only the assembled LOT matters.
        _ => {}
    }
    out
}

/// Convert a C string pointer to an owned Rust `String` (lossy on
/// non-UTF-8 input — libnrsc5 has been observed to emit Latin-1
/// quote marks in real broadcasts, so strict UTF-8 would error on
/// well-formed signals).
///
/// # Safety
///
/// `p` must be either null or a valid pointer to a NUL-terminated
/// C string with a lifetime that covers this call.
unsafe fn cstr_to_string(p: *const c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    // SAFETY: per the function's safety contract.
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

/// Short human label for `NRSC5_PROGRAM_TYPE_*` values, matching the
/// strings the existing stderr-parser path emits today. Anything we
/// don't recognise stringifies as `"Type N"` so the UI keeps showing
/// *something* rather than going blank.
fn program_type_name(t: u32) -> String {
    let label: &'static str = match t {
        sys::NRSC5_PROGRAM_TYPE_UNDEFINED => "Undefined",
        sys::NRSC5_PROGRAM_TYPE_NEWS => "News",
        sys::NRSC5_PROGRAM_TYPE_INFORMATION => "Information",
        sys::NRSC5_PROGRAM_TYPE_SPORTS => "Sports",
        sys::NRSC5_PROGRAM_TYPE_TALK => "Talk",
        sys::NRSC5_PROGRAM_TYPE_ROCK => "Rock",
        sys::NRSC5_PROGRAM_TYPE_CLASSIC_ROCK => "Classic Rock",
        sys::NRSC5_PROGRAM_TYPE_ADULT_HITS => "Adult Hits",
        sys::NRSC5_PROGRAM_TYPE_SOFT_ROCK => "Soft Rock",
        sys::NRSC5_PROGRAM_TYPE_TOP_40 => "Top 40",
        sys::NRSC5_PROGRAM_TYPE_COUNTRY => "Country",
        sys::NRSC5_PROGRAM_TYPE_OLDIES => "Oldies",
        sys::NRSC5_PROGRAM_TYPE_SOFT => "Soft",
        sys::NRSC5_PROGRAM_TYPE_NOSTALGIA => "Nostalgia",
        sys::NRSC5_PROGRAM_TYPE_JAZZ => "Jazz",
        sys::NRSC5_PROGRAM_TYPE_CLASSICAL => "Classical",
        sys::NRSC5_PROGRAM_TYPE_RHYTHM_AND_BLUES => "Rhythm and Blues",
        sys::NRSC5_PROGRAM_TYPE_SOFT_RHYTHM_AND_BLUES => "Soft Rhythm and Blues",
        sys::NRSC5_PROGRAM_TYPE_FOREIGN_LANGUAGE => "Foreign Language",
        sys::NRSC5_PROGRAM_TYPE_RELIGIOUS_MUSIC => "Religious Music",
        sys::NRSC5_PROGRAM_TYPE_RELIGIOUS_TALK => "Religious Talk",
        sys::NRSC5_PROGRAM_TYPE_PERSONALITY => "Personality",
        sys::NRSC5_PROGRAM_TYPE_PUBLIC => "Public",
        sys::NRSC5_PROGRAM_TYPE_COLLEGE => "College",
        sys::NRSC5_PROGRAM_TYPE_SPANISH_TALK => "Spanish Talk",
        sys::NRSC5_PROGRAM_TYPE_SPANISH_MUSIC => "Spanish Music",
        sys::NRSC5_PROGRAM_TYPE_HIP_HOP => "Hip Hop",
        sys::NRSC5_PROGRAM_TYPE_WEATHER => "Weather",
        sys::NRSC5_PROGRAM_TYPE_EMERGENCY_TEST => "Emergency Test",
        sys::NRSC5_PROGRAM_TYPE_EMERGENCY => "Emergency",
        sys::NRSC5_PROGRAM_TYPE_TRAFFIC => "Traffic",
        sys::NRSC5_PROGRAM_TYPE_SPECIAL_READING_SERVICES => "Special Reading Services",
        _ => "",
    };
    if label.is_empty() {
        format!("Type {t}")
    } else {
        label.to_string()
    }
}

/// Short label for the SIS sound-experience field. libnrsc5 doesn't
/// expose explicit constants for these values; the labels mirror what
/// the stderr-parser path observes in practice today.
fn sound_experience_name(s: u32) -> String {
    let label: &'static str = match s {
        0 => "Unspecified",
        1 => "Mono",
        2 => "Stereo",
        3 => "Stereo Surround",
        4 => "Surround",
        _ => "",
    };
    if label.is_empty() {
        format!("SoundExp {s}")
    } else {
        label.to_string()
    }
}

// =====================================================================
// Tests — exercise the trampoline + translation table without a live
// libnrsc5 session. We never call any `#[link]`-ed function here so
// these tests don't require libnrsc5.dll at link or run time.
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;
    use std::sync::{Arc, Mutex};

    /// All variants of `nrsc5_event_payload` are POD with pointer /
    /// integer / float fields; a zeroed union is therefore safe to
    /// construct (every variant reads "null pointer / 0 / 0.0").
    fn zeroed_payload() -> sys::nrsc5_event_payload {
        // SAFETY: see above.
        unsafe { std::mem::zeroed() }
    }

    fn dispatch(evt: sys::nrsc5_event_t, ctx: &mut CallbackCtx) {
        let ctx_ptr = ctx as *mut CallbackCtx as *mut c_void;
        // SAFETY: `evt` is borrowed as `*const` only for the call;
        // `ctx_ptr` is valid for the duration of the call.
        unsafe { trampoline(&evt as *const _, ctx_ptr) };
    }

    fn capture() -> (CallbackCtx, Arc<Mutex<Vec<NrscEvent>>>) {
        let captured: Arc<Mutex<Vec<NrscEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = captured.clone();
        let cb: EventCallback = Box::new(move |ev| sink.lock().unwrap().push(ev));
        (
            CallbackCtx {
                event_cb: Some(cb),
                pcm_sink: None,
                bitrate: BitrateAccum::default(),
            },
            captured,
        )
    }

    #[test]
    fn translates_sync() {
        let (mut ctx, out) = capture();
        let mut payload = zeroed_payload();
        // FM mode: libnrsc5 sets all four AM-only indicators to -1.
        // Without these sentinels, the v3.2.0 `Sync` arm would also
        // emit a `SyncAm` event from the zeroed AM fields.
        payload.sync = sys::nrsc5_event_sync {
            freq_offset: 1.25,
            psmi: 11,
            pli: -1,
            hppi: -1,
            aabi: -1,
            rdbi: -1,
        };
        dispatch(
            sys::nrsc5_event_t {
                event: sys::NRSC5_EVENT_SYNC,
                payload,
            },
            &mut ctx,
        );
        let got = out.lock().unwrap();
        assert_eq!(got.len(), 1);
        match got[0] {
            NrscEvent::Sync {
                freq_offset_hz,
                psmi,
            } => {
                assert_eq!(freq_offset_hz, 1.25);
                assert_eq!(psmi, 11);
            }
            ref other => panic!("expected Sync, got {other:?}"),
        }
    }

    #[test]
    fn translates_sync_am() {
        // AM mode: libnrsc5 reports real values for pli/hppi/aabi/rdbi.
        // Translation should emit Sync followed by SyncAm.
        let (mut ctx, out) = capture();
        let mut payload = zeroed_payload();
        payload.sync = sys::nrsc5_event_sync {
            freq_offset: -0.75,
            psmi: 3,
            pli: 3,
            hppi: 1,
            aabi: 2,
            rdbi: 0,
        };
        dispatch(
            sys::nrsc5_event_t {
                event: sys::NRSC5_EVENT_SYNC,
                payload,
            },
            &mut ctx,
        );
        let got = out.lock().unwrap();
        assert_eq!(got.len(), 2);
        match got[0] {
            NrscEvent::Sync {
                freq_offset_hz,
                psmi,
            } => {
                assert_eq!(freq_offset_hz, -0.75);
                assert_eq!(psmi, 3);
            }
            ref other => panic!("expected Sync, got {other:?}"),
        }
        match got[1] {
            NrscEvent::SyncAm {
                pli,
                hppi,
                aabi,
                rdbi,
            } => {
                assert_eq!(pli, 3);
                assert_eq!(hppi, 1);
                assert_eq!(aabi, 2);
                assert_eq!(rdbi, 0);
            }
            ref other => panic!("expected SyncAm, got {other:?}"),
        }
    }

    /// Feed an HDC event. `bytes` is the packet size, `crc_ok` clears
    /// the CRC-error flag.
    fn dispatch_hdc(ctx: &mut CallbackCtx, program: u32, bytes: usize, crc_ok: bool) {
        let mut payload = zeroed_payload();
        payload.hdc = sys::nrsc5_event_hdc {
            program,
            data: ptr::null(),
            count: bytes,
            flags: if crc_ok {
                sys::NRSC5_PKT_FLAGS_NONE
            } else {
                sys::NRSC5_PKT_FLAGS_CRC_ERROR
            },
        };
        dispatch(
            sys::nrsc5_event_t {
                event: sys::NRSC5_EVENT_HDC,
                payload,
            },
            ctx,
        );
    }

    #[test]
    fn hdc_emits_bitrate_after_full_window() {
        let (mut ctx, out) = capture();
        // A constant 1024-byte packet over a full 32-frame window.
        // kbps = 1024*32 * 8 * 44100 / 2048 / 32 / 1000 = 176.4
        let bytes = 1024usize;
        for _ in 0..BITRATE_FRAME_WINDOW {
            dispatch_hdc(&mut ctx, 0, bytes, true);
        }
        let got = out.lock().unwrap();
        assert_eq!(got.len(), 1, "exactly one bit-rate event per window");
        match got[0] {
            NrscEvent::AudioBitRate { program, kbps } => {
                assert_eq!(program, 0);
                assert!(
                    (kbps - 176.4).abs() < 0.05,
                    "expected ~176.4 kbps, got {kbps}"
                );
            }
            ref other => panic!("expected AudioBitRate, got {other:?}"),
        }
    }

    #[test]
    fn hdc_no_emit_before_window() {
        let (mut ctx, out) = capture();
        for _ in 0..(BITRATE_FRAME_WINDOW - 1) {
            dispatch_hdc(&mut ctx, 0, 1024, true);
        }
        assert!(
            out.lock().unwrap().is_empty(),
            "no event until a full window of CRC-valid frames"
        );
    }

    #[test]
    fn hdc_crc_error_counts_bytes_not_frames() {
        // CRC-error packets add bytes but don't advance the frame
        // counter, so they inflate the averaged byte total without
        // shortening the window — mirroring the nrsc5 CLI.
        let (mut ctx, out) = capture();
        // One CRC-error packet, then a full window of valid 1024-byte
        // packets. Total bytes = 1024*33, frames = 32.
        // kbps = 1024*33 * 8 * 44100 / 2048 / 32 / 1000 = 181.9125
        dispatch_hdc(&mut ctx, 0, 1024, false);
        for _ in 0..BITRATE_FRAME_WINDOW {
            dispatch_hdc(&mut ctx, 0, 1024, true);
        }
        let got = out.lock().unwrap();
        assert_eq!(got.len(), 1);
        match got[0] {
            NrscEvent::AudioBitRate { program, kbps } => {
                assert_eq!(program, 0);
                assert!(
                    (kbps - 181.9125).abs() < 0.05,
                    "expected ~181.9 kbps, got {kbps}"
                );
            }
            ref other => panic!("expected AudioBitRate, got {other:?}"),
        }
    }

    #[test]
    fn hdc_windows_are_per_program() {
        // Program 0 completes a window; program 1 does not. Only the
        // program-0 estimate should be emitted, tagged with program 0.
        let (mut ctx, out) = capture();
        for _ in 0..BITRATE_FRAME_WINDOW {
            dispatch_hdc(&mut ctx, 0, 512, true);
        }
        for _ in 0..(BITRATE_FRAME_WINDOW - 1) {
            dispatch_hdc(&mut ctx, 1, 512, true);
        }
        let got = out.lock().unwrap();
        assert_eq!(got.len(), 1);
        assert!(matches!(
            got[0],
            NrscEvent::AudioBitRate { program: 0, .. }
        ));
    }

    #[test]
    fn hdc_program_out_of_range_is_ignored() {
        // libnrsc5 caps programs at 8; a stray program index must not
        // panic or emit.
        let (mut ctx, out) = capture();
        for _ in 0..BITRATE_FRAME_WINDOW {
            dispatch_hdc(&mut ctx, 99, 1024, true);
        }
        assert!(out.lock().unwrap().is_empty());
    }

    #[test]
    fn translates_mer() {
        let (mut ctx, out) = capture();
        let mut payload = zeroed_payload();
        payload.mer = sys::nrsc5_event_mer {
            lower: -3.5,
            upper: 4.25,
        };
        dispatch(
            sys::nrsc5_event_t {
                event: sys::NRSC5_EVENT_MER,
                payload,
            },
            &mut ctx,
        );
        let got = out.lock().unwrap();
        match got[0] {
            NrscEvent::Mer { lower, upper } => {
                assert!((lower - -3.5).abs() < 1e-5);
                assert!((upper - 4.25).abs() < 1e-5);
            }
            ref other => panic!("expected Mer, got {other:?}"),
        }
    }

    #[test]
    fn translates_id3_metadata_and_xhdr() {
        let (mut ctx, out) = capture();
        let title = CString::new("Song").unwrap();
        let artist = CString::new("Band").unwrap();
        let album = CString::new("Album").unwrap();
        let genre = CString::new("").unwrap();
        let mut payload = zeroed_payload();
        payload.id3 = sys::nrsc5_event_id3 {
            program: 0,
            title: title.as_ptr(),
            artist: artist.as_ptr(),
            album: album.as_ptr(),
            genre: genre.as_ptr(),
            ufid: sys::nrsc5_event_id3_ufid {
                owner: ptr::null(),
                id: ptr::null(),
            },
            xhdr: sys::nrsc5_event_id3_xhdr {
                mime: sys::NRSC5_MIME_JPEG,
                param: 1,
                lot: 42,
            },
            comments: ptr::null_mut(),
        };
        dispatch(
            sys::nrsc5_event_t {
                event: sys::NRSC5_EVENT_ID3,
                payload,
            },
            &mut ctx,
        );
        let got = out.lock().unwrap();
        assert_eq!(got.len(), 2);
        match &got[0] {
            NrscEvent::Metadata {
                title,
                artist,
                album,
                ..
            } => {
                assert_eq!(title, "Song");
                assert_eq!(artist, "Band");
                assert_eq!(album, "Album");
            }
            other => panic!("expected Metadata, got {other:?}"),
        }
        match &got[1] {
            NrscEvent::Xhdr { mime, param, lot, .. } => {
                assert_eq!(*mime, sys::NRSC5_MIME_JPEG);
                assert_eq!(*param, 1);
                assert_eq!(lot, "42");
            }
            other => panic!("expected Xhdr, got {other:?}"),
        }
    }

    #[test]
    fn lot_event_copies_payload_bytes() {
        let (mut ctx, out) = capture();
        let name = CString::new("123_cover.jpg").unwrap();
        // Use a non-trivial byte pattern so the test catches a wrong
        // length, off-by-one, or missing copy.
        let bytes: Vec<u8> = (0u8..32).collect();
        let mut payload = zeroed_payload();
        payload.lot = sys::nrsc5_event_lot {
            port: 0,
            lot: 123,
            size: bytes.len() as u32,
            mime: sys::NRSC5_MIME_JPEG,
            name: name.as_ptr(),
            data: bytes.as_ptr(),
            expiry_utc: ptr::null_mut(),
            service: ptr::null_mut(),
            component: ptr::null_mut(),
        };
        dispatch(
            sys::nrsc5_event_t {
                event: sys::NRSC5_EVENT_LOT,
                payload,
            },
            &mut ctx,
        );
        let got = out.lock().unwrap();
        assert_eq!(got.len(), 1);
        match &got[0] {
            NrscEvent::LotFile {
                lot,
                name,
                data,
                program,
                mime,
                lot_component_mime,
            } => {
                assert_eq!(lot, "123");
                assert_eq!(name, "123_cover.jpg");
                assert_eq!(*program, 0); // api.rs hardcodes 0; rewritten by mod.rs.
                assert_eq!(data, &bytes);
                assert_eq!(*mime, sys::NRSC5_MIME_JPEG);
                assert_eq!(*lot_component_mime, None);
            }
            other => panic!("expected LotFile, got {other:?}"),
        }
    }

    #[test]
    fn lot_event_reads_component_data_mime() {
        let (mut ctx, out) = capture();
        let name = CString::new("logo.png").unwrap();
        let bytes: Vec<u8> = vec![0x89, b'P', b'N', b'G'];
        let mut component = sys::nrsc5_sig_component_t {
            next: ptr::null_mut(),
            type_: sys::NRSC5_SIG_COMPONENT_DATA,
            id: 7,
            variant: sys::nrsc5_sig_component_variant {
                data: sys::nrsc5_sig_component_data {
                    port: 0,
                    service_data_type: 0,
                    type_: 0,
                    mime: sys::NRSC5_MIME_STATION_LOGO,
                },
            },
        };
        let mut payload = zeroed_payload();
        payload.lot = sys::nrsc5_event_lot {
            port: 0,
            lot: 9,
            size: bytes.len() as u32,
            mime: sys::NRSC5_MIME_PNG,
            name: name.as_ptr(),
            data: bytes.as_ptr(),
            expiry_utc: ptr::null_mut(),
            service: ptr::null_mut(),
            component: &mut component,
        };
        dispatch(
            sys::nrsc5_event_t {
                event: sys::NRSC5_EVENT_LOT,
                payload,
            },
            &mut ctx,
        );
        let got = out.lock().unwrap();
        assert_eq!(got.len(), 1);
        match &got[0] {
            NrscEvent::LotFile {
                mime,
                lot_component_mime,
                ..
            } => {
                assert_eq!(*mime, sys::NRSC5_MIME_PNG);
                assert_eq!(
                    *lot_component_mime,
                    Some(sys::NRSC5_MIME_STATION_LOGO)
                );
            }
            other => panic!("expected LotFile, got {other:?}"),
        }
    }

    #[test]
    fn lot_event_with_null_data_yields_empty_vec() {
        let (mut ctx, out) = capture();
        let name = CString::new("noop.bin").unwrap();
        let mut payload = zeroed_payload();
        payload.lot = sys::nrsc5_event_lot {
            port: 0,
            lot: 7,
            size: 999, // size lies; data is null -> must be ignored.
            mime: 0,
            name: name.as_ptr(),
            data: ptr::null(),
            expiry_utc: ptr::null_mut(),
            service: ptr::null_mut(),
            component: ptr::null_mut(),
        };
        dispatch(
            sys::nrsc5_event_t {
                event: sys::NRSC5_EVENT_LOT,
                payload,
            },
            &mut ctx,
        );
        let got = out.lock().unwrap();
        assert_eq!(got.len(), 1);
        match &got[0] {
            NrscEvent::LotFile { data, .. } => assert!(data.is_empty()),
            other => panic!("expected LotFile, got {other:?}"),
        }
    }

    #[test]
    fn id3_without_xhdr_does_not_emit_xhdr() {
        let (mut ctx, out) = capture();
        let title = CString::new("Title").unwrap();
        let empty = CString::new("").unwrap();
        let mut payload = zeroed_payload();
        payload.id3 = sys::nrsc5_event_id3 {
            program: 1,
            title: title.as_ptr(),
            artist: empty.as_ptr(),
            album: empty.as_ptr(),
            genre: empty.as_ptr(),
            ufid: sys::nrsc5_event_id3_ufid {
                owner: ptr::null(),
                id: ptr::null(),
            },
            // lot < 0 means "no LOT" per upstream convention.
            xhdr: sys::nrsc5_event_id3_xhdr {
                mime: 0,
                param: 0,
                lot: -1,
            },
            comments: ptr::null_mut(),
        };
        dispatch(
            sys::nrsc5_event_t {
                event: sys::NRSC5_EVENT_ID3,
                payload,
            },
            &mut ctx,
        );
        let got = out.lock().unwrap();
        assert_eq!(got.len(), 1);
        assert!(matches!(got[0], NrscEvent::Metadata { .. }));
    }

    #[test]
    fn empty_sis_emits_nothing() {
        let (mut ctx, out) = capture();
        // Everything zeroed → all strings null, lat/lon zero, audio_services null.
        dispatch(
            sys::nrsc5_event_t {
                event: sys::NRSC5_EVENT_SIS,
                payload: zeroed_payload(),
            },
            &mut ctx,
        );
        assert!(out.lock().unwrap().is_empty());
    }

    #[test]
    fn sis_emits_station_and_country() {
        let (mut ctx, out) = capture();
        let name = CString::new("KEXP").unwrap();
        let country = CString::new("US").unwrap();
        let mut payload = zeroed_payload();
        // Set country code + facility id; leave lat/lon/slogan/etc. unset.
        payload.sis = sys::nrsc5_event_sis {
            country_code: country.as_ptr(),
            fcc_facility_id: 12345,
            name: name.as_ptr(),
            slogan: ptr::null(),
            message: ptr::null(),
            alert: ptr::null(),
            latitude: 0.0,
            longitude: 0.0,
            altitude: 0,
            audio_services: ptr::null_mut(),
            data_services: ptr::null_mut(),
            alert_cnt: ptr::null(),
            alert_cnt_length: 0,
            alert_category1: 0,
            alert_category2: 0,
            alert_location_format: 0,
            alert_num_locations: 0,
            alert_locations: ptr::null(),
        };
        dispatch(
            sys::nrsc5_event_t {
                event: sys::NRSC5_EVENT_SIS,
                payload,
            },
            &mut ctx,
        );
        let got = out.lock().unwrap();
        assert_eq!(got.len(), 2);
        match &got[0] {
            NrscEvent::StationName(n) => assert_eq!(n, "KEXP"),
            other => panic!("expected StationName, got {other:?}"),
        }
        match &got[1] {
            NrscEvent::CountryFcc {
                country,
                facility_id,
            } => {
                assert_eq!(country, "US");
                assert_eq!(*facility_id, 12345);
            }
            other => panic!("expected CountryFcc, got {other:?}"),
        }
    }

    #[test]
    fn trampoline_swallows_panics() {
        let cb: EventCallback = Box::new(|_| panic!("intentional — trampoline must absorb this"));
        let mut ctx = CallbackCtx {
            event_cb: Some(cb),
            pcm_sink: None,
            bitrate: BitrateAccum::default(),
        };
        // If this propagates, the test harness will catch the panic and
        // fail. Required behaviour: trampoline absorbs it so libnrsc5's
        // C frames don't see an unwind.
        dispatch(
            sys::nrsc5_event_t {
                event: sys::NRSC5_EVENT_SYNC,
                payload: zeroed_payload(),
            },
            &mut ctx,
        );
    }

    #[test]
    fn pcm_sink_receives_samples() {
        let captured: Arc<Mutex<Vec<(u32, Vec<i16>)>>> = Arc::new(Mutex::new(Vec::new()));
        let cap = captured.clone();
        let sink: PcmSink = Box::new(move |p, s| cap.lock().unwrap().push((p, s.to_vec())));
        let mut ctx = CallbackCtx {
            event_cb: None,
            pcm_sink: Some(sink),
            bitrate: BitrateAccum::default(),
        };
        let samples: [i16; 4] = [1, 2, -3, 4];
        let mut payload = zeroed_payload();
        payload.audio = sys::nrsc5_event_audio {
            program: 2,
            data: samples.as_ptr(),
            count: samples.len(),
        };
        dispatch(
            sys::nrsc5_event_t {
                event: sys::NRSC5_EVENT_AUDIO,
                payload,
            },
            &mut ctx,
        );
        let lock = captured.lock().unwrap();
        assert_eq!(lock.len(), 1);
        assert_eq!(lock[0].0, 2);
        assert_eq!(lock[0].1, vec![1i16, 2, -3, 4]);
    }

    #[test]
    fn audio_event_without_sink_is_dropped_silently() {
        // event_cb installed, pcm_sink NOT. An AUDIO event must not
        // trip the event callback (it isn't a metadata event) and
        // must not panic.
        let (mut ctx, out) = capture();
        let samples: [i16; 2] = [0, 0];
        let mut payload = zeroed_payload();
        payload.audio = sys::nrsc5_event_audio {
            program: 0,
            data: samples.as_ptr(),
            count: samples.len(),
        };
        dispatch(
            sys::nrsc5_event_t {
                event: sys::NRSC5_EVENT_AUDIO,
                payload,
            },
            &mut ctx,
        );
        assert!(out.lock().unwrap().is_empty());
    }

    #[test]
    fn unknown_event_tag_is_ignored() {
        let (mut ctx, out) = capture();
        // 999 is well past the highest defined tag (26).
        dispatch(
            sys::nrsc5_event_t {
                event: 999,
                payload: zeroed_payload(),
            },
            &mut ctx,
        );
        assert!(out.lock().unwrap().is_empty());
    }

    #[test]
    fn program_type_name_known_and_unknown() {
        assert_eq!(program_type_name(sys::NRSC5_PROGRAM_TYPE_ROCK), "Rock");
        assert_eq!(program_type_name(sys::NRSC5_PROGRAM_TYPE_TRAFFIC), "Traffic");
        // Value 27 is in the upstream gap between HIP_HOP (26) and
        // WEATHER (29); should fall through to the stringified form.
        assert_eq!(program_type_name(27), "Type 27");
    }

    #[test]
    fn mode_to_raw_matches_constants() {
        assert_eq!(Mode::Fm.to_raw(), sys::NRSC5_MODE_FM);
        assert_eq!(Mode::Am.to_raw(), sys::NRSC5_MODE_AM);
    }

    // -----------------------------------------------------------------
    // Signal-quality / loss events
    // -----------------------------------------------------------------

    #[test]
    fn translates_ber() {
        let (mut ctx, out) = capture();
        let mut payload = zeroed_payload();
        payload.ber = sys::nrsc5_event_ber { cber: 0.0125 };
        dispatch(
            sys::nrsc5_event_t {
                event: sys::NRSC5_EVENT_BER,
                payload,
            },
            &mut ctx,
        );
        let got = out.lock().unwrap();
        assert_eq!(got.len(), 1);
        match got[0] {
            NrscEvent::Ber { cber } => assert!((cber - 0.0125).abs() < 1e-6),
            ref other => panic!("expected Ber, got {other:?}"),
        }
    }

    #[test]
    fn lost_device_event_signals_loss() {
        // The hard signal-loss path: libnrsc5 reports the SDR vanished
        // (USB unplug / backend error). Must surface as `LostDevice`
        // so the app can tear down and re-arm the Start button.
        let (mut ctx, out) = capture();
        dispatch(
            sys::nrsc5_event_t {
                event: sys::NRSC5_EVENT_LOST_DEVICE,
                payload: zeroed_payload(),
            },
            &mut ctx,
        );
        let got = out.lock().unwrap();
        assert_eq!(got.len(), 1);
        assert!(matches!(got[0], NrscEvent::LostDevice));
    }

    #[test]
    fn lost_sync_event_signals_loss() {
        // The soft signal-loss path: HD lock dropped (fading, antenna
        // bump) but the device is still present. Surfaces as
        // `LostSync` so the UI can clear the constellation/MER readout.
        let (mut ctx, out) = capture();
        dispatch(
            sys::nrsc5_event_t {
                event: sys::NRSC5_EVENT_LOST_SYNC,
                payload: zeroed_payload(),
            },
            &mut ctx,
        );
        let got = out.lock().unwrap();
        assert_eq!(got.len(), 1);
        assert!(matches!(got[0], NrscEvent::LostSync));
    }

    // -----------------------------------------------------------------
    // Empty / corrupt audio buffers (must never deref a bad pointer)
    // -----------------------------------------------------------------

    #[test]
    fn empty_audio_buffer_does_not_invoke_sink() {
        // count == 0 → the trampoline must not synthesize a
        // zero-length slice or call the sink. Guards the `count > 0`
        // half of the audio arm's safety check.
        let calls: Arc<Mutex<usize>> = Arc::new(Mutex::new(0));
        let cnt = calls.clone();
        let sink: PcmSink = Box::new(move |_, _| *cnt.lock().unwrap() += 1);
        let mut ctx = CallbackCtx {
            event_cb: None,
            pcm_sink: Some(sink),
            bitrate: BitrateAccum::default(),
        };
        let sample: [i16; 1] = [0];
        let mut payload = zeroed_payload();
        payload.audio = sys::nrsc5_event_audio {
            program: 0,
            data: sample.as_ptr(),
            count: 0,
        };
        dispatch(
            sys::nrsc5_event_t {
                event: sys::NRSC5_EVENT_AUDIO,
                payload,
            },
            &mut ctx,
        );
        assert_eq!(*calls.lock().unwrap(), 0, "sink must not fire on empty buffer");
    }

    #[test]
    fn null_audio_data_does_not_invoke_sink() {
        // A corrupt event whose `count` lies (> 0) while `data` is
        // null must be rejected by the null guard, not dereferenced.
        let calls: Arc<Mutex<usize>> = Arc::new(Mutex::new(0));
        let cnt = calls.clone();
        let sink: PcmSink = Box::new(move |_, _| *cnt.lock().unwrap() += 1);
        let mut ctx = CallbackCtx {
            event_cb: None,
            pcm_sink: Some(sink),
            bitrate: BitrateAccum::default(),
        };
        let mut payload = zeroed_payload();
        payload.audio = sys::nrsc5_event_audio {
            program: 0,
            data: ptr::null(),
            count: 512,
        };
        dispatch(
            sys::nrsc5_event_t {
                event: sys::NRSC5_EVENT_AUDIO,
                payload,
            },
            &mut ctx,
        );
        assert_eq!(*calls.lock().unwrap(), 0, "sink must not fire on null data");
    }

    // -----------------------------------------------------------------
    // Error-handling surface (libnrsc5 returning non-zero)
    // -----------------------------------------------------------------

    #[test]
    fn api_error_display_propagates_rc() {
        // The fallible wrapper methods map libnrsc5's non-zero return
        // codes to typed errors; the Display text must surface the rc
        // so a failed open / set-frequency / pipe is diagnosable from
        // the log without a debugger.
        assert!(Nrsc5ApiError::OpenFailed(-3).to_string().contains("-3"));
        assert!(Nrsc5ApiError::PipeFailed(7).to_string().contains('7'));
        assert!(Nrsc5ApiError::SetFrequencyFailed(2).to_string().contains('2'));
        assert!(Nrsc5ApiError::SetModeFailed(9).to_string().contains('9'));
        let big = Nrsc5ApiError::PipeChunkTooLarge { len: 9_000_000_000 };
        assert!(big.to_string().contains("9000000000"));
    }

    // -----------------------------------------------------------------
    // Real-time streaming continuity
    // -----------------------------------------------------------------

    #[test]
    fn bitrate_emits_once_per_window_across_multiple_windows() {
        // A long stream must produce a steady cadence of estimates —
        // one per completed 32-frame window — rather than a single
        // value or a runaway accumulation. Confirms the accumulator
        // resets cleanly between windows.
        let (mut ctx, out) = capture();
        for _ in 0..(BITRATE_FRAME_WINDOW * 3) {
            dispatch_hdc(&mut ctx, 0, 1024, true);
        }
        let got = out.lock().unwrap();
        assert_eq!(got.len(), 3, "three full windows → three estimates");
        for ev in got.iter() {
            assert!(matches!(ev, NrscEvent::AudioBitRate { program: 0, .. }));
        }
    }

    #[test]
    fn normal_decode_flow_delivers_metadata_then_audio() {
        // End-to-end happy path through one shared callback context,
        // the way libnrsc5 drives it on its worker thread: lock
        // acquired, song metadata + cover-art pointer, signal quality,
        // a bit-rate window, then decoded PCM on the fast path.
        let events: Arc<Mutex<Vec<NrscEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let pcm: Arc<Mutex<Vec<(u32, Vec<i16>)>>> = Arc::new(Mutex::new(Vec::new()));
        let ev_out = events.clone();
        let pcm_out = pcm.clone();
        let mut ctx = CallbackCtx {
            event_cb: Some(Box::new(move |e| ev_out.lock().unwrap().push(e))),
            pcm_sink: Some(Box::new(move |p, s| pcm_out.lock().unwrap().push((p, s.to_vec())))),
            bitrate: BitrateAccum::default(),
        };

        // 1. Sync (FM): -1 sentinels keep it FM-only (no SyncAm).
        let mut p = zeroed_payload();
        p.sync = sys::nrsc5_event_sync {
            freq_offset: 0.5,
            psmi: 1,
            pli: -1,
            hppi: -1,
            aabi: -1,
            rdbi: -1,
        };
        dispatch(
            sys::nrsc5_event_t {
                event: sys::NRSC5_EVENT_SYNC,
                payload: p,
            },
            &mut ctx,
        );

        // 2. ID3: song metadata + cover-art (xhdr) pointer.
        let title = CString::new("Track").unwrap();
        let artist = CString::new("Artist").unwrap();
        let album = CString::new("LP").unwrap();
        let empty = CString::new("").unwrap();
        let mut p = zeroed_payload();
        p.id3 = sys::nrsc5_event_id3 {
            program: 0,
            title: title.as_ptr(),
            artist: artist.as_ptr(),
            album: album.as_ptr(),
            genre: empty.as_ptr(),
            ufid: sys::nrsc5_event_id3_ufid {
                owner: ptr::null(),
                id: ptr::null(),
            },
            xhdr: sys::nrsc5_event_id3_xhdr {
                mime: sys::NRSC5_MIME_JPEG,
                param: 0,
                lot: 7,
            },
            comments: ptr::null_mut(),
        };
        dispatch(
            sys::nrsc5_event_t {
                event: sys::NRSC5_EVENT_ID3,
                payload: p,
            },
            &mut ctx,
        );

        // 3. MER.
        let mut p = zeroed_payload();
        p.mer = sys::nrsc5_event_mer {
            lower: 12.0,
            upper: 13.0,
        };
        dispatch(
            sys::nrsc5_event_t {
                event: sys::NRSC5_EVENT_MER,
                payload: p,
            },
            &mut ctx,
        );

        // 4. One full HDC window → exactly one AudioBitRate estimate.
        for _ in 0..BITRATE_FRAME_WINDOW {
            dispatch_hdc(&mut ctx, 0, 1024, true);
        }

        // 5. Decoded PCM for program 0 (fast path, not the event channel).
        let samples: [i16; 4] = [5, -5, 6, -6];
        let mut p = zeroed_payload();
        p.audio = sys::nrsc5_event_audio {
            program: 0,
            data: samples.as_ptr(),
            count: samples.len(),
        };
        dispatch(
            sys::nrsc5_event_t {
                event: sys::NRSC5_EVENT_AUDIO,
                payload: p,
            },
            &mut ctx,
        );

        let evs = events.lock().unwrap();
        assert_eq!(
            evs.len(),
            5,
            "expected Sync, Metadata, Xhdr, Mer, AudioBitRate; got {evs:?}"
        );
        assert!(matches!(evs[0], NrscEvent::Sync { .. }));
        assert!(matches!(evs[1], NrscEvent::Metadata { .. }));
        assert!(matches!(evs[2], NrscEvent::Xhdr { .. }));
        assert!(matches!(evs[3], NrscEvent::Mer { .. }));
        assert!(matches!(evs[4], NrscEvent::AudioBitRate { program: 0, .. }));

        let audio = pcm.lock().unwrap();
        assert_eq!(audio.len(), 1);
        assert_eq!(audio[0].0, 0);
        assert_eq!(audio[0].1, vec![5i16, -5, 6, -6]);
    }
}
