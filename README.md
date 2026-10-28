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

`veecut doctor` checks ffmpeg, transcription, agent readiness and the skills catalog. Claude Code and Codex checks distinguish signed-out accounts from broken configuration; fixes include `claude auth login` and `codex login`. Checks do not change your settings. `veecut edit --kind tutorial` (or any kind below) picks another recipe.

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

Choose a model and effort for one run:

```sh
veecut agent "bigger captions" --with opencode --model opencode/glm-5.3-flash-free --effort low
veecut agent "bigger captions" --with claude --model haiku --effort low
veecut agent "bigger captions" --with codex --model gpt-5.6-luna --effort low
veecut agent "bigger captions" --with gemini --model gemini-2.5-flash
veecut agent --models --with opencode
```

`--models` without `--with` lists all four agents. OpenCode's list combines built-in choices
with `opencode models`. `--model default` uses VeeCut's default: free GLM for OpenCode,
or the CLI's own model for other agents. `--effort default` uses the agent's own effort.
Gemini has no effort setting. Usage appears after a run when reported by the agent;
unknown token counts or cost are not treated as zero.

Studio saves model and effort choices per agent in `~/.veecut/config.toml`:

```toml
[agent.selection.opencode]
model = "opencode/glm-5.3-flash-free"
effort = "low"

[agent.selection.claude]
model = "haiku"
effort = "low"
```

Omit `effort` to use the agent's default. OpenCode defaults to GLM 5.3 Flash Free; the other
agents use their CLI's default model. Existing custom command overrides take precedence;
VeeCut does not add model, effort or output-format flags to them.

Claude's `ultracode` effort enables its workflow planner, uses `max` effort and allows
`Workflow` in headless mode. This is the most expensive choice, roughly five times the
tokens of a normal run, and requires a model that supports `xhigh`.

Ctrl+C stops the agent and its inherited helpers. No project change is rolled back; saved
edits remain undoable.

When OpenCode reports an exhausted free allowance or an unavailable model, VeeCut stops
the run and shows the provider problem. Free access can take a day or more to return.
Errors that OpenCode writes only to its private log cannot trigger this early stop;
the evaluation harness still enforces its deadline.

## The desktop editor

`veecut studio` opens the editor on the workspace; `veecut studio -p PROJECT` opens one project. The welcome screen lists your projects and inbox videos. Under New edit, pick a name, a kind of video, a canvas and a source file, then choose "Auto-edit with the recipe" (runs the recipe and opens the result, without rendering) or "Create empty edit".

With a project open:

- The top bar has the canvas preset, Undo, Redo, Export, and a box to ask a coding agent for a change.
- The left panel has the Media, Text, Captions, Look & Audio, Skills and Agent tabs.
- The preview sits in the middle, with play, frame steps and an `HH:MM:SS:FF` timecode.
- The inspector on the right edits the selected item, or the project when nothing is selected.
- The timeline at the bottom has the magnetic main track, overlay, text, caption and audio tracks, with waveforms and snapping.
- Studio notes apply ops and refresh the preview. Agents render or open a video only when
  the note asks for one. The Agent tab has model and effort pickers, elapsed time, Stop,
  and a readiness Refresh button. Your latest note, reply and reported usage stay visible;
  tool output is folded below them.
- Talking-head Auto-edit keeps face boxes over time and places captions clear of the mouth
  and chin through crops and punch-ins. It moves or shrinks crowded pages, tries an upper
  slot clear of the hook, and reports pages it cannot safely show or check. Existing projects
  need Auto-edit again to collect face data.
- `veecut ops` reports safe-area clamps and actual caption boxes for layout changes;
  `veecut frame --at T` reports the caption placement at that time.
- Check frames stay in the project's `.cache/` by default. `frame` and `frames` refuse
  destinations in `inbox/` or `outbox/`, including symlink aliases.
- Automatic hooks use complete short sentences. If none fits, Auto-edit omits the hook
  and asks for a line in review.
- Auto-edit decisions stay under **Needs review** in the inspector until dismissed. They
  survive reopening and cuts; Dismiss is saved and supports Undo within the current session.

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

For development, `cargo run -p veecut-studio --example studio -- <workspace> [project.json]` starts the editor straight from the repository. Set `VEECUT_STUDIO_SHOTS=<empty directory>` and open a project at least 3 seconds long to save four screenshots: `00-paused.png`, `01-playback.png`, `02-playback.png`, and `03-agent.png` with the model and effort pickers. The editor then quits. This hook captures only the editor's own window and needs no GUI automation.

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

## Model evaluation harness

`veecut-eval` is an unpublished workspace tool. Its fixture manifest and media live outside
git, by default in `~/veecut-eval/`. Media, run workspaces and screenshots need their own
backup; they are not included in the repository.

```sh
cargo run -p veecut-eval -- baselines
cargo run -p veecut-eval -- run --screening
cargo run -p veecut-eval -- run --fixture ths-s-mark-pond-31s --task caption-size \
  --agent opencode --model opencode/glm-5.3-flash-free --trials 2
cargo run -p veecut-eval -- ladder --fixture ths-s-mark-pond-31s --level 2
cargo run -p veecut-eval -- report
```

`baselines` runs each fixture's recipe locally on CPU, with no agent and no render. It
links inputs into `work/baselines/<id>/inbox/`, records `baseline.json`, `report.json`
and `baseline.log`, and skips completed baselines. Failed attempts remain available
when retried. `--fixture`, `--kind` and `--length` accept comma-separated filters.
Use `baselines --screening` to prepare only the six screening fixtures.

`run --screening` runs all nine listed OpenCode free models once at their default
variant on five tasks, plus a visual task on the four image-capable models: 49 runs.
Runs are sequential. Each gets independent input copies under `work/runs/`; failing
workspaces and agent logs remain for inspection. Ordinary `run` also accepts `--task`,
`--level`, `--effort` and `--trials`. `--timeout` overrides the 600-second deadline
(1,200 seconds for level 1 on L/XL fixtures); the deadline uses the engine's Stop handle.

Results append to `results/YYYY-MM-DD.jsonl`. Existing cells are skipped; `--rerun`
starts a new generation without overwriting prior evidence. `ladder` requires two
passes, with a third trial after a split, before recommending a rung. Paid models
are refused unless the caller explicitly supplies `--allow-paid`. Custom agent
commands are refused because they can override the requested model.

After fixing a check, use `recheck --failure-class check` to judge retained run artifacts
without calling a model. Rechecks append linked rows and preserve the original run's
time, tokens and cost. Reports label these as offline rechecks, count each original
attempt once, and do not treat a recheck as another reliability trial.

Private task expectations, logs, answers and frozen check inputs live under
`results/artifacts/<run-id>/`, outside the agent workspace. Tasks start from a state
that fails their check, so doing nothing cannot pass. A run that accessed private
check expectations is marked invalid and needs a fresh live trial; recheck cannot
make that run valid.

`report` prints per-kind, length and task summaries, the screening table and failure
evidence. `report --out FILE` saves it. Pass `--root DIR` or `--manifest FILE` before
the subcommand to use another fixture collection. A one-trial screening pass is not
a reliability recommendation. Visual checks disclose their detector-derived face
evidence; missing evidence is a check failure, not proof that captions are safe.

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
