use crate::config::{GainMode, Preset};
use crate::gui::state::{AppState, LogViewMode};
use crate::play_log::PlayLog;
use egui::{Color32, DragValue, RichText, Ui, Vec2, WidgetText};
use egui_dock::TabViewer;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone)]
pub enum UiCommand {
    Start,
    Stop,
    TuneMhz(f32),
    SelectProgram(u32),
    SavePreset(usize),
    RecallPreset(usize),
    /// Commit a full preset edit (name, frequency, subchannel) for a slot.
    SetPreset(usize, Preset),
    /// Clear/forget the preset at the given slot.
    ClearPreset(usize),
    /// Set the per-process audio output volume (0.0..=1.0).
    SetVolume(f32),
    /// Toggle / set mute state for the per-process audio session.
    SetMute(bool),
    /// Set the maximum number of album-art tiles shown in the Collage tab.
    /// Snapped server-side to a power of two in [1, 512].
    SetCollageTileCap(u32),
    /// Write the current play log to a CSV file. App resolves the path and
    /// surfaces it through `AppState::log_export_status`.
    ExportLogCsv,
    /// Drop every entry from the in-memory play log and persist the
    /// empty state to disk.
    ClearLog,
    /// Update the rolling-window retention (in hours) for the play log
    /// and persist the new value to config. Value is clamped to the
    /// supported range on apply.
    SetPlayLogRetention(u32),
    /// Switch tuner gain control mode (Auto / Manual / HardwareAgc).
    /// Persisted to config; takes effect on the next piped Start.
    SetGainMode(GainMode),
    /// Set the manual tuner gain in tenths of dB. Snapped to the nearest
    /// R820T2 step at apply time. Persisted to config; takes effect on
    /// the next piped Start.
    SetManualGainTenths(i32),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DockTab {
    Tuner,
    NowPlaying,
    Traffic,
    Weather,
    Signal,
    Collage,
    /// QPSK constellation "scope" — animated scatter of synthesized symbol
    /// samples whose tightness is driven by per-sideband MER from nrsc5.
    Constellation,
    /// 24-hour rolling song log. Closed by default; opened from the panel
    /// toolbar.
    Log,
    /// SDR# / Gqrx-style spectrum + waterfall. Fed by the piped-SDR
    /// FFT tap; renders idle when the tap has no data (e.g. before the
    /// first Start, or in legacy USB / rtl_tcp modes).
    Spectrum,
}

impl DockTab {
    /// All panel variants in the order they should appear in the View menu.
    pub const ALL: [DockTab; 9] = [
        DockTab::Tuner,
        DockTab::NowPlaying,
        DockTab::Collage,
        DockTab::Spectrum,
        DockTab::Signal,
        DockTab::Constellation,
        DockTab::Traffic,
        DockTab::Weather,
        DockTab::Log,
    ];

    /// Compact label (emoji + short name) for the top-toolbar tab toggles.
    pub fn toolbar_label(&self) -> &'static str {
        match self {
            DockTab::Tuner => "\u{1F4FB} Tuner",
            DockTab::NowPlaying => "\u{1F3B5} Now Playing",
            DockTab::Collage => "\u{1F5BC} Collage",
            DockTab::Spectrum => "\u{1F4CA} Spectrum",
            DockTab::Signal => "\u{1F4F6} Signal",
            DockTab::Constellation => "\u{1F30C} Constellation",
            DockTab::Traffic => "\u{1F697} Traffic",
            DockTab::Weather => "\u{2601} Weather",
            DockTab::Log => "\u{1F4DD} Log",
        }
    }
}

pub struct DockViewer<'a> {
    pub app_state: &'a mut AppState,
    pub commands: &'a mut Vec<UiCommand>,
    pub presets: &'a [Preset],
    pub play_log: &'a PlayLog,
}

impl TabViewer for DockViewer<'_> {
    type Tab = DockTab;

    fn title(&mut self, tab: &mut Self::Tab) -> WidgetText {
        match tab {
            DockTab::Tuner => "\u{1F4FB} Tuner".into(),
            DockTab::NowPlaying => "\u{1F3B5} Now Playing".into(),
            DockTab::Traffic => "\u{1F697} Traffic".into(),
            DockTab::Weather => "\u{2601} Weather".into(),
            DockTab::Signal => "\u{1F4F6} Signal".into(),
            DockTab::Collage => "\u{1F5BC} Collage".into(),
            DockTab::Constellation => "\u{1F30C} Constellation".into(),
            DockTab::Log => "\u{1F4DD} Log".into(),
            DockTab::Spectrum => "\u{1F4CA} Spectrum".into(),
        }
    }

    fn ui(&mut self, ui: &mut Ui, tab: &mut Self::Tab) {
        match tab {
            DockTab::Tuner => self.tuner_ui(ui),
            DockTab::NowPlaying => self.now_playing_ui(ui),
            DockTab::Traffic => self.traffic_ui(ui),
            DockTab::Weather => self.weather_ui(ui),
            DockTab::Signal => self.signal_ui(ui),
            DockTab::Collage => self.collage_ui(ui),
            DockTab::Constellation => self.constellation_ui(ui),
            DockTab::Log => self.log_ui(ui),
            DockTab::Spectrum => self.spectrum_ui(ui),
        }
    }
}

