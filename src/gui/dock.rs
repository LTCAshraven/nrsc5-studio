use crate::config::{GainMode, Preset, SdrTransport};
use crate::gui::state::{AppState, LogViewMode, NowPlayingImageMode};
use crate::play_log::PlayLog;
use egui::{Color32, DragValue, RichText, Ui, Vec2, WidgetText};
use egui_dock::TabViewer;
use serde::{Deserialize, Serialize};
use std::time::Instant;

#[cfg(test)]
mod tests {
    use super::now_playing_rds_fallback_text;
    use super::sis_header_logo_layout;
    use crate::gui::state::ProgramRuntime;

    #[test]
    fn uses_rds_when_hd_metadata_is_empty() {
        let slot = ProgramRuntime::default();
        assert_eq!(
            now_playing_rds_fallback_text(&slot, Some("WXYZ")),
            Some("WXYZ".to_string())
        );
    }

    #[test]
    fn suppresses_rds_when_hd_metadata_is_present() {
        let mut slot = ProgramRuntime::default();
        slot.artist = "Artist".to_string();
        assert_eq!(now_playing_rds_fallback_text(&slot, Some("WXYZ")), None);
    }

    #[test]
    fn sis_header_stacks_below_breakpoint() {
        let layout = sis_header_logo_layout(699.0);
        assert!(layout.compact_header);
    }

    #[test]
    fn sis_header_side_by_side_at_breakpoint() {
        let layout = sis_header_logo_layout(700.0);
        assert!(!layout.compact_header);
    }

    #[test]
    fn sis_header_logo_clamps_to_min_size_on_tiny_width() {
        let layout = sis_header_logo_layout(100.0);
        assert!((layout.logo_size.x - 120.0).abs() < 0.01);
        assert!((layout.logo_size.y - 52.0).abs() < 0.01);
    }

    #[test]
    fn sis_header_logo_uses_available_width_in_compact_mode() {
        let layout = sis_header_logo_layout(200.0);
        assert!(layout.compact_header);
        assert!((layout.logo_size.x - 184.0).abs() < 0.01);
    }
}

#[derive(Debug, Clone, Copy)]
struct SisHeaderLogoLayout {
    compact_header: bool,
    logo_col_width: f32,
    logo_size: Vec2,
}

/// Compute responsive SIS header geometry for the logo/provenance lane.
fn sis_header_logo_layout(header_width: f32) -> SisHeaderLogoLayout {
    const LOGO_COL_MIN_WIDTH: f32 = 170.0;
    const LOGO_COL_MAX_WIDTH: f32 = 236.0;
    const LOGO_MAX_SIZE: Vec2 = Vec2::new(224.0, 80.0);
    const HEADER_STACK_BREAKPOINT: f32 = 700.0;

    let width = header_width.max(0.0);
    let compact_header = width < HEADER_STACK_BREAKPOINT;
    let logo_col_width = (width * 0.38).clamp(LOGO_COL_MIN_WIDTH, LOGO_COL_MAX_WIDTH);
    let compact_logo_width = (width - 16.0).clamp(120.0, LOGO_MAX_SIZE.x);
    let wide_logo_width = (logo_col_width - 12.0).clamp(120.0, LOGO_MAX_SIZE.x);
    let logo_width = if compact_header {
        compact_logo_width
    } else {
        wide_logo_width
    };
    let logo_height = (logo_width * (LOGO_MAX_SIZE.y / LOGO_MAX_SIZE.x)).clamp(52.0, LOGO_MAX_SIZE.y);

    SisHeaderLogoLayout {
        compact_header,
        logo_col_width,
        logo_size: Vec2::new(logo_width, logo_height),
    }
}

fn now_playing_rds_fallback_text(
    slot: &crate::gui::state::ProgramRuntime,
    rds_program_service: Option<&str>,
) -> Option<String> {
    let has_hd_metadata = !slot.artist.is_empty()
        || !slot.title.is_empty()
        || !slot.album.is_empty()
        || !slot.genre.is_empty();
    if has_hd_metadata {
        None
    } else {
        rds_program_service
            .filter(|value| !value.trim().is_empty())
            .map(str::to_string)
    }
}

#[derive(Debug, Clone)]
pub enum UiCommand {
    Start,
    Stop,
    TuneMhz(f32),
    SetRadioBand(crate::config::RadioBand),
    SelectProgram(u32),
    /// Show / hide the HD5..HD8 row of the program selector.
    /// Persisted via `AppConfig::show_hd5_hd8`.
    SetShowHd5Hd8(bool),
    /// Set how many preset slots the Tuner panel renders. Clamped
    /// to 1..=48 at apply time. Persisted via
    /// `AppConfig::preset_slot_count`.
    SetPresetSlotCount(u32),
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
    /// Drop every entry from the rolling album-art collage. Wipes the
    /// in-memory history, persisted manifest, and on-disk image cache.
    ClearCollage,
    /// Permanently block the album-art image with the given content hash.
    /// Removes it from the collage immediately and prevents any future
    /// re-appearance regardless of the LOT filename.
    BlockCover(u64),
    /// Clear the persistent album-art block list so all previously-blocked
    /// images can appear again on next arrival.
    ClearArtBlocklist,
    /// Toggle Linux fallback behavior for collage tile right-click blocking
    /// when the context-menu popup is suppressed by the host compositor.
    SetCollageSecondaryClickFallback(bool),
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
    /// Enable or disable the experimental analog-FM fallback path.
    /// Persisted via `AppConfig::analog_fallback_enabled` and applied on
    /// the next piped Start.
    SetAnalogFallbackMode(crate::config::AnalogFallbackMode),
    SetAnalogFallbackStereo(bool),
    SetAnalogFallbackRdsEnabled(bool),
    SetAnalogFallbackMerThresholdDb(Option<f32>),
    SetAmAnalogFilter(crate::config::AmAnalogFilter),
    /// v0.6.0 — override the amplitude pre-stage RMS target (dBFS).
    /// `None` clears the override and reverts to the per-device profile
    /// default. `Some(x)` is clamped to [−30, −10] on apply. Takes
    /// effect on the next cold-start tune (cache-miss path); cache hits
    /// skip AmpProbe entirely so the override is irrelevant there.
    /// Persisted via `AppConfig::agc_amp_target_dbfs_override`.
    SetAgcAmpTargetDbfs(Option<f32>),
    /// Re-enumerate attached SoapySDR devices and refresh the device
    /// picker list shown in the SDR Settings modal. Triggered by the
    /// "Refresh" button there and once when the modal is first opened.
    RefreshSdrDevices,
    /// Apply a user-chosen device from the SDR Settings modal. The
    /// payload is the full SoapySDR args string (e.g.
    /// `"driver=rtlsdr,device=1"`). Persisted to config and applied on
    /// the next piped Start.
    SelectSdrDevice {
        driver: String,
        device_args: String,
    },
    /// Set a per-element manual gain (dB) for the currently configured
    /// device. The element name is whatever the device exposes
    /// (`TUNER` for RTL-SDR, `IFGR`/`RFGR` for SDRplay, `LNA`/`VGA`/`AMP`
    /// for HackRF). Persisted into `AppConfig.sdr.gains` and applied to
    /// the live SDR if a piped stream is running.
    SetSdrGainElement {
        element: String,
        value_db: f64,
    },
    /// Set the SoapySDR frequency-correction PPM for the active device.
    /// Persisted; applied mid-stream when possible (RTL-SDR supports
    /// runtime PPM; SDRplay does not — the call no-ops there).
    SetSdrFreqCorrectionPpm(f64),
    /// Pick which antenna input the SDR uses. The payload is the
    /// Soapy antenna name as returned by `Sdr::antennas()` (e.g.
    /// `"Tuner 1 50ohm"` on RSPduo). Persisted into
    /// `AppConfig.sdr.antenna`; takes effect by restarting the
    /// active stream (antenna switching is not hot-swappable on
    /// every driver, and a clean restart is the simplest path that
    /// works across the supported set). When no stream is running
    /// the change just lands in config and applies on the next Start.
    SetSdrAntenna(String),
    /// Reset everything in the `[sdr]` config section back to default
    /// (`driver=rtlsdr`, empty args, 0 PPM, no gain overrides). The SDR
    /// Settings modal's "Reset to defaults" button.
    ResetSdrConfig,
    /// Switch which transport feeds the in-process piped pipeline.
    /// Picks local SoapySDR, SoapyRemote, or a native rtl_tcp client.
    /// Persisted to `AppConfig.sdr.transport`; applied on next Start.
    SelectSdrTransport(SdrTransport),
    /// Update the host string used by `SoapyRemote` / `RtlTcpRemote`
    /// transports. Trimmed and persisted; ignored when transport is
    /// `LocalSoapy`.
    SetSdrRemoteHost(String),
    /// Update the port used by `SoapyRemote` / `RtlTcpRemote`
    /// transports. Persisted; ignored when transport is `LocalSoapy`.
    SetSdrRemotePort(u16),
    /// Update the trailing args string appended to `SoapyRemote`
    /// connections (power-user override). Empty string clears the
    /// field. Ignored for non-SoapyRemote transports.
    SetSdrRemoteExtraArgs(String),
    /// Show the SDR Settings modal.
    ShowSdrSettings,
    /// Hide the SDR Settings modal.
    HideSdrSettings,
    /// Show the About dialog.
    ShowAbout,
    /// Hide the About dialog.
    HideAbout,
    /// Phase 3 of the v0.4.0 AGC overhaul — wipe the persisted
    /// per-frequency gain cache. Fired by the Tools/hamburger menu's
    /// "Clear gain cache…" entry. The handler shows no confirmation
    /// dialog (cache is regenerated automatically by future tunes);
    /// the menu entry text itself already calls out the consequence
    /// ("…" suffix per the agent-UI convention).
    ClearGainCache,
    /// Phase 4 — start an Opus recording locked to the currently
    /// selected program (i.e. `app_state.selected_program`). The
    /// recording target stays on that subchannel even if the user
    /// later swaps the active speaker, so they can listen to HD2 talk
    /// while recording HD1 music or vice versa. No-op when a
    /// recording is already in progress — the dock disables the
    /// button in that case.
    StartRecording,
    /// Phase 4 — stop the active recording, flush + close the .opus
    /// file, and surface the saved path in the status line. Idempotent
    /// when no recording is active.
    StopRecording,
    /// Switch the recording mode (Off / On). Persisted via
    /// `AppConfig::recording_mode`. Doesn't stop an in-progress
    /// recording — the new mode applies to the next StartRecording.
    // Kept: the App handles this command (see app.rs), but the dock
    // doesn't currently emit it — the recording-mode selector is
    // unwired, so the build flags the variant as never constructed.
    #[allow(dead_code)]
    SetRecordingMode(crate::config::RecordingMode),
    /// Set the per-file rotation cap in minutes. Snapped server-side
    /// to [1, 240]. Persisted; applies to the next file rotation.
    SetRecordingMaxMinutes(u32),
    /// Toggle the "per-station subfolder" layout for new recordings.
    /// Persisted; applies to the next StartRecording.
    SetRecordingSubfolderPerStation(bool),
    /// Set a custom output directory for new recordings. `None`
    /// reverts to `paths::default_recording_dir()`. Persisted.
    SetRecordingDir(Option<String>),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DockTab {
    Tuner,
    NowPlaying,
    /// Aggregated SIS readout for the currently tuned station — call
    /// sign, slogan, location, inferred service mode, per-program
    /// info, data services, alerts. Closed by default; opened from
    /// the toolbar's panel toggle row.
    StationInfo,
    /// Raw nrsc5 telemetry grouped for radio-nerd diagnostics.
    EngineeringInfo,
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
    pub const ALL: [DockTab; 11] = [
        DockTab::Tuner,
        DockTab::NowPlaying,
        DockTab::Signal,
        DockTab::Traffic,
        DockTab::Weather,
        DockTab::Collage,
        DockTab::StationInfo,
        DockTab::Spectrum,
        DockTab::Constellation,
        DockTab::EngineeringInfo,
        DockTab::Log,
    ];

    /// Compact label (emoji + short name) for the top-toolbar tab toggles.
    pub fn toolbar_label(&self) -> &'static str {
        match self {
            DockTab::Tuner => "\u{1F4FB} Tuner",
            DockTab::NowPlaying => "\u{1F3B5} Now Playing",
            DockTab::StationInfo => "\u{1F4DA} Station Info",
            DockTab::EngineeringInfo => "\u{1F6E0} Engineering",
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
            DockTab::StationInfo => "\u{1F4DA} Station Info".into(),
            DockTab::EngineeringInfo => "\u{1F6E0} Engineering".into(),
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
            DockTab::StationInfo => self.station_info_ui(ui),
            DockTab::EngineeringInfo => self.engineering_info_ui(ui),
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
            let mut radio_band = self.app_state.radio_band;
            ui.label(RichText::new("Band").strong());
            ui.selectable_value(
                &mut radio_band,
                crate::config::RadioBand::Fm,
                crate::config::RadioBand::Fm.label(),
            );
            ui.selectable_value(
                &mut radio_band,
                crate::config::RadioBand::Am,
                crate::config::RadioBand::Am.label(),
            );
            if radio_band != self.app_state.radio_band {
                self.commands.push(UiCommand::SetRadioBand(radio_band));
            }

            let (freq_suffix, freq_range, freq_speed, tune_step, step_label, unit_label) =
                match self.app_state.radio_band {
                    crate::config::RadioBand::Fm => (
                        " MHz",
                        87.9..=107.9,
                        0.2,
                        0.2,
                        "0.2",
                        "MHz",
                    ),
                    crate::config::RadioBand::Am => (
                        " kHz",
                        530.0..=1710.0,
                        10.0,
                        10.0,
                        "10",
                        "kHz",
                    ),
                };

            ui.label(RichText::new("Frequency").strong());
            let freq_resp = ui.add(
                DragValue::new(&mut self.app_state.frequency_mhz)
                    .speed(freq_speed)
                    .suffix(freq_suffix)
                    .range(freq_range),
            );
            // Treat <Enter> in the Frequency field as a Tune click so the
            // user doesn't have to grab the mouse after typing a freq.
            // `lost_focus()` + `Enter` is egui's idiomatic "submit" gesture
            // for text-editable widgets including `DragValue`.
            let enter_pressed = freq_resp.lost_focus()
                && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if ui
                .button("<")
                .on_hover_text(format!("Tune down {step_label} {unit_label}"))
                .clicked()
            {
                self.commands.push(UiCommand::TuneMhz(
                    self.app_state.frequency_mhz - tune_step,
                ));
            }
            if ui.button("Tune").clicked() || enter_pressed {
                self.commands
                    .push(UiCommand::TuneMhz(self.app_state.frequency_mhz));
            }
            if ui
                .button(">")
                .on_hover_text(format!("Tune up {step_label} {unit_label}"))
                .clicked()
            {
                self.commands.push(UiCommand::TuneMhz(
                    self.app_state.frequency_mhz + tune_step,
                ));
            }
        });
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Program").strong());

