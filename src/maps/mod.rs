use chrono::{DateTime, Local};
use image::{DynamicImage, GenericImageView, RgbaImage};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

/// A single composited radar frame: the file we wrote and the wall-clock time
/// we wrote it. The timestamp is what the UI uses to label the slider scrubber.
#[derive(Debug, Clone)]
pub struct WeatherFrame {
    pub path: String,
    pub captured_at: DateTime<Local>,
}

/// Traffic map state — collects 3×3 tiles and stitches them.
pub struct TrafficMap {
    /// The AAS dump directory where nrsc5 writes files.
    aas_dir: PathBuf,
    /// 3×3 grid of tile filenames (row, col). None = not yet received.
    tiles: [[Option<String>; 3]; 3],
    /// Path to the most recently stitched complete traffic map.
    pub completed_path: Option<String>,
}

impl TrafficMap {
    pub fn new(aas_dir: &Path) -> Self {
        Self {
            aas_dir: aas_dir.to_path_buf(),
            tiles: Default::default(),
            completed_path: None,
        }
    }

    /// Process a LOT filename. Returns true if a complete map was stitched.
    pub fn process_lot(&mut self, filename: &str) -> bool {
        // Strip the leading "{lot}_" prefix to get the raw name.
        let raw = match filename.find('_') {
            Some(i) => &filename[i + 1..],
            None => return false,
        };

        if !raw.starts_with("TMT_") {
            return false;
        }

        // Parse: TMT_{provider}_{X}_{Y}_{YYYYMMDD}_{HHMM}_{HEX}.png
        // We need X (1-3) and Y (1-3) to place the tile.
        if let Some((row, col)) = parse_tmt_position(raw) {
            // If we already had a tile at this grid position, the previous
            // file is dead — nrsc5 just dumped a fresher version under a
            // new LOT ID. Delete the stale one so AAS doesn't accumulate.
            if let Some(prev) = self.tiles[row][col].take() {
                if prev != filename {
                    let _ = std::fs::remove_file(self.aas_dir.join(&prev));
                }
            }
            self.tiles[row][col] = Some(filename.to_string());

            // Check if all 9 tiles are present.
            if self.all_tiles_present() {
                if let Some(path) = self.stitch() {
                    self.completed_path = Some(path);
                    return true;
                }
            }
        }
        false
    }

    /// Clear all tiles (e.g. on retune). Best-effort delete the still-tracked
    /// tile files so an old station's traffic tiles don't linger across a
    /// retune.
    pub fn clear(&mut self) {
        for row in 0..3 {
            for col in 0..3 {
                if let Some(name) = self.tiles[row][col].take() {
                    let _ = std::fs::remove_file(self.aas_dir.join(&name));
                }
            }
        }
        self.completed_path = None;
    }

    fn all_tiles_present(&self) -> bool {
        self.tiles.iter().all(|row| row.iter().all(|t| t.is_some()))
    }

    fn stitch(&self) -> Option<String> {
        // Each tile is 200×200, final image is 600×600.
        let mut canvas = RgbaImage::new(600, 600);

        for row in 0..3 {
            for col in 0..3 {
                let filename = self.tiles[row][col].as_ref()?;
                let tile_path = self.aas_dir.join(filename);
                let tile = image::open(&tile_path).ok()?;
                let tile_rgba = tile.to_rgba8();

                let (tw, th) = tile_rgba.dimensions();
                for y in 0..th.min(200) {
                    for x in 0..tw.min(200) {
                        let px = tile_rgba.get_pixel(x, y);
                        canvas.put_pixel(col as u32 * 200 + x, row as u32 * 200 + y, *px);
                    }
                }
            }
        }

        let out_path = self.aas_dir.join("TrafficMap.png");
        canvas.save(&out_path).ok()?;
        Some(out_path.to_string_lossy().to_string())
    }
}

