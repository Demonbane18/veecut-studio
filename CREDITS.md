# Credits

VeeCut Studio learned from other people's work. This file says from whom and what, and under which license. VeeCut is written in Rust and reimplements the ideas below in its own code.

## Projects VeeCut learned from

[free247editor](https://github.com/riecodes/free247editor) by riecodes, MIT. The workflow ideas: an `inbox/` for raw recordings and an `outbox/` for numbered drafts and finals, notes that become lasting rules in `my-rules.md` (rules beat style, style beats house style), the short multiple-choice style setup with a 5-second sample, plain-word notes in English, Tagalog and Taglish, and the choice between free local transcription and a free Groq key.

[Herk Edit Kit](https://github.com/nateherkai/herk-edit-kit) by Nate Herk, MIT. The editing playbook behind the recipes, with its rules and numbers: cutting pauses and keeping natural breaths, finding stutters, false starts and retakes, caption size, timing, color, contrast ("never white on light") and placement, the camera rhythm of punch-ins and shot lengths, story beats, music and sound effect levels, and the checks before delivery. The AI Automation Society brand assets in that kit belong to their owner and are not used here.

[OpenCut](https://github.com/OpenCut-app/OpenCut), MIT. Feature ideas for the desktop editor: the panel layout, a magnetic main track, timeline behaviors and the default keyboard shortcuts, one Editor API for every change, and the ideas for an MCP server and headless use. VeeCut uses no OpenCut code, name or logo.

[awesome-claude-video-skills](https://github.com/zhuyansen/awesome-claude-video-skills) by zhuyansen. The list itself is CC0 1.0, and it is the source of VeeCut's skills catalog. Each listed skill keeps its own license. VeeCut installs a skill from its own repository at a pinned commit and records that commit in `skills.lock.json`.

## Rules adapted from catalog skills

Some workflow files in the built-in skill (`crates/veecut-engine/skill/workflows/`) restate rules from these catalog skills in our own words, with a link to the source. Each was read at the commit below.

| Skill | License | Commit | Used in |
| --- | --- | --- | --- |
| [kmilo93sd/agent-video-editor](https://github.com/kmilo93sd/agent-video-editor) | MIT | `8e32c0a` | tutorial, podcast, vlog |
| [gquthier/autonomous-sop-video](https://github.com/gquthier/autonomous-sop-video) | MIT | `19a7f7f` | tutorial |
| [vincentsch/explainroo](https://github.com/vincentsch/explainroo) | MIT | `e95232c` | explainer |
| [znyupup/ai-video-editing-skill](https://github.com/znyupup/ai-video-editing-skill) | MIT | `b6429ab` | vlog |
| [charlie947/motion-graphics-skills](https://github.com/charlie947/motion-graphics-skills) | MIT | `229022e` | promo |
| [Finderchangchang/promo-video-skill](https://github.com/Finderchangchang/promo-video-skill) (BrewReel) | Apache-2.0 | `217eb1f` | promo |
| [BuzyOwl/song-video-director](https://github.com/BuzyOwl/song-video-director) | MIT | `c0f4752` | music-video |
| [EGSECDA/vocaloid-style-mv-pipeline](https://github.com/EGSECDA/vocaloid-style-mv-pipeline) | MIT | `13b380d` | music-video |

## Bundled fonts

The caption and text fonts in `crates/veecut-engine/assets/fonts/` are under the SIL Open Font License 1.1. Each family's `OFL.txt` sits next to its font files.

| Family | Files | Copyright |
| --- | --- | --- |
| Abril Fatface | Regular | TypeTogether; Reserved Font Names "Abril" and "Abril Fatface" |
| Anton | Regular | The Anton Project Authors |
| Archivo Black | Regular | The Archivo Black Project Authors |
| Bebas Neue | Regular | Dharma Type |
| Inter | SemiBold | The Inter Project Authors |
| Montserrat | Bold, ExtraBold | The Montserrat.Git Project Authors |
| Poppins | Bold | The Poppins Project Authors |

## Face detection model

`crates/veecut-engine/assets/models/seeta_fd_frontal_v1.0.bin` is the SeetaFace frontal face detection model, the same file that ships with the rustface crate. It is under the BSD 2-Clause license, copyright the Visual Information Processing and Learning (VIPL) group, Institute of Computing Technology, Chinese Academy of Sciences. The license text is in `assets/models/LICENSE-seetaface.txt`.

## Main libraries

Licenses as reported by `cargo metadata --format-version 1`.

| Library | Version | License | Used for |
| --- | --- | --- | --- |
| egui and eframe | 0.37.0 | MIT OR Apache-2.0 | the desktop editor |
| cosmic-text | 0.19.0 | MIT OR Apache-2.0 | text shaping and layout |
| tiny-skia | 0.12.0 | BSD-3-Clause | drawing frames, text and captions |
| whisper-rs and whisper-rs-sys | 0.16.0, 0.15.0 | Unlicense | local transcription |
| whisper.cpp (bundled in whisper-rs-sys) | | MIT, The ggml authors | the speech recognition engine |
| rustface | 0.1.7 | BSD-2-Clause | face detection for framing |
| fast_image_resize | 6.1.0 | MIT OR Apache-2.0 | scaling video frames |
| ureq | 3.5.0 | MIT OR Apache-2.0 | downloads: models, the catalog, skills, Groq |

`cargo metadata` lists every other dependency with its license.

## Not bundled

ffmpeg and ffprobe are separate programs that you install yourself. VeeCut runs them and does not ship them; they come under their own license, which depends on how your build was made. Whisper models are downloaded on first use from the whisper.cpp model repository on Hugging Face into `~/.veecut/models`.
