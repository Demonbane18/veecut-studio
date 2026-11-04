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
| `crates/veecut-engine` | library | model, ops, media I/O, analysis, transcription, graphics, compositor, recipes, skills catalog and importer, look packs, workspace |
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
  footage/                 found footage from `veecut fetch`, each file with <file>.source.json
  .veecut/find/            the last search (last.json) and its contact sheet (last.png)
  .veecut/sfx/v2/          the generated sound kit
  .agents/skills/          built-in skills veecut, veecut-arcade and veecut-editorial (copied from the binary)
                           plus catalog, added and own skills
  .claude/skills/          the same skills for Claude Code
  AGENTS.md, CLAUDE.md     tell every agent to use the veecut skill
  opencode.json            registers `veecut mcp` with OpenCode
  skills.lock.json         catalog skills pinned to commits; added skills with source, SHA-256 and notes
  .env                     GROQ_API_KEY, GITHUB_TOKEN, PEXELS_API_KEY (optional, never committed)
```

Global state lives in `~/.veecut/` (override with `VEECUT_HOME`): `config.toml`,
`catalog.json` and `models/` (whisper models and the MODNet cut-out model).

## Project model (`project.rs`, done)

- Times are seconds (f64) on the timeline; positions and sizes are fractions of the canvas.
- `tracks[0]` is the magnetic main track: clips play back to back, `normalize()` recomputes
  their starts, crossfades overlap the previous clip.
- Other tracks keep absolute starts. Visual tracks draw in order (later on top).
- Item bodies: `Clip` (asset piece with speed, volume, fit, focus, zooms, frame, fades,
  transition, Ken Burns, duck), `Text` (preset, style overrides, position, animations;
  `*word*` = accent color) and `Caption` (words with times relative to the item start).
- `KenBurns.smooth: bool` defaults to false for existing projects (linear interpolation).
  True applies clamped quintic smootherstep to both scale and focus in the shared transform.
  `KenBurns.phase: [f32; 2]` defaults to `[0, 1]`, with finite ordered values within 0..1.
  Slicing composes this phase range while retaining original scale/focus endpoints; evaluation
  interpolates phase before easing, preserving motion across splits, trims and cut-outs.
- `Project::source_to_timeline(asset, t)` maps a source time through the cuts.
- `faces` stores timestamped face boxes in source coordinates, keyed by asset id. Recipes set
  it through ops. Caption layout maps the boxes through the same crop and zoom as the picture.
  Old projects load without face data; rerun Auto-edit to analyze them.
- Studio stores Auto-edit review items in `markers`, with `at = -1` and an
  `Auto-edit review: ` prefix. These unpinned notes survive ripple cuts. Dismiss sets `done`
  through ops; pending and dismissed states survive reopening, with in-session editor Undo.
- `fonts` lists font files, absolute or relative to the project folder like asset paths. Look
  packs register their fonts here; see [Skills and look packs](#skills-and-look-packs).
- Documentary additions (ideas and measurements from NextPhase Editor V1, see CREDITS.md):
  - `VideoKind::Documentary` (`documentary`): a talking head cut into a fast documentary.
  - `Clip.grade: Option<Treatment>`: a per-clip colour treatment applied to that clip's own
    picture before compositing (`look` natural/teal/mono/warm/cold/invert, `halftone` 0..1,
    `vignette` 0..1, optional `recolor` hue band to target hue). The project `look` still
    applies to the whole frame afterwards. The speaker's camera normally has no grade.
  - `TransitionKind` adds `negative`, `glitch`, `tiles`, `focus`, `whip`. Transitions work on
    main and overlay clips: on main it is from the previous clip, on an overlay it is how the
    clip enters over what is below. Only crossfade overlaps main clips; the rest are cut effects
    whose `duration` is the effect length (30 fps: flash 1 frame, negative 4, glitch 4, tiles 9,
    focus 9, whip 4).
  - `Clip.mask: Option<Mask>` (`asset`, `offset`, `outline`): a mask video the size of the
    clip's asset, brightness = opacity, framed exactly like the clip, mask time = clip source
    time − `offset`; `outline` draws a white sticker edge (fraction of canvas height).
    `AssetRole::Mask` marks those videos.
  - `TextPreset` adds `keyword`, `chapter`, `shock`, `black_card`, `mega`. `mega` is drawn
    under the overlay tracks so a cut-out of the speaker stands in front of it.
    `TextPreset::default_anim()` is what `add_text` uses when no animation is given: `none` for
    `shock`, `black_card` and `mega` (they cut on hard), `fade` for the rest.
  - `CaptionMode::Reveal` and `CaptionStyle.ink_on_light`.
  - `Project.sources: BTreeMap<asset id, Source>`: provenance and licence of found files
    (`url`, `title`, `author`, `license`, `license_url`, `credit`, `provider`, `flag`,
    `flag_reason`, `approved`, `retrieved`). `Source::blocks_final()` = flagged and not approved.
    Add new records through ops (`{"op":"set","path":"/sources/asset-4","value":{...}}`). CLI and MCP
    `ops` refuse approving, changing `flag`/`flag_reason`, deleting or replacing a record, replacing
    `/sources` or replacing the whole project: approval is the user's step, through Studio's Sources
    tab or `veecut sources --approve` with an interactive terminal on stdin.

## Editor API (`ops.rs`, done)

`cut_range`, `split`, `trim`, `move`, `delete`, `duplicate`, `add_clip`, `add_text`, `add_zoom`,
`patch` (JSON merge patch on an item), `set` (JSON pointer on the project), `add_marker`,
`add_track`. `apply` and `apply_all` are atomic and validate the result. The JSON schema comes
from `schemars::schema_for!(Op)`.

Phrase edits use `Transcript::find_phrase(&Project, &str) -> Result<Vec<PhraseMatch>>` and
`Transcript::cut_phrase(&mut Project, &str, Option<usize>) -> Result<PhraseCut>`.
Search matches consecutive transcript words kept on the playing timeline, ignoring case
and punctuation without substring or fuzzy matching. A removed intervening word breaks
the match; a cut between neighbouring words does not. Matches include 1-based occurrences,
word IDs, sentence context and source/timeline start/end seconds rounded to milliseconds.
`cut_phrase` requires one match or an explicit occurrence; missing and ambiguous matches
leave the project unchanged. It applies one `CutRange` through `ops::apply`, then the
command saves one undo step. Matching uses the transcript asset's main-track clips, or
its audio-track clips when the voice is separate; the cut ripples through every track.

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
/// Refuses an `out` that is any project asset (same canonical path or file identity). Encodes to a
/// temp sibling and renames onto `out` only after success; failure or cancel removes only the temp.
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
    /// Loads each new file in `project.fonts` once; missing files are skipped. Call it before
    /// drawing or measuring a project that may use look-pack fonts.
    pub fn load_project_fonts(&mut self, project: &Project, project_dir: &Path);
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
/// Auto provider: Groq when GROQ_API_KEY is set, else local whisper.cpp. Models download on first
/// use to ~/.veecut/models from whisper.cpp's Hugging Face repo at a pinned commit, with size and
/// SHA-256 checked; a `<model>.sha256` marker records the verified file so later runs check size only.
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
/// When drawn, a page is split into phrases of at most two lines; the line length is `max_chars`
/// lowered to what fits the safe width at the caption size (`graphics::caption_line_chars`), so long
/// words start the next phrase instead of shrinking the page. Placement measures the same phrases.
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
    pub fn init(root: &Path) -> Result<Workspace>;      // idempotent; rewrites the three built-in skills in both skill roots
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
pub struct Usage { pub input_tokens: u64, pub output_tokens: u64, pub cached_tokens: u64, pub first_request_input_tokens: Option<u64>, pub cost_usd: Option<f64>, pub turns: Option<u64> }
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
Studio and the evaluation harness share `agent::studio_prompt`: phrase cuts use `cut-phrase`
with optional `--occurrence` and `transcript-search` to list matches (MCP `cut_phrase` and
`transcript_search`); other edits use ops. The prompt allows an optional cached frame,
asks for a short reply, and forbids render or open unless a video is requested.
Tool discovery and the agent's PATH prepend the executable's directory only for `veecut`
and `veecut-studio` product binaries. Evaluator and test binaries leave the usual search
order intact, so a neighbouring stale `target/debug/veecut` does not shadow the installed CLI.
Codex, OpenCode and Claude structured output separates answers, tool logs and cumulative usage.
Input tokens exclude cached tokens; `Usage::total_tokens` adds input, output and cached once.
`first_request_input_tokens` is the first model request's input, including cache reads and
writes once, not the run's cumulative input or total. It is `None` when unreported and
omitted from serialized output. Claude uses the first main-thread assistant usage event;
OpenCode uses the first `step_finish`; Codex reads the first `token_count` event's
`last_token_usage.input_tokens` from the native rollout identified by `thread.started`.
Codex's value already includes cached input. Gemini does not report this field.
Missing usage or cost stays unknown. Studio keeps the latest note, reply, usage and result
outside the folded log.
Claude and Codex readiness includes their read-only login checks; failures retain the fix.
Other providers and custom command overrides disclose what was not checked. Refresh reruns
the checks without changing agent settings.
Model and effort choices live in `[agent.selection.<agent>]` in `config.toml`. Callers resolve
the config selection and apply per-run overrides before calling `run`; an absent effort in
`Selection` means the agent's default. OpenCode defaults to `opencode/glm-5.3-flash-free`;
other agents use their CLI defaults after the per-run restrictions below. A custom command
vector takes precedence and runs unchanged, without added context, model, effort or output flags.
The default OpenCode driver stops on reported free-allowance exhaustion and unavailable-model
errors, preserves usage received so far and returns an actionable error to CLI and Studio.
Only emitted stdout/stderr errors are observable; private OpenCode log entries are not read.

Claude `ultracode` maps to `--effort max --settings '{"ultracode":true}'` and adds `Workflow`
to the allowed tools. The picker labels its roughly fivefold token cost. Gemini has no
effort flag. Model lists are suggestions; custom model IDs remain accepted.

#### Per-run agent context

Default drivers keep VeeCut workspace instructions and skills while limiting unrelated
user context through process arguments and environment variables. Global files and OAuth
credentials are not changed. Custom command overrides bypass these restrictions.

| Agent | Per-run settings | Limits |
| --- | --- | --- |
| Claude | `--setting-sources project,local --strict-mcp-config`, explicit VeeCut-only `--mcp-config`, `CLAUDE_CODE_DISABLE_AUTO_MEMORY=1` | VeeCut MCP tools are allowed in headless runs. User settings, plugins, hooks, skills and `CLAUDE.md` are excluded. User-level `apiKeyHelper`, environment and provider settings are also excluded. OAuth login stays available; VeeCut does not use API-key-only `--bare`. |
| OpenCode | `--pure --print-logs --log-level ERROR`, `OPENCODE_DISABLE_CLAUDE_CODE=1`, `OPENCODE_DISABLE_EXTERNAL_SKILLS=1`, workspace `.agents/skills` through `OPENCODE_CONFIG_CONTENT`, `XDG_CONFIG_HOME=<workspace>/.veecut/agent-config` | The per-run config location excludes inherited global config, plugins and MCP servers. Workspace `opencode.json` supplies VeeCut MCP. Sign-in data stays in its original data directory; credentials are not copied. Providers defined only in global config are unavailable. |
| Codex | `--ignore-user-config`, `-c skills.include_instructions=false`, `-c 'mcp_servers.veecut={command="veecut",args=["mcp"],default_tools_approval_mode="approve"}'` | Authentication remains available. The global skill catalog is omitted; workspace `AGENTS.md` points to the VeeCut skill by path. User config defaults, including model, network access and service tier, do not apply; `~/.codex/AGENTS.md` still loads. |
| Gemini | `-e none --allowed-mcp-server-names veecut` | No equivalent flag disables user skills, `GEMINI.md` or settings hooks. Signed-in execution has not been verified. |


### `skills`

```rust
pub struct Catalog { pub source_commit: String, pub fetched_at: DateTime<Utc>, pub categories: Vec<Category>, pub entries: Vec<Entry> }
pub struct Entry { pub repo: String, pub url: String, pub stars: u32, pub description: String, pub security: String, pub categories: Vec<String>, pub tested: bool }
pub fn sync() -> Result<SyncReport>;                       // fetch the awesome list, write ~/.veecut/catalog.json
pub fn load_catalog() -> Result<Option<Catalog>>;
pub fn auto_sync_if_stale(config: &Config) -> Option<Result<SyncReport>>;
pub fn search<'a>(catalog: &'a Catalog, query: &str, category: Option<&str>) -> Vec<&'a Entry>;
pub fn install(ws: &Workspace, repo: &str, force: bool) -> Result<InstallReport>; // non-SAFE entries need force or safe_only off
pub fn update(ws: &Workspace, repo: Option<&str>) -> Result<Vec<UpdateReport>>;  // also re-fetches added path and URL sources
pub fn remove(ws: &Workspace, target: &str) -> Result<()>;  // a repo, an added source or one skill name; never a built-in
// skills.lock.json is untrusted input: every persisted skill name must be one safe path component
// and not a built-in skill name, checked before any remove, update or reinstall touches the filesystem.
pub fn installed(ws: &Workspace) -> Result<Lock>;
/// Best catalog entries for a kind of video: SAFE, tested first, then stars. Data-driven, so it
/// follows the catalog as it updates.
pub fn recommended<'a>(catalog: &'a Catalog, kind: VideoKind, n: usize) -> Vec<&'a Entry>;
// add, installed_skills, new_skill and check: see "Skills and look packs".
```

### `recipes`, `qa`

```rust
pub enum ExplainerMotion { Hold, Push, Glide } // serialized as hold/push/glide
pub struct EditRequest { pub kind: VideoKind, pub sources: Vec<PathBuf>, pub broll: Vec<PathBuf>, pub music: Option<PathBuf>,
                         pub voice: Option<PathBuf>, pub copy: Option<String>, pub canvas: Option<Canvas>, pub name: Option<String>,
                         pub hook: Option<String>, pub language: Option<String>, pub transcribe: Option<TranscribeOptions>,
                         pub clips: usize, pub render: bool, pub find: bool /* documentary: search footage */,
                         pub motion: Option<ExplainerMotion>, pub skill: Option<String> /* installed look to apply */ }
