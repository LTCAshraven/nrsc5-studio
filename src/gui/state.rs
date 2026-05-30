use std::time::{Duration, Instant};

pub use crate::maps::WeatherFrame;
use crate::config::GainMode;
use crate::dsp::{AgcSnapshot, SpectrumSnapshot, SpectrumTap};
use crate::sdr::{DeviceInfo, GainElement};
use crate::station_info::StationInfo;

/// Lightweight mirror of the live `RecordingSession` for the GUI to
/// read every frame. Lives on `AppState` so the dock can render a
/// "● REC HD<N> — 0:34" pill without touching the actual
/// `RecordingSession` handle (which owns a `JoinHandle` and is
/// neither `Clone` nor cheaply borrowable from the dock layer).
/// Updated by `App::update` whenever the recording session is
/// started, stopped, or rotated to a new file.
#[derive(Debug, Clone)]
pub struct RecordingStatus {
    /// HD subchannel (0..=7) currently being recorded. Independent of
    /// `active_speaker` — the user can be listening to HD2 while
    /// recording HD1, or vice versa.
    pub program: u32,
    /// Wall-clock moment the *current file* was opened. Drives the
    /// elapsed-time readout in the REC pill (e.g. "0:34") and is
    /// consulted by the file-rotation logic against
    /// `recording_max_minutes`.
    pub started_at: Instant,
    /// Path to the .opus file currently being written. Surfaced via
    /// the dock's hover-text and in the post-stop "saved to ..."
    /// toast.
    pub output_path: String,
}

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

/// Per-HD-program runtime metadata. One slot per subchannel (HD1–HD8)
/// in [`AppState::programs`]. Holds everything that has to track the
/// active speaker independently of the others: now-playing PSD
/// (title / artist / album / genre + each field's freshness
/// timestamp), and the live cover-art path for that subchannel.
///
/// Station-level metadata (call sign, slogan, location, etc.) and
/// the global album-art collage / heat-map stay on `AppState` directly
/// — those are shared across all subchannels of one station and don't
/// need per-program duplication.
#[derive(Default, Debug, Clone)]
pub struct ProgramRuntime {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub genre: String,
    /// Wall-clock time the `title` field was last refreshed by a
    /// `NrscEvent::Metadata` event. `None` whenever no value has
    /// been observed since the last retune / Stop. Per-field
    /// freshness lets the Station Information panel fade out
    /// individual rows (some stations drop Genre/Album between
    /// songs while still pushing Title/Artist).
    pub title_updated: Option<Instant>,
    pub artist_updated: Option<Instant>,
    pub album_updated: Option<Instant>,
    pub genre_updated: Option<Instant>,
    /// Full path to the current cover-art image for this program,
    /// if any. Set by `record_album_art` via
    /// `NrscEvent::Xhdr { param: 0, .. }`. Cleared by retune / Stop.
    pub cover_art_path: Option<String>,
}

impl ProgramRuntime {
    /// Clear every field. Called on retune / Stop / LostDevice so a
    /// stale program slot doesn't carry song metadata into the next
    /// session.
    pub fn clear(&mut self) {
        self.title.clear();
        self.artist.clear();
        self.album.clear();
        self.genre.clear();
        self.title_updated = None;
        self.artist_updated = None;
        self.album_updated = None;
        self.genre_updated = None;
        self.cover_art_path = None;
    }
}

