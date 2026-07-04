#!/usr/bin/env bash
#
# Build libnrsc5.dylib (macOS) from the pinned upstream nrsc5 tag and
# stage it at bin/libnrsc5.dylib for the Rust link step.
#
# Usage from the repo root:
#   bash scripts/build-nrsc5-macos.sh
#
# Optional environment overrides:
#   NRSC5_TAG=v3.2.0
#   NRSC5_JOBS=8
#   NRSC5_INSTALL_DEPS=1

set -euo pipefail

TAG="${NRSC5_TAG:-v3.2.0}"
JOBS="${NRSC5_JOBS:-$(sysctl -n hw.ncpu 2>/dev/null || echo 4)}"
ARCH="$(uname -m)"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
BIN_DIR="$REPO_ROOT/bin"
BUILD_ROOT="${TMPDIR:-/tmp}/nrsc5-macos-build"
SRC_DIR="$BUILD_ROOT/nrsc5-$TAG"

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "ERROR: this script is for macOS/Darwin only" >&2
  exit 1
fi

echo "==> Building libnrsc5.dylib from nrsc5 $TAG (arch=$ARCH, jobs=$JOBS)"

missing=()
for tool in git cmake make cc pkg-config; do
  command -v "$tool" >/dev/null 2>&1 || missing+=("$tool")
done
if [[ ${#missing[@]} -gt 0 ]]; then
  echo "ERROR: missing build tools: ${missing[*]}" >&2
  echo "Install Xcode Command Line Tools and Homebrew first." >&2
  exit 1
fi

if ! command -v brew >/dev/null 2>&1; then
  echo "ERROR: Homebrew (brew) not found" >&2
  exit 1
fi

required_pkgs=(libusb libusb-compat fftw faad2 librtlsdr soapysdr soapyrtlsdr)
if [[ "${NRSC5_INSTALL_DEPS:-0}" == "1" ]]; then
  echo "==> Installing required Homebrew packages"
  brew install "${required_pkgs[@]}"
else
  missing_pkgs=()
  for pkg in "${required_pkgs[@]}"; do
    brew list --formula "$pkg" >/dev/null 2>&1 || missing_pkgs+=("$pkg")
  done
  if [[ ${#missing_pkgs[@]} -gt 0 ]]; then
    echo "ERROR: missing Homebrew packages: ${missing_pkgs[*]}" >&2
    echo "Install them with:" >&2
    echo "  NRSC5_INSTALL_DEPS=1 bash scripts/build-nrsc5-macos.sh" >&2
    exit 1
  fi
fi

BREW_PREFIX="$(brew --prefix)"
export PKG_CONFIG_PATH="$BREW_PREFIX/lib/pkgconfig:$BREW_PREFIX/opt/libusb/lib/pkgconfig:$BREW_PREFIX/opt/fftw/lib/pkgconfig:$BREW_PREFIX/opt/librtlsdr/lib/pkgconfig:$BREW_PREFIX/opt/soapysdr/lib/pkgconfig:${PKG_CONFIG_PATH:-}"
export CMAKE_PREFIX_PATH="$BREW_PREFIX:${CMAKE_PREFIX_PATH:-}"

rm -rf "$SRC_DIR"
mkdir -p "$BUILD_ROOT"
git clone --depth 1 --branch "$TAG" https://github.com/theori-io/nrsc5.git "$SRC_DIR"
cd "$SRC_DIR"
echo "==> ref: $(git log -1 --format='%H %s')"

mkdir -p build
cd build

cmake_args=(
  -D CMAKE_BUILD_TYPE=Release
  -D BUILD_SHARED_LIBS=ON
  -D BUILD_CLI=OFF
  -D BUILD_DOC=OFF
  -D USE_STATIC=OFF
  -D USE_FAAD2=ON
  -D USE_SYSTEM_LIBUSB=ON
  -D USE_SYSTEM_RTLSDR=ON
  -D USE_SYSTEM_FFTW=ON
  -D USE_SSE=OFF
  -D CMAKE_POSITION_INDEPENDENT_CODE=ON
)
if [[ "$ARCH" == "arm64" ]]; then
  cmake_args+=( -D CMAKE_OSX_ARCHITECTURES=arm64 )
elif [[ "$ARCH" == "x86_64" ]]; then
  cmake_args+=( -D CMAKE_OSX_ARCHITECTURES=x86_64 )
fi

cmake "${cmake_args[@]}" ..
make "-j$JOBS" nrsc5

BUILT_SO="$(find "$SRC_DIR/build" -name 'libnrsc5*.dylib' -type f -print -quit)"
if [[ -z "$BUILT_SO" || ! -f "$BUILT_SO" ]]; then
  echo "ERROR: build did not produce a libnrsc5*.dylib file" >&2
  find "$SRC_DIR/build" -name 'libnrsc5*' -print >&2 || true
  exit 1
fi

echo "==> built: $BUILT_SO"

mkdir -p "$BIN_DIR"
STAGED="$BIN_DIR/libnrsc5.dylib"
if [[ -f "$STAGED" ]]; then
  cp -f "$STAGED" "$STAGED.bak-$(date +%Y%m%d-%H%M%S)"
fi
cp -f "$BUILT_SO" "$STAGED"
chmod 0755 "$STAGED"
install_name_tool -id @rpath/libnrsc5.dylib "$STAGED" 2>/dev/null || true

echo "==> staged: $STAGED"
cp -f "$SRC_DIR/include/nrsc5.h" "$REPO_ROOT/res/nrsc5.h"
echo "==> synced upstream header -> res/nrsc5.h"

echo
echo "==> Done. Staged $STAGED"
ls -la "$STAGED"
echo "Next: cargo build --release  (links against bin/libnrsc5.dylib)"