pub fn edit(ws: &Workspace, req: &EditRequest, progress: &mut dyn FnMut(&str)) -> Result<EditReport>;
pub fn check(path: &Path) -> Result<QaReport>; // qa.rs: duration, size, fps, loudness, true peak, black frames, problems
```

An explicit recipe transcription override bypasses global provider settings. Evaluation
baselines use local transcription with `cpu_only = true`, so configured Groq credentials
cannot cause an upload and whisper.cpp does not use GPU acceleration.
The harness also sets `VEECUT_LOCAL_CPU_ONLY=1` before starting workers. The transcription
entry point honors this process-scoped override in inherited CLI and MCP children, even
when their global config selects Groq.

#### Explainer motion and narration

`ExplainerMotion` exposes `ALL`, `label()`, `slug()` and `FromStr`. `EditRequest.motion`
defaults to `None`, preserving original recipe motion. An explicit value with another kind
is rejected before project creation. Hold adds no Ken Burns; Push uses smooth centered
1.0x to 1.12x zoom; Glide uses smooth modest zoom with alternating restrained focus.
Explicit modes apply to images and muted visual videos.

`--voice` is authoritative. Otherwise an audio source supplies narration, falling back to
the first video's audio only when no audio source exists. Ordered visuals repeat to cover
narration; a short video repeats within its sentence without shifting the next shot boundary.
Zero-length visuals are rejected. Narration and captions retain coherent timeline times.
All changes use ops, and preview/export share the Rust transform. Character generation,
external renderers and depth/parallax models are outside this recipe.

Studio's Explainer New edit takes narration audio/video, ordered B-roll/images with
Up/Down/Remove, optional music with Choose/Remove, and Recipe default/Hold/Push/Glide.
Music is forwarded only for Explainer; its B-roll is forwarded without the Documentary-only
find option. The welcome screen's Inbox lists videos, audio and pictures in `inbox/` (Refresh
rereads it): clicking a video or audio file makes it the source or narration; for Explainer and
Documentary each video/picture row has Add to visuals (or Add as B-roll) plus an add-all button,
and for Explainer audio rows have Use as music, so a whole edit can be set up without a file
dialog. For native welcome-screen QA, run
`VEECUT_STUDIO_SHOTS=<empty-dir> VEECUT_STUDIO_NEW_KIND=explainer veecut --workspace <ws> studio`
without a project. This captures the explainer controls and exits; project capture remains
the playback entrypoint.

#### Recipe safeguards (from real-footage QA, 2026-11-04)

- Framing: `Job::finish` aims each main-track clip at the median face centre over its own source
  range (short clips borrow neighbours within 5 s, so one false detection cannot drag the crop);
  zooms without a focus get the median over their stretch. Caption placement protects the face from
  just below the brows to under the chin (`placement::FACE_TOP`, `CHIN_MARGIN`).
- Hooks: an automatic hook must be a complete sentence that can open the video (ends in `.?!`, no
  leading conjunction or dangling pronoun, positive highlight score); otherwise none and a review line.
  A short hook holds 3 s.
- A failed `edit` removes the project folders it created and did not finish; finished projects stay.
- Talking heads: punch-in switches prefer cuts and phrase boundaries and fall back to word gaps to
  meet the shot limit, never inside a word.
- Tutorial: only transcript-word-free silence is sped up and muted; a silence cut never passes the
  next word. Speed labels sit bottom centre.
- Retakes: review and cut cover only the repeated sentence (or one near segment within 12 s), at
  most 16 s; far repeats are review-only, and review text asks to listen before cutting.
- Clips: windows skip a leading false start, end on a sentence and are sized by kept time after
  cuts (20 to 60 s).
- Beats: tempo prefers the 80 to 160 BPM relative (1x, 2x, 1.5x) of the strongest period and the
  grid never steps onto an offbeat. Montages start each source past its opening, skip black stretches
  (`lit_run` checks start, middle and end frames), lengthen crossfaded vlog shots by the overlap and
  report footage that runs out; promo lines end before the next line starts.
- Documentary find: an acronym spoken once (SLS) can be the subject; every query keeps one subject
  anchor and a result title must contain it as a whole word. Providers try medium/smaller renditions
  first and skip known over-cap files.
- Rights: `Workspace::add_asset` imports an adjacent `<file>.source.json` into `project.sources`
  (approval reset: it is project-local), so credits and the final-render gate follow fetched files
  into any recipe; Studio and `cutout --world` persist those sources with the assets.
- Explainer: glide starts at 1.06x and keeps focus endpoints inside 80% of the initial pan room, so
  the cover crop never clamps mid-shot. Crops discarding over 30% of a picture are named in review
  with a `fit: contain` patch. Headings attach to the shot where the narration says them (in order);
  unmatched lines go to review. Voice-only edits use one large centered word on a slate background.
  16:9 explainers default captions to 0.8. A sole narration video keeps picture and voice on the
  same source cuts. Every visual is probed before the project is created.

### Documentary modules (done)

Ideas and published measurements come from NextPhase Editor V1 (see CREDITS.md). Its code, text,
design tokens and sounds are not copied: its licence allows making videos but not repackaging, so
everything here is written fresh.

```rust
// render: per-clip grades, all transition kinds on main and overlay clips, overlay alpha, masks
// with outline. Frame order: background → main track → overlay tracks, with gfx.draw_behind
// (mega text) drawn just before the first overlay track holding a masked clip (after all
// overlays when none has one) → cut effects (dip/flash/negative/glitch) → project look →
// gfx.draw (text, captions). So the world sits under the mega word and the cut-out over it.

