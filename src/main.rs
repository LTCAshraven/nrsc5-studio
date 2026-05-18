// In release builds, mark the binary as a Windows GUI app so no console
// window appears when the user double-clicks the .exe. Debug builds keep the
// console so `println!` / `eprintln!` remain visible during development.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod art_cache;
mod collage;
mod config;
mod dsp;
mod ffi;
mod gui;
mod icon;
mod maps;
mod play_log;
mod sdr;
mod sdr_detect;
#[cfg(target_os = "windows")]
mod winaudio;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_app_id("nrsc5-studio")
            // Default first-launch geometry. Logical (DPI-aware) pixels, so
            // it looks consistently sized regardless of monitor DPI. eframe
            // clamps this to the available work area on smaller monitors,
            // and `persist_window: true` means user resizes stick afterward.
            .with_inner_size([1623.0, 1179.0])
            .with_min_inner_size([960.0, 600.0])
            .with_icon(icon::build_window_icon()),
        persist_window: true,
        ..Default::default()
    };
    eframe::run_native(
        "NRSC5 Studio",
        options,
        Box::new(|cc| Ok(Box::new(app::Nrsc5App::new(cc)))),
    )
}
