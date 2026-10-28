# VeeCut Studio architecture

VeeCut Studio is an AI video editor written in Rust. Coding agents (OpenCode with a free model,
Claude Code, Codex and others) and a desktop editor work on the same project file. Built-in
recipes do the heavy lifting for each kind of video, so even a small free model only has to make
small decisions.

## Principles

- **Rust does the picture work.** ffmpeg only decodes, encodes and filters audio. Cutting,
  zooms, reframing, color, text, captions and transitions are drawn in Rust, so any stock ffmpeg
  works, including builds without libass or drawtext (Homebrew's current build has neither).
- **Preview equals export.** The desktop preview and the final render use the same compositor
  and the same graphics code.
- **One project file, one Editor API.** `project.json` plus the ops in `ops.rs`. The CLI, the
  MCP server, the desktop editor and agents all change projects through the same ops.
- **Originals are never modified.** Assets are referenced by path; renders go to `outbox/`.
- **Nothing paid or uploaded without asking.** Groq transcription uses the user's free key; the
  default is local whisper.cpp.

## Crates

| Crate | Kind | Owns |
| --- | --- | --- |
| `crates/veecut-engine` | library | model, ops, media I/O, analysis, transcription, graphics, compositor, recipes, skills catalog, workspace |
| `crates/veecut-studio` | library | the egui desktop editor; entry point `veecut_studio::run` |
| `crates/veecut` | binary | the `veecut` command: CLI, MCP server (`veecut mcp`), `veecut studio` |
| `crates/veecut-eval` | unpublished binary | fixture baselines, agent trials, escalation ladders and result reports |

## The user's workspace

`veecut init` turns any folder into a workspace:

```
my-videos/
  .veecut/workspace.toml   marks the workspace root
  inbox/                   raw recordings (never modified)
  outbox/                  drafts and finals: <slug>-v1.mp4, <slug>-final.mp4
  projects/<slug>/         project.json, transcript.json, .history/ (undo snapshots), .cache/
  my-style.toml            the user's style choices
  my-rules.md              lasting rules learned from the user's notes
  my-sounds/music/, my-sounds/sfx/
  .agents/skills/veecut/   the built-in skill (copied from the binary) + installed catalog skills
  .claude/skills/          the same skills for Claude Code
  AGENTS.md, CLAUDE.md     tell every agent to use the veecut skill
  opencode.json            registers `veecut mcp` with OpenCode
  skills.lock.json         installed catalog skills, pinned to commits
  .env                     GROQ_API_KEY, GITHUB_TOKEN (optional, never committed)
```

Global state lives in `~/.veecut/` (override with `VEECUT_HOME`): `config.toml`,
`catalog.json` and `models/` (whisper models).

## Project model (`project.rs`, done)

- Times are seconds (f64) on the timeline; positions and sizes are fractions of the canvas.
- `tracks[0]` is the magnetic main track: clips play back to back, `normalize()` recomputes
  their starts, crossfades overlap the previous clip.
- Other tracks keep absolute starts. Visual tracks draw in order (later on top).
- Item bodies: `Clip` (asset piece with speed, volume, fit, focus, zooms, frame, fades,
  transition, Ken Burns, duck), `Text` (preset, style overrides, position, animations;
  `*word*` = accent color) and `Caption` (words with times relative to the item start).
- `Project::source_to_timeline(asset, t)` maps a source time through the cuts.
- `faces` stores timestamped face boxes in source coordinates, keyed by asset id. Recipes set
  it through ops. Caption layout maps the boxes through the same crop and zoom as the picture.
  Old projects load without face data; rerun Auto-edit to analyze them.
- Studio stores Auto-edit review items in `markers`, with `at = -1` and an
  `Auto-edit review: ` prefix. These unpinned notes survive ripple cuts. Dismiss sets `done`
  through ops; pending and dismissed states survive reopening, with in-session editor Undo.

## Editor API (`ops.rs`, done)

`cut_range`, `split`, `trim`, `move`, `delete`, `duplicate`, `add_clip`, `add_text`, `add_zoom`,
`patch` (JSON merge patch on an item), `set` (JSON pointer on the project), `add_marker`,
`add_track`. `apply` and `apply_all` are atomic and validate the result. The JSON schema comes
from `schemars::schema_for!(Op)`.

## Module contracts

Signatures other modules depend on. Owners may add more, but must not change these without
updating every caller.

### `media` (done)

`ffmpeg()`, `ffprobe()`, `ffmpeg_command()`, `run(&mut Command) -> Result<Vec<u8>>`,
`probe(&Path) -> Result<MediaInfo>`, `pcm_mono(&Path, sample_rate, Option<(start, end)>) -> Result<Vec<f32>>`,
`frame_rgba(&Path, t, max_width) -> Result<image::RgbaImage>`.

### `render`

```rust
pub enum Quality { Draft, Final }
pub struct RenderOptions { pub width: u32, pub height: u32, pub range: Option<(f64, f64)>, pub quality: Quality, pub audio: bool }
impl RenderOptions { pub fn for_canvas(canvas: &Canvas, quality: Quality) -> Self }
pub struct Progress { pub stage: &'static str, pub done: f64, pub total: f64 }
pub struct ExportReport { pub path: PathBuf, pub duration: f64, pub width: u32, pub height: u32, pub frames: u64, pub seconds_taken: f64 }

/// Renders the project to an MP4 (H.264 + AAC). Returning false from `progress` cancels.
pub fn export(project: &Project, project_dir: &Path, out: &Path, opts: &RenderOptions,
              progress: &mut dyn FnMut(&Progress) -> bool) -> Result<ExportReport>;
/// One fully composited frame (video, color, graphics, captions) at timeline time `t`.
pub fn render_frame(project: &Project, project_dir: &Path, t: f64, width: u32, height: u32) -> Result<tiny_skia::Pixmap>;
/// Composited frames from `from` onward at the canvas frame rate, for playback.
pub struct FrameStream;
impl FrameStream { pub fn new(project: &Project, project_dir: &Path, from: f64, width: u32, height: u32) -> Result<Self>; }
impl Iterator for FrameStream { type Item = Result<(f64, tiny_skia::Pixmap)>; }
/// The project's mixed audio as 48 kHz stereo 16-bit WAV.
pub fn mix_audio(project: &Project, project_dir: &Path, out_wav: &Path, range: Option<(f64, f64)>, normalize: bool) -> Result<()>;
/// Maps a normalized source rectangle through the compositor's crop, focus and zoom.
pub fn canvas_rect(clip: &Clip, src: (u32, u32), canvas: (u32, u32), local: f64, duration: f64, rect: Rect) -> Rect;
```

### `graphics`

```rust
pub struct Graphics;
impl Graphics {
    /// Loads the bundled fonts (OFL, in crates/veecut-engine/assets/fonts) and system fonts.
    pub fn new() -> Self;
    /// Draws every visible text and caption item at timeline time `t`. The pixmap has the
    /// canvas aspect ratio at any size; drawing scales to it.
    pub fn draw(&mut self, project: &Project, t: f64, pixmap: &mut tiny_skia::PixmapMut);
    pub fn font_families(&self) -> Vec<String>;
    /// Caption page bounds and placement decisions, shared with draw. Word-pop bounds include
    /// its maximum animated size. Hidden pages report the box that could not be placed.
    pub fn caption_placement_at(&mut self, project: &Project, t: f64) -> Option<CaptionPlacement>;
    pub fn caption_placements(&mut self, project: &Project) -> Vec<CaptionPlacement>;
}
/// Area clear of platform buttons and captions for this canvas, normalized.
pub fn safe_area(canvas: &Canvas) -> Rect;
/// Totals and at most `limit` examples: safe-area clamps, moved/shrunk pages, hidden pages,
/// unchecked pages and detection gaps.
pub fn caption_report(graphics: &mut Graphics, project: &Project, limit: usize) -> CaptionReport;
```

Captions use one placement per page. The requested position is tried first, then a lower slot
at smaller sizes, then an upper slot clear of text. Pages with no clear slot are hidden and
reported for review. Missing face detections are reported as unchecked, never as clear.


### `transcribe`

```rust
pub enum Provider { Groq, Local }
pub struct TranscribeOptions { pub provider: Option<Provider>, pub language: Option<String>, pub model: Option<String>, pub cpu_only: bool }
/// Auto provider: Groq when GROQ_API_KEY is set, else local whisper.cpp (model downloaded to ~/.veecut/models on first use).
pub fn transcribe(media: &Path, asset_id: &str, opts: &TranscribeOptions, progress: &mut dyn FnMut(&str)) -> Result<Transcript>;
pub fn status() -> Vec<String>; // one line per provider: ready or what is missing
```

### `analyze`

```rust
pub struct CutSettings { pub min_silence: f64, pub pad: f64, pub remove_fillers: bool, pub remove_retakes: bool }
pub enum CutReason { Silence, Filler, Retake }
pub struct Removal { pub start: f64, pub end: f64, pub reason: CutReason, pub text: String }
pub struct CutPlan { pub keep: Vec<(f64, f64)>, pub removed: Vec<Removal> } // source seconds
pub fn plan_cuts(transcript: &Transcript, pcm: &[f32], sample_rate: u32, settings: &CutSettings) -> CutPlan;
pub fn silences(pcm: &[f32], sample_rate: u32, threshold_db: f32, min_len: f64) -> Vec<(f64, f64)>;
pub struct Beats { pub tempo: f64, pub beats: Vec<f64>, pub downbeats: Vec<f64>, pub onsets: Vec<f64> }
pub fn beats(pcm: &[f32], sample_rate: u32) -> Beats;
/// Index of the loudest speaker per window, for multi-mic podcasts: (start, speaker).
pub fn speaker_turns(tracks: &[Vec<f32>], sample_rate: u32, min_shot: f64) -> Vec<(f64, usize)>;
/// Largest frontal face about every half second (at most 1,200 samples), in source coordinates.
pub fn face_track(path: &Path, range: (f64, f64)) -> Result<Vec<FaceSample>>;
/// Median center of the detected boxes, used for crop focus.
pub fn focus(faces: &[FaceSample]) -> Option<Point>;
pub struct Highlight { pub start: f64, pub end: f64, pub score: f32, pub title: String, pub reason: String }
pub fn highlights(transcript: &Transcript, min_len: f64, max_len: f64, count: usize) -> Vec<Highlight>;
/// Words worth emphasizing in captions: numbers, money, names, strong words.
pub fn emphasis(words: &[TranscriptWord]) -> Vec<bool>;
```

### `captions`

```rust
/// Caption pages for every transcript word kept on the main track, grouped by the project's caption style.
pub fn build(project: &Project, transcript: &Transcript) -> Vec<Item>;
/// Replaces the items on the first captions track with `build`.
pub fn apply(project: &mut Project, transcript: &Transcript) -> Result<usize>;
pub fn to_srt(project: &Project) -> String;
pub fn to_vtt(project: &Project) -> String;
```

### `workspace`, `style`, `config`, `agent`

```rust
pub struct Workspace { pub root: PathBuf }
impl Workspace {
    pub fn init(root: &Path) -> Result<Workspace>;      // idempotent; refreshes the built-in skill files
    pub fn open(start: &Path) -> Result<Workspace>;     // walks up to the folder holding .veecut/
    pub fn inbox(&self) -> PathBuf; pub fn outbox(&self) -> PathBuf; pub fn projects(&self) -> PathBuf;
    pub fn create_project(&self, name: &str, kind: VideoKind, canvas: Canvas) -> Result<(PathBuf, Project)>;
    pub fn add_asset(&self, project: &mut Project, project_dir: &Path, file: &Path, role: AssetRole) -> Result<String>;
    pub fn save_project(&self, project_dir: &Path, project: &Project) -> Result<()>; // snapshots the previous version for undo
    pub fn undo(&self, project_dir: &Path) -> Result<Project>;
    pub fn list_projects(&self) -> Result<Vec<(String, PathBuf)>>;
    pub fn newest_inbox_video(&self) -> Option<PathBuf>;
    pub fn next_draft_path(&self, slug: &str) -> PathBuf;   // outbox/<slug>-v<n>.mp4
}
// style.rs
pub struct Style { .. }  // my-style.toml; Style::load(&Path), save, apply_to(&mut Project)
pub fn add_rule(path: &Path, rule: &str, user_words: &str) -> Result<()>;
pub fn rules(path: &Path) -> Result<Vec<String>>;
// config.rs
pub fn home() -> PathBuf;   // ~/.veecut or VEECUT_HOME
pub struct Config { .. }    // Config::load(), save(); skills, agent commands and selections, transcription defaults
// Config::agent_selection(kind) resolves per-agent defaults; set_agent_selection(kind, selection) changes the saved choice.
pub fn load_dotenv(root: &Path); // sets variables from <root>/.env that are not already set
// agent.rs
pub enum AgentKind { OpenCode, Claude, Codex, Gemini }
pub enum Readiness { Ready, NotInstalled, SignedOut, CheckFailed, Unverified }
pub struct AgentStatus { pub kind: AgentKind, pub readiness: Readiness, pub detail: String, pub fix: Option<String>, pub command_override: bool }
pub fn detect() -> Vec<AgentStatus>; // bounded, read-only sign-in/config probes
pub struct Stop; // Clone + Default; one per run. stop() ends the process group/job.
pub enum Outcome { Finished(std::process::ExitStatus), Stopped }
pub struct Selection { pub model: Option<String>, pub effort: Option<String> }
pub struct Usage { pub input_tokens: u64, pub output_tokens: u64, pub cached_tokens: u64, pub cost_usd: Option<f64>, pub turns: Option<u64> }
pub enum Output { Answer(String), Log(String), Usage(Usage) }
pub fn run(ws: &Workspace, kind: AgentKind, prompt: &str, selection: &Selection, stop: &Stop, on_output: &mut dyn FnMut(Output)) -> Result<Outcome>;
pub fn studio_prompt(project: &Path, note: &str) -> String;
pub fn known_models(kind: AgentKind) -> Vec<ModelChoice>;
pub fn models(kind: AgentKind) -> Result<Vec<ModelChoice>>; // adds bounded `opencode models` discovery
pub fn efforts(kind: AgentKind, model: Option<&str>) -> Vec<EffortChoice>;
```

Agents run in a separate Unix process group or a Windows Job Object. Stop and CLI Ctrl+C
terminate that group/job, reap the agent and drain output before reporting Stopped. Studio
also stops its agent on exit. Unix helpers that deliberately create a new session/group
escape group termination; ordinary inherited hooks and MCP children remain in the group.
Studio and the evaluation harness share `agent::studio_prompt`: ops, an optional cached frame,
a short reply, and no render or open unless a video is requested.
Codex, OpenCode and Claude structured output separates answers, tool logs and cumulative usage.
Input tokens exclude cached tokens; `Usage::total_tokens` adds input, output and cached once.
Missing usage or cost stays unknown. Studio keeps the latest note, reply, usage and result
outside the folded log.
Claude and Codex readiness includes their read-only login checks; failures retain the fix.
Other providers and custom command overrides disclose what was not checked. Refresh reruns
the checks without changing agent settings.
Model and effort choices live in `[agent.selection.<agent>]` in `config.toml`. Callers resolve
the config selection and apply per-run overrides before calling `run`; an absent effort in
`Selection` means the agent's default. OpenCode defaults to `opencode/glm-5.3-flash-free`;
other agents use their CLI's default model. A custom command vector takes precedence and
is not given extra model, effort or output-format arguments.

Claude `ultracode` maps to `--effort max --settings '{"ultracode":true}'` and adds `Workflow`
to the allowed tools. The picker labels its roughly fivefold token cost. Gemini has no
effort flag. Model lists are suggestions; custom model IDs remain accepted.


### `skills`

```rust
pub struct Catalog { pub source_commit: String, pub fetched_at: DateTime<Utc>, pub categories: Vec<Category>, pub entries: Vec<Entry> }
pub struct Entry { pub repo: String, pub url: String, pub stars: u32, pub description: String, pub security: String, pub categories: Vec<String>, pub tested: bool }
pub fn sync() -> Result<SyncReport>;                       // fetch the awesome list, write ~/.veecut/catalog.json
pub fn load_catalog() -> Result<Option<Catalog>>;
pub fn auto_sync_if_stale(config: &Config) -> Option<Result<SyncReport>>;
pub fn search<'a>(catalog: &'a Catalog, query: &str, category: Option<&str>) -> Vec<&'a Entry>;
pub fn install(ws: &Workspace, repo: &str) -> Result<InstallReport>;
pub fn update(ws: &Workspace, repo: Option<&str>) -> Result<Vec<UpdateReport>>;
pub fn remove(ws: &Workspace, repo: &str) -> Result<()>;
pub fn installed(ws: &Workspace) -> Result<Lock>;
/// Best catalog entries for a kind of video: SAFE, tested first, then stars. Data-driven, so it
/// follows the catalog as it updates.
pub fn recommended<'a>(catalog: &'a Catalog, kind: VideoKind, n: usize) -> Vec<&'a Entry>;
```

### `recipes`, `qa`

```rust
pub struct EditRequest { pub kind: VideoKind, pub sources: Vec<PathBuf>, pub broll: Vec<PathBuf>, pub music: Option<PathBuf>,
                         pub voice: Option<PathBuf>, pub copy: Option<String>, pub canvas: Option<Canvas>, pub name: Option<String>,
                         pub transcribe: Option<TranscribeOptions>, /* other recipe options */ }
pub fn edit(ws: &Workspace, req: &EditRequest, progress: &mut dyn FnMut(&str)) -> Result<EditReport>;
pub fn check(path: &Path) -> Result<QaReport>; // qa.rs: duration, size, fps, loudness, true peak, black frames, problems
```

An explicit recipe transcription override bypasses global provider settings. Evaluation
baselines use local transcription with `cpu_only = true`, so configured Groq credentials
cannot cause an upload and whisper.cpp does not use GPU acceleration.
The harness also sets `VEECUT_LOCAL_CPU_ONLY=1` before starting workers. The transcription
entry point honors this process-scoped override in inherited CLI and MCP children, even
when their global config selects Groq.

## Command line (`veecut`)

Commands that work on a project take `-p/--project P` (a slug, a project folder or a
`project.json` path); without it they use the most recently changed project in the workspace.
Every command accepts `--json` for machine-readable output and `--workspace DIR`.

```
veecut init [DIR]                     set up a workspace (folders, built-in skill, agent config)
veecut doctor                         check ffmpeg, transcription, agents and the skills catalog
veecut edit [FILE...] [--kind K] [--aspect 9:16|16:9|4:5|1:1] [--broll FILE...] [--music FILE]
            [--voice FILE] [--copy TEXT|FILE] [--name NAME] [--hook TEXT] [--language CODE]
            [--clips N] [--no-render]
                                      run the recipe for kind K (default talking-head-short) on the
                                      files, or the newest inbox video; prints projects, drafts,
                                      what it did and what needs review
veecut status                         kind, length, tracks and items, drafts, open notes
veecut add FILE... [--role source|broll|music|sfx|voice|image|logo]
                                      bring files into the project as assets; prints ids for add_clip
veecut transcript [--srt|--vtt|--raw]   sentences with timeline times; captions as SRT/VTT; raw words
veecut ops JSON|--file F|-            apply Editor API ops atomically; layout changes also report actual caption bounds
veecut schema                         JSON schema of the ops
veecut undo / veecut redo
veecut render [--final] [--aspect A] [--range A-B] [--out FILE]
                                      draft to outbox/<slug>-v<n>.mp4, final to outbox/<slug>-final.mp4
veecut frame --at T [--width W] [--out FILE]      composited PNG plus caption placement at T
veecut frames [--every S] [--out FILE]            contact sheet of frames
                                      check PNGs default to project .cache; inbox/outbox destinations are refused,
                                      including symlink aliases, before rendering
veecut captions [--rebuild] [--mode word_pop|highlight|pills|subtitle|off]
veecut note "NOTE" [--at T] [--rule]             pin a note; --rule also saves it to my-rules.md
veecut rules [add "RULE" --words "USER WORDS"]   list or add lasting rules
veecut style [show|set KEY VALUE]
veecut skills [sync|list|search Q|show REPO|install REPO [--force]|update [REPO]|remove REPO|recommend [--kind K]]
veecut qa FILE
veecut agent "PROMPT" [--with opencode|claude|codex|gemini] [--model M] [--effort E]
veecut agent --models [--with AGENT]    built-in model and effort choices; OpenCode also queries its installed CLI
veecut mcp                            MCP server on stdio (tools below)
veecut studio                         open the desktop editor
veecut fonts                          list caption and text fonts
```

MCP tools: `status`, `add`, `edit`, `ops`, `schema`, `render`, `frame`, `frames`, `transcript`, `note`,
`undo` (with `redo: true` to redo), `skills_search`, `skills_install`. Each takes the same
arguments as the matching command; `frame` and `frames` also return the PNG as an image.

## Evaluation harness (`veecut-eval`)

The unpublished harness depends on the engine, not the CLI or Studio. Its commands are
`baselines`, `run`, `ladder` and `report`; root and manifest overrides precede the command.
Filters select fixture IDs, kinds and lengths, with task and level filters for agent runs.
The fixture manifest remains outside git. No harness operation writes into fixture media.

Baselines run the matching engine recipe with rendering disabled and local CPU transcription.
Each has its own workspace, linked inbox, saved report, log and completion record under
`~/veecut-eval/work/baselines/<id>`. Completed baselines are reusable; incomplete attempts
remain available when retried. Each agent trial copies its baseline into `work/runs/`,
materializes independent inbox files and rebases project paths. Level 1 starts without a
project. Other levels use the same `agent::studio_prompt` as Studio.

Tasks are code templates filled from manifest notes and frozen baseline evidence. Checks
cover kind/canvas/source identity, exact style changes, kept source ranges, timed assets,
kind-specific edits and caption/mouth geometry. Cue timestamps alone are not word timing.
Visual truth is disclosed as a detector-derived proxy; missing evidence fails as `check`.
Every check variant has passing and failing project tests.

The runner calls `agent::run` with an explicit selection and a Stop watchdog: 600 seconds,
or 1,200 for level 1 on long fixtures, unless overridden. It records one JSONL row per trial
under `results/YYYY-MM-DD.jsonl`: fixture, kind, length, task, level, agent, model, effort,
trial, pass, check detail, seconds, input/output/cached tokens, reported cost, claimed
success and failure class. Logs and failing workspaces remain inspectable. Failure classes
are `model`, `app`, `environment` and `check`; classification includes supporting evidence.

Custom agent commands are refused because they override model selection. Free runs accept
the known free models; paid runs require `--allow-paid`. Screening runs 49 cells sequentially:
nine free models at their default variant on five tasks and four image-capable models on
the visual task. Existing cells are skipped; `--rerun` preserves earlier generations.
Ladders require two passes, trying a third trial on a split. Reports separate one-trial
screening outcomes from reliable rungs and show medians and failure evidence.

## The built-in skill

`crates/veecut-engine/skill/` holds the agent skill copied into every workspace by
`veecut init`: `SKILL.md` (router), `workflows/<kind>.md` (one per `VideoKind::slug()`) and
`reference/{editor-api,notes-and-rules,style-setup}.md`. The workspace module embeds these
files with `include_str!`.

## Credits

See [CREDITS.md](../CREDITS.md). Code ideas come from free247editor and Herk Edit Kit (MIT),
editor ideas from OpenCut (MIT, name and logo excluded), and workflow ideas from the projects in
awesome-claude-video-skills (list CC0; each project under its own license).