// graphics
impl Graphics {
    /// Draws only `mega` text items (called between the main track and the overlays).
    pub fn draw_behind(&mut self, project: &Project, t: f64, pixmap: &mut tiny_skia::PixmapMut);
    // `draw` skips mega items.
}

// footage: free, licence-tracked material
pub const VIDEO_CAP: u64 = 150 * 1024 * 1024;   // largest video fetch downloads
pub const IMAGE_CAP: u64 = 40 * 1024 * 1024;    // largest picture
pub enum Provider { Nasa, Commons, Archive, Openverse, Pexels }   // Pexels only with PEXELS_API_KEY
impl Provider { pub const ALL: [Provider; 5]; pub fn as_str(self) -> &'static str; pub fn label(self) -> &'static str;
                pub fn available() -> Vec<Provider>; pub fn parse(name: &str) -> Result<Provider>; }
pub enum MediaKind { Image, Video }
pub struct Candidate { pub id: String /* "nasa:ID" */, pub provider: Provider, pub kind: MediaKind, pub title: String,
                       pub page_url: String, pub thumb_url: String, pub width: u32, pub height: u32, pub duration: f64,
                       pub license: String, pub license_url: Option<String>, pub author: Option<String>,
                       pub flag: bool, pub flag_reason: Option<String> }
