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
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
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
