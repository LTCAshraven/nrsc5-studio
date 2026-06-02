# Third-Party Notices

NRSC5 Studio is distributed with several third-party binaries that have their
own licenses. This file documents them and points to their corresponding
source code, as required by the GNU General Public License and the GNU
Lesser General Public License under which most of these components are
released.

## Licensing summary

- The **Rust source code** of NRSC5 Studio is licensed under the **MIT
  License** — see [LICENSE](LICENSE). MIT-licensed reuse of the source
  (including in non-GPL projects, as long as it is not linked against
  libnrsc5 or the other GPL components listed below) is welcomed.
- The **distributed binary** of NRSC5 Studio (the `nrsc5-studio.exe` in
  the Windows portable zip, the `nrsc5-studio` ELF in the Linux .deb /
  .rpm, and the matching DLLs / shared objects shipped alongside it)
  links against `libnrsc5` (GPL-3.0) at load time and is therefore a
  **combined work distributed under the terms of GPL-3.0**. A copy of
  the GPL-3.0 license text is bundled in the release as
  `COPYING.GPL-3.0`.

In plain English: feel free to copy MIT-licensed code out of this
repository into your own projects under MIT terms; the *built binary*
you download from the Releases page is GPL-3.0 because it dynamically
links a GPL library, and your obligations as a redistributor of that
binary are governed by GPL-3.0 Section 6 (see "Obtaining the source
code" below).

---

## Bundled runtime binaries

These files ship in the portable Windows zip and the Linux .deb / .rpm.
They are unmodified copies of upstream releases. NRSC5 Studio links
against `libnrsc5` directly via the platform dynamic linker
(`#[link(name = "nrsc5")]`); the remaining libraries below are loaded
transitively by `libnrsc5`, the SoapySDR plugin modules, or the C
runtime.

### `libnrsc5.dll` / `libnrsc5.so` — pinned at upstream tag `v3.1.0`

- **Project:** NRSC5 — an HD Radio (NRSC-5) receiver
- **License:** GNU General Public License version 3.0 (GPL-3.0)
- **Source:** <https://github.com/theori-io/nrsc5/tree/v3.1.0>
- **Copyright:** Theori (Aiden) and contributors
- **Statically embedded inside this DLL** (per nrsc5 v3.1.0's
  `CMakeLists.txt` `ExternalProject_Add` blocks, when the Windows
  build is run with `USE_STATIC=ON`):
  - **FFTW3** v3.3.10 — single-precision FFT library. GPL-2.0+.
    Source: <https://www.fftw.org/fftw-3.3.10.tar.gz>
    (SHA-256: `56c932549852cddcfafdab3820b0200c7742675be92179e59e6215b340e26467`).
  - **libusb** v1.0.27 — USB I/O. LGPL-2.1+.
    Source: <https://github.com/libusb/libusb/releases/tag/v1.0.27>.
  - **librtlsdr** v2.0.2 — Osmocom RTL-SDR driver (includes RTL-SDR
    Blog V4 detection support). GPL-2.0+.
    Source: <https://gitea.osmocom.org/sdr/rtl-sdr/src/tag/v2.0.2>.
  - **FAAD2** v2.11.2 (knik0 fork, with the HDC-AAC patch from
    nrsc5's tree applied). GPL-2.0.
    Source: <https://github.com/knik0/faad2/releases/tag/2.11.2>
    + the patch at
    <https://github.com/theori-io/nrsc5/blob/v3.1.0/support/faad2-hdc-support.patch>.
  - Note: libao is **not** statically embedded in `libnrsc5.dll`.
    nrsc5 only links libao into the standalone `nrsc5` CLI binary
    (`BUILD_CLI`), which this distribution does not ship.

### `librtlsdr.dll` (Windows; Linux uses the distro's `librtlsdr2`)

- **Project:** rtl-sdr — driver and library for RTL2832U-based DVB-T dongles
- **License:** GNU General Public License version 2.0 or later (GPL-2.0+)
- **Source:** <https://gitea.osmocom.org/sdr/rtl-sdr>
- **Copyright:** Osmocom and contributors
- Loaded separately from the rtl-sdr statically embedded in `libnrsc5.dll`;
  this shared copy is used by SoapySDR's RTL-SDR plugin.

### `libusb-1.0.dll` (Windows; Linux uses the distro's `libusb-1.0-0`)

- **Project:** libusb — cross-platform USB library
- **License:** GNU Lesser General Public License version 2.1 or later (LGPL-2.1+)
- **Source:** <https://github.com/libusb/libusb>
- **Copyright:** libusb contributors
- The Windows build of this shared copy was sourced from the
  MSYS2 package and identifies internally as v1.0.28; a separate
  v1.0.27 build is statically embedded in `libnrsc5.dll` (see
  above).

### `libSoapySDR.dll` + `bin/SoapySDR/modules0.8/*.dll`

- **Project:** SoapySDR — vendor-neutral SDR abstraction layer
- **License:** Boost Software License 1.0 (BSL-1.0)
- **Source:** <https://github.com/pothosware/SoapySDR>
- **Copyright:** Pothosware LLC and contributors
- Notes: bundled plugin modules include `librtlsdrSupport.dll`
  (MIT, <https://github.com/pothosware/SoapyRTLSDR>),
  `libHackRFSupport.dll` (MIT, <https://github.com/pothosware/SoapyHackRF>),
  and `libsdrPlaySupport.dll` (MIT,
  <https://github.com/pothosware/SoapySDRPlay3>). The SDRplay plugin
  requires Xperi / SDRplay's proprietary `sdrplay_api.dll` to be
  installed separately by the end user; that proprietary component is
  **not** bundled.

### `libgcc_s_seh-1.dll`, `libstdc++-6.dll`, `libwinpthread-1.dll` (Windows)

- **Project:** GCC / libstdc++ / MinGW-w64 winpthreads, shipped with the
  llvm-mingw toolchain used to build SoapySDR
- **License:** GPL-3.0 *with the GCC Runtime Library Exception 3.1*
  (libgcc, libstdc++); MIT-style (winpthreads). The GCC Runtime Library
  Exception explicitly permits distributing programs that link these
  libraries under any license, so they impose no additional obligation
  on this distribution.
- **Source (GCC components):** <https://gcc.gnu.org/>
- **Source (llvm-mingw bundle):** <https://github.com/mstorsjo/llvm-mingw/releases/tag/20260505>
- **Copyright:** Free Software Foundation, Inc.; MinGW-w64 contributors

### `libunwind.dll` (Windows)

- **Project:** LLVM libunwind — stack unwinder used by the llvm-mingw toolchain
- **License:** Apache-2.0 WITH LLVM-exception
- **Source:** <https://github.com/llvm/llvm-project/tree/main/libunwind>
- **Copyright:** The LLVM Project

---

## Obtaining the source code

GPL-3.0 Section 6 requires anyone who distributes a binary form of the
combined work to also make the **corresponding source code** available
— i.e. the exact source that was used to build the distributed binary.

This release satisfies that obligation via GPL-3.0 Section 6(d): the
binary is conveyed from a designated network location (the project's
GitHub Releases page), and the corresponding source for every component
is available at no further charge from the URLs listed below, for as
long as the binary itself is hosted there.

| Component | Version | Corresponding source |
|---|---|---|
| NRSC5 Studio (this project) | matches the release tag | <https://github.com/LTCAshraven/nrsc5-studio> at tag `v<version>` |
| libnrsc5 | `v3.1.0` | <https://github.com/theori-io/nrsc5/releases/tag/v3.1.0> |
| FFTW3 (embedded in libnrsc5) | `3.3.10` | <https://www.fftw.org/fftw-3.3.10.tar.gz> |
| libusb (embedded in libnrsc5) | `v1.0.27` | <https://github.com/libusb/libusb/releases/tag/v1.0.27> |
| librtlsdr (embedded in libnrsc5) | `v2.0.2` | <https://gitea.osmocom.org/sdr/rtl-sdr/src/tag/v2.0.2> |
| FAAD2 (embedded in libnrsc5) | knik0 `2.11.2` + nrsc5 HDC patch | <https://github.com/knik0/faad2/releases/tag/2.11.2> |
| librtlsdr (standalone bin/librtlsdr.dll) | MSYS2 build (current Osmocom upstream) | <https://gitea.osmocom.org/sdr/rtl-sdr> |
| libusb (standalone bin/libusb-1.0.dll) | `v1.0.28` | <https://github.com/libusb/libusb/releases/tag/v1.0.28> |
| SoapySDR + plugin modules | as bundled in the upstream MSYS2 package at build time | <https://github.com/pothosware/SoapySDR/releases>, <https://github.com/pothosware/SoapySDRPlay3>, <https://github.com/pothosware/SoapyRTLSDR>, <https://github.com/pothosware/SoapyHackRF> |
| GCC runtime libraries (libgcc, libstdc++, libwinpthread) | as bundled in llvm-mingw 20260505 | <https://github.com/mstorsjo/llvm-mingw/releases/tag/20260505> |
| LLVM libunwind | as bundled in llvm-mingw 20260505 | <https://github.com/mstorsjo/llvm-mingw/releases/tag/20260505> |

NRSC5 Studio ships these third-party binaries unmodified. The Windows
build script for `libnrsc5.dll` is in this repository at
[scripts/build-nrsc5-msys2.ps1](scripts/build-nrsc5-msys2.ps1) and pins
the upstream tag explicitly; that script, plus the upstream
`v3.1.0` tag it references, together constitute the corresponding
source for the bundled `libnrsc5.dll`.

If any upstream source URL becomes unreachable for any reason, please
open an issue on the [NRSC5 Studio GitHub repository](https://github.com/LTCAshraven/nrsc5-studio)
and a matching snapshot will be provided.

---

## Rust dependencies

NRSC5 Studio's executable statically links a number of MIT- or Apache-2.0-
licensed Rust crates at build time. None of these introduce additional
redistribution obligations beyond preserving their copyright notices, which
this section is intended to satisfy.

Notable runtime dependencies (full list in [Cargo.lock](Cargo.lock)):

| Crate | License | Project |
|---|---|---|
| `eframe`, `egui`, `egui_extras` | MIT OR Apache-2.0 | <https://github.com/emilk/egui> |
| `egui_dock` | MIT | <https://github.com/Adanos020/egui_dock> |
| `image` | MIT OR Apache-2.0 | <https://github.com/image-rs/image> |
| `chrono` | MIT OR Apache-2.0 | <https://github.com/chronotope/chrono> |
| `serde`, `serde_*` | MIT OR Apache-2.0 | <https://serde.rs/> |
| `toml`, `ron` | MIT OR Apache-2.0 | various |
| `windows` | MIT OR Apache-2.0 | <https://github.com/microsoft/windows-rs> |
| `crossbeam-channel` | MIT OR Apache-2.0 | <https://github.com/crossbeam-rs/crossbeam> |
| `cpal` | Apache-2.0 | <https://github.com/RustAudio/cpal> |
| `anyhow`, `thiserror` | MIT OR Apache-2.0 | <https://github.com/dtolnay> |

A complete dependency tree with exact versions is available via `cargo tree`.
