# Ubuntu Bring-Up (Linux Port)

This project is currently Windows-first, but Linux bring-up is in progress.
Use this runbook on Ubuntu 22.04.5 LTS to validate native Linux builds.

## 1. Clone and enter the repo

```bash
git clone <your-fork-or-origin-url> nrsc5-rust
cd nrsc5-rust
```

## 2. Run the setup/check script

```bash
bash scripts/linux-ubuntu-bringup.sh
```

What this does:
- Installs required apt packages (clang/libclang, SoapySDR, RTL-SDR, X11/Wayland dev libs, GTK).
- Installs rustup if missing.
- Ensures stable toolchain and Linux target.
- Ensures an `nrsc5` helper binary is available (tries `apt install nrsc5`).
- Runs `cargo check --target x86_64-unknown-linux-gnu`.

## 3. Run the app (dev build)

```bash
cargo run
```

Note: Linux needs `nrsc5` (no `.exe`) available either on PATH or at
`./bin/nrsc5` inside the repo/runtime folder.

If you compile `nrsc5` yourself, copy/symlink it to `./bin/nrsc5`.
This is the preferred dev setup because it keeps the helper version
explicit and independent from distro package revisions.

If app start reports `failed to spawn nrsc5 process: permission denied`,
ensure the helper is executable:

```bash
chmod +x ./bin/nrsc5
```

## 4. If SoapySDR device discovery fails

```bash
SoapySDRUtil --find
SoapySDRUtil --probe="driver=rtlsdr"
```

If those fail, verify that `librtlsdr` and Soapy modules are installed and visible.

## 5. Useful SSH workflow from Windows

From PowerShell on your Windows machine:

```powershell
ssh <user>@<ubuntu-host>
cd ~/nrsc5-rust
bash scripts/linux-ubuntu-bringup.sh
cargo run
```

## Notes

- First Linux milestone is compile + launch + SDR detection + tune/start/stop.
- Linux per-process volume parity with Windows COM is not required for the first bring-up milestone.
