//! Closed-loop AGC controller — algorithm only, no SDR FFI.
//!
//! ## Why a custom AGC at all
//!
//! The R820T2's hardware tuner AGC drives the analog front-end gain to
//! "fill the ADC" — exactly the wrong target for HD Radio, where the
//! 25 dB above the analog audio carrier is mostly empty spectrum and
//! the digital sidebands ride 25 dB below it. Aggressive hardware AGC
//! over-amplifies the analog carrier and clips the ADC, destroying MER
//! before the COFDM demodulator ever sees a clean symbol. Per-station
//! manual gain works but is tedious; this controller automates it.
//!
//! ## Algorithm in one paragraph
//!
//! Two-phase search on a per-profile gain table (29 steps on R820T2,
//! same on SDRplay's synthesized table), driven by an EMA of
//! `min(MER_lower, MER_upper)`.
//!
//! 1. **Coarse phase** sweeps a small (3–5 point) middle-biased probe
//!    set from the device profile — e.g. `[7.7, 13.7, 19.7, 25.7, 32.8] dB`
//!    on R820T2 — recording EMA at each. Skips the dead bottom of the
//!    table no realistic over-the-air antenna ever needs. Empty
//!    coarse set = skip straight to Fine (legacy behavior).
//! 2. **Fine phase** ±1 hill-climbs around the coarse winner. Direction
//!    flips whenever a step doesn't improve the best; never revisits
//!    an explored idx (the non-oscillation guarantee).
//!
//! Probes are judged on **MER sample count** (default 4 samples ≈ 1 s
//! at nrsc5's 4 Hz MER cadence) rather than a static 5 s timer, with a
//! 250 ms hard floor for event-bus jitter and a 3 s soft ceiling for
//! the "no MER arriving" case. Settles when EMA ≥ 18 dB (raised from
//! 10 dB so the shortcut fires at "optimal", not "audible") or when
//! both neighbours of the best idx are explored. Bails after
//! `bail_after_changes` non-improving probes.
//!
//! ## Surface
//!
//! [`AgcController`] is a pure state machine — no I/O, no threads, no
//! [`Sdr`](crate::sdr::Sdr) reference. The driver (see
//! [`crate::ffi`]) is responsible for:
//!
//! 1. Calling [`AgcController::initial_action`] at startup and applying
//!    its tenths-of-dB value via `Sdr::set_tuner_gain_tenths`.
//! 2. Forwarding every [`NrscEvent`] through [`AgcController::on_event`].
//! 3. Periodically (every ~500 ms is plenty) calling
//!    [`AgcController::tick`] and applying any returned
//!    [`AgcAction`].
//! 4. Reading [`AgcController::snapshot`] for the UI.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use crate::ffi::NrscEvent;

/// Top-level state of the controller. The driver applies whatever the
/// last `apply()` told it to; this enum is for human consumption (UI
/// readout, logs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgcStatus {
    /// Actively probing. The next `tick()` may step gain.
    Probing,
    /// Converged — `tick()` will return `None` from here on.
    Settled,
    /// Gave up after the bail budget. `tick()` will return `None` from
    /// here on. Gain has already been restored to the best-known value.
    Bailed,
}

/// Sub-state of `AgcStatus::Probing`. Surfaced separately so the UI
/// pill can show "PROBING (amp)" / "PROBING (coarse)" / "PROBING (fine)".
/// `Done` is the terminal state for both Settled and Bailed.
///
/// Phase ordering on a fresh tune (libnrsc5 v3.2.0 / v0.6.0):
///   `AmpProbe` -> `MerQualityCheck` -> (`Done` if MER good enough, else
///   `Fine` seeded from the amplitude winner). The legacy `Coarse`
///   phase is skipped on cold start because the amplitude binary search
///   brackets the HD sweet spot more tightly than the 5-point coarse
///   set ever could. Cache-hit warm starts skip both `AmpProbe` and
///   `Coarse` and go directly to `Fine`, preserving the v0.5.x fast
///   path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchPhase {
    /// Binary-search amplitude pre-stage (v0.6.0). Drives RMS dBFS
    /// to `cfg.amp_target_dbfs` in ~5 probes before MER
    /// telemetry is even consulted. Cold-start only; skipped entirely
    /// when `cfg.seeded_from_cache` is true or `cfg.amp_enable` is
    /// false.
    AmpProbe,
    /// Brief MER hand-off after `AmpProbe` converges. Waits a few MER
    /// frames at the amplitude winner; transitions to `Done` if MER
    /// is already good, otherwise to `Fine` seeded from the
    /// amplitude-probe result.
    MerQualityCheck,
    /// Sweeping the profile's `coarse_probe_tenths` set, in order.
    /// Only reached today via the legacy `amp_enable = false` path
    /// (kept for test fixtures and as a fallback if AmpProbe is
    /// disabled).
    Coarse,
    /// ±1 hill-climb centred on the coarse / amplitude-probe winner
    /// (or on the legacy `initial_tenths` when no coarse / amp set
    /// is configured).
    Fine,
    /// Terminal — controller has settled or bailed.
    Done,
}

/// Read-only view of controller state, safe to clone into UI threads.
#[derive(Debug, Clone)]
pub struct AgcSnapshot {
    pub status: AgcStatus,
    pub phase: SearchPhase,
    pub current_idx: usize,
    pub current_tenths: i32,
    pub best_idx: usize,
    /// Tenths-of-dB at `best_idx`. Convenience field so the driver
    /// thread can log "best=X.X dB" without reaching into the
    /// controller's private table. Mirrors `table[best_idx]`.
    pub best_tenths: i32,
    pub best_mer: Option<f32>,
    pub probes_done: u32,
    pub last_change_at: Instant,
    pub last_reason: String,
    /// True when the controller was seeded with a previously-cached
    /// gain (Phase 3 trust-but-verify path). The UI uses this to
    /// render the "(cached)" suffix on the AGC pill at SETTLED.
    /// Mirrors `AgcConfig::seeded_from_cache`.
    pub from_cache: bool,
}

/// A gain change the driver should apply immediately. The driver maps
/// `new_tenths` to [`crate::sdr::Sdr::set_tuner_gain_tenths`] and updates
/// the UI's "last changed" timestamp from the moment of the actual call.
#[derive(Debug, Clone)]
pub struct AgcAction {
    pub new_idx: usize,
    pub new_tenths: i32,
    pub reason: String,
}

/// Knobs. Defaults are Spike 2's validated values; users typically never
/// touch these (and the UI doesn't currently expose them).
#[derive(Debug, Clone, Copy)]
pub struct AgcConfig {
    /// Target dB for `min(MER_lower, MER_upper)`. Reaching this declares
    /// SETTLED immediately. Default 18.0 dB (raised from Spike 2's
    /// 10.0 dB in v0.4.0): 10 dB stops at "audible", 18 dB is
    /// comfortably above the HD demodulator's lock margin and is what
    /// users actually want for clean HD3/HD4 reception. On marginal
    /// stations the threshold is never hit and the explored-set
    /// stability shortcut takes over naturally — so this change only
    /// affects strong stations, where it produces measurably higher
    /// MER without slowing convergence.
    pub mer_target_db: f32,
    /// **Soft ceiling** on time between probes. After this elapses a
    /// probe is judged regardless of how many MER samples have
    /// arrived — covers the "no MER arriving at all" case (no sync)
    /// so the controller still makes forward progress. Set to 4000 ms
    /// to comfortably outlast the nominal 8-sample window at 4 Hz
    /// (~2 s) plus jitter, while still bailing in a reasonable time
    /// on no-sync stations.
    pub probe_period: Duration,
    /// **MER samples** that must arrive at the current gain before the
    /// next probe decision. Replaces the static-time settle gate from
    /// v0.3.x. nrsc5 emits MER ~4 Hz, so 8 samples ≈ 2 s on a steady
    /// station. The first sample after a gain change is contaminated
    /// by sync-recovery transients; at the EMA's α=0.4 its weight
    /// drops from 22 % (4 samples) to 2.8 % (8 samples), which is what
    /// motivated the bump in v0.4.0. Falls back to `probe_period` as a
    /// soft ceiling if no MER ever arrives (no-sync station).
    pub min_mer_samples_post_change: u32,
    /// Give up after this many consecutive non-improving probes. Default
    /// 15 — large enough to walk most of the 29-step table from a
    /// pessimistic start, small enough that we don't probe forever on
    /// hopeless stations (the KNTU 88.1 case).
    pub bail_after_changes: u32,
    /// Starting gain in tenths of dB. Will be snapped to the nearest
    /// step in the supplied gain table. Default 197 = 19.7 dB, the
    /// "moderately hot" point on the R820T2 table where most strong HD
    /// stations sit comfortably while marginal stations need walking
    /// DOWN to find sync.
    pub initial_tenths: i32,
    /// First direction the controller walks the gain table when its
    /// initial gain does not lock immediately. `-1` = walk DOWN
    /// (legacy RTL-SDR behavior — stepping up from an over-clipped
    /// gain can lose sync, while stepping down from a working gain
    /// never does). `+1` = walk UP first — the right call when the
    /// initial gain sits near the conservative end of the table (e.g.
    /// SDRplay starting at mid-table 39 dB where the HD sweet spot is
    /// typically 39..44 dB, not below).
    pub initial_direction: i32,
    /// Coarse probe set in tenths of dB, sourced from the device
    /// profile. Snapped to nearest indices in `table` at controller
    /// construction and visited in order during the **Coarse** phase
    /// before the **Fine** phase ±1 hill-climbs around the winner.
    /// Empty slice = skip coarse entirely (legacy v0.3.x behavior,
    /// useful in tests). See [`crate::sdr::profile::DeviceProfile::coarse_probe_tenths`].
    pub coarse_probe_tenths: &'static [i32],
    /// Phase 3 cache-hit hint: when `true`, the controller starts
    /// directly in [`SearchPhase::Fine`] regardless of
    /// `coarse_probe_tenths` and reports `from_cache=true` on its
    /// snapshot so the UI can render the "(cached)" suffix on
    /// SETTLED. The caller is responsible for setting `initial_tenths`
    /// to the cached gain and `mer_target_db` to `cached_mer - 3.0`
    /// (the trust-but-verify floor) at the same time — this flag only
    /// controls phase-entry and snapshot labeling. Default `false`,
    /// matching all the cache-miss code paths.
    pub seeded_from_cache: bool,

