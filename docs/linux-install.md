# Installing NRSC5 Studio on Linux

NRSC5 Studio ships as a `.deb` (Debian / Ubuntu) and a `.rpm` (Fedora)
on every GitHub Release. The `nrsc5` HD Radio decoder is bundled with
the package as `libnrsc5.so` (installed to `/usr/lib/nrsc5-studio/`),
so no separate decoder install is required — just install the package
and you're ready to tune.

> **Historical note:** Releases before v0.5.0 shipped `nrsc5` as a
> separate subprocess and required a one-shot
> `/usr/share/nrsc5-studio/install-nrsc5-helper.sh`. v0.5.0 moved the
> decoder in-process via `libnrsc5`, and v0.6.0 finished cleaning the
> packaging cruft. If you have an old install with the helper script
> on disk, it's safe to delete.

## Installing NRSC5 Studio

### Debian / Ubuntu

```bash
sudo apt install ./nrsc5-studio_0.6.0-1_amd64.deb
```

`apt` will pull in the runtime shared-library dependencies
automatically (libSoapySDR, librtlsdr, libasound2, etc.) from your
distro's package archive.

### Fedora

```bash
sudo dnf install ./nrsc5-studio-0.6.0-1.x86_64.rpm
```

## Verifying the install

```bash
which nrsc5-studio  # → /usr/bin/nrsc5-studio
```

Launch NRSC5 Studio from your desktop environment's launcher (it
appears under **Sound & Video** / **Audio**) or from a terminal:

```bash
nrsc5-studio
```

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

Run NRSC5 Studio from a terminal to see the decoder's stderr output:

```bash
nrsc5-studio 2>&1 | tee /tmp/nrsc5-studio.log
```

Look for `Synchronized`, `Lost synchronization`, `MER:`, and `BER:`
lines to understand what the demodulator is seeing. The AGC
controller also writes a detailed trace log to
`$XDG_DATA_HOME/nrsc5-studio/agc-trace.log` (or
`~/.local/share/nrsc5-studio/agc-trace.log`) — every gain change and
search-phase transition is recorded there.
