---
name: veecut-arcade
description: Apply VeeCut's portrait arcade look with pixel type, a story-progress meter, pixel-resolve cuts and synthesized game sounds. Suits explainers, promos and short talking heads.
license: MIT
---

# Arcade look

Use the core `veecut` skill for the editing workflow. Start with `veecut edit --skill veecut-arcade --kind explainer` when the user supplies narration and ordered visuals, or apply this look to an existing project with `veecut skills apply veecut-arcade`.

Keep each beat readable before the next cut. Use short hooks and counters: Press Start 2P takes more width than normal text. The HP bar shows elapsed story progress, not a health score. It spans the project and fills in twelve steps. Clip pixelation affects illustrations and B-roll, never the speaker's own camera.

The pack sets a dark violet canvas, white word-pop captions with amber accents, slow pushes and 0.4-second pixel-resolve transitions. Coin cues mark text entrances; short blips mark cuts. Its original synthesized chiptune bed sits at -20 dB gain. Check the mix against the user's voice; reduce or remove cues when they mask words.

Run `veecut skills check veecut-arcade`, then inspect representative frames with `veecut frame --at T`. In Studio, do not export unless asked. For a custom version, save the project's look with `veecut skills new my-arcade --from-project`, then edit its `veecut.toml` and check again.

## Disclosure

VeeCut's own MIT instructions and look values. Timings and sizes follow published measurements in Jad M.H's (NextPhase AI) Arcade Motion Graphics skill; implementation written fresh. No pack scripts, artwork, fonts, melodies or sound files are included. Press Start 2P comes from upstream Google Fonts under the OFL and is bundled with VeeCut.
