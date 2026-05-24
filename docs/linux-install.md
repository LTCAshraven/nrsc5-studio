# Installing NRSC5 Studio on Linux

NRSC5 Studio ships as a `.deb` (Debian / Ubuntu) and a `.rpm` (Fedora)
on every GitHub Release. There is one prerequisite the package
**cannot** install for you: the upstream `nrsc5` HD Radio demodulator.

This document covers both steps.

## Why nrsc5 isn't bundled

`nrsc5` is the open-source HD Radio demodulator that NRSC5 Studio
spawns as a subprocess to do the actual RF demodulation. It lives at
[github.com/theori-io/nrsc5](https://github.com/theori-io/nrsc5).

It isn't shipped with NRSC5 Studio for two reasons:

1. **Licensing.** `nrsc5` is **AGPL-3.0-or-later**; NRSC5 Studio is
   **MIT**. Keeping the binaries separate (and communicating between
   them via pipes — "mere aggregation" under the GPL) means each
   project keeps its own license cleanly.
2. **Distro availability.** `nrsc5` is **not packaged in Debian or
   Ubuntu repositories at all**. Fedora has it in the third-party
   *RPM Fusion Free* repo. Bundling our own copy would entangle every
   user with the AGPL and add ongoing maintenance burden every time
   theori-io publishes an upstream release. The simpler answer is to
   point you at the upstream source.

## Installing nrsc5

NRSC5 Studio ships a one-shot installer that does everything for you.
After installing the `nrsc5-studio` package, run:

```bash
/usr/share/nrsc5-studio/install-nrsc5-helper.sh
```

The script:

1. Detects your package manager (`apt`, `dnf`, `pacman`).
2. Installs the build prerequisites (`cmake`, `libao-dev`,
   `libfftw3-dev`, `librtlsdr-dev`, `libusb-1.0-0-dev`).
3. Clones [`theori-io/nrsc5`](https://github.com/theori-io/nrsc5) at
   the pinned tag (currently `v3.1.0`, which matches the Windows
   build).
4. Builds with `cmake` + `make`.
5. Installs to `/usr/local/bin/nrsc5` via `sudo make install`.

Run it as a normal user. It calls `sudo` only for the package install
and the final `make install`.

You can override the pinned tag or the working directory if you want:

```bash
NRSC5_TAG=v3.2.0 NRSC5_JOBS=12 \
    /usr/share/nrsc5-studio/install-nrsc5-helper.sh
```

## Installing nrsc5 manually

If you'd rather drive the build yourself, the canonical sequence is:

```bash
# Build prerequisites (Debian / Ubuntu)
sudo apt-get install -y \
    build-essential cmake git \
    libao-dev libfftw3-dev librtlsdr-dev libusb-1.0-0-dev

# Clone and build
git clone --depth 1 --branch v3.1.0 https://github.com/theori-io/nrsc5.git
cd nrsc5
mkdir build && cd build
cmake -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX=/usr/local ..
make -j$(nproc)
sudo make install
sudo ldconfig
```

The Fedora / Arch equivalents swap the package-install line; the rest
of the steps are identical.

## Installing NRSC5 Studio

Once `nrsc5` is on `PATH`, install NRSC5 Studio itself:

### Debian / Ubuntu

```bash
sudo apt install ./nrsc5-studio_0.3.7-1_amd64.deb
```

`apt` will pull in the runtime shared-library dependencies
automatically (libSoapySDR, librtlsdr, libasound2, etc.) from your
distro's package archive.

### Fedora

```bash
sudo dnf install ./nrsc5-studio-0.3.7-1.x86_64.rpm
```

## Verifying the install

```bash
which nrsc5         # → /usr/local/bin/nrsc5
which nrsc5-studio  # → /usr/bin/nrsc5-studio
```

Launch NRSC5 Studio from your desktop environment's launcher (it
appears under **Sound & Video** / **Audio**) or from a terminal:

```bash
nrsc5-studio
```

On first launch, if `nrsc5` isn't on `PATH`, NRSC5 Studio displays a
modal dialog pointing back at this document — so you'll know
immediately if something went wrong.

## Troubleshooting

### "No SDR devices detected"

NRSC5 Studio uses SoapySDR for device discovery. Make sure the
matching driver module is installed:

```bash
# Debian / Ubuntu
sudo apt install soapysdr-tools soapysdr-module-rtlsdr

# Fedora
sudo dnf install soapysdr SoapyRTLSDR
```

Then plug in your RTL-SDR / SDRplay / HackRF and confirm SoapySDR can
see it:

```bash
SoapySDRUtil --find
```

### USB permission errors on RTL-SDR

Stock Ubuntu / Debian sometimes ships a default udev rule that hands
RTL-SDR devices to the kernel DVB-T driver instead of librtlsdr.
Blacklist the kernel driver:

```bash
echo 'blacklist dvb_usb_rtl28xxu' | \
    sudo tee /etc/modprobe.d/blacklist-rtl.conf
sudo update-initramfs -u   # Debian/Ubuntu
sudo dracut --force        # Fedora
```

Then unplug and re-plug the dongle, or reboot.

### SDRplay

SDRplay requires the proprietary SDRplay API service to be installed
separately from [sdrplay.com/downloads](https://www.sdrplay.com/downloads/).
SoapySDRPlay3 module is then auto-loaded by SoapySDR when the service
is running.

### Sync flickers / MER stuck / AGC bails (intermittent)

If you occasionally see the spectrum look fine but `nrsc5` never
reaches sync (MER reading frozen, AGC eventually gives up), it's
usually transient. Things to try, in order:

* Stop and Start the stream once. The SDRplay API service in
  particular can hand back stale state on the first open after a
  cold start.
* If you have more than one USB SDR plugged in, try unplugging the
  ones you aren't using. Multiple Soapy modules loading into the
  same process can interact in unexpected ways on Linux, and the
  kernel's `dvb_usb_rtl28xxu` driver polls any idle RTL-SDR, which
  can add USB scheduling jitter on a shared root hub.
* Move the SDR to a different USB port (preferably USB 3.0 on its
  own root hub) to rule out bus contention or power sag.

### Debugging sync / audio issues

Set `NRSC5_STUDIO_DEBUG=1` to mirror every line of `nrsc5`'s stderr
to your terminal, prefixed with `[nrsc5]`:

```bash
NRSC5_STUDIO_DEBUG=1 nrsc5-studio 2>&1 | tee /tmp/nrsc5-studio.log
```

Look for `Synchronized`, `Lost synchronization`, `MER:`, and `BER:`
lines to understand what the demodulator is seeing.
