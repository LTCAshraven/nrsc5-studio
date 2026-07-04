# NRSC5 Studio 0.3.10

A small tuner-ergonomics release. Two long-standing rough edges in
the SDR path get fixed: the **manual gain slider now applies live**,
and **multi-input SDRplay RSPs (RSPduo, RSPdx) get a real antenna
picker** in the Tuner panel.

Both fixes are UI wiring on top of the same `apply_agc_action` and
`set_antenna` paths the closed-loop AGC already exercises, so there
is no new hot-path code — just the missing user-facing controls that
should have existed since 0.3.0.

Thanks to the Redditor who specifically asked for SDRplay antenna
selection support — that request is what kicked this release loose.

## What's new

### Antenna selector in the Tuner panel

Multi-input SDRplay devices (RSPduo, RSPdx) now show an **Antenna**
dropdown in the Tuner panel, just under the gain-mode controls.
Picking a new entry:

- Persists the choice into `config.toml` (`[sdr] antenna = "..."`),
  so it survives across launches and frequency changes.
- Triggers a brief stream restart (~250 ms audio gap) so the next
  `configure()` applies the new input cleanly. SDRplay reports a
  fresh gain range per input and some Soapy modules refuse
  `setAntenna` outside `configure`; the restart is the simplest path
  that works on every supported driver.

Single-input devices (RTL-SDR Blog V3, HackRF One, RSP1A) collapse
the entire dropdown to nothing — it only renders when the live SDR
reports more than one antenna, so you don't see a useless one-item
picker.

SDRplay devices get `"Tuner 1 50ohm"` as the default on first launch
via the new `default_antenna` field on `DeviceProfile`. That entry
exists on every SDRplay member (including the single-input RSP1A),
so applying it unconditionally is harmless.

### Manual gain slider hot-applies

Dragging the gain slider in Manual mode while a stream is running
now pushes the new value through the same `apply_agc_action` path
the closed-loop AGC uses every probe tick. Same brief distortion
blip, no audio gap, no restart.

The "(restart stream to apply)" hint that previously appeared on
every drag is gone. Auto and Hardware-AGC modes are unchanged —
the slider isn't visible in those modes anyway.

## What's unchanged

- RTL-SDR, HackRF One, and single-input SDRplay (RSP1A) reception:
  no DSP / AGC / audio changes since 0.3.9.
- Opus 96 kbps recording, station info panel, collage, play log:
  unchanged.
- `config.toml` and `play_log.csv` formats: backward-compatible.
  The new `antenna` field defaults to `None` on existing configs.

## Installation

Download `nrsc5-studio-0.3.10-windows-x64.zip` from the Assets
section below, unzip anywhere, and run `nrsc5-studio.exe`.
Everything the app needs (nrsc5, SoapySDR, RTL-SDR, SDRplay,
HackRF support, the MSYS2 runtime) is bundled.

Linux users on Debian/Ubuntu install the `.deb`; Fedora users
install the `.rpm`. Both ship the desktop launcher, manpage,
AppStream metainfo, and hicolor icons.

SDRplay receivers still need the free **SDRplay API v3.x** service
installed from <https://sdrplay.com> — that one can't be bundled
under SDRplay's license. RTL-SDR works out of the box.

## Upgrading from 0.3.9

Drop the new zip's contents next to (or over) your 0.3.9 install,
or run the `.deb` / `.rpm` over the existing package. No config
migration needed.

## Verifying the download

```
Get-FileHash .\nrsc5-studio-0.3.10-windows-x64.zip -Algorithm SHA256
```

Compare against the SHA256 listed in the Assets section.
