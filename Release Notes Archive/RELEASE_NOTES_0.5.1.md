# NRSC5 Studio 0.5.1 — Single-Session-Per-Station-Tuned

**Release date:** 2026-06-03
**Type:** Patch release (architectural fix)

Thanks to **argilo** (upstream `nrsc5` maintainer) for the architectural
review that prompted this release.

## TL;DR

v0.5.0 introduced libnrsc5 in-process decode but ran one libnrsc5
session per HD subchannel, because I forgot that in reality a single
`nrsc5_pipe_samples_cu8` session demuxes **every** advertised
subchannel internally and emits per-program PCM via the
`program` field on the audio callback.

v0.5.1 rewires the pipeline to use one session per station tuned.
Every advertised HD subchannel now decodes automatically. The
per-program on/off toggle in the Tuner panel is gone; the HD1–HD4
buttons now just pick which program reaches the speaker.

## What changed

### Decode pipeline
- **One** libnrsc5 session per station tuned (was: up to four, one
  per HD1–HD4 toggle).
- I/Q bus fans into a single feeder thread that synchronously calls
  `nrsc5_pipe_samples_cu8`.
- PCM callback demuxes incoming `&[i16]` into one of eight per-program
  ring buffers based on the `program` argument. The active speaker
  ring is selected at the cpal sink.
- Recording continues to target whichever program is selected at the
  moment Record is pressed; the recorder taps the same per-program
  ring.

### GUI
- HD1–HD4 buttons in the Tuner panel no longer have a per-program
  on/off toggle switch.
- Buttons light up automatically once PCM starts flowing on that
  program (independent of whether SIS has advertised it yet).
- Hover hint on a lit button: "Decoding (audio on air)."
- Removed Settings entries: "Auto-decode all advertised subchannels"
  and "Max concurrent decoders" (both meaningless under the new
  model).

### Keyboard shortcuts
- `Alt+1`..`Alt+8` — select the speaker program (unchanged).
- Removed: `Ctrl+Alt+1`..`Ctrl+Alt+8` (add decoder),
  `Ctrl+Alt+X` (remove decoder). The corresponding `UiCommand`
  variants and handlers are gone too.

### Packaging (Linux)
- Debian and RPM packages no longer ship `install-nrsc5-helper.sh`
  or declare `Recommends: nrsc5`. v0.5.0 already moved decode
  in-process via `libnrsc5`; the external `nrsc5` CLI hasn't been
  used at runtime since then.

## Upgrade notes

No user-facing config migration is required. Old `config.toml` files
that contain `auto_decode_all_advertised` or `max_concurrent_decoders`
are silently ignored.

## Internals (for contributors)

- `src/ffi/decoder.rs`: `DecoderInstance` is gone. The new
  `ActiveSession` owns the feeder thread, shutdown channel, eight
  optional `Arc<PcmRing>`s, and eight `Arc<AtomicBool>` audio-started
  flags (one per program slot).
- `src/ffi/mod.rs`: `Nrsc5Process` now holds `session: Option<ActiveSession>`
  instead of `decoders: Vec<DecoderInstance>`. New error variant
  `Nrsc5Error::InvalidProgram(u32)` replaces the per-decoder slot errors.
- `src/app.rs`: removed `reconcile_auto_decoders`, `SetDecoderEnabled`,
  `SetAutoDecodeAllAdvertised`, `SetMaxConcurrentDecoders` handlers.
- `src/gui/state.rs`: removed `auto_decode_all_advertised`,
  `max_concurrent_decoders`, `auto_add_attempted` fields.

## Credits

- **argilo** — for the upstream architectural review of v0.5.0 that
  caught the duplicate-session problem.
