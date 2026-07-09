//! Single-source, fan-out I/Q byte bus.
//!
//! Phase 2 of the 0.4.0 audio-path refactor introduces this module to
//! decouple the **SDR pump** (one producer of raw I/Q bytes) from
//! **nrsc5's stdin pump** (one consumer today; multiple after Phase 3
//! lands multi-program decode). Externally observable behaviour is
//! unchanged in 0.3.x → 0.4.0-Phase 2 — the bus just sits between two
//! threads that already talked, so we have somewhere to attach the
//! per-program decoders Phase 3 will spawn.
//!
//! # Model
//!
//! * **One producer.** The SDR backend's `run_stream` callback hands
//!   the same byte slice to [`IqBus::publish`] on every USB transfer.
//! * **Zero-N consumers.** Each subscriber registers via
//!   [`IqBus::subscribe`] and gets a private bounded
//!   [`crossbeam_channel::Receiver`]. Payloads are `Arc<[u8]>` so a
//!   single allocation per publish is shared across all subscribers
//!   on the hot path — no per-consumer copy.
//! * **Back-pressure-free producer.** [`publish`](IqBus::publish) does
//!   a non-blocking `try_send` on each subscriber and drops the
//!   payload on [`Full`](crossbeam_channel::TrySendError::Full). A
//!   slow consumer will see HD re-sync as nrsc5 misses frames — *not*
//!   the SDR thread blocking on USB and stalling the spectrum tap.
//! * **Lazy pruning.** Subscribers that report
//!   [`Disconnected`](crossbeam_channel::TrySendError::Disconnected)
//!   (their `Receiver` was dropped) are removed from the list on the
//!   next [`publish`](IqBus::publish), so churn-heavy Phase 3
//!   "enable / disable HDn" toggles don't accumulate dead senders.
//! * **Bulk shutdown.** [`shutdown`](IqBus::shutdown) drops every
//!   `Sender` at once. Each subscriber thread's next `recv` returns
//!   [`RecvError`](crossbeam_channel::RecvError) and the thread can
//!   exit cleanly. Called by the SDR pump after `run_stream` returns,
//!   so a user-`Stop` or a backend failure tears down every consumer
//!   without per-thread cancellation plumbing.

use crossbeam_channel::{bounded, Receiver, RecvTimeoutError, Sender, TryRecvError, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Single-source, fan-out byte bus over `Arc<[u8]>` payloads.
///
/// Wrap in `Arc<IqBus>` so the SDR pump thread and every subscriber
/// thread can hold their own clone. The bus itself is interior-mutable
/// (a `Mutex<Vec<Sender>>`) so this is the only sharing primitive
/// callers need.
#[derive(Default)]
pub struct IqBus {
    /// One `Sender` per live subscriber. Locked once per `publish`
    /// (~750 Hz at 1.488 Msps × 2 B / 4 KB chunks — trivial), and
    /// once per `subscribe` / `shutdown`. Poisoning is treated as a
    /// fatal bug; we `expect` rather than try to recover, since a
    /// poisoned bus mid-stream means a panic on either the SDR or
    /// stdin pump and recovery would re-enter undefined behavior.
    subs: Mutex<Vec<Sender<Arc<[u8]>>>>,
}

impl IqBus {
    /// Construct an empty bus. Subscribers are added via
    /// [`subscribe`](Self::subscribe); the SDR pump publishes via
    /// [`publish`](Self::publish).
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a new subscriber. The returned [`Receiver`] is the
    /// caller's; when it's dropped, the next
    /// [`publish`](Self::publish) prunes the matching `Sender`.
    ///
    /// `capacity` is the bounded queue depth for this subscriber, in
    /// payload units. At 1.488 Msps CS16 (≈3 MB/s) in ~4 KB chunks, a
    /// capacity of 64 payloads is ≈100 ms of buffer — enough to
    /// absorb GC / cache / scheduler hiccups on the consumer without
    /// back-pressuring the SDR.
    pub fn subscribe(&self, capacity: usize) -> Receiver<Arc<[u8]>> {
        let (tx, rx) = bounded(capacity);
        self.subs
            .lock()
            .expect("iq_bus subs mutex poisoned")
            .push(tx);
        rx
    }

    /// Publish one payload to every current subscriber. Non-blocking.
    ///
    /// * Payload is dropped on `Full` (subscriber is slow → its HD
    ///   decode will re-sync rather than us blocking the SDR pump).
    /// * Subscribers reporting `Disconnected` are pruned from the
    ///   list and won't receive future payloads.
    pub fn publish(&self, bytes: &[u8]) {
        let payload: Arc<[u8]> = Arc::from(bytes);
        let mut subs = self.subs.lock().expect("iq_bus subs mutex poisoned");
        subs.retain(|tx| match tx.try_send(payload.clone()) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => true,
            Err(TrySendError::Disconnected(_)) => false,
        });
    }

    /// Drop every subscriber's `Sender`. Each subscriber thread's
    /// next `recv` will return
    /// [`RecvError`](crossbeam_channel::RecvError) and the thread can
    /// exit cleanly without needing a per-thread stop flag. Idempotent.
    pub fn shutdown(&self) {
        self.subs
            .lock()
            .expect("iq_bus subs mutex poisoned")
            .clear();
    }

    /// Current subscriber count. One-shot mutex lock; intended for
    /// diagnostics and Phase 3 sanity checks (e.g. "the multiplexer
    /// thinks N decoders are alive, the bus agrees").
    #[allow(dead_code)]
    pub fn subscriber_count(&self) -> usize {
        self.subs.lock().expect("iq_bus subs mutex poisoned").len()
    }
}

