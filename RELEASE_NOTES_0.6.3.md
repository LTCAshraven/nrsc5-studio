# NRSC5 Studio 0.6.3

A maps-and-metadata release. NRSC5 Studio now decodes the **HERE** traffic /
weather data service in addition to the **Total Traffic Network (TTN)** feed
it already supported, gains a brand-new **Engineering Info panel** for
decoder / RF diagnostics, can render maps against an **optional
high-resolution 2× basemap**, reads its service-mode badge straight from the
decoder's **SYNC telemetry** instead of guessing, and **prunes its AAS scratch
directory** so long sessions don't pile up stale broadcast files.

If you don't care about the internals: traffic and weather now work on more
stations, there's a new diagnostics tab, sharper maps if you grab the optional
basemap, a more accurate MP1/MP3/MP11 badge, and tidier disk usage on marathon
listening sessions.

## What's new

### HERE traffic & weather data service

NRSC5 Studio previously decoded traffic and weather only from the **Total
Traffic Network (TTN)** feed. It now also decodes the **HERE** map data
service:

- **HERE traffic tiles** are stitched through the same traffic-map
  compositor as TTN. Tile dimensions are inferred per grid, so TTN's
  200 × 200 tiles and HERE's grids share one code path.
- **HERE weather images** arrive as directly-displayable full frames with a
  geographic bounding box; they're cropped against the basemap and pushed
  into the rolling weather-radar animation just like TTN's DWRO / DWRI
  overlays.

The upshot: stations that broadcast their traffic / weather over HERE now
light up the Traffic and Weather tabs where before they showed nothing.

### New Engineering Info panel

A brand-new **"Engineering Info — Decoder & RF Diagnostics"** tab collects
the broadcast-plant and decoder internals that used to be mixed into the
Station Information panel:

- RF / decoder health, including the tuned **carrier frequency offset (Hz)**.
- Exciter / importer equipment and local-time / leap-second data.
- Live payload presence plus a **rolling, timestamped payload log** that
  records each incoming AAS object (cover art, station logo, traffic tile,
  weather frame) as it arrives.

With these diagnostics moved out, the **Station Information** panel is now a
focused listener-facing identity view. Call sign, slogan, message,
per-subchannel logos, transmitter location, FCC ID, the subchannel line-up,
and data services, with each block living in exactly one panel. (MER / BER
intentionally stay visible in both the Engineering and Signal panels.)

### Optional high-resolution (2×) map basemap

Traffic and weather maps can now render against `res/map2x.png` — a
12032 × 6912 basemap with four times the pixels of the standard
6016 × 3456 `res/map.png` that ships with the app. When it's present the app
prefers it automatically and the overlays render noticeably sharper on large
windows; when it's absent everything falls back to the bundled `map.png`, so
it's a pure drop-in upgrade with nothing to configure.

Because the file is ~57 MB, it's distributed as a **separate download on the
Releases page** rather than baked into the portable zip / `.deb` / `.rpm`:

- **Windows (portable):** drop `map2x.png` into the `res\` folder next to
  `nrsc5-studio.exe`.
- **Linux (.deb / .rpm):** place it at `/usr/share/nrsc5-studio/map2x.png`.

Restart the app and it picks it up. Delete the file to revert to the standard
basemap. See the README's "Optional: high-resolution map basemap" section for
the full walkthrough.

### Accurate service-mode badge from SYNC telemetry

The MP1 / MP3 / MP11 service-mode badge in the Station Information panel is
now driven by the **PSMI value carried in the libnrsc5 `SYNC` event** instead
of being inferred from the highest populated subchannel slot. AM tunes report
their own service modes (`MA1` / `MA3`), and the tuned **carrier frequency
offset (in Hz)** is now surfaced as raw telemetry in the Engineering panel.
The old slot-count heuristic is kept only as a fallback for when the PSMI
value isn't available. Credit to @Argilo for bringing that to my attention.

### Station logos, per subchannel and persisted (#9)

The Station Information panel now shows the broadcast **station logo** for the
selected subchannel — and keeps showing the right one as you switch frequency
or subchannel ([issue #9](https://github.com/LTCAshraven/nrsc5-studio/issues/9)).

HD Radio transmits logos as LOT/AAS image files whose **filename encodes which
subchannel they belong to**: `…SL<CALLSIGN>$$<NN>…`, where `SL` marks a station
logo and `$$<NN>` is the 1-based subchannel number (HD1–HD8). NRSC5 Studio
parses that to route each logo to the correct program slot, then caches it to
disk keyed by frequency and subchannel — `<freq×10>_hd<N>_<hash>.<ext>` (for
example `1003_hd1_1a2b3c4d.png` for HD1 on 100.3 MHz). The logo is
content-hashed and de-duplicated, so only the latest image per (frequency,
subchannel) is kept.

On every retune the cache is replayed into the eight per-subchannel slots, so
the correct logo appears **instantly from disk** — even before the station
re-broadcasts it — and it survives app restarts.

### Maps: resolution-independent projection + cache bootstrap

The traffic/weather overlay-to-basemap projection now scales to the
basemap's actual pixel dimensions rather than assuming the standard `map.png`
size, so overlays land in the right place on either the standard or the 2×
basemap. Traffic and weather maps also **bootstrap from the on-disk AAS cache
on launch**, replaying the most recent tiles and frames so the maps
repopulate immediately after a restart instead of starting blank.

## Fixed

### AAS scratch directory no longer grows unbounded

The shared AAS scratch directory — where LOT payloads (cover art, station
logos, traffic tiles, weather frames) are staged on disk — is now pruned
automatically. Files older than one hour are removed on a five-minute sweep,
so long listening sessions no longer accumulate stale broadcast objects.

## Notes

- The optional `map2x.png` is **not** included in the portable zip or the
  Linux packages — grab it from the Releases assets only if you want the
  sharper maps. Everything works without it.
