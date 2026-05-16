//! Runtime window icon. Delegates pixel rendering to `icon_render` (shared
//! with `build.rs`) and wraps the result in `egui::IconData` so it can be
//! handed to eframe via `ViewportBuilder::with_icon`.

#[path = "icon_render.rs"]
mod render;

const SIZE: u32 = 256;

/// Build the eframe icon descriptor for the application window.
pub fn build_window_icon() -> egui::IconData {
    let img = render::render(SIZE);
    let (w, h) = img.dimensions();
    egui::IconData {
        rgba: img.into_raw(),
        width: w,
        height: h,
    }
}