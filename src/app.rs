use crate::collage::CollageEngine;
use crate::config::{load_config, save_config, AppConfig};
use crate::ffi::{Nrsc5Process, NrscEvent};
use crate::gui::dock::{DockTab, DockViewer, UiCommand};
use crate::gui::state::{AppState, ArtTile};
use crate::maps::{TrafficMap, WeatherMap};
use egui_dock::{DockArea, DockState, NodeIndex, NodePath, SurfaceIndex};
use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Hard cap on tracked album-art tiles — prevents the collage from getting
/// unbounded if a session runs all day on a very busy station.
const MAX_ART_TILES: usize = 64;
/// Rolling window for the album-art collage. Plays older than this are
/// pruned on every new event so the heat-map keeps moving instead of
/// freezing.
const ART_WINDOW: Duration = Duration::from_secs(8 * 60 * 60);

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
    /// Last cover-art path we counted, used to debounce repeated XHDR
    /// emissions while the same song is playing.
    last_counted_art_path: Option<String>,
    /// Last time we pruned expired plays from `art_history`. Throttled so
    /// we don't walk the map every UI frame.
    last_art_prune_at: Option<Instant>,
    /// Where each recently-closed panel used to live, so re-opening it from
    /// the toolbar restores it to (roughly) the same spot.
    closed_tab_locations: HashMap<DockTab, ClosedTabInfo>,
    /// Layout snapshot from the previous frame, used to detect tabs that
    /// were closed via the dock area's own "X" button.
    prev_layout: LayoutSnapshot,
}

impl Nrsc5App {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        egui_extras::install_image_loaders(&_cc.egui_ctx);
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

        let aas_dir = nrsc5
            .as_ref()
            .map(|n| n.aas_dir().to_path_buf())
            .unwrap_or_else(|| std::env::temp_dir().join("nrsc5-tui-aas"));

        Self {
            app_state: AppState {
                frequency_mhz: config.frequency_mhz,
                selected_program: config.selected_program,
                dark_mode: config.dark_mode,
                station_name: format!("HD{}", config.selected_program + 1),
                nrsc5_status,
                volume: config.volume.clamp(0.0, 1.0),
                muted: config.muted,
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
            art_history: HashMap::new(),
            last_counted_art_path: None,
            last_art_prune_at: None,
            closed_tab_locations: HashMap::new(),
            prev_layout: LayoutSnapshot::default(),
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

        ui.horizontal(|ui| {
            let theme_icon = if self.app_state.dark_mode { "☀" } else { "🌙" };
            if ui
                .button(egui::RichText::new(theme_icon).size(16.0))
                .on_hover_text("Toggle light/dark theme")
                .clicked()
            {
                self.app_state.dark_mode = !self.app_state.dark_mode;
                Self::apply_theme(ui.ctx(), self.app_state.dark_mode);
            }
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
        });
        ui.separator();

        let mut commands = Vec::new();
        let mut viewer = DockViewer {
            app_state: &mut self.app_state,
            commands: &mut commands,
            presets: &self.config.presets,
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

        // Keep the rolling 8-hour collage window honest even in quiet periods
        // by pruning expired plays roughly once a minute.
        self.maybe_prune_art_history();
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, "dock_state", &self.dock_state);
    }

    fn on_exit(&mut self) {
        if let Some(mut nrsc5) = self.nrsc5.take() {
            nrsc5.stop();
        }

        self.config.frequency_mhz = self.app_state.frequency_mhz;
        self.config.selected_program = self.app_state.selected_program;
        self.config.dark_mode = self.app_state.dark_mode;
        self.config.volume = self.app_state.volume;
        self.config.muted = self.app_state.muted;
        save_config(&self.config);
    }
}

impl Nrsc5App {
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

    fn apply_theme(ctx: &egui::Context, dark: bool) {
        let accent = egui::Color32::from_rgb(100, 160, 255);

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

        ctx.set_visuals(visuals);

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

    /// Update the album-art heat-map histogram with a newly-displayed cover.
    /// Dedupes by content hash so the same image transmitted under different
    /// LOT IDs still counts as one tile, and debounces same-path emissions
    /// while a song is still playing.
    fn record_album_art(&mut self, full_path: &std::path::Path, path_str: &str) {
        // Debounce: only count when the displayed art *transitions* to a new
        // path. Repeated XHDR pings for the same song should not inflate the
        // count.
        if self.last_counted_art_path.as_deref() == Some(path_str) {
            return;
        }
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
            return;
        };
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        bytes.hash(&mut hasher);
        let key = hasher.finish();

        // Grab the song metadata currently on display so we can label this
        // cover later in tooltips. Trim and skip empty pieces so we don't
        // accumulate noise entries like ("", "").
        let title = self.app_state.title.trim().to_string();
        let artist = self.app_state.artist.trim().to_string();
        let album = self.app_state.album.trim().to_string();

        let entry = self.art_history.entry(key).or_insert_with(|| ArtEntry {
            path: path_str.to_string(),
            plays: VecDeque::new(),
            songs: Vec::new(),
            album: album.clone(),
        });
        entry.plays.push_back(now);
        // Always refresh path — a re-emitted image may live at a new LOT path.
        entry.path = path_str.to_string();
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
        self.last_counted_art_path = Some(path_str.to_string());
        self.rebuild_art_tiles();
    }

