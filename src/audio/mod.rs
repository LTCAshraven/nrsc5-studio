//! In-process audio playback. Owns a cpal output stream fed by raw
//! s16le 44.1 kHz stereo PCM from one or more `nrsc5.exe` child
//! processes invoked with `-o -`.
//!
//! Architecture (Phase 1, single-stream specialization):
//! ```text
//! nrsc5.exe (-o -) ─ stdout pipe ─► pcm_pump (FFI layer)
//!                                       │
//!                                       ▼
//!                                AudioSink::push(&[i16])
//!                                       │
//!                       Arc<Mutex<VecDeque<i16>>> (bounded ring)
//!                                       │
//!                                       ▼
//!                                  cpal callback
//!                                       │
//!                                       ▼
//!                            default output device (44.1 kHz)
//! ```
//!
//! Volume and mute are read by the cpal callback on every fill via two
//! atomics; setting them from the UI thread is wait-free. The queue is
//! bounded at `MAX_QUEUE_LEN` samples (~200 ms of stereo audio); on
//! overflow we drop the oldest samples to keep playback latency bounded
//! regardless of how fast `nrsc5.exe` produces PCM.
//!
//! Phase 3 will generalize this to per-decoder sub-queues feeding a
//! routing layer ("active speaker selector") so multiple `nrsc5.exe`
//! instances can decode in parallel with only one of them audible at a
//! time. The single-stream API here is the natural specialization of
//! that design.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

/// Native sample rate of nrsc5's `-o -` output. We request this rate
/// from cpal directly. On Windows (WASAPI) and Linux (PulseAudio /
/// PipeWire dmix) the OS mixer almost always honors 44.1 kHz even when
/// the underlying DAC runs natively at 48 kHz, so this is the right
/// default. Devices that refuse 44.1 kHz outright surface an
/// `init_error` and the app continues silently (Phase 4 will add
/// rubato-based fallback resampling for those edge cases).
pub const NRSC5_SAMPLE_RATE: u32 = 44_100;

/// Hard cap on the playback queue length in **interleaved samples**
/// (not frames). 8820 frames × 2 channels = 17 640 samples ≈ 200 ms at
/// 44.1 kHz stereo. Beyond this, the producer is faster than the
/// device can drain, so the producer trims oldest samples on push to
/// keep latency bounded. In steady state the queue oscillates near a
/// few tens of milliseconds.
const MAX_QUEUE_LEN: usize = 8_820 * 2;

/// Owns the cpal output stream and the shared playback state. Built
/// once at app startup by `Nrsc5App::new`. The `_stream` field keeps
/// the cpal callback alive for the player's lifetime; dropping the
/// player stops audio output.
///
/// `cpal::Stream` is `!Send`/`!Sync` on most backends, so `AudioPlayer`
/// is also not `Send`. It lives on the egui main thread.
pub struct AudioPlayer {
    sink: AudioSink,
    _stream: Option<cpal::Stream>,
    /// `Some(message)` when device open failed; `None` on success.
    /// Surfaced via the status line so the user knows audio is dead.
    // Kept: holds the cpal-open error for the (currently unwired)
    // status-line surfacing; read only via `is_ready()`.
    #[allow(dead_code)]
    pub init_error: Option<String>,
}

/// Clone-cheap handle for pushing PCM frames into the player. Lives on
/// every FFI `pcm_pump` thread that consumes a child's stdout. Cloning
/// is just `Arc::clone` on three pointers — safe to hand out freely.
#[derive(Clone)]
pub struct AudioSink {
    queue: Arc<Mutex<VecDeque<i16>>>,
    /// Bits of an `f32` in [0.0, 1.0]. Read on every cpal fill.
    volume: Arc<AtomicU32>,
    muted: Arc<AtomicBool>,
}

impl Default for AudioPlayer {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioPlayer {
    pub fn new() -> Self {
        let queue: Arc<Mutex<VecDeque<i16>>> =
            Arc::new(Mutex::new(VecDeque::with_capacity(MAX_QUEUE_LEN)));
        let volume = Arc::new(AtomicU32::new(1.0f32.to_bits()));
        let muted = Arc::new(AtomicBool::new(false));

        let sink = AudioSink {
            queue: Arc::clone(&queue),
            volume: Arc::clone(&volume),
            muted: Arc::clone(&muted),
        };

        let (stream, init_error) = open_stream(queue, volume, muted);
        Self {
            sink,
            _stream: stream,
            init_error,
        }
    }