    // --- v0.6.0 amplitude-first AGC pre-stage knobs --------------------
    //
    // The amplitude pre-stage binary-searches the gain table to drive
    // RMS dBFS to `amp_target_dbfs` in ~5 probes, before any
    // MER telemetry is consulted. Each probe applies a candidate gain,
    // flushes the SDR USB transfer pipeline for `amp_flush_ms`, then
    // measures peak amplitude over `amp_probe_ms` of fresh samples. The
    // amplitude winner seeds the existing MER hill-climb, so cold-start
    // tune-to-decode drops from 5–15 s (legacy coarse-then-fine) to
    // sub-second on strong stations.
    /// Target peak amplitude for the pre-stage in dBFS. Default `-6.0`
    /// matches argilo's choice in upstream nrsc5: leaves comfortable
    /// headroom for transients while keeping the SNR floor low
    /// enough that the digital sidebands ride well above quantization
    /// noise. Per-profile overridable (see
    /// [`crate::sdr::profile::DeviceProfile::amp_target_dbfs`]).
    pub amp_target_dbfs: f32,
    /// Number of samples (complex pairs) to scan when measuring peak
    /// amplitude at a candidate gain. Default 16384 samples ≈ 11 ms at
    /// the cu8 sample rate of 1.488 Msps — long enough to catch peaks
    /// in a typical FM broadcast envelope, short enough that 5 probes
    /// total well under 100 ms of measurement time.
    pub amp_probe_samples: u32,
    /// Flush window (milliseconds) between writing a candidate gain
    /// and starting the amplitude measurement. Discards in-flight USB
    /// chunks so the measured peak reflects the new gain, not the
    /// previous one. Default 80 ms (RTL-SDR-tuned). SDRplay needs
    /// longer per its startup-grace evidence (see
    /// [`crate::sdr::profile::DeviceProfile::amp_flush_ms`]).
    pub amp_flush_ms: u32,
    /// Master switch for the amplitude pre-stage. When `false` the
    /// controller falls back to the legacy Coarse-then-Fine algorithm
    /// (preserved for tests and as a kill switch if the pre-stage
    /// ever misbehaves on an exotic station). Default `true`.
    pub amp_enable: bool,
}

impl Default for AgcConfig {
    fn default() -> Self {
        Self {
            mer_target_db: 18.0,
            probe_period: Duration::from_millis(4000),
            min_mer_samples_post_change: 8,
            bail_after_changes: 15,
            initial_tenths: 197,
            initial_direction: -1,
            coarse_probe_tenths: &[],
            seeded_from_cache: false,
            amp_target_dbfs: -20.0,
            amp_probe_samples: 16384,
            amp_flush_ms: 250,
            amp_enable: true,
        }
    }
}

/// Closed-loop AGC state machine. See module docs for the algorithm.
///
/// Lifecycle:
///
/// ```ignore
/// let mut agc = AgcController::new(sdr.gain_table_tenths(), AgcConfig::default());
/// let initial = agc.initial_action();
/// sdr.set_tuner_gain_tenths(initial.new_tenths)?;
/// // ... in the event pump:
/// agc.on_event(&event);
/// // ... in a periodic timer:
/// if let Some(action) = agc.tick() {
///     sdr.set_tuner_gain_tenths(action.new_tenths)?;
/// }
/// ```
pub struct AgcController {
    /// Gain table from `Sdr::gain_table_tenths()` — 29 entries on R820T2.
    /// Owned so the controller isn't lifetime-tied to its SDR backend.
    /// The table is short enough (29 × 4 B on R820T2) that the clone at
    /// construction is trivial.
    table: Vec<i32>,
    cfg: AgcConfig,

    /// Index into `table` for the gain we believe is currently applied.
    gain_idx: usize,
    /// +1 = walking up the table, -1 = walking down. Starts at -1 because
    /// stepping up from an over-clipped gain typically loses sync, while
    /// stepping down from a working gain never does.
    last_dir: i32,

    /// Exponential moving average of `min(MER_lower, MER_upper)`. Reset
    /// to `None` on every gain change so the post-step assessment isn't
    /// polluted by readings from the previous gain.
    ema_mer_min: Option<f32>,
    /// Best EMA observed across the entire run, with the gain idx that
    /// produced it. Used for the "did we improve?" check and as the
    /// restore target on BAIL.
    best_mer_seen: f32,
    best_gain_idx: usize,

    /// Consecutive `tick()`s that did not improve `best_mer_seen`. Once
    /// it reaches `cfg.bail_after_changes`, the controller bails.
    probes_without_improvement: u32,

    /// Set when `apply()` returns an action (i.e. the driver is about to
    /// change gain) — used to enforce `cfg.probe_period` between probes.
    last_change_at: Instant,

    /// Number of `NrscEvent::Mer` events observed since the last gain
    /// change. Drives the Phase 2b adaptive settle gate — `tick()`
    /// returns `None` until this reaches `cfg.min_mer_samples_post_change`
    /// (subject to the 250 ms hard floor and the `cfg.probe_period`
    /// soft ceiling).
    mer_samples_since_change: u32,

    /// Have we ever observed a `Sync` event? Used to differentiate
    /// "MER reading from real lock" vs "garbage from no lock" if we
    /// ever need to in the future. Currently informational.
    has_ever_synced: bool,

    status: AgcStatus,
    /// Current search sub-state. Surfaces in `AgcSnapshot.phase`.
    /// Transitions: `Coarse` → `Fine` (when coarse set exhausted) →
    /// `Done` (when SETTLED or BAILED).
    phase: SearchPhase,
    /// Coarse probe set snapped to indices into `table`, in the order
    /// the controller will visit them. Empty if the profile has no
    /// coarse set — in which case the controller starts in `Fine`.
    coarse_table: Vec<usize>,
    /// Index into `coarse_table` for the next coarse probe. Advances
    /// monotonically; once it reaches `coarse_table.len()` the
    /// controller transitions to `Fine`.
    coarse_pos: usize,
    probes_done: u32,
    last_reason: String,

    /// Indices we've already probed. NEVER REVISITED — that's the
    /// non-oscillation guarantee. Sentinel value `f32::NEG_INFINITY`
    /// means "probed but no MER reading available" (no sync at that
    /// gain). Real EMA values are recorded for the stability shortcut.
    explored: BTreeMap<usize, f32>,

    // --- v0.6.0 amplitude-first AGC pre-stage state -------------------
    /// Live binary-search window over `table` indices. `lo` and `hi`
    /// bracket the highest-safe-gain index; `mid` is the candidate
    /// being measured. Only meaningful while `phase == AmpProbe`.
    amp_lo: usize,
    amp_hi: usize,
    /// Index whose amplitude is currently being measured. `None` until
    /// the first probe is dispatched.
    amp_probing: Option<usize>,
    /// `true` once at least one amp-probe came back at or below the
    /// target dBFS. Used at termination to distinguish "`amp_lo` is
    /// genuinely the highest confirmed-safe gain" from "`amp_lo`
    /// never advanced because every probe was too hot" (e.g. a strong
    /// signal combined with a profile whose minimum gain still
    /// over-drives the target). When false at termination, we abort
    /// the amplitude pre-stage and fall through to Coarse seeded from
    /// the profile's default initial gain, rather than committing the
    /// (unprobed) `idx 0` as the "winner".
    amp_ever_safe: bool,
    /// MER frames remaining in `MerQualityCheck` before the handoff
    /// decision (settle vs. fall through to Fine). Counted down via
    /// `on_event` each MER event.
    mer_check_remaining: u32,
}

