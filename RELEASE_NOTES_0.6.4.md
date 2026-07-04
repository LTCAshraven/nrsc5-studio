# NRSC5 Studio 0.6.4

This release adds an **analog-FM fallback path** so a tuned station keeps
playing even when the HD signal can't lock — with full **stereo** decoding and
**RDS** (Program Service + RadioText) text. It also fixes the Engineering Info
service-mode badge, which is now **band-aware** so FM stations are no longer
mislabeled with an AM-only service mode.

If you don't care about the internals: weak FM stations now fall back to clean
analog stereo (with the station name and scrolling text from RDS) instead of
going silent, and the little `MP1` / `MA1` mode badge shows the correct code
for the band you're tuned to.

I'm calling this the 'My God Bones, what have I done.' release, LOL! #IYKYK

## What's new

### Analog-FM fallback with stereo and RDS

When the HD signal drops out — a weak fringe station, a deep fade, or a
non-HD FM broadcast — NRSC5 Studio can now demodulate the underlying analog
FM signal from the same I/Q stream and keep audio flowing.

A new **Mode Select** control on the Station Information panel chooses the
source:

- **Digital Only** — the analog path stays silent (the classic DXer /
  silence-as-cue behavior). This is the default, so existing setups are
  unchanged.
- **Automatic** — plays HD audio while the decoder is synced, then falls
  back down the ladder to analog stereo → mono → squelch as the signal
  weakens, and climbs back to HD when it recovers.
- **Analog Only** — forces the analog-FM demodulator to own the audio,
  ignoring HD entirely.

The analog path is a full stereo receive chain built on the shared I/Q bus:

- **Stereo decode** locks the 19 kHz pilot with a PLL and coherently
  recovers the L−R difference. A pilot-strength blend fades stereo width
  toward zero as the pilot weakens, so it degrades continuously to clean
  mono instead of getting noisy. Stereo can be toggled off to force mono.
- **75 µs de-emphasis** per channel and resampling to the playback rate.
- **RDS decoding** of the 57 kHz subcarrier surfaces the **Program
  Service** name (used as the station-name display) and **RadioText** (the
  long scrolling song/artist/promo field), shown in a full-width RDS ticker
  and as the now-playing fallback when no HD metadata is present. RDS can be
  toggled off independently.

All three toggles (mode, stereo, RDS) persist in the config.

### Station-logo discovery via three-step MIME detection

Station logos now surface reliably even when LOT metadata is incomplete or
uses generic image MIME types. The app uses a three-step fallback cascade:

1. **Direct MIME tags** — if the LOT payload's own MIME field or the
   associated SIG component's MIME says **`NRSC5_MIME_STATION_LOGO`**,
   classify it as a station logo immediately.
2. **Album-art vs. generic image MIME** — check for **`NRSC5_MIME_PRIMARY_IMAGE`**
   (album art) or generic **JPEG/PNG** MIME tags to distinguish cover art
   from logos.
3. **Filename heuristic** — when MIME is generic or missing, parse the LOT
   filename for station-logo naming patterns:
   - `SL<CALLSIGN>$$<NN>` (legacy/known-good)
   - `SL...HD<n>` (SLHD variants)
   - `<CALLSIGN>HD<n>` (bare callsign + subchannel)

   If a match is found, recover the station logo even without explicit MIME
   tagging. All logos are cached by content hash and keyed with a `.json`
   sidecar tracking the classification method for transparency.

## What's fixed

### FM service-mode badge no longer mislabeled as an AM mode

The Engineering Info panel mapped raw **PSMI** values to service-mode codes
without accounting for the tuned band. Because AM and FM reuse the same
underlying PSMI numbers, a standard FM hybrid station (PSMI 1) was shown as
**MA1** — an AM-only mode — instead of **MP1**.

The badge is now band-aware, using the same band discriminator as the rest of
the SYNC telemetry:

- **AM tunes** report `MA1` / `MA3`.
- **FM tunes** report `MP1` / `MP2` / `MP3` / `MP5` / `MP6` / `MP11`.

These map directly to nrsc5's own `SERVICE_MODE_*` definitions, so the badge
now agrees with the decoder for both bands.

Thanks to [@TechnicalLee](https://github.com/TechnicalLee) for catching the
incorrect descriptor
([#16](https://github.com/LTCAshraven/nrsc5-studio/issues/16)).

## Downloads

- **Windows (portable):** `nrsc5-studio-0.6.4-windows-x64.zip` — unzip and run
  `nrsc5-studio.exe`.
- **Linux (Debian/Ubuntu):** `nrsc5-studio_0.6.4-1_amd64.deb`
- **Linux (Fedora/RHEL):** `nrsc5-studio-0.6.4-1.x86_64.rpm`

The optional high-resolution `map2x.png` basemap from 0.6.3 still applies and
is unchanged — grab it from the 0.6.3 assets if you want the sharper maps.
