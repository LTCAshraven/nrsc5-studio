# NRSC5 Studio 0.3.7

Linux packaging release. NRSC5 Studio now ships as native `.deb` and
`.rpm` packages alongside the existing Windows portable zip, all built
from the same Rust source tree. **No DSP, SDR, or audio behavior
changes versus 0.3.6** — this release is about meeting Linux users
where they already are.

## What's new

### Linux packages

- **Debian / Ubuntu**: `nrsc5-studio_0.3.7-1_amd64.deb`
  - Targets Ubuntu 22.04+, Debian 12+
  - Runtime shared-library dependencies auto-resolved by
    `dpkg-shlibdeps` at package time
    (`libc6`, `libsoapysdr0.8`, `libasound2t64`, etc.)
  - `Recommends:` `soapysdr-module-rtlsdr`, `soapysdr-tools`,
    `rtl-sdr`
  - `Suggests:` `pulseaudio-utils`, `pipewire-pulse`
- **Fedora**: `nrsc5-studio-0.3.7-1.x86_64.rpm`
  - Targets Fedora 41+
  - `Recommends:` `SoapySDR`
  - `Suggests:` `rtl-sdr`, `pipewire-pulseaudio`

Both packages install the same asset tree:

- `/usr/bin/nrsc5-studio` — the app binary
- `/usr/share/applications/nrsc5-studio.desktop` — desktop launcher
  (appears under **Sound & Video**)
- `/usr/share/metainfo/io.github.ltcashraven.Nrsc5Studio.metainfo.xml`
  — AppStream metainfo for GNOME Software / KDE Discover
- `/usr/share/icons/hicolor/{16,32,48,64,128,256}x{...}/apps/nrsc5-studio.png`
  — hicolor icon set, rendered from the same procedural
  `icon_render` module as the Windows `.ico` resource (byte-identical
  across platforms)
- `/usr/share/man/man1/nrsc5-studio.1.gz` — gzipped manpage
- `/usr/share/nrsc5-studio/install-nrsc5-helper.sh` — one-shot
  helper installer (see below)
- `/usr/share/doc/nrsc5-studio/` — `README.md`, `linux-install.md`,
  `CHANGELOG.md` (and the Debian-mandated `changelog.Debian.gz` in
  the `.deb`)

### The `nrsc5` helper story

The upstream
[`nrsc5`](https://github.com/theori-io/nrsc5) HD Radio demodulator is
not packaged in Debian, Ubuntu, or Fedora's main archives, so NRSC5
Studio cannot pull it in as a hard dependency. Instead, the package
ships a one-shot installer at:

```
/usr/share/nrsc5-studio/install-nrsc5-helper.sh
```

which detects `apt` / `dnf` / `pacman`, installs the build deps,
clones `theori-io/nrsc5` at the pinned tag (currently `v3.1.0`,
matching the Windows build), builds with `cmake`+`make`, and installs
to `/usr/local/bin`.

If NRSC5 Studio is launched before `nrsc5` is installed, a modal
dialog points the user at this script — no cryptic stderr line, no
silent failure.

### Debugging env var

Setting `NRSC5_STUDIO_DEBUG=1` in the environment now mirrors every
raw line of the upstream `nrsc5` helper's stderr to NRSC5 Studio's
own stderr with an `[nrsc5]` prefix. Useful for diagnosing sync /
MER / audio issues:

```bash
NRSC5_STUDIO_DEBUG=1 nrsc5-studio 2>&1 | tee nrsc5-studio.log
```

### About dialog license fix

The About dialog used to claim "GPL-3.0-or-later (matches nrsc5)" for
the License field. Both halves were wrong: NRSC5 Studio itself is
**MIT** and the upstream nrsc5 helper is **AGPL-3.0-or-later**
(separately executed). The dialog now lists both, on their own rows.

### Per-app volume control on Linux (bug fix)

The per-app sink-input volume slider could get stuck on the system
master sink for the lifetime of a play session. The cause: when Start
was pressed, the studio called `set_volume()` immediately, but the
`nrsc5` helper's libao back end hadn't yet connected to PulseAudio,
so the per-process sink-input lookup failed and the controller fell
back to the default sink — and then *cached* that fallback. Even
after libao connected a second or two later and a per-app sink-input
appeared, the slider kept moving the master volume.

The controller now treats `SystemSink` as a transient state: every
slider motion in fallback mode re-runs the per-process lookup, so as
soon as libao publishes its sink-input the controller transparently
upgrades to per-app mode for the rest of the session. The audio
panel's mode indicator updates with it.

## What's unchanged

- All SDR backends (RTL-SDR, SDRplay, HackRF): identical to 0.3.6.
- All DSP: spectrum, waterfall, constellation, MER/BER, closed-loop
  AGC: identical to 0.3.6.
- Audio pipeline: identical to 0.3.6.
- Windows behavior: identical to 0.3.6. Same MSYS2 bundle, same
  portable-mode layout, same icon, same window state.
- `config.toml`, `play_log.csv`, art cache layouts: unchanged.

## Installation

### Debian / Ubuntu

```bash
sudo apt install ./nrsc5-studio_0.3.7-1_amd64.deb
sudo apt install soapysdr-tools soapysdr-module-rtlsdr

# Build and install the nrsc5 helper (one-time):
/usr/share/nrsc5-studio/install-nrsc5-helper.sh
```

### Fedora

```bash
sudo dnf install ./nrsc5-studio-0.3.7-1.x86_64.rpm
sudo dnf install SoapySDR SoapyRTLSDR

# Build and install the nrsc5 helper (one-time):
/usr/share/nrsc5-studio/install-nrsc5-helper.sh
```

### Windows

Download `nrsc5-studio-0.3.7-windows-x64.zip` from the Assets section
below, unzip anywhere, and run `nrsc5-studio.exe`. Everything the app
needs (nrsc5, SoapySDR, RTL-SDR, SDRplay, HackRF support, the MSYS2
runtime) is bundled.

SDRplay receivers still need the free **SDRplay API v3.x** service
installed from <https://sdrplay.com> — that one can't be bundled
under SDRplay's license. RTL-SDR works out of the box.

## Upgrading from 0.3.6

- Windows: drop the new zip's contents next to (or over) your 0.3.6
  install.
- Linux: `sudo apt install ./nrsc5-studio_0.3.7-1_amd64.deb` (or the
  `dnf` equivalent) will upgrade in place.

`config.toml` and `play_log.csv` formats are unchanged from 0.3.6.

## Verifying the download

```bash
sha256sum nrsc5-studio_0.3.7-1_amd64.deb
sha256sum nrsc5-studio-0.3.7-1.x86_64.rpm
```

```powershell
Get-FileHash .\nrsc5-studio-0.3.7-windows-x64.zip -Algorithm SHA256
```

Compare against the SHA-256 published in the release assets below.
