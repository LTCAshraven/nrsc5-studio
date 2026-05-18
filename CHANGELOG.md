# Changelog

All notable changes to NRSC5 Studio are documented here. The format roughly
follows [Keep a Changelog](https://keepachangelog.com/), and the project
adheres to [Semantic Versioning](https://semver.org/).

## [0.2.2] - 2026-05-18

A polish + portability release on top of 0.2.1. No architectural
changes — the piped-SDR backend, closed-loop AGC, and spectrum panel
behave identically.

### Added

- **Portable mode.** A zero-byte `portable.txt` next to
  `nrsc5-studio.exe` redirects every persistent path
  (`config.toml`, AAS file cache, album-art cache, play log, egui
  window-state DB) from `%APPDATA%\nrsc5-studio\` into a
  `./data/` folder next to the executable. New module
  `src/paths.rs` owns the portable-vs-roaming dispatch and is
  the single source of truth for all paths; callers go through
  `paths::config_path()`, `paths::play_log_path()`,
  `paths::aas_dir()`, `paths::art_cache_db()`, etc.
- **Portable-zip wiring.** `scripts/package-portable.ps1` now
  seeds `portable.txt` and a fresh `./data/` next to the exe so
  the shipped zip is self-contained.
- **`eframe::NativeOptions::persistence_path`** wired to
  `paths::egui_persistence_db()` so window state honors portable
  mode alongside the rest of the per-install data.
- **Configurable play-log retention.** New
  `play_log_retention_hours` config field (1..168, default 24).
  Surfaced in the `📝 Log` tab as a **Rolling window** dropdown
  with seven choices (1h / 6h / 12h / 1d / 2d / 3d / 7d).
  Persisted to `config.toml` and applied on the next prune cycle.
- **Clear log button** in the `📝 Log` tab — wipes both the
  in-memory log and the on-disk `play_log.csv`.
- **Native Save As dialog for CSV export** (replaces the silent
  fixed-path save). Powered by `rfd 0.15`.
- **Glyph audit script** (`scripts/probe-glyphs.ps1`). Reads each
  bundled TTF's `cmap` via `System.Windows.Media.GlyphTypeface`
  and prints a per-codepoint coverage table. Reproducible audit
  for any future emoji additions.

### Changed

- **`use_piped_sdr` default → `true`.** Fresh installs now ship
  with the in-process piped backend enabled out of the box. Both
  the closed-loop AGC and the spectrum FFT tap are wired only
  through `start_piped`, so the legacy USB default silently
  disabled the v0.2.x flagship features. With the corrected
  default, AGC and spectrum work on first launch without
  editing the config file.
- **Proportional font fallback.** Appended `Hack-Regular.ttf`
  to egui's default `FontFamily::Proportional` chain via a new
  `Nrsc5App::install_fonts` step. egui's stock proportional
  chain (Ubuntu-Light → NotoEmoji → emoji-icon-font) excludes
  Hack, so geometric shapes (●, ○, ■, □, →, ▸, ◆) rendered
  as tofu in label text. Now they resolve to Hack as a final
  fallback without affecting Latin letter selection.
- **Retention dropdown selected indicator** changed from `✓`
  (U+2713, uncovered by any bundled font) to `✔` (U+2714,
  covered by NotoEmoji + emoji-icon-font).
- **`play_log::Log`** gained `retention_hours`, `set_retention_hours`,
  `clear_all`, plus `RETENTION_CHOICES`, `MIN_RETENTION_HOURS`,
  `MAX_RETENTION_HOURS`, `DEFAULT_RETENTION_HOURS`, and
  `clamp_retention`.

### Fixed

- **Dark mode pin.** Explicit `egui::ThemePreference` set during
  theme install so a saved `dark_mode = true` is honored on a
  light-OS desktop (and vice versa). Previously the OS theme
  could override the saved preference.
- **Stale call sign cleared on Stop / TuneMhz.** Retuning to a
  station that doesn't broadcast a SIS call sign no longer
  leaves the previous letters frozen in the Tuner panel.
- **Weather radar without basemap.** `WeatherMap::process_overlay`
  now bails early when no basemap is in hand instead of
  compositing radar onto a transparent background. Frame state
  is reset on basemap change so the loop replays correctly.
- **Album-art cache invalidation.** Switching backends or
  stopping a stream now clears stale tile state so a previous
  cover doesn't linger.

### Internal

- New `UiCommand::ClearLog` and
  `UiCommand::SetPlayLogRetention(u32)` variants; both round-trip
  through `AppConfig`.
- `AppConfig::sanitize` clamps `play_log_retention_hours` on load
  via `play_log::clamp_retention`.
- `rfd 0.15` added as a dependency for native file dialogs.

## [0.2.1] - 2026-05-18

Closed-loop AGC for the piped-SDR backend, plus a user-facing gain
mode picker in the Signal panel. No architectural changes from 0.2.0.

### Added

- **Closed-loop AGC controller** (`src/dsp/agc.rs`). Explored-set
  hill-climber over the 29-step R820T2 gain table (0.0 dB → 49.6 dB).
  ~5 s probe period per step, 15-probe bail budget, MER metric is
  `min(MER_lower, MER_upper)` EMA-smoothed (α = 0.4) against
  single-frame noise. Starts at 19.7 dB (mid-table) and walks down
  first to find the noise floor, then up. Settled state is sticky;
  re-evaluates only on retune or sustained MER drops. Driver thread
  in `Nrsc5Process` polls the controller every 500 ms and pushes a
  new gain via `rtlsdr_set_tuner_gain` when a step is taken.
- **Gain mode picker** in the `📶 Signal` dock tab. Three modes:
  `Auto` (the new closed-loop controller, default), `Manual`
  (slider over the R820T2 gain table), and `HardwareAgc` (hand
  control to the tuner chip's built-in AGC). Mode + manual value
  persist in `config.toml` as `gain_mode` and `manual_gain_tenths`.
- **Live AGC readout** in the Signal panel: current gain in dB,
  time since last gain change, and a status badge (probing /
  settled / bailed) sourced from `AgcController::snapshot()`.
- **"Restart stream to apply" hint** next to the gain mode dropdown
  whenever the active stream's mode disagrees with the chosen
  one. Avoids the silent-no-op trap if the user changes the mode
  mid-stream.
- **`NrscEvent::AgcDecision { tenths, reason }`** event variant
  emitted by the AGC driver thread on every gain change. Mirrored
  into `AppState::agc_db` so the existing Tuner-panel gain
  readout stays accurate on the piped backend (where `nrsc5.exe`
  doesn't emit its own `Agc` line).

### Changed

- **`AgcController` owns its gain table** as a `Vec<i32>` rather
  than borrowing a slice from the SDR backend. Eliminates the
  lifetime tie to `Sdr::gain_table_tenths()` and lets the
  controller outlive a stream restart without dancing around
  borrow checking.

### Internal

- New `pub use` re-exports in `src/dsp/mod.rs`:
  `AgcConfig`, `AgcController`, `AgcSnapshot`, `AgcStatus`.
- `R820T2_GAINS_TENTHS` exposed from `src/sdr/mod.rs` so the
  manual-gain slider can snap to legal table values.
- `Nrsc5Process::start_piped` now takes `gain_mode` +
  `manual_gain_tenths` and branches three ways at startup,
  installing the AGC controller and driver thread only in the
  `Auto` case.
- `UiCommand::SetGainMode(GainMode)` and
  `UiCommand::SetManualGainTenths(i32)` added; both update the
  in-memory `AppState`, write through to `AppConfig`, and persist
  to `config.toml` immediately.
- `src/dsp/agc.rs` ships with 4 unit tests covering the EMA
  smoothing, the explored-set walk, the bail budget, and the
  settled-state stickiness. All passing on
  `x86_64-pc-windows-gnullvm`.

## [0.2.0] - 2026-05-17

The "we own the radio now" release. The single biggest architectural change
since 0.1.0: NRSC5 Studio is no longer a thin GUI wrapper that hands the
RTL-SDR dongle to `nrsc5.exe` and gets out of the way — it now opens the
dongle itself, pipes raw I/Q into `nrsc5.exe -r -`, and taps the same
stream for a live spectrum / waterfall visualization. Audio playback,
metadata, traffic and weather data still flow exactly as before.

### Added

- **Spectrum / waterfall panel** — new `📊 Spectrum` dock tab. Top 40%
  is a live FFT trace with a translucent blue→cyan gradient fill under
  the curve (SDR# style), painted as a per-vertex-colored triangle-strip
  mesh. The dB grid (every 20 dB), frequency labels along the bottom,
  faint shading at the HD digital sidebands (±129–199 kHz), and a red
  vertical center-carrier line are all overlaid. Bottom 60% is a 256-row
  rolling waterfall with a turbo-style blue→cyan→yellow→red colormap.
  Driven by a dedicated FFT tap on the I/Q thread; throttled to ~30 Hz
  so CPU cost is negligible. The waterfall texture is rebuilt only when
  the tap's generation counter advances.
- **Piped-SDR backend** — a new in-process RTL-SDR backend (see
  `src/sdr/` + `src/sdr_detect.rs`) opens the dongle via the modern
  osmocom `librtlsdr.dll`, configures it (1.488 Msps cu8, default gains),
  and pumps I/Q into `nrsc5.exe -r -`. Feeds the spectrum tap in parallel.
- **Modern librtlsdr + libusb** — bundled DLLs upgraded to the osmocom
  nightly (`librtlsdr.dll` 2026-05-16, `libusb-1.0.dll` 2026-05-16).
  Brings in the canonical upstream fix for `rtlsdr_close` after
  `rtlsdr_cancel_async` (commit `2659e2df` "lib: force wait state after
  cancel of usb transfer", 2022-01-08) and the 2026-01-26 fix for
  application hang on USB transfer errors (commit `65f06585`).
- **`nrsc5.exe` upgraded to v3.1.0** with rebuilt `libnrsc5.dll`. Picks
  up upstream decoder + AAS handling improvements.

### Changed

- **Clean open-on-Start / close-on-Stop semantics.** With the modern
  DLL handling `rtlsdr_close` cleanly, the v0.1.x "open-once for app
  lifetime" workaround is gone. Stopping the stream now fully releases
  the USB device — the LED on the dongle goes off, the device is
  unclaimed, and the next Start (or a switch to USB / rtl_tcp mode)
  gets a fresh handle. Retune is a uniform stop → 250 ms breather →
  start-in-same-mode; piped, USB, and rtl_tcp modes share the same
  path. Removed `IqSink`, `ensure_sdr_running`, the eternal pump, and
  the sink mutex from `src/ffi/mod.rs` (~80 lines deleted).
- **DLL search path** — both `librtlsdr` load sites
  (`src/sdr_detect.rs::lib` and `src/sdr/rtl.rs::load_api`) now call
  `libloading::os::windows::Library::load_with_flags(&path,
  LOAD_WITH_ALTERED_SEARCH_PATH)` on Windows so the modern librtlsdr's
  dynamic libusb dependency is resolved out of `bin\` rather than the
  app's working directory. Non-Windows builds fall back to plain
  `Library::new` for future Linux portability.

### Internal

- New `src/dsp/` module with `spectrum.rs` (rustfft-based FFT tap,
  Hann window, magnitude-to-dB conversion, fftshift, rolling
  waterfall ring buffer).
- New dependency: `rustfft = "6"`.
- `Nrsc5Process::set_spectrum_tap(tap)` installs the shared tap; the
  same `SpectrumTap` clone is held on `AppState` so the dock panel
  reads from it directly without any channel plumbing.
- `LastStartMode` enum (`Usb` / `Piped` / `RtlTcp`) added to
  `Nrsc5Process` so `retune` knows which `start*` to call after
  `stop`, without forcing the caller to re-plumb mode selection.

## [0.1.3] - 2026-05-16

Single-feature release: the 24-hour rolling song log. Never tagged
publicly; superseded by 0.2.0.

### Added

- **24-hour rolling song log.** New `📝 Log` dock tab with two views:
  **Timeline** (one row per play, newest first) and **Top Played**
  (grouped by `(title, artist)`, sorted by count). Both rendered with
  `egui_extras::TableBuilder` for virtualized row recycling. CSV export
  button writes RFC-4180 to
  `Documents\nrsc5-studio-playlog-{YYYYMMDD-HHMMSS}.csv`. Log is
  persisted as RON at `%LOCALAPPDATA%\nrsc5-studio\play-log.ron`
  with atomic `.tmp+rename` writes and a 5,000-entry hard cap.
- **Layered push gate** prevents station IDs and slogans from
  polluting the log: pair-equality dedup against the last entry,
  ≥30 s rate limit, a heuristic that rejects fields containing the
  call sign / formatted frequency / broadcast tokens (`fm`, `am`,
  `mhz`, `hd1`–`hd4`), and a recent-cover-art window (metadata-only
  updates only count if a fresh cover landed within 30 s).

## [0.1.2] - 2026-05-16

This release is mostly about polish, persistence, and disk hygiene. The
album-art collage in particular is now a dramatically more compelling part
of the app — it survives restarts, fits the panel without gaps, and lets you
control the tile density on the fly.

### Added

- **Persistent album-art cache.** Every unique cover seen on the station is
  content-addressed and saved under
  `%LOCALAPPDATA%\nrsc5-studio\art-cache\` alongside an atomic RON manifest
  recording the rolling 8-hour play history per cover, plus the
  `(title, artist)` pairs and most recently observed album name. The
  collage repopulates the moment you launch and survives Stop/Start cycles
  and full app restarts. Orphaned image files are swept on prune so the
  cache never bloats.
- **Configurable collage tile cap (1–512).** A small stepper on the collage
  header (`tiles − 64 +`) snaps to powers of two so the geeky binary
  progression is the only thing you can pick. The cap is persisted in
  `config.toml`. Hard-clamped to 512 so a borked config can't ask for a
  million tiles.
- **Discrete-size square heat-map layout.** Tiles are now perfect squares
  bucketed by play-count quantile (top 0.5% become 6×6-cell tiles, then
  4×4, 3×3, 2×2, 1×1 for the long tail). A largest-first packer with
  pseudo-random placement scatters the heavy hitters around the panel
  instead of clumping them in one corner, and a tight first-fit pass plugs
  the holes with singletons. Result is gap-free at any cap from 1 to 512.
- **Cover hover tooltip** listing the album name and every unique
  `(title, artist)` pair that has been displayed with the cover.
- **Friendly "Plug in an RTL-SDR" overlay.** If no dongle is detected on
  launch, the cryptic empty state is replaced with a centered overlay and
  a Refresh button. A live `librtlsdr` probe runs every 2 seconds and
  auto-dismisses the overlay the moment a dongle is inserted.

### Changed

- **Per-content-hash 4-minute play-count cooldown.** Eliminates the
  inflated counts (×440, ×381…) that came from the same album cover being
  retransmitted under different LOT IDs in quick succession.
- **Removed `×NNN` play-count badge from collage tiles.** Tile size now
  carries the frequency information on its own; the badge was visual
  clutter at high tile counts.
- **Clicking Start no longer wipes the collage.** The pre-persistence
  reset was a holdover from 0.1.1 and defeated the durability work. The
  8-hour rolling window handles its own pruning.

### Fixed

- **Collage missed the first 1–2 covers.** The square-heat-map packer
  bucketed the top tile to a 6×6 cell, but when only one or two unique
  covers had been seen the panel had fewer than 6 rows, so the placer's
  bounds check silently dropped it and the collage looked empty. Tile
  sizes are now clamped to whatever the grid can actually hold.
- **Weather radar appeared on a black background on first start.** If a
  DWRO overlay arrived before the DWRI text file in the broadcast cycle,
  the first composited frame was rendered onto the dark fallback fill
  even when a cropped basemap from a prior session was already cached on
  disk; the dedup hash then made later identical DWROs get skipped, so
  the broken frame stuck around. The cache bootstrap now also picks up
  the freshest `BaseMap_*.png` as a starter, and once the real basemap
  becomes available any frames composited without it are dropped so the
  next overlay re-renders onto the map.
- **AAS dump dir cleanup** under `%TEMP%\nrsc5-tui-aas`:
  - Album-art LOT JPGs are deleted after a successful cache store.
  - Weather radar overlay (DWRO) PNGs are deleted after compositing into
    the rolling frame buffer.
  - Traffic map (TMT) tiles are deleted when replaced in the 3×3 grid and
    when the map is cleared.

  Previously, none of these were cleaned up — long listening sessions
  accumulated thousands of orphan files in the temp directory.

### Internal

- New module `src/art_cache.rs` (cache + manifest, versioned, atomic
  writes).
- New module `src/sdr_detect.rs` (background dongle probe).
- Significant refactor of `src/gui/dock.rs` for the new collage layout.

## [0.1.1]

- Embedded `.exe` icon.
- Album-art hover tooltips (title/artist/album).
- Initial panel-restore work.

## [0.1.0]

Initial portable release.