/// Maximum number of composited weather frames kept on disk for the radar
/// animation. nrsc5 emits a new DWRO overlay roughly every 8 minutes, so 12
/// frames covers about 90 minutes of weather history.
pub const MAX_WEATHER_FRAMES: usize = 12;

/// Weather map state — composites radar overlay onto a cropped base map.
pub struct WeatherMap {
    aas_dir: PathBuf,
    /// Path to the full US base map image (res/map.png).
    map_file: Option<PathBuf>,
    /// Bounding box [lat1, lon1, lat2, lon2] from DWRI_ text file.
    pub coordinates: Option<[f64; 4]>,
    /// Area ID from DWRI_ text file.
    pub area_id: Option<String>,
    /// Path to the cached cropped base map for the current area.
    base_map_path: Option<PathBuf>,
    /// Rolling history of composited frames (oldest → newest).
    pub frames: Vec<WeatherFrame>,
    /// Monotonically increasing counter used to name unique output files so
    /// egui's image loader doesn't serve stale cached textures.
    frame_counter: u64,
    /// Hash of the most recently accepted DWRO overlay's raw bytes. nrsc5
    /// emits a fresh LOT every few minutes but the underlying radar source
    /// only updates every ~10 min, so most consecutive overlays are byte-for-
    /// byte identical — we skip those instead of cluttering the animation
    /// with duplicate frames.
    last_overlay_hash: Option<u64>,
    /// Whether the most recently composited frame used a real cropped
    /// basemap. If a DWRO arrives before any DWRI/cached basemap is
    /// available, the frame is rendered onto a dark fallback fill — we want
    /// to throw that frame away once the real basemap finally lands.
    last_frame_had_basemap: bool,
}

impl WeatherMap {
    pub fn new(aas_dir: &Path) -> Self {
        let map_file = find_map_file();

        let mut wm = Self {
            aas_dir: aas_dir.to_path_buf(),
            map_file,
            coordinates: None,
            area_id: None,
            base_map_path: None,
            frames: Vec::new(),
            frame_counter: 0,
            last_overlay_hash: None,
            last_frame_had_basemap: false,
        };

        // nrsc5 deduplicates LOT files it has already received, so on a station
        // we've been tuned to before, the DWRI text file may never re-arrive.
        // Pick up any cached DWRI file from a prior run so the cropped base map
        // is available for the first DWRO overlay that does arrive.
        wm.bootstrap_from_cache();
        wm
    }

