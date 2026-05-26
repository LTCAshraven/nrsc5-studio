//! One running `nrsc5.exe` child plus its supporting plumbing.
//!
//! Phase 3 of the 0.4.0 audio-path refactor introduces this type so
//! that `Nrsc5Process` can eventually hold a **vector** of instances
//! (one per HD program being decoded simultaneously). For Chunk 1 of
//! Phase 3 we keep `Nrsc5Process` single-decoder — it owns exactly
//! one `Option<DecoderInstance>` — but the per-program state is now
//! encapsulated in a struct instead of being spread across four
//! independent `Option<…>` fields on the process. This is the safe
//! refactor checkpoint before the multi-decoder API lands.
//!
//! # What's in here
//!
//! * The decoder's `Child` (nrsc5.exe).
//! * The threads that service its stdio:
//!   - `stderr_thread` — parses status events; always present.
//!   - `stdin_thread` — subscribes to the shared [`IqBus`] and writes
//!     raw I/Q to the child's stdin. Only populated on the piped path
//!     (Chunk 0's `start_piped`); `None` for the legacy USB and
//!     rtl_tcp paths where nrsc5 reads I/Q directly from a dongle or
//!     a network socket.
//!   - `pcm_thread` — reads decoded PCM from the child's stdout and
//!     feeds the audio sink. `None` when no audio sink is installed
//!     (headless tests) or for the legacy USB / rtl_tcp paths where
//!     nrsc5 drives libao itself.
//!
//! # What's *not* in here
//!
//! Anything that's shared across decoders (the SDR pump, the IqBus,
//! the AGC controller, the spectrum tap, the audio sink) stays on
//! `Nrsc5Process`. Per-program PCM ring buffers and the speaker-
//! routing thread arrive in Phase 3 Chunk 2.
//!
//! [`IqBus`]: crate::sdr::IqBus

use std::process::Child;
use std::sync::Arc;
use std::thread::JoinHandle;

use crate::audio::PcmRing;

/// One nrsc5.exe child plus the threads that service its stdio.
///
/// Lifetime: created during one of the `Nrsc5Process::start*` paths
/// after the child has been spawned and its three pumps have been
/// wired up. Consumed during `Nrsc5Process::stop` (joins all threads,
/// kills the child).
pub(crate) struct DecoderInstance {
    /// HD program number (0-based) this child is decoding. nrsc5
    /// reports `program=N` in stderr events for the program it was
    /// invoked with; we keep our own copy here so callers don't have
    /// to fish it out of the event stream.
    pub program: u32,
    /// The running nrsc5.exe child. Owns stdio handles; the pump
    /// threads below own clones of `Child::stdin` / `Child::stdout`
    /// /  `Child::stderr` taken before this struct was constructed.
    pub child: Child,
    /// stderr parser thread. Always present — every spawned decoder
    /// has a stderr pump because that's how we surface SIS / song
    /// metadata / sync / MER events.
    pub stderr_thread: JoinHandle<()>,
    /// I/Q stdin pump thread. `Some` on the piped path; `None` on
    /// USB / rtl_tcp where nrsc5 reads its own I/Q.
    pub stdin_thread: Option<JoinHandle<()>>,
    /// PCM stdout pump thread. `Some` when an audio sink is installed
    /// AND we asked nrsc5 for `-o -` (piped path). `None` otherwise.
    pub pcm_thread: Option<JoinHandle<()>>,
    /// Per-decoder PCM ring. The `pcm_thread` above pushes decoded
    /// samples here instead of into the global `AudioSink`; the
    /// shared `SpeakerRouter` thread drains this ring and routes its
    /// samples to the speakers iff this decoder is the active
    /// speaker. `Some` on the piped path with an installed audio
    /// sink; `None` for the legacy USB / rtl_tcp paths (where nrsc5
    /// drives libao itself) and for headless tests with no sink.
    pub pcm_ring: Option<Arc<PcmRing>>,
}
