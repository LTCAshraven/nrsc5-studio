# Fedora spec scaffold for nrsc5-studio.
#
# This file is the parallel of packaging/debian/* — a hand-authored,
# RPM-policy-aware starting point for an eventual COPR or Fedora-archive
# upload. It is **not** used by the day-to-day Linux build pipeline.
#
# For local development and GitHub-Release .rpm builds, the
# [package.metadata.generate-rpm] section of the root Cargo.toml and
# cargo-generate-rpm are the source of truth. Run:
#
#     scripts/build-linux-packages.sh
#
# to produce a .deb and .rpm via the cargo-native path.
#
# To build with this spec (advanced):
#   sudo dnf install rpm-build rpmlint cmake cargo rust pkgconf-pkg-config \
#       clang-devel SoapySDR-devel alsa-lib-devel wayland-devel \
#       libxkbcommon-devel mesa-libGL-devel gtk3-devel
#   rpmbuild -bb packaging/fedora/nrsc5-studio.spec --define "_topdir $(pwd)/build/rpm"
#
# The build artifact lands in $(pwd)/build/rpm/RPMS/x86_64/.

%global appname     nrsc5-studio
%global appid       io.github.ltcashraven.Nrsc5Studio
%global debug_package %{nil}

Name:           %{appname}
Version:        0.6.3
Release:        2%{?dist}
Summary:        HD Radio FM receiver and station explorer built on nrsc5 and SoapySDR

License:        MIT
URL:            https://github.com/LTCAshraven/nrsc5-studio
Source0:        %{url}/archive/refs/tags/v%{version}.tar.gz#/%{name}-%{version}.tar.gz

ExclusiveArch:  x86_64 aarch64

BuildRequires:  cargo
BuildRequires:  rust >= 1.74
BuildRequires:  pkgconf-pkg-config
BuildRequires:  clang-devel
BuildRequires:  SoapySDR-devel
BuildRequires:  alsa-lib-devel
BuildRequires:  wayland-devel
BuildRequires:  libxkbcommon-devel
BuildRequires:  mesa-libGL-devel
BuildRequires:  gtk3-devel
BuildRequires:  desktop-file-utils
BuildRequires:  libappstream-glib

Recommends:     SoapySDR
Recommends:     SoapyRTLSDR
Suggests:       rtl-sdr
Suggests:       pipewire-pulseaudio
Provides:       libnrsc5.so()(64bit)
Provides:       libnrsc5.so(LIBNRSC5_1.0)(64bit)

%description
NRSC5 Studio is a native desktop application for listening to HD Radio
FM broadcasts (87.5-108 MHz) with an RTL-SDR, SDRplay, or HackRF One
software-defined radio. It is built around the open-source nrsc5
demodulator and the SoapySDR device layer, and adds a polished,
persistent graphical front end on top.

Features include full HD1-HD8 subchannel selection with a SIS-aware
selector grid, a Now-Playing pane with album art and station logo, a
Station Information pane surfacing the full SIS table, a rolling
8-hour album-art collage as a squarified-treemap heat map, a 24-hour
song log with CSV export, live spectrum and waterfall scope, QPSK
constellation, MER and BER signal-quality readout, closed-loop
automatic gain control, TPEG traffic-tile decoding, and animated
weather radar overlay.

The nrsc5 HD Radio decoder ships as a bundled, self-contained
libnrsc5.so under %{_libdir}/%{appname}/; no separate helper
installation is required.

%prep
%autosetup -n %{name}-%{version}

%build
# Build the self-contained libnrsc5.so (USE_STATIC=ON) and stage it at
# bin/libnrsc5.so so the cargo link step below resolves -lnrsc5 and the
# %install step can ship it. Requires network access to clone the
# pinned upstream tag.
bash scripts/build-nrsc5-linux.sh
cargo build --release --locked

%install
install -D -m 0755 target/release/%{appname} \
    %{buildroot}%{_bindir}/%{appname}

# Bundled self-contained HD Radio decoder; the binary finds it via the
# %{_libdir}/%{appname} rpath baked in at link time (build.rs emits
# /usr/lib/nrsc5-studio).
install -D -m 0755 bin/libnrsc5.so \
    %{buildroot}%{_prefix}/lib/%{appname}/libnrsc5.so

install -D -m 0644 packaging/linux/%{appname}.desktop \
    %{buildroot}%{_datadir}/applications/%{appname}.desktop

install -D -m 0644 packaging/linux/%{appid}.metainfo.xml \
    %{buildroot}%{_metainfodir}/%{appid}.metainfo.xml

