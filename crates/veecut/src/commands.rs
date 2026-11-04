//! Every command, shared by the command line and the MCP server. Each returns an [`Out`]: text
//! for people, JSON for agents, and a PNG when the command made a picture.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Component, Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result, anyhow, bail};
use clap::{Args, Subcommand};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use veecut_engine::agent::{self, AgentKind};
use veecut_engine::config::{self, Config};
use veecut_engine::graphics::{self, FaceFit, Graphics};
use veecut_engine::ops::{self, Op};
use veecut_engine::project::{
    AssetKind, AssetRole, Canvas, CaptionMode, ItemBody, PROJECT_FILE, Project, Source, TrackKind,
    VideoKind,
};
use veecut_engine::recipes::{self, EditRequest, ExplainerMotion};
use veecut_engine::render::{self, Quality, RenderOptions};
use veecut_engine::skills;
use veecut_engine::style::{self, Format, Style};
use veecut_engine::transcript::Transcript;
use veecut_engine::workspace::Workspace;
use veecut_engine::{captions, media, qa, transcribe};

/// A command's result.
pub struct Out {
    pub text: String,
    pub json: Value,
    pub png: Option<Vec<u8>>,
}

impl Out {
    pub(crate) fn new(text: impl Into<String>, json: Value) -> Self {
        Out {
            text: text.into(),
            json,
            png: None,
        }
    }
    pub fn quiet() -> Self {
        Out::new("", Value::Null)
    }
}

#[derive(Args, Deserialize, JsonSchema, Default, Clone)]
#[serde(default)]
pub struct ProjectArg {
    /// Project slug, folder or project.json path. Default: the most recently changed project.
    #[arg(from_global)]
    pub project: Option<String>,
}

#[derive(Args, Deserialize, JsonSchema, Default)]
#[serde(default)]
pub struct EditArgs {
    /// Recordings to edit (paths, or names of files in inbox/). Default: the newest video in inbox/.
    pub files: Vec<PathBuf>,
    /// talking-head-short, talking-head-long, tutorial, podcast, clips, vlog, explainer, promo,
    /// music-video or documentary.
    #[arg(long, short)]
    pub kind: Option<String>,
    /// 9:16, 16:9, 4:5 or 1:1.
    #[arg(long)]
    pub aspect: Option<String>,
    /// Cutaway footage and images.
    #[arg(long, num_args = 1..)]
    pub broll: Vec<PathBuf>,
    #[arg(long)]
    pub music: Option<PathBuf>,
    /// Narration or a separate microphone recording.
    #[arg(long)]
    pub voice: Option<PathBuf>,
    /// Explainer visual motion: hold, push or glide. Omit to keep recipe defaults.
    #[arg(long, value_name = "MOTION")]
    pub motion: Option<ExplainerMotion>,
    /// Text the edit shows (promo lines, list items, LRC lyrics), or a path to a text file
    /// (inside the workspace over MCP).
    #[arg(long)]
    pub copy: Option<String>,
    #[arg(long)]
    pub name: Option<String>,
    /// Hook text for the first seconds.
    #[arg(long)]
    pub hook: Option<String>,
    /// Spoken language code, such as en or tl.
    #[arg(long)]
    pub language: Option<String>,
    /// How many clips the clips recipe makes.
    #[arg(long)]
    pub clips: Option<usize>,
    /// Build the project but do not render a draft.
    #[arg(long)]
    pub no_render: bool,
    /// Documentary edits: search free, licence-tracked footage for lines without --broll files.
    #[arg(long)]
    pub find: bool,
}

#[derive(Args, Deserialize, JsonSchema, Default)]
#[serde(default)]
pub struct AddArgs {
    /// Files to bring into the project (paths, or names of files in inbox/).
    pub files: Vec<PathBuf>,
    /// What they are for: source, broll, music, sfx, voice, image or logo. Default: broll for
    /// video, image for pictures, music for audio.
    #[arg(long)]
    pub role: Option<String>,
    #[command(flatten)]
    #[serde(flatten)]
    pub project: ProjectArg,
}

#[derive(Args, Deserialize, JsonSchema, Default)]
#[serde(default)]
pub struct TranscriptArgs {
    /// Captions of the edit as SubRip subtitles.
    #[arg(long)]
    pub srt: bool,
    /// Captions of the edit as WebVTT subtitles.
    #[arg(long)]
    pub vtt: bool,
    /// The raw word-level transcript.
    #[arg(long)]
    pub raw: bool,
    #[command(flatten)]
    #[serde(flatten)]
    pub project: ProjectArg,
}

#[derive(Args, Deserialize, JsonSchema, Default)]
pub struct TranscriptSearchArgs {
    /// Exact spoken phrase; ignores case and punctuation, never uses fuzzy matching.
    pub phrase: String,
    #[command(flatten)]
    #[serde(flatten)]
    pub project: ProjectArg,
}

#[derive(Args, Deserialize, JsonSchema, Default)]
pub struct CutPhraseArgs {
    /// Exact spoken phrase to remove from the kept timeline.
    pub phrase: String,
    /// One-based match number from transcript-search, required when the phrase repeats.
    #[arg(long)]
    pub occurrence: Option<usize>,
    #[command(flatten)]
    #[serde(flatten)]
    pub project: ProjectArg,
}

#[derive(Args, Deserialize, JsonSchema, Default)]
#[serde(default)]
pub struct OpsArgs {
    /// One op, a JSON array of ops, or JSON lines. On the command line `-` reads standard input.
    pub ops: Option<String>,
    /// Read the ops from a file. Not offered over MCP: tools send the ops as JSON.
    #[arg(long)]
    #[serde(skip)]
    pub file: Option<PathBuf>,
    #[command(flatten)]
    #[serde(flatten)]
    pub project: ProjectArg,
}

#[derive(Args, Deserialize, JsonSchema, Default)]
#[serde(default)]
pub struct RenderArgs {
    /// Final quality to outbox/<slug>-final.mp4.
    #[arg(long = "final")]
    #[serde(rename = "final")]
    pub final_: bool,
    /// Render another shape: 9:16, 16:9, 4:5 or 1:1. Text and captions re-place themselves.
    #[arg(long)]
    pub aspect: Option<String>,
    /// Part of the timeline in seconds, like 0-5.
    #[arg(long)]
    pub range: Option<String>,
    /// Output file; default outbox/. Never inside inbox/; MCP keeps it inside the workspace.
    #[arg(long)]
    pub out: Option<PathBuf>,
    #[command(flatten)]
    #[serde(flatten)]
    pub project: ProjectArg,
}

#[derive(Args, Deserialize, JsonSchema, Default)]
#[serde(default)]
pub struct FrameArgs {
    /// Timeline time in seconds. Default: 0.
    #[arg(long, default_value_t = 0.0)]
    pub at: f64,
    /// Width in pixels (default 540).
    #[arg(long)]
    pub width: Option<u32>,
    /// Check PNG destination; default project .cache. inbox/ and outbox/ are protected; MCP keeps
    /// it inside the workspace.
    #[arg(long)]
    pub out: Option<PathBuf>,
    #[command(flatten)]
    #[serde(flatten)]
    pub project: ProjectArg,
}

#[derive(Args, Deserialize, JsonSchema, Default)]
#[serde(default)]
pub struct FramesArgs {
    /// Seconds between frames. Default: about 12 frames across the edit.
    #[arg(long)]
    pub every: Option<f64>,
    /// Check PNG destination; default project .cache. inbox/ and outbox/ are protected; MCP keeps
    /// it inside the workspace.
    #[arg(long)]
    pub out: Option<PathBuf>,
    #[command(flatten)]
    #[serde(flatten)]
    pub project: ProjectArg,
}

#[derive(Args, Deserialize, JsonSchema, Default)]
#[serde(default)]
pub struct CaptionsArgs {
    /// Rebuild captions from the transcript.
    #[arg(long)]
    pub rebuild: bool,
    /// word_pop, highlight, pills, subtitle or off.
    #[arg(long)]
    pub mode: Option<String>,
    #[command(flatten)]
    #[serde(flatten)]
    pub project: ProjectArg,
}

#[derive(Args, Deserialize, JsonSchema, Default)]
pub struct NoteArgs {
    /// The note, in the user's words.
    pub note: String,
    /// Timeline time the note is about.
    #[arg(long)]
    pub at: Option<f64>,
    /// Also save it as a lasting rule in my-rules.md.
    #[arg(long)]
    #[serde(default)]
    pub rule: bool,
    #[command(flatten)]
    #[serde(flatten)]
    pub project: ProjectArg,
}

