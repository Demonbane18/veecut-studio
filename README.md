# VeeCut Studio

VeeCut Studio is an open-source AI video editor written in Rust. You put a recording in a folder and ask a coding agent to "edit this". The agent runs the `veecut` command, which cuts, frames, captions and mixes the video with a recipe for that kind of video and renders a draft. You give notes in plain words, in English, Tagalog or Taglish, and get the next draft. A desktop editor opens the same project when you want to change something by hand.

## What it does

- Coding agents and a desktop editor work on one timeline. The command line, the MCP server, the editor and the agents all change a project through the same Editor API, so every change can be undone.
- Built-in recipes for nine kinds of video (table below) make the editing decisions by rule: where to cut, when to punch in, where captions go, how loud the music sits. Most rules come from Herk Edit Kit's editing playbook.
- It is built for free models. Because the recipes do the heavy work, a small free model such as GLM 5.3 Flash Free in OpenCode only has to run a command, read the result and pass on your notes.
- Notes you repeat become rules in `my-rules.md`, so you do not have to say them twice.
- A skills catalog synced from [awesome-claude-video-skills](https://github.com/zhuyansen/awesome-claude-video-skills) lists community video skills by kind of video. It refreshes itself every 24 hours, and installed skills graded SAFE update with it. `veecut skills sync` and `veecut skills update` do the same by hand.
- Everything runs on your computer. Rust draws the picture (cuts, zooms, reframing, color, text, captions, transitions); ffmpeg only decodes, encodes and filters audio, so any ffmpeg build works, including ones without libass or drawtext.
- Text and captions stay readable: when the footage behind them would make them hard to read, VeeCut puts a dark or light scrim behind them. Your colors are never changed.

## Requirements

- Rust 1.90 or newer
- ffmpeg and ffprobe on your PATH
- cmake and a C++ compiler, for local transcription with whisper.cpp

## Install from source

The repository is private until v0.1 works. From a clone:

```
git clone https://github.com/Demonbane18/veecut-studio
cd veecut-studio
cargo install --path crates/veecut
```

To build without local transcription (no cmake or C++ compiler needed), use `cargo install --path crates/veecut --no-default-features --features studio`. Transcription then needs a free Groq key.

## Quick start

```
mkdir my-videos && cd my-videos
veecut init                 # inbox/, outbox/, your style and rules, the agent skill
cp ~/Movies/take1.mp4 inbox/
veecut edit                 # edits the newest video in inbox/ and renders outbox/<slug>-v1.mp4
veecut studio               # opens the desktop editor on the same projects
```

`veecut doctor` checks ffmpeg, transcription, the coding agents it can find and the skills catalog. `veecut edit --kind tutorial` (or any kind below) picks another recipe.

Transcription runs locally with whisper.cpp; the first run downloads a model to `~/.veecut/models`. For faster transcription, put a free Groq key from https://console.groq.com/keys in the workspace `.env` as `GROQ_API_KEY=...`. With the key set, audio is uploaded to Groq.

## Using it with a coding agent

`veecut init` copies the VeeCut skill into `.agents/skills/veecut/` and `.claude/skills/veecut/`, writes `AGENTS.md`, `CLAUDE.md` and `GEMINI.md` so agents use it, and registers `veecut mcp` as an MCP server in `opencode.json`.

OpenCode with GLM 5.3 Flash Free (no account needed):

1. Run `veecut init` in your videos folder and put a recording in `inbox/`.
2. Run `opencode` in that folder.
3. Type `/models` and pick GLM 5.3 Flash Free.
4. Say "edit this" (or "edit mo to"). Then give notes: "bigger captions", "cut the part where I cough", "ok na".

Claude Code: run `claude` in the workspace and say "edit this". It reads `CLAUDE.md` and the skill in `.claude/skills/veecut/`.

Codex: run `codex` in the workspace and say "edit this". It reads `AGENTS.md`, which points at the skill in `.agents/skills/veecut/`.

Gemini CLI: run `gemini` in the workspace and say "edit this". It reads `GEMINI.md`, which imports `AGENTS.md`.

To hand a request to an agent without opening it, run `veecut agent "edit this"`. It uses OpenCode with the free model by default; `--with claude`, `--with codex` or `--with gemini` picks another one.

## The desktop editor

`veecut studio` opens the editor on the workspace; `veecut studio -p PROJECT` opens one project. The welcome screen lists your projects and inbox videos. Under New edit, pick a name, a kind of video, a canvas and a source file, then choose "Auto-edit with the recipe" (runs the recipe and opens the result, without rendering) or "Create empty edit".

With a project open:

- The top bar has the canvas preset, Undo, Redo, Export, and a box to ask a coding agent for a change.
- The left panel has the Media, Text, Captions, Look & Audio, Skills and Agent tabs.
- The preview sits in the middle, with play, frame steps and an `HH:MM:SS:FF` timecode.
- The inspector on the right edits the selected item, or the project when nothing is selected.
- The timeline at the bottom has the magnetic main track, overlay, text, caption and audio tracks, with waveforms and snapping.

Keyboard shortcuts (Cmd on macOS, Ctrl elsewhere):

| Keys | Action |
| --- | --- |
| Space or K | Play or pause |
| J / L | Back / forward 1 second |
| Left / Right | Back / forward 1 frame |
| Shift+Left / Shift+Right | Back / forward 5 seconds |
| Home or Enter / End | Go to the start / end |
| S | Split at the playhead |
| Q / W | Split and delete the left / right part |
| Delete or Backspace | Delete the selection |
| Cmd+C / Cmd+V | Copy / paste at the playhead |
| Cmd+D | Duplicate |
| Cmd+A | Select all |
| Cmd+Z | Undo |
| Shift+Cmd+Z or Cmd+Y | Redo |
| N | Snapping on or off |
| Esc | Clear the selection |

For development, `cargo run -p veecut-studio --example studio -- <workspace> [project.json]` starts the editor straight from the repository. Set `VEECUT_STUDIO_SHOTS=<empty directory>` and open a project at least 3 seconds long to have the editor save three screenshots of itself (`00-paused.png`, then `01-playback.png` and `02-playback.png` one second apart during playback) and quit. It is useful for docs and bug reports, and it only captures the editor's own window.

## Nine kinds of video

| Kind | For | What the recipe does |
| --- | --- | --- |
| `talking-head-short` | Reels, TikTok, Shorts | 9:16, cuts pauses and retakes, face crop, 1.15x punch-ins so no shot runs past 2.8 s, hook text, captions off the face, B-roll from `--broll`, music and sound effects from your style |
| `talking-head-long` | YouTube, Facebook feed | 16:9, natural cuts, 1.10x punch-ins about every 7 s, captions off the face, B-roll from `--broll` |
| `tutorial` | Screen recordings | 16:9, natural cuts, silent stretches of 2 s or more played 2 to 8 times faster with a speed label, subtitles low in the frame |
| `podcast` | Podcasts, interviews | One camera: gentle punch-ins. Two to four cameras: synced by sound, cuts to whoever speaks |
| `clips` | Long video to shorts | Finds the strongest 20 to 60 s moments and makes one vertical draft per moment |
| `vlog` | Trips, days, events | Footage cut to the music's beat, each shot used once, photos panned and zoomed |
| `explainer` | Faceless narration | One visual per sentence, heading cards, ducked music; big centered captions without visuals |
| `promo` | Launches, ads | Lines of copy slammed on the beat over footage, ending on a call to action |
| `music-video` | Songs, lyric videos | Footage cut on the beat, lyrics from LRC or transcribed from the song |

## Skills catalog

`veecut skills recommend --kind K` shows the best catalog skills for a kind of video: SAFE ones, tested first, then by stars. `veecut skills search Q`, `show REPO`, `install REPO`, `update [REPO]` and `remove REPO` do the rest. VeeCut never runs anything from a skill repository: it downloads the repository at a pinned commit, copies the folders that hold a `SKILL.md` into the workspace and records the commit in `skills.lock.json`. Each skill keeps its own license. Many catalog skills need Node, Chrome, Remotion or HyperFrames; read a skill's README before you use it.

The catalog settings live in `~/.veecut/config.toml` (or under `VEECUT_HOME`):

```toml
[skills]
auto_sync_hours = 24   # 0 turns automatic syncing off
auto_update = true     # update installed SAFE skills after a sync
safe_only = true       # refuse skills not graded SAFE unless you pass --force
```

## Privacy

- Your recordings stay where you put them. VeeCut never modifies `inbox/`; drafts and finals go to `outbox/`.
- Video, pictures and sound are processed on your computer.
- Transcription is local unless you set `GROQ_API_KEY`; then audio goes to Groq.
- The network is used to download whisper models on first use, to sync the skills catalog from GitHub and to install skills you choose.
- Coding agents send your prompts and what they read (such as transcripts and command output) to their model provider.
- `veecut init` keeps `.env`, `inbox/`, `outbox/` and `my-sounds/` out of git with a `.gitignore`.

## More

- [docs/architecture.md](docs/architecture.md): crates, the project model, the Editor API and the full command line.
- [CREDITS.md](CREDITS.md): the projects VeeCut learned from, bundled fonts and libraries.

## License

MIT. See [LICENSE](LICENSE).
