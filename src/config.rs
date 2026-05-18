use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum GainMode {
    /// Closed-loop software AGC (the controller in `src/dsp/agc.rs`).
    /// This is the default — it's what makes weak/marginal stations
    /// usable on the R820T2 without per-station hand-tuning.
    #[default]
    Auto,
    /// Hold a fixed manual gain (value in `manual_gain_tenths`). Useful
    /// for A/B testing or when a station is known to want a specific
    /// gain. The value is snapped to the nearest table step at apply time.
    Manual,
    /// Hand control to the R820T2's hardware AGC. Almost always wrong
    /// for HD Radio (it over-amplifies the analog carrier and clips the
    /// ADC, killing MER) but kept as an escape hatch for debugging and
    /// for parity with USB / rtl_tcp paths where nrsc5 owns the dongle.
    HardwareAgc,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Preset {
    pub name: String,
    pub frequency_mhz: f32,
    pub program: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub frequency_mhz: f32,
    pub selected_program: u32,
    pub dark_mode: bool,
    pub use_rtl_tcp: bool,
    pub rtl_device_index: u32,
    pub rtl_tcp_host: String,
    pub rtl_tcp_port: u16,
    /// **Dev-only** (v0.2.0 step 4): when `true`, the Start command
    /// drives `nrsc5.exe` from our own in-process [`Sdr`](crate::sdr)
    /// backend via stdin (`-r -`) instead of letting nrsc5 open the USB
    /// device itself. Off by default — flip in `config.toml` to test the
    /// piped path. Once the waterfall (step 5+) is wired in, this will
    /// become the default and the flag will graduate to a real UI toggle.
    #[serde(default)]
    pub use_piped_sdr: bool,
    #[serde(default)]
    pub presets: Vec<Preset>,
    #[serde(default = "default_volume")]
    pub volume: f32,
    #[serde(default)]
    pub muted: bool,
    /// Maximum number of tiles shown in the album-art collage. The UI
    /// snaps this to powers of two in [1, 512]; manual edits outside the
    /// range are clamped on load.
    #[serde(default = "default_collage_max_tiles")]
    pub collage_max_tiles: u32,
    /// How the tuner gain is controlled on the piped backend. Defaults
    /// to `Auto` (closed-loop AGC); see [`GainMode`] for the alternatives.
    #[serde(default)]
    pub gain_mode: GainMode,
    /// Tuner gain in tenths of dB used when `gain_mode == Manual`.
    /// Snapped to the nearest R820T2 table step at apply time. Default
    /// 197 (19.7 dB), the mid-range starting point inherited from the AGC.
    #[serde(default = "default_manual_gain_tenths")]
    pub manual_gain_tenths: i32,
}

fn default_volume() -> f32 {
    1.0
}

fn default_collage_max_tiles() -> u32 {
    64
}

fn default_manual_gain_tenths() -> i32 {
    197
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            frequency_mhz: 101.1,
            selected_program: 0,
            dark_mode: true,
            use_rtl_tcp: false,
            rtl_device_index: 0,
            rtl_tcp_host: "127.0.0.1".to_string(),
            rtl_tcp_port: 1234,
            use_piped_sdr: false,
            presets: Vec::new(),
            volume: 1.0,
            muted: false,
            collage_max_tiles: 64,
            gain_mode: GainMode::Auto,
            manual_gain_tenths: 197,
        }
    }
}

fn config_path() -> Option<PathBuf> {
    let base = dirs::config_dir()?;
    Some(base.join("nrsc5-studio").join("config.toml"))
}

/// Legacy config path used by the pre-rename builds ("NRSC5 Rust" /
/// `nrsc5-tui-rust`). Loaded as a fallback so existing users don't lose
/// their presets when upgrading.
fn legacy_config_path() -> Option<PathBuf> {
    let base = dirs::config_dir()?;
    Some(base.join("nrsc5-tui-rust").join("config.toml"))
}

pub fn load_config() -> AppConfig {
    // Prefer the current path.
    if let Some(path) = config_path() {
        if let Ok(raw) = fs::read_to_string(&path) {
            if let Ok(cfg) = toml::from_str::<AppConfig>(&raw) {
                return cfg;
            }
        }
    }
    // Fall back to the legacy location so upgrades preserve presets, etc.
    if let Some(path) = legacy_config_path() {
        if let Ok(raw) = fs::read_to_string(&path) {
            if let Ok(cfg) = toml::from_str::<AppConfig>(&raw) {
                return cfg;
            }
        }
    }
    AppConfig::default()
}

pub fn save_config(cfg: &AppConfig) {
    let Some(path) = config_path() else {
        return;
    };

    let Some(parent) = path.parent() else {
        return;
    };

    if fs::create_dir_all(parent).is_err() {
        return;
    }

    let Ok(raw) = toml::to_string_pretty(cfg) else {
        return;
    };

    let _ = fs::write(path, raw);
}
