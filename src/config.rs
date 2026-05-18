use serde::{Deserialize, Serialize};
use std::fs;

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
    /// Rolling-window retention for the play log, in hours. The UI
    /// offers preset choices (1, 6, 12, 24, 48, 72, 168); manual edits
    /// outside [1, 168] are clamped on load. The on-disk `HARD_CAP`
    /// still applies (≤5000 entries) regardless of retention.
    #[serde(default = "default_play_log_retention_hours")]
    pub play_log_retention_hours: u32,
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

fn default_play_log_retention_hours() -> u32 {
    24
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
            // Default to the in-process piped backend. The Spectrum
            // panel's FFT tap and the closed-loop AGC controller are
            // both wired only through `start_piped`; the legacy direct
            // USB path (`nrsc5.start`) hands the dongle to nrsc5.exe
            // and leaves us no way to observe I/Q or steer gain. We
            // ship piped as the default so the v0.2.x features that
            // depend on it work out-of-the-box on a fresh install.
            use_piped_sdr: true,
            presets: Vec::new(),
            volume: 1.0,
            muted: false,
            collage_max_tiles: 64,
            gain_mode: GainMode::Auto,
            manual_gain_tenths: 197,
            play_log_retention_hours: 24,
        }
    }
}

pub fn load_config() -> AppConfig {
    // Prefer the current path.
    if let Some(path) = crate::paths::config_path() {
        if let Ok(raw) = fs::read_to_string(&path) {
            if let Ok(mut cfg) = toml::from_str::<AppConfig>(&raw) {
                sanitize(&mut cfg);
                return cfg;
            }
        }
    }
    // Fall back to the legacy location so upgrades preserve presets, etc.
    // (Skipped in portable mode — a portable install has no claim on host data.)
    if let Some(path) = crate::paths::legacy_config_path() {
        if let Ok(raw) = fs::read_to_string(&path) {
            if let Ok(mut cfg) = toml::from_str::<AppConfig>(&raw) {
                sanitize(&mut cfg);
                return cfg;
            }
        }
    }
    AppConfig::default()
}

/// Clamp deserialized config values to their supported ranges. Keeps
/// hand-edited `config.toml` from poisoning runtime state with
/// out-of-range numbers.
fn sanitize(cfg: &mut AppConfig) {
    cfg.play_log_retention_hours =
        crate::play_log::clamp_retention(cfg.play_log_retention_hours);
}

pub fn save_config(cfg: &AppConfig) {
    let Some(path) = crate::paths::config_path() else {
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