            // Snapshot the "which subchannels has the station advertised"
            // gate up-front so the closure doesn't borrow `self.app_state`
            // mutably while we're still reading it. Also snapshot the
            // active-speaker / decoded bitmap so each row of the selector
            // can render its lit / toggled state without juggling borrow
            // scopes per cell.
            let available = self.app_state.available_programs();
            let active_idx = self.app_state.active_idx();
            let decoded = self.app_state.decoded;
            let is_streaming = self.app_state.is_streaming;
            let show_hd5_hd8 = self.app_state.show_hd5_hd8;
            let rows: u32 = if show_hd5_hd8 { 2 } else { 1 };

            // Multi-decoder program selector. Each HD slot owns two
            // controls stacked vertically:
            //
            //   [ HD<N> ]   <-- button: set this subchannel as the
            //                  active speaker (cheap switch when the
            //                  decoder is already running; auto-spawns
            //                  one otherwise via SelectProgram's new
            //                  multi-decoder-aware handler).
            //   [ \u{25CF} ]      <-- iOS-style toggle: gate the
            //                  background decoder for this subchannel.
            //                  Independent of which one is currently
            //                  on the speakers, so the user can keep
            //                  HD1+HD2 decoding while listening to
            //                  HD2.
            //
            // The HD5\u2013HD8 row is hidden by default and revealed via
            // the "Show HD5\u2013HD8 row" checkbox in the SDR Settings
            // modal \u2014 most stations only advertise up to HD4 and the
            // second row would otherwise just sit there muted forever.
            ui.vertical(|ui| {
                for row in 0..rows {
                    self.render_program_row(ui, row, &available, active_idx, &decoded, is_streaming);
                }
            });
        });
        ui.add_space(4.0);
        ui.separator();
        ui.add_space(2.0);

        // Volume slider + mute toggle. Always enabled — the cpal-backed
        // AudioPlayer is constructed at app startup and reflects any
        // wait-free volume/mute store on the very next audio callback,
        // whether or not a station is currently tuned.
        ui.horizontal(|ui| {
            let mute_icon = if self.app_state.muted { "🔇" } else { "🔊" };
            let mute_btn = ui
                .button(RichText::new(mute_icon).size(14.0))
                .on_hover_text("Toggle mute");
            if mute_btn.clicked() {
                self.commands
                    .push(UiCommand::SetMute(!self.app_state.muted));
            }

            // Slider works in 0..=100 for display, mapped to 0.0..=1.0 internally.
            let mut percent = (self.app_state.volume * 100.0).round() as i32;
            let slider_resp = ui.add(
                egui::Slider::new(&mut percent, 0..=100)
                    .suffix("%")
                    .show_value(true),
            );
            if slider_resp.changed() {
                let new_vol = (percent as f32 / 100.0).clamp(0.0, 1.0);
                self.commands.push(UiCommand::SetVolume(new_vol));
            }
        });
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

            // Phase 4 — Record button. Locked-to-selected-subchannel
            // model: clicking captures whatever HD is selected right
            // now, and stays on it until Stop is clicked (independent
            // of speaker swaps). Enabled as soon as a stream is up;
            // there's no separate "mode" toggle anymore.
            let is_recording = self.app_state.recording.is_some();
            let recording_disabled = !self.app_state.is_streaming;
            let rec_fill = if is_recording {
                Color32::from_rgb(200, 40, 40) // bright red while live
            } else {
                Color32::from_rgb(96, 32, 32) // muted brick when idle
            };
            let rec_label = if let Some(status) = self.app_state.recording.as_ref() {
                let elapsed = status.started_at.elapsed().as_secs();
                format!(
                    "● REC HD{} {}:{:02}",
                    status.program + 1,
                    elapsed / 60,
                    elapsed % 60,
                )
            } else {
                "● Rec".to_string()
            };
            // Compact button when idle, full-width pill when armed —
            // the subchannel + timer readout needs the extra room and
            // also signals "this is now important" to the user.
            let rec_min_size = if is_recording {
                egui::vec2(120.0, 26.0)
            } else {
                egui::vec2(60.0, 26.0)
            };
            let rec_btn = ui.add_enabled(
                !recording_disabled,
                egui::Button::new(
                    RichText::new(rec_label).color(btn_text).strong(),
                )
                .fill(rec_fill)
                .min_size(rec_min_size),
            );
            let rec_btn = rec_btn.on_hover_text(if is_recording {
                self.app_state
                    .recording
                    .as_ref()
                    .map(|s| s.output_path.as_str())
                    .unwrap_or("")
                    .to_string()
            } else if recording_disabled {
                "Start a stream before recording".to_string()
            } else {
                format!(
                    "Record HD{} (locked at start; stays put across speaker swaps)",
                    self.app_state.selected_program + 1,
                )
            });
            if rec_btn.clicked() {
                if is_recording {
                    self.commands.push(UiCommand::StopRecording);
                } else {
                    self.commands.push(UiCommand::StartRecording);
                }
            }
        });

        // Preset buttons
        ui.add_space(4.0);
        ui.separator();
        ui.add_space(2.0);
        ui.label(RichText::new("Presets").strong().small());
        ui.horizontal_wrapped(|ui| {
            let accent = crate::gui::accent_color(self.app_state.dark_mode);
            let dim = Color32::from_gray(120);
            // User-configurable slot count (clamped to 1..=48 at
            // apply time in `App::handle_command`). 0 would mean
            // "no preset row at all" which we don't expose.
            let slot_count = self.app_state.preset_slot_count.clamp(1, 48) as usize;
            for i in 0..slot_count {
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
                            // 2x4 grid of HD1..HD8 — mirrors the tuner
                            // panel's layout. Always all enabled here
                            // (this is a config editor; the user may
                            // save a preset for a subchannel they
                            // haven't tuned to yet).
                            ui.vertical(|ui| {
                                for row in 0..2u32 {
                                    ui.horizontal(|ui| {
                                        for col in 0..4u32 {
                                            let sub = row * 4 + col;
                                            ui.selectable_value(
                                                &mut self.app_state.editing_preset_program,
                                                sub,
                                                format!("HD{}", sub + 1),
                                            );
                                        }
                                    });
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

    /// Render one HD-N row of the multi-decoder program selector: a
    /// horizontal strip of four `HD<N>` buttons over a second strip
    /// of four iOS-style toggle switches. The two strips are aligned
    /// so each toggle sits directly under "its" button.
    ///
    /// `row` is 0 (HD1\u2013HD4) or 1 (HD5\u2013HD8). `active_idx` is the
    /// program currently on the speakers (or `selected_program` when
    /// no session is running) \u2014 used to highlight the matching button.
    /// `decoded[i]` is the live "is this subchannel's background
    /// decoder running" gate, polled per-frame from
    /// `Nrsc5Process::decoded_programs()`; drives the toggle's bool
    /// and disables the switch when no session is running.
    fn render_program_row(
        &mut self,
        ui: &mut Ui,
        row: u32,
        available: &[bool; 8],
        active_idx: usize,
        decoded: &[bool; 8],
        is_streaming: bool,
    ) {
        // v0.5.1 single-session refactor: every advertised
        // subchannel is decoded automatically by the one libnrsc5
        // session, so the per-button toggle switches that used to
        // start / stop background decoders are gone. The row is now
        // just the four HD buttons; selecting one swaps the speaker.
        // `decoded` is consulted to keep the button lit even when SIS
        // hasn't yet advertised the subchannel but PCM is flowing.
        let _ = is_streaming;
        ui.horizontal(|ui| {
            for col in 0..4u32 {
                let i = row * 4 + col;
                let lit = available[i as usize];
                let on_air = decoded[i as usize];
                let mut text = RichText::new(format!("HD{}", i + 1));
                if !lit && !on_air {
                    text = text.weak();
                }
                let selected = active_idx as u32 == i;
                let mut resp = ui.selectable_label(selected, text);
                if !lit && !on_air {
                    resp = resp.on_hover_text(
                        "Not advertised by this station. \
                         Click to tune anyway.",
                    );
                } else if on_air {
                    resp = resp.on_hover_text("Decoding (audio on air).");
                }
                if resp.clicked() && active_idx as u32 != i {
                    self.commands.push(UiCommand::SelectProgram(i));
                }
            }
        });
        ui.add_space(2.0);
    }

    fn now_playing_ui(&mut self, ui: &mut Ui) {
        let accent = crate::gui::accent_color(self.app_state.dark_mode);
        let dim = Color32::from_gray(160);

        // Pull the active subchannel's slot so artist/title/cover follow
        // the speaker switch instead of being stuck on whichever decoder
        // emitted Metadata most recently.
        let slot = self.app_state.active_program();

        let rds_fallback_text = now_playing_rds_fallback_text(
            slot,
            self.app_state
                .station_info
                .rds_ticker_text()
                .as_deref(),
        );

        // Line 1: Artist (long station name OR song artist — changes with broadcast).
        if !slot.artist.is_empty() {
            ui.label(
                RichText::new(&slot.artist)
                    .heading()
                    .color(accent),
            );
        }

        // Line 2: Title (slogan OR song title).
        if !slot.title.is_empty() {
            ui.label(
                RichText::new(&slot.title)
                    .size(15.0)
                    .color(dim),
            );
        }

        // (Station identity line removed — the upcoming Station Information
        // panel (0.3.5) is the canonical surface for call sign / frequency /
        // subchannel. Avoids the stale-callsign bug here.)
        ui.add_space(4.0);

        let image_path = match self.app_state.now_playing_image_mode {
            NowPlayingImageMode::StationLogo => self
                .app_state
                .station_logo_paths
                .get(self.app_state.active_idx())
                .and_then(|p| p.clone())
                .filter(|path| std::path::Path::new(path.as_str()).exists()),
            NowPlayingImageMode::CoverArt => slot.cover_art_path.clone(),
        };

        if let Some(path) = image_path {
            let uri = format!("file:///{}", path.replace('\\', "/"));
            let available = ui.available_size();
            match self.app_state.now_playing_image_mode {
                NowPlayingImageMode::StationLogo => {
                    let max_w = available.x.min(300.0);
                    let max_h = available.y.min(120.0);
                    egui::Frame::new()
                        .fill(Color32::from_rgba_unmultiplied(255, 255, 255, 16))
                        .corner_radius(egui::CornerRadius::same(6))
                        .inner_margin(egui::Margin::symmetric(8, 8))
                        .show(ui, |ui| {
                            ui.add(
                                egui::Image::new(&uri)
                                    .fit_to_exact_size(Vec2::new(max_w, max_h))
                                    .corner_radius(4),
                            );
                        });
                }
                NowPlayingImageMode::CoverArt => {
                    let max_side = available.x.min(available.y).min(300.0);
                    ui.add(
                        egui::Image::new(&uri)
                            .fit_to_exact_size(Vec2::new(max_side, max_side))
                            .corner_radius(6),
                    );
                }
            }
        } else {
            let waiting = match self.app_state.now_playing_image_mode {
                NowPlayingImageMode::StationLogo => "Waiting for station logo...",
                NowPlayingImageMode::CoverArt => "Waiting for album art...",
            };
            ui.label(RichText::new(waiting).color(dim));
        }

        if let Some(rds) = rds_fallback_text {
            ui.add_space(8.0);
            // RDS-derived Artist/Title pops in statically once a field is
            // confirmed rather than scrolling — the decode is discrete and
            // a marquee here would fight the album-art layout.
            ui.label(RichText::new(&rds).size(22.0).color(accent));
        }
    }

    /// Full-width horizontal RDS ticker. The Program Service text always
    /// scrolls right-to-left, entering from the right edge and looping
    /// continuously (a second copy trails a fixed gap behind so the wrap
    /// is seamless). Painting is clipped to the allocated strip, and a
    /// repaint is requested each frame so the motion stays smooth.
    fn rds_ticker(ui: &mut Ui, text: &str, color: Color32, font_size: f32, id_salt: &str) {
        let font_id = egui::FontId::proportional(font_size);
        // Bake a separator into the repeated string so each copy carries
        // its own trailing "   |   ". The separator then scrolls along
        // with the text and marks the wrap point between repeats, and its
        // width doubles as the inter-copy gap.
        let display = format!("{text}   |   ");
        let galley = ui
            .painter()
            .layout_no_wrap(display, font_id, color);
        let text_w = galley.size().x;
        let text_h = galley.size().y;
        let desired = Vec2::new(ui.available_width(), text_h + 8.0);
        let (rect, _resp) = ui.allocate_exact_size(desired, egui::Sense::hover());
        let painter = ui.painter_at(rect);
        let y = rect.top() + (rect.height() - text_h) * 0.5;

        // Scroll right-to-left as a continuous marquee: the message
        // repeats back-to-back (the baked-in separator is the only gap),
        // so the next copy starts re-entering from the right as soon as
        // the previous one begins leaving the left — no waiting for the
        // whole message to clear the strip first.
        let period = text_w;
        let speed = 32.0; // px/sec

        // Advance a persistent offset by the frame delta instead of
        // recomputing it from absolute time. Deriving the offset from
        // `time % period` snaps the marquee whenever `period` changes
        // (i.e. every time a new PS segment lengthens the text); a
        // continuously-integrated offset stays smooth across those
        // changes. Keyed by `id_salt` so multiple tickers are independent.
        let id = ui.id().with(("rds_ticker", id_salt));
        let dt = ui.input(|i| i.stable_dt).min(0.1);
        let mut offset = ui.data(|d| d.get_temp::<f32>(id)).unwrap_or(0.0);
        offset += dt * speed;
        if period > 0.0 {
            offset = offset.rem_euclid(period);
        }
        ui.data_mut(|d| d.insert_temp(id, offset));

        // Draw as many repeating copies as needed to cover the full strip
        // width. Start one period left of the first copy so a partial
        // copy is always present on the left edge as the message wraps.
        if period > 0.0 {
            let mut x = rect.right() - offset - period;
            while x < rect.right() {
                painter.galley(egui::pos2(x, y), galley.clone(), color);
                x += period;
            }
        }
        ui.ctx().request_repaint();
    }

    /// Station Information panel. Two stacked tables:
    ///
    /// 1. **PSD (Program Service Data)** \u2014 the per-song ID3-style
    ///    metadata for whatever is currently playing on the tuned
    ///    subchannel: Song Title / Artist / Album / Genre. Rows appear
    ///    only when the field is non-empty, and the whole section is
    ///    hidden once `psd_last_updated` is older than
    ///    `AppState::PSD_STALE_AFTER` (so a stale title between songs
    ///    doesn't claim to be the current track).
    /// 2. **SIS (Station Information Service)** \u2014 the station-level
    ///    fields nrsc5 surfaces from the broadcast (call sign, slogan,
    ///    message, alert, country / FCC ID, transmitter location,
    ///    per-program advertised subchannels, data services). Each row
    ///    only renders when the underlying field is populated. The
    ///    aggregate state is cleared on retune / Stop, so we don't need
    ///    a per-field timeout here.
    ///
    /// The panel shows a single "Waiting for station data\u2026" placeholder
    /// when neither section currently has anything to show.
    fn station_info_ui(&mut self, ui: &mut Ui) {
        let accent = crate::gui::accent_color(self.app_state.dark_mode);
        let dim = Color32::from_gray(160);
        let muted = Color32::from_gray(120);
        let alert_red = Color32::from_rgb(220, 80, 80);

        let slot = self.app_state.active_program();
        let title_fresh = !slot.title.is_empty()
            && AppState::is_psd_field_fresh(slot.title_updated);
        let artist_fresh = !slot.artist.is_empty()
            && AppState::is_psd_field_fresh(slot.artist_updated);
        let album_fresh = !slot.album.is_empty()
            && AppState::is_psd_field_fresh(slot.album_updated);
        let genre_fresh = !slot.genre.is_empty()
            && AppState::is_psd_field_fresh(slot.genre_updated);
        let psd_has_any = title_fresh || artist_fresh || album_fresh || genre_fresh;
        let sis_has_any = self.app_state.station_info.has_any_data();

        if !psd_has_any && !sis_has_any {
            ui.add_space(8.0);
            ui.vertical_centered(|ui| {
                ui.label(
                    RichText::new("Waiting for station data\u{2026}")
                        .italics()
                        .color(dim),
                );
                ui.add_space(4.0);
                ui.label(
                    RichText::new(
                        "Song metadata and station identity appear here\nonce HD sync stabilizes.",
                    )
                    .small()
                    .color(muted),
                );
            });
            return;
        }

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if psd_has_any {
                    self.render_psd_section(ui, accent, dim, muted);
                }

                if psd_has_any && sis_has_any {
                    ui.add_space(10.0);
                }

                if sis_has_any {
                    self.render_sis_section(ui, accent, dim, muted, alert_red);
                }
            });

        // Keep the relative-time footers ticking and PSD staleness
        // re-evaluated even when no events are arriving.
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_secs(1));
    }

    /// PSD section \u2014 per-song metadata. Rendered as a label + two-column
    /// grid. Caller has already verified at least one field is present
    /// and within the freshness window.
    fn render_psd_section(
        &self,
        ui: &mut Ui,
        accent: Color32,
        dim: Color32,
        muted: Color32,
    ) {
        ui.label(
            RichText::new("PSD \u{2014} Program Service Data")
                .color(accent)
                .strong(),
        );
        ui.add_space(2.0);

        egui::Grid::new("station_info_psd_grid")
            .num_columns(2)
            .spacing([12.0, 3.0])
            .show(ui, |ui| {
                let slot = self.app_state.active_program();
                Self::psd_row(
                    ui,
                    "Song Title",
                    &slot.title,
                    slot.title_updated,
                    muted,
                    dim,
                );
                Self::psd_row(
                    ui,
                    "Artist",
                    &slot.artist,
                    slot.artist_updated,
                    muted,
                    dim,
                );
                Self::psd_row(
                    ui,
                    "Album",
                    &slot.album,
                    slot.album_updated,
                    muted,
                    dim,
                );
                Self::psd_row(
                    ui,
                    "Genre",
                    &slot.genre,
                    slot.genre_updated,
                    muted,
                    dim,
                );
            });

        if let Some(ts) = self.app_state.psd_latest_updated() {
            let txt = format!("PSD updated {}", Self::fmt_elapsed_bucketed(ts.elapsed().as_secs()));
            ui.add_space(2.0);
            ui.label(RichText::new(txt).small().color(muted));
        }
    }

    /// One PSD row. Skipped entirely when the value is empty OR its
    /// per-field timestamp has aged past [`AppState::PSD_STALE_AFTER`],
    /// so each row appears and disappears independently as the station
    /// keeps (or drops) the underlying ID3 frame between songs.
    fn psd_row(
        ui: &mut Ui,
        label: &str,
        value: &str,
        updated_at: Option<Instant>,
        muted: Color32,
        dim: Color32,
    ) {
        if value.is_empty() || !AppState::is_psd_field_fresh(updated_at) {
            return;
        }
        ui.label(RichText::new(label).color(muted));
        ui.label(RichText::new(value).color(dim));
        ui.end_row();
    }

    /// SIS section \u2014 station-level fields. Mirrors the structure the
    /// 0.3.5 panel originally had (alert banner / call sign + service
    /// mode / slogan / message / identity / location / subchannels /
    /// data services / updated-at footer) but each block only renders
    /// when its underlying field is populated.
    fn render_sis_section(
        &self,
        ui: &mut Ui,
        accent: Color32,
        dim: Color32,
        muted: Color32,
        alert_red: Color32,
    ) {
        let info = &self.app_state.station_info;

        ui.label(
            RichText::new("SIS \u{2014} Station Information Service")
                .color(accent)
                .strong(),
        );
        ui.add_space(2.0);

        // Alert banner pinned to the top of the SIS section when active.
        if let Some(text) = info.alert.clone() {
            egui::Frame::new()
                .fill(Color32::from_rgba_unmultiplied(220, 80, 80, 40))
                .corner_radius(egui::CornerRadius::same(4))
                .inner_margin(egui::Margin::symmetric(8, 6))
                .show(ui, |ui| {
                    ui.label(
                        RichText::new(format!("\u{26A0} {}", text))
                            .color(alert_red)
                            .strong(),
                    );
                });
            ui.add_space(6.0);
        }

        // Header row: station identity on the left, selected-program logo
        // and provenance on the right.
        let logo_path = self
            .app_state
            .station_logo_paths
            .get(self.app_state.selected_program as usize)
            .and_then(|p| p.clone())
            .filter(|path| std::path::Path::new(path.as_str()).exists());
        let logo_source = self
            .app_state
            .station_logo_sources
            .get(self.app_state.selected_program as usize)
            .and_then(|s| s.clone());
        let has_header = info.call_sign.is_some()
            || info.sync_psmi.is_some()
            || info.slogan.is_some()
            || info.message.is_some()
            || logo_path.is_some();
        if has_header {
            let header_width = ui.available_width();
            let header_layout = sis_header_logo_layout(header_width);
            let compact_header = header_layout.compact_header;
            let logo_col_width = header_layout.logo_col_width;
            let logo_size = header_layout.logo_size;

            let render_identity = |ui: &mut Ui| {
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        if let Some(call_sign) = info.call_sign.clone() {
                            ui.label(RichText::new(call_sign).heading().color(accent));
                        }
                        if let Some(psmi_badge) = info.sync_psmi_badge() {
                            ui.add_space(10.0);
                            let resp = ui.label(
                                RichText::new(format!("[{}]", psmi_badge))
                                    .small()
                                    .color(muted),
                            );
                            resp.on_hover_text(
                                "Raw PSMI from the libnrsc5 SYNC event. The app keeps this value\nas received instead of inferring service mode from SIS.",
                            );
                        }
                    });
                    if let Some(slogan) = &info.slogan {
                        ui.label(RichText::new(slogan).italics().color(dim));
                    }
                    if let Some(message) = &info.message {
                        ui.label(RichText::new(message).color(dim));
                    }
                });
            };

            let render_logo = |ui: &mut Ui, align: egui::Align| {
                if let Some(path) = logo_path.as_ref() {
                    let uri = format!("file:///{}", path.replace('\\', "/"));
                    let src = logo_source.as_deref().unwrap_or("Unknown");
                    ui.with_layout(egui::Layout::top_down(align), |ui| {
                        ui.label(
                            RichText::new(format!("Source: {src}"))
                                .small()
                                .color(muted),
                        );
                        ui.add_space(2.0);
                        egui::Frame::new()
                            .fill(Color32::from_rgba_unmultiplied(255, 255, 255, 16))
                            .corner_radius(egui::CornerRadius::same(4))
                            .inner_margin(egui::Margin::symmetric(6, 4))
                            .show(ui, |ui| {
                                ui.add(
                                    egui::Image::new(&uri)
                                        .fit_to_exact_size(logo_size)
                                        .corner_radius(3),
                                );
                            });
                    });
                }
            };

            if compact_header {
                // Narrow dock widths: stack the logo block below identity to
                // avoid compressing call-sign/slogan text to unreadability.
                render_identity(ui);
                if logo_path.is_some() {
                    ui.add_space(6.0);
                    render_logo(ui, egui::Align::LEFT);
                }
            } else {
                // Wide layouts: keep the logo/provenance lane pinned right.
                ui.allocate_ui_with_layout(
                    Vec2::new(header_width, 0.0),
                    egui::Layout::left_to_right(egui::Align::TOP).with_main_justify(true),
                    |ui| {
                        render_identity(ui);
                        if logo_path.is_some() {
                            ui.allocate_ui_with_layout(
                                Vec2::new(logo_col_width, 0.0),
                                egui::Layout::top_down(egui::Align::RIGHT),
                                |ui| {
                                    render_logo(ui, egui::Align::RIGHT);
                                },
                            );
                        }
                    },
                );
            }
        }

        // Full-width scrolling RDS ticker (analog FM fallback Program
        // Service). Driven by the accumulated segment history so
        // dynamic-PS stations produce a continuous marquee across the
        // whole panel instead of a static 8-char chip. Always shown when
        // any PS text has been received, regardless of HD metadata.
        if let Some(rds) = info.rds_ticker_text() {
            ui.add_space(6.0);
            egui::Frame::new()
                .fill(Color32::from_rgba_unmultiplied(255, 255, 255, 12))
                .corner_radius(egui::CornerRadius::same(4))
                .inner_margin(egui::Margin::symmetric(8, 6))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("RDS").small().color(muted));
                        ui.add_space(8.0);
                        Self::rds_ticker(ui, &rds, dim, 15.0, "station_info");
                    });
                });
        }

        let has_identity_row = info.country.is_some()
            || info.fcc_facility_id.is_some()
            || info.location.is_some();
        if has_identity_row {
            ui.add_space(6.0);
            ui.separator();
            ui.add_space(4.0);

            if info.country.is_some() || info.fcc_facility_id.is_some() {
                ui.horizontal(|ui| {
                    if let Some(country) = &info.country {
                        ui.label(RichText::new("Country:").color(muted));
                        ui.label(RichText::new(country).monospace());
                    }
                    if let Some(fcc) = info.fcc_facility_id {
                        ui.add_space(14.0);
                        ui.label(RichText::new("FCC ID:").color(muted));
                        ui.label(RichText::new(fcc.to_string()).monospace());
                    }
                });
            }

            if let Some(loc) = info.location {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Location:").color(muted));
                    ui.label(
                        RichText::new(format!(
                            "{:.4}\u{00B0}, {:.4}\u{00B0}  (alt {} m)",
                            loc.latitude, loc.longitude, loc.altitude_m
                        ))
                        .monospace(),
                    );
                });
            }
        }

        // Subchannels block \u2014 only when SIS has actually advertised any.
        if info.program_count() > 0 {
            ui.add_space(6.0);
            ui.separator();
            ui.add_space(4.0);
            ui.label(RichText::new("Subchannels").color(muted).small());
            egui::Grid::new("station_info_programs_grid")
                .num_columns(4)
                .spacing([10.0, 2.0])
                .show(ui, |ui| {
                    for (i, slot) in info.programs.iter().enumerate() {
                        let Some(prog) = slot else { continue };
                        ui.label(
                            RichText::new(format!("HD{}", i + 1))
                                .strong()
                                .color(accent),
                        );
                        let name_txt = if prog.short_name.is_empty() {
                            "\u{2014}".to_string()
                        } else {
                            prog.short_name.clone()
                        };
                        ui.label(RichText::new(name_txt));

                        // Per-program Now Playing cell, in place of
                        // the program_type / sound_experience pair
                        // we used to render here \u2014 those rarely
                        // populated, and the live artist/title from
                        // the active decoder for this subchannel is
                        // way more useful for the user. Pulled from
                        // the runtime slot populated by
                        // `NrscEvent::Metadata { program, .. }`, so
                        // every decoded subchannel surfaces its own
                        // current song independently of which one is
                        // on the speakers. Empty (placeholder em-dash)
                        // when nothing has been observed yet \u2014
                        // either the decoder isn't running, or PSD
                        // hasn't arrived yet.
                        let np_txt = self
                            .app_state
                            .programs
                            .get(i)
                            .map(|p| {
                                let art = p.artist.trim();
                                let titl = p.title.trim();
                                match (art.is_empty(), titl.is_empty()) {
                                    (false, false) => {
                                        format!("{} \u{2014} {}", art, titl)
                                    }
                                    (false, true) => art.to_string(),
                                    (true, false) => titl.to_string(),
                                    (true, true) => "\u{2014}".to_string(),
                                }
                            })
                            .unwrap_or_else(|| "\u{2014}".to_string());
                        ui.label(RichText::new(np_txt).color(dim).italics());

                        let bitrate_txt = prog
                            .bit_rate_kbps
                            .map(|k| format!("{:.0} kbps", k))
                            .unwrap_or_else(|| "\u{2014}".to_string());
                        ui.label(RichText::new(bitrate_txt).color(dim).monospace());
                        ui.end_row();
                    }
                });
        }

        if !info.data_services.is_empty() {
            ui.add_space(6.0);
            ui.separator();
            ui.add_space(4.0);
            ui.label(RichText::new("Data Services").color(muted).small());
            for svc in &info.data_services {
                ui.label(RichText::new(format!("#{}  {}", svc.number, svc.name)));
            }
        }

        // Equipment (exciter/importer firmware) and Local Time / Leap-second
        // blocks intentionally live only in the Engineering Info panel now —
        // they're broadcast-plant diagnostics, not listener-facing identity.

        if let Some(ts) = info.last_updated {
            ui.add_space(8.0);
            let txt = format!("SIS updated {}", Self::fmt_elapsed_bucketed(ts.elapsed().as_secs()));
            ui.label(RichText::new(txt).small().color(muted));
        }
    }

    /// Raw nrsc5 telemetry grouped for radio-nerd diagnostics.
    fn engineering_info_ui(&mut self, ui: &mut Ui) {
        let info = &self.app_state.station_info;
        let accent = Color32::from_rgb(120, 190, 255);
        let dim = Color32::from_gray(138);
        let muted = Color32::from_gray(170);
        let green = Color32::from_rgb(100, 200, 100);

        ui.label(
            RichText::new("Engineering Info \u{2014} Decoder & RF Diagnostics")
                .color(accent)
                .strong(),
        );
        ui.add_space(4.0);

        // Check what data we have. Station identity (call sign, slogan,
        // FCC ID, location) and the broadcast topology table now live
        // exclusively in the Station Info panel — this panel is the
        // pure signal/hardware diagnostics surface.
        let has_rf_health = info.sync_psmi.is_some()
            || info.sync_freq_offset_hz.is_some()
            || self.app_state.mer != 0.0
            || self.app_state.ber != 0.0;
        let has_equipment = info.exciter.is_some() || info.importer.is_some();
        let has_time = info.local_time.is_some() || info.leap_second.is_some();
        let has_live_payloads = !self.app_state.payload_log.is_empty()
            || self.app_state.traffic_map_path.is_some()
            || !self.app_state.weather_frames.is_empty()
            || self.app_state.station_logo_paths.iter().any(|p| p.is_some())
            || self.app_state.active_program().cover_art_path.is_some();

        if !(has_rf_health || has_live_payloads || has_equipment || has_time) {
            ui.centered_and_justified(|ui| {
                ui.label(
                    RichText::new("Waiting for SIS data and raw telemetry...")
                        .italics()
                        .color(muted),
                );
            });
            return;
        }

        // ============================================
        // 1. RF / DECODER HEALTH
        // ============================================
        if has_rf_health {
            ui.label(RichText::new("1. RF / Decoder Health").small().color(accent));
            
            // SYNC info
            if let Some(freq_offset_hz) = info.sync_freq_offset_hz {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Frequency offset:").color(muted));
                    ui.label(RichText::new(format!("{:.1} Hz", freq_offset_hz)).monospace().color(dim));
                });
            }
            
            if let Some(psmi) = info.sync_psmi {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("PSMI:").color(muted));
                    // AM and FM reuse overlapping PSMI values, so the band
                    // decides the prefix: MA* for AM tunes, MP* for FM.
                    // (nrsc5 defines.h: SERVICE_MODE_MA1 = 1, MA3 = 2; FM
                    // PSMI values 1/2/3/5/6/11 map directly to MP1…MP11.)
                    // `am_sync` is populated on the same SYNC event as the
                    // PSMI for AM tunes, matching `sync_psmi_label()`'s
                    // discriminator so the badge and description agree.
                    let mode_code = if info.am_sync.is_some() {
                        match psmi {
                            1 => "MA1",
                            2 => "MA3",
                            _ => "Unknown",
                        }
                    } else {
                        match psmi {
                            1 => "MP1",
                            2 => "MP2",
                            3 => "MP3",
                            5 => "MP5",
                            6 => "MP6",
                            11 => "MP11",
                            _ => "Unknown",
                        }
                    };
                    let mode_desc = info.sync_psmi_label().unwrap_or("Unknown mode");
                    let mode_text = if mode_desc.eq_ignore_ascii_case(mode_code) {
                        mode_code.to_string()
                    } else {
                        format!("{} - {}", mode_code, mode_desc)
                    };
                    let resp = ui.label(RichText::new(mode_text).monospace().color(dim));
                    resp.on_hover_text(
                        format!(
                            "Raw PSMI value = {} from the libnrsc5 SYNC event.",
                            psmi
                        ),
                    );
                });
            }
            
            // MER readout
            ui.horizontal(|ui| {
                ui.label(RichText::new("MER:").color(muted));
                let mer_txt = if self.app_state.mer > 0.0 {
                    format!("lower {:.1} dB, upper {:.1} dB", self.app_state.mer_lower, self.app_state.mer_upper)
                } else {
                    "—".to_string()
                };
                ui.label(RichText::new(mer_txt).monospace().color(if self.app_state.mer > 0.0 { dim } else { muted }));
            });
            
            // BER readout
            ui.horizontal(|ui| {
                ui.label(RichText::new("BER:").color(muted));
                let ber_txt = if self.app_state.ber > 0.0 {
                    format!("{:.2e}", self.app_state.ber)
                } else {
                    "—".to_string()
                };
                ui.label(RichText::new(ber_txt).monospace().color(if self.app_state.ber > 0.0 { dim } else { muted }));
            });
            
            // Sync status: use plain text labels so the state remains
            // legible even when emoji/symbol fonts are missing.
            let sync_status = if self.app_state.currently_synced
                || self.app_state.nrsc5_status == "synced"
            {
                ("SYNC LOCKED", green)
            } else if self.app_state.is_streaming {
                ("NO SYNC", Color32::from_rgb(200, 100, 100))
            } else {
                ("IDLE", muted)
            };
            ui.horizontal(|ui| {
                ui.label(RichText::new("Status:").color(muted));
                ui.label(
                    RichText::new(sync_status.0)
                        .monospace()
                        .color(sync_status.1),
                );
            });

            // AM indicators (only on AM mode)
            if let Some(am) = info.am_sync {
                ui.add_space(4.0);
                ui.label(RichText::new("AM Indicators:").small().color(muted));
                let rows = [
                    ("PLI", if am.pli >= 0 { if am.pli != 0 { "high" } else { "low" } } else { "unknown" }),
                    ("HPPI", if am.hppi >= 0 { if am.hppi != 0 { "high" } else { "low" } } else { "unknown" }),
                    ("AABI", if am.aabi >= 0 { if am.aabi != 0 { "8 kHz" } else { "5 kHz" } } else { "unknown" }),
                    ("RDBI", if am.rdbi >= 0 { if am.rdbi != 0 { "reduced" } else { "full" } } else { "unknown" }),
                ];

                for (label, value) in rows {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(format!("  {}:", label)).color(muted));
                        ui.label(RichText::new(value).monospace().color(dim));
                    });
                }
            }

            ui.add_space(6.0);
            ui.separator();
            ui.add_space(4.0);
        }

        // ============================================
        // 2. EQUIPMENT & TIME (combined)
        // ============================================
        if has_equipment || has_time {
            if has_equipment {
                ui.label(RichText::new("2. Equipment").small().color(accent));

                if let Some(eq) = &info.exciter {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Exciter:").color(muted));
                        let mut txt = format!(
                            "{}  core {} ({})  mfr {} ({})",
                            if eq.manufacturer_id.is_empty() { "—" } else { eq.manufacturer_id.as_str() },
                            eq.core_version_string(),
                            crate::station_info::EquipmentInfo::status_label(eq.core_status),
                            eq.manufacturer_version_string(),
                            crate::station_info::EquipmentInfo::status_label(eq.manufacturer_status),
                        );
                        if let Some(connected) = eq.importer_connected {
                            txt.push_str(if connected { "  · importer connected" } else { "  · no importer" });
                        }
                        ui.label(RichText::new(txt).monospace().color(dim));
                    });
                }

                if let Some(eq) = &info.importer {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Importer:").color(muted));
                        let txt = format!(
                            "{}  core {} ({})  mfr {} ({})",
                            if eq.manufacturer_id.is_empty() { "—" } else { eq.manufacturer_id.as_str() },
                            eq.core_version_string(),
                            crate::station_info::EquipmentInfo::status_label(eq.core_status),
                            eq.manufacturer_version_string(),
                            crate::station_info::EquipmentInfo::status_label(eq.manufacturer_status),
                        );
                        ui.label(RichText::new(txt).monospace().color(dim));
                    });
                }
            }

            if has_time {
                if has_equipment {
                    ui.add_space(4.0);
                }
                ui.label(RichText::new("Time / Leap Second").small().color(muted));

                if let Some(lt) = info.local_time {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Local time:").color(muted));
                        let dst_txt = if lt.dst_local && lt.dst_regional {
                            "DST in effect"
                        } else if lt.dst_local || lt.dst_regional {
                            "DST partial"
                        } else {
                            "no DST"
                        };
                        let txt = format!(
                            "{}  ·  {} ({} schedule)",
                            lt.offset_string(),
                            dst_txt,
                            lt.dst_schedule_label(),
                        );
                        ui.label(RichText::new(txt).monospace().color(dim));
                    });
                }

                if let Some(ls) = info.leap_second {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("GPS-UTC:").color(muted));
                        let mut txt = format!("{}s", ls.current_offset);
                        if ls.has_pending() {
                            txt.push_str(&format!(
                                "  ·  pending {}s @ ALFN {}",
                                ls.pending_offset, ls.pending_alfn
                            ));
                        }
                        let resp = ui.label(RichText::new(txt).monospace().color(dim));
                        resp.on_hover_text(
                            "Current GPS-to-UTC offset in seconds, plus any leap-second\nadjustment scheduled by the broadcaster (ALFN = absolute L1 frame number).",
                        );
                    });
                }
            }

            ui.add_space(6.0);
            ui.separator();
            ui.add_space(4.0);
        }

        // ============================================
        // 3. LIVE PAYLOADS
        // ============================================
        if has_live_payloads {
            ui.label(RichText::new("3. Live Payloads").small().color(accent));

            ui.horizontal(|ui| {
                ui.label(RichText::new("Cover art:").color(muted));
                let cover = self
                    .app_state
                    .active_program()
                    .cover_art_path
                    .as_ref()
                    .map(|_| "present")
                    .unwrap_or("none");
                ui.label(RichText::new(cover).monospace().color(dim));
            });
            ui.horizontal(|ui| {
                ui.label(RichText::new("Station logo:").color(muted));
                let logo = if self
                    .app_state
                    .station_logo_paths
                    .iter()
                    .any(|p| p.is_some())
                {
                    "present"
                } else {
                    "none"
                };
                ui.label(RichText::new(logo).monospace().color(dim));
            });
            ui.horizontal(|ui| {
                ui.label(RichText::new("Traffic Map:").color(muted));
                let traffic = self
                    .app_state
                    .traffic_map_last_updated_hhmmss
                    .as_ref()
                    .map(|ts| format!("Last Updated {}", ts))
                    .unwrap_or_else(|| "none".to_string());
                ui.label(RichText::new(traffic).monospace().color(dim));
            });
            ui.horizontal(|ui| {
                ui.label(RichText::new("Weather Map:").color(muted));
                let weather = self
                    .app_state
                    .weather_map_last_updated_hhmmss
                    .as_ref()
                    .map(|ts| format!("Last Updated {}", ts))
                    .unwrap_or_else(|| "none".to_string());
                ui.label(RichText::new(weather).monospace().color(dim));
            });

            ui.add_space(4.0);
            ui.label(RichText::new("Rolling payload log:").small().color(muted));
            egui::ScrollArea::vertical().max_height(130.0).show(ui, |ui| {
                for entry in self.app_state.payload_log.iter().rev() {
                    ui.label(
                        RichText::new(format!("- [{}] {}", entry.hhmmss, entry.text))
                            .monospace()
                            .color(dim),
                    );
                }
                if self.app_state.payload_log.is_empty() {
                    ui.label(RichText::new("- waiting for LOT/XHDR payloads...").monospace().color(muted));
                }
            });
        }
    }

    /// Bucket an elapsed time so the Station Info footers don't flicker
    /// once per second between "just now" and "1s ago". Buckets:
    /// `< 10s` -> "just now"; `10..60s` -> nearest 10s step; minutes /
    /// hours otherwise. Combined with the 1 Hz repaint, the label
    /// changes at most every 10 seconds while still tracking real time.
    fn fmt_elapsed_bucketed(secs: u64) -> String {
        if secs < 10 {
            "just now".to_string()
        } else if secs < 60 {
            format!("{}s ago", (secs / 10) * 10)
        } else if secs < 3600 {
            format!("{}m ago", secs / 60)
        } else {
            format!("{}h ago", secs / 3600)
        }
    }

    fn traffic_ui(&mut self, ui: &mut Ui) {
        let dim = Color32::from_gray(120);
        if let Some(ref path) = self.app_state.traffic_map_path {
            sync_image_texture(
                ui,
                path,
                &mut self.app_state.traffic_texture_path,
                &mut self.app_state.traffic_texture,
                "traffic_map",
            );
            let available = ui.available_size();
            ui.vertical_centered(|ui| {
                if let Some(tex) = self.app_state.traffic_texture.as_ref() {
                    let size = fit_map_size(tex.size_vec2(), available);
                    ui.add(
                        egui::Image::new(tex)
                            .fit_to_exact_size(size)
                            .corner_radius(4),
                    );
                } else {
                    ui.label(
                        RichText::new("Traffic map image unavailable")
                            .color(dim)
                            .italics(),
                    );
                }
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
        sync_image_texture(
            ui,
            &path,
            &mut self.app_state.weather_texture_path,
            &mut self.app_state.weather_texture,
            "weather_map",
        );
        let available = ui.available_size();
        // Size the image to fill the panel while preserving the frame's aspect
        // ratio (clamped against excessive upscaling). When the texture is not
        // yet loaded, fall back to a square placeholder.
        let img_size = self
            .app_state
            .weather_texture
            .as_ref()
            .map(|tex| fit_map_size(tex.size_vec2(), available))
            .unwrap_or_else(|| {
                let s = available.x.min(available.y).min(600.0).max(120.0);
                Vec2::new(s, s)
            });

        ui.vertical_centered(|ui| {
            // Allocate the image rect. The transport controls are painted on
            // top of the bottom strip as an overlay.
            let (img_rect, _resp) = ui.allocate_exact_size(img_size, egui::Sense::hover());
            if let Some(tex) = self.app_state.weather_texture.as_ref() {
                egui::Image::new(tex).corner_radius(4).paint_at(ui, img_rect);
            } else {
                ui.painter().text(
                    img_rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "Weather frame unavailable",
                    egui::FontId::proportional(14.0),
                    dim,
                );
            }

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
    /// triangle strip mesh, with a faint dB grid and a channel-raster
    /// frequency scale overlaid. Left-clicking the spectrum tunes to the
    /// clicked frequency (snapped server-side to the 200 kHz FM raster via
    /// `UiCommand::TuneMhz` handling). A center-band analog passband
    /// overlay marks which part of the spectrum is currently emphasized by
    /// the active analog path: AM follows the selected 3.5/4.2/5.0 kHz
    /// filter preset; FM uses the analog channel filter width.
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
        let is_am_band = self.app_state.radio_band == crate::config::RadioBand::Am;
        let am_target_half_span_hz =
            (self.app_state.am_analog_filter.cutoff_hz() * 6.0).clamp(18_000.0, 60_000.0);
        let sample_rate = self.app_state.spectrum_snapshot.sample_rate_sps.max(1.0);

        // Header: band-aware frequency and span readout.
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            let freq_label = if is_am_band {
                format!("{:.0} khz", self.app_state.frequency_mhz)
            } else {
                format!("{:.3} MHz", self.app_state.frequency_mhz)
            };
            ui.label(RichText::new(freq_label).monospace().strong());
            ui.add_space(8.0);
            let span_label = if is_am_band {
                format!("view {:.0} khz", (am_target_half_span_hz * 2.0) / 1000.0)
            } else {
                format!("view {:.3} MHz", sample_rate / 1_000_000.0)
            };
            ui.label(RichText::new(span_label).small().color(dim));
            ui.add_space(8.0);
            if !self.app_state.is_streaming {
                ui.label(
                    RichText::new("(stopped \u{2014} press Start to feed the FFT)")
                        .small()
                        .color(dim),
                );
            } else {
                ui.label(
                    RichText::new("Click to tune")
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
        let panel_resp = ui.allocate_rect(total_rect, Sense::click());
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

        // Per-band visible span. FM keeps the full Nyquist width; AM is
        // zoomed in around center so its much narrower analog bandwidth is
        // easier to inspect.
        if sample_rate > 1.0 {
            let nyquist = sample_rate * 0.5;
            let display_half_span_hz = if is_am_band {
                am_target_half_span_hz.min(nyquist)
            } else {
                nyquist
            };
            if display_half_span_hz <= 0.0 {
                return;
            }

            // Convert a "Hz from center" offset to a pixel x within the
            // currently visible span (which may be AM-zoomed).
            let hz_to_x = |hz: f32| -> f32 {
                let t = (hz + display_half_span_hz) / (2.0 * display_half_span_hz);
                spec_rect.left() + t.clamp(0.0, 1.0) * spec_rect.width()
            };

            // Source-aware shading: FM analog audio = center passband;
            // FM digital audio = HD sidebands. AM keeps center passband
            // when analog is active.
            let analog_source = match self.app_state.analog_fallback_mode {
                crate::config::AnalogFallbackMode::AnalogOnly => true,
                crate::config::AnalogFallbackMode::Automatic => !self.app_state.hd_audio_active,
                crate::config::AnalogFallbackMode::DigitalOnly => false,
            };
            let digital_source = self.app_state.hd_audio_active && !analog_source;

            if !is_am_band && digital_source {
                let hd_inner = 129_000.0;
                let hd_outer = 199_000.0;
                let hd_color = Color32::from_rgba_unmultiplied(80, 160, 255, 18);
                let hd_edge = Color32::from_rgba_unmultiplied(110, 190, 255, 90);

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
                for hz in [-hd_outer, -hd_inner, hd_inner, hd_outer] {
                    let x = hz_to_x(hz);
                    painter.line_segment(
                        [pos2(x, spec_rect.top()), pos2(x, spec_rect.bottom())],
                        Stroke::new(1.0, hd_edge),
                    );
                }
                painter.text(
                    pos2(spec_rect.center().x, spec_rect.top() + 4.0),
                    egui::Align2::CENTER_TOP,
                    "FM digital sidebands",
                    egui::FontId::monospace(10.0),
                    hd_edge,
                );
            } else if analog_source || is_am_band {
                let (half_bw_hz, band_label, shade_color, edge_color) = if is_am_band {
                    let bw = self.app_state.am_analog_filter.cutoff_hz();
                    (
                        bw,
                        format!("AM analog passband ±{:.1} kHz", bw / 1000.0),
                        Color32::from_rgba_unmultiplied(220, 170, 70, 24),
                        Color32::from_rgba_unmultiplied(230, 190, 90, 90),
                    )
                } else {
                    // Keep this in sync with `src/dsp/fm_analog.rs`
                    // (`CHANNEL_CUTOFF_HZ = 100_000.0`).
                    let bw = 100_000.0;
                    (
                        bw,
                        format!("FM analog passband ±{:.0} kHz", bw / 1000.0),
                        Color32::from_rgba_unmultiplied(80, 160, 255, 20),
                        Color32::from_rgba_unmultiplied(110, 190, 255, 90),
                    )
                };

                let left = hz_to_x(-half_bw_hz);
                let right = hz_to_x(half_bw_hz);
                let passband_rect = Rect::from_min_max(
                    pos2(left, spec_rect.top()),
                    pos2(right, spec_rect.bottom()),
                );

                painter.rect_filled(
                    passband_rect,
                    egui::CornerRadius::ZERO,
                    shade_color,
                );
                painter.line_segment(
                    [pos2(left, spec_rect.top()), pos2(left, spec_rect.bottom())],
                    Stroke::new(1.0, edge_color),
                );
                painter.line_segment(
                    [pos2(right, spec_rect.top()), pos2(right, spec_rect.bottom())],
                    Stroke::new(1.0, edge_color),
                );
                painter.text(
                    pos2(spec_rect.center().x, spec_rect.top() + 4.0),
                    egui::Align2::CENTER_TOP,
                    band_label,
                    egui::FontId::monospace(10.0),
                    edge_color,
                );
            }

            // Build the spectrum line + filled triangle strip. We sample only
            // the currently visible frequency window, which is AM-zoomed.
            let spec = &self.app_state.spectrum_snapshot.spectrum_db;
            let n = spec.len().max(1);
            let width = spec_rect.width().max(1.0);
            let columns = (width.round() as usize).clamp(64, n);
            let trace_color = Color32::from_rgb(220, 230, 255);
            let fill_top = Color32::from_rgba_unmultiplied(80, 150, 255, 200);
            let fill_bottom = Color32::from_rgba_unmultiplied(20, 50, 120, 30);

            let bin_hz = sample_rate / n as f32;
            let half_bins = (display_half_span_hz / bin_hz).round() as usize;
            let center_bin = n / 2;
            let vis_start = center_bin.saturating_sub(half_bins.max(1));
            let vis_end = (center_bin + half_bins.max(1)).min(n);
            let vis_n = (vis_end - vis_start).max(1);

            let mut mesh = Mesh::default();
            let mut trace_pts: Vec<Pos2> = Vec::with_capacity(columns);
            for i in 0..columns {
                let x = spec_rect.left() + (i as f32 / (columns as f32 - 1.0)) * width;
                let rel_start = (i * vis_n) / columns;
                let rel_end = (((i + 1) * vis_n) / columns)
                    .max(rel_start + 1)
                    .min(vis_n);
                let bin_start = vis_start + rel_start;
                let bin_end = vis_start + rel_end;
                let mut peak = f32::NEG_INFINITY;
                for k in bin_start..bin_end {
                    if spec[k] > peak {
                        peak = spec[k];
                    }
                }
                let y_top = db_to_y(peak);
                let y_bot = spec_rect.bottom();
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
                    let p = idx_top - 2;
                    let q = idx_top - 1;
                    let r = idx_top;
                    let s = idx_top + 1;
                    mesh.indices.extend_from_slice(&[p, q, s, p, s, r]);
                }
                trace_pts.push(pos2(x, y_top));
            }
            painter.add(Shape::mesh(mesh));
            painter.add(Shape::line(trace_pts, Stroke::new(1.2, trace_color)));

            // Frequency scale: FM keeps 200 kHz raster; AM shows local kHz ticks.
            let center_hz = self.app_state.spectrum_snapshot.center_freq_hz;
            let view_min_hz = center_hz - display_half_span_hz as f64;
            let view_max_hz = center_hz + display_half_span_hz as f64;
            if !is_am_band {
                let view_min_mhz = view_min_hz / 1_000_000.0;
                let view_max_mhz = view_max_hz / 1_000_000.0;
                const FM_BASE_MHZ: f64 = 87.9;
                const FM_STEP_MHZ: f64 = 0.2;
                if view_max_mhz > view_min_mhz {
                    let first = ((view_min_mhz - FM_BASE_MHZ) / FM_STEP_MHZ).ceil() as i32;
                    let last = ((view_max_mhz - FM_BASE_MHZ) / FM_STEP_MHZ).floor() as i32;
                    for slot in first..=last {
                        let mhz = FM_BASE_MHZ + (slot as f64) * FM_STEP_MHZ;
                        let t = ((mhz - view_min_mhz) / (view_max_mhz - view_min_mhz)).clamp(0.0, 1.0) as f32;
                        let x = spec_rect.left() + t * spec_rect.width();
                        painter.line_segment(
                            [pos2(x, spec_rect.top()), pos2(x, spec_rect.bottom())],
                            Stroke::new(0.6, Color32::from_rgb(36, 48, 70)),
                        );
                        painter.text(
                            pos2(x, spec_rect.bottom() - 2.0),
                            egui::Align2::CENTER_BOTTOM,
                            format!("{:.1}", mhz),
                            egui::FontId::monospace(10.0),
                            Color32::from_rgb(150, 170, 200),
                        );
                    }
                }
            } else {
                let view_min_khz = view_min_hz / 1_000.0;
                let view_max_khz = view_max_hz / 1_000.0;
                let step_khz = if display_half_span_hz <= 30_000.0 { 5.0 } else { 10.0 };
                let first = (view_min_khz / step_khz).ceil() as i32;
                let last = (view_max_khz / step_khz).floor() as i32;
                if view_max_khz > view_min_khz {
                    for slot in first..=last {
                        let khz = (slot as f64) * step_khz;
                        let t = ((khz - view_min_khz) / (view_max_khz - view_min_khz)).clamp(0.0, 1.0) as f32;
                        let x = spec_rect.left() + t * spec_rect.width();
                        painter.line_segment(
                            [pos2(x, spec_rect.top()), pos2(x, spec_rect.bottom())],
                            Stroke::new(0.6, Color32::from_rgb(60, 52, 34)),
                        );
                        painter.text(
                            pos2(x, spec_rect.bottom() - 2.0),
                            egui::Align2::CENTER_BOTTOM,
                            format!("{:.0}", khz),
                            egui::FontId::monospace(10.0),
                            Color32::from_rgb(190, 165, 110),
                        );
                    }
                }
            }

            // Click-to-tune in the visible span; AM uses kHz units in
            // `UiCommand::TuneMhz` despite the legacy name.
            if panel_resp.clicked() {
                if let Some(pos) = panel_resp.interact_pointer_pos() {
                    if spec_rect.contains(pos) {
                        let t = ((pos.x - spec_rect.left()) / spec_rect.width()).clamp(0.0, 1.0);
                        let clicked_hz = center_hz + (2.0 * t as f64 - 1.0) * display_half_span_hz as f64;
                        let clicked = if is_am_band {
                            (clicked_hz / 1_000.0) as f32
                        } else {
                            (clicked_hz / 1_000_000.0) as f32
                        };
                        self.commands.push(UiCommand::TuneMhz(clicked));
                    }
                }
            }
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

        // Click-to-tune is handled in the span/shading block above so AM
        // can tune in kHz while FM tunes in MHz.

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
            use crate::dsp::SearchPhase;
            // Status icons are drawn from blocks egui's default font
            // covers (General Punctuation, Latin-1) — the Geometric
            // Shapes block (● ○ ◐) renders as tofu. Color is still the
            // primary cue; the glyph is decoration that disambiguates
            // for anyone color-blind.
            //
            // While PROBING, append the search sub-phase ("coarse" or
            // "fine") so users can see whether the controller is
            // mid-sweep or mid-hill-climb. Saves a config.toml dive
            // for anyone diagnosing convergence behavior.
            let (status_text, status_color) = match snap.status {
                AgcStatus::Probing => {
                    let phase_suffix = match snap.phase {
                        SearchPhase::AmpProbe => " (amp)",
                        SearchPhase::MerQualityCheck => " (mer)",
                        SearchPhase::Coarse => " (coarse)",
                        SearchPhase::Fine => " (fine)",
                        SearchPhase::Done => "",
                    };
                    (
                        format!("\u{2026} PROBING{}", phase_suffix), // U+2026 HORIZONTAL ELLIPSIS — "in motion"
                        Color32::from_rgb(200, 160, 50),
                    )
                }
                AgcStatus::Settled => {
                    // Phase 3: surface cache-driven settle so users
                    // can correlate near-instant lock with a warm
                    // gain-cache entry vs a fresh coarse search.
                    let suffix = if snap.from_cache { " (cached)" } else { "" };
                    (
                        format!("\u{2022} SETTLED{}", suffix), // U+2022 BULLET — "locked in"
                        Color32::from_rgb(60, 170, 90),
                    )
                }
                AgcStatus::Bailed => (
                    "\u{00D7} BAILED".to_string(), // U+00D7 MULTIPLICATION SIGN — "gave up"
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

        // ----- Antenna selector -----------------------------------------
        // Only renders when the live SDR reports more than one
        // antenna input. Single-input devices (RTL-SDR Blog V3,
        // HackRF One, RSP1A) collapse this entire block to nothing.
        // Picking a new entry pushes `SetSdrAntenna` which restarts
        // the stream so the next `configure()` applies the choice
        // \u2014 brief audio gap (~250 ms), no decoder gymnastics.
        if self.app_state.sdr_antennas.len() > 1 {
            ui.separator();
            ui.label(RichText::new("Antenna").small().strong().color(dim));
            let active = self
                .app_state
                .active_antenna
                .clone()
                .unwrap_or_else(|| "<default>".to_string());
            let mut new_antenna: Option<String> = None;
            egui::ComboBox::from_id_salt("sdr_antenna_combo")
                .selected_text(active.clone())
                .width(180.0)
                .show_ui(ui, |ui| {
                    for name in &self.app_state.sdr_antennas {
                        if ui
                            .selectable_label(*name == active, name)
                            .on_hover_text(
                                "Switching antennas briefly restarts the stream.",
                            )
                            .clicked()
                        {
                            new_antenna = Some(name.clone());
                        }
                    }
                });
            if let Some(name) = new_antenna {
                self.commands.push(UiCommand::SetSdrAntenna(name));
            }
            ui.add_space(6.0);
        }

        ui.separator();
        let mut analog_fallback_mode = self.app_state.analog_fallback_mode;
        let mut analog_fallback_stereo = self.app_state.analog_fallback_stereo;
        let mut analog_fallback_rds_enabled = self.app_state.analog_fallback_rds_enabled;
        let mut analog_fallback_mer_threshold_db = self.app_state.analog_fallback_mer_threshold_db;
        let mut am_analog_filter = self.app_state.am_analog_filter;
        let is_am_band = self.app_state.radio_band == crate::config::RadioBand::Am;
        let analog_audible = self.app_state.analog_fallback_mode.is_analog_audible();
        // What's actually reaching the speakers depends on both sync *and*
        // the fallback mode. In Analog Only the audio stays on the FM path
        // even when HD locks (HD subchannels only feed metadata there), so
        // sync alone can't decide the label.
        let hd_audio_active = self.app_state.hd_audio_active;
        let source_mode = self.app_state.analog_fallback_mode;
        let mode_select_id = ui.make_persistent_id("mode_select_section");
        egui::collapsing_header::CollapsingState::load_with_default_open(
            ui.ctx(),
            mode_select_id,
            true,
        )
        .show_header(ui, |ui| {
            ui.label(RichText::new("Mode Select").small().strong().color(dim));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let (source_text, source_color) = match source_mode {
                    // Analog Only: always the analog path, regardless of HD sync.
                    crate::config::AnalogFallbackMode::AnalogOnly => {
                        if is_am_band {
                            ("Analog AM", Color32::from_rgb(200, 160, 50))
                        } else {
                            ("Analog FM", Color32::from_rgb(200, 160, 50))
                        }
                    }
                    // Digital Only: HD when locked, otherwise nothing audible.
                    crate::config::AnalogFallbackMode::DigitalOnly => {
                        if hd_audio_active {
                            ("HD Radio", Color32::from_rgb(60, 170, 90))
                        } else {
                            ("No Signal", Color32::from_rgb(200, 70, 70))
                        }
                    }
                    // Automatic: HD once locked, analog fallback until then.
                    crate::config::AnalogFallbackMode::Automatic => {
                        if hd_audio_active {
                            ("HD Radio", Color32::from_rgb(60, 170, 90))
                        } else {
                            if is_am_band {
                                ("Analog AM", Color32::from_rgb(200, 160, 50))
                            } else {
                                ("Analog FM", Color32::from_rgb(200, 160, 50))
                            }
                        }
                    }
                };
                ui.label(RichText::new(source_text).small().strong().color(source_color));
                ui.label(RichText::new("Current Source:").small().color(dim));
            });
        })
        .body(|ui| {
                // Mode row, centered.
                ui.vertical_centered(|ui| {
                    ui.horizontal(|ui| {
                        ui.selectable_value(
                            &mut analog_fallback_mode,
                            crate::config::AnalogFallbackMode::DigitalOnly,
                            crate::config::AnalogFallbackMode::DigitalOnly.label(),
                        );
                        ui.selectable_value(
                            &mut analog_fallback_mode,
                            crate::config::AnalogFallbackMode::Automatic,
                            crate::config::AnalogFallbackMode::Automatic.label(),
                        );
                        ui.selectable_value(
                            &mut analog_fallback_mode,
                            crate::config::AnalogFallbackMode::AnalogOnly,
                            crate::config::AnalogFallbackMode::AnalogOnly.label(),
                        );
                    });
                });
                if is_am_band {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("AM Filter").small().strong().color(dim));
                        for preset in [
                            crate::config::AmAnalogFilter::Narrow35,
                            crate::config::AmAnalogFilter::Medium42,
                            crate::config::AmAnalogFilter::Wide50,
                            crate::config::AmAnalogFilter::UltraWide100,
                        ] {
                            let resp = ui.add_enabled(
                                analog_audible,
                                egui::RadioButton::new(am_analog_filter == preset, preset.label()),
                            );
                            if resp.clicked() {
                                am_analog_filter = preset;
                            }
                        }
                    });
                    ui.label(
                        RichText::new("Choose how AM analog fallback behaves when HD is unavailable.")
                            .small()
                            .color(dim),
                    );
                    ui.label(
                        RichText::new("AM filter presets are active only when analog audio is audible.")
                            .small()
                            .color(dim),
                    );
                } else {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Stereo FM").small().strong().color(dim));
                        let stereo_response = ui.add_enabled(analog_audible, egui::RadioButton::new(analog_fallback_stereo, "Stereo"));
                        let mono_response = ui.add_enabled(analog_audible, egui::RadioButton::new(!analog_fallback_stereo, "Mono"));
                        if stereo_response.clicked() {
                            analog_fallback_stereo = true;
                        }
                        if mono_response.clicked() {
                            analog_fallback_stereo = false;
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("RDS").small().strong().color(dim));
                        ui.add_enabled(
                            analog_audible,
                            egui::Checkbox::new(&mut analog_fallback_rds_enabled, "On"),
                        );
                    });
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("HD→Analog MER").small().strong().color(dim));
                        let automatic =
                            analog_fallback_mode == crate::config::AnalogFallbackMode::Automatic;
                        let mut enabled = analog_fallback_mer_threshold_db.is_some();
                        let enable_resp = ui.add_enabled(
                            automatic,
                            egui::Checkbox::new(&mut enabled, "Use threshold"),
                        );
                        if enable_resp.changed() {
                            analog_fallback_mer_threshold_db = if enabled {
                                Some(analog_fallback_mer_threshold_db.unwrap_or(6.0).clamp(0.0, 30.0))
                            } else {
                                None
                            };
                        }
                        let mut threshold =
                            analog_fallback_mer_threshold_db.unwrap_or(6.0).clamp(0.0, 30.0);
                        let threshold_resp = ui.add_enabled(
                            automatic && enabled,
                            egui::DragValue::new(&mut threshold)
                                .speed(0.2)
                                .range(0.0..=30.0)
                                .suffix(" dB"),
                        );
                        if threshold_resp.changed() && enabled {
                            analog_fallback_mer_threshold_db = Some(threshold.clamp(0.0, 30.0));
                        }
                    });
                    ui.label(
                        RichText::new("Choose how the analog FM fallback behaves when HD is unavailable. Stereo/RDS are active only when analog audio is audible; MER threshold applies in Automatic mode.")
                            .small()
                            .color(dim),
                    );
                }
            });
        if analog_fallback_mode != self.app_state.analog_fallback_mode {
            self.commands.push(UiCommand::SetAnalogFallbackMode(analog_fallback_mode));
        }
        if analog_fallback_stereo != self.app_state.analog_fallback_stereo {
            self.commands.push(UiCommand::SetAnalogFallbackStereo(analog_fallback_stereo));
        }
        if analog_fallback_rds_enabled != self.app_state.analog_fallback_rds_enabled {
            self.commands.push(UiCommand::SetAnalogFallbackRdsEnabled(analog_fallback_rds_enabled));
        }
        if analog_fallback_mer_threshold_db != self.app_state.analog_fallback_mer_threshold_db {
            self.commands.push(UiCommand::SetAnalogFallbackMerThresholdDb(
                analog_fallback_mer_threshold_db,
            ));
        }
        if am_analog_filter != self.app_state.am_analog_filter {
            self.commands.push(UiCommand::SetAmAnalogFilter(am_analog_filter));
        }
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
        // Use the dedicated `currently_synced` flag (set by NrscEvent::Sync /
        // LostSync) rather than parsing the transient `nrsc5_status` string,
        // which gets clobbered to e.g. "switched to HD2" on subchannel
        // changes even though the underlying demod is still locked.
        let synced = self.app_state.is_streaming && self.app_state.currently_synced;
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
            ui.add_space(12.0);
            let clear_resp = ui
                .add(egui::Button::new("\u{1F5D1} Clear").small())
                .on_hover_text(
                    "Drop every cover from the rolling collage. \
                     Wipes the in-memory history and the on-disk \
                     art cache. Cannot be undone.",
                );
            if clear_resp.clicked() {
                self.commands.push(UiCommand::ClearCollage);
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
            // Use a real clickable Image widget (with a stable per-path ID)
            // so egui can route secondary-click context menus reliably.
            let resp = ui
                .push_id(path, |ui| {
                    ui.put(
                        outer,
                        egui::Image::new(&uri).uv(uv).sense(egui::Sense::click()),
                    )
                })
                .inner;

            // Right-click → block this image permanently.
            let hash = tile.hash;
            resp.context_menu(|ui| {
                if ui.button("\u{1F6AB} Block this image").clicked() {
                    self.commands.push(UiCommand::BlockCover(hash));
                    ui.close_menu();
                }
            });

            // Some Linux compositor/WM combinations fail to surface egui's
            // context-menu popup for image widgets even though secondary
            // clicks are delivered. Fall back to direct block-on-right-click
            // so the action still works on Ubuntu hosts.
            if cfg!(target_os = "linux")
                && self.app_state.collage_secondary_click_fallback
                && resp.secondary_clicked()
                && !resp.context_menu_opened()
            {
                self.commands.push(UiCommand::BlockCover(hash));
            }

            // Hover tooltip listing the album and every unique song seen
            // with this cover.
            if !tile.songs.is_empty() || !tile.album.is_empty() {
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
                        .color(crate::gui::accent_color(self.app_state.dark_mode)),
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

fn sync_image_texture(
    ui: &Ui,
    path: &str,
    texture_path: &mut Option<String>,
    texture: &mut Option<egui::TextureHandle>,
    texture_name: &str,
) {
    if texture_path.as_deref() == Some(path) && texture.is_some() {
        return;
    }

    let Ok(img) = image::open(path) else {
        *texture_path = None;
        *texture = None;
        return;
    };
    let rgba = img.to_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    let color = egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw());

    match texture.as_mut() {
        Some(handle) => handle.set(color, egui::TextureOptions::LINEAR),
        None => {
            *texture = Some(ui.ctx().load_texture(
                texture_name,
                color,
                egui::TextureOptions::LINEAR,
            ));
        }
    }
    *texture_path = Some(path.to_string());
}

/// Largest factor a map texture may be scaled up beyond its native pixel
/// size when filling the panel. Keeps the standard-resolution `map.png`
/// basemap from going mushy on large windows while still letting it grow
/// past the old fixed cap; the high-resolution `map2x.png` (twice the
/// pixels) can therefore fill roughly twice the on-screen area at the same
/// sharpness.
const MAP_MAX_UPSCALE: f32 = 2.0;

/// Compute the on-screen size for a map texture: scale to fit within
/// `available` while preserving the texture's aspect ratio, but never
/// upscale beyond `MAP_MAX_UPSCALE`× its native pixel size. Downscaling is
/// always permitted so the map shrinks to fit a small panel.
fn fit_map_size(tex_size: Vec2, available: Vec2) -> Vec2 {
    if tex_size.x <= 0.0 || tex_size.y <= 0.0 {
        return Vec2::ZERO;
    }
    let fit = (available.x / tex_size.x).min(available.y / tex_size.y);
    let scale = fit.min(MAP_MAX_UPSCALE).max(0.0);
    tex_size * scale
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