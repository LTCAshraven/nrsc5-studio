## NRSC5 Studio 0.2.1

A focused follow-up to 0.2.0: closed-loop automatic gain control for the
piped-SDR backend, plus user-selectable gain modes surfaced in the
Signal panel. No architectural changes — 0.2.0's I/Q ownership story
is unchanged.

### New: closed-loop AGC

The piped-SDR backend (introduced in 0.2.0) now runs its own AGC
controller in the host app, separately from anything `nrsc5.exe`
does internally. It picks a tuner gain, watches the per-sideband MER
that nrsc5 reports back, and walks the R820T2 gain table toward the
setting that gives the highest steady-state MER on this signal in
this RF environment right now.

Mechanics:

- 29-step gain table (the canonical R820T2 ladder, 0.0 dB → 49.6 dB).
- Probe period: ~5 s per gain step, with a 15-probe bail budget so
  pathological RF environments can't pin the controller forever.
- MER metric: `min(MER_lower, MER_upper)`, EMA-smoothed
  (α = 0.4) to reject single-frame outliers.
- Initial gain: 19.7 dB (mid-table), walks **down** first to find
  the noise floor, then **up** until MER stops improving.
- Settled state is sticky — once the controller is satisfied it
  stops touching the dongle. Re-evaluates only on retune or when
  a sustained MER drop suggests something changed (interference,
  multipath, station fade).

The result is hands-off lock on stations that previously needed
fiddling with `manual_gain_tenths` in the config file. As a small
bonus, watching the constellation cloud condense as the AGC sweeps
through gain settings is a satisfying visual confirmation that the
controller is doing its job.

### New: gain mode picker in the Signal panel

Three modes, switchable live from the **📶 Signal** dock tab:

- **Auto AGC** — the new closed-loop controller above. Default for
  fresh installs.
- **Manual gain** — pick a specific R820T2 gain step with a slider
  (snaps to legal table values, shows the dB readout in tenths).
- **Hardware AGC** — hand control of gain back to the tuner chip's
  built-in AGC, the same behavior `nrsc5.exe` uses on its own.

Mode + manual value are persisted in `%APPDATA%\nrsc5-studio\config.toml`
under `gain_mode` and `manual_gain_tenths` and applied on the next
stream start. The Signal panel shows a "(restart stream to apply)"
hint whenever the active stream's mode disagrees with the chosen
one, so it's clear when a change has and hasn't taken effect.

The Signal panel also gains a live AGC readout: current gain in dB,
time since the last gain change, and a status badge (probing /
settled / bailed). All three reflect what the controller is doing
*right now*, independent of whatever the Tuner panel last showed.

### Download

`nrsc5-studio-0.2.1-windows-x64.zip` — portable, x86-64 Windows
10/11. Unzip anywhere and run `nrsc5-studio.exe`. Requires an
RTL-SDR USB dongle (generic RTL2832U + R820T2 works fine;
RTL-SDR Blog V4 also supported via the bundled modern librtlsdr).

### Upgrading from 0.2.0

Drop-in. Your `config.toml`, presets, art cache, play log, and dock
layout all carry over. The new `gain_mode` and `manual_gain_tenths`
fields take their defaults (`Auto` AGC at 19.7 dB) on first load and
are written back to the file the first time you touch the dropdown
or the manual-gain slider.

### Acknowledgments

This release continues to lean on upstream work:

- [theori-io/nrsc5](https://github.com/theori-io/nrsc5) (Aiden / theori) — the v3.1.0 decoder shipping in the bundle.
- [osmocom/rtl-sdr](https://github.com/osmocom/rtl-sdr) — modern `librtlsdr.dll` with clean `rtlsdr_set_tuner_gain` semantics.

The closed-loop AGC controller, gain-mode dropdown, and Signal-panel
readout were developed in collaboration with GitHub Copilot.

MIT licensed.
