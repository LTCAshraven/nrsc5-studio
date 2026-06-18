#!/usr/bin/env bash
#
# Build libnrsc5.so (the Linux counterpart of bin/libnrsc5.dll) from the
# pinned upstream nrsc5 tag, with all native dependencies statically
# embedded, and stage it at bin/libnrsc5.so for both the Rust link step
# and the .deb / .rpm packaging.
#
# This mirrors scripts/build-nrsc5-msys2.ps1 (the Windows DLL build):
#   USE_STATIC=ON          -> libusb / librtlsdr / FFTW / FAAD2 are
#                             built from the bundled submodules and
#                             linked *into* libnrsc5.so, so the only
#                             runtime deps the .so pulls from the distro
#                             are glibc + libm + libstdc++ (resolved by
#                             $auto in the package metadata).
#   USE_SYSTEM_*=OFF       -> don't pick up distro copies of those deps.
#   BUILD_CLI=OFF          -> we only need libnrsc5.so, not the nrsc5
#                             executable (whose built-in libao path is
#                             Windows-only anyway).
#
# Output: bin/libnrsc5.so  (a single, self-contained shared object whose
# SONAME is normalised to "libnrsc5.so" so the app links against, and
# ships, exactly one file). The matching upstream header is synced to
# res/nrsc5.h, identical to the Windows build.
#
# Usage from the repo root (Ubuntu 22.04+ / Debian 12+ host):
#
#   bash scripts/build-nrsc5-linux.sh
#
# Optional environment overrides:
#   NRSC5_TAG=v3.2.0   # which upstream tag to build
#   NRSC5_JOBS=8       # make -j N (default: nproc)

set -euo pipefail

TAG="${NRSC5_TAG:-v3.2.0}"
JOBS="${NRSC5_JOBS:-$(nproc)}"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
BIN_DIR="$REPO_ROOT/bin"
BUILD_ROOT="${TMPDIR:-/tmp}/nrsc5-linux-build"
SRC_DIR="$BUILD_ROOT/nrsc5-$TAG"

echo "==> Building libnrsc5.so from nrsc5 $TAG (jobs=$JOBS)"

# --- 0. build prerequisites -------------------------------------------
# The bringup script (scripts/linux-ubuntu-bringup.sh) installs the bulk
# of these; this is a defensive check so a bare host gets a clear error
# rather than a confusing CMake failure deep in the build.
missing=()
for tool in git cmake make cc patchelf; do
  command -v "$tool" >/dev/null 2>&1 || missing+=("$tool")
