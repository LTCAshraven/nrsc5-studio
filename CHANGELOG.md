# Changelog

All notable changes to NRSC5 Studio are documented here. The format roughly
follows [Keep a Changelog](https://keepachangelog.com/), and the project
adheres to [Semantic Versioning](https://semver.org/).

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
