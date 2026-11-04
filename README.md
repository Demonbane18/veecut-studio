# VeeCut Studio

VeeCut Studio is an open-source AI video editor written in Rust. You put a recording in a folder and ask a coding agent to "edit this". The agent runs the `veecut` command, which cuts, frames, captions and mixes the video with a recipe for that kind of video and renders a draft. You give notes in plain words, in English, Tagalog or Taglish, and get the next draft. A desktop editor opens the same project when you want to change something by hand.

## What it does

- Coding agents and a desktop editor work on one timeline. The command line, the MCP server, the editor and the agents all change a project through the same Editor API, so every change can be undone.
- Built-in recipes for ten kinds of video (table below) make the editing decisions by rule: where to cut, when to punch in, where captions go, how loud the music sits. Most rules come from Herk Edit Kit's editing playbook; the documentary edit follows NextPhase Editor V1's published measurements.
- It is built for free models. Because the recipes do the heavy work, a small free model such as GLM 5.3 Flash Free in OpenCode only has to run a command, read the result and pass on your notes.
- Notes you repeat become rules in `my-rules.md`, so you do not have to say them twice.
- A skills catalog synced from [awesome-claude-video-skills](https://github.com/zhuyansen/awesome-claude-video-skills) lists community video skills by kind of video. It refreshes itself every 24 hours, and installed skills graded SAFE update with it. `veecut skills sync` and `veecut skills update` do the same by hand.
- Everything runs on your computer. Rust draws the picture (cuts, zooms, reframing, color, text, captions, transitions); ffmpeg only decodes, encodes and filters audio, so any ffmpeg build works, including ones without libass or drawtext.
- Text and captions stay readable: when the footage behind them would make them hard to read, VeeCut puts a dark or light scrim behind them. Your colors are never changed.

## Requirements

- Rust 1.95 or newer
- ffmpeg and ffprobe on your PATH
- cmake and a C++ compiler, for local transcription with whisper.cpp

VeeCut is developed and tested on macOS (Apple Silicon). Continuous integration builds and tests it on macOS and Linux; Windows builds are not tested yet.

## Install from source

There are no prebuilt binaries yet. From a clone:

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

`veecut init` copies the VeeCut skill into `.agents/skills/veecut/` and `.claude/skills/veecut/`, writes `AGENTS.md`, `CLAUDE.md` and `GEMINI.md` so agents use it, and registers `veecut mcp` as an MCP server in `opencode.json`. Workflow and reference paths are relative to the loaded `SKILL.md` directory, never the workspace root.

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
Reported first-request input counts the first model request's input, including cache reads
and writes once. It is separate from cumulative run tokens and stays unknown when unreported.

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
agents use their CLI defaults after the per-run context restrictions below. Existing custom
command overrides take precedence and run as written.

VeeCut-started runs keep workspace instructions and skills while suppressing unrelated
user context where each CLI supports it. Existing OAuth sign-ins stay intact; VeeCut does
not rewrite global agent settings. Claude and Codex skip user settings/config while explicitly
registering VeeCut MCP; Codex omits the global skill catalog. OpenCode uses a workspace-local
config location with workspace skills and VeeCut MCP, leaving sign-in data untouched.
User-configured model/provider defaults may not apply. Codex and Gemini retain some global
instructions their flags cannot disable. See [per-run context limits](docs/architecture.md#per-run-agent-context)
for details. These restrictions do not change an agent you start yourself.

Claude's `ultracode` effort enables its workflow planner, uses `max` effort and allows
`Workflow` in headless mode. This is the most expensive choice, roughly five times the
tokens of a normal run, and requires a model that supports `xhigh`.

Ctrl+C stops the agent and its inherited helpers. No project change is rolled back; saved
edits remain undoable.

When OpenCode reports an exhausted free allowance or an unavailable model, VeeCut stops
the run and shows the provider problem. Free access can take a day or more to return.
Errors that OpenCode writes only to its private log cannot trigger this early stop;
the evaluation harness still enforces its deadline.

## Cutting a phrase

```sh
veecut transcript-search "the words to remove" -p SLUG --json
veecut cut-phrase "the words to remove" -p SLUG
```

Search matches whole spoken words, ignoring case and punctuation. It reports source and
timeline start/end times to millisecond precision; `veecut transcript` also reports sentence
ranges. If wording occurs more than once, choose the 1-based `--occurrence N`.
Without a selection, an ambiguous cut refuses to change the project.
Cuts use the transcript's word timings and can be undone with `veecut undo`.

CLI `-p` and MCP `project` accept a slug, project folder or `project.json` path. Omit the
selector to use the most recently changed project. CLI `-p` works before or after the
subcommand; an existing project path can find its workspace from outside it. An explicit
`--workspace` takes precedence, and MCP stays in its server workspace. Paths cannot escape
the selected workspace. MCP exposes `transcript_search` and `cut_phrase` with `phrase`,
plus optional `occurrence` for the cut.

## The desktop editor

`veecut studio` opens the editor on the workspace; `veecut studio -p PROJECT` opens one project. The welcome screen lists your projects and inbox videos. Under New edit, pick a name, a kind of video, a canvas and a source file, then choose "Auto-edit with the recipe" (runs the recipe and opens the result, without rendering) or "Create empty edit".

With a project open:

- The top bar has the canvas preset, Undo, Redo, Export, and a box to ask a coding agent for a change.
- The left panel has the Media, Footage, Sources, Text, Captions, Look & Audio, Skills and Agent tabs.
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
- With the Documentary kind, New edit adds "Find footage online (free, licence-tracked)" and "Choose B-roll files...".
- The Footage tab searches the free footage providers with thumbnails. Filter by Any, Videos or Images and tick the providers (Pexels only with `PEXELS_API_KEY`). Each result shows its licence, author, length and why it is flagged, if it is. Add downloads it into the project; Add at playhead also places it on the B-roll track (2 s for a picture, up to 3 s for a video). Downloads run in the background and can be undone.
- The Sources tab lists every found file with its provider, licence, author and a button to open its page. Approve (I checked the rights) clears a flag; Flag with a reason adds one. Credit lines for the video description sit below with Copy credits.
- Exporting at final quality lists flagged sources nobody approved and stays disabled until each is approved or no longer used.
- In the inspector, clips have a Grade section (Look, Halftone, Vignette and Recolor) and the new transitions: Negative, Glitch, Tiles, Focus and Whip. On overlay tracks the transition is how the clip enters; picking a kind fills in its usual length. A cut-out clip shows its Mask with an Outline slider and Remove mask. A selected video clip on the main track offers "Cut out the speaker": a length from the playhead, an outline and an optional mega word.
- Text has the Keyword, Chapter, Shock, Black card and Mega presets; captions have the Reveal mode and an "Ink on light pictures" switch.

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

These options change what the hook captures. They take effect only with `VEECUT_STUDIO_SHOTS` set:

| Variable | Effect |
| --- | --- |
| `VEECUT_STUDIO_TAB=media\|sources\|footage` | Opens that tab, saves only `00-paused.png` and quits |
| `VEECUT_STUDIO_SELECT=<item id>` | Selects that item first, so the inspector shows it |
| `VEECUT_STUDIO_AT=<seconds>` | Moves the playhead there before the first screenshot |
| `VEECUT_STUDIO_QUERY=<words>` | The Footage tab's search, run for real (default `moon`); the screenshot waits for the results and thumbnails |

## Ten kinds of video

| Kind | For | What the recipe does |
| --- | --- | --- |
| `talking-head-short` | Reels, TikTok, Shorts | 9:16, cuts pauses and retakes, face crop, 1.15x punch-ins so no shot runs past 2.8 s, hook text, captions off the face, B-roll from `--broll`, music and sound effects from your style |
| `talking-head-long` | YouTube, Facebook feed | 16:9, natural cuts, 1.10x punch-ins about every 7 s, captions off the face, B-roll from `--broll` |
| `tutorial` | Screen recordings | 16:9, natural cuts, word-free silent stretches of 2 s or more played 2 to 8 times faster with a speed label (narration always at normal speed), subtitles low in the frame |
| `podcast` | Podcasts, interviews | One camera: gentle punch-ins. Two to four cameras: synced by sound, cuts to whoever speaks |
| `clips` | Long video to shorts | Finds the strongest 20 to 60 s moments and makes one vertical draft per moment |
| `vlog` | Trips, days, events | Footage cut to the music's beat, each shot used once, photos panned and zoomed |
| `explainer` | Faceless narration | Ordered images/video per sentence, hold/push/glide motion, heading cards, ducked music; big centered captions without visuals |
| `promo` | Launches, ads | Lines of copy slammed on the beat over footage, ending on a call to action |
| `music-video` | Songs, lyric videos | Footage cut on the beat, lyrics from LRC or transcribed from the song |
| `documentary` | A talking head as a fast documentary | 16:9, found pictures on most lines, re-coloured, with your face between runs, cut effects, word-by-word captions, a click on every change and a music bed cut under the punchline |

## The faceless explainer

Record your own short narration and arrange owned or licensed visuals in story order:

```sh
veecut edit --kind explainer --voice inbox/narration.m4a --broll inbox/chart.png inbox/demo.mp4 --motion glide --music my-sounds/music/bed.mp3
veecut edit inbox/narration.mp4 --kind explainer --broll inbox/chart.png inbox/demo.mp4 --motion hold --no-render
```

Without `--voice`, an audio source supplies narration, or the first video supplies its sound when no audio source is given. `--voice` always takes precedence. Visuals repeat to cover narration; short B-roll repeats within its sentence without shifting later shots. Visual video audio stays muted. Music follows the style's sound setting.

`--motion hold` leaves framing still, `push` adds a smooth 1.0x to 1.12x zoom, and `glide` adds restrained zoom and alternating focus movement. These options apply to images and videos and are valid only for `explainer`. Omission preserves the original recipe motion. In Studio's New edit, choose Explainer, narration audio or video, ordered B-roll/images, optional music and Recipe default, Hold, Push or Glide.

Write an original factual script and plan its visual beats before recording. Inspect each beat with `veecut frames -p SLUG` and `veecut frame -p SLUG --at T`, then render with `veecut render -p SLUG` and watch the whole draft. After approval, use `veecut render -p SLUG --final`. Original media stays unchanged.

This workflow uses VeeCut's shared Rust preview/export compositor; it installs no external renderer or generation model. Own recordings and local transcription need no paid generation API, though transcription setup may download a model and configured Groq uploads audio. Generated characters and depth/parallax remain external: import finished, rights-cleared clips normally. See [CREDITS.md](CREDITS.md#faceless-and-animated-shorts-source-review) for the reviewed packs' licences and limits.

## The documentary edit

`veecut edit --kind documentary` turns a talk to camera into a fast documentary in the style of NextPhase Editor V1. Found pictures cover most lines, each re-coloured (teal, mono, warm, cold or natural) and slowly pushed in, while your own camera stays as it is. Your face comes back between runs of pictures, about 30 percent of the time. Between pictures sit cut effects: a one-frame white box, a negative, a datamosh, a 3x3 tile reveal, a rack focus or a whip. Small captions appear word by word; stressed words, numbers and words like "nothing" get display words. Every picture change gets a click, and the music bed runs under your voice and cuts out under the punchline. The pacing, levels and type sizes are Editor V1's published measurements; VeeCut copies none of its code, text or sounds (see [CREDITS.md](CREDITS.md)).

```sh
veecut edit inbox/take.mp4 --kind documentary --find --music my-sounds/music/bed.mp3
veecut edit inbox/take.mp4 --kind documentary --broll inbox/office.mp4 inbox/chart.png
```

`--broll` brings your own pictures, which go first. `--find` searches free footage for the rest of the lines. Each search pairs a line's own name or concrete word with the video's subject (the things you say more than once), so "the test" becomes "sls test" in a video about the SLS. Results only go in when their title matches the search, and titles about abuse, violence or sex are left out. Found footage is still matched by words, so look at every picture before you publish.

### Found footage and rights

`veecut find "IDEA"` searches the NASA Image and Video Library, Wikimedia Commons, the Internet Archive and Openverse, plus Pexels when `PEXELS_API_KEY` is in `.env`. It numbers the results and saves a contact sheet to `.veecut/find/last.png`. `veecut fetch N` downloads number N into `footage/` (at most 150 MB for a video, 40 MB for a picture, preferring medium-size renditions), checks that it really is a picture or a video, and adds it to the project with its source, licence and credit line. `--find` does the same for each run of pictures, up to 12 files per edit, and skips anything flagged. A fetched file keeps its licence record (`<file>.source.json`) wherever you use it later, for example as `--broll` in another edit.

Each file keeps its own licence, recorded in the project. `veecut sources` shows the rights table; `veecut sources --credits` prints credit lines for the video description.

| Licence | VeeCut |
| --- | --- |
| Public domain, public domain mark, CC0 | Allowed |
| CC BY, CC BY-SA | Allowed; the credit line says the work was re-coloured, cropped and cut |
| Pexels License | Allowed; credited as "Photo by NAME on Pexels" |
| Non-commercial (NC) | Flagged: no commercial use |
| No derivatives (ND) | Flagged: VeeCut always re-colours, crops and cuts |
| GFDL only | Flagged: it needs the whole licence text shipped with the video |
| Fair use, non-free, unknown or missing | Flagged: a person must decide |
| NASA files whose credit names someone else or that mention copyright | Flagged: only NASA's own media is public domain |

A final render refuses flagged files you have not approved; drafts still render. Check the file's page and licence, then run `veecut sources --approve ASSET` yourself in a terminal or press Approve in Studio's Sources tab. Approval is yours alone: `--approve` needs an interactive terminal, agent `ops` cannot approve or unflag a source, and the MCP `sources` tool has no approve option.

### Cut-out and the mega word

`veecut cutout --at T --dur D --mega WORD` cuts you out of your own camera inside one main-track clip and puts a huge red word behind your head, between the picture and your cut-out. `--world FILE` places a picture behind you; `--outline W` sets the white sticker edge (default 0.006 of the frame height). The cut-out runs MODNet (Apache-2.0) in Rust with tract. The first cut-out downloads the model once, about 26 MB, to `~/.veecut/models`, and checks it against a pinned SHA-256; masks are cached in the project. Once the model is there, the documentary recipe adds mega words by itself. `veecut doctor` says whether it is downloaded.

### The sound kit

VeeCut makes its own sound effects in code: four clicks, a tick, a pane click, a swish, a glitch, a riser, a sub tone, a sub boom, an impact, a hit and an outro, written as WAV files to `.veecut/sfx/v1/`. `veecut sfx` lists them with their lengths and roles. The documentary edit places each hit 8 ms after its picture change at a fixed level under your voice (clicks 4.5 dB under). The other recipes play your files from `my-sounds/sfx/` when text lands, and the kit's tick when that folder is empty.

### The words check

`veecut qa outbox/<slug>-v1.mp4 --words` transcribes the render again and compares it with your transcript. It lists words that went missing or came back different, and says whether a cut sits within 0.25 s or a sound or music clip starts within 0.15 s of each. Run it before a final: a click or the bed can mask a short word. It runs locally unless your config picked Groq.

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
cargo run -p veecut-eval -- ladder --fixture ths-s-mark-pond-31s --level 2 \
  --rungs haiku@low,gpt-5.6-luna@low --allow-paid
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

`ladder --rungs` selects a comma-separated list before any probe or trial: `free` (or
`opencode`), `claude`, `codex`, or a model with optional `@EFFORT`, such as `haiku@low`.
A model alone selects all its ladder efforts; `@default` selects no explicit effort.
Paid selections still require `--allow-paid`.

Before a free model's first trial in a batch, the harness makes one small real availability
request, bounded by `--probe-timeout` (20 seconds by default). An unavailable model is
recorded under `results/deferred/`, not as a failed trial, and does not affect reliability
or result totals. An exhausted free allowance defers the remaining selected free rungs
without more probes. A deferred free rung blocks paid escalation for that task; a paid-only
`--rungs` selection skips free probes. Run the same command later to retry; `report` lists
open deferrals separately.

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
- The network is used to download whisper models and the cut-out model on first use, to sync the skills catalog from GitHub and to install skills you choose.
- Footage search and downloads (`veecut find`, `veecut fetch`, `veecut edit --find` and Studio's Footage tab) send your search words to the providers searched and download the chosen files. Nothing is uploaded.
- Coding agents send your prompts and what they read (such as transcripts and command output) to their model provider.
- `veecut init` keeps `.env`, `inbox/`, `outbox/`, `my-sounds/`, `footage/` and `.veecut/find/` out of git with a `.gitignore`.

## More

- [docs/architecture.md](docs/architecture.md): crates, the project model, the Editor API and the full command line.
- [CREDITS.md](CREDITS.md): the projects VeeCut learned from, bundled fonts, the cut-out model, libraries and the footage providers' terms.

## License

MIT. See [LICENSE](LICENSE).