done
if [[ ${#missing[@]} -gt 0 ]]; then
  echo "ERROR: missing build tools: ${missing[*]}" >&2
  echo "Install them first, e.g.:" >&2
  echo "  sudo apt-get install -y git cmake make build-essential patchelf autoconf automake libtool" >&2
  exit 1
fi

# --- 1. clone the pinned tag ------------------------------------------
rm -rf "$SRC_DIR"
mkdir -p "$BUILD_ROOT"
git clone --depth 1 --branch "$TAG" https://github.com/theori-io/nrsc5.git "$SRC_DIR"
cd "$SRC_DIR"
echo "==> ref: $(git log -1 --format='%H %s')"

# --- 1a. patch an upstream Linux static-build quirk --------------------
# nrsc5's CMakeLists hardcodes the *Windows* static-library filename for
# rtl-sdr (librtlsdr_static.a) in its IMPORTED_LOCATION. The osmocom
# rtl-sdr v2.0.2 build installs the static archive as librtlsdr.a on
# Linux (the "_static" suffix only exists on Windows, to avoid clashing
# with the import lib). Without this fix the final libnrsc5.so link
# fails with: No rule to make target 'rtlsdr-prefix/lib/librtlsdr_static.a'.
if grep -q 'librtlsdr_static\.a' CMakeLists.txt; then
  sed -i 's#/lib/librtlsdr_static\.a#/lib/librtlsdr.a#' CMakeLists.txt
  echo "==> patched CMakeLists: librtlsdr_static.a -> librtlsdr.a (Linux)"
fi

# nrsc5's USE_STATIC mode appends a bare "-static" to the shared
# libnrsc5.so link (src/CMakeLists.txt). That is meaningful on MinGW
# (the Windows build) but on GNU/Linux it forces the non-PIC static C
# runtime (crtbeginT.o) into a -shared link and ld aborts with
#   relocation R_X86_64_32 against hidden symbol `__TMC_END__` ...
# We still want the dependencies linked *statically* (USE_STATIC keeps
# that); we just must not pass -static to the shared-object link. Blank
# STATIC_LINKER_FLAGS so libnrsc5.so links its static .a deps but uses
# the normal PIC C runtime. (The CLI app target is not built here:
# BUILD_CLI=OFF.)
if grep -q 'set (STATIC_LINKER_FLAGS -static)' src/CMakeLists.txt; then
  sed -i 's/set (STATIC_LINKER_FLAGS -static)/set (STATIC_LINKER_FLAGS )/' src/CMakeLists.txt
  echo "==> patched src/CMakeLists: dropped -static from shared link"
fi

# The bundled static archives must be position-independent to live
# inside a shared object. nrsc5 already builds FAAD2 with -fPIC, but the
# FFTW (autotools), libusb (autotools) and rtl-sdr (cmake) sub-builds
# are not PIC by default, so ld fails with
#   relocation R_X86_64_TPOFF32 ... recompile with -fPIC
# Inject PIC into each external project's own configure/cmake step.
sed -i '/fftw_external\/configure/  s/$/ --with-pic/'  CMakeLists.txt
# libusb: PIC + disable the udev backend. Static libusb otherwise pulls
# in udev_* symbols that would have to be satisfied by a NEEDED
# libudev.so.1 at the final libnrsc5.so link; --disable-udev makes
# libusb enumerate via sysfs instead, keeping the .so self-contained
# (NEEDED stays libc/libm only). nrsc5 opens the RTL-SDR by index, so
# udev hotplug events are not required.
sed -i '/libusb_external\/configure/ s/$/ --with-pic --disable-udev/' CMakeLists.txt
sed -i '/-std=gnu17/a\            -DCMAKE_POSITION_INDEPENDENT_CODE:BOOL=ON' CMakeLists.txt
echo "==> patched CMakeLists: forced -fPIC into fftw/libusb/rtlsdr sub-builds (libusb --disable-udev)"

# --- 2. configure + build ---------------------------------------------
# CMAKE_POSITION_INDEPENDENT_CODE=ON: the statically-embedded deps must
# be -fPIC so they can live inside a shared object.
mkdir -p build
cd build
cmake \
  -D CMAKE_BUILD_TYPE=Release \
  -D BUILD_SHARED_LIBS=ON \
  -D BUILD_CLI=OFF \
  -D BUILD_DOC=OFF \
  -D USE_STATIC=ON \
  -D USE_FAAD2=ON \
  -D USE_SYSTEM_LIBUSB=OFF \
  -D USE_SYSTEM_RTLSDR=OFF \
  -D USE_SYSTEM_FFTW=OFF \
  -D USE_SSE=ON \
  -D CMAKE_POSITION_INDEPENDENT_CODE=ON \
  ..
# Build only the `nrsc5` target (libnrsc5.so). nrsc5 also defines a
# `nrsc5_static` target (libnrsc5_static.so) we don't ship; skipping it
# avoids building a second copy. The external dependency projects are
# dependencies of `nrsc5`, so they still build first.
make "-j$JOBS" nrsc5

# --- 3. locate the produced shared object -----------------------------
# Upstream may emit libnrsc5.so, or a versioned libnrsc5.so.<N> with an
# unversioned dev symlink. Resolve to the *real* file (follow symlinks)
# so we stage one concrete object.
BUILT_SO="$(find "$SRC_DIR/build" -name 'libnrsc5.so*' -type f -print -quit)"
if [[ -z "$BUILT_SO" || ! -f "$BUILT_SO" ]]; then
  echo "ERROR: build did not produce a libnrsc5.so* file" >&2
  find "$SRC_DIR/build" -name 'libnrsc5*' -print >&2 || true
  exit 1
fi
echo "==> built: $BUILT_SO"
echo "    soname: $(patchelf --print-soname "$BUILT_SO" 2>/dev/null || echo '(none)')"

# --- 4. stage as bin/libnrsc5.so with a normalised SONAME -------------
# Normalising the SONAME to the bare "libnrsc5.so" means the Rust binary
# records DT_NEEDED=libnrsc5.so and we ship exactly one file (no version
# symlink chain to package).
mkdir -p "$BIN_DIR"
STAGED="$BIN_DIR/libnrsc5.so"
if [[ -f "$STAGED" ]]; then
  cp -f "$STAGED" "$STAGED.bak-$(date +%Y%m%d-%H%M%S)"
fi
cp -f "$BUILT_SO" "$STAGED"
chmod 0755 "$STAGED"
patchelf --set-soname libnrsc5.so "$STAGED"
# Strip debug info / unneeded symbols from the shipped artifact (the
# build leaves ~4.7 MB with debug_info; stripping drops it to ~1-2 MB).
# Keep the exported dynamic API (.dynsym) intact.
strip --strip-unneeded "$STAGED" 2>/dev/null || true
echo "==> staged: $STAGED (soname=$(patchelf --print-soname "$STAGED"))"

# --- 5. sync the upstream header --------------------------------------
# Keep res/nrsc5.h locked to the same tag we just built, identical to the
# Windows build script, so cargo check + bindgen see the matching ABI.
cp -f "$SRC_DIR/include/nrsc5.h" "$REPO_ROOT/res/nrsc5.h"
echo "==> synced upstream header -> res/nrsc5.h"

# --- 6. verify exported symbols ---------------------------------------
# Fail loudly here if upstream renamed/removed a symbol the Rust FFI
# wrapper links against, instead of breaking the cargo link step later
# with a cryptic undefined-reference error.
expected=(
  nrsc5_open_pipe
  nrsc5_set_callback
  nrsc5_pipe_samples_cu8
  nrsc5_pipe_samples_cs16
  nrsc5_start
  nrsc5_stop
  nrsc5_close
  nrsc5_set_mode
  nrsc5_set_frequency
  nrsc5_get_version
)
echo "==> verifying exported symbols"
# Dynamic symbol table; -D = dynamic, --defined-only filters out the
# (empty) UND imports so a grep for a NEEDED symbol can't false-positive.
# nrsc5 attaches an ELF symbol version (libnrsc5.map -> name@@LIBNRSC5_1.0);
# strip the @@VERSION suffix so the bare API name matches.
exports="$(nm -D --defined-only "$STAGED" 2>/dev/null | awk '{n=$NF; sub(/@@.*/,"",n); print n}')"
missing_syms=()
for sym in "${expected[@]}"; do
  grep -qx "$sym" <<<"$exports" || missing_syms+=("$sym")
done
if [[ ${#missing_syms[@]} -gt 0 ]]; then
  echo "ERROR: libnrsc5.so is missing ${#missing_syms[@]} expected export(s):" >&2
  printf '  - %s\n' "${missing_syms[@]}" >&2
  echo "Upstream ABI change?" >&2
  exit 1
fi
echo "    all ${#expected[@]} expected symbols present"

# --- 7. report runtime dependencies -----------------------------------
# Informational: confirms USE_STATIC actually embedded the SDR deps and
# only system libraries remain (resolved by the package's $auto depends).
echo "==> libnrsc5.so external NEEDED entries:"
patchelf --print-needed "$STAGED" | sed 's/^/    /'

echo
echo "==> Done. Staged $STAGED"
ls -la "$STAGED"
echo "Next: cargo build --release  (links against bin/libnrsc5.so)"