    /// Rebuild the AppState's sorted tile list from `art_history`.
    fn rebuild_art_tiles(&mut self) {
        let mut tiles: Vec<ArtTile> = self
            .art_history
            .values()
            .map(|e| ArtTile {
                path: e.path.clone(),
                count: e.plays.len() as u32,
                songs: e.songs.clone(),
                album: e.album.clone(),
            })
            .collect();
        // Sort by count desc, then by path for stable layout.
        tiles.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.path.cmp(&b.path)));
        tiles.truncate(MAX_ART_TILES);
        self.app_state.art_tiles = tiles;
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
        }
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
                self.app_state.nrsc5_status = "synced".to_string();
            }
            NrscEvent::LostSync => {
                self.app_state.nrsc5_status = "sync lost".to_string();
            }
            NrscEvent::Mer { lower, upper } => {
                self.app_state.mer = (lower + upper) / 2.0;
            }
            NrscEvent::Ber { cber } => {
                self.app_state.ber = cber;
            }
            NrscEvent::Agc { gain_db } => {
                self.app_state.agc_db = gain_db;
                self.app_state.nrsc5_status = format!("best gain: {:.1} dB", gain_db);
            }
            NrscEvent::AudioStarted { .. } => {
                self.last_signal_at = Some(Instant::now());
                self.app_state.active_program = self.app_state.selected_program;
                self.app_state.station_name =
                    format!("HD{}", self.app_state.selected_program + 1);

                if let Some(started) = self.start_requested_at.take() {
                    self.app_state.nrsc5_status = format!(
                        "audio started on HD{} in {:.1}s",
                        self.app_state.selected_program + 1,
                        started.elapsed().as_secs_f32()
                    );
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

                if !title.is_empty() {
                    self.app_state.title = title;
                }
                if !artist.is_empty() {
                    self.app_state.artist = artist;
                }
                if !album.is_empty() {
                    self.app_state.album = album;
                }
                if !genre.is_empty() {
                    self.app_state.genre = genre;
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
                            // Cover art
                            self.app_state.cover_art_path = Some(path_str.clone());
                            self.record_album_art(&full_path, &path_str);
                        } else if param == 1 {
                            // Station logo
                            self.app_state.station_logo_path = Some(path_str);
                        }
                    }
                }
            }
            NrscEvent::StationName(name) => {
                self.app_state.station_name = name;
            }
            NrscEvent::SigServiceAudio { number, name } => {
                // `number` is 1-indexed; store under the matching program slot.
                if number >= 1 && number <= 4 {
                    let idx = (number - 1) as usize;
                    self.app_state.short_names[idx] = name;
                }
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
                        Ok(p) => self.nrsc5 = Some(p),
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

                let result = if self.config.use_rtl_tcp {
                    nrsc5.start_rtltcp(
                        mhz,
                        program,
                        &self.config.rtl_tcp_host,
                        self.config.rtl_tcp_port,
                    )
                } else {
                    nrsc5.start(mhz, program, self.config.rtl_device_index)
                };

                if let Err(err) = result {
                    self.app_state.nrsc5_status = format!("start failed: {err}");
                    return;
                }

                self.app_state.is_streaming = true;
                self.start_requested_at = Some(Instant::now());
                self.last_signal_at = None;
                // A fresh Start (after Stop) resets the 8-hour collage horizon
                // and clears any accumulated tiles.
                self.art_history.clear();
                self.last_counted_art_path = None;
                self.app_state.art_tiles.clear();
                self.app_state.art_session_started = Some(Instant::now());
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
                self.lot_files.clear();
                self.app_state.cover_art_path = None;
                self.app_state.station_logo_path = None;
                self.app_state.traffic_map_path = None;
                self.app_state.weather_frames.clear();
                self.app_state.weather_current_frame = 0;
                self.app_state.weather_playing = false;
                self.traffic_map.clear();
                self.weather_map.clear();
                self.app_state.nrsc5_status = "stream stopped".to_string();
            }
            UiCommand::TuneMhz(mhz) => {
                self.app_state.frequency_mhz = mhz;
                self.app_state.station_name =
                    format!("HD{}", self.app_state.selected_program + 1);

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
                self.app_state.station_name = format!("HD{}", clamped + 1);

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
                let short = self
                    .app_state
                    .short_names
                    .get(self.app_state.selected_program as usize)
                    .cloned()
                    .unwrap_or_default();
                let name = if !short.is_empty() {
                    short
                } else if !self.app_state.artist.is_empty() {
                    self.app_state.artist.clone()
                } else {
                    self.app_state.station_name.clone()
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
                    self.app_state.station_name =
                        format!("HD{}", preset.program + 1);
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
    let [_old_left, _left_bottom] =
        tree.split_below(left, 0.35, vec![DockTab::Signal]);
    let [_old_root, right] =
        tree.split_right(NodeIndex::root(), 0.32, vec![DockTab::Traffic]);
    let [_old_right, _right_bottom] = tree.split_below(right, 0.5, vec![DockTab::Weather]);
    let [_old_center, _center_bottom] =
        tree.split_below(NodeIndex::root(), 0.67, vec![DockTab::Collage]);
    ds
}