---
name: veecut-editorial
description: Apply VeeCut's paper-and-ink editorial look with lifted-black monochrome footage, reveal captions, terracotta highlights and soft cut accents. Suits explainers, documentaries and long talking heads.
license: MIT
---

# Editorial look

Use the core `veecut` skill for the editing workflow. Choose `veecut edit --skill veecut-editorial --kind documentary` for a speaker with found footage, or apply this look to an existing project with `veecut skills apply veecut-editorial`.

Use real, licensed material to support each claim. Keep one main word or number per beat. Paper, dark ink and terracotta carry the layout; the clip-only soft monochrome grade keeps accent colours intact. B-roll pushes slowly while the speaker's own camera keeps its original treatment.

The pack uses Inter reveal captions, a paper canvas and whip transitions at every second cut. It never introduces crossfades. Use the `marker` text preset to underline an exact phrase with a moving highlight band. Keyword, chapter and marker styles share the terracotta accent. A quiet synthesized sub boom marks cuts; this pack adds no music. Use the user's own licensed bed if requested.

Run `veecut skills check veecut-editorial` and inspect both text and footage with `veecut frame --at T`. Check dark type against the footage as well as paper. In Studio, do not export unless asked. Save a variant with `veecut skills new my-editorial --from-project`, edit its `veecut.toml`, check and reapply.

## Disclosure

VeeCut's own MIT instructions and look values. Timings and sizes follow published measurements in Jad M.H's (NextPhase AI) Motion Graphics V3 skill; implementation written fresh. No pack scripts, artwork, fonts or sound files are included. Fonts come from VeeCut's separately licensed upstream font collection.
