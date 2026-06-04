# NRSC5 Studio 0.5.0

The **in-process libnrsc5** release. NRSC5 Studio no longer ships and
shells out to a separate `nrsc5.exe` child process — it links directly
against `libnrsc5.dll` at load time and decodes inside its own
address space. With the decoder living in-process, the same plumbing
now drives **multiple HD subchannels in parallel** (up to four of
HD1–HD4) from a single tune.

Beyond the cutover: a Linux `rtl_tcp` GUI-freeze bug is fixed, the
weather map is finally bundled correctly into the `.deb` / `.rpm`
packages, and the project's licensing posture is brought into full
compliance with GPL-3.0 §6 now that the shipped binary is a combined
work.

If you just want to use the new bits: hit Start, then look at the
Tuner panel for the new per-program enable checkboxes — HD2/HD3/HD4
can be brought up alongside HD1 without retuning. The radio buttons
control which subchannel feeds the audio output.

## What's new

### libnrsc5 in-process

`nrsc5.exe` is gone from `bin/`. So is the Linux `nrsc5` binary.
In their place is `libnrsc5.dll` (Windows) / `libnrsc5.so` (Linux),
dynamically linked into NRSC5 Studio via a hand-curated FFI
layer. The startup cost of spawning a child process and
piping s16le PCM through a stdout pipe is gone. So is every class
of bug that came with the previous arrangement — orphan processes,
broken pipes on shutdown, the stderr-parser falling out of sync
with upstream's log format, the `--write-iq -` tee that doubled
the I/O budget. The decoder runs as a thread inside the app now.

The FFI work is split across three files under `src/ffi/`:

