use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
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

/// Phase 4 — Opus 96 kbps recording mode. Persisted so the user's
/// choice survives across sessions. Chunk 4.3 wires Off and
/// Continuous; Chunk 4.4 adds the PerSong PSD-split logic on top of
/// the same `RecordingSession` lifecycle.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum RecordingMode {
    /// Pressing the Record button is a no-op; the recorder never
    /// spawns. Default — we don't want to surprise a user with disk
    /// fills on first launch.
    #[default]
    Off,
    /// Record the locked subchannel continuously, rotating to a
    /// fresh file whenever `recording_max_minutes` is reached. The
    /// old `per_song` and `continuous` config values both
    /// deserialize to this — PSD timing on real-world stations is
    /// too irregular for reliable song-boundary splits, so they're
    /// folded into one "just record" mode.
    #[serde(alias = "per_song", alias = "continuous")]
    On,
}

impl RecordingMode {
    /// Short label for the Settings dropdown.
    pub fn label(self) -> &'static str {
        match self {
            RecordingMode::Off => "Off",
            RecordingMode::On => "Record (rotate at max minutes)",
        }
    }
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
    /// Backend-agnostic SDR configuration. Holds the SoapySDR driver
    /// key + device-args string + per-element manual gain values + PPM
    /// correction. Populated by [`migrate_legacy_sdr`] on load when an
    /// older config (with only `rtl_device_index` / `use_rtl_tcp`) is
    /// upgraded; the legacy fields are preserved alongside so 0.4.0's
    /// rtl_tcp restoration can still find them.
    #[serde(default)]
    pub sdr: SdrConfigSection,
    /// When true, the HD program selector exposes HD5..HD8 in a
    /// second row below HD1..HD4. Off by default because most
    /// stations only advertise up to HD4 and the extra row eats
    /// vertical dock space; users can flip it once they tune to a
    /// station with the MP11 partition.
    #[serde(default)]
    pub show_hd5_hd8: bool,
    /// When true, every subchannel advertised by SIS gets a
    /// background decoder spawned automatically as soon as it shows
    /// up in the station info table. Off by default — most users
    /// only want HD1 streaming, and an MP3 station with four
    /// advertised programs would otherwise pin 3× the per-decoder
    /// CPU as soon as you tune. Persisted so power users who do
    /// want all subchannels always-on can set it once.
    #[serde(default)]
    pub auto_decode_all_advertised: bool,
    /// Phase 4 — selected recording mode. See `RecordingMode` for the
    /// behavior of each variant.
    #[serde(default)]
    pub recording_mode: RecordingMode,
    /// Override for the recording output directory. `None` means "use
    /// `paths::default_recording_dir()`" — portable root in portable
    /// mode, `~/Documents/nrsc5-studio/recordings/` otherwise. The
    /// SDR Settings → Recording dialog writes here when the user
    /// picks a custom location.
    #[serde(default)]
    pub recording_dir: Option<String>,
    /// Per-file rotation cap in minutes. Applies to both Continuous
    /// (file rotates at this many minutes elapsed) and PerSong
    /// (file closes at this cap even if metadata hasn't changed,
    /// catching stations stuck broadcasting the same PSD for hours).
    /// Default 60 minutes — fits a typical talk-show hour, keeps
    /// per-file size around 40 MB at 96 kbps so a crash never loses
    /// more than ~40 MB of audio.
    #[serde(default = "default_recording_max_minutes")]
    pub recording_max_minutes: u32,
    /// When true, recordings get filed under a per-station subfolder
    /// (e.g. `recordings/KEGL-FM_The Eagle/...`) rather than directly
    /// in the output root. Default true — the alternative gets
    /// unmanageable fast once you've recorded from a few stations.
    #[serde(default = "default_true")]
    pub recording_subfolder_per_station: bool,
}

/// SoapySDR-keyed configuration for the v0.3.0 in-process backend.
///
/// `driver` is the Soapy driver key (`"rtlsdr"`, `"sdrplay"`,
/// `"hackrf"`). `device_args` is the rest of the args string passed to
/// [`SoapySdr::open`](crate::sdr::SoapySdr::open) — e.g. `"device=1"`
/// to pick the second RTL-SDR, or `"serial=02000001"` to pick a
/// specific SDRplay. Together they form the full args string
/// `format!("driver={driver},{device_args}")` (or just `driver={driver}`
/// if `device_args` is empty).
///
/// `gains` is a per-element override map (`{"TUNER": 19.7}` for
/// RTL-SDR; `{"IFGR": 40.0, "RFGR": 4.0}` for SDRplay). The SDR
/// Settings modal writes here; the AGC adapter ignores this map and
/// drives the profile's target element directly. Stored as a
/// `BTreeMap` to keep TOML key order deterministic across saves.
///
/// `freq_correction_ppm` is applied once per stream at start; mid-stream
/// PPM nudges from the SDR Settings modal call
/// [`Sdr::set_frequency_correction_ppm`](crate::sdr::Sdr::set_frequency_correction_ppm)
/// directly without going through config.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SdrConfigSection {
    #[serde(default = "default_sdr_driver")]
    pub driver: String,
    #[serde(default)]
    pub device_args: String,
    #[serde(default)]
    pub freq_correction_ppm: f64,
    #[serde(default)]
    pub gains: BTreeMap<String, f64>,
}