    /// Scan the AAS dump dir for an existing `*_DWRI_*.txt` file and process it.
    fn bootstrap_from_cache(&mut self) {
        let Ok(entries) = std::fs::read_dir(&self.aas_dir) else {
            return;
        };
        // First pass: prefer a DWRI text file, which gives us both the
        // coordinates and the area id so we know the cached basemap matches
        // the current station.
        let mut cached_basemap: Option<(std::path::PathBuf, std::time::SystemTime)> = None;
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name_str) = name.to_str() else { continue };
            // Track the freshest cached BaseMap so we can fall back to it if
            // no DWRI is on disk yet. This avoids the "first DWRO arrives
            // before the broadcast cycle re-sends DWRI, so the first
            // composite frame has no basemap" bug.
            if name_str.starts_with("BaseMap_") && name_str.ends_with(".png") {
                if let Ok(meta) = entry.metadata() {
                    if let Ok(mtime) = meta.modified() {
                        match &cached_basemap {
                            Some((_, prev)) if *prev >= mtime => {}
                            _ => cached_basemap = Some((entry.path(), mtime)),
                        }
                    }
                }
                continue;
            }
            let raw = match name_str.find('_') {
                Some(i) => &name_str[i + 1..],
                None => continue,
            };
            if raw.starts_with("DWRI_") {
                self.parse_weather_info(name_str);
                if self.base_map_path.is_some() {
                    return;
                }
            }
        }
        // No DWRI on disk yet -- use the cached BaseMap as a starter. The
        // next DWRI broadcast will reconcile area_id/coordinates and (if
        // needed) rebuild the basemap for the actual station.
        if self.base_map_path.is_none() {
            if let Some((path, _)) = cached_basemap {
                self.base_map_path = Some(path);
            }
        }
    }

    /// Process a LOT filename. Returns true if a weather map was produced.
    pub fn process_lot(&mut self, filename: &str) -> bool {
        let raw = match filename.find('_') {
            Some(i) => &filename[i + 1..],
            None => return false,
        };

        if raw.starts_with("DWRI_") {
            self.parse_weather_info(filename);
            return false;
        }

        if raw.starts_with("DWRO_") {
            return self.process_overlay(filename);
        }

        false
    }

    pub fn clear(&mut self) {
        self.coordinates = None;
        self.area_id = None;
        self.base_map_path = None;
        self.last_overlay_hash = None;
        self.last_frame_had_basemap = false;
        // Best-effort delete old composited frames so the AAS dir doesn't grow
        // unbounded across sessions.
        for frame in &self.frames {
            let _ = std::fs::remove_file(&frame.path);
        }
        self.frames.clear();
    }

    fn parse_weather_info(&mut self, filename: &str) {
        let path = self.aas_dir.join(filename);
        let Ok(content) = std::fs::read_to_string(&path) else {
            return;
        };

        let mut new_id: Option<String> = None;
        let mut new_coords: Option<[f64; 4]> = None;

        for line in content.lines() {
            if let Some(rest) = line.strip_prefix("DWR_Area_ID=\"") {
                if let Some(id) = rest.strip_suffix('"') {
                    new_id = Some(id.to_string());
                }
            } else if line.starts_with("Coordinates=") {
                let coords: Vec<f64> = line
                    .split('(')
                    .skip(1)
                    .filter_map(|part| {
                        let inner = part.split(')').next()?;
                        let mut nums = inner.split(',');
                        let lat = nums.next()?.parse::<f64>().ok()?;
                        let lon = nums.next()?.parse::<f64>().ok()?;
                        Some(vec![lat, lon])
                    })
                    .flatten()
                    .collect();
                if coords.len() >= 4 {
                    new_coords = Some([coords[0], coords[1], coords[2], coords[3]]);
                }
            }
        }

        if let (Some(id), Some(coords)) = (new_id, new_coords) {
            // Rebuild base map if area changed.
            let changed = self.area_id.as_deref() != Some(&id)
                || self.coordinates != Some(coords);
            let had_basemap_before = self.base_map_path.is_some();
            self.area_id = Some(id.clone());
            self.coordinates = Some(coords);
            if changed {
                self.base_map_path = None; // force rebuild
                self.make_base_map(&id, coords);
            }
            // If we previously composited frames against the dark fallback
            // (no real basemap), they look broken — drop them now that the
            // basemap is available so the next DWRO re-renders cleanly. Also
            // clear the dedup hash so an identical DWRO will be re-accepted.
            let basemap_just_arrived = !had_basemap_before && self.base_map_path.is_some();
            if (basemap_just_arrived || changed) && !self.last_frame_had_basemap && !self.frames.is_empty() {
                for frame in &self.frames {
                    let _ = std::fs::remove_file(&frame.path);
                }
                self.frames.clear();
                self.last_overlay_hash = None;
            }
        }
    }

    /// Crop the full US map to the radar coverage area using Web Mercator projection.
    fn make_base_map(&mut self, id: &str, coords: [f64; 4]) {
        let Some(ref map_file) = self.map_file else {
            return;
        };

        let base_path = self.aas_dir.join(format!("BaseMap_{id}.png"));
        if base_path.exists() {
            self.base_map_path = Some(base_path);
            return;
        }

        let Ok(full_map) = image::open(map_file) else {
            return;
        };

        let (x1, y1, x2, y2) = get_map_area(coords[0], coords[1], coords[2], coords[3]);

        // Clamp to image bounds.
        let (mw, mh) = full_map.dimensions();
        let x1 = x1.max(0).min(mw as i32 - 1) as u32;
        let y1 = y1.max(0).min(mh as i32 - 1) as u32;
        let x2 = x2.max(0).min(mw as i32) as u32;
        let y2 = y2.max(0).min(mh as i32) as u32;

        if x2 <= x1 || y2 <= y1 {
            return;
        }

        let cropped = full_map.crop_imm(x1, y1, x2 - x1, y2 - y1);
        if cropped.save(&base_path).is_ok() {
            self.base_map_path = Some(base_path);
        }
    }

    fn process_overlay(&mut self, filename: &str) -> bool {
        let overlay_path = self.aas_dir.join(filename);
        if !overlay_path.exists() {
            return false;
        }

        // Dedup: if the raw overlay bytes match the last accepted one, the
        // underlying radar imagery hasn't updated yet — skip so the animation
        // doesn't accrue identical frames.
        let Ok(bytes) = std::fs::read(&overlay_path) else {
            return false;
        };
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        bytes.hash(&mut hasher);
        let hash = hasher.finish();
        if self.last_overlay_hash == Some(hash) {
            return false;
        }

        let Ok(overlay) = image::load_from_memory(&bytes) else {
            return false;
        };
        // Only commit the hash *after* we know the bytes decode — a corrupt
        // file shouldn't prevent the next attempt.
        self.last_overlay_hash = Some(hash);

        // Target size for the final weather map.
        let target_size = 981u32;

        // Try to load the cropped base map; fall back to a solid dark background.
        let mut had_basemap = false;
        let mut canvas = if let Some(ref base_path) = self.base_map_path {
            if let Ok(base) = image::open(base_path) {
                had_basemap = true;
                image::imageops::resize(
                    &base.to_rgba8(),
                    target_size,
                    target_size,
                    image::imageops::FilterType::Lanczos3,
                )
            } else {
                RgbaImage::from_pixel(target_size, target_size, image::Rgba([30, 30, 40, 255]))
            }
        } else {
            RgbaImage::from_pixel(target_size, target_size, image::Rgba([30, 30, 40, 255]))
        };

        // Resize the overlay to match.
        let overlay_rgba = overlay.to_rgba8();
        let resized_overlay = image::imageops::resize(
            &overlay_rgba,
            target_size,
            target_size,
            image::imageops::FilterType::Lanczos3,
        );

        // Create alpha channel from luminance (matching Python DUI approach).
        // Colored radar pixels become opaque; black/empty areas stay transparent.
        for y in 0..target_size {
            for x in 0..target_size {
                let px = resized_overlay.get_pixel(x, y);
                let lum = (px[0] as u16 + px[1] as u16 + px[2] as u16) / 3;
                if lum > 8 {
                    // Alpha-composite the radar pixel onto the base map.
                    let alpha = (lum as u8).max(px[3]);
                    let base = canvas.get_pixel(x, y);
                    let a = alpha as f32 / 255.0;
                    let inv_a = 1.0 - a;
                    let r = (px[0] as f32 * a + base[0] as f32 * inv_a) as u8;
                    let g = (px[1] as f32 * a + base[1] as f32 * inv_a) as u8;
                    let b = (px[2] as f32 * a + base[2] as f32 * inv_a) as u8;
                    canvas.put_pixel(x, y, image::Rgba([r, g, b, 255]));
                }
            }
        }

        let out_name = format!("WeatherMap_{:04}.png", self.frame_counter);
        self.frame_counter = self.frame_counter.wrapping_add(1);
        let out_path = self.aas_dir.join(&out_name);
        if DynamicImage::ImageRgba8(canvas).save(&out_path).is_err() {
            return false;
        }
        self.last_frame_had_basemap = had_basemap;
        // The raw DWRO overlay has been baked into the composited frame and
        // we never re-read it. Delete it so the AAS dir doesn't accumulate
        // ~50 KB per overlay forever across long sessions.
        let _ = std::fs::remove_file(&overlay_path);
        self.frames.push(WeatherFrame {
            path: out_path.to_string_lossy().to_string(),
            captured_at: Local::now(),
        });
        // Prune oldest frames once we exceed the rolling buffer size.
        while self.frames.len() > MAX_WEATHER_FRAMES {
            let old = self.frames.remove(0);
            let _ = std::fs::remove_file(&old.path);
        }
        true
    }
}

