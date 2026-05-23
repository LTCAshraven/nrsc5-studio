#!/usr/bin/env bash
set -euo pipefail

# Ubuntu 22.04+ developer bring-up for NRSC5 Studio Linux builds.
# Installs toolchain/system deps, then runs a baseline cargo check.

if [[ "${EUID}" -eq 0 ]]; then
  echo "Run as a normal user (script uses sudo where needed)."
  exit 1
fi

echo "==> Updating apt metadata"
sudo apt-get update

echo "==> Installing build dependencies"
sudo apt-get install -y \
  build-essential \
  pkg-config \
  curl \
  git \
  clang \
  libclang-dev \
  libsoapysdr-dev \
  soapysdr-tools \
  librtlsdr-dev \
  libusb-1.0-0-dev \
  libasound2-dev \
  libwayland-dev \
  libxkbcommon-dev \
  libx11-dev \
  libxrandr-dev \
  libxi-dev \
  libxcursor-dev \
  libxinerama-dev \
  libgl1-mesa-dev \
  libgtk-3-dev \
  pulseaudio-utils \
  xdg-desktop-portal

if ! command -v rustup >/dev/null 2>&1; then
  echo "==> Installing rustup"
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable
fi

# Ensure cargo is on PATH whether rustup was just installed or was already present.
# shellcheck disable=SC1090,SC1091
source "$HOME/.cargo/env"

echo "==> Ensuring rustup toolchain + target"
rustup toolchain install stable
rustup default stable
rustup target add x86_64-unknown-linux-gnu

echo "==> Printing SoapySDR probe"
SoapySDRUtil --info || true

# Handle SoapySDRPlay3 module if bundled in ./bin/
if [[ -f "./bin/libsdrPlaySupport.so" ]]; then
  echo "==> Installing bundled SoapySDRPlay3 module"
  sudo mkdir -p /usr/local/lib/SoapySDR/modules0.8
  sudo cp ./bin/libsdrPlaySupport.so /usr/local/lib/SoapySDR/modules0.8/
  sudo ldconfig
  if [[ -f /usr/local/lib/SoapySDR/modules0.8/libsdrPlaySupport.so ]]; then
    echo "✓ SoapySDRPlay3 module installed successfully"
  else
    echo "✗ SoapySDRPlay3 module install failed"
    exit 1
  fi
fi

NRSC5_HELPER=""
if [[ -f "./bin/nrsc5" && ! -x "./bin/nrsc5" ]]; then
  echo "==> fixing execute bit on ./bin/nrsc5"
  chmod +x ./bin/nrsc5 || true
fi

if [[ -x "./bin/nrsc5" ]]; then
  NRSC5_HELPER="$(pwd)/bin/nrsc5"
elif command -v nrsc5 >/dev/null 2>&1; then
  NRSC5_HELPER="$(command -v nrsc5)"
else
  echo "==> nrsc5 helper not found; attempting apt install"
  sudo apt-get install -y nrsc5 || true
  if command -v nrsc5 >/dev/null 2>&1; then
    NRSC5_HELPER="$(command -v nrsc5)"
  fi
fi

if [[ -z "${NRSC5_HELPER}" ]]; then
  cat <<'EOF'
ERROR: `nrsc5` helper is required for streaming tests and was not found.

Provide one of these:
1) Install from distro packages so `nrsc5` is on PATH, or
2) Place your compiled helper at ./bin/nrsc5

Then re-run this script.
EOF
  exit 1
fi

echo "==> nrsc5 helper found: ${NRSC5_HELPER}"
"${NRSC5_HELPER}" --version || true

echo "==> Running format-safe compile check"
cargo check --target x86_64-unknown-linux-gnu

echo "==> Done"
