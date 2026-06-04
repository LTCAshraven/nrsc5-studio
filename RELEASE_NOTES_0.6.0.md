# NRSC5 Studio 0.6.0

A focused release on **cold-start tune speed** and a **libnrsc5
v3.2.0** refresh. The AGC controller grew an amplitude-first probe
phase that brackets the gain choice in well under a second before the
existing MER hill-climb takes over, and the bundled libnrsc5 jumps
from v3.1.0 to v3.2.0 — which both shaves CPU off the synchronizer
FFT and exposes four new SIS events (exciter/importer equipment info,
broadcaster local time, GPS-UTC leap-second offset) in the Station
Info panel.

Thanks to **argilo** (upstream `nrsc5` maintainer) for both the
amplitude-AGC algorithm we lifted from
[theori-io/nrsc5#385](https://github.com/theori-io/nrsc5/pull/385) and
for shipping the v3.2.0 release this build picks up.

If you don't care about the internals: tunes are noticeably faster
to lock, the Station Info panel has more rows on supported stations,
and Linux packages no longer ship a dead helper script.

## What's new

### Amplitude-first AGC pre-stage

The AGC controller now runs an `AmpProbe` phase **before** the
existing MER coarse/fine search. It binary-searches the device gain
table against an RMS-dBFS target (−20 dBFS for RTL-SDR Blog V3,
−22 dBFS for SDRplay) and picks the highest safe gain that doesn't
push the ADC into clipping. The existing MER hill-climb then starts
from that bracket instead of from a fixed mid-table guess.

End-to-end cold-start tune times on RTL-SDR Blog V3 with a real OTA
antenna land in the **13–17 s** range; the amplitude bracket itself
is picked in **under a second**.

When the persistent gain cache already has a fresh entry for the
current (driver, antenna, frequency, ppm) key, the controller skips
`AmpProbe` entirely and resumes the `Fine` MER hill-climb at the
cached gain — so retunes to previously-tuned stations are still
near-instant.

### Advanced AGC tuning

Power users with unusual antenna / preamp chains can override the
per-device amplitude target without a rebuild. **Settings → Gain →
"Advanced AGC tuning"** has a collapsing section with a checkbox
+ slider (−30 to −10 dBFS, 0.5 dB steps). The override is persisted
to `config.toml` and takes effect on the next Re-tune.

Cache hits ignore the override (they skip `AmpProbe` entirely).
If you change the target and want to force a fresh probe, clear the
gain cache from the same Settings tab.

### libnrsc5 v3.2.0

The bundled `libnrsc5.dll` jumps from v3.1.0 to v3.2.0. Two upstream
changes show up immediately:

- **FFTW input/output alignment**
  ([nrsc5#482](https://github.com/theori-io/nrsc5/pull/482)) — a
  measurable CPU reduction in the synchronizer FFT path.
- **Audio output queue refactor**
  ([nrsc5#500](https://github.com/theori-io/nrsc5/pull/500)) — the
  per-program PCM rings stay clean under steady-state load.

Four new SIS event types are wired end-to-end through the safe FFI
wrapper into the Station Info panel:

- **`EXCITER_INFO` / `IMPORTER_INFO`** → an **Equipment** block
  showing manufacturer ID (e.g. `GG` = Continental, `L7` = Nautel),
  core firmware version + release status, manufacturer firmware
  version + status, and whether the exciter reports an importer
  connected.
- **`LOCAL_TIME`** → a **Time** block showing the broadcaster's UTC
  offset, DST regional and local flags, and DST schedule (US/Canada
  vs EU).
- **`LEAP_SECOND_OFFSET`** → a **GPS−UTC offset** row with a hover
  tooltip describing any pending leap-second adjustment the
  broadcaster has scheduled.

Stations that don't transmit these fields simply don't show the new
rows.

### Linux packaging: helper script gone for real

The v0.5.1 changelog claimed `install-nrsc5-helper.sh` was dropped,
but the script was still installed by `debian/rules`, the lintian
overrides still referenced it, the Fedora spec still shipped it, and
both the AppStream metainfo and `docs/linux-install.md` still
pointed users at it. v0.6.0 finishes the cleanup: the script is
deleted, every packaging file is updated, and `linux-install.md` is
rewritten to reflect the in-process `libnrsc5` reality (install
the package, done).

### SDRplay AGC fix

Smoke-testing the amplitude pre-stage on an RSPdx with an outdoor
antenna (103.7 MHz) exposed a hardware-specific mismatch: SDRplay's
aggregate `Gain` element wraps an internal LNA + IFGR split where the
HD-Radio sweet spot is dominated by IF-chain noise figure, not by ADC
headroom. Amp-probe (which optimises for "loudest non-clipping") drove
the gain to the 20 dB table floor and the receiver bailed at ~2 dB
MER, even though manually setting 40.7 dB yielded 14 dB MER on the
same station.

v0.6.0 disables `amp_enable` for SDRplay so the legacy Coarse
`[26, 32, 38, 43, 47]` → Fine pipeline runs directly on that
hardware. RTL-SDR keeps the amplitude pre-stage enabled (its
single-stage R820T2 has the opposite tradeoff — ADC clipping really
is the binding constraint there). The AGC controller also gained a
generic safeguard: if the amplitude binary search collapses without
ever confirming a safe gain, the controller now hands off to
Coarse/Fine seeded from the profile's default `initial_tenths`
instead of committing the never-confirmed table-floor index as a
winner.

### AGC: initial-gain placeholder no longer wedges the search

A second smoke test on 97.1 MHz (same SDRplay setup) caught a
longer-standing bug exposed by the new Coarse/Fine pipeline. On the
very first Coarse tick the controller was recording the MER it
observed at the profile's `initial_tenths` placeholder gain — the
gain the radio sat at while nrsc5 was booting up — as if it were a
deliberate probe. If every coarse probe came back worse than that
placeholder (97.1 MHz: initial idx 19 held MER 3.88 dB; all five
coarse probes scored lower), `best_gain_idx` stayed pinned to the
initial index. Fine then bracketed at idx 19 with both ±1 neighbours
falsely marked "explored" by the coarse sweep, and bailed
immediately.

v0.6.0 now discards the first-tick observation when the controller
entered the `Coarse` phase, so the best coarse probe wins outright
and Fine starts from a real measurement. Fine-only configs (empty
coarse table → controller enters `Fine` directly) are unaffected.

## Upgrading

- **Windows:** drop in the new binary or zip; settings carry over
  from any 0.4.x / 0.5.x install. The bundled `bin/libnrsc5.dll`
  was rebuilt against the v3.2.0 tag — keep it paired with the
  matching `nrsc5-studio.exe`.
- **Linux:** install the new `.deb` / `.rpm` over the previous
  package. Any stale `/usr/share/nrsc5-studio/install-nrsc5-helper.sh`
  left over from earlier installs is safe to delete.

## What's still on the roadmap

- Animated heat-map / smoothly animated live collage.
- Android port (Kotlin + Jetpack Compose over a Rust core).
- Continued upstream-tracking of libnrsc5 releases.
