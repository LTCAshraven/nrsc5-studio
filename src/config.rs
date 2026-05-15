use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

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
    #[serde(default)]
    pub presets: Vec<Preset>,
    #[serde(default = "default_volume")]
    pub volume: f32,
    #[serde(default)]
    pub muted: bool,
}

fn default_volume() -> f32 {
    1.0
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
            presets: Vec::new(),
            volume: 1.0,
            muted: false,
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
