#!/usr/bin/env bash
#
# Build NRSC5 Studio Linux release artifacts (.deb and .rpm).
#
# Runs on an Ubuntu 22.04+ / Debian 12+ host that has already been
# bootstrapped with `scripts/linux-ubuntu-bringup.sh`. The bringup
# script installs the system development packages (clang, SoapySDR
# dev, librtlsdr dev, ALSA dev, X11/Wayland dev, GTK, etc.); this
# script adds the two Rust packaging helpers `cargo-deb` and
# `cargo-generate-rpm`, runs a release build, and emits both
# artifacts into `dist/`.
#
# Usage from the repo root:
#
#   bash scripts/build-linux-release.sh
#
# Output:
#
#   dist/nrsc5-studio_<version>-1_amd64.deb
#   dist/nrsc5-studio-<version>-1.x86_64.rpm
#
# Both can be scp'd back to a Windows host and attached to the
# GitHub release.

set -euo pipefail

# Resolve repo root from this script's location so it works regardless
# of where the user invoked it from.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

# Pull the version straight from Cargo.toml so artifact names always
# match the source of truth — avoids a release-day mismatch between
# the tag and the package file names.
VERSION="$(grep -m1 '^version = ' Cargo.toml | sed -E 's/version = "([^"]+)"/\1/' | tr -d '\r\n')"
if [[ -z "$VERSION" ]]; then
  echo "ERROR: could not parse version from Cargo.toml" >&2
  exit 1
fi
echo "==> Building NRSC5 Studio $VERSION Linux artifacts"

# Ensure cargo is on PATH whether this was invoked from a login shell
# (rustup-installed cargo is in ~/.cargo/bin which login shells pick up)
# or from a non-login shell that hasn't sourced ~/.cargo/env yet.
if [[ -f "$HOME/.cargo/env" ]]; then
  # shellcheck disable=SC1090,SC1091
  source "$HOME/.cargo/env"
fi

if ! command -v cargo >/dev/null 2>&1; then
  echo "ERROR: cargo not found on PATH. Run scripts/linux-ubuntu-bringup.sh first." >&2
  exit 1
fi

# Install packaging helpers if missing. Both are pure-Rust tools that
# install into ~/.cargo/bin, so no apt step is needed.
if ! command -v cargo-deb >/dev/null 2>&1; then
  echo "==> Installing cargo-deb"
  cargo install cargo-deb
fi

if ! command -v cargo-generate-rpm >/dev/null 2>&1; then
  echo "==> Installing cargo-generate-rpm"
  cargo install cargo-generate-rpm
fi

# Build the self-contained libnrsc5.so and stage it at bin/libnrsc5.so.
# This must run before the cargo build below so the link step resolves
# -lnrsc5, and before cargo deb / generate-rpm so the .so is present to
# bundle. Set NRSC5_SKIP_SO_BUILD=1 to reuse an already-staged copy.
if [[ "${NRSC5_SKIP_SO_BUILD:-0}" != "1" ]]; then
  echo "==> Building bundled libnrsc5.so"
  bash scripts/build-nrsc5-linux.sh
elif [[ ! -f bin/libnrsc5.so ]]; then
  echo "ERROR: NRSC5_SKIP_SO_BUILD=1 but bin/libnrsc5.so is missing." >&2
  echo "       Run scripts/build-nrsc5-linux.sh first." >&2
  exit 1
fi

# Clean release build. Strip is handled by the [profile.release] section
# in Cargo.toml so the resulting binary is small even before packaging.
echo "==> cargo build --release"
cargo build --release

# Refresh hicolor icon PNGs from src/icon_render.rs so the packaged
# icons always match the in-app icon. Cheap to re-run (sub-second after
# the dev-dep crates are cached). The PNGs are also tracked in git so
# this is normally a no-op.
echo "==> Rendering Linux icons"
cargo run --quiet --example render_linux_icons

# Sanity check: the binary exists and is stripped.
BIN="target/release/nrsc5-studio"
if [[ ! -x "$BIN" ]]; then
  echo "ERROR: expected $BIN to exist after build" >&2
  exit 1
fi

mkdir -p dist

# .deb
echo "==> cargo deb --no-build"
cargo deb --no-build
DEB_SRC="target/debian/nrsc5-studio_${VERSION}-1_amd64.deb"
if [[ ! -f "$DEB_SRC" ]]; then
  # cargo-deb sometimes drops the "-1" Debian revision when none is
  # specified; check both filename forms before giving up.
  DEB_SRC_ALT="target/debian/nrsc5-studio_${VERSION}_amd64.deb"
  if [[ -f "$DEB_SRC_ALT" ]]; then
    DEB_SRC="$DEB_SRC_ALT"
  else
    echo "ERROR: expected .deb at $DEB_SRC or $DEB_SRC_ALT" >&2
    ls -la target/debian/ || true
    exit 1
  fi
fi
cp -f "$DEB_SRC" "dist/$(basename "$DEB_SRC")"

# .rpm
echo "==> cargo generate-rpm"
cargo generate-rpm
RPM_SRC="$(find target/generate-rpm -maxdepth 1 -name '*.rpm' -print -quit)"
if [[ -z "$RPM_SRC" || ! -f "$RPM_SRC" ]]; then
  echo "ERROR: cargo-generate-rpm did not produce an .rpm in target/generate-rpm" >&2
  ls -la target/generate-rpm/ || true
  exit 1
fi
cp -f "$RPM_SRC" "dist/$(basename "$RPM_SRC")"

echo
echo "==> Artifacts ready in dist/:"
ls -la dist/*.deb dist/*.rpm

# Optional validators (run only if installed; non-fatal). lintian on the
# .deb surfaces packaging policy violations; rpmlint catches RPM-specific
# problems. Both are advisory.
if command -v lintian >/dev/null 2>&1; then
  echo
  echo "==> lintian dist/$(basename "$DEB_SRC")"
  lintian --no-tag-display-limit "dist/$(basename "$DEB_SRC")" || true
fi
if command -v rpmlint >/dev/null 2>&1; then
  echo
  echo "==> rpmlint dist/$(basename "$RPM_SRC")"
  rpmlint "dist/$(basename "$RPM_SRC")" || true
fi

echo
echo "Smoke-test the .deb on this host with:"
echo "  sudo apt install ./dist/$(basename "$DEB_SRC")"
echo "Smoke-test the .rpm on a Fedora VM with:"
echo "  sudo dnf install ./dist/$(basename "$RPM_SRC")"
