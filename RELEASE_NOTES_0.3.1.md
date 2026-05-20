## NRSC5 Studio 0.3.1

Follow-up to 0.3.0 that actually makes **SDRplay** work end-to-end for
HD Radio. The 0.3.0 multi-SDR release enumerated and tuned SDRplay
devices but the HD Radio demodulator never synced because SDRplay's
hardware can't produce nrsc5's required 1.488375 Msps sample rate.
This release adds the missing software resampler, cleans up the
SDRplay gain UI, and shakes out the SDRplay AGC interactions that
made closed-loop gain unstable.

RTL-SDR behavior is unchanged.

### New: fractional IQ resampler

SDRplay's MSi001 chain quantizes to {62.5, 96, 125, 192, 250, 384,
500, 768, 1000} ksps discretely and then a continuous range from
2 Msps up — none of which match nrsc5's required 1,488,375 sps.
v0.3.1 adds a 128-tap polyphase sinc resampler (`rubato` 0.16,
Blackman-Harris window) that asks SDRplay for 2 Msps and converts
down to 1.488375 Msps in software. CPU cost is negligible at HD
Radio's bandwidth and stopband attenuation sits well below the
receiver noise floor. Activated automatically when the active
device's minimum sample rate is above 1.488375 Msps.

### New: single "Gain" slider for SDRplay

SoapySDRPlay3 exposes IFGR (IF Gain Reduction, 20..59 dB, *inverted*)
and RFGR (RF Gain Reduction, 0..9, *inverted*). v0.3.0 surfaced both
directly, which was confusing — sliders looked maxed when actually at
minimum gain. v0.3.1 pins the LNA at its most sensitive state
(`rfgain_sel=0`) and collapses the two reduction knobs into one
**Gain (dB) 0..48** slider mapped to libSoapySDRPlay's aggregate-gain
API. Higher dB = more gain, the way every other SDR's slider works.

### SDRplay HD Radio: now actually works

Combined with the resampler and the LNA/notch defaults already in
0.3.0, **SDRplay RSP1A / RSP1B / RSPduo / RSPdx now decode FM HD
Radio end-to-end** without user-side workarounds. Bench-confirmed on
an RSP1A at 101.1 MHz, MER 14+ dB in Manual mode, lock in <2 seconds
in Auto mode.

### Closed-loop AGC fixes for SDRplay

Three issues surfaced during 0.3.1 bench testing:

- **Driver-key case normalization.** Soapy 0.8's `Device::driver_key()`
  returns mixed-case (`"SDRplay"`, `"RTLSDR"`) but every internal
  lookup keyed on the lowercase form. SDRplay sessions silently fell
  back to the RTL-SDR profile so none of the bandwidth / notch /
  AGC-element overrides took effect. Fixed by lowercasing at the
  open boundary.
- **Force HW AGC off.** SoapySDRPlay3's internal hardware AGC was
  left enabled in Auto gain mode and overrode every `setGain` from
  the closed-loop driver thread, leading to USB-stream churn and
  occasional `lost-device` events. `configure` now unconditionally
  disables HW AGC for SDRplay.
- **Per-profile AGC start gain.** The closed-loop AGC's global
  default (19.7 dB) is fine on RTL-SDR's 0..49 dB table but landed
  at the bottom of SDRplay's 20..48 dB table and forced a long climb
  before MER came up. New `default_agc_initial_tenths` per profile:
  19.7 dB on RTL-SDR (unchanged), 38 dB on SDRplay, 24 dB on HackRF.
- **AGC tick rate** on SDRplay is now 500 ms (was 250 ms). The
  SoapySDRPlay3 `setGain` call is more disruptive to the USB stream
  than RTL-SDR's tuner-gain write; 500 ms ticks let each change
  settle without occasionally tripping a stream read error.

### Migration

No config changes required. Existing v0.3.0 `[sdr]` blocks with
`driver = "sdrplay"` will Just Work. If you had manual entries for
`gains.IFGR` or `gains.RFGR` in your config they'll be silently
ignored — the new collapsed model reads / writes `gains.Gain`
instead. Restoring the default (delete the `gains` block under
`[sdr]`) is the simplest path.

### Install

Download `nrsc5-studio-0.3.1-windows-x64.zip` below, extract anywhere,
run `bin\nrsc5-studio.exe`. SDRplay users additionally need the free
SDRplay API service from sdrplay.com (it can't be bundled per the
Xperi / SDRplay licensing).