impl DockViewer<'_> {
    fn tuner_ui(&mut self, ui: &mut Ui) {
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Frequency").strong());
            ui.add(
                DragValue::new(&mut self.app_state.frequency_mhz)
                    .speed(0.1)
                    .suffix(" MHz")
                    .range(87.5..=108.0),
            );
            if ui.button("Tune").clicked() {
                self.commands
                    .push(UiCommand::TuneMhz(self.app_state.frequency_mhz));
            }
        });
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Program").strong());

            let mut selected = self.app_state.selected_program.min(3);
            let mut changed = false;
            for i in 0..4u32 {
                let label = format!("HD{}", i + 1);
                if ui.selectable_value(&mut selected, i, label).changed() {
                    changed = true;
                }
            }

            if changed {
                self.commands.push(UiCommand::SelectProgram(selected));
            }
        });
        ui.add_space(4.0);
        ui.separator();
        ui.add_space(2.0);

        // Volume slider + mute toggle. Both are disabled until the audio
        // session for the nrsc5 child has been discovered.
        ui.horizontal(|ui| {
            let mute_icon = if self.app_state.muted { "🔇" } else { "🔊" };
            let mute_btn = ui
                .add_enabled(
                    self.app_state.audio_session_ready,
                    egui::Button::new(RichText::new(mute_icon).size(14.0)),
                )
                .on_hover_text("Toggle mute");
            if mute_btn.clicked() {
                self.commands
                    .push(UiCommand::SetMute(!self.app_state.muted));
            }

            // Slider works in 0..=100 for display, mapped to 0.0..=1.0 internally.
            let mut percent = (self.app_state.volume * 100.0).round() as i32;
            let slider_resp = ui.add_enabled(
                self.app_state.audio_session_ready,
                egui::Slider::new(&mut percent, 0..=100)
                    .suffix("%")
                    .show_value(true),
            );
            if slider_resp.changed() {
                let new_vol = (percent as f32 / 100.0).clamp(0.0, 1.0);
                self.commands.push(UiCommand::SetVolume(new_vol));
            }
        });
        if !self.app_state.audio_session_ready {
            ui.label(
                RichText::new("(volume available once audio is playing)")
                    .small()
                    .color(Color32::from_gray(120)),
            );
        }
        ui.add_space(2.0);
        ui.separator();
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            // Solid-colored Start/Stop buttons. The fills are dark enough to
            // read well against both light- and dark-theme backgrounds, and
            // the text uses a near-white grey that contrasts with both fills.
            let btn_text = Color32::from_gray(230);
            let start_fill = Color32::from_rgb(34, 139, 72); // forest green
            let stop_fill = Color32::from_rgb(176, 48, 48); // brick red

            let start_btn = ui.add_sized(
                [64.0, 26.0],
                egui::Button::new(
                    RichText::new("▶ Start").color(btn_text).strong(),
                )
                .fill(start_fill),
            );
            if start_btn.clicked() {
                self.commands.push(UiCommand::Start);
            }

            let stop_btn = ui.add_sized(
                [64.0, 26.0],
                egui::Button::new(
                    RichText::new("■ Stop").color(btn_text).strong(),
                )
                .fill(stop_fill),
            );
            if stop_btn.clicked() {
                self.commands.push(UiCommand::Stop);
            }
        });

        // Preset buttons
        ui.add_space(4.0);
        ui.separator();
        ui.add_space(2.0);
        ui.label(RichText::new("Presets").strong().small());
        ui.horizontal_wrapped(|ui| {
            let accent = Color32::from_rgb(100, 160, 255);
            let dim = Color32::from_gray(120);
            for i in 0..6 {
                let preset = self.presets.get(i);
                let label = if let Some(p) = preset {
                    if p.name.is_empty() {
                        format!("{:.1}", p.frequency_mhz)
                    } else {
                        p.name.clone()
                    }
                } else {
                    "—".to_string()
                };

                let is_populated = preset.is_some();

                let btn_text = if is_populated {
                    RichText::new(&label).small().color(accent)
                } else {
                    RichText::new(&label).small().color(dim)
                };

                let btn = ui.add_sized([72.0, 22.0], egui::Button::new(btn_text));

                if btn.clicked() && is_populated {
                    self.commands.push(UiCommand::RecallPreset(i));
                }
                if btn.secondary_clicked() {
                    self.commands.push(UiCommand::SavePreset(i));
                }
                if btn.double_clicked() {
                    // Pre-fill the popup with either the existing preset
                    // values, or sensible defaults (the current tuner state)
                    // for an empty slot.
                    let (init_name, init_freq, init_prog) = match preset {
                        Some(p) => (p.name.clone(), p.frequency_mhz, p.program),
                        None => (
                            String::new(),
                            self.app_state.frequency_mhz,
                            self.app_state.selected_program,
                        ),
                    };
                    self.app_state.editing_preset = Some(i);
                    self.app_state.editing_preset_text = init_name;
                    self.app_state.editing_preset_freq = init_freq;
                    self.app_state.editing_preset_program = init_prog;
                    self.app_state.editing_preset_just_opened = true;
                }
            }
        });
        ui.label(
            RichText::new(
                "Click to tune · Right-click to save · Double-click to edit",
            )
            .small()
            .color(Color32::from_gray(100)),
        );

        // Floating preset editor — modal-ish window with name/freq/subchannel
        // fields plus Save / Clear / Cancel actions. Rendered here (rather
        // than at the dock root) so it only appears while the Tuner tab is
        // visible, which is where it makes contextual sense.
        if let Some(slot) = self.app_state.editing_preset {
            let mut keep_open = true;
            let title = format!("Edit Preset {}", slot + 1);
            egui::Window::new(title)
                .open(&mut keep_open)
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ui.ctx(), |ui| {
                    ui.add_space(4.0);
                    egui::Grid::new(format!("preset-edit-grid-{slot}"))
                        .num_columns(2)
                        .spacing([10.0, 8.0])
                        .show(ui, |ui| {
                            ui.label("Name");
                            let name_resp = ui.add_sized(
                                [180.0, 22.0],
                                egui::TextEdit::singleline(
                                    &mut self.app_state.editing_preset_text,
                                ),
                            );
                            if self.app_state.editing_preset_just_opened {
                                name_resp.request_focus();
                                self.app_state.editing_preset_just_opened = false;
                            }
                            ui.end_row();

                            ui.label("Frequency");
                            ui.add(
                                egui::DragValue::new(
                                    &mut self.app_state.editing_preset_freq,
                                )
                                .speed(0.1)
                                .range(87.5..=108.0)
                                .suffix(" MHz"),
                            );
                            ui.end_row();

                            ui.label("Subchannel");
                            ui.horizontal(|ui| {
                                for sub in 0..4u32 {
                                    ui.selectable_value(
                                        &mut self.app_state.editing_preset_program,
                                        sub,
                                        format!("HD{}", sub + 1),
                                    );
                                }
                            });
                            ui.end_row();
                        });
                    ui.add_space(8.0);
                    ui.separator();
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new("Save")
                                        .color(Color32::from_rgb(80, 220, 120)),
                                )
                                .min_size(Vec2::new(70.0, 24.0)),
                            )
                            .clicked()
                        {
                            let preset = Preset {
                                name: self
                                    .app_state
                                    .editing_preset_text
                                    .trim()
                                    .to_string(),
                                frequency_mhz: self.app_state.editing_preset_freq,
                                program: self.app_state.editing_preset_program,
                            };
                            self.commands.push(UiCommand::SetPreset(slot, preset));
                            self.app_state.editing_preset = None;
                        }
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new("Clear")
                                        .color(Color32::from_rgb(240, 80, 80)),
                                )
                                .min_size(Vec2::new(70.0, 24.0)),
                            )
                            .on_hover_text("Forget this preset slot")
                            .clicked()
                        {
                            self.commands.push(UiCommand::ClearPreset(slot));
                            self.app_state.editing_preset = None;
                        }
                        if ui
                            .add(
                                egui::Button::new("Cancel")
                                    .min_size(Vec2::new(70.0, 24.0)),
                            )
                            .clicked()
                        {
                            self.app_state.editing_preset = None;
                        }
                    });
                });
            // X-button closed the window — clear edit state.
            if !keep_open {
                self.app_state.editing_preset = None;
            }
            // Escape also closes.
            if self.app_state.editing_preset.is_some()
                && ui.input(|i| i.key_pressed(egui::Key::Escape))
            {
                self.app_state.editing_preset = None;
            }
        }
    }

    fn now_playing_ui(&mut self, ui: &mut Ui) {
        let accent = Color32::from_rgb(100, 160, 255);
        let dim = Color32::from_gray(160);
        let muted = Color32::from_gray(120);

        // Line 1: Artist (long station name OR song artist — changes with broadcast).
        if !self.app_state.artist.is_empty() {
            ui.label(
                RichText::new(&self.app_state.artist)
                    .heading()
                    .color(accent),
            );
        }

        // Line 2: Title (slogan OR song title).
        if !self.app_state.title.is_empty() {
            ui.label(
                RichText::new(&self.app_state.title)
                    .size(15.0)
                    .color(dim),
            );
        }

        // Line 3: Derived station identity — "KEGL 97.1 HD2".
        let hd = self.app_state.selected_program + 1;
        let identity = if !self.app_state.call_sign.is_empty() {
            format!(
                "{} {:.1} HD{}",
                self.app_state.call_sign, self.app_state.frequency_mhz, hd
            )
        } else {
            format!("{:.1} HD{}", self.app_state.frequency_mhz, hd)
        };
        ui.label(
            RichText::new(&identity)
                .monospace()
                .small()
                .color(muted),
        );
        ui.add_space(6.0);

        // Album art
        if let Some(ref path) = self.app_state.cover_art_path {
            let uri = format!("file:///{}", path.replace('\\', "/"));
            let available = ui.available_size();
            let max_side = available.x.min(available.y).min(300.0);
            ui.add(
                egui::Image::new(&uri)
                    .fit_to_exact_size(Vec2::new(max_side, max_side))
                    .corner_radius(6),
            );
        } else {
            ui.label(RichText::new("Waiting for album art...").color(dim));
        }
    }

    fn traffic_ui(&mut self, ui: &mut Ui) {
        let dim = Color32::from_gray(120);
        if let Some(ref path) = self.app_state.traffic_map_path {
            let uri = format!("file:///{}", path.replace('\\', "/"));
            let available = ui.available_size();
            let max_side = available.x.min(available.y).min(600.0);
            ui.vertical_centered(|ui| {
                ui.add(
                    egui::Image::new(&uri)
                        .fit_to_exact_size(Vec2::new(max_side, max_side))
                        .corner_radius(4),
                );
            });
        } else {
            ui.centered_and_justified(|ui| {
                ui.label(
                    RichText::new("Waiting for traffic map tiles...")
                        .color(dim)
                        .italics(),
                );
            });
        }
    }

    fn weather_ui(&mut self, ui: &mut Ui) {
        let dim = Color32::from_gray(120);
        let frame_count = self.app_state.weather_frames.len();
        if frame_count == 0 {
            ui.centered_and_justified(|ui| {
                ui.label(
                    RichText::new("Waiting for weather radar overlay...")
                        .color(dim)
                        .italics(),
                );
            });
            return;
        }

        // Auto-advance every ~500ms while playing.
        if self.app_state.weather_playing && frame_count > 1 {
            let now = std::time::Instant::now();
            let due = self
                .app_state
                .weather_last_advance
                .map(|t| now.duration_since(t) >= std::time::Duration::from_millis(500))
                .unwrap_or(true);
            if due {
                let next = (self.app_state.weather_current_frame + 1) % frame_count;
                self.app_state.weather_current_frame = next;
                self.app_state.weather_last_advance = Some(now);
            }
            // Keep the UI refreshing while the animation runs.
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(50));
        }

        // Clamp current frame in case the buffer just shrank.
        if self.app_state.weather_current_frame >= frame_count {
            self.app_state.weather_current_frame = frame_count - 1;
        }

        let cur = self.app_state.weather_current_frame;
        let frame = &self.app_state.weather_frames[cur];
        let path = frame.path.clone();
        let timestamp = frame.captured_at.format("%H:%M").to_string();
        let uri = format!("file:///{}", path.replace('\\', "/"));
        let available = ui.available_size();
        let max_side = available.x.min(available.y).min(600.0).max(120.0);

        ui.vertical_centered(|ui| {
            // Allocate the image square. The transport controls are painted on
            // top of the bottom strip as an overlay.
            let (img_rect, _resp) = ui.allocate_exact_size(
                Vec2::new(max_side, max_side),
                egui::Sense::hover(),
            );
            egui::Image::new(&uri)
                .corner_radius(4)
                .paint_at(ui, img_rect);

            // Translucent dark strip along the bottom of the image, rounded
            // only on the bottom corners so it tucks under the image frame.
            let strip_h = 36.0;
            let strip = egui::Rect::from_min_max(
                egui::pos2(img_rect.min.x, img_rect.max.y - strip_h),
                img_rect.max,
            );
            let painter = ui.painter_at(img_rect);
            painter.rect_filled(
                strip,
                egui::CornerRadius {
                    nw: 0,
                    ne: 0,
                    sw: 4,
                    se: 4,
                },
                Color32::from_rgba_unmultiplied(0, 0, 0, 170),
            );

            // Place the transport widgets inside the strip using a child UI.
            let inner = strip.shrink2(egui::vec2(8.0, 4.0));
            let mut child = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(inner)
                    .layout(egui::Layout::left_to_right(egui::Align::Center)),
            );
            child.style_mut().visuals.override_text_color = Some(Color32::WHITE);
            child.spacing_mut().item_spacing.x = 8.0;

            let (label, hover) = if self.app_state.weather_playing {
                ("\u{23F8}", "Pause animation")
            } else {
                ("\u{25B6}", "Play animation")
            };
            let play_btn = egui::Button::new(
                RichText::new(label).size(16.0).color(Color32::WHITE),
            )
            .fill(Color32::from_rgba_unmultiplied(255, 255, 255, 30))
            .min_size(egui::vec2(28.0, 24.0));
            if child
                .add_enabled(frame_count > 1, play_btn)
                .on_hover_text(hover)
                .clicked()
            {
                self.app_state.weather_playing = !self.app_state.weather_playing;
                self.app_state.weather_last_advance = Some(std::time::Instant::now());
            }

            // Current frame timestamp, fixed width so the slider stays put.
            child.label(
                RichText::new(&timestamp)
                    .monospace()
                    .strong()
                    .color(Color32::WHITE),
            );

            let max_idx = frame_count.saturating_sub(1);
            let mut idx = self.app_state.weather_current_frame as u32;
            let max_u = max_idx as u32;
            // Slider fills remaining horizontal space.
            let remaining_w = (child.available_width() - 4.0).max(60.0);
            child.spacing_mut().slider_width = remaining_w;
            let slider = egui::Slider::new(&mut idx, 0..=max_u).show_value(false);
            if child.add_enabled(frame_count > 1, slider).changed() {
                self.app_state.weather_current_frame = idx as usize;
                // Manual scrubbing pauses auto-advance.
                self.app_state.weather_playing = false;
            }
        });
    }

    /// SDR# / Gqrx-style spectrum + waterfall.
    ///
    /// Top half: live FFT line with a translucent gradient fill from the
    /// trace down to the baseline, painted as a per-vertex-colored
    /// triangle strip mesh, with a faint dB grid and frequency labels
    /// overlaid. The HD digital sidebands (±129..±199 kHz from carrier)
    /// are highlighted as faint colored regions so the user can see the
    /// shoulders rise above the FM analog signal.
    ///
    /// Bottom half: rolling waterfall, 256 rows × 1024 bins. Each row is
    /// a snapshot of the FFT mapped through a turbo-style colormap. The
    /// texture is regenerated only when the spectrum tap's generation
    /// counter advances; in-between frames just re-blit the cached image.
    ///
    /// Driven by [`crate::dsp::SpectrumTap`] which is fed by the piped
    /// I/Q thread (see [`crate::ffi::Nrsc5Process::start_piped`]). When
    /// the tap is absent or has no data, the panel renders a centered
    /// "no data yet" message; the legacy USB and rtl_tcp paths don't
    /// surface raw I/Q to us, so the panel is most useful in piped mode.
    fn spectrum_ui(&mut self, ui: &mut Ui) {
        use crate::dsp::{FFT_SIZE, WATERFALL_ROWS};
        use egui::{
            epaint::{Mesh, Vertex},
            pos2, vec2, Color32, ColorImage, Pos2, Rect, Sense, Shape, Stroke,
            TextureOptions,
        };

        let dim = Color32::from_gray(140);

        // Header: live frequency + sample rate readout.
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!(
                    "{:.4} MHz",
                    self.app_state.frequency_mhz
                ))
                .monospace()
                .strong(),
            );
            ui.add_space(8.0);
            ui.label(
                RichText::new("span 1.488 Msps")
                    .small()
                    .color(dim),
            );
            ui.add_space(8.0);
            if !self.app_state.is_streaming {
                ui.label(
                    RichText::new("(stopped \u{2014} press Start to feed the FFT)")
                        .small()
                        .color(dim),
                );
            }
        });
        ui.add_space(2.0);

        // Hand back if there's no tap installed at all (e.g. backend
        // failed to initialize). The panel is informative even then,
        // because a future Start might wire one up.
        let Some(tap) = self.app_state.spectrum_tap.clone() else {
            ui.centered_and_justified(|ui| {
                ui.label(
                    RichText::new("Spectrum unavailable: SDR backend not initialized")
                        .color(dim),
                );
            });
            return;
        };

        // Snapshot the current state. Allocations happen only on the
        // first paint or after a resize of the underlying buffers.
        tap.snapshot_into(&mut self.app_state.spectrum_snapshot);
        let generation = self.app_state.spectrum_snapshot.generation;

        // Reserve the full panel area, then split 40% spectrum / 60%
        // waterfall vertically.
        let total_rect = ui.available_rect_before_wrap();
        let split_y = total_rect.top() + total_rect.height() * 0.40;
        let spec_rect = Rect::from_min_max(total_rect.min, pos2(total_rect.right(), split_y));
        let wf_rect = Rect::from_min_max(pos2(total_rect.left(), split_y + 2.0), total_rect.max);
        ui.allocate_rect(total_rect, Sense::hover());
        let painter = ui.painter_at(total_rect);

        // Background panels (slightly different shades so the split is
        // visually obvious without a hard divider).
        let bg_spec = Color32::from_rgb(8, 10, 16);
        let bg_wf = Color32::from_rgb(4, 5, 10);
        painter.rect_filled(spec_rect, egui::CornerRadius::same(2), bg_spec);
        painter.rect_filled(wf_rect, egui::CornerRadius::same(2), bg_wf);

        // ---- Spectrum trace + fill ------------------------------------------------

        // Map dB → pixel y. Top of `spec_rect` = -10 dB, bottom = -100 dB.
        const DB_TOP: f32 = -10.0;
        const DB_BOT: f32 = -100.0;
        let db_to_y = |db: f32| -> f32 {
            let t = ((db - DB_BOT) / (DB_TOP - DB_BOT)).clamp(0.0, 1.0);
            spec_rect.bottom() - t * spec_rect.height()
        };

        // dB gridlines every 20 dB.
        let grid_color = Color32::from_rgb(28, 36, 52);
        let mut db_mark = DB_TOP;
        while db_mark >= DB_BOT {
            let y = db_to_y(db_mark);
            painter.line_segment(
                [pos2(spec_rect.left(), y), pos2(spec_rect.right(), y)],
                Stroke::new(0.6, grid_color),
            );
            painter.text(
                pos2(spec_rect.left() + 4.0, y - 2.0),
                egui::Align2::LEFT_BOTTOM,
                format!("{:.0}", db_mark),
                egui::FontId::monospace(9.0),
                Color32::from_rgb(110, 130, 160),
            );
            db_mark -= 20.0;
        }

        // HD sideband shading: the digital sidebands are bands of
        // 0.129..0.199 of the sample rate either side of center.
        let sample_rate = self.app_state.spectrum_snapshot.sample_rate_sps;
        if sample_rate > 1.0 {
            let bin_hz = sample_rate / FFT_SIZE as f32;
            let nyquist = sample_rate * 0.5;
            // Convert a "Hz from center" offset to a pixel x.
            let hz_to_x = |hz: f32| -> f32 {
                let t = (hz + nyquist) / (2.0 * nyquist);
                spec_rect.left() + t.clamp(0.0, 1.0) * spec_rect.width()
            };
            let hd_inner = 129_000.0;
            let hd_outer = 199_000.0;
            let hd_color = Color32::from_rgba_unmultiplied(80, 160, 255, 18);
            painter.rect_filled(
                Rect::from_min_max(
                    pos2(hz_to_x(-hd_outer), spec_rect.top()),
                    pos2(hz_to_x(-hd_inner), spec_rect.bottom()),
                ),
                egui::CornerRadius::ZERO,
                hd_color,
            );
            painter.rect_filled(
                Rect::from_min_max(
                    pos2(hz_to_x(hd_inner), spec_rect.top()),
                    pos2(hz_to_x(hd_outer), spec_rect.bottom()),
                ),
                egui::CornerRadius::ZERO,
                hd_color,
            );
            let _ = bin_hz;
        }

        // Build the spectrum line + filled triangle strip. We sample
        // FFT_SIZE bins across the rect width; if the rect is narrower
        // than FFT_SIZE we downsample by stepping.
        let spec = &self.app_state.spectrum_snapshot.spectrum_db;
        let n = spec.len().max(1);
        let width = spec_rect.width().max(1.0);
        let columns = (width.round() as usize).clamp(64, n);
        let trace_color = Color32::from_rgb(220, 230, 255);
        let fill_top = Color32::from_rgba_unmultiplied(80, 150, 255, 200);
        let fill_bottom = Color32::from_rgba_unmultiplied(20, 50, 120, 30);

        let mut mesh = Mesh::default();
        let mut trace_pts: Vec<Pos2> = Vec::with_capacity(columns);
        for i in 0..columns {
            let x = spec_rect.left() + (i as f32 / (columns as f32 - 1.0)) * width;
            // Pick the max bin in this column's slice for a "peak hold"
            // feel that doesn't average away thin carrier spikes.
            let bin_start = (i * n) / columns;
            let bin_end = (((i + 1) * n) / columns).max(bin_start + 1).min(n);
            let mut peak = f32::NEG_INFINITY;
            for k in bin_start..bin_end {
                if spec[k] > peak {
                    peak = spec[k];
                }
            }
            let y_top = db_to_y(peak);
            let y_bot = spec_rect.bottom();
            // Top vertex (line color, alpha fade based on dB height).
            let h = ((peak - DB_BOT) / (DB_TOP - DB_BOT)).clamp(0.0, 1.0);
            let top_col = lerp_color(fill_bottom, fill_top, h);
            let idx_top = mesh.vertices.len() as u32;
            mesh.vertices.push(Vertex {
                pos: pos2(x, y_top),
                uv: egui::epaint::WHITE_UV,
                color: top_col,
            });
            mesh.vertices.push(Vertex {
                pos: pos2(x, y_bot),
                uv: egui::epaint::WHITE_UV,
                color: fill_bottom,
            });
            if i > 0 {
                // Two triangles per quad between this column and the previous.
                let p = idx_top - 2; // previous top
                let q = idx_top - 1; // previous bottom
                let r = idx_top; // this top
                let s = idx_top + 1; // this bottom
                mesh.indices.extend_from_slice(&[p, q, s, p, s, r]);
            }
            trace_pts.push(pos2(x, y_top));
        }
        painter.add(Shape::mesh(mesh));
        // Crisp trace line on top of the fill.
        painter.add(Shape::line(trace_pts, Stroke::new(1.2, trace_color)));

        // Frequency labels along the bottom of the spectrum rect.
        let center_mhz = self.app_state.spectrum_snapshot.center_freq_hz / 1_000_000.0;
        let half_span_mhz = sample_rate as f64 / 2.0 / 1_000_000.0;
        for slot in 0..=4 {
            let t = slot as f32 / 4.0;
            let x = spec_rect.left() + t * spec_rect.width();
            let mhz = center_mhz - half_span_mhz + (t as f64) * 2.0 * half_span_mhz;
            painter.text(
                pos2(x, spec_rect.bottom() - 2.0),
                if slot == 0 {
                    egui::Align2::LEFT_BOTTOM
                } else if slot == 4 {
                    egui::Align2::RIGHT_BOTTOM
                } else {
                    egui::Align2::CENTER_BOTTOM
                },
                format!("{:.3} MHz", mhz),
                egui::FontId::monospace(10.0),
                Color32::from_rgb(150, 170, 200),
            );
        }

        // ---- Waterfall ------------------------------------------------------------

        // Rebuild the waterfall texture only when the tap has advanced.
        if self.app_state.spectrum_texture.is_none()
            || self.app_state.spectrum_last_drawn_generation != generation
        {
            let wf = &self.app_state.spectrum_snapshot.waterfall;
            let head = self.app_state.spectrum_snapshot.waterfall_head;
            // We want the NEWEST row at the TOP of the image. The ring's
            // newest row is one slot before `head` (mod ROWS), and the
            // oldest row is at `head`. So image row `r` (top-to-bottom)
            // pulls from ring row `(head + WATERFALL_ROWS - 1 - r)
            // % WATERFALL_ROWS`.
            let mut pixels = Vec::with_capacity(WATERFALL_ROWS * FFT_SIZE);
            for r in 0..WATERFALL_ROWS {
                let ring_row = (head + WATERFALL_ROWS - 1 - r) % WATERFALL_ROWS;
                let row_start = ring_row * FFT_SIZE;
                for k in 0..FFT_SIZE {
                    pixels.push(turbo_colormap(wf[row_start + k]));
                }
            }
            let img = ColorImage {
                size: [FFT_SIZE, WATERFALL_ROWS],
                pixels,
                source_size: vec2(FFT_SIZE as f32, WATERFALL_ROWS as f32),
            };
            match self.app_state.spectrum_texture.as_mut() {
                Some(handle) => handle.set(img, TextureOptions::LINEAR),
                None => {
                    let new = ui.ctx().load_texture(
                        "spectrum_waterfall",
                        img,
                        TextureOptions::LINEAR,
                    );
                    self.app_state.spectrum_texture = Some(new);
                }
            }
            self.app_state.spectrum_last_drawn_generation = generation;
        }

        if let Some(tex) = self.app_state.spectrum_texture.as_ref() {
            let mut mesh = Mesh::with_texture(tex.id());
            mesh.add_rect_with_uv(
                wf_rect,
                Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                Color32::WHITE,
            );
            painter.add(Shape::mesh(mesh));
        }

        // Vertical center-frequency tick across both halves so the
        // carrier line is obvious.
        let cx = spec_rect.center().x;
        painter.line_segment(
            [pos2(cx, spec_rect.top()), pos2(cx, spec_rect.bottom())],
            Stroke::new(0.7, Color32::from_rgba_unmultiplied(255, 60, 60, 110)),
        );
        painter.line_segment(
            [pos2(cx, wf_rect.top()), pos2(cx, wf_rect.bottom())],
            Stroke::new(0.7, Color32::from_rgba_unmultiplied(255, 60, 60, 110)),
        );

        // Keep repainting while the panel is on screen so we get smooth
        // waterfall scroll even when no other UI is animating.
        if self.app_state.is_streaming {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(33));
        }
    }

    fn signal_ui(&mut self, ui: &mut Ui) {
        let dim = Color32::from_gray(140);

        // MER: higher is better. Typical: -10 to +30 dB. Good > 10 dB.
        let mer = self.app_state.mer;
        let mer_color = if mer > 10.0 {
            Color32::from_rgb(60, 170, 90)
        } else if mer > 5.0 {
            Color32::from_rgb(200, 160, 50)
        } else {
            Color32::from_rgb(200, 70, 70)
        };

        // BER: lower is better. Good < 0.001, OK < 0.01.
        let ber = self.app_state.ber;
        let ber_color = if ber < 0.001 {
            Color32::from_rgb(60, 170, 90)
        } else if ber < 0.01 {
            Color32::from_rgb(200, 160, 50)
        } else {
            Color32::from_rgb(200, 70, 70)
        };

        ui.add_space(2.0);
        signal_badge(ui, "MER", &format!("{:.1} dB", mer), mer_color);
        ui.add_space(4.0);
        signal_badge(ui, "BER", &format!("{:.5}", ber), ber_color);
        ui.add_space(6.0);

        // ----- AGC readout ----------------------------------------------
        // Only meaningful while the closed-loop AGC is actually running
        // (piped backend, stream active). The snapshot is `None`
        // otherwise — fall back to the existing `agc_db` line in the
        // status row below in that case.
        if let Some(snap) = self.app_state.agc_snapshot.as_ref() {
            use crate::dsp::AgcStatus;
            // Status icons are drawn from blocks egui's default font
            // covers (General Punctuation, Latin-1) — the Geometric
            // Shapes block (● ○ ◐) renders as tofu. Color is still the
            // primary cue; the glyph is decoration that disambiguates
            // for anyone color-blind.
            let (status_text, status_color) = match snap.status {
                AgcStatus::Probing => (
                    "\u{2026} PROBING",      // U+2026 HORIZONTAL ELLIPSIS — "in motion"
                    Color32::from_rgb(200, 160, 50),
                ),
                AgcStatus::Settled => (
                    "\u{2022} SETTLED",      // U+2022 BULLET — "locked in"
                    Color32::from_rgb(60, 170, 90),
                ),
                AgcStatus::Bailed => (
                    "\u{00D7} BAILED",       // U+00D7 MULTIPLICATION SIGN — "gave up"
                    Color32::from_rgb(200, 70, 70),
                ),
            };
            let gain_db = snap.current_tenths as f32 / 10.0;
            let secs_since_change = snap.last_change_at.elapsed().as_secs();
            let ago = if secs_since_change < 60 {
                format!("{}s ago", secs_since_change)
            } else if secs_since_change < 3600 {
                format!("{}m ago", secs_since_change / 60)
            } else {
                format!("{}h ago", secs_since_change / 3600)
            };
            ui.separator();
            ui.horizontal(|ui| {
                ui.label(RichText::new("AGC").small().strong().color(dim));
                ui.add_space(4.0);
                ui.label(RichText::new(status_text).small().color(status_color));
            });
            ui.label(
                RichText::new(format!("Gain: {:.1} dB \u{00B7} {}", gain_db, ago))
                    .small()
                    .color(dim),
            )
            .on_hover_text(&snap.last_reason);
            ui.add_space(6.0);
        }

        // ----- Gain mode control ----------------------------------------
        // Always visible, regardless of whether anything is streaming so
        // the user can pick a mode before pressing Start. Selection is
        // persisted immediately; takes effect on the next piped Start.
        // The "(restart to apply)" hint surfaces when the live stream's
        // mode differs from the user's choice.
        ui.separator();
        ui.label(RichText::new("Gain mode").small().strong().color(dim));
        let selected = self.app_state.gain_mode;
        let mut new_mode: Option<GainMode> = None;
        egui::ComboBox::from_id_salt("gain_mode_combo")
            .selected_text(match selected {
                GainMode::Auto => "Auto (closed-loop)",
                GainMode::Manual => "Manual",
                GainMode::HardwareAgc => "Hardware AGC",
            })
            .width(180.0)
            .show_ui(ui, |ui| {
                for (variant, label, hint) in [
                    (
                        GainMode::Auto,
                        "Auto (closed-loop)",
                        "Software AGC that walks the R820T2 gain table to maximize MER. Default — best choice for most stations.",
                    ),
                    (
                        GainMode::Manual,
                        "Manual",
                        "Hold a fixed tuner gain. Use the slider below to pick a value from the 29-step R820T2 table.",
                    ),
                    (
                        GainMode::HardwareAgc,
                        "Hardware AGC",
                        "Let the R820T2's own hardware AGC drive gain. Usually wrong for HD Radio (over-amplifies the analog carrier) — escape hatch only.",
                    ),
                ] {
                    if ui
                        .selectable_label(selected == variant, label)
                        .on_hover_text(hint)
                        .clicked()
                    {
                        new_mode = Some(variant);
                    }
                }
            });
        if let Some(mode) = new_mode {
            self.commands.push(UiCommand::SetGainMode(mode));
        }

        // Manual-gain slider, only visible when Manual is selected.
        if self.app_state.gain_mode == GainMode::Manual {
            let table = crate::sdr::R820T_GAINS_TENTHS;
            let mut idx = table
                .iter()
                .position(|&t| t == self.app_state.manual_gain_tenths)
                .unwrap_or_else(|| {
                    // Off-table value (hand-edited config); pick the
                    // nearest step so the slider has something to anchor.
                    let target = self.app_state.manual_gain_tenths;
                    table
                        .iter()
                        .enumerate()
                        .min_by_key(|(_, &t)| (t - target).abs())
                        .map(|(i, _)| i)
                        .unwrap_or(0)
                });
            let initial_idx = idx;
            ui.horizontal(|ui| {
                ui.label(RichText::new("Gain").small().color(dim));
                ui.add(
                    egui::Slider::new(&mut idx, 0..=(table.len() - 1))
                        .show_value(false)
                        .clamping(egui::SliderClamping::Always),
                );
                ui.label(
                    RichText::new(format!("{:.1} dB", table[idx] as f32 / 10.0))
                        .small()
                        .monospace()
                        .color(dim),
                );
            });
            if idx != initial_idx {
                self.commands
                    .push(UiCommand::SetManualGainTenths(table[idx]));
            }
        }

        // "(restart to apply)" hint when the live stream's mode/value
        // doesn't match the user's current choice.
        let needs_restart = match self.app_state.active_gain_mode {
            Some(active) => {
                active != self.app_state.gain_mode
                    || (self.app_state.gain_mode == GainMode::Manual
                        && self.app_state.active_manual_gain_tenths
                            != Some(self.app_state.manual_gain_tenths))
            }
            None => false,
        };
        if needs_restart {
            ui.label(
                RichText::new("(restart stream to apply)")
                    .small()
                    .italics()
                    .color(Color32::from_rgb(200, 160, 50)),
            );
        }
        ui.add_space(6.0);

        ui.separator();
        ui.label(
            RichText::new(format!("Status: {}", self.app_state.nrsc5_status))
                .small()
                .color(dim),
        );
        ui.label(
            RichText::new(format!("Event: {}", self.app_state.last_event))
                .small()
                .color(dim),
        );
    }

    /// QPSK "scope" panel — animated scatter of synthesized symbol samples
    /// that visually tightens or fuzzes based on per-sideband MER reported
    /// by nrsc5. Lower-sideband MER governs the spread of samples on the
    /// left half of the plot; upper-sideband MER governs the right half.
    ///
    /// Note: these samples are *generated* from MER, not captured from the
    /// real demodulator — nrsc5 doesn't expose post-equalizer symbol data
    /// to us. The cloud shape is statistically faithful (σ ≈ 10^(-MER/20),
    /// which is the standard EVM relationship) so a well-tuned strong
    /// station collapses into four crisp dots, and a marginal one smears.
    fn constellation_ui(&mut self, ui: &mut Ui) {
        let dim = Color32::from_gray(140);
        // "Locked" iff nrsc5 has signaled sync and we're actively streaming.
        // Without a lock we render very wide noise so the panel makes it
        // obvious nothing is being received.
        let synced = self.app_state.is_streaming
            && (self.app_state.nrsc5_status == "synced"
                || self.app_state.nrsc5_status.starts_with("audio started"));
        let lock_color = if synced {
            Color32::from_rgb(60, 170, 90)
        } else {
            Color32::from_rgb(200, 70, 70)
        };
        let lock_text = if synced { "\u{25CF} LOCK" } else { "\u{25CB} no lock" };

        ui.add_space(2.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new(lock_text).strong().color(lock_color));
            ui.add_space(8.0);
            ui.label(
                RichText::new(format!(
                    "MER  L {:>5.1} dB   U {:>5.1} dB",
                    self.app_state.mer_lower, self.app_state.mer_upper,
                ))
                .monospace()
                .color(dim),
            );
        });
        ui.add_space(4.0);

        // Allocate a square viewport — constellations only look right at 1:1.
        let avail = ui.available_size();
        let side = avail.x.min(avail.y).max(80.0);
        let (rect, _resp) =
            ui.allocate_exact_size(Vec2::new(side, side), egui::Sense::hover());
        let painter = ui.painter_at(rect);

        // Dark "oscilloscope" backdrop, independent of light/dark theme so the
        // phosphor-green samples stay legible either way.
        let scope_bg = Color32::from_rgb(8, 12, 14);
        painter.rect_filled(rect, egui::CornerRadius::same(4), scope_bg);

        // Map normalized symbol coords (±1.6 view window) into the square.
        let cx = rect.center().x;
        let cy = rect.center().y;
        let scale = (side * 0.5) / 1.6;
        let to_screen = |x: f32, y: f32| -> egui::Pos2 {
            // Invert Y so +Q is up, matching textbook constellation diagrams.
            egui::pos2(cx + x * scale, cy - y * scale)
        };

        // Faint unit-magnitude gridlines through ±1, then brighter I/Q axes.
        let grid = Color32::from_rgb(28, 60, 40);
        let axis = Color32::from_rgb(50, 110, 70);
        for &v in &[-1.0f32, 1.0] {
            painter.line_segment(
                [to_screen(v, -1.5), to_screen(v, 1.5)],
                egui::Stroke::new(0.5, grid),
            );
            painter.line_segment(
                [to_screen(-1.5, v), to_screen(1.5, v)],
                egui::Stroke::new(0.5, grid),
            );
        }
        painter.line_segment(
            [to_screen(0.0, -1.5), to_screen(0.0, 1.5)],
            egui::Stroke::new(0.8, axis),
        );
        painter.line_segment(
            [to_screen(-1.5, 0.0), to_screen(1.5, 0.0)],
            egui::Stroke::new(0.8, axis),
        );

        // Crosshairs at the four ideal QPSK symbol locations.
        let target = Color32::from_rgba_unmultiplied(200, 255, 220, 90);
        for &sx in &[-1.0f32, 1.0] {
            for &sy in &[-1.0f32, 1.0] {
                let c = to_screen(sx, sy);
                painter.line_segment(
                    [c - egui::vec2(5.0, 0.0), c + egui::vec2(5.0, 0.0)],
                    egui::Stroke::new(1.0, target),
                );
                painter.line_segment(
                    [c - egui::vec2(0.0, 5.0), c + egui::vec2(0.0, 5.0)],
                    egui::Stroke::new(1.0, target),
                );
            }
        }

        // Ring buffer + RNG state, lazily initialized on first paint.
        const RING: usize = 1024;
        const NEW_PER_FRAME: usize = 24;
        let st = &mut self.app_state;
        if st.constellation_samples.len() != RING {
            st.constellation_samples = vec![[0.0_f32, 0.0_f32]; RING];
            st.constellation_head = 0;
        }
        if st.constellation_rng == 0 {
            // Mix in a per-run salt so two side-by-side instances don't look
            // identical; the exact seed doesn't matter as long as it's nonzero.
            st.constellation_rng = 0x9E37_79B9_7F4A_7C15
                ^ (std::time::Instant::now().elapsed().as_nanos() as u64)
                    .wrapping_mul(0xD2B7_4407_B1CE_6E93);
            if st.constellation_rng == 0 {
                st.constellation_rng = 0xA5A5_A5A5_A5A5_A5A5;
            }
        }

        // EVM ≈ 10^(-MER/20). Clamped so a 30 dB station doesn't show *zero*
        // jitter (looks dead) and a -5 dB one doesn't extend off-screen.
        fn sigma_from_mer(mer_db: f32, synced: bool) -> f32 {
            if !synced || !mer_db.is_finite() {
                return 1.2;
            }
            let lin = 10f32.powf(-mer_db / 20.0);
            lin.clamp(0.03, 1.4)
        }
        let sigma_l_target = sigma_from_mer(st.mer_lower, synced);
        let sigma_u_target = sigma_from_mer(st.mer_upper, synced);

        // Low-pass the displayed σ so 1 Hz MER ticks become a smooth
        // tightening/loosening of the cloud instead of a visible step.
        // α=0.08 ≈ quarter-second settle at 30 fps, which reads as a
        // satisfying "locking on" animation when MER rapidly improves.
        if st.constellation_sigma_l <= 0.0 {
            st.constellation_sigma_l = sigma_l_target;
            st.constellation_sigma_u = sigma_u_target;
        } else {
            st.constellation_sigma_l +=
                (sigma_l_target - st.constellation_sigma_l) * 0.08;
            st.constellation_sigma_u +=
                (sigma_u_target - st.constellation_sigma_u) * 0.08;
        }
        let sigma_l = st.constellation_sigma_l;
        let sigma_u = st.constellation_sigma_u;

        // Push fresh samples. Bits 0/1 of the RNG word pick which QPSK
        // symbol; Gaussian noise from box_muller is scaled by the σ for
        // whichever sideband that symbol falls into.
        for _ in 0..NEW_PER_FRAME {
            let bits = xorshift64(&mut st.constellation_rng);
            let bx = if (bits & 1) == 0 { -1.0_f32 } else { 1.0 };
            let by = if (bits & 2) == 0 { -1.0_f32 } else { 1.0 };
            let sigma = if bx < 0.0 { sigma_l } else { sigma_u };
            let nx = box_muller(&mut st.constellation_rng) * sigma;
            let ny = box_muller(&mut st.constellation_rng) * sigma;
            let idx = st.constellation_head;
            st.constellation_samples[idx] = [bx + nx, by + ny];
            st.constellation_head = (idx + 1) % RING;
        }

        // Draw oldest → newest so the freshest samples overdraw stale ones.
        // Alpha ramps from 30 (oldest) to 220 (newest), giving the cloud a
        // subtle motion-trail / phosphor-persistence feel.
        for i in 0..RING {
            let buf_idx = (st.constellation_head + i) % RING;
            let p = st.constellation_samples[buf_idx];
            let pos = to_screen(p[0], p[1]);
            if !rect.contains(pos) {
                continue;
            }
            let age01 = i as f32 / (RING - 1) as f32;
            let alpha = (30.0 + age01 * 190.0) as u8;
            let color = Color32::from_rgba_unmultiplied(80, 240, 140, alpha);
            painter.circle_filled(pos, 1.6, color);
        }

        // Tiny axis legends in the corners ("I" right, "Q" top) for the
        // SDR-aficionado vibe.
        let label_color = Color32::from_rgba_unmultiplied(120, 200, 150, 180);
        let font = egui::FontId::monospace(10.0);
        painter.text(
            egui::pos2(rect.max.x - 10.0, cy - 6.0),
            egui::Align2::RIGHT_BOTTOM,
            "I",
            font.clone(),
            label_color,
        );
        painter.text(
            egui::pos2(cx + 6.0, rect.min.y + 2.0),
            egui::Align2::LEFT_TOP,
            "Q",
            font,
            label_color,
        );

        // Keep animating at ~30 Hz while the tab is visible.
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(33));
    }

    fn collage_ui(&mut self, ui: &mut Ui) {
        let dim = Color32::from_gray(120);
        let tiles = self.app_state.art_tiles.clone();
        if tiles.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.label(
                    RichText::new(
                        "Album art will appear here as the station plays songs.",
                    )
                    .color(dim)
                    .italics(),
                );
            });
            return;
        }

        // Header strip showing session age, unique-art count, and a small
        // tile-cap stepper. Cap snaps to powers of two so a "geeky"
        // 1/2/4/8/.../512 progression is the only thing the user can pick.
        let session_label = match self.app_state.art_session_started {
            Some(t) => {
                let secs = t.elapsed().as_secs().min(8 * 3600);
                let hours = secs / 3600;
                let mins = (secs % 3600) / 60;
                let span = if secs >= 8 * 3600 {
                    "last 8h".to_string()
                } else {
                    format!("last {hours}h{mins:02}m")
                };
                format!(
                    "{span} \u{2022} {} unique covers (rolling)",
                    tiles.len()
                )
            }
            None => format!("{} covers", tiles.len()),
        };
        let cap = self.app_state.collage_tile_cap.clamp(1, 512);
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(session_label)
                    .small()
                    .color(Color32::from_gray(150)),
            );
            ui.add_space(12.0);
            ui.label(
                RichText::new("tiles")
                    .small()
                    .color(Color32::from_gray(150)),
            );
            let halve = ui
                .add_enabled(cap > 1, egui::Button::new("\u{2212}").small())
                .on_hover_text("Halve the tile cap");
            if halve.clicked() {
                self.commands
                    .push(UiCommand::SetCollageTileCap((cap / 2).max(1)));
            }
            ui.label(
                RichText::new(format!("{cap}"))
                    .small()
                    .monospace()
                    .color(Color32::from_gray(200)),
            );
            let dbl = ui
                .add_enabled(cap < 512, egui::Button::new("+").small())
                .on_hover_text("Double the tile cap (max 512)");
            if dbl.clicked() {
                self.commands
                    .push(UiCommand::SetCollageTileCap((cap * 2).min(512)));
            }
        });
        ui.add_space(4.0);

        // Allocate the rest of the tab for the treemap.
        let avail = ui.available_size();
        let (rect, _resp) =
            ui.allocate_exact_size(avail, egui::Sense::hover());

        let weights: Vec<(f64, String)> = tiles
            .iter()
            .map(|t| (t.count.max(1) as f64, t.path.clone()))
            .collect();
        let placements = square_grid_pack(&weights, rect);

        for ((tile_rect, _placement_path), tile) in
            placements.into_iter().zip(tiles.iter())
        {
            let path = &tile.path;
            // Paint into the full treemap cell with no inter-tile gap so
            // covers butt right up against each other. Album art is 1:1, so
            // when the cell isn't square we center-crop the source via the
            // UV rect (object-fit: cover) -- this keeps the visible portion
            // proportional rather than anamorphically squishing the cover.
            let outer = tile_rect;
            if outer.width() < 8.0 || outer.height() < 8.0 {
                continue;
            }
            let aspect = outer.width() / outer.height();
            let uv = if aspect >= 1.0 {
                // Cell wider than tall: trim top/bottom of the square cover.
                let crop = (1.0 - 1.0 / aspect) * 0.5;
                egui::Rect::from_min_max(
                    egui::pos2(0.0, crop),
                    egui::pos2(1.0, 1.0 - crop),
                )
            } else {
                // Cell taller than wide: trim left/right of the square cover.
                let crop = (1.0 - aspect) * 0.5;
                egui::Rect::from_min_max(
                    egui::pos2(crop, 0.0),
                    egui::pos2(1.0 - crop, 1.0),
                )
            };
            let uri = format!("file:///{}", path.replace('\\', "/"));
            egui::Image::new(&uri).uv(uv).paint_at(ui, outer);

            // Hover region with a tooltip listing the album and every unique
            // song we've seen displayed with this cover.
            if !tile.songs.is_empty() || !tile.album.is_empty() {
                let id = egui::Id::new(("art_tile", path));
                let resp = ui.interact(outer, id, egui::Sense::hover());
                let album = tile.album.clone();
                let songs = tile.songs.clone();
                resp.on_hover_ui(|ui| {
                    if !album.is_empty() {
                        ui.label(RichText::new(&album).strong().size(14.0));
                    }
                    if !songs.is_empty() && !album.is_empty() {
                        ui.separator();
                    }
                    for (title, artist) in &songs {
                        let line = match (title.is_empty(), artist.is_empty()) {
                            (false, false) => format!("\u{201c}{}\u{201d} \u{2014} {}", title, artist),
                            (false, true) => format!("\u{201c}{}\u{201d}", title),
                            (true, false) => artist.clone(),
                            (true, true) => continue,
                        };
                        ui.label(line);
                    }
                });
            }
        }
    }

    fn log_ui(&mut self, ui: &mut Ui) {
        // Header strip: title, view toggle, count, export.
        ui.horizontal(|ui| {
            ui.label(RichText::new("\u{1F4DD} Log").strong());
            ui.separator();
            let mut mode = self.app_state.log_view_mode;
            ui.selectable_value(&mut mode, LogViewMode::Timeline, "Timeline");
            ui.selectable_value(&mut mode, LogViewMode::TopPlayed, "Top Played");
            self.app_state.log_view_mode = mode;
            ui.separator();
            let label = match self.play_log.len() {
                1 => "1 play".to_string(),
                n => format!("{n} plays"),
            };
            ui.label(RichText::new(label).color(Color32::from_gray(170)));

            // Retention dropdown — click the "rolling Xh" label to change
            // the rolling window. Persisted to config.toml.
            let cur_hours = self.play_log.retention_hours();
            let cur_label = format_retention(cur_hours);
            let menu_label = format!("\u{2022} rolling {cur_label}");
            let menu_resp = ui
                .menu_button(
                    RichText::new(&menu_label).color(Color32::from_gray(140)),
                    |ui| {
                        ui.label(
                            RichText::new("Rolling window")
                                .strong()
                                .small(),
                        );
                        ui.separator();
                        for &hours in crate::play_log::RETENTION_CHOICES {
                            let label = format!(
                                "{}{}",
                                if hours == cur_hours { "\u{2714} " } else { "  " },
                                format_retention(hours),
                            );
                            if ui.button(label).clicked() {
                                if hours != cur_hours {
                                    self.commands
                                        .push(UiCommand::SetPlayLogRetention(hours));
                                }
                                ui.close();
                            }
                        }
                    },
                )
                .response;
            menu_resp.on_hover_text(
                "How far back the log keeps entries before pruning them. \
                 Capped at 5,000 entries regardless of window.",
            );
            ui.separator();
            if ui
                .button("\u{1F4BE} Export CSV")
                .on_hover_text("Write the current log to a CSV file")
                .clicked()
            {
                self.commands.push(UiCommand::ExportLogCsv);
            }
            let clear_enabled = !self.play_log.is_empty();
            let clear_resp = ui.add_enabled(
                clear_enabled,
                egui::Button::new("\u{1F5D1} Clear"),
            );
            let clear_resp = clear_resp.on_hover_text(
                "Drop every entry from the play log. The on-disk \
                 file is rewritten to an empty log immediately.",
            );
            if clear_resp.clicked() {
                self.commands.push(UiCommand::ClearLog);
            }
            if let Some(status) = self.app_state.log_export_status.clone() {
                ui.label(
                    RichText::new(status)
                        .small()
                        .color(Color32::from_rgb(120, 180, 240)),
                );
            }
        });
        ui.separator();

        if self.play_log.is_empty() {
            ui.vertical_centered(|ui| {
                ui.add_space(40.0);
                ui.label(
                    RichText::new("No plays logged yet.")
                        .color(Color32::from_gray(180)),
                );
                ui.add_space(8.0);
                ui.label(
                    RichText::new(
                        "Tune to a station with HD metadata and listen for a few minutes.",
                    )
                    .small()
                    .color(Color32::from_gray(120)),
                );
            });
            return;
        }

        match self.app_state.log_view_mode {
            LogViewMode::Timeline => self.log_timeline_table(ui),
            LogViewMode::TopPlayed => self.log_top_played_table(ui),
        }

        // Drop any stale export status if the user clicked elsewhere.
        if ui.input(|i| i.pointer.any_click()) {
            self.app_state.log_export_status = None;
        }
    }

    fn log_timeline_table(&self, ui: &mut Ui) {
        use egui_extras::{Column, TableBuilder};
        // Snapshot to a Vec<&PlayEntry> so we can index by row.
        let entries: Vec<_> = self.play_log.entries().iter().rev().collect();

        TableBuilder::new(ui)
            .striped(true)
            .resizable(true)
            .column(Column::initial(60.0).at_least(50.0))
            .column(Column::initial(180.0).at_least(80.0))
            .column(Column::remainder().at_least(140.0))
            .column(Column::initial(100.0).at_least(70.0))
            .header(20.0, |mut h| {
                h.col(|ui| {
                    ui.strong("Time");
                });
                h.col(|ui| {
                    ui.strong("Artist");
                });
                h.col(|ui| {
                    ui.strong("Title");
                });
                h.col(|ui| {
                    ui.strong("Station");
                });
            })
            .body(|body| {
                body.rows(18.0, entries.len(), |mut row| {
                    let e = entries[row.index()];
                    row.col(|ui| {
                        ui.monospace(crate::play_log::fmt_local_hhmm(e.ts_millis));
                    });
                    row.col(|ui| {
                        ui.label(&e.artist);
                    });
                    row.col(|ui| {
                        ui.label(&e.title);
                    });
                    row.col(|ui| {
                        ui.monospace(e.station_label());
                    });
                });
            });
    }

    fn log_top_played_table(&self, ui: &mut Ui) {
        use egui_extras::{Column, TableBuilder};
        use std::collections::HashMap;

        struct Grouped<'a> {
            title: &'a str,
            artist: &'a str,
            plays: u32,
            last_ts: i64,
        }

        let mut groups: HashMap<(&str, &str), Grouped> = HashMap::new();
        for e in self.play_log.entries() {
            let key = (e.artist.as_str(), e.title.as_str());
            let g = groups.entry(key).or_insert_with(|| Grouped {
                title: &e.title,
                artist: &e.artist,
                plays: 0,
                last_ts: 0,
            });
            g.plays += 1;
            if e.ts_millis > g.last_ts {
                g.last_ts = e.ts_millis;
            }
        }
        let mut grouped: Vec<_> = groups.into_values().collect();
        grouped.sort_by(|a, b| {
            b.plays
                .cmp(&a.plays)
                .then_with(|| b.last_ts.cmp(&a.last_ts))
        });

        TableBuilder::new(ui)
            .striped(true)
            .resizable(true)
            .column(Column::initial(50.0).at_least(40.0))
            .column(Column::initial(180.0).at_least(80.0))
            .column(Column::remainder().at_least(140.0))
            .column(Column::initial(70.0).at_least(50.0))
            .header(20.0, |mut h| {
                h.col(|ui| {
                    ui.strong("Plays");
                });
                h.col(|ui| {
                    ui.strong("Artist");
                });
                h.col(|ui| {
                    ui.strong("Title");
                });
                h.col(|ui| {
                    ui.strong("Last");
                });
            })
            .body(|body| {
                body.rows(18.0, grouped.len(), |mut row| {
                    let g = &grouped[row.index()];
                    row.col(|ui| {
                        ui.monospace(g.plays.to_string());
                    });
                    row.col(|ui| {
                        ui.label(g.artist);
                    });
                    row.col(|ui| {
                        ui.label(g.title);
                    });
                    row.col(|ui| {
                        ui.monospace(crate::play_log::fmt_local_hhmm(g.last_ts));
                    });
                });
            });
    }
}