    /// Hand out a clone-cheap handle for FFI pcm_pump threads to push
    /// PCM into.
    pub fn sink(&self) -> AudioSink {
        self.sink.clone()
    }

    /// UI-thread setters. Both are wait-free relaxed atomic stores.
    pub fn set_volume(&self, value: f32) {
        let v = value.clamp(0.0, 1.0);
        self.sink.volume.store(v.to_bits(), Ordering::Relaxed);
    }

    pub fn set_mute(&self, muted: bool) {
        self.sink.muted.store(muted, Ordering::Relaxed);
    }

    /// True if the cpal output stream was opened successfully. Used by
    /// the UI to gate the volume slider; false means `init_error`
    /// carries a human-readable reason.
    // Kept: audio-health accessor for the planned status-line surfacing
    // of `init_error`; no current caller.
    #[allow(dead_code)]
    pub fn is_ready(&self) -> bool {
        self.init_error.is_none()
    }
}

impl AudioSink {
    /// Push a chunk of interleaved s16 LE stereo @ 44.1 kHz into the
    /// playback queue. Trims oldest samples if it would overflow the
    /// queue cap, so a busy decoder can't run latency away.
    ///
    /// Cheap: one Mutex lock + one extend-equivalent. Lock hold time
    /// is microseconds. The cpal callback contends for the same lock
    /// but its hold time is also microseconds, so contention is in
    /// the noise.
    pub fn push(&self, frames: &[i16]) {
        if frames.is_empty() {
            return;
        }
        let Ok(mut q) = self.queue.lock() else { return };
        // Bound latency: trim oldest if we'd overflow.
        let total = q.len() + frames.len();
        if total > MAX_QUEUE_LEN {
            let q_len = q.len();
            let drop_n = (total - MAX_QUEUE_LEN).min(q_len);
            q.drain(..drop_n);
        }
        q.extend(frames.iter().copied());
    }

    /// Drop everything currently queued. Called on stream stop so the
    /// next Start doesn't replay a fraction of a second of old audio.
    pub fn clear(&self) {
        if let Ok(mut q) = self.queue.lock() {
            q.clear();
        }
    }
}

/// Shared gate that arbitrates between the HD speaker path and the
/// analog-FM fallback so only one source feeds the cpal sink at a
/// time.
///
/// The [`SpeakerRouter`] stamps [`mark_hd_audio`](Self::mark_hd_audio)
/// every time it forwards decoded HD PCM to the sink. The analog
/// fallback thread checks [`hd_recent`](Self::hd_recent) before
/// pushing its own demodulated audio and stays silent while HD audio
/// is flowing. The asymmetry is deliberate: HD takes over the instant
/// it produces audio, while analog only resumes after HD has been
/// absent for a full window — so a brief HD dropout doesn't flap the
/// two sources against each other.
///
/// Cloning is `Arc::clone` on one pointer plus a `Copy` of the origin
/// `Instant`; both clones share the same atomic timestamp.
#[derive(Clone)]
pub struct AnalogHandoff {
    /// Milliseconds since `origin` at which HD audio was last pushed.
    /// Zero means "never" (HD has not produced audio this stream).
    last_hd_ms: Arc<AtomicU64>,
    /// Whether the gain stage is trustworthy enough to hand the sink
    /// over to HD. In closed-loop AGC (Auto) mode this is `false`
    /// while the controller is still searching and flips `true` once
    /// it settles (or bails). In Manual / Hardware-AGC modes there is
    /// no search, so it stays `true` for the whole stream and the
    /// handoff keys purely off HD-audio presence. Holding analog
    /// through the AGC search keeps a clean, full-volume fallback
    /// playing instead of letting brief mid-search HD locks flap the
    /// two sources against each other.
    agc_ready: Arc<AtomicBool>,
    /// Whether the HD decoder is currently OFDM-synced. Driven by the
    /// `Sync` / `LostSync` events. HD is only allowed to own the sink
    /// while locked, so a sync loss immediately releases the sink back
    /// to the analog fallback instead of waiting for the PCM ring to
    /// run dry (which can lag, or never happen if the decoder keeps
    /// emitting silence).
    hd_synced: Arc<AtomicBool>,
    /// Whether the current fallback mode is forcing analog-only output.
    /// In this mode the speaker router must never let HD PCM claim the
    /// sink, even when AGC and sync are otherwise healthy.
    analog_only: Arc<AtomicBool>,
    /// Optional minimum MER (tenths of dB) required for HD audio to own
    /// the sink. `i32::MIN` means disabled. Driven from the UI/config.
    hd_min_mer_tenths: Arc<AtomicI32>,
    /// Most recently observed MER (tenths of dB), typically the average of
    /// lower/upper sideband MER from `NrscEvent::Mer`. `i32::MIN` means
    /// unknown/not observed yet this stream.
    hd_mer_tenths: Arc<AtomicI32>,
    /// Latch for MER-threshold hysteresis. `true` means HD already owns the
    /// sink via MER gating and can stay there until MER drops below the lower
    /// release edge.
    hd_mer_gate_open: Arc<AtomicBool>,
    /// Common monotonic origin so both clones compute the same
    /// elapsed time. `Instant` is `Copy`, so each clone carries the
    /// same value.
    origin: std::time::Instant,
}

impl Default for AnalogHandoff {
    fn default() -> Self {
        Self::new()
    }
}

impl AnalogHandoff {
    pub fn new() -> Self {
        Self {
            last_hd_ms: Arc::new(AtomicU64::new(0)),
            // Default ready: Manual / Hardware-AGC streams never call
            // `set_agc_ready(false)`, so they hand over as soon as HD
            // audio appears. Auto mode explicitly arms the gate at
            // stream start.
            agc_ready: Arc::new(AtomicBool::new(true)),
            // Nothing is locked at stream start; the first `Sync`
            // event flips this true.
            hd_synced: Arc::new(AtomicBool::new(false)),
            analog_only: Arc::new(AtomicBool::new(false)),
            hd_min_mer_tenths: Arc::new(AtomicI32::new(i32::MIN)),
            hd_mer_tenths: Arc::new(AtomicI32::new(i32::MIN)),
            hd_mer_gate_open: Arc::new(AtomicBool::new(false)),
            origin: std::time::Instant::now(),
        }
    }

