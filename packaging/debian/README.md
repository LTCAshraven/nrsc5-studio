# Debian source package skeleton for nrsc5-studio

This directory is a **scaffolding-only** Debian source package layout
for an eventual upload of `nrsc5-studio` to the Debian / Ubuntu
archives. It is **not** used by the day-to-day Linux build pipeline.

For local development and GitHub-Release `.deb` builds, the
`[package.metadata.deb]` section of the root `Cargo.toml` and
`cargo-deb` are the source of truth. Run:

```bash
scripts/build-linux-packages.sh
```

to produce a `.deb` and `.rpm`. That path does **not** consult any
file in this directory.

## When would this skeleton be used?

If/when the project is uploaded to the Debian archive (so users can
run `apt install nrsc5-studio` directly without adding a PPA), the
files here form the starting point for the formal Debian source
package. Reasons it's not used yet:

- The upstream `nrsc5` helper isn't packaged in Debian, which means
  any Debian upload of `nrsc5-studio` requires a parallel Debian
  packaging effort for `nrsc5` first (see `docs/linux-install.md` for
  background).
- A formal upload would need to be sponsored by an existing Debian
  Developer, and the package would need to clear Lintian without
  errors. The skeleton in this directory is policy-aware but is not
  yet warning-free.

## Files

| File                              | Purpose                                           |
|-----------------------------------|---------------------------------------------------|
| `control`                         | Package metadata, build / runtime dependencies.   |
| `rules`                           | Debhelper-driven build/install entry points.      |
| `changelog`                       | dpkg changelog (also tracked for cargo-deb hint). |
| `copyright`                       | DEP-5 machine-readable copyright file.            |
| `source/format`                   | `3.0 (quilt)` for archive uploads.                |
| `nrsc5-studio.install`            | File list for `dh_install`.                       |
| `nrsc5-studio.manpages`           | Manpage list for `dh_installman`.                 |
| `nrsc5-studio.lintian-overrides`  | Documented lintian noise the package is allowed.  |
| `upstream/metadata`               | Hints for `debian/upstream` style trackers.       |

## How to use it manually (advanced)

If you want to drive a Debian build the traditional way:

```bash
# Stage the debian/ tree at the workspace root.
cp -a packaging/debian ./debian

# Build with debuild (apt: build-essential, debhelper, devscripts).
sudo apt-get build-dep .
debuild -us -uc -b
```

The resulting `.deb` will land one directory above the workspace.
Remember to `rm -rf ./debian` before committing.
