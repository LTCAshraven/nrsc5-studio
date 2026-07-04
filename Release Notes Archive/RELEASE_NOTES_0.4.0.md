# NRSC5 Studio 0.4.0

A focused release on the host-side automatic-gain-control loop —
the bit of NRSC5 Studio that pokes your SDR's tuner gain knob
until HD audio locks in. The v0.3.x AGC was a flat hill-climb that
worked most of the time but occasionally settled one or two steps
off the actual peak (or, on a few stations, oscillated wildly
between extremes before bailing). This release rewrites the
controller from the ground up around a **Coarse-then-Fine** search,
adds a **persistent gain cache** so frequently-tuned stations skip
the cold search entirely, and ships a **trace-log file** so you can
see exactly what the AGC is thinking, probe by probe.

If you don't care about AGC internals: tune any station, listen,
and notice that subsequent tunes to the same frequency now lock
in roughly as fast as the SDR can change frequency. If you do care
about AGC internals: there's a new "AGC trace log (power users)"
section in the README and the file rotates per-tune so you can
diff one tune against the next.

## What's new

### Coarse-then-Fine AGC search

The closed-loop AGC now runs in two phases. **Coarse** visits a
small set of widely-spaced gain points from the device profile
(R820T2: 5 points across the gain table; SDRplay: 5 points tuned
for the mid-table HD sweet spot) and picks the one with the best
MER. **Fine** then walks ±1 around that winner until both adjacent
neighbours are explored or the configured MER target is met. The
result is a search that locates the global peak instead of
settling on a local shoulder, in a comparable number of probes to
the old flat hill-climb.

### Persistent gain cache (7-day TTL)

Successful settles write the chosen gain and observed MER to
`gain-cache.ron` next to the rest of the app's state. The next
time you tune the same frequency on the same SDR + antenna, the
controller seeds directly into the **Fine** phase at the cached
gain with a "trust but verify" MER floor (cached MER − 3 dB), so
typical re-tune cost drops to a single verification probe — under
two seconds in practice.

Cache entries expire after 7 days, which is long enough to cover
"I always listen to my four favorite stations" and short enough
to re-discover the optimal gain after a tower power change, an
antenna swap, or a seasonal propagation shift. Writes are atomic
(temp file + rename) so a crash mid-write can't corrupt the file.

### Sample-driven settle gate (8 MER reports = ~2 s)

The previous AGC waited a fixed 5 seconds per probe. The new
controller waits for 8 MER reports at the new gain — roughly 2
seconds at nrsc5's 4 Hz cadence on a station with lock, and at
most 4 seconds otherwise (the soft ceiling for "no MER arriving
at all" cases).

The reason this matters: the very first MER reading after a gain
change is contaminated by SDR sync-recovery transients and is
often deeply negative regardless of the actual signal quality at
that gain. The EMA-based decision metric weighs the first sample
at 22 % when averaged over 4 reports, but only 2.8 % when averaged
over 8. In practice that's the difference between Fine settling at
the true peak (which is what 0.4.0 does) and bailing one step off
it (which is what 0.3.x sometimes did on SDRplay).

### Fine-phase oscillation, fixed

Reported behavior on weak stations in 0.3.x: the Coarse phase
would find a decent gain, then Fine would alternate between
extreme high (>40 dB) and low (<18 dB) gains before bailing. Root
cause: the unexplored-neighbour walk was anchored on the
controller's *current* position, which after a couple of
direction flips would step *past* the contiguous explored block
around the best-seen index and start probing the table edges.

The walk is now anchored on the best-seen index and only ever
probes the immediate ±1 neighbours. If both neighbours are
already explored, the controller either **settles** (if MER is
acceptable) or **bails** (if not) — no more long-range thrashing.

### `agc-trace.log` — see what the AGC is thinking

Every tune now writes a short, human-readable trace to a file
beside the rest of the app's state. The file is **overwritten at
the start of every tune**, so it always reflects the most recent
attempt and never grows unbounded. Contents:

- A header naming the frequency, driver, antenna, and PPM.
- A `cache HIT` or `cache MISS` line.
- One line per probe with phase (Coarse / Fine / Done), probe
  number, current gain in dB + table index, best-seen gain so
  far, current EMA MER, and the reason the controller is moving
  (or holding).
- A closing `SETTLED` or `BAILED` line with the final gain and
  best observed MER.

To tail it live from PowerShell while a tune is in progress, see
the new "AGC trace log (power users)" section in the README.

This is intentionally a feature, not a debug toggle — there's no
performance cost, the file is tiny (~2 KB per tune), and writes
silently no-op if the data directory is read-only.

## What's unchanged

- SDR backends (RTL-SDR, SDRplay RSP, HackRF): no changes since 0.3.10.
- Audio pipeline (cpal, Opus recording, per-app volume): no changes.
- Station Information / PSD / SIS tables: no changes.
- Album-Art Collage, 24-Hour Song Log, Traffic, Weather radar
  animation: no changes.
- Window-layout persistence and dock behavior: no changes.

## Still deferred

- **rtl_tcp / networked SDRs** — next up in **v0.5.0**, which will
  add both native `rtl_tcp` compatibility and full SoapyRemote
  support so any networked SDR exposed by `SoapySDRServer` is
  selectable from the **📡 SDR Settings…** dialog. If your existing
  `config.toml` has `use_rtl_tcp = true` you get the same one-shot
  warning and local-USB fallback as in 0.3.x for now.

## Installation

Download `nrsc5-studio-0.4.0-windows-x64.zip` from the Assets
section below, unzip anywhere, and run `nrsc5-studio.exe`.
Everything the app needs (nrsc5, SoapySDR, RTL-SDR, SDRplay,
HackRF support, the MSYS2 runtime) is bundled.

SDRplay receivers still need the free **SDRplay API v3.x** service
installed from <https://sdrplay.com> — that one can't be bundled
under SDRplay's license. RTL-SDR works out of the box.

## Upgrading from 0.3.x

Drop the new zip's contents next to (or over) your existing
install. `config.toml`, `play-log.ron`, and `art-cache\` formats
are unchanged. The new `agc-trace.log` and `gain-cache.ron` files
appear next to them on first tune; both are safe to delete at any
time (the gain cache simply rebuilds itself; the trace log
rebuilds on the next tune).

## Verifying the download

```powershell
Get-FileHash .\nrsc5-studio-0.4.0-windows-x64.zip -Algorithm SHA256
```

Compare against the SHA-256 published in the release assets.
