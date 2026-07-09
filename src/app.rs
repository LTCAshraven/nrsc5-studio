// TODO(egui-0.34 migration): egui 0.34 deprecated several panel/menu APIs
// used below (TopBottomPanel/SidePanel, Ui::close_menu, Context::screen_rect,
// Panel::exact_width, Slider::clamp_to_range). Migrating them touches live UI
// layout and needs visual verification, so it's tracked as a separate change;
// silence the deprecation lint here in the meantime.
#![allow(deprecated)]

use crate::collage::CollageEngine;
use crate::config::{load_config, save_config, AppConfig};
use crate::ffi::{Nrsc5Process, NrscEvent};
use crate::gui::dock::{DockTab, DockViewer, UiCommand};
use crate::gui::state::{AppState, ArtTile, NowPlayingImageMode};
use crate::maps::{TrafficMap, WeatherMap};
use chrono::{Local, Utc};
use egui_dock::{DockArea, DockState, NodeIndex, NodePath, SurfaceIndex};
use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Result of one background SDR-presence probe. `rtl` is the librtlsdr
/// device count (cheap, always probed); `soapy` is the Soapy supported-
/// driver count (potentially slow, only probed when `rtl` came up
/// empty and we're not streaming); `sdrplay_service` is the Windows
/// `SDRplayAPIService` SCM state (cheap, always probed on Windows;
/// always `None` on other platforms). `None` distinguishes "probe
/// errored / unavailable / not applicable" from a real reading.
#[derive(Debug, Clone, Copy)]
struct SdrProbeResult {
    rtl: Option<u32>,
    soapy: Option<u32>,
    sdrplay_service: Option<crate::sdr_detect::SdrplayServiceState>,
}

#[derive(Debug)]
struct SdrDeviceRefreshResult {
    devices: Vec<crate::sdr::DeviceInfo>,
    gain_elements: Vec<crate::sdr::GainElement>,
    diagnostics: String,
}

/// Hard cap on tracked album-art tiles — prevents the collage from getting
/// unbounded if a session runs all day on a very busy station.
/// Hard cap on tracked album-art tiles — prevents the collage from getting
/// silly on long sessions and bounds memory + persistence cost. The actual
/// displayed cap is read from `AppConfig.collage_max_tiles` (1..=512,
/// snapped to a power of two) and may be lower than this.
const ART_TILES_HARD_MAX: usize = 512;

/// FM channel raster used by tuner inputs and tune commands.
/// US HD/FM centers are 200 kHz spaced and anchored to 87.9 MHz.
const FM_TUNE_MIN_MHZ: f32 = 87.9;
const FM_TUNE_MAX_MHZ: f32 = 107.9;
const FM_TUNE_STEP_MHZ: f32 = 0.2;
const FM_TUNE_BASE_MHZ: f32 = 87.9;

/// Resolve the user's preferred collage tile cap from config, clamping to
/// the supported range and snapping to the nearest power of two. The UI
/// only emits exact powers of two, so this only matters for hand-edited
/// `config.toml` files.
fn collage_tile_cap(cfg: &AppConfig) -> usize {
    (cfg.collage_max_tiles.max(1) as usize)
        .min(ART_TILES_HARD_MAX)
        .next_power_of_two()
        .min(ART_TILES_HARD_MAX)
}

/// Clamp + snap an FM frequency to the 200 kHz channel raster.
fn snap_fm_tune_mhz(mhz: f32) -> f32 {
    let clamped = mhz.clamp(FM_TUNE_MIN_MHZ, FM_TUNE_MAX_MHZ);
    let slots = ((clamped - FM_TUNE_BASE_MHZ) / FM_TUNE_STEP_MHZ).round();
    let snapped = FM_TUNE_BASE_MHZ + slots * FM_TUNE_STEP_MHZ;
    (snapped * 10.0).round() / 10.0
}

fn should_auto_refresh_sdr_devices(
    transport: crate::config::SdrTransport,
    refresh_in_flight: bool,
    devices_empty: bool,
) -> bool {
    matches!(transport, crate::config::SdrTransport::LocalSoapy)
        && !refresh_in_flight
        && devices_empty
}

fn pick_best_sdr_device<'a>(
    configured_driver: &str,
    configured_device_args: &str,
    devices: &'a [crate::sdr::DeviceInfo],
) -> Option<&'a crate::sdr::DeviceInfo> {
    if devices.is_empty() {
        return None;
    }

    if let Some(dev) = devices.iter().find(|dev| {
        dev.driver.eq_ignore_ascii_case(configured_driver)
            && dev
                .args_after_driver()
                .eq_ignore_ascii_case(configured_device_args)
    }) {
        return Some(dev);
    }

    if let Some(dev) = devices
        .iter()
        .find(|dev| dev.driver.eq_ignore_ascii_case("rtlsdr"))
    {
        return Some(dev);
    }

    if let Some(dev) = devices
        .iter()
        .find(|dev| dev.driver.eq_ignore_ascii_case("sdrplay"))
    {
        return Some(dev);
    }

    devices.first()
}

/// Snap an arbitrary tenths-of-dB gain value to the nearest entry in
/// the active device profile's gain table. The dongle (or its Soapy
/// driver) silently rounds off-table values to its nearest step, so
/// we do the same here and store the snapped value — keeps the UI
/// readout honest. Falls back to a no-op when the profile has an
/// empty/continuous table (the value is returned unchanged).
fn snap_to_gain_table(tenths: i32, table: &[i32]) -> i32 {
    if table.is_empty() {
        return tenths;
    }
    let mut best = table[0];
    let mut best_diff = (tenths - best).abs();
    for &step in &table[1..] {
        let d = (tenths - step).abs();
        if d < best_diff {
            best_diff = d;
            best = step;
        }
    }
    best
}

/// Replace characters Windows refuses in filenames (and a couple more
/// that confuse shells / players) with `_`. Used for the per-station
/// subfolder name when persisting recordings. Conservative: ASCII-safe
/// subset only — anything outside `[A-Za-z0-9._-]` becomes `_`.
fn sanitize_filename(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        out.push('_');
    }
    out
}

fn here_image_type_label(image_type: i32) -> &'static str {
    match image_type {
        crate::ffi::nrsc5_sys::NRSC5_HERE_IMAGE_TRAFFIC => "traffic",
        crate::ffi::nrsc5_sys::NRSC5_HERE_IMAGE_WEATHER => "weather",
        _ => "other",
    }
}

/// Rolling window for the album-art collage. Plays older than this are
/// pruned on every new event so the heat-map keeps moving instead of
/// freezing.
const ART_WINDOW: Duration = Duration::from_secs(8 * 60 * 60);
/// How long a raw LOT dump survives in the AAS scratch dir before the
/// background sweep deletes it. Active outputs (latest traffic map, the
/// rolling weather frames, base-map crops) are exempt regardless of age.
const AAS_RETENTION: Duration = Duration::from_secs(60 * 60);
/// Minimum gap between AAS scratch sweeps so we don't stat the directory
/// every UI frame.
const AAS_PRUNE_INTERVAL: Duration = Duration::from_secs(5 * 60);
/// Minimum time between successive *counted* plays of the exact same cover.
/// nrsc5 re-emits XHDR pointing at the same LOT image many times while a
/// song plays; without this cooldown, a 4-minute song can rack up dozens
/// of phantom plays. Stations almost never back-to-back two songs from the
/// same album, so a 4-minute floor is safe.
const ART_COUNT_COOLDOWN: Duration = Duration::from_secs(4 * 60);

/// One unique image observed in the rolling collage window.
#[derive(Debug, Clone)]
struct ArtEntry {
    path: String,
    /// Timestamps of every distinct play within the current window, oldest
    /// first. Pruned to `ART_WINDOW` on each new event.
    plays: VecDeque<Instant>,
    /// Unique (title, artist) pairs that have been displayed with this cover.
    /// Capped to avoid unbounded growth on chatty stations.
    songs: Vec<(String, String)>,
    /// Most recently observed album name for this cover, if any.
    album: String,
}

/// Bookkeeping that lets us put a closed panel back where it lived.
///
/// On every frame we record where each open tab is, and diff before/after
/// the dock area is shown. Tabs that disappeared are tracked here so that the
/// next click on the toolbar puts them back in (roughly) the same spot.
#[derive(Debug, Clone, Default)]
struct ClosedTabInfo {
    /// Exact leaf the tab lived in when it was closed.
    location: Option<NodePath>,
    /// Other tabs that shared the same leaf — first-tier fallback anchor.
    leaf_mates: Vec<DockTab>,
    /// Other tabs in the same surface (window) — second-tier fallback anchor.
    surface_mates: Vec<DockTab>,
}

/// Snapshot of where every open tab currently lives, used for frame-to-frame
/// diffing so we can detect tabs that disappeared from the dock state.
#[derive(Debug, Default)]
struct LayoutSnapshot {
    tabs: HashMap<DockTab, NodePath>,
    leaves: HashMap<(SurfaceIndex, NodeIndex), Vec<DockTab>>,
    surfaces: HashMap<SurfaceIndex, Vec<DockTab>>,
}

impl LayoutSnapshot {
    fn build(state: &DockState<DockTab>) -> Self {
        let mut snap = LayoutSnapshot::default();
        for (path, tab) in state.iter_all_tabs() {
            let np = NodePath {
                surface: path.surface,
                node: path.node,
            };
            snap.tabs.insert(tab.clone(), np);
            snap.leaves
                .entry((path.surface, path.node))
                .or_default()
                .push(tab.clone());
            snap.surfaces
                .entry(path.surface)
                .or_default()
                .push(tab.clone());
        }
        snap
    }
}

pub struct Nrsc5App {
    app_state: AppState,
    dock_state: DockState<DockTab>,
    config: AppConfig,
    nrsc5: Option<Nrsc5Process>,
    /// Background thread for retune (kill old process + start new).
    retune_task: Option<JoinHandle<(Nrsc5Process, Option<String>)>>,
    /// Phase 4 — live Opus recording session, if any. Locked to one
    /// HD subchannel (see `RecordingSession::program()`) independent
    /// of the active speaker. Set on `UiCommand::StartRecording`,
    /// cleared on `StopRecording` or on stream teardown
    /// (Stop / TuneMhz / decoder removal of the recorded program).
    recording_session: Option<crate::recorder::RecordingSession>,
    start_requested_at: Option<Instant>,
    last_signal_at: Option<Instant>,
    _collage: CollageEngine,
    /// Maps LOT ID → filename written in the AAS directory.
    lot_files: HashMap<String, String>,
    /// Path to the AAS dump directory.
    aas_dir: PathBuf,
    traffic_map: TrafficMap,
    weather_map: WeatherMap,
    /// In-process audio playback. Owns the cpal output stream and a
    /// clone-cheap `AudioSink` that gets handed to every `Nrsc5Process`
    /// for piped-mode PCM. Volume and mute are wait-free atomic stores
    /// on the sink; no more per-process session probing.
    audio_player: crate::audio::AudioPlayer,
    /// Histogram of unique album art images seen this session, keyed by a
    /// content hash so re-emissions of the same bytes — even under different
    /// LOT filenames — collapse into a single tile.
    art_history: HashMap<u64, ArtEntry>,
    /// Set of content hashes the user has permanently blocked from the
    /// collage. Populated from `config.art_blocklist` on startup and kept
    /// in sync with it on every mutation.
    art_blocklist: std::collections::HashSet<u64>,
    /// Persistent on-disk cache so the collage survives restarts. `None`
    /// only if the local data dir couldn't be resolved/created, in which
    /// case the collage falls back to in-memory-only behavior.
    art_cache: Option<crate::art_cache::ArtCache>,
    /// Last time we pruned expired plays from `art_history`. Throttled so
    /// we don't walk the map every UI frame.
    last_art_prune_at: Option<Instant>,
    /// Set when a cover-art event updates `art_history` so the (expensive)
    /// collage relayout + art-cache disk write can be coalesced to a single
    /// pass per frame instead of running once per event. Keeps a backlog
    /// catch-up (e.g. after the window was unfocused) from replaying every
    /// historical cover one relayout/write at a time.
    art_dirty: bool,
    /// Last time the collage tiles were relaid out. Used to defer the
    /// (visible) relayout during a large refocus catch-up so the collage
    /// jumps straight to the final layout instead of stepping through
    /// intermediate states, while bounding how long it can look stale under
    /// a sustained cover-art flood.
    last_art_rebuild_at: Option<Instant>,
    /// Last time we pruned stale raw LOT dumps from the AAS scratch dir.
    /// Throttled so we don't scan the directory every UI frame.
    last_aas_prune_at: Option<Instant>,
    /// Where each recently-closed panel used to live, so re-opening it from
    /// the toolbar restores it to (roughly) the same spot.
    closed_tab_locations: HashMap<DockTab, ClosedTabInfo>,
    /// Layout snapshot from the previous frame, used to detect tabs that
    /// were closed via the dock area's own "X" button.
    prev_layout: LayoutSnapshot,
    /// 24-hour rolling song log. Survives restarts via RON file under
    /// `%LOCALAPPDATA%\nrsc5-studio\play-log.ron`.
    play_log: crate::play_log::PlayLog,
    /// Unix-millis timestamp of the last new-play cover event. Used to
    /// gate play-log pushes from the metadata handler so station slogans
    /// (which arrive via title/artist updates but without a fresh cover)
    /// don't pollute the log.
    last_cover_play_at: Option<i64>,
    /// MPSC endpoints for background SDR-presence probes. The poll
    /// tick spawns a one-shot worker thread whenever a probe is due and
    /// no probe is currently in flight; the worker sends its result
    /// back on `sdr_probe_tx` and pokes egui via `Context::request_repaint`
    /// so the result is picked up promptly. Doing this on the GUI thread
    /// was the source of the "Not Responding" freeze on SDRplay
    /// hotplug: `soapysdr::enumerate("")` blocks for seconds while the
    /// SDRplay API service finishes its device-discovery handshake.
    sdr_probe_tx: mpsc::Sender<SdrProbeResult>,
    sdr_probe_rx: mpsc::Receiver<SdrProbeResult>,
    /// Set when a background probe is running, cleared when a result
    /// is drained from `sdr_probe_rx`. Prevents stacking multiple
    /// probes if one runs longer than the throttle interval.
    sdr_probe_in_flight: bool,
    /// Background SDR device-refresh work. The Settings modal opens
    /// immediately and the refresh result is applied on the next UI
    /// frame once the worker finishes.
    sdr_refresh_tx: mpsc::Sender<SdrDeviceRefreshResult>,
    sdr_refresh_rx: mpsc::Receiver<SdrDeviceRefreshResult>,
    sdr_refresh_in_flight: bool,
}

impl Nrsc5App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        egui_extras::install_image_loaders(&cc.egui_ctx);
        Self::install_fonts(&cc.egui_ctx);
        let mut config = load_config();
        let snapped_boot_freq = snap_fm_tune_mhz(config.frequency_mhz);
        if (config.frequency_mhz - snapped_boot_freq).abs() > f32::EPSILON {
            config.frequency_mhz = snapped_boot_freq;
            save_config(&config);
        }
        Self::apply_theme(&cc.egui_ctx, config.dark_mode);
        // Restore the user's saved dock layout (their panel arrangement,
        // including any floating windows) if one is persisted and still
        // parses; otherwise fall back to the curated default. A saved
        // layout that fails to deserialize (e.g. after an egui_dock schema
        // change) is silently discarded so a stale layout can't brick
        // startup.
        let dock_state = cc
            .storage
            .and_then(|s| eframe::get_value::<DockState<DockTab>>(s, DOCK_LAYOUT_KEY))
            .unwrap_or_else(default_dock_state);

        let (nrsc5, nrsc5_status) = match Nrsc5Process::new() {
            Ok(backend) => {
                let version = backend.version();
                (Some(backend), format!("ready: {version}"))
            }
            Err(err) => (None, format!("NRSC5 unavailable: {err}")),
        };

        // Install the shared FFT tap so the Spectrum panel has a feed
        // every time the piped path starts. Done once at app startup
        // (and again when the backend is recreated on a config switch).
        let spectrum_tap = crate::dsp::SpectrumTap::new(1_488_375.0);
        let smoothing_alpha = config.spectrum_smoothing_alpha.clamp(0.1, 1.0);
        let effective_alpha = if config.spectrum_smoothing_enabled {
            smoothing_alpha
        } else {
            1.0
        };
        spectrum_tap.set_smoothing_alpha(effective_alpha);
        // Build the audio player (opens the default output device) and
        // grab a clone-cheap sink for the nrsc5 backend to push PCM into.
        // Device-open failures are surfaced via `init_error` rather than
        // panicking; the app continues to run, just silently.
        let audio_player = crate::audio::AudioPlayer::new();
        let audio_sink = audio_player.sink();
        // Push initial volume / mute from config into the audio player
        // so the user's last session preferences take effect immediately
        // — no waiting for them to wiggle the slider.
        audio_player.set_volume(config.volume.clamp(0.0, 1.0));
        audio_player.set_mute(config.muted);
        let nrsc5 = nrsc5.map(|mut backend| {
            backend.set_spectrum_tap(spectrum_tap.clone());
            backend.set_audio_sink(audio_sink.clone());
            backend
        });

        let aas_dir = nrsc5
            .as_ref()
            .map(|n| n.aas_dir().to_path_buf())
            .unwrap_or_else(crate::paths::aas_temp_dir);

        // Channel for the background SDR-presence probe. Built here
        // so both endpoints land in the struct literal as a matched
        // pair without any post-construction reshuffling.
        let (sdr_probe_tx, sdr_probe_rx) = mpsc::channel();
        let (sdr_refresh_tx, sdr_refresh_rx) = mpsc::channel();

        // Open the on-disk art cache and load any history from previous
        // sessions. Failure here is non-fatal — we just start with an
        // empty collage and degrade to in-memory-only behavior.
        let art_cache = crate::art_cache::ArtCache::new();
        let (art_history, art_tiles, art_session_started) =
            restore_art_history(art_cache.as_ref(), collage_tile_cap(&config));

        // Load the permanent block list from config.
        let art_blocklist: std::collections::HashSet<u64> =
            config.art_blocklist.iter().map(|&h| h as u64).collect();
        let art_blocklist_count = art_blocklist.len();

        // Snapshot any per-field config values needed after `config` is
        // moved into the struct literal below.
        let play_log_retention_hours = config.play_log_retention_hours;

        let mut app = Self {
            app_state: AppState {
                frequency_mhz: config.frequency_mhz,
                selected_program: config.selected_program,
                dark_mode: config.dark_mode,
                nrsc5_status,
                volume: config.volume.clamp(0.0, 1.0),
                muted: config.muted,
                gain_mode: config.gain_mode,
                manual_gain_tenths: config.manual_gain_tenths,
                agc_amp_target_dbfs_override: config.agc_amp_target_dbfs_override,
                show_hd5_hd8: config.show_hd5_hd8,
                preset_slot_count: config.preset_slot_count.clamp(1, 48),
                recording_mode: config.recording_mode,
                // Default to "present" + "probe available" so the no-SDR
                // overlay only appears once we've actually probed and seen
                // zero devices — avoids a flash on launch.
                sdr_present: true,
                sdr_probe_available: true,
                sdrplay_service_stopped: false,
                art_tiles,
                art_session_started,
                art_blocklist_count,
                collage_secondary_click_fallback: config.collage_secondary_click_fallback,
                analog_fallback_mode: config.analog_fallback_mode,
                analog_fallback_stereo: config.analog_fallback_stereo,
                analog_fallback_rds_enabled: config.analog_fallback_rds_enabled,
                collage_tile_cap: collage_tile_cap(&config) as u32,
                spectrum_tap: Some(spectrum_tap),
                spectrum_smoothing_enabled: config.spectrum_smoothing_enabled,
                spectrum_smoothing_alpha: smoothing_alpha,
                ..AppState::default()
            },
            dock_state,
            config,
            nrsc5,
            retune_task: None,
            recording_session: None,
            start_requested_at: None,
            last_signal_at: None,
            _collage: CollageEngine::new(8),
            lot_files: HashMap::new(),
            aas_dir: aas_dir.clone(),
            traffic_map: TrafficMap::new(&aas_dir),
            weather_map: WeatherMap::new(&aas_dir),
            audio_player,
            art_history,
            art_blocklist,
            art_cache,
            last_art_prune_at: None,
            art_dirty: false,
            last_art_rebuild_at: None,
            last_aas_prune_at: None,
            closed_tab_locations: HashMap::new(),
            prev_layout: LayoutSnapshot::default(),
            play_log: {
                let mut log = crate::play_log::PlayLog::load();
                log.set_retention_hours(play_log_retention_hours);
                log
            },
            last_cover_play_at: None,
            sdr_probe_tx,
            sdr_probe_rx,
            sdr_probe_in_flight: false,
            sdr_refresh_tx,
            sdr_refresh_rx,
            sdr_refresh_in_flight: false,
        };
        if !app.weather_map.frames.is_empty() {
            app.apply_weather_frame_update();
        }
        app
    }
}

impl eframe::App for Nrsc5App {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        _visuals.panel_fill.to_normalized_gamma_f32()
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.update_runtime_metrics();
        self.drain_sdr_refresh_results();
        ui.ctx().request_repaint_after(Duration::from_millis(50));

        // Phase 4: rotate the active recording's .opus file when its
        // per-file max-minutes cap is reached. No-op when no
        // recording is active.
        self.maybe_rotate_recording();

        // Mirror the speaker router's current active program into
        // AppState so the GUI's Now Playing panel (and every other
        // reader of `active_idx()`) follows speaker switches without
        // having to plumb a query through every call site. Falls
        // back to `None` when no piped session is running so the
        // panel naturally drops back to `selected_program`.
        self.app_state.active_speaker = self.nrsc5.as_ref().and_then(|n| n.active_speaker());

        // Mirror the per-subchannel decoded-state bitmap so the
        // HD selector's toggle switches reflect reality (e.g. an
        // `add_decoder` that failed against the cap, or one that
        // exited on its own after losing its child process) instead
        // of the user's last click intent. Reset to all-false when
        // no session is running so toggling a stale "on" doesn't
        // try to drive a dead backend.
        let mut decoded = [false; 8];
        if let Some(n) = self.nrsc5.as_ref() {
            for p in n.decoded_programs() {
                if (p as usize) < decoded.len() {
                    decoded[p as usize] = true;
                }
            }
        }
        self.app_state.decoded = decoded;

        // Drain events from the nrsc5 process. Cap the number processed
        // per frame: when the window is minimized/occluded, Windows
        // suspends painting so `update` stops running and events
        // (metadata + LOT image payloads) pile up in the unbounded
        // channel. Draining the whole backlog in a single frame on
        // refocus froze the UI for seconds and, with a large image
        // backlog, could exhaust memory during the texture-upload burst.
        // A bounded batch keeps each frame cheap; if events remain we ask
        // egui for another frame right away so catch-up stays prompt.
        let mut more_events_pending = false;
        if let Some(nrsc5) = &self.nrsc5 {
            const MAX_EVENTS_PER_FRAME: usize = 128;
            let mut pending = Vec::new();
            while pending.len() < MAX_EVENTS_PER_FRAME {
                match nrsc5.events().try_recv() {
                    Ok(evt) => {
                        // Tee MER / Sync / etc into the AGC controller
                        // centrally. Used to live in each decoder's
                        // libnrsc5 callback but moved here so AGC keeps
                        // getting fed when the primary decoder is disabled
                        // and so that starting on a non-HD1 program still
                        // drives AGC.
                        nrsc5.forward_event_to_agc(&evt);
                        nrsc5.forward_event_to_handoff(&evt);
                        pending.push(evt);
                    }
                    Err(_) => break,
                }
            }
            more_events_pending = !nrsc5.events().is_empty();
            for evt in pending {
                self.app_state.last_event = evt.label().to_string();
                self.handle_nrsc5_event(evt);
            }
            // Backlog left over (we hit the per-frame cap): keep the
            // catch-up going without waiting for the next scheduled paint.
            if more_events_pending {
                ui.ctx().request_repaint();
            }
        }

