//! Every command, shared by the command line and the MCP server. Each returns an [`Out`]: text
//! for people, JSON for agents, and a PNG when the command made a picture.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use clap::{Args, Subcommand};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use veecut_engine::agent::{self, AgentKind};
use veecut_engine::config::{self, Config};
use veecut_engine::graphics::Graphics;
use veecut_engine::ops::{self, Op};
use veecut_engine::project::{
    AssetKind, AssetRole, Canvas, CaptionMode, ItemBody, PROJECT_FILE, Project, TrackKind,
    VideoKind,
};
use veecut_engine::recipes::{self, EditRequest};
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
    fn new(text: impl Into<String>, json: Value) -> Self {
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
    #[arg(short, long, value_name = "P")]
    pub project: Option<String>,
}

#[derive(Args, Deserialize, JsonSchema, Default)]
#[serde(default)]
pub struct EditArgs {
    /// Recordings to edit (paths, or names of files in inbox/). Default: the newest video in inbox/.
    pub files: Vec<PathBuf>,
    /// talking-head-short, talking-head-long, tutorial, podcast, clips, vlog, explainer, promo or music-video.
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
    /// Text the edit shows (promo lines, list items, LRC lyrics), or a path to a text file.
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
#[serde(default)]
pub struct OpsArgs {
    /// One op, a JSON array of ops, or JSON lines. `-` reads standard input.
    pub ops: Option<String>,
    /// Read the ops from a file.
    #[arg(long)]
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
    #[arg(long)]
    pub out: Option<PathBuf>,
    #[command(flatten)]
    #[serde(flatten)]
    pub project: ProjectArg,
}

#[derive(Args, Deserialize, JsonSchema, Default)]
#[serde(default)]
pub struct FrameArgs {
    /// Timeline time in seconds.
    #[arg(long)]
    pub at: f64,
    /// Width in pixels (default 540).
    #[arg(long)]
    pub width: Option<u32>,
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
#[serde(default)]
pub struct NoteArgs {
    /// The note, in the user's words.
    pub note: String,
    /// Timeline time the note is about.
    #[arg(long)]
    pub at: Option<f64>,
    /// Also save it as a lasting rule in my-rules.md.
    #[arg(long)]
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
#[serde(default)]
pub struct SkillsSearchArgs {
    pub query: String,
    pub category: Option<String>,
}

#[derive(Args, Deserialize, JsonSchema, Default)]
#[serde(default)]
pub struct SkillsInstallArgs {
    /// owner/name of a repo in the catalog.
    pub repo: String,
    pub force: bool,
}

#[derive(Args, Deserialize, JsonSchema, Default)]
#[serde(default)]
pub struct AgentArgs {
    pub prompt: String,
    /// opencode (default), claude, codex or gemini.
    #[arg(long = "with")]
    #[serde(rename = "with")]
    pub with: Option<String>,
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

fn rel(ws: &Workspace, path: &Path) -> String {
    path.strip_prefix(&ws.root)
        .unwrap_or(path)
        .display()
        .to_string()
}

fn time(t: f64) -> String {
    let t = t.max(0.0);
    format!("{}:{:04.1}", (t / 60.0) as u64, t % 60.0)
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

    pub fn project(&self, arg: &ProjectArg) -> Result<(PathBuf, Project)> {
        let dir = match arg.project.as_deref() {
            Some(sel) => {
                let path = Path::new(sel);
                if path.is_file() {
                    path.parent().unwrap_or(Path::new(".")).to_path_buf()
                } else if path.join(PROJECT_FILE).is_file() {
                    path.to_path_buf()
                } else if self.ws.projects().join(sel).join(PROJECT_FILE).is_file() {
                    self.ws.projects().join(sel)
                } else {
                    bail!(
                        "no project {sel:?}; `veecut status --json` and the projects/ folder list them"
                    );
                }
            }
            None => self
                .ws
                .list_projects()?
                .into_iter()
                .next()
                .map(|(_, dir)| dir)
                .context("no projects yet; run `veecut edit` first")?,
        };
        let project = Project::load(&dir.join(PROJECT_FILE))?;
        Ok((dir, project))
    }

    /// An input file: a path as given, or a name inside inbox/ or the workspace.
    fn input(&self, path: &Path) -> Result<PathBuf> {
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
        let mut text = String::from("timeline  source  sentence\n");
        let mut rows = Vec::new();
        for s in t.sentences() {
            let at = p.source_to_timeline(&t.asset, s.start);
            let _ = writeln!(
                text,
                "{:>8}  {:>6}  {}",
                at.map(time).unwrap_or_else(|| "(cut)".into()),
                time(s.start),
                s.text
            );
            rows.push(json!({ "timeline": at, "source": s.start, "end": s.end, "text": s.text }));
        }
        Ok(Out::new(
            text,
            json!({ "asset": t.asset, "language": t.language, "sentences": rows }),
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
        let list = ops::parse_ops(&input)?;
        let (dir, mut p) = self.project(&args.project)?;
        let done = ops::apply_all(&mut p, &list)?;
        self.ws.save_project(&dir, &p)?;
        let mut text = String::new();
        for d in &done {
            let _ = writeln!(text, "- {}", d.summary);
        }
        let _ = write!(text, "Saved. `veecut render` shows the change.");
        Ok(Out::new(
            text,
            json!({ "applied": done, "duration": p.duration() }),
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
            Some(o) => o,
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
        let width = args.width.unwrap_or(540).clamp(64, 3840) & !1;
        let height =
            ((width as f64 * p.canvas.height as f64 / p.canvas.width as f64).round() as u32).max(2)
                & !1;
        let pixmap = render::render_frame(&p, &dir, args.at, width, height)?;
        let png = pixmap.encode_png().context("encoding the frame")?;
        let out = args
            .out
            .unwrap_or_else(|| dir.join(".cache").join(format!("frame-{:.2}.png", args.at)));
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&out, &png)?;
        let mut o = Out::new(
            format!(
                "Frame at {} saved to {}",
                time(args.at),
                rel(&self.ws, &out)
            ),
            json!({ "path": rel(&self.ws, &out), "at": args.at }),
        );
        o.png = Some(png);
        Ok(o)
    }

    pub fn frames(&self, args: FramesArgs) -> Result<Out> {
        let (dir, p) = self.project(&args.project)?;
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
        let out = args
            .out
            .unwrap_or_else(|| dir.join(".cache").join("frames.png"));
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
        let kind = match &args.with {
            Some(w) => AgentKind::from_name(w).ok_or_else(|| {
                anyhow!("unknown agent {w:?}; use opencode, claude, codex or gemini")
            })?,
            None => Config::load()?.default_agent(),
        };
        let status = agent::run(&self.ws, kind, &args.prompt, &mut |line| println!("{line}"))?;
        if !status.success() {
            bail!("{} exited with {status}", kind.label());
        }
        Ok(Out::quiet())
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
    for (kind, version) in agent::detect() {
        let detail = version
            .clone()
            .unwrap_or_else(|| format!("not installed ({})", kind.install_hint()));
        check(kind.label(), version.is_some(), detail);
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
