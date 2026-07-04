# NRSC5 Studio 0.3.6

A small, focused follow-up to the 0.3.5 Station Information release.
The new **Station Info** panel now has a top half too: a **PSD
(Program Service Data)** table showing the per-song ID3-style
metadata the station actually broadcasts, alongside the existing
**SIS (Station Information Service)** table below it.

Two of the four PSD fields (Album and Genre) have been parsed from
nrsc5 stderr since the very first release but never had anywhere
to go on screen. They now do.

Thanks to Redditor **u/minecrafter1OOO** for suggesting the
inclusion of the raw station and track stream information, the structure of this release came
directly from their idea.

## What's new

### PSD table at the top of Station Info

Four rows, each rendered only when the station sends the
corresponding ID3v2.4 frame:

- **Song Title** — `TIT2`
- **Artist** — `TPE1`
- **Album** — `TALB` (new on screen)
- **Genre** — `TCON` (new on screen)

Each row appears when the field arrives and disappears about 15
seconds after that *specific* field stops refreshing. So if your
local station keeps pushing Title and Artist between songs but
drops Genre, the Genre row falls off on its own without yanking
the rest of the table with it.

### Cleaner timestamps

The "PSD updated Xs ago" and "SIS updated Xs ago" footers no
longer flip every second. They now step in 10-second buckets
(`just now`, `10s ago`, `20s ago`, ..., `1m ago`, `2m ago`),
which keeps the panel calm to look at while still showing real
freshness.

### Stale data on retune / Stop

Tuning to a different frequency or stopping the stream now
explicitly clears the four PSD strings (in addition to the
already-existing SIS reset), so the panel can no longer briefly
show the previous station's song title while the new station's
metadata rolls in.

## What's unchanged

- SDR backends (RTL-SDR, SDRplay): no changes since 0.3.1.
- DSP / AGC / audio pipeline: no changes since 0.3.1.
- All of the 0.3.5 Station Information features (call sign +
  inferred service mode badge, slogan / message, country / FCC
  facility ID, transmitter location, subchannels grid, data
  services, emergency alerts) work exactly as they did before
  and now live in the lower **SIS** section of the same panel.

## Installation

Download `nrsc5-studio-0.3.6-windows-x64.zip` from the Assets
section below, unzip anywhere, and run `nrsc5-studio.exe`.
Everything the app needs (nrsc5, SoapySDR, RTL-SDR, SDRplay,
HackRF support, the MSYS2 runtime) is bundled.

SDRplay receivers still need the free **SDRplay API v3.x** service
installed from <https://sdrplay.com> — that one can't be bundled
under SDRplay's license. RTL-SDR works out of the box.

## Upgrading from 0.3.5

Drop the new zip's contents next to (or over) your 0.3.5 install.
`config.toml` and `play_log.csv` formats are unchanged.

## Verifying the download

```
Get-FileHash .\nrsc5-studio-0.3.6-windows-x64.zip -Algorithm SHA256
```

Compare against the SHA-256 published in the release assets.
