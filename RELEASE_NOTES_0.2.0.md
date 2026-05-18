## NRSC5 Studio 0.2.0

The "we own the radio now" release. A serious under-the-hood refactor, a marquee new visualization panel, and the song log shipping to the public for the first time. Audio quality and stability are unchanged — but a lot has changed about how the bytes get there.

### The big one: NRSC5 Studio is now a real SDR app

In every previous release, NRSC5 Studio handed your RTL-SDR dongle straight to `nrsc5.exe` and waited for audio + metadata to come back. That worked, but it left no place to hook a spectrum viewer, a waterfall, or any of the other things SDR users expect.

0.2.0 inverts that. The app now opens the dongle itself via a modern `librtlsdr.dll` (osmocom nightly, 2026-05-16), pumps raw I/Q to `nrsc5.exe -r -`, and taps the same stream for visualization. Two upstream `librtlsdr` bug fixes (commit `2659e2df` from 2022, fixing `close` after `cancel_async`, and `65f06585` from 2026, fixing application hang on USB transfer errors) made this possible without sacrificing audio reliability — pressing **Stop** now fully releases the device (LED off, USB unclaimed) and the next **Start** opens a fresh handle. None of the workarounds the 0.1.x line carried for older DLL bugs survive into 0.2.

### New: Spectrum / waterfall panel

A new **📊 Spectrum** dock tab gives you what SDR# / Gqrx users have always had:

- Live 1024-bin FFT trace with an SDR#-style translucent gradient fill under the curve.
- 20 dB grid lines and frequency labels along the bottom.
- Faint shaded bands at the HD digital sidebands (±129–199 kHz from the carrier) — the two distinctive humps that make HD Radio visually recognizable on a spectrum.
- Red center-carrier line bridging both halves.
- 256-row scrolling waterfall underneath with a turbo-style blue→cyan→yellow→red colormap.

The whole pipeline is throttled to ~30 Hz internally and the waterfall texture re-uploads only when there's new data, so CPU cost is negligible even on a laptop.

### New: 24-hour rolling song log (publicly released for the first time)

The `📝 Log` dock tab — quietly shipped in code as 0.1.3 but never tagged for the public — is now part of 0.2.0:

- **Timeline** view: every play, newest first.
- **Top Played** view: grouped by `(title, artist)`, sorted by count.
- **Export CSV** button writes RFC-4180 to `Documents\nrsc5-studio-playlog-{YYYYMMDD-HHMMSS}.csv` for the scrobbler crowd.
- Persists across restarts at `%LOCALAPPDATA%\nrsc5-studio\play-log.ron` (atomic writes, 5,000-entry hard cap).
- Layered push gate keeps station IDs, slogans, and call signs out of the log — only real song metadata lands.

### Other notable changes under the hood

- **`nrsc5.exe` upgraded to v3.1.0** with rebuilt `libnrsc5.dll`.
- **`librtlsdr.dll` + `libusb-1.0.dll`** swapped for the osmocom nightly (2026-05-16). Dynamically links libusb where the old DLL static-linked it, so the bundle is slightly slimmer.
- **DLL search path fix** — load sites now request `LOAD_WITH_ALTERED_SEARCH_PATH` on Windows so the new librtlsdr's libusb dependency is resolved out of `bin\` rather than the app's current working directory.
- **Uniform retune path** — stop → 250 ms breather → restart in the same mode. Works identically for piped, USB, and rtl_tcp backends.
- **Removed ~80 lines** of mutex / sink / eternal-pump plumbing from `src/ffi/mod.rs` that existed only to work around the old DLL bugs.

### Download

`nrsc5-studio-0.2.0-windows-x64.zip` — portable, x86-64 Windows 10/11. Unzip anywhere and run `nrsc5-studio.exe`. Requires an RTL-SDR USB dongle (generic RTL2832U + R820T2 works fine; RTL-SDR Blog V4 also supported via the bundled modern librtlsdr).

### Upgrading from 0.1.x

Drop-in. Your `config.toml`, presets, art cache, and dock layout all carry over. The piped-SDR path is on by default; if you'd rather keep the old behavior (where `nrsc5.exe` owns the dongle directly), set `use_piped_sdr = false` in `%APPDATA%\nrsc5-studio\config.toml`. The spectrum panel only feeds when the piped path is active.

### Acknowledgments

This release leans heavily on upstream work:

- [theori-io/nrsc5](https://github.com/theori-io/nrsc5) (Aiden / theori) — v3.1.0 brings decoder + AAS handling improvements.
- [osmocom/rtl-sdr](https://github.com/osmocom/rtl-sdr) — the modern `librtlsdr.dll` fixes both the close-after-cancel crash (2022) and the transfer-error hang (2026) that the 0.1.x line worked around.
- [cmnybo/nrsc5-gui](https://github.com/cmnybo/nrsc5-gui) and [markjfine/nrsc5-dui](https://github.com/markjfine/nrsc5-dui) — prior-art GUIs whose feature lists informed the roadmap.

The GUI, persistence, dock layout, piped-SDR plumbing, spectrum panel, and integration work were developed in collaboration with GitHub Copilot.

MIT licensed.