#[derive(Subcommand)]
pub enum RulesCommand {
    /// Save a lasting rule with the user's own words.
    Add {
        rule: String,
        #[arg(long)]
        words: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum StyleCommand {
    /// Print my-style.toml.
    Show,
    /// Change one setting, for example `captions.position lower` or `pace tight`.
    Set { key: String, value: String },
}

#[derive(Subcommand)]
pub enum SkillsCommand {
    /// Download the latest catalog from awesome-claude-video-skills.
    Sync,
    /// Catalog entries, or the skills installed in this workspace with --installed.
    List {
        #[arg(long)]
        category: Option<String>,
        #[arg(long)]
        installed: bool,
    },
    /// Search the catalog.
    Search {
        query: String,
        #[arg(long)]
        category: Option<String>,
    },
    /// One catalog entry and its install state.
    Show { repo: String },
    /// Install a catalog repo's skills into this workspace.
    Install {
        repo: String,
        /// Install even when the catalog does not grade it SAFE.
        #[arg(long)]
        force: bool,
    },
    /// Update installed skills whose upstream moved (all, or one repo).
    Update { repo: Option<String> },
    /// Remove an installed repo's skills.
    Remove { repo: String },
    /// The best catalog skills for a kind of video.
    Recommend {
        #[arg(long)]
        kind: Option<String>,
    },
}

#[derive(Args, Deserialize, JsonSchema, Default)]
pub struct SkillsSearchArgs {
    pub query: String,
    pub category: Option<String>,
}

#[derive(Args, Deserialize, JsonSchema, Default)]
pub struct SkillsInstallArgs {
    /// owner/name of a repo in the catalog.
    pub repo: String,
    #[serde(default)]
    pub force: bool,
}

#[derive(Args, Deserialize, JsonSchema, Default)]
#[serde(default)]
pub struct AgentArgs {
    /// What to ask the agent. Not needed with --models.
    #[arg(required_unless_present = "models")]
    pub prompt: Option<String>,
    /// opencode (default), claude, codex or gemini.
    #[arg(long = "with")]
    #[serde(rename = "with")]
    pub with: Option<String>,
    /// Model for this run: an id from --models, any id the agent accepts, or `default` for
    /// VeeCut's default (the free GLM model for OpenCode, the agent's own choice for the rest).
    /// Without it, the model saved in config.toml.
    #[arg(long)]
    pub model: Option<String>,
    /// Effort for this run: see --models, or `default` for the agent's own effort.
    #[arg(long)]
    pub effort: Option<String>,
    /// List the models and efforts known for the agent (all agents without --with), then stop.
    #[arg(long)]
    pub models: bool,
}

fn kind_of(s: &str) -> Result<VideoKind> {
    VideoKind::from_slug(s).ok_or_else(|| {
        anyhow!(
            "unknown kind {s:?}; use one of: {}",
            VideoKind::ALL.map(|k| k.slug()).join(", ")
        )
    })
}

fn aspect_of(s: &str) -> Result<Canvas> {
    Canvas::from_aspect(s)
        .ok_or_else(|| anyhow!("unknown aspect {s:?}; use 9:16, 16:9, 4:5 or 1:1"))
}

pub(crate) fn rel(ws: &Workspace, path: &Path) -> String {
    path.strip_prefix(&ws.root)
        .unwrap_or(path)
        .display()
        .to_string()
}

/// `path` made absolute with every existing part resolved, so a symlink cannot hide where a file
/// lands; parts that do not exist yet stay as given.
pub(crate) fn resolved(path: &Path) -> Result<PathBuf> {
    let absolute = std::path::absolute(path)?;
    let mut out = PathBuf::new();
    for part in absolute.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            _ => {
                out.push(part);
                match std::fs::symlink_metadata(&out) {
                    Ok(_) => out = out.canonicalize()?,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
            }
        }
    }
    Ok(out)
}

fn frame_output_path(ws: &Workspace, path: &Path) -> Result<PathBuf> {
    let out = resolved(path)?;
    for protected in [ws.inbox(), ws.outbox()] {
        if out.starts_with(resolved(&protected)?) {
            bail!(
                "check frames cannot be saved in inbox/ or outbox/; omit --out to use the project's .cache"
            );
        }
    }
    Ok(out)
}

/// The original recordings in inbox/ are never changed, so no render lands there.
fn render_output_path(ws: &Workspace, path: &Path) -> Result<PathBuf> {
    let out = resolved(path)?;
    if out.starts_with(resolved(&ws.inbox())?) {
        bail!(
            "renders cannot be saved in inbox/, where the original recordings stay unchanged; omit --out to render into outbox/"
        );
    }
    Ok(out)
}

pub(crate) const APPROVAL_IS_HUMAN: &str = "Approving or unflagging footage is your decision: use Studio's Sources tab or run `veecut sources --approve` in a terminal.";

/// Every asset's id and resolved file; asset paths are relative to the project folder or absolute.
fn asset_files(dir: &Path, p: &Project) -> Result<Vec<(String, PathBuf)>> {
    p.assets
        .iter()
        .map(|a| Ok((a.id.clone(), resolved(&dir.join(&a.path))?)))
        .collect()
}

/// Ops come from agents and scripts, so they may not grant approval, change a flag, delete or
/// replace a source record, or replace the rights table or the whole project. A record covers
/// its asset's file too: ops may not point that asset elsewhere, drop it, or give the file to
/// another asset. Adding a new, unapproved record and editing other source fields still work.
/// `files` is [`asset_files`] before the ops (empty when there were no records).
fn check_rights(
    dir: &Path,
    before: &BTreeMap<String, Source>,
    files: &[(String, PathBuf)],
    list: &[Op],
    after: &Project,
) -> Result<()> {
    let replaces = list.iter().any(|op| {
        let Op::Set { path, .. } = op else {
            return false;
        };
        let keys: Vec<String> = path
            .split('/')
            .skip(1)
            .map(|k| k.replace("~1", "/").replace("~0", "~"))
            .collect();
        match keys.as_slice() {
            [] => true,
            [key] => key.is_empty() || key == "sources",
            [key, id] => key == "sources" && before.contains_key(id),
            _ => false,
        }
    });
    let changed = before.iter().any(|(id, old)| {
        after.sources.get(id).is_none_or(|new| {
            (new.approved && !old.approved)
                || new.flag != old.flag
                || new.flag_reason != old.flag_reason
        })
    }) || after
        .sources
        .iter()
        .any(|(id, new)| new.approved && !before.contains_key(id));
    if replaces || changed {
        bail!(APPROVAL_IS_HUMAN);
    }
    if before.is_empty() {
        return Ok(());
    }
    let now = asset_files(dir, after)?;
    let had = |id: &str, file: &Path| files.iter().any(|(i, f)| i == id && f == file);
    let moved = files
        .iter()
        .filter(|(id, _)| before.contains_key(id))
        .any(|(id, file)| {
            !now.iter().any(|(i, f)| i == id && f == file)
                || now.iter().any(|(i, f)| f == file && i != id && !had(i, f))
        });
    if moved {
        bail!(APPROVAL_IS_HUMAN);
    }
    Ok(())
}

fn time(t: f64) -> String {
    let t = t.max(0.0);
    format!("{}:{:04.1}", (t / 60.0) as u64, t % 60.0)
}

fn time_ms(t: f64) -> String {
    let ms = (t.max(0.0) * 1000.0).round() as u64;
    format!("{}:{:02}.{:03}", ms / 60_000, ms / 1000 % 60, ms % 1000)
}

fn project_transcript(dir: &Path, project: &Project) -> Result<Transcript> {
    let transcript = Transcript::load(&dir.join("transcript.json"))
        .context("cannot read transcript.json; select a speech project, for example --project projects/my-edit/project.json")?;
    if project.asset(&transcript.asset).is_none() {
        bail!(
            "transcript asset {:?} is not in this project; use a transcript recorded for this project's assets",
            transcript.asset
        );
    }
    Ok(transcript)
}

/// A workspace plus project lookup.
pub struct Ctx {
    pub ws: Workspace,
}

impl Ctx {
    pub fn open(workspace: Option<&Path>) -> Result<Ctx> {
        let start = match workspace {
            Some(w) => w.to_path_buf(),
            None => std::env::current_dir()?,
        };
        let ws = Workspace::open(&start).context(
            "this folder is not a VeeCut workspace; run `veecut init` in your videos folder, or pass --workspace",
        )?;
        config::load_dotenv(&ws.root);
        Ok(Ctx { ws })
    }

    /// CLI project paths establish their owning workspace unless --workspace was explicit.
    pub fn open_project(workspace: Option<&Path>, project: Option<&str>) -> Result<Ctx> {
        if workspace.is_none() {
            if let Some(path) = project.map(Path::new).filter(|path| path.exists()) {
                return Self::open(Some(path));
            }
        }
        Self::open(workspace)
    }

    pub fn project(&self, arg: &ProjectArg) -> Result<(PathBuf, Project)> {
        let dir = self.ws.resolve_project(arg.project.as_deref())?;
        let project = Project::load(&dir.join(PROJECT_FILE))?;
        Ok((dir, project))
    }

