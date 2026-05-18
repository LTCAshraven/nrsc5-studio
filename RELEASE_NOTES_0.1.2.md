## NRSC5 Studio 0.1.2

First public release of NRSC5 Studio — a native Windows desktop app for listening to HD Radio broadcasts with an RTL-SDR dongle. Free, MIT-licensed, no installer, no telemetry, no account, no nag.

### Highlights

- **Full HD Radio playback** — HD1 / HD2 / HD3 / HD4 subchannel selection, automatic retune, persistent presets you can save, recall, rename, and re-target.
- **Persistent album-art collage** — every unique cover seen on the station is content-addressed and cached to `%LOCALAPPDATA%\nrsc5-studio\art-cache\` with an atomic RON manifest tracking a rolling 8-hour play history. Close the app, reopen, the collage is right where you left it.
- **Discrete-size square heat-map layout** — tiles are perfect squares, bucketed by play-count quantile (6×6 mega tiles for the very top, down to 1×1 for the long tail). Gap-free from 1 to 512 tiles, and the cap is user-adjustable via a power-of-two stepper.
- **QPSK constellation scope** driven by live per-sideband MER.
- **TPEG traffic-tile map** and a **90-minute weather radar loop** with frame scrubber, for iHeartMedia stations that broadcast them.
- **Cover hover tooltip** showing the album name and every `(title, artist)` pair that has been displayed under that cover.
- **Friendly "Plug in an RTL-SDR" overlay** if no dongle is detected — a live 2-second probe auto-dismisses it the moment one is inserted.
- **Windows per-app volume slider** (COM-based) so the app's volume doesn't drag the whole system.
- **Persistent dockable tabs**, dark/light themes, DPI-aware sizing.

### Download

`nrsc5-studio-0.1.2-windows-x64.zip` — portable, x86-64 Windows 10/11. Unzip anywhere and run `nrsc5-studio.exe`. Requires an RTL-SDR USB dongle (generic RTL2832U + R820T2 works fine).

### Acknowledgments

This project stands on the shoulders of the HD Radio reverse-engineering community:

- [theori-io/nrsc5](https://github.com/theori-io/nrsc5) (Aiden / theori) — the HD Radio decoder this project links against.
- [cmnybo/nrsc5-gui](https://github.com/cmnybo/nrsc5-gui).
- [markjfine/nrsc5-dui](https://github.com/markjfine/nrsc5-dui).

The GUI, persistence, dock layout, and integration work was developed in collaboration with GitHub Copilot.

MIT licensed.
