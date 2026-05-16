use std::time::Instant;

pub use crate::maps::WeatherFrame;

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

#[derive(Debug, Clone, Default)]
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
}
