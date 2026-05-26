# NRSC5 Studio 0.3.9

Recording is here. The Tuner panel now has a Rec button that captures
whatever subchannel is on the speakers into a 96 kbps Ogg/Opus file,
with automatic file rotation at a duration of your choosing and
per-file Vorbis tags that VLC, foobar2000, and Rhythmbox all pick up
natively.

This is also the first release with first-class Linux packages.
Ubuntu / Debian users get a `.deb`; Fedora users get a `.rpm`. The
two install cleanly with the standard package managers and resolve
the SoapySDR / RTL-SDR runtime deps automatically. Windows users
continue to get the same portable zip they've always had.

A handful of UI polish items round out the release: the Constellation
panel no longer drops "no lock" when you flip between HD subchannels,
the light-mode theme is readable instead of pastel, and recordings
default into a folder next to the executable so the portable design
stays portable end-to-end.

## Downloads

| Platform | Artifact |
|---|---|
| Windows (any 64-bit) | `nrsc5-studio-0.3.9-windows-x64.zip` |
| Debian / Ubuntu 22.04+ | `nrsc5-studio_0.3.9-1_amd64.deb` |
| Fedora 38+ | `nrsc5-studio-0.3.9-1.x86_64.rpm` |

### Windows install

Unzip anywhere — the bundle is portable and runs from any folder
including a USB stick. No installer, no registry writes, no admin
rights. First-run config and recordings live next to the executable.

### Linux install (Debian / Ubuntu)

```bash
sudo apt install ./nrsc5-studio_0.3.9-1_amd64.deb
```

apt will pull in `libsoapysdr0.8`, `librtlsdr0`, and `libasound2`
automatically. You'll also want the `nrsc5` helper binary — it's
declared as `Recommends:`, so apt will offer to install it on
Debian/Ubuntu where it's packaged. If it's not, build it from source
(see https://github.com/theori-io/nrsc5).

### Linux install (Fedora)

```bash
sudo dnf install ./nrsc5-studio-0.3.9-1.x86_64.rpm
```

This pulls in `SoapySDR`, `rtl-sdr`, and `alsa-lib`. The `nrsc5`
helper isn't in Fedora's default repos and isn't declared as a
requirement — install it from source from
https://github.com/theori-io/nrsc5 and put the `nrsc5` binary on
your `PATH` (e.g. in `/usr/local/bin/`).

## What's new

### Opus 96 kbps recording

A new **Rec** button on the Tuner panel starts an Ogg/Opus capture
of whatever subchannel is currently on the speakers at the moment
you click it. The recorder locks to that subchannel for the
lifetime of the file — even if you switch to a different HD button
mid-recording, the file keeps capturing the one you originally
selected. That means you can listen to HD2 while recording HD1, or
vice versa.

Files are written to `<exe_dir>/recordings/` by default (was: your
Documents folder), so the portable design stays consistent. You can
change the path in **Settings → Recording**.

#### Automatic file rotation

**Settings → Recording → Max minutes per file** (default 60, range
1–240) controls when a file rotates. When the current file reaches
that duration, the recorder writes a clean Ogg EOS page, opens the
next file with a fresh wall-clock timestamp in its name, and keeps
encoding without dropping a single sample. There's no audible click
at the seam.

If you want a "one big file" capture, set the slider to 240 minutes
and just let it ride. If you want each file kept manageable for a
DAW import, drop it to 5 or 10 minutes.

#### Per-file Vorbis tags

Each `.opus` file is tagged with:

- `ARTIST` — station call sign (e.g. `KEGL-FM`)
- `ALBUM` — call sign + subchannel (e.g. `KEGL-FM HD2`)
- `TITLE` — `HD<N> recorded YYYY-MM-DD HH:MM:SS`
- `DATE` — `YYYY-MM-DD`
- `COMMENT` — frequency + subchannel (e.g. `97.1 MHz HD2`)

The tags are written into the Opus header at file-open time, so
they're correct whether you stop the recording manually, the file
rotates on its own, or the app crashes mid-stream. VLC, foobar2000,
Rhythmbox, and any other player that reads Vorbis comments will
surface them in the playlist.

### First-class Linux packages

`.deb` and `.rpm` packages are now built and attached on every
release. Runtime dependencies (`libsoapysdr0.8` / `librtlsdr0` /
`libasound2` on Debian; `SoapySDR` / `rtl-sdr` / `alsa-lib` on
Fedora) are declared in the package metadata so a single
`apt install` or `dnf install` gets you a working app.

The same source tree builds on Windows (cross-compiled with
llvm-mingw) and Linux (native cargo build). The audio path
(`src/audio/mod.rs`) is the same code on both platforms thanks to
cpal handling WASAPI / ALSA / PulseAudio / PipeWire transparently.

If you've been building from source via
`scripts/linux-ubuntu-bringup.sh`, that path still works — the new
packages are an alternative, not a replacement.

### Theme polish

**Constellation lock indicator survives subchannel switches.**
Pre-0.3.9 the panel inferred sync from the transient
`nrsc5_status` text field, which gets clobbered to
`"switched to HD2"` when you click another HD button. The cloud
collapsed to "no lock" wide-noise even though the demod was
locked the whole time. Fixed by reading the dedicated
`currently_synced` flag, which is what the rest of the app uses.

**Light-mode accent is finally readable.** The previous accent
blue (RGB 100/160/255) looked great on dark chrome but only had
~2.3:1 contrast on white — washed-out artist names, callsigns, and
section headings in the Now Playing / Station Info panels. Light
mode now uses a darker, more saturated blue (~5.5:1 contrast,
WCAG-AA pass). Dark mode is unchanged.

### Smaller items

- **Recordings folder defaults next to the executable** rather
  than under `~/Documents/nrsc5-studio/recordings/`. Override in
  Settings → Recording if the exe dir isn't writable on your
  install.
- **iOS-style toggle switches** on the HD subchannel grid now
  pick the correct accent color based on the active theme.

## Known limitations

- **Constellation plot is still synthesized** from MER, not real
  post-equalizer symbols. nrsc5's public API doesn't expose the
  underlying symbol samples. Getting a real constellation would
  require forking nrsc5 to add a new callback — not in this
  release.
- **Recording locks to one subchannel per session.** If you want
  to capture HD1 and HD2 simultaneously, run the app twice (each
  instance is independent and self-contained in portable mode).
- **Fedora package omits `nrsc5` from Requires:** because the
  upstream `nrsc5` decoder isn't packaged in Fedora's default
  repos. The app surfaces a clear status message if it can't
  find `nrsc5` on PATH; install it from source per the upstream
  README.

## Upgrading from 0.3.8

- **Existing recordings folder setting carries over.** If you
  previously pointed Settings → Recording at a specific folder,
  that path is preserved across the upgrade. Only users who left
  it at "default" will see the new
  `<exe_dir>/recordings/` location take effect.
- **Config file format unchanged.** No migration required.
- **Old `per_song` / `continuous` recording modes** (which never
  shipped) are silently mapped to the new "On" mode if they
  appear in a config file from a development build.
