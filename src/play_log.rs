//! 24-hour rolling song log.
//!
//! Records every observed `(title, artist)` play on a station with a
//! wall-clock timestamp. Survives restarts via a RON file under
//! `%LOCALAPPDATA%\nrsc5-studio\play-log.ron`. Entries older than 24 h are
//! pruned on every push and on load.
//!
//! Designed to feed:
//! - A live in-app "Log" panel (chronological + grouped views)
//! - An on-demand CSV export, suitable for ingestion by an external script
//!   (e.g. a Spotipy playlist-builder)

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};

/// Rolling retention window — entries older than this are dropped on every
/// push and on every load.
const RETENTION_MS: i64 = 24 * 60 * 60 * 1000;

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
}

impl PlayLog {
    /// Load from disk. Missing / unreadable / malformed files yield an
    /// empty log — failure is always non-fatal. Entries older than the
    /// retention window are dropped immediately.
    pub fn load() -> Self {
        let mut log = Self::default();
        if let Some(path) = log_path() {
            if let Ok(raw) = fs::read_to_string(&path) {
                if let Ok(parsed) = ron::from_str::<OnDiskFormat>(&raw) {
                    log.entries = parsed.entries.into();
                }
            }
        }
        log.prune();
        log
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
        let cutoff = now_millis() - RETENTION_MS;
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

    /// Persist atomically to `%LOCALAPPDATA%\nrsc5-studio\play-log.ron`.
    /// Failure is non-fatal — the log keeps working in memory.
    pub fn save(&self) {
        let Some(path) = log_path() else { return };
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

/// Suggested CSV export destination: `Documents\nrsc5-studio-playlog-<ts>.csv`.
/// Falls back to the play-log dir if Documents can't be resolved.
pub fn suggested_csv_path() -> Option<PathBuf> {
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    let filename = format!("nrsc5-studio-playlog-{stamp}.csv");
    if let Some(docs) = dirs::document_dir() {
        return Some(docs.join(&filename));
    }
    let parent = log_path()?.parent()?.to_path_buf();
    Some(parent.join(filename))
}

fn log_path() -> Option<PathBuf> {
    let base = dirs::data_local_dir()?;
    Some(base.join("nrsc5-studio").join("play-log.ron"))
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
