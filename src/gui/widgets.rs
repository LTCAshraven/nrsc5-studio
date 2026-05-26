//! Custom egui widgets used by the dock UI.
//!
//! Currently houses just the iOS-style toggle switch used by the
//! HD program selector to gate per-subchannel background decoders.
//! Adapted from `egui_demo_lib/src/demo/toggle_switch.rs` (MIT
//! licensed, copyright (c) 2018-2024 Emil Ernerfeldt) and lightly
//! tweaked to match this app's accent palette and animation feel.

use egui::{
    pos2, vec2, Color32, Response, Sense, StrokeKind, Ui, Vec2,
};

/// Width of the toggle track relative to its height. Apple's pill is
/// roughly 1.6×; we match it so the thumb has visible travel without
/// the switch ballooning to take more horizontal space than the HD
/// button above it.
const TRACK_ASPECT: f32 = 1.6;

/// Toggle-switch height as a fraction of the current
/// `Ui::spacing().interact_size.y`. Slightly smaller than a regular
/// button so a row of switches sitting under a row of buttons reads
/// as "controls for the row above", not "competing controls of the
/// same prominence".
const TRACK_HEIGHT_FACTOR: f32 = 0.8;

/// iOS-style toggle switch. Stores its bool via `on`; returns the
/// `Response` so callers can react to clicks / hovers exactly like
/// any other widget.
///
/// Adapted from <https://github.com/emilk/egui/blob/main/crates/egui_demo_lib/src/demo/toggle_switch.rs>
/// — same geometry math, but uses this app's accent color when on
/// and a darker neutral when off, and respects the current style's
/// disabled-alpha so undecoded / unavailable subchannels render
/// muted.
pub fn toggle_switch(ui: &mut Ui, on: &mut bool) -> Response {
    let interact_h = ui.spacing().interact_size.y;
    let desired_h = interact_h * TRACK_HEIGHT_FACTOR;
    let desired_size = vec2(desired_h * TRACK_ASPECT, desired_h);
    let (rect, mut response) = ui.allocate_exact_size(desired_size, Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    response
        .widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Checkbox, ui.is_enabled(), *on, ""));

    if ui.is_rect_visible(rect) {
        // Animate the thumb position over a few frames so toggling
        // feels physical rather than instant.
        let how_on = ui.ctx().animate_bool_responsive(response.id, *on);
        let visuals = ui.style().interact_selectable(&response, *on);
        let rect = rect.expand(visuals.expansion);
        let radius = 0.5 * rect.height();

        // Track fill: app accent when on, neutral dark when off. Picked
        // explicitly (rather than borrowing `visuals.bg_fill`) so the
        // off-state contrasts crisply with the "decoded" highlight on
        // the button above without relying on egui theme drift.
        let on_color = crate::gui::accent_color(ui.visuals().dark_mode);
        let off_color = Color32::from_gray(70);
        let track_color = lerp_color(off_color, on_color, how_on);

        ui.painter().rect(
            rect,
            radius,
            track_color,
            visuals.bg_stroke,
            StrokeKind::Inside,
        );

        // Thumb: slides between left and right ends of the track.
        let thumb_x = egui::lerp((rect.left() + radius)..=(rect.right() - radius), how_on);
        let thumb_center = pos2(thumb_x, rect.center().y);
        // A touch smaller than the track radius so the track edge is
        // visible around the thumb — same look as iOS.
        let thumb_radius = radius * 0.85;
        ui.painter().circle(
            thumb_center,
            thumb_radius,
            Color32::WHITE,
            visuals.fg_stroke,
        );
    }

    response
}

/// Linear interpolation between two RGBA colors. egui doesn't expose
/// a public color-lerp helper, so we roll our own — just the four
/// channels independently, clamped to byte range by `as u8` (the
/// inputs are already bytes so no clamping is actually needed, but
/// stays robust if the input domain ever widens).
fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let inv = 1.0 - t;
    Color32::from_rgba_unmultiplied(
        (a.r() as f32 * inv + b.r() as f32 * t) as u8,
        (a.g() as f32 * inv + b.g() as f32 * t) as u8,
        (a.b() as f32 * inv + b.b() as f32 * t) as u8,
        (a.a() as f32 * inv + b.a() as f32 * t) as u8,
    )
}

/// Convenience: estimate the width the toggle will allocate so
/// callers can horizontally center it under a button without
/// asking egui mid-layout. Mirrors the math in [`toggle_switch`].
pub fn toggle_switch_size(ui: &Ui) -> Vec2 {
    let interact_h = ui.spacing().interact_size.y;
    let desired_h = interact_h * TRACK_HEIGHT_FACTOR;
    vec2(desired_h * TRACK_ASPECT, desired_h)
}
