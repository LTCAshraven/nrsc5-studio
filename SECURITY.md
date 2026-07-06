# Security Policy

## Supported versions

NRSC5 Studio is developed as a rolling release. Security fixes land on the
latest published version; there are no long-term maintenance branches for
older releases.

| Version | Supported |
|---------|-----------|
| Latest `0.6.x` | ✅ |
| Older releases | ❌ (please upgrade) |

## Reporting a vulnerability

**Please do not open a public issue for security vulnerabilities.**

Report privately through GitHub's built-in vulnerability reporting:

1. Go to the repository's **Security** tab →
   <https://github.com/LTCAshraven/nrsc5-studio/security/advisories>
2. Click **Report a vulnerability** and fill in the advisory form.

This opens a private channel visible only to you and the maintainers.

When reporting, please include:

- The affected version (see **Help → About** in the app, or the release tag).
- Your operating system and SDR hardware (RTL-SDR / SDRplay / other).
- A clear description of the issue and its impact.
- Steps to reproduce, and a proof of concept if you have one.
- Any relevant logs (e.g. `agc-trace.log`) — but **redact anything
  sensitive** first.

## What to expect

- Acknowledgement of your report as soon as the maintainers are able to
  triage it.
- An assessment of whether the report is accepted, and if so, a fix plan.
- Coordinated disclosure: we will agree on a timeline before any public
  details are shared, and credit you in the advisory unless you prefer to
  remain anonymous.

## Scope

This project is a desktop application that decodes over-the-air HD Radio
(NRSC-5) broadcasts. Reports that are in scope include, but are not limited
to:

- Memory-safety or logic bugs reachable by parsing broadcast data
  (SIS/PSD metadata, LOT/AAS payloads such as cover art, station logos,
  traffic tiles, and weather frames — all attacker-influenced since they
  arrive over the air).
- Path-traversal or arbitrary-file-write issues when LOT payloads are
  staged to the AAS scratch directory.
- Vulnerabilities in how the app loads native libraries at startup
  (`bin/` PATH prepend, `SOAPY_SDR_PLUGIN_PATH`).
- Issues in the network SDR transports (`rtl_tcp`, SoapyRemote).

## Bundled native components

NRSC5 Studio links against native libraries that are built from upstream
sources and shipped in the release bundle:

- `libnrsc5` (the HD Radio decoder), which statically embeds **FFTW**,
  **FAAD2**, **libusb**, and **rtl-sdr**.
- **SoapySDR** and its device modules.

If a vulnerability originates in one of these upstream projects, please also
consider reporting it to the respective upstream maintainer. We will pick up
security-relevant upstream releases and rebuild the bundled libraries as part
of a new NRSC5 Studio release. The pinned upstream versions and their
licenses are documented in [`THIRD_PARTY_NOTICES.md`](THIRD_PARTY_NOTICES.md).

## Dependency scanning

Rust dependencies are continuously checked in CI with
[`cargo-deny`](https://github.com/EmbarkStudios/cargo-deny), which audits the
tree against the [RustSec advisory database](https://rustsec.org/) and
enforces the license / source policy in [`deny.toml`](deny.toml).

## Safe harbor

We consider security research conducted in good faith — that respects user
privacy, avoids service disruption, and follows this policy — to be
authorized. We will not pursue or support legal action against researchers
who act accordingly.