#[derive(Default)]
pub struct AppState {
    pub frequency_mhz: f32,
    pub selected_program: u32,
    pub dark_mode: bool,
    pub active_program: u32,
    pub is_streaming: bool,
    /// True while nrsc5 reports OFDM sync against the current station.
    /// Distinct from `is_streaming` (which only means the child process
    /// is up) and from `lost_sync_at` (which is `Some` only *after* a
    /// known sync was lost). Used by [`AppState::available_programs`]
    /// to gate the implicit HD1 light — a station with no HD signal
    /// at all never sets this true, so HD1 stays dark.
    pub currently_synced: bool,
    /// Wall-clock time we last received `NrscEvent::LostSync` since the
    /// most recent `Sync`. `None` when we're currently synced (or have
    /// never synced). Combined with [`AppState::LOST_SYNC_GRACE`] this
    /// distinguishes brief sync fades (don't blank the UI) from
    /// sustained loss of signal (blank advertised-but-now-unreachable
    /// HD subchannel buttons).
    pub lost_sync_at: Option<Instant>,
    /// Aggregated SIS data for the currently tuned station — call sign,
    /// slogan, location, per-program info, data services, etc. Populated
    /// by the event dispatcher in `app.rs`; rendered by the Station
    /// Information panel (0.3.5). Reset on every retune.
    pub station_info: StationInfo,
    /// Broadcaster call sign (e.g. "KEGL"), derived from LOT filenames.
    /// Distinct from `station_info.call_sign` (which is the SIS-reported
    /// `Station name:` line). Used by the play log heuristics and as a
    /// fallback when SIS hasn't arrived yet.
    pub call_sign: String,
    /// Per-program runtime metadata, indexed 0..=7 (HD1..HD8). Phase
    /// 3 Chunk 4 moved the PSD (`title` / `artist` / `album` /
    /// `genre`) and live cover-art path off the top-level `AppState`
    /// and into here so multi-decoder sessions can keep each
    /// subchannel's now-playing state independent. The GUI reads
    /// from `programs[active_idx()]`; the event handlers in
    /// `app.rs` write into the slot indicated by the originating
    /// `NrscEvent`'s `program` field.
    pub programs: [ProgramRuntime; 8],
    /// HD program currently routed to the speakers, polled every
    /// frame from `Nrsc5Process::active_speaker()`. `None` when no
    /// piped session is active. Used by [`Self::active_idx`] to
    /// pick which `programs[]` slot the Now Playing panel renders.
    pub active_speaker: Option<u32>,
    /// Per-subchannel "decoder is running" gate, mirrored from
    /// `Nrsc5Process::decoded_programs()` every frame. Drives the
    /// toggle-switch state under each HD button so the GUI shows
    /// reality, not the user's last click intent (e.g. an
    /// `add_decoder` that failed because the cap was reached).
    pub decoded: [bool; 8],
    /// User setting: when true the HD program selector renders a
    /// second row exposing HD5..HD8. Default false because most
    /// stations only advertise up to HD4 and the extra row eats
    /// vertical space in the dock. Persisted via
    /// `AppConfig::show_hd5_hd8`.
    pub show_hd5_hd8: bool,
    /// User setting: when true the per-frame reconcile loop in
    /// `App::update` auto-spawns a background decoder for every
    /// subchannel SIS advertises. Independent of `active_speaker`
    /// (which one is currently routed to the speakers); the user
    /// can still flip individual toggles off after the auto-add.
    /// Persisted via `AppConfig::auto_decode_all_advertised`.
    pub auto_decode_all_advertised: bool,
    /// Number of preset slots the Tuner panel renders. Range 1..=48
    /// clamped at apply time. Mirror of `AppConfig::preset_slot_count`
    /// so the dock doesn't take a borrow on config every frame.
    pub preset_slot_count: u32,
    /// Mirror of `AppConfig::recording_mode` so the dock can disable
    /// the Record button when set to `Off` without taking a borrow
    /// on the config. Updated every frame by `App::update`.
    pub recording_mode: crate::config::RecordingMode,
    /// Per-subchannel "we already tried to auto-spawn this one"
    /// guard. Set inside the reconcile loop the first time a slot
    /// goes from "advertised + not decoded" to an `add_decoder`
    /// call, regardless of whether the call succeeded. Prevents the
    /// reconcile loop from hammering `add_decoder` every frame on
    /// a station that legitimately can't allocate another decoder
    /// (e.g. MAX_DECODERS already hit). Cleared on Stop / TuneMhz
    /// so a re-tune gets a fresh shot at every subchannel.
    pub auto_add_attempted: [bool; 8],
    /// Phase 4 — lightweight mirror of the live `RecordingSession` (if
    /// any) for the GUI to read. The session itself lives on `App`
    /// alongside `nrsc5` because it owns non-clone resources (the
    /// encoder JoinHandle and the crossbeam sender into the
    /// `SpeakerRouter`); only the surfaceable fields are mirrored
    /// here so the dock can render a "● REC HD<N> — 0:34" pill
    /// without taking a borrow on `App`.
    pub recording: Option<RecordingStatus>,
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
    /// Current output volume for the in-process audio player (0.0..=1.0).
    /// Applied via wait-free atomic store; no session probing needed.
    pub volume: f32,
    /// Mute state for the in-process audio player.
    pub muted: bool,
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
    /// True iff the Windows `SDRplayAPIService` is installed on this
    /// machine but isn't currently running. Set from the background
    /// SDR probe; consumed by the no-SDR overlay to surface a clear
    /// "start the service" hint instead of the generic "plug in a
    /// dongle" message. Always `false` on non-Windows platforms.
    pub sdrplay_service_stopped: bool,
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
    /// Which left-rail tab is currently active in the Settings modal.
    /// Persisted across opens (in-process only) so the user lands back
    /// where they left off — e.g. if they were tweaking gain and
    /// reopened to bump it further, they don't have to re-navigate to
    /// the Gain tab.
    pub settings_tab: SettingsTab,
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
    /// Antenna names enumerated from the live SDR. Empty when no
    /// stream is running OR the device only has a single (unnamed)
    /// input. The Tuner-panel antenna dropdown only renders when
    /// `len() > 1` so single-input dongles don't get a useless picker.
    pub sdr_antennas: Vec<String>,
    /// Currently-active antenna name on the live SDR (`Sdr::antenna`).
    /// Pre-selects the right entry in the Tuner-panel dropdown. `None`
    /// when no stream is running or the device doesn't expose antenna
    /// selection.
    pub active_antenna: Option<String>,
    /// Live edit buffer for the SDR Settings "Host" field. Must
    /// persist across frames so partial input isn't lost between
    /// keystrokes — egui's `TextEdit` only writes back the user's
    /// current keystroke to the backing string, so binding it to a
    /// per-frame `let` would discard everything on the next repaint.
    /// Seeded from `config.sdr.remote_host` on first modal open and
    /// whenever the user switches transport.
    pub sdr_remote_host_buf: String,
    /// Live edit buffer for the SDR Settings "Port" field. Same
    /// lifetime story as `sdr_remote_host_buf`.
    pub sdr_remote_port_buf: u16,
    /// Live edit buffer for the SDR Settings SoapyRemote "Extra args"
    /// field. Same lifetime story as `sdr_remote_host_buf`.
    pub sdr_remote_extra_buf: String,
    /// Tracks whether the remote-input buffers above have been seeded
    /// from `config.sdr` yet. `None` means they're still at their
    /// `Default::default()` values and need a one-shot seed from the
    /// loaded config before the modal first renders.
    pub sdr_remote_buf_seeded: bool,
}

