//! Rolling song log with user-configurable retention window.
//!
//! Records every observed `(title, artist)` play on a station with a
//! wall-clock timestamp. Survives restarts via a RON file under
//! `<data>/play-log.ron` (`%LOCALAPPDATA%\\nrsc5-studio\\` installed, or
//! `<exe>\\data\\` portable). Entries older than the configured retention
//! window are pruned on every push and on load. The window defaults to
//! 24 hours and is configurable via [`AppConfig::play_log_retention_hours`].
//!
//! Designed to feed:
//! - A live in-app "Log" panel (chronological + grouped views)
//! - An on-demand CSV export, suitable for ingestion by an external script
//!   (e.g. a Spotipy playlist-builder)

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::fs;
use std::path::Path;

/// Default rolling retention window in hours. Used when no explicit
/// retention has been set on the log (e.g. before [`PlayLog::load`]
/// returns into the app, or in tests).
pub const DEFAULT_RETENTION_HOURS: u32 = 24;

/// Hard floor/ceiling for retention. The ceiling matches the practical
/// max imposed by `HARD_CAP` at typical play rates (≈7 days).
pub const MIN_RETENTION_HOURS: u32 = 1;
pub const MAX_RETENTION_HOURS: u32 = 168;

/// Allowed dropdown choices surfaced in the UI. Kept in module scope so
/// the config validator and the UI agree on the set.
pub const RETENTION_CHOICES: &[u32] = &[1, 6, 12, 24, 48, 72, 168];

/// Defensive cap on entries held in memory. Far above the realistic max
/// (~30 plays/h × 24 h = 720) so it only ever activates in pathological
/// metadata-flap scenarios.
const HARD_CAP: usize = 5000;

/// Minimum interval between successive accepted pushes. Filters out
/// metadata flapping during retune / signal hiccups without affecting the
/// pair-equality dedup.
const PUSH_RATE_LIMIT_MS: i64 = 30_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayEntry {
    /// Unix epoch milliseconds (UTC). Use [`fmt_local_hhmm`] or
    /// [`fmt_local_rfc3339`] for display / export.
    pub ts_millis: i64,
    pub title: String,
    pub artist: String,
    pub frequency_mhz: f32,
    pub program: u32,
}