    fn now_ms(&self) -> u64 {
        self.origin.elapsed().as_millis() as u64
    }

    /// Record that HD audio was just forwarded to the sink. Called by
    /// the [`SpeakerRouter`] on every tick that pushes active-program
    /// samples. Wait-free relaxed store.
    pub fn mark_hd_audio(&self) {
        // Saturate 0 to 1 so a push in the first millisecond still
        // reads as "seen" rather than the sentinel "never".
        self.last_hd_ms.store(self.now_ms().max(1), Ordering::Relaxed);
    }

    /// Arm or disarm the AGC-ready gate. Called once at stream start
    /// (`false` for Auto so analog holds through the search, `true`
    /// for Manual / Hardware-AGC) and flipped `true` by the AGC
    /// driver thread when the controller settles or bails.
    pub fn set_agc_ready(&self, ready: bool) {
        self.agc_ready.store(ready, Ordering::Relaxed);
    }

    /// Record the HD decoder's OFDM sync state. Driven by the
    /// `Sync` (true) / `LostSync` (false) events. When sync drops,
    /// HD stops owning the sink so the analog fallback resumes even
    /// if the PCM ring is still draining its last few frames.
    pub fn set_hd_synced(&self, synced: bool) {
        self.hd_synced.store(synced, Ordering::Relaxed);
    }

    /// Force the handoff gate into analog-only mode. When enabled, HD
    /// PCM must never claim the sink, even if AGC is settled and OFDM
    /// sync is healthy.
    pub fn set_analog_only(&self, analog_only: bool) {
        self.analog_only.store(analog_only, Ordering::Relaxed);
    }

    /// Configure the optional minimum MER required for HD to own the sink.
    /// `None` disables the MER gate.
    pub fn set_hd_min_mer_db(&self, min_mer_db: Option<f32>) {
        let tenths = min_mer_db
            .map(|v| (v.clamp(-20.0, 40.0) * 10.0).round() as i32)
            .unwrap_or(i32::MIN);
        self.hd_min_mer_tenths.store(tenths, Ordering::Relaxed);
        if tenths == i32::MIN {
            self.hd_mer_gate_open.store(true, Ordering::Relaxed);
        } else {
            self.hd_mer_gate_open.store(false, Ordering::Relaxed);
        }
    }

