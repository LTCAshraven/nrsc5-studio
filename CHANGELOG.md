# Changelog

All notable changes to NRSC5 Studio are documented here. The format roughly
follows [Keep a Changelog](https://keepachangelog.com/), and the project
adheres to [Semantic Versioning](https://semver.org/).

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
