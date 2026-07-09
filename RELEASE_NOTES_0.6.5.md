# NRSC5 Studio 0.6.5 — "The Colour and the Shape"

This is a **fixes-and-tweaks** release: your panel arrangement now **persists across restarts**, fresh installs open into a **redesigned multi-panel layout** instead of one crowded
tab bar, and the Spectrum panel gains optional **smoothing** to tame the
frame-to-frame jitter of the FFT trace. Alongside those it clears a batch of
long-standing annoyances — dead HD subchannel buttons, a stray station-logo
load error, a freeze on refocusing a minimized window, and album-art blocks
that didn't survive a restart.

If you don't care about the internals: the app remembers how you left your
windows, looks better out of the box, draws a steadier spectrum, and stops
doing several small irritating things.

## What's new

### Dock layout persists across restarts

Whatever panel arrangement you leave the app in — docked splits *and* detached
floating windows — is now saved on exit and restored on the next launch. A
saved layout that fails to deserialize (for example after an internal layout
schema change) is discarded silently, so a stale layout can never brick
startup. A hidden **Ctrl+Shift+D** helper dumps the live layout to
`dock-layout-dump.ron` for capturing future defaults.

### Redesigned default dock layout

Fresh installs — and any launch with no saved layout — now open into a curated
multi-panel layout instead of every panel collapsed into a single tab bar:

- **Left:** Tuner / Station Info / Signal / Engineering
- **Center:** Now Playing / Collage / Spectrum / Constellation, with a Log
  strip beneath
- **Right:** Weather / Traffic

The layout is a single-surface, fraction-based split tree, so it scales
proportionally across resolutions (1080p ↔ 4K). It also serves as the fallback
whenever a saved layout can't be restored.

### Spectrum smoothing

The Spectrum panel gains an optional **Spectrum Smoothing** toggle with a
strength slider. When enabled, the drawn spectrum trace is run through an
exponential moving average, taming the frame-to-frame jitter of the FFT line
into a steadier curve; the slider trades responsiveness for smoothness (higher
= smoother). Only the rendered line is smoothed — the waterfall keeps raw FFT
values so its history stays faithful. Off by default, and both the toggle and
strength persist in the config.

## What's fixed

### Greyed-out HD subchannels are no longer clickable

Program buttons for subchannels the tuned station doesn't deliver were drawn
greyed but stayed interactive — their tooltip even read "Click to tune anyway,"
and clicking one selected a dead subchannel with unpredictable results
([#20](https://github.com/LTCAshraven/nrsc5-studio/issues/20)). Each HD button
is now rendered as a genuinely disabled widget when the station neither
advertises the subchannel nor has audio flowing for it, so it can't be clicked;
only advertised / on-air slots (and the currently active one) remain
selectable.

### Station logo preload no longer loads `.src` sidecars as images

Each cached logo is stored alongside a `<image>.src` source sidecar that shares
the `{freq}_hd{n}_` prefix. The preloader scanned the cache directory by prefix
without filtering, so the sidecar could be parsed as an image path and handed
to egui — producing an intermittent "No matching ImageLoader" error in the
Station Information panel (e.g. `971_hd2_d9eca340.png.src`). The preload now
skips any file ending in `.src`.

### Unfocused / minimized window no longer freezes on refocus

Decoder events (metadata + cover-art payloads) flow through a channel the GUI
only drains while painting. When the window was minimized or unfocused, Windows
suspended painting, so events piled up for the whole time the app sat in the
background; on refocus the entire backlog was processed in a single frame —
freezing the UI for seconds and, on a busy cover-art station, occasionally
exhausting memory during the texture-upload burst. Three changes address it:
the per-frame event drain is now capped (spreading catch-up across frames
instead of one giant hitch); the decoder wakes the UI via a repaint callback so
it keeps draining while unfocused-but-visible; and the collage relayout and
art-cache disk write are deferred until the backlog is fully drained, so the
collage jumps straight to the current state instead of visibly stepping through
hundreds of intermediate layouts.

### Album-art block list now persists across restarts for every image

Block entries are content hashes written to `config.toml`, but TOML integers
are signed 64-bit — a hash above `i64::MAX` (roughly half of all hashes)
couldn't be serialized, which silently aborted the *entire* config write, so
those blocks disappeared on the next launch. Hashes are now stored via a
lossless bit-cast (a high hash round-trips as a negative integer); existing
entries are unaffected, and a serialization failure is now logged instead of
swallowed.

## Under the hood

A project-quality / tooling pass landed with this release: a GitHub Actions CI
workflow (rustfmt, Clippy with `-D warnings`, the test suite, and `cargo-deny`),
plus `SECURITY.md`, `CONTRIBUTING.md`, issue/PR templates, a `deny.toml`
dependency-and-license policy, and a pinned `rustfmt.toml`. `anyhow` was bumped
to clear a RustSec advisory, `cargo fmt` was run across the whole tree, and the
resulting Clippy backlog was cleared. No user-facing behavior change from the
tooling work.

## Downloads

- **Windows (portable):** `nrsc5-studio-0.6.5-windows-x64.zip` — unzip and run
  `nrsc5-studio.exe`.
- **Linux (Debian/Ubuntu):** `nrsc5-studio_0.6.5-1_amd64.deb`
- **Linux (Fedora/RHEL):** `nrsc5-studio-0.6.5-1.x86_64.rpm`

The optional high-resolution `map2x.png` basemap from 0.6.3 still applies and is
unchanged — grab it from the 0.6.3 assets if you want the sharper maps.