    /// Update the latest observed HD MER. `None` marks MER unknown.
    pub fn set_hd_mer_db(&self, mer_db: Option<f32>) {
        let tenths = mer_db
            .map(|v| (v.clamp(-20.0, 40.0) * 10.0).round() as i32)
            .unwrap_or(i32::MIN);
        self.hd_mer_tenths.store(tenths, Ordering::Relaxed);
    }

    fn hd_mer_ok(&self) -> bool {
        let min_tenths = self.hd_min_mer_tenths.load(Ordering::Relaxed);
        if min_tenths == i32::MIN {
            return true;
        }
        let mer_tenths = self.hd_mer_tenths.load(Ordering::Relaxed);
        if mer_tenths == i32::MIN {
            self.hd_mer_gate_open.store(false, Ordering::Relaxed);
            return false;
        }

        // Hysteresis around the configured threshold to prevent source
        // flapping near the edge. Enter-HD requires crossing the high edge;
        // staying on HD only requires staying above the lower release edge.
        const MER_HYSTERESIS_TENTHS: i32 = 15; // 1.5 dB
        let high_edge = min_tenths;
        let low_edge = min_tenths.saturating_sub(MER_HYSTERESIS_TENTHS);
        let was_open = self.hd_mer_gate_open.load(Ordering::Relaxed);
        let is_open = if was_open {
            mer_tenths >= low_edge
        } else {
            mer_tenths >= high_edge
        };
        if is_open != was_open {
            self.hd_mer_gate_open.store(is_open, Ordering::Relaxed);
        }
        is_open
    }

    /// True when the HD speaker path is allowed to feed the sink:
    /// the gain stage is trustworthy (AGC settled / bailed, or no
    /// search running), the decoder is currently locked, and the
    /// current mode is not forcing analog-only output. The
    /// [`SpeakerRouter`] checks this before forwarding HD PCM so a
    /// mid-AGC-search burst or a post-sync-loss dribble can't fight
    /// the analog fallback for the sink.
    pub fn hd_output_allowed(&self) -> bool {
        !self.analog_only.load(Ordering::Relaxed)
            && self.agc_ready.load(Ordering::Relaxed)
            && self.hd_synced.load(Ordering::Relaxed)
            && self.hd_mer_ok()
    }

    /// True if HD audio was forwarded within the last `window_ms`.
    fn hd_recent(&self, window_ms: u64) -> bool {
        let last = self.last_hd_ms.load(Ordering::Relaxed);
        if last == 0 {
            return false;
        }
        self.now_ms().saturating_sub(last) < window_ms
    }

    /// True when the analog fallback should stay silent because HD is
    /// currently the sink owner. The [`SpeakerRouter`] only stamps
    /// [`mark_hd_audio`](Self::mark_hd_audio) when
    /// [`hd_output_allowed`](Self::hd_output_allowed) holds, so a
    /// recent stamp already means HD is both trustworthy and locked;
    /// the window just adds hysteresis so a one-frame audio gap
    /// doesn't flap the two sources.
    pub fn suppress_analog(&self, window_ms: u64) -> bool {
        self.analog_only.load(Ordering::Relaxed) || self.hd_recent(window_ms)
    }

    /// True when HD audio has owned the sink recently enough to be considered
    /// the current audible source.
    pub fn hd_owns_sink_recently(&self, window_ms: u64) -> bool {
        self.hd_recent(window_ms)
    }

