## NRSC5 Studio 0.3.5

Identity release. Everything nrsc5 prints from a station's SIS (Station
Information Service) table — call sign, slogan, message banner, country
and FCC facility ID, transmitter location, per-subchannel program
metadata, data services, emergency alerts — now has a first-class home
in a new **📚 Station Information** dock tab. The Tuner panel's
HD1–HD8 selector is SIS-aware: subchannels that the station actually
advertises light up; the rest stay clickable but dimmed.

No SDR backend or DSP changes. RTL-SDR and SDRplay behave identically
to 0.3.1.

### New: 📚 Station Information dock tab

A dedicated panel surfacing everything the broadcast hands you about
the station you're tuned to:

- **Call sign** plus a service-mode badge (`MP1` / `MP3` / `MP11`,
  marked *inferred* since nrsc5 doesn't emit the mode directly —
  it's derived from the highest populated program slot).
- **Slogan** and **station message** (the rolling text banner some
  stations broadcast).
- **Emergency alerts** in a red callout banner when set.
- **Country** and **FCC facility ID**, with the FCC ID linked to
  fcc.gov's public facility lookup so you can pivot straight from
  "what am I hearing?" to "who owns this transmitter?"
- **Transmitter location** — latitude, longitude, altitude in
  meters.
- **Subchannel grid** with five columns per program slot: program
  number, short name, program type, sound experience, and the
  new per-program audio bit rate in kbps.
- **Data services** list (SIG-table service number, name, MIME
  type, service-data-type label) — same services that power the
  Traffic, Weather, and Album-Art collage panels, now visible
  side-by-side with their broadcast metadata.
- **"Last updated" footer** so it's clear how recently each field
  has been refreshed by the broadcast cycle.

Fields populate progressively after sync — call sign and slogan
arrive within seconds, FCC ID and country a minute or so later,
location can take several minutes, and many stations simply never
broadcast `Message` or `Alert` at all. A `Waiting for SIS…`
placeholder is shown until the first field lands. State is cleared
on retune and Stop, and a 5-second grace window keeps brief sync
flickers from blanking the panel.

### New: SIS-aware HD1–HD8 program selector

The Tuner panel's subchannel buttons now consult the station's
advertised program list:

- Subchannels the station broadcasts render at full intensity.
- Subchannels it doesn't broadcast are dimmed but still clickable,
  with a tooltip explaining the station doesn't list that program.
  You can still tune them in case SIS hasn't caught up — the click
  is never blocked.

Practical effect: at a glance you can see whether the station you're
on is HD1-only, HD1+HD2, or a full HD1+HD2+HD3 stack, without
having to probe each button.

### New: per-program audio bit rate

The subchannel grid's new fifth column shows the live audio bit
rate nrsc5 reports for each subchannel — typically 24–96 kbps
depending on whether the station is MP1 (one program at high
bitrate), MP3 (one main + two secondaries at lower bitrates), or
MP11 (different split). Values update continuously as nrsc5 re-
reports them, not just once at start of stream.

### Diagnostic: SoapySDR stream failures now logged

When the in-process SoapySDR backend's read loop hits an error,
the actual underlying error text is now printed to stderr as
`[sdr] run_stream failed: <error>` immediately before the
`device lost` event is propagated to the UI. Previously the
real cause (USB stall, sample-rate mismatch, API timeout, etc.)
was swallowed and only the generic "device lost" surfaced. Makes
SDRplay troubleshooting tickets one screenshot instead of three.

### Migration

Drop-in upgrade from 0.3.1. Saved presets, play-log entries, dock
layout, art cache, weather frames, and config all carry over. Two
under-the-hood changes happen silently on first launch:

- The legacy `station_name` / `short_names` in-memory fields were
  replaced by the unified `station_info.programs[]` aggregate.
  Nothing on disk is affected.
- The Now Playing tab's old "KEGL 101.1 HD2" identity line was
  removed — that information now lives in the Station Information
  panel where it can be shown alongside slogan, message,
  location, and the rest of the SIS table.

Preset auto-labels now fall back through SIS short name → artist
→ SIS call sign → LOT-derived call sign → `HDn` (was: just the
legacy `station_name`).

### Install

Download `nrsc5-studio-0.3.5-windows-x64.zip` below, extract
anywhere, run `bin\nrsc5-studio.exe`. SDRplay users additionally
need the free SDRplay API service from sdrplay.com (it can't be
bundled per the Xperi / SDRplay licensing).

### Acknowledgments

This release continues to lean on upstream work:

- [theori-io/nrsc5](https://github.com/theori-io/nrsc5) (Aiden / theori) — the v3.1.0 decoder shipping in the bundle.
- [osmocom/rtl-sdr](https://github.com/osmocom/rtl-sdr) — modern `librtlsdr.dll` with clean tuner-gain semantics.
- [pothosware/SoapySDR](https://github.com/pothosware/SoapySDR) — the multi-SDR abstraction layer.
- [pothosware/SoapySDRPlay3](https://github.com/pothosware/SoapySDRPlay3) — SDRplay backend.

The Station Information panel, SIS-aware HD1–HD8 selector, per-
program audio bit rate display, and SoapySDR error-surfacing
diagnostic were developed in collaboration with GitHub Copilot.

MIT licensed.
