use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;

/// Which I/Q transport feeds the in-process piped pipeline. Persisted
/// in `[sdr]` so the SDR Settings modal can route Start through the
/// right backend without consulting any legacy 0.2.x fields. `LocalSoapy`
/// is the default for fresh installs; `SoapyRemote` and `RtlTcpRemote`
/// add the matching connection fields (`remote_host`, `remote_port`,
/// `remote_extra_args`) to [`SdrConfigSection`].
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum SdrTransport {
    /// Open a local SoapySDR device described by `driver` + `device_args`.
    /// The default — what you get on a fresh install with a USB SDR.
    #[default]
    LocalSoapy,
    /// Open a SoapyRemote device. Composed args end up as
    /// `driver=remote,remote=<host>:<port>` plus any extras from
    /// `remote_extra_args`. Requires `SoapySDRServer` on the remote host.
    SoapyRemote,
    /// Open a native rtl_tcp connection. Routes through the
    /// `RtlTcpSdr` backend rather than SoapySDR. Requires an
    /// `rtl_tcp` server on the remote host.
    RtlTcpRemote,
}

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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum AnalogFallbackMode {
    /// Never route the analog FM demod to the speakers. This is the
    /// DXer / silence-as-cue mode.
    #[default]
    DigitalOnly,
    /// Use the full HD → analog ladder: HD audio while synced, then
    /// analog stereo, then mono, then squelch.
    Automatic,
    /// Force the analog-FM demod to own the audio sink and ignore HD.
    AnalogOnly,
}

impl AnalogFallbackMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::DigitalOnly => "Digital Only",
            Self::Automatic => "Automatic",
            Self::AnalogOnly => "Analog Only",
        }
    }

    pub fn is_analog_audible(self) -> bool {
        !matches!(self, Self::DigitalOnly)
    }
}

/// Phase 4 — Opus 96 kbps recording mode. Persisted so the user's
/// choice survives across sessions. Chunk 4.3 wires Off and
/// Continuous; Chunk 4.4 adds the PerSong PSD-split logic on top of
/// the same `RecordingSession` lifecycle.
///
/// As of 0.4.x there's only effectively one mode: recording is
/// always available when a stream is up, and arms when the user
/// clicks the Rec button on the Tuner panel. The enum is retained
/// only for backward compatibility with existing on-disk configs;
/// the default is now `On` so users who never touched the legacy
/// dropdown get sensible behavior.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum RecordingMode {
    /// Legacy "disabled" state. The Rec button no longer honors this
    /// — kept only so we don't break deserialization of old configs.
    Off,
    /// Record the locked subchannel continuously, rotating to a
    /// fresh file whenever `recording_max_minutes` is reached. The
    /// old `per_song` and `continuous` config values both
    /// deserialize to this — PSD timing on real-world stations is
    /// too irregular for reliable song-boundary splits, so they're
    /// folded into one "just record" mode.
    #[default]
    #[serde(alias = "per_song", alias = "continuous")]
    On,
}

