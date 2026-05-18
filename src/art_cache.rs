//! On-disk album-art cache that survives across runs.
//!
//! The in-memory `art_history` map in `app.rs` is keyed by a content hash
//! of the cover image bytes. We use the same hash here as a filename so the
//! cache is naturally content-addressable: the same album cover transmitted
//! under different LOT IDs collapses to a single file.
//!
//! Layout under `dirs::data_local_dir()` (e.g. `%LOCALAPPDATA%` on Windows):
//!
//! ```text
//! nrsc5-studio/
//!   art-cache/
//!     history.ron        ← manifest (this file)
//!     a1b2c3d4....jpg    ← one file per unique cover
//!     ...
//! ```
//!
//! The manifest records, per unique cover, the wall-clock timestamps of
//! every play observed within the 8-hour rolling window plus the
//! `(title, artist)` pairs and most recently observed album name. Play
//! timestamps are stored as Unix milliseconds (signed i64) so the file is
//! portable across machines and time zones without pulling in chrono's
//! serde feature.
//!
//! Errors are intentionally swallowed everywhere: a corrupt or unwritable
//! cache should never prevent the radio from working — at worst the user
//! starts with an empty collage on next launch.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

const MANIFEST_FILENAME: &str = "history.ron";
/// Bump on any breaking change to the on-disk format. Old manifests are
/// silently ignored (treated as empty) rather than migrated.
const MANIFEST_VERSION: u32 = 1;

/// One unique cover image as persisted to disk. Mirrors `ArtEntry` in
/// `app.rs` minus the in-memory-only `Instant`-based timestamps, which are
/// converted to/from wall-clock millis at the persistence boundary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedEntry {
    /// 64-bit content hash of the image bytes.
    pub hash: u64,
    /// Filename within the cache directory (e.g. `"a1b2c3d4....jpg"`).
    /// Stored as a bare filename — not an absolute path — so the cache
    /// stays portable if the user moves their profile.
    pub filename: String,
    /// Unix milliseconds for each play within the rolling window, oldest
    /// first.
    pub plays_unix_ms: Vec<i64>,
    /// Unique `(title, artist)` pairs seen with this cover.
    pub songs: Vec<(String, String)>,
    /// Most recently observed album name, or empty if none.
    pub album: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct PersistedHistory {
    version: u32,
    entries: Vec<PersistedEntry>,
}

pub struct ArtCache {
    dir: PathBuf,
}

impl ArtCache {
    /// Resolve and create the cache directory. In installed mode this is
    /// under `%LOCALAPPDATA%\nrsc5-studio\art-cache`; in portable mode
    /// it's `<exe_dir>\data\art-cache`. Returns `None` if path resolution
    /// or directory creation fails (very rare — e.g. read-only profile).
    pub fn new() -> Option<Self> {
        let dir = crate::paths::art_cache_dir()?;
        std::fs::create_dir_all(&dir).ok()?;
        Some(Self { dir })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn manifest_path(&self) -> PathBuf {
        self.dir.join(MANIFEST_FILENAME)
    }

    /// Compute the deterministic filename we'd give an image of the given
    /// content hash, preserving the source file's extension when possible.
    /// Used both by `store_image` and when reconstructing in-memory paths
    /// from a persisted entry.
    pub fn filename_for(hash: u64, source_path: &Path) -> String {
        let ext = source_path
            .extension()
            .and_then(|s| s.to_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("bin");
        format!("{:016x}.{}", hash, ext)
    }

    /// Save raw image bytes under their content-addressed filename. If a
    /// file with the same name already exists it is left alone (the hash
    /// guarantees identical contents). Returns the absolute cache path on
    /// success.
    pub fn store_image(
        &self,
        hash: u64,
        bytes: &[u8],
        source_path: &Path,
    ) -> Option<PathBuf> {
        let filename = Self::filename_for(hash, source_path);
        let dest = self.dir.join(&filename);
        if dest.exists() {
            return Some(dest);
        }
        // Atomic-ish write: stage to a tmp sibling then rename. Avoids
        // half-written files if the process is killed mid-write.
        let tmp = self.dir.join(format!("{}.tmp", filename));
        if let Err(e) = std::fs::write(&tmp, bytes) {
            eprintln!("art-cache: failed writing {}: {e}", tmp.display());
            return None;
        }
        if let Err(e) = std::fs::rename(&tmp, &dest) {
            eprintln!("art-cache: rename {} -> {} failed: {e}", tmp.display(), dest.display());
            let _ = std::fs::remove_file(&tmp);
            return None;
        }
        Some(dest)
    }

    /// Load and parse the manifest. Returns an empty list on any error
    /// (file missing, parse failure, version mismatch) — never panics.
    pub fn load_manifest(&self) -> Vec<PersistedEntry> {
        let path = self.manifest_path();
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
            Err(e) => {
                eprintln!("art-cache: reading {} failed: {e}", path.display());
                return Vec::new();
            }
        };
        match ron::from_str::<PersistedHistory>(&text) {
            Ok(p) if p.version == MANIFEST_VERSION => p.entries,
            Ok(_) => {
                eprintln!("art-cache: manifest version mismatch, starting fresh");
                Vec::new()
            }
            Err(e) => {
                eprintln!("art-cache: manifest parse error ({e}), starting fresh");
                Vec::new()
            }
        }
    }

    /// Write the manifest atomically (.tmp + rename). Best-effort; logs on
    /// failure and returns the error so callers can decide whether to
    /// retry on next event.
    pub fn save_manifest(&self, entries: Vec<PersistedEntry>) -> std::io::Result<()> {
        let path = self.manifest_path();
        let tmp = self.dir.join(format!("{}.tmp", MANIFEST_FILENAME));
        let wrapper = PersistedHistory {
            version: MANIFEST_VERSION,
            entries,
        };
        let text = ron::to_string(&wrapper)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, &path)
    }

    /// Sweep the cache directory for any files not in `keep`. Skips the
    /// manifest and any in-flight `.tmp` files. Useful after pruning so we
    /// don't accumulate orphaned image files from expired entries.
    pub fn sweep_orphans(&self, keep: &HashSet<String>) {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return;
        };
        for entry in entries.flatten() {
            let name_os = entry.file_name();
            let Some(name) = name_os.to_str() else { continue; };
            if name == MANIFEST_FILENAME || name.ends_with(".tmp") {
                continue;
            }
            if !keep.contains(name) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}