/// Render an integer-hour retention window as a compact human label
/// (`6h`, `24h`, `7d`). Used by the log header and its dropdown so the
/// visible value and the menu items stay in sync.
fn format_retention(hours: u32) -> String {
    if hours >= 24 && hours % 24 == 0 {
        let days = hours / 24;
        format!("{days}d")
    } else {
        format!("{hours}h")
    }
}

/// Linear interpolation between two `Color32` values in straight-alpha
/// space. Used to fade the spectrum fill from a bright top edge into a
/// near-transparent baseline.
fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let mix = |x: u8, y: u8| -> u8 {
        let xf = x as f32;
        let yf = y as f32;
        (xf + (yf - xf) * t).round().clamp(0.0, 255.0) as u8
    };
    Color32::from_rgba_unmultiplied(
        mix(a.r(), b.r()),
        mix(a.g(), b.g()),
        mix(a.b(), b.b()),
        mix(a.a(), b.a()),
    )
}

/// "Turbo"-style colormap (Google's polynomial approximation, simplified).
/// Maps an 8-bit intensity to a perceptually-uniform blue→cyan→green→
/// yellow→red gradient. We hand-roll a cheap piecewise-linear version
/// rather than pulling in another dependency; visually indistinguishable
/// from the real turbo at the per-pixel sizes we render.
fn turbo_colormap(v: u8) -> Color32 {
    // 6-stop gradient: black → deep blue → cyan → yellow → red → white.
    // Tuned to match the SDR# / Gqrx "waterfall" feel the user asked for.
    const STOPS: [(u8, u8, u8); 6] = [
        (0, 0, 16),     // near-black at floor
        (32, 60, 160),  // deep blue
        (40, 200, 220), // cyan
        (240, 220, 60), // yellow
        (240, 80, 40),  // red-orange
        (255, 240, 220),// near-white at ceiling
    ];
    let t = v as f32 / 255.0;
    let n = STOPS.len() - 1;
    let scaled = t * n as f32;
    let idx = (scaled.floor() as usize).min(n - 1);
    let frac = scaled - idx as f32;
    let (r0, g0, b0) = STOPS[idx];
    let (r1, g1, b1) = STOPS[idx + 1];
    let mix = |a: u8, b: u8| -> u8 {
        (a as f32 + (b as f32 - a as f32) * frac).round() as u8
    };
    Color32::from_rgb(mix(r0, r1), mix(g0, g1), mix(b0, b1))
}

