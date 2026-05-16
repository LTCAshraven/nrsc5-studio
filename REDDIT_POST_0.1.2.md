# Reddit launch post — NRSC5 Studio 0.1.2

Strategy + ready-to-paste copy for the r/RTLSDR announcement.

---

## Strategy

- **Image post**, not text. The album-art collage at high tile count is the
  visual hook nobody else's nrsc5 frontend has — make that the main image.
  r/RTLSDR engagement on image posts is ~5–10x text.
- **Upstream credit in the first paragraph.** Aiden/theori (`nrsc5`), cmnybo
  (`nrsc5-gui`), markjfine (`nrsc5-dui`). That sub will check, and leading
  with it earns immediate goodwill.
- **"Free, MIT, open source"** in the first three lines. The sub has burned
  closed-source HD Radio apps before.
- **Time it Tue–Thu, ~10am–1pm US Eastern.** Avoid weekends (drowned by
  hardware-question floods).
- **Pinned first comment** with a short FAQ — preempts pile-ons.
- **Skip the "I'm not a real dev" preface.** Charming in the README, invites
  snark on Reddit.

---

## Title (recommended)

> **NRSC5 Studio — a free, open-source Windows HD Radio app for RTL-SDR, with an 8-hour album-art heat-map**

Alternatives:

- *Built a Rust + egui frontend around nrsc5: HD1–HD4 playback, traffic maps, weather radar loops, album-art collage*
- *Wanted a nice Windows GUI for nrsc5, so I built one — NRSC5 Studio (MIT, RTL-SDR)*

---

## Body

```
NRSC5 Studio is a native Windows desktop app for listening to HD Radio
broadcasts with an RTL-SDR dongle. Free, MIT-licensed, no installer, no
telemetry, no account, no nag.

It stands on the shoulders of:
- Aiden / theori — the underlying nrsc5 decoder
  https://github.com/theori-io/nrsc5
- cmnybo — nrsc5-gui (Python/Tk)
  https://github.com/cmnybo/nrsc5-dui
- markjfine — nrsc5-dui (Python/GTK)
  https://github.com/markjfine/nrsc5-dui

I wanted a polished, native-feeling Windows app and a place to play with
some ideas the existing frontends don't do, so I wrote one in Rust + egui.

Highlights:
- HD1 / HD2 / HD3 / HD4 subchannel playback with persistent presets
- Now-Playing pane with cover art and station logo
- Album-art heat-map collage — every unique cover seen in the last 8
  hours becomes a square tile, bucketed by play count (top tiles grow
  to 6x6 cells, long tail stays 1x1). Cached to disk so it survives
  Stop/Start and full restarts. Tile cap is user-adjustable (1-512,
  power-of-two stepper).
- QPSK constellation scope driven by live per-sideband MER
- TPEG traffic-tile map and 90-minute weather radar loop with scrubber
  (iHeart stations only — they're the ones broadcasting it)
- Live MER (upper/lower) and BER readouts
- Windows per-app volume slider (COM-based), so the app's volume
  doesn't drag your whole system
- Persistent dockable tabs, dark/light themes, DPI-aware

Hardware: any generic RTL2832U + R820T2 dongle works fine. Windows 10/11
x86_64. Linux is on the roadmap but blocked on replacing the
Windows-specific audio path.

Repo:    https://github.com/LTCAshraven/nrsc5-studio
Release: https://github.com/LTCAshraven/nrsc5-studio/releases/tag/v0.1.2

Feedback, bug reports, and "have you tried station XYZ" reports welcome.
```

---

## Pinned first comment (post immediately after the main post)

```
Quick FAQ before it gets asked:

- Why Rust? Wanted a single-binary, no-runtime, native build.
  egui made the GUI side painless.
- Linux / macOS? Yes eventually. Audio output is the only
  Windows-bound piece; everything else is portable.
- RSP1A / SDRplay / Airspy? RTL-SDR only today. RSP1A is on
  the list.
- Is this just a reskin of nrsc5.exe? No — nrsc5 is linked as a
  library; the GUI, the collage, the weather scrubber, the dock,
  the per-app volume, the presets, all of that is mine.
- Does it phone home? No. No network access at all except what
  nrsc5 / RTL-SDR libraries need locally.
```

---

## Reddit mechanics tips

- **Account age & karma.** r/RTLSDR has an auto-mod minimum (~30-day-old
  account, some positive karma). If your account is brand new, your post will
  be silently filtered. Comment on a few threads in the days before posting.
- **Use the "Project" flair** if it exists (check the sidebar's
  required-flair list).
- **One image only** as the main image. Reddit's gallery posts get less
  reach than single images. The 8-hour collage at ~256 tiles is the best
  one — it's the "wow" frame.
- **Don't edit the post for ~2 hours** after posting. Reddit's ranking
  algorithm de-prioritizes edited posts in the first hour.
- **Crosspost candidates** (later, after r/RTLSDR settles): r/HDRadio,
  r/amateurradio (carefully — stricter about scope), r/rust (as a
  project-show post, not a self-promo).
- **Don't reply defensively** to anything in the thread. "Good point, will
  look at it" beats engaging with criticism every time.
