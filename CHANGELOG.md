# Changelog

All notable changes to NRSC5 Studio are documented here. The format roughly
follows [Keep a Changelog](https://keepachangelog.com/), and the project
adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.6.0] - 2026-06-03

The **amplitude-first AGC + libnrsc5 v3.2.0** release. Cold-start tunes
now bracket the gain choice in well under a second via an
amplitude-directed binary search, then hand off to the existing MER
hill-climb seeded at that bracket. The bundled `libnrsc5` jumps from
v3.1.0 to v3.2.0, picking up an FFTW alignment perf bump and four new
SIS events that surface broadcaster equipment + time-zone metadata in
the Station Info panel.

Thanks to **argilo** (upstream `nrsc5` maintainer) for both the
amplitude-AGC algorithm we lifted from `theori-io/nrsc5#385` and the
v3.2.0 release that this build picks up.

### Added

- **AGC amplitude pre-stage (`SearchPhase::AmpProbe`).** Runs ahead of
  the existing MER coarse/fine controller. Binary-searches the device
  gain table against a per-profile RMS-dBFS target (−20 dBFS for
  RTL-SDR, −22 dBFS for SDRplay), picking the highest safe gain that
  doesn't push the ADC into clipping. The MER hill-climb then starts
  from that bracket instead of from a fixed mid-table guess. On
  RTL-SDR Blog V3 with a real OTA antenna, end-to-end cold-start tune
  times are 13–17 s; the amplitude bracket is picked in under a
  second.
- **`src/sdr/iq_bus.rs` helpers:** `rms_dbfs_cu8` (RMS over a window)
  and `drain_now` (non-blocking try-recv to flush stale chunks). The
  AGC driver thread now does drain → sleep → drain → measure around
  each probe so leftover USB chunks at the old gain don't poison the
  RMS reading at the new gain.
- **Cache-hit fast path.** When the gain cache has a fresh entry for
  the current `(driver, antenna, frequency, ppm)` key, the controller
  skips `AmpProbe` entirely and resumes `Fine` at the cached gain.
  Cuts retune time on previously-tuned stations.
- **Settings → Gain → "Advanced AGC tuning".** Collapsing section
  with a checkbox + slider (−30 to −10 dBFS, 0.5 dB steps) that lets
  power users override the per-device amplitude target without a
  rebuild. The override is persisted to `config.toml` and takes
  effect on the next Re-tune. Cache hits ignore the override (they
  skip AmpProbe entirely); clear the gain cache to force a fresh
  amplitude probe.
- **Station Info: Equipment block.** New rows for `EXCITER_INFO` and
  `IMPORTER_INFO` events (libnrsc5 v3.2.0). Shows manufacturer ID
  (e.g. "GG" = Continental, "L7" = Nautel), core firmware version +
  status (release / engineering / patch), manufacturer firmware
  version + status, and whether the exciter reports an importer
  connected.
- **Station Info: Time block.** New rows for `LOCAL_TIME` and
  `LEAP_SECOND_OFFSET` events (libnrsc5 v3.2.0). Shows the
  broadcaster's UTC offset, DST regional/local flags, DST schedule
  (US/Canada vs EU), and a GPS-UTC offset row with a hover tooltip
  for any pending leap-second adjustment.

### Changed