/// Render a single rounded colored "pill" with a label and numeric value,
/// used for the at-a-glance MER and BER displays in the Signal tab.
fn signal_badge(ui: &mut Ui, label: &str, value: &str, color: Color32) {
    egui::Frame::new()
        .fill(color)
        .corner_radius(egui::CornerRadius::same(10))
        .inner_margin(egui::Margin::symmetric(10, 4))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(label)
                        .strong()
                        .size(13.0)
                        .color(Color32::WHITE),
                );
                ui.add_space(8.0);
                ui.label(
                    RichText::new(value)
                        .monospace()
                        .strong()
                        .size(18.0)
                        .color(Color32::WHITE),
                );
            });
        });
}

/// Discrete-size square-tile layout for the album-art collage. Unlike the
/// proportional treemap (which produces variable-aspect rectangles), this
/// packer puts every cover into a perfect square whose side is a small
/// integer multiple of a base cell. Heavy-rotation covers get 6x6-cell
/// squares, singletons get 1x1, and a skyline packer drops them in
/// largest-first so there are no gaps.
///
/// Returns a `Vec<(Rect, payload)>` in the **same order as the input** so
/// the caller can keep pairing placements with its own ordered tile list.
/// Tiles that didn't fit are returned with a zero-sized rect; the caller
/// already skips anything below an 8px minimum.
fn square_grid_pack(
    items: &[(f64, String)],
    rect: egui::Rect,
) -> Vec<(egui::Rect, String)> {
    let n = items.len();
    if n == 0 || rect.width() <= 0.0 || rect.height() <= 0.0 {
        return Vec::new();
    }

    // Quantile-bucket each item into a side multiplier. Top 0.5% mega-hits
    // are huge, next 2.5% heavy, next 7% medium-heavy, next 20% medium,
    // remainder singletons. Adapts gracefully to any tile cap.
    let mut rank_order: Vec<usize> = (0..n).collect();
    rank_order.sort_by(|&a, &b| {
        items[b]
            .0
            .partial_cmp(&items[a].0)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut sizes = vec![1usize; n];
    for (rank, &orig_idx) in rank_order.iter().enumerate() {
        let frac = rank as f64 / n as f64;
        sizes[orig_idx] = if frac < 0.005 {
            6
        } else if frac < 0.03 {
            4
        } else if frac < 0.10 {
            3
        } else if frac < 0.30 {
            2
        } else {
            1
        };
    }

    // Pick a base cell size so the total area used by the buckets fits the
    // available rect. Cells are kept exactly square (cell = min of the two
    // axis-fit sizes) so every placed tile is a perfect square.
    let total_cells: f64 = sizes.iter().map(|&s| (s * s) as f64).sum();
    let area = rect.width() as f64 * rect.height() as f64;
    let base = (area / total_cells.max(1.0)).sqrt().max(4.0);
    let cols = ((rect.width() as f64 / base).floor() as usize).max(1);
    let rows = ((rect.height() as f64 / base).floor() as usize).max(1);
    let cell = (rect.width() / cols as f32).min(rect.height() / rows as f32);

    // Clamp every bucket size to what the grid can actually hold. With very
    // small tile counts the quantile bucketing assigns a 6x6 "mega" tile
    // but the grid may only be 3 rows tall -- without this cap the packer's
    // s > rows check silently drops that tile and the collage looks like
    // it's missing a cover.
    let max_dim = cols.min(rows).max(1);
    for s in sizes.iter_mut() {
        if *s > max_dim {
            *s = max_dim;
        }
    }

    // Scattered placement: process tiles largest-first so the big ones
    // always find a home, but for any tile bigger than 1x1 pick a random
    // valid position rather than the lowest-skyline corner. Singletons
    // (1x1) then fall back to a tight first-fit scan to plug the holes.
    //
    // The RNG is seeded from the combined tile-path hash so the layout is
    // deterministic for a given set of covers (no frame-to-frame jitter)
    // but changes naturally when new art arrives.
    let mut rng_state: u64 = 0x9E37_79B9_7F4A_7C15;
    for (_, p) in items.iter() {
        // FNV-1a-ish folding of the path bytes into the seed.
        for b in p.as_bytes() {
            rng_state ^= *b as u64;
            rng_state = rng_state.wrapping_mul(0x100000001b3);
        }
    }
    fn next_rand(state: &mut u64) -> u64 {
        // LCG from Numerical Recipes -- not cryptographic, just a stable
        // way to spread big tiles across the grid.
        *state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        *state
    }

    let mut occupied = vec![vec![false; rows]; cols];
    let mut placement_rects: Vec<egui::Rect> = vec![
        egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::ZERO);
        n
    ];

    let mut pack_order: Vec<usize> = (0..n).collect();
    pack_order.sort_by(|&a, &b| sizes[b].cmp(&sizes[a]));

    for &i in &pack_order {
        let s = sizes[i];
        if s > cols || s > rows {
            continue;
        }
        // Collect every valid (c, r) where an s x s block is fully clear.
        let mut valid: Vec<(usize, usize)> = Vec::new();
        let max_c = cols - s;
        let max_r = rows - s;
        'outer: for c in 0..=max_c {
            'inner: for r in 0..=max_r {
                for dx in 0..s {
                    for dy in 0..s {
                        if occupied[c + dx][r + dy] {
                            continue 'inner;
                        }
                    }
                }
                valid.push((c, r));
                // Singletons only need the first hit for a tight fill.
                if s == 1 {
                    break 'outer;
                }
            }
        }
        if valid.is_empty() {
            continue;
        }
        let (c0, r0) = if s >= 2 {
            // Pick a deterministic-pseudo-random valid spot so big tiles
            // scatter across the grid instead of clumping in one corner.
            let idx = (next_rand(&mut rng_state) as usize) % valid.len();
            valid[idx]
        } else {
            valid[0]
        };
        for dx in 0..s {
            for dy in 0..s {
                occupied[c0 + dx][r0 + dy] = true;
            }
        }
        let min = egui::pos2(
            rect.min.x + c0 as f32 * cell,
            rect.min.y + r0 as f32 * cell,
        );
        let size = egui::vec2(s as f32 * cell, s as f32 * cell);
        placement_rects[i] = egui::Rect::from_min_size(min, size);
    }

    placement_rects
        .into_iter()
        .zip(items.iter())
        .map(|(r, (_, p))| (r, p.clone()))
        .collect()
}

