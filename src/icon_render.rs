//! Pure-Rust rendering of the "Broadcast Tower Glyph" mark used as the
//! application icon. Lives in its own file so it can be shared between the
//! main binary (for the runtime window icon) and `build.rs` (for embedding
//! a Windows resource icon into the .exe). Depends only on the `image`
//! crate — no egui, no other runtime types — so both contexts can include
//! it directly.

use image::{Rgba, RgbaImage};

const ACCENT_DARK: [u8; 4] = [30, 64, 175, 255];
const WHITE: [u8; 4] = [255, 255, 255, 255];
const ARC: [u8; 4] = [200, 220, 255, 255];

/// Render the icon into a square `RgbaImage` of the requested pixel size.
pub fn render(size: u32) -> RgbaImage {
    let mut img = RgbaImage::from_pixel(size, size, Rgba([0, 0, 0, 0]));

    // 1. Rounded-square background tile in the app accent colour.
    let radius = (size as f32 * 0.18) as u32;
    fill_rounded_rect(&mut img, 0, 0, size, size, radius, ACCENT_DARK);

    let s = size as f32;
    let cx = s * 0.5;

    // 2. Tower silhouette: trapezoid (wide at the base, narrow at the top).
    let base_y = s * 0.85;
    let apex_y = s * 0.46;
    let base_hw = s * 0.18;
    let apex_hw = s * 0.05;
    let trapezoid = [
        (cx - base_hw, base_y),
        (cx + base_hw, base_y),
        (cx + apex_hw, apex_y),
        (cx - apex_hw, apex_y),
    ];
    fill_polygon(&mut img, &trapezoid, WHITE);

    // 3. Short antenna mast on top of the trapezoid.
    let mast_top = s * 0.36;
    let mast_hw = s * 0.018;
    let mast = [
        (cx - mast_hw, apex_y),
        (cx + mast_hw, apex_y),
        (cx + mast_hw, mast_top),
        (cx - mast_hw, mast_top),
    ];
    fill_polygon(&mut img, &mast, WHITE);

    // 4. Cap dot on top of the mast.
    fill_circle(&mut img, cx, mast_top, s * 0.028, WHITE);

    // 5. Three concentric arc segments fanning above the antenna tip.
    //    Angles are in image-space (y-down): -130° → -50° sweeps the top.
    let stroke = (s * 0.030).max(1.0);
    let start = -130.0_f32.to_radians();
    let end = -50.0_f32.to_radians();
    for i in 1..=3 {
        let r = s * 0.07 * i as f32 + s * 0.04;
        draw_arc_band(&mut img, cx, mast_top, r, stroke, start, end, ARC);
    }

    img
}

/// Fill a rounded rectangle with the given colour. Corner is a quarter-disc
/// of the requested radius.
fn fill_rounded_rect(
    img: &mut RgbaImage,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    radius: u32,
    color: [u8; 4],
) {
    let c = Rgba(color);
    let r = radius as i32;
    let r2 = (radius * radius) as i32;
    for py in y..(y + h) {
        for px in x..(x + w) {
            let pxi = px as i32;
            let pyi = py as i32;
            // Distance from the nearest corner centre (clamped to 0 inside
            // the straight edges).
            let dx = if pxi < (x as i32) + r {
                (x as i32) + r - pxi
            } else if pxi >= (x as i32) + w as i32 - r {
                pxi - ((x as i32) + w as i32 - r - 1)
            } else {
                0
            };
            let dy = if pyi < (y as i32) + r {
                (y as i32) + r - pyi
            } else if pyi >= (y as i32) + h as i32 - r {
                pyi - ((y as i32) + h as i32 - r - 1)
            } else {
                0
            };
            if dx * dx + dy * dy <= r2 {
                img.put_pixel(px, py, c);
            }
        }
    }
}

