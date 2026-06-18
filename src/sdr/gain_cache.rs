//! Per-frequency gain cache.
//!
//! Records the gain at which AGC settled for a given `(freq, driver,
//! antenna, ppm)` tuple. On the next tune to the same tuple, AGC seeds
//! itself with the cached gain and runs a short trust-but-verify pass
//! instead of a fresh coarse-then-fine search — dropping warm-tune
//! convergence from ~10-15 s to ~3 s.
//!
//! ## Design
//!
//! - **Key includes antenna** so RSP Duo / RSPdx users with multiple
//!   inputs don't cross-contaminate gains across antennas. For
//!   single-antenna SDRs the field is always `None` (a stable identity)
//!   so the key is well-formed today even before the antenna selector
//!   (Phase 0 of the AGC overhaul plan) ships.
//! - **Key includes `ppm_x10`** (PPM × 10 rounded to nearest integer)
//!   so floating-point noise doesn't fragment the cache. ±0.05 ppm of
//!   jitter never crosses a bucket; deliberate PPM changes (e.g.
//!   recalibrating after temperature drift) correctly miss the cache.
//! - **TTL 7 days by default.** FM is line-of-sight stable so a fresh
//!   cache from a week ago is almost always still useful; longer than
//!   that risks stale entries surviving real antenna / station / hardware
//!   changes that the user has long forgotten.
//! - **Persisted as RON** with the same atomic `.tmp + rename` pattern
//!   used by [`crate::play_log`]. Failure to load yields an empty cache;
//!   failure to save is logged but non-fatal — losing the cache is
//!   strictly a performance regression, not a correctness bug.
//!
//! ## Schema version
//!
//! The on-disk format carries a `version: u32` field. Mismatched versions
//! are silently treated as a missing file and the cache starts empty —
//! same defensive behavior as the play log. Bump [`SCHEMA_VERSION`] when
//! making breaking changes to the on-disk format.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime};

/// On-disk schema version. Bump on breaking changes.
pub const SCHEMA_VERSION: u32 = 1;

/// Default cache TTL. Entries older than this are not returned by
/// [`GainCache::lookup`] and are eligible for cleanup on next save.
pub const DEFAULT_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// Cache lookup key. Uniquely identifies "the same tune" across
/// restarts. All fields are part of the identity — changing any one
/// (even PPM by 0.1) deliberately misses the cache.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GainCacheKey {
    /// Center frequency in Hz. Always rounded to the nearest integer
    /// at the call site — FM HD channels are spaced at 200 kHz so
    /// fractional-Hz precision is meaningless.
    pub freq_hz: u32,
    /// SoapySDR driver name (`"rtlsdr"`, `"sdrplay"`, `"hackrf"`, …).
    /// Same SDR family on different hosts shares cache entries; a
    /// different family with similar gain numbers does not.
    pub driver: String,
    /// Currently-selected antenna name, when the SDR has more than one.
    /// `None` for single-antenna devices (RTL-SDR, RSP1A). On RSP Duo
    /// / RSPdx this disambiguates `(97.1 MHz, Ant A)` from
    /// `(97.1 MHz, Ant B)`. Wired through from
    /// `Sdr::antenna()` once Phase 0 ships; until then always `None`.
    pub antenna: Option<String>,
    /// PPM correction × 10, rounded to nearest integer. ±0.05 ppm of
    /// jitter never crosses a bucket; deliberate PPM changes
    /// (recalibration, swapping dongles) miss the cache by design.
    pub ppm_x10: i32,
}

impl GainCacheKey {
    /// Build a key from raw inputs, normalizing PPM to the `ppm_x10`
    /// bucket. Floating-point PPM values from config are rounded once
    /// here rather than at every call site.
    pub fn new(
        freq_hz: u32,
        driver: impl Into<String>,
        antenna: Option<String>,
        ppm: f32,
    ) -> Self {
        Self {
            freq_hz,
            driver: driver.into(),
            antenna,
            ppm_x10: (ppm * 10.0).round() as i32,
        }
    }
}