pub struct SearchOptions { pub kind: Option<MediaKind>, pub providers: Vec<Provider>, pub per_provider: usize }   // default: available(), 4
pub struct SearchResult { pub candidates: Vec<Candidate>, pub warnings: Vec<String> }
/// Providers in parallel; a failing provider becomes a warning. per_provider is clamped to 1..=20.
pub fn search(query: &str, opts: &SearchOptions) -> Result<SearchResult>;
pub fn contact_sheet(candidates: &[Candidate], out: &Path) -> Result<()>;   // numbered thumbnails
pub struct Fetched { pub path: PathBuf, pub source: Source, pub candidate: Candidate }
/// Into <ws>/footage/<provider>-<id>.<ext> through a .part file, within the caps, ffprobe-checked as the
/// expected kind, with <file>.source.json beside it. An earlier download of the same id is reused.
pub fn fetch(ws: &Workspace, id: &str) -> Result<Fetched>;
/// Adds a fetched file to a project as an asset and records its source through ops.
pub fn add_to_project(ws: &Workspace, project: &mut Project, project_dir: &Path, fetched: &Fetched, role: AssetRole) -> Result<String>;
/// Flagged sources nobody approved that the project's clips use.
pub fn blocking_sources(project: &Project) -> Vec<(String, Source)>;
/// Refuses a final render while blocking_sources is not empty (CLI render --final, MCP render, Studio export).
pub fn check_final(project: &Project) -> Result<()>;
pub fn source_op(project: &Project, asset: &str, source: &Source) -> Result<Op>;   // the set op that records a source
pub fn used_assets(project: &Project) -> BTreeSet<&str>;
// rights (crate-private): public domain, PDM, CC0, CC BY, CC BY-SA and the Pexels License pass; NC, ND,
// GFDL alone, fair use/non-free and unknown are flagged. NASA items whose credit names someone else or
// that mention copyright, and Archive items with no licence outside Prelinger, are flagged too.