impl RecordingMode {
    /// Short label for the Settings dropdown.
    // Kept: pairs with the recording-mode Settings dropdown, which is
    // wired in `App` but not yet emitted from the dock (see
    // `UiCommand::SetRecordingMode`).
    #[allow(dead_code)]
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
    /// **Legacy (pre-0.5.0).** Kept only so old configs deserialize.
    /// Migrated into `sdr.transport` + `sdr.remote_host` + `sdr.remote_port`
    /// by [`migrate_legacy_sdr`] and never written back to disk.
    #[serde(default, skip_serializing)]
    pub use_rtl_tcp: bool,
    /// **Legacy (pre-0.3.0).** Migrated into `sdr.device_args`
    /// (`device=N`) by [`migrate_legacy_sdr`] and never written back.
    #[serde(default, skip_serializing)]
    pub rtl_device_index: u32,
    /// **Legacy (pre-0.5.0).** Migrated into `sdr.remote_host`.
    #[serde(default = "default_rtl_tcp_host", skip_serializing)]
    pub rtl_tcp_host: String,
    /// **Legacy (pre-0.5.0).** Migrated into `sdr.remote_port`.
    #[serde(default = "default_rtl_tcp_port", skip_serializing)]
    pub rtl_tcp_port: u16,
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
    /// v0.6.0 amplitude-pre-stage RMS target override (dBFS). `None`
    /// lets the per-device profile pick (e.g. −20 dBFS for RTL-SDR,
    /// −22 dBFS for SDRplay). `Some(x)` overrides for the next tune
    /// (cold-start cache-miss path only — cache HITs skip AmpProbe).
    /// Range −30 to −10 dBFS at the UI; values outside that are
    /// clamped at apply time. Persisted so users can pin a value
    /// across restarts without rebuilding.
    #[serde(default)]
    pub agc_amp_target_dbfs_override: Option<f32>,
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
    /// Number of preset slots rendered on the Tuner panel. Range is
    /// clamped to 1..=48 at apply time so a hand-edited config can't
    /// blow up the layout. Default 6 matches the original hardcoded
    /// value before this was made configurable.
    #[serde(default = "default_preset_slot_count")]
    pub preset_slot_count: u32,
    /// Enable spectrum-line smoothing in the Spectrum panel.
    /// When false, smoothing alpha is effectively forced to 1.0.
    #[serde(default)]
    pub spectrum_smoothing_enabled: bool,
    /// EMA alpha used for spectrum-line smoothing.
    /// 1.0 = no smoothing, 0.1 = strongest smoothing.
    #[serde(default = "default_spectrum_smoothing_alpha")]
    pub spectrum_smoothing_alpha: f32,
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
    /// Content hashes of album-art images the user has permanently blocked
    /// from the collage. Any image whose hash appears here is silently
    /// rejected on arrival — the filename doesn't matter.
    ///
    /// Stored as `i64` even though the live value is a `u64`: TOML integers
    /// are signed 64-bit, so a `u64` hash above `i64::MAX` can't be
    /// serialized and would silently drop the block (and abort the whole
    /// config write). The app bit-casts `u64` <-> `i64` at the boundary,
    /// which is lossless — a high hash just round-trips as a negative
    /// integer, and existing positive entries are unaffected.
    #[serde(default)]
    pub art_blocklist: Vec<i64>,
    /// Linux fallback: when true, a secondary-click on a collage tile can
    /// trigger block directly even if the context-menu popup fails to appear
    /// on some compositor/window-manager combinations.
    #[serde(default = "default_collage_secondary_click_fallback")]
    pub collage_secondary_click_fallback: bool,
    /// Analog-FM fallback source selection. `DigitalOnly` keeps the analog
    /// path silent, `Automatic` uses the HD → analog ladder, and
    /// `AnalogOnly` forces the analog demod to own the sink.
    #[serde(default)]
    pub analog_fallback_mode: AnalogFallbackMode,
    /// When true, the analog path decodes stereo width and blends into
    /// stereo audio when the pilot is strong. False forces mono output.
    #[serde(default = "default_true")]
    pub analog_fallback_stereo: bool,
    /// When true, the analog path decodes the 57 kHz RDS subcarrier and
    /// surfaces Program Service text in the UI.
    #[serde(default = "default_true")]
    pub analog_fallback_rds_enabled: bool,
    /// Legacy flag kept only for config migration from the old boolean
    /// surface. The new `analog_fallback_mode` drives runtime behavior.
    #[serde(default, skip_serializing)]
    pub analog_fallback_enabled: bool,
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
    /// Selected transport. Determines how the in-process pipeline
    /// opens the I/Q source: a local Soapy device, a SoapyRemote
    /// connection, or a native rtl_tcp server.
    #[serde(default)]
    pub transport: SdrTransport,
    #[serde(default = "default_sdr_driver")]
    pub driver: String,
    #[serde(default)]
    pub device_args: String,
    #[serde(default)]
    pub freq_correction_ppm: f64,
    #[serde(default)]
    pub gains: BTreeMap<String, f64>,
    /// Selected antenna input name (Soapy element). `None` =
    /// "let the device profile's `default_antenna` decide, or fall
    /// back to whatever the driver enumerates first". Persisted so
    /// users with multi-input SDRplay RSPs don't have to re-pick the
    /// antenna on every launch. The Tuner panel only surfaces the
    /// dropdown when the live SDR reports more than one antenna.
    #[serde(default)]
    pub antenna: Option<String>,
    /// Host for `SoapyRemote` / `RtlTcpRemote` transports. Ignored
    /// when `transport == LocalSoapy`.
    #[serde(default)]
    pub remote_host: Option<String>,
    /// Port for `SoapyRemote` / `RtlTcpRemote` transports. Ignored
    /// when `transport == LocalSoapy`. Defaults applied per-transport
    /// by [`SdrConfigSection::effective_remote_port`].
    #[serde(default)]
    pub remote_port: Option<u16>,
    /// Optional trailing args appended to the SoapyRemote args string
    /// (e.g. `"remote:driver=rtlsdr"`). Power-user escape hatch; the
    /// UI fills the common `host:port` case via the dedicated fields
    /// above. Ignored for non-SoapyRemote transports.
    #[serde(default)]
    pub remote_extra_args: Option<String>,
}

