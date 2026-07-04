# NRSC5 Studio 0.6.4

A milestone + handoff release.

This release marks the jump from HD-digital-only playback to a full hybrid FM
path: in-app analog FM demodulation with Stereo and RDS support. That required
building and integrating a custom analog receive path alongside the HD decode
pipeline so the app can now provide graceful analog behavior when digital lock
is unavailable.

v0.6.4 also ships stability fixes from main and publishes the AM work branch
for contributors who can test against live AM HD service.

## Release scope

1. FM analog demodulation path with Stereo + RDS support now in the shipped app
2. FM analog fallback quality-of-life and stability updates from main
2. Signal panel source reporting corrections
3. Bug fixes and packaging/version maintenance for this release
4. AM Analog and Digital work on-hold due to no available on-air HD signals in range.

## Highlight: FM Analog + Stereo + RDS

NRSC5 Studio now supports a meaningful FM analog listening path in addition to
HD digital decode.

- Analog FM audio is available in-app when digital is unavailable.
- Stereo/mono behavior is exposed in the FM mode controls.
- RDS is surfaced as part of the analog fallback experience.

This is a major architectural step: the app now handles both sides of hybrid
broadcast behavior instead of being digital-only.

## Fixed

### Automatic fallback now uses MER hysteresis and true audible-source reporting

- In Automatic mode, HD-to-analog ownership now uses hysteresis to prevent
  source flapping near the MER threshold.
- The Signal panel's **Current Source** label now reflects the real audible
  sink owner (HD vs analog), not just sync-lock state.

## AM status and contributor callout

AM Analog and Digital work on-hold due to no available on-air HD signals in
range. The only AM HD station near the maintainer is not currently radiating
usable HD most of the time, and even when it does, local SNR is generally only
high enough at night for intermittent validation.

To keep momentum, the AM development branch is published for community
continuation:

- Branch: `am-radio`
- PR entry point: https://github.com/LTCAshraven/nrsc5-studio/pull/new/am-radio

If you have nearby AM HD coverage and want to help, please include in your PR
or review notes:

1. Region and AM HD stations tested
2. SDR hardware and antenna setup
3. Repro steps and expected vs actual behavior
4. Logs/screenshots (including Signal and Engineering Info panes when useful)

## Notes

- This release intentionally does not claim full AM HD validation.
- AM branch work is available for contributors with compatible RF conditions.