/// Squarified-treemap layout (Bruls/Huijsen/van Wijk 2000). Given a list of
/// `(weight, payload)` pairs sorted by weight descending and a bounding `Rect`,
/// returns a `Vec<(Rect, payload)>` partitioning the rect into rectangles whose
/// areas are proportional to weights and whose aspect ratios are kept as close
/// to 1:1 as possible. This is what makes the album-art tiles "look right"
/// instead of getting stretched into skinny strips.
#[allow(dead_code)]
fn squarified_treemap(
    items: &[(f64, String)],
    rect: egui::Rect,
) -> Vec<(egui::Rect, String)> {
    if items.is_empty() || rect.width() <= 0.0 || rect.height() <= 0.0 {
        return Vec::new();
    }
    let total_weight: f64 = items.iter().map(|(w, _)| *w).sum();
    if total_weight <= 0.0 {
        return Vec::new();
    }
    let total_area = rect.width() as f64 * rect.height() as f64;
    // Normalize weights to areas.
    let scaled: Vec<(f64, String)> = items
        .iter()
        .map(|(w, p)| (*w / total_weight * total_area, p.clone()))
        .collect();

    let mut placements: Vec<(egui::Rect, String)> = Vec::with_capacity(scaled.len());
    let mut remaining = rect;
    let mut row: Vec<(f64, String)> = Vec::new();
    let mut idx = 0;

    while idx < scaled.len() {
        let shortest = remaining.width().min(remaining.height()) as f64;
        if shortest <= 0.0 {
            break;
        }
        let candidate = &scaled[idx];
        let current_worst = if row.is_empty() {
            f64::INFINITY
        } else {
            worst_ratio(&row, shortest)
        };
        let with_candidate = {
            let mut tmp = row.clone();
            tmp.push(candidate.clone());
            worst_ratio(&tmp, shortest)
        };
        if row.is_empty() || with_candidate <= current_worst {
            row.push(candidate.clone());
            idx += 1;
        } else {
            let (placed, new_remaining) = layout_row(&row, remaining);
            placements.extend(placed);
            remaining = new_remaining;
            row.clear();
        }
    }
    if !row.is_empty() {
        let (placed, _) = layout_row(&row, remaining);
        placements.extend(placed);
    }
    placements
}