fn default_sdr_driver() -> String {
    "rtlsdr".to_string()
}

impl Default for SdrConfigSection {
    fn default() -> Self {
        Self {
            transport: SdrTransport::default(),
            driver: default_sdr_driver(),
            device_args: String::new(),
            freq_correction_ppm: 0.0,
            gains: BTreeMap::new(),
            antenna: None,
            remote_host: None,
            remote_port: None,
            remote_extra_args: None,
        }
    }
}

impl SdrConfigSection {
    /// Build the full SoapySDR args string for `Device::new`. Composes
    /// based on the active transport:
    ///
    /// * `LocalSoapy`   → `driver=<driver>[,device_args]`
    /// * `SoapyRemote`  → `driver=remote,remote=<host>:<port>[,remote_extra_args]`
    /// * `RtlTcpRemote` → also returns the LocalSoapy form, but callers
    ///   on this transport should open via the
    ///   `RtlTcpSdr` backend instead of SoapySDR.
    ///   This string is unused in that path but kept
    ///   deterministic so logging / display still works.
    pub fn to_args_string(&self) -> String {
        match self.transport {
            SdrTransport::SoapyRemote => {
                let host = self.effective_remote_host();
                let port = self.effective_remote_port();
                let mut s = format!("driver=remote,remote={host}:{port}");
                if let Some(extra) = self.remote_extra_args.as_ref() {
                    let extra = extra.trim();
                    if !extra.is_empty() {
                        s.push(',');
                        s.push_str(extra);
                    }
                }
                s
            }
            SdrTransport::LocalSoapy | SdrTransport::RtlTcpRemote => {
                if self.device_args.trim().is_empty() {
                    format!("driver={}", self.driver)
                } else {
                    format!("driver={},{}", self.driver, self.device_args.trim())
                }
            }
        }
    }

    /// Transport-aware short label suitable for the top-bar chip
    /// (e.g. "sdrplay", "remote", "rtl_tcp"). Unlike `driver` this
    /// reflects what's actually being talked to right now — when the
    /// user switches to `rtl_tcp` the chip should stop saying
    /// "sdrplay" even though that's still the last-bound local
    /// driver.
    pub fn chip_label(&self) -> String {
        match self.transport {
            SdrTransport::LocalSoapy => self.driver.clone(),
            SdrTransport::SoapyRemote => "remote".to_string(),
            SdrTransport::RtlTcpRemote => "rtl_tcp".to_string(),
        }
    }

    /// Transport-aware connection summary suitable for the Settings
    /// modal's right-hand-side `code(...)` readout. Unlike
    /// `to_args_string()` this NEVER returns the SoapySDR
    /// `driver=sdrplay,...` form when the active transport is
    /// `rtl_tcp` — that was technically correct (it's the local
    /// driver that's "selected") but it confused users who saw
    /// `driver=sdrplay` while clearly streaming over the network.
    pub fn display_connection_string(&self) -> String {
        match self.transport {
            SdrTransport::SoapyRemote | SdrTransport::LocalSoapy => self.to_args_string(),
            SdrTransport::RtlTcpRemote => {
                format!(
                    "rtl_tcp://{}:{}",
                    self.effective_remote_host(),
                    self.effective_remote_port(),
                )
            }
        }
    }