// =====================================================================
// Consumer-side helpers for the v0.6.0 amplitude-first AGC pre-stage.
// `IqBus` carries cu8 today (RTL-SDR native; SDRplay resamples to cu8
// in the path), so the helpers below assume cu8 and read each chunk
// as raw bytes representing interleaved I, Q centered on 127.5.
// Cf32/cs16 amplitude probes are deferred to the Airspy roadmap item.
// =====================================================================

/// Measure RMS amplitude over `min_samples` complex samples (or as
/// many as arrive before `deadline`), returning dBFS relative to
/// cu8's full-scale magnitude of 127.5.
///
/// RMS (not peak) is the right metric for the v0.6.0 AGC pre-stage:
///
/// * **Outlier-robust.** A single byte of value 0 or 255 — from DC
///   offset, a USB framing glitch, or one stale sample carried over
///   from a previous gain — saturates a peak metric to ~0 dBFS
///   regardless of the actual signal level. RMS averages those out
///   over the whole `min_samples` window.
/// * **Stable across gain probes.** Two probes 80 ms apart at
///   radically different gains should produce monotonically related
///   RMS readings; peak readings on 8-bit cu8 are dominated by noise
///   spikes and quantization edge cases.
/// * **Standard AGC currency.** Broadcast FM rides ~10 dB peak above
///   RMS, so targeting RMS ≈ −20 dBFS leaves comfortable headroom for
///   transients without clipping. (Peak ≈ −6 dBFS, which the old
///   metric chased, is the SAME operating point expressed two
///   different ways — but the RMS-of-real-signal converges on it
///   reliably where peak-of-spiky-data oscillates.)
///
/// Returns `Some(rms_dbfs)` clamped to a floor of −120 dBFS when at
/// least one chunk was observed, or `None` if no chunk arrived before
/// `deadline` (caller should treat as "SDR stalled" and back off).
pub fn rms_dbfs_cu8(
    rx: &Receiver<Arc<[u8]>>,
    min_samples: usize,
    deadline: Duration,
) -> Option<f32> {
    let start = Instant::now();
    let mut samples_seen: usize = 0;
    // Sum of squared deviations from center (128). u64 fits ~2^57
    // before overflow at max-magnitude (~128^2 = 16384) per pair
    // — plenty for any realistic min_samples.
    let mut sumsq: u64 = 0;
    let mut got_any = false;

    while samples_seen < min_samples {
        let remaining = deadline.checked_sub(start.elapsed()).unwrap_or_default();
        if remaining.is_zero() && got_any {
            break;
        }

        let chunk = if got_any {
            match rx.try_recv() {
                Ok(c) => c,
                Err(TryRecvError::Empty) => {
                    let nap = remaining.min(Duration::from_millis(5));
                    if nap.is_zero() {
                        break;
                    }
                    match rx.recv_timeout(nap) {
                        Ok(c) => c,
                        Err(_) => break,
                    }
                }
                Err(TryRecvError::Disconnected) => break,
            }
        } else {
            match rx.recv_timeout(remaining) {
                Ok(c) => c,
                Err(RecvTimeoutError::Timeout) => return None,
                Err(RecvTimeoutError::Disconnected) => return None,
            }
        };
        got_any = true;

        // cu8: each byte is one channel of one sample. dev = b - 128
        // gives signed deviation in [-128, 127]. Square and accumulate.
        for &b in chunk.iter() {
            let dev = b as i16 - 128;
            sumsq += (dev * dev) as u64;
        }
        // Two bytes per complex sample; the RMS denominator counts
        // bytes, not complex pairs, so we get the correct per-channel
        // RMS rather than per-pair (which would be sqrt(2) too high).
        samples_seen += chunk.len() / 2;
    }

    if !got_any {
        return None;
    }

    // Total bytes processed = samples_seen * 2 (I + Q).
    let byte_count = (samples_seen as u64) * 2;
    if byte_count == 0 {
        return Some(-120.0);
    }
    let mean_sq = sumsq as f64 / byte_count as f64;
    let rms = mean_sq.sqrt() as f32;
    if rms <= 0.0 {
        return Some(-120.0);
    }
    // 127.5 is cu8 full-scale magnitude (matches the peak metric's
    // convention so the dBFS scales line up).
    Some((20.0 * (rms / 127.5).log10()).max(-120.0))
}

