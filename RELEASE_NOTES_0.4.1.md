# NRSC5 Studio 0.4.1

A focused release on **SDR transport flexibility** and **UI polish**.
NRSC5 Studio now treats the SDR data source as a first-class explicit
choice instead of a tangle of legacy fallbacks, gains a native
end-to-end-Rust **rtl_tcp** client (no Soapy on the wire), and ships
a redesigned Settings modal plus a dock layout that finally fits on a
1920×1080 monitor.

If you don't care about the internals: open Settings, pick how you
want NRSC5 Studio to talk to your SDR (Local USB, SoapyRemote server,
or a plain `rtl_tcp` server on a Raspberry Pi), enter the host/port,
and Start. Everything else — the spectrum, the AGC, the persistent
gain cache, the Opus recorder, the album-art collage — works
identically regardless of which transport you picked.

## What's new

### Explicit `SdrTransport` choice

The `[sdr]` section in `config.toml` now models the data source as
an explicit enum:

- **LocalSoapy** — in-process SoapySDR, the default. Identical to
  every previous release.
- **SoapyRemote** — connect to a `SoapySDRServer` instance over
  the network. Useful for shared dongles, hosts with better antenna
  placement, or running the GUI on a laptop while the SDR lives on
  a server.
- **rtl_tcp** — connect to a plain `rtl_tcp` server. End-to-end
  Rust implementation in `src/sdr/rtltcp.rs` (12-byte dongle-info
  header parse with `RTL0` magic, 5-byte BE command frames for
  set-freq / set-sample-rate / set-gain-mode / set-tuner-gain /
  set-PPM / set-AGC, blocking CU8 read loop wired into the same
  callback as the SoapySDR path). No Soapy server required on the
  remote machine — `rtl_tcp -a 0.0.0.0` is enough.

All three transports feed the same in-process piped IQ → spectrum
→ AGC → nrsc5 pipeline, so the persistent gain cache, AGC trace
log, per-element gain sliders, and every other downstream feature
work identically across them.

### Settings modal redesign

The old single-column SDR Settings dialog has been replaced with a
4-tab modal: **Connection** (transport picker + device list + host /
port / extras), **Gain** (mode, manual slider, per-element sliders
where applicable), **Display** (theme, preset slot count), and
**Recording** (output directory, Opus bitrate). Everything is
egui-panel-based with proper scroll regions, capped at 95%/85% of
the screen so it never overflows.

### Top-bar and chip cleanup

- The SDR chip in the top bar (📡) now reflects the **active
  transport** instead of the last-cached local driver. Switching
  from `sdrplay` to `rtl_tcp` updates the chip in lockstep.
- The Settings modal's right-hand connection-string display follows
  the same rule — `rtl_tcp://192.168.0.20:1234` for rtl_tcp,
  `driver=sdrplay,…` for local Soapy.
- The top bar wraps to a second line on narrow windows so panel
  toggles stay reachable down to ~1200px wide.
- The status label was shortened to just "nrsc5 process"; the full
  `nrsc5.exe` path moved to a hover tooltip so it doesn't shove the
  panel buttons off-screen on smaller monitors.

### 1080p-friendly default dock layout

The bundled `DEFAULT_DOCK_RON` was recaptured at ~1560×880 so a
fresh install fits comfortably inside a 1920×1080 monitor with the
Windows taskbar visible. The new default opens just three
sub-windows (Tuner + StationInfo grouped, NowPlaying, Weather +
Traffic grouped); other panels stay closed and reopen from the
top-bar toggles.

### Legacy code removed

The 0.2.x runtime fallbacks that were deferred forward have been
cleared out: `Nrsc5Process::start` (USB-direct) and
`Nrsc5Process::start_rtltcp` (legacy rtl_tcp process launch) are
gone, along with their `LastStartMode` variants. Only the piped
path remains. Old `config.toml` files with `use_rtl_tcp = true`
(or `rtl_device_index` / `rtl_tcp_host` / `rtl_tcp_port`) migrate
transparently to the new `transport = "rtl_tcp_remote"` shape on
first load; the legacy keys are dropped on the next save.

## Upgrading

- Drop in the new binary or zip — settings carry over from any
  0.3.x or 0.4.0 install. Legacy rtl_tcp config keys auto-migrate.
- If you preferred the old denser default panel layout, just drag
  your panels into the desired arrangement once; the layout
  persists to disk and survives upgrades.

## What's still on the roadmap

- GitHub Actions release workflow (tag → builds → draft release).
- Animated heat-map / smoothly animated live collage.
- Android port (Kotlin + Jetpack Compose over a Rust core).
