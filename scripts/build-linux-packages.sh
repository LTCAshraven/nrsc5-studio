#!/usr/bin/env bash
# build-linux-packages.sh — produce a .deb and a .rpm for nrsc5-studio
# from the cargo-deb / cargo-generate-rpm metadata in Cargo.toml.
#
# Designed to run on any modern Linux host with cargo + rustc installed.
# Re-running is safe; rebuilds idempotently.
#
# Outputs:
#   target/debian/nrsc5-studio_<version>-1_<arch>.deb
#   target/generate-rpm/nrsc5-studio-<version>-1.<arch>.rpm

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "${REPO_ROOT}"

# -----------------------------------------------------------------------------
# Toolchain check
# -----------------------------------------------------------------------------

if ! command -v cargo >/dev/null 2>&1; then
    echo "ERROR: cargo not found. Install rustup from https://rustup.rs."
    exit 1
fi

need_install_cargo_subcommand() {
    local subcmd="$1"
    cargo --list 2>/dev/null | awk '{print $1}' | grep -qx "${subcmd}"
}

if ! need_install_cargo_subcommand deb; then
    echo "==> Installing cargo-deb"
    cargo install cargo-deb --locked
fi
if ! need_install_cargo_subcommand generate-rpm; then
    echo "==> Installing cargo-generate-rpm"
    cargo install cargo-generate-rpm --locked
fi

# -----------------------------------------------------------------------------
# Refresh the Linux icon set so the package's assets reflect the current
# icon_render.rs. Re-rendering is cheap (sub-second) and deterministic.
# -----------------------------------------------------------------------------

echo "==> Rendering Linux icon PNGs"
cargo run --quiet --example render_linux_icons

# -----------------------------------------------------------------------------
# Build the release binary once. Both cargo-deb and cargo-generate-rpm
# pick the artifact up from target/release/.
# -----------------------------------------------------------------------------

echo "==> cargo build --release"
cargo build --release --locked

# -----------------------------------------------------------------------------
# .deb
# -----------------------------------------------------------------------------

echo "==> cargo deb --no-build"
cargo deb --no-build

# -----------------------------------------------------------------------------
# .rpm
# -----------------------------------------------------------------------------

echo "==> cargo generate-rpm"
cargo generate-rpm

# -----------------------------------------------------------------------------
# Summary
# -----------------------------------------------------------------------------

echo
echo "Artifacts produced:"
find target/debian -maxdepth 1 -name '*.deb' -printf '  %p\n' || true
find target/generate-rpm -maxdepth 1 -name '*.rpm' -printf '  %p\n' || true

# -----------------------------------------------------------------------------
# Optional validators (run if installed; non-fatal)
# -----------------------------------------------------------------------------

DEB_FILE="$(find target/debian -maxdepth 1 -name '*.deb' | head -n1 || true)"
RPM_FILE="$(find target/generate-rpm -maxdepth 1 -name '*.rpm' | head -n1 || true)"

if [[ -n "${DEB_FILE}" ]] && command -v lintian >/dev/null 2>&1; then
    echo
    echo "==> lintian ${DEB_FILE}"
    lintian --no-tag-display-limit --pedantic --info "${DEB_FILE}" || true
fi

if [[ -n "${RPM_FILE}" ]] && command -v rpmlint >/dev/null 2>&1; then
    echo
    echo "==> rpmlint ${RPM_FILE}"
    rpmlint "${RPM_FILE}" || true
fi

echo
echo "Done."