    /// Resolved remote host string, falling back to `127.0.0.1` when
    /// the user hasn't filled the field. Trims whitespace.
    pub fn effective_remote_host(&self) -> String {
        self.remote_host
            .as_ref()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "127.0.0.1".to_string())
    }

    /// Resolved remote port, with per-transport defaults: `1234` for
    /// `RtlTcpRemote`, `55132` for `SoapyRemote`, `0` otherwise (the
    /// caller is expected to ignore the value on `LocalSoapy`).
    pub fn effective_remote_port(&self) -> u16 {
        if let Some(p) = self.remote_port {
            if p != 0 {
                return p;
            }
        }
        match self.transport {
            SdrTransport::RtlTcpRemote => 1234,
            SdrTransport::SoapyRemote => 55132,
            SdrTransport::LocalSoapy => 0,
        }
    }
}

fn default_volume() -> f32 {
    1.0
}

fn default_collage_max_tiles() -> u32 {
    64
}

fn default_collage_secondary_click_fallback() -> bool {
    false
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

fn default_rtl_tcp_host() -> String {
    "127.0.0.1".to_string()
}

fn default_rtl_tcp_port() -> u16 {
    1234
}

fn default_preset_slot_count() -> u32 {
    6
}

fn default_spectrum_smoothing_alpha() -> f32 {
    0.5
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
            collage_max_tiles: 64,
            gain_mode: GainMode::Auto,
            manual_gain_tenths: 197,
            agc_amp_target_dbfs_override: None,
            play_log_retention_hours: 24,
            sdr: SdrConfigSection::default(),
            show_hd5_hd8: false,
            preset_slot_count: default_preset_slot_count(),
            spectrum_smoothing_enabled: false,
            spectrum_smoothing_alpha: default_spectrum_smoothing_alpha(),
            recording_mode: RecordingMode::Off,
            recording_dir: None,
            recording_max_minutes: 60,
            recording_subfolder_per_station: true,
            art_blocklist: Vec::new(),
            collage_secondary_click_fallback: default_collage_secondary_click_fallback(),
            analog_fallback_mode: AnalogFallbackMode::default(),
            analog_fallback_stereo: true,
            analog_fallback_rds_enabled: true,
            analog_fallback_enabled: false,
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
    cfg.play_log_retention_hours = crate::play_log::clamp_retention(cfg.play_log_retention_hours);
    cfg.spectrum_smoothing_alpha =
        ((cfg.spectrum_smoothing_alpha.clamp(0.1, 1.0) * 10.0).round() / 10.0).clamp(0.1, 1.0);
    if cfg.analog_fallback_enabled && cfg.analog_fallback_mode == AnalogFallbackMode::default() {
        cfg.analog_fallback_mode = AnalogFallbackMode::Automatic;
    }
    migrate_legacy_sdr(cfg);
}

/// Migrate the pre-0.3.0 / pre-0.5.0 SDR config layout into the new
/// `[sdr]` section. Triggered on every load — idempotent in steady
/// state because we only fill in fields the user hasn't already configured.
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
/// * If `use_rtl_tcp == true` AND the user hasn't already picked a
///   modern transport, set `sdr.transport = RtlTcpRemote` and copy
///   the legacy `rtl_tcp_host` / `rtl_tcp_port` into the new
///   `sdr.remote_host` / `sdr.remote_port` fields.
///
/// The legacy fields are kept on `AppConfig` as `skip_serializing`
/// stubs so old files still deserialize, but never round-trip back
/// to disk — the only canonical runtime source of truth is the `[sdr]`
/// section.
fn migrate_legacy_sdr(cfg: &mut AppConfig) {
    // Populate device_args from rtl_device_index if the user hasn't
    // set anything explicit. We use the heuristic "default driver AND
    // empty device_args" rather than just "empty device_args" so the
    // first migration cleanly seeds, and subsequent loads (where the
    // user has switched to e.g. driver=sdrplay) leave the args alone.
    if cfg.sdr.driver == "rtlsdr" && cfg.sdr.device_args.is_empty() && cfg.rtl_device_index > 0 {
        cfg.sdr.device_args = format!("device={}", cfg.rtl_device_index);
    }

    // Seed a TUNER gain entry from the legacy manual_gain_tenths if
    // gains is empty. Skipped when gain_mode == Auto since the AGC
    // controller manages the value anyway. Stored as dB (not tenths)
    // to match the new section's convention.
    if cfg.sdr.gains.is_empty() && cfg.gain_mode == GainMode::Manual {
        cfg.sdr
            .gains
            .insert("TUNER".to_string(), cfg.manual_gain_tenths as f64 / 10.0);
    }

    // Legacy rtl_tcp → modern transport. Only fires when the user
    // hadn't already chosen a modern transport (so re-loads of an
    // already-migrated config are a no-op).
    if cfg.use_rtl_tcp && cfg.sdr.transport == SdrTransport::LocalSoapy {
        cfg.sdr.transport = SdrTransport::RtlTcpRemote;
        if cfg.sdr.remote_host.is_none() {
            let host = cfg.rtl_tcp_host.trim();
            if !host.is_empty() {
                cfg.sdr.remote_host = Some(host.to_string());
            }
        }
        if cfg.sdr.remote_port.is_none() && cfg.rtl_tcp_port != 0 {
            cfg.sdr.remote_port = Some(cfg.rtl_tcp_port);
        }
        eprintln!(
            "[config] migrated legacy use_rtl_tcp=true → sdr.transport=rtl_tcp_remote \
             (host={}, port={}). Legacy fields will no longer be written back.",
            cfg.sdr.effective_remote_host(),
            cfg.sdr.effective_remote_port()
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

    let raw = match toml::to_string_pretty(cfg) {
        Ok(raw) => raw,
        Err(e) => {
            eprintln!("[config] failed to serialize config, not saving: {e}");
            return;
        }
    };

    let _ = fs::write(path, raw);
}

#[cfg(test)]
mod tests {
    #![allow(clippy::field_reassign_with_default)] // test fixtures build configs field-by-field for readability
    use super::*;

    #[test]
    fn legacy_use_rtl_tcp_migrates_to_transport() {
        let raw = r#"
            frequency_mhz = 101.1
            selected_program = 0
            dark_mode = true
            use_rtl_tcp = true
            rtl_device_index = 0
            rtl_tcp_host = "192.168.1.50"
            rtl_tcp_port = 1234
        "#;
        let mut cfg: AppConfig = toml::from_str(raw).expect("parse legacy config");
        sanitize(&mut cfg);
        assert_eq!(cfg.sdr.transport, SdrTransport::RtlTcpRemote);
        assert_eq!(cfg.sdr.remote_host.as_deref(), Some("192.168.1.50"));
        assert_eq!(cfg.sdr.remote_port, Some(1234));
    }

    #[test]
    fn legacy_rtl_device_index_migrates_to_device_args() {
        let raw = r#"
            frequency_mhz = 101.1
            selected_program = 0
            dark_mode = true
            use_rtl_tcp = false
            rtl_device_index = 2
            rtl_tcp_host = "127.0.0.1"
            rtl_tcp_port = 1234
        "#;
        let mut cfg: AppConfig = toml::from_str(raw).expect("parse legacy config");
        sanitize(&mut cfg);
        assert_eq!(cfg.sdr.transport, SdrTransport::LocalSoapy);
        assert_eq!(cfg.sdr.device_args, "device=2");
    }

    #[test]
    fn already_migrated_config_is_idempotent() {
        let raw = r#"
            frequency_mhz = 101.1
            selected_program = 0
            dark_mode = true
            use_rtl_tcp = true
            rtl_device_index = 0
            rtl_tcp_host = "127.0.0.1"
            rtl_tcp_port = 1234

            [sdr]
            transport = "soapy_remote"
            driver = "rtlsdr"
            remote_host = "10.0.0.5"
            remote_port = 55132
        "#;
        let mut cfg: AppConfig = toml::from_str(raw).expect("parse modern config");
        sanitize(&mut cfg);
        // Modern transport already set — the legacy migration must NOT
        // clobber it back to RtlTcpRemote.
        assert_eq!(cfg.sdr.transport, SdrTransport::SoapyRemote);
        assert_eq!(cfg.sdr.remote_host.as_deref(), Some("10.0.0.5"));
        assert_eq!(cfg.sdr.remote_port, Some(55132));
    }

    #[test]
    fn save_drops_legacy_fields() {
        let mut cfg = AppConfig::default();
        cfg.use_rtl_tcp = true;
        cfg.rtl_device_index = 7;
        let raw = toml::to_string_pretty(&cfg).expect("serialize default config");
        assert!(
            !raw.contains("use_rtl_tcp"),
            "use_rtl_tcp must not round-trip back to disk; raw was:\n{raw}"
        );
        assert!(
            !raw.contains("rtl_device_index"),
            "rtl_device_index must not round-trip; raw was:\n{raw}"
        );
        assert!(
            !raw.contains("rtl_tcp_host"),
            "rtl_tcp_host must not round-trip; raw was:\n{raw}"
        );
        assert!(
            !raw.contains("rtl_tcp_port"),
            "rtl_tcp_port must not round-trip; raw was:\n{raw}"
        );
    }

    #[test]
    fn analog_fallback_mode_round_trips_through_toml() {
        let mut cfg = AppConfig::default();
        cfg.analog_fallback_mode = AnalogFallbackMode::AnalogOnly;
        cfg.analog_fallback_stereo = false;
        cfg.analog_fallback_rds_enabled = false;

        let raw = toml::to_string_pretty(&cfg).expect("serialize config");
        assert!(raw.contains("analog_fallback_mode = \"analog_only\""));
        assert!(raw.contains("analog_fallback_stereo = false"));
        assert!(raw.contains("analog_fallback_rds_enabled = false"));

        let decoded: AppConfig = toml::from_str(&raw).expect("deserialize config");
        assert_eq!(decoded.analog_fallback_mode, AnalogFallbackMode::AnalogOnly);
        assert!(!decoded.analog_fallback_stereo);
        assert!(!decoded.analog_fallback_rds_enabled);
    }

    #[test]
    fn soapy_remote_args_string() {
        let mut sdr = SdrConfigSection::default();
        sdr.transport = SdrTransport::SoapyRemote;
        sdr.remote_host = Some("192.168.1.20".to_string());
        sdr.remote_port = Some(55132);
        assert_eq!(
            sdr.to_args_string(),
            "driver=remote,remote=192.168.1.20:55132"
        );
    }

    #[test]
    fn soapy_remote_args_string_falls_back_to_loopback() {
        let mut sdr = SdrConfigSection::default();
        sdr.transport = SdrTransport::SoapyRemote;
        // remote_host left None → fallback to 127.0.0.1
        assert_eq!(sdr.to_args_string(), "driver=remote,remote=127.0.0.1:55132");
    }

    #[test]
    fn soapy_remote_args_string_with_extras() {
        let mut sdr = SdrConfigSection::default();
        sdr.transport = SdrTransport::SoapyRemote;
        sdr.remote_host = Some("rs.local".to_string());
        sdr.remote_port = Some(1337);
        sdr.remote_extra_args = Some("remote:driver=rtlsdr".to_string());
        assert_eq!(
            sdr.to_args_string(),
            "driver=remote,remote=rs.local:1337,remote:driver=rtlsdr"
        );
    }

    #[test]
    fn local_soapy_args_string_unchanged() {
        let mut sdr = SdrConfigSection::default();
        sdr.driver = "sdrplay".to_string();
        sdr.device_args = "serial=02000001".to_string();
        assert_eq!(sdr.to_args_string(), "driver=sdrplay,serial=02000001");
    }
}
