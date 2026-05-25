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
//! Explored-set hill-climber on the 29-step R820T2 gain table, driven by
//! an EMA of `min(MER_lower, MER_upper)`. Starts by stepping DOWN (the
//! direction that can't lose sync on a working signal). Each gain index
//! is probed at most once per AGC run — guaranteeing forward progress
//! and eliminating the oscillation failure mode of naive hill-climbers.
//! Declares SETTLED when either (a) EMA ≥ target dB, or (b) the
//! best-known gain has both neighbours probed and best_mer ≥ 6 dB.
//! BAILS after `bail_after_changes` non-improving probes, restoring the
//! best-known gain first.
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

/// Read-only view of controller state, safe to clone into UI threads.
#[derive(Debug, Clone)]
pub struct AgcSnapshot {
    pub status: AgcStatus,
    pub current_idx: usize,
    pub current_tenths: i32,
    pub best_idx: usize,
    pub best_mer: Option<f32>,
    pub probes_done: u32,
    pub last_change_at: Instant,
    pub last_reason: String,
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
    /// SETTLED immediately. Default 10.0 dB matches Spike 2.
    pub mer_target_db: f32,
    /// Minimum gap between gain changes. Lets the EMA reflect the new
    /// gain before we judge it. Default 5000 ms; below ~4000 ms feels
    /// rushed against the librtlsdr ring buffer's 1.25 s of latency.
    pub probe_period: Duration,
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
}

impl Default for AgcConfig {
    fn default() -> Self {
        Self {
            mer_target_db: 10.0,
            probe_period: Duration::from_millis(5000),
            bail_after_changes: 15,
            initial_tenths: 197,
            initial_direction: -1,
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

    /// Have we ever observed a `Sync` event? Used to differentiate
    /// "MER reading from real lock" vs "garbage from no lock" if we
    /// ever need to in the future. Currently informational.
    has_ever_synced: bool,

    status: AgcStatus,
    probes_done: u32,
    last_reason: String,

    /// Indices we've already probed. NEVER REVISITED — that's the
    /// non-oscillation guarantee. Sentinel value `f32::NEG_INFINITY`
    /// means "probed but no MER reading available" (no sync at that
    /// gain). Real EMA values are recorded for the stability shortcut.
    explored: BTreeMap<usize, f32>,
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
            has_ever_synced: false,
            status: AgcStatus::Probing,
            probes_done: 0,
            last_reason: "initial start gain".to_string(),
            explored: BTreeMap::new(),
        }
    }

