use crate::collage::CollageEngine;
use crate::config::{load_config, save_config, AppConfig};
use crate::ffi::{Nrsc5Process, NrscEvent};
use crate::gui::dock::{DockTab, DockViewer, UiCommand};
use crate::gui::state::{AppState, ArtTile};
use crate::maps::{TrafficMap, WeatherMap};
use chrono::Utc;
use egui_dock::{DockArea, DockState, NodeIndex, NodePath, SurfaceIndex};
use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Hard cap on tracked album-art tiles — prevents the collage from getting
/// unbounded if a session runs all day on a very busy station.
/// Hard cap on tracked album-art tiles — prevents the collage from getting
/// silly on long sessions and bounds memory + persistence cost. The actual
/// displayed cap is read from `AppConfig.collage_max_tiles` (1..=512,
/// snapped to a power of two) and may be lower than this.
const ART_TILES_HARD_MAX: usize = 512;

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
/// Rolling window for the album-art collage. Plays older than this are
/// pruned on every new event so the heat-map keeps moving instead of
/// freezing.
const ART_WINDOW: Duration = Duration::from_secs(8 * 60 * 60);
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
    start_requested_at: Option<Instant>,
    last_signal_at: Option<Instant>,
    _collage: CollageEngine,
    /// Maps LOT ID → filename written in the AAS directory.
    lot_files: HashMap<String, String>,
    /// Path to the AAS dump directory.
    aas_dir: PathBuf,
    traffic_map: TrafficMap,
    weather_map: WeatherMap,
    /// COM-based per-process volume controller (Windows only).
    #[cfg(target_os = "windows")]
    volume_ctl: crate::winaudio::ProcessVolumeControl,
    /// Last time we tried to (re)discover the nrsc5 audio session.
    last_session_probe_at: Option<Instant>,
    /// Histogram of unique album art images seen this session, keyed by a
    /// content hash so re-emissions of the same bytes — even under different
    /// LOT filenames — collapse into a single tile.
    art_history: HashMap<u64, ArtEntry>,
    /// Persistent on-disk cache so the collage survives restarts. `None`
    /// only if the local data dir couldn't be resolved/created, in which
    /// case the collage falls back to in-memory-only behavior.
    art_cache: Option<crate::art_cache::ArtCache>,
    /// Last time we pruned expired plays from `art_history`. Throttled so
    /// we don't walk the map every UI frame.
    last_art_prune_at: Option<Instant>,
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
}

impl Nrsc5App {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        egui_extras::install_image_loaders(&_cc.egui_ctx);
        Self::install_fonts(&_cc.egui_ctx);
        let config = load_config();
        Self::apply_theme(&_cc.egui_ctx, config.dark_mode);
        let dock_state = _cc
            .storage
            .and_then(|s| eframe::get_value::<DockState<DockTab>>(s, "dock_state"))
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
        let nrsc5 = nrsc5.map(|mut backend| {
            backend.set_spectrum_tap(spectrum_tap.clone());
            backend
        });

        let aas_dir = nrsc5
            .as_ref()
            .map(|n| n.aas_dir().to_path_buf())
            .unwrap_or_else(crate::paths::aas_temp_dir);

        // Open the on-disk art cache and load any history from previous
        // sessions. Failure here is non-fatal — we just start with an
        // empty collage and degrade to in-memory-only behavior.
        let art_cache = crate::art_cache::ArtCache::new();
        let (art_history, art_tiles, art_session_started) =
            restore_art_history(art_cache.as_ref(), collage_tile_cap(&config));

        // Snapshot any per-field config values needed after `config` is
        // moved into the struct literal below.
        let play_log_retention_hours = config.play_log_retention_hours;

        Self {
            app_state: AppState {
                frequency_mhz: config.frequency_mhz,
                selected_program: config.selected_program,
                dark_mode: config.dark_mode,
                nrsc5_status,
                volume: config.volume.clamp(0.0, 1.0),
                muted: config.muted,
                gain_mode: config.gain_mode,
                manual_gain_tenths: config.manual_gain_tenths,
                // Default to "present" + "probe available" so the no-SDR
                // overlay only appears once we've actually probed and seen
                // zero devices — avoids a flash on launch.
                sdr_present: true,
                sdr_probe_available: true,
                art_tiles,
                art_session_started,
                collage_tile_cap: collage_tile_cap(&config) as u32,
                spectrum_tap: Some(spectrum_tap),
                ..AppState::default()
            },
            dock_state,
            config,
            nrsc5,
            retune_task: None,
            start_requested_at: None,
            last_signal_at: None,
            _collage: CollageEngine::new(8),
            lot_files: HashMap::new(),
            aas_dir: aas_dir.clone(),
            traffic_map: TrafficMap::new(&aas_dir),
            weather_map: WeatherMap::new(&aas_dir),
            #[cfg(target_os = "windows")]
            volume_ctl: crate::winaudio::ProcessVolumeControl::new(),
            last_session_probe_at: None,
            art_history,
            art_cache,
            last_art_prune_at: None,
            closed_tab_locations: HashMap::new(),
            prev_layout: LayoutSnapshot::default(),
            play_log: {
                let mut log = crate::play_log::PlayLog::load();
                log.set_retention_hours(play_log_retention_hours);
                log
            },
            last_cover_play_at: None,
        }
    }
}

impl eframe::App for Nrsc5App {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        _visuals.panel_fill.to_normalized_gamma_f32()
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.update_runtime_metrics();
        ui.ctx().request_repaint_after(Duration::from_millis(50));

        // Drain events from the nrsc5 process.
        if let Some(nrsc5) = &self.nrsc5 {
            let mut pending = Vec::new();
            while let Ok(evt) = nrsc5.events().try_recv() {
                pending.push(evt);
            }
            for evt in pending {
                self.app_state.last_event = evt.label().to_string();
                self.handle_nrsc5_event(evt);
            }
        }

        // Check if a background retune task finished.
        if let Some(handle) = self.retune_task.as_ref() {
            if handle.is_finished() {
                let handle = self.retune_task.take().unwrap();
                match handle.join() {
                    Ok((backend, None)) => {
                        self.nrsc5 = Some(backend);
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
                        self.app_state.is_streaming = false;
                        self.app_state.nrsc5_status = format!("retune failed: {err_msg}");
                    }
                    Err(_panic) => {
                        self.app_state.nrsc5_status = "retune thread panicked".to_string();
                    }
                }
            }
        }