        // Collage refresh, deferred until the event backlog is drained.
        // Cover-art ingest only marks the tiles dirty; the actual (expensive)
        // relayout + art-cache disk write happens here. Crucially, while a
        // large refocus catch-up is still draining we HOLD the relayout so
        // the collage doesn't visibly step through hundreds of intermediate
        // states (each one a relayout + texture churn + disk write) — instead
        // it jumps straight to the final layout once we're caught up, and
        // catch-up frames stay cheap. A 2-second fallback bounds how long the
        // collage can look stale under a sustained cover-art flood.
        if self.art_dirty {
            let caught_up = !more_events_pending;
            let stale = self
                .last_art_rebuild_at
                .map(|t| t.elapsed() >= Duration::from_secs(2))
                .unwrap_or(true);
            if caught_up || stale {
                self.rebuild_art_tiles();
                self.persist_art_history();
                self.art_dirty = false;
                self.last_art_rebuild_at = Some(Instant::now());
            }
        }

        // Check if a background retune task finished.
        if let Some(handle) = self.retune_task.as_ref() {
            if handle.is_finished() {
                let handle = self.retune_task.take().unwrap();
                match handle.join() {
                    Ok((backend, None)) => {
                        self.nrsc5 = Some(backend);
                        if let Some(n) = self.nrsc5.as_ref() {
                            let ctx = ui.ctx().clone();
                            n.set_repaint_waker(move || ctx.request_repaint());
                        }
                        self.app_state.is_streaming = true;
                        self.start_requested_at = Some(Instant::now());
                        self.app_state.nrsc5_status = format!(
                            "retuned to {:.1} MHz (HD{})",
                            self.app_state.frequency_mhz,
                            self.app_state.selected_program + 1
                        );
                    }
                    Ok((backend, Some(err_msg))) => {
                        self.nrsc5 = Some(backend);
                        if let Some(n) = self.nrsc5.as_ref() {
                            let ctx = ui.ctx().clone();
                            n.set_repaint_waker(move || ctx.request_repaint());
                        }
                        self.app_state.is_streaming = false;
                        self.app_state.nrsc5_status = format!("retune failed: {err_msg}");
                    }
                    Err(_panic) => {
                        self.app_state.nrsc5_status = "retune thread panicked".to_string();
                    }
                }
            }
        }

        // --- Hidden debug keyboard shortcut: set_active_speaker -----------
        // v0.5.1 collapsed multi-session decode to a single per-tune
        // session, so `add_decoder` / `remove_decoder` no longer exist;
        // every advertised subchannel is decoded by the one session
        // and the only thing the user can change at runtime is which
        // program is on the speakers. Alt+1..8 swaps the active
        // speaker; visible feedback goes to stderr.
        if let Some(nrsc5) = self.nrsc5.as_mut() {
            const NUM_KEYS: [egui::Key; 8] = [
                egui::Key::Num1,
                egui::Key::Num2,
                egui::Key::Num3,
                egui::Key::Num4,
                egui::Key::Num5,
                egui::Key::Num6,
                egui::Key::Num7,
                egui::Key::Num8,
            ];
            for (idx, key) in NUM_KEYS.iter().enumerate() {
                let program = idx as u32;
                let speak = egui::KeyboardShortcut::new(egui::Modifiers::ALT, *key);
                if ui.ctx().input_mut(|i| i.consume_shortcut(&speak)) {
                    match nrsc5.set_active_speaker(program) {
                        Ok(()) => eprintln!("[speaker] set_active_speaker({program}) ok"),
                        Err(e) => eprintln!("[speaker] set_active_speaker({program}) err: {e}"),
                    }
                }
            }
        }

        // --- Hidden dev helper: capture the current dock layout ----------
        // Ctrl+Shift+D serializes the live dock layout to
        // `dock-layout-dump.ron` in the working directory so a hand-tuned
        // arrangement can be captured and baked in as the default. Arrange
        // the panels as *docked* splits (drag tabs into position rather than
        // popping out floating windows) so the captured layout stays
        // single-surface — that keeps it resolution-independent and avoids
        // the egui_dock startup layer-id assert on restore.
        {
            let dump = egui::KeyboardShortcut::new(
                egui::Modifiers::CTRL | egui::Modifiers::SHIFT,
                egui::Key::D,
            );
            if ui.ctx().input_mut(|i| i.consume_shortcut(&dump)) {
                match ron::ser::to_string_pretty(
                    &self.dock_state,
                    ron::ser::PrettyConfig::default(),
                ) {
                    Ok(s) => {
                        let path = std::env::current_dir()
                            .unwrap_or_default()
                            .join("dock-layout-dump.ron");
                        match std::fs::write(&path, &s) {
                            Ok(()) => {
                                eprintln!("[layout] dumped dock layout to {}", path.display())
                            }
                            Err(e) => eprintln!("[layout] dump write failed: {e}"),
                        }
                    }
                    Err(e) => eprintln!("[layout] dump serialize failed: {e}"),
                }
            }
        }

        // Collect commands emitted by top-bar buttons (hamburger menu
        // items, the SDR chip) so they get processed by the same
        // dispatch loop as commands emitted by the dock panels. Declared
        // here so the closure below can push into it.
        let mut commands_from_top_bar: Vec<UiCommand> = Vec::new();
        // `horizontal_wrapped` so the strip can flow onto a second
        // line when the window is narrower than ~1600px instead of
        // clipping panel buttons off the right edge (which would
        // strand the user with no way to reopen closed panels).
        ui.horizontal_wrapped(|ui| {
            // Theme toggle stays at the very left as the most-used
            // single-purpose button.
            let mut menu_commands: Vec<UiCommand> = Vec::new();
            let theme_icon = if self.app_state.dark_mode {
                "☀"
            } else {
                "🌙"
            };
            if ui
                .button(egui::RichText::new(theme_icon).size(18.0))
                .on_hover_text("Toggle light/dark theme")
                .clicked()
            {
                self.app_state.dark_mode = !self.app_state.dark_mode;
                Self::apply_theme(ui.ctx(), self.app_state.dark_mode);
                self.config.dark_mode = self.app_state.dark_mode;
                save_config(&self.config);
            }

            // Hamburger menu — top-bar entry point for app-wide actions
            // that don't deserve a permanent slot (SDR Settings, About,
            // Reset Layout, etc.). Sits between the theme toggle and
            // the "NRSC5 Studio" title so it's always in roughly the
            // same screen position regardless of window width.
            ui.menu_button(egui::RichText::new("☰").size(18.0), |ui| {
                ui.set_min_width(200.0);
                if ui.button("\u{2699}  Settings...").clicked() {
                    menu_commands.push(UiCommand::ShowSdrSettings);
                    ui.close_menu();
                }
                ui.separator();
                if ui.button("\u{21BA}  Reset Panel Layout").clicked() {
                    self.dock_state = default_dock_state();
                    ui.close_menu();
                }
                // Phase 3 (v0.4.0): expose the gain cache as a
                // wipeable resource. Entry shows the current entry
                // count as a parenthetical so the user can tell at a
                // glance whether the action would do anything. The
                // count comes from the live Nrsc5Process; when the
                // backend isn't initialized yet we hide the entry
                // entirely rather than show "(0 entries)".
                if let Some(count) = self.nrsc5.as_ref().map(|p| p.gain_cache_len()) {
                    if count > 0 {
                        let label = format!(
                            "\u{1F5D1}  Clear gain cache\u{2026}  ({} {})",
                            count,
                            if count == 1 { "entry" } else { "entries" },
                        );
                        if ui.button(label).clicked() {
                            menu_commands.push(UiCommand::ClearGainCache);
                            ui.close_menu();
                        }
                    }
                }
                ui.separator();
                if ui.button("\u{2139}  About NRSC5 Studio...").clicked() {
                    menu_commands.push(UiCommand::ShowAbout);
                    ui.close_menu();
                }
            });

            ui.separator();
            ui.label(
                egui::RichText::new("NRSC5 Studio")
                    .strong()
                    .color(crate::gui::accent_color(self.app_state.dark_mode)),
            );
            ui.separator();
            ui.label(
                egui::RichText::new(format!("{:.1} MHz", self.app_state.frequency_mhz)).monospace(),
            );
            ui.separator();
            // Active SDR device chip — clickable shortcut to the SDR
            // Settings modal. Shows just the driver key in the top bar
            // to keep the space tight; full label is in the modal.
            let sdr_chip_text = format!("\u{1F4E1} {}", self.config.sdr.chip_label());
            if ui
                .button(egui::RichText::new(sdr_chip_text).monospace())
                .on_hover_text("Open Settings (transport • device • gain • display • recording)")
                .clicked()
            {
                menu_commands.push(UiCommand::ShowSdrSettings);
            }
            ui.separator();
            let status_color = if self.app_state.is_streaming {
                egui::Color32::from_rgb(80, 220, 120)
            } else {
                egui::Color32::from_gray(140)
            };
            let status_resp =
                ui.label(egui::RichText::new(&self.app_state.nrsc5_status).color(status_color));
            if let Some(version) = self.nrsc5.as_ref().map(|n| n.version()) {
                status_resp.on_hover_text(version);
            }
            ui.separator();

            // Panel toggle buttons. Selected state indicates the panel is
            // currently open in the dock; clicking a closed panel restores it
            // to its previous location (when possible), clicking an open one
            // focuses it.
            for tab in DockTab::ALL {
                let is_open = self.dock_state.find_tab(&tab).is_some();
                let label = tab.toolbar_label();
                let response = ui.selectable_label(is_open, label);
                if response.clicked() {
                    if let Some(loc) = self.dock_state.find_tab(&tab) {
                        let _ = self.dock_state.set_active_tab(loc);
                    } else {
                        self.reopen_tab(tab);
                    }
                }
            }

            // Reset-layout button, inline (no right-alignment
            // wrapper — that fights with `horizontal_wrapped`
            // because a wrapping row has no fixed right edge).
            if ui
                .button("↺")
                .on_hover_text("Reset panel layout to default")
                .clicked()
            {
                self.dock_state = default_dock_state();
            }

            // Queue menu commands for processing in the existing dispatch
            // loop below so handler logic stays in one place.
            for cmd in menu_commands {
                commands_from_top_bar.push(cmd);
            }
        });
        ui.separator();

        let mut commands = commands_from_top_bar;
        let mut viewer = DockViewer {
            app_state: &mut self.app_state,
            commands: &mut commands,
            presets: &self.config.presets,
            play_log: &self.play_log,
        };
        // Snapshot the layout before show_inside so we can detect tabs that
        // the user closes via the "X" button in this frame.
        let pre_layout = LayoutSnapshot::build(&self.dock_state);
        DockArea::new(&mut self.dock_state)
            .style(egui_dock::Style::from_egui(ui.style()))
            .show_inside(ui, &mut viewer);
        let post_layout = LayoutSnapshot::build(&self.dock_state);
        self.record_closures(&pre_layout, &post_layout);
        self.prev_layout = post_layout;

        for command in commands {
            self.handle_command(command);
        }

        // Periodically refresh dock/layout bookkeeping.
        // (Audio session probing was removed in v0.4.0 — the cpal-backed
        //  AudioPlayer owns the stream in-process now, so there is no
        //  external session to discover.)

        // Probe librtlsdr for attached devices every ~2 s so the no-SDR
        // overlay below reflects the current state of the USB bus.
        self.poll_sdr_presence(ui.ctx());

        // Render the "no SDR detected" overlay last so it sits on top of the
        // dock when applicable. Self-dismissing as soon as a dongle appears.
        self.render_no_sdr_overlay(ui.ctx());

        // App-wide modals (SDR Settings, About). Rendered after the dock
        // and the no-SDR overlay so they layer on top of everything else.
        // Both are gated by AppState flags; the close affordances inside
        // each emit Hide* UiCommands that handle_command processes.
        let mut modal_commands: Vec<UiCommand> = Vec::new();
        if self.app_state.show_sdr_settings {
            self.render_sdr_settings_modal(ui.ctx(), &mut modal_commands);
        }
        if self.app_state.show_about {
            self.render_about_dialog(ui.ctx(), &mut modal_commands);
        }
        for cmd in modal_commands {
            self.handle_command(cmd);
        }

        // Keep the rolling 8-hour collage window honest even in quiet periods
        // by pruning expired plays roughly once a minute.
        self.maybe_prune_art_history();

        // Bound the AAS scratch dir: drop raw LOT dumps older than the
        // retention window so the folder doesn't grow without limit.
        self.maybe_prune_aas_scratch();
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        // Persist the dock layout so the user's panel arrangement (docked
        // splits and floating windows alike) survives restarts. eframe
        // flushes `storage` after this returns.
        eframe::set_value(storage, DOCK_LAYOUT_KEY, &self.dock_state);
        // Flush runtime state to the TOML config too, so a crash or
        // Task-Manager kill doesn't lose the user's last frequency,
        // subchannel, theme, volume, or mute state. eframe calls save()
        // on its own ~30 s auto-save cadence and again on clean exit.
        self.sync_runtime_to_config();
        save_config(&self.config);
    }

    fn on_exit(&mut self) {
        if let Some(mut nrsc5) = self.nrsc5.take() {
            nrsc5.stop();
        }

        self.sync_runtime_to_config();
        save_config(&self.config);

        // Last-chance flush of the album-art history. The cache is also
        // written incrementally on every play event so a hard crash still
        // preserves most of the data — this just covers any tail changes
        // (e.g. expirations from a prune that happened after the last play).
        self.persist_art_history();

        // Same belt-and-braces flush for the rolling play log.
        self.play_log.save();
    }
}

impl Nrsc5App {
    /// Copy the live UI state (frequency, subchannel, theme, volume, mute)
    /// into the persisted `AppConfig`. Called from both `save()` (eframe's
    /// periodic ~30 s auto-save) and `on_exit()`, plus a handful of eager
    /// call sites in command handlers so deliberate user changes survive
    /// a hard kill even within the auto-save window.
    fn sync_runtime_to_config(&mut self) {
        self.config.frequency_mhz = self.app_state.frequency_mhz;
        self.config.selected_program = self.app_state.selected_program;
        self.config.dark_mode = self.app_state.dark_mode;
        self.config.volume = self.app_state.volume;
        self.config.muted = self.app_state.muted;
    }

    /// Record any tabs that existed in `pre` but are missing from `post`.
    /// Their previous location and neighbours are stashed so the next click
    /// on the panel toolbar can restore them to (roughly) the same spot.
    fn record_closures(&mut self, pre: &LayoutSnapshot, post: &LayoutSnapshot) {
        for (tab, np) in &pre.tabs {
            if post.tabs.contains_key(tab) {
                continue;
            }
            let leaf_mates: Vec<DockTab> = pre
                .leaves
                .get(&(np.surface, np.node))
                .map(|v| v.iter().filter(|t| *t != tab).cloned().collect())
                .unwrap_or_default();
            let surface_mates: Vec<DockTab> = pre
                .surfaces
                .get(&np.surface)
                .map(|v| v.iter().filter(|t| *t != tab).cloned().collect())
                .unwrap_or_default();
            self.closed_tab_locations.insert(
                tab.clone(),
                ClosedTabInfo {
                    location: Some(*np),
                    leaf_mates,
                    surface_mates,
                },
            );
        }
        // Drop bookkeeping for any tab that has since reappeared.
        for tab in post.tabs.keys() {
            self.closed_tab_locations.remove(tab);
        }
    }

    /// Push `tab` back into the dock state, preferring its previous location.
    /// Falls back through several heuristics so the panel comes back near
    /// where it was even after intervening layout changes.
    fn reopen_tab(&mut self, tab: DockTab) {
        let info = self.closed_tab_locations.remove(&tab).unwrap_or_default();

        // 1) Exact previous leaf, if it still exists.
        if let Some(np) = info.location {
            let still_alive = self
                .dock_state
                .iter_all_tabs()
                .any(|(p, _)| p.surface == np.surface && p.node == np.node);
            if still_alive {
                self.dock_state.set_focused_node_and_surface(np);
                self.dock_state.push_to_focused_leaf(tab);
                return;
            }
        }

        // 2) Any former leaf-mate that's still around.
        for mate in &info.leaf_mates {
            if let Some(path) = self.dock_state.find_tab(mate) {
                self.dock_state.set_focused_node_and_surface(NodePath {
                    surface: path.surface,
                    node: path.node,
                });
                self.dock_state.push_to_focused_leaf(tab);
                return;
            }
        }

        // 3) Any tab from the same surface that's still around.
        for mate in &info.surface_mates {
            if let Some(path) = self.dock_state.find_tab(mate) {
                self.dock_state.set_focused_node_and_surface(NodePath {
                    surface: path.surface,
                    node: path.node,
                });
                self.dock_state.push_to_focused_leaf(tab);
                return;
            }
        }

        // 4) Last resort: focused leaf.
        self.dock_state.push_to_focused_leaf(tab);
    }