// sfx: a synthesized sound kit and its placement grammar
pub enum Sound { Click, Tick, PaneClick, Swish, Glitch, Riser, SubTone, SubBoom, Impact, Hit, Outro, Chiptune, Coin, Blip }
impl Sound { pub fn role(self) -> &'static str; }
/// `transient` = seconds to the hit (the file's loudest sample); `level` = loudest 50 ms, K-weighted, LUFS scale.
pub struct KitSound { pub name: String, pub sound: Sound, pub path: PathBuf, pub transient: f64, pub duration: f64, pub level: f32 }
pub fn kit_dir(ws: &Workspace) -> PathBuf;   // <ws>/.veecut/sfx/v{KIT_VERSION}/
pub fn kit(ws: &Workspace) -> Result<Vec<KitSound>>;   // 17 deterministic 48 kHz mono WAVs (click-1..4 and one of each other sound); missing files written
pub fn regenerate(ws: &Workspace) -> Result<Vec<KitSound>>;   // writes every file again
// KIT_VERSION = 2 (folder vN), PEAK_DB = -3 dBFS sample peak, LAG = 0.008 s, DEFAULT_VOICE_LUFS = -20.
// The synthesized samples and levels are made once per process (LazyLock), so repeated kit() calls
// only write files that are missing.
pub struct Cue { pub at: f64 /* the picture change it belongs to */, pub sound: Sound }
/// Places cues on the `sfx` audio track (created when missing), one Sfx asset per kit file: each hit
/// lands LAG after its cue (heads before 0 are trimmed, tails stop at the main track's end), clicks
/// closer than a frame merge, the four click variants rotate, and each sound's loudest 50 ms sits a
/// fixed distance under the voice (clicks 4.5 dB, panes 9, swish/glitch/tick 5.5, riser/impact/hit
/// 1.5, outro 8, sub boom 15, sub tone 18, coin 5.5, blip 6, chiptune 18), capped so a sound's
/// peak stays under -1 dBFS.
/// Returns the number placed.
pub fn place(ws: &Workspace, project: &mut Project, project_dir: &Path, cues: &[Cue], voice_lufs: Option<f32>) -> Result<usize>;
pub fn loudness(path: &Path, range: Option<(f64, f64)>) -> Result<f32>;   // integrated LUFS (ebur128); under 400 ms reads -70
pub fn bed_volume(voice_lufs: f32, music_lufs: f32, under_lu: f32) -> f32;   // linear trim, never above 1.0
/// Cuts (dB <= -60 is silence) or pulls down a music bed over timeline ranges: splits its clips at
/// the edges, multiplies the middle pieces' volume, 20 ms fades on each side. Atomic through ops.
pub fn shape_bed(project: &mut Project, track: &str, moves: &[(f64, f64, f32)]) -> Result<()>;

// matte: the speaker cut out of their own camera
pub fn status() -> String;
pub fn ready() -> bool;   // the model file is present with the pinned size and SHA-256; no network
// MODNet ONNX (Xenova/modnet, revision fa2fa546, 25,888,640 bytes, Apache-2.0) downloaded on first cutout to
// ~/.veecut/models/modnet-fp32-07c308cf.onnx; run by tract at a 512-pixel short edge; masks cached in
// <project>/.cache/masks/. The cut-out goes on an overlay track named "Cut-out", above B-roll.
pub struct CutoutReport { pub mask_asset: String, pub overlay_item: String, pub frames: usize, pub seconds_taken: f64 }
/// Masks the main-track clip at timeline [from, to) and places an overlay copy of it with the mask.
pub fn cutout(ws: &Workspace, project: &mut Project, project_dir: &Path, from: f64, to: f64, outline: f32,
              progress: &mut dyn FnMut(&str)) -> Result<CutoutReport>;

// qa: words check
pub struct WordIssue { pub at: f64, pub expected: String, pub heard: Option<String>, pub missing: bool,
                       pub near_cut: bool, pub near_sound: Option<String> }
pub struct WordsReport { pub expected: usize, pub matched: usize, pub missing: usize, pub changed: usize,
                         pub issues: Vec<WordIssue>, pub provider: String, pub verdict: String }
/// Expected = words of transcript.json and transcript-*.json placed on the timeline (the caption
/// mapping). Tokens are normalized (case, punctuation, digits spelled out, "every one" = "everyone")
/// and aligned by edit distance with a time-proximity cost inside a ±10 s band. `near_cut`: a
/// main-track cut within 0.25 s of the word; `near_sound`: a clip on an audible audio track (not a
/// transcribed voice) starting within 0.15 s. The CLI and MCP use local whisper unless the
/// config's provider is `groq`.
pub fn check_words(project: &Project, project_dir: &Path, render: &Path, opts: &TranscribeOptions,
                   progress: &mut dyn FnMut(&str)) -> Result<WordsReport>;
```

Workspace additions: `footage/` (`Workspace::footage()`, downloaded found material, never edited after
download), `.veecut/find/` (`last.json` and the contact sheet `last.png` of the last search),
`.veecut/sfx/v2/` (the generated kit). `veecut init` adds `footage/` and `.veecut/find/` to `.gitignore`
and a commented `PEXELS_API_KEY=` line to the `.env` template.

#### The documentary recipe (`recipes/documentary.rs`)

Defaults to 16:9. Cuts pauses and mistakes like the talking-head recipes and crops to the face, then
plans beats on word onsets: runs of about 4 s of pictures (0.6 to 1.5 s each, aim 0.95; past 180 s the
body after the first 10 s uses 1.2 to 2.0, aim 1.6) between face beats of 1.2 to 2.5 s (aim 1.8). It
opens on pictures and never places two face beats in a row; without material the face stays on screen.
`--broll` files come first; `find` searches one query per run (the run's own name or concrete word,
anchored to `topic_terms`: concrete words said at least twice, names weighted), keeps results whose
title shares a five-letter stem with the query and has no `UNSAFE_WORDS` stem, skips flagged candidates,
prefers video, takes at most 2 files per run and 12 per edit, and leaves out a provider after its second
failure. Video pictures start at `past_opening` (max of 10 % and min(10 s, 25 %)) and `lit_start` steps
past starts whose middle frame is 95 % under 10 % luma. Each picture is an overlay clip on `broll`, muted, with a
`grade` chosen from the line's words (warm, mono, cold, teal) or an eight-step rotation (natural 3, teal 2,
mono 2, warm 1), halftone 0.22 on non-mono stills, vignette 0.35 natural or 0.25, and a Ken Burns push of
5.5 % per second capped at 1.2x. Transitions: tiles (9 frames) on the first picture and every third run,
negative (4) into a line after a 1 s pause, focus (9) at other run starts, glitch (4) on words like
"broke" or "leaked", whip (4) out of a shot of 1.2 s or more at most every 20 s, flash (1) on every third
cut inside a run. Captions switch to `reveal` (Inter, 0.057 of the height in 16:9, `ink_on_light`) unless
off. Display words: one `keyword` (1.2 s) per run on a stressed word, `shock` (0.8 s) for numbers and
money within round(1.5 per minute), `black_card` for "nothing", "nobody", "none", "wala" or "no one" (at
most 2); never on a face beat, never overlapping, and keywords and shocks only on `display_worthy`
words (an amount, an acronym or a concrete word of 5+ letters). When `matte::ready()`, `mega` words go
behind a cut-out on face beats of 1.2 s or more holding such a word where `face_through` finds the face
at nearly every check, floor(length / 45 s) of them, at least 1;
otherwise review gives the `veecut cutout` command. Cues: click per picture change and face beat, nine
pane clicks per tile reveal, glitch, swish on whip, tick per display word, sub tone per black card,
impact where the bed returns; none with a `voice_only` style. The bed (`--music`, else the first file in
`my-sounds/music`) runs from 0 at `bed_volume(voice, music, 5.5)` (0.35 when unmeasured), fade in
0.03 s, out 0.9 s; `shape_bed` silences it from 0.05 s before the punchline (the last sentence of at
most 12 words starting after 40 % of the length, not a call to action) to 0.02 s before the next line,
and pulls it 8 dB under black cards.

#### Studio additions

Footage tab (`footage_panel.rs`): one `footage::search` worker and at most 3 thumbnail downloads,
answers tagged by search generation; Add and Add at playhead fetch on a background thread and merge
only `assets`, `sources` and `tracks` through the undo path, refusing if those changed meanwhile.
Sources tab: rights table, Approve and Flag through `set` ops on `/sources/<asset>/...`, credits with
Copy. Final-quality export lists `blocking_sources` with Approve buttons and calls `check_final`.
Inspector: Grade (look, halftone, vignette, recolor), Mask (outline, remove), all transition kinds with
per-kind default lengths (crossfade and dip 0.5 s, flash 1 frame, negative, glitch and whip 4, tiles
and focus 9, capped at the clip), "Cut out the speaker" on a selected main-track video clip, the
documentary text presets, the Reveal caption mode and Ink on light. New edit with the Documentary kind
takes B-roll files and a find switch. Capture hook: `VEECUT_STUDIO_TAB=media|sources|footage` saves
only `00-paused.png` of that tab and quits; `VEECUT_STUDIO_SELECT` selects an item id,
`VEECUT_STUDIO_AT` seeks (seconds), `VEECUT_STUDIO_QUERY` is the Footage search (default `moon`) and
the capture waits for its thumbnails.

## Skills and look packs

Modules `look.rs` and `skills/` (`mod.rs` for the catalog, `add.rs` for imports). A skill is a
folder holding `SKILL.md` (frontmatter `name` and `description`; YAML folded `>-` and literal `|`
descriptions are read as joined text) with instructions for agents. It may also hold
`veecut.toml`, a look pack VeeCut applies with its own renderer, and `fonts/` (`.ttf`/`.otf` with
their licence), `sounds/` and `music/` files the pack refers to. Anything else, scripts included,
is kept for reading and never run. Each skill lives in both `.agents/skills/<name>/` and
`.claude/skills/<name>/`.

### Look packs (`look.rs`)

```rust
pub const LOOK_FILE: &str = "veecut.toml";
/// Schema 1. Unknown keys are errors; every field except `schema` is optional.
pub struct LookPack {
    pub schema: u32,                            // must be 1
    pub kinds: Vec<VideoKind>,                  // recipes it suits; Studio's New edit switches to the first
    pub aspect: Option<String>,                 // 9:16, 16:9, 4:5 or 1:1; sets the canvas size
    pub captions: Option<CaptionPatch>,         // every CaptionStyle field, each optional
    pub look: Option<Look>,                     // the whole-frame project look
    pub background: Option<Color>,              // canvas background
    pub text: BTreeMap<TextPreset, TextStyle>,  // patches every existing text item of that preset
    pub overlays: Vec<OverlayLook>,             // text items the look adds
    pub clips: Option<ClipLook>,                // grade (Treatment), motion (hold/push/glide), fit
    pub transitions: Option<TransitionLook>,    // kind, duration, every (each Nth main-track cut; default 1)
    pub sound: Option<SoundLook>,               // text and cut cues, music bed, music_db
    pub fonts: Vec<String>,                     // relative to the skill folder; empty = every fonts/*.ttf|otf
}
pub struct OverlayLook { pub preset: TextPreset, pub text: String, pub position: Option<Point>, pub style: TextStyle,
                         pub start: f64, pub end: Option<f64> /* None = project end */,
                         pub anim_in: Option<Anim>, pub anim_out: Option<Anim> }
/// Parses and validates: schema 1, aspect, transition and overlay times, refs inside the folder.
pub fn load(skill_dir: &Path) -> Result<LookPack>;
/// Plans Editor API ops without changing `project`.
pub fn ops_for(ws: &Workspace, project: &Project, project_dir: &Path, skill_dir: &Path, pack: &LookPack) -> Result<Vec<Op>>;
/// load, ops_for, then one ops::apply_all. Returns the op count.
pub fn apply(ws: &Workspace, project: &mut Project, project_dir: &Path, skill_dir: &Path) -> Result<usize>;
/// The project's look as a pack, for `skills new --from-project`.
pub fn capture(project: &Project) -> LookPack;
```

- A look is one batch. `ops_for` plans set, patch and `add_text` ops and `apply` runs them through
  a single `ops::apply_all`, so applying is atomic and the caller saves it as one undo step.
- Sound refs are a file inside the skill folder (`sounds/pop.wav`) or `kit:<slug>`, an
  `sfx::Sound` in snake_case. `load` refuses absolute paths, `..` and missing files. `text` cues
  land on text entrances and `cut` cues on main-track cuts, both placed by `sfx::place` on a
  scratch copy. `music` loops under the whole project with 1.5 s fades, at `music_db` or, without
  it, the `sfx::bed_volume` rule. New sound assets arrive as `/assets` and `/sources` set ops, and
  pack fonts as a `/fonts` set op with absolute paths.
- Items a look adds get reserved ids: `look-overlay-*` for overlays and `look-sound-*` for cue and
  bed clips. Every apply first removes all items whose id starts with `look-`, so applying the same
  look again, or another one, replaces them instead of stacking. Ordinary text and hand-placed
  sounds stay; manual items must not use the `look-` prefix.
- Clip grade, motion and fit skip the speaker: a main-track video in a talking-head or documentary
  project whose asset has a transcript (`transcript.json` or `transcript-<asset>.json`). They apply
  to images, B-roll and source videos on overlay tracks, explainer main-track clips and documentary
  main-track B-roll.
- Text overrides patch the items present at apply time. Text added later keeps its preset default
  until the look is applied again.
- `capture` keeps caption settings, the project look, the background, the first style override
  seen per preset, `look-` overlays (one that ends with the project keeps an open end), the first
  eligible visual clip's grade, fit and nearest hold/push/glide motion, and the main-track
  transition pattern. It leaves out kinds, aspect, sound and fonts, whose files may live outside
  the new skill, and the exact Ken Burns path.
- `Project.fonts` paths load through `Graphics::load_project_fonts`, which loads each new file once,
  skips missing ones and refreshes `font_families()`. `render_frame`, `FrameStream` (and so
  export), recipe and CLI caption reports and `veecut frame` call it before drawing or measuring.
- `EditRequest.skill` names an installed skill. `Job::finish` applies its look after the recipe has
  built and aimed the project, then validates, saves and renders; the summary gains
  "Applied look NAME (N changes)". A missing skill or `veecut.toml` is an error.

### Importing skills (`skills/add.rs`)

```rust
pub fn add(ws: &Workspace, source: &str) -> Result<InstallReport>;   // InstallReport also carries notes
pub enum Origin { BuiltIn, Catalog(String /* repo */), Added(String /* source */), Own }
pub struct InstalledSkill { pub name: String, pub description: String, pub dir: PathBuf, pub origin: Origin,
                            pub has_look: bool, pub notes: Vec<String> }
pub fn installed_skills(ws: &Workspace) -> Result<Vec<InstalledSkill>>;   // folders in .agents/skills with a SKILL.md
/// SKILL.md plus the given look or a commented schema-1 template, written to both roots.
/// Refuses existing, unsafe or built-in names.
pub fn new_skill(ws: &Workspace, name: &str, description: &str, look: Option<LookPack>) -> Result<PathBuf>;
pub struct CheckReport { pub name: String, pub has_look: bool, pub kinds: Vec<VideoKind>, pub fonts: Vec<String>, pub notes: Vec<String> }
/// The look loads, every pack font parses, and one fresh scan gives the script and licence notes.
pub fn check(ws: &Workspace, name: &str) -> Result<CheckReport>;
```

`add` takes an existing folder, `.zip`, `.tar.gz` or `.tgz`; a GitHub `owner/repo` or github.com
URL, which goes through the catalog `install`, so grading applies; a Google Drive file link
(`/file/d/ID/...` or `?id=ID`), fetched from
`https://drive.usercontent.google.com/download?id=ID&export=download&confirm=t`; or any other https
URL to an archive. Every folder holding a `SKILL.md` becomes a skill named from its frontmatter,
with `-2`, `-3` and so on after a clash.

Content is staged and validated before anything installed changes. Links and special files are
refused, as are absolute, `..` and drive-letter paths; `.git` and `node_modules` are skipped. An
archive may be at most 200 MB and so may its expanded content; files over 25 MB are skipped and
reported, and zips inside the archive unpack at most four levels deep. A symlinked skill root is
refused. Nothing is executed. Notes list the scripts found (`*.sh`, `*.py`, `*.js`, `*.mjs`,
`*.ps1`, `package.json`) with "VeeCut never runs these", and the licence files or "No licence file:
personal use only; do not redistribute". Adding the same source again replaces its folders, and
`update` re-fetches path and URL sources.

`LockedRepo` gains `source` (`path:/abs` or `url:https://...`; `None` for GitHub), `sha256` (the
archive bytes, or a deterministic folder digest) and `notes`. All three default, so older
`skills.lock.json` files still load. Non-GitHub entries use `repo = "local/<first skill name>"`
and store the digest as `commit`.

`veecut`, `veecut-arcade` and `veecut-editorial` are `BuiltIn`: noted "Licence: MIT (VeeCut)",
never warned about a missing licence, reserved case-insensitively against imports, `new_skill` and
lock entries, and never removed. A lock entry with a source is `Added`; one without is `Catalog`,
which shows GitHub's SPDX licence when known. Any other folder is `Own`. `remove` takes a skill
name, a source, a repo or a GitHub URL. `check` keeps only lock notes the fresh scan does not
cover, with paths relative to the skill folder, so no note prints twice.

### Native devices for looks

Timings and sizes follow published measurements from Jad M.H's Arcade Motion Graphics and Motion
Graphics V3 (see CREDITS.md); the code is new.

- `Treatment.pixelate: f32` (serde default 0 = off, valid 0..=1): nearest-colour blocks of
  `round(pixelate × canvas height)` pixels on that clip; the arcade look uses 0.012.
- `TreatLook::SoftMono` (`soft_mono`): black and white from 0.10 to 0.90 with gentle contrast.
- `TransitionKind::Pixelate` (`pixelate`): the incoming clip starts at blocks of 0.08 of the
  height and resolves to sharp in six held steps. It is a cut effect on main and overlay clips;
  Studio's default length is 0.4 s.
- `TextPreset::Marker` (`marker`): dark `#1C1B1E` text over a `#D97757` band at 0.62 alpha with
  0.25 em padding and scaled 4 px corners, wiping in left to right over 0.35 s (quadratic
  ease-in-out).
- `TextPreset::Meter` (`meter`): a twelve-segment bar lighting floor(progress through the item ×
  12) segments, with the text as an optional label on its left. It defaults to the top left of
  the safe area, and caption placement avoids its whole footprint. Spanning the project makes it
  a story progress bar.
- Marker and Meter default to no entrance or exit animation.
- Press Start 2P (OFL, from upstream Google Fonts) is embedded and preferred over an installed copy.
- Kit sounds `chiptune` (a seamless 27.4 s loop, 16 bars at 140 BPM), `coin` (0.25 s, 988 to
  1319 Hz) and `blip` (a 45 ms pip). `KIT_VERSION` is 2, so the kit lives in `.veecut/sfx/v2/`.

## Command line (`veecut`)

Commands that work on a project take `-p/--project P` (a slug, a project folder or a
`project.json` path); without it they use the most recently changed project in the workspace.
CLI and MCP share this resolver. Folder and file paths may be absolute, relative to the
current directory or relative to the workspace. CLI `-p` works before or after the
subcommand. Without `--workspace`, an existing folder or `project.json` path discovers its
owning workspace, even from outside a workspace. An explicit `--workspace DIR` is
authoritative; MCP stays rooted in its server workspace. Both reject project paths outside
the selected workspace, including symlink escapes. Rooted MCP also confines what it writes
(`frame`/`frames`/`render` `out`) and text files it reads (`edit` `copy`) to the workspace, and
never writes into `inbox/`; MCP `ops` takes JSON only. Parse errors give the line number and
reason without echoing input. CLI `render --out` refuses paths inside `inbox/`. Every command
accepts `--json` for machine-readable output.

```
veecut init [DIR]                     set up a workspace (folders, built-in skill, agent config)
veecut doctor                         check ffmpeg, transcription, the cut-out model, agents and the skills catalog
veecut edit [FILE...] [--kind K] [--aspect 9:16|16:9|4:5|1:1] [--broll FILE...] [--music FILE]
            [--voice FILE] [--copy TEXT|FILE] [--name NAME] [--hook TEXT] [--language CODE]
            [--clips N] [--no-render] [--find] [--motion hold|push|glide]
                                      run the recipe for kind K on the files, or the newest inbox video;
                                      defaults to talking-head-short (talking-head-long for horizontal style);
                                      prints projects, drafts, what it did and what needs review
veecut status                         kind, length, tracks and items, drafts, open notes
veecut add FILE... [--role source|broll|music|sfx|voice|image|logo]
                                      bring files into the project as assets; prints ids for add_clip
veecut transcript [--srt|--vtt|--raw]   sentence source/timeline ranges to milliseconds; captions as SRT/VTT; raw words
veecut transcript-search "PHRASE"     exact spoken-word matches with word timings and 1-based occurrences
veecut cut-phrase "PHRASE" [--occurrence N]
                                      remove one selected match; refuses ambiguous cuts
veecut ops JSON|--file F|-            apply Editor API ops atomically; layout changes also report actual caption bounds
veecut schema                         JSON schema of the ops
veecut undo / veecut redo
veecut render [--final] [--aspect A] [--range A-B] [--out FILE]
                                      draft to outbox/<slug>-v<n>.mp4, final to outbox/<slug>-final.mp4
veecut frame [--at T] [--width W] [--out FILE]    composited PNG plus caption placement; defaults T=0, W=540
veecut frames [--every S] [--out FILE]            contact sheet; default interval max(length/12, 0.5) seconds
                                      check PNGs default to project .cache; inbox/outbox destinations are refused,
                                      including symlink aliases, before rendering
veecut captions [--rebuild] [--mode word_pop|highlight|pills|subtitle|off]   (the parser also takes reveal)
veecut note "NOTE" [--at T] [--rule]             pin a note; --rule also saves it to my-rules.md
veecut rules [add "RULE" --words "USER WORDS"]   list or add lasting rules
veecut style [show|set KEY VALUE]
veecut skills [sync|list [--installed]|search Q|show REPO|install REPO [--force]|update [REPO]|remove NAME|REPO|recommend [--kind K]]
veecut skills add SOURCE             a folder, .zip, .tar.gz, GitHub owner/repo, Google Drive or https link; copied and
                                      inspected, never run; notes list scripts and licence
veecut skills new NAME [--description T] [--from-project] [-p P]   own skill: SKILL.md + veecut.toml look
veecut skills check NAME              look parses, files exist, fonts load, script and licence notes
veecut skills apply NAME [-p P]       apply the look to a project: one undo step
veecut edit --skill NAME              style a recipe edit with an installed skill's look
veecut qa FILE [--words]              --words re-transcribes the render (local unless the config picked Groq) and
                                      lists missing or changed words, with a cut within 0.25 s or a sound
                                      starting within 0.15 s, and a verdict
veecut find "IDEA" [--images|--videos] [--from nasa,commons,archive,openverse,pexels] [--count N]
                                      numbered candidates with licence and flags (N per provider, 1 to 20,
                                      default 4), saved to .veecut/find/last.json with the contact sheet last.png
veecut fetch N|ID [--role broll|image|music]
                                      download a candidate (index from the last find, or its id) into footage/
                                      and add it to the project with its source and licence
veecut sources [--approve ASSET] [--flag ASSET --reason TEXT] [--credits]
                                      rights table; --approve is the user's step after checking the rights
                                      and needs an interactive terminal (agents cannot run it);
                                      a final render refuses flagged sources nobody approved
veecut sfx [--regenerate]             the built-in synthesized sound kit: each file's name, length, hit
                                      time and role, and its folder; writes missing files first
veecut cutout --at T --dur D [--outline W] [--mega WORD] [--world FILE|ASSET]
                                      cut the speaker out of one main-track clip there, with an optional mega
                                      word and picture behind; downloads MODNet (about 26 MB) on first use
veecut agent "PROMPT" [--with opencode|claude|codex|gemini] [--model M] [--effort E]
veecut agent --models [--with AGENT]    built-in model and effort choices; OpenCode also queries its installed CLI
veecut mcp                            MCP server on stdio (tools below)
veecut studio                         open the desktop editor
veecut fonts                          list caption and text fonts
```

MCP tools: `status`, `add`, `edit`, `ops`, `schema`, `render`, `frame`, `frames`, `transcript`,
`transcript_search`, `cut_phrase`, `captions`, `note`, `undo`, `redo`, `skills_search`,
`skills_install`, `skills_installed`, `skills_check`, `skills_apply`, `skills_new`, `find_footage`,
`fetch_footage`, `sources`, `cutout`, `verify_words`. There is no MCP `skills add`: installing outside
content stays a human action. `skills_apply` takes `project` and an installed skill `name`; `edit` takes `skill`.
`project` accepts the same selectors as CLI `-p`; `frame` and `frames`
also return the PNG as an image. `transcript_search` takes `phrase`; `cut_phrase` adds
optional 1-based `occurrence`.
`edit` takes optional `motion` (`hold`, `push` or `glide`) for `explainer` only,
matching CLI `--motion`; omission keeps original recipe motion.
`veecut edit --kind documentary [--find]` builds the documentary edit; `--find` (MCP `edit` `find`)
sources footage for lines without `--broll` material from the licence-tracked providers.
`find_footage` takes `idea`, `images`, `videos`, `from` and `count` and returns the contact sheet as an
image; `fetch_footage` takes `item` and `role`; `sources` takes `flag`, `reason` and `credits` and has
no approve field, because approving is the user's decision; `cutout` takes `at`, `dur`, `outline`,
`mega` and `world`; `verify_words` takes `file` (a path or a name in `outbox/`) and is `qa --words`.
CLI and MCP share these defaults; explicit frame-sheet intervals are clamped to at least
0.1 seconds. MCP rejects unknown fields and reports missing required arguments.
Redo uses its own tool; `undo` rejects a `redo` argument.

## Evaluation harness (`veecut-eval`)

The unpublished harness depends on the engine, not the CLI or Studio. Its commands are
`baselines`, `run`, `ladder`, `recheck` and `report`; root and manifest overrides precede the command.
Filters select fixture IDs, kinds and lengths, with task and level filters for agent runs.
The fixture manifest remains outside git. No harness operation writes into fixture media.

Baselines run the matching engine recipe with rendering disabled and local CPU transcription.
Each has its own workspace, linked inbox, saved report, log and completion record under
`~/veecut-eval/work/baselines/<id>`. Completed baselines are reusable; incomplete attempts
remain available when retried. Each agent trial copies its baseline into `work/runs/`,
materializes independent inbox files and rebases project paths. Level 1 starts without a
project. Other levels use the same `agent::studio_prompt` as Studio.
Private task expectations, logs, answers and frozen before-state evidence live in
`results/artifacts/<run-id>/`, not in the agent workspace. Per-task setup must produce
a valid project that fails the requested check before dispatch; an unchanged baseline
cannot earn a pass. Visual setup preserves independent face truth while making the
drawn captions overlap the mouth, so the task requires a repair.

Tasks are code templates filled from manifest notes and frozen baseline evidence. Checks
cover kind/canvas/source identity, exact style changes, kept source ranges, timed assets,
kind-specific edits and caption/mouth geometry. Cue timestamps alone are not word timing.
Visual truth is disclosed as a detector-derived proxy; missing evidence fails as `check`.
Every check variant has passing and failing project tests.
Unrelated-field comparisons tolerate numeric roundoff up to `1e-6` and ignore derived
main-track starts. Recipes can save float noise that later ops normalize; this must not
turn a correct edit into a check failure.

The runner calls `agent::run` with an explicit selection and a Stop watchdog: 600 seconds,
or 1,200 for level 1 on long fixtures, unless overridden. It records one JSONL row per trial
under `results/YYYY-MM-DD.jsonl`: fixture, kind, length, task, level, agent, model, effort,
trial, pass, check detail, seconds, input/output/cached tokens, reported cost, claimed
success and failure class. Logs and failing workspaces remain inspectable. Failure classes
are `model`, `app`, `environment` and `check`; classification includes supporting evidence.
Rows also carry `first_request_input_tokens` when reported, and the full parsed `Usage`
object under `evidence.usage`. First-request input is a separate measure, not additional spend.

Custom agent commands are refused because they override model selection. Free runs accept
the known free models; paid runs require `--allow-paid`. Screening runs 49 cells sequentially:
nine free models at their default variant on five tasks and four image-capable models on
the visual task. Existing cells are skipped; `--rerun` preserves earlier generations.
Ladders require two passes, trying a third trial on a split. Reports separate one-trial
screening outcomes from reliable rungs and show medians and failure evidence.

`ladder --rungs` filters before probing or running models. Its comma-separated selectors
accept case-insensitive providers `free`/`opencode`, `claude`, `codex`, or `MODEL[@EFFORT]`.
A model alone selects all its ladder efforts; `@default` selects no explicit effort.
Invalid selectors list valid rung IDs, and paid selections without `--allow-paid` fail
before any run. Omitting `--rungs` keeps the default ladder.

Each selected free model receives at most one small real availability request through
`agent::run` per batch, just before its first trial. `--probe-timeout` is 20 seconds by
default (range 5 to 600). Environment failures, quota/model availability errors or a
deadline without a reply defer the rung. A reported exhausted free allowance defers all
remaining selected free rungs without more probes; unavailable-model errors and timeouts
stay per model. Only that probe's own output is read, never OpenCode's shared log; paid
models are not probed. Deferrals go to `results/deferred/YYYY-MM-DD.jsonl` with the reason,
probe duration, workspace and log tail. They are not trial rows and do not affect
reliability, recommendations or result totals. Paid escalation waits while a selected free
rung for that task is deferred. A paid-only `--rungs` selection skips free probes entirely.

`report` lists open deferrals under "Deferred rungs (not model trials)". Rerun the same
command to retry. A newer real attempt for the same fixture/task/agent/model/effort clears
the open deferral; previous evidence remains on disk.

`recheck` evaluates retained artifacts without a model call, appending a linked row with
`rechecked = true` and an incremented generation. It preserves source-run costs rather
than counting them as new spend. Reports and reliability calculations identify the original
attempt so an offline recheck cannot become an independent second trial. Runs that read
private checker expectations keep `invalid = true` across rechecks and require live reruns.

## The built-in skill

`crates/veecut-engine/skill/` holds the agent skill copied into every workspace by
`veecut init`: `SKILL.md` (router), `workflows/<kind>.md` (one per `VideoKind::slug()`) and
`reference/{editor-api,notes-and-rules,style-setup,sourcing,sound,skills}.md`. `skills.md` tells
agents how to turn any skill into a look pack. `skill/looks/arcade/` and `skill/looks/editorial/`
(`SKILL.md` and `veecut.toml` each) are installed as `veecut-arcade` and `veecut-editorial`. The
workspace module embeds all of these with `include_str!` and rewrites them in both skill roots on
every init. All workflow and reference paths resolve relative to the loaded `SKILL.md` directory
(`.agents/skills/veecut/` or `.claude/skills/veecut/`), never the workspace root.

## Credits

See [CREDITS.md](../CREDITS.md). Code ideas come from free247editor and Herk Edit Kit (MIT),
editor ideas from OpenCut (MIT, name and logo excluded), workflow ideas from the projects in
awesome-claude-video-skills (list CC0; each project under its own license), the documentary
edit's ideas and measurements from NextPhase Editor V1 (no code, text, tokens or sounds copied),
and the arcade and editorial looks' timings, sizes and colours from Jad M.H's Arcade Motion
Graphics and Motion Graphics V3 (no licence, so nothing copied). The cut-out model is MODNet
(Apache-2.0), downloaded on first use; Press Start 2P is OFL from upstream Google Fonts.