fn default_sdr_driver() -> String {
    "rtlsdr".to_string()
}

impl Default for SdrConfigSection {
    fn default() -> Self {
        Self {
            driver: default_sdr_driver(),
            device_args: String::new(),
            freq_correction_ppm: 0.0,
            gains: BTreeMap::new(),
        }
    }
}

impl SdrConfigSection {
    /// Build the full SoapySDR args string for `Device::new`. Combines
    /// `driver` and `device_args` so the caller doesn't need to repeat
    /// the formatting. Returns `"driver=rtlsdr"` for the default
    /// case, or `"driver=rtlsdr,device=2"` etc. when args are set.
    pub fn to_args_string(&self) -> String {
        if self.device_args.trim().is_empty() {
            format!("driver={}", self.driver)
        } else {
            format!("driver={},{}", self.driver, self.device_args.trim())
        }
    }
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

fn default_recording_max_minutes() -> u32 {
    60
}

fn default_true() -> bool {
    true
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
            sdr: SdrConfigSection::default(),
            show_hd5_hd8: false,
            auto_decode_all_advertised: false,
            recording_mode: RecordingMode::Off,
            recording_dir: None,
            recording_max_minutes: 60,
            recording_subfolder_per_station: true,
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
    migrate_legacy_sdr(cfg);
}

/// Migrate the pre-0.3.0 SDR config layout into the new `[sdr]`
/// section. Triggered on every load — idempotent in steady state
/// because we only fill in fields the user hasn't already configured.
///
/// Migration rules:
///
/// * If `cfg.sdr.driver` is the default `"rtlsdr"` AND `cfg.sdr.device_args`
///   is empty, populate `device_args` from the legacy
///   `rtl_device_index` field (`"device=N"` for N > 0; left empty for 0
///   to match Soapy's default-first-device behavior).
/// * If `cfg.sdr.gains` is empty AND legacy `manual_gain_tenths` is
///   non-default, seed a `TUNER` entry so saving the config produces
///   a sensible round-trip.
/// * If `use_rtl_tcp` is `true`, log a one-shot warning that rtl_tcp
///   support is deferred to 0.4.0 (via SoapyRemote) — but DON'T touch
///   the legacy fields. The user's preferences for host/port survive
///   the upgrade; we just fall back to local USB RTL-SDR for the
///   0.3.x cycle. See `/memories/session/plan-0.4.0-stub.md` for the
///   restoration plan.
///
/// The legacy fields (`rtl_device_index`, `use_rtl_tcp`,
/// `rtl_tcp_host`, `rtl_tcp_port`, `use_piped_sdr`, `manual_gain_tenths`,
/// `gain_mode`) are NOT removed from `AppConfig` — they keep
/// round-tripping unchanged so 0.4.0 can read them.
fn migrate_legacy_sdr(cfg: &mut AppConfig) {
    // Populate device_args from rtl_device_index if the user hasn't
    // set anything explicit. We use the heuristic "default driver AND
    // empty device_args" rather than just "empty device_args" so the
    // first migration cleanly seeds, and subsequent loads (where the
    // user has switched to e.g. driver=sdrplay) leave the args alone.
    if cfg.sdr.driver == "rtlsdr" && cfg.sdr.device_args.is_empty()
        && cfg.rtl_device_index > 0
    {
        cfg.sdr.device_args = format!("device={}", cfg.rtl_device_index);
    }

    // Seed a TUNER gain entry from the legacy manual_gain_tenths if
    // gains is empty. Skipped when gain_mode == Auto since the AGC
    // controller manages the value anyway. Stored as dB (not tenths)
    // to match the new section's convention.
    if cfg.sdr.gains.is_empty() && cfg.gain_mode == GainMode::Manual {
        cfg.sdr.gains.insert(
            "TUNER".to_string(),
            cfg.manual_gain_tenths as f64 / 10.0,
        );
    }

    // rtl_tcp deferral notice. Logged through eprintln rather than
    // tracing/log because the rest of this module is unaware of those
    // facades; the goal is just to surface the change once on launch
    // without breaking anyone's existing config.
    if cfg.use_rtl_tcp {
        eprintln!(
            "[config] WARN: use_rtl_tcp=true in config — rtl_tcp support is \
             deferred to v0.4.0 via SoapyRemote (see CHANGELOG and \
             README 'Supported SDRs'). Falling back to local USB RTL-SDR \
             via SoapySDR for this session. Your rtl_tcp_host/port settings \
             are preserved untouched and will be re-honored when 0.4.0 ships."
        );
    }
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
