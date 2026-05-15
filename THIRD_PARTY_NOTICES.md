# Third-Party Notices

NRSC5 Studio is distributed with several third-party binaries that have their
own licenses. This file documents them and points to their original sources, as
required by the GNU General Public License and the GNU Lesser General Public
License under which most of these components are released.

The Rust source code of NRSC5 Studio itself is licensed separately under the
MIT License — see [LICENSE](LICENSE).

---

## Bundled runtime binaries (in `bin/`)

These files ship in the portable distribution. They are unmodified copies of
upstream releases; NRSC5 Studio invokes them as a separate process and does
not link them into its own executable.

### `nrsc5.exe`, `libnrsc5.dll`

- **Project:** NRSC5 — an HD Radio (NRSC-5) receiver
- **License:** GNU General Public License version 3.0 (GPL-3.0)
- **Source:** <https://github.com/theori-io/nrsc5>
- **Copyright:** Theori (Aiden) and contributors

### `librtlsdr.dll`

- **Project:** rtl-sdr — driver and library for RTL2832U-based DVB-T dongles
- **License:** GNU General Public License version 2.0 (GPL-2.0)
- **Source:** <https://github.com/osmocom/rtl-sdr>
- **Copyright:** Osmocom and contributors

### `libusb-1.0.dll`

- **Project:** libusb — cross-platform USB library
- **License:** GNU Lesser General Public License version 2.1 (LGPL-2.1)
- **Source:** <https://libusb.info/>
- **Copyright:** libusb contributors

### `libao-4.dll`

- **Project:** libao — cross-platform audio output library
- **License:** GNU General Public License version 2.0 (GPL-2.0)
- **Source:** <https://www.xiph.org/ao/>
- **Copyright:** Xiph.Org Foundation and contributors

### `libgcc_s_dw2-1.dll`

- **Project:** GCC runtime support library (shipped with MinGW-w64)
- **License:** GPL-3.0 with GCC Runtime Library Exception 3.1
- **Source:** <https://gcc.gnu.org/>
- **Copyright:** Free Software Foundation, Inc.

### `libunwind.dll`

- **Project:** LLVM libunwind — stack unwinder used by the llvm-mingw toolchain
- **License:** Apache-2.0 WITH LLVM-exception
- **Source:** <https://github.com/llvm/llvm-project/tree/main/libunwind>
- **Copyright:** The LLVM Project

---

## Obtaining the source code

The GPL and LGPL require that anyone who distributes binaries also make the
corresponding source code available (or provide a written offer to do so).
The source for every bundled component is publicly available at the upstream
URLs listed above. NRSC5 Studio ships these binaries unmodified.

If you cannot reach those upstream sources for any reason, please open an
issue on the [NRSC5 Studio GitHub repository](https://github.com/LTCAshraven/nrsc5-studio) and a
matching snapshot will be made available.

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
