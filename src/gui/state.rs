use std::time::Instant;

pub use crate::maps::WeatherFrame;
use crate::config::GainMode;
use crate::dsp::{AgcSnapshot, SpectrumSnapshot, SpectrumTap};
use crate::sdr::{DeviceInfo, GainElement};

/// One tile in the album-art heat-map collage: the file path to the image,
/// the number of times it has appeared, and the unique (title, artist) pairs
/// observed while it was on screen. Used to render hover tooltips.
#[derive(Debug, Clone, Default)]
pub struct ArtTile {
    pub path: String,
    pub count: u32,
    /// Unique (title, artist) pairs that have been displayed with this cover.
    pub songs: Vec<(String, String)>,
    /// Most recently observed album name for this cover, if any.
    pub album: String,
}

#[derive(Default)]
pub struct AppState {
    pub frequency_mhz: f32,
    pub selected_program: u32,
    pub dark_mode: bool,
    pub active_program: u32,
    pub is_streaming: bool,
    pub station_name: String,
    /// Short per-program station names from SIG Service, indexed by program (0..4).
    pub short_names: [String; 4],
    /// Broadcaster call sign (e.g. "KEGL"), derived from LOT filenames.
    pub call_sign: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub genre: String,
    pub mer: f32,
    /// MER on the lower OFDM sideband, in dB. Drives the left half of the
    /// constellation cloud.
    pub mer_lower: f32,
    /// MER on the upper OFDM sideband, in dB. Drives the right half of the
    /// constellation cloud.
    pub mer_upper: f32,
    pub ber: f32,
    pub agc_db: f32,
    pub startup_wait_s: f32,
    pub silence_s: f32,
    pub nrsc5_status: String,
    pub last_event: String,
    /// Full path to the current cover art image file, if any.
    pub cover_art_path: Option<String>,
    /// Full path to the station logo image file, if any.
    pub station_logo_path: Option<String>,
    /// Full path to the stitched traffic map image, if any.
    pub traffic_map_path: Option<String>,
    /// Ordered history of composited weather radar frames (oldest → newest).
    /// Each entry pairs a full path to the `WeatherMap_NNNN.png` snapshot with
    /// the wall-clock time it was captured.
    pub weather_frames: Vec<WeatherFrame>,
    /// Currently-displayed frame index into `weather_frames`.
    pub weather_current_frame: usize,
    /// True when the radar animation is auto-advancing.
    pub weather_playing: bool,
    /// Last time the radar animation advanced to the next frame.
    pub weather_last_advance: Option<Instant>,
    /// Heat-map tiles for the album-art collage, sorted by `count` descending.
    pub art_tiles: Vec<ArtTile>,
    /// Wall-clock time the current listening session began, used to enforce
    /// the 8-hour collage tracking horizon.
    pub art_session_started: Option<Instant>,
    /// Preset slot currently being edited via the popup (None = no popup).
    pub editing_preset: Option<usize>,
    /// In-progress name text for the preset editor.
    pub editing_preset_text: String,
    /// In-progress frequency value for the preset editor (MHz).
    pub editing_preset_freq: f32,
    /// In-progress subchannel value for the preset editor (0-indexed program).
    pub editing_preset_program: u32,
    /// True for the single frame after entering preset-edit mode so we can
    /// request focus once without trapping focus forever.
    pub editing_preset_just_opened: bool,
    /// Current output volume for the nrsc5 audio session (0.0..=1.0).
    pub volume: f32,
    /// Mute state for the nrsc5 audio session.
    pub muted: bool,
    /// True once the per-process audio session has been located. Slider is
    /// disabled until this becomes true.
    pub audio_session_ready: bool,
    /// Rolling ring buffer of synthesized QPSK constellation samples, in
    /// normalized symbol coordinates (ideal points at (±1, ±1)). Allocated
    /// lazily by the Constellation panel on first paint.
    pub constellation_samples: Vec<[f32; 2]>,
    /// Next write index into `constellation_samples` (circular).
    pub constellation_head: usize,
    /// Xorshift64 state for the constellation panel's sample generator.
    /// Seeded lazily on first paint.
    pub constellation_rng: u64,
    /// Smoothed per-sideband σ for the constellation cloud. nrsc5 only
    /// reports MER once per second; lerping these toward their target each
    /// frame turns a 1 Hz step into a smooth tightening/loosening of the
    /// cloud as signal quality changes.
    pub constellation_sigma_l: f32,
    pub constellation_sigma_u: f32,
    /// True iff we believe an RTL-SDR is currently attached. Defaults to
    /// `true` so the no-SDR overlay doesn't flash on launch before the
    /// first probe completes.
    pub sdr_present: bool,
    /// True iff `librtlsdr.dll` was loadable on this system. When `false`
    /// (e.g. the DLL is missing in a stripped-down install) the no-SDR
    /// overlay stays hidden — we'd rather show no warning than a wrong one.
    pub sdr_probe_available: bool,
    /// Last time we asked the probe how many SDRs are attached. Used to
    /// throttle the probe to roughly once every two seconds.
    pub sdr_last_probed: Option<Instant>,
    /// Mirror of `AppConfig.collage_max_tiles`. The Collage tab reads this
    /// to drive its tile-cap controls; the app updates it whenever the
    /// user changes the cap, then persists to config.
    pub collage_tile_cap: u32,
    /// Which view the Log tab is currently rendering.
    pub log_view_mode: LogViewMode,
    /// Transient status line shown next to the Log tab's Export button
    /// (e.g. the path the CSV was written to). Cleared by the next interaction.
    pub log_export_status: Option<String>,
    /// Optional FFT tap shared with the piped I/Q thread. `None` when no
    /// piped stream has been started yet, or when the backend failed to
    /// initialize. The Spectrum panel reads through this every paint.
    pub spectrum_tap: Option<SpectrumTap>,
    /// Reusable snapshot buffer for the Spectrum panel so it doesn't
    /// allocate on every paint.
    pub spectrum_snapshot: SpectrumSnapshot,
    /// Last generation drawn into `spectrum_texture`. Used to skip the
    /// per-frame texture re-upload when nothing has changed.
    pub spectrum_last_drawn_generation: u64,
    /// Cached GPU texture for the scrolling waterfall. Re-uploaded when
    /// `spectrum_last_drawn_generation` falls behind the tap.
    pub spectrum_texture: Option<egui::TextureHandle>,
    /// Latest snapshot of the closed-loop AGC controller, refreshed once
    /// per frame from `Nrsc5Process::agc_snapshot()`. `None` whenever no
    /// piped stream is active (USB / rtl_tcp backends don't run our AGC;
    /// `agc_db` carries nrsc5's own reading there).
    pub agc_snapshot: Option<AgcSnapshot>,
    /// User-selected tuner gain control mode. Mirrors
    /// `AppConfig.gain_mode`; the UI mutates this and the app pushes
    /// the change back into config on `UiCommand::SetGainMode`.
    pub gain_mode: GainMode,
    /// User-selected manual tuner gain in tenths of dB. Mirrors
    /// `AppConfig.manual_gain_tenths`. Only meaningful when
    /// `gain_mode == Manual`.
    pub manual_gain_tenths: i32,
    /// Gain mode actually in effect for the currently-running piped
    /// stream, or `None` when nothing is streaming on the piped backend.
    /// Compared against `gain_mode` to decide whether to show the
    /// "(restart stream to apply)" hint next to the dropdown.
    pub active_gain_mode: Option<GainMode>,
    /// Manual gain tenths actually in effect for the current piped
    /// stream. Compared against `manual_gain_tenths` for the same
    /// "restart to apply" purpose.
    pub active_manual_gain_tenths: Option<i32>,
    /// Set when the hamburger-menu's "SDR Settings" item is clicked.
    /// The modal is rendered as long as this is true; closed by the
    /// modal's own dismiss button or Esc key.
    pub show_sdr_settings: bool,
    /// Set when "About" is clicked from the hamburger menu.
    pub show_about: bool,
    /// Snapshot of enumerated SoapySDR devices, refreshed on
    /// `UiCommand::RefreshSdrDevices` and once when the SDR Settings
    /// modal is opened. Empty when no devices were found OR when the
    /// modal hasn't been opened yet this session.
    pub sdr_devices: Vec<DeviceInfo>,
    /// Snapshot of gain elements exposed by the device matching the
    /// active config args. Refreshed alongside `sdr_devices`. Empty
    /// when no device is currently configured or enumeration failed.
    /// The SDR Settings modal renders one slider per entry.
    pub sdr_gain_elements: Vec<GainElement>,
    /// Wall-clock time of the last `RefreshSdrDevices` apply. Used to
    /// throttle automatic refreshes and to show "Last refreshed Xs ago"
    /// in the modal.
    pub sdr_devices_last_refreshed: Option<Instant>,
}

/// Render mode for the Log tab — chronological list of every play, or a
/// `(artist, title)`-grouped view sorted by play count.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LogViewMode {
    #[default]
    Timeline,
    TopPlayed,
}