/// What the cache remembers for one tune.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GainCacheEntry {
    /// Gain in tenths of a dB at which AGC settled.
    pub gain_tenths: i32,
    /// Best EMA of `min(MER_lower, MER_upper)` observed at the settled
    /// gain. Used by the trust-but-verify pass: the next tune declares
    /// SETTLED early when the live EMA reaches at least
    /// `best_mer_db - 3.0`, so a station with intrinsically marginal
    /// MER (e.g. 14 dB) doesn't get held to the production 18 dB target.
    pub best_mer_db: f32,
    /// Wall-clock recording time. Compared against [`GainCache::ttl`]
    /// to expire stale entries. Serialized as duration-since-UNIX_EPOCH
    /// for stability across timezones / clock changes.
    pub recorded_at: SystemTime,
}

/// On-disk format. Versioned so future schema migrations are clean.
/// Stored as a `Vec<(Key, Entry)>` rather than a serialized map because
/// RON can't natively serialize maps with struct keys; the load path
/// rebuilds the `HashMap` from the vec.
#[derive(Debug, Serialize, Deserialize)]
struct OnDiskFormat {
    version: u32,
    entries: Vec<(GainCacheKey, GainCacheEntry)>,
}

/// The cache. Cheap to clone (entries are small POD); typically wrapped
/// in `Arc<Mutex<_>>` and shared between the AGC driver thread (writer)
/// and the SDR start path (reader).
#[derive(Debug, Clone)]
pub struct GainCache {
    entries: HashMap<GainCacheKey, GainCacheEntry>,
    ttl: Duration,
}

impl Default for GainCache {
    fn default() -> Self {
        Self {
            entries: HashMap::new(),
            ttl: DEFAULT_TTL,
        }
    }
}

impl GainCache {
    /// Empty cache with the default TTL.
    pub fn new() -> Self {
        Self::default()
    }

    /// Empty cache with an explicit TTL. Mainly for tests.
    // Kept: explicit-TTL constructor for tests / future tuning; no
    // current caller.
    #[allow(dead_code)]
    pub fn with_ttl(ttl: Duration) -> Self {
        Self {
            entries: HashMap::new(),
            ttl,
        }
    }

    /// Load from `path`. Missing / unreadable / malformed / version-mismatched
    /// files yield an empty cache — failure is always non-fatal because
    /// a missing cache only costs a fresh coarse search, never breaks
    /// playback.
    pub fn load(path: &Path) -> Self {
        let mut cache = Self::default();
        let Ok(raw) = fs::read_to_string(path) else {
            return cache;
        };
        let Ok(parsed) = ron::from_str::<OnDiskFormat>(&raw) else {
            return cache;
        };
        if parsed.version != SCHEMA_VERSION {
            return cache;
        }
        for (key, entry) in parsed.entries {
            // Drop already-expired entries at load time so the cache
            // doesn't carry dead weight in memory.
            if Self::is_entry_fresh(&entry, cache.ttl) {
                cache.entries.insert(key, entry);
            }
        }
        cache
    }

    /// Persist atomically (`.tmp` + rename). Failure is non-fatal — the
    /// cache keeps working in memory. The parent directory is created
    /// on demand to match [`crate::play_log::PlayLog::save`].
    pub fn save(&self, path: &Path) {
        let Some(parent) = path.parent() else { return };
        if fs::create_dir_all(parent).is_err() {
            return;
        }
        let payload = OnDiskFormat {
            version: SCHEMA_VERSION,
            entries: self
                .entries
                .iter()
                .filter(|(_, e)| Self::is_entry_fresh(e, self.ttl))
                .map(|(k, e)| (k.clone(), e.clone()))
                .collect(),
        };
        let Ok(serialized) = ron::ser::to_string_pretty(
            &payload,
            ron::ser::PrettyConfig::default().compact_arrays(true),
        ) else {
            return;
        };
        let tmp = path.with_extension("ron.tmp");
        if fs::write(&tmp, serialized).is_err() {
            return;
        }
        let _ = fs::rename(&tmp, path);
    }

    /// Look up a key. Returns `None` if absent or expired.
    pub fn lookup(&self, key: &GainCacheKey) -> Option<&GainCacheEntry> {
        let entry = self.entries.get(key)?;
        if Self::is_entry_fresh(entry, self.ttl) {
            Some(entry)
        } else {
            None
        }
    }