    /// Forget any HD-audio history and clear the sync flag. Called on
    /// stream start so a fresh tune begins with analog enabled until
    /// the HD decoder locks.
    pub fn reset(&self) {
        self.last_hd_ms.store(0, Ordering::Relaxed);
        self.hd_synced.store(false, Ordering::Relaxed);
        self.hd_mer_tenths.store(i32::MIN, Ordering::Relaxed);
        self.hd_mer_gate_open.store(false, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::AnalogHandoff;

    #[test]
    fn hd_output_is_blocked_when_analog_only_mode_is_active() {
        let handoff = AnalogHandoff::new();
        handoff.set_agc_ready(true);
        handoff.set_hd_synced(true);
        handoff.set_analog_only(true);
        assert!(!handoff.hd_output_allowed());
    }

    #[test]
    fn hd_output_is_allowed_when_analog_only_mode_is_inactive() {
        let handoff = AnalogHandoff::new();
        handoff.set_agc_ready(true);
        handoff.set_hd_synced(true);
        handoff.set_analog_only(false);
        assert!(handoff.hd_output_allowed());
    }

    #[test]
    fn hd_output_requires_mer_when_threshold_is_enabled() {
        let handoff = AnalogHandoff::new();
        handoff.set_agc_ready(true);
        handoff.set_hd_synced(true);
        handoff.set_analog_only(false);
        handoff.set_hd_min_mer_db(Some(8.0));
        handoff.set_hd_mer_db(Some(7.5));
        assert!(!handoff.hd_output_allowed());
        handoff.set_hd_mer_db(Some(8.1));
        assert!(handoff.hd_output_allowed());
    }
}

/// Build the cpal output stream against the default output device.
///
/// **Rate negotiation.** We don't force 44.1 kHz any more — that
/// silently failed on most Windows WASAPI devices, which only expose
/// the OS-negotiated shared-mode format (almost always 48 kHz f32).
/// Instead we ask cpal for the device's `default_output_config()` and
/// open the stream at the device's native rate, then linearly
/// interpolate from our 44.1 kHz source queue to the device rate in
/// the callback. Linear interp is bit-exact for the matched-rate
/// case and adds negligible latency (one source frame ≈ 23 µs);
/// quality is fine for HD Radio AAC-HE that's already band-limited
/// at the encoder.
///
/// **Channel handling.** Source is interleaved s16 L,R. If the device
/// is stereo we copy directly; if it's mono we mix (L+R)/2; if it's
/// multichannel we put L on channel 0, R on channel 1, and silence
/// the rest.
///
/// **Sample format.** Only F32 output is supported (every desktop OS
/// in this decade defaults to it). Devices that advertise a different
/// default surface as `init_error` so the user gets a clear message
/// instead of mysterious silence.
fn open_stream(
    queue: Arc<Mutex<VecDeque<i16>>>,
    volume: Arc<AtomicU32>,
    muted: Arc<AtomicBool>,
) -> (Option<cpal::Stream>, Option<String>) {
    let host = cpal::default_host();
    let Some(device) = host.default_output_device() else {
        let msg = "no default audio output device".to_string();
        eprintln!("[audio] init failed: {msg}");
        return (None, Some(msg));
    };
    let device_name = device.name().unwrap_or_else(|_| "<unknown>".into());

    // Query the device's OS-negotiated default config. This is the
    // format the device will accept without negotiation overhead.
    let supported = match device.default_output_config() {
        Ok(s) => s,
        Err(e) => {
            let msg = format!(
                "audio device '{device_name}' default_output_config failed: {e}"
            );
            eprintln!("[audio] init failed: {msg}");
            return (None, Some(msg));
        }
    };
    let sample_format = supported.sample_format();
    if sample_format != cpal::SampleFormat::F32 {
        let msg = format!(
            "audio device '{device_name}' default format {sample_format:?} \
             not yet supported (need F32); add the format case in audio::open_stream"
        );
        eprintln!("[audio] init failed: {msg}");
        return (None, Some(msg));
    }
    let dev_rate = supported.sample_rate().0;
    let dev_channels = supported.channels() as usize;
    let stream_config: cpal::StreamConfig = supported.into();

    let err_fn = move |err| eprintln!("[audio] stream error: {err}");

    // Linear-interp resampler state. `frac` is the fractional position
    // within the "current" source frame in [0, step]. Initialized to
    // `step` (which is >= step), so the first output frame triggers a
    // source-frame pull, populating `cur_*` before the first interp.
    let step = NRSC5_SAMPLE_RATE as f32 / dev_rate as f32;
    let mut prev_l: f32 = 0.0;
    let mut prev_r: f32 = 0.0;
    let mut cur_l: f32 = 0.0;
    let mut cur_r: f32 = 0.0;
    let mut frac: f32 = 1.0;
    let inv32k: f32 = 1.0 / 32768.0;

    let stream_res = device.build_output_stream(
        &stream_config,
        move |out: &mut [f32], _info: &cpal::OutputCallbackInfo| {
            let muted_now = muted.load(Ordering::Relaxed);
            let vol = f32::from_bits(volume.load(Ordering::Relaxed));
            let scale = if muted_now { 0.0 } else { vol };
            let frames = out.len() / dev_channels;

            let Ok(mut q) = queue.lock() else {
                for slot in out.iter_mut() {
                    *slot = 0.0;
                }
                return;
            };

            for f in 0..frames {
                // Pull a new source frame whenever the fractional
                // position has advanced past 1.0. The `while` loop
                // (not `if`) handles dev_rate << src_rate where one
                // output frame consumes multiple source frames.
                while frac >= 1.0 {
                    if q.len() < 2 {
                        // Under-run: silence the rest and bail.
                        for slot in out[f * dev_channels..].iter_mut() {
                            *slot = 0.0;
                        }
                        return;
                    }
                    prev_l = cur_l;
                    prev_r = cur_r;
                    cur_l = q.pop_front().unwrap_or(0) as f32 * inv32k;
                    cur_r = q.pop_front().unwrap_or(0) as f32 * inv32k;
                    frac -= 1.0;
                }
                let l = prev_l + (cur_l - prev_l) * frac;
                let r = prev_r + (cur_r - prev_r) * frac;
                // Write the interpolated stereo frame into the device's
                // channel layout.
                let base = f * dev_channels;
                match dev_channels {
                    1 => {
                        out[base] = ((l + r) * 0.5) * scale;
                    }
                    _ => {
                        out[base] = l * scale;
                        out[base + 1] = r * scale;
                        for slot in out[base + 2..base + dev_channels].iter_mut() {
                            *slot = 0.0;
                        }
                    }
                }
                frac += step;
            }
        },
        err_fn,
        None,
    );

    let stream = match stream_res {
        Ok(s) => s,
        Err(e) => {
            let msg = format!(
                "audio device '{device_name}' build_output_stream failed at \
                 {dev_channels}ch {dev_rate} Hz f32: {e}"
            );
            eprintln!("[audio] init failed: {msg}");
            return (None, Some(msg));
        }
    };

    if let Err(e) = stream.play() {
        let msg = format!("audio stream.play failed: {e}");
        eprintln!("[audio] init failed: {msg}");
        return (None, Some(msg));
    }

    eprintln!(
        "[audio] opened '{device_name}' @ {dev_channels}ch {dev_rate} Hz f32 \
         (src 44.1 kHz, resample ratio {:.4}, ~{} ms latency cap)",
        step,
        (MAX_QUEUE_LEN / 2 * 1000) / NRSC5_SAMPLE_RATE as usize
    );

    (Some(stream), None)
}

// ---------------------------------------------------------------------------
// Phase 3 multi-decoder routing layer
// ---------------------------------------------------------------------------
//
// `PcmRing` is one decoder's private bounded ring of interleaved s16
// stereo PCM. Each `DecoderInstance`'s pcm_pump pushes into its own
// ring instead of into the cpal `AudioSink` directly. A single
// `SpeakerRouter` thread, owned by `Nrsc5Process`, drains every
// registered ring on a short polling loop and forwards the *active*
// program's samples into the cpal `AudioSink`. Inactive programs'
// samples are drained-and-discarded — this is what keeps an inactive
// decoder's ring from growing without bound while still letting it
// run in the background (Phase 4 will pull from these same rings for
// per-program Opus recording).
//
// Chunk 2 wires this routing in for the single-decoder case so the
// new plumbing can be smoke-tested in isolation; Chunk 3 turns
// `Nrsc5Process` into a multiplexer and adds the public
// `add_decoder` / `remove_decoder` / `set_active_speaker` API.

use crossbeam_channel::{unbounded, RecvTimeoutError, Sender, TryRecvError};
use std::collections::HashMap;
use std::thread::JoinHandle;
use std::time::Duration;

/// Per-decoder bounded ring of interleaved s16 stereo PCM at 44.1 kHz.
///
/// Identical drop-oldest semantics to `AudioSink`'s internal queue —
/// when the producer (`pcm_pump`) outruns the consumer (`SpeakerRouter`)
/// the oldest samples are evicted to keep latency bounded.
pub(crate) struct PcmRing {
    queue: Mutex<VecDeque<i16>>,
}

impl PcmRing {
    pub fn new() -> Self {
        Self {
            queue: Mutex::new(VecDeque::with_capacity(MAX_QUEUE_LEN)),
        }
    }

    /// Drop-oldest push. Matches `AudioSink::push` byte-for-byte so
    /// the routing layer is a transparent insertion in the audio path.
    pub fn push(&self, frames: &[i16]) {
        if frames.is_empty() {
            return;
        }
        let Ok(mut q) = self.queue.lock() else { return };
        let total = q.len() + frames.len();
        if total > MAX_QUEUE_LEN {
            let q_len = q.len();
            let drop_n = (total - MAX_QUEUE_LEN).min(q_len);
            q.drain(..drop_n);
        }
        q.extend(frames.iter().copied());
    }

    /// Drain the entire ring into `dst`. `dst` is cleared first.
    /// Called by the `SpeakerRouter` on every poll tick.
    fn drain_into(&self, dst: &mut Vec<i16>) {
        dst.clear();
        let Ok(mut q) = self.queue.lock() else { return };
        dst.reserve(q.len());
        dst.extend(q.drain(..));
    }
}

/// Commands sent from the FFI layer to the `SpeakerRouter` worker
/// thread. The router maintains its own `HashMap<program, ring>` so
/// adding/removing decoders is wait-free from the caller's side.
pub(crate) enum SpeakerCmd {
    /// Register a new decoder's PCM ring. The router will start
    /// draining it on the next tick; whether its samples reach the
    /// speakers depends on `SetActive`.
    AddDecoder { program: u32, ring: Arc<PcmRing> },
    /// Stop draining and forget the ring for `program`. Idempotent;
    /// if `program` was the active speaker the router goes silent
    /// until the next `SetActive`.
    RemoveDecoder(u32),
    /// Route this program's samples to the cpal sink. If no decoder
    /// is registered for `program` yet the router remembers the
    /// request and applies it once `AddDecoder(program)` arrives.
    SetActive(u32),
    /// Attach an Opus recorder tap to a decoder's stream. The router
    /// will `try_send` a clone of every drained chunk for `program`
    /// down `tap` after the normal speaker-routing path runs. The
    /// tap is independent of the active speaker so the recorder can
    /// follow a different subchannel than the cpal sink — e.g.
    /// recording HD1 music while listening to HD2 talk. `try_send`
    /// means encoder back-pressure drops the oldest tick rather than
    /// stalling the audio path. Sending `AttachRecorder` for a
    /// program that's already tapped replaces the previous tap
    /// (last-writer-wins); the old `Sender` gets dropped and the
    /// previous recorder thread exits naturally.
    AttachRecorder { program: u32, tap: Sender<Vec<i16>> },
    /// Remove the Opus recorder tap for `program`. Idempotent;
    /// safe to call even if no tap is attached.
    DetachRecorder(u32),
    /// Tear down. Sent on `Nrsc5Process::Drop`.
    Stop,
}

/// Handle to the speaker-routing worker thread. Cheap to clone the
/// command sender; the join handle stays on the `Nrsc5Process` for
/// orderly teardown.
pub(crate) struct SpeakerRouter {
    cmd_tx: Sender<SpeakerCmd>,
    join: Option<JoinHandle<()>>,
}

impl SpeakerRouter {
    /// Spawn the router thread. It will drain registered rings and
    /// forward the active one's samples into `sink`. Long-lived;
    /// survives multiple Start/Stop cycles. Killed via `shutdown()`
    /// or implicitly on drop.
    ///
    /// `handoff` is stamped every time active-program HD audio is
    /// forwarded to the sink, so the analog-FM fallback thread can
    /// stay silent while HD is live.
    pub fn spawn(sink: AudioSink, handoff: AnalogHandoff) -> Self {
        let (cmd_tx, cmd_rx) = unbounded();
        let join = std::thread::spawn(move || run_speaker_loop(sink, handoff, cmd_rx));
        Self {
            cmd_tx,
            join: Some(join),
        }
    }

    /// Clone of the command sender, safe to hand to other threads.
    /// Currently the FFI layer is the only sender.
    pub fn cmd_tx(&self) -> Sender<SpeakerCmd> {
        self.cmd_tx.clone()
    }

    /// Send `Stop` and join the worker. Called from `Drop`.
    pub fn shutdown(&mut self) {
        let _ = self.cmd_tx.send(SpeakerCmd::Stop);
        if let Some(h) = self.join.take() {
            let _ = h.join();
        }
    }
}

impl Drop for SpeakerRouter {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Drain interval when at least one ring is registered. Short enough
/// to keep latency low (one cpal callback is ~10 ms at typical
/// buffer sizes) but long enough that the wake-up rate stays cheap.
const ROUTER_TICK_MS: u64 = 5;

fn run_speaker_loop(
    sink: AudioSink,
    handoff: AnalogHandoff,
    cmd_rx: crossbeam_channel::Receiver<SpeakerCmd>,
) {
    let mut rings: HashMap<u32, Arc<PcmRing>> = HashMap::new();
    let mut active: Option<u32> = None;
    // Phase 4 (Chunk 4.2): per-program Opus recorder tap. Sits next
    // to `rings` so the per-tick drain loop can fan-out one ring's
    // samples to *both* the cpal sink (if `active`) and the
    // recorder tap (if attached) in one pass. Independent from
    // `active` so the recorder can follow a different subchannel
    // than the speakers — the entire reason this lives in the
    // router instead of being a property of the cpal sink.
    let mut recorders: HashMap<u32, Sender<Vec<i16>>> = HashMap::new();
    // Reusable scratch buffer to avoid per-tick allocation. Sized for
    // the worst-case drain of one ring at MAX_QUEUE_LEN.
    let mut scratch: Vec<i16> = Vec::with_capacity(MAX_QUEUE_LEN);

    loop {
        // 1. Drain pending commands non-blocking.
        loop {
            match cmd_rx.try_recv() {
                Ok(SpeakerCmd::AddDecoder { program, ring }) => {
                    rings.insert(program, ring);
                }
                Ok(SpeakerCmd::RemoveDecoder(p)) => {
                    rings.remove(&p);
                    // A program with no ring can't be recorded; drop
                    // any stale recorder tap so the encoder thread
                    // exits cleanly (its forwarder sees a closed
                    // channel and forwards a Stop).
                    recorders.remove(&p);
                    if active == Some(p) {
                        active = None;
                    }
                }
                Ok(SpeakerCmd::SetActive(p)) => {
                    active = Some(p);
                }
                Ok(SpeakerCmd::AttachRecorder { program, tap }) => {
                    // Last-writer-wins: inserting a new tap drops
                    // any previous Sender for the same program, and
                    // the old recorder thread's forwarder sees the
                    // dropped channel → sends Stop → file flushes.
                    recorders.insert(program, tap);
                }
                Ok(SpeakerCmd::DetachRecorder(p)) => {
                    recorders.remove(&p);
                }
                Ok(SpeakerCmd::Stop) => return,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }

        // 2. If no rings registered, block on the command channel
        // until there's work to do — saves the periodic wake-up when
        // no stream is running.
        if rings.is_empty() {
            match cmd_rx.recv_timeout(Duration::from_secs(60)) {
                Ok(SpeakerCmd::AddDecoder { program, ring }) => {
                    rings.insert(program, ring);
                }
                Ok(SpeakerCmd::RemoveDecoder(p)) => {
                    rings.remove(&p);
                    recorders.remove(&p);
                    if active == Some(p) {
                        active = None;
                    }
                }
                Ok(SpeakerCmd::SetActive(p)) => {
                    active = Some(p);
                }
                Ok(SpeakerCmd::AttachRecorder { program, tap }) => {
                    recorders.insert(program, tap);
                }
                Ok(SpeakerCmd::DetachRecorder(p)) => {
                    recorders.remove(&p);
                }
                Ok(SpeakerCmd::Stop) => return,
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
            continue;
        }

        // 3. Drain every ring; forward only the active program's
        // samples to the cpal sink, and (independently) clone
        // anything we just drained into the recorder tap if one is
        // attached for that program.
        for (prog, ring) in rings.iter() {
            ring.drain_into(&mut scratch);
            if scratch.is_empty() {
                continue;
            }
            if active == Some(*prog) && handoff.hd_output_allowed() {
                sink.push(&scratch);
                // Tell the analog fallback HD audio is live so it
                // stays silent and the two sources don't flap. Only
                // stamped while HD is allowed to own the sink (AGC
                // trustworthy AND decoder locked); otherwise the HD
                // PCM is dropped here so the analog fallback keeps
                // the sink to itself through the AGC search and
                // after a sync loss.
                handoff.mark_hd_audio();
            }
            if let Some(tap) = recorders.get(prog) {
                // Clone the scratch into a fresh Vec for the
                // recorder thread. `try_send` so a backed-up
                // encoder drops the tick instead of stalling the
                // speaker path; if the channel is also full or
                // disconnected we just drop the tick — either
                // way the audio path stays unblocked.
                if tap.try_send(scratch.clone()).is_err() {
                    // No-op: full channel = lost tick (typ. ~5 ms),
                    // disconnected = recorder thread already exited
                    // and a future DetachRecorder will clean up.
                }
            }
        }

        std::thread::sleep(Duration::from_millis(ROUTER_TICK_MS));
    }
}