    /// An input file: a path as given, or a name inside inbox/ or the workspace.
    pub(crate) fn input(&self, path: &Path) -> Result<PathBuf> {
        for candidate in [
            path.to_path_buf(),
            self.ws.inbox().join(path),
            self.ws.root.join(path),
        ] {
            if candidate.is_file() {
                return Ok(std::path::absolute(candidate)?);
            }
        }
        bail!("{} not found (also looked in inbox/)", path.display())
    }

    pub fn edit(&self, args: EditArgs, progress: &mut dyn FnMut(&str)) -> Result<Out> {
        let style = Style::load(&self.ws.style_path())?;
        let kind = match &args.kind {
            Some(k) => kind_of(k)?,
            None if style.format == Format::Horizontal => VideoKind::TalkingHeadLong,
            None => VideoKind::TalkingHeadShort,
        };
        let mut req = EditRequest::new(kind);
        req.sources = args
            .files
            .iter()
            .map(|f| self.input(f))
            .collect::<Result<_>>()?;
        req.broll = args
            .broll
            .iter()
            .map(|f| self.input(f))
            .collect::<Result<_>>()?;
        req.music = args.music.as_deref().map(|f| self.input(f)).transpose()?;
        req.voice = args.voice.as_deref().map(|f| self.input(f)).transpose()?;
        req.motion = args.motion;
        req.copy = match args.copy {
            Some(c) if Path::new(&c).is_file() => Some(std::fs::read_to_string(&c)?),
            // A shell passes "Line one\nLine two" with a literal backslash-n.
            Some(c) if !c.contains('\n') => Some(c.replace("\\n", "\n")),
            other => other,
        };
        req.canvas = args.aspect.as_deref().map(aspect_of).transpose()?;
        req.name = args.name;
        req.hook = args.hook;
        req.language = args.language;
        if let Some(n) = args.clips {
            req.clips = n;
        }
        req.render = !args.no_render;
        req.find = args.find;
        let report = recipes::edit(&self.ws, &req, progress)?;
        let mut text = String::new();
        for line in &report.summary {
            let _ = writeln!(text, "- {line}");
        }
        for d in &report.drafts {
            let _ = writeln!(text, "Draft: {}", rel(&self.ws, d));
        }
        for p in &report.projects {
            let _ = writeln!(text, "Project: {}", rel(&self.ws, p));
        }
        if !report.review.is_empty() {
            let _ = writeln!(text, "Needs a look:");
            for r in &report.review {
                let _ = writeln!(text, "- {r}");
            }
        }
        let _ = write!(
            text,
            "Next: watch the draft, then turn notes into `veecut ops`, and `veecut render` again."
        );
        Ok(Out::new(text, serde_json::to_value(&report)?))
    }

    /// Brings files into a project as assets so ops can place them.
    pub fn add(&self, args: AddArgs) -> Result<Out> {
        if args.files.is_empty() {
            bail!("name at least one file to add");
        }
        let role = args
            .role
            .as_deref()
            .map(|r| {
                serde_json::from_value::<AssetRole>(json!(r.trim().to_lowercase())).map_err(|_| {
                    anyhow!(
                        "unknown role {r:?}; use source, broll, music, sfx, voice, image or logo"
                    )
                })
            })
            .transpose()?;
        let (dir, mut p) = self.project(&args.project)?;
        let mut text = String::new();
        let mut added = Vec::new();
        for file in &args.files {
            let path = self.input(file)?;
            let info = media::probe(&path)?;
            let role = role.unwrap_or(match info.kind {
                AssetKind::Video => AssetRole::Broll,
                AssetKind::Image => AssetRole::Image,
                AssetKind::Audio => AssetRole::Music,
            });
            let id = self.ws.add_asset(&mut p, &dir, &path, role)?;
            let _ = writeln!(
                text,
                "{id}: {} ({:?}, {:?}, {:.1} s)",
                rel(&self.ws, &path),
                info.kind,
                role,
                info.duration
            );
            added.push(json!({ "id": id, "path": rel(&self.ws, &path), "kind": info.kind, "role": role, "duration": info.duration }));
        }
        self.ws.save_project(&dir, &p)?;
        if let Some(first) = added.first().and_then(|a| a["id"].as_str()) {
            let _ = write!(
                text,
                "Place it with an op, for example {{\"op\":\"add_clip\",\"asset\":\"{first}\",\"start\":5.0,\"duration\":3.0}}"
            );
        }
        Ok(Out::new(text, json!({ "added": added })))
    }

