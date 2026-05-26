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

use crossbeam_channel::{bounded, Receiver, Sender, TrySendError};
use std::sync::{Arc, Mutex};

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
        self.subs
            .lock()
            .expect("iq_bus subs mutex poisoned")
            .len()
    }
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
        assert_eq!(&*rx1.recv_timeout(Duration::from_millis(50)).unwrap(), &[7, 7]);
        assert_eq!(&*rx2.recv_timeout(Duration::from_millis(50)).unwrap(), &[7, 7]);
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
}
