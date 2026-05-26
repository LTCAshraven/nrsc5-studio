pub mod dock;
pub mod state;
pub mod widgets;

/// App accent color, picked to remain legible in both themes.
///
/// Dark mode uses a soft cool-blue that glows nicely on the near-black
/// chrome. Light mode uses a noticeably darker, more saturated blue
/// because the soft variant gets washed out against an off-white
/// panel — the lighter shade only has ~2.3:1 contrast on white,
/// which fails WCAG-AA for body text. The darker shade reaches
/// ~5.5:1, well into AA territory for the small tab labels and
/// in-line callouts that use this color.
pub fn accent_color(dark: bool) -> egui::Color32 {
    if dark {
        egui::Color32::from_rgb(100, 160, 255)
    } else {
        egui::Color32::from_rgb(28, 100, 210)
    }
}