    pub fn status(&self, args: ProjectArg) -> Result<Out> {
        let (dir, p) = self.project(&args)?;
        let slug = dir
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let mut text = format!(
            "{} ({}), {}x{} at {} fps, {:.1} s\n",
            p.name,
            p.kind.label(),
            p.canvas.width,
            p.canvas.height,
            p.canvas.fps,
            p.duration()
        );
        let mut tracks = Vec::new();
        for t in &p.tracks {
            let _ = writeln!(
                text,
                "  {} [{}] {:?}: {} items{}",
                t.name,
                t.id,
                t.kind,
                t.items.len(),
                if t.hidden {
                    " (hidden)"
                } else if t.muted {
                    " (muted)"
                } else {
                    ""
                }
            );
            let items: Vec<Value> = t
                .items
                .iter()
                .map(|i| {
                    let what = match &i.body {
                        ItemBody::Clip(c) => json!({ "type": "clip", "asset": c.asset, "src_in": c.src_in, "speed": c.speed, "zooms": c.zooms.len() }),
                        ItemBody::Text(x) => json!({ "type": "text", "text": x.text, "preset": x.preset }),
                        ItemBody::Caption(c) => json!({ "type": "caption", "words": c.words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ") }),
                    };
                    json!({ "id": i.id, "start": i.start, "duration": i.duration, "item": what })
                })
                .collect();
            tracks.push(json!({ "id": t.id, "kind": t.kind, "name": t.name, "muted": t.muted, "hidden": t.hidden, "items": items }));
        }
        for a in &p.assets {
            let _ = writeln!(
                text,
                "  asset {}: {} ({:?}, {:?}, {:.1} s)",
                a.id,
                a.path.display(),
                a.kind,
                a.role,
                a.duration
            );
        }
        let _ = writeln!(
            text,
            "  look {:?}; captions {:?} at {:.2} of the height",
            p.look.preset, p.captions.mode, p.captions.position
        );
        let mut drafts: Vec<PathBuf> = std::fs::read_dir(self.ws.outbox())
            .map(|rd| {
                rd.flatten()
                    .map(|e| e.path())
                    .filter(|f| {
                        f.file_name()
                            .is_some_and(|n| n.to_string_lossy().starts_with(&format!("{slug}-")))
                    })
                    .collect()
            })
            .unwrap_or_default();
        drafts.sort();
        for d in &drafts {
            let _ = writeln!(text, "  draft {}", rel(&self.ws, d));
        }
        for m in p.markers.iter().filter(|m| !m.done) {
            let _ = writeln!(text, "  note at {}: {}", time(m.at), m.note);
        }
        let _ = write!(text, "  folder {}", rel(&self.ws, &dir));
        let json = json!({
            "project": rel(&self.ws, &dir), "name": p.name, "kind": p.kind, "canvas": p.canvas, "duration": p.duration(),
            "look": p.look, "captions": p.captions, "audio": p.audio, "assets": p.assets, "tracks": tracks,
            "markers": p.markers, "drafts": drafts.iter().map(|d| rel(&self.ws, d)).collect::<Vec<_>>(),
            "transcript": dir.join("transcript.json").is_file(),
        });
        Ok(Out::new(text, json))
    }

    pub fn transcript(&self, args: TranscriptArgs) -> Result<Out> {
        let (dir, p) = self.project(&args.project)?;
        if args.srt || args.vtt {
            let text = if args.srt {
                captions::to_srt(&p)
            } else {
                captions::to_vtt(&p)
            };
            return Ok(Out::new(text.clone(), json!({ "text": text })));
        }
        let t = Transcript::load(&dir.join("transcript.json"))
            .context("this project has no transcript; `veecut captions --rebuild` needs one too")?;
        if args.raw {
            return Ok(Out::new(
                serde_json::to_string_pretty(&t)?,
                serde_json::to_value(&t)?,
            ));
        }
        let mut mapped = vec![None; t.words.len()];
        for word in t.placed_words(&p) {
            if let Some(slot) = word
                .id
                .strip_prefix('w')
                .and_then(|id| id.parse::<usize>().ok())
                .and_then(|index| mapped.get_mut(index))
            {
                slot.get_or_insert((word.timeline_start, word.timeline_end));
            }
        }
        let mut text =
            String::from("timeline_start  timeline_end  source_start  source_end  sentence\n");
        let mut rows = Vec::new();
        for s in t.sentences() {
            let start = mapped[s.first].map(|(start, _)| start);
            let end = mapped[s.end_word - 1].map(|(_, end)| end);
            let _ = writeln!(
                text,
                "{:>14}  {:>12}  {:>12}  {:>10}  {}",
                start.map(time_ms).unwrap_or_else(|| "(cut)".into()),
                end.map(time_ms).unwrap_or_else(|| "(cut)".into()),
                time_ms(s.start),
                time_ms(s.end),
                s.text
            );
            rows.push(json!({
                "timeline_start": start, "timeline_end": end,
                "source_start": (s.start * 1000.0).round() / 1000.0,
                "source_end": (s.end * 1000.0).round() / 1000.0,
                "text": s.text
            }));
        }
        Ok(Out::new(
            text,
            json!({ "asset": t.asset, "language": t.language, "sentences": rows }),
        ))
    }

    pub fn transcript_search(&self, args: TranscriptSearchArgs) -> Result<Out> {
        let (dir, p) = self.project(&args.project)?;
        let transcript = project_transcript(&dir, &p)?;
        let matches = transcript.find_phrase(&p, &args.phrase)?;
        let mut text = format!("{} exact phrase matches.\n", matches.len());
        for found in &matches {
            let _ = writeln!(
                text,
                "{}: timeline {}-{}, source {}-{}: {}",
                found.occurrence,
                time_ms(found.timeline_start),
                time_ms(found.timeline_end),
                time_ms(found.source_start),
                time_ms(found.source_end),
                found.sentence
            );
        }
        Ok(Out::new(
            text,
            json!({ "phrase": args.phrase, "matches": matches }),
        ))
    }

    pub fn cut_phrase(&self, args: CutPhraseArgs) -> Result<Out> {
        let (dir, mut p) = self.project(&args.project)?;
        let transcript = project_transcript(&dir, &p)?;
        let cut = transcript.cut_phrase(&mut p, &args.phrase, args.occurrence)?;
        self.ws.save_project(&dir, &p)?;
        Ok(Out::new(
            format!(
                "Cut phrase occurrence {}. Saved; `veecut undo` restores this change.",
                cut.cut.occurrence
            ),
            serde_json::to_value(&cut)?,
        ))
    }

    pub fn ops(&self, args: OpsArgs) -> Result<Out> {
        let input = match (&args.file, args.ops.as_deref()) {
            (Some(f), _) => {
                std::fs::read_to_string(f).with_context(|| format!("reading {}", f.display()))?
            }
            (None, Some("-")) => std::io::read_to_string(std::io::stdin())?,
            (None, Some(s)) => s.to_string(),
            (None, None) => bail!(
                "give the ops as JSON, with --file, or `-` for standard input; `veecut schema` shows them"
            ),
        };
        self.apply_ops(&input, &args.project)
    }

    /// Applies ops given as JSON text; MCP calls this directly, so it never reads files or stdin.
    pub(crate) fn apply_ops(&self, input: &str, project: &ProjectArg) -> Result<Out> {
        let list = ops::parse_ops(input)?;
        let (dir, mut p) = self.project(project)?;
        let before = p.sources.clone();
        let files = if before.is_empty() {
            Vec::new()
        } else {
            asset_files(&dir, &p)?
        };
        let done = ops::apply_all(&mut p, &list)?;
        check_rights(&dir, &before, &files, &list, &p)?;
        self.ws.save_project(&dir, &p)?;
        let mut text = String::new();
        for d in &done {
            let _ = writeln!(text, "- {}", d.summary);
        }
        let layout_changed = list.iter().any(|op| match op {
            Op::Set { path, .. } => {
                ["/captions", "/canvas", "/tracks", "/faces"]
                    .iter()
                    .any(|&prefix| {
                        path == prefix
                            || path
                                .strip_prefix(prefix)
                                .is_some_and(|s| s.starts_with('/'))
                    })
            }
            Op::AddMarker { .. } | Op::AddTrack { .. } => false,
            _ => true,
        });
        let layout = layout_changed.then(|| graphics::caption_report(&mut Graphics::new(), &p, 5));
        if let Some(report) = &layout {
            let _ = writeln!(
                text,
                "Captions requested at {:.3}: {} pages, {} safe-area clamps, {} moved/shrunk, {} hidden, {} unchecked, {} with detection gaps.",
                report.requested,
                report.pages,
                report.clamped,
                report.moved,
                report.hidden,
                report.unchecked,
                report.gaps
            );
            for page in &report.examples {
                let _ = writeln!(
                    text,
                    "  {} {:.2}-{:.2}s: {} box x={:.3} y={:.3} w={:.3} h={:.3}, size={:.3}, face={:?}.",
                    page.item,
                    page.start,
                    page.end,
                    if page.face == FaceFit::Hidden {
                        "hidden; proposed"
                    } else {
                        "drawn"
                    },
                    page.bounds.x,
                    page.bounds.y,
                    page.bounds.w,
                    page.bounds.h,
                    page.size,
                    page.face
                );
            }
        }
        let _ = write!(
            text,
            "Saved. Studio previews the change; `veecut frame` checks a still."
        );
        Ok(Out::new(
            text,
            json!({ "applied": done, "duration": p.duration(), "caption_layout": layout }),
        ))
    }

    pub fn undo(&self, args: ProjectArg, redo: bool) -> Result<Out> {
        let (dir, _) = self.project(&args)?;
        let p = if redo {
            self.ws.redo(&dir)?
        } else {
            self.ws.undo(&dir)?
        };
        let word = if redo { "Redid" } else { "Undid" };
        Ok(Out::new(
            format!(
                "{word} the last change; the project is {:.1} s long.",
                p.duration()
            ),
            json!({ "duration": p.duration() }),
        ))
    }

    pub fn render(&self, args: RenderArgs, progress: &mut dyn FnMut(&str)) -> Result<Out> {
        let (dir, mut p) = self.project(&args.project)?;
        // A final render refuses flagged found files nobody approved; drafts are never blocked.
        if args.final_ {
            veecut_engine::footage::check_final(&p)?;
        }
        let slug = dir
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let mut suffix = String::new();
        if let Some(a) = &args.aspect {
            let c = aspect_of(a)?;
            suffix = format!("-{}", c.aspect_name().replace(':', "x"));
            p.canvas.width = c.width;
            p.canvas.height = c.height;
        }
        let quality = if args.final_ {
            Quality::Final
        } else {
            Quality::Draft
        };
        let mut opts = RenderOptions::for_canvas(&p.canvas, quality);
        if let Some(r) = &args.range {
            let (a, b) = r.split_once('-').context("--range looks like 0-5")?;
            opts.range = Some((
                a.trim().parse().context("--range start")?,
                b.trim().parse().context("--range end")?,
            ));
        }
        let out = match args.out {
            Some(o) => render_output_path(&self.ws, &o)?,
            None if args.final_ => self.ws.outbox().join(format!("{slug}-final{suffix}.mp4")),
            None => {
                let next = self.ws.next_draft_path(&slug);
                let stem = next
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                next.with_file_name(format!("{stem}{suffix}.mp4"))
            }
        };
        let mut last = -1.0;
        let report = render::export(&p, &dir, &out, &opts, &mut |pr| {
            if pr.total > 0.0 && pr.done - last >= pr.total / 10.0 {
                last = pr.done;
                progress(&format!("{} {:.0}%", pr.stage, 100.0 * pr.done / pr.total));
            }
            true
        })?;
        let check = qa::check(&out)?;
        let mut text = format!(
            "Rendered {} ({:.1} s, {}x{}) in {:.1} s.",
            rel(&self.ws, &out),
            report.duration,
            report.width,
            report.height,
            report.seconds_taken
        );
        for problem in &check.problems {
            let _ = write!(text, "\nCheck: {problem}");
        }
        Ok(Out::new(
            text,
            json!({ "path": rel(&self.ws, &out), "duration": report.duration, "width": report.width, "height": report.height, "qa": check }),
        ))
    }

    pub fn frame(&self, args: FrameArgs) -> Result<Out> {
        let (dir, p) = self.project(&args.project)?;
        let out = frame_output_path(
            &self.ws,
            &args
                .out
                .unwrap_or_else(|| dir.join(".cache").join(format!("frame-{:.2}.png", args.at))),
        )?;
        let width = args.width.unwrap_or(540).clamp(64, 3840) & !1;
        let height =
            ((width as f64 * p.canvas.height as f64 / p.canvas.width as f64).round() as u32).max(2)
                & !1;
        let pixmap = render::render_frame(&p, &dir, args.at, width, height)?;
        let png = pixmap.encode_png().context("encoding the frame")?;
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&out, &png)?;
        let placement = Graphics::new().caption_placement_at(&p, args.at);
        let mut text = format!(
            "Frame at {} saved to {}",
            time(args.at),
            rel(&self.ws, &out)
        );
        if let Some(page) = &placement {
            let _ = write!(
                text,
                "\nCaption: requested y={:.3}, {} box x={:.3} y={:.3} w={:.3} h={:.3}, size={:.3}, safe-area clamp={}, face={:?}, detection gaps={}.",
                page.requested,
                if page.face == FaceFit::Hidden {
                    "hidden; proposed"
                } else {
                    "drawn"
                },
                page.bounds.x,
                page.bounds.y,
                page.bounds.w,
                page.bounds.h,
                page.size,
                page.clamped,
                page.face,
                page.face_gaps
            );
        }
        let mut o = Out::new(
            text,
            json!({ "path": rel(&self.ws, &out), "at": args.at, "caption_placement": placement }),
        );
        o.png = Some(png);
        Ok(o)
    }

    pub fn frames(&self, args: FramesArgs) -> Result<Out> {
        let (dir, p) = self.project(&args.project)?;
        let out = frame_output_path(
            &self.ws,
            &args
                .out
                .unwrap_or_else(|| dir.join(".cache").join("frames.png")),
        )?;
        let length = p.duration();
        if length <= 0.0 {
            bail!("the project is empty");
        }
        let every = args.every.unwrap_or((length / 12.0).max(0.5)).max(0.1);
        let times: Vec<f64> = (0..36)
            .map(|i| every / 2.0 + i as f64 * every)
            .take_while(|t| *t < length)
            .collect();
        let vertical = p.canvas.height > p.canvas.width;
        let (w, cols) = if vertical {
            (216u32, 6usize)
        } else {
            (320u32, 4usize)
        };
        let h = ((w as f64 * p.canvas.height as f64 / p.canvas.width as f64).round() as u32) & !1;
        let rows = times.len().div_ceil(cols);
        let gap = 4u32;
        let mut sheet = image::RgbaImage::from_pixel(
            cols as u32 * (w + gap) + gap,
            rows as u32 * (h + gap) + gap,
            image::Rgba([24, 24, 24, 255]),
        );
        for (n, &t) in times.iter().enumerate() {
            let frame = render::render_frame(&p, &dir, t, w, h)?;
            let tile = image::RgbaImage::from_raw(w, h, frame.data().to_vec())
                .context("frame size mismatch")?;
            let (x, y) = (
                (n % cols) as u32 * (w + gap) + gap,
                (n / cols) as u32 * (h + gap) + gap,
            );
            image::imageops::overlay(&mut sheet, &tile, x as i64, y as i64);
        }
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)?;
        }
        sheet
            .save(&out)
            .with_context(|| format!("writing {}", out.display()))?;
        let png = std::fs::read(&out)?;
        let legend: Vec<String> = times.iter().map(|t| time(*t)).collect();
        let mut o = Out::new(
            format!(
                "{} frames, {} per row, left to right: {}\nSaved to {}",
                times.len(),
                cols,
                legend.join(", "),
                rel(&self.ws, &out)
            ),
            json!({ "path": rel(&self.ws, &out), "times": times, "columns": cols }),
        );
        o.png = Some(png);
        Ok(o)
    }

