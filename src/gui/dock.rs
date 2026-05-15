use crate::config::Preset;
use crate::gui::state::AppState;
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
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DockTab {
    Tuner,
    NowPlaying,
    Traffic,
    Weather,
    Signal,
    Collage,
}

impl DockTab {
    /// All panel variants in the order they should appear in the View menu.
    pub const ALL: [DockTab; 6] = [
        DockTab::Tuner,
        DockTab::NowPlaying,
        DockTab::Collage,
        DockTab::Signal,
        DockTab::Traffic,
        DockTab::Weather,
    ];

    /// Compact label (emoji + short name) for the top-toolbar tab toggles.
    pub fn toolbar_label(&self) -> &'static str {
        match self {
            DockTab::Tuner => "\u{1F4FB} Tuner",
            DockTab::NowPlaying => "\u{1F3B5} Now Playing",
            DockTab::Collage => "\u{1F5BC} Collage",
            DockTab::Signal => "\u{1F4F6} Signal",
            DockTab::Traffic => "\u{1F697} Traffic",
            DockTab::Weather => "\u{2601} Weather",
        }
    }
}

pub struct DockViewer<'a> {
    pub app_state: &'a mut AppState,
    pub commands: &'a mut Vec<UiCommand>,
    pub presets: &'a [Preset],
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

        // Header strip showing session age and unique-art count.
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
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(session_label)
                    .small()
                    .color(Color32::from_gray(150)),
            );
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
        let placements = squarified_treemap(&weights, rect);

        // Find max count for relative font sizing on play-count overlay.
        let max_count = tiles.iter().map(|t| t.count).max().unwrap_or(1);

        for (tile_rect, path, count) in placements
            .into_iter()
            .zip(tiles.iter())
            .map(|((r, p), tile)| (r, p, tile.count))
        {
            // Tiny gap between tiles.
            let outer = tile_rect.shrink(1.0);
            if outer.width() < 8.0 || outer.height() < 8.0 {
                continue;
            }
            // Album art is always 1:1; fit a centred square inside the
            // treemap cell so covers never stretch. Any leftover space stays
            // as the tab's background colour (mostly invisible since the
            // treemap already tries to keep cells near-square).
            let side = outer.width().min(outer.height());
            let inner = egui::Rect::from_center_size(
                outer.center(),
                egui::vec2(side, side),
            );
            let uri = format!("file:///{}", path.replace('\\', "/"));
            egui::Image::new(&uri)
                .corner_radius(3)
                .paint_at(ui, inner);

            // For tiles representing a "dominant" cover (>=2 plays *and* big
            // enough to be readable), overlay the play count in the corner.
            if count >= 2 && inner.width() >= 60.0 && inner.height() >= 60.0 {
                let intensity =
                    (count as f32 / max_count.max(1) as f32).clamp(0.4, 1.0);
                let badge_color = Color32::from_rgba_unmultiplied(
                    0,
                    0,
                    0,
                    (180.0 * intensity) as u8,
                );
                let text = format!("\u{00d7}{count}");
                let font = egui::FontId::proportional(
                    (inner.height() * 0.15).clamp(12.0, 28.0),
                );
                let painter = ui.painter().with_clip_rect(inner);
                let galley = painter.layout_no_wrap(
                    text.clone(),
                    font.clone(),
                    Color32::WHITE,
                );
                let pad = egui::vec2(6.0, 2.0);
                let badge_size = galley.size() + pad * 2.0;
                let badge_pos = egui::pos2(
                    inner.max.x - badge_size.x - 4.0,
                    inner.max.y - badge_size.y - 4.0,
                );
                let badge_rect = egui::Rect::from_min_size(badge_pos, badge_size);
                painter.rect_filled(
                    badge_rect,
                    egui::CornerRadius::same(4),
                    badge_color,
                );
                painter.galley(badge_pos + pad, galley, Color32::WHITE);
            }
        }
    }
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

/// Squarified-treemap layout (Bruls/Huijsen/van Wijk 2000). Given a list of
/// `(weight, payload)` pairs sorted by weight descending and a bounding `Rect`,
/// returns a `Vec<(Rect, payload)>` partitioning the rect into rectangles whose
/// areas are proportional to weights and whose aspect ratios are kept as close
/// to 1:1 as possible. This is what makes the album-art tiles "look right"
/// instead of getting stretched into skinny strips.
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