impl PlayEntry {
    /// `"103.7 HD1"` — derived for display/export, never stored.
    pub fn station_label(&self) -> String {
        format!("{:.1} HD{}", self.frequency_mhz, self.program + 1)
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct OnDiskFormat {
    entries: Vec<PlayEntry>,
}

#[derive(Debug, Default)]
pub struct PlayLog {
    entries: VecDeque<PlayEntry>,
    /// Active retention window in hours. Defaults to
    /// [`DEFAULT_RETENTION_HOURS`]; the app overrides this with the
    /// configured value via [`PlayLog::set_retention_hours`].
    retention_hours: u32,
}

impl PlayLog {
    /// Load from disk. Missing / unreadable / malformed files yield an
    /// empty log — failure is always non-fatal. Entries older than the
    /// retention window are dropped immediately.
    pub fn load() -> Self {
        let mut log = Self {
            entries: VecDeque::new(),
            retention_hours: DEFAULT_RETENTION_HOURS,
        };
        if let Some(path) = crate::paths::play_log_path() {
            if let Ok(raw) = fs::read_to_string(&path) {
                if let Ok(parsed) = ron::from_str::<OnDiskFormat>(&raw) {
                    log.entries = parsed.entries.into();
                }
            }
        }
        log.prune();
        log
    }

    /// Update the retention window and immediately prune entries that
    /// fall outside it. Caller is responsible for persisting after the
    /// change if the prune mutated the log.
    pub fn set_retention_hours(&mut self, hours: u32) {
        self.retention_hours = clamp_retention(hours);
        self.prune();
    }

    pub fn retention_hours(&self) -> u32 {
        self.retention_hours
    }

    /// Drop every entry from the in-memory log. Caller persists.
    pub fn clear_all(&mut self) {
        self.entries.clear();
    }

    /// Try to record a new play. Returns `true` if an entry was pushed.
    ///
    /// Skips the push if:
    /// - `title` or `artist` is empty after trimming
    /// - either field looks like station identification (see
    ///   [`is_likely_station_string`])
    /// - `(title, artist)` matches the most recent entry (pair-equality dedup)
    /// - the most recent entry was pushed within [`PUSH_RATE_LIMIT_MS`]
    pub fn try_push(
        &mut self,
        now_millis: i64,
        title: &str,
        artist: &str,
        frequency_mhz: f32,
        program: u32,
        call_sign: &str,
    ) -> bool {
        let title = title.trim();
        let artist = artist.trim();
        if title.is_empty() || artist.is_empty() {
            return false;
        }
        if is_likely_station_string(title, call_sign, frequency_mhz)
            || is_likely_station_string(artist, call_sign, frequency_mhz)
        {
            return false;
        }
        if let Some(last) = self.entries.back() {
            if last.title == title && last.artist == artist {
                return false;
            }
            if (now_millis - last.ts_millis) < PUSH_RATE_LIMIT_MS {
                return false;
            }
        }
        self.entries.push_back(PlayEntry {
            ts_millis: now_millis,
            title: title.to_string(),
            artist: artist.to_string(),
            frequency_mhz,
            program,
        });
        while self.entries.len() > HARD_CAP {
            self.entries.pop_front();
        }
        self.prune();
        true
    }

    /// Drop entries older than the retention window. Safe to call often.
    pub fn prune(&mut self) {
        let hours = if self.retention_hours == 0 {
            DEFAULT_RETENTION_HOURS
        } else {
            self.retention_hours
        };
        let retention_ms = (hours as i64) * 60 * 60 * 1000;
        let cutoff = now_millis() - retention_ms;
        while self.entries.front().is_some_and(|e| e.ts_millis < cutoff) {
            self.entries.pop_front();
        }
    }

    pub fn entries(&self) -> &VecDeque<PlayEntry> {
        &self.entries
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Persist atomically (tmp + rename). Failure is non-fatal — the log
    /// keeps working in memory.
    pub fn save(&self) {
        let Some(path) = crate::paths::play_log_path() else { return };
        let Some(parent) = path.parent() else { return };
        if fs::create_dir_all(parent).is_err() {
            return;
        }
        let payload = OnDiskFormat {
            entries: self.entries.iter().cloned().collect(),
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
        let _ = fs::rename(&tmp, &path);
    }

    /// Write the current log as CSV to `path`. Columns:
    /// `timestamp_iso,artist,title,station,frequency_mhz,program`.
    /// Chronological (oldest first) so the file matches the on-disk order.
    pub fn export_csv(&self, path: &Path) -> std::io::Result<()> {
        use std::io::Write;
        let mut f = fs::File::create(path)?;
        writeln!(
            f,
            "timestamp_iso,artist,title,station,frequency_mhz,program"
        )?;
        for e in &self.entries {
            writeln!(
                f,
                "{},{},{},{},{:.1},{}",
                fmt_local_rfc3339(e.ts_millis),
                csv_field(&e.artist),
                csv_field(&e.title),
                csv_field(&e.station_label()),
                e.frequency_mhz,
                e.program,
            )?;
        }
        Ok(())
    }
}

pub fn now_millis() -> i64 {
    use std::time::SystemTime;
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub fn fmt_local_hhmm(ts_millis: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ts_millis)
        .map(|dt| dt.with_timezone(&chrono::Local).format("%H:%M").to_string())
        .unwrap_or_default()
}

pub fn fmt_local_rfc3339(ts_millis: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ts_millis)
        .map(|dt| dt.with_timezone(&chrono::Local).to_rfc3339())
        .unwrap_or_default()
}

/// Build the default CSV filename used as the initial filename in the
/// Save-As dialog: `nrsc5-studio-playlog-<YYYYMMDD-HHMMSS>.csv`.
pub fn suggested_csv_filename() -> String {
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    format!("nrsc5-studio-playlog-{stamp}.csv")
}

/// Snap an arbitrary retention value to the supported range.
pub fn clamp_retention(hours: u32) -> u32 {
    hours.clamp(MIN_RETENTION_HOURS, MAX_RETENTION_HOURS)
}

fn csv_field(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') || s.contains('\r') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// Heuristic: does this metadata field look like the station identifying
/// itself rather than a song? Filters out "WXYZ 103.7 FM" / "The Eagle" /
/// "HD2" / "97.1 MHz" style strings that some broadcasters wedge into the
/// title or artist field between songs.
///
/// Conservative on purpose — we'd rather log a few weird strings than drop
/// real songs. The reject criteria are:
/// - Contains the broadcaster call sign (case-insensitive), when known.
/// - Contains the station frequency formatted as `"{N.N}"` (e.g. `"103.7"`).
/// - Whole-word match (case-insensitive) on a small set of broadcast
///   identifiers: `FM`, `AM`, `MHz`, `HD1`..`HD4`.
pub fn is_likely_station_string(field: &str, call_sign: &str, frequency_mhz: f32) -> bool {
    let lower = field.to_ascii_lowercase();

    if !call_sign.is_empty() {
        let cs = call_sign.to_ascii_lowercase();
        // Require at least 3 chars so very short / accidental call signs
        // don't match common letter trigraphs in song titles.
        if cs.len() >= 3 && lower.contains(&cs) {
            return true;
        }
    }

    let freq_str = format!("{:.1}", frequency_mhz);
    if lower.contains(&freq_str) {
        return true;
    }

    // Whole-word match against broadcast identifiers.
    const TOKENS: &[&str] = &["fm", "am", "mhz", "hd1", "hd2", "hd3", "hd4"];
    for word in lower.split(|c: char| !c.is_ascii_alphanumeric()) {
        if TOKENS.contains(&word) {
            return true;
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_call_sign() {
        assert!(is_likely_station_string("KEGL 97.1", "KEGL", 97.1));
        assert!(is_likely_station_string("Visit kegl.com", "KEGL", 97.1));
    }

    #[test]
    fn rejects_frequency() {
        assert!(is_likely_station_string("103.7 The Mix", "", 103.7));
        assert!(is_likely_station_string("FM 95.5", "", 95.5));
    }

    #[test]
    fn rejects_broadcast_tokens() {
        assert!(is_likely_station_string("More Music FM", "", 99.9));
        assert!(is_likely_station_string("HD2 Rocks", "", 99.9));
    }

    #[test]
    fn accepts_real_songs() {
        assert!(!is_likely_station_string("Bohemian Rhapsody", "KEGL", 97.1));
        assert!(!is_likely_station_string("Don't Stop Me Now", "KEGL", 97.1));
        assert!(!is_likely_station_string("Take On Me", "WXYZ", 103.7));
    }

    #[test]
    fn ignores_very_short_call_signs() {
        // A 2-letter "call sign" would match too much (e.g. "I" inside titles).
        assert!(!is_likely_station_string("It's a Long Way to the Top", "AB", 99.9));
    }
}