/// Parse TMT filename position: TMT_{provider}_{X}_{Y}_{date}_{time}_{hex}.png
/// Returns (row, col) as 0-indexed.
fn parse_tmt_position(raw: &str) -> Option<(usize, usize)> {
    // raw = "TMT_02qris_1_1_20260514_2024_0351.png"
    // Split on '_' and find the X,Y fields.
    // Pattern: TMT, provider, X, Y, date, time, hex.png
    let parts: Vec<&str> = raw.split('_').collect();
    if parts.len() < 4 {
        return None;
    }
    // parts[0] = "TMT", parts[1] = provider, parts[2] = X, parts[3] = Y
    let x: usize = parts[2].parse().ok()?;
    let y: usize = parts[3].parse().ok()?;
    if x >= 1 && x <= 3 && y >= 1 && y <= 3 {
        // In the Python code: x=col, y=row (TMT naming is X_Y = row_column)
        // Actually looking at the Python: x = int(m.group(1))-1, y = int(m.group(2))-1
        // and paste at (j*200, i*200) where i=x, j=y → so group(1) is row, group(2) is col
        Some((x - 1, y - 1))
    } else {
        None
    }
}

/// Convert lat/lon bounding box to pixel coordinates on the base map image.
/// Uses Web Mercator projection with constants calibrated for the shipped map.png.
/// Ported directly from the Python DUI's getMapArea() (credit: hdfm project).
fn get_map_area(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> (i32, i32, i32, i32) {
    let top = f64::asinh(f64::tan(lat_rad(52.482780)));
    let y_ref = f64::asinh(f64::tan(lat_rad(38.898)));

    let lat1_m = top - f64::asinh(f64::tan(lat_rad(lat1)));
    let lat2_m = top - f64::asinh(f64::tan(lat_rad(lat2)));

    let x1 = ((lon1 + 130.781250) * 7162.0 / 39.34135).round() as i32;
    let x2 = ((lon2 + 130.781250) * 7162.0 / 39.34135).round() as i32;
    let y1 = (lat1_m * 3565.0 / (top - y_ref)).round() as i32;
    let y2 = (lat2_m * 3565.0 / (top - y_ref)).round() as i32;

    (x1, y1, x2, y2)
}

fn lat_rad(deg: f64) -> f64 {
    deg * std::f64::consts::PI / 180.0
}

/// Locate res/map.png using the same search strategy as find_nrsc5_exe:
/// exe_dir/res/, cwd/res/, and walk up from exe_dir for dev builds
/// (target/x86_64-.../debug/ → project root).
fn find_map_file() -> Option<PathBuf> {
    let name = Path::new("res").join("map.png");

    // 1. Next to the executable.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join(&name);
            if candidate.exists() {
                return Some(candidate);
            }
            // Walk up from exe dir (handles target/.../debug/).
            let mut ancestor = dir.to_path_buf();
            for _ in 0..4 {
                if let Some(parent) = ancestor.parent() {
                    ancestor = parent.to_path_buf();
                    let candidate = ancestor.join(&name);
                    if candidate.exists() {
                        return Some(candidate);
                    }
                }
            }
        }
    }

    // 2. Current working directory.
    if let Ok(cwd) = std::env::current_dir() {
        let candidate = cwd.join(&name);
        if candidate.exists() {
            return Some(candidate);
        }
    }

    None
}