        // Collect commands emitted by top-bar buttons (hamburger menu
        // items, the SDR chip) so they get processed by the same
        // dispatch loop as commands emitted by the dock panels. Declared
        // here so the closure below can push into it.
        let mut commands_from_top_bar: Vec<UiCommand> = Vec::new();
        ui.horizontal(|ui| {
            // Theme toggle stays at the very left as the most-used
            // single-purpose button.
            let mut menu_commands: Vec<UiCommand> = Vec::new();
            let theme_icon = if self.app_state.dark_mode { "☀" } else { "🌙" };
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
                if ui.button("\u{1F4E1}  SDR Settings...").clicked() {
                    menu_commands.push(UiCommand::ShowSdrSettings);
                    ui.close_menu();
                }
                ui.separator();
                if ui.button("\u{21BA}  Reset Panel Layout").clicked() {
                    self.dock_state = default_dock_state();
                    ui.close_menu();
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
                    .color(egui::Color32::from_rgb(100, 160, 255)),
            );
            ui.separator();
            ui.label(
                egui::RichText::new(format!("{:.1} MHz", self.app_state.frequency_mhz))
                    .monospace(),
            );
            ui.separator();
            // Active SDR device chip — clickable shortcut to the SDR
            // Settings modal. Shows just the driver key in the top bar
            // to keep the space tight; full label is in the modal.
            let sdr_chip_text = format!("\u{1F4E1} {}", self.config.sdr.driver);
            if ui
                .button(egui::RichText::new(sdr_chip_text).monospace())
                .on_hover_text("Open SDR Settings (driver + device + gains)")
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
            ui.label(egui::RichText::new(&self.app_state.nrsc5_status).color(status_color));
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

            // Reset-layout button, right-aligned by allocating remaining space.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .button("↺")
                    .on_hover_text("Reset panel layout to default")
                    .clicked()
                {
                    self.dock_state = default_dock_state();
                }
            });

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