    /// Insert or overwrite. The caller is responsible for persisting.
    pub fn record(&mut self, key: GainCacheKey, entry: GainCacheEntry) {
        self.entries.insert(key, entry);
    }

    /// Drop every entry. Caller persists.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Number of (fresh and stale) entries currently in memory.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Convenience for empty-check.
    // Kept: standard `is_empty` companion to `len`; no current caller.
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn is_entry_fresh(entry: &GainCacheEntry, ttl: Duration) -> bool {
        match SystemTime::now().duration_since(entry.recorded_at) {
            Ok(age) => age <= ttl,
            // `recorded_at` is in the future (clock skew) — treat as fresh.
            Err(_) => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn entry_now(gain_tenths: i32, mer: f32) -> GainCacheEntry {
        GainCacheEntry {
            gain_tenths,
            best_mer_db: mer,
            recorded_at: SystemTime::now(),
        }
    }

    fn entry_at(gain_tenths: i32, mer: f32, age: Duration) -> GainCacheEntry {
        GainCacheEntry {
            gain_tenths,
            best_mer_db: mer,
            recorded_at: SystemTime::now() - age,
        }
    }

    #[test]
    fn key_ppm_quantization_buckets_correctly() {
        // ±0.04 ppm jitter rounds to the same bucket; +0.10 ppm
        // crosses a bucket.
        let k0 = GainCacheKey::new(97_100_000, "rtlsdr", None, 1.04);
        let k1 = GainCacheKey::new(97_100_000, "rtlsdr", None, 1.00);
        let k2 = GainCacheKey::new(97_100_000, "rtlsdr", None, 1.10);
        assert_eq!(k0, k1, "1.04 ppm should bucket with 1.00 ppm");
        assert_ne!(k0, k2, "1.10 ppm should be a different bucket from 1.04");
    }

    #[test]
    fn key_includes_antenna_disambiguator() {
        // RSP Duo Ant A vs Ant B at the same frequency are distinct
        // entries — they cannot share a gain optimum.
        let ant_a = GainCacheKey::new(
            97_100_000,
            "sdrplay",
            Some("Tuner 1 50ohm".into()),
            0.0,
        );
        let ant_b = GainCacheKey::new(
            97_100_000,
            "sdrplay",
            Some("Tuner 2 50ohm".into()),
            0.0,
        );
        assert_ne!(ant_a, ant_b);
        // Single-antenna (None) is also distinct from either.
        let no_ant = GainCacheKey::new(97_100_000, "sdrplay", None, 0.0);
        assert_ne!(ant_a, no_ant);
    }

    #[test]
    fn lookup_returns_recorded_entry() {
        let mut cache = GainCache::new();
        let key = GainCacheKey::new(97_100_000, "rtlsdr", None, 0.0);
        cache.record(key.clone(), entry_now(254, 22.5));
        let hit = cache.lookup(&key).expect("just-recorded entry should hit");
        assert_eq!(hit.gain_tenths, 254);
        assert!((hit.best_mer_db - 22.5).abs() < 1e-6);
    }

    #[test]
    fn lookup_misses_unrecorded_key() {
        let cache = GainCache::new();
        let key = GainCacheKey::new(97_100_000, "rtlsdr", None, 0.0);
        assert!(cache.lookup(&key).is_none());
    }

    #[test]
    fn ttl_expires_old_entries() {
        // TTL of 1 hour; entry recorded 2 hours ago is dead on lookup.
        let mut cache = GainCache::with_ttl(Duration::from_secs(3600));
        let key = GainCacheKey::new(97_100_000, "rtlsdr", None, 0.0);
        cache.record(
            key.clone(),
            entry_at(254, 22.5, Duration::from_secs(7200)),
        );
        assert!(
            cache.lookup(&key).is_none(),
            "2-hour-old entry should be expired under 1-hour TTL"
        );
    }

    #[test]
    fn fresh_entry_inside_ttl_returns() {
        // Same setup as above but the entry is well inside the window.
        let mut cache = GainCache::with_ttl(Duration::from_secs(3600));
        let key = GainCacheKey::new(97_100_000, "rtlsdr", None, 0.0);
        cache.record(
            key.clone(),
            entry_at(254, 22.5, Duration::from_secs(600)),
        );
        assert!(cache.lookup(&key).is_some());
    }

    #[test]
    fn record_overwrites_existing_key() {
        let mut cache = GainCache::new();
        let key = GainCacheKey::new(97_100_000, "rtlsdr", None, 0.0);
        cache.record(key.clone(), entry_now(200, 18.0));
        cache.record(key.clone(), entry_now(280, 22.0));
        let hit = cache.lookup(&key).unwrap();
        assert_eq!(hit.gain_tenths, 280, "later record should overwrite");
        assert!((hit.best_mer_db - 22.0).abs() < 1e-6);
    }

    #[test]
    fn clear_drops_everything() {
        let mut cache = GainCache::new();
        cache.record(
            GainCacheKey::new(97_100_000, "rtlsdr", None, 0.0),
            entry_now(254, 22.5),
        );
        cache.record(
            GainCacheKey::new(93_300_000, "rtlsdr", None, 0.0),
            entry_now(197, 14.0),
        );
        assert_eq!(cache.len(), 2);
        cache.clear();
        assert_eq!(cache.len(), 0);
        assert!(cache.is_empty());
    }

    #[test]
    fn save_load_round_trips_entries() {
        let tmpdir = std::env::temp_dir().join(format!(
            "nrsc5-gain-cache-test-{}",
            std::process::id()
        ));
        let _ = fs::create_dir_all(&tmpdir);
        let path = tmpdir.join("gain-cache.ron");

        let mut cache = GainCache::new();
        let key_a = GainCacheKey::new(97_100_000, "rtlsdr", None, 0.0);
        let key_b = GainCacheKey::new(
            93_300_000,
            "sdrplay",
            Some("Tuner 1 50ohm".into()),
            -1.2,
        );
        cache.record(key_a.clone(), entry_now(254, 22.5));
        cache.record(key_b.clone(), entry_now(380, 18.7));
        cache.save(&path);

        let reloaded = GainCache::load(&path);
        let hit_a = reloaded.lookup(&key_a).expect("key A should reload");
        let hit_b = reloaded.lookup(&key_b).expect("key B should reload");
        assert_eq!(hit_a.gain_tenths, 254);
        assert_eq!(hit_b.gain_tenths, 380);
        assert_eq!(hit_b.best_mer_db, 18.7);

        let _ = fs::remove_dir_all(&tmpdir);
    }

    #[test]
    fn load_missing_file_yields_empty_cache() {
        let path = std::env::temp_dir().join(format!(
            "nrsc5-nonexistent-{}-cache.ron",
            std::process::id()
        ));
        let _ = fs::remove_file(&path); // ensure missing
        let cache = GainCache::load(&path);
        assert!(cache.is_empty());
    }

    #[test]
    fn load_malformed_file_yields_empty_cache() {
        let tmpdir = std::env::temp_dir().join(format!(
            "nrsc5-malformed-test-{}",
            std::process::id()
        ));
        let _ = fs::create_dir_all(&tmpdir);
        let path = tmpdir.join("gain-cache.ron");
        fs::write(&path, "this is not valid RON syntax @!#$").unwrap();
        let cache = GainCache::load(&path);
        assert!(
            cache.is_empty(),
            "malformed file must produce an empty cache, not panic"
        );
        let _ = fs::remove_dir_all(&tmpdir);
    }

    #[test]
    fn load_wrong_version_yields_empty_cache() {
        let tmpdir = std::env::temp_dir().join(format!(
            "nrsc5-wrongver-test-{}",
            std::process::id()
        ));
        let _ = fs::create_dir_all(&tmpdir);
        let path = tmpdir.join("gain-cache.ron");
        // Hand-craft an OnDiskFormat with a bogus version.
        let bogus = OnDiskFormat {
            version: 9999,
            entries: vec![(
                GainCacheKey::new(97_100_000, "rtlsdr", None, 0.0),
                entry_now(254, 22.5),
            )],
        };
        fs::write(
            &path,
            ron::ser::to_string(&bogus).expect("ser ok"),
        )
        .unwrap();
        let cache = GainCache::load(&path);
        assert!(
            cache.is_empty(),
            "future schema version must be silently ignored"
        );
        let _ = fs::remove_dir_all(&tmpdir);
    }
}