/// Drain everything currently queued on `rx` without waiting. Returns
/// the number of chunks dropped. Cheap (no syscalls beyond the
/// channel's try_recv) so the AGC pre-stage can call this at the
/// start of every probe to discard pre-gain-change carry-over
/// regardless of how deep the subscriber queue happened to be.
pub fn drain_now(rx: &Receiver<Arc<[u8]>>) -> usize {
    let mut dropped = 0;
    while rx.try_recv().is_ok() {
        dropped += 1;
    }
    dropped
}

/// Drain everything currently queued on `rx` plus everything that
/// arrives during `flush`. Used by the AGC pre-stage to swallow the
/// in-flight chunks held in the SDR USB transfer pipeline (RTL-SDR:
/// ~50 ms; SDRplay: longer per profile) before measuring peak
/// amplitude after a gain write.
///
/// Returns the number of chunks actually drained — useful for the
/// trace log to confirm the flush window matched the device's
/// transfer pipeline depth.
// Kept: exercised by `discard_for_drains_existing_queue` and retained as
// the canonical AGC post-gain-write flush helper; no production caller
// today, so the non-test build flags it dead.
#[allow(dead_code)]
pub fn discard_for(rx: &Receiver<Arc<[u8]>>, flush: Duration) -> usize {
    let start = Instant::now();
    let mut dropped: usize = 0;
    while start.elapsed() < flush {
        let remaining = flush.checked_sub(start.elapsed()).unwrap_or_default();
        if remaining.is_zero() {
            break;
        }
        // Cap the per-recv wait so callers passing a long `flush`
        // still respond promptly when the bus stops publishing
        // (e.g. shutdown mid-flush).
        let nap = remaining.min(Duration::from_millis(5));
        match rx.recv_timeout(nap) {
            Ok(_) => dropped += 1,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    dropped
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn publish_to_single_subscriber_delivers_payload() {
        let bus = IqBus::new();
        let rx = bus.subscribe(4);
        bus.publish(&[1, 2, 3]);
        let got = rx.recv_timeout(Duration::from_millis(50)).unwrap();
        assert_eq!(&*got, &[1, 2, 3]);
    }

    #[test]
    fn publish_fans_out_to_every_subscriber() {
        let bus = IqBus::new();
        let rx1 = bus.subscribe(4);
        let rx2 = bus.subscribe(4);
        bus.publish(&[7, 7]);
        assert_eq!(
            &*rx1.recv_timeout(Duration::from_millis(50)).unwrap(),
            &[7, 7]
        );
        assert_eq!(
            &*rx2.recv_timeout(Duration::from_millis(50)).unwrap(),
            &[7, 7]
        );
    }

    #[test]
    fn full_subscriber_drops_payload_does_not_block() {
        let bus = IqBus::new();
        let _rx = bus.subscribe(1); // capacity 1, never recv'd
        bus.publish(&[1]); // fills the queue
                           // Subsequent publishes must not block or panic; payload silently dropped.
        bus.publish(&[2]);
        bus.publish(&[3]);
        assert_eq!(bus.subscriber_count(), 1);
    }

    #[test]
    fn disconnected_subscriber_pruned_on_next_publish() {
        let bus = IqBus::new();
        let rx = bus.subscribe(4);
        assert_eq!(bus.subscriber_count(), 1);
        drop(rx);
        // Subscriber count drops to zero only after the next publish
        // (lazy pruning).
        bus.publish(&[1]);
        assert_eq!(bus.subscriber_count(), 0);
    }

    #[test]
    fn shutdown_wakes_blocked_subscriber_with_disconnect() {
        let bus = Arc::new(IqBus::new());
        let rx = bus.subscribe(4);
        let bus2 = Arc::clone(&bus);
        let h = std::thread::spawn(move || {
            // Block until shutdown drops the Sender.
            rx.recv()
        });
        std::thread::sleep(Duration::from_millis(20));
        bus2.shutdown();
        let res = h.join().unwrap();
        assert!(res.is_err(), "expected RecvError after shutdown");
    }

    #[test]
    fn rms_dbfs_cu8_zero_signal_floors() {
        // All bytes == 128 → zero deviation from center → -120 dBFS floor.
        let bus = IqBus::new();
        let rx = bus.subscribe(4);
        bus.publish(&vec![128u8; 4096]);
        let got = rms_dbfs_cu8(&rx, 512, Duration::from_millis(50)).unwrap();
        assert!((got - -120.0).abs() < 1e-3, "expected floor, got {got}");
    }

    #[test]
    fn rms_dbfs_cu8_full_scale_is_zero_db() {
        // All bytes at max deviation (255) → RMS = 127 → 20·log10(127/127.5)
        // ≈ -0.034 dBFS. Tests the maximum-possible RMS reading.
        let bus = IqBus::new();
        let rx = bus.subscribe(4);
        let chunk = vec![255u8; 4096];
        bus.publish(&chunk);
        let got = rms_dbfs_cu8(&rx, 512, Duration::from_millis(50)).unwrap();
        assert!(got.abs() < 0.1, "expected ~0 dBFS, got {got}");
    }

    #[test]
    fn rms_dbfs_cu8_half_scale_is_minus_six() {
        // All bytes at deviation 63.75 → RMS = 63.75 → 20·log10(63.75/127.5)
        // = -6.02 dBFS. Use 191/65 to get a mix that averages out around
        // half-scale. Simplest: all bytes 191 (dev=63) → RMS=63.
        let bus = IqBus::new();
        let rx = bus.subscribe(4);
        let chunk = vec![191u8; 4096]; // deviation = 63 every byte
        bus.publish(&chunk);
        let got = rms_dbfs_cu8(&rx, 512, Duration::from_millis(50)).unwrap();
        // 20·log10(63/127.5) ≈ -6.12 dBFS.
        assert!(
            (got - -6.12).abs() < 0.05,
            "expected ~-6.12 dBFS, got {got}"
        );
    }

    #[test]
    fn rms_dbfs_cu8_single_outlier_does_not_saturate() {
        // The whole point of switching from peak to RMS: one rogue
        // byte must NOT drag the measurement to 0 dBFS. With one
        // saturated byte in 4096, RMS ≈ sqrt(128²/4096) = 2 →
        // 20·log10(2/127.5) ≈ -36 dBFS.
        let bus = IqBus::new();
        let rx = bus.subscribe(4);
        let mut chunk = vec![128u8; 4096];
        chunk[42] = 255; // single saturated outlier
        bus.publish(&chunk);
        let got = rms_dbfs_cu8(&rx, 512, Duration::from_millis(50)).unwrap();
        // Should be well below -20 dBFS — the regression guard for
        // the v0.6.0 fine-tune that prompted this rewrite.
        assert!(
            got < -20.0,
            "single outlier should not saturate RMS, got {got}"
        );
    }

    #[test]
    fn rms_dbfs_cu8_no_data_returns_none() {
        let bus = IqBus::new();
        let rx = bus.subscribe(4);
        // No publish → recv_timeout should fire and return None.
        let got = rms_dbfs_cu8(&rx, 512, Duration::from_millis(20));
        assert!(got.is_none(), "expected None on timeout, got {got:?}");
    }

    #[test]
    fn discard_for_drains_existing_queue() {
        let bus = IqBus::new();
        let rx = bus.subscribe(16);
        for _ in 0..5 {
            bus.publish(&[0u8; 8]);
        }
        let dropped = discard_for(&rx, Duration::from_millis(15));
        assert_eq!(dropped, 5, "expected to drain 5 chunks");
        // Queue should be empty after drain.
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn drain_now_empties_queue_without_waiting() {
        let bus = IqBus::new();
        let rx = bus.subscribe(16);
        for _ in 0..7 {
            bus.publish(&[0u8; 8]);
        }
        let before = std::time::Instant::now();
        let dropped = drain_now(&rx);
        let elapsed = before.elapsed();
        assert_eq!(dropped, 7);
        // Drain must be effectively instant — no recv_timeout naps.
        assert!(elapsed.as_millis() < 5, "drain_now took {elapsed:?}");
        assert!(rx.try_recv().is_err());
    }

    // =================================================================
    // Real-time streaming behaviour
    // =================================================================

    #[test]
    fn publish_preserves_fifo_order() {
        // The decoder relies on I/Q chunks arriving in the order the
        // SDR produced them; a reorder would corrupt OFDM sync.
        let bus = IqBus::new();
        let rx = bus.subscribe(32);
        for i in 0..10u8 {
            bus.publish(&[i]);
        }
        for i in 0..10u8 {
            let got = rx.recv_timeout(Duration::from_millis(50)).unwrap();
            assert_eq!(&*got, &[i], "payloads must arrive in publish order");
        }
    }

    #[test]
    fn publish_empty_payload_delivers_empty_chunk() {
        // A zero-length USB transfer is benign — it must round-trip as
        // an empty `Arc<[u8]>`, never a panic or a swallowed delivery.
        let bus = IqBus::new();
        let rx = bus.subscribe(4);
        bus.publish(&[]);
        let got = rx.recv_timeout(Duration::from_millis(50)).unwrap();
        assert!(got.is_empty());
    }

    #[test]
    fn slow_consumer_drops_while_fast_consumer_keeps_up() {
        // Back-pressure isolation: a wedged decoder (never draining,
        // capacity 1) must not throttle the SDR pump or starve the
        // healthy spectrum tap running beside it.
        let bus = IqBus::new();
        let slow = bus.subscribe(1); // never drained
        let fast = bus.subscribe(256); // drained below
        for i in 0..100u8 {
            // Must never block despite `slow` being permanently full.
            bus.publish(&[i]);
        }
        let mut fast_got = 0;
        while fast.try_recv().is_ok() {
            fast_got += 1;
        }
        assert_eq!(fast_got, 100, "fast consumer should receive all 100");
        let mut slow_got = 0;
        while slow.try_recv().is_ok() {
            slow_got += 1;
        }
        assert_eq!(
            slow_got, 1,
            "slow consumer is capacity-bounded → drops the rest"
        );
    }

    // =================================================================
    // Thread start/stop & teardown
    // =================================================================

    #[test]
    fn shutdown_wakes_all_blocked_subscribers() {
        // The bus's bulk shutdown is the app's single teardown lever:
        // every blocked consumer thread must unblock with a RecvError
        // so it can exit without a per-thread stop flag.
        let bus = Arc::new(IqBus::new());
        let mut handles = Vec::new();
        for _ in 0..5 {
            let rx = bus.subscribe(4);
            handles.push(std::thread::spawn(move || rx.recv().is_err()));
        }
        std::thread::sleep(Duration::from_millis(20));
        bus.shutdown();
        for h in handles {
            assert!(
                h.join().unwrap(),
                "every blocked recv must return Err after shutdown"
            );
        }
    }

    #[test]
    fn bus_reusable_after_shutdown() {
        // start_piped builds a fresh bus per session, but shutdown must
        // still leave the instance clean and re-subscribable (and be
        // idempotent) rather than poisoned.
        let bus = IqBus::new();
        let rx1 = bus.subscribe(4);
        bus.publish(&[1]);
        assert_eq!(&*rx1.recv_timeout(Duration::from_millis(50)).unwrap(), &[1]);
        bus.shutdown();
        bus.shutdown(); // idempotent
        assert_eq!(bus.subscriber_count(), 0);
        // The old receiver is now disconnected.
        assert!(rx1.recv().is_err());
        // A fresh subscription works on the same bus instance.
        let rx2 = bus.subscribe(4);
        bus.publish(&[2]);
        assert_eq!(&*rx2.recv_timeout(Duration::from_millis(50)).unwrap(), &[2]);
    }

    // =================================================================
    // SDR signal loss
    // =================================================================

    #[test]
    fn rms_dbfs_cu8_returns_none_when_disconnected_before_any_data() {
        // SDR vanished before the first chunk arrived: the AGC
        // pre-stage must get None (→ "stalled, back off"), never a
        // bogus reading synthesized from zero samples.
        let bus = IqBus::new();
        let rx = bus.subscribe(4);
        bus.shutdown(); // drop the sender → rx is disconnected
        let got = rms_dbfs_cu8(&rx, 512, Duration::from_millis(50));
        assert!(
            got.is_none(),
            "disconnect before data must yield None, got {got:?}"
        );
    }

    // =================================================================
    // Concurrency safety
    // =================================================================

    #[test]
    fn concurrent_publish_and_subscribe_is_safe() {
        // Stress the real producer/consumer race: one thread hammers
        // publish while several consumers subscribe, drain, and drop
        // concurrently. Asserts only timing-independent invariants —
        // no panic, no deadlock, clean pruning afterwards.
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

        let bus = Arc::new(IqBus::new());
        let stop = Arc::new(AtomicBool::new(false));
        let received = Arc::new(AtomicUsize::new(0));

        let prod_bus = Arc::clone(&bus);
        let prod_stop = Arc::clone(&stop);
        let producer = std::thread::spawn(move || {
            let mut n: u64 = 0;
            while !prod_stop.load(Ordering::Relaxed) {
                prod_bus.publish(&n.to_le_bytes());
                n += 1;
            }
            n
        });

        let mut consumers = Vec::new();
        for _ in 0..4 {
            let cbus = Arc::clone(&bus);
            let crecv = Arc::clone(&received);
            consumers.push(std::thread::spawn(move || {
                let rx = cbus.subscribe(32);
                let deadline = std::time::Instant::now() + Duration::from_millis(60);
                while std::time::Instant::now() < deadline {
                    if rx.recv_timeout(Duration::from_millis(5)).is_ok() {
                        crecv.fetch_add(1, Ordering::Relaxed);
                    }
                }
                // rx dropped here → producer prunes it on next publish.
            }));
        }

        for c in consumers {
            c.join().expect("consumer thread panicked");
        }
        stop.store(true, Ordering::Relaxed);
        let published = producer.join().expect("producer thread panicked");

        assert!(published > 0, "producer published nothing");
        assert!(
            received.load(Ordering::Relaxed) > 0,
            "no consumer received any payload"
        );
        // After every consumer dropped, one more publish prunes the
        // stragglers; the bus must settle to zero live subscribers.
        bus.publish(&[0]);
        assert_eq!(bus.subscriber_count(), 0, "dead subscribers not pruned");
    }
}
