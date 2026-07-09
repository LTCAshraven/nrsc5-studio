# Contributing to NRSC5 Studio

Thanks for your interest in improving NRSC5 Studio! This project is a native
Windows/Linux desktop receiver for HD Radio (NRSC-5), wrapping the `nrsc5`
decoder in a Rust/egui GUI. Contributions of all kinds are welcome — bug
reports, hardware-compatibility feedback, documentation, and code.

## Ways to contribute

- **Report a bug** — open an issue and include your OS, SDR hardware
  (RTL-SDR / SDRplay / other), the app version (**Help → About**), and, when
  relevant, the `agc-trace.log` and the in-app **Log** panel contents.
- **Report hardware compatibility** — this app talks to many SDR devices
  through SoapySDR. Success/failure reports on specific dongles (especially
  SDRplay models and non-reference RTL-SDR tuners) are valuable.
- **Fix a bug or add a feature** — see the setup and workflow below.
- **Report a security vulnerability** — please do **not** open a public
  issue; follow [`SECURITY.md`](SECURITY.md) instead.

## Development setup

The project targets **Windows** (primary) and **Linux**.

### Windows

The Windows build uses a bundled llvm-mingw toolchain targeting
`x86_64-pc-windows-gnullvm`, plus an MSYS2 install providing `pkg-config`,
`libclang`, and the SoapySDR dev files (see [.cargo/config.toml](.cargo/config.toml)
for the expected `C:\msys64\...` paths).

```powershell
# 1. One-time: fetch the toolchain + native dependencies.
.\scripts\bootstrap-deps.ps1

# 2. Debug build (handles PATH, toolchain install, and aliases for you):
.\scripts\cargo-gnu.ps1

# Release build:
.\scripts\cargo-gnu.ps1 -Configuration release

# Run the test suite (on the gnullvm target):
.\scripts\cargo-gnu.ps1 -Command test
```

### Linux

**Prerequisites** (Ubuntu 22.04+ / Debian 12+) — the bringup script installs
them, or install manually per [docs/linux-install.md](docs/linux-install.md):

```bash
# Installs build deps and the Rust toolchain.
bash scripts/linux-ubuntu-bringup.sh

# Build the bundled decoder library once (staged at bin/libnrsc5.so):
bash scripts/build-nrsc5-linux.sh

# Then the usual cargo commands work directly:
cargo build --release
cargo test
```

## Before you open a pull request

Please make sure the same checks CI runs pass locally (see
[.github/workflows/ci.yml](.github/workflows/ci.yml)):

```bash
cargo fmt --all --check                       # formatting
cargo clippy --all-targets --all-features -- -D warnings   # lints (no warnings)
cargo test --all-features                     # tests
cargo deny check                              # advisories + licenses + sources
```

On Windows, run the build/test through `.\scripts\cargo-gnu.ps1` so the
toolchain and `libnrsc5` link paths are set up correctly.

`cargo deny` requires a one-time `cargo install cargo-deny`. If it flags a
new advisory on a transitive dependency you can't upgrade, add a documented
`ignore` entry (with a `reason`) to [deny.toml](deny.toml) rather than
disabling the check.

## Coding conventions

These mirror the patterns already in the codebase (see
[.github/copilot-instructions.md](.github/copilot-instructions.md) for the
full architecture tour):

- **Error handling:** `anyhow::Result` for application-level code;
  `thiserror` enums (`SdrError`, `Nrsc5Error`, `Nrsc5ApiError`) for typed
  domain errors in the SDR/FFI layers.
- **Unsafe code:** 100% of the project's `unsafe` lives in
  [src/ffi/api.rs](src/ffi/api.rs) (the FFI callback trampoline and
  copy-out). Keep it that way — everything else is safe Rust.
- **Threading:** background threads communicate with the GUI via
  `crossbeam-channel` / `std::sync::mpsc`; the GUI thread never blocks on
  I/O.
- **GUI data flow is one-way:** `App` → `AppState` → `DockViewer` →
  `UiCommand` → `App`. The dock renders from a read-only snapshot and sends
  commands back; it never mutates app state directly.
- **Gain values** are stored in tenths of a dB (`i32`) throughout and snapped
  to the nearest device gain-table step at apply time.
- **Platform-specific code** is gated with `cfg(windows)` /
  `cfg(target_os = "linux")`.
- Add unit tests alongside the code you change (`#[cfg(test)] mod tests`),
  matching the existing per-module test style.

## Changelog and versioning

- The project follows [Keep a Changelog](https://keepachangelog.com/) and
  [Semantic Versioning](https://semver.org/).
- Add an entry to the appropriate section (**Added / Changed / Fixed /
  …**) under the in-progress version heading in [CHANGELOG.md](CHANGELOG.md)
  as part of your change.

## Pull request process

1. Fork the repo and create a topic branch off `main`.
2. Make your change, including tests and a CHANGELOG entry.
3. Run the full check set above and make sure it's green.
4. Open a PR with a clear description of *what* changed and *why*. Link any
   related issue.
5. Keep PRs focused — one logical change per PR is easier to review.

## Licensing of contributions

The project **source** is licensed under **MIT** (see [LICENSE](LICENSE)). By
submitting a contribution, you agree to license it under the same terms.

Note that the distributed **binary** is **GPL-3.0**, because it links the
GPL-licensed `libnrsc5` decoder. This source/binary split is explained in
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md); it does not change the terms
under which you contribute source code.