for size in 16 32 48 64 128 256; do
    install -D -m 0644 \
        packaging/linux/icons/hicolor/${size}x${size}/apps/%{appname}.png \
        %{buildroot}%{_datadir}/icons/hicolor/${size}x${size}/apps/%{appname}.png
done

install -D -m 0644 packaging/linux/%{appname}.1 \
    %{buildroot}%{_mandir}/man1/%{appname}.1

install -D -m 0644 README.md \
    %{buildroot}%{_docdir}/%{appname}/README.md
install -D -m 0644 docs/linux-install.md \
    %{buildroot}%{_docdir}/%{appname}/linux-install.md
install -D -m 0644 CHANGELOG.md \
    %{buildroot}%{_docdir}/%{appname}/CHANGELOG.md
install -D -m 0644 LICENSE \
    %{buildroot}%{_licensedir}/%{appname}/LICENSE

%check
desktop-file-validate %{buildroot}%{_datadir}/applications/%{appname}.desktop
appstream-util validate-relax --nonet \
    %{buildroot}%{_metainfodir}/%{appid}.metainfo.xml

%files
%license LICENSE
%doc README.md CHANGELOG.md docs/linux-install.md
%{_bindir}/%{appname}
%{_prefix}/lib/%{appname}/libnrsc5.so
%{_datadir}/applications/%{appname}.desktop
%{_metainfodir}/%{appid}.metainfo.xml
%{_datadir}/icons/hicolor/*/apps/%{appname}.png
%{_mandir}/man1/%{appname}.1*

%changelog
* Sun Jun 28 2026 LTCAshraven <LTCAshraven@users.noreply.github.com> - 0.6.3-1
- HERE traffic & weather data-service support alongside the existing Total
  Traffic Network (TTN) feed: HERE traffic tiles and weather images now
  populate the Traffic and Weather tabs
- New "Engineering Info - Decoder & RF Diagnostics" panel split out from
  the Station Information panel (RF/decoder health, equipment, time/leap
  second, live payload log)
- Service-mode badge (MP1/MP3/MP11, AM MA1/MA3) now read from the libnrsc5
  SYNC PSMI telemetry instead of inferred from program slots
- Per-subchannel station logo display, persisted across tunes and restarts
  via filename-encoded subchannel routing (closes #9)
- Optional high-resolution 2x map basemap (res/map2x.png) auto-detected
  when present; distributed as a separate download, not packaged
- Resolution-independent map projection + traffic/weather cache replay on
  launch
- AAS scratch directory is pruned automatically (1 h retention)

* Thu Jun 18 2026 LTCAshraven <LTCAshraven@users.noreply.github.com> - 0.6.2-2
- RPM metadata fix: explicit Provides for bundled libnrsc5.so SONAME/
    symbol version to satisfy solver dependencies without --nodeps
- Install bundled libnrsc5.so with executable mode (0755)

* Tue Jun 16 2026 LTCAshraven <LTCAshraven@users.noreply.github.com> - 0.6.2-1
- Per-program decoded audio bit rate in the Station Info panel, derived
  Rust-side from the libnrsc5 v3.2.0 HDC packet stream (stock libnrsc5
  has no bit-rate event)
- FM tuning snapped to the US 200 kHz channel raster (87.9-107.9 MHz)
- Linux: ship a self-contained bundled libnrsc5.so at
  /usr/lib/nrsc5-studio/ (loaded via RUNPATH) instead of relying on a
  system decoder; built from upstream theori-io/nrsc5 v3.2.0 by
  scripts/build-nrsc5-linux.sh
- Internal dead-code cleanup (no behavior change)

* Wed Jun 12 2026 LTCAshraven <LTCAshraven@users.noreply.github.com> - 0.6.1-1
- Collage image block list with persistent storage (content-hash keyed)
- Station logo rendering in SIS header and Now Playing tab
- Dynamic Now Playing mode switching on XHDR param (0 = cover art, 1 = logo)
- Linux right-click collage interaction fallback when context menus suppressed

* Sun May 24 2026 LTCAshraven <LTCAshraven@users.noreply.github.com> - 0.3.7-1
- Linux packaging debut. .deb and .rpm now built from the same Rust
  source tree as the Windows portable zip. No DSP/SDR behavior change
  versus 0.3.6 — packaging metadata, install scripts, AppStream
  metainfo, desktop entry, hicolor icons, manpage, and a first-launch
  missing-helper dialog.

* Sat May 23 2026 LTCAshraven <LTCAshraven@users.noreply.github.com> - 0.3.6-1
- Initial Fedora spec scaffold. Not yet uploaded to Fedora or COPR.
  End-user .rpm builds are produced by cargo-generate-rpm against
  [package.metadata.generate-rpm] in Cargo.toml.