impl AgcController {
    /// Create a fresh controller for the supplied gain table.
    ///
    /// `table` must be sorted ascending in tenths of dB (which is the
    /// contract for `Sdr::gain_table_tenths`). Empty tables panic in
    /// debug; in release they would deadlock on `tick`, so callers must
    /// validate the SDR has a usable table before constructing.
    pub fn new(table: &[i32], cfg: AgcConfig) -> Self {
        debug_assert!(!table.is_empty(), "AGC requires a non-empty gain table");
        let gain_idx = nearest_idx(table, cfg.initial_tenths);
        // Normalize the configured direction to ±1. Anything zero or
        // bogus falls back to the legacy walk-down behavior.
        let last_dir = if cfg.initial_direction >= 1 { 1 } else { -1 };
        // Snap the profile's coarse probe set to real table indices and
        // deduplicate while preserving the supplied order — the first
        // appearance of a duplicate wins so the profile author controls
        // visit sequence. Empty coarse set = skip straight to Fine,
        // which keeps the legacy v0.3.x algorithm available for tests.
        let mut coarse_table: Vec<usize> = Vec::with_capacity(cfg.coarse_probe_tenths.len());
        for &t in cfg.coarse_probe_tenths {
            let idx = nearest_idx(table, t);
            if !coarse_table.contains(&idx) {
                coarse_table.push(idx);
            }
        }
        // Phase 3 cache hit: skip Coarse entirely and walk ±1 from
        // the cached seed. The coarse_table stays around for
        // introspection but `coarse_pos` is parked at the end so
        // `tick()` never advances into it. Cache miss = legacy
        // behavior (Coarse if non-empty, else Fine).
        //
        // v0.6.0: cold-start cache miss now enters `AmpProbe` first
        // when `amp_enable` is true and the table is wide enough for a
        // binary search to be worthwhile (2+ steps). The amplitude
        // pre-stage tracks its own `amp_lo` / `amp_hi` window over the
        // full table; the coarse / fine fall-through is preserved as
        // the `amp_enable = false` path so tests and emergency kill
        // switch stay deterministic.
        let (phase, coarse_pos) = if cfg.seeded_from_cache {
            (SearchPhase::Fine, coarse_table.len())
        } else if cfg.amp_enable && table.len() >= 2 {
            (SearchPhase::AmpProbe, 0)
        } else if coarse_table.is_empty() {
            (SearchPhase::Fine, 0)
        } else {
            (SearchPhase::Coarse, 0)
        };
        let amp_hi = table.len().saturating_sub(1);
        Self {
            table: table.to_vec(),
            cfg,
            gain_idx,
            last_dir,
            ema_mer_min: None,
            best_mer_seen: f32::NEG_INFINITY,
            best_gain_idx: gain_idx,
            probes_without_improvement: 0,
            last_change_at: Instant::now(),
            mer_samples_since_change: 0,
            has_ever_synced: false,
            status: AgcStatus::Probing,
            phase,
            coarse_table,
            coarse_pos,
            probes_done: 0,
            last_reason: "initial start gain".to_string(),
            explored: BTreeMap::new(),
            amp_lo: 0,
            amp_hi,
            amp_probing: None,
            amp_ever_safe: false,
            mer_check_remaining: 0,
        }
    }

    /// Initial gain to apply before any events flow. The driver should
    /// call this once at AGC startup and apply the returned tenths via
    /// `Sdr::set_tuner_gain_tenths`. Subsequent gains come from
    /// [`Self::tick`].
    pub fn initial_action(&mut self) -> AgcAction {
        let tenths = self.table[self.gain_idx];
        self.last_change_at = Instant::now();
        self.mer_samples_since_change = 0;
        AgcAction {
            new_idx: self.gain_idx,
            new_tenths: tenths,
            reason: "initial start gain".to_string(),
        }
    }

    /// Snapshot for the UI thread. Cheap (clones a `String` and a few
    /// scalars); safe to call every paint.
    pub fn snapshot(&self) -> AgcSnapshot {
        AgcSnapshot {
            status: self.status,
            phase: self.phase,
            current_idx: self.gain_idx,
            current_tenths: self.table[self.gain_idx],
            best_idx: self.best_gain_idx,
            best_tenths: self.table[self.best_gain_idx],
            best_mer: if self.best_mer_seen.is_finite() {
                Some(self.best_mer_seen)
            } else {
                None
            },
            probes_done: self.probes_done,
            last_change_at: self.last_change_at,
            last_reason: self.last_reason.clone(),
            from_cache: self.cfg.seeded_from_cache,
        }
    }

    /// Feed the controller a single nrsc5 event. Cheap; safe to call
    /// from the stderr-parser thread for every event in the stream.
    pub fn on_event(&mut self, ev: &NrscEvent) {
        match ev {
            NrscEvent::Sync { .. } => {
                self.has_ever_synced = true;
            }
            NrscEvent::Mer { lower, upper } => {
                let m = lower.min(*upper);
                self.ema_mer_min = Some(match self.ema_mer_min {
                    Some(prev) => 0.6 * prev + 0.4 * m,
                    None => m,
                });
                // Count toward the Phase 2b adaptive settle gate.
                // Saturate at u32::MAX so a no-decision stream doesn't
                // wrap around (it'd take ~33 years at 4 Hz, but
                // saturating is free and explicit).
                self.mer_samples_since_change =
                    self.mer_samples_since_change.saturating_add(1);
                // v0.6.0: drive the MerQualityCheck countdown so the
                // amplitude-pre-stage handoff fires after `n` MER
                // frames at the amplitude winner, regardless of how
                // often the driver calls `tick`.
                if self.phase == SearchPhase::MerQualityCheck
                    && self.mer_check_remaining > 0
                {
                    self.mer_check_remaining -= 1;
                }
            }
            // BER, LostSync, audio events, metadata — informational only
            // for now. A future version may use sustained high BER as a
            // secondary "this gain isn't working" signal, but Spike 2
            // showed MER alone is sufficient.
            _ => {}
        }
    }

    /// Periodic decision step. Returns `Some(action)` when the controller
    /// wants the driver to change gain right now; `None` otherwise.
    /// Safe to call at any cadence — internally throttled by the
    /// Phase 2b adaptive settle gate (sample count + soft time ceiling).
    /// Idempotent once `status` is `Settled` or `Bailed`.
    pub fn tick(&mut self) -> Option<AgcAction> {
        if self.status != AgcStatus::Probing {
            return None;
        }

        // v0.6.0: AmpProbe phase is driven by `tick_amp`, not this
        // method. The driver picks the right entry per phase. Silently
        // no-op here so a tick during AmpProbe doesn't stomp state.
        if self.phase == SearchPhase::AmpProbe {
            return None;
        }

        // v0.6.0: MerQualityCheck is the bridge between AmpProbe and
        // the legacy state machine. Block in this phase until either
        // (a) enough MER frames have arrived to trust the reading, or
        // (b) the soft `probe_period` ceiling fires (no-MER case).
        // Once unblocked, settle if MER meets target, otherwise hand
        // off to Fine seeded from the amplitude-probe winner.
        if self.phase == SearchPhase::MerQualityCheck {
            let elapsed = self.last_change_at.elapsed();
            let samples_ok =
                self.mer_samples_since_change >= self.cfg.min_mer_samples_post_change
                    && self.mer_check_remaining == 0;
            let timeout_ok = elapsed >= self.cfg.probe_period;
            if !samples_ok && !timeout_ok {
                return None;
            }
            // Decide settle vs. fall-through to Fine.
            self.probes_done += 1;
            let current_ema = self.ema_mer_min;
            if let Some(e) = current_ema {
                // Mirror the bookkeeping `tick`'s main path does so
                // the snapshot / explored map stay consistent.
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
                }
                if e >= self.cfg.mer_target_db {
                    self.last_reason = format!(
                        "amp+mer: ema {:.2} dB >= target {:.1} dB; settled",
                        e, self.cfg.mer_target_db
                    );
                    self.status = AgcStatus::Settled;
                    self.phase = SearchPhase::Done;
                    return None;
                }
            }
            // MER below target (or no MER): hand off to Fine seeded
            // from the amplitude winner. Reset the explored map so the
            // Fine ±1 walk can examine the amplitude winner's
            // neighbours without "already explored" short-circuits.
            self.phase = SearchPhase::Fine;
            self.last_dir = 1;
            self.explored.clear();
            self.probes_without_improvement = 0;
            self.last_reason = match current_ema {
                Some(e) => format!(
                    "amp+mer: ema {:.2} dB below target; handing off to fine from idx {} ({:.1} dB)",
                    e,
                    self.gain_idx,
                    self.table[self.gain_idx] as f32 / 10.0,
                ),
                None => format!(
                    "amp+mer: no MER lock; handing off to fine from idx {} ({:.1} dB)",
                    self.gain_idx,
                    self.table[self.gain_idx] as f32 / 10.0,
                ),
            };
            // Fall through into the main tick logic so Fine makes its
            // first move immediately rather than waiting another tick.
        }

