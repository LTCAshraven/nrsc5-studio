# NRSC5 Studio 0.3.8

In-process audio. `nrsc5.exe` now pipes raw PCM into NRSC5 Studio
on stdout, and a single `cpal`-backed output stream owned by the
studio process plays it back. The Windows volume mixer now shows
the slider under `nrsc5-studio.exe` instead of `nrsc5.exe`, volume
and mute work before you even tune a station, and a handful of
long-standing reliability bugs around SDR hot-plug and external
process kills got cleaned up along the way.

This release is the foundation for the next three (I/Q fan-out,
multi-program decode, Opus recording). On its own it's a pure
quality-of-life upgrade — same single-program listening, just
with the audio plumbing finally living in the right place.

## What's new

### Audio is now owned by NRSC5 Studio itself

Pre-0.3.8, `nrsc5.exe` opened its own libao audio session on the
side. The studio app couldn't see the PCM; it could only beg the
OS for a handle to nrsc5's WASAPI session to drive the per-app
volume slider. The slider sometimes showed up minutes late, and
sometimes not at all.

Now the helper is launched with `-o -` and emits 44.1 kHz stereo
PCM on stdout. The studio process reads that pipe and pumps it
through cpal. Concretely:

- **Volume slider and mute toggle are always live.** They work
  before a station is tuned, instead of waiting for nrsc5's audio
  session to appear in WASAPI.
- **In the Windows volume mixer:** the slider you want is the
  one under `nrsc5-studio.exe`. The `nrsc5.exe` entry is gone.
- **Audio works on devices that only expose 48 kHz.** The output
  stream now opens at whatever sample rate the default device
  advertises and resamples 44.1 → device-native inline. Before,
  many modern Windows defaults would just produce silence.

### Faster, calmer AGC on SDRplay

The controller now starts at 39 dB and walks **up** by default
(was: 38 dB walking down). On weak / fringe stations it settles
noticeably faster, and a total loss-of-sync at the new gain
correctly flips the walk direction back toward the best-seen MER
instead of marching further into overload.

RTL-SDR and HackRF behavior is unchanged.

### Reliability fixes

- **Hot-plug an SDR mid-session without freezing.** The presence
  probe now runs on a background thread. Previously, replugging
  an SDRplay made the GUI go "Not Responding" for several seconds
  while the SDRplay API service did its device-discovery handshake.
- **External kills auto-recover.** If `nrsc5.exe` dies — `taskkill`,
  a crash, anything — NRSC5 Studio notices, stops cleanly, and
  the Start button lights up. No more Stop+Start dance to get
  audio back.
- **No-SDR overlay no longer false-positives on SDRplay.** The
  probe now falls back to `SoapySDRUtil` enumeration for any of
  the supported non-RTL drivers (SDRplay, Airspy, HackRF, Lime,
  PlutoSDR, remote).

## A note on Linux

The audio refactor is platform-neutral. `cpal` already covers
Windows (WASAPI), Linux (ALSA / Pulse / PipeWire), and macOS
(CoreAudio), and the new audio module has zero `#[cfg(target_os)]`
gates — same code drives every platform. The Linux build chain
itself is still pending validation on an Ubuntu host (the issue
sits in `soapysdr-sys` bindgen, not in audio), but the audio side
of a Linux port is no longer separate work.

## What's unchanged

- **The bundled `nrsc5.exe` is the same revision** as 0.3.6
  (aa645c2). No helper-side changes were needed.
- **SDR backends:** RTL-SDR and SDRplay both work the same way
  as 0.3.6.
- **DSP:** spectrum, constellation, MER/BER, and PSD/SIS panels
  are byte-identical.
- **Recording:** still not in this release. That's Phase 4 of
  the 0.4.0 arc.

## Upgrade notes

Drop-in. No config migration, no res/ asset changes, no new CLI
flags. If you were running 0.3.6, just unzip 0.3.8 over the top
and launch.

## Thanks

Phase 1 of the 0.4.0 work was scoped, planned, and executed in
a single afternoon thanks to the dock-out / dock-in smoke-test
discipline that 0.3.x's panel system made possible. Step 6 of
the smoke test (`taskkill /F /IM nrsc5.exe` while a station was
playing) caught the missing auto-recovery; if you find any
behavioral edge case I missed, please file an issue with the
exact reproduction.