- `nrsc5_sys.rs` — raw bindings against the upstream
  [`nrsc5.h`](https://github.com/theori-io/nrsc5/blob/v3.1.0/include/nrsc5.h)
  pinned at v3.1.0. The build script keeps the bundled header in
  sync with the upstream tag automatically.
- `api.rs` — safe Rust wrapper. Owns the `Nrsc5Session` handle,
  installs the callback trampoline, copies every C string and
  byte slice into owned `NrscEvent` variants so consumers never
  see a borrowed C pointer, and exposes a typed `PcmSink` callback
  for the audio fast path.
- `decoder.rs` — the per-program `DecoderInstance` glue (one per
  active subchannel), each owning its own `Nrsc5Session` and a
  dedicated I/Q feeder thread that pumps samples from the shared
  `IqBus` into `pipe_samples_cu8`.

100 % of the project's `unsafe` lives inside `api.rs`. Everything
outside `src/ffi/` is safe Rust.
#### Why we made this change

The previous arrangement worked, but it forced the audio pipeline to
cross a process boundary 22 times per second. Every PCM buffer
nrsc5 produced had to be `write()`'d into a kernel pipe, followed by
a context switch back into NRSC5 Studio and a `read()` out of the
pipe \u2014 syscalls and scheduler round-trips for roughly 176 KB/s of
audio data, plus a separate stderr stream we had to regex-parse for
every metadata event. With the decoder in-process, audio buffers
arrive as a direct function call (`&[i16]` straight into the
`PcmRing`), and metadata events arrive as typed structs the moment
libnrsc5 produces them.

The user-visible consequences:

- **Less audio jitter and lower latency.** No kernel pipe between
  the decoder and `cpal`. The OS scheduler isn't in the audio path
  anymore.
- **Faster metadata updates.** Now-playing, station info, and
  album-art events appear sub-millisecond after libnrsc5 emits them,
  instead of waiting for stdout buffering to flush whichever line
  the parser was looking for.
- **Faster Start.** No process-spawn + pipe-handshake cost on every
  tune (was 100\u2013300 ms; now milliseconds).
- **Multi-program decode becomes possible.** You can't sanely fan a
  single I/Q stream into four parallel child processes over four
  stdin pipes. With everything in-process, the shared `IqBus` hands
  every active `DecoderInstance` a borrow of the same buffer.
- **Fewer failure modes.** Orphaned `nrsc5.exe` processes after a
  crash, broken pipes on shutdown, and the stderr-parser falling
  out of sync with upstream's log format are all gone.

Decode quality and per-program CPU cost are unchanged \u2014 it's the
same C decoder doing the same DSP. The win is entirely on the data
path and lifecycle around it.
### Multi-program HD decode (HD1–HD4 in parallel)

A single SDR tune can now feed up to four HD subchannels at the
same time. The shared `IqBus` fans I/Q samples out to one
`DecoderInstance` per enabled program; a central AGC tee feeds
every decoder from the same gain trajectory; the per-program
`play_log` dedups song metadata across subchannels so the rolling
24-hour song log doesn't double-count crossovers. There's a soft
cap of 4 active decoders (out of nrsc5's 8 max) to keep CPU
reasonable on lower-end machines.

The Tuner panel has new per-program enable toggles. Bringing HD3
up alongside HD1 is one click — no retune required.

### Linux `rtl_tcp` freeze fix

Selecting the `rtl_tcp` transport on Linux with a wrong host or
port used to lock up the GUI for as long as it took the OS to
abandon the connect (about 22 seconds on glibc, after which the
desktop's Force-Quit dialog would appear). The fix bounds three
separate timeouts:

- `to_socket_addrs()` resolution happens before the connect, so
  bad hostnames fail in milliseconds.
- `TcpStream::connect_timeout(addr, 3s)` for the actual TCP open.
- A 2 s `set_read_timeout` is installed **before** the
  `read_exact` of the 12-byte dongle-info header, so a server that
  accepts the connection but never sends data times out in 2 s
  instead of forever.
- The same socket switches to a 5 s read timeout once streaming
  starts.

Worst case on a misconfigured remote: ~5 seconds of GUI block
before Start fails with a clean error. A longer-term refactor to
move the entire Start path off the GUI thread is on the roadmap.

### Weather map shipped with `.deb` / `.rpm`

The fallback CONUS base map (`res/map.png`, drawn under the
weather overlay when no live tile is available) wasn't being
installed by the Linux packages. It now lands at
`/usr/share/nrsc5-studio/res/map.png`, and the maps module checks
both the FHS install path and the portable `res/` directory.

### Licensing: combined work

Linking against `libnrsc5.dll` makes the shipped binary a
**combined work under GPL-3.0**. The project source remains MIT —
nothing about the licence of NRSC5 Studio's own Rust code
changes — but the binary you download from the release page
falls under GPL-3.0. The full GPL-3.0 text now ships alongside
the binary (in the portable zip, and at
`/usr/share/doc/nrsc5-studio/COPYING.GPL-3.0` on Linux), and
`THIRD_PARTY_NOTICES.md` has been rewritten to spell out the
situation honestly, including the libraries statically embedded
inside `libnrsc5.dll` that the previous notice file didn't list
(FFTW3, FAAD2, libusb 1.0.27, rtl-sdr 2.0.2 Osmocom).

The complete corresponding source for the release is attached to
this GitHub release as ten upstream tarballs under
`corresponding-source/` (libnrsc5 v3.1.0, FFTW 3.3.10, FAAD2
2.11.2, libusb v1.0.27 + v1.0.28, rtl-sdr v2.0.2, SoapySDR 0.8.1,
SoapyRTLSDR, SoapyHackRF, SoapySDRPlay3). The matching NRSC5
Studio source is this repository at the `v0.5.0` tag.

`scripts/fetch-corresponding-source.ps1` regenerates the tarball
set reproducibly.

### Smaller things

- **LOT filename prefix** is now written exactly once (was being
  doubled in some paths, breaking the cover-art / weather-map /
  traffic-map processors' filename match).
- **MER no longer "sticks" across retunes** — the Signal panel
  reads zero while the controller is searching instead of
  retaining the previous station's value.
- **Enter key tunes** from inside the Tuner panel's frequency
  text input.
- **`scripts/cargo-gnu.ps1`** prepends MSYS2 `mingw64\bin` to
  PATH (instead of appending) so the correct `pkg-config` and
  `libclang` are picked up, and tolerates native stderr output
  from the toolchain (was being misclassified as build failure).

## Upgrading

- Drop in the new Windows zip or install the new `.deb`/`.rpm`.
  Settings carry over from any 0.4.x install.
- `bin/nrsc5.exe` is no longer in the package; if you have a
  custom `[nrsc5]` config block, it's ignored (the in-process
  decoder reads its config from the Rust side).
- The startup PATH/DLL search routine looks for `libnrsc5.dll`
  in the same `bin/` directory it always loaded `libSoapySDR.dll`
  from. No new configuration required.

## Licensing change at a glance

- **Source:** MIT, unchanged. See `LICENSE`.
- **Binary:** GPL-3.0 (combined work via `libnrsc5.dll`). See
  `COPYING.GPL-3.0` and `THIRD_PARTY_NOTICES.md`.
- **Corresponding source for the binary:** attached to this
  release as the `corresponding-source/` tarball set, plus this
  repository at the `v0.5.0` tag.

## What's still on the roadmap

- Move `Nrsc5Process::start_piped` off the GUI thread so transport
  errors (rtl_tcp, SoapyRemote) surface as a banner instead of as
  a multi-second GUI freeze even with the bounded timeouts.
- GitHub Actions release workflow (tag → builds → draft release
  with corresponding-source assets auto-attached).
- Animated heat-map / smoothly animated live collage.
- Android port (Kotlin + Jetpack Compose over a Rust core).
