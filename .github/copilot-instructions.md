# Copilot Instructions — NRSC5 Studio

## What this project is

NRSC5 Studio is a native Windows/Linux desktop app for HD Radio reception via RTL-SDR or SDRplay dongles. It wraps the `nrsc5` C decoder in a Rust/egui GUI with real-time DSP (spectrum, AGC, constellation), album-art collage, traffic/weather maps, and Opus recording.

## Build

**Windows (primary platform):**

```powershell
# Debug build (recommended for development):
.\scripts\cargo-gnu.ps1

# Release build:
.\scripts\cargo-gnu.ps1 -Configuration release

# Package portable zip (requires release build first):
.\scripts\package-portable.ps1
```

The build targets `x86_64-pc-windows-gnullvm` using a bundled llvm-mingw toolchain at `.toolchains\llvm-mingw-20260505-ucrt-x86_64\`. The `cargo-gnu.ps1` script handles PATH setup, toolchain installation, and alias creation automatically.

**Linux:**

```bash
# Full pipeline:
scripts/build-linux-release.sh

# Manual:
cargo build --release
cargo deb --no-build     # .deb
cargo generate-rpm       # .rpm
```

**Prerequisites:** MSYS2 with `pkg-config`, `libclang`, and SoapySDR dev packages. See `.cargo/config.toml` for the hardcoded MSYS2 paths (`C:\msys64\mingw64\...`) used for pkg-config and libclang.

**Bindgen:** FFI bindings from `res/nrsc5.h` are only regenerated when `NRSC5_GENERATE_BINDINGS=1` is set. Normal builds skip this step.

## Architecture

### Data flow pipeline

```
SDR hardware
  → SoapySdr / RtlTcpSdr (src/sdr/)         # I/Q acquisition
  → IqBus (src/sdr/iq_bus.rs)                # fan-out to consumers
  → SpectrumTap (src/dsp/spectrum.rs)        # FFT for waterfall/spectrum UI
  → Nrsc5Session::pipe_samples_cu8 (src/ffi/api.rs)
                                             # in-process libnrsc5.dll decode
  → trampoline → NrscEvent channel           # metadata, MER, BER, sync, LOT bytes
  → PcmSink → PcmRing → cpal audio output    # s16le 44.1kHz stereo via src/audio/
  → RecordingSession (src/recorder/)         # optional 96kbps Opus recording
```

`libnrsc5.dll` (built by `scripts/build-nrsc5-msys2.ps1` from the upstream
`theori-io/nrsc5` repo at tag `v3.1.0`) is loaded in-process via implicit
linking (`#[link(name = "nrsc5")]`). Each `DecoderInstance` owns one
`Nrsc5Session` driven by a dedicated feeder thread that pumps I/Q samples
from the shared `IqBus` and synchronously calls `pipe_samples_cu8`.
Decoded events arrive on libnrsc5's worker thread via a C callback; the
safe wrapper in `src/ffi/api.rs` translates each `nrsc5_event_t` into an
owned `NrscEvent` (copying every C string and slice — strings are
`to_string_lossy` since station messages may be Latin-1). PCM audio
takes a separate fast path: a typed `PcmSink` callback receives a
borrowed `&[i16]` and pushes into the per-program `PcmRing` without
allocation.

LOT (large-object-transfer) payloads arrive in-callback with the raw
bytes; the app layer writes them to the AAS scratch directory
(`Nrsc5Process::aas_dir`) before the cover-art / traffic-map /
weather-map processors read them back from disk.

### GUI architecture (immediate-mode)

The app uses egui's immediate-mode pattern with `egui_dock` for a dockable tab layout:

- **`App` (src/app.rs)** — the eframe entry point. Owns the `Nrsc5Process`, processes `NrscEvent`s from the decoder channel each frame, and dispatches `UiCommand`s from the dock.
- **`AppState` (src/gui/state.rs)** — read-only snapshot of runtime state passed to the dock each frame. The dock never mutates app state directly.
- **`DockViewer` / `DockTab` (src/gui/dock.rs)** — renders each tab (Tuner, Spectrum, Signal, Log, Collage, etc.). User actions produce `UiCommand` enum variants sent back to `App` via an `mpsc` channel.
- **One-way data flow:** `App` → `AppState` → `DockViewer` → `UiCommand` → `App`.

### SDR backend abstraction

`trait Sdr` (src/sdr/mod.rs) abstracts over device backends:
- `SoapySdr` — primary backend via libSoapySDR (RTL-SDR, SDRplay, HackRF)
- `RtlTcpSdr` — native rtl_tcp network client
- `DeviceProfile` (src/sdr/profile.rs) — per-device gain tables, AGC element selection, sample rate negotiation

SDRplay devices require software resampling (2 Msps → 1.488375 Msps) via the `rubato` crate because nrsc5 requires an exact sample rate that falls in a gap of SDRplay's supported rates.

### Persistence

- `config.rs` — user settings serialized to `config.toml` (RON for gain cache)
- `art_cache.rs` — content-addressed on-disk image cache (SHA-256 keyed)
- `play_log.rs` — 24-hour rolling song log with CSV export
- Portable mode (presence of `portable.txt`) keeps all state in `data\` beside the exe; otherwise uses `%APPDATA%` / `%LOCALAPPDATA%`.

## Key conventions

- **Error handling:** `anyhow::Result` for application-level errors; `thiserror` enums (`SdrError`, `Nrsc5Error`, `Nrsc5ApiError`) for typed domain errors in the SDR and FFI layers.
- **Threading model:** Background threads (SDR stream, per-decoder I/Q feeder, libnrsc5 worker, AGC controller) communicate with the GUI via `crossbeam-channel` or `std::sync::mpsc`. The GUI thread never blocks on I/O.
- **Unsafe surface:** 100% of the project's `unsafe` lives in `src/ffi/api.rs` (callback trampoline + linked-list walks + slice/string copy-out). Everything else is safe Rust.
- **Gain values** are stored in tenths of dB (`i32`) throughout the codebase and snapped to the nearest device gain-table step at apply time.
- **Release profile** optimizes aggressively: `opt-level = "z"`, LTO, `panic = "abort"`, strip symbols. Debug builds set `opt-level = 3` for dependencies only (real-time DSP can't keep up at opt-level 0).
- **Window subsystem:** Release builds use `#![windows_subsystem = "windows"]` (no console); debug builds keep the console for println debugging.
- **DLL loading:** Native DLLs (`libSoapySDR.dll`, `libnrsc5.dll`, `librtlsdr.dll`, Soapy modules) are resolved at startup by prepending `bin/` to PATH and setting `SOAPY_SDR_PLUGIN_PATH`. `libnrsc5.dll` is implicitly linked via `#[link(name = "nrsc5")]`; the Soapy modules use runtime `libloading`.
- **Platform guards:** Windows-specific code (e.g., `rfd` file dialogs, DLL path setup) is gated with `cfg(windows)` / `cfg(target_os = "linux")`. Linux uses the `xdg-portal` feature for file dialogs.
