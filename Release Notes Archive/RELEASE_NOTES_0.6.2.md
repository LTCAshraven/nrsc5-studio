# NRSC5 Studio 0.6.2

A small feature-and-fix release. The Station Information panel gains a
**live per-program audio bit rate** readout, the tuner now **snaps to
the US 200 kHz FM channel raster**, and Linux packages finally ship a
**self-contained `libnrsc5.so`** so a fresh `.deb` / `.rpm` install
runs without a system-installed decoder.

If you don't care about the internals: HD subchannels now show their
decoded kbps, you can't accidentally tune off-channel, and Linux
installs Just Work.

## What's new

### Per-program decoded audio bit rate

The Station Information panel now shows a live `kbps` readout for the
tuned subchannel (HD1–HD8), right alongside the Now Playing line.

Stock `libnrsc5` v3.2.0 doesn't emit a decoded-bit-rate event, so the
safe FFI wrapper derives it itself from the raw HDC packet stream:
it accumulates packet bytes and CRC-valid frames per program and emits
an estimate every 32-frame window, reproducing the upstream `nrsc5`
CLI's `Audio bit rate:` math:

```text
kbps = bytes * 8 * SAMPLE_RATE_AUDIO / AUDIO_FRAME_SAMPLES / frames / 1000
```

Thanks to **TheDaChicken**, **argilo**, and **pclov3r** on the upstream
[`theori-io/nrsc5`](https://github.com/theori-io/nrsc5) repo for keeping
me on track about where to hook the HDC packet stream and how the CLI
derives the rate.

### FM tuning snapped to the channel raster

Tuner input, presets, and the boot frequency are now clamped to
87.9–107.9 MHz and snapped to the nearest valid 0.2 MHz US FM channel
center (anchored at 87.9 MHz). An out-of-raster frequency left in an
existing `config.toml` is corrected and re-saved on the next launch,
so you can't end up parked between channels.

### Linux: self-contained bundled libnrsc5.so

This fixes a runtime "library not found" failure on a clean Linux
install. The `.deb` / `.rpm` now ship a private `libnrsc5.so` at
`/usr/lib/nrsc5-studio/`, resolved via the binary's `RUNPATH`, instead
of relying on a system-installed decoder.

A new `scripts/build-nrsc5-linux.sh` builds that library from upstream
[`theori-io/nrsc5`](https://github.com/theori-io/nrsc5) **v3.2.0** with
three Linux-specific static-link patches, leaving the `.so` dependent
only on `libm` / `libc`. `dpkg-shlibdeps` treats the bundled library as
private, so the package `Depends` stay clean (no bogus external
`libnrsc5` dependency). Packaging metadata, lintian overrides, the man
page, AppStream metainfo, and the install docs were all updated to
describe the bundled library.

## Internal

- **Dead-code cleanup.** Removed orphaned code from past pivots (the
  `widgets.rs` iOS toggle switch from the removed multi-decoder gates,
  six dead `SdrError` variants from the native-librtlsdr backend, the
  legacy `use_piped_sdr` config field, and the external-process `pid()`
  shim) and annotated intentional future-use API with
  `#[allow(dead_code)]` + a rationale comment. Zero dead-code warnings
  remain; all 135 tests pass. No user-facing behavior change.

## Acknowledgements

- **TheDaChicken**, **argilo**, and **pclov3r** (upstream
  [`theori-io/nrsc5`](https://github.com/theori-io/nrsc5)) for the
  guidance on deriving the decoded audio bit rate from the HDC stream.
