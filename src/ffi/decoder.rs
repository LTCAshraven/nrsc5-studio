//! One running libnrsc5 session plus the I/Q feeder thread that
//! drives it.
//!
//! v0.5.1 — single-session-per-tune redesign. libnrsc5 already
//! decodes every advertised HD subchannel from one I/Q stream and
//! tags each `NRSC5_EVENT_AUDIO` / `NRSC5_EVENT_ID3` callback with
//! its `program` field (0..=7). The pre-0.5.1 architecture spawned
//! one [`Nrsc5Session`] per program and filtered the PCM callback
//! down to the assigned program, paying for N parallel COFDM +
//! Viterbi + AAC pipelines to keep only one each. This module now
//! holds the **one** session per tune; the PCM callback demuxes
//! at the source into per-program rings.
//!
//! Lifecycle:
//!
//! * subscribes to the shared [`IqBus`],
//! * blocks on `recv` or on the shutdown channel via
//!   `crossbeam::select!`,
//! * pushes each payload into the owned session with
//!   `Nrsc5Session::pipe_samples_cu8`,
//! * drops the session on exit so its `Drop` impl can run
//!   `nrsc5_stop` → `nrsc5_close` and join libnrsc5's worker thread.
//!
//! Metadata events and decoded PCM are delivered through the two
//! callbacks installed on the session before `start`; those run on
//! libnrsc5's worker thread, not on the feeder thread.
//!
//! [`IqBus`]: crate::sdr::IqBus
//! [`Nrsc5Session`]: super::api::Nrsc5Session

use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::thread::JoinHandle;

use crate::audio::PcmRing;

/// Number of HD subchannels libnrsc5 can emit on one station
/// (`NRSC5_MAX_SUPPLEMENTAL_PROGRAMS + 1`). Sized as a const so the
/// per-program ring + flag arrays below can be `[T; N]` instead of
/// `Vec<T>` — no allocator hits in the PCM callback, O(1) indexed
/// access in the dispatch path.
pub(crate) const MAX_PROGRAMS: usize = 8;

/// One libnrsc5 session plus the feeder thread that pumps I/Q into
/// it. Owns one ring + one "audio observed" flag per subchannel; the
/// PCM callback demuxes by `program` into the right slot.
///
/// Lifetime: created by [`super::Nrsc5Process::start_piped`] once
/// the SDR is open and the session has been configured + started.
/// Consumed by [`super::Nrsc5Process::stop`]: dropping `shutdown_tx`
/// wakes the feeder thread's `select!`, the feeder drops the
/// session (which runs `nrsc5_stop` + `nrsc5_close` to join
/// libnrsc5's worker thread), and `feeder_thread.join()` returns.
pub(crate) struct ActiveSession {
    /// I/Q feeder thread. Owns the [`Nrsc5Session`]; pumps the bus
    /// receiver into `pipe_samples_cu8` until either the bus
    /// disconnects (global stop) or `shutdown_tx` is dropped.
    /// Drops the session at exit, which calls `nrsc5_stop` +
    /// `nrsc5_close` to join libnrsc5's worker thread.
    ///
    /// [`Nrsc5Session`]: super::api::Nrsc5Session
    pub feeder_thread: JoinHandle<()>,
    /// Sender side of the shutdown channel. Drop this in `stop()`
    /// to wake the feeder thread's `select!` without tearing down
    /// the shared IqBus (which `stop()` also tears down through
    /// the I/Q pump on its own).
    pub shutdown_tx: crossbeam_channel::Sender<()>,
    /// Per-subchannel PCM rings, indexed 0..=7. The session's PCM
    /// sink callback pushes decoded samples for program `p` into
    /// `rings[p]`; the shared `SpeakerRouter` thread drains every
    /// ring and routes the currently-selected program's samples to
    /// the cpal sink (the rest are drained-and-discarded to keep
    /// latency bounded). `None` only in headless tests with no
    /// audio sink installed.
    pub rings: Option<[Arc<PcmRing>; MAX_PROGRAMS]>,
    /// Per-subchannel "audio has been observed at least once" flags.
    /// Set inside the PCM callback on the first sample chunk per
    /// program; read by `is_decoding` / `decoded_programs` to drive
    /// the GUI's per-subchannel "lit" indicators. The flag stays
    /// set for the lifetime of the session (a program that briefly
    /// stops broadcasting doesn't go dark in the UI) — full reset
    /// happens on the next `stop()` → `start_piped()` cycle.
    pub audio_started: [Arc<AtomicBool>; MAX_PROGRAMS],
}