        // -- 0. Phase 2b adaptive settle gate. -------------------------
        //
        // Two gates, OR'd. Fire a probe decision if EITHER:
        //   (a) `min_mer_samples_post_change` MER events have arrived
        //       at the current gain (the EMA reflects a real reading), or
        //   (b) `probe_period` has elapsed since the last gain change
        //       (covers the "no sync, no MER" case so the controller
        //       still makes forward progress when the radio is dead).
        //
        // Default 4 samples ≈ 1 s at nrsc5's 4 Hz MER cadence; 3 s
        // soft ceiling. Tests zero both knobs and drive synthetic
        // events to get deterministic single-tick decisions.
        let elapsed = self.last_change_at.elapsed();
        let samples_ok =
            self.mer_samples_since_change >= self.cfg.min_mer_samples_post_change;
        let timeout_ok = elapsed >= self.cfg.probe_period;
        if !samples_ok && !timeout_ok {
            return None;
        }

        self.probes_done += 1;

        // v0.6.0 placeholder-skip guard: on the very first Coarse tick
        // the current gain reflects the profile's placeholder
        // `initial_tenths`, NOT a deliberate probe. Anchoring
        // `best_gain_idx` to that placeholder measurement wedges the
        // search when none of the coarse probes happen to beat it
        // (SDRplay on 97.1 MHz, June 2026: initial idx 19 MER 3.88
        // dB held the crown through the entire coarse sweep, then
        // Fine bracketed at idx 19 with both neighbours falsely
        // marked "explored" by the coarse pass). The placeholder
        // also doesn't belong in `explored` for the same reason — if
        // it coincides with a coarse point the coarse sweep will
        // probe it for real and record the result then. Fine-only
        // configs (empty coarse table → phase starts as Fine) are
        // unaffected.
        let skip_record = self.probes_done == 1 && self.phase == SearchPhase::Coarse;