    /// Initial gain to apply before any events flow. The driver should
    /// call this once at AGC startup and apply the returned tenths via
    /// `Sdr::set_tuner_gain_tenths`. Subsequent gains come from
    /// [`Self::tick`].
    pub fn initial_action(&mut self) -> AgcAction {
        let tenths = self.table[self.gain_idx];
        self.last_change_at = Instant::now();
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
            current_idx: self.gain_idx,
            current_tenths: self.table[self.gain_idx],
            best_idx: self.best_gain_idx,
            best_mer: if self.best_mer_seen.is_finite() {
                Some(self.best_mer_seen)
            } else {
                None
            },
            probes_done: self.probes_done,
            last_change_at: self.last_change_at,
            last_reason: self.last_reason.clone(),
        }
    }

    /// Feed the controller a single nrsc5 event. Cheap; safe to call
    /// from the stderr-parser thread for every event in the stream.
    pub fn on_event(&mut self, ev: &NrscEvent) {
        match ev {
            NrscEvent::Sync => {
                self.has_ever_synced = true;
            }
            NrscEvent::Mer { lower, upper } => {
                let m = lower.min(*upper);
                self.ema_mer_min = Some(match self.ema_mer_min {
                    Some(prev) => 0.6 * prev + 0.4 * m,
                    None => m,
                });
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
    /// Safe to call at any cadence — internally throttled by
    /// `cfg.probe_period`. Idempotent once `status` is `Settled` or
    /// `Bailed`.
    pub fn tick(&mut self) -> Option<AgcAction> {
        if self.status != AgcStatus::Probing {
            return None;
        }
        if self.last_change_at.elapsed() < self.cfg.probe_period {
            return None; // settle hold — let EMA reflect the current gain
        }

        self.probes_done += 1;

        // -- 1. Record what we observed at the current gain. -------------
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
                self.explored
                    .entry(self.gain_idx)
                    .or_insert(f32::NEG_INFINITY);
                self.probes_without_improvement += 1;
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
                return None;
            }
        }

        // -- 3. Bail-out budget exhausted? ----------------------------
        if self.probes_without_improvement >= self.cfg.bail_after_changes {
            // Restore best-known gain before bailing (even if best is
            // garbage — leave the radio in the least-bad state).
            self.status = AgcStatus::Bailed;
            if self.best_gain_idx != self.gain_idx {
                let new_idx = self.best_gain_idx;
                let new_tenths = self.table[new_idx];
                self.gain_idx = new_idx;
                self.last_change_at = Instant::now();
                self.ema_mer_min = None;
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

        // -- 4. Pick direction. ---------------------------------------
        //
        // If the latest probe was the best, keep walking the same way.
        // If it was strictly worse, flip. If MER vanished entirely AND
        // we previously had a working gain, that's a strong "we just
        // walked off the cliff" signal (typical when SDRplay or RTL-SDR
        // hits front-end overload at high gain -- sync drops, no MER
        // events arrive for the full probe window, current_ema stays
        // None). Treat it the same as a strictly-worse probe and flip
        // back toward the known-good gain. Without this, the controller
        // would keep stepping in the original direction and walk
        // arbitrarily deep into overload before the bail budget
        // eventually rescues it. The "no MER yet, no best either"
        // case (first probes on a weak station) still keeps the
        // current direction so we can scan looking for any lock.
        let preferred_dir = match current_ema {
            Some(e) if (e - self.best_mer_seen).abs() < 0.01 => self.last_dir,
            Some(_) => -self.last_dir,
            None if self.best_mer_seen.is_finite() => -self.last_dir,
            None => self.last_dir,
        };

        // -- 5. Find next unexplored idx in preferred dir, then other. -
        let (next_idx, chosen_dir) = match self.next_unexplored(preferred_dir) {
            Some(idx) => (idx, preferred_dir),
            None => match self.next_unexplored(-preferred_dir) {
                Some(idx) => (idx, -preferred_dir),
                None => {
                    // Entire reachable table explored.
                    let usable = self.best_mer_seen >= 6.0;
                    if self.best_gain_idx != self.gain_idx {
                        let new_idx = self.best_gain_idx;
                        let new_tenths = self.table[new_idx];
                        self.gain_idx = new_idx;
                        self.last_change_at = Instant::now();
                        self.ema_mer_min = None;
                        self.status = if usable {
                            AgcStatus::Settled
                        } else {
                            AgcStatus::Bailed
                        };
                        self.last_reason = format!(
                            "all explored -- {} at best (ema {:.2})",
                            if usable { "settled" } else { "bailing" },
                            self.best_mer_seen
                        );
                        return Some(AgcAction {
                            new_idx,
                            new_tenths,
                            reason: self.last_reason.clone(),
                        });
                    }
                    self.status = if usable {
                        AgcStatus::Settled
                    } else {
                        AgcStatus::Bailed
                    };
                    self.last_reason = "all explored, already at best".to_string();
                    return None;
                }
            },
        };
        self.last_dir = chosen_dir;

        // -- 6. Stability shortcut. -----------------------------------
        // If we've already probed both neighbours of best_gain_idx and
        // best_mer_seen is decent, return to best and settle. Avoids
        // wandering past the known optimum.
        if self.probes_done >= 4 && self.best_mer_seen >= 6.0 {
            let bi = self.best_gain_idx;
            let max_i = self.table.len() - 1;
            let left_done = bi == 0 || self.explored.contains_key(&(bi - 1));
            let right_done = bi == max_i || self.explored.contains_key(&(bi + 1));
            if left_done && right_done {
                self.status = AgcStatus::Settled;
                if self.gain_idx != bi {
                    let new_tenths = self.table[bi];
                    self.gain_idx = bi;
                    self.last_change_at = Instant::now();
                    self.ema_mer_min = None;
                    self.last_reason = format!(
                        "stability: both neighbours of best gain probed (ema {:.2})",
                        self.best_mer_seen
                    );
                    return Some(AgcAction {
                        new_idx: bi,
                        new_tenths,
                        reason: self.last_reason.clone(),
                    });
                }
                self.last_reason = format!(
                    "stability: at best, neighbours probed (ema {:.2})",
                    self.best_mer_seen
                );
                return None;
            }
        }

        // -- 7. Probe next gain. --------------------------------------
        let new_tenths = self.table[next_idx];
        self.gain_idx = next_idx;
        self.last_change_at = Instant::now();
        self.ema_mer_min = None;
        self.last_reason = match current_ema {
            Some(e) => format!(
                "ema {:.2} best {:.2} -> probing idx {} ({})",
                e,
                self.best_mer_seen,
                next_idx,
                if chosen_dir > 0 { "up" } else { "down" }
            ),
            None => format!(
                "no MER at this gain -> probing idx {} ({})",
                next_idx,
                if chosen_dir > 0 { "up" } else { "down" }
            ),
        };
        Some(AgcAction {
            new_idx: next_idx,
            new_tenths,
            reason: self.last_reason.clone(),
        })
    }

    /// First gain index in `dir` from `gain_idx` that is NOT in
    /// `explored`. Walking off the end returns `None`.
    fn next_unexplored(&self, dir: i32) -> Option<usize> {
        let n = self.table.len() as i32;
        let mut i = self.gain_idx as i32 + dir;
        while i >= 0 && i < n {
            if !self.explored.contains_key(&(i as usize)) {
                return Some(i as usize);
            }
            i += dir;
        }
        None
    }
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
            mer_target_db: 10.0,
            // Skip the time-based settle hold in tests by setting it to
            // zero — every tick() can immediately make a decision.
            probe_period: Duration::from_millis(0),
            bail_after_changes: 10,
            initial_tenths: 197,
            initial_direction: -1,
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
    }

    #[test]
    fn walks_down_on_over_clipping() {
        // KEGL-style: initial gain produces terrible MER, AGC must walk
        // down and converge on a lower gain.
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
}