    /// Extend egui's default font fallback chain so glyphs that ship in
    /// `Hack-Regular.ttf` (geometric shapes like `\u{25CF}` / `\u{25CB}`,
    /// math arrows, etc.) also render in `FontFamily::Proportional`
    /// text. By default egui only puts Hack in the Monospace chain,
    /// which means a label like "● LOCK" rendered as ordinary
    /// proportional text would show a tofu box. We keep the existing
    /// chain order and just append Hack at the end so coverage degrades
    /// gracefully without affecting which font gets picked for normal
    /// letters.
    ///
    /// See `scripts/probe-glyphs.ps1` for the full audit of which
    /// codepoints each bundled font covers.
    fn install_fonts(ctx: &egui::Context) {
        let mut fonts = egui::FontDefinitions::default();
        if let Some(chain) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
            if !chain.iter().any(|n| n == "Hack") {
                chain.push("Hack".to_owned());
            }
        }
        ctx.set_fonts(fonts);
    }

    fn apply_theme(ctx: &egui::Context, dark: bool) {
        let accent = crate::gui::accent_color(dark);
        let theme = if dark {
            egui::Theme::Dark
        } else {
            egui::Theme::Light
        };

        let mut visuals = if dark {
            let mut v = egui::Visuals::dark();
            let bg = egui::Color32::from_rgb(25, 25, 30);
            let panel_bg = egui::Color32::from_rgb(30, 30, 38);
            v.panel_fill = panel_bg;
            v.window_fill = panel_bg;
            v.extreme_bg_color = bg;
            v.faint_bg_color = egui::Color32::from_rgb(35, 35, 45);
            v.widgets.noninteractive.bg_stroke =
                egui::Stroke::new(0.5_f32, egui::Color32::from_gray(60));
            v.widgets.inactive.bg_stroke = egui::Stroke::new(0.5_f32, egui::Color32::from_gray(80));
            v
        } else {
            let mut v = egui::Visuals::light();
            let panel_bg = egui::Color32::from_rgb(245, 245, 250);
            v.panel_fill = panel_bg;
            v.window_fill = egui::Color32::WHITE;
            v.extreme_bg_color = egui::Color32::from_rgb(235, 235, 240);
            v.faint_bg_color = egui::Color32::from_rgb(240, 240, 248);
            v.widgets.noninteractive.bg_stroke =
                egui::Stroke::new(0.5_f32, egui::Color32::from_gray(200));
            v.widgets.inactive.bg_stroke =
                egui::Stroke::new(0.5_f32, egui::Color32::from_gray(180));
            v
        };

        visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0_f32, accent);
        visuals.widgets.active.bg_stroke = egui::Stroke::new(1.0_f32, accent);

        let cr = egui::CornerRadius::same(4);
        visuals.widgets.noninteractive.corner_radius = cr;
        visuals.widgets.inactive.corner_radius = cr;
        visuals.widgets.hovered.corner_radius = cr;
        visuals.widgets.active.corner_radius = cr;
        visuals.window_corner_radius = egui::CornerRadius::same(6);

        visuals.selection.bg_fill = accent.linear_multiply(0.3);
        visuals.selection.stroke = egui::Stroke::new(1.0_f32, accent);
        visuals.hyperlink_color = accent;

        // egui 0.34 has a dual-theme system with `ThemePreference::System`
        // as the default, which means it follows the OS. Without an
        // explicit `set_theme`, calling `set_visuals` only updates the
        // visuals slot for the currently-active theme \u2014 and on the next
        // pass egui re-resolves the system theme and overwrites them with
        // its built-ins. Pin the preference AND install our visuals into
        // the matching theme slot.
        ctx.set_theme(theme);
        ctx.set_visuals_of(theme, visuals);

        let mut style = (*ctx.global_style()).clone();
        style.spacing.item_spacing = egui::vec2(6.0, 4.0);
        style.spacing.button_padding = egui::vec2(8.0, 3.0);
        style.spacing.window_margin = egui::Margin::same(8);
        ctx.set_global_style(style);
    }

    fn update_runtime_metrics(&mut self) {
        self.app_state.startup_wait_s = self
            .start_requested_at
            .map(|t| t.elapsed().as_secs_f32())
            .unwrap_or(0.0);

        self.app_state.silence_s = self
            .last_signal_at
            .map(|t| t.elapsed().as_secs_f32())
            .unwrap_or(0.0);

        // Refresh the AGC snapshot for the Signal panel readout. Cheap
        // (Mutex try-lock + shallow Clone) and only `Some` while a
        // piped stream is active; cleared back to `None` on stop.
        self.app_state.agc_snapshot = self.nrsc5.as_ref().and_then(|n| n.agc_snapshot());
        // Refresh the active gain-mode mirrors so the dropdown can show
        // a "(restart stream to apply)" hint when the user changes the
        // selection mid-stream.
        self.app_state.active_gain_mode = self.nrsc5.as_ref().and_then(|n| n.active_gain_mode());
        self.app_state.active_manual_gain_tenths = self
            .nrsc5
            .as_ref()
            .and_then(|n| n.active_manual_gain_tenths());
        // Refresh antenna state for the Tuner-panel dropdown. Both
        // are best-effort: a live SDR with no multi-antenna concept
        // returns an empty `Vec` and `None`, which collapses the
        // dropdown to nothing (intended).
        self.app_state.sdr_antennas = self
            .nrsc5
            .as_ref()
            .map(|n| n.sdr_antennas())
            .unwrap_or_default();
        self.app_state.active_antenna = self.nrsc5.as_ref().and_then(|n| n.active_antenna());
    }

    /// Push the current `app_state.volume` into the audio player. Wait-
    /// free atomic store; the cpal callback picks it up on the next fill.
    fn apply_volume(&mut self) {
        self.audio_player.set_volume(self.app_state.volume);
    }

    /// Push the current `app_state.muted` into the audio player. Wait-
    /// free atomic store; the cpal callback picks it up on the next fill.
    fn apply_mute(&mut self) {
        self.audio_player.set_mute(self.app_state.muted);
    }

    /// Push the current smoothing settings into the shared SpectrumTap.
    fn apply_spectrum_smoothing(&mut self) {
        let alpha = if self.app_state.spectrum_smoothing_enabled {
            self.app_state.spectrum_smoothing_alpha
        } else {
            1.0
        };
        if let Some(tap) = self.app_state.spectrum_tap.as_ref() {
            tap.set_smoothing_alpha(alpha);
        }
    }

    /// Probe attached SDRs and update `app_state.sdr_present` /
    /// `sdr_probe_available`. Throttled to roughly one probe every two
    /// seconds. Two layers:
    ///
    ///   1. `librtlsdr` device count via `sdr_detect::device_count()`.
    ///      Cheap (single-digit ms when no devices are attached), but
    ///      only sees RTL-SDR dongles.
    ///   2. `soapysdr::enumerate("")` filtered to the supported driver
    ///      list. Catches SDRplay, Airspy, HackRF, etc. Only run when
    ///      the cheap probe says zero AND we're not currently
    ///      streaming, so we never contend with a live USB device.
    ///
    /// **Threading.** Both probes run on a short-lived background
    /// worker thread — `soapysdr::enumerate` can block for several
    /// seconds on SDRplay hotplug while the API service does its
    /// device-discovery handshake, and running that on the GUI thread
    /// would put the window into Windows' "Not Responding" state. The
    /// GUI thread only drains results (non-blocking) and decides
    /// whether to spawn another probe.
    ///
    /// If neither probe is available on this system (no `librtlsdr.dll`
    /// AND Soapy enumeration errored) we silently mark probing
    /// unavailable and never show the no-SDR overlay — a false
    /// "missing" warning would be worse than no warning at all.
    fn poll_sdr_presence(&mut self, ctx: &egui::Context) {
        // Drain any completed background probes. Keep only the latest
        // result if more than one is queued (which shouldn't happen
        // given the in-flight guard, but is cheap to handle anyway).
        let mut latest: Option<SdrProbeResult> = None;
        while let Ok(r) = self.sdr_probe_rx.try_recv() {
            latest = Some(r);
            self.sdr_probe_in_flight = false;
        }
        if let Some(r) = latest {
            let any_probe_ok = r.rtl.is_some() || r.soapy.is_some();
            let any_present = r.rtl.unwrap_or(0) > 0 || r.soapy.unwrap_or(0) > 0;
            // SDRplay API service state is independent of the RTL /
            // Soapy probes: it's installed-or-not, running-or-not.
            // When the service exists but isn't Running, surface the
            // hint regardless of whether any other SDR was detected
            // — a user with both an RTL dongle and an SDRplay still
            // benefits from knowing the SDRplay path is blocked.
            self.app_state.sdrplay_service_stopped = matches!(
                r.sdrplay_service,
                Some(crate::sdr_detect::SdrplayServiceState::Stopped)
                    | Some(crate::sdr_detect::SdrplayServiceState::Pending)
                    | Some(crate::sdr_detect::SdrplayServiceState::Other)
            );
            if any_probe_ok {
                self.app_state.sdr_probe_available = true;
                // While streaming, trust that a device is present (the
                // stream is using one). Otherwise reflect probe results.
                self.app_state.sdr_present = self.app_state.is_streaming || any_present;
            } else {
                self.app_state.sdr_probe_available = false;
                self.app_state.sdr_present = true;
            }
        }

        // Decide whether to kick off another probe. Throttle to one
        // per ~2 s, and never stack a second probe while one is in
        // flight (which can outlast the throttle interval when the
        // SDRplay API service is slow).
        let now = Instant::now();
        let due = self
            .app_state
            .sdr_last_probed
            .map(|t| now.duration_since(t) >= Duration::from_millis(2000))
            .unwrap_or(true);

        if due && !self.sdr_probe_in_flight {
            self.app_state.sdr_last_probed = Some(now);
            self.sdr_probe_in_flight = true;
            let tx = self.sdr_probe_tx.clone();
            let is_streaming = self.app_state.is_streaming;
            let repaint_ctx = ctx.clone();
            std::thread::spawn(move || {
                let rtl = crate::sdr_detect::device_count();
                // Cheap (one `sc.exe query`); always probe so the UI
                // notices a service stop/start mid-session without
                // waiting for the next attempted Start to fail.
                //
                // We need this BEFORE the Soapy enumerate below: once
                // the SDRplay Soapy module has been loaded into the
                // process, calling `enumerate("")` against a stopped
                // service can crash the module's `find_SDRplay` (it
                // segfaults trying to talk to the dead named pipe),
                // which on Windows brings down the whole process via
                // SEH. Pre-checking the service lets us skip the
                // Soapy probe entirely when SDRplay support is
                // present but not running.
                let sdrplay_service = crate::sdr_detect::sdrplay_service_state();
                let sdrplay_blocked = matches!(
                    sdrplay_service,
                    Some(crate::sdr_detect::SdrplayServiceState::Stopped)
                        | Some(crate::sdr_detect::SdrplayServiceState::Pending)
                        | Some(crate::sdr_detect::SdrplayServiceState::Other)
                );
                // Only run the heavier Soapy enumeration if librtlsdr
                // came up empty AND no stream is active AND the
                // SDRplay API service isn't in a state that could
                // crash the module. Streaming with a non-RTL device
                // (e.g. SDRplay) implies a device is present, and we
                // don't want to contend with a live capture.
                let soapy = match (rtl, is_streaming, sdrplay_blocked) {
                    (Some(n), _, _) if n > 0 => None,
                    (_, true, _) => None,
                    (_, _, true) => None,
                    _ => crate::sdr_detect::soapy_supported_count(),
                };
                let _ = tx.send(SdrProbeResult {
                    rtl,
                    soapy,
                    sdrplay_service,
                });
                // Wake the GUI so the result is applied promptly
                // even if the user isn't interacting with the app.
                repaint_ctx.request_repaint();
            });
        }

        // While the overlay is showing, ensure egui keeps repainting even
        // if the user isn't interacting — otherwise we'd stop polling.
        if self.app_state.sdr_probe_available && !self.app_state.sdr_present {
            ctx.request_repaint_after(Duration::from_millis(2100));
        }
    }

    /// Render the centered "no SDR detected" panel on top of the dock when
    /// we've confirmed no RTL-SDR is attached and we're not already
    /// streaming. The overlay is informational only — the rest of the UI
    /// remains usable behind it (so users can still arrange panels and
    /// configure presets), and it self-dismisses the moment a device shows
    /// up on the next probe tick.
    fn render_no_sdr_overlay(&mut self, ctx: &egui::Context) {
        if !self.app_state.sdr_probe_available
            || self.app_state.sdr_present
            || self.app_state.is_streaming
        {
            return;
        }

        let mut refresh_clicked = false;
        egui::Area::new(egui::Id::new("no-sdr-overlay"))
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .order(egui::Order::Foreground)
            .interactable(true)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style())
                    .fill(egui::Color32::from_rgb(28, 32, 42))
                    .stroke(egui::Stroke::new(
                        1.0_f32,
                        egui::Color32::from_rgb(90, 120, 180),
                    ))
                    .corner_radius(egui::CornerRadius::same(10))
                    .inner_margin(egui::Margin::same(24))
                    .show(ui, |ui| {
                        ui.set_max_width(380.0);
                        ui.vertical_centered(|ui| {
                            ui.label(egui::RichText::new("\u{1F4F6}").size(56.0));
                            ui.add_space(2.0);
                            ui.label(
                                egui::RichText::new("No SDR detected")
                                    .heading()
                                    .color(egui::Color32::from_rgb(230, 200, 110)),
                            );
                            ui.add_space(10.0);
                            ui.label(
                                egui::RichText::new(
                                    "Plug in your RTL-SDR dongle, then click Refresh.",
                                )
                                .size(14.0),
                            );
                            ui.add_space(4.0);
                            ui.label(
                                egui::RichText::new("(or just wait \u{2014} we keep checking)")
                                    .small()
                                    .color(egui::Color32::from_gray(150)),
                            );
                            // Extra hint when the SDRplay API service
                            // is installed but stopped: tell the user
                            // exactly what to do. Starting / stopping
                            // a Windows service requires admin, so we
                            // can't fix it ourselves \u2014 the message
                            // points at Services.msc.
                            if self.app_state.sdrplay_service_stopped {
                                ui.add_space(12.0);
                                ui.label(
                                    egui::RichText::new("SDRplay API service is stopped.")
                                        .size(13.0)
                                        .color(egui::Color32::from_rgb(230, 160, 100)),
                                );
                                ui.add_space(2.0);
                                ui.label(
                                    egui::RichText::new(
                                        "Start it from Services.msc (admin), then Refresh.",
                                    )
                                    .small()
                                    .color(egui::Color32::from_gray(180)),
                                );
                            }
                            ui.add_space(16.0);
                            let btn = ui.add_sized(
                                [140.0, 30.0],
                                egui::Button::new(
                                    egui::RichText::new("\u{21BB}  Refresh")
                                        .strong()
                                        .color(egui::Color32::from_gray(230)),
                                )
                                .fill(egui::Color32::from_rgb(60, 95, 160)),
                            );
                            if btn.clicked() {
                                refresh_clicked = true;
                            }
                        });
                    });
            });

        if refresh_clicked {
            // Force the next frame to re-probe immediately rather than
            // waiting for the 2-second cadence to elapse.
            self.app_state.sdr_last_probed = None;
            ctx.request_repaint();
        }
    }

    /// Update the album-art heat-map histogram with a newly-displayed cover.
    /// Dedupes by content hash so the same image transmitted under different
    /// LOT IDs still counts as one tile, and enforces `ART_COUNT_COOLDOWN`
    /// between successive counted plays of the same hash so a single song's
    /// repeated XHDR emissions don't inflate the count.
    ///
    /// Also takes ownership of the cover-display lifecycle: sets
    /// `programs[program].cover_art_path` to the durable cache copy when we
    /// have one (falling back to the AAS-dump path otherwise), and deletes
    /// the redundant AAS-dump file after a successful cache write so the
    /// temp dir doesn't accumulate ~50 KB per song forever.
    fn record_album_art(&mut self, program: u32, full_path: &std::path::Path, path_str: &str) {
        let slot_idx = (program as usize).min(self.app_state.programs.len() - 1);
        let now = Instant::now();
        // Anchor the session timestamp on the first art event so the UI can
        // still report "how long you've been listening" — it's now purely
        // informational since the window is rolling.
        if self.app_state.art_session_started.is_none() {
            self.app_state.art_session_started = Some(now);
        }
        // Slide the 8-hour window forward: drop every play older than the
        // cutoff, and forget entries whose deque drains empty.
        let cutoff = now.checked_sub(ART_WINDOW);
        if let Some(cutoff) = cutoff {
            self.art_history.retain(|_, entry| {
                while entry.plays.front().is_some_and(|t| *t < cutoff) {
                    entry.plays.pop_front();
                }
                !entry.plays.is_empty()
            });
        }
        let Ok(bytes) = std::fs::read(full_path) else {
            // Couldn't read the dump — fall back to the AAS path so the
            // live cover at least *tries* to load. Don't delete anything.
            self.app_state.programs[slot_idx].cover_art_path = Some(path_str.to_string());
            return;
        };
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        bytes.hash(&mut hasher);
        let key = hasher.finish();

        // Permanently blocked — discard the AAS dump and bail immediately.
        if self.art_blocklist.contains(&key) {
            let _ = std::fs::remove_file(full_path);
            return;
        }

        // Per-hash cooldown. If we counted this exact image less than
        // `ART_COUNT_COOLDOWN` ago, treat this XHDR as a re-broadcast of
        // the same in-progress song rather than a new play. Still redirect
        // the live cover to the existing cache copy and prune the new AAS
        // dump — its contents are byte-identical to what we already have.
        if let Some(existing) = self.art_history.get(&key) {
            if let Some(&last) = existing.plays.back() {
                if now.duration_since(last) < ART_COUNT_COOLDOWN {
                    let cached_path = existing.path.clone();
                    self.app_state.programs[slot_idx].cover_art_path = Some(cached_path.clone());
                    // Only delete the AAS dump if we actually have a cache
                    // path to fall back on (cached_path != AAS path).
                    if self.art_cache.is_some() && cached_path != path_str {
                        let _ = std::fs::remove_file(full_path);
                    }
                    return;
                }
            }
        }

        // Copy the image bytes into our persistent cache. On success we use
        // the cache path as the authoritative location for both the live
        // cover display and the heat-map tile; on failure we keep the AAS
        // dump in place as a fallback.
        let cached_path: Option<String> = self.art_cache.as_ref().and_then(|cache| {
            cache
                .store_image(key, &bytes, full_path)
                .map(|p| p.to_string_lossy().into_owned())
        });
        let resolved_path = cached_path.clone().unwrap_or_else(|| path_str.to_string());

        // Live cover display follows the cache when possible.
        self.app_state.programs[slot_idx].cover_art_path = Some(resolved_path.clone());
        // With a durable cache copy in hand, the AAS-dir dump is dead weight.
        if cached_path.is_some() {
            let _ = std::fs::remove_file(full_path);
        }

        // Grab the song metadata currently on display so we can label this
        // cover later in tooltips. Trim and skip empty pieces so we don't
        // accumulate noise entries like ("", ""). Pulled from the same
        // program slot whose cover just changed so the labels match the
        // image regardless of which subchannel is on the speakers.
        let slot = &self.app_state.programs[slot_idx];
        let title = slot.title.trim().to_string();
        let artist = slot.artist.trim().to_string();
        let album = slot.album.trim().to_string();

        let entry = self.art_history.entry(key).or_insert_with(|| ArtEntry {
            path: resolved_path.clone(),
            plays: VecDeque::new(),
            songs: Vec::new(),
            album: album.clone(),
        });
        entry.plays.push_back(now);
        // Always refresh path — a re-emitted image may live at a new LOT path,
        // and on first load from disk we want to upgrade to the cache path.
        entry.path = resolved_path.clone();
        if !album.is_empty() {
            entry.album = album;
        }
        if !title.is_empty() || !artist.is_empty() {
            const MAX_SONGS_PER_COVER: usize = 16;
            let pair = (title, artist);
            if !entry.songs.iter().any(|p| p == &pair) {
                if entry.songs.len() >= MAX_SONGS_PER_COVER {
                    entry.songs.remove(0);
                }
                entry.songs.push(pair);
            }
        }
        // Defer the expensive collage relayout + art-cache disk write to the
        // single coalesced pass at the end of the frame (see `art_dirty`).
        // During a large catch-up this collapses N relayouts + N disk
        // writes into one per frame, eliminating the visible step-through.
        self.art_dirty = true;

        // A genuinely new play just landed (the cover-hash cooldown has
        // gated this code path). Remember the moment so the metadata
        // handler can decide whether subsequent title/artist updates
        // belong to a real song vs. a station-slogan flap, and try to push
        // the play to the rolling 24-hour log right now.
        self.last_cover_play_at = Some(crate::play_log::now_millis());
        self.try_record_play(program);
    }

    /// Phase 4 — spawn a new Opus recording locked to whatever
    /// subchannel the user currently has *selected* (not necessarily
    /// the active speaker; the recorder follows the selection at
    /// start time, then stays put even if the user swaps speakers).
    ///
    /// Refuses to start if:
    ///   * no stream is running                                    (Start first)
    ///   * a recording is already active                            (Stop the current one first)
    ///   * the selected program isn't being decoded                 (caller auto-spawns via SelectProgram, but if that fails we surface it here)
    ///
    /// Surfaces all of the above through `nrsc5_status` rather than
    /// failing silently — the dock's Record button has no other way
    /// to tell the user what's wrong.
    fn start_recording(&mut self) {
        if self.recording_session.is_some() {
            self.app_state.nrsc5_status =
                "already recording — Stop before starting again".to_string();
            return;
        }
        if !self.app_state.is_streaming {
            self.app_state.nrsc5_status = "press Start before recording".to_string();
            return;
        }

        // Lock the recording target to the *selected* program at the
        // moment of Record. Stays put across speaker swaps.
        let program = self.app_state.selected_program;
        // Quick "is the decoder up?" check (drops the borrow before
        // we compute paths/tags below — those need an immutable
        // borrow of self).
        let decoder_up = self
            .nrsc5
            .as_ref()
            .map(|n| n.is_decoding(program))
            .unwrap_or(false);
        if self.nrsc5.is_none() {
            self.app_state.nrsc5_status = "no SDR backend — cannot record".to_string();
            return;
        }
        if !decoder_up {
            self.app_state.nrsc5_status = format!(
                "HD{} hasn't produced audio yet \u{2014} wait for it to come on air before recording",
                program + 1,
            );
            return;
        }

        // Resolve output path: <base>/<station_subfolder?>/<yyyy-mm-dd>_HD<n>_<timestamp>.opus
        let base_dir = match self
            .config
            .recording_dir
            .as_ref()
            .map(std::path::PathBuf::from)
            .or_else(crate::paths::default_recording_dir)
        {
            Some(p) => p,
            None => {
                self.app_state.nrsc5_status =
                    "no recording directory resolved — set one in Settings".to_string();
                return;
            }
        };
        let mut dir = base_dir;
        if self.config.recording_subfolder_per_station {
            let station = self
                .app_state
                .station_info
                .call_sign
                .as_deref()
                .filter(|s| !s.is_empty())
                .map(sanitize_filename)
                .unwrap_or_else(|| format!("{:.1}MHz", self.config.frequency_mhz));
            dir.push(station);
        }
        let now = chrono::Local::now();
        let stamp = now.format("%Y-%m-%d_%H%M%S").to_string();
        let filename = format!("{}_HD{}_recording.opus", stamp, program + 1);
        let output_path = dir.join(filename);
        let tags = self.build_recording_tags(program, &now);

        // Now take the mutable borrow on nrsc5 for the actual spawn
        // + attach. Re-check existence in case something dropped it
        // between the read-only probe above and here (impossible on
        // current code paths, but defensive).
        let Some(nrsc5) = self.nrsc5.as_mut() else {
            self.app_state.nrsc5_status = "no SDR backend — cannot record".to_string();
            return;
        };

        match crate::recorder::RecordingSession::spawn(program, output_path.clone(), tags) {
            Ok((session, pcm_tx)) => {
                // Wire the SpeakerRouter tap. Failure here means the
                // program disappeared between the is_decoding check
                // above and now — race against decoder teardown.
                if let Err(err) = nrsc5.attach_recorder(program, pcm_tx) {
                    self.app_state.nrsc5_status =
                        format!("recording attach failed for HD{}: {err}", program + 1,);
                    // Drop the session so its forwarder sees a
                    // closed channel and flushes the (empty) file.
                    drop(session);
                    return;
                }
                self.app_state.recording = Some(crate::gui::state::RecordingStatus {
                    program,
                    started_at: Instant::now(),
                    output_path: output_path.display().to_string(),
                });
                self.recording_session = Some(session);
                self.app_state.nrsc5_status =
                    format!("● recording HD{} → {}", program + 1, output_path.display(),);
            }
            Err(err) => {
                self.app_state.nrsc5_status = format!("recording start failed: {err}");
            }
        }
    }

    /// Phase 4 — stop the active recording (if any), flush the
    /// .opus file, detach the SpeakerRouter tap. `fatal == true`
    /// when the recording is being closed by stream teardown
    /// (Stop / TuneMhz / SDR disconnect) rather than the user
    /// explicitly hitting Stop on the Record button — changes the
    /// status-line wording so the user knows the closure was a
    /// side-effect of the bigger action, not the recorder choking.
    fn stop_recording(&mut self, fatal: bool) {
        let Some(session) = self.recording_session.take() else {
            if !fatal {
                self.app_state.nrsc5_status = "no active recording to stop".to_string();
            }
            return;
        };
        let program = session.program();
        let path = session.output_path().display().to_string();
        // Detach the router tap *before* calling session.stop() so
        // the forwarder thread sees its sender drop first, sends a
        // clean RecorderCmd::Stop, and the encoder thread exits via
        // its normal flush path rather than via the 60-second
        // recv_timeout fallback.
        if let Some(nrsc5) = self.nrsc5.as_mut() {
            nrsc5.detach_recorder(program);
        }
        match session.stop() {
            Ok(()) => {
                self.app_state.nrsc5_status = if fatal {
                    format!("recording closed (stream stopped) → {path}")
                } else {
                    format!("recording saved → {path}")
                };
            }
            Err(err) => {
                self.app_state.nrsc5_status = format!("recording stop error: {err}");
            }
        }
        self.app_state.recording = None;
    }

    /// Build the file-lifetime metadata baked into the OpusTags
    /// packet at the start of each recording file (and again after
    /// each rotation). PSD timing on real-world stations is too
    /// irregular to put per-song TITLE/ARTIST in here, so the tags
    /// are intentionally station-level only: call sign, HD slot,
    /// tuned frequency, and the wall-clock when *this file* started
    /// (not the recording session — each rotated file gets a fresh
    /// timestamp).
    fn build_recording_tags(
        &self,
        program: u32,
        now: &chrono::DateTime<chrono::Local>,
    ) -> crate::recorder::RecordingTags {
        crate::recorder::RecordingTags {
            station: self
                .app_state
                .station_info
                .call_sign
                .clone()
                .unwrap_or_default(),
            program,
            frequency_mhz: self.config.frequency_mhz,
            started_human: now.format("%Y-%m-%d %H:%M:%S").to_string(),
            date: now.format("%Y-%m-%d").to_string(),
        }
    }

    /// Per-frame check called from `ui()`: if the active recording
    /// has been writing the *current* file for longer than
    /// `recording_max_minutes`, send a Rotate command to the
    /// encoder thread (which closes the current .opus file cleanly
    /// and opens a fresh one with a new timestamp). No-op when no
    /// recording is active or when the cap hasn't been reached.
    fn maybe_rotate_recording(&mut self) {
        let Some(status) = self.app_state.recording.as_ref() else {
            return;
        };
        let cap =
            Duration::from_secs((self.config.recording_max_minutes as u64).saturating_mul(60));
        if status.started_at.elapsed() < cap {
            return;
        }
        let program = status.program;
        if self.recording_session.is_none() {
            return;
        }

        // Compute the new file path with a fresh timestamp, using
        // the same base-dir + per-station-subfolder layout
        // start_recording uses. Anything that's changed since (new
        // call sign, new freq) gets re-resolved naturally here.
        let base_dir = match self
            .config
            .recording_dir
            .as_ref()
            .map(std::path::PathBuf::from)
            .or_else(crate::paths::default_recording_dir)
        {
            Some(p) => p,
            None => return,
        };
        let mut dir = base_dir;
        if self.config.recording_subfolder_per_station {
            let station = self
                .app_state
                .station_info
                .call_sign
                .as_deref()
                .filter(|s| !s.is_empty())
                .map(sanitize_filename)
                .unwrap_or_else(|| format!("{:.1}MHz", self.config.frequency_mhz));
            dir.push(station);
        }
        let now = chrono::Local::now();
        let stamp = now.format("%Y-%m-%d_%H%M%S").to_string();
        let filename = format!("{}_HD{}_recording.opus", stamp, program + 1);
        let new_path = dir.join(filename);
        let tags = self.build_recording_tags(program, &now);

        // Take the session borrow last so the immutable borrow used
        // by build_recording_tags above is already released.
        let Some(session) = self.recording_session.as_mut() else {
            return;
        };
        session.rotate(new_path.clone(), tags);
        // Update the mirror so the dock's REC timer resets to 0 for
        // the new file, and the hover-text path follows.
        self.app_state.recording = Some(crate::gui::state::RecordingStatus {
            program,
            started_at: Instant::now(),
            output_path: new_path.display().to_string(),
        });
        self.app_state.nrsc5_status = format!(
            "● recording HD{} → rotated to {}",
            program + 1,
            new_path.display(),
        );
    }

    /// Try to record the currently-displayed song into the rolling play
    /// log. Idempotent — the log's own gate (pair-equality dedup +
    /// rate-limit) drops noisy re-calls. Persists on success. `program`
    /// is the HD subchannel that produced the song (i.e. the originating
    /// `NrscEvent::Metadata.program`), not necessarily the active
    /// speaker — each decoder's metadata gets logged against its own
    /// subchannel.
    fn try_record_play(&mut self, program: u32) {
        let now_ms = crate::play_log::now_millis();
        let slot_idx = (program as usize).min(self.app_state.programs.len() - 1);
        let slot = &self.app_state.programs[slot_idx];
        let title = slot.title.clone();
        let artist = slot.artist.clone();
        let freq = self.config.frequency_mhz;
        let call_sign = self.app_state.call_sign.clone();
        if self
            .play_log
            .try_push(now_ms, &title, &artist, freq, program, &call_sign)
        {
            self.play_log.save();
        }
    }

    /// Rebuild the AppState's tile list from `art_history`. We first keep
    /// the top `collage_tile_cap()` covers by play count (so the heat-map
    /// always shows the actual heavy-rotation winners) and then re-order
    /// the survivors by *arrival* — the timestamp of the oldest play still
    /// inside the rolling window. The squarified-treemap algorithm works on
    /// any order; passing arrival order instead of count-desc scatters the
    /// big tiles through the layout rather than clumping them to one side.
    fn rebuild_art_tiles(&mut self) {
        let cap = collage_tile_cap(&self.config);
        let mut entries: Vec<(u64, &ArtEntry, Instant)> = self
            .art_history
            .iter()
            .filter_map(|(k, e)| e.plays.front().map(|t| (*k, e, *t)))
            .collect();
        // Step 1: keep the most-played covers within the tile cap.
        entries.sort_by_key(|e| std::cmp::Reverse(e.1.plays.len()));
        entries.truncate(cap);
        // Step 2: re-order the survivors by arrival so spatial layout
        // reflects "order they came in", not "how many plays".
        entries.sort_by(|a, b| a.2.cmp(&b.2).then_with(|| a.1.path.cmp(&b.1.path)));
        self.app_state.art_tiles = entries
            .into_iter()
            .map(|(k, e, _)| ArtTile {
                path: e.path.clone(),
                hash: k,
                count: e.plays.len() as u32,
                songs: e.songs.clone(),
                album: e.album.clone(),
            })
            .collect();
    }

    /// Throttled background prune so the rolling collage window keeps moving
    /// during long quiet stretches with no song changes. Called once per UI
    /// frame; does work at most every 60 seconds.
    fn maybe_prune_art_history(&mut self) {
        if self.art_history.is_empty() {
            return;
        }
        let now = Instant::now();
        let due = self
            .last_art_prune_at
            .map(|t| now.duration_since(t) >= Duration::from_secs(60))
            .unwrap_or(true);
        if !due {
            return;
        }
        self.last_art_prune_at = Some(now);
        let Some(cutoff) = now.checked_sub(ART_WINDOW) else {
            return;
        };
        let before = self.art_history.len();
        let mut any_changed = false;
        self.art_history.retain(|_, entry| {
            let original_len = entry.plays.len();
            while entry.plays.front().is_some_and(|t| *t < cutoff) {
                entry.plays.pop_front();
                any_changed = true;
            }
            if entry.plays.len() != original_len {
                any_changed = true;
            }
            !entry.plays.is_empty()
        });
        if any_changed || before != self.art_history.len() {
            self.rebuild_art_tiles();
            self.persist_art_history();
        }

        // Keep the rolling 24-hour play log honest on the same cadence.
        // Cheap walk — the log holds at most a few hundred entries.
        let pre = self.play_log.len();
        self.play_log.prune();
        if self.play_log.len() != pre {
            self.play_log.save();
        }
    }

    /// Throttled cleanup of the AAS scratch directory. nrsc5 dumps every
    /// LOT payload (traffic tiles, raw radar overlays, DWRI text, HERE
    /// weather images) to disk, and the stitched `TrafficMap_NNNN.png`
    /// composites accumulate one per stitch. Only the *latest* traffic map
    /// and the rolling ≤12 `WeatherMap_*` frames are still referenced;
    /// everything older than `AAS_RETENTION` is garbage. Base-map crops and
    /// the stable `TrafficMap.png` alias are kept regardless of age (bounded
    /// and cheap), and anything the UI is currently displaying is exempt.
    /// Cache one station logo for `program` (0-based subchannel) keyed by
    /// the current tuned frequency, returning the durable path. The file
    /// name embeds a short content hash so a genuinely updated logo lands
    /// under a fresh path — egui caches decoded textures by URI, so reusing
    /// a name would keep showing the stale image — while byte-identical
    /// re-sends from the carousel reuse the existing file. Stale logos for
    /// the same (frequency, subchannel) are removed so the cache holds at
    /// most one file per slot.
    fn store_station_logo(
        &self,
        program: u32,
        ext: &str,
        bytes: &[u8],
        source: &str,
    ) -> Option<String> {
        let dir = crate::paths::station_logo_dir()?;
        std::fs::create_dir_all(&dir).ok()?;
        let freq_key = (self.config.frequency_mhz * 10.0).round() as u32;
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        bytes.hash(&mut hasher);
        let hash = hasher.finish() as u32;
        let target = dir.join(format!("{freq_key}_hd{}_{hash:08x}.{ext}", program + 1));
        let source_sidecar = station_logo_source_sidecar_path(&target);
        if !target.exists() {
            std::fs::write(&target, bytes).ok()?;
            let stale_prefix = format!("{freq_key}_hd{}_", program + 1);
            if let Ok(entries) = std::fs::read_dir(&dir) {
                for e in entries.flatten() {
                    if e.file_name().to_string_lossy().starts_with(&stale_prefix)
                        && e.path() != target
                    {
                        let _ = std::fs::remove_file(e.path());
                    }
                }
            }
        }
        let _ = std::fs::write(&source_sidecar, source);
        Some(target.to_string_lossy().into_owned())
    }

    /// Repopulate `station_logo_paths` from the durable logo cache for the
    /// frequency we're (re)tuning to. Clears every slot first; slots with
    /// no cached file stay `None`.
    fn preload_station_logos(&mut self, mhz: f32) {
        self.app_state.station_logo_paths = Default::default();
        self.app_state.station_logo_sources = Default::default();
        let Some(dir) = crate::paths::station_logo_dir() else {
            return;
        };
        let prefix = format!("{}_hd", (mhz * 10.0).round() as u32);
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return;
        };
        for e in entries.flatten() {
            let fname = e.file_name();
            let fname = fname.to_string_lossy();
            // Skip logo source sidecars (`<image>.src`); they share the
            // `{freq}_hd{n}_` prefix and would otherwise be parsed as an
            // image path for slot n and handed to egui, which reports
            // "No matching ImageLoader" when it fails to decode them.
            if fname.ends_with(".src") {
                continue;
            }
            let Some(rest) = fname.strip_prefix(&prefix) else {
                continue;
            };
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(n) = digits.parse::<u32>() {
                if (1..=8).contains(&n) {
                    let idx = (n - 1) as usize;
                    let logo_path = e.path();
                    self.app_state.station_logo_paths[idx] =
                        Some(logo_path.to_string_lossy().into_owned());
                    let source =
                        std::fs::read_to_string(station_logo_source_sidecar_path(&logo_path))
                            .ok()
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty())
                            .unwrap_or_else(|| "Unknown".to_string());
                    self.app_state.station_logo_sources[idx] = Some(format!("{} (Cached)", source));
                }
            }
        }
    }

    fn maybe_prune_aas_scratch(&mut self) {
        let now = Instant::now();
        let due = self
            .last_aas_prune_at
            .map(|t| now.duration_since(t) >= AAS_PRUNE_INTERVAL)
            .unwrap_or(true);
        if !due {
            return;
        }
        self.last_aas_prune_at = Some(now);

        let Some(cutoff) = std::time::SystemTime::now().checked_sub(AAS_RETENTION) else {
            return;
        };

        // Files still in active use that must survive regardless of age.
        let mut keep: std::collections::HashSet<std::path::PathBuf> =
            std::collections::HashSet::new();
        if let Some(p) = self.traffic_map.completed_path.as_deref() {
            keep.insert(std::path::PathBuf::from(p));
        }
        for frame in &self.app_state.weather_frames {
            keep.insert(std::path::PathBuf::from(&frame.path));
        }

        let prune_dir = |dir: &std::path::Path| {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_file() {
                    continue;
                }
                let name = entry.file_name();
                let name = name.to_string_lossy();
                // Never age-delete base-map crops or the latest-traffic alias.
                if name.starts_with("BaseMap_") || name == "TrafficMap.png" {
                    continue;
                }
                // Never delete a file the UI is currently displaying.
                if keep.contains(&path) {
                    continue;
                }
                let Ok(meta) = entry.metadata() else {
                    continue;
                };
                let Ok(modified) = meta.modified() else {
                    continue;
                };
                if modified < cutoff {
                    let _ = std::fs::remove_file(&path);
                }
            }
        };

        prune_dir(&self.aas_dir);
        prune_dir(&self.aas_dir.join("here"));
    }

    /// Serialize the in-memory `art_history` map to disk so the collage
    /// survives restarts. Converts each `Instant` play timestamp to Unix
    /// milliseconds via the wall-clock offset between `Instant::now()` and
    /// `Utc::now()` at call time. Best-effort: writes are atomic via a
    /// .tmp + rename, and any failure is logged but never escalated.
    ///
    /// Also runs a one-shot orphan sweep of the cache directory so files
    /// left over from expired entries are removed promptly.
    fn persist_art_history(&self) {
        let Some(cache) = self.art_cache.as_ref() else {
            return;
        };

        let now_inst = Instant::now();
        let now_utc_ms = Utc::now().timestamp_millis();

        let mut entries: Vec<crate::art_cache::PersistedEntry> =
            Vec::with_capacity(self.art_history.len());
        let mut keep: HashSet<String> = HashSet::with_capacity(self.art_history.len());

        for (hash, entry) in &self.art_history {
            // Convert Instant → wall-clock millis by subtracting the play's
            // age (in monotonic time) from the current wall-clock now.
            let plays_unix_ms: Vec<i64> = entry
                .plays
                .iter()
                .map(|t| {
                    let age = now_inst.saturating_duration_since(*t);
                    now_utc_ms - age.as_millis() as i64
                })
                .collect();

            // Derive the cache-relative filename from the live path so we
            // don't depend on string conventions outside this module.
            let filename = std::path::Path::new(&entry.path)
                .file_name()
                .and_then(|s| s.to_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| {
                    crate::art_cache::ArtCache::filename_for(
                        *hash,
                        std::path::Path::new(&entry.path),
                    )
                });
            keep.insert(filename.clone());

            entries.push(crate::art_cache::PersistedEntry {
                hash: *hash,
                filename,
                plays_unix_ms,
                songs: entry.songs.clone(),
                album: entry.album.clone(),
            });
        }

        if let Err(e) = cache.save_manifest(entries) {
            eprintln!("art-cache: failed to save manifest: {e}");
        }
        cache.sweep_orphans(&keep);
    }

    fn apply_traffic_composite_update(&mut self) {
        self.app_state.traffic_map_path = self.traffic_map.completed_path.clone();
        self.app_state.traffic_map_last_updated_hhmmss =
            Some(Local::now().format("%H:%M:%S").to_string());
        self.app_state.push_payload_log("Traffic composite updated");
    }

    fn apply_weather_frame_update(&mut self) {
        let prev_len = self.app_state.weather_frames.len();
        let prev_idx = self.app_state.weather_current_frame;
        let new_frames = self.weather_map.frames.clone();
        let new_last = new_frames.len().saturating_sub(1);
        // Viewer was "following the live tail" if either the
        // animation is playing or the viewer was already on (or
        // past) the previous newest frame, or we had no frames
        // at all yet.
        let following = prev_len == 0 || self.app_state.weather_playing || prev_idx + 1 >= prev_len;
        // If the buffer was at capacity, the new frame caused the
        // oldest to be dropped, so every existing index shifts
        // down by one.
        let shifted_idx =
            if prev_len == crate::maps::MAX_WEATHER_FRAMES && new_frames.len() == prev_len {
                prev_idx.saturating_sub(1)
            } else {
                prev_idx
            };
        self.app_state.weather_frames = new_frames;
        self.app_state.weather_current_frame = if following {
            new_last
        } else {
            shifted_idx.min(new_last)
        };
        self.app_state.weather_map_last_updated_hhmmss =
            Some(Local::now().format("%H:%M:%S").to_string());
        self.app_state.push_payload_log(format!(
            "Weather frame buffered ({} total)",
            self.app_state.weather_frames.len()
        ));
    }

    fn handle_nrsc5_event(&mut self, evt: NrscEvent) {
        match evt {
            NrscEvent::LostDevice => {
                self.app_state.is_streaming = false;
                self.start_requested_at = None;
                self.last_signal_at = None;
                // The child process has exited; clean up our handle.
                if let Some(nrsc5) = self.nrsc5.as_mut() {
                    nrsc5.stop();
                }
                // Preserve an already-present detailed reason if one was
                // emitted by the backend thread in the same failure cycle.
                if !self.app_state.nrsc5_status.starts_with("device lost:") {
                    self.app_state.nrsc5_status = "device lost".to_string();
                }
            }
            NrscEvent::LostDeviceDetail(detail) => {
                self.app_state.nrsc5_status = format!("device lost: {detail}");
            }
            NrscEvent::ChildExited => {
                // The libnrsc5 feeder thread exited. v0.5.1 runs one
                // session per tune, so this always means the decode
                // pipeline is gone (`stop()` tripped it, the bus
                // died, or libnrsc5 refused a chunk). Treat as
                // pipeline-fatal so the GUI flips back to "stream
                // ended" and the user can re-Start.
                let still_streaming = self
                    .nrsc5
                    .as_ref()
                    .map(|n| n.is_streaming())
                    .unwrap_or(false);
                if self.app_state.is_streaming && !still_streaming {
                    self.app_state.is_streaming = false;
                    self.start_requested_at = None;
                    self.last_signal_at = None;
                    if let Some(nrsc5) = self.nrsc5.as_mut() {
                        nrsc5.stop();
                    }
                    // Preserve any device-loss status already set in
                    // the same failure cycle so we don't paper over a
                    // more specific error with the generic "stream
                    // ended" label.
                    if !self.app_state.nrsc5_status.starts_with("device lost") {
                        self.app_state.nrsc5_status = "stream ended".to_string();
                    }
                }
            }
            NrscEvent::Sync {
                freq_offset_hz,
                psmi,
            } => {
                self.last_signal_at = Some(Instant::now());
                self.app_state.currently_synced = true;
                self.app_state.lost_sync_at = None;
                self.app_state.nrsc5_status = "synced".to_string();
                self.app_state.station_info.sync_freq_offset_hz = Some(freq_offset_hz);
                self.app_state.station_info.sync_psmi = Some(psmi);
            }
            NrscEvent::LostSync => {
                // Stamp the loss; the dock's `available_programs()` and
                // `sync_data_stale()` honor a grace window before
                // actually blanking the UI.
                self.app_state.currently_synced = false;
                if self.app_state.lost_sync_at.is_none() {
                    self.app_state.lost_sync_at = Some(Instant::now());
                }
                self.app_state.nrsc5_status = "sync lost".to_string();
            }
            NrscEvent::Mer { lower, upper } => {
                self.app_state.mer = (lower + upper) / 2.0;
                self.app_state.mer_lower = lower;
                self.app_state.mer_upper = upper;
            }
            NrscEvent::Ber { cber } => {
                self.app_state.ber = cber;
            }
            NrscEvent::Agc { gain_db } => {
                self.app_state.agc_db = gain_db;
                self.app_state.nrsc5_status = format!("best gain: {:.1} dB", gain_db);
            }
            NrscEvent::AgcDecision { tenths, reason: _ } => {
                // Closed-loop AGC just applied a new gain. Mirror it
                // into `agc_db` so the existing readout stays accurate
                // even on the piped backend (where nrsc5 won't emit an
                // `Agc { gain_db }` line of its own). The detailed
                // status (probing/settled/bailed, reason, time since
                // change) is surfaced via `agc_snapshot` in the Signal
                // panel — we deliberately do NOT clobber
                // `nrsc5_status` here, because that string is the
                // single source of truth for stream sync state used by
                // other panels (Constellation lock indicator, etc).
                let db = tenths as f32 / 10.0;
                self.app_state.agc_db = db;
            }
            NrscEvent::AudioStarted { .. } => {
                self.last_signal_at = Some(Instant::now());
                self.app_state.active_program = self.app_state.selected_program;

                if let Some(started) = self.start_requested_at.take() {
                    self.app_state.nrsc5_status = format!(
                        "audio started on HD{} in {:.1}s",
                        self.app_state.selected_program + 1,
                        started.elapsed().as_secs_f32()
                    );
                }
            }
            NrscEvent::AudioBitRate { program, kbps } => {
                // Per-program audio bit rate readout for the Station
                // Info panel. nrsc5 only decodes one program at a time,
                // so this always lands on whatever subchannel the user
                // is currently tuned to. Upsert the slot in case
                // bit-rate lines arrive before any SIG Service / Audio
                // Program line (rare but possible on weak signals).
                let idx = program as usize;
                if idx < 8 {
                    let slot = &mut self.app_state.station_info.programs[idx];
                    let info = slot.get_or_insert_with(|| {
                        crate::station_info::ProgramInfo::from_short_name(String::new())
                    });
                    info.bit_rate_kbps = Some(kbps);
                    self.app_state.station_info.last_updated = Some(Instant::now());
                }
            }
            NrscEvent::Metadata {
                program,
                title,
                artist,
                album,
                genre,
            } => {
                if !self.app_state.is_streaming {
                    return;
                }

                self.last_signal_at = Some(Instant::now());
                self.app_state.active_program = program;

                let now = Instant::now();
                let slot_idx = (program as usize).min(self.app_state.programs.len() - 1);
                let slot = &mut self.app_state.programs[slot_idx];
                if !title.is_empty() {
                    slot.title = title;
                    slot.title_updated = Some(now);
                }
                if !artist.is_empty() {
                    slot.artist = artist;
                    slot.artist_updated = Some(now);
                }
                if !album.is_empty() {
                    slot.album = album;
                    slot.album_updated = Some(now);
                }
                if !genre.is_empty() {
                    slot.genre = genre;
                    slot.genre_updated = Some(now);
                }

                // Try to record this metadata update to the play log only
                // if a fresh cover-art change happened recently. Station
                // slogans / IDs arrive through the same title/artist
                // events but without a corresponding cover swap, so this
                // recent-cover gate filters them out without a blocklist.
                if let Some(last) = self.last_cover_play_at {
                    let now_ms = crate::play_log::now_millis();
                    if now_ms - last < 30_000 {
                        self.try_record_play(program);
                    }
                }
            }
            NrscEvent::LotFile {
                program,
                lot,
                name,
                data,
                mime,
                lot_component_mime,
            } => {
                // Reconstruct the `{lot}_{name}` filename that the old
                // `nrsc5.exe --dump-aas-files` flag used to write.
                // Downstream consumers (`extract_call_sign`,
                // `TrafficMap::process_lot`, `WeatherMap::process_lot`,
                // `WeatherMap::bootstrap_from_cache`) all split on the
                // first `_` to strip that prefix, so the on-disk name
                // and the strings we pass them must include it; otherwise
                // `TMT_…` / `DWRO_…` / `DWRI_…` get their leading token
                // mistaken for the lot prefix and fail every
                // `starts_with` check.
                let filename = if name.is_empty() {
                    String::new()
                } else {
                    format!("{lot}_{name}")
                };
                let image_class = classify_lot_image(&filename, mime, lot_component_mime);
                // Persist the payload to the AAS scratch directory.
                // Downstream map / cover-art code reads it back via
                // `aas_dir.join(filename)`. Empty payloads (libnrsc5
                // emitted a null/zero-size data pointer) are skipped
                // because an empty file would corrupt the image-decode
                // paths.
                #[cfg(debug_assertions)]
                eprintln!(
                    "[lot] arrive lot={} name={} bytes={}",
                    lot,
                    filename,
                    data.len()
                );
                if !filename.is_empty() {
                    let kind = if filename.contains("_TMT_") {
                        "traffic tile"
                    } else if filename.contains("_DWRI_") || filename.contains("_DWRO_") {
                        "weather frame"
                    } else {
                        match &image_class {
                            LotImageClass::StationLogo { definitive, source } => {
                                if *definitive {
                                    match *source {
                                        "lot.mime" => "station logo (lot mime)",
                                        "lot.component.data.mime" => {
                                            "station logo (component mime)"
                                        }
                                        _ => "station logo",
                                    }
                                } else {
                                    "station logo (filename heuristic)"
                                }
                            }
                            LotImageClass::CoverArt { source } => match *source {
                                "lot.mime" => "cover art (lot mime)",
                                "lot.component.data.mime" => "cover art (component mime)",
                                _ => "cover art",
                            },
                            LotImageClass::GenericImage { source } => match *source {
                                "generic_image_mime" => "generic image payload (jpeg/png)",
                                _ => "generic image payload",
                            },
                            LotImageClass::Other => "payload",
                        }
                    };
                    self.app_state.push_payload_log(format!(
                        "LOT {} {} ({} B): {} [mime={}, component={}]",
                        lot,
                        kind,
                        data.len(),
                        filename,
                        mime_hex(mime),
                        lot_component_mime
                            .map(mime_hex)
                            .unwrap_or_else(|| "none".to_string())
                    ));
                }
                if !data.is_empty() && !filename.is_empty() {
                    let path = self.aas_dir.join(&filename);
                    if let Err(e) = std::fs::write(&path, &data) {
                        eprintln!(
                            "[aas] failed to write LOT {} -> {}: {}",
                            lot,
                            path.display(),
                            e
                        );
                    }
                }
                // Station-logo LOT path (MIME-first): classify image type
                // from LOT metadata first, then use filename only as a
                // fallback heuristic when broadcasters emit generic/missing
                // MIME tags.
                if !filename.is_empty() && !data.is_empty() {
                    if let LotImageClass::StationLogo { source, .. } = image_class {
                        let source_label = match source {
                            "lot.mime" => "LOT mime",
                            "lot.component.data.mime" => "LOT component mime",
                            "filename_heuristic" => "LOT heuristic",
                            _ => "LOT",
                        };
                        let parsed = parse_station_logo_filename(&filename);
                        let (logo_program, program_source) = if let Some((p, _)) = parsed.as_ref() {
                            (*p, "filename")
                        } else if program == 0 {
                            // libnrsc5 currently tends to emit 0 for LOT
                            // program; when no filename hint exists we map to
                            // the selected subchannel rather than pinning HD1.
                            (self.app_state.selected_program.min(7), "selected_program")
                        } else {
                            (program.min(7), "lot.program")
                        };
                        let callsign = parsed
                            .as_ref()
                            .map(|(_, cs)| cs.as_str())
                            .filter(|cs| !cs.is_empty())
                            .unwrap_or("unknown");
                        if let Some(cached) = self.store_station_logo(
                            logo_program,
                            &logo_ext(&filename),
                            &data,
                            source_label,
                        ) {
                            let _ = std::fs::remove_file(self.aas_dir.join(&filename));
                            self.app_state.station_logo_paths[logo_program as usize] = Some(cached);
                            self.app_state.station_logo_sources[logo_program as usize] =
                                Some(source_label.to_string());
                            self.app_state.push_payload_log(format!(
                                "Station logo {callsign} -> HD{} (LOT {}, source={}, map={})",
                                logo_program + 1,
                                lot,
                                source,
                                program_source
                            ));
                        }
                    }
                }
                if self.app_state.call_sign.is_empty() {
                    if let Some(cs) = extract_call_sign(&filename) {
                        self.app_state.call_sign = cs;
                    }
                }
                // Feed to map processors before storing.
                let traffic_completed = self.traffic_map.process_lot(&filename);
                #[cfg(debug_assertions)]
                if filename.contains("_TMT_") {
                    eprintln!(
                        "[map] traffic tile name={} completed_stitch={}",
                        filename, traffic_completed
                    );
                }
                if traffic_completed {
                    self.apply_traffic_composite_update();
                } else if self.traffic_map.completed_path.is_none() {
                    // Source switched or map was cleared; avoid showing stale
                    // composite from the previous feed while new tiles arrive.
                    self.app_state.traffic_map_path = None;
                    self.app_state.traffic_map_last_updated_hhmmss = None;
                }
                let weather_new = self.weather_map.process_lot(&filename);
                #[cfg(debug_assertions)]
                if filename.contains("_DWRI_") || filename.contains("_DWRO_") {
                    eprintln!(
                        "[map] weather lot name={} new_frame={} total_frames={}",
                        filename,
                        weather_new,
                        self.weather_map.frames.len()
                    );
                }
                if weather_new {
                    self.apply_weather_frame_update();
                }
                self.lot_files.insert(lot, filename);
            }
            NrscEvent::Xhdr {
                program, mime, lot, ..
            } => {
                // The album-art vs. station-logo distinction is carried
                // by the XHDR MIME type, NOT by `param` (which is the
                // upstream LOT-present flag). `NRSC5_MIME_STATION_LOGO`
                // is the only value that means "this is a logo"; every
                // other image MIME (`NRSC5_MIME_PRIMARY_IMAGE`, JPEG,
                // PNG) is now-playing cover art.
                if let Some(filename) = self.lot_files.get(&lot) {
                    let full_path = self.aas_dir.join(filename);
                    if full_path.exists() {
                        let path_str = full_path.to_string_lossy().to_string();
                        if mime == crate::ffi::nrsc5_sys::NRSC5_MIME_STATION_LOGO {
                            // XHDR-delivered logo — cache it against the
                            // now-playing subchannel and drop the AAS scratch
                            // copy. Also temporarily takes over the Now Playing
                            // artwork slot until the next cover-art XHDR arrives.
                            let ext = logo_ext(filename);
                            if let Some(cached) = std::fs::read(&full_path).ok().and_then(|bytes| {
                                self.store_station_logo(program, &ext, &bytes, "XHDR mime")
                            }) {
                                let _ = std::fs::remove_file(&full_path);
                                self.app_state.station_logo_paths[program as usize] = Some(cached);
                                self.app_state.station_logo_sources[program as usize] =
                                    Some("XHDR mime".to_string());
                            }
                            self.app_state.now_playing_image_mode =
                                NowPlayingImageMode::StationLogo;
                            self.app_state.push_payload_log(format!(
                                "XHDR station logo HD{} (LOT {})",
                                program + 1,
                                lot
                            ));
                        } else {
                            self.app_state.now_playing_image_mode = NowPlayingImageMode::CoverArt;
                            self.app_state.push_payload_log(format!(
                                "XHDR cover art -> HD{} (LOT {})",
                                program + 1,
                                lot
                            ));
                            // Cover art. `record_album_art` sets
                            // `programs[program].cover_art_path` itself
                            // (preferring the durable cache copy) and
                            // prunes the AAS-dir dump after a successful
                            // cache write.
                            self.record_album_art(program, &full_path, &path_str);
                        }
                    }
                }
            }
            NrscEvent::HereImage {
                image_type,
                seq,
                n1,
                n2,
                latitude1,
                longitude1,
                latitude2,
                longitude2,
                has_time_utc,
                name,
                size,
                data,
            } => {
                let kind = here_image_type_label(image_type);
                let safe_name = sanitize_filename(&name);
                let leaf = if safe_name == "_" {
                    "payload.bin".to_string()
                } else {
                    safe_name
                };
                let filename = format!("HERE_{}_{}_{}", image_type, seq, leaf);
                let here_dir = self.aas_dir.join("here");
                let path = here_dir.join(&filename);

                if data.is_empty() {
                    self.app_state.push_payload_log(format!(
                        "HERE {} seq={} signaled {} B (empty payload): {}",
                        kind, seq, size, filename
                    ));
                    self.app_state.push_payload_log(format!(
                        "HERE meta seq={} tile=({}, {}) bbox=({:.4}, {:.4})..({:.4}, {:.4}) utc={}",
                        seq,
                        n1,
                        n2,
                        latitude1,
                        longitude1,
                        latitude2,
                        longitude2,
                        if has_time_utc { "present" } else { "none" }
                    ));
                } else {
                    if let Err(e) = std::fs::create_dir_all(&here_dir) {
                        eprintln!(
                            "[here] failed to create output dir {}: {}",
                            here_dir.display(),
                            e
                        );
                    } else if let Err(e) = std::fs::write(&path, &data) {
                        eprintln!("[here] failed to write {}: {}", path.display(), e);
                    }
                    self.app_state.push_payload_log(format!(
                        "HERE {} seq={} ({} B): {}",
                        kind,
                        seq,
                        data.len(),
                        filename
                    ));
                    self.app_state.push_payload_log(format!(
                        "HERE meta seq={} tile=({}, {}) bbox=({:.4}, {:.4})..({:.4}, {:.4}) utc={}",
                        seq,
                        n1,
                        n2,
                        latitude1,
                        longitude1,
                        latitude2,
                        longitude2,
                        if has_time_utc { "present" } else { "none" }
                    ));

                    let rel_filename = format!("here/{}", filename);
                    if image_type == crate::ffi::nrsc5_sys::NRSC5_HERE_IMAGE_TRAFFIC {
                        let traffic_completed =
                            self.traffic_map.process_here_tile(&rel_filename, n1, n2);
                        #[cfg(debug_assertions)]
                        eprintln!(
                            "[map] HERE traffic tile name={} tile=({}, {}) completed_stitch={}",
                            rel_filename, n1, n2, traffic_completed
                        );
                        if traffic_completed {
                            self.apply_traffic_composite_update();
                        } else if self.traffic_map.completed_path.is_none() {
                            self.app_state.traffic_map_path = None;
                            self.app_state.traffic_map_last_updated_hhmmss = None;
                        }
                    } else if image_type == crate::ffi::nrsc5_sys::NRSC5_HERE_IMAGE_WEATHER {
                        let weather_new = self.weather_map.process_here_image(
                            &rel_filename,
                            Some([
                                latitude1 as f64,
                                longitude1 as f64,
                                latitude2 as f64,
                                longitude2 as f64,
                            ]),
                        );
                        #[cfg(debug_assertions)]
                        eprintln!(
                            "[map] HERE weather image name={} new_frame={} total_frames={}",
                            rel_filename,
                            weather_new,
                            self.weather_map.frames.len()
                        );
                        if weather_new {
                            self.apply_weather_frame_update();
                        }
                    }
                }
            }
            NrscEvent::StationName(name) => {
                self.app_state.station_info.call_sign = Some(name);
                self.app_state.station_info.last_updated = Some(Instant::now());
            }
            NrscEvent::Slogan(text) => {
                self.app_state.station_info.slogan = Some(text);
                self.app_state.station_info.last_updated = Some(Instant::now());
            }
            NrscEvent::Message(text) => {
                self.app_state.station_info.message = Some(text);
                self.app_state.station_info.last_updated = Some(Instant::now());
            }
            NrscEvent::RdsProgramService(text) => {
                self.app_state.station_info.push_rds_ps(&text);
                self.app_state.station_info.last_updated = Some(Instant::now());
            }
            NrscEvent::RdsRadioText(text) => {
                self.app_state.station_info.push_rds_radiotext(&text);
                self.app_state.station_info.last_updated = Some(Instant::now());
            }
            NrscEvent::Location {
                latitude,
                longitude,
                altitude_m,
            } => {
                self.app_state.station_info.location = Some(crate::station_info::Location {
                    latitude,
                    longitude,
                    altitude_m,
                });
                self.app_state.station_info.last_updated = Some(Instant::now());
            }
            NrscEvent::CountryFcc {
                country,
                facility_id,
            } => {
                self.app_state.station_info.country = Some(country);
                self.app_state.station_info.fcc_facility_id = Some(facility_id);
                self.app_state.station_info.last_updated = Some(Instant::now());
            }
            NrscEvent::SigServiceAudio { number, name } => {
                // `number` is 1-indexed (HD1..HD8). Upsert into the
                // 0-indexed program slot — create if missing, update
                // the short name if a slot already exists from an
                // earlier AudioProgram event.
                if (1..=8).contains(&number) {
                    let idx = (number - 1) as usize;
                    let slot = &mut self.app_state.station_info.programs[idx];
                    match slot {
                        Some(info) => info.short_name = name,
                        None => {
                            *slot = Some(crate::station_info::ProgramInfo::from_short_name(name));
                        }
                    }
                    self.app_state.station_info.last_updated = Some(Instant::now());
                }
            }
            NrscEvent::AudioProgram {
                number,
                program_type,
                sound_experience,
            } => {
                // Same upsert pattern as SigServiceAudio — either event
                // can arrive first depending on the SIS cycle.
                if (1..=8).contains(&number) {
                    let idx = (number - 1) as usize;
                    let slot = &mut self.app_state.station_info.programs[idx];
                    let info = slot.get_or_insert_with(|| {
                        crate::station_info::ProgramInfo::from_short_name(String::new())
                    });
                    info.program_type = Some(program_type);
                    info.sound_experience = Some(sound_experience);
                    self.app_state.station_info.last_updated = Some(Instant::now());
                }
            }
            NrscEvent::SigServiceData { number, name } => {
                // Dedup by SIS-assigned number: SIS repeats every few
                // seconds, so replace any existing entry rather than
                // appending duplicates.
                let services = &mut self.app_state.station_info.data_services;
                if let Some(existing) = services.iter_mut().find(|s| s.number == number) {
                    existing.name = name;
                } else {
                    services.push(crate::station_info::DataService {
                        number,
                        name,
                        mime: None,
                        service_data_type: None,
                    });
                }
                self.app_state.station_info.last_updated = Some(Instant::now());
            }
            NrscEvent::EmergencyAlert { text } => {
                self.app_state.station_info.alert = Some(text);
                self.app_state.station_info.last_updated = Some(Instant::now());
            }
            NrscEvent::ExciterInfo {
                manufacturer_id,
                core_version,
                core_status,
                manufacturer_version,
                manufacturer_status,
                importer_connected,
            } => {
                self.app_state.station_info.exciter = Some(crate::station_info::EquipmentInfo {
                    manufacturer_id,
                    core_version,
                    core_status,
                    manufacturer_version,
                    manufacturer_status,
                    importer_connected: Some(importer_connected),
                });
                self.app_state.station_info.last_updated = Some(Instant::now());
            }
            NrscEvent::ImporterInfo {
                manufacturer_id,
                core_version,
                core_status,
                manufacturer_version,
                manufacturer_status,
            } => {
                self.app_state.station_info.importer = Some(crate::station_info::EquipmentInfo {
                    manufacturer_id,
                    core_version,
                    core_status,
                    manufacturer_version,
                    manufacturer_status,
                    importer_connected: None,
                });
                self.app_state.station_info.last_updated = Some(Instant::now());
            }
            NrscEvent::LocalTime {
                utc_offset_minutes,
                dst_regional,
                dst_local,
                dst_schedule,
            } => {
                self.app_state.station_info.local_time = Some(crate::station_info::LocalTimeInfo {
                    utc_offset_minutes,
                    dst_regional,
                    dst_local,
                    dst_schedule,
                });
                self.app_state.station_info.last_updated = Some(Instant::now());
            }
            NrscEvent::LeapSecondOffset {
                pending_offset,
                current_offset,
                pending_alfn,
            } => {
                self.app_state.station_info.leap_second =
                    Some(crate::station_info::LeapSecondInfo {
                        current_offset,
                        pending_offset,
                        pending_alfn,
                    });
                self.app_state.station_info.last_updated = Some(Instant::now());
            }
            NrscEvent::SyncAm {
                pli,
                hppi,
                aabi,
                rdbi,
            } => {
                // Plumbed for diagnostics; not rendered today.
                self.app_state.station_info.am_sync = Some(crate::station_info::AmSyncIndicators {
                    pli,
                    hppi,
                    aabi,
                    rdbi,
                });
            }
        }
    }

    fn handle_command(&mut self, command: UiCommand) {
        match command {
            UiCommand::Start => {
                if self.app_state.is_streaming {
                    self.app_state.nrsc5_status = "stream already running".to_string();
                    return;
                }

                if self.nrsc5.is_none() {
                    match Nrsc5Process::new() {
                        Ok(mut p) => {
                            if let Some(tap) = self.app_state.spectrum_tap.clone() {
                                p.set_spectrum_tap(tap);
                            }
                            self.nrsc5 = Some(p);
                        }
                        Err(err) => {
                            self.app_state.nrsc5_status = format!("NRSC5 unavailable: {err}");
                            return;
                        }
                    }
                }

                let nrsc5 = self.nrsc5.as_mut().unwrap();
                let mhz = self.app_state.frequency_mhz;
                let program = self.app_state.selected_program;

                // v0.5.0: every Start path goes through the in-process
                // SoapySDR or rtl_tcp backend (`start_piped`). The legacy
                // USB-direct and `nrsc5 -H` dispatchers were removed when
                // the explicit `sdr.transport` field landed. Remote
                // transports (SoapyRemote, rtl_tcp) plug into the same
                // piped pipeline via the transport-aware backend opener.
                let transport = self.config.sdr.transport;
                let sdr_args = self.config.sdr.to_args_string();
                let remote_owned = match transport {
                    crate::config::SdrTransport::RtlTcpRemote
                    | crate::config::SdrTransport::SoapyRemote => Some((
                        self.config.sdr.effective_remote_host(),
                        self.config.sdr.effective_remote_port(),
                    )),
                    crate::config::SdrTransport::LocalSoapy => None,
                };
                // Antenna picker is meaningless on rtl_tcp (single
                // input); skip the profile default lookup for that
                // transport so we don't waste a `set_antenna` call on
                // a backend that ignores it.
                let antenna = if matches!(transport, crate::config::SdrTransport::RtlTcpRemote) {
                    None
                } else {
                    self.config.sdr.antenna.clone().or_else(|| {
                        crate::sdr::profile::lookup(&self.config.sdr.driver)
                            .and_then(|p| p.default_antenna.map(String::from))
                    })
                };
                let result = nrsc5.start_piped(
                    mhz,
                    program,
                    transport,
                    &sdr_args,
                    remote_owned.as_ref().map(|(h, p)| (h.as_str(), *p)),
                    self.config.sdr.freq_correction_ppm,
                    self.config.gain_mode,
                    self.config.manual_gain_tenths,
                    antenna,
                    self.config.agc_amp_target_dbfs_override,
                    self.config.analog_fallback_mode,
                    self.config.analog_fallback_stereo,
                    self.config.analog_fallback_rds_enabled,
                );

                if let Err(err) = result {
                    self.app_state.nrsc5_status = format!("start failed: {err}");
                    return;
                }

                self.app_state.is_streaming = true;
                self.start_requested_at = Some(Instant::now());
                self.last_signal_at = None;
                // Note: we deliberately do NOT clear the collage here. The
                // 8-hour rolling window already prunes stale plays on its
                // own, and the on-disk cache makes the heat-map persistent
                // across Start/Stop and full app restarts. A user-driven
                // "wipe collage" affordance can live on the Collage tab if
                // we ever need one.
                self.app_state.nrsc5_status = format!(
                    "started {mhz:.1} MHz HD{}; waiting for sync...",
                    program + 1
                );
            }
            UiCommand::Stop => {
                if !self.app_state.is_streaming {
                    self.app_state.nrsc5_status = "stream already stopped".to_string();
                    return;
                }

                // Phase 4: a Stop tears down every decoder, which
                // takes the recorder's source ring with it. Flush
                // the .opus file cleanly *before* killing the
                // stream so the EOS page is the last thing on
                // disk \u2014 otherwise the file would just end at
                // whatever frame the encoder happened to have
                // queued when the channel disconnected.
                if self.recording_session.is_some() {
                    self.stop_recording(/* fatal = */ true);
                }

                if let Some(nrsc5) = self.nrsc5.as_mut() {
                    nrsc5.stop();
                }

                self.app_state.is_streaming = false;
                self.start_requested_at = None;
                self.last_signal_at = None;
                self.app_state.currently_synced = false;
                self.app_state.lost_sync_at = None;
                self.lot_files.clear();
                self.app_state.now_playing_image_mode = NowPlayingImageMode::CoverArt;
                self.app_state.traffic_map_path = None;
                self.app_state.traffic_map_last_updated_hhmmss = None;
                self.app_state.weather_frames.clear();
                self.app_state.weather_map_last_updated_hhmmss = None;
                self.app_state.weather_current_frame = 0;
                self.app_state.weather_playing = false;
                self.app_state.clear_payload_log();
                self.app_state.call_sign.clear();
                // Clear aggregated SIS so a fresh Start re-discovers identity
                // from scratch rather than rendering stale fields from the
                // last session.
                self.app_state.station_info.reset();
                // Wipe per-program PSD + cover art so the Station Info
                // panel doesn't claim the last-heard track is the
                // "current" one once the stream is no longer running.
                self.app_state.clear_all_programs();
                self.app_state.active_speaker = None;
                self.traffic_map.clear();
                self.weather_map.clear();
                self.app_state.nrsc5_status = "stream stopped".to_string();
            }
            UiCommand::TuneMhz(mhz) => {
                let mhz = snap_fm_tune_mhz(mhz);
                self.app_state.frequency_mhz = mhz;
                // Phase 4: a tune is a station change — the recording's
                // station-identity metadata (and the per-station
                // subfolder if enabled) is now wrong. Close the file
                // before we move on; the user can hit Record again
                // after the new station's SIS arrives if they want
                // to capture it. Treat as fatal so the status line
                // reflects "saved <path>" instead of "stopped by
                // user".
                if self.recording_session.is_some() {
                    self.stop_recording(/* fatal = */ true);
                }
                // Wipe per-station identity/state so a stale call sign,
                // SIS data, or LOT filename from the previous station
                // can't bleed into the new one before its first SIS
                // cycle arrives. Step 12 will add LostSync-grace-period
                // handling on top so brief flickers don't blank the
                // panel.
                self.app_state.station_info.reset();
                self.app_state.currently_synced = false;
                self.app_state.lost_sync_at = None;
                self.app_state.call_sign.clear();
                self.app_state.now_playing_image_mode = NowPlayingImageMode::CoverArt;
                // meters and AGC-status readout don't show last station's
                // value during the gap between tune and first new MER
                // event. The next NRSC5_EVENT_MER refills these once
                // libnrsc5 re-acquires sync on the new carrier.
                self.app_state.mer = 0.0;
                self.app_state.mer_lower = 0.0;
                self.app_state.mer_upper = 0.0;
                // PSD belongs to the previous station's broadcast; clear
                // every program slot so the panel doesn't show the wrong
                // song while the new station's SIS / PSD roll in.
                self.app_state.clear_all_programs();
                // Repopulate logos from the durable cache for the new dial
                // position so a previously-heard station's logos appear
                // instantly, before its SL carousel comes back around.
                self.preload_station_logos(mhz);
                self.lot_files.clear();
                self.app_state.traffic_map_last_updated_hhmmss = None;
                self.app_state.weather_map_last_updated_hhmmss = None;
                self.app_state.clear_payload_log();
                self.config.frequency_mhz = mhz;
                save_config(&self.config);

                if self.retune_task.is_some() {
                    self.app_state.nrsc5_status = "retune already in progress...".to_string();
                    return;
                }

                // If streaming, retune in a background thread.
                if self.app_state.is_streaming {
                    if let Some(backend) = self.nrsc5.take() {
                        self.app_state.is_streaming = false;

                        let program = self.app_state.selected_program;
                        let handle = std::thread::spawn(move || {
                            let mut backend = backend;
                            if let Err(err) = backend.retune(mhz, program) {
                                return (backend, Some(format!("{err}")));
                            }
                            (backend, None)
                        });

                        self.retune_task = Some(handle);
                        self.app_state.nrsc5_status = format!("retuning to {mhz:.1} MHz...");
                        return;
                    }
                }

                self.start_requested_at = None;
                self.app_state.nrsc5_status = format!(
                    "tuned to {mhz:.1} MHz (HD{})",
                    self.app_state.selected_program + 1
                );
            }
            UiCommand::SelectProgram(program) => {
                let clamped = program.min(7);

                // v0.5.1 single-session-per-tune: every advertised
                // subchannel is decoded by the one libnrsc5 session
                // (PCM is demuxed by program into per-program rings),
                // so selecting an HD just switches which ring the
                // speaker router reads from. No per-program decoder
                // spawn, no "is it decoding yet" check.
                self.app_state.selected_program = clamped;
                self.config.selected_program = clamped;
                save_config(&self.config);

                if self.app_state.is_streaming {
                    if let Some(nrsc5) = self.nrsc5.as_mut() {
                        match nrsc5.set_active_speaker(clamped) {
                            Ok(()) => {
                                self.app_state.nrsc5_status =
                                    format!("switched to HD{}", clamped + 1);
                            }
                            Err(err) => {
                                self.app_state.nrsc5_status =
                                    format!("could not switch to HD{}: {err}", clamped + 1);
                            }
                        }
                    }
                    return;
                }

                self.app_state.nrsc5_status = format!("selected HD{} (staged)", clamped + 1);
            }
            UiCommand::SetShowHd5Hd8(flag) => {
                self.app_state.show_hd5_hd8 = flag;
                self.config.show_hd5_hd8 = flag;
                save_config(&self.config);
            }
            UiCommand::SetSpectrumSmoothingEnabled(enabled) => {
                self.app_state.spectrum_smoothing_enabled = enabled;
                self.config.spectrum_smoothing_enabled = enabled;
                self.apply_spectrum_smoothing();
                save_config(&self.config);
            }
            UiCommand::SetSpectrumSmoothingAlpha(alpha) => {
                let snapped = ((alpha.clamp(0.1, 1.0) * 10.0).round() / 10.0).clamp(0.1, 1.0);
                self.app_state.spectrum_smoothing_alpha = snapped;
                self.config.spectrum_smoothing_alpha = snapped;
                self.apply_spectrum_smoothing();
                save_config(&self.config);
            }
            UiCommand::StartRecording => self.start_recording(),
            UiCommand::StopRecording => self.stop_recording(/* fatal = */ false),
            UiCommand::SetPresetSlotCount(n) => {
                let clamped = n.clamp(1, 48);
                self.app_state.preset_slot_count = clamped;
                self.config.preset_slot_count = clamped;
                save_config(&self.config);
            }
            UiCommand::SetRecordingMode(mode) => {
                self.config.recording_mode = mode;
                self.app_state.recording_mode = mode;
                save_config(&self.config);
                self.app_state.nrsc5_status = match mode {
                    crate::config::RecordingMode::Off => "recording disabled".to_string(),
                    crate::config::RecordingMode::On => {
                        "recording enabled (rotates at max minutes)".to_string()
                    }
                };
            }
            UiCommand::SetRecordingMaxMinutes(mins) => {
                let clamped = mins.clamp(1, 240);
                self.config.recording_max_minutes = clamped;
                save_config(&self.config);
            }
            UiCommand::SetRecordingSubfolderPerStation(flag) => {
                self.config.recording_subfolder_per_station = flag;
                save_config(&self.config);
            }
            UiCommand::SetRecordingDir(dir) => {
                self.config.recording_dir = dir;
                save_config(&self.config);
            }
            UiCommand::SavePreset(slot) => {
                // Fallback chain for the preset's display name:
                //   1. SIS per-program short name (e.g. "The Eagle")
                //   2. currently-playing artist (when audio is up)
                //   3. SIS-reported station call sign (e.g. "KEGL-FM")
                //   4. LOT-derived call sign (heuristic from filenames)
                //   5. bare "HDn" label
                let idx = self.app_state.selected_program as usize;
                let short = self
                    .app_state
                    .station_info
                    .programs
                    .get(idx)
                    .and_then(|s| s.as_ref())
                    .map(|p| p.short_name.clone())
                    .unwrap_or_default();
                let name = if !short.is_empty() {
                    short
                } else if !self.app_state.active_program().artist.is_empty() {
                    self.app_state.active_program().artist.clone()
                } else if let Some(cs) = self.app_state.station_info.call_sign.clone() {
                    cs
                } else if !self.app_state.call_sign.is_empty() {
                    self.app_state.call_sign.clone()
                } else {
                    format!("HD{}", idx + 1)
                };
                let preset = crate::config::Preset {
                    name,
                    frequency_mhz: snap_fm_tune_mhz(self.app_state.frequency_mhz),
                    program: self.app_state.selected_program,
                };
                // Extend the vec if needed.
                while self.config.presets.len() <= slot {
                    self.config.presets.push(crate::config::Preset::default());
                }
                self.config.presets[slot] = preset;
                save_config(&self.config);
                self.app_state.nrsc5_status = format!("saved preset {}", slot + 1);
            }
            UiCommand::SetPreset(slot, preset) => {
                let mut preset = preset;
                preset.frequency_mhz = snap_fm_tune_mhz(preset.frequency_mhz);
                // Extend the vec if needed so editing an empty slot works.
                while self.config.presets.len() <= slot {
                    self.config.presets.push(crate::config::Preset::default());
                }
                self.config.presets[slot] = preset;
                save_config(&self.config);
                self.app_state.nrsc5_status = format!("saved preset {}", slot + 1);
            }
            UiCommand::ClearPreset(slot) => {
                if slot < self.config.presets.len() {
                    self.config.presets[slot] = crate::config::Preset::default();
                    save_config(&self.config);
                    self.app_state.nrsc5_status = format!("cleared preset {}", slot + 1);
                }
            }
            UiCommand::RecallPreset(slot) => {
                if let Some(preset) = self.config.presets.get(slot).cloned() {
                    let tuned = snap_fm_tune_mhz(preset.frequency_mhz);
                    self.app_state.frequency_mhz = tuned;
                    self.app_state.selected_program = preset.program;
                    self.app_state.nrsc5_status =
                        format!("preset {}: {:.1} HD{}", slot + 1, tuned, preset.program + 1);
                    // If streaming, retune to the new station.
                    if self.app_state.is_streaming {
                        self.handle_command(UiCommand::TuneMhz(tuned));
                    }
                }
            }
            UiCommand::SetVolume(value) => {
                let value = value.clamp(0.0, 1.0);
                self.app_state.volume = value;
                self.config.volume = value;
                self.apply_volume();
            }
            UiCommand::SetMute(mute) => {
                self.app_state.muted = mute;
                self.config.muted = mute;
                self.apply_mute();
            }
            UiCommand::SetCollageTileCap(value) => {
                // Snap to nearest power of two in [1, ART_TILES_HARD_MAX]. The
                // UI only ever emits exact powers of two, but defensive
                // clamping protects against hand-edited configs.
                let snapped = (value.max(1) as usize)
                    .min(ART_TILES_HARD_MAX)
                    .next_power_of_two()
                    .min(ART_TILES_HARD_MAX) as u32;
                if self.config.collage_max_tiles == snapped {
                    return;
                }
                self.config.collage_max_tiles = snapped;
                self.app_state.collage_tile_cap = snapped;
                self.rebuild_art_tiles();
                save_config(&self.config);
            }
            UiCommand::ClearCollage => {
                // Drop in-memory history first; rebuild empties the UI
                // tiles, then persist with an empty manifest. The orphan
                // sweep inside persist_art_history will delete every
                // image file in the cache dir since `keep` is empty.
                if self.art_history.is_empty() && self.app_state.art_tiles.is_empty() {
                    return;
                }
                self.art_history.clear();
                self.app_state.art_session_started = None;
                self.rebuild_art_tiles();
                self.persist_art_history();
            }
            UiCommand::BlockCover(hash) => {
                // Add to the persistent block list.
                if self.art_blocklist.insert(hash) {
                    self.config.art_blocklist =
                        self.art_blocklist.iter().map(|&h| h as i64).collect();
                    save_config(&self.config);
                    self.app_state.art_blocklist_count = self.art_blocklist.len();
                }
                // Remove from history and delete the cached file on disk.
                if let Some(entry) = self.art_history.remove(&hash) {
                    let _ = std::fs::remove_file(&entry.path);
                }
                self.rebuild_art_tiles();
                self.persist_art_history();
            }
            UiCommand::ClearArtBlocklist => {
                if !self.art_blocklist.is_empty() {
                    self.art_blocklist.clear();
                    self.config.art_blocklist.clear();
                    save_config(&self.config);
                    self.app_state.art_blocklist_count = 0;
                }
            }
            UiCommand::SetCollageSecondaryClickFallback(enabled) => {
                self.app_state.collage_secondary_click_fallback = enabled;
                self.config.collage_secondary_click_fallback = enabled;
                save_config(&self.config);
            }
            UiCommand::SetGainMode(mode) => {
                if self.config.gain_mode == mode {
                    self.app_state.gain_mode = mode;
                    return;
                }
                self.config.gain_mode = mode;
                self.app_state.gain_mode = mode;
                save_config(&self.config);
            }
            UiCommand::SetAgcAmpTargetDbfs(target) => {
                // Clamp at the boundary so a hand-edited config can't
                // sneak past the UI's [-30, -10] window. None clears
                // the override and reverts to the per-device profile
                // default on the next tune. No hot-apply: the override
                // only matters during the AmpProbe phase which runs
                // exactly once per cold-start tune, so this lands in
                // config + AppState and the next TuneMhz picks it up.
                let normalized = target.map(|t| t.clamp(-30.0, -10.0));
                if self.config.agc_amp_target_dbfs_override == normalized {
                    self.app_state.agc_amp_target_dbfs_override = normalized;
                    return;
                }
                self.config.agc_amp_target_dbfs_override = normalized;
                self.app_state.agc_amp_target_dbfs_override = normalized;
                save_config(&self.config);
            }
            UiCommand::SetManualGainTenths(tenths) => {
                // Snap to the nearest gain-table step for the active
                // device profile. RTL-SDR has 29 discrete R820T2 steps;
                // SDRplay's table is synthesized 1 dB. Falls back to no-op
                // for unknown drivers (the value goes through unsnapped).
                let table = crate::sdr::profile::lookup(&self.config.sdr.driver)
                    .map(|p| p.agc_tenths_table)
                    .unwrap_or(&[]);
                let snapped = snap_to_gain_table(tenths, table);
                if self.config.manual_gain_tenths == snapped {
                    self.app_state.manual_gain_tenths = snapped;
                    return;
                }
                self.config.manual_gain_tenths = snapped;
                self.app_state.manual_gain_tenths = snapped;
                save_config(&self.config);
                // Hot-apply if streaming in Manual mode \u2014 same
                // infrastructure the closed-loop AGC uses, so the
                // slider drag feels identical to AGC probing (brief
                // distortion blip, no audio gap). Outside Manual
                // mode the slider isn't visible anyway, but we still
                // guard so a programmatic SetManualGainTenths during
                // Auto doesn't fight the AGC.
                if self.config.gain_mode == crate::config::GainMode::Manual {
                    if let Some(nrsc5) = self.nrsc5.as_mut() {
                        if let Err(e) = nrsc5.set_manual_gain_tenths(snapped) {
                            eprintln!("[gain] hot-apply failed: {e}");
                        }
                    }
                }
            }
            UiCommand::SetAnalogFallbackMode(mode) => {
                if self.config.analog_fallback_mode == mode {
                    self.app_state.analog_fallback_mode = mode;
                    return;
                }
                self.config.analog_fallback_mode = mode;
                self.app_state.analog_fallback_mode = mode;
                save_config(&self.config);
                if self.app_state.is_streaming {
                    self.app_state.nrsc5_status = match mode {
                        crate::config::AnalogFallbackMode::DigitalOnly => {
                            "restarting stream to disable analog fallback".to_string()
                        }
                        crate::config::AnalogFallbackMode::Automatic => {
                            "restarting stream to apply automatic analog fallback".to_string()
                        }
                        crate::config::AnalogFallbackMode::AnalogOnly => {
                            "restarting stream to force analog fallback".to_string()
                        }
                    };
                    self.handle_command(UiCommand::Stop);
                    self.handle_command(UiCommand::Start);
                }
            }
            UiCommand::SetAnalogFallbackStereo(stereo) => {
                if self.config.analog_fallback_stereo == stereo {
                    self.app_state.analog_fallback_stereo = stereo;
                    return;
                }
                self.config.analog_fallback_stereo = stereo;
                self.app_state.analog_fallback_stereo = stereo;
                save_config(&self.config);
                if self.app_state.is_streaming {
                    self.app_state.nrsc5_status = if stereo {
                        "restarting stream to enable stereo analog fallback".to_string()
                    } else {
                        "restarting stream to force mono analog fallback".to_string()
                    };
                    self.handle_command(UiCommand::Stop);
                    self.handle_command(UiCommand::Start);
                }
            }
            UiCommand::SetAnalogFallbackRdsEnabled(enabled) => {
                if self.config.analog_fallback_rds_enabled == enabled {
                    self.app_state.analog_fallback_rds_enabled = enabled;
                    return;
                }
                self.config.analog_fallback_rds_enabled = enabled;
                self.app_state.analog_fallback_rds_enabled = enabled;
                save_config(&self.config);
                if self.app_state.is_streaming {
                    self.app_state.nrsc5_status = if enabled {
                        "restarting stream to enable analog RDS".to_string()
                    } else {
                        "restarting stream to disable analog RDS".to_string()
                    };
                    self.handle_command(UiCommand::Stop);
                    self.handle_command(UiCommand::Start);
                }
            }
            UiCommand::ExportLogCsv => {
                // Native Save-As dialog defaults to Documents with a
                // timestamped filename. User can redirect anywhere (e.g.
                // OneDrive, a USB stick, the portable bundle's own folder).
                let suggested_filename = crate::play_log::suggested_csv_filename();
                let start_dir =
                    crate::paths::documents_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
                let chosen = rfd::FileDialog::new()
                    .set_title("Export play log as CSV")
                    .set_directory(&start_dir)
                    .set_file_name(&suggested_filename)
                    .add_filter("CSV", &["csv"])
                    .save_file();
                let Some(path) = chosen else {
                    // User cancelled — silent, matching Windows convention.
                    return;
                };
                match self.play_log.export_csv(&path) {
                    Ok(()) => {
                        self.app_state.log_export_status =
                            Some(format!("saved to {}", path.display()));
                    }
                    Err(err) => {
                        self.app_state.log_export_status = Some(format!("export failed: {err}"));
                    }
                }
            }
            UiCommand::ClearLog => {
                if self.play_log.is_empty() {
                    return;
                }
                self.play_log.clear_all();
                self.play_log.save();
                self.app_state.log_export_status = Some("log cleared".to_string());
            }
            UiCommand::SetPlayLogRetention(hours) => {
                let snapped = crate::play_log::clamp_retention(hours);
                if snapped == self.config.play_log_retention_hours {
                    return;
                }
                self.config.play_log_retention_hours = snapped;
                save_config(&self.config);
                let pre = self.play_log.len();
                self.play_log.set_retention_hours(snapped);
                if self.play_log.len() != pre {
                    self.play_log.save();
                }
            }
            UiCommand::ShowSdrSettings => {
                // First-open: refresh the device list so the picker has
                // something to show. Subsequent opens use the cached
                // list; users can hit "Refresh" inside the modal to
                // re-enumerate on demand.
                if !self.app_state.show_sdr_settings
                    && should_auto_refresh_sdr_devices(
                        self.config.sdr.transport,
                        self.sdr_refresh_in_flight,
                        self.app_state.sdr_devices.is_empty(),
                    )
                {
                    self.refresh_sdr_devices();
                }
                // Seed the remote-input edit buffers from config so
                // the Host/Port/Extra-args fields show the persisted
                // values when the modal opens. Skip if the user has
                // already typed into the buffers in this session
                // (`sdr_remote_buf_seeded`) so we don't clobber an
                // in-progress edit on a re-open.
                if !self.app_state.sdr_remote_buf_seeded {
                    self.app_state.sdr_remote_host_buf =
                        self.config.sdr.remote_host.clone().unwrap_or_default();
                    self.app_state.sdr_remote_port_buf = self.config.sdr.effective_remote_port();
                    self.app_state.sdr_remote_extra_buf = self
                        .config
                        .sdr
                        .remote_extra_args
                        .clone()
                        .unwrap_or_default();
                    self.app_state.sdr_remote_buf_seeded = true;
                }
                self.app_state.show_sdr_settings = true;
            }
            UiCommand::HideSdrSettings => {
                self.app_state.show_sdr_settings = false;
            }
            UiCommand::ShowAbout => {
                self.app_state.show_about = true;
            }
            UiCommand::HideAbout => {
                self.app_state.show_about = false;
            }
            UiCommand::ClearGainCache => {
                // Phase 3 (v0.4.0): drop every persisted entry. Next
                // tune to any frequency runs a fresh coarse-then-fine
                // search and re-seeds the cache on SETTLED. No
                // confirmation dialog — the cost of an accidental
                // wipe is one slow AGC cycle, not data loss.
                if let Some(nrsc5) = self.nrsc5.as_ref() {
                    let dropped = nrsc5.clear_gain_cache();
                    eprintln!(
                        "[ui] cleared gain cache ({} entr{} dropped)",
                        dropped,
                        if dropped == 1 { "y" } else { "ies" },
                    );
                }
            }
            UiCommand::RefreshSdrDevices => {
                self.refresh_sdr_devices();
            }
            UiCommand::SelectSdrDevice {
                driver,
                device_args,
            } => {
                // Persist immediately. Takes effect on the next piped
                // Start (we don't restart the stream automatically —
                // the user might be mid-tune and that would be jarring).
                self.config.sdr.driver = driver;
                self.config.sdr.device_args = device_args;
                save_config(&self.config);
                // Refresh element list for the newly chosen device so
                // the gain sliders below it repopulate immediately.
                self.refresh_sdr_devices();
            }
            UiCommand::SetSdrGainElement { element, value_db } => {
                self.config.sdr.gains.insert(element.clone(), value_db);
                save_config(&self.config);
                // Push to the live SDR if a piped stream is running.
                // No-op if the element doesn't exist on this device
                // (apply_agc_action's same fall-through behavior).
                if let Some(nrsc5) = self.nrsc5.as_ref() {
                    let _ = nrsc5.set_sdr_gain_element(&element, value_db);
                }
            }
            UiCommand::SetSdrFreqCorrectionPpm(ppm) => {
                self.config.sdr.freq_correction_ppm = ppm;
                save_config(&self.config);
                if let Some(nrsc5) = self.nrsc5.as_ref() {
                    let _ = nrsc5.set_sdr_freq_correction_ppm(ppm);
                }
            }
            UiCommand::SetSdrAntenna(name) => {
                // Empty string from the dropdown means "use device
                // default" \u2014 normalize to `None` so the persisted
                // form matches the resolved-on-start logic.
                let new_antenna = if name.is_empty() { None } else { Some(name) };
                if self.config.sdr.antenna == new_antenna {
                    return;
                }
                self.config.sdr.antenna = new_antenna;
                save_config(&self.config);
                // Antenna selection isn't hot-swappable on every
                // driver (SDRplay reports a fresh gain range per
                // input; some Soapy modules refuse `setAntenna`
                // outside `configure`), and the user explicitly
                // accepted a brief restart in the antenna-dropdown
                // tooltip. Restart by dispatching Stop+Start so
                // the next `configure()` picks up the new value
                // via the same resolve-then-pass path used at
                // Start.
                if self.app_state.is_streaming {
                    self.handle_command(UiCommand::Stop);
                    self.handle_command(UiCommand::Start);
                }
            }
            UiCommand::ResetSdrConfig => {
                self.config.sdr = crate::config::SdrConfigSection::default();
                save_config(&self.config);
                self.refresh_sdr_devices();
            }
            UiCommand::SelectSdrTransport(transport) => {
                if self.config.sdr.transport == transport {
                    return;
                }
                self.config.sdr.transport = transport;
                save_config(&self.config);
                // Re-seed the port buffer so the default port flips to
                // match the new transport (1234 for rtl_tcp, 55132 for
                // SoapyRemote) when the user hasn't pinned an explicit
                // value.
                self.app_state.sdr_remote_port_buf = self.config.sdr.effective_remote_port();
            }
            UiCommand::SetSdrRemoteHost(host) => {
                let trimmed = host.trim();
                let new_host = if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed.to_string())
                };
                if self.config.sdr.remote_host == new_host {
                    return;
                }
                self.config.sdr.remote_host = new_host;
                save_config(&self.config);
            }
            UiCommand::SetSdrRemotePort(port) => {
                let new_port = if port == 0 { None } else { Some(port) };
                if self.config.sdr.remote_port == new_port {
                    return;
                }
                self.config.sdr.remote_port = new_port;
                save_config(&self.config);
            }
            UiCommand::SetSdrRemoteExtraArgs(extra) => {
                let trimmed = extra.trim();
                let new_extra = if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed.to_string())
                };
                if self.config.sdr.remote_extra_args == new_extra {
                    return;
                }
                self.config.sdr.remote_extra_args = new_extra;
                save_config(&self.config);
            }
        }
    }

    /// Re-enumerate SoapySDR devices and refresh the live gain-element
    /// list for the configured device. The work runs in a background
    /// thread so opening the Settings modal stays responsive even when a
    /// Soapy module is slow to enumerate (e.g. SDRplay service startup).
    fn refresh_sdr_devices(&mut self) {
        if !matches!(
            self.config.sdr.transport,
            crate::config::SdrTransport::LocalSoapy
        ) {
            return;
        }
        if self.sdr_refresh_in_flight {
            return;
        }

        self.sdr_refresh_in_flight = true;
        self.app_state.sdr_devices_refresh_in_flight = true;
        self.app_state.sdr_devices_last_refreshed = None;

        let tx = self.sdr_refresh_tx.clone();
        let args = self.config.sdr.to_args_string();
        let is_streaming = self.app_state.is_streaming;
        std::thread::spawn(move || {
            let (devices, diagnostics) = crate::sdr::SoapySdr::enumerate_devices_with_diagnostics();
            let gain_elements = if is_streaming {
                Vec::new()
            } else {
                match crate::sdr::SoapySdr::open(&args) {
                    Ok(sdr) => {
                        use crate::sdr::Sdr;
                        sdr.gain_elements()
                    }
                    Err(_) => Vec::new(),
                }
            };
            let _ = tx.send(SdrDeviceRefreshResult {
                devices,
                gain_elements,
                diagnostics,
            });
        });
    }

    fn drain_sdr_refresh_results(&mut self) {
        let mut latest = None;
        while let Ok(result) = self.sdr_refresh_rx.try_recv() {
            latest = Some(result);
        }

        if let Some(result) = latest {
            let matching_device = pick_best_sdr_device(
                &self.config.sdr.driver,
                &self.config.sdr.device_args,
                &result.devices,
            );
            if let Some(dev) = matching_device {
                let same_selection = dev.driver == self.config.sdr.driver
                    && dev.args_after_driver() == self.config.sdr.device_args;
                if !same_selection {
                    self.config.sdr.driver = dev.driver.clone();
                    self.config.sdr.device_args = dev.args_after_driver();
                    save_config(&self.config);
                }
            }

            self.app_state.sdr_devices = result.devices;
            self.app_state.sdr_devices_last_refreshed = Some(Instant::now());
            self.app_state.sdr_gain_elements = result.gain_elements;
            self.app_state.sdr_devices_refresh_in_flight = false;
            self.sdr_refresh_in_flight = false;

            if let Some(path) = crate::paths::sdr_diagnostics_file() {
                if let Some(parent) = path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let _ = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)
                    .and_then(|mut f| {
                        std::io::Write::write_all(&mut f, result.diagnostics.as_bytes())
                    });
            }
        }
    }

    /// Render the unified Settings modal. Five tabs in a left rail —
    /// Connection (transport + remote host/port), Device (local SoapySDR
    /// picker + profile notes), Gain (per-element sliders + PPM), Display
    /// (HD5\u{2013}HD8 row, auto-decode toggle), Recording (mode, folder,
    /// rotation, subfolders).
    ///
    /// Header strip above the tabs always shows the active device + live
    /// stream status + effective transport string so the user has
    /// consistent feedback regardless of which tab they're on.
    ///
    /// Closing dispatches `HideSdrSettings` rather than mutating state
    /// directly so the next-tick is consistent with other state changes.
    fn render_sdr_settings_modal(&mut self, ctx: &egui::Context, commands: &mut Vec<UiCommand>) {
        use crate::gui::state::SettingsTab;
        let mut open = true;

        // ---- Snapshot config / state ------------------------------------
        // Pulled to locals so the closure body doesn't fight the borrow
        // checker when it also needs `&mut self.app_state` for the live
        // edit buffers and `commands: &mut Vec<UiCommand>`.
        let active_args = self.config.sdr.display_connection_string();
        let active_driver = self.config.sdr.driver.clone();
        let current_ppm = self.config.sdr.freq_correction_ppm;
        let show_hd5_hd8 = self.app_state.show_hd5_hd8;
        let collage_secondary_click_fallback = self.app_state.collage_secondary_click_fallback;
        let transport = self.config.sdr.transport;
        let is_streaming = self.app_state.is_streaming;
        let config_remote_host = self.config.sdr.remote_host.clone().unwrap_or_default();
        let config_remote_port = self.config.sdr.effective_remote_port();
        let config_remote_extra = self
            .config
            .sdr
            .remote_extra_args
            .clone()
            .unwrap_or_default();
        let last_refreshed_label = if self.app_state.sdr_devices_refresh_in_flight {
            "refreshing…".to_string()
        } else {
            self.app_state
                .sdr_devices_last_refreshed
                .map(|t| format!("refreshed {}s ago", t.elapsed().as_secs()))
                .unwrap_or_else(|| "not yet refreshed".to_string())
        };

        // Short, plain-English summary of the current transport for the
        // header strip. Mirrors what `to_args_string()` produces but in
        // a form that's friendly to glance at instead of parse.
        let transport_summary = match transport {
            crate::config::SdrTransport::LocalSoapy => {
                format!("Local \u{2022} {}", active_driver)
            }
            crate::config::SdrTransport::SoapyRemote => format!(
                "SoapyRemote \u{2022} {}:{}",
                self.config
                    .sdr
                    .remote_host
                    .as_deref()
                    .unwrap_or("127.0.0.1"),
                config_remote_port,
            ),
            crate::config::SdrTransport::RtlTcpRemote => format!(
                "rtl_tcp \u{2022} {}:{}",
                self.config
                    .sdr
                    .remote_host
                    .as_deref()
                    .unwrap_or("127.0.0.1"),
                config_remote_port,
            ),
        };

        // Bound the window: a max of 80% of the screen prevents it from
        // running off the bottom on small displays (the prior version
        // hit a feedback loop where the right-pane ScrollArea kept
        // claiming more height each frame). The user can still resize
        // smaller down to (560 \u{00D7} 380).
        let screen = ctx.screen_rect();
        let max_w = (screen.width() * 0.95).max(560.0);
        let max_h = (screen.height() * 0.85).max(380.0);

        egui::Window::new(egui::RichText::new("\u{2699}  Settings").size(18.0))
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_width(720.0)
            .default_height(540.0)
            .min_width(560.0)
            .min_height(380.0)
            .max_width(max_w)
            .max_height(max_h)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                // Use proper egui panel hierarchy so the window stays
                // resizable. Mixing horizontal layouts with min_height
                // constraints (the previous approach) caused vertical
                // runaway because the ScrollArea, the inner vertical,
                // and the Window all fed each other's available-height
                // calculations.
                //
                //   top    header strip
                //   bottom footer buttons
                //   left   tab nav
                //   center per-tab content (scrollable)

                // ---- Footer (declared first so top/left know how much
                //      vertical space remains for them) --------------
                egui::TopBottomPanel::bottom("settings_footer")
                    .resizable(false)
                    .show_separator_line(true)
                    .show_inside(ui, |ui| {
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            if ui
                                .button("Reset SDR config")
                                .on_hover_text(
                                    "Restore driver=rtlsdr, empty device args, \
                                     0 PPM, and clear all per-element gain \
                                     overrides. Does not touch Display or \
                                     Recording settings.",
                                )
                                .clicked()
                            {
                                commands.push(UiCommand::ResetSdrConfig);
                            }
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.button("Close").clicked() {
                                        commands.push(UiCommand::HideSdrSettings);
                                    }
                                },
                            );
                        });
                        ui.add_space(2.0);
                    });

                // ---- Header strip ----------------------------------
                egui::TopBottomPanel::top("settings_header")
                    .resizable(false)
                    .show_separator_line(true)
                    .show_inside(ui, |ui| {
                        ui.add_space(2.0);
                        ui.horizontal(|ui| {
                            let (status_color, status_label) = if is_streaming {
                                (egui::Color32::from_rgb(80, 220, 120), "streaming")
                            } else {
                                (egui::Color32::from_gray(140), "idle")
                            };
                            ui.colored_label(status_color, "\u{25CF}");
                            ui.label(
                                egui::RichText::new(status_label)
                                    .small()
                                    .color(status_color),
                            );
                            ui.separator();
                            ui.label(egui::RichText::new(&transport_summary).monospace());
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.code(&active_args)
                                        .on_hover_text("Effective Soapy args / connection string");
                                },
                            );
                        });
                        ui.add_space(2.0);
                    });

                // ---- Left rail (tab nav) ---------------------------
                egui::SidePanel::left("settings_nav")
                    .resizable(false)
                    .exact_width(150.0)
                    .show_inside(ui, |ui| {
                        ui.add_space(4.0);
                        // `top_down_justified(LEFT)` makes each row
                        // stretch the full panel width AND aligns the
                        // label text to the left edge, so the emoji
                        // glyphs (which have varying advance widths)
                        // all start at the same x.
                        ui.with_layout(egui::Layout::top_down_justified(egui::Align::LEFT), |ui| {
                            for tab in [
                                SettingsTab::Connection,
                                SettingsTab::Gain,
                                SettingsTab::Display,
                                SettingsTab::Recording,
                            ] {
                                let selected = self.app_state.settings_tab == tab;
                                let resp = ui.selectable_label(selected, tab.label());
                                if resp.clicked() && !selected {
                                    self.app_state.settings_tab = tab;
                                }
                            }
                        });
                    });

                // ---- Center pane (active tab body) -----------------
                egui::CentralPanel::default().show_inside(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt("settings_tab_body")
                        .auto_shrink([false, false])
                        .show(ui, |ui| match self.app_state.settings_tab {
                            SettingsTab::Connection => {
                                self.render_settings_connection_tab(
                                    ui,
                                    commands,
                                    transport,
                                    &active_driver,
                                    &config_remote_host,
                                    config_remote_port,
                                    &config_remote_extra,
                                    &last_refreshed_label,
                                );
                            }
                            SettingsTab::Gain => {
                                self.render_settings_gain_tab(
                                    ui,
                                    commands,
                                    &active_driver,
                                    current_ppm,
                                );
                            }
                            SettingsTab::Display => {
                                self.render_settings_display_tab(
                                    ui,
                                    commands,
                                    show_hd5_hd8,
                                    collage_secondary_click_fallback,
                                );
                            }
                            SettingsTab::Recording => {
                                self.render_settings_recording_tab(ui, commands);
                            }
                        });
                });
            });

        if !open {
            // User dismissed via the "X" on the window title bar.
            commands.push(UiCommand::HideSdrSettings);
        }
    }

    /// Connection tab: transport picker + (for Local) device list +
    /// (for Remote) host/port form, plus the active-device profile
    /// notes. The device section was previously its own tab; folding
    /// it back into Connection keeps "where IQ comes from" in one
    /// place.
    #[allow(clippy::too_many_arguments)] // Settings tab wiring: each arg is a
                                         // distinct piece of connection form state; grouping into a struct would
                                         // just move the fan-out to the call site.
    fn render_settings_connection_tab(
        &mut self,
        ui: &mut egui::Ui,
        commands: &mut Vec<UiCommand>,
        transport: crate::config::SdrTransport,
        active_driver: &str,
        config_remote_host: &str,
        config_remote_port: u16,
        config_remote_extra: &str,
        last_refreshed_label: &str,
    ) {
        // ---- Transport selector ------------------------------------
        ui.heading("Transport");
        ui.label(
            egui::RichText::new("Transport changes take effect on the next Stop/Start cycle.")
                .small()
                .color(egui::Color32::from_gray(140)),
        );
        ui.add_space(2.0);

        ui.horizontal(|ui| {
            for (variant, label, hover) in [
                (
                    crate::config::SdrTransport::LocalSoapy,
                    "Local SDR",
                    "Enumerate and open a SoapySDR device attached to this \
                     machine. Pick the specific dongle from the list below.",
                ),
                (
                    crate::config::SdrTransport::SoapyRemote,
                    "SoapyRemote",
                    "Connect to a SoapySDRServer instance on the remote \
                     host. The remote machine must have SoapyRemote \
                     installed alongside the device's Soapy module (e.g. \
                     SoapyRTLSDR, SoapySDRPlay3). Default port 55132.",
                ),
                (
                    crate::config::SdrTransport::RtlTcpRemote,
                    "rtl_tcp",
                    "Connect to a native rtl_tcp server on the remote host. \
                     Only a single tuner-gain control is exposed (no \
                     per-element sliders, no antenna picker). Default port \
                     1234.",
                ),
            ] {
                let resp = ui
                    .selectable_label(transport == variant, label)
                    .on_hover_text(hover);
                if resp.clicked() && transport != variant {
                    commands.push(UiCommand::SelectSdrTransport(variant));
                }
            }
        });

        ui.add_space(12.0);
        ui.separator();
        ui.add_space(8.0);

        // ---- Per-transport device section --------------------------
        match transport {
            crate::config::SdrTransport::LocalSoapy => {
                self.render_local_device_section(ui, commands, active_driver, last_refreshed_label);
            }
            crate::config::SdrTransport::SoapyRemote
            | crate::config::SdrTransport::RtlTcpRemote => {
                ui.heading("Remote host");
                ui.add_space(4.0);
                self.render_remote_form(
                    ui,
                    commands,
                    transport,
                    config_remote_host,
                    config_remote_port,
                    config_remote_extra,
                );
            }
        }
    }

    /// Local-device picker + profile notes. Extracted so the Connection
    /// tab's match arm doesn't balloon.
    fn render_local_device_section(
        &mut self,
        ui: &mut egui::Ui,
        commands: &mut Vec<UiCommand>,
        active_driver: &str,
        last_refreshed_label: &str,
    ) {
        // Toolbar: heading on the left, Refresh + timestamp +
        // diagnostics on the right.
        ui.horizontal(|ui| {
            ui.heading("Local devices");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .small_button("Open diagnostics\u{2026}")
                    .on_hover_text(
                        "Open the SDR-diagnostics text file (PATH, \
                             SOAPY_SDR_PLUGIN_PATH, per-driver enumerate \
                             results) so you can paste it into a bug report.",
                    )
                    .clicked()
                {
                    if let Some(p) = crate::paths::sdr_diagnostics_file() {
                        let _ = std::process::Command::new("cmd")
                            .args(["/C", "start", "", p.to_string_lossy().as_ref()])
                            .spawn();
                    }
                }
                ui.label(
                    egui::RichText::new(last_refreshed_label)
                        .small()
                        .color(egui::Color32::from_gray(140)),
                );
                if ui.button("\u{21BB}  Refresh").clicked() {
                    commands.push(UiCommand::RefreshSdrDevices);
                }
            });
        });
        ui.add_space(4.0);

        if self.app_state.sdr_devices.is_empty() {
            ui.colored_label(
                egui::Color32::from_rgb(220, 160, 80),
                "No SoapySDR devices detected. Check the dongle is plugged \
                 in (and Zadig-bound for RTL-SDR on Windows), then click \
                 Refresh.",
            );
        } else {
            // Radio buttons so users immediately recognize this is a
            // single-select list.
            egui::ScrollArea::vertical()
                .id_salt("sdr_devices_list")
                .max_height(180.0)
                .show(ui, |ui| {
                    for dev in &self.app_state.sdr_devices {
                        let is_active = dev.driver == active_driver
                            && self.config.sdr.device_args == dev.args_after_driver();
                        let label = if dev.label.is_empty() {
                            format!("[{}]  {}", dev.driver, dev.args_after_driver())
                        } else {
                            format!("[{}]  {}", dev.driver, dev.label)
                        };
                        let resp = ui.radio(is_active, label);
                        if resp.clicked() && !is_active {
                            commands.push(UiCommand::SelectSdrDevice {
                                driver: dev.driver.clone(),
                                device_args: dev.args_after_driver(),
                            });
                        }
                    }
                });
        }

        // ---- Profile notes -----------------------------------------
        ui.add_space(10.0);
        ui.separator();
        ui.add_space(6.0);

        ui.label(
            egui::RichText::new("Currently selected device")
                .small()
                .color(egui::Color32::from_gray(150)),
        );

        if let Some(profile) = crate::sdr::profile::lookup(active_driver) {
            ui.horizontal(|ui| {
                ui.heading(format!("{} ({})", profile.display_name, profile.driver));
                if !profile.bench_validated {
                    ui.label(
                        egui::RichText::new("\u{26A0} not bench-validated")
                            .small()
                            .color(egui::Color32::from_rgb(220, 160, 80)),
                    )
                    .on_hover_text(
                        "AGC behavior on this device is heuristic. Please \
                         file a GitHub issue if reception is poor \u{2014} \
                         bench logs welcome.",
                    );
                }
            });
            ui.collapsing("HD Radio notes", |ui| {
                ui.label(profile.hd_radio_notes);
            });
        } else {
            ui.colored_label(
                egui::Color32::from_rgb(220, 160, 80),
                format!(
                    "No device profile is configured for driver \"{}\". AGC \
                     will fall back to the rtlsdr profile; results may vary.",
                    active_driver
                ),
            );
        }
    }

    /// Remote host/port form for SoapyRemote / rtl_tcp transports.
    fn render_remote_form(
        &mut self,
        ui: &mut egui::Ui,
        commands: &mut Vec<UiCommand>,
        transport: crate::config::SdrTransport,
        config_remote_host: &str,
        config_remote_port: u16,
        config_remote_extra: &str,
    ) {
        egui::Grid::new("remote_form_grid")
            .num_columns(2)
            .spacing([12.0, 6.0])
            .show(ui, |ui| {
                ui.label("Host:");
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut self.app_state.sdr_remote_host_buf)
                        .desired_width(220.0)
                        .hint_text("192.168.0.10"),
                );
                if resp.lost_focus() && self.app_state.sdr_remote_host_buf != config_remote_host {
                    commands.push(UiCommand::SetSdrRemoteHost(
                        self.app_state.sdr_remote_host_buf.clone(),
                    ));
                }
                ui.end_row();

                ui.label("Port:");
                let resp = ui.add(
                    egui::DragValue::new(&mut self.app_state.sdr_remote_port_buf).range(1..=65535),
                );
                if (resp.drag_stopped() || resp.lost_focus())
                    && self.app_state.sdr_remote_port_buf != config_remote_port
                {
                    commands.push(UiCommand::SetSdrRemotePort(
                        self.app_state.sdr_remote_port_buf,
                    ));
                }
                ui.end_row();

                if transport == crate::config::SdrTransport::SoapyRemote {
                    ui.label("Extra args:");
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut self.app_state.sdr_remote_extra_buf)
                            .desired_width(320.0)
                            .hint_text("(optional, e.g. remote:driver=rtlsdr)"),
                    );
                    if resp.lost_focus()
                        && self.app_state.sdr_remote_extra_buf != config_remote_extra
                    {
                        commands.push(UiCommand::SetSdrRemoteExtraArgs(
                            self.app_state.sdr_remote_extra_buf.clone(),
                        ));
                    }
                    ui.end_row();
                }
            });

        if transport == crate::config::SdrTransport::RtlTcpRemote {
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(
                    "Note: rtl_tcp exposes only a single tuner gain. The \
                     Gain tab's per-element sliders won't appear, and \
                     antenna selection isn't available.",
                )
                .small()
                .color(egui::Color32::from_gray(140)),
            );
        }
    }

    /// Gain tab: per-element sliders + PPM correction. Sliders are hidden
    /// when no elements are reported (e.g. rtl_tcp transport).
    fn render_settings_gain_tab(
        &mut self,
        ui: &mut egui::Ui,
        commands: &mut Vec<UiCommand>,
        active_driver: &str,
        current_ppm: f64,
    ) {
        ui.heading("Manual gain");
        // AGC drives whichever element is canonical for this device.
        // If AGC mode is active, surface that so the user understands
        // their manual drag may be overridden on next tune.
        if self.app_state.gain_mode == crate::config::GainMode::Auto {
            ui.label(
                egui::RichText::new(
                    "AGC is active \u{2014} sliders may be overridden on the \
                     next tune or AGC settle cycle. Switch to Manual gain \
                     mode in the Tuner panel to pin a value.",
                )
                .small()
                .color(egui::Color32::from_rgb(220, 160, 80)),
            );
            ui.add_space(2.0);
        }

        if self.app_state.sdr_gain_elements.is_empty() {
            ui.colored_label(
                egui::Color32::from_gray(140),
                "No gain elements reported. Either the device isn't \
                 currently attached, or the active transport (e.g. rtl_tcp) \
                 doesn't expose them.",
            );
        } else {
            ui.label(
                egui::RichText::new(
                    "Sliders are live: changes apply immediately to a \
                     running stream and are persisted to config.",
                )
                .small()
                .color(egui::Color32::from_gray(140)),
            );
            ui.add_space(4.0);

            let elements = self.app_state.sdr_gain_elements.clone();
            egui::Grid::new("gain_sliders_grid")
                .num_columns(2)
                .spacing([12.0, 4.0])
                .show(ui, |ui| {
                    for elem in &elements {
                        let mut value = self
                            .config
                            .sdr
                            .gains
                            .get(&elem.name)
                            .copied()
                            .unwrap_or(elem.current_db);
                        let step = if elem.step_db > 0.0 {
                            elem.step_db
                        } else {
                            0.1
                        };
                        ui.label(egui::RichText::new(format!("{:>6}", elem.name)).monospace());
                        let resp = ui.add(
                            egui::Slider::new(&mut value, elem.min_db..=elem.max_db)
                                .step_by(step)
                                .suffix(" dB")
                                .clamp_to_range(true),
                        );
                        if resp.drag_stopped() || resp.lost_focus() || resp.changed() {
                            let prev = self.config.sdr.gains.get(&elem.name).copied();
                            if prev.is_none_or(|p| (p - value).abs() > 1e-6) {
                                commands.push(UiCommand::SetSdrGainElement {
                                    element: elem.name.clone(),
                                    value_db: value,
                                });
                            }
                        }
                        ui.end_row();
                    }
                });
        }

        // ---- Frequency correction (PPM) ------------------------------
        // Only RTL-SDR honors a user-supplied PPM; SDRplay and HackRF
        // use internal calibration. Show the control with a clear note
        // when it's a no-op so users aren't surprised by the field.
        ui.add_space(12.0);
        ui.separator();
        ui.add_space(6.0);
        ui.heading("Frequency correction");

        let honors_ppm = active_driver == "rtlsdr";
        let mut ppm = current_ppm;
        ui.horizontal(|ui| {
            ui.label("PPM:");
            ui.add_enabled_ui(honors_ppm, |ui| {
                let resp = ui.add(
                    egui::DragValue::new(&mut ppm)
                        .speed(0.1)
                        .range(-100.0..=100.0)
                        .suffix(" ppm"),
                );
                if honors_ppm
                    && (resp.drag_stopped() || resp.lost_focus())
                    && (ppm - current_ppm).abs() > 1e-6
                {
                    commands.push(UiCommand::SetSdrFreqCorrectionPpm(ppm));
                }
            });
        });
        ui.label(
            egui::RichText::new(if honors_ppm {
                "RTL-SDR honors this immediately."
            } else {
                "Disabled \u{2014} this driver uses its internal calibration \
                 and ignores user-supplied PPM."
            })
            .small()
            .color(egui::Color32::from_gray(140)),
        );

        // ---- v0.6.0 Advanced AGC tuning ------------------------------
        // The amplitude pre-stage's RMS target is normally set by the
        // per-device profile (e.g. -20 dBFS for RTL-SDR, -22 dBFS for
        // SDRplay), but power users with unusual antennas / noise
        // floors / preamps benefit from being able to nudge it. The
        // override only takes effect on the next cold-start tune
        // (cache hits skip AmpProbe entirely), so a Re-tune is needed
        // for changes to be visible in the agc-trace log.
        ui.add_space(12.0);
        ui.separator();
        ui.add_space(6.0);
        egui::CollapsingHeader::new("Advanced AGC tuning")
            .default_open(self.app_state.agc_amp_target_dbfs_override.is_some())
            .show(ui, |ui| {
                let profile_default = crate::sdr::profile::lookup(active_driver)
                    .map(|p| p.amp_target_dbfs)
                    .unwrap_or(-20.0);

                ui.label("Initial signal strength target");
                ui.label(
                    egui::RichText::new(
                        "When you tune to a new station, the AGC quickly picks \
                         a starting gain that aims for this signal level, then \
                         fine-tunes from there. The default works well for most \
                         antennas \u{2014} you only need this if HD lock is \
                         unreliable in your specific RF environment.",
                    )
                    .small()
                    .color(egui::Color32::from_gray(150)),
                );
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new(format!(
                        "Default for {}: {:.1} dBFS",
                        active_driver, profile_default,
                    ))
                    .small()
                    .color(egui::Color32::from_gray(140)),
                );
                ui.add_space(6.0);

                let mut override_enabled = self.app_state.agc_amp_target_dbfs_override.is_some();
                let mut override_value = self
                    .app_state
                    .agc_amp_target_dbfs_override
                    .unwrap_or(profile_default);

                let checkbox_resp = ui.checkbox(&mut override_enabled, "Use a custom target");
                if checkbox_resp.changed() {
                    if override_enabled {
                        commands.push(UiCommand::SetAgcAmpTargetDbfs(Some(override_value)));
                    } else {
                        commands.push(UiCommand::SetAgcAmpTargetDbfs(None));
                    }
                }

                ui.add_enabled_ui(override_enabled, |ui| {
                    let slider_resp = ui.add(
                        egui::Slider::new(&mut override_value, -30.0..=-10.0)
                            .step_by(0.5)
                            .suffix(" dBFS")
                            .text("Target"),
                    );
                    if override_enabled
                        && (slider_resp.drag_stopped()
                            || slider_resp.lost_focus()
                            || slider_resp.changed())
                    {
                        let prev = self.app_state.agc_amp_target_dbfs_override;
                        if prev.is_none_or(|p| (p - override_value).abs() > 1e-3) {
                            commands.push(UiCommand::SetAgcAmpTargetDbfs(Some(override_value)));
                        }
                    }
                });

                ui.add_space(8.0);
                ui.label(egui::RichText::new("When to adjust this:").strong().small());
                ui.label(
                    egui::RichText::new(
                        "\u{2022} Stations near you bail out or take a long time \
                         to lock (MER stuck low) \u{2014} try a lower (more \
                         negative) value like \u{2212}2 from the default. The AGC \
                         will pick a gentler gain that's less likely to overload \
                         the receiver on strong signals.\n\
                         \u{2022} Distant or fringe stations never lock \u{2014} \
                         try a higher (less negative) value. The AGC will start \
                         at a hotter gain to pull weak signals up. But don't \
                         push too far or strong local stations may distort.\n\
                         \u{2022} Most users should leave this alone. The \
                         per-device default is conservative and works for \
                         typical indoor / outdoor antennas.",
                    )
                    .small()
                    .color(egui::Color32::from_gray(150)),
                );
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(
                        "Changes apply on the next Re-tune. Stations already in \
                         the gain cache use their stored gain and ignore this \
                         setting until you clear the cache.",
                    )
                    .small()
                    .italics()
                    .color(egui::Color32::from_gray(130)),
                );
            });
    }

    /// Display tab: UI preferences that aren't tied to the SDR — HD5\u{2013}HD8
    /// row visibility and the auto-decode-every-subchannel toggle.
    fn render_settings_display_tab(
        &mut self,
        ui: &mut egui::Ui,
        commands: &mut Vec<UiCommand>,
        show_hd5_hd8: bool,
        collage_secondary_click_fallback: bool,
    ) {
        ui.heading("Program selector");
        ui.add_space(4.0);

        let mut show_hd5_hd8 = show_hd5_hd8;
        let resp = ui.checkbox(
            &mut show_hd5_hd8,
            "Show HD5\u{2013}HD8 row in program selector",
        );
        ui.label(
            egui::RichText::new(
                "Most stations only advertise HD1\u{2013}HD4. Enable this \
                 when tuning to an MP11-partition broadcaster with up to 8 \
                 audio subchannels.",
            )
            .small()
            .color(egui::Color32::from_gray(140)),
        );
        if resp.changed() {
            commands.push(UiCommand::SetShowHd5Hd8(show_hd5_hd8));
        }

        ui.add_space(14.0);
        ui.separator();
        ui.add_space(6.0);
        ui.heading("Presets");
        ui.add_space(4.0);

        // Plain numeric field. DragValue accepts typed input (click to
        // type, scroll/drag to nudge), and the `range` plus a final
        // clamp on the commit path keep the value sane regardless of
        // how the user gets it there.
        let mut slot_count = self.app_state.preset_slot_count.clamp(1, 48) as i32;
        ui.horizontal(|ui| {
            ui.label("Preset slots on Tuner panel:");
            let resp = ui.add(
                egui::DragValue::new(&mut slot_count)
                    .range(1..=48)
                    .speed(0.1),
            );
            if (resp.drag_stopped() || resp.lost_focus() || resp.changed())
                && (slot_count as u32) != self.app_state.preset_slot_count
            {
                commands.push(UiCommand::SetPresetSlotCount(slot_count.clamp(1, 48) as u32));
            }
        });
        ui.label(
            egui::RichText::new(
                "Default 6, range 1\u{2013}48. The Tuner panel wraps preset \
                 buttons across multiple rows as needed \u{2014} cranking \
                 this past ~12 will noticeably grow the dock.",
            )
            .small()
            .color(egui::Color32::from_gray(140)),
        );

        ui.add_space(14.0);
        ui.separator();
        ui.add_space(6.0);
        ui.heading("Collage");
        ui.add_space(4.0);

        let blocked_count = self.app_state.art_blocklist_count;
        let status_label = if blocked_count == 0 {
            "No images are currently blocked.".to_string()
        } else {
            format!(
                "{blocked_count} image{} permanently blocked from the collage.",
                if blocked_count == 1 { "" } else { "s" }
            )
        };
        ui.label(
            egui::RichText::new(status_label)
                .small()
                .color(egui::Color32::from_gray(140)),
        );
        ui.add_space(4.0);
        let clear_resp = ui
            .add_enabled(
                blocked_count > 0,
                egui::Button::new("\u{1F5D1} Clear Block List"),
            )
            .on_hover_text(
                "Allow all previously-blocked images to appear in the \
                 collage again on next arrival.",
            );
        if clear_resp.clicked() {
            commands.push(UiCommand::ClearArtBlocklist);
        }

        ui.add_space(8.0);
        let mut fallback = collage_secondary_click_fallback;
        let fallback_resp = ui.checkbox(
            &mut fallback,
            "Linux: block immediately on right-click if menu fails",
        );
        ui.label(
            egui::RichText::new(
                "Keeps right-click blocking usable on some Ubuntu/Linux \
                 desktop setups where context-menu popups on image tiles \
                 can be suppressed. No effect on non-Linux builds.",
            )
            .small()
            .color(egui::Color32::from_gray(140)),
        );
        if fallback_resp.changed() {
            commands.push(UiCommand::SetCollageSecondaryClickFallback(fallback));
        }
    }

    /// Recording tab: mode, output folder, max minutes per file, per-
    /// station subfolders.
    fn render_settings_recording_tab(&mut self, ui: &mut egui::Ui, commands: &mut Vec<UiCommand>) {
        ui.heading("Recording");
        ui.label(
            egui::RichText::new(
                "Record any one HD subchannel as a 96 kbps Opus file. The \
                 recording locks to whichever HD button is selected at the \
                 moment you press Record \u{2014} you can then listen to a \
                 different subchannel without disturbing it.",
            )
            .small()
            .color(egui::Color32::from_gray(140)),
        );
        ui.add_space(8.0);

        // ---- Folder + max-minutes -----------------------------------
        // Recording is always available when a stream is up; there's
        // no separate "mode" toggle. The Rec button on the Tuner
        // panel is the single entry point.
        egui::Grid::new("recording_form_grid")
            .num_columns(2)
            .spacing([12.0, 6.0])
            .show(ui, |ui| {
                ui.label("Folder:");
                let current_dir = self.config.recording_dir.clone().unwrap_or_else(|| {
                    crate::paths::default_recording_dir()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| "<unresolved>".to_string())
                });
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut current_dir.clone())
                            .desired_width(260.0)
                            .interactive(false),
                    );
                    if ui.button("Browse\u{2026}").clicked() {
                        let start_dir = self
                            .config
                            .recording_dir
                            .as_ref()
                            .map(std::path::PathBuf::from)
                            .or_else(crate::paths::default_recording_dir);
                        let mut dialog =
                            rfd::FileDialog::new().set_title("Choose recordings folder");
                        if let Some(d) = start_dir.as_ref() {
                            dialog = dialog.set_directory(d);
                        }
                        if let Some(chosen) = dialog.pick_folder() {
                            commands.push(UiCommand::SetRecordingDir(Some(
                                chosen.display().to_string(),
                            )));
                        }
                    }
                    if self.config.recording_dir.is_some()
                        && ui
                            .button("Reset")
                            .on_hover_text("Revert to the default recordings folder")
                            .clicked()
                    {
                        commands.push(UiCommand::SetRecordingDir(None));
                    }
                });
                ui.end_row();

                ui.label("Max minutes per file:");
                let mut max_minutes = self.config.recording_max_minutes as i32;
                let resp = ui.add(
                    egui::DragValue::new(&mut max_minutes)
                        .range(1..=240)
                        .speed(1.0)
                        .suffix(" min"),
                );
                if resp.changed() {
                    commands.push(UiCommand::SetRecordingMaxMinutes(
                        max_minutes.clamp(1, 240) as u32
                    ));
                }
                ui.end_row();
            });

        ui.label(
            egui::RichText::new(
                "Hard cap on a single .opus file. Continuous mode rotates \
                 to a new file when reached; per-song mode splits sooner \
                 whenever the song changes.",
            )
            .small()
            .color(egui::Color32::from_gray(140)),
        );

        ui.add_space(10.0);
        let mut subfolder = self.config.recording_subfolder_per_station;
        let resp = ui.checkbox(&mut subfolder, "Group files into per-station subfolders");
        if resp.changed() {
            commands.push(UiCommand::SetRecordingSubfolderPerStation(subfolder));
        }
    }

    /// Render the About dialog. Shows version (from `CARGO_PKG_VERSION`
    /// — set by Cargo at build time), license, attribution, and a few
    /// quick-reference links to project URLs.
    fn render_about_dialog(&mut self, ctx: &egui::Context, commands: &mut Vec<UiCommand>) {
        let mut open = true;
        egui::Window::new(egui::RichText::new("\u{2139}  About").size(18.0))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(440.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    ui.heading(
                        egui::RichText::new("NRSC5 Studio")
                            .color(crate::gui::accent_color(self.app_state.dark_mode)),
                    );
                    ui.label(
                        egui::RichText::new(format!("Version {}", env!("CARGO_PKG_VERSION")))
                            .monospace(),
                    );
                });
                ui.add_space(8.0);
                ui.separator();
                ui.add_space(8.0);

                ui.label(
                    "A native Windows HD Radio receiver and station explorer, \
                     built on the open-source nrsc5 demodulator and the \
                     SoapySDR backend.",
                );
                ui.add_space(8.0);

                egui::Grid::new("about_grid")
                    .num_columns(2)
                    .spacing([12.0, 6.0])
                    .show(ui, |ui| {
                        ui.label(egui::RichText::new("License").strong());
                        ui.label("GPL-3.0-or-later (matches nrsc5)");
                        ui.end_row();

                        ui.label(egui::RichText::new("Project").strong());
                        ui.hyperlink_to(
                            "github.com/LTCAshraven/nrsc5-studio",
                            "https://github.com/LTCAshraven/nrsc5-studio",
                        );
                        ui.end_row();

                        ui.label(egui::RichText::new("nrsc5").strong());
                        ui.hyperlink_to(
                            "github.com/theori-io/nrsc5",
                            "https://github.com/theori-io/nrsc5",
                        );
                        ui.end_row();

                        ui.label(egui::RichText::new("SoapySDR").strong());
                        ui.hyperlink_to(
                            "github.com/pothosware/SoapySDR",
                            "https://github.com/pothosware/SoapySDR",
                        );
                        ui.end_row();

                        ui.label(egui::RichText::new("egui / eframe").strong());
                        ui.hyperlink_to("github.com/emilk/egui", "https://github.com/emilk/egui");
                        ui.end_row();
                    });

                ui.add_space(12.0);
                ui.label(
                    egui::RichText::new(
                        "HD Radio is a registered trademark of \
                         Xperi Corporation. This app is an independent \
                         project and is not affiliated with Xperi.",
                    )
                    .small()
                    .italics()
                    .color(egui::Color32::from_gray(140)),
                );

                ui.add_space(12.0);
                ui.separator();
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Close").clicked() {
                        commands.push(UiCommand::HideAbout);
                    }
                });
            });

        if !open {
            commands.push(UiCommand::HideAbout);
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum LotImageClass {
    StationLogo {
        definitive: bool,
        source: &'static str,
    },
    CoverArt {
        source: &'static str,
    },
    GenericImage {
        source: &'static str,
    },
    Other,
}

fn mime_hex(mime: u32) -> String {
    format!("0x{mime:08X}")
}

fn station_logo_source_sidecar_path(path: &std::path::Path) -> std::path::PathBuf {
    let mut with_suffix = path.as_os_str().to_os_string();
    with_suffix.push(".src");
    std::path::PathBuf::from(with_suffix)
}

fn lot_name_without_prefix(filename: &str) -> &str {
    filename
        .split_once('_')
        .map(|(_, rest)| rest)
        .unwrap_or(filename)
}

/// Parse any `HD<n>` / `HD0<n>` token from a broadcaster-provided LOT name.
/// Returns the zero-based subchannel index (HD1 => 0 ... HD8 => 7).
fn parse_hd_program_token(text_upper: &str) -> Option<u32> {
    let bytes = text_upper.as_bytes();
    let len = bytes.len();
    if len < 3 {
        return None;
    }
    for i in 0..(len - 2) {
        if bytes[i] != b'H' || bytes[i + 1] != b'D' {
            continue;
        }
        let mut j = i + 2;
        if j < len && bytes[j] == b'0' {
            j += 1;
        }
        if j < len {
            let d = bytes[j];
            if (b'1'..=b'8').contains(&d) {
                return Some((d - b'1') as u32);
            }
        }
    }
    None
}

/// Filename fallback heuristic for station logos. This is intentionally
/// best-effort and only used when LOT metadata is generic/missing.
///
/// Recognized families include:
/// - `SL<CALLSIGN>$$<NN>...` (legacy/known-good)
/// - `SLHD<n>...` (seen in external markets)
/// - `<CALLSIGN>HD0<n>...` / `<CALLSIGN>HD<n>...`
fn parse_station_logo_filename(filename: &str) -> Option<(u32, String)> {
    let raw = lot_name_without_prefix(filename);
    let upper = raw.to_ascii_uppercase();

    // Strongest signal: SL<CALLSIGN>$$<NN>...
    if let Some(rest) = upper.strip_prefix("SL") {
        if let Some((callsign, after)) = rest.split_once("$$") {
            let digits: String = after.chars().take(2).collect();
            if let Ok(sub) = digits.parse::<u32>() {
                if (1..=8).contains(&sub) {
                    return Some((sub - 1, callsign.to_string()));
                }
            }
        }
    }

    // SLHD<n> / SL...HD<n>
    if upper.starts_with("SL") {
        if let Some(program) = parse_hd_program_token(&upper) {
            let callsign = if let Some(hd_idx) = upper.find("HD") {
                let maybe = upper[2..hd_idx].trim();
                if (3..=5).contains(&maybe.len()) && maybe.chars().all(|c| c.is_ascii_uppercase()) {
                    maybe.to_string()
                } else {
                    String::new()
                }
            } else {
                String::new()
            };
            return Some((program, callsign));
        }
    }

    // <CALLSIGN>HD<n>...
    if let Some(program) = parse_hd_program_token(&upper) {
        if let Some(hd_idx) = upper.find("HD") {
            let prefix = upper[..hd_idx].trim();
            if (3..=5).contains(&prefix.len()) && prefix.chars().all(|c| c.is_ascii_uppercase()) {
                return Some((program, prefix.to_string()));
            }
        }
    }

    None
}

fn is_likely_station_logo_filename(filename: &str) -> bool {
    let raw = lot_name_without_prefix(filename);
    let upper = raw.to_ascii_uppercase();

    let mut score: i32 = 0;
    if upper.starts_with("SL") && upper.contains("$$") {
        score += 5;
    }
    if upper.starts_with("SL") && parse_hd_program_token(&upper).is_some() {
        score += 3;
    }
    if let Some(hd_idx) = upper.find("HD") {
        let prefix = upper[..hd_idx].trim();
        if (3..=5).contains(&prefix.len()) && prefix.chars().all(|c| c.is_ascii_uppercase()) {
            score += 2;
        }
    }
    if upper.starts_with("MT") || upper.starts_with("MN") || upper.starts_with("SD") {
        score -= 2;
    }

    score >= 3
}

fn classify_lot_image(
    filename: &str,
    lot_mime: u32,
    lot_component_mime: Option<u32>,
) -> LotImageClass {
    use crate::ffi::nrsc5_sys::{
        NRSC5_MIME_JPEG, NRSC5_MIME_PNG, NRSC5_MIME_PRIMARY_IMAGE, NRSC5_MIME_STATION_LOGO,
    };

    if lot_mime == NRSC5_MIME_STATION_LOGO {
        return LotImageClass::StationLogo {
            definitive: true,
            source: "lot.mime",
        };
    }
    if lot_component_mime == Some(NRSC5_MIME_STATION_LOGO) {
        return LotImageClass::StationLogo {
            definitive: true,
            source: "lot.component.data.mime",
        };
    }
    if lot_mime == NRSC5_MIME_PRIMARY_IMAGE {
        return LotImageClass::CoverArt { source: "lot.mime" };
    }
    if lot_component_mime == Some(NRSC5_MIME_PRIMARY_IMAGE) {
        return LotImageClass::CoverArt {
            source: "lot.component.data.mime",
        };
    }
    let is_generic = lot_mime == NRSC5_MIME_JPEG
        || lot_mime == NRSC5_MIME_PNG
        || lot_component_mime == Some(NRSC5_MIME_JPEG)
        || lot_component_mime == Some(NRSC5_MIME_PNG);

    if is_generic {
        if is_likely_station_logo_filename(filename) {
            return LotImageClass::StationLogo {
                definitive: false,
                source: "filename_heuristic",
            };
        }
        return LotImageClass::GenericImage {
            source: "generic_image_mime",
        };
    }

    if is_likely_station_logo_filename(filename) {
        return LotImageClass::StationLogo {
            definitive: false,
            source: "filename_heuristic",
        };
    }

    LotImageClass::Other
}

/// Lower-cased file extension for a logo filename, defaulting to `png`
/// when there's no recognizable extension.
fn logo_ext(filename: &str) -> String {
    std::path::Path::new(filename)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_else(|| "png".to_string())
}

fn extract_call_sign(filename: &str) -> Option<String> {
    let raw = filename
        .split_once('_')
        .map(|(_, rest)| rest)
        .unwrap_or(filename);

    // Find "HD" followed by an ASCII digit.
    let mut search_from = 0;
    loop {
        let hd_pos = raw[search_from..].find("HD")?;
        let abs = search_from + hd_pos;
        let after = &raw[abs + 2..];
        if after
            .chars()
            .next()
            .map(|c| c.is_ascii_digit())
            .unwrap_or(false)
        {
            let call_sign = &raw[..abs];
            if (3..=5).contains(&call_sign.len())
                && call_sign.chars().all(|c| c.is_ascii_uppercase())
            {
                return Some(call_sign.to_string());
            }
        }
        search_from = abs + 2;
        if search_from >= raw.len() {
            return None;
        }
    }
}

/// Restore the persisted album-art history from disk and produce three
/// pieces of state the constructor needs: the rebuilt `art_history` map
/// (live `Instant`-based timestamps), the sorted `ArtTile` list, and a
/// best-guess `art_session_started` Instant derived from the oldest
/// persisted play.
///
/// Entries with no plays inside the 8-hour rolling window are discarded.
/// Entries whose backing image file is missing on disk are also dropped.
fn restore_art_history(
    cache: Option<&crate::art_cache::ArtCache>,
    cap: usize,
) -> (HashMap<u64, ArtEntry>, Vec<ArtTile>, Option<Instant>) {
    let mut map: HashMap<u64, ArtEntry> = HashMap::new();
    let mut tiles: Vec<ArtTile> = Vec::new();
    let mut oldest_play: Option<Instant> = None;

    let Some(cache) = cache else {
        return (map, tiles, None);
    };

    let now_inst = Instant::now();
    let now_utc_ms = Utc::now().timestamp_millis();
    let cutoff_ms = now_utc_ms.saturating_sub(ART_WINDOW.as_millis() as i64);

    for entry in cache.load_manifest() {
        let abs_path = cache.dir().join(&entry.filename);
        if !abs_path.exists() {
            // Manifest references a file we no longer have. Skip it; the
            // sweep on the next save will tidy the manifest itself.
            continue;
        }
        // Drop expired plays and convert the survivors to monotonic
        // Instants by subtracting their wall-clock age from `now_inst`.
        let mut plays: VecDeque<Instant> = VecDeque::new();
        for ms in entry.plays_unix_ms {
            if ms < cutoff_ms {
                continue;
            }
            let age_ms = (now_utc_ms - ms).max(0) as u64;
            let age = Duration::from_millis(age_ms);
            if let Some(inst) = now_inst.checked_sub(age) {
                plays.push_back(inst);
            }
        }
        if plays.is_empty() {
            continue;
        }
        if let Some(front) = plays.front() {
            oldest_play = Some(oldest_play.map(|cur| cur.min(*front)).unwrap_or(*front));
        }
        let path = abs_path.to_string_lossy().into_owned();
        map.insert(
            entry.hash,
            ArtEntry {
                path,
                plays,
                songs: entry.songs,
                album: entry.album,
            },
        );
    }

    // Mirror `Nrsc5App::rebuild_art_tiles` so the collage paints correctly
    // on the very first frame after restore, before any new event arrives.
    let mut entries: Vec<(u64, &ArtEntry, Instant)> = map
        .iter()
        .filter_map(|(k, e)| e.plays.front().map(|t| (*k, e, *t)))
        .collect();
    entries.sort_by_key(|e| std::cmp::Reverse(e.1.plays.len()));
    entries.truncate(cap);
    entries.sort_by(|a, b| a.2.cmp(&b.2).then_with(|| a.1.path.cmp(&b.1.path)));
    tiles = entries
        .into_iter()
        .map(|(k, e, _)| ArtTile {
            path: e.path.clone(),
            hash: k,
            count: e.plays.len() as u32,
            songs: e.songs.clone(),
            album: e.album.clone(),
        })
        .collect();

    (map, tiles, oldest_play)
}

/// Build the initial dock layout.
///
/// We intentionally avoid restoring the embedded RON snapshot here.
/// The current egui/egui_dock combo can assert during tab-bar rendering
/// when window-surface metadata from serialized layouts disagrees with
/// live layer assignment. A deterministic hand-built single-surface tree
/// keeps startup stable.
/// eframe storage key under which the persisted dock layout is saved.
const DOCK_LAYOUT_KEY: &str = "dock_layout";

/// Default dock layout — used on a fresh install and as the fallback
/// whenever no saved layout is present (or a saved one fails to parse).
/// A hand-arranged, single-surface layout captured with the Ctrl+Shift+D
/// dev helper and baked in from `res/default-layout.ron`. If the embedded
/// RON ever fails to parse (e.g. an egui_dock schema bump), it falls back
/// further to a programmatic split layout.
fn default_dock_state() -> DockState<DockTab> {
    const DEFAULT_LAYOUT_RON: &str = include_str!("../res/default-layout.ron");
    ron::from_str::<DockState<DockTab>>(DEFAULT_LAYOUT_RON).unwrap_or_else(|e| {
        eprintln!("[layout] embedded default layout failed to parse ({e}); using fallback");
        fallback_dock_state()
    })
}

/// Programmatic fraction-based fallback used only if the baked-in default
/// layout fails to parse. Single-surface and resolution-independent.
fn fallback_dock_state() -> DockState<DockTab> {
    // Center column starts as the root leaf.
    let mut state = DockState::new(vec![
        DockTab::NowPlaying,
        DockTab::Collage,
        DockTab::Spectrum,
        DockTab::Constellation,
    ]);
    let tree = state.main_surface_mut();

    // Left column (~22%). `fraction` is the *old* (center) node's share and
    // the new leaf is placed to the left, so 0.78 leaves the left ~22%.
    let [center, _left] = tree.split_left(
        NodeIndex::root(),
        0.78,
        vec![
            DockTab::Tuner,
            DockTab::StationInfo,
            DockTab::Signal,
            DockTab::EngineeringInfo,
        ],
    );

    // Right column (~28% of the remaining center width): the maps.
    let [center, _right] = tree.split_right(center, 0.72, vec![DockTab::Weather, DockTab::Traffic]);

    // Log strip along the bottom of the center column (~22% of its height).
    let [_center, _log] = tree.split_below(center, 0.78, vec![DockTab::Log]);

    state
}

#[cfg(test)]
mod tests {
    use super::*;

    // =================================================================
    // Frequency-change state transition
    //
    // `snap_fm_tune_mhz` is the pure core of the retune path: every
    // TuneMhz command runs the raw value through it before the app
    // resets station state and spawns the background retune. These
    // tests pin the band clamping and 200 kHz raster snapping.
    // =================================================================

    #[test]
    fn snap_fm_tune_clamps_below_band() {
        assert_eq!(snap_fm_tune_mhz(80.0), FM_TUNE_MIN_MHZ);
        assert_eq!(snap_fm_tune_mhz(0.0), FM_TUNE_MIN_MHZ);
        assert_eq!(snap_fm_tune_mhz(f32::NEG_INFINITY), FM_TUNE_MIN_MHZ);
    }

    #[test]
    fn snap_fm_tune_clamps_above_band() {
        assert_eq!(snap_fm_tune_mhz(120.0), FM_TUNE_MAX_MHZ);
        assert_eq!(snap_fm_tune_mhz(f32::INFINITY), FM_TUNE_MAX_MHZ);
    }

    #[test]
    fn snap_fm_tune_rounds_to_200khz_raster() {
        // Off-grid inputs (well clear of x.5 ties) snap to the nearest
        // odd-tenth channel; exact grid values pass through unchanged.
        assert_eq!(snap_fm_tune_mhz(97.13), 97.1);
        assert_eq!(snap_fm_tune_mhz(100.29), 100.3);
        assert_eq!(snap_fm_tune_mhz(88.5), 88.5);
        assert_eq!(snap_fm_tune_mhz(107.9), 107.9);
    }

    #[test]
    fn snap_fm_tune_sweep_in_band_and_idempotent() {
        // Property sweep: every output lands inside the band, and
        // re-snapping a snapped value is a fixed point (the UI applies
        // this repeatedly as the user nudges the dial).
        let mut mhz = 70.0_f32;
        while mhz < 130.0 {
            let s = snap_fm_tune_mhz(mhz);
            assert!(
                (FM_TUNE_MIN_MHZ..=FM_TUNE_MAX_MHZ).contains(&s),
                "{s} out of band for input {mhz}"
            );
            assert!(
                (snap_fm_tune_mhz(s) - s).abs() < 1e-4,
                "not idempotent at {mhz} (snapped {s})"
            );
            mhz += 0.01;
        }
    }

    // =================================================================
    // Gain-table snapping (apply-time correctness)
    // =================================================================

    #[test]
    fn snap_to_gain_table_empty_returns_input() {
        // Continuous-gain devices expose an empty table → no-op.
        assert_eq!(snap_to_gain_table(123, &[]), 123);
    }

    #[test]
    fn snap_to_gain_table_nearest_neighbor() {
        let table = [0, 90, 140, 240, 480];
        assert_eq!(snap_to_gain_table(240, &table), 240); // exact
        assert_eq!(snap_to_gain_table(100, &table), 90); // nearer 90
        assert_eq!(snap_to_gain_table(200, &table), 240); // nearer 240
        assert_eq!(snap_to_gain_table(-50, &table), 0); // below min → first
        assert_eq!(snap_to_gain_table(9999, &table), 480); // above max → last
    }

    #[test]
    fn snap_to_gain_table_tie_prefers_earlier_entry() {
        // The strict `<` comparison keeps the first (lower) candidate
        // on an exact midpoint.
        assert_eq!(snap_to_gain_table(150, &[100, 200]), 100);
    }

    #[test]
    fn snap_to_gain_table_single_entry() {
        assert_eq!(snap_to_gain_table(7, &[42]), 42);
    }

    // =================================================================
    // Collage tile cap (hand-edited config robustness)
    // =================================================================

    fn cfg_with_tiles(n: u32) -> AppConfig {
        AppConfig {
            collage_max_tiles: n,
            ..Default::default()
        }
    }

    #[test]
    fn collage_tile_cap_snaps_to_power_of_two() {
        assert_eq!(collage_tile_cap(&cfg_with_tiles(100)), 128);
        assert_eq!(collage_tile_cap(&cfg_with_tiles(64)), 64);
        assert_eq!(collage_tile_cap(&cfg_with_tiles(3)), 4);
    }

    #[test]
    fn collage_tile_cap_clamps_to_hard_max() {
        assert_eq!(
            collage_tile_cap(&cfg_with_tiles(100_000)),
            ART_TILES_HARD_MAX
        );
        assert_eq!(collage_tile_cap(&cfg_with_tiles(512)), 512);
    }

    #[test]
    fn collage_tile_cap_floors_at_one() {
        // 0 from a corrupt hand-edited config must not yield zero tiles.
        assert_eq!(collage_tile_cap(&cfg_with_tiles(0)), 1);
        assert_eq!(collage_tile_cap(&cfg_with_tiles(1)), 1);
    }

    // =================================================================
    // Filename sanitization (recording subfolders)
    // =================================================================

    #[test]
    fn sanitize_filename_empty_becomes_underscore() {
        assert_eq!(sanitize_filename(""), "_");
    }

    #[test]
    fn sanitize_filename_preserves_safe_chars() {
        assert_eq!(sanitize_filename("KEXP-FM_90.3"), "KEXP-FM_90.3");
    }

    #[test]
    fn sanitize_filename_replaces_unsafe_chars() {
        assert_eq!(sanitize_filename("A/B\\C:D*E?"), "A_B_C_D_E_");
        assert_eq!(sanitize_filename("hello world"), "hello_world");
    }

    #[test]
    fn sanitize_filename_non_ascii_becomes_underscore() {
        // Conservative ASCII-only policy: each non-ASCII scalar → one `_`.
        assert_eq!(sanitize_filename("caf\u{00e9}"), "caf_");
    }

    // =================================================================
    // HERE image-type labelling
    // =================================================================

    #[test]
    fn here_image_type_label_maps_known_and_unknown() {
        use crate::ffi::nrsc5_sys;
        assert_eq!(
            here_image_type_label(nrsc5_sys::NRSC5_HERE_IMAGE_TRAFFIC),
            "traffic"
        );
        assert_eq!(
            here_image_type_label(nrsc5_sys::NRSC5_HERE_IMAGE_WEATHER),
            "weather"
        );
        assert_eq!(here_image_type_label(0), "other");
        assert_eq!(here_image_type_label(999), "other");
    }
}
