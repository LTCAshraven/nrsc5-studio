<div align="center">

![NRSC5 Studio Icon](packaging/linux/icons/hicolor/64x64/apps/nrsc5-studio.png)

# NRSC5 Studio

**HD Radio reception, decoded and visualized in real time.**

A native Windows and Linux desktop app for listening to **HD Radio** broadcasts with an RTL-SDR, SDRplay, or HackRF receiver. Built in Rust with [egui](https://www.egui.rs/), wrapped around the excellent [`nrsc5`](https://github.com/theori-io/nrsc5) HD Radio decoder.

![Stars](https://img.shields.io/github/stars/LTCAshraven/nrsc5-studio?style=flat-square)
![Forks](https://img.shields.io/github/forks/LTCAshraven/nrsc5-studio?style=flat-square)
![Watchers](https://img.shields.io/github/watchers/LTCAshraven/nrsc5-studio?style=flat-square)

![Version](https://img.shields.io/badge/version-0.6.3-blue?style=flat-square)
![License](https://img.shields.io/badge/license-GPL--3.0-green?style=flat-square)
![Platform](https://img.shields.io/badge/platform-windows%20%7C%20linux-lightgrey?style=flat-square)

**[📖 Quick Start](#quick-start)** • **[✨ Features](#features)** • **[📡 Hardware](#hardware-requirements)** • **[🔧 Building](#building-from-source)** • **[📥 Install](#install-portable)**

<sub>If you find this project useful, please consider giving it a [star on GitHub](https://github.com/LTCAshraven/nrsc5-studio) — it helps others discover it and keeps the feedback and development flowing! ⭐</sub>

</div>

---

## Screenshots

![NRSC5 Studio default layout](docs/screenshot01.png)

*The default dock layout — Tuner, Now-Playing, Station Information, Spectrum, Constellation, Traffic, Weather, and the rolling Album-Art Collage all visible at once.*

![NRSC5 Studio collage-focused layout](docs/screenshot02.png)

*The egui-based dock is fully configurable: drag tabs into floating panes, close the ones you don't need, and feature the ones you do. Here the album-art collage takes center stage on a station that doesn't broadcast traffic or weather.*

---

## Features

- **Full HD Radio playback** — **HD1–HD8** subchannel selection (the full HD Radio program range), with a **SIS-aware selector grid**: subchannels the station actually advertises light up at full intensity; the rest stay dimmed-but-clickable with a tooltip explaining the station doesn't list that program (you can still probe). Automatic retune on frequency change, persistent presets you can save, recall, rename, and re-target via a double-click edit dialog.
- **Now-Playing pane** — title / artist / album / genre from broadcast metadata, plus cover art and the station logo if the station you are listening to transmits it.
- **Station Information pane** — a first-class home for everything the station broadcasts about *itself* via the HD Radio SIS (Station Information Service) table: call sign, slogan, rolling message banner, country and FCC facility ID (linked to the FCC public lookup), transmitter latitude / longitude / altitude, per-subchannel short name + program type + sound experience + live audio bit rate in kbps, the station's data services (Traffic, Weather, Album Art, etc.), and any active emergency alert in a red callout banner. An **MP1 / MP3 / MP11 service-mode badge** is also displayed. Fields populate progressively after sync (call sign and slogan in seconds; location and FCC ID can take a minute or two).
- **Album-Art Collage** — a rolling 8-hour squarified-treemap heat-map of every unique cover seen on the station. Frequent plays grow into bigger tiles; the layout re-flows as new art comes in. **Survives restarts** — covers are cached to disk (`data\art-cache\` in portable mode, `%LOCALAPPDATA%\nrsc5-studio\art-cache\` otherwise) so the heat-map repopulates instantly on relaunch (within the 8-hour window).
- **24-Hour Song Log** — every play the station broadcasts metadata for is captured with a timestamp and persisted across restarts. Two views: a **Timeline** of the most recent plays and a **Top Played** grouping by `(title, artist)`. Export to RFC-4180 CSV for the scrobbler crowd. Aggressive filtering keeps station IDs, slogans, and call signs out of the log.
- **Spectrum + Waterfall** — a dedicated SDR scope tab with a 1024-bin live FFT trace (SDR#-style translucent gradient fill, ±20 dB grid, faint shading at the HD digital sidebands at ±129–199 kHz) and a 256-row scrolling waterfall underneath with a turbo colormap. Driven from a tap on the same I/Q stream that feeds the decoder, so what you see is what nrsc5 sees.
- **QPSK Constellation** — a phosphor-green scope showing the OFDM-subcarrier constellation cloud, with cloud spread driven by live MER per sideband. Watch the cloud tighten as signal quality improves — especially satisfying while the AGC walks into its sweet spot.
- **Closed-Loop AGC** — a host-side automatic-gain-control loop (separate from the dongle's built-in AGC) that drives the active SDR's primary gain element to maximize per-sideband MER for whatever signal you're tuned to. Profile-driven: on RTL-SDR it walks the R820T2 gain table; on SDRplay it controls IF gain reduction (with sign-flip handled automatically); on HackRF it drives the LNA. Switch between **Auto** / **Manual** / **Hardware AGC** in the Signal panel; choice persists between runs.
- **Traffic Map** — TPEG traffic-tile decode, stitched into a single map image the moment all tiles for an area arrive. Carried by stations that broadcast the Total Traffic Network (TTN) or HERE data services.
- **Weather Radar Animation** — every weather overlay frame from the broadcast is captured with its real wall-clock timestamp. Play / pause / scrub through up to 90 minutes of frames with a rocker slider; duplicate frames are deduplicated by content hash so the loop only advances when the station actually pushes new radar. Carried by stations that broadcast the Total Traffic Network (TTN) or HERE data services.
- **Signal Quality** — live MER (lower / upper sidebands), BER counters, and the current AGC gain in dB with a status badge (probing / settled / bailed) and time-since-last-change.
- **Per-app Volume** — on Windows, a COM-based per-process volume / mute control so NRSC5 Studio's volume slider only changes NRSC5 Studio's audio (not the whole system). On Linux the slider applies a software-side gain to the cpal output stream.
- **Multi-SDR support** — RTL-SDR (R820T2 / E4000), SDRplay (RSP1A / RSPduo / RSPdx via the proprietary SDRplay API), and HackRF One out of the box. Switch devices via the hamburger menu's **📡 SDR Settings…** modal; per-element gain sliders, PPM correction, and HD-Radio-specific notes are surfaced per driver.
- **Friendly first-run experience** — if no SDR is plugged in, a centered "Plug in an SDR and press Refresh" overlay replaces the cryptic empty state. The overlay auto-dismisses the moment a device is detected.
- **Persistent dock layout** — drag tabs into floating sub-panes, split horizontally or vertically. Your layout is restored on the next launch.
- **Dark / light themes**, DPI-aware sizing, and a procedurally-rendered window icon.

---

## Hardware requirements

- An installed and working **SDR** with an antenna suitable for FM (87.5–108 MHz). Generic RTL2832U + R820T2 dongles are still the cheapest, most-tested option.
- A nearby HD Radio FM broadcaster. (Most U.S. metro areas have several.)
- Windows 10 or 11 (x86_64), or a Linux distribution recent enough to run a modern egui app (Debian 12+, Ubuntu 22.04+, Fedora 40+) on x86_64.
- **Have a different SDR you'd like supported?** If it has a [SoapySDR](https://github.com/pothosware/SoapySDR) module, [open an issue](https://github.com/LTCAshraven/nrsc5-studio/issues) and I'll get right on it. It should be straightforward to add.

### Supported SDRs (v0.3.0)

NRSC5 Studio v0.3.0 introduces a unified [SoapySDR](https://github.com/pothosware/SoapySDR) backend so the same build supports multiple SDR families. Switch between them via the **hamburger menu → 📡 SDR Settings…**.

| Device family       | Status        | Notes                                                                                          |
|---------------------|---------------|------------------------------------------------------------------------------------------------|
| RTL-SDR (R820T2)    | ✅ Validated   | Reference platform. Cheapest entry point.                                                       |
| RTL-SDR (E4000)     | ✅ Validated   | Nooelec SmartXTR and similar. AGC drives `TUNER`; six other IF stages settable manually.        |
| SDRplay RSP1A       | ✅ Validated   | 14-bit ADC, much wider dynamic range than RTL-SDR. **Requires SDRplay API v3.x** (see below).   |
| SDRplay (other RSP) | 🟡 Should work | RSPduo / RSPdx use the same profile as RSP1A; bench-validation contributions welcome.            |
| HackRF One          | 🟡 Profile-only | Profile ships but is not yet bench-validated. AGC drives `LNA`; report any issues you find.     |

#### RTL-SDR (Zadig)

If you don't already have working RTL-SDR drivers, install [Zadig](https://zadig.akeo.ie/) and follow the standard [WinUSB driver setup](https://www.rtl-sdr.com/rtl-sdr-quick-start-guide/) once before running NRSC5 Studio. This is the only end-user prerequisite for RTL-SDR support.

#### SDRplay (proprietary API)

SDRplay receivers (RSP1A, RSPduo, RSPdx, …) require the SDRplay API service to be installed separately. It's free but **cannot be redistributed** under SDRplay's license — so the portable zip ships only the open-source `libsdrPlaySupport.dll` bridge module. To use an SDRplay device:

1. Download and install the **SDRplay API v3.x** from [sdrplay.com/downloads](https://www.sdrplay.com/downloads/).
2. Plug in your SDRplay device.
3. Launch NRSC5 Studio. Open **📡 SDR Settings…**, click **Refresh**, and pick the SDRplay entry.

Users without an SDRplay device can ignore this entirely — the bundled module loads lazily.

#### HackRF One

HackRF support ships in v0.3.0 but is **not yet bench-validated**. The device profile (`LNA`, `VGA`, `AMP` gain stages) is conservative but may need tuning for HD Radio. If you have a HackRF and try it, opening an issue with your findings would be hugely appreciated.

### Remote SDRs (SoapyRemote and rtl_tcp)

NRSC5 Studio can drive a remote SDR via one of two protocols, picked from the **Transport** row at the top of **📡 SDR Settings…**:

- **SoapyRemote** — connects to a `SoapySDRServer` instance on the remote host. Use this when the remote dongle is anything other than a plain RTL-SDR (e.g. SDRplay over the network) or when you want SoapySDR's full feature set (per-element gain, antenna selection, etc.). The remote machine must have **SoapyRemote** installed alongside the device's Soapy module (`SoapyRTLSDR`, `SoapySDRPlay3`, …). Default port `55132`.
- **rtl_tcp** — connects directly to a native `rtl_tcp` server. Use this for the classic RTL-SDR-over-network case where you only need a single tuner-gain control. The remote machine just needs `rtl_tcp` (ships with `rtl-sdr` on most distributions). Default port `1234`.

Pick the transport, enter the remote host and port, and press Start as usual. The in-process spectrum, AGC, and audio pipeline are identical regardless of transport.

Configs from earlier 0.2.x / 0.3.x releases that used `use_rtl_tcp = true` are migrated automatically to `transport = "rtl_tcp_remote"` on first launch; the legacy keys are then dropped from `config.toml`.

---

## Install (portable)

### Windows

1. Download the latest `nrsc5-studio-portable.zip` from the Releases page.
2. Unzip it anywhere — `Documents`, `Program Files`, a USB stick, wherever.
3. Plug in your RTL-SDR dongle.
4. Run `nrsc5-studio.exe`.

No installer, no registry edits, no admin rights required.

By default the zip ships in **portable mode** (a `portable.txt` marker file lives next to the executable). In this mode the app keeps everything it writes — presets, theme, window layout, album-art cache, 24-hour song log, traffic/weather scratch — inside a `data\` folder beside the exe. Move the folder, the whole state moves with it. Plug the USB stick into a different Windows machine and it just works.

If you'd rather use the standard Windows convention (state under `%APPDATA%\nrsc5-studio\` and `%LOCALAPPDATA%\nrsc5-studio\`), delete `portable.txt` and relaunch.

### Linux (`.deb` / `.rpm`)

1. Download the `.deb` (Debian/Ubuntu) or `.rpm` (Fedora) from the Releases page.
2. Install it with `sudo apt install ./nrsc5-studio_<version>_amd64.deb` or `sudo dnf install ./nrsc5-studio-<version>.x86_64.rpm`. The package depends on `libsoapysdr` and the SDR plugin you want (e.g. `soapysdr-module-rtlsdr`).
3. Plug in your SDR. (On Debian/Ubuntu, RTL-SDR needs a udev rule and your user in the `plugdev` group — see [docs/linux-install.md](docs/linux-install.md).)
4. Launch from your desktop menu or run `nrsc5-studio` from a terminal.

Installed-mode state lives under `$XDG_DATA_HOME/nrsc5-studio/` (typically `~/.local/share/nrsc5-studio/`).

### Optional: high-resolution map basemap

The Traffic and Weather maps draw their overlays on top of a US base-map image. The portable zip and the `.deb` / `.rpm` packages ship the standard **`map.png`** (6016 × 3456). A higher-resolution **`map2x.png`** (12032 × 6912 — four times the pixels) is available for sharper maps on large windows, but at ~57 MB it's distributed as a **separate download** rather than bundled.

It's a pure drop-in upgrade: the app prefers `map2x.png` when it's present and silently falls back to the standard `map.png` when it isn't. Nothing to configure.

1. Download `map2x.png` from the [Releases page](https://github.com/LTCAshraven/nrsc5-studio/releases) assets (listed alongside the portable zip and Linux packages).
2. Put it where the app looks for basemaps:
   - **Windows (portable):** the `res\` folder next to `nrsc5-studio.exe` (i.e. `res\map2x.png`).
   - **Linux (.deb / .rpm):** `/usr/share/nrsc5-studio/map2x.png`.
3. Restart NRSC5 Studio. The maps now render against the high-resolution basemap.

To revert, just delete `map2x.png` — the app falls back to the bundled `map.png` on the next launch.

---

## Quick start

1. Launch the app. The **Tuner** pane is on the left.
2. Type a frequency (e.g. `101.1`) and pick a subchannel (`HD1`–`HD8`). Subchannels the station advertises light up; the rest stay clickable in case you want to probe.
3. Click the green **Start** button.
4. After 5–15 seconds you should see signal lock and audio.
5. Right-click any preset slot to save the current station to it; double-click to edit the label, frequency, and subchannel.

Tips:

- **Live signal lock** is reported in the top bar and on the Signal pane. MER lower/upper should be **≥ 8 dB** for clean HD audio; below that you'll hear audio dropouts.
- **Station Information** populates as the SIS table rolls in — call sign and slogan are usually first (a few seconds), with country / FCC ID / location arriving over the next minute or two. Many U.S. stations never broadcast `Message` or `Alert`; that's the station, not the app.
- **Tabs are draggable.** Pull a tab title bar off into its own floating sub-window, split panes, or close panes you don't need.
- **Album-art collage** starts empty and grows as the station plays songs. After a couple of hours on a busy station you'll see real heat-map structure form.
- **Weather radar** can take several minutes to receive a full frame even on a strong signal — that's a property of the broadcast, not the app. Will take several more minutes until enough of the frames change to show the animation.

---

## AGC trace log (power users)

Every tune writes a short, human-readable trace of the closed-loop AGC's reasoning to a single file, overwritten at the start of each new tune so it always reflects the most recent attempt. If you're curious *why* the AGC picked a particular gain, or want to confirm the cache is being honored, this is the file to look at.

Location:

- **Portable** (default for the released zip): `data\agc-trace.log` beside `nrsc5-studio.exe`.
- **Installed** (no `portable.txt` marker): `%LOCALAPPDATA%\nrsc5-studio\agc-trace.log`.

Tail it live from PowerShell while tuning:

```powershell
# Portable layout (run from the unzipped folder):
Get-Content -Wait .\data\agc-trace.log

# Installed layout:
Get-Content -Wait $env:LOCALAPPDATA\nrsc5-studio\agc-trace.log
```

What you'll see:

- A header naming the frequency, driver, antenna, and PPM correction.
- A **cache HIT** or **cache MISS** line indicating whether a cached gain was reused.
- One line per probe: current phase (`Coarse` / `Fine` / `Done`), probe number, current gain in dB and table index, the best-seen gain so far, and the reason the controller is moving (or holding).
- A closing **SETTLED** or **BAILED** line with the final gain and best observed MER.

The gain cache itself lives next door at `gain-cache.ron` (same directory) and has a 7-day TTL. Delete it to force a full cold search on every station.

Logging silently no-ops if the data directory is read-only, so this can't break a normal run.

---

## Building from source

Most users won't need this — grab the portable zip and you're done. But if you want to hack on it, feel free!:

### Prerequisites

- A Rust toolchain (install via [rustup](https://rustup.rs/)). The build pins `stable-x86_64-pc-windows-gnullvm`.
- The bundled `llvm-mingw` toolchain. The repo expects it at `.toolchains\llvm-mingw-20260505-ucrt-x86_64\` — download a release from [mstorsjo/llvm-mingw](https://github.com/mstorsjo/llvm-mingw/releases) and extract it there.
- The bundled `bin\` runtime (`libnrsc5.dll`, `libSoapySDR.dll`, `librtlsdr.dll`, Soapy plugin modules) — already in the repo. If you ever need to rebuild `libnrsc5.dll` from upstream source, `scripts\build-nrsc5-msys2.ps1` drives the full MSYS2 build of the pinned nrsc5 v3.2.0 tag with `USE_STATIC=ON`.

### Build

From an elevated PowerShell prompt in the repo root:

```powershell
.\scripts\cargo-gnu.ps1
```

This installs the gnullvm Rust toolchain if missing, then produces `target\x86_64-pc-windows-gnullvm\debug\nrsc5-studio.exe`.

For a release build:

```powershell
$env:PATH = "$env:USERPROFILE\.cargo\bin;$PWD\.toolchains\llvm-mingw-20260505-ucrt-x86_64\bin;$env:PATH"
cargo +stable-x86_64-pc-windows-gnullvm build --release --target x86_64-pc-windows-gnullvm
```

### Package a portable zip

```powershell
.\scripts\package-portable.ps1
```

Bundles the release exe with `bin\` runtime into `dist\nrsc5-studio-portable\`.

### Linux build

Linux builds use the system toolchain and SoapySDR packages. For an
end-to-end pipeline (cargo build + `.deb` + `.rpm`) see
[scripts/build-linux-release.sh](scripts/build-linux-release.sh). For
Ubuntu 22.04.5 LTS bring-up instructions, see
[docs/linux-ubuntu-bringup.md](docs/linux-ubuntu-bringup.md).

---

## Project structure

```
src/
  main.rs           entry point, window/viewport setup
  app.rs            top-level eframe app, event loop, command dispatch
  lib.rs            crate root for the integration-test surface
  icon.rs           procedurally-rendered broadcast-tower window icon
  icon_render.rs    pre-rendered icon PNG generation for Linux packaging
  config.rs         persisted user settings (presets, theme, volume, gain mode, transport, …)
  paths.rs          portable vs installed path resolution (config, cache, AAS scratch, logs)
  art_cache.rs      content-addressed on-disk cache for album art + station logos
  play_log.rs       24-hour rolling song log + RFC-4180 CSV export, per-program dedup
  station_info.rs   SIS table model (call sign, slogan, FCC ID, location, services, alerts)
  sdr_detect.rs     SDR presence probe for the no-SDR overlay
  ffi/
    nrsc5_sys.rs    raw FFI bindings against nrsc5.h (v3.2.0)
    api.rs          safe Rust wrapper around libnrsc5: Nrsc5Session, callback trampoline,
                    typed NrscEvent / PcmSink — all project unsafe lives here
    decoder.rs      per-program DecoderInstance + I/Q feeder thread
    mod.rs          AGC driver thread, multi-decoder lifecycle (Nrsc5Process)
  sdr/
    mod.rs          trait Sdr + device discovery
    soapy.rs        SoapySDR backend (RTL-SDR, SDRplay, HackRF, SoapyRemote)
    rtltcp.rs       native rtl_tcp client (end-to-end Rust)
    profile.rs      per-device gain tables, AGC element selection, sample-rate negotiation
    iq_bus.rs       I/Q fan-out to decoder + spectrum tap + recorder
    gain_cache.rs   7-day persistent per-station gain cache (RON)
    resampler.rs    rubato wrapper for SDRplay's 2 Msps → 1.488375 Msps software resample
  dsp/
    agc.rs          closed-loop AGC controller (AmpProbe → Coarse → Fine search)
    spectrum.rs     FFT-based spectrum tap feeding the waterfall + scope UI
    mod.rs
  audio/            cpal output stream + Windows COM per-process volume; Linux software gain
  recorder/         96 kbps Opus session recorder (Ogg Opus, Vorbis tags, rotation)
  collage/          album-art history + squarified-treemap layout
  maps/             traffic + weather map composition (PNG outputs)
  gui/
    dock.rs         tab definitions and tab UIs (Tuner, Spectrum, Signal, Log, Collage, …)
    state.rs        runtime state snapshot shared with the GUI
    widgets.rs      shared egui widgets
    mod.rs
res/                config defaults, nrsc5.h header, weather base map
bin/                bundled runtime: libnrsc5.dll, libSoapySDR.dll, librtlsdr.dll,
                    libusb-1.0.dll, GCC/libunwind runtime, SoapySDR\modules0.8\*
scripts/            PowerShell + bash build/package helpers
packaging/          Debian, Fedora, and Linux desktop integration assets
```

---

## Credits

NRSC5 Studio is a thin Rust GUI on top of a lot of excellent open-source work:

- **[`nrsc5`](https://github.com/theori-io/nrsc5)** — the HD Radio decoder this app dynamically links against (`libnrsc5`). License: GPL-3.0.
- **[`librtlsdr`](https://github.com/osmocom/rtl-sdr)** — RTL-SDR driver library, loaded via SoapySDR's RTL-SDR plugin. License: GPL-2.0-or-later.
- **[`libusb`](https://libusb.info/)** — cross-platform USB I/O, used transitively by librtlsdr. License: LGPL-2.1-or-later.
- **[`SoapySDR`](https://github.com/pothosware/SoapySDR)** — vendor-neutral SDR abstraction layer + RTL-SDR / HackRF / SDRplay plugin modules. License: Boost Software License 1.0 (core) / MIT (plugins).
- **[`egui`](https://www.egui.rs/) / [`eframe`](https://github.com/emilk/egui)** — the immediate-mode GUI framework. License: MIT or Apache-2.0.
- **[`egui_dock`](https://github.com/Adanos020/egui_dock)** — the dockable tab system. License: MIT.

A complete list of third-party components and their licenses, including the corresponding-source URLs required by GPL-3.0 Section 6, is in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

---

## License

- **The Rust source code in this repository is licensed under the [MIT License](LICENSE).** You're welcome to reuse it in your own projects under MIT terms.
- **The distributed binary** (the executable in the Windows portable zip / Linux .deb / .rpm) **dynamically links against `libnrsc5` (GPL-3.0)** and is therefore a combined work licensed as a whole under GPL-3.0. The full GPL-3.0 license text is bundled in the release as `COPYING.GPL-3.0`. The corresponding source for every GPL/LGPL component is enumerated in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) and available from this repository plus the upstream URLs listed there.

In short: the source is MIT, the compiled binary is GPL-3.0. If you redistribute the binary you inherit the GPL-3.0 obligations from Section 6 (provide source, preserve the notices).

---

## Acknowledgments

This project stands on the shoulders of the HD Radio reverse-engineering community — particularly:

- **TheDaChicken / Argilo** — [`nrsc5`](https://github.com/theori-io/nrsc5), the HD Radio decoder this project links against.
- **cmnybo** — [`nrsc5-gui`](https://github.com/cmnybo/nrsc5-gui).
- **markjfine** — [`nrsc5-dui`](https://github.com/markjfine/nrsc5-dui).

The GUI, persistence, dock layout, and integration work was developed in collaboration with GitHub Copilot.

---

## License

NRSC5 Studio's own source code is released under the [MIT License](LICENSE).

The portable distribution also bundles several third-party binaries that
remain under their original licenses (GPL-2.0, GPL-3.0, and LGPL-2.1). Their
full notices and upstream sources are listed in
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