impl AppState {
    /// Grace period after a `LostSync` during which the HD subchannel
    /// selector keeps its previously-observed lit buttons lit. Real HD
    /// Radio reception fades and recovers on the order of one to a few
    /// seconds in moving vehicles; greying out HD2..HD8 on every
    /// flicker would make the UI feel broken. Five seconds is long
    /// enough to ride out typical fades, short enough that a truly
    /// lost signal blanks the selector promptly.
    pub const LOST_SYNC_GRACE: Duration = Duration::from_secs(5);

    /// How long a PSD field is considered "fresh" after the last
    /// `NrscEvent::Metadata` update before the Station Information
    /// panel hides its rows. Most stations refresh PSD on every song
    /// change (every 2-4 minutes) but emit the four fields one line
    /// at a time spaced over a few seconds, so the timeout has to be
    /// generous enough to ride out the per-field staggering without
    /// flickering rows in and out. Fifteen seconds covers the typical
    /// title -> artist -> album -> genre roll-in while still hiding
    /// "stale" PSD between songs on stations that pause metadata.
    pub const PSD_STALE_AFTER: Duration = Duration::from_secs(15);

    /// True if we've been out of sync for longer than [`Self::LOST_SYNC_GRACE`].
    /// The dock uses this to decide whether the cached SIS program list
    /// should be considered stale for display purposes — the underlying
    /// `station_info` data is *not* cleared (step 12 is the dedicated
    /// lifecycle pass), only its UI surface dims.
    pub fn sync_data_stale(&self) -> bool {
        self.lost_sync_at
            .map(|t| t.elapsed() >= Self::LOST_SYNC_GRACE)
            .unwrap_or(false)
    }

    /// True if any PSD field on the **active program** has been
    /// updated within the [`Self::PSD_STALE_AFTER`] window. Used by
    /// the Station Information panel to decide whether to show the
    /// PSD section at all.
    pub fn psd_is_fresh(&self) -> bool {
        let p = &self.programs[self.active_idx()];
        Self::is_psd_field_fresh(p.title_updated)
            || Self::is_psd_field_fresh(p.artist_updated)
            || Self::is_psd_field_fresh(p.album_updated)
            || Self::is_psd_field_fresh(p.genre_updated)
    }

    /// True iff a per-field PSD timestamp is within
    /// [`Self::PSD_STALE_AFTER`]. `None` -> always stale.
    pub fn is_psd_field_fresh(ts: Option<Instant>) -> bool {
        ts.map(|t| t.elapsed() < Self::PSD_STALE_AFTER).unwrap_or(false)
    }

