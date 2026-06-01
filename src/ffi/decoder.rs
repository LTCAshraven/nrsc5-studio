//! One running libnrsc5 session plus the I/Q feeder thread that
//! drives it.
//!
//! Phase 3 of the libnrsc5 migration: each `DecoderInstance` now owns
//! a [`Nrsc5Session`] (the safe wrapper around `libnrsc5.dll`'s in-
//! process API) instead of an `nrsc5.exe` child process. The four
//! external-process threads (stdin pump, stderr parser, PCM pump,
//! plus the child handle) collapse into one in-process `feeder_thread`
//! which:
//!
//! * subscribes to the shared [`IqBus`],
//! * blocks on `recv` or on a per-decoder shutdown channel via
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

use std::sync::Arc;
use std::thread::JoinHandle;

use crate::audio::PcmRing;

/// One libnrsc5 session plus the feeder thread that pumps I/Q into
/// it.
///
/// Lifetime: created during one of the `Nrsc5Process::start*` paths
/// (or `add_decoder`) once the session has been opened, callbacks
/// installed, and the feeder thread spawned. Consumed during
/// `remove_decoder` / `stop` — dropping `shutdown_tx` wakes the
/// feeder thread's `select!`, the feeder drops the session (which
/// triggers `nrsc5_stop` + `nrsc5_close`), and `feeder_thread.join()`
/// returns.
pub(crate) struct DecoderInstance {
    /// HD program number (0-based) this session is decoding. We keep
    /// our own copy here so callers don't have to fish it out of the
    /// event stream or the session.
    pub program: u32,
    /// I/Q feeder thread. Owns the [`Nrsc5Session`]; pumps the bus
    /// receiver into `pipe_samples_cu8` until either the bus
    /// disconnects (global stop) or `shutdown_tx` is dropped
    /// (per-decoder remove). Drops the session at exit, which calls
    /// `nrsc5_stop` + `nrsc5_close` to join libnrsc5's worker thread.
    ///
    /// [`Nrsc5Session`]: super::api::Nrsc5Session
    pub feeder_thread: JoinHandle<()>,
    /// Sender side of the per-decoder shutdown channel. Drop this to
    /// ask `feeder_thread` to exit without tearing down the whole
    /// shared IqBus — used by `remove_decoder`. `stop()` also drops
    /// it (along with shutting the bus down) on the way through.
    pub shutdown_tx: crossbeam_channel::Sender<()>,
    /// Per-decoder PCM ring. The session's PCM sink callback (set up
    /// in `Nrsc5Process::spawn_decoder`) pushes decoded samples here;
    /// the shared `SpeakerRouter` thread drains the ring and routes
    /// its samples to the speakers iff this decoder is the active
    /// speaker. `Some` when an audio sink is installed on
    /// `Nrsc5Process`; `None` for headless tests with no sink.
    pub pcm_ring: Option<Arc<PcmRing>>,
}