/// Fill a convex polygon via the even-odd scanline rule.
fn fill_polygon(img: &mut RgbaImage, pts: &[(f32, f32)], color: [u8; 4]) {
    let c = Rgba(color);
    let n = pts.len();
    if n < 3 {
        return;
    }
    let ymin = pts
        .iter()
        .map(|p| p.1)
        .fold(f32::INFINITY, f32::min)
        .max(0.0)
        .floor() as i32;
    let ymax = pts
        .iter()
        .map(|p| p.1)
        .fold(f32::NEG_INFINITY, f32::max)
        .min((img.height() - 1) as f32)
        .ceil() as i32;
    for y in ymin..=ymax {
        let yf = y as f32 + 0.5;
        let mut crossings: Vec<f32> = Vec::with_capacity(n);
        for i in 0..n {
            let (x0, y0) = pts[i];
            let (x1, y1) = pts[(i + 1) % n];
            // Standard "ray from -infinity along x" test.
            let crosses = (y0 <= yf && y1 > yf) || (y1 <= yf && y0 > yf);
            if crosses {
                let t = (yf - y0) / (y1 - y0);
                crossings.push(x0 + t * (x1 - x0));
            }
        }
        crossings.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        for pair in crossings.chunks(2) {
            if pair.len() < 2 {
                break;
            }
            let x_start = pair[0].max(0.0).floor() as i32;
            let x_end = pair[1]
                .min((img.width() - 1) as f32)
                .ceil() as i32;
            if y < 0 || y >= img.height() as i32 {
                continue;
            }
            for x in x_start..=x_end {
                if x < 0 || x >= img.width() as i32 {
                    continue;
                }
                img.put_pixel(x as u32, y as u32, c);
            }
        }
    }
}

/// Fill a circle of radius `r` centred at `(cx, cy)`.
fn fill_circle(img: &mut RgbaImage, cx: f32, cy: f32, r: f32, color: [u8; 4]) {
    let c = Rgba(color);
    let r2 = r * r;
    let x0 = ((cx - r).floor() as i32).max(0);
    let x1 = ((cx + r).ceil() as i32).min(img.width() as i32 - 1);
    let y0 = ((cy - r).floor() as i32).max(0);
    let y1 = ((cy + r).ceil() as i32).min(img.height() as i32 - 1);
    for py in y0..=y1 {
        for px in x0..=x1 {
            let dx = px as f32 + 0.5 - cx;
            let dy = py as f32 + 0.5 - cy;
            if dx * dx + dy * dy <= r2 {
                img.put_pixel(px as u32, py as u32, c);
            }
        }
    }
}

/// Draw a thick arc band — every pixel whose distance from `(cx, cy)` is
/// within `±thickness/2` of `r` *and* whose angle falls inside
/// `[start_rad, end_rad]` (measured with `atan2(dy, dx)` in image space) is
/// painted with `color`.
fn draw_arc_band(
    img: &mut RgbaImage,
    cx: f32,
    cy: f32,
    r: f32,
    thickness: f32,
    start_rad: f32,
    end_rad: f32,
    color: [u8; 4],
) {
    let c = Rgba(color);
    let inner = (r - thickness * 0.5).max(0.0);
    let outer = r + thickness * 0.5;
    let inner2 = inner * inner;
    let outer2 = outer * outer;
    let x0 = ((cx - outer).floor() as i32).max(0);
    let x1 = ((cx + outer).ceil() as i32).min(img.width() as i32 - 1);
    let y0 = ((cy - outer).floor() as i32).max(0);
    let y1 = ((cy + outer).ceil() as i32).min(img.height() as i32 - 1);
    for py in y0..=y1 {
        for px in x0..=x1 {
            let dx = px as f32 + 0.5 - cx;
            let dy = py as f32 + 0.5 - cy;
            let d2 = dx * dx + dy * dy;
            if d2 < inner2 || d2 > outer2 {
                continue;
            }
            let angle = dy.atan2(dx);
            if angle >= start_rad && angle <= end_rad {
                img.put_pixel(px as u32, py as u32, c);
            }
        }
    }
}
