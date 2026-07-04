# Draft PR: AM analog/digital handoff (community testing wanted)

## Title

WIP: AM analog + digital scaffolding handoff (seeking on-air AM HD testers)

## Summary

This branch publishes in-progress AM analog/digital work and scaffolding so
contributors with nearby AM HD service can continue validation and iteration.

AM Analog and Digital work on-hold due to no available on-air HD signals in
range.

## Why this PR exists

- Keep AM work unblocked while preserving current mainline release velocity.
- Invite contributors with real AM HD coverage to test and refine the path.
- Capture validation data and implementation feedback in one place.

## What this branch includes

- Existing AM analog-path work already completed on this branch
- AM-facing scaffolding for continued integration/testing
- Current behavior snapshots for follow-on contributors

## What this PR is not

- Not merge-ready without on-air AM HD validation
- Not claiming full AM feature completion

## Testers wanted

If you can test live AM HD, please comment with:

1. Region and stations tested
2. SDR hardware + antenna
3. Exact tune/mode steps
4. Expected vs actual behavior
5. Logs/screenshots (Signal panel + Engineering Info where relevant)

## Maintainer notes

- I will add additional implementation notes and expectations in follow-up
  comments.
- Mainline release work continues in parallel via v0.6.4.
