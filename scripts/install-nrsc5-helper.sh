#!/usr/bin/env bash
# install-nrsc5-helper.sh — build and install the upstream nrsc5 HD Radio
# demodulator from source.
#
# NRSC5 Studio spawns the standalone `nrsc5` binary as a subprocess for
# RF demodulation. That binary is AGPL-licensed and is NOT packaged in
# Debian or Ubuntu (Fedora ships it in the third-party RPM Fusion Free
# repo). This script clones the pinned upstream tag, builds it with
# cmake/make, and installs it into /usr/local/bin so it lands on PATH
# ahead of any future distro packaging.
#
# Run as a regular user — sudo is invoked for the apt/dnf step and the
# final `make install`. Re-running is safe; the existing source tree
# under $WORKDIR is removed and re-cloned to guarantee a clean build at
# the pinned tag.
#
# Override the pin or work directory with environment variables:
#   NRSC5_TAG=v3.1.0          which upstream tag to build (default: v3.1.0)
#   NRSC5_WORKDIR=$HOME/src   parent directory for the source clone
#                             (default: $HOME/.cache/nrsc5-studio-build)
#   NRSC5_JOBS=8              make -j N (default: nproc)

set -euo pipefail

NRSC5_TAG="${NRSC5_TAG:-v3.1.0}"
NRSC5_WORKDIR="${NRSC5_WORKDIR:-$HOME/.cache/nrsc5-studio-build}"
NRSC5_JOBS="${NRSC5_JOBS:-$(nproc 2>/dev/null || echo 4)}"

if [[ "${EUID}" -eq 0 ]]; then
    echo "Run this script as a normal user; it uses sudo only for the apt/dnf step"
    echo "and the final 'make install'."
    exit 1
fi

detect_pkg_mgr() {
    if command -v apt-get >/dev/null 2>&1; then
        echo "apt"
    elif command -v dnf >/dev/null 2>&1; then
        echo "dnf"
    elif command -v yum >/dev/null 2>&1; then
        echo "yum"
    elif command -v pacman >/dev/null 2>&1; then
        echo "pacman"
    else
        echo "unknown"
    fi
}

PKG_MGR="$(detect_pkg_mgr)"

echo "==> Installing build dependencies for nrsc5 (${PKG_MGR})"
case "${PKG_MGR}" in
    apt)
        sudo apt-get update
        sudo apt-get install -y \
            build-essential \
            cmake \
            git \
            libao-dev \
            libfftw3-dev \
            librtlsdr-dev \
            libusb-1.0-0-dev
        ;;
    dnf|yum)
        sudo "${PKG_MGR}" install -y \
            @development-tools \
            cmake \
            git \
            libao-devel \
            fftw-devel \
            rtl-sdr-devel \
            libusb1-devel
        ;;
    pacman)
        sudo pacman -S --needed --noconfirm \
            base-devel \
            cmake \
            git \
            libao \
            fftw \
            rtl-sdr \
            libusb
        ;;
    *)
        echo "Unknown package manager. Install these packages manually and re-run:"
        echo "  build-essential / @development-tools / base-devel"
        echo "  cmake, git, libao-dev, libfftw3-dev, librtlsdr-dev, libusb-1.0-0-dev"
        exit 1
        ;;
esac

mkdir -p "${NRSC5_WORKDIR}"
SRC_DIR="${NRSC5_WORKDIR}/nrsc5-${NRSC5_TAG}"

if [[ -d "${SRC_DIR}" ]]; then
    echo "==> Removing existing source tree at ${SRC_DIR}"
    rm -rf "${SRC_DIR}"
fi

echo "==> Cloning theori-io/nrsc5 at ${NRSC5_TAG}"
git clone --depth 1 --branch "${NRSC5_TAG}" \
    https://github.com/theori-io/nrsc5.git "${SRC_DIR}"

echo "==> Configuring (CMake)"
mkdir -p "${SRC_DIR}/build"
cd "${SRC_DIR}/build"
cmake -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX=/usr/local ..

echo "==> Building (-j ${NRSC5_JOBS})"
make "-j${NRSC5_JOBS}"

echo "==> Installing to /usr/local (sudo)"
sudo make install
sudo ldconfig

INSTALLED="$(command -v nrsc5 || true)"
if [[ -z "${INSTALLED}" ]]; then
    echo "ERROR: nrsc5 was built and installed but is not on PATH."
    echo "       Check /usr/local/bin is on your PATH and re-run."
    exit 1
fi

echo
echo "==> Done. nrsc5 installed at: ${INSTALLED}"
echo
echo "Source tree retained at ${SRC_DIR} for inspection; safe to delete."
