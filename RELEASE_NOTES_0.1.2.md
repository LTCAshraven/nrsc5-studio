## NRSC5 Studio 0.1.2

A polish-and-persistence release. The album-art collage is the headline feature — it now survives restarts, fits the panel without gaps at any tile count, and lets you tune the density on the fly. The radar pane and the no-dongle empty state both got friendlier too.

### Highlights

- **Persistent album-art collage.** Every unique cover seen on the station is content-addressed and saved under `%LOCALAPPDATA%\nrsc5-studio\art-cache\` with an atomic RON manifest tracking the rolling 8-hour play history. Close the app, reopen, the collage is right where you left it. Stop/Start no longer wipes it either.
- **Discrete-size square heat-map layout.** Tiles are now perfect squares, bucketed by play-count quantile (6×6 mega tiles for the very top, down to 1×1 for the long tail). A largest-first packer with scattered placement keeps the heavy hitters from clumping, and a tight first-fit pass plugs the holes with singletons. Gap-free from 1 to 512 tiles.
- **Configurable tile cap (1–512).** Small `tiles − 64 +` stepper on the collage header, snapping to powers of two. Persisted in `config.toml`.
- **Cover hover tooltip** showing the album name and every `(title, artist)` pair that has been displayed under that cover.
- **"Plug in an RTL-SDR" overlay** replacing the cryptic empty state, with a live 2-second probe that auto-dismisses the moment a dongle is inserted.

### Fixed

- The collage no longer drops the first one or two covers (the top-rank tile would get bucketed too big to fit the small initial grid).
- The weather radar no longer paints on pure black on first start — the cached basemap is now picked up at bootstrap, and any frames composited before the basemap was available are replaced once it arrives.
- AAS temp dir (`%TEMP%\nrsc5-tui-aas`) is now cleaned: album-art LOTs after caching, DWRO overlays after compositing, TMT tiles when replaced or cleared. Long sessions no longer accumulate thousands of orphan files.
- Per-content-hash 4-minute play-count cooldown — eliminates the inflated ×440 / ×381 counts from the same cover being retransmitted under different LOT IDs.

### Download

`nrsc5-studio-0.1.2-windows-x64.zip` — portable, x86-64 Windows 10/11. Unzip anywhere and run `nrsc5-studio.exe`. Requires an RTL-SDR USB dongle.

### Credits

Built on the work of the upstream HD Radio reverse-engineering community:
- [theori-io/nrsc5](https://github.com/theori-io/nrsc5) (Aiden / theori) — the original C decoder library this project links against.
- [cmnybo/nrsc5-dui](https://github.com/cmnybo/nrsc5-dui) and [markjfine/nrsc5-dui](https://github.com/markjfine/nrsc5-dui) — the Python DUI projects that inspired the layout and weather/traffic compositing.

MIT licensed.