    pub fn captions(&self, args: CaptionsArgs) -> Result<Out> {
        let (dir, mut p) = self.project(&args.project)?;
        let before = p.captions.mode;
        if let Some(m) = &args.mode {
            p.captions.mode = serde_json::from_value::<CaptionMode>(json!(m.replace('-', "_")))
                .map_err(|_| anyhow!("unknown caption mode {m:?}; use word_pop, highlight, pills, subtitle or off"))?;
        }
        let has_pages = p
            .tracks
            .iter()
            .any(|t| t.kind == TrackKind::Captions && !t.items.is_empty());
        let mut text = format!("Captions: {:?}", p.captions.mode);
        if args.rebuild
            || (!has_pages && before == CaptionMode::Off && p.captions.mode != CaptionMode::Off)
        {
            let t = Transcript::load(&dir.join("transcript.json"))
                .context("no transcript to build captions from")?;
            let n = captions::apply(&mut p, &t)?;
            let _ = write!(text, ", rebuilt {n} pages");
        }
        self.ws.save_project(&dir, &p)?;
        Ok(Out::new(text, json!({ "mode": p.captions.mode })))
    }

    pub fn note(&self, args: NoteArgs) -> Result<Out> {
        if args.note.trim().is_empty() {
            bail!("the note is empty");
        }
        let (dir, mut p) = self.project(&args.project)?;
        ops::apply(
            &mut p,
            &Op::AddMarker {
                at: args.at.unwrap_or(0.0),
                note: args.note.clone(),
            },
        )?;
        self.ws.save_project(&dir, &p)?;
        let mut text = format!("Noted at {}: {}", time(args.at.unwrap_or(0.0)), args.note);
        if args.rule {
            style::add_rule(&self.ws.rules_path(), &args.note, &args.note)?;
            let _ = write!(text, "\nSaved as a rule: {}", args.note);
        }
        Ok(Out::new(
            text,
            json!({ "note": args.note, "rule": args.rule }),
        ))
    }

    pub fn rules(&self, command: Option<RulesCommand>) -> Result<Out> {
        if let Some(RulesCommand::Add { rule, words }) = command {
            style::add_rule(
                &self.ws.rules_path(),
                &rule,
                words.as_deref().unwrap_or(&rule),
            )?;
            return Ok(Out::new(
                format!("Saved as a rule: {rule}"),
                json!({ "added": rule }),
            ));
        }
        let rules = style::rules(&self.ws.rules_path())?;
        let text = if rules.is_empty() {
            "No rules yet.".to_string()
        } else {
            rules
                .iter()
                .map(|r| format!("- {r}"))
                .collect::<Vec<_>>()
                .join("\n")
        };
        Ok(Out::new(text, json!({ "rules": rules })))
    }

    pub fn style(&self, command: Option<StyleCommand>) -> Result<Out> {
        let path = self.ws.style_path();
        if let Some(StyleCommand::Set { key, value }) = command {
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            let mut table: toml::Table = toml::from_str(&text)
                .with_context(|| format!("{} is not valid TOML", path.display()))?;
            let parsed: toml::Value = toml::from_str::<toml::Table>(&format!("v = {value}"))
                .ok()
                .and_then(|mut t| t.remove("v"))
                .unwrap_or_else(|| toml::Value::String(value.clone()));
            let mut node = &mut table;
            let parts: Vec<&str> = key.split('.').collect();
            for part in &parts[..parts.len() - 1] {
                node = node
                    .entry(part.to_string())
                    .or_insert_with(|| toml::Value::Table(Default::default()))
                    .as_table_mut()
                    .with_context(|| format!("{part} is not a section"))?;
            }
            node.insert(parts[parts.len() - 1].to_string(), parsed);
            let style: Style = toml::from_str(&toml::to_string(&table)?)
                .with_context(|| format!("{key} cannot be {value}"))?;
            style.save(&path)?;
            return Ok(Out::new(
                format!("{key} = {value}"),
                json!({ "key": key, "value": value }),
            ));
        }
        let style = Style::load(&path)?;
        Ok(Out::new(
            toml::to_string_pretty(&style)?,
            serde_json::to_value(&style)?,
        ))
    }

    pub fn agent(&self, args: AgentArgs) -> Result<Out> {
        if args.models {
            return agent_models_for(&args);
        }
        let config = Config::load()?;
        let kind = match agent_named(&args)? {
            Some(kind) => kind,
            None => config.default_agent(),
        };
        let Some(prompt) = args.prompt.as_deref() else {
            bail!("say what the agent should do, or use --models");
        };
        let saved = config.agent_selection(kind);
        let selection = agent::Selection {
            model: pick(saved.model, args.model.as_deref(), "--model")?,
            effort: pick(saved.effort, args.effort.as_deref(), "--effort")?,
        };
        let custom = config.agent.command(kind).is_some_and(|c| !c.is_empty());
        if custom && (args.model.is_some() || args.effort.is_some()) {
            eprintln!(
                "note: config.toml has a custom command for {}; it runs as written and --model/--effort are ignored.",
                kind.label()
            );
        }
        let stop = agent::Stop::default();
        let signal_stop = stop.clone();
        ctrlc::set_handler(move || signal_stop.stop()).context("installing agent stop handler")?;
        let started = Instant::now();
        let mut usage = None;
        let outcome = agent::run(&self.ws, kind, prompt, &selection, &stop, &mut |output| {
            match output {
                agent::Output::Answer(text) | agent::Output::Log(text) => println!("{text}"),
                // Cumulative: the last snapshot is the run's total.
                agent::Output::Usage(total) => usage = Some(total),
            }
        })?;
        let seconds = started.elapsed().as_secs_f64();
        let line = usage_line(usage.as_ref(), seconds);
        let usage = usage_json(usage.as_ref(), seconds);
        match outcome {
            agent::Outcome::Finished(status) if status.success() => {
                Ok(Out::new(line, json!({ "usage": usage })))
            }
            agent::Outcome::Finished(status) => {
                bail!("{} exited with {status} ({line})", kind.label())
            }
            agent::Outcome::Stopped => Ok(Out::new(
                format!("Stopped. {line}"),
                json!({ "stopped": true, "usage": usage }),
            )),
        }
    }