/// Worst (largest) aspect ratio of any item in `row` if laid out along the
/// shorter side `w`. Used to decide when to "close" a row in the squarified
/// treemap algorithm.
#[allow(dead_code)]
fn worst_ratio(row: &[(f64, String)], w: f64) -> f64 {
    if w <= 0.0 {
        return f64::INFINITY;
    }
    let sum: f64 = row.iter().map(|(a, _)| *a).sum();
    let mut max_a = 0.0f64;
    let mut min_a = f64::INFINITY;
    for (a, _) in row {
        if *a > max_a {
            max_a = *a;
        }
        if *a < min_a {
            min_a = *a;
        }
    }
    if sum <= 0.0 || min_a <= 0.0 {
        return f64::INFINITY;
    }
    let w2 = w * w;
    let sum2 = sum * sum;
    (w2 * max_a / sum2).max(sum2 / (w2 * min_a))
}

/// Place a completed row of `(area, payload)` items inside `rect`, returning
/// the placed rectangles and the remaining area for the next row.
#[allow(dead_code)]
fn layout_row(
    row: &[(f64, String)],
    rect: egui::Rect,
) -> (Vec<(egui::Rect, String)>, egui::Rect) {
    let sum: f64 = row.iter().map(|(a, _)| *a).sum();
    let w = rect.width() as f64;
    let h = rect.height() as f64;
    let mut placed = Vec::with_capacity(row.len());
    if w >= h {
        // Lay out vertically on the left, row occupies width = sum / h.
        let row_w = if h > 0.0 { sum / h } else { 0.0 };
        let mut y = rect.min.y as f64;
        for (a, p) in row {
            let item_h = if row_w > 0.0 { a / row_w } else { 0.0 };
            let r = egui::Rect::from_min_size(
                egui::pos2(rect.min.x, y as f32),
                egui::vec2(row_w as f32, item_h as f32),
            );
            placed.push((r, p.clone()));
            y += item_h;
        }
        let new_remaining = egui::Rect::from_min_max(
            egui::pos2(rect.min.x + row_w as f32, rect.min.y),
            rect.max,
        );
        (placed, new_remaining)
    } else {
        // Lay out horizontally on top, row occupies height = sum / w.
        let row_h = if w > 0.0 { sum / w } else { 0.0 };
        let mut x = rect.min.x as f64;
        for (a, p) in row {
            let item_w = if row_h > 0.0 { a / row_h } else { 0.0 };
            let r = egui::Rect::from_min_size(
                egui::pos2(x as f32, rect.min.y),
                egui::vec2(item_w as f32, row_h as f32),
            );
            placed.push((r, p.clone()));
            x += item_w;
        }
        let new_remaining = egui::Rect::from_min_max(
            egui::pos2(rect.min.x, rect.min.y + row_h as f32),
            rect.max,
        );
        (placed, new_remaining)
    }
}