        // -- 1. Record what we observed at the current gain. -------------
        let current_ema = self.ema_mer_min;
        if !skip_record {
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
                    self.explored
                        .entry(self.gain_idx)
                        .or_insert(f32::NEG_INFINITY);
                    self.probes_without_improvement += 1;
                }
            }
        }

        // -- 2. Target hit? -------------------------------------------
        if let Some(e) = current_ema {
            if e >= self.cfg.mer_target_db {
                self.last_reason = format!(
                    "ema {:.2} dB >= target {:.1} dB",
                    e, self.cfg.mer_target_db
                );
                self.status = AgcStatus::Settled;
                self.phase = SearchPhase::Done;
                return None;
            }
        }

        // -- 3. Bail-out budget exhausted? ----------------------------
        if self.probes_without_improvement >= self.cfg.bail_after_changes {
            // Restore best-known gain before bailing (even if best is
            // garbage — leave the radio in the least-bad state).
            self.status = AgcStatus::Bailed;
            self.phase = SearchPhase::Done;
            if self.best_gain_idx != self.gain_idx {
                let new_idx = self.best_gain_idx;
                let new_tenths = self.table[new_idx];
                self.gain_idx = new_idx;
                self.last_change_at = Instant::now();
                self.ema_mer_min = None;
                self.mer_samples_since_change = 0;
                self.last_reason = format!(
                    "bail-out: restoring best-known gain (best ema {:.2})",
                    self.best_mer_seen
                );
                return Some(AgcAction {
                    new_idx,
                    new_tenths,
                    reason: self.last_reason.clone(),
                });
            }
            self.last_reason = format!(
                "bail-out: no improvement in {} probes",
                self.cfg.bail_after_changes
            );
            return None;
        }

        // -- 4. Coarse phase: visit the next unprobed coarse point. ----
        //
        // The coarse set is sized 3–5 points biased toward the family's
        // HD sweet spot. We iterate it in order; any coarse point
        // already in `explored` (e.g. the initial idx if it happened
        // to match one) is skipped silently so we don't waste a probe.
        // When the sweep exhausts, transition to Fine and emit a
        // "return to best" step so the ±1 hill-climb starts from the
        // coarse winner. If `gain_idx` is already at best (e.g. the
        // last coarse point WAS the best), we fall through into the
        // Fine direction-walker on this same tick — no need to waste
        // a no-op probe.
        if self.phase == SearchPhase::Coarse {
            while self.coarse_pos < self.coarse_table.len() {
                let idx = self.coarse_table[self.coarse_pos];
                self.coarse_pos += 1;
                if !self.explored.contains_key(&idx) {
                    let new_tenths = self.table[idx];
                    self.gain_idx = idx;
                    self.last_change_at = Instant::now();
                    self.ema_mer_min = None;
                    self.mer_samples_since_change = 0;
                    self.last_reason = format!(
                        "coarse {}/{} -> probing idx {} ({:.1} dB)",
                        self.coarse_pos,
                        self.coarse_table.len(),
                        idx,
                        new_tenths as f32 / 10.0,
                    );
                    return Some(AgcAction {
                        new_idx: idx,
                        new_tenths,
                        reason: self.last_reason.clone(),
                    });
                }
            }
            // Coarse exhausted — hand off to Fine.
            self.phase = SearchPhase::Fine;
            // Default Fine to walk UP from the coarse winner. The
            // coarse set is middle-biased toward the family's HD sweet
            // spot, so the true optimum is typically AT or slightly
            // above the coarse winner; walking up is the right first
            // guess. Bad guess auto-corrects on the next tick via the
            // standard "no improvement → flip direction" logic.
            self.last_dir = 1;
            if self.gain_idx != self.best_gain_idx {
                let new_idx = self.best_gain_idx;
                let new_tenths = self.table[new_idx];
                self.gain_idx = new_idx;
                self.last_change_at = Instant::now();
                self.ema_mer_min = None;
                self.mer_samples_since_change = 0;
                self.last_reason = format!(
                    "coarse done; centring on best idx {} ({:.1} dB, ema {:.2}) for fine",
                    new_idx,
                    new_tenths as f32 / 10.0,
                    self.best_mer_seen
                );
                return Some(AgcAction {
                    new_idx,
                    new_tenths,
                    reason: self.last_reason.clone(),
                });
            }
            // Already at best — fall through into Fine on this tick.
        }

        // -- 5. Fine phase: ±1 hill-climb anchored on best_gain_idx. --
        //
        // CRITICAL: the next probe is `best_gain_idx ± 1`, NOT
        // `gain_idx ± 1`. Anchoring on best (rather than the last
        // probe position) makes the search a strict contiguous
        // expansion of the explored region around the running peak,
        // which:
        //   1. Guarantees we never "jump over" the explored block when
        //      flipping direction. The pre-fix algorithm walked from
        //      gain_idx looking for the next unexplored cell, which
        //      after a few flips would skip past the entire explored
        //      contiguous span and land arbitrarily far from best —
        //      producing the high/low oscillation that prompted this
        //      rewrite.
        //   2. Forces the unimodal-MER assumption: once both
        //      immediate neighbours of best are explored, the peak is
        //      bracketed and we settle. The previous "stability
        //      shortcut" tried to express this AFTER the bad jump
        //      had already been queued, which left a window where
        //      the controller wandered before catching itself.
        //
        // Direction choice:
        //   * If the last probe IMPROVED best (current_ema ≈
        //     best_mer_seen after the update in step 1), keep the
        //     same direction — we're climbing successfully, the next
        //     step is `new_best ± 1` in the same dir.
        //   * If the last probe was strictly worse (or no MER and we
        //     have a finite best), flip — the other side of best is
        //     unexplored.
        //   * If no MER ever, no best yet (first ticks on a dead
        //     station), keep the current direction so we scan for
        //     any lock.
        let preferred_dir = match current_ema {
            Some(e) if (e - self.best_mer_seen).abs() < 0.01 => self.last_dir,
            Some(_) => -self.last_dir,
            None if self.best_mer_seen.is_finite() => -self.last_dir,
            None => self.last_dir,
        };

        let bi = self.best_gain_idx as i32;
        let max_i = self.table.len() as i32 - 1;
        let neighbour = |dir: i32| -> Option<usize> {
            let i = bi + dir;
            if i < 0 || i > max_i {
                return None;
            }
            let idx = i as usize;
            if self.explored.contains_key(&idx) {
                None
            } else {
                Some(idx)
            }
        };

        let (next_idx, chosen_dir) = match neighbour(preferred_dir) {
            Some(idx) => (idx, preferred_dir),
            None => match neighbour(-preferred_dir) {
                Some(idx) => (idx, -preferred_dir),
                None => {
                    // Both immediate neighbours of best are explored
                    // (or off the table). The peak is bracketed.
                    // Settle if best is usable, bail otherwise.
                    let usable = self.best_mer_seen >= 6.0;
                    self.phase = SearchPhase::Done;
                    self.status = if usable {
                        AgcStatus::Settled
                    } else {
                        AgcStatus::Bailed
                    };
                    if self.gain_idx != self.best_gain_idx {
                        let new_idx = self.best_gain_idx;
                        let new_tenths = self.table[new_idx];
                        self.gain_idx = new_idx;
                        self.last_change_at = Instant::now();
                        self.ema_mer_min = None;
                        self.mer_samples_since_change = 0;
                        self.last_reason = format!(
                            "peak bracketed at idx {} ({:.1} dB, ema {:.2}); {}",
                            new_idx,
                            new_tenths as f32 / 10.0,
                            self.best_mer_seen,
                            if usable { "settled" } else { "bailing" }
                        );
                        return Some(AgcAction {
                            new_idx,
                            new_tenths,
                            reason: self.last_reason.clone(),
                        });
                    }
                    self.last_reason = format!(
                        "peak bracketed at best idx (ema {:.2}); {}",
                        self.best_mer_seen,
                        if usable { "settled" } else { "bailing" }
                    );
                    return None;
                }
            },
        };
        self.last_dir = chosen_dir;

        // -- 6. Probe the chosen neighbour of best. --------------------
        let new_tenths = self.table[next_idx];
        self.gain_idx = next_idx;
        self.last_change_at = Instant::now();
        self.ema_mer_min = None;
        self.mer_samples_since_change = 0;
        self.last_reason = match current_ema {
            Some(e) => format!(
                "fine: ema {:.2} best {:.2} @ idx {} -> probing idx {} ({:.1} dB, {})",
                e,
                self.best_mer_seen,
                self.best_gain_idx,
                next_idx,
                new_tenths as f32 / 10.0,
                if chosen_dir > 0 { "up" } else { "down" }
            ),
            None => format!(
                "fine: no MER at idx {} -> probing idx {} ({:.1} dB, {})",
                self.best_gain_idx,
                next_idx,
                new_tenths as f32 / 10.0,
                if chosen_dir > 0 { "up" } else { "down" }
            ),
        };
        Some(AgcAction {
            new_idx: next_idx,
            new_tenths,
            reason: self.last_reason.clone(),
        })
    }

    /// v0.6.0 amplitude pre-stage entry point. The driver thread calls
    /// this while `phase == AmpProbe`:
    ///
    /// 1. With `rms_dbfs = None` on first entry — the controller
    ///    seeds the binary-search midpoint and returns an
    ///    [`AgcAction`] to write that gain.
    /// 2. With `rms_dbfs = Some(reading)` on every subsequent call,
    ///    where `reading` is the measured **RMS** dBFS over a fresh
    ///    `cfg.amp_probe_samples` window taken *after* a
    ///    drain → `cfg.amp_flush_ms` sleep → drain sequence (the
    ///    drain-sleep-drain pattern is what guarantees no
    ///    pre-gain-change samples leak into the measurement; see
    ///    `sdr::iq_bus::rms_dbfs_cu8` for why RMS instead of peak).
    ///    The controller compares the reading to
    ///    `cfg.amp_target_dbfs`, narrows the window, and either
    ///    returns the next probe action or transitions to
    ///    [`SearchPhase::MerQualityCheck`] (returning `Some` to write
    ///    the amplitude winner one last time so the table-index and
    ///    the applied gain are in sync).
    ///
    /// Returns `None` when the controller has nothing for the driver
    /// to do (which today means the phase is no longer `AmpProbe`).
    /// Idempotent once `phase` advances past `AmpProbe`.
    ///
    /// Binary-search bias: we hunt for the **highest** index whose
    /// RMS is still ≤ target (i.e. the loudest non-clipping gain).
    /// On "too hot" we shrink `hi = mid - 1`; on "safe" we expand
    /// `lo = mid` (NOT `mid + 1` — `mid` itself is now a confirmed
    /// safe value, and we want to consider it the floor while
    /// continuing to probe higher).
    pub fn tick_amp(&mut self, rms_dbfs: Option<f32>) -> Option<AgcAction> {
        if self.status != AgcStatus::Probing || self.phase != SearchPhase::AmpProbe {
            return None;
        }

        // Score the last probe (if any) and tighten the window.
        if let (Some(probed), Some(reading)) = (self.amp_probing, rms_dbfs) {
            if reading > self.cfg.amp_target_dbfs {
                // Too hot — lower bound stays, upper bound contracts.
                // saturating_sub at probed=0 collapses the window
                // (`hi = 0` even though idx 0 is NOT safe); the
                // `amp_ever_safe` check at termination catches that
                // case and aborts cleanly rather than committing
                // the unprobed default to `MerQualityCheck`.
                self.amp_hi = probed.saturating_sub(1);
            } else {
                // Safe — `probed` is now the highest confirmed-safe
                // index. Bias high: keep `mid` as the new floor.
                self.amp_lo = probed;
                self.amp_ever_safe = true;
            }
            self.probes_done += 1;
        }

        // Abort path: window collapsed but no probe was ever safe.
        // Every gain in the table over-drives the target dBFS —
        // typically means the antenna is hot enough that even the
        // minimum gain is above target, OR (more commonly on SDRplay)
        // the profile's amp_target is unreachable because the
        // aggregate-gain floor sits well above the real ADC noise
        // floor. Bail out of the amp pre-stage and let the legacy
        // Coarse-then-Fine search take over from the profile's
        // default initial gain. The Coarse set is biased toward each
        // family's HD sweet spot, so this fallback lands in a
        // useful place without an MER-blind probe sweep.
        if self.amp_lo >= self.amp_hi && !self.amp_ever_safe {
            let seed_idx = nearest_idx(&self.table, self.cfg.initial_tenths);
            let seed_tenths = self.table[seed_idx];
            // Pick Coarse if the profile has a non-empty set,
            // otherwise Fine. Mirrors the cold-start path in `new`.
            self.phase = if self.coarse_table.is_empty() {
                SearchPhase::Fine
            } else {
                SearchPhase::Coarse
            };
            self.amp_probing = None;
            self.best_gain_idx = seed_idx;
            self.best_mer_seen = f32::NEG_INFINITY;
            self.explored.clear();
            self.probes_without_improvement = 0;
            self.last_dir = if self.cfg.initial_direction >= 1 { 1 } else { -1 };
            self.last_reason = format!(
                "amp-probe: every gain above target {:.1} dBFS; aborting amp \
                 pre-stage and seeding {} from idx {} ({:.1} dB)",
                self.cfg.amp_target_dbfs,
                match self.phase {
                    SearchPhase::Coarse => "coarse",
                    SearchPhase::Fine => "fine",
                    _ => "search",
                },
                seed_idx,
                seed_tenths as f32 / 10.0,
            );
            self.gain_idx = seed_idx;
            self.last_change_at = Instant::now();
            self.ema_mer_min = None;
            self.mer_samples_since_change = 0;
            return Some(AgcAction {
                new_idx: seed_idx,
                new_tenths: seed_tenths,
                reason: self.last_reason.clone(),
            });
        }

        // Termination: window collapsed. The amplitude winner is
        // `amp_lo` (highest confirmed safe). Transition to
        // MerQualityCheck and emit one final gain write so the
        // applied gain matches the winner index (the last probe may
        // have been the over-hot `mid` that triggered the `hi` cut).
        if self.amp_lo >= self.amp_hi {
            let winner = self.amp_lo;
            let winner_tenths = self.table[winner];
            let already_here = self.gain_idx == winner;
            self.phase = SearchPhase::MerQualityCheck;
            // Wait 3 MER frames (~750 ms at nrsc5's 4 Hz cadence)
            // for the EMA to stabilize before settle/handoff.
            self.mer_check_remaining = 3;
            self.amp_probing = None;
            self.last_reason = format!(
                "amp-probe converged: idx {} ({:.1} dB, rms {:.2} dBFS); awaiting mer",
                winner,
                winner_tenths as f32 / 10.0,
                rms_dbfs.unwrap_or(f32::NAN),
            );
            if already_here {
                // Driver still needs the gain refreshed so EMA reset
                // semantics match a normal phase change. Reset the
                // settle gate clocks even when not writing gain.
                self.last_change_at = Instant::now();
                self.mer_samples_since_change = 0;
                self.ema_mer_min = None;
                return None;
            }
            self.gain_idx = winner;
            self.last_change_at = Instant::now();
            self.ema_mer_min = None;
            self.mer_samples_since_change = 0;
            return Some(AgcAction {
                new_idx: winner,
                new_tenths: winner_tenths,
                reason: self.last_reason.clone(),
            });
        }

        // Otherwise pick the next midpoint. Bias high: round the
        // midpoint up so a 2-element window probes the upper element,
        // matching our "highest safe gain wins" rule.
        let mid = self.amp_lo + (self.amp_hi - self.amp_lo + 1) / 2;
        let mid_tenths = self.table[mid];
        self.amp_probing = Some(mid);
        self.gain_idx = mid;
        self.last_change_at = Instant::now();
        self.ema_mer_min = None;
        self.mer_samples_since_change = 0;
        self.last_reason = match rms_dbfs {
            Some(p) => format!(
                "amp-probe [{}..{}] -> idx {} ({:.1} dB) target {:.1} dBFS, last rms {:.2} dBFS",
                self.amp_lo,
                self.amp_hi,
                mid,
                mid_tenths as f32 / 10.0,
                self.cfg.amp_target_dbfs,
                p,
            ),
            None => format!(
                "amp-probe seed [{}..{}] -> idx {} ({:.1} dB) target {:.1} dBFS",
                self.amp_lo,
                self.amp_hi,
                mid,
                mid_tenths as f32 / 10.0,
                self.cfg.amp_target_dbfs,
            ),
        };
        Some(AgcAction {
            new_idx: mid,
            new_tenths: mid_tenths,
            reason: self.last_reason.clone(),
        })
    }

    // (Phase 2c: removed `next_unexplored` — the Fine walk now
    // queries `best_gain_idx ± 1` directly, see step 5 in `tick`.
    // Kept this comment as a tombstone so a future "why no
    // next_unexplored?" question lands on the answer.)
}

