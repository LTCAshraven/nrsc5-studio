//! Path resolution with portable-mode support.
//!
//! Default ("installed") mode places persistent state under the user's
//! standard system locations:
//!   - Config:      `%APPDATA%\nrsc5-studio\`
//!   - Data/cache:  `%LOCALAPPDATA%\nrsc5-studio\`
//!   - AAS scratch: `%TEMP%\nrsc5-tui-aas\` (unchanged from earlier builds)
//!   - eframe persistence (window + dock layout): eframe default
//!
//! Portable mode is enabled by placing a `portable.txt` marker file beside
//! the executable. All persistent state then lives under `<exe_dir>\data\`:
//!   - `<exe_dir>\data\config.toml`
//!   - `<exe_dir>\data\art-cache\`
//!   - `<exe_dir>\data\play-log.ron`
//!   - `<exe_dir>\data\eframe\`   (window geometry + dock layout)
//!   - `<exe_dir>\data\aas\`      (traffic / weather scratch)
//!
//! Detection is cached at first call. Release zips ship with `portable.txt`
//! pre-included so the bundle is self-contained when extracted.

use std::path::PathBuf;
use std::sync::OnceLock;

const MARKER_FILENAME: &str = "portable.txt";
const APP_DIRNAME: &str = "nrsc5-studio";
const PORTABLE_DATA_DIR: &str = "data";
/// Legacy AAS scratch dir name preserved from the 0.1.x / pre-rename era.
/// Kept stable so upgrades don't orphan files in `%TEMP%`.
const AAS_DIR_NAME: &str = "nrsc5-tui-aas";

fn portable_root() -> Option<&'static PathBuf> {
    static CACHED: OnceLock<Option<PathBuf>> = OnceLock::new();
    CACHED
        .get_or_init(|| {
            let exe = std::env::current_exe().ok()?;
            let exe_dir = exe.parent()?.to_path_buf();
            if exe_dir.join(MARKER_FILENAME).is_file() {
                Some(exe_dir.join(PORTABLE_DATA_DIR))
            } else {
                None
            }
        })
        .as_ref()
}

/// `true` when a `portable.txt` marker was found beside the executable.
pub fn is_portable() -> bool {
    portable_root().is_some()
}

fn config_root() -> Option<PathBuf> {
    if let Some(root) = portable_root() {
        return Some(root.clone());
    }
    Some(dirs::config_dir()?.join(APP_DIRNAME))
}

fn data_root() -> Option<PathBuf> {
    if let Some(root) = portable_root() {
        return Some(root.clone());
    }
    Some(dirs::data_local_dir()?.join(APP_DIRNAME))
}

/// Path to `config.toml`. `None` only if the OS refuses to surface a
/// config directory (effectively never on Windows).
pub fn config_path() -> Option<PathBuf> {
    Some(config_root()?.join("config.toml"))
}

/// Legacy installed-mode config path used by the pre-rename builds
/// (`nrsc5-tui-rust`). Returns `None` in portable mode — a portable
/// install has no claim on host-side legacy data.
pub fn legacy_config_path() -> Option<PathBuf> {
    if is_portable() {
        return None;
    }
    Some(dirs::config_dir()?.join("nrsc5-tui-rust").join("config.toml"))
}

/// Directory for cached album-art images and their manifest.
pub fn art_cache_dir() -> Option<PathBuf> {
    Some(data_root()?.join("art-cache"))
}

/// Path to the 24-hour rolling play-log RON file.
pub fn play_log_path() -> Option<PathBuf> {
    Some(data_root()?.join("play-log.ron"))
}

/// Override file path for eframe's window/dock persistence RON. `None`
/// means "use eframe's default" (which is what installed mode wants).
///
/// Despite the field name `persistence_path`, eframe treats this as the
/// full path to the `.ron` file, not a directory. We give it
/// `<root>\eframe\app.ron` so the file lives in its own subfolder
/// matching eframe's conventional layout; `save_to_disk` mkdirs the
/// parent for us.
pub fn eframe_storage_file() -> Option<PathBuf> {
    Some(portable_root()?.join("eframe").join("app.ron"))
}

/// Scratch directory where `nrsc5.exe --dump-aas-files` writes traffic
/// tiles, weather radar frames, album art, and other AAS assets. Always
/// returns a path (no `Option`) — `%TEMP%` is always available.
pub fn aas_temp_dir() -> PathBuf {
    if let Some(root) = portable_root() {
        return root.join("aas");
    }
    std::env::temp_dir().join(AAS_DIR_NAME)
}

/// Starting directory for the CSV-export Save-As dialog. Documents in
/// both modes — CSV is a user-facing export, not part of portable data.
pub fn documents_dir() -> Option<PathBuf> {
    dirs::document_dir()
}
