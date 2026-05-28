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

/// Directory containing the running executable. Cached on first call.
/// `None` when the OS refuses to tell us where we are (extremely rare —
/// only happens on heavily-sandboxed configurations).
pub fn exe_dir() -> Option<PathBuf> {
    static CACHED: OnceLock<Option<PathBuf>> = OnceLock::new();
    CACHED
        .get_or_init(|| {
            std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        })
        .clone()
}

/// Directory containing bundled native DLLs (`libSoapySDR.dll`,
/// `nrsc5.exe`, the librtlsdr / libhackrf / libsdrPlaySupport
/// support libraries, etc.). Two layouts are supported:
///
/// 1. **Portable-zip / installed layout**: `<exe_dir>\bin\` holds
///    all native deps, with a `SoapySDR\modules0.8\` subfolder for
///    the loadable SoapySDR modules. This is what
///    `scripts/package-portable.ps1` ships and matches the repo
///    layout used by `cargo run` (next case).
/// 2. **Dev `cargo run` layout**: cargo emits the binary into
///    `<repo>\target\…\{debug,release}\`. We walk up to 6 directory
///    levels from the exe looking for a sibling `bin\` folder. We
///    deliberately don't hardcode depth — `..\..\..\bin` would
///    break for the gnullvm target which adds an extra directory
///    level versus the host triple.
///
/// Returns `None` only when neither match — caller then trusts that
/// whoever launched us put the DLLs on PATH already.
pub fn bundled_dll_dir() -> Option<PathBuf> {
    let exe = exe_dir()?;
    // Layout 1: <exe_dir>\bin\
    let portable = exe.join("bin");
    if portable.is_dir() {
        return Some(portable);
    }
    // Layout 2: walk up looking for a sibling `bin\`.
    let mut probe = exe.clone();
    for _ in 0..6 {
        if let Some(parent) = probe.parent() {
            let candidate = parent.join("bin");
            if candidate.is_dir() {
                return Some(candidate);
            }
            probe = parent.to_path_buf();
        } else {
            break;
        }
    }
    None
}

/// Directory containing SoapySDR module DLLs that libSoapySDR loads
/// at runtime (`SoapyRTLSDR.dll`, `libsdrPlaySupport.dll`, etc.).
/// Conventionally `<bundled_dll_dir>\SoapySDR\modules0.8\`. Returns
/// `None` when the bundle isn't present.
pub fn bundled_soapy_modules_dir() -> Option<PathBuf> {
    let bin = bundled_dll_dir()?;
    let modules = bin.join("SoapySDR").join("modules0.8");
    if modules.is_dir() {
        Some(modules)
    } else {
        None
    }
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

/// Path to the per-frequency gain cache (Phase 3 of the v0.4.0 AGC
/// overhaul). Same data-root as the play log; the schema-versioned RON
/// file holds one entry per `(freq, driver, antenna, ppm)` tuple. See
/// [`crate::sdr::gain_cache`].
///
/// Portable mode: `<exe_dir>\data\gain-cache.ron`.
/// Installed mode: `%LOCALAPPDATA%\nrsc5-studio\gain-cache.ron`.
pub fn gain_cache_path() -> Option<PathBuf> {
    Some(data_root()?.join("gain-cache.ron"))
}

/// Path to the AGC search trace log (Phase 2c). The piped-mode AGC
/// driver thread writes one human-readable line per gain change plus
/// SETTLED/BAILED transitions. Truncated at the start of every
/// `start_piped` call so the file reflects the current tune's run
/// only — old runs are not preserved (the gain cache captures the
/// converged outcome, the trace is purely diagnostic).
///
/// Portable mode: `<exe_dir>\data\agc-trace.log`.
/// Installed mode: `%LOCALAPPDATA%\nrsc5-studio\agc-trace.log`.
pub fn agc_trace_path() -> Option<PathBuf> {
    Some(data_root()?.join("agc-trace.log"))
}

/// Path to the SDR diagnostics snapshot written every time
/// `SoapySdr::enumerate_devices()` runs. Captures env vars + per-driver
/// enumeration outcomes so a "no devices detected" report can be
/// triaged without rebuilding the app.
///
/// Portable mode: `<exe_dir>\data\sdr-diagnostics.txt`.
/// Installed mode: `%APPDATA%\nrsc5-studio\sdr-diagnostics.txt`.
pub fn sdr_diagnostics_file() -> Option<PathBuf> {
    Some(data_root()?.join("sdr-diagnostics.txt"))
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

/// Default base directory for Opus recordings.
///
/// We always default to `<exe_dir>/recordings` regardless of portable
/// vs. installed mode, because recordings are user-facing media the
/// user wants to find next to the app — not buried under
/// `data/recordings/` in portable mode or under `~/Documents/` in
/// installed mode. This keeps the design portable-first: a USB-stick
/// install owns its own captures end-to-end, and an installed-mode
/// run that points at a writable exe directory (which is the common
/// case for unprivileged installs / dev runs from `target/...`)
/// likewise keeps everything in one place.
///
/// If `<exe_dir>` isn't writable (e.g. an admin install to
/// `Program Files`), the user can override the path in
/// Settings → Recording. The recorder thread surfaces a clear "could
/// not create file" status if the chosen dir is read-only.
///
/// Falls back to Documents only if the OS refuses to tell us where
/// the executable lives (extremely rare).
pub fn default_recording_dir() -> Option<PathBuf> {
    if let Some(dir) = exe_dir() {
        return Some(dir.join("recordings"));
    }
    Some(dirs::document_dir()?.join(APP_DIRNAME).join("recordings"))
}

/// Starting directory for the CSV-export Save-As dialog. Documents in
/// both modes — CSV is a user-facing export, not part of portable data.
pub fn documents_dir() -> Option<PathBuf> {
    dirs::document_dir()
}
