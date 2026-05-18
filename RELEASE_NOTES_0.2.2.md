## NRSC5 Studio 0.2.2

A polish + portability release on top of 0.2.1. No architectural
changes — the piped-SDR backend, closed-loop AGC, and spectrum panel
all behave identically. What changed: the app can now run as a
true portable install, the song log got real controls, and a long
list of small UX papercuts got cleaned up.

### New: portable mode

Drop a zero-byte `portable.txt` next to `nrsc5-studio.exe` and the
app stops writing to `%APPDATA%\nrsc5-studio\` entirely. Everything
that used to live there — `config.toml`, the AAS file cache, the
album-art cache, the play log, even the egui window-state DB — moves
into a single `./data/` folder next to the executable. The official
portable zip ships with the marker file in place, so the zip is
truly self-contained: unzip to a USB stick, run, copy the whole
folder somewhere else, run again, the dock layout and presets and
collage all follow.

Non-portable installs are unaffected. If `portable.txt` is missing
the old `%APPDATA%` path is used, and the app will even migrate a
legacy config from `%APPDATA%\NRSC5-RUST\` if it finds one. A single
new module (`src/paths.rs`) owns the portable-vs-roaming dispatch
so the rest of the code just calls `crate::paths::config_path()`,
`crate::paths::play_log_path()`, etc., without caring which mode is
active.

### New: configurable play-log retention + Clear button

The 24-hour rolling song log is no longer hard-coded to 24 hours.
The `📝 Log` tab now exposes:

- A **Rolling window** dropdown with seven choices: 1h / 6h / 12h /
  1d / 2d / 3d / 7d. Selection is persisted to `config.toml`
  as `play_log_retention_hours` and applied immediately — the next
  prune cycle uses the new window.
- A **Clear** button that wipes the in-memory log and the on-disk
  `play_log.csv`. Useful when you want a fresh capture without
  waiting for the rolling window to age old entries out.
- A **native Save As dialog** for CSV export (replaces the silent
  "save to fixed path" behavior). Powered by `rfd` so it gets the
  same dialog you see in Notepad / Explorer.

Defaults: `Auto AGC` retention falls back to 24 hours if the saved
value is missing or out of range (clamped to 1h..168h on load).

### Fixes

- **Dark mode pin.** On a system with the OS theme set to light, the
  app was respecting the OS instead of `config.dark_mode = true`,
  giving a light UI despite the saved preference. Explicit
  `egui::ThemePreference` set during font/theme install fixes it
  for both polarities.
- **Stale call-sign clearing.** Hitting Stop, or retuning to a
  station that doesn't broadcast a call sign in its SIS table,
  used to leave the previous station's letters frozen in the
  Tuner panel. Now cleared on every Stop and TuneMhz so it
  visibly reflects the live state.
- **Weather radar without basemap.** When a station broadcast a
  weather overlay frame before (or in lieu of) its basemap tile,
  the radar pixels were being composited onto a transparent
  background and then dropped, producing the "radar appears,
  vanishes again, never comes back" pattern. The overlay
  processor now skips composition until the basemap is in hand,
  and frame state is reset on basemap change so the loop replays
  correctly.
- **Glyph audit + font fallback.** A handful of UI glyphs were
  showing as tofu (`□`) because egui's default Proportional font
  chain (Ubuntu-Light → NotoEmoji → emoji-icon-font) doesn't
  include `Hack-Regular.ttf` — and a few characters live only
  there. Hack is now appended as a Proportional fallback so
  geometric shapes (●, ○, ■, □, →, ▸, ◆, …) render correctly
  everywhere, not just in monospace contexts. The retention
  dropdown's "✓ selected" indicator that started this audit was
  the only character not covered by *any* bundled font; replaced
  with `✔` (covered by NotoEmoji). A new helper script
  (`scripts/probe-glyphs.ps1`) walks the bundled TTF cmaps and
  prints a coverage table for every codepoint the UI uses, so the
  same audit is reproducible going forward.
- **Piped backend now the default.** A fresh install used to write
  `use_piped_sdr = false` to `config.toml`, which silently
  disabled both the closed-loop AGC and the spectrum FFT tap —
  both of those are only wired through the piped path. The
  default is now `true` so the features that ship with v0.2.x
  work out of the box without editing the config file by hand.
- **Album-art cache cleanup.** Switching backends or stopping a
  stream now invalidates the in-memory tile cache so a stale
  cover doesn't linger.

### Download

`nrsc5-studio-0.2.2-windows-x64.zip` — portable, x86-64 Windows
10/11. Unzip anywhere and run `nrsc5-studio.exe`. Requires an
RTL-SDR USB dongle (generic RTL2832U + R820T2 works fine;
RTL-SDR Blog V4 also supported via the bundled modern librtlsdr).

### Upgrading from 0.2.1

Drop-in. Your existing `%APPDATA%\nrsc5-studio\config.toml`, presets,
art cache, play log, and dock layout all carry over unchanged
*unless* you opt into portable mode by dropping a `portable.txt`
next to the new exe, in which case you start with a fresh
`./data/` folder. New config keys (`play_log_retention_hours`)
take their defaults on first load and are written back the first
time you touch the dropdown.

### Acknowledgments

This release continues to lean on upstream work:

- [theori-io/nrsc5](https://github.com/theori-io/nrsc5) (Aiden / theori) — the v3.1.0 decoder shipping in the bundle.
- [osmocom/rtl-sdr](https://github.com/osmocom/rtl-sdr) — modern `librtlsdr.dll` with clean `rtlsdr_set_tuner_gain` semantics.

Portable-mode plumbing, the play-log Clear + retention controls,
the weather-overlay fix, and the font-fallback audit were
developed in collaboration with GitHub Copilot.

MIT licensed.
