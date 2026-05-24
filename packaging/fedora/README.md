# Fedora .spec scaffold for nrsc5-studio

This directory is the Fedora-side counterpart of
`../debian/` — a hand-authored RPM spec that would be the starting
point for a COPR or Fedora-archive upload.

For local development and GitHub-Release `.rpm` builds, the
`[package.metadata.generate-rpm]` section of the root `Cargo.toml` and
`cargo-generate-rpm` are the source of truth. Run:

```bash
scripts/build-linux-packages.sh
```

to produce both a `.deb` and a `.rpm` via the cargo-native path.

This `.spec` would be used for:

- **Fedora COPR**: upload the source tarball + `.spec` to
  `copr.fedorainfracloud.org` for hosted builds across Fedora releases.
- **Fedora package archive**: a formal Fedora package would start
  here, sponsored by an existing Fedora packager, and would need to
  pass `rpmlint` and `fedora-review`.

Neither path is active yet. See `../debian/README.md` for the parallel
Debian story.

## Building locally with this spec (advanced)

```bash
sudo dnf install rpm-build rpmlint
sudo dnf builddep packaging/fedora/nrsc5-studio.spec
rpmbuild -bb packaging/fedora/nrsc5-studio.spec \
    --define "_topdir $(pwd)/build/rpm"
```

The output `.rpm` lands in `build/rpm/RPMS/x86_64/`. The `Source0:`
URL points at a GitHub release tarball; for local iteration you can
copy `packaging/fedora/nrsc5-studio.spec` next to a release tarball
named `nrsc5-studio-0.3.7.tar.gz` first.
