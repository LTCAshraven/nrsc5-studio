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

echo "==> Running format-safe compile check"
cargo check --target x86_64-unknown-linux-gnu

echo "==> Done"