        // Periodically try to discover/refresh the nrsc5 audio session so the
        // volume slider becomes usable once playback starts.
        self.poll_audio_session();

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
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, "dock_state", &self.dock_state);
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
        if let Some(chain) =
            fonts.families.get_mut(&egui::FontFamily::Proportional)
        {
            if !chain.iter().any(|n| n == "Hack") {
                chain.push("Hack".to_owned());
            }
        }
        ctx.set_fonts(fonts);
    }

    fn apply_theme(ctx: &egui::Context, dark: bool) {
        let accent = egui::Color32::from_rgb(100, 160, 255);
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
                egui::Stroke::new(0.5, egui::Color32::from_gray(60));
            v.widgets.inactive.bg_stroke =
                egui::Stroke::new(0.5, egui::Color32::from_gray(80));
            v
        } else {
            let mut v = egui::Visuals::light();
            let panel_bg = egui::Color32::from_rgb(245, 245, 250);
            v.panel_fill = panel_bg;
            v.window_fill = egui::Color32::WHITE;
            v.extreme_bg_color = egui::Color32::from_rgb(235, 235, 240);
            v.faint_bg_color = egui::Color32::from_rgb(240, 240, 248);
            v.widgets.noninteractive.bg_stroke =
                egui::Stroke::new(0.5, egui::Color32::from_gray(200));
            v.widgets.inactive.bg_stroke =
                egui::Stroke::new(0.5, egui::Color32::from_gray(180));
            v
        };

        visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, accent);
        visuals.widgets.active.bg_stroke = egui::Stroke::new(1.0, accent);

        let cr = egui::CornerRadius::same(4);
        visuals.widgets.noninteractive.corner_radius = cr;
        visuals.widgets.inactive.corner_radius = cr;
        visuals.widgets.hovered.corner_radius = cr;
        visuals.widgets.active.corner_radius = cr;
        visuals.window_corner_radius = egui::CornerRadius::same(6);

        visuals.selection.bg_fill = accent.linear_multiply(0.3);
        visuals.selection.stroke = egui::Stroke::new(1.0, accent);
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
        self.app_state.active_gain_mode =
            self.nrsc5.as_ref().and_then(|n| n.active_gain_mode());
        self.app_state.active_manual_gain_tenths =
            self.nrsc5.as_ref().and_then(|n| n.active_manual_gain_tenths());
    }

    /// Periodically attempt to (re)discover the nrsc5 audio session. The
    /// session only exists once the child process starts producing audio,
    /// so we retry every ~500ms while streaming until it appears, then
    /// push the current volume/mute state into it.
    fn poll_audio_session(&mut self) {
        #[cfg(target_os = "windows")]
        {
            let pid = self.nrsc5.as_ref().and_then(|p| p.pid());
            let Some(pid) = pid else {
                if self.app_state.audio_session_ready {
                    self.app_state.audio_session_ready = false;
                    self.volume_ctl.detach();
                }
                return;
            };

            // Throttle probe attempts.
            let now = Instant::now();
            let due = self
                .last_session_probe_at
                .map(|t| now.duration_since(t) >= Duration::from_millis(500))
                .unwrap_or(true);
            if !due && self.app_state.audio_session_ready {
                return;
            }
            self.last_session_probe_at = Some(now);

            // Try a no-op read to test/establish the session.
            match self.volume_ctl.get_volume(pid) {
                Ok(_) => {
                    if !self.app_state.audio_session_ready {
                        self.app_state.audio_session_ready = true;
                        // First time we've seen the session this run: push
                        // our persisted volume/mute state into it.
                        self.apply_volume();
                        self.apply_mute();
                    }
                }
                Err(_) => {
                    self.app_state.audio_session_ready = false;
                }
            }
        }
    }

    fn apply_volume(&mut self) {
        #[cfg(target_os = "windows")]
        {
            let Some(pid) = self.nrsc5.as_ref().and_then(|p| p.pid()) else {
                return;
            };
            let _ = self.volume_ctl.set_volume(pid, self.app_state.volume);
        }
    }

    fn apply_mute(&mut self) {
        #[cfg(target_os = "windows")]
        {
            let Some(pid) = self.nrsc5.as_ref().and_then(|p| p.pid()) else {
                return;
            };
            let _ = self.volume_ctl.set_mute(pid, self.app_state.muted);
        }
    }

    /// Probe `librtlsdr` for the number of attached RTL-SDR devices and
    /// update `app_state.sdr_present` / `sdr_probe_available`. Throttled to
    /// roughly one probe every two seconds. If the probe is unavailable on
    /// this system (DLL missing), we silently mark it so and never show the
    /// no-SDR overlay — a false "missing" warning would be worse than no
    /// warning at all.
    fn poll_sdr_presence(&mut self, ctx: &egui::Context) {
        let now = Instant::now();
        let due = self
            .app_state
            .sdr_last_probed
            .map(|t| now.duration_since(t) >= Duration::from_millis(2000))
            .unwrap_or(true);
        if !due {
            return;
        }
        self.app_state.sdr_last_probed = Some(now);

        match crate::sdr_detect::device_count() {
            Some(n) => {
                self.app_state.sdr_probe_available = true;
                self.app_state.sdr_present = n > 0;
            }
            None => {
                self.app_state.sdr_probe_available = false;
                self.app_state.sdr_present = true;
            }
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
                        1.0,
                        egui::Color32::from_rgb(90, 120, 180),
                    ))
                    .corner_radius(egui::CornerRadius::same(10))
                    .inner_margin(egui::Margin::same(24))
                    .show(ui, |ui| {
                        ui.set_max_width(380.0);
                        ui.vertical_centered(|ui| {
                            ui.label(
                                egui::RichText::new("\u{1F4F6}")
                                    .size(56.0),
                            );
                            ui.add_space(2.0);
                            ui.label(
                                egui::RichText::new("No SDR detected")
                                    .heading()
                                    .color(egui::Color32::from_rgb(
                                        230, 200, 110,
                                    )),
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
                                egui::RichText::new(
                                    "(or just wait \u{2014} we keep checking)",
                                )
                                .small()
                                .color(egui::Color32::from_gray(150)),
                            );
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
    /// `app_state.cover_art_path` to the durable cache copy when we have one
    /// (falling back to the AAS-dump path otherwise), and deletes the
    /// redundant AAS-dump file after a successful cache write so the temp
    /// dir doesn't accumulate ~50 KB per song forever.
    fn record_album_art(&mut self, full_path: &std::path::Path, path_str: &str) {
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
            self.app_state.cover_art_path = Some(path_str.to_string());
            return;
        };
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        bytes.hash(&mut hasher);
        let key = hasher.finish();

        // Per-hash cooldown. If we counted this exact image less than
        // `ART_COUNT_COOLDOWN` ago, treat this XHDR as a re-broadcast of
        // the same in-progress song rather than a new play. Still redirect
        // the live cover to the existing cache copy and prune the new AAS
        // dump — its contents are byte-identical to what we already have.
        if let Some(existing) = self.art_history.get(&key) {
            if let Some(&last) = existing.plays.back() {
                if now.duration_since(last) < ART_COUNT_COOLDOWN {
                    let cached_path = existing.path.clone();
                    self.app_state.cover_art_path = Some(cached_path.clone());
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
        let resolved_path = cached_path
            .clone()
            .unwrap_or_else(|| path_str.to_string());

        // Live cover display follows the cache when possible.
        self.app_state.cover_art_path = Some(resolved_path.clone());
        // With a durable cache copy in hand, the AAS-dir dump is dead weight.
        if cached_path.is_some() {
            let _ = std::fs::remove_file(full_path);
        }

        // Grab the song metadata currently on display so we can label this
        // cover later in tooltips. Trim and skip empty pieces so we don't
        // accumulate noise entries like ("", "").
        let title = self.app_state.title.trim().to_string();
        let artist = self.app_state.artist.trim().to_string();
        let album = self.app_state.album.trim().to_string();

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
        self.rebuild_art_tiles();
        self.persist_art_history();

        // A genuinely new play just landed (the cover-hash cooldown has
        // gated this code path). Remember the moment so the metadata
        // handler can decide whether subsequent title/artist updates
        // belong to a real song vs. a station-slogan flap, and try to push
        // the play to the rolling 24-hour log right now.
        self.last_cover_play_at = Some(crate::play_log::now_millis());
        self.try_record_play();
    }

    /// Try to record the currently-displayed song into the rolling play
    /// log. Idempotent — the log's own gate (pair-equality dedup +
    /// rate-limit) drops noisy re-calls. Persists on success.
    fn try_record_play(&mut self) {
        let now_ms = crate::play_log::now_millis();
        let title = self.app_state.title.clone();
        let artist = self.app_state.artist.clone();
        let freq = self.config.frequency_mhz;
        let program = self.app_state.selected_program;
        let call_sign = self.app_state.call_sign.clone();
        if self.play_log.try_push(
            now_ms,
            &title,
            &artist,
            freq,
            program,
            &call_sign,
        ) {
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
        let mut entries: Vec<(&ArtEntry, Instant)> = self
            .art_history
            .values()
            .filter_map(|e| e.plays.front().map(|t| (e, *t)))
            .collect();
        // Step 1: keep the most-played covers within the tile cap.
        entries.sort_by(|a, b| b.0.plays.len().cmp(&a.0.plays.len()));
        entries.truncate(cap);
        // Step 2: re-order the survivors by arrival so spatial layout
        // reflects "order they came in", not "how many plays".
        entries.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.path.cmp(&b.0.path)));
        self.app_state.art_tiles = entries
            .into_iter()
            .map(|(e, _)| ArtTile {
                path: e.path.clone(),
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
                self.app_state.nrsc5_status = "device lost".to_string();
            }
            NrscEvent::Sync => {
                self.last_signal_at = Some(Instant::now());
                self.app_state.currently_synced = true;
                self.app_state.lost_sync_at = None;
                self.app_state.nrsc5_status = "synced".to_string();
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
                        crate::station_info::ProgramInfo::from_short_name(
                            String::new(),
                        )
                    });
                    info.bit_rate_kbps = Some(kbps);
                    self.app_state.station_info.last_updated =
                        Some(Instant::now());
                }
            }
            NrscEvent::Metadata {
                title,
                artist,
                album,
                genre,
                ..
            } => {
                if !self.app_state.is_streaming {
                    return;
                }

                self.last_signal_at = Some(Instant::now());
                self.app_state.active_program = self.app_state.selected_program;

                let now = Instant::now();
                if !title.is_empty() {
                    self.app_state.title = title;
                    self.app_state.title_updated = Some(now);
                }
                if !artist.is_empty() {
                    self.app_state.artist = artist;
                    self.app_state.artist_updated = Some(now);
                }
                if !album.is_empty() {
                    self.app_state.album = album;
                    self.app_state.album_updated = Some(now);
                }
                if !genre.is_empty() {
                    self.app_state.genre = genre;
                    self.app_state.genre_updated = Some(now);
                }

                // Try to record this metadata update to the play log only
                // if a fresh cover-art change happened recently. Station
                // slogans / IDs arrive through the same title/artist
                // events but without a corresponding cover swap, so this
                // recent-cover gate filters them out without a blocklist.
                if let Some(last) = self.last_cover_play_at {
                    let now_ms = crate::play_log::now_millis();
                    if now_ms - last < 30_000 {
                        self.try_record_play();
                    }
                }
            }
            NrscEvent::LotFile { lot, name } => {
                // Try to derive the broadcaster call sign from the filename.
                if self.app_state.call_sign.is_empty() {
                    if let Some(cs) = extract_call_sign(&name) {
                        self.app_state.call_sign = cs;
                    }
                }
                // Feed to map processors before storing.
                if self.traffic_map.process_lot(&name) {
                    self.app_state.traffic_map_path =
                        self.traffic_map.completed_path.clone();
                }
                if self.weather_map.process_lot(&name) {
                    let prev_len = self.app_state.weather_frames.len();
                    let prev_idx = self.app_state.weather_current_frame;
                    let new_frames = self.weather_map.frames.clone();
                    let new_last = new_frames.len().saturating_sub(1);
                    // Viewer was "following the live tail" if either the
                    // animation is playing or the viewer was already on (or
                    // past) the previous newest frame, or we had no frames
                    // at all yet.
                    let following = prev_len == 0
                        || self.app_state.weather_playing
                        || prev_idx + 1 >= prev_len;
                    // If the buffer was at capacity, the new frame caused the
                    // oldest to be dropped, so every existing index shifts
                    // down by one.
                    let shifted_idx = if prev_len == crate::maps::MAX_WEATHER_FRAMES
                        && new_frames.len() == prev_len
                    {
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
                }
                self.lot_files.insert(lot, name);
            }
            NrscEvent::Xhdr { param, lot } => {
                if let Some(filename) = self.lot_files.get(&lot) {
                    let full_path = self.aas_dir.join(filename);
                    if full_path.exists() {
                        let path_str = full_path.to_string_lossy().to_string();
                        if param == 0 {
                            // Cover art. `record_album_art` sets
                            // `cover_art_path` itself (preferring the durable
                            // cache copy) and prunes the AAS-dir dump after
                            // a successful cache write.
                            self.record_album_art(&full_path, &path_str);
                        } else if param == 1 {
                            // Station logo
                            self.app_state.station_logo_path = Some(path_str);
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
            NrscEvent::Location {
                latitude,
                longitude,
                altitude_m,
            } => {
                self.app_state.station_info.location =
                    Some(crate::station_info::Location {
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
                            *slot = Some(
                                crate::station_info::ProgramInfo::from_short_name(
                                    name,
                                ),
                            );
                        }
                    }
                    self.app_state.station_info.last_updated =
                        Some(Instant::now());
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
                        crate::station_info::ProgramInfo::from_short_name(
                            String::new(),
                        )
                    });
                    info.program_type = Some(program_type);
                    info.sound_experience = Some(sound_experience);
                    self.app_state.station_info.last_updated =
                        Some(Instant::now());
                }
            }
            NrscEvent::SigServiceData { number, name } => {
                // Dedup by SIS-assigned number: SIS repeats every few
                // seconds, so replace any existing entry rather than
                // appending duplicates.
                let services = &mut self.app_state.station_info.data_services;
                if let Some(existing) =
                    services.iter_mut().find(|s| s.number == number)
                {
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
            _ => {}
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
                            self.app_state.nrsc5_status =
                                format!("NRSC5 unavailable: {err}");
                            return;
                        }
                    }
                }

                let nrsc5 = self.nrsc5.as_mut().unwrap();
                let mhz = self.app_state.frequency_mhz;
                let program = self.app_state.selected_program;

                // v0.3.0: every Start path goes through the in-process
                // SoapySDR backend (`start_piped`). Legacy USB and
                // rtl_tcp dispatch was removed when the native librtlsdr
                // backend was deleted; rtl_tcp restoration is tracked
                // for v0.4.0 via SoapyRemote. `migrate_legacy_sdr` in
                // config.rs logs a one-shot warning at load time when a
                // user's existing config still has `use_rtl_tcp = true`.
                let sdr_args = self.config.sdr.to_args_string();
                let result = nrsc5.start_piped(
                    mhz,
                    program,
                    &sdr_args,
                    self.config.sdr.freq_correction_ppm,
                    self.config.gain_mode,
                    self.config.manual_gain_tenths,
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

                if let Some(nrsc5) = self.nrsc5.as_mut() {
                    nrsc5.stop();
                }

                self.app_state.is_streaming = false;
                self.start_requested_at = None;
                self.last_signal_at = None;
                self.app_state.currently_synced = false;
                self.app_state.lost_sync_at = None;
                self.lot_files.clear();
                self.app_state.cover_art_path = None;
                self.app_state.station_logo_path = None;
                self.app_state.traffic_map_path = None;
                self.app_state.weather_frames.clear();
                self.app_state.weather_current_frame = 0;
                self.app_state.weather_playing = false;
                self.app_state.call_sign.clear();
                // Clear aggregated SIS so a fresh Start re-discovers identity
                // from scratch rather than rendering stale fields from the
                // last session.
                self.app_state.station_info.reset();
                // Wipe PSD so the Station Info panel doesn't claim the
                // last-heard track is the "current" one once the stream
                // is no longer running.
                self.app_state.title.clear();
                self.app_state.artist.clear();
                self.app_state.album.clear();
                self.app_state.genre.clear();
                self.app_state.title_updated = None;
                self.app_state.artist_updated = None;
                self.app_state.album_updated = None;
                self.app_state.genre_updated = None;
                self.traffic_map.clear();
                self.weather_map.clear();
                self.app_state.nrsc5_status = "stream stopped".to_string();
            }
            UiCommand::TuneMhz(mhz) => {
                self.app_state.frequency_mhz = mhz;
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
                // PSD belongs to the previous station's broadcast; clear
                // it so the panel doesn't show the wrong song while the
                // new station's SIS / PSD roll in.
                self.app_state.title.clear();
                self.app_state.artist.clear();
                self.app_state.album.clear();
                self.app_state.genre.clear();
                self.app_state.title_updated = None;
                self.app_state.artist_updated = None;
                self.app_state.album_updated = None;
                self.app_state.genre_updated = None;
                self.lot_files.clear();
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
                        let device_index = self.config.rtl_device_index;
                        let handle = std::thread::spawn(move || {
                            let mut backend = backend;
                            if let Err(err) =
                                backend.retune(mhz, program, device_index)
                            {
                                return (backend, Some(format!("{err}")));
                            }
                            (backend, None)
                        });

                        self.retune_task = Some(handle);
                        self.app_state.nrsc5_status =
                            format!("retuning to {mhz:.1} MHz...");
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
                if clamped == self.app_state.selected_program {
                    return;
                }

                self.app_state.selected_program = clamped;
                self.config.selected_program = clamped;
                save_config(&self.config);

                // If streaming, restart with the new program.
                if self.app_state.is_streaming {
                    if let Some(backend) = self.nrsc5.take() {
                        self.app_state.is_streaming = false;

                        let mhz = self.app_state.frequency_mhz;
                        let device_index = self.config.rtl_device_index;
                        let handle = std::thread::spawn(move || {
                            let mut backend = backend;
                            if let Err(err) =
                                backend.retune(mhz, clamped, device_index)
                            {
                                return (backend, Some(format!("{err}")));
                            }
                            (backend, None)
                        });

                        self.retune_task = Some(handle);
                        self.app_state.nrsc5_status =
                            format!("switching to HD{}...", clamped + 1);
                        return;
                    }
                }

                self.app_state.nrsc5_status =
                    format!("selected HD{} (staged)", clamped + 1);
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
                } else if !self.app_state.artist.is_empty() {
                    self.app_state.artist.clone()
                } else if let Some(cs) = self.app_state.station_info.call_sign.clone()
                {
                    cs
                } else if !self.app_state.call_sign.is_empty() {
                    self.app_state.call_sign.clone()
                } else {
                    format!("HD{}", idx + 1)
                };
                let preset = crate::config::Preset {
                    name,
                    frequency_mhz: self.app_state.frequency_mhz,
                    program: self.app_state.selected_program,
                };
                // Extend the vec if needed.
                while self.config.presets.len() <= slot {
                    self.config.presets.push(crate::config::Preset::default());
                }
                self.config.presets[slot] = preset;
                save_config(&self.config);
                self.app_state.nrsc5_status =
                    format!("saved preset {}", slot + 1);
            }
            UiCommand::SetPreset(slot, preset) => {
                // Extend the vec if needed so editing an empty slot works.
                while self.config.presets.len() <= slot {
                    self.config.presets.push(crate::config::Preset::default());
                }
                self.config.presets[slot] = preset;
                save_config(&self.config);
                self.app_state.nrsc5_status =
                    format!("saved preset {}", slot + 1);
            }
            UiCommand::ClearPreset(slot) => {
                if slot < self.config.presets.len() {
                    self.config.presets[slot] = crate::config::Preset::default();
                    save_config(&self.config);
                    self.app_state.nrsc5_status =
                        format!("cleared preset {}", slot + 1);
                }
            }
            UiCommand::RecallPreset(slot) => {
                if let Some(preset) = self.config.presets.get(slot).cloned() {
                    self.app_state.frequency_mhz = preset.frequency_mhz;
                    self.app_state.selected_program = preset.program;
                    self.app_state.nrsc5_status = format!(
                        "preset {}: {:.1} HD{}",
                        slot + 1,
                        preset.frequency_mhz,
                        preset.program + 1
                    );
                    // If streaming, retune to the new station.
                    if self.app_state.is_streaming {
                        self.handle_command(UiCommand::TuneMhz(preset.frequency_mhz));
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
            UiCommand::SetGainMode(mode) => {
                if self.config.gain_mode == mode {
                    self.app_state.gain_mode = mode;
                    return;
                }
                self.config.gain_mode = mode;
                self.app_state.gain_mode = mode;
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
            }
            UiCommand::ExportLogCsv => {
                // Native Save-As dialog defaults to Documents with a
                // timestamped filename. User can redirect anywhere (e.g.
                // OneDrive, a USB stick, the portable bundle's own folder).
                let suggested_filename = crate::play_log::suggested_csv_filename();
                let start_dir = crate::paths::documents_dir()
                    .unwrap_or_else(|| std::path::PathBuf::from("."));
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
                        self.app_state.log_export_status =
                            Some(format!("export failed: {err}"));
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
                if !self.app_state.show_sdr_settings {
                    self.refresh_sdr_devices();
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
            UiCommand::RefreshSdrDevices => {
                self.refresh_sdr_devices();
            }
            UiCommand::SelectSdrDevice { driver, device_args } => {
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
            UiCommand::ResetSdrConfig => {
                self.config.sdr = crate::config::SdrConfigSection::default();
                save_config(&self.config);
                self.refresh_sdr_devices();
            }
        }
    }

    /// Re-enumerate SoapySDR devices and refresh the live gain-element
    /// list for the configured device. Called when the SDR Settings
    /// modal is first opened, when the user clicks "Refresh", and after
    /// any config change that affects which device is active.
    ///
    /// The enumeration runs synchronously on the UI thread — every
    /// supported SDR driver currently completes it in well under 50 ms.
    /// If that becomes a problem (e.g. when SoapyRemote arrives in 0.4.0
    /// and starts walking the LAN), this should move to a worker thread.
    fn refresh_sdr_devices(&mut self) {
        let (devices, diagnostics) =
            crate::sdr::SoapySdr::enumerate_devices_with_diagnostics();
        self.app_state.sdr_devices = devices;
        self.app_state.sdr_devices_last_refreshed = Some(Instant::now());

        // Best-effort write of the per-call diagnostic snapshot. When
        // a user reports "no devices detected" they (or we) can open
        // this file to see exactly what each module's enumerate pass
        // returned + which env vars were active. Failure to write the
        // file is not a reason to drop the device list.
        //
        // We **append** rather than truncate so that a subsequent
        // Refresh click does not wipe the post-`configure` blocks
        // the SDR backend writes on Start. Triage flow: user clicks
        // Refresh → enumeration block appended; user clicks Start
        // → configure marker + state block appended by the SDR
        // backend; user clicks Open diagnostics → sees the full
        // chronological log. Without the append change, Refresh
        // would silently erase the very lines we need.
        if let Some(path) = crate::paths::sdr_diagnostics_file() {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .and_then(|mut f| {
                    std::io::Write::write_all(&mut f, diagnostics.as_bytes())
                });
        }

        // For the gain elements we need a live device, not just an
        // enumeration entry. Open the configured device read-only just
        // long enough to query its element list, then drop it. Don't
        // touch a device that's already open for a running stream —
        // that would race with the I/Q pump.
        self.app_state.sdr_gain_elements = if self.app_state.is_streaming {
            // Streaming: ask the live SDR backend through the FFI
            // wrapper which already holds the device open.
            self.nrsc5
                .as_ref()
                .map(|n| n.sdr_gain_elements())
                .unwrap_or_default()
        } else {
            // Idle: try a quick open-and-close. Errors here just mean
            // the user's configured device isn't currently attached,
            // which the modal renders as an empty element list with a
            // "device not found" hint.
            let args = self.config.sdr.to_args_string();
            match crate::sdr::SoapySdr::open(&args) {
                Ok(sdr) => {
                    use crate::sdr::Sdr;
                    sdr.gain_elements()
                }
                Err(_) => Vec::new(),
            }
        };
    }

    /// Render the SDR Settings modal: device picker, per-element gain
    /// sliders for the active device, PPM correction, "Reset to
    /// defaults" / "Refresh" / "Close" buttons.
    ///
    /// The modal layers on top of the dock via egui's Window with
    /// `collapsible(false) + anchor center`. Closing dispatches a
    /// `HideSdrSettings` command rather than mutating state directly
    /// so the next-tick is consistent with other state changes.
    fn render_sdr_settings_modal(
        &mut self,
        ctx: &egui::Context,
        commands: &mut Vec<UiCommand>,
    ) {
        let mut open = true;
        // Snapshot config + devices into locals so the closure can
        // read them without holding a &self reference (which would
        // collide with the &mut self borrow needed for save_config
        // later if we tried to do it inline).
        let active_args = self.config.sdr.to_args_string();
        let active_driver = self.config.sdr.driver.clone();
        let current_ppm = self.config.sdr.freq_correction_ppm;
        let last_refreshed_label = self
            .app_state
            .sdr_devices_last_refreshed
            .map(|t| format!("refreshed {}s ago", t.elapsed().as_secs()))
            .unwrap_or_else(|| "not yet refreshed".to_string());

        egui::Window::new(egui::RichText::new("\u{1F4E1}  SDR Settings").size(18.0))
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_width(540.0)
            .default_height(560.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                // ---- Device picker -------------------------------------
                ui.heading("Device");
                ui.horizontal(|ui| {
                    ui.label("Active:");
                    ui.code(&active_args);
                });
                ui.add_space(4.0);

                if self.app_state.sdr_devices.is_empty() {
                    ui.colored_label(
                        egui::Color32::from_rgb(220, 160, 80),
                        "No SoapySDR devices detected. Check the dongle is \
                         plugged in (and Zadig-bound for RTL-SDR on Windows), \
                         then click Refresh.",
                    );
                } else {
                    egui::ScrollArea::vertical()
                        .id_source("sdr_devices_list")
                        .max_height(140.0)
                        .show(ui, |ui| {
                            for dev in &self.app_state.sdr_devices {
                                let is_active = dev.driver == active_driver
                                    && self.config.sdr.device_args == dev.args_after_driver();
                                let label = if dev.label.is_empty() {
                                    format!("[{}]  {}", dev.driver, dev.args_after_driver())
                                } else {
                                    format!("[{}]  {}", dev.driver, dev.label)
                                };
                                let resp = ui.selectable_label(is_active, label);
                                if resp.clicked() && !is_active {
                                    commands.push(UiCommand::SelectSdrDevice {
                                        driver: dev.driver.clone(),
                                        device_args: dev.args_after_driver(),
                                    });
                                }
                            }
                        });
                }

                ui.horizontal(|ui| {
                    if ui.button("\u{21BB}  Refresh").clicked() {
                        commands.push(UiCommand::RefreshSdrDevices);
                    }
                    ui.label(
                        egui::RichText::new(last_refreshed_label)
                            .small()
                            .color(egui::Color32::from_gray(140)),
                    );
                    // Reveal the diagnostic dump in Explorer so a user
                    // reporting "no devices detected" can quickly grab
                    // the file. Best-effort: silently no-op if the
                    // file hasn't been written yet (no Refresh clicked)
                    // or the OS shell hookup fails.
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
                            // `start` shells out to whatever default
                            // handler is registered for .txt — Notepad
                            // on a stock Windows install. Discard the
                            // command's output / errors; if it fails
                            // the user can just navigate to data\.
                            let _ = std::process::Command::new("cmd")
                                .args(["/C", "start", "", p.to_string_lossy().as_ref()])
                                .spawn();
                        }
                    }
                });

                ui.add_space(8.0);
                ui.separator();

                // ---- Profile notes -------------------------------------
                if let Some(profile) =
                    crate::sdr::profile::lookup(&active_driver)
                {
                    ui.heading(format!("{} ({})", profile.display_name, profile.driver));
                    if !profile.bench_validated {
                        ui.colored_label(
                            egui::Color32::from_rgb(220, 160, 80),
                            "\u{26A0}  This device profile is NOT bench-validated. \
                             AGC behavior may need tuning.",
                        );
                    }
                    ui.collapsing("HD Radio notes", |ui| {
                        ui.label(profile.hd_radio_notes);
                    });
                } else {
                    ui.colored_label(
                        egui::Color32::from_rgb(220, 160, 80),
                        format!(
                            "No device profile is configured for driver \"{}\". \
                             AGC will fall back to the rtlsdr profile; \
                             results may vary.",
                            active_driver
                        ),
                    );
                }

                ui.add_space(8.0);
                ui.separator();

                // ---- Per-element gain sliders --------------------------
                ui.heading("Manual gain");
                if self.app_state.sdr_gain_elements.is_empty() {
                    ui.colored_label(
                        egui::Color32::from_gray(140),
                        "No gain elements reported. Either the device isn't \
                         currently attached or the driver doesn't expose any.",
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
                    ui.add_space(2.0);
                    // Clone the element list out of state so the slider
                    // closure doesn't need to borrow it while we also
                    // need a mutable command vec.
                    let elements = self.app_state.sdr_gain_elements.clone();
                    for elem in &elements {
                        // Look up the override value from config; if
                        // there's no override yet, use the device's
                        // currently-reported value as the slider start.
                        let mut value = self
                            .config
                            .sdr
                            .gains
                            .get(&elem.name)
                            .copied()
                            .unwrap_or(elem.current_db);
                        // Step granularity: prefer device-reported step,
                        // fall back to 0.1 dB for "continuous" elements.
                        let step = if elem.step_db > 0.0 { elem.step_db } else { 0.1 };
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(format!("{:>6}", elem.name))
                                    .monospace(),
                            );
                            let resp = ui.add(
                                egui::Slider::new(&mut value, elem.min_db..=elem.max_db)
                                    .step_by(step)
                                    .suffix(" dB")
                                    .clamp_to_range(true),
                            );
                            if resp.drag_stopped() || resp.lost_focus() || resp.changed() {
                                // Coalesce: only emit when the snapped
                                // value differs from what we have in
                                // config (or no value yet).
                                let prev = self.config.sdr.gains.get(&elem.name).copied();
                                if prev.map_or(true, |p| (p - value).abs() > 1e-6) {
                                    commands.push(UiCommand::SetSdrGainElement {
                                        element: elem.name.clone(),
                                        value_db: value,
                                    });
                                }
                            }
                        });
                    }
                }

                ui.add_space(8.0);
                ui.separator();

                // ---- PPM correction -----------------------------------
                ui.heading("Frequency correction");
                let mut ppm = current_ppm;
                ui.horizontal(|ui| {
                    ui.label("PPM:");
                    let resp = ui.add(
                        egui::DragValue::new(&mut ppm)
                            .speed(0.1)
                            .range(-100.0..=100.0)
                            .suffix(" ppm"),
                    );
                    if (resp.drag_stopped() || resp.lost_focus())
                        && (ppm - current_ppm).abs() > 1e-6
                    {
                        commands.push(UiCommand::SetSdrFreqCorrectionPpm(ppm));
                    }
                });
                ui.label(
                    egui::RichText::new(
                        "RTL-SDR honors this immediately. SDRplay ignores it \
                         (uses its internal calibration); HackRF does too.",
                    )
                    .small()
                    .color(egui::Color32::from_gray(140)),
                );

                ui.add_space(12.0);
                ui.separator();

                // ---- Footer buttons -----------------------------------
                ui.horizontal(|ui| {
                    if ui.button("Reset to defaults").clicked() {
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
            });

        if !open {
            // User dismissed via the "X" on the window title bar.
            commands.push(UiCommand::HideSdrSettings);
        }
    }

    /// Render the About dialog. Shows version (from `CARGO_PKG_VERSION`
    /// — set by Cargo at build time), license, attribution, and a few
    /// quick-reference links to project URLs.
    fn render_about_dialog(
        &mut self,
        ctx: &egui::Context,
        commands: &mut Vec<UiCommand>,
    ) {
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
                            .color(egui::Color32::from_rgb(100, 160, 255)),
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
                        ui.hyperlink_to(
                            "github.com/emilk/egui",
                            "https://github.com/emilk/egui",
                        );
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
                ui.with_layout(
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| {
                        if ui.button("Close").clicked() {
                            commands.push(UiCommand::HideAbout);
                        }
                    },
                );
            });

        if !open {
            commands.push(UiCommand::HideAbout);
        }
    }
}

/// Derive the broadcaster call sign from an AAS LOT filename.
///
/// nrsc5 writes files as `{lot}_{name}` where `name` typically looks like
/// `KEGLHD01da41.jpg` (call sign + "HD" + 2-digit subchannel + hex). We
/// strip the leading lot prefix, find the "HD" marker, and return the
/// 3-5 uppercase ASCII letters preceding it.
fn extract_call_sign(filename: &str) -> Option<String> {
    // Strip the leading "{lot}_" prefix.
    let raw = filename.split_once('_').map(|(_, rest)| rest).unwrap_or(filename);

    // Find "HD" followed by an ASCII digit.
    let mut search_from = 0;
    loop {
        let hd_pos = raw[search_from..].find("HD")?;
        let abs = search_from + hd_pos;
        let after = &raw[abs + 2..];
        if after.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false) {
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

/// Default dock layout used on a fresh install (when no persisted state
/// exists). Captured live with the in-app "Dump Layout" helper at a
/// 1623×1179 inner window size, so the floating sub-windows assume roughly
/// that much room. Smaller windows still work — the user can drag any
/// sub-window back into place, and persistence saves their changes.
const DEFAULT_DOCK_RON: &str = r#"(
    surfaces: [Main((
        nodes: [],
        focused_node: Some((0)),
        collapsed: false,
        collapsed_leaf_count: 0,
)), Window((
        nodes: [Leaf((
            rect: (
                min: (
                    x: 305.0,
                    y: 55.0,
                ),
                max: (
                    x: 626.875,
                    y: 468.09375,
                ),
            ),
            viewport: (
                min: (
                    x: 305.0,
                    y: 79.0,
                ),
                max: (
                    x: 626.875,
                    y: 468.09375,
                ),
            ),
            tabs: [NowPlaying],
            active: (0),
            scroll: 0.0,
            collapsed: false,
        ))],
        focused_node: None,
        collapsed: false,
        collapsed_leaf_count: 0,
    ), (
        screen_rect: None,
        dragged: false,
        next_position: None,
        next_size: None,
        expanded_height: None,
        new: false,
        minimized: false,
    )), Window((
        nodes: [Leaf((
            rect: (
                min: (
                    x: 652.34375,
                    y: 54.34375,
                ),
                max: (
                    x: 1594.9688,
                    y: 466.625,
                ),
            ),
            viewport: (
                min: (
                    x: 652.34375,
                    y: 78.34375,
                ),
                max: (
                    x: 1594.9688,
                    y: 466.625,
                ),
            ),
            tabs: [Collage],
            active: (0),
            scroll: 0.0,
            collapsed: false,
        ))],
        focused_node: None,
        collapsed: false,
        collapsed_leaf_count: 0,
    ), (
        screen_rect: None,
        dragged: false,
        next_position: None,
        next_size: None,
        expanded_height: None,
        new: false,
        minimized: false,
    )), Window((
        nodes: [Vertical((
            rect: (
                min: (
                    x: 24.34375,
                    y: 55.0,
                ),
                max: (
                    x: 281.53125,
                    y: 554.75,
                ),
            ),
            fraction: 0.61205405,
            fully_collapsed: false,
            collapsed_leaf_count: 0,
        )), Leaf((
            rect: (
                min: (
                    x: 24.34375,
                    y: 55.0,
                ),
                max: (
                    x: 281.53125,
                    y: 360.66666,
                ),
            ),
            viewport: (
                min: (
                    x: 24.34375,
                    y: 79.0,
                ),
                max: (
                    x: 281.53125,
                    y: 360.65625,
                ),
            ),
            tabs: [Tuner],
            active: (0),
            scroll: 0.0,
            collapsed: false,
        )), Leaf((
            rect: (
                min: (
                    x: 24.34375,
                    y: 361.33334,
                ),
                max: (
                    x: 281.53125,
                    y: 554.75,
                ),
            ),
            viewport: (
                min: (
                    x: 24.34375,
                    y: 385.34375,
                ),
                max: (
                    x: 281.53125,
                    y: 554.75,
                ),
            ),
            tabs: [Signal],
            active: (0),
            scroll: 0.0,
            collapsed: false,
        ))],
        focused_node: Some((1)),
        collapsed: false,
        collapsed_leaf_count: 0,
    ), (
        screen_rect: None,
        dragged: false,
        next_position: None,
        next_size: None,
        expanded_height: None,
        new: false,
        minimized: false,
    )), Window((
        nodes: [Horizontal((
            rect: (
                min: (
                    x: 305.0,
                    y: 493.0,
                ),
                max: (
                    x: 1595.4063,
                    y: 1139.4063,
                ),
            ),
            fraction: 0.5,
            fully_collapsed: false,
            collapsed_leaf_count: 0,
        )), Leaf((
            rect: (
                min: (
                    x: 305.0,
                    y: 493.0,
                ),
                max: (
                    x: 950.0,
                    y: 1139.4063,
                ),
            ),
            viewport: (
                min: (
                    x: 305.0,
                    y: 517.0,
                ),
                max: (
                    x: 950.0,
                    y: 1139.4063,
                ),
            ),
            tabs: [Traffic],
            active: (0),
            scroll: 0.0,
            collapsed: false,
        )), Leaf((
            rect: (
                min: (
                    x: 950.6667,
                    y: 493.0,
                ),
                max: (
                    x: 1595.4063,
                    y: 1139.4063,
                ),
            ),
            viewport: (
                min: (
                    x: 950.65625,
                    y: 517.0,
                ),
                max: (
                    x: 1595.4063,
                    y: 1139.4063,
                ),
            ),
            tabs: [Weather],
            active: (0),
            scroll: 0.0,
            collapsed: false,
        ))],
        focused_node: Some((2)),
        collapsed: false,
        collapsed_leaf_count: 0,
    ), (
        screen_rect: None,
        dragged: false,
        next_position: None,
        next_size: None,
        expanded_height: None,
        new: false,
        minimized: false,
    ))],
    focused_surface: Some((4)),
    translations: (
        tab_context_menu: (
            close_button: "Close",
            eject_button: "Eject",
        ),
        leaf: (
            close_button_disabled_tooltip: "This leaf contains non-closable tabs.",
            close_all_button: "Close window",
            close_all_button_menu_hint: "Right click to close this window.",
            close_all_button_modifier_hint: "Press modifier keys (Shift by default) to close this window.",
            close_all_button_modifier_menu_hint: "Press modifier keys (Shift by default) or right click to close this window.",
            close_all_button_disabled_tooltip: "This window contains non-closable tabs.",
            minimize_button: "Minimize window",
            minimize_button_menu_hint: "Right click to minimize this window.",
            minimize_button_modifier_hint: "Press modifier keys (Shift by default) to minimize this window.",
            minimize_button_modifier_menu_hint: "Press modifier keys (Shift by default) or right click to minimize this window.",
        ),
    ),
)"#;

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
            oldest_play = Some(
                oldest_play
                    .map(|cur| cur.min(*front))
                    .unwrap_or(*front),
            );
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
    let mut entries: Vec<(&ArtEntry, Instant)> = map
        .values()
        .filter_map(|e| e.plays.front().map(|t| (e, *t)))
        .collect();
    entries.sort_by(|a, b| b.0.plays.len().cmp(&a.0.plays.len()));
    entries.truncate(cap);
    entries.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.path.cmp(&b.0.path)));
    tiles = entries
        .into_iter()
        .map(|(e, _)| ArtTile {
            path: e.path.clone(),
            count: e.plays.len() as u32,
            songs: e.songs.clone(),
            album: e.album.clone(),
        })
        .collect();

    (map, tiles, oldest_play)
}

/// Build the initial dock layout. Tries the captured RON first; falls back
/// to a hand-built docked layout if parsing fails (e.g. after a future
/// egui_dock version bump changes the serialization shape).
fn default_dock_state() -> DockState<DockTab> {
    if let Ok(ds) = ron::from_str::<DockState<DockTab>>(DEFAULT_DOCK_RON) {
        return ds;
    }

    eprintln!(
        "warning: failed to parse embedded default dock layout; using built-in fallback"
    );
    let mut ds = DockState::new(vec![DockTab::NowPlaying]);
    let tree = ds.main_surface_mut();
    let [_old, left] = tree.split_left(NodeIndex::root(), 0.25, vec![DockTab::Tuner]);
    // Signal + Constellation share a leaf so the user can flip between the
    // numeric MER/BER readout and the visual scope without rearranging.
    let [_old_left, _left_bottom] =
        tree.split_below(left, 0.35, vec![DockTab::Signal, DockTab::Constellation]);
    let [_old_root, right] =
        tree.split_right(NodeIndex::root(), 0.32, vec![DockTab::Traffic]);
    let [_old_right, _right_bottom] = tree.split_below(right, 0.5, vec![DockTab::Weather]);
    let [_old_center, _center_bottom] =
        tree.split_below(NodeIndex::root(), 0.67, vec![DockTab::Collage]);
    ds
}