    pub fn studio(&self, args: ProjectArg) -> Result<Out> {
        #[cfg(feature = "studio")]
        {
            let project = match args.project {
                Some(_) => Some(self.project(&args)?.0.join(PROJECT_FILE)),
                None => None,
            };
            veecut_studio::run(veecut_studio::StudioOptions {
                workspace: self.ws.root.clone(),
                project,
            })?;
            Ok(Out::quiet())
        }
        #[cfg(not(feature = "studio"))]
        {
            let _ = args;
            bail!("this build has no desktop editor; reinstall with the `studio` feature")
        }
    }
}

pub fn init(dir: Option<&Path>) -> Result<Out> {
    let dir = match dir {
        Some(d) => d.to_path_buf(),
        None => std::env::current_dir()?,
    };
    let ws = Workspace::init(&dir)?;
    let text = format!(
        "Workspace ready at {}\n  inbox/     put raw recordings here (never changed)\n  outbox/    drafts and finals\n  projects/  one folder per edit\n  my-style.toml, my-rules.md, my-sounds/\n  AGENTS.md, opencode.json and the veecut skill for your coding agent\nNext: put a video in inbox/ and run `veecut edit`, or open your agent here and say \"edit this\".",
        ws.root.display()
    );
    Ok(Out::new(text, json!({ "root": ws.root })))
}

pub fn doctor(workspace: Option<&Path>) -> Result<Out> {
    let mut lines = Vec::new();
    let mut checks = Vec::new();
    let mut check = |name: &str, ok: bool, detail: String| {
        lines.push(format!(
            "{} {name}: {detail}",
            if ok { "ok     " } else { "missing" }
        ));
        checks.push(json!({ "name": name, "ok": ok, "detail": detail }));
    };
    match media::ffmpeg() {
        Ok(path) => {
            let version = media::run(std::process::Command::new(&path).arg("-version"))
                .ok()
                .and_then(|o| String::from_utf8_lossy(&o).lines().next().map(String::from))
                .unwrap_or_default();
            check("ffmpeg", true, version);
            let encoders =
                media::run(std::process::Command::new(&path).args(["-hide_banner", "-encoders"]))
                    .map(|o| String::from_utf8_lossy(&o).to_string())
                    .unwrap_or_default();
            let h264 = [
                "libx264",
                "h264_videotoolbox",
                "h264_nvenc",
                "h264_qsv",
                "h264_mf",
            ]
            .into_iter()
            .find(|e| encoders.contains(e));
            check(
                "H.264 encoder",
                h264.is_some(),
                h264.unwrap_or("none found; renders fall back to MPEG-4")
                    .to_string(),
            );
        }
        Err(e) => check("ffmpeg", false, format!("{e:#}")),
    }
    check(
        "ffprobe",
        media::ffprobe().is_ok(),
        media::ffprobe()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|e| format!("{e:#}")),
    );
    check(
        "cutout model",
        veecut_engine::matte::ready(),
        veecut_engine::matte::status(),
    );
    let ws = workspace
        .map(Path::to_path_buf)
        .unwrap_or(std::env::current_dir()?);
    match Workspace::open(&ws) {
        Ok(w) => {
            config::load_dotenv(&w.root);
            check("workspace", true, w.root.display().to_string());
        }
        Err(_) => check(
            "workspace",
            false,
            "not inside one; run `veecut init`".into(),
        ),
    }
    for line in transcribe::status() {
        check(
            "transcription",
            !line.contains("missing") && !line.contains("not "),
            line,
        );
    }
    match skills::load_catalog() {
        Ok(Some(c)) => check(
            "skills catalog",
            true,
            format!(
                "{} skills, synced {}",
                c.entries.len(),
                c.fetched_at.format("%Y-%m-%d %H:%M UTC")
            ),
        ),
        Ok(None) => check(
            "skills catalog",
            false,
            "not synced yet; run `veecut skills sync`".into(),
        ),
        Err(e) => check("skills catalog", false, format!("{e:#}")),
    }
    for status in agent::detect() {
        let label = match status.readiness {
            agent::Readiness::Ready => "ready",
            agent::Readiness::NotInstalled => "missing",
            agent::Readiness::SignedOut => "signed out",
            agent::Readiness::CheckFailed => "check failed",
            agent::Readiness::Unverified => "unchecked",
        };
        let mut line = format!("{label} {}: {}", status.kind.label(), status.detail);
        if let Some(fix) = &status.fix {
            let _ = write!(line, "; {fix}");
        }
        lines.push(line);
        checks.push(json!({
            "name": status.kind.name(), "ok": status.can_run(), "detail": status.detail,
            "readiness": status.readiness, "fix": status.fix, "command_override": status.command_override
        }));
    }
    Ok(Out::new(lines.join("\n"), json!({ "checks": checks })))
}

pub fn schema() -> Result<Out> {
    let schema = schemars::schema_for!(Op);
    let text = serde_json::to_string_pretty(&schema)?;
    Ok(Out::new(text, serde_json::to_value(&schema)?))
}

pub fn qa(file: &Path) -> Result<Out> {
    let r = qa::check(file)?;
    let mut text = format!(
        "{}: {:.2} s, {}x{} at {:.2} fps, audio {}",
        file.display(),
        r.duration,
        r.width,
        r.height,
        r.fps,
        if r.has_audio { "yes" } else { "no" }
    );
    if let (Some(l), Some(tp)) = (r.integrated_lufs, r.true_peak_db) {
        let _ = write!(text, ", {l:.1} LUFS, true peak {tp:.1} dBTP");
    }
    if r.problems.is_empty() {
        let _ = write!(text, "\nNo problems found.");
    }
    for p in &r.problems {
        let _ = write!(text, "\nProblem: {p}");
    }
    Ok(Out::new(text, serde_json::to_value(&r)?))
}

pub fn fonts() -> Result<Out> {
    let families = Graphics::new().font_families();
    Ok(Out::new(families.join("\n"), json!({ "fonts": families })))
}

fn entry_line(e: &skills::Entry, installed: bool) -> String {
    format!(
        "{}  {} stars  {}{}{}\n    {}",
        e.repo,
        e.stars,
        e.security,
        if e.tested { "  tested" } else { "" },
        if installed { "  installed" } else { "" },
        e.description
    )
}