// -- Constellation RNG helpers ---------------------------------------------
//
// We don't pull in a full RNG crate just for the constellation panel: a
// xorshift64 seeded from the system clock is plenty for visualization-only
// jitter, and keeps the dependency footprint flat.

/// Xorshift64 step. State must be nonzero on entry; never returns 0.
fn xorshift64(s: &mut u64) -> u64 {
    let mut x = *s;
    if x == 0 {
        x = 0x9E37_79B9_7F4A_7C15;
    }
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    *s = x;
    x
}

/// Uniform float in [0, 1) using the high 53 bits of one xorshift step.
fn rand_unit(s: &mut u64) -> f32 {
    let v = xorshift64(s) >> 11;
    (v as f64 / (1u64 << 53) as f64) as f32
}

/// Standard-normal sample via Box-Muller. Returns one of the two outputs;
/// the other is discarded since this is purely for visual jitter and the
/// cost of computing both is irrelevant here.
fn box_muller(s: &mut u64) -> f32 {
    let mut u1 = rand_unit(s);
    if u1 < 1e-7 {
        u1 = 1e-7;
    }
    let u2 = rand_unit(s);
    let r = (-2.0 * u1.ln()).sqrt();
    let theta = std::f32::consts::TAU * u2;
    r * theta.cos()
}