    /// Most recent PSD update across all four fields of the **active
    /// program**, used for the "PSD updated Xs ago" footer. `None`
    /// whenever no PSD has been observed since the last retune /
    /// Stop on the active subchannel.
    pub fn psd_latest_updated(&self) -> Option<Instant> {
        let p = &self.programs[self.active_idx()];
        [
            p.title_updated,
            p.artist_updated,
            p.album_updated,
            p.genre_updated,
        ]
        .into_iter()
        .flatten()
        .max()
    }

    /// Index into [`Self::programs`] whose metadata the GUI should
    /// render. Follows the active speaker while a piped session is
    /// running; falls back to `selected_program` otherwise so the
    /// Now Playing panel still shows whatever the user last tuned
    /// to (even with empty PSD slots) instead of always defaulting
    /// to HD1.
    pub fn active_idx(&self) -> usize {
        let idx = self.active_speaker.unwrap_or(self.selected_program) as usize;
        idx.min(self.programs.len() - 1)
    }

    /// Mutable reference to the per-program runtime metadata for
    /// the currently-active subchannel. Convenience for the small
    /// number of call sites that need to update the displayed PSD
    /// without first computing `active_idx()`.
    pub fn active_program_mut(&mut self) -> &mut ProgramRuntime {
        let idx = self.active_idx();
        &mut self.programs[idx]
    }

    /// Read-only counterpart to [`Self::active_program_mut`].
    pub fn active_program(&self) -> &ProgramRuntime {
        &self.programs[self.active_idx()]
    }

    /// Clear every per-program slot (PSD + cover art). Called on
    /// retune / Stop / LostDevice — the entire station's metadata is
    /// invalid when the underlying SDR pipeline restarts, regardless
    /// of which subchannels are decoded.
    pub fn clear_all_programs(&mut self) {
        for p in &mut self.programs {
            p.clear();
        }
    }

    /// Derived `[bool; 8]` indicating which HD subchannels should be
    /// rendered as "lit up" in the program selector (indices 0..7
    /// correspond to HD1..HD8).
    ///
    /// Derivation rules:
    /// - Not streaming → all `false` (the selector greys out entirely).
    /// - Out of sync past the grace window → all `false`.
    /// - HD1 is force-lit whenever we've been synced this session
    ///   (currently synced *or* within the LostSync grace window).
    ///   nrsc5 doesn't always emit a `SIG Service` line for the
    ///   implicit main program, so we light HD1 by reception, not by
    ///   SIS advertisement.
    /// - Otherwise per-slot: `true` for any slot the station has
    ///   advertised in SIS (i.e. `station_info.programs[i].is_some()`).
    pub fn available_programs(&self) -> [bool; 8] {
        if !self.is_streaming || self.sync_data_stale() {
            return [false; 8];
        }
        let mut out = [false; 8];
        // HD1 implicit-light rule: synced now, or synced recently
        // (within the grace window — `sync_data_stale()` already
        // returned false above, so any `lost_sync_at` value here is
        // fresh). Either way we have sync confidence on this station.
        if self.currently_synced || self.lost_sync_at.is_some() {
            out[0] = true;
        }
        for (i, slot) in self.station_info.programs.iter().enumerate() {
            if slot.is_some() {
                out[i] = true;
            }
        }
        out
    }
}

/// Render mode for the Log tab — chronological list of every play, or a
/// `(artist, title)`-grouped view sorted by play count.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LogViewMode {
    #[default]
    Timeline,
    TopPlayed,
}

/// Which left-rail tab is currently active in the Settings modal.
/// Defaults to `Connection` so a freshly-opened modal shows the
/// transport / host / port form first — the most common reason to
/// open the modal on first run.
///
/// Device picker + profile notes live inside the Connection tab
/// (below the transport selector) because they're conceptually part
/// of "where does the IQ come from" — splitting them across two tabs
/// fragmented the mental model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SettingsTab {
    #[default]
    Connection,
    Gain,
    Display,
    Recording,
}

impl SettingsTab {
    /// Short label shown in the left-rail nav. Includes a leading
    /// emoji glyph for quick visual scanning; matches the iconography
    /// used in the dock tab titles.
    pub fn label(self) -> &'static str {
        match self {
            SettingsTab::Connection => "\u{1F50C}  Connection",
            // The egui bundled NotoEmoji subset doesn't include
            // "level slider" (U+1F39A) or "classical building"
            // (U+1F3DB) — both rendered as tofu. U+1F509 (speaker
            // low volume) IS in the subset and reads naturally as
            // "audio level / gain".
            SettingsTab::Gain => "\u{1F509}  Gain",
            SettingsTab::Display => "\u{1F5A5}  Display",
            SettingsTab::Recording => "\u{1F534}  Recording",
        }
    }
}
