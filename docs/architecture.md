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
}
/// Area clear of platform buttons and captions for this canvas, normalized.
pub fn safe_area(canvas: &Canvas) -> Rect;
```

### `transcribe`

```rust
pub enum Provider { Groq, Local }
pub struct TranscribeOptions { pub provider: Option<Provider>, pub language: Option<String>, pub model: Option<String> }
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
/// Median face center over `samples` frames of `range`, if a face is found.
pub fn face_focus(path: &Path, range: (f64, f64), samples: usize) -> Result<Option<Point>>;
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
pub struct Config { .. }    // Config::load(), save(); skills auto-sync/auto-update, agent commands, transcription defaults
pub fn load_dotenv(root: &Path); // sets variables from <root>/.env that are not already set
// agent.rs
pub enum AgentKind { OpenCode, Claude, Codex, Gemini }
pub fn detect() -> Vec<(AgentKind, Option<String>)>;
pub fn run(ws: &Workspace, kind: AgentKind, prompt: &str, on_line: &mut dyn FnMut(&str)) -> Result<std::process::ExitStatus>;
```

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
                         pub voice: Option<PathBuf>, pub copy: Option<String>, pub canvas: Option<Canvas>, pub name: Option<String> }
pub fn edit(ws: &Workspace, req: &EditRequest, progress: &mut dyn FnMut(&str)) -> Result<EditReport>;
pub fn check(path: &Path) -> Result<QaReport>; // qa.rs: duration, size, fps, loudness, true peak, black frames, problems
```

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
veecut ops JSON|--file F|-            apply Editor API ops atomically (one op, an array, or JSON lines)
veecut schema                         JSON schema of the ops
veecut undo / veecut redo
veecut render [--final] [--aspect A] [--range A-B] [--out FILE]
                                      draft to outbox/<slug>-v<n>.mp4, final to outbox/<slug>-final.mp4
veecut frame --at T [--width W] [--out FILE]      one composited frame as PNG (for vision models)
veecut frames [--every S] [--out FILE]            contact sheet of frames
veecut captions [--rebuild] [--mode word_pop|highlight|pills|subtitle|off]
veecut note "NOTE" [--at T] [--rule]             pin a note; --rule also saves it to my-rules.md
veecut rules [add "RULE" --words "USER WORDS"]   list or add lasting rules
veecut style [show|set KEY VALUE]
veecut skills [sync|list|search Q|show REPO|install REPO [--force]|update [REPO]|remove REPO|recommend [--kind K]]
veecut qa FILE
veecut agent "PROMPT" [--with opencode|claude|codex|gemini]
veecut mcp                            MCP server on stdio (tools below)
veecut studio                         open the desktop editor
veecut fonts                          list caption and text fonts
```

MCP tools: `status`, `add`, `edit`, `ops`, `schema`, `render`, `frame`, `frames`, `transcript`, `note`,
`undo` (with `redo: true` to redo), `skills_search`, `skills_install`. Each takes the same
arguments as the matching command; `frame` and `frames` also return the PNG as an image.

## The built-in skill

`crates/veecut-engine/skill/` holds the agent skill copied into every workspace by
`veecut init`: `SKILL.md` (router), `workflows/<kind>.md` (one per `VideoKind::slug()`) and
`reference/{editor-api,notes-and-rules,style-setup}.md`. The workspace module embeds these
files with `include_str!`.

## Credits

See [CREDITS.md](../CREDITS.md). Code ideas come from free247editor and Herk Edit Kit (MIT),
editor ideas from OpenCut (MIT, name and logo excluded), and workflow ideas from the projects in
awesome-claude-video-skills (list CC0; each project under its own license).
