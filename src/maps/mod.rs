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

fn load_image_with_no_limits(path: impl AsRef<Path>) -> image::ImageResult<DynamicImage> {
    use image::ImageReader;

    let mut reader = ImageReader::open(path)?;
    reader.no_limits();
    reader.decode()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MapFeedSource {
    Unknown,
    Ttn,
    Here,
}

/// Traffic map state — collects 3×3 tiles and stitches them.
pub struct TrafficMap {
    /// The AAS dump directory where nrsc5 writes files.
    aas_dir: PathBuf,
    /// 3×3 grid of tile filenames (row, col). None = not yet received.
    tiles: [[Option<String>; 3]; 3],
    /// Path to the most recently stitched complete traffic map.
    pub completed_path: Option<String>,
    /// Monotonic suffix for output filenames so UI texture caches reload
    /// fresh composites across source switches and restitches.
    composite_counter: u64,
    source: MapFeedSource,
}

impl TrafficMap {
    pub fn new(aas_dir: &Path) -> Self {
        Self {
            aas_dir: aas_dir.to_path_buf(),
            tiles: Default::default(),
            completed_path: None,
            composite_counter: 0,
            source: MapFeedSource::Unknown,
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

        self.ensure_source(MapFeedSource::Ttn);

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

    /// Process a HERE traffic tile already written in `aas_dir/here`.
    ///
    /// `filename` should be relative to `aas_dir` (e.g. `here/HERE_...png`).
    /// `n1`/`n2` are tile row/column indices from the HERE event.
    pub fn process_here_tile(&mut self, filename: &str, n1: i32, n2: i32) -> bool {
        self.ensure_source(MapFeedSource::Here);

        // Prefer filename parsing for HERE traffic placement; observed HERE
        // `n1`/`n2` are often not 0..2 tile indices (e.g. 3,9), so they are
        // better treated as metadata.
        let (row, col) =
            if let Some(name) = Path::new(filename).file_name().and_then(|s| s.to_str()) {
                parse_here_traffic_position(name)
                    .or_else(|| {
                        if (0..=2).contains(&n1) && (0..=2).contains(&n2) {
                            Some((n1 as usize, n2 as usize))
                        } else {
                            None
                        }
                    })
                    .unwrap_or((usize::MAX, usize::MAX))
            } else {
                (usize::MAX, usize::MAX)
            };
        if row > 2 || col > 2 {
            return false;
        }

        if let Some(prev) = self.tiles[row][col].take() {
            if prev != filename {
                let _ = std::fs::remove_file(self.aas_dir.join(&prev));
            }
        }
        self.tiles[row][col] = Some(filename.to_string());

        if self.all_tiles_present() {
            if let Some(path) = self.stitch() {
                self.completed_path = Some(path);
                return true;
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
        if let Some(prev) = self.completed_path.take() {
            let _ = std::fs::remove_file(prev);
        }
        let _ = std::fs::remove_file(self.aas_dir.join("TrafficMap.png"));
        self.source = MapFeedSource::Unknown;
    }

    fn ensure_source(&mut self, source: MapFeedSource) {
        if self.source != MapFeedSource::Unknown && self.source != source {
            self.clear();
        }
        self.source = source;
    }

    fn all_tiles_present(&self) -> bool {
        self.tiles.iter().all(|row| row.iter().all(|t| t.is_some()))
    }

    fn stitch(&mut self) -> Option<String> {
        // Infer tile dimensions from the first present tile so both TTN
        // (200x200) and HERE traffic grids can share the same compositor.
        let first_name = self.tiles[0][0]
            .as_ref()
            .or(self.tiles[0][1].as_ref())
            .or(self.tiles[0][2].as_ref())
            .or(self.tiles[1][0].as_ref())
            .or(self.tiles[1][1].as_ref())
            .or(self.tiles[1][2].as_ref())
            .or(self.tiles[2][0].as_ref())
            .or(self.tiles[2][1].as_ref())
            .or(self.tiles[2][2].as_ref())?;
        let first_tile = image::open(self.aas_dir.join(first_name)).ok()?;
        let (tile_w, tile_h) = first_tile.dimensions();
        if tile_w == 0 || tile_h == 0 {
            return None;
        }

        let mut canvas = RgbaImage::new(tile_w * 3, tile_h * 3);

        for row in 0..3 {
            for col in 0..3 {
                let filename = self.tiles[row][col].as_ref()?;
                let tile_path = self.aas_dir.join(filename);
                let tile = image::open(&tile_path).ok()?;
                let tile_rgba = tile.to_rgba8();

                let (tw, th) = tile_rgba.dimensions();
                for y in 0..th.min(tile_h) {
                    for x in 0..tw.min(tile_w) {
                        let px = tile_rgba.get_pixel(x, y);
                        canvas.put_pixel(col as u32 * tile_w + x, row as u32 * tile_h + y, *px);
                    }
                }
            }
        }

        let out_name = format!("TrafficMap_{:04}.png", self.composite_counter);
        self.composite_counter = self.composite_counter.wrapping_add(1);
        let out_path = self.aas_dir.join(out_name);
        canvas.save(&out_path).ok()?;
        // Keep a stable latest alias for manual inspection on disk.
        let latest = self.aas_dir.join("TrafficMap.png");
        let _ = canvas.save(&latest);
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
    /// Path to the full US base map image (res/map2x.png when present, else res/map.png).
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
    source: MapFeedSource,
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
            source: MapFeedSource::Unknown,
        };

        // nrsc5 deduplicates LOT files it has already received, so on a station
        // we've been tuned to before, the DWRI text file may never re-arrive.
        // Pick up any cached DWRI file from a prior run so the cropped base map
        // is available for the first DWRO overlay that does arrive.
        wm.bootstrap_from_cache();
        wm
    }

    /// Scan the AAS dump dir for cached weather payloads and replay them so a
    /// restart can recover the latest weather frame from existing DWRI/DWRO files.
    fn bootstrap_from_cache(&mut self) {
        let Ok(entries) = std::fs::read_dir(&self.aas_dir) else {
            return;
        };

        let mut cached_dwri: Vec<String> = Vec::new();
        let mut cached_dwro: Vec<String> = Vec::new();
        let mut cached_basemap: Option<(std::path::PathBuf, std::time::SystemTime)> = None;

        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name_str) = name.to_str() else {
                continue;
            };
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
                cached_dwri.push(name_str.to_string());
            } else if raw.starts_with("DWRO_") {
                cached_dwro.push(name_str.to_string());
            }
        }

        for name in &cached_dwri {
            #[cfg(debug_assertions)]
            eprintln!("[map] bootstrap DWRI {}", name);
            let _ = self.process_lot(name);
            #[cfg(debug_assertions)]
            eprintln!(
                "[map] bootstrap after DWRI base_map={} area_id={:?} coords={:?}",
                self.base_map_path.is_some(),
                self.area_id,
                self.coordinates
            );
            if self.base_map_path.is_some() {
                break;
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

        if self.base_map_path.is_some() {
            for name in &cached_dwro {
                #[cfg(debug_assertions)]
                eprintln!("[map] bootstrap DWRO {}", name);
                let _ = self.process_lot(name);
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
            self.ensure_source(MapFeedSource::Ttn);
            self.parse_weather_info(filename);
            return false;
        }

        if raw.starts_with("DWRO_") {
            self.ensure_source(MapFeedSource::Ttn);
            return self.process_overlay(filename);
        }

        false
    }

    /// Process a HERE weather image already written under `aas_dir`.
    ///
    /// Unlike TTN DWRO frames (overlay + basemap), HERE weather payloads can
    /// be directly displayed as full frames, so we store them in the same
    /// rolling `frames` buffer used by the weather UI.
    pub fn process_here_image(&mut self, filename: &str, bbox: Option<[f64; 4]>) -> bool {
        self.ensure_source(MapFeedSource::Here);
        let src_path = self.aas_dir.join(filename);
        if !src_path.exists() {
            return false;
        }

        // HERE weather includes a geographic bbox on the event; use it to
        // build a basemap crop so we can render like TTN DWRO+DWRI.
        if let Some(coords) = bbox {
            let changed = self.coordinates != Some(coords);
            self.coordinates = Some(coords);
            if changed {
                self.base_map_path = None;
                let id = format!(
                    "HERE_{:.4}_{:.4}_{:.4}_{:.4}",
                    coords[0], coords[1], coords[2], coords[3]
                )
                .replace('-', "m")
                .replace('.', "p");
                self.area_id = Some(id.clone());
                self.make_base_map(&id, coords);
                if !self.frames.is_empty() {
                    for frame in &self.frames {
                        let _ = std::fs::remove_file(&frame.path);
                    }
                    self.frames.clear();
                    self.last_overlay_hash = None;
                }
            }
        }

        let Ok(bytes) = std::fs::read(&src_path) else {
            return false;
        };
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        bytes.hash(&mut hasher);
        let hash = hasher.finish();
        if self.last_overlay_hash == Some(hash) {
            return false;
        }

        let Ok(img) = image::load_from_memory(&bytes) else {
            return false;
        };
        self.last_overlay_hash = Some(hash);

        let rendered = if let Some(ref base_path) = self.base_map_path {
            if let Ok(base) = image::open(base_path) {
                let target_size = 981u32;
                let mut canvas = image::imageops::resize(
                    &base.to_rgba8(),
                    target_size,
                    target_size,
                    image::imageops::FilterType::Lanczos3,
                );
                let overlay_rgba = img.to_rgba8();
                let resized_overlay = image::imageops::resize(
                    &overlay_rgba,
                    target_size,
                    target_size,
                    image::imageops::FilterType::Lanczos3,
                );
                for y in 0..target_size {
                    for x in 0..target_size {
                        let px = resized_overlay.get_pixel(x, y);
                        let lum = (px[0] as u16 + px[1] as u16 + px[2] as u16) / 3;
                        if lum > 8 {
                            let alpha = (lum as u8).max(px[3]);
                            let base_px = canvas.get_pixel(x, y);
                            let a = alpha as f32 / 255.0;
                            let inv_a = 1.0 - a;
                            let r = (px[0] as f32 * a + base_px[0] as f32 * inv_a) as u8;
                            let g = (px[1] as f32 * a + base_px[1] as f32 * inv_a) as u8;
                            let b = (px[2] as f32 * a + base_px[2] as f32 * inv_a) as u8;
                            canvas.put_pixel(x, y, image::Rgba([r, g, b, 255]));
                        }
                    }
                }
                DynamicImage::ImageRgba8(canvas)
            } else {
                img
            }
        } else {
            img
        };

        let out_name = format!("WeatherMap_{:04}.png", self.frame_counter);
        self.frame_counter = self.frame_counter.wrapping_add(1);
        let out_path = self.aas_dir.join(&out_name);
        if rendered.save(&out_path).is_err() {
            return false;
        }

        self.frames.push(WeatherFrame {
            path: out_path.to_string_lossy().to_string(),
            captured_at: Local::now(),
        });
        while self.frames.len() > MAX_WEATHER_FRAMES {
            let old = self.frames.remove(0);
            let _ = std::fs::remove_file(&old.path);
        }
        true
    }

    pub fn clear(&mut self) {
        self.coordinates = None;
        self.area_id = None;
        self.base_map_path = None;
        self.last_overlay_hash = None;
        // Best-effort delete old composited frames so the AAS dir doesn't grow
        // unbounded across sessions.
        for frame in &self.frames {
            let _ = std::fs::remove_file(&frame.path);
        }
        self.frames.clear();
        self.source = MapFeedSource::Unknown;
    }

    fn ensure_source(&mut self, source: MapFeedSource) {
        if self.source != MapFeedSource::Unknown && self.source != source {
            self.clear();
        }
        self.source = source;
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
            } else if let Some(rest) = line.strip_prefix("Coordinates=") {
                let mut coords: Vec<f64> = Vec::new();
                for token in rest.split(';') {
                    let token = token.trim().trim_matches('"');
                    let Some(inner) = token.strip_prefix('(').and_then(|s| s.strip_suffix(')'))
                    else {
                        continue;
                    };
                    let mut nums = inner.split(',');
                    if let (Some(lat), Some(lon)) = (
                        nums.next().and_then(|s| s.trim().parse::<f64>().ok()),
                        nums.next().and_then(|s| s.trim().parse::<f64>().ok()),
                    ) {
                        coords.push(lat);
                        coords.push(lon);
                    }
                }
                if coords.len() >= 4 {
                    new_coords = Some([coords[0], coords[1], coords[2], coords[3]]);
                }
            }
        }

        if let (Some(id), Some(coords)) = (new_id, new_coords) {
            #[cfg(debug_assertions)]
            eprintln!("[map] parsed DWRI id={} coords={:?}", id, coords);
            // Rebuild base map if area changed.
            let changed = self.area_id.as_deref() != Some(&id) || self.coordinates != Some(coords);
            self.area_id = Some(id.clone());
            self.coordinates = Some(coords);
            if changed || self.base_map_path.is_none() {
                self.base_map_path = None; // force rebuild
                self.make_base_map(&id, coords);
                // Existing composited frames are for the prior area and
                // would render the new radar overlay against the wrong
                // map. Drop them and reset the dedup so the next DWRO
                // is freshly composited.
                if !self.frames.is_empty() {
                    for frame in &self.frames {
                        let _ = std::fs::remove_file(&frame.path);
                    }
                    self.frames.clear();
                    self.last_overlay_hash = None;
                }
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

        #[cfg(debug_assertions)]
        eprintln!("[map] making base map id={} map_file={:?}", id, map_file);

        let Ok(full_map) = load_image_with_no_limits(map_file) else {
            #[cfg(debug_assertions)]
            eprintln!("[map] failed to open map_file {:?}", map_file);
            return;
        };

        // Clamp to image bounds.
        let (mw, mh) = full_map.dimensions();
        let (x1, y1, x2, y2) = get_map_area(coords[0], coords[1], coords[2], coords[3], mw, mh);
        #[cfg(debug_assertions)]
        eprintln!(
            "[map] make_base_map dims={}x{} crop=({}, {}, {}, {})",
            mw, mh, x1, y1, x2, y2
        );
        let x1 = x1.max(0).min(mw as i32 - 1) as u32;
        let y1 = y1.max(0).min(mh as i32 - 1) as u32;
        let x2 = x2.max(0).min(mw as i32) as u32;
        let y2 = y2.max(0).min(mh as i32) as u32;

        if x2 <= x1 || y2 <= y1 {
            #[cfg(debug_assertions)]
            eprintln!("[map] invalid crop bounds for {}", id);
            return;
        }

        let cropped = full_map.crop_imm(x1, y1, x2 - x1, y2 - y1);
        #[cfg(debug_assertions)]
        eprintln!(
            "[map] cropped size={}x{}",
            cropped.width(),
            cropped.height()
        );
        if cropped.save(&base_path).is_ok() {
            self.base_map_path = Some(base_path.clone());
            #[cfg(debug_assertions)]
            eprintln!("[map] wrote base map {}", base_path.display());
        } else {
            #[cfg(debug_assertions)]
            eprintln!("[map] failed to write base map {}", base_path.display());
        }
    }

    fn process_overlay(&mut self, filename: &str) -> bool {
        let overlay_path = self.aas_dir.join(filename);
        if !overlay_path.exists() {
            return false;
        }

        // If we don't yet have a cropped basemap for this station,
        // refuse to composite. Otherwise the composite renders the
        // radar onto a flat dark fill (no map underneath) and those
        // frames live on in the rolling buffer until a basemap finally
        // arrives \u2014 which can be many minutes later, since nrsc5
        // dedups identical LOTs across its broadcast cycle. The UI
        // already shows "Waiting for weather radar overlay\u2026" while
        // `frames` is empty, so this is the better degraded state.
        //
        // We deliberately do NOT delete or hash-record the raw DWRO
        // here: keeping it on disk lets the next call (after a DWRI
        // arrives and `make_base_map` runs) re-attempt the composite.
        if self.base_map_path.is_none() {
            #[cfg(debug_assertions)]
            eprintln!(
                "[map] DWRO {} dropped: no basemap yet (DWRI not received)",
                filename
            );
            return false;
        }

        // Dedup: if the raw overlay bytes match the last accepted one, the
        // underlying radar imagery hasn't updated yet \u2014 skip so the animation
        // doesn't accrue identical frames.
        let Ok(bytes) = std::fs::read(&overlay_path) else {
            return false;
        };
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        bytes.hash(&mut hasher);
        let hash = hasher.finish();
        if self.last_overlay_hash == Some(hash) {
            #[cfg(debug_assertions)]
            eprintln!(
                "[map] DWRO {} dropped: identical bytes to last accepted overlay (no radar update)",
                filename
            );
            return false;
        }

        let Ok(overlay) = image::load_from_memory(&bytes) else {
            return false;
        };
        // Only commit the hash *after* we know the bytes decode \u2014 a corrupt
        // file shouldn't prevent the next attempt.
        self.last_overlay_hash = Some(hash);

        // Target size for the final weather map.
        let target_size = 981u32;

        // Load the cropped base map. The early-return above guarantees
        // `base_map_path` is set, but `image::open` can still fail (file
        // pruned externally, permissions, corrupted PNG). In that case
        // bail out so we don't fall back to the dark-fill again.
        let Some(ref base_path) = self.base_map_path else {
            return false;
        };
        let Ok(base) = load_image_with_no_limits(base_path) else {
            return false;
        };
        let mut canvas = image::imageops::resize(
            &base.to_rgba8(),
            target_size,
            target_size,
            image::imageops::FilterType::Lanczos3,
        );

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
    if (1..=3).contains(&x) && (1..=3).contains(&y) {
        // In the Python code: x=col, y=row (TMT naming is X_Y = row_column)
        // Actually looking at the Python: x = int(m.group(1))-1, y = int(m.group(2))-1
        // and paste at (j*200, i*200) where i=x, j=y → so group(1) is row, group(2) is col
        Some((x - 1, y - 1))
    } else {
        None
    }
}

/// Parse HERE traffic tile position from names like:
/// `trafficMap_<row>_<col>_<provider>.png`.
fn parse_here_traffic_position(name: &str) -> Option<(usize, usize)> {
    let stem = name.strip_suffix(".png").unwrap_or(name);
    let parts: Vec<&str> = stem.split('_').collect();
    let idx = parts.iter().position(|p| *p == "trafficMap")?;
    if parts.len() <= idx + 2 {
        return None;
    }
    let row_raw: usize = parts[idx + 1].parse().ok()?;
    let col_raw: usize = parts[idx + 2].parse().ok()?;

    let row = if row_raw <= 2 {
        row_raw
    } else if (1..=3).contains(&row_raw) {
        row_raw - 1
    } else {
        return None;
    };
    let col = if col_raw <= 2 {
        col_raw
    } else if (1..=3).contains(&col_raw) {
        col_raw - 1
    } else {
        return None;
    };

    if row <= 2 && col <= 2 {
        Some((row, col))
    } else {
        None
    }
}

/// Reference base map dimensions the Web Mercator projection constants below
/// were calibrated against (`res/map.png`). The projection scale factors
/// (`MAP_REF_X_SCALE` / `MAP_REF_Y_SCALE`) are pixels-per-projection-unit for
/// a map of exactly this size whose top-left corner sits at
/// (lat 52.48278, lon -130.78125). Higher-resolution maps such as
/// `res/map2x.png` are an exact multiple of these dimensions, so the scale is
/// adjusted by the actual/reference ratio rather than by the raw image size.
const MAP_REF_WIDTH: f64 = 12032.0;
const MAP_REF_HEIGHT: f64 = 6912.0;
/// Pixels spanning `39.34135` degrees of longitude at the reference resolution.
const MAP_REF_X_SCALE: f64 = 7162.0;
/// Pixels spanning the `top - y_ref` Mercator latitude band at the reference
/// resolution.
const MAP_REF_Y_SCALE: f64 = 3565.0;

/// Convert lat/lon bounding box to pixel coordinates on the base map image.
/// Uses Web Mercator projection. The scale constants are tied to the reference
/// map dimensions and scaled by the actual image size so the same logic works
/// for `map.png` and exact higher-resolution multiples like `map2x.png`.
fn get_map_area(
    lat1: f64,
    lon1: f64,
    lat2: f64,
    lon2: f64,
    width: u32,
    height: u32,
) -> (i32, i32, i32, i32) {
    let top = f64::asinh(f64::tan(lat_rad(52.482780)));
    let y_ref = f64::asinh(f64::tan(lat_rad(38.898)));
    let lat_span = top - y_ref;
    let width = width.max(1) as f64;
    let height = height.max(1) as f64;

    // Scale the calibrated projection constants from the reference map size to
    // this image's resolution (map2x.png is an exact 2x of map.png).
    let x_scale = MAP_REF_X_SCALE * width / MAP_REF_WIDTH;
    let y_scale = MAP_REF_Y_SCALE * height / MAP_REF_HEIGHT;

    let lat1_m = top - f64::asinh(f64::tan(lat_rad(lat1)));
    let lat2_m = top - f64::asinh(f64::tan(lat_rad(lat2)));

    let x1 = ((lon1 + 130.781250) * x_scale / 39.34135).round() as i32;
    let x2 = ((lon2 + 130.781250) * x_scale / 39.34135).round() as i32;
    let y1 = (lat1_m * y_scale / lat_span).round() as i32;
    let y2 = (lat2_m * y_scale / lat_span).round() as i32;

    (x1, y1, x2, y2)
}

fn lat_rad(deg: f64) -> f64 {
    deg * std::f64::consts::PI / 180.0
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)] // find_map_file is defined below the tests; harmless here.
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn get_map_area_scales_with_image_size() {
        let (x1, y1, x2, y2) = get_map_area(37.7, -122.4, 37.8, -122.3, 1024, 1024);
        let (x1_2x, y1_2x, x2_2x, y2_2x) = get_map_area(37.7, -122.4, 37.8, -122.3, 2048, 2048);

        // Doubling the resolution doubles the pixel coordinates, within a
        // 1px tolerance for independent rounding of each endpoint.
        assert!((x1_2x - x1 * 2).abs() <= 1, "x1: {x1_2x} vs {}", x1 * 2);
        assert!((y1_2x - y1 * 2).abs() <= 1, "y1: {y1_2x} vs {}", y1 * 2);
        assert!((x2_2x - x2 * 2).abs() <= 1, "x2: {x2_2x} vs {}", x2 * 2);
        assert!((y2_2x - y2 * 2).abs() <= 1, "y2: {y2_2x} vs {}", y2 * 2);
    }

    #[test]
    fn get_map_area_crop_in_bounds_for_reference_and_2x_maps() {
        // A real DWRI coverage box (station 02qris) that previously projected
        // off the bottom of map2x.png, collapsing the crop to a 1px strip.
        let (lat1, lon1, lat2, lon2) = (35.16645, -99.82576, 30.6434, -94.43587);

        // Reference map.png resolution.
        let (x1, y1, x2, y2) = get_map_area(lat1, lon1, lat2, lon2, 12032, 6912);
        assert!(
            x1 >= 0 && x2 <= 12032 && x1 < x2,
            "x out of bounds: {x1}..{x2}"
        );
        assert!(
            y1 >= 0 && y2 <= 6912 && y1 < y2,
            "y out of bounds: {y1}..{y2}"
        );

        // 2x map2x.png resolution: crop must be in bounds and ~2x the box.
        let (x1d, y1d, x2d, y2d) = get_map_area(lat1, lon1, lat2, lon2, 24064, 13824);
        assert!(
            x1d >= 0 && x2d <= 24064 && x1d < x2d,
            "2x x out of bounds: {x1d}..{x2d}"
        );
        assert!(
            y1d >= 0 && y2d <= 13824 && y1d < y2d,
            "2x y out of bounds: {y1d}..{y2d}"
        );
        assert!((x1d - x1 * 2).abs() <= 1, "x1: {x1d} vs {}", x1 * 2);
        assert!((y1d - y1 * 2).abs() <= 1, "y1: {y1d} vs {}", y1 * 2);
        assert!((x2d - x2 * 2).abs() <= 1, "x2: {x2d} vs {}", x2 * 2);
        assert!((y2d - y2 * 2).abs() <= 1, "y2: {y2d} vs {}", y2 * 2);

        // Crop must be a usable region, not a degenerate strip.
        assert!(
            (y2 - y1) > 100,
            "reference crop height too small: {}",
            y2 - y1
        );
    }

    #[test]
    fn load_image_with_no_limits_handles_large_png_basemap() {
        let map_file = Path::new(env!("CARGO_MANIFEST_DIR")).join("res/map2x.png");
        // map2x.png is an optional high-resolution basemap distributed
        // separately from the repo (it is gitignored), so it is absent on
        // clean checkouts such as CI. Skip rather than fail when it's missing.
        if !map_file.exists() {
            eprintln!("skipping: {map_file:?} not present (optional basemap)");
            return;
        }
        let img = load_image_with_no_limits(&map_file);
        assert!(
            img.is_ok(),
            "expected {:?} to decode: {:?}",
            map_file,
            img.err()
        );
    }

    #[test]
    fn bootstrap_from_cache_replays_existing_dwr_files() {
        let tempdir =
            std::env::temp_dir().join(format!("nrsc5-weather-bootstrap-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tempdir);
        fs::create_dir_all(&tempdir).unwrap();

        fs::write(
            tempdir.join("1119_DWRI_02qris_rev06_045f.txt"),
            "DWR_Area_ID=\"02qris\"\nCoordinates=\"(35.16645,-99.82576)\";\"(30.64340,-94.43587)\"\n",
        )
        .unwrap();

        let overlay_path = tempdir.join("1224_DWRO_02qris_rev06_20260622_2245_04c8.png");
        let overlay = image::ImageBuffer::from_fn(16, 16, |x, y| {
            if x < 8 && y < 8 {
                image::Rgba([255u8, 0u8, 0u8, 255u8])
            } else {
                image::Rgba([0u8, 0u8, 0u8, 0u8])
            }
        });
        overlay.save(&overlay_path).unwrap();

        let weather_map = WeatherMap::new(&tempdir);
        assert!(weather_map.base_map_path.is_some());
        assert!(!weather_map.frames.is_empty());

        let _ = fs::remove_dir_all(&tempdir);
    }
}

/// Locate the basemap using the same search strategy as find_nrsc5_exe:
/// exe_dir/res/, cwd/res/, and walk up from exe_dir for dev builds
/// (target/x86_64-.../debug/ → project root). Prefer map2x.png when present,
/// then fall back to map.png. On Unix, also check the standard FHS install
/// locations the Linux packages use (`/usr/share/nrsc5-studio/map.png`, etc.)
/// so a system-installed build can find the basemap without relying on $PWD.
fn find_map_file() -> Option<PathBuf> {
    let names = [
        Path::new("res").join("map2x.png"),
        Path::new("res").join("map.png"),
    ];

    // 1. Next to the executable.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            for name in &names {
                let candidate = dir.join(name);
                if candidate.exists() {
                    return Some(candidate);
                }
            }
            // Walk up from exe dir (handles target/.../debug/).
            let mut ancestor = dir.to_path_buf();
            for _ in 0..4 {
                if let Some(parent) = ancestor.parent() {
                    ancestor = parent.to_path_buf();
                    for name in &names {
                        let candidate = ancestor.join(name);
                        if candidate.exists() {
                            return Some(candidate);
                        }
                    }
                }
            }
        }
    }

    // 2. Current working directory.
    if let Ok(cwd) = std::env::current_dir() {
        for name in &names {
            let candidate = cwd.join(name);
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }

    // 3. Unix install locations. The Linux .deb and .rpm install the
    // basemap to `/usr/share/nrsc5-studio/map.png` (see Cargo.toml
    // `[package.metadata.deb]` and `[package.metadata.generate-rpm]`
    // asset lists). Also honour `XDG_DATA_DIRS` (colon-separated)
    // and `/usr/local/share` so users who install from source land
    // on a working path too.
    #[cfg(unix)]
    {
        let mut data_dirs: Vec<PathBuf> = Vec::new();
        if let Some(xdg) = std::env::var_os("XDG_DATA_DIRS") {
            for part in std::env::split_paths(&xdg) {
                data_dirs.push(part);
            }
        }
        data_dirs.push(PathBuf::from("/usr/local/share"));
        data_dirs.push(PathBuf::from("/usr/share"));
        for base in data_dirs {
            for name in ["map2x.png", "map.png"] {
                let candidate = base.join("nrsc5-studio").join(name);
                if candidate.exists() {
                    return Some(candidate);
                }
            }
        }
    }

    None
}