- **Bundled `libnrsc5` upgraded from v3.1.0 to v3.2.0.** Picks up
  FFTW input/output alignment ([theori-io/nrsc5#482](https://github.com/theori-io/nrsc5/pull/482)) —
  a measurable CPU reduction in the synchronizer FFT path — and the
  audio output queue refactor from
  [#500](https://github.com/theori-io/nrsc5/pull/500). The per-program
  `PcmRing` drop counter is verified clean under steady-state load.
  `scripts/build-nrsc5-msys2.ps1` defaults to the v3.2.0 tag; the
  built DLL ships in `bin/libnrsc5.dll`.
- **`res/nrsc5.h` refreshed** against the v3.2.0 header. Four new
  event constants (`NRSC5_EVENT_EXCITER_INFO`, `IMPORTER_INFO`,
  `LEAP_SECOND_OFFSET`, `LOCAL_TIME`), the AM-mode telemetry fields
  on `sync` events (`pli`, `hppi`, `aabi`, `rdbi` — all -1 in FM
  mode and not currently rendered), and a new `NRSC5_DEVICE_VERSION_LENGTH`
  constant for the exciter/importer version strings.

### Fixed

- **AGC: placeholder initial-gain reading no longer wedges the
  search.** On the very first `Coarse` tick the controller used to
  record the MER it observed at the profile's `initial_tenths`
  placeholder gain — the gain the radio sat at while nrsc5 booted —
  as if it were a deliberate probe. If every coarse probe came back
  worse than that placeholder (SDRplay 97.1 MHz, June 2026: initial
  idx 19 held MER 3.88 dB; all five coarse probes scored lower),
  `best_gain_idx` stayed pinned to the initial index. Fine then
  bracketed at idx 19 with both ±1 neighbours falsely marked
  "explored" by the coarse sweep, and bailed at the start. v0.6.0
  now discards the first-tick observation when we entered `Coarse`,
  so the best coarse probe wins outright and Fine starts from a real
  measurement. Fine-only configs (empty coarse table → controller
  enters `Fine` directly) are unaffected.
- **SDRplay: amp-probe no longer parks the gain at the floor.**
  Real-world testing on an RSPdx with an outdoor antenna (103.7 MHz)
  showed amp-probe driving the aggregate `Gain` element to 20 dB
  (the bottom of the SDRplay table), where MER never climbed above
  ~2 dB and the controller bailed. SDRplay's aggregate gain wraps an
  internal LNA + IFGR split where the HD sweet spot is dominated by
  IF-chain noise figure — "loudest non-clipping" is the wrong target
  on that hardware. v0.6.0 disables `amp_enable` in the SDRplay
  profile so the legacy Coarse `[26, 32, 38, 43, 47]` → Fine pipeline
  runs directly; manual testing produced 14 dB MER at 40.7 dB on the
  same station. RTL-SDR keeps the amplitude pre-stage enabled (its
  single-stage R820T2 has the opposite tradeoff: ADC clipping is the
  binding constraint).
- **AGC: graceful abort when no probe is ever safe.** Even with the
  SDRplay profile fix, the controller now defends against the
  "everything clips" edge case generically: if the amplitude binary
  search collapses without ever confirming a safe gain, the
  controller hands off to Coarse/Fine seeded from the profile's
  default `initial_tenths` instead of committing the never-confirmed
  table-floor index as a winner.

### Packaging

- **Linux: `install-nrsc5-helper.sh` is gone for real this time.** The
  v0.5.1 changelog claimed the helper script was dropped, but
  `debian/rules` was still installing it, the lintian-overrides file
  still referenced it, the Fedora spec still shipped it, and the
  AppStream metainfo + `docs/linux-install.md` still pointed users at
  it. v0.6.0 finishes the cleanup: the script is deleted, every
  packaging file is updated, and `linux-install.md` is rewritten
  to reflect the in-process `libnrsc5` reality (one line: install
  the package, you're done).

## [0.5.1] - 2026-06-03

The **single-session-per-station-tuned** correction release. Many thanks to
**argilo** (upstream `nrsc5` maintainer) for the architectural review
that prompted this change.

### Fixed
- Decode pipeline now runs **one** `nrsc5_pipe_samples_cu8` session
  per station tuned, not one per HD subchannel. A single libnrsc5
  session
  already demuxes every advertised program internally and emits
  per-program PCM via the `program` field on the audio callback,
  so the previous v0.5.0 design (one feeder thread + one session
  per HD1..HD4 button toggle) was running the same decode work up
  to four times in parallel against the same I/Q stream. The new
  layout fans the I/Q bus into one feeder and demuxes PCM into one
  of eight per-program rings inside the audio callback.

### Changed
- HD1..HD4 buttons in the Tuner panel no longer have a per-program
  on/off toggle switch. Every advertised subchannel decodes
  automatically; the button row now just selects which program
  reaches the speaker. Recording continues to target whichever
  program is selected at the moment Record is pressed.
- The "Auto-decode all advertised subchannels" and
  "Max concurrent decoders" Settings entries are gone (the new
  model makes both meaningless).
- Keyboard shortcuts simplified: `Alt+1`..`Alt+8` selects the
  speaker program. The old `Ctrl+Alt+1`..`Ctrl+Alt+8` add /
  `Ctrl+Alt+X` remove shortcuts have been removed.

### Packaging
- Removed the `nrsc5` runtime helper from the Debian and RPM
  packages (`recommends = "nrsc5"`, `install-nrsc5-helper.sh`).
  v0.5.0 already moved decode in-process via `libnrsc5`, so the
  external `nrsc5` CLI is no longer used at runtime.

## [0.5.0] - 2026-06-02

The **libnrsc5 in-process cutover** release. NRSC5 Studio no longer
shells out to a bundled `nrsc5.exe` child process for HD decode —
instead it links directly against `libnrsc5.dll` at load time and
runs the decoder inside its own address space. The four-phase FFI
rewrite (raw `bindgen`-style bindings → safe Rust wrapper → audio
path cutover → multi-decoder spawning) replaces the previous
stdin/stdout pipe + s16le child-process pipeline with a single
typed `Nrsc5Session` driven by a dedicated I/Q feeder thread, with
PCM samples delivered straight to `cpal` via a callback. The same
plumbing now decodes **up to four HD subchannels (HD1–HD4) in
parallel** from a single tune.

Alongside the cutover: a Linux-side `rtl_tcp` GUI-freeze bug is
fixed; the weather-map `res/map.png` is now correctly installed by
the `.deb`/`.rpm`; the licensing posture is brought into compliance
with GPL-3.0 §6 now that the shipped binary is a combined work
(the source remains MIT but the binary is GPL-3.0).

### Added

- **In-process `libnrsc5.dll` FFI** under `src/ffi/`:
  - `src/ffi/nrsc5_sys.rs` — 710-line hand-curated raw FFI bindings
    against the `res/nrsc5.h` header pinned at nrsc5 v3.1.0.
  - `src/ffi/api.rs` — 1082-line safe Rust wrapper. Owns the
    `Nrsc5Session` handle, runs the callback trampoline, copies
    every C string and slice into owned `NrscEvent` variants on
    libnrsc5's worker thread, and exposes a typed `PcmSink`
    callback for the audio fast path.
  - `build.rs` — Phase 0 build script that auto-syncs `res/nrsc5.h`
    against the upstream theori-io/nrsc5 v3.1.0 release, emits the
    correct `cargo:rustc-link-search` entries for `bin/` (Windows)
    and the Unix link paths (Linux), and verifies the bundled DLL
    actually exports every symbol the wrapper expects.
- **Multi-program decode (HD1–HD4 in parallel).** A single SDR tune
  now fans I/Q out to one decoder per program. A central AGC tee
  feeds every decoder from the same gain trajectory, the per-program
  `play_log` dedups song metadata across subchannels, and a soft
  cap of 4 simultaneously active decoders (of nrsc5's 8 max)
  protects CPU on lower-end machines. The Tuner panel sprouts
  per-program enable toggles.
- **`scripts/build-nrsc5-msys2.ps1`** — reproducible builder for
  `libnrsc5.dll` from the upstream theori-io/nrsc5 v3.1.0 tag,
  driven through MSYS2 with `USE_STATIC=ON` to statically embed
  FFTW3, FAAD2, libusb, and rtl-sdr inside the DLL.
- **`COPYING.GPL-3.0`** — canonical FSF text of the GPL-3.0,
  shipped in the repo root, in the portable Windows zip, and in
  the `.deb`/`.rpm` at `/usr/share/doc/nrsc5-studio/`.
- **`scripts/fetch-corresponding-source.ps1`** — reproducible
  downloader that gathers the 10 upstream source tarballs
  (libnrsc5 v3.1.0, FFTW 3.3.10, FAAD2 2.11.2, libusb v1.0.27 +
  v1.0.28, rtl-sdr v2.0.2 Osmocom, SoapySDR 0.8.1, SoapyRTLSDR
  0.3.3, SoapyHackRF 0.3.4, SoapySDRPlay3) that constitute the
  GPL-3.0 §6 corresponding source. Output staged to
  `dist/corresponding-source/` (gitignored) and intended as a
  release-page asset.

### Changed

- **`Nrsc5Process` rewritten** around in-process decoding. The old
  `nrsc5.exe` `Child` is gone, along with its stdin write loop,
  stderr parser, and stdout PCM reader. Each `DecoderInstance` now
  owns one `Nrsc5Session` driven by a dedicated I/Q feeder thread
  that pumps samples from the shared `IqBus` and calls
  `pipe_samples_cu8` synchronously; metadata events arrive on
  libnrsc5's worker thread via the safe wrapper's callback.
- **Status bar `nrsc5 process` label** is gone — the in-process
  decoder is no longer a separate process. Top-bar status now
  reports the libnrsc5 version string from `nrsc5_get_version()`.
- **Window-subsystem release builds** use `#![windows_subsystem =
  "windows"]` more aggressively now that there's no child process
  writing to stderr; everything that matters is mirrored to
  `agc-trace.log` and the in-app Log panel.
- **`THIRD_PARTY_NOTICES.md` fully rewritten** to reflect the
  GPL-3.0 binary disclosure obligation. New Licensing Summary
  explains the MIT-source / GPL-3.0-binary split; the components
  statically embedded inside `libnrsc5.dll` (FFTW 3.3.10 GPL-2.0+,
  FAAD2 2.11.2 GPL-2.0, libusb 1.0.27 LGPL-2.1+, rtl-sdr 2.0.2
  Osmocom GPL-2.0+) are now documented; SoapySDR and its plugins
  (BSL-1.0 / MIT) and the GCC runtime (GPL-3.0 + RLE 3.1) are
  listed; a corresponding-source URL table is included.
- **`README.md`** gained a License section explaining the
  source/binary dual licensing and updated Credits to drop libao
  (no longer linked into anything we ship) and acknowledge the
  dynamic linkage against libnrsc5.
- **`bin/` slimmed.** `nrsc5.exe` (~6.6 MB), `nrsc5` Linux binary
  (~1.0 MB), `libao-4.dll` (~208 KB), and `libgcc_s_dw2-1.dll`
  (~52 KB) all removed. `objdump -p` confirms nothing else in the
  bundled DLL set imports the removed libraries.

### Fixed

- **`rtl_tcp` Linux GUI freeze on Start.** Selecting the `rtl_tcp`
  transport with an unreachable host or wrong port would lock the
  GUI thread until the OS produced the Force-Quit dialog (~22 s
  on glibc). `RtlTcpSdr::open` now performs hostname resolution via
  `to_socket_addrs()`, applies a 3 s `CONNECT_TIMEOUT`, installs a
  2 s `HANDSHAKE_READ_TIMEOUT` **before** the `read_exact` for the
  dongle-info header, then switches to a 5 s `READ_TIMEOUT` for
  the streaming loop. Worst-case GUI block on a misconfigured
  remote is now ~5 s.
- **LOT filename prefix.** Large-object-transfer payloads written
  to the AAS scratch directory had their port-id prefix doubled
  in some paths, breaking the cover-art / weather-map / traffic-map
  processors' file-name match logic. The prefix is now written
  exactly once.
- **MER no longer "sticks" across retunes.** The MER readout used
  to retain the previous station's value until the new station
  produced a sync report — confusing on a frequency with no signal.
  Retune now resets the MER snapshot to zero, so the Signal panel
  reads honestly while the controller is searching.
- **Enter key tunes from the frequency field.** Pressing Enter
  inside the Tuner panel's frequency text input now triggers
  `Retune` directly instead of requiring a click on the Tune
  button.
- **Weather map missing from `.deb`/`.rpm` installs.** The bundled
  `res/map.png` (CONUS base map used as the weather-overlay
  underlay when no live tile is available) was not listed in the
  packaging assets. `cargo deb` / `cargo generate-rpm` now install
  it to `/usr/share/nrsc5-studio/res/map.png`, and `src/maps/mod.rs`
  searches the FHS install path in addition to the portable
  `res/` directory.
- **`scripts/cargo-gnu.ps1` PATH ordering.** The MSYS2 `mingw64`
  bin directory was being appended after the gnullvm toolchain
  bin, so the wrong `pkg-config` could be picked up in some shells.
  It's now prepended. The script also tolerates native stderr
  output from the toolchain (previously misclassified as failure).

### Internal

- **Four-phase libnrsc5 cutover.** Tracked across branches
  `refactor/libnrsc5-build` (Phase 0 — build script), then
  `refactor/libnrsc5-bindings` (Phase 1 — raw FFI), then
  `refactor/libnrsc5-api` (Phase 2 — safe wrapper), then
  `refactor/libnrsc5-cutover` (Phase 3 — runtime cutover), then
  `feat/multi-program-decode` (multi-decoder layer + Linux
  packaging + rtl_tcp fix + licensing). All merged forward into
  `main` via fast-forward.
- **100% of project `unsafe`** is now isolated to `src/ffi/api.rs`
  (callback trampoline, linked-list walks, slice/string copy-out
  from C). Everything outside `src/ffi/` is safe Rust.
- **Packaging assets refreshed.** `Cargo.toml`'s
  `[package.metadata.deb].assets` and `[package.metadata.generate-rpm].assets`
  now install `COPYING.GPL-3.0` and `res/map.png`.
  `scripts/package-portable.ps1` sweeps `COPYING.GPL-3.0` into the
  Windows zip alongside `LICENSE`, `README.md`, and
  `THIRD_PARTY_NOTICES.md`.

## [0.4.1] - 2026-05-30

SDR transport cleanup and Settings modal redesign. The `[sdr]` section
now models the data source as an explicit `transport` choice —
**LocalSoapy** (in-process SoapySDR, default and unchanged),
**SoapyRemote** (Soapy-over-TCP via a `SoapySDRServer` instance), or
**rtl_tcp** (native rtl_tcp client implemented end-to-end in Rust, no
Soapy on the wire). Both remote transports feed the same in-process
piped IQ → spectrum → AGC → nrsc5 pipeline that the local path uses,
so every downstream feature (persistent gain cache, AGC trace log,
per-element gain sliders where applicable) works identically across
all three. The SDR Settings modal grew a Transport row at the top
with per-transport host / port (and, for SoapyRemote, an extra-args)
form.

The 0.2.x runtime fallbacks that were deferred forward have been
removed: `Nrsc5Process::start` (USB-direct) and
`Nrsc5Process::start_rtltcp` (legacy rtl_tcp process launch) are
gone, along with their `LastStartMode` variants. Old `config.toml`
files with `use_rtl_tcp = true` (or `rtl_device_index`,
`rtl_tcp_host`, `rtl_tcp_port`) are migrated transparently to the
new `transport = "rtl_tcp_remote"` shape on first load; the legacy
keys are then dropped on save.

Alongside the transport work, the Settings modal got a full
left-rail / 4-tab redesign (Connection / Gain / Display / Recording),
the top-bar SDR chip and Settings header now reflect the active
transport instead of the cached local driver, the top bar wraps to a
second line on narrow windows so panel toggles stay reachable, and
the bundled default dock layout was recaptured to fit comfortably on
a 1920×1080 monitor with the Windows taskbar visible.

### Added

- **`SdrTransport` enum** in `src/config.rs` with
  `LocalSoapy` / `SoapyRemote` / `RtlTcpRemote` variants and matching
  `[sdr]` fields (`remote_host`, `remote_port`,
  `remote_extra_args`).
- **Native rtl_tcp backend** at `src/sdr/rtltcp.rs` implementing the
  `Sdr` trait: 12-byte dongle-info header parse (`RTL0` magic),
  5-byte BE command frames for set-freq / set-sample-rate /
  set-gain-mode / set-tuner-gain / set-PPM / set-AGC, blocking CU8
  read loop wired into the same callback the SoapySDR path uses.
- **Transport-aware open** in `Nrsc5Process::start_piped` — branches
  on the configured `SdrTransport` and constructs the right backend
  (`SoapySdr::open` for Local / SoapyRemote, `RtlTcpSdr::open` for
  RtlTcpRemote). `retune` rebuilds via the same cached transport
  choice.
- **Transport picker** in the SDR Settings modal with per-transport
  Host / Port (and SoapyRemote "Extra args") inputs and contextual
  help text describing which server must run on the remote machine.
- **Redesigned Settings modal** with a left-rail tab nav (Connection,
  Gain, Display, Recording), proper egui panel hierarchy capped at
  95%/85% of the screen, radio-button device list, configurable
  preset slot count (1..=48, default 6), and a transport-aware
  connection-string display in the header.
- **`SdrConfigSection::chip_label()`** and
  **`SdrConfigSection::display_connection_string()`** helpers driving
  the top-bar SDR chip and the Settings modal header so both reflect
  the active transport instead of the cached local driver.
- **`Nrsc5Process::exe_path()`** accessor + hover tooltip on the
  top-bar status label so the bound `nrsc5.exe` path is one mouse-over
  away without consuming horizontal space in the menu strip.
- **Eight unit tests** for the config migration / `to_args_string()`
  composition and **five unit tests** for the rtl_tcp command-frame
  encoder and dongle-info parser.

### Removed

- `Nrsc5Process::start` (USB-direct, 0.2.x).
- `Nrsc5Process::start_rtltcp` (legacy rtl_tcp process launch, 0.2.x).
- `LastStartMode::Usb` and `LastStartMode::RtlTcp` variants — only
  `Piped` remains.
- The "Deferred to v0.5.0: rtl_tcp / networked SDRs" README block.
- The "Recording Mode" dropdown in the Settings modal — the Rec
  button alone is the on/off control now.

### Changed

- `Nrsc5Process::retune` signature simplified: the dropped
  `device_index` parameter is no longer threaded through `UiCommand`.
- `[sdr]` config no longer serializes `use_rtl_tcp`,
  `rtl_device_index`, `rtl_tcp_host`, `rtl_tcp_port`, or
  `use_piped_sdr` — they're read for migration and then dropped on
  the next save.
- Top-bar row switched from `ui.horizontal` to `ui.horizontal_wrapped`
  so the panel-toggle buttons flow onto a second line at narrow widths
  instead of clipping off the right edge of the OS window.
- Default dock layout (`DEFAULT_DOCK_RON`) recaptured at ~1560×880
  with a minimal panel set (Tuner + StationInfo, NowPlaying, Weather
  + Traffic) so fresh installs fit on a 1920×1080 monitor.
- `Nrsc5Process::version()` shortened to just `"nrsc5 process"`
  (full binary path moved to a hover tooltip).

## [0.4.0] - 2026-05-28

Closed-loop AGC overhaul. The host-side gain controller is rewritten
around a **Coarse-then-Fine** search instead of the flat hill-climb
that v0.3.x used. The Coarse phase samples a small set of widely-spaced
gain points to locate the general area, then the Fine phase
hill-climbs ±1 around the best-seen index until the peak is bracketed.
The new controller is far less likely to settle on a sub-optimal local
shoulder, but it's not perfect.
And a new **persistent gain cache** with a 7-day TTL lets
the AGC skip the cold search entirely on stations you visit regularly
(typical re-tune is now one verification probe instead of a full sweep).

The settle gate also moves from raw elapsed time to **sample-driven**:
the controller now waits for 8 MER reports (≈2 s at nrsc5's 4 Hz
cadence) at each gain before making a decision. The first sample after
a gain change is contaminated by SDR sync-recovery transients, and
averaging across more samples drops the first sample's weight in the
EMA from 22 % to under 3 %. The net effect is that the Fine phase
actually finds the true peak instead of being misled by a single bad
reading on the first probe of a new gain.

Finally, an **`agc-trace.log` file** is written next to the other app
data and overwritten at the start of every tune. It contains one line
per probe — phase, gain in dB, table index, best-seen so far, and a
reason string — plus the cache HIT/MISS edge and the final
SETTLED/BAILED outcome. The README has a new "AGC trace log" section
explaining where it lives and how to tail it.

### Added

- **Coarse-then-Fine AGC search.** New `SearchPhase::{Coarse, Fine,
  Done}` enum in `AgcController`. The Coarse phase visits a small set
  of widely-spaced gain points from the device profile
  (`DeviceProfile.coarse_probe_tenths`) to bracket the global peak,
  then the Fine phase ±1 hill-climbs around the winner until both
  adjacent neighbours are explored. R820T2 ships with a 5-point coarse
  set; SDRplay ships with a 5-point set tuned for its mid-table sweet
  spot. Empty coarse set falls back to legacy Fine-only behaviour
  (used by the test suite for determinism).
- **Persistent per-station gain cache.** Successful settles write
  `gain-cache.ron` (RON format, schema v1) under the same data
  directory as the other app state. Re-tuning the same frequency on
  the same driver/antenna combination within 7 days seeds the
  controller directly into the Fine phase at the cached gain with
  `mer_target_db = cached_mer - 3 dB` (the "trust but verify" floor),
  so typical re-tune cost drops to a single verification probe. Cache
  is keyed on `(driver, antenna, freq_khz)`; entries older than 7 days
  are dropped on read. Writes are atomic (`.tmp` + rename) so a crash
  mid-write can't corrupt the file.
- **`agc-trace.log` observability file.** Every tune writes a
  human-readable trace of the AGC's reasoning to a single file —
  truncated at the start of each new tune so it never grows
  unbounded. Contains a header (frequency, driver, antenna, PPM),
  cache HIT/MISS, one line per probe with phase + gain + best-so-far
  + reason, and a final SETTLED/BAILED line with the chosen gain and
  best observed MER. Lives at `data\agc-trace.log` in portable mode
  and `%LOCALAPPDATA%\nrsc5-studio\agc-trace.log` installed.
- **`AgcSnapshot.best_tenths` field.** Exposes the gain in tenths-dB
  at the controller's `best_gain_idx` so the UI and trace log can
  display the actual best-seen gain instead of the current probe
  gain (which used to be a confusing mislabel).
- **`paths::agc_trace_path()` and `paths::gain_cache_path()`**
  helpers, both honoring portable mode automatically.

### Changed

- **Sample-driven settle gate.** Default
  `min_mer_samples_post_change` raised from 4 to **8** (≈2 s at
  4 Hz). At the EMA's α=0.4, the first sample's weight in the final
  EMA drops from 22 % (at 4 samples) to 2.8 % (at 8 samples) — large
  enough to keep transient sync-loss readings on the first sample
  after a gain change from contaminating the decision. Empirically
  this is the difference between Fine settling at the actual peak vs.
  bailing one step off it. Time cost: ~1 s per probe, ~6–8 s per
  cold tune.
- **`probe_period` soft ceiling** raised from 3000 ms to **4000 ms** to
  outlast the nominal 8-sample window at 4 Hz plus jitter, while
  still bailing in reasonable time on no-sync stations.
- **`mer_target_db` raised** from 10 dB to **18 dB**. The settle
  threshold is now meaningful on strong stations (lights up the
  Settled badge when MER actually clears the HD3/HD4 threshold) and
  the explored-set stability shortcut still handles marginal stations
  cleanly via Fine convergence.
- **AGC driver thread (in `ffi/mod.rs`)** now mirrors every per-probe
  log line, cache decision, and settle/bail edge to both stderr and
  `agc-trace.log`. Release builds use `windows_subsystem = "windows"`
  which detaches stdio, so the file mirror is the channel that
  actually reaches end users.

### Fixed

- **Fine-phase oscillation when the peak sits at the Coarse winner.**
  The previous Fine logic walked unexplored gains starting from
  `gain_idx`, which after a few direction flips would step *past* the
  contiguous explored block around `best_gain_idx` and probe extreme
  high/low gains for no good reason. The walk now anchors on
  `best_gain_idx` and only ever probes the immediate ±1 neighbours;
  if both neighbours are already explored, the controller settles
  (if MER is acceptable) or bails (if not). Regression test:
  `dsp::agc::tests::no_oscillation_revisits`.

### Internal

- `AgcConfig` gained `coarse_probe_tenths: &'static [i32]` and
  `seeded_from_cache: bool`. Both have safe defaults so existing
  call sites and tests keep working.
- `DeviceProfile` gained `coarse_probe_tenths: &'static [i32]` so
  the AGC's coarse set is profile-driven; new profiles can opt in by
  populating the field, opt out by leaving it empty.
- Test suite expanded to 12 AGC unit tests covering Coarse → Fine
  transitions, cache-hit phase entry, the trust-but-verify settle
  threshold, peak-bracketed early termination, and the oscillation
  regression. All tests use a deterministic `cfg_fast()` that zeroes
  the sample/timing gates so single-tick decisions are exercisable.

## [0.3.10] - 2026-05-27

Tuner ergonomics release. Two long-standing rough edges in the SDR
path are addressed: the **manual gain slider now applies live**
instead of requiring a stream restart, and **multi-input SDRplay
RSPs (RSPduo, RSPdx) get a real antenna picker** in the Tuner panel.
Both changes use the same `apply_agc_action` / `set_antenna` paths
already exercised by the closed-loop AGC, so there is no new
hot-path code — just UI wiring that should have existed since 0.3.0.

### Added

- **Antenna selector in the Tuner panel.** Multi-input SDRplay
  devices (RSPduo, RSPdx) now show a dropdown listing every antenna
  the driver enumerates. Picking a new entry persists the choice
  and briefly restarts the stream so the next `configure()` applies
  the new input cleanly. Single-input devices (RTL-SDR Blog V3,
  HackRF One, RSP1A) collapse the dropdown to nothing — the dropdown
  only renders when `Sdr::antennas().len() > 1` so there is no
  useless one-item picker. SDRplay devices get `"Tuner 1 50ohm"` as
  the default on first launch via the new
  `DeviceProfile.default_antenna` field.
- **Persisted antenna choice.** `[sdr] antenna = "<name>"` in
  `config.toml` survives across launches. Empty / missing falls back
  to the device profile's default.

### Changed

- **Manual gain slider now hot-applies.** Dragging the slider in
  Manual mode while a stream is running pushes the new gain through
  the same `apply_agc_action` path the closed-loop AGC uses — same
  brief distortion blip, no audio gap, no restart. Outside Manual
  mode the slider isn't visible. The "(restart stream to apply)"
  hint that previously appeared on every drag is gone.

### Internal

- `Sdr` trait gained `antennas() -> Vec<String>`, `antenna() ->
  Option<String>`, and `set_antenna(&str) -> Result<(), SdrError>`
  with no-op defaults so non-Soapy backends don't have to implement.
- `SdrConfig` (the runtime per-stream config) gained
  `antenna: Option<String>`; `Copy` derive was dropped (was unused).
- `Nrsc5Handle::start_piped()` signature gained a 7th `antenna:
  Option<String>` parameter; `retune()` forwards `last_antenna`
  across stop/start cycles so the antenna survives frequency hops.
- `Nrsc5Handle::set_manual_gain_tenths()` synthesizes an `AgcAction`
  and routes through `apply_agc_action()` — the existing
  SDRplay-IFGR-sign-flip + case-insensitive element matching apply
  uniformly to manual mode now.

## [0.3.9] - 2026-05-26

Recording release, plus the first Linux packages. Opus 96 kbps session
recording is now a first-class feature: a Rec button on the Tuner
dock, a one-knob "max minutes per file" rotation setting in Settings,
and per-file Vorbis tags carrying station / subchannel / wall-clock
metadata. The release also ships .deb and .rpm packages so Ubuntu /
Debian and Fedora users get the same first-run experience Windows
users have always had. A handful of UI polish items round out the
release: the Constellation panel no longer drops "no lock" when you
flip subchannels, the light-mode theme is readable instead of pastel,
and recordings default into a folder next to the executable so the
portable design stays portable end-to-end.

### Added

- **Opus 96 kbps session recording.** The Rec button on the Tuner
  panel starts an Ogg/Opus capture of whatever subchannel is on the
  speakers at the time the button was clicked. The recorder runs on
  its own thread, never touches the cpal output path, and tolerates
  arbitrary GUI lag without back-pressuring the SDR. The encoder
  takes nrsc5's 44.1 kHz s16 stereo PCM, resamples to 48 kHz with a
  high-quality sinc resampler, and writes 96 kbps VBR Opus to disk.
- **Time-based file rotation.** Settings → Recording exposes a
  `recording_max_minutes` knob (default 60, range 1..=240). When a
  file reaches that duration, the recorder writes a clean Ogg EOS
  page, opens a new file with a fresh wall-clock timestamp in its
  name, and keeps the encoder + resampler state alive across the
  boundary — no audible click at the seam. The user can also stop
  recording manually at any time.
- **Per-file Vorbis tags.** Each `.opus` file is tagged with
  `ARTIST=<call sign>`, `ALBUM=<call sign> HD<N>`,
  `TITLE=HD<N> recorded YYYY-MM-DD HH:MM:SS`, `DATE=YYYY-MM-DD`,
  and `COMMENT=<frequency MHz> HD<N>`. VLC / foobar2000 / Rhythmbox
  pick these up natively.
- **Linux packages.** `.deb` (Ubuntu 22.04+, Debian 12+) and `.rpm`
  (Fedora 38+) packages are now first-class release artifacts.
  System dependencies (`libsoapysdr0.8`, `librtlsdr0`, `libasound2`
  on Debian; `SoapySDR`, `rtl-sdr`, `alsa-lib` on Fedora) are
  declared so `apt install ./nrsc5-studio_0.3.9_amd64.deb` and
  `dnf install ./nrsc5-studio-0.3.9-1.x86_64.rpm` resolve them
  automatically. `nrsc5` is declared as `Recommends:` on Debian
  (not `Requires:`) because Fedora's default repos don't ship it;
  the GUI surfaces a clear "nrsc5 not found on PATH" status if
  the user hasn't installed it yet.
- **Recordings folder defaults next to the executable.** The
  default recording dir is now `<exe_dir>/recordings/` instead of
  the user's Documents folder. Keeps the portable-first design
  consistent: a USB-stick install owns its own captures, and an
  installed run that points at a writable exe directory does the
  same. Users can still override the path in Settings → Recording.

### Changed

- **App accent color is now theme-aware.** The previous soft blue
  (RGB 100/160/255) was readable in dark mode but only reached
  ~2.3:1 contrast on white, which fails WCAG-AA for body text and
  made the "M.I.A." / "Eagle-FM" callouts and PSD/SIS headings
  feel washed out in light mode. Light mode now uses a darker,
  more saturated blue (RGB 28/100/210, ~5.5:1 contrast). Dark
  mode is unchanged.
- **Package description widened to "Windows and Linux"** to
  reflect the new platform parity.

### Fixed

- **Constellation panel no longer drops "no lock" on subchannel
  switch.** The panel was inferring sync state by string-matching
  the transient `nrsc5_status` field, which gets clobbered to
  `"switched to HD2"` when the user clicks another HD button. The
  underlying demod is still locked the whole time. Now reads the
  dedicated `currently_synced` flag, which is the real source of
  truth (set by `NrscEvent::Sync` / `LostSync`).

### Internal

- **New module: `src/recorder/mod.rs`.** Owns the encoder thread
  via `RecordingSession::spawn`. Outer/inner loop pattern keeps
  one `ogg::PacketWriter` alive per file (required for Ogg page
  sequence numbers) while sharing one Opus encoder + rubato
  resampler across rotations.
- **New PCM tap path.** `SpeakerRouter` now exposes
  `AttachRecorder { program, tap }` and `DetachRecorder { program }`
  commands. The audio thread fans out decoded PCM to both the cpal
  output sink and (optionally) a per-program `crossbeam-channel`
  feeding the recorder, so recording and listening can target
  different subchannels independently.
- **Linux packaging metadata.** `[package.metadata.deb]` and
  `[package.metadata.generate-rpm]` sections in `Cargo.toml`
  declare assets + runtime deps for `cargo-deb` and
  `cargo-generate-rpm`. `scripts/build-linux-release.sh` runs
  both tools end-to-end after `cargo build --release` on the
  Linux host.

## [0.3.8] - 2026-05-25

In-process audio release. `nrsc5.exe` is now invoked with `-o -` and
emits raw s16 LE 44.1 kHz stereo PCM on stdout; NRSC5 Studio reads
that pipe and plays it through a single `cpal`-backed output stream
owned by the studio process. The volume slider in the Windows mixer
now sits under `nrsc5-studio.exe` instead of `nrsc5.exe`, which is
the foundation Phase 2-4 (I/Q fan-out, multi-program decode, Opus
recording) build on. SDR behavior, DSP, and AGC are unchanged from
0.3.6 with two narrow exceptions: SDRplay AGC now walks up from
39 dB (was: down from 38 dB) and settles noticeably faster on weak
signals, and a loss-of-sync at the new gain correctly flips the
walk direction back toward the best-seen gain.

### Added

- **In-process audio output** via `cpal 0.15`. Single output stream
  owned by `nrsc5-studio.exe`; volume and mute are wait-free atomic
  stores on the audio sink. The 200 ms bounded queue drops the
  oldest packet on overflow so a stalled GUI thread never
  back-pressures the SDR pump.
- **Device-native sample-rate negotiation.** The output stream
  opens at whatever rate WASAPI advertises (typically 48 kHz on
  modern Windows defaults), and the playback callback does linear
  interpolation 44.1 → device-native inline. Previously the stream
  was hard-coded to 44.1 kHz and silently failed to open on the
  many WASAPI default devices that only expose 48 kHz.
- **`NrscEvent::ChildExited` event.** The PCM pump emits it on EOF
  or BrokenPipe from `nrsc5.exe`'s stdout. The app handler treats
  it like `LostDevice` but with status "stream ended", gated on
  `is_streaming` so the user-`Stop` path stays a no-op. External
  `taskkill /F /IM nrsc5.exe`, a child crash, or a clean nrsc5 exit
  all auto-recover without the user pressing Stop+Start.
- **Background SDR-presence probe.** `poll_sdr_presence` now runs
  on a short-lived worker thread; the GUI thread only drains
  results from an `mpsc::channel`. `soapysdr::enumerate("")` on
  SDRplay hot-plug can block for seconds while the SDRplay API
  service does its USB device-discovery handshake — doing that on
  the GUI thread put the window into "Not Responding" the moment
  the user replugged the dongle.
- **Per-profile AGC initial direction.** `DeviceProfile` gains
  `default_agc_initial_direction: i32`. SDRplay walks up from
  39 dB; RTL-SDR and HackRF still walk down from their existing
  starting points.

### Changed

- **`nrsc5.exe` invocation in piped mode** now passes `-o -`. Its
  stdout is piped into a `pcm_pump` thread on our side; nrsc5 no
  longer opens its own libao audio session.
- **Volume slider and mute toggle are always live.** Both work
  before a station is tuned (they previously waited for a per-app
  audio session under `nrsc5.exe` to appear in WASAPI).
- **`windows = "0.62"` dependency dropped.** With `winaudio`
  retired, the `Win32_Media_Audio` / `Win32_System_Com_*` /
  `Win32_System_Variant` / `Win32_UI_Shell_PropertiesSystem`
  feature surface is no longer referenced anywhere in the tree.
- **SDRplay AGC starting gain** moved from 38 dB to 39 dB with
  `default_agc_initial_direction = +1`. The controller walks up
  from there and stability-shortcuts when both neighbours have
  been probed — settles in noticeably fewer ticks on weak
  signals than the old "walk down from 38" strategy did.

### Fixed

- **Audio dead silence on non-44.1 kHz WASAPI defaults.** The
  default audio device on modern Windows installs is almost always
  48 kHz; cpal does not auto-negotiate sample rate on WASAPI, so
  the stream silently failed to start. Fixed by opening at the
  device's reported `default_output_config()` and resampling in
  the callback.
- **AGC walking into overload after total sync loss.** When the
  new gain caused MER to go silent for the full probe window, the
  direction picker treated `None` as "no info" and kept walking the
  same way. A `None` after `best_mer_seen.is_finite()` is now read
  as "worse" and flips back toward best.
- **No-SDR popup false positive on SDRplay-only setups.** The
  presence probe now falls back to `soapysdr::enumerate("")`
  filtered to the supported-driver list when the librtlsdr probe
  reports zero AND no stream is active. Previously the overlay
  showed even with an SDRplay plugged in.
- **GUI freeze ("Not Responding") on SDRplay re-plug.** See
  Background SDR-presence probe above.
- **Manual Stop + Start required after external `taskkill nrsc5.exe`.**
  See `NrscEvent::ChildExited` above.

### Internal

- **Retired modules:** `src/winaudio/mod.rs`, `src/linaudio.rs`,
  `src/audioctl.rs`. All three only existed to *control* libao's
  per-platform audio session; with cpal owning playback they have
  no purpose. The `volume_ctl` field, `poll_audio_session()`
  function, and `audio_session_ready` / `audio_session_mode`
  `AppState` fields are gone too.
- **New module:** `src/audio/mod.rs` with `AudioPlayer` (owns the
  cpal stream) and `AudioSink` (clone-cheap producer handle).
- **`[profile.dev.package."*"] opt-level = 3`** added to
  `Cargo.toml`. Debug-mode `rubato` couldn't keep up with 2 MHz
  CS16 I/Q resampling in real time — `cargo run` produced silence
  and a flood of `O` overflow markers from SoapySDRPlay3.
  Optimising dependencies (but leaving our own code at the
  default debug profile) keeps `cargo run` realtime without
  hurting our own debug experience.
- **Cross-platform.** `cpal` covers Windows (WASAPI), Linux
  (ALSA / PulseAudio / PipeWire via the ALSA backend), and macOS
  (CoreAudio). The new `src/audio/mod.rs` has no
  `#[cfg(target_os)]` gates; the same code drives every platform.
  Linux audio bring-up is therefore a no-op on the audio side —
  pending validation of the rest of the Linux build chain on an
  Ubuntu host.

## [0.3.6] - 2026-05-20

PSD release. The Station Information panel is now split into two
stacked tables — a new **PSD (Program Service Data)** section on
top surfacing the per-song ID3-style metadata (Song Title, Artist,
Album, Genre) the broadcast actually carries, and the existing
**SIS (Station Information Service)** section below it. Every row
appears and disappears on its own as the station sends or drops
each underlying field, with a 15-second per-field freshness window
so stale data between songs can't claim to be the current track.
No SDR or DSP behavior changes from 0.3.5.

### Added

- **PSD section in Station Information.** Four-row table for the
  song-level ID3v2.4 frames nrsc5 emits:
  - **Song Title** (`TIT2`)
  - **Artist** (`TPE1`)
  - **Album** (`TALB`) — previously parsed but never rendered.
  - **Genre** (`TCON`) — previously parsed but never rendered.

  Each row only appears when the corresponding field is non-empty,
  and disappears 15 seconds after the last refresh of *that*
  specific field, so a stale Genre from the previous song doesn't
  linger when the next song omits it.
- **Per-field freshness timestamps.** `AppState` now tracks
  `title_updated` / `artist_updated` / `album_updated` /
  `genre_updated` independently. `AppState::is_psd_field_fresh()`
  and `psd_latest_updated()` derive the visibility and footer
  state from those.
- **Per-section footers** in the Station Information panel.
  "PSD updated Xs ago" and "SIS updated Xs ago" lines bucket the
  elapsed time in 10-second steps (`just now` → `10s ago` →
  `20s ago` → `1m ago`) so they refresh visibly only when the
  number actually changes, instead of flickering every second.

### Changed

- **Station Information layout.** The panel is now a scrollable
  two-section table: PSD on top, SIS below. Either section is
  hidden when it has nothing to show; the combined empty-state
  placeholder ("Waiting for station data…") appears when both
  are empty.
- **SIS section rendering** now skips each block (call sign +
  service-mode header, slogan, message, country/FCC row,
  location, subchannels grid, data services) individually when
  the underlying field is absent, instead of drawing separator
  rules for empty sections.

### Fixed

- **Album and Genre are now actually displayed.** Both PSD frames
  were parsed from nrsc5 stderr into `AppState` since the very
  first release but had no rendering path. They now appear in the
  new PSD table as the station sends them.
- **Stale PSD on retune / Stop.** `UiCommand::TuneMhz` and
  `UiCommand::Stop` now explicitly clear `title` / `artist` /
  `album` / `genre` (and all four per-field timestamps) alongside
  the existing `station_info.reset()`, so the Station Information
  panel can no longer briefly show the previous station's song
  metadata while the new station's SIS / PSD rolls in.

### Internal

- `AppState::PSD_STALE_AFTER` constant (15 s) and
  `AppState::is_psd_field_fresh()` helper mirror the existing
  `LOST_SYNC_GRACE` / `sync_data_stale()` pattern, keeping the
  freshness policy in one place.
- `TabViewer::station_info_ui()` refactored into
  `render_psd_section()` + `render_sis_section()` helpers and a
  shared `fmt_elapsed_bucketed()` formatter so both footers
  render through the same code path.

## [0.3.5] - 2026-05-20

Identity release. Everything nrsc5 prints from a station's SIS table —
call sign, slogan, message banner, country / FCC facility ID,
transmitter lat / lon / altitude, per-subchannel program metadata,
data services, emergency alerts — now has a first-class home in a new
**📚 Station Information** dock tab. The Tuner panel's HD1–HD8
selector is SIS-aware: subchannels that the station actually
advertises light up; the rest stay clickable but dimmed with a
"Not advertised by this station" tooltip. No SDR backend or DSP
changes; both RTL-SDR and SDRplay behave identically to 0.3.1.

### Added

- **`📚 Station Information` dock tab.** New panel surfacing the
  full SIS table:
  - **Call sign** + service mode badge (`MP1` / `MP3` / `MP11`,
    marked "inferred" since nrsc5 doesn't emit the mode directly
    — derived from the highest populated program slot).
  - **Slogan** and **station message** (the rolling text banner
    some stations broadcast).
  - **Emergency alerts** rendered in a red callout banner when set.
  - **Country** and **FCC facility ID** with the FCC ID linked to
    `fcc.gov`'s public facility lookup.
  - **Transmitter location** — latitude, longitude, altitude in
    meters.
  - **Subchannel grid** with five columns per program slot:
    program number, short name, program type, sound experience,
    and audio bit rate in kbps.
  - **Data services** list (SIG-table service number, name, MIME
    type, service-data-type label).
  - **"Last updated" footer** so it's clear how recently each
    field has been refreshed by the broadcast cycle.
  - A `Waiting for SIS…` placeholder while the table is still
    being filled in after sync.
- **SIS-aware HD1–HD8 program selector.** The Tuner panel's
  subchannel buttons now consult `station_info.programs[]`:
  advertised subchannels render at full intensity; the rest are
  dimmed but still clickable with a tooltip explaining the station
  doesn't list that program (you can still probe in case SIS hasn't
  caught up).
- **`src/station_info.rs`** — new domain module with `StationInfo`,
  `ProgramInfo`, `Location`, `DataService`, and `ServiceMode`
  types. `infer_service_mode()` derives MP1 / MP3 / MP11 from the
  highest populated program slot. `reset()` is called on every
  retune and Stop so an old station's identity doesn't carry over.
- **Six new SIS stderr-parser events** in `src/ffi/mod.rs` with
  format-locked unit tests against nrsc5's literal output lines:
  `Slogan`, `Message`, `Location`, `CountryFcc`, `AudioProgram`,
  `SigServiceData`.
- **Per-program audio bit rate parsing.** New
  `NrscEvent::AudioBitRate { program, kbps }` variant emitted on
  every `Audio bit rate:` line nrsc5 prints (not just the first),
  with the value pushed into `station_info.programs[program]
  .bit_rate_kbps` and rendered in the subchannel grid's new
  fifth column.
- **Diagnostic stderr for SoapySDR stream failures.** When the
  in-process SoapySDR backend's `run_stream` returns an error,
  the actual `SoapySDR` error text is now printed as
  `[sdr] run_stream failed: <error>` immediately before the
  `LostDevice` event is sent. Makes triaging SDRplay `device
  lost` reports straightforward — the underlying USB / API /
  timeout reason now shows up in the log instead of being
  swallowed.

### Changed

- **Now Playing tab no longer claims station identity.** The old
  "KEGL 101.1 HD2" line (call sign + frequency + active program)
  was removed; that information now lives in the Station
  Information panel where it can be shown alongside slogan,
  message, location, and the rest of the SIS table.
- **Preset save fallback chain.** When saving a tune as a preset,
  the auto-derived label now falls back through SIS short name →
  artist → SIS call sign → LOT-derived call sign → `HDn` (was:
  just the legacy `station_name`).
- **`station_name` / `short_names` migrated to `station_info
  .programs[]`.** The legacy fields are gone from
  `gui::AppState`; the rest of the code now reads from the
  unified `station_info` aggregate. Saved presets and play-log
  entries from prior versions continue to load unchanged — only
  the in-memory representation changed.

### Internal

- **5 s `sync_data_stale()` grace window.** Brief `LostSync`
  flickers (sub-second sync drops during fades / multipath) no
  longer blank the Station Info panel. Fields are only cleared on
  retune, Stop, or a sustained sync loss exceeding the grace
  window.

## [0.3.1] - 2026-05-19

Follow-up to 0.3.0 that actually makes SDRplay work end-to-end. The
0.3.0 multi-SDR release enumerated and tuned SDRplay devices but the
HD Radio demodulator never synced because SDRplay's hardware can't
produce nrsc5's required 1.488375 Msps sample rate. This release adds
the missing software resampler and cleans up the SDRplay gain UI.

### Added

- **Fractional IQ resampler** (`src/sdr/resampler.rs`). New polyphase
  sinc resampler bridging SDR backends whose minimum hardware sample
  rate sits above nrsc5's required 1.488375 Msps. SDRplay's MSi001
  chain quantizes to {62.5, 96, 125, 192, 250, 384, 500, 768, 1000}
  ksps discretely and then a continuous range from 2 Msps up; the
  resampler asks the device for 2 Msps and converts down to
  1.488375 Msps in software (ratio 0.7441875) with a 128-tap
  Blackman-Harris-windowed kernel. CPU cost is negligible at HD
  Radio's bandwidth and the stopband attenuation is well below the
  receiver noise floor.
- **`rubato` 0.16** dependency (default-features off) backing the
  resampler. Time-domain sinc only — no FFT path, no new system
  libraries.

### Changed

- **SDRplay gain UI is now a single "Gain" slider.** SoapySDRPlay3
  exposes IFGR (IF Gain Reduction, 20..59 dB, *inverted*) and RFGR
  (RF Gain Reduction / LNA state, 0..9, *inverted*) as raw gain
  elements. v0.3.0 surfaced both directly which was confusing —
  sliders looked maxed when actually at minimum gain. v0.3.1 pins
  the LNA at its most sensitive state (`rfgain_sel=0`, already in
  0.3.0) and collapses the two reduction knobs into a single "Gain
  (dB)" slider mapped to libSoapySDRPlay's aggregate-gain API,
  which has un-inverted semantics (higher dB = more gain). The
  AGC adapter drives the same knob. RTL-SDR and other multi-element
  devices keep their per-element sliders unchanged.
- **SDRplay sample rate** is now requested at 2 Msps internally
  (previously a futile 1.488375 Msps request that silently snapped
  to 2 Msps anyway). Visible only in `SoapySDRUtil` probes; the
  app's spectrum view continues to report the post-resampler rate.

### Fixed

- **HD Radio sync on SDRplay.** Combined effect of the resampler
  fix and the LNA/notch defaults already shipped in 0.3.0 means
  SDRplay RSP1A / RSP1B / RSPduo / RSPdx now decode FM HD Radio
  end-to-end without any user-side workarounds.
- **SDRplay closed-loop AGC stability.** Three follow-on fixes
  surfaced during 0.3.1 bench testing:
  - **Driver-key case normalization.** `Device::driver_key()`
    returns mixed-case (`"SDRplay"`, `"RTLSDR"`) on Soapy
    0.8 while every internal lookup keyed on the lowercase form;
    SDRplay sessions silently fell back to the RTL-SDR profile so
    none of the bandwidth, notch, or AGC-element overrides took
    effect. `SoapySdr::open` now lowercases the driver key
    immediately.
  - **Force HW AGC off.** `SoapySDRPlay3`'s internal hardware
    AGC was left enabled in Auto gain mode and overrode every
    `setGain` from the closed-loop driver thread, leading to
    USB-stream churn and `lost-device` events. Configure now
    unconditionally calls `set_gain_mode(false)` for SDRplay
    regardless of UI gain mode.
  - **Per-profile AGC start gain.** The closed-loop AGC's global
    default (19.7 dB) is fine on RTL-SDR's 0..49 dB table but
    landed at the bottom of SDRplay's 20..48 dB table and forced
    a long climb before MER came up. New `DeviceProfile::
    default_agc_initial_tenths` lets each profile pick its own
    sweet-spot start: 19.7 dB on RTL-SDR (unchanged), 38 dB on
    SDRplay, 24 dB on HackRF.
  - **AGC tick rate** on SDRplay is now 500 ms (was 250 ms). The
    SoapySDRPlay3 `setGain` call is more disruptive to the USB
    stream than RTL-SDR's tuner-gain write and 250 ms ticks
    occasionally tripped a `lost-device` event during AGC probing.

### Migration

No config changes required. Existing v0.3.0 `[sdr]` blocks with
`driver = "sdrplay"` will Just Work. If you had manual entries for
`gains.IFGR` or `gains.RFGR` in your config they'll be silently
ignored — the new collapsed model reads / writes `gains.Gain`
instead. Restoring the default (delete the `gains` block under
`[sdr]`) is the simplest path.

## [0.3.0] - 2026-05-19

A multi-SDR release. The native `librtlsdr` backend is retired in
favor of a unified [SoapySDR](https://github.com/pothosware/SoapySDR)
device layer so the same build now talks to RTL-SDR, HackRF One, and
SDRplay (RSP1A / RSPduo / RSPdx) without recompilation.

### Added

- **SoapySDR backend.** New `src/sdr/soapy.rs` opens any device that
  libSoapySDR can enumerate (`driver=rtlsdr`, `driver=hackrf`,
  `driver=sdrplay`, …). Replaces the v0.2.x native librtlsdr binding.
  Existing RTL-SDR users see no behavioral change; HD Radio
  reception is unchanged on the reference R820T2 hardware.
- **Device profiles** (`src/sdr/profile.rs`). Per-driver descriptors
  encode which gain element the closed-loop AGC drives, whether that
  element is straight-gain (RTL-SDR `TUNER`) or gain-reduction
  (SDRplay `IFGR` — sign-flipped automatically), the AGC tick rate,
  the manual-gain element list for the UI, and HD-Radio-specific
  notes. v0.3.0 ships profiles for `rtlsdr`, `sdrplay`, and `hackrf`.
- **Profile-driven AGC adapter.** `ffi::apply_agc_action` translates
  the controller's tenths-of-dB decisions into the right
  `set_gain_element` call for the active device, clamping to each
  element's reported range. Same controller, three SDR families.
- **SDR Settings modal** (hamburger menu → `📡 SDR Settings…`). Live
  device picker driven by `SoapySdr::enumerate_devices()`, one
  slider per gain element on the active device, PPM correction
  field, per-driver HD Radio notes, "Reset to defaults" / "Refresh"
  / "Close" footer. Changes apply immediately to a running stream
  and persist to `config.toml`.
- **Top-bar hamburger menu** + **About dialog** with version,
  license, and clickable project URLs.
- **`[sdr]` config section** (`driver`, `device_args`,
  `freq_correction_ppm`, `gains` map). Legacy `rtl_device_index`,
  `use_rtl_tcp`, `rtl_tcp_host`, `rtl_tcp_port`, `manual_gain_tenths`,
  `gain_mode` fields are preserved unchanged for the v0.4.0
  SoapyRemote restoration; first launch on an upgraded config
  migrates the necessary values automatically.
- **Self-locating native DLLs.** `main.rs` resolves
  `<exe_dir>\bin\` at startup and prepends it to `PATH`, then
  sets `SOAPY_SDR_PLUGIN_PATH` to
  `<exe_dir>\bin\SoapySDR\modules0.8\`. Cargo runs and portable
  installs both work out of the box — no shell env setup needed.
- **Bundled SoapySDR modules.** Portable zip now ships
  `librtlsdrSupport.dll`, `libHackRFSupport.dll`, and
  `libsdrPlaySupport.dll`. The packaging script (`scripts/
  package-portable.ps1`) reports presence of each module and
  reminds packagers about the SDRplay API runtime dependency.
- **`scripts/build-soapysdrplay3-msys2.ps1`** — idempotent builder
  for `libsdrPlaySupport.dll` from upstream SoapySDRPlay3 sources.
- **`examples/iq_compare.rs`** — FFT-based spectral parity gate
  used during the v0.2.x → v0.3.0 cutover. Validates the new
  Soapy backend against the legacy librtlsdr backend on the same
  RTL-SDR hardware (RMS, DC offset, noise floor, and SNR within
  tight tolerances).
- **Version in window title.** Window title now reads
  `NRSC5 Studio <version>` (sourced from `CARGO_PKG_VERSION`).

### Changed

- **`Sdr` trait widened.** New methods `gain_elements()`,
  `set_gain_element(name, db)`, `set_frequency_correction_ppm(ppm)`,
  and `driver()` round out the device-agnostic surface. The legacy
  tenths-only `set_tuner_gain_tenths` is still present for the AGC
  fast path but is no longer the only knob the rest of the app
  uses.
- **`Nrsc5Process::start_piped`** signature now takes a SoapySDR
  args string and a PPM correction value instead of a u32 device
  index. App-level callers route through
  `config.sdr.to_args_string()`.
- **All Start paths construct a SoapySdr.** The previous Start
  branching (`use_rtl_tcp` / `use_piped_sdr` / legacy USB) is
  retired; `app.rs` always calls `start_piped`. The legacy
  `Nrsc5Process::start` (USB-direct) and `start_rtltcp` methods
  remain as dead code for the v0.4.0 SoapyRemote / rtl_tcp restoration.

### Removed

- **Native librtlsdr backend** (`src/sdr/rtl.rs`). All RTL-SDR
  access now goes through SoapyRTLSDR. The `librtlsdr.dll` file
  is still bundled because SoapyRTLSDR depends on it; the Rust
  binding has been deleted.
- **`R820T_GAINS_TENTHS` from `src/sdr/mod.rs`**. Moved to
  `src/sdr/profile.rs` and surfaced only through the `RTLSDR`
  device profile's `agc_tenths_table`.

### Deprecated / Deferred

- **rtl_tcp networked input** is deferred to v0.4.0 with full
  restoration via SoapyRemote. v0.3.0 logs a one-shot WARN on
  load when a user's `config.toml` still has `use_rtl_tcp = true`
  and falls back to local USB RTL-SDR for the session. Existing
  `rtl_tcp_host` / `rtl_tcp_port` settings are preserved untouched
  and will be re-honored when 0.4.0 ships.

### Supported devices (v0.3.0)

| Device family       | Status     | Notes                                                   |
|---------------------|------------|---------------------------------------------------------|
| RTL-SDR (R820T2)    | Validated  | Reference platform. Bench-validated.                    |
| RTL-SDR (E4000)     | Works      | 7-element gain stack (IF1..IF6+TUNER). Bench-validated. |
| SDRplay RSP1A       | Validated  | Requires SDRplay API v3.x from sdrplay.com.             |
| SDRplay (other RSP) | Should work| Same profile as RSP1A. Bench-validation welcome.        |
| HackRF One          | Profile only | Profile ships; bench-validation deferred.             |

### Migration notes

Existing v0.2.x users: drop in the new exe; your `config.toml`
auto-migrates on first launch. The legacy `rtl_device_index` is
translated into `[sdr] device_args = "device=N"` when N > 0; the
default `device_index = 0` case becomes `[sdr] device_args = ""`
(SoapyRTLSDR picks the first device). Your saved presets, play log,
album art cache, and dock layout all carry over unchanged.

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