/// Skills commands. Listing and searching work outside a workspace; installing needs one.
pub fn skills(
    workspace: Option<&Path>,
    command: Option<SkillsCommand>,
    progress: &mut dyn FnMut(&str),
) -> Result<Out> {
    let ctx = Ctx::open(workspace).ok();
    let config = Config::load()?;
    if !matches!(command, Some(SkillsCommand::Sync)) {
        let auto = match &ctx {
            Some(c) => skills::auto_sync_and_update(&c.ws, &config)
                .map(|r| r.map(|(s, u)| (s, u.into_iter().filter(|u| u.updated).count()))),
            None => skills::auto_sync_if_stale(&config).map(|r| r.map(|s| (s, 0))),
        };
        match auto {
            Some(Ok((s, updated))) => progress(&format!(
                "Catalog refreshed: {} skills; {updated} installed skills updated",
                s.entries
            )),
            Some(Err(e)) => progress(&format!("Could not refresh the catalog: {e:#}")),
            None => {}
        }
    }
    let need_ws = || {
        ctx.as_ref()
            .map(|c| &c.ws)
            .context("run this inside a workspace (`veecut init`)")
    };
    let installed_repos: Vec<String> = ctx
        .as_ref()
        .and_then(|c| skills::installed(&c.ws).ok())
        .map(|l| l.repos.into_iter().map(|r| r.repo).collect())
        .unwrap_or_default();
    let is_installed = |repo: &str| installed_repos.iter().any(|r| r.eq_ignore_ascii_case(repo));
    let catalog = || -> Result<skills::Catalog> {
        skills::load_catalog()?.context("no catalog yet; run `veecut skills sync`")
    };
    match command.unwrap_or(SkillsCommand::List {
        category: None,
        installed: false,
    }) {
        SkillsCommand::Sync => {
            let r = skills::sync()?;
            Ok(Out::new(
                format!(
                    "Catalog synced from awesome-claude-video-skills at {}: {} skills in {} categories.",
                    &r.commit[..r.commit.len().min(8)],
                    r.entries,
                    r.categories
                ),
                serde_json::to_value(&r)?,
            ))
        }
        SkillsCommand::List {
            category,
            installed: true,
        } => {
            let _ = category;
            let lock = skills::installed(need_ws()?)?;
            let text = if lock.repos.is_empty() {
                "No catalog skills installed in this workspace.".to_string()
            } else {
                lock.repos
                    .iter()
                    .map(|r| {
                        format!(
                            "{} at {} ({}): {}",
                            r.repo,
                            &r.commit[..r.commit.len().min(8)],
                            r.license.as_deref().unwrap_or("license unknown"),
                            r.skills.join(", ")
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            Ok(Out::new(text, serde_json::to_value(&lock)?))
        }
        SkillsCommand::List {
            category,
            installed: false,
        } => {
            let c = catalog()?;
            let found = skills::search(&c, "", category.as_deref());
            let mut text = format!(
                "{} skills (synced {}). Categories: {}\n",
                found.len(),
                c.fetched_at.format("%Y-%m-%d"),
                c.categories
                    .iter()
                    .map(|k| k.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            for e in found.iter().take(40) {
                let _ = writeln!(text, "{}", entry_line(e, is_installed(&e.repo)));
            }
            if found.len() > 40 {
                let _ = write!(
                    text,
                    "... {} more; narrow with --category or `veecut skills search`",
                    found.len() - 40
                );
            }
            Ok(Out::new(text, json!({ "entries": found })))
        }
        SkillsCommand::Search { query, category } => {
            let c = catalog()?;
            let found = skills::search(&c, &query, category.as_deref());
            let text = if found.is_empty() {
                format!("Nothing matches {query:?}.")
            } else {
                found
                    .iter()
                    .take(25)
                    .map(|e| entry_line(e, is_installed(&e.repo)))
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            Ok(Out::new(text, json!({ "entries": found })))
        }
        SkillsCommand::Show { repo } => {
            let c = catalog()?;
            let e = c
                .entries
                .iter()
                .find(|e| e.repo.eq_ignore_ascii_case(&repo))
                .with_context(|| format!("{repo} is not in the catalog"))?;
            let text = format!(
                "{}\n{}\ncategories: {}\n{}",
                entry_line(e, is_installed(&e.repo)),
                e.url,
                e.categories.join(", "),
                if e.is_safe() {
                    "Install: veecut skills install ".to_string() + &e.repo
                } else {
                    "Not graded SAFE: install only with --force after reading it".into()
                }
            );
            Ok(Out::new(text, serde_json::to_value(e)?))
        }
        SkillsCommand::Install { repo, force } => {
            let r = skills::install(need_ws()?, &repo, force)?;
            Ok(Out::new(
                format!(
                    "Installed {} at {}: {}. Skills land in .agents/skills and .claude/skills; restart your agent to load them.",
                    r.repo,
                    &r.commit[..r.commit.len().min(8)],
                    r.skills.join(", ")
                ),
                serde_json::to_value(&r)?,
            ))
        }
        SkillsCommand::Update { repo } => {
            let reports = skills::update(need_ws()?, repo.as_deref())?;
            let text = if reports.is_empty() {
                "Nothing installed to update.".to_string()
            } else {
                reports
                    .iter()
                    .map(|u| match (&u.error, u.updated) {
                        (Some(e), _) => format!("{}: {e}", u.repo),
                        (None, true) => format!(
                            "{}: updated {} -> {}",
                            u.repo,
                            &u.from[..u.from.len().min(8)],
                            &u.to[..u.to.len().min(8)]
                        ),
                        (None, false) => format!("{}: up to date", u.repo),
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            Ok(Out::new(text, serde_json::to_value(&reports)?))
        }
        SkillsCommand::Remove { repo } => {
            skills::remove(need_ws()?, &repo)?;
            Ok(Out::new(
                format!("Removed {repo}."),
                json!({ "removed": repo }),
            ))
        }
        SkillsCommand::Recommend { kind } => {
            let kind = match kind {
                Some(k) => kind_of(&k)?,
                None => ctx
                    .as_ref()
                    .and_then(|c| c.project(&ProjectArg::default()).ok())
                    .map(|(_, p)| p.kind)
                    .unwrap_or_default(),
            };
            let c = catalog()?;
            let found = skills::recommended(&c, kind, 8);
            let text = format!(
                "Best catalog skills for {}:\n{}",
                kind.label(),
                found
                    .iter()
                    .map(|e| entry_line(e, is_installed(&e.repo)))
                    .collect::<Vec<_>>()
                    .join("\n")
            );
            Ok(Out::new(text, json!({ "kind": kind, "entries": found })))
        }
    }
}

/// A flag overrides the saved value; `default` clears it to VeeCut's default for the agent.
fn pick(saved: Option<String>, flag: Option<&str>, name: &str) -> Result<Option<String>> {
    match flag.map(str::trim) {
        None => Ok(saved),
        Some("") => bail!("{name} needs a value, or `default`"),
        Some("default") => Ok(None),
        Some(value) => Ok(Some(value.to_string())),
    }
}

/// "42 s, 61k tokens, free"; just the time when the agent reported no usage.
fn usage_line(usage: Option<&agent::Usage>, seconds: f64) -> String {
    match usage {
        Some(usage) => usage.summary(seconds),
        None => format!("{seconds:.0} s"),
    }
}

fn usage_json(usage: Option<&agent::Usage>, seconds: f64) -> Value {
    match usage {
        Some(u) => json!({
            "seconds": seconds,
            "input_tokens": u.input_tokens,
            "output_tokens": u.output_tokens,
            "cached_tokens": u.cached_tokens,
            "total_tokens": u.total_tokens(),
            "cost_usd": u.cost_usd,
            "turns": u.turns,
        }),
        None => json!({ "seconds": seconds }),
    }
}

/// `veecut agent`: listing models needs no workspace; a run does.
pub fn agent_command(workspace: Option<&Path>, args: AgentArgs) -> Result<Out> {
    if args.models {
        agent_models_for(&args)
    } else {
        Ctx::open(workspace).and_then(|ctx| ctx.agent(args))
    }
}

fn agent_named(args: &AgentArgs) -> Result<Option<AgentKind>> {
    args.with
        .as_deref()
        .map(|w| {
            AgentKind::from_name(w).ok_or_else(|| {
                anyhow!("unknown agent {w:?}; use opencode, claude, codex or gemini")
            })
        })
        .transpose()
}

fn agent_models_for(args: &AgentArgs) -> Result<Out> {
    let kinds = agent_named(args)?.map_or(AgentKind::ALL.to_vec(), |kind| vec![kind]);
    agent_models(&kinds)
}

/// The built-in models, plus what OpenCode lists on this machine, and the efforts per agent.
fn agent_models(kinds: &[AgentKind]) -> Result<Out> {
    let mut text = String::new();
    let mut all = Vec::new();
    for &kind in kinds {
        let (models, problem) = match agent::models(kind) {
            Ok(models) => (models, None),
            Err(error) => (agent::known_models(kind), Some(format!("{error:#}"))),
        };
        let efforts = agent::efforts(kind, None);
        text.push_str(&describe_models(
            kind,
            &models,
            &efforts,
            problem.as_deref(),
        ));
        all.push(json!({
            "agent": kind.name(),
            "models": models.iter().map(|m| json!({ "id": m.id, "note": m.note })).collect::<Vec<_>>(),
            "efforts": efforts.iter().map(|e| json!({ "id": e.id, "note": e.note })).collect::<Vec<_>>(),
            "models_error": problem,
        }));
    }
    Ok(Out::new(text, json!({ "agents": all })))
}

fn describe_models(
    kind: AgentKind,
    models: &[agent::ModelChoice],
    efforts: &[agent::EffortChoice],
    problem: Option<&str>,
) -> String {
    let mut text = format!("{} ({})\n  models:\n", kind.label(), kind.name());
    for model in models {
        let _ = writeln!(text, "    {}  {}", model.id, model.note);
    }
    if let Some(problem) = problem {
        let _ = writeln!(text, "    (built-in list only: {problem})");
    }
    if efforts.is_empty() {
        text.push_str("  efforts: none\n");
    } else {
        text.push_str("  efforts:\n");
        for effort in efforts {
            let _ = writeln!(text, "    {}  {}", effort.id, effort.note);
        }
    }
    text
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn speech_project() -> (tempfile::TempDir, Ctx, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let ws = Workspace {
            root: temp.path().to_path_buf(),
        };
        std::fs::create_dir_all(ws.root.join(".veecut")).unwrap();
        std::fs::write(ws.root.join(".veecut/workspace.toml"), "version = 1\n").unwrap();
        let dir = ws.projects().join("my-edit");
        std::fs::create_dir_all(&dir).unwrap();
        let mut project = Project::new("My edit", VideoKind::default(), Canvas::new(1080, 1920));
        project.assets.push(
            serde_json::from_value(json!({
                "id": "speech", "path": "speech.mp4", "kind": "video", "role": "source",
                "duration": 10.0, "width": 1080, "height": 1920, "fps": 30.0, "has_audio": true
            }))
            .unwrap(),
        );
        let clip: Op = serde_json::from_value(json!({
            "op": "add_clip", "asset": "speech", "duration": 10.0
        }))
        .unwrap();
        ops::apply(&mut project, &clip).unwrap();
        project.save(&dir.join(PROJECT_FILE)).unwrap();
        let transcript: Transcript = serde_json::from_value(json!({
            "asset": "speech", "provider": "test", "duration": 10.0,
            "words": [
                {"text": "Hello,", "start": 1.123, "end": 1.456},
                {"text": "there.", "start": 1.5, "end": 2.789},
                {"text": "Keep.", "start": 3.0, "end": 3.5},
                {"text": "Hello", "start": 5.123, "end": 5.456},
                {"text": "there!", "start": 5.5, "end": 6.789},
                {"text": "End.", "start": 8.0, "end": 8.5}
            ]
        }))
        .unwrap();
        transcript.save(&dir.join("transcript.json")).unwrap();
        (temp, Ctx { ws }, dir)
    }

    #[test]
    fn transcript_reports_both_sentence_edges_at_millisecond_precision() {
        let (_temp, ctx, _) = speech_project();
        let out = ctx.transcript(TranscriptArgs::default()).unwrap();
        assert_eq!(out.json["sentences"][0]["source_start"], 1.123);
        assert_eq!(out.json["sentences"][0]["source_end"], 2.789);
        assert_eq!(out.json["sentences"][0]["timeline_start"], 1.123);
        assert_eq!(out.json["sentences"][0]["timeline_end"], 2.789);
        assert!(out.text.contains("0:01.123") && out.text.contains("0:02.789"));
        assert_eq!(time_ms(59.9999), "1:00.000");
    }

    #[test]
    fn phrase_commands_reject_missing_or_unrelated_transcripts_without_saving() {
        let (_temp, ctx, dir) = speech_project();
        let mut transcript = Transcript::load(&dir.join("transcript.json")).unwrap();
        transcript.asset = "another-recording".into();
        transcript.save(&dir.join("transcript.json")).unwrap();
        let before = std::fs::read(dir.join(PROJECT_FILE)).unwrap();
        let error = ctx
            .cut_phrase(CutPhraseArgs {
                phrase: "hello there".into(),
                occurrence: Some(1),
                ..Default::default()
            })
            .err()
            .unwrap();
        assert!(error.to_string().contains("transcript asset"));
        assert_eq!(before, std::fs::read(dir.join(PROJECT_FILE)).unwrap());
        let error = project_transcript(
            &dir.join("missing"),
            &Project::load(&dir.join(PROJECT_FILE)).unwrap(),
        )
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("--project projects/my-edit/project.json"),
            "{error}"
        );
    }

    #[test]
    fn model_and_effort_flags_override_saved_choices() {
        let saved = Some("opus".to_string());
        assert_eq!(pick(saved.clone(), None, "--model").unwrap(), saved);
        assert_eq!(
            pick(saved.clone(), Some(" sonnet "), "--model").unwrap(),
            Some("sonnet".into())
        );
        assert_eq!(
            pick(saved.clone(), Some("default"), "--model").unwrap(),
            None
        );
        assert!(pick(saved, Some("  "), "--effort").is_err());
    }

    #[test]
    fn model_list_shows_models_and_efforts_per_agent() {
        let claude = describe_models(
            AgentKind::Claude,
            &agent::known_models(AgentKind::Claude),
            &agent::efforts(AgentKind::Claude, None),
            None,
        );
        assert!(claude.starts_with("Claude Code (claude)\n  models:\n"));
        assert!(claude.contains(agent::ULTRACODE), "{claude}");
        let gemini = describe_models(
            AgentKind::Gemini,
            &agent::known_models(AgentKind::Gemini),
            &agent::efforts(AgentKind::Gemini, None),
            Some("offline"),
        );
        assert!(gemini.contains("efforts: none") && gemini.contains("built-in list only: offline"));
    }

    #[test]
    fn usage_line_without_report_is_only_the_time() {
        assert_eq!(usage_line(None, 41.6), "42 s");
        assert_eq!(usage_json(None, 2.0), json!({ "seconds": 2.0 }));
    }

    #[test]
    fn model_listing_needs_no_workspace_but_a_run_does() {
        let empty = tempfile::tempdir().unwrap();
        let listing = AgentArgs {
            models: true,
            with: Some("gemini".into()),
            ..Default::default()
        };
        let out = agent_command(Some(empty.path()), listing).unwrap();
        assert!(out.text.starts_with("Gemini CLI (gemini)"), "{}", out.text);
        let run = AgentArgs {
            prompt: Some("tighter cuts".into()),
            with: Some("gemini".into()),
            ..Default::default()
        };
        let error = agent_command(Some(empty.path()), run).err().unwrap();
        assert!(format!("{error:#}").contains("not a VeeCut workspace"));
    }

    #[test]
    fn check_frames_reject_protected_folders_and_symlink_aliases() {
        let temp = tempfile::tempdir().unwrap();
        let ws = Workspace {
            root: temp.path().to_owned(),
        };
        for folder in ["inbox", "outbox", "projects/check/.cache"] {
            std::fs::create_dir_all(ws.root.join(folder)).unwrap();
        }
        let cache = ws
            .root
            .join("projects/check/.cache")
            .canonicalize()
            .unwrap();
        assert_eq!(
            frame_output_path(&ws, &cache.join("check.png")).unwrap(),
            cache.join("check.png")
        );
        for path in [
            "inbox/check.png",
            "outbox/new/check.png",
            "projects/../outbox/check.png",
        ] {
            assert!(frame_output_path(&ws, &ws.root.join(path)).is_err());
        }
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(ws.outbox(), cache.join("alias")).unwrap();
            assert!(frame_output_path(&ws, &cache.join("alias/new/check.png")).is_err());
            let source = ws.inbox().join("source");
            std::fs::write(&source, b"original").unwrap();
            std::os::unix::fs::symlink(&source, cache.join("check.png")).unwrap();
            assert!(frame_output_path(&ws, &cache.join("check.png")).is_err());
        }
    }

    #[test]
    fn render_refuses_outputs_in_inbox() {
        let (_temp, ctx, _) = speech_project();
        std::fs::create_dir_all(ctx.ws.inbox()).unwrap();
        let source = ctx.ws.inbox().join("source.mp4");
        std::fs::write(&source, b"original").unwrap();
        for out in [
            source.clone(),
            ctx.ws.root.join("projects/../inbox/source.mp4"),
        ] {
            let args = RenderArgs {
                out: Some(out),
                ..Default::default()
            };
            let error = ctx.render(args, &mut |_: &str| {}).err().unwrap();
            assert!(
                format!("{error:#}").contains("cannot be saved in inbox/"),
                "{error:#}"
            );
        }
        assert_eq!(std::fs::read(&source).unwrap(), b"original");
    }

    #[test]
    fn ops_cannot_hand_a_sourced_file_to_another_asset() {
        let (_temp, ctx, dir) = speech_project();
        let file = dir.join(PROJECT_FILE);
        let mut project = Project::load(&file).unwrap();
        project.assets.push(
            serde_json::from_value(json!({
                "id": "asset-2", "path": "other.mp4", "kind": "video", "role": "broll",
                "duration": 10.0, "width": 1080, "height": 1920, "fps": 30.0, "has_audio": false
            }))
            .unwrap(),
        );
        project.sources.insert(
            "speech".into(),
            serde_json::from_value(json!({
                "url": "https://example.org/speech", "license": "CC BY-NC 2.0",
                "flag": true, "flag_reason": "non-commercial"
            }))
            .unwrap(),
        );
        project.save(&file).unwrap();
        let before = std::fs::read(&file).unwrap();
        let mut swapped = serde_json::to_value(&project.assets).unwrap();
        swapped[0]["id"] = json!("asset-2");
        swapped[1]["id"] = json!("speech");
        let mut copy = serde_json::to_value(&project.assets[0]).unwrap();
        copy["id"] = json!("copy");
        copy["path"] = json!("./speech.mp4");
        for op in [
            json!({"op": "set", "path": "/assets/1/path", "value": "speech.mp4"}),
            json!([
                {"op": "set", "path": "/assets/0/path", "value": "other.mp4"},
                {"op": "set", "path": "/assets/1/path", "value": "speech.mp4"}
            ]),
            json!({"op": "set", "path": "/assets", "value": swapped}),
            json!({"op": "set", "path": "/assets/-", "value": copy}),
        ] {
            let error = ctx
                .ops(OpsArgs {
                    ops: Some(op.to_string()),
                    ..Default::default()
                })
                .err()
                .unwrap();
            assert_eq!(format!("{error:#}"), APPROVAL_IS_HUMAN, "{op}");
        }
        assert_eq!(before, std::fs::read(&file).unwrap());
        let elsewhere = json!({"op": "set", "path": "/assets/1/path", "value": "elsewhere.mp4"});
        ctx.ops(OpsArgs {
            ops: Some(elsewhere.to_string()),
            ..Default::default()
        })
        .unwrap();
        let saved = Project::load(&file).unwrap();
        assert_eq!(saved.assets[1].path, Path::new("elsewhere.mp4"));
        assert!(saved.sources["speech"].blocks_final());
    }
}