/// Nearest index in `table` to `target_tenths`. Used at startup to snap
/// the requested initial gain to a real step.
fn nearest_idx(table: &[i32], target_tenths: i32) -> usize {
    let mut best = 0usize;
    let mut best_diff = i32::MAX;
    for (i, &g) in table.iter().enumerate() {
        let d = (g - target_tenths).abs();
        if d < best_diff {
            best_diff = d;
            best = i;
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sdr::R820T_GAINS_TENTHS;

    fn cfg_fast() -> AgcConfig {
        AgcConfig {
            // Keep the legacy 10 dB target so the existing "good MER →
            // settle" tests don't need to feed 18 dB synthetic readings.
            // Real-world default is 18 dB (Phase 2c); the algorithm is
            // identical at any positive threshold.
            mer_target_db: 10.0,
            // Skip the time-based settle hold in tests by setting both
            // gates to zero — every tick() can immediately make a
            // decision once MER samples are fed in.
            probe_period: Duration::from_millis(0),
            min_mer_samples_post_change: 0,
            bail_after_changes: 10,
            initial_tenths: 197,
            initial_direction: -1,
            // Phase 2 default: legacy direction-walker (no coarse).
            // Coarse-specific tests construct their own configs.
            coarse_probe_tenths: &[],
            // Phase 3 default: cache-miss path (fresh search). The
            // dedicated cache-hit tests opt in explicitly.
            seeded_from_cache: false,
            // v0.6.0: disable the amplitude pre-stage in the legacy
            // tests so the Coarse/Fine state machine is exercised
            // directly. AmpProbe gets its own dedicated tests below.
            amp_target_dbfs: -20.0,
            amp_probe_samples: 16384,
            amp_flush_ms: 120,
            amp_enable: false,
        }
    }

    fn drive(agc: &mut AgcController, ema: f32) -> Option<AgcAction> {
        // Two MER events with matched lower/upper → EMA converges quickly.
        for _ in 0..5 {
            agc.on_event(&NrscEvent::Mer {
                lower: ema,
                upper: ema,
            });
        }
        agc.tick()
    }

    #[test]
    fn settles_immediately_when_initial_gain_is_good() {
        let mut agc = AgcController::new(R820T_GAINS_TENTHS, cfg_fast());
        let initial = agc.initial_action();
        assert_eq!(initial.new_tenths, R820T_GAINS_TENTHS[nearest_idx(R820T_GAINS_TENTHS, 197)]);
        let action = drive(&mut agc, 12.5);
        assert!(action.is_none(), "expected settle, got step");
        assert_eq!(agc.snapshot().status, AgcStatus::Settled);
        assert_eq!(agc.snapshot().phase, SearchPhase::Done);
    }

    #[test]
    fn walks_down_on_over_clipping() {
        // KEGL-style: initial gain produces terrible MER, AGC must walk
        // down and converge on a lower gain. Uses legacy Fine-only
        // config so the direction-walker path is exercised directly.
        let mut agc = AgcController::new(R820T_GAINS_TENTHS, cfg_fast());
        let _ = agc.initial_action();
        // Probe 1: terrible MER → step down
        let step1 = drive(&mut agc, -5.0).expect("expected step");
        assert!(
            step1.new_idx < nearest_idx(R820T_GAINS_TENTHS, 197),
            "expected to walk DOWN from 197 tenths, got idx {}",
            step1.new_idx
        );
    }

    #[test]
    fn bails_after_budget_when_nothing_works() {
        let mut agc = AgcController::new(
            R820T_GAINS_TENTHS,
            AgcConfig {
                bail_after_changes: 3,
                ..cfg_fast()
            },
        );
        let _ = agc.initial_action();
        // Feed terrible MER repeatedly — no gain helps.
        for _ in 0..10 {
            let _ = drive(&mut agc, -10.0);
            if agc.snapshot().status == AgcStatus::Bailed {
                return;
            }
        }
        panic!("expected BAIL after budget exhausted, status = {:?}", agc.snapshot().status);
    }

    #[test]
    fn no_oscillation_revisits() {
        // The explored-set guarantee: while status is `Probing`, no
        // unexplored index ever gets two `apply()` calls in the same
        // run. Once status becomes Settled or Bailed the controller is
        // allowed (and required) to restore the best-known gain — which
        // is by definition already in `explored`. That final restore
        // does not count as oscillation.
        let mut agc = AgcController::new(R820T_GAINS_TENTHS, cfg_fast());
        let initial = agc.initial_action();
        let mut visited = std::collections::HashSet::new();
        visited.insert(initial.new_idx);
        for _ in 0..R820T_GAINS_TENTHS.len() * 2 {
            // Alternate good/bad MER to provoke direction changes.
            let mer = if visited.len() % 2 == 0 { 5.0 } else { -2.0 };
            let Some(action) = drive(&mut agc, mer) else {
                break; // settled or bailed without a final step
            };
            match agc.snapshot().status {
                AgcStatus::Probing => {
                    assert!(
                        visited.insert(action.new_idx),
                        "AGC revisited idx {} while still probing (visited so far: {:?})",
                        action.new_idx,
                        visited
                    );
                }
                AgcStatus::Settled | AgcStatus::Bailed => {
                    // Final restore-to-best is allowed.
                    break;
                }
            }
        }
    }

    // --- Phase 2a: Coarse-then-Fine ---------------------------------

    #[test]
    fn coarse_phase_visits_supplied_points_in_order() {
        // Construct a controller with a 3-point coarse set on the
        // R820T2 table. After initial_action() each subsequent tick
        // should step to the NEXT coarse point in declaration order,
        // marking each in `explored`. The initial idx (snapped from
        // initial_tenths) is intentionally NOT recorded on the first
        // tick (v0.6.0 placeholder-skip guard) so it does not
        // contaminate `best_gain_idx` before any real coarse probe
        // has run.
        //
        // Coarse points chosen to land on exact R820T table entries
        // so the index assertion is unambiguous:
        //   77  -> idx 5  (table[5]  = 77)
        //   254 -> idx 14 (table[14] = 254)
        //   372 -> idx 20 (table[20] = 372)
        let cfg = AgcConfig {
            coarse_probe_tenths: &[77, 254, 372],
            ..cfg_fast()
        };
        let mut agc = AgcController::new(R820T_GAINS_TENTHS, cfg);
        assert_eq!(agc.snapshot().phase, SearchPhase::Coarse);
        let _initial = agc.initial_action();

        // Drive with mediocre MER so target-hit doesn't fire and bail
        // doesn't fire. Each tick should jump to the next coarse point.
        let expected_idx = [5usize, 14, 20];
        for &want_idx in &expected_idx {
            let action = drive(&mut agc, 4.0).expect("expected a coarse-step action");
            assert_eq!(
                action.new_idx, want_idx,
                "coarse probe order broken; phase = {:?}",
                agc.snapshot().phase
            );
        }
        // After the last coarse point is probed, the next tick should
        // transition to Fine (and either return-to-best or fall
        // through into a Fine step depending on whether gain_idx
        // already equals best_gain_idx).
        let _next = drive(&mut agc, 4.0);
        assert!(
            agc.snapshot().phase == SearchPhase::Fine
                || agc.snapshot().phase == SearchPhase::Done,
            "expected Fine or Done after coarse exhausted, got {:?}",
            agc.snapshot().phase
        );
    }

    #[test]
    fn coarse_winner_becomes_fine_centre() {
        // Three coarse points; the middle one produces the best MER.
        // After coarse exhausts, the controller must recenter on the
        // winner (idx 14 in this setup) before the Fine ±1 walk.
        let cfg = AgcConfig {
            coarse_probe_tenths: &[77, 254, 372],
            mer_target_db: 99.0, // never hit the target shortcut
            ..cfg_fast()
        };
        let mut agc = AgcController::new(R820T_GAINS_TENTHS, cfg);
        let _ = agc.initial_action();

        // Each `drive()` call FIRST records an observation at the
        // current gain (set by the previous tick), THEN steps to the
        // next coarse point. The v0.6.0 "skip placeholder record"
        // guard drops the first record entirely (the initial gain
        // was not a deliberate probe), so the recorded sequence is:
        //   drive 1 records nothing,         steps -> idx 5
        //   drive 2 records idx 5,           steps -> idx 14
        //   drive 3 records idx 14,          steps -> idx 20  <- winner observed here
        //   drive 4 records idx 20,          coarse done -> recenter to 14
        let _ = drive(&mut agc, 2.0); // placeholder reading discarded
        let _ = drive(&mut agc, 2.0); // records idx 5 (poor)
        let _ = drive(&mut agc, 9.5); // records idx 14 (GREAT -> winner)
        assert_eq!(agc.snapshot().best_idx, 14, "winner should be idx 14");

        // Drive 4: records idx 20 (mediocre), then coarse done -> recenter to best.
        let next = drive(&mut agc, 4.0);
        if let Some(action) = next {
            // Either we recentred to best, or Fine already probed
            // best ± 1. Both are acceptable; just confirm we're not
            // probing a totally unrelated coarse point.
            assert!(
                action.new_idx == 14 || action.new_idx == 13 || action.new_idx == 15,
                "post-coarse step should be at or adjacent to best idx 14, got {}",
                action.new_idx
            );
        }
        // Either way we should now be in Fine (or Done if the Fine
        // path immediately settled).
        let phase = agc.snapshot().phase;
        assert!(
            phase == SearchPhase::Fine || phase == SearchPhase::Done,
            "expected Fine or Done after coarse handoff, got {:?}",
            phase
        );
    }

    #[test]
    fn placeholder_initial_reading_does_not_anchor_best() {
        // v0.6.0 regression guard for SDRplay 97.1 MHz wedge: the
        // initial gain produced a mediocre-but-finite MER (~4 dB)
        // and every coarse probe came back worse. Pre-fix, the
        // placeholder reading anchored `best_gain_idx` at the initial
        // index and Fine subsequently bracketed and bailed because
        // both ±1 neighbours had been marked "explored" by the
        // coarse pass. Post-fix, the initial reading is discarded
        // and the best coarse probe wins outright.
        //
        // Setup: 3-point coarse table; initial gain at idx 11 sees
        // EMA 4.0 (the "placeholder mediocre" reading); every coarse
        // probe sees EMA 1.0 (worse). Expectation: best_idx winds up
        // at one of the coarse points (5, 14, or 20), NOT at the
        // initial idx 11.
        let cfg = AgcConfig {
            coarse_probe_tenths: &[77, 254, 372],
            mer_target_db: 99.0, // never hit target shortcut
            ..cfg_fast()
        };
        let mut agc = AgcController::new(R820T_GAINS_TENTHS, cfg);
        let _ = agc.initial_action();

        // drive 1: would have recorded initial idx 11 EMA 4.0 (pre-fix bug).
        //          post-fix: skip record, advance coarse to idx 5.
        let _ = drive(&mut agc, 4.0);
        // drive 2..4: each coarse probe gets EMA 1.0 — worse than the
        // initial 4.0. Pre-fix, best stayed at idx 11. Post-fix,
        // idx 5 becomes best on drive 2 (first real measurement).
        for _ in 0..3 {
            let _ = drive(&mut agc, 1.0);
        }
        let snap = agc.snapshot();
        assert_ne!(
            snap.best_idx, 11,
            "placeholder initial idx 11 must not be the winner after coarse \
             sweep (pre-fix regression: best_idx == 11)",
        );
        assert!(
            matches!(snap.best_idx, 5 | 14 | 20),
            "expected best_idx at a coarse point (5/14/20), got {}",
            snap.best_idx
        );
    }

    #[test]
    fn empty_coarse_set_starts_in_fine() {
        // Default cfg_fast() has an empty coarse set — controller
        // should start directly in Fine, matching legacy v0.3.x
        // behavior. The walks_down_on_over_clipping test already
        // depends on this implicitly; this test asserts it directly.
        let agc = AgcController::new(R820T_GAINS_TENTHS, cfg_fast());
        assert_eq!(agc.snapshot().phase, SearchPhase::Fine);
    }

    // --- Phase 2b: Adaptive probe timing ----------------------------

    #[test]
    fn waits_for_min_mer_samples_before_judging() {
        // Production-default sample count: 4. With probe_period long
        // enough that the soft ceiling doesn't fire, tick() must
        // return None until exactly 4 MER events have been observed.
        let cfg = AgcConfig {
            min_mer_samples_post_change: 4,
            probe_period: Duration::from_secs(60), // soft ceiling unreachable in test
            ..cfg_fast()
        };
        let mut agc = AgcController::new(R820T_GAINS_TENTHS, cfg);
        let _ = agc.initial_action();

        // 0 samples → no decision yet.
        assert!(agc.tick().is_none(), "tick fired with 0 samples");

        // 3 samples → still not enough.
        for _ in 0..3 {
            agc.on_event(&NrscEvent::Mer { lower: 8.0, upper: 8.0 });
        }
        assert!(agc.tick().is_none(), "tick fired with only 3 samples");

        // 4th sample → gate opens, tick can now decide (and will, since
        // EMA ≈ 8.0 is below cfg_fast's 10 dB target — so it steps).
        agc.on_event(&NrscEvent::Mer { lower: 8.0, upper: 8.0 });
        // 4 samples is enough; the controller has a real EMA reading
        // and will either step or settle. Either way it's not None.
        let snap_before = agc.snapshot();
        let _ = agc.tick();
        let snap_after = agc.snapshot();
        assert!(
            snap_after.probes_done > snap_before.probes_done
                || snap_after.status != AgcStatus::Probing,
            "tick made no decision after 4 samples"
        );
    }

    #[test]
    fn soft_ceiling_fires_without_samples() {
        // No MER events → samples_ok stays false forever. The soft
        // ceiling (probe_period) must still let tick() fire so the
        // controller doesn't deadlock on a dead radio. We can't
        // easily wait the real ceiling in unit-test time; instead we
        // set probe_period to 0 (cfg_fast default) so timeout_ok is
        // immediately true even with samples_ok false, which is the
        // semantic check.
        let cfg = AgcConfig {
            min_mer_samples_post_change: 10, // unreachable in this test
            probe_period: Duration::from_millis(0),
            ..cfg_fast()
        };
        let mut agc = AgcController::new(R820T_GAINS_TENTHS, cfg);
        let _ = agc.initial_action();
        // No MER events fed. Tick should still decide because the
        // soft ceiling has elapsed (immediately, with 0 ms).
        let _ = agc.tick();
        assert!(
            agc.snapshot().probes_done >= 1,
            "soft ceiling did not fire with 0 ms probe_period"
        );
    }

    // --- Phase 3: cache-hit trust-but-verify ------------------------

    #[test]
    fn cache_hit_skips_coarse_and_starts_in_fine() {
        // Even when the profile has a coarse set, a cache hit
        // (`seeded_from_cache=true`) must bypass Coarse entirely and
        // begin in Fine — that's the whole point of the cache: avoid
        // re-walking the table for a station we already know.
        let cfg = AgcConfig {
            // Non-empty coarse set; would normally force Coarse phase.
            coarse_probe_tenths: &[77, 254, 372],
            seeded_from_cache: true,
            // Cached gain at idx 14 (254 tenths) is what the caller
            // would have overridden initial_tenths to.
            initial_tenths: 254,
            ..cfg_fast()
        };
        let agc = AgcController::new(R820T_GAINS_TENTHS, cfg);
        let snap = agc.snapshot();
        assert_eq!(
            snap.phase,
            SearchPhase::Fine,
            "cache hit must start directly in Fine, not Coarse"
        );
        assert!(
            snap.from_cache,
            "cache-hit snapshot must report from_cache=true"
        );
    }

    #[test]
    fn cache_miss_default_does_not_report_from_cache() {
        // A vanilla controller (cfg_fast, no cache flag) must report
        // from_cache=false so the UI suffix logic is correct on misses.
        let agc = AgcController::new(R820T_GAINS_TENTHS, cfg_fast());
        let snap = agc.snapshot();
        assert!(
            !snap.from_cache,
            "default cfg must report from_cache=false"
        );
    }

    #[test]
    fn cache_hit_with_good_mer_settles_at_lower_threshold() {
        // Trust-but-verify simulation: cached station had MER ~14 dB
        // (a marginal station). Caller sets `mer_target_db = 14-3 = 11`.
        // Even at idx 14, feed in 12 dB MER — the controller should
        // settle on the early-target shortcut without walking
        // neighbours, because 12 ≥ 11.
        let cfg = AgcConfig {
            seeded_from_cache: true,
            initial_tenths: 254, // table[14]
            mer_target_db: 11.0, // 14 - 3 = trust-but-verify floor
            ..cfg_fast()
        };
        let mut agc = AgcController::new(R820T_GAINS_TENTHS, cfg);
        let _ = agc.initial_action();
        let _ = drive(&mut agc, 12.0);
        let snap = agc.snapshot();
        assert_eq!(snap.status, AgcStatus::Settled);
        assert!(snap.from_cache, "still from_cache after SETTLED");
        assert_eq!(snap.best_idx, 14, "settled at the cached idx");
    }

    // === v0.6.0 amplitude-pre-stage tests ============================
    //
    // These exercise `tick_amp` directly: the algorithm is a plain
    // binary search over `agc_tenths_table` indices, biased to the
    // high (loud) side, terminating when the window collapses. The
    // driver thread will plumb real `rms_dbfs` readings from the
    // IqBus; these tests fake them so the search is deterministic.

    /// Returns an `AgcConfig` with `amp_enable = true` and the other
    /// timing knobs set to zero so each `tick_amp` returns a decision
    /// immediately. `bail_after_changes` is high enough that 5+ probes
    /// don't trip it.
    fn cfg_amp() -> AgcConfig {
        AgcConfig {
            amp_enable: true,
            amp_target_dbfs: -20.0,
            ..cfg_fast()
        }
    }

    #[test]
    fn amp_probe_enters_when_enabled() {
        let agc = AgcController::new(R820T_GAINS_TENTHS, cfg_amp());
        // Cold-start with amp_enable=true → controller MUST be in
        // AmpProbe phase. Regression guard for the constructor.
        assert_eq!(agc.snapshot().phase, SearchPhase::AmpProbe);
    }

    #[test]
    fn amp_probe_skipped_when_disabled() {
        // amp_enable=false (the cfg_fast default) → legacy phase
        // entry. Empty coarse table = Fine; non-empty = Coarse.
        let agc = AgcController::new(R820T_GAINS_TENTHS, cfg_fast());
        assert_eq!(agc.snapshot().phase, SearchPhase::Fine);
    }

    #[test]
    fn amp_probe_skipped_when_seeded_from_cache() {
        // Cache hit beats amplitude pre-stage: the Phase 3 fast path
        // must still skip straight to Fine regardless of amp_enable.
        let cfg = AgcConfig {
            seeded_from_cache: true,
            ..cfg_amp()
        };
        let agc = AgcController::new(R820T_GAINS_TENTHS, cfg);
        assert_eq!(agc.snapshot().phase, SearchPhase::Fine);
    }

    #[test]
    fn amp_probe_first_call_picks_mid_high() {
        let mut agc = AgcController::new(R820T_GAINS_TENTHS, cfg_amp());
        let _ = agc.initial_action();
        // First call: peak=None (no measurement yet). Expect the
        // controller to seed the binary search at the upper mid:
        // mid = 0 + (28-0+1)/2 = 14.
        let action = agc
            .tick_amp(None)
            .expect("first amp probe should emit an action");
        assert_eq!(action.new_idx, 14);
        assert_eq!(action.new_tenths, R820T_GAINS_TENTHS[14]);
        assert_eq!(agc.snapshot().phase, SearchPhase::AmpProbe);
    }

    #[test]
    fn amp_probe_binary_search_converges_high_safe_gain() {
        // Imagine a station where indices 0..=18 are safe (peak ≤
        // -6 dBFS) and 19..=28 clip (peak > -6 dBFS). The binary
        // search should converge on idx 18 in O(log N) probes (≤ 5
        // for the 29-step table) and transition to MerQualityCheck.
        let mut agc = AgcController::new(R820T_GAINS_TENTHS, cfg_amp());
        let _ = agc.initial_action();
        let mut last_idx = None;
        for probe_num in 0..10 {
            let measurement = last_idx.map(|idx: usize| {
                if idx <= 18 { -25.0 } else { -10.0 } // -25 = safe, -10 = hot (target -20)
            });
            let action = agc.tick_amp(measurement);
            if agc.snapshot().phase == SearchPhase::MerQualityCheck {
                // Final action (if any) must land at the safe winner.
                if let Some(a) = action {
                    assert_eq!(
                        a.new_idx, 18,
                        "expected winner=18, got {} after {} probes",
                        a.new_idx, probe_num
                    );
                }
                assert_eq!(agc.snapshot().current_idx, 18);
                return;
            }
            last_idx = action.map(|a| a.new_idx);
        }
        panic!(
            "amp probe failed to converge in 10 iterations, phase = {:?}",
            agc.snapshot().phase
        );
    }

    #[test]
    fn amp_probe_window_collapses_at_index_zero() {
        // Pathological "everything clips" station: every probe
        // returns "too hot" and `amp_lo` never advances off idx 0.
        // v0.6.0 behaviour: instead of committing the never-confirmed
        // idx 0 to MerQualityCheck (which used to bail at the table
        // edge with a bogus seed), the controller MUST abort the amp
        // pre-stage and hand off to the legacy Coarse/Fine search
        // seeded from `initial_tenths`. `cfg_amp` inherits `cfg_fast`'s
        // empty coarse table, so the fallback target phase is Fine.
        let mut agc = AgcController::new(R820T_GAINS_TENTHS, cfg_amp());
        let _ = agc.initial_action();
        let seed_idx = nearest_idx(R820T_GAINS_TENTHS, 197);
        let mut last_idx = None;
        for _ in 0..20 {
            let measurement = last_idx.map(|_: usize| -5.0); // always hot vs -20 dBFS target
            let _ = agc.tick_amp(measurement);
            if agc.snapshot().phase == SearchPhase::Fine {
                assert_eq!(
                    agc.snapshot().current_idx,
                    seed_idx,
                    "amp abort must reset to initial gain, not park at the floor"
                );
                return;
            }
            last_idx = Some(agc.snapshot().current_idx);
        }
        panic!("amp probe failed to abort on the always-hot station");
    }

    #[test]
    fn amp_probe_handoff_settles_when_mer_meets_target() {
        // Convergence into MerQualityCheck, then good MER → Settled.
        let mut agc = AgcController::new(R820T_GAINS_TENTHS, cfg_amp());
        let _ = agc.initial_action();
        // One probe + "safe" report → window expands; second probe at
        // top of table is safe too → converges to idx 28.
        for _ in 0..10 {
            let measurement = Some(-25.0); // always safe vs -20 dBFS target
            let _ = agc.tick_amp(measurement);
            if agc.snapshot().phase == SearchPhase::MerQualityCheck {
                break;
            }
        }
        assert_eq!(agc.snapshot().phase, SearchPhase::MerQualityCheck);
        // Now feed good MER. `drive()` pumps 5 events which is enough
        // to clear `mer_check_remaining=3` and trigger settle.
        let _ = drive(&mut agc, 14.0);
        assert_eq!(agc.snapshot().status, AgcStatus::Settled);
        assert_eq!(agc.snapshot().phase, SearchPhase::Done);
    }

    #[test]
    fn amp_probe_handoff_falls_through_to_fine_when_mer_bad() {
        // Convergence into MerQualityCheck, then bad MER → Fine.
        let mut agc = AgcController::new(R820T_GAINS_TENTHS, cfg_amp());
        let _ = agc.initial_action();
        for _ in 0..10 {
            let _ = agc.tick_amp(Some(-25.0));
            if agc.snapshot().phase == SearchPhase::MerQualityCheck {
                break;
            }
        }
        // Feed bad MER → controller should hand off to Fine and
        // continue probing instead of settling.
        let _ = drive(&mut agc, 2.0);
        assert!(
            matches!(agc.snapshot().phase, SearchPhase::Fine | SearchPhase::Done),
            "expected Fine handoff (or Done bail), got {:?}",
            agc.snapshot().phase
        );
        // Must still be in Probing (not Settled) — the amp winner
        // wasn't good enough on its own.
        assert_ne!(
            agc.snapshot().status,
            AgcStatus::Settled,
            "amp+mer should not settle on bad MER"
        );
    }

    #[test]
    fn tick_amp_is_no_op_outside_amp_phase() {
        // Defensive: tick_amp called while phase is Fine (cache hit
        // path) MUST do nothing, not crash or stomp gain.
        let cfg = AgcConfig {
            seeded_from_cache: true,
            ..cfg_amp()
        };
        let mut agc = AgcController::new(R820T_GAINS_TENTHS, cfg);
        let _ = agc.initial_action();
        let before = agc.snapshot().current_idx;
        let action = agc.tick_amp(Some(-3.0));
        assert!(action.is_none(), "tick_amp must no-op outside AmpProbe");
        assert_eq!(agc.snapshot().current_idx, before);
    }
}
