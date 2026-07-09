//! Render the application icon at the standard hicolor sizes so the
//! `.deb` and `.rpm` packaging metadata can pick them up as static
//! assets. Sources the same procedural renderer used by `build.rs` to
//! generate the Windows .ico resource, so the Linux and Windows builds
//! ship a visually identical icon.
//!
//! Run once after editing `src/icon_render.rs`:
//!
//!     cargo run --example render_linux_icons
//!
//! The freshly-rendered PNGs are written to
//! `packaging/linux/icons/hicolor/<size>x<size>/apps/nrsc5-studio.png`
//! and should be committed to the repository so packaging builds don't
//! need a cargo build of this example.

use std::fs;
use std::path::PathBuf;

#[path = "../src/icon_render.rs"]
mod icon_render;

const SIZES: &[u32] = &[16, 32, 48, 64, 128, 256];

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let icon_root = manifest_dir
        .join("packaging")
        .join("linux")
        .join("icons")
        .join("hicolor");

    for &size in SIZES {
        let img = icon_render::render(size);
        let dir = icon_root.join(format!("{size}x{size}")).join("apps");
        fs::create_dir_all(&dir)?;
        let out = dir.join("nrsc5-studio.png");
        img.save(&out)?;
        println!("wrote {}", out.display());
    }

    Ok(())
}
