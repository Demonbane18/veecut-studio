//! The user's editing folder: inbox, outbox, projects, style, rules, skills, history and undo.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::media;
use crate::project::{Asset, AssetRole, Canvas, PROJECT_FILE, Project, VideoKind};
use crate::style::{RULES_HEADER, Style};

/// The file that marks a workspace root.
const MARKER: &str = ".veecut/workspace.toml";
/// Undo snapshots kept per project.
const HISTORY_KEEP: usize = 50;
const VIDEO_EXTENSIONS: [&str; 8] = ["mp4", "mov", "m4v", "mkv", "webm", "avi", "mts", "mpg"];

macro_rules! skill_file {
    ($path:literal) => {
        ($path, include_str!(concat!("../skill/", $path)))
    };
}

/// The built-in skill, relative to `<skills dir>/veecut/`.
const SKILL_FILES: &[(&str, &str)] = &[
    skill_file!("SKILL.md"),
    skill_file!("workflows/talking-head-short.md"),
    skill_file!("workflows/talking-head-long.md"),
    skill_file!("workflows/tutorial.md"),
    skill_file!("workflows/podcast.md"),
    skill_file!("workflows/clips.md"),
    skill_file!("workflows/vlog.md"),
    skill_file!("workflows/explainer.md"),
    skill_file!("workflows/promo.md"),
    skill_file!("workflows/music-video.md"),
    skill_file!("workflows/documentary.md"),
    skill_file!("reference/editor-api.md"),
    skill_file!("reference/notes-and-rules.md"),
    skill_file!("reference/style-setup.md"),
    skill_file!("reference/sourcing.md"),
    skill_file!("reference/sound.md"),
];

const BLOCK_START: &str = "<!-- veecut:start -->";
const BLOCK_END: &str = "<!-- veecut:end -->";

const AGENTS_BLOCK: &str = "<!-- veecut:start -->
# VeeCut workspace

This folder is a VeeCut video editing workspace.

- For any video request, use the `veecut` skill (`.agents/skills/veecut/SKILL.md`) and the `veecut` command.
- Never modify, move, re-encode over or delete anything in `inbox/`. Drafts and finals go to `outbox/`.
- Never upload or post anything anywhere.
- Ask before anything that costs money: paid APIs, paid models, paid assets.
- Never print, log or copy API keys or the contents of `.env`.
<!-- veecut:end -->
";

const GITIGNORE: &str = "inbox/\noutbox/\nprojects/*/.cache/\n.env\nmy-sounds/\n";

const ENV_TEMPLATE: &str = "# Optional keys for VeeCut. This file is never committed or shown.\n# Free Groq key for fast transcription: https://console.groq.com/keys\n# GROQ_API_KEY=\n# Raises GitHub's rate limit for skill installs.\n# GITHUB_TOKEN=\n";

/// A VeeCut workspace folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Workspace {
    pub root: PathBuf,
}

impl Workspace {
    /// Creates the workspace layout in `root`. Safe to run again: it never overwrites the user's
    /// style, rules, `.env` or `.gitignore`, keeps their own text in AGENTS.md, CLAUDE.md,
    /// GEMINI.md and opencode.json, and refreshes the built-in skill.
    pub fn init(root: &Path) -> Result<Workspace> {
        let root =
            std::path::absolute(root).with_context(|| format!("resolving {}", root.display()))?;
        let ws = Workspace { root };
        for dir in [
            ".veecut",
            "inbox",
            "outbox",
            "projects",
            "my-sounds/music",
            "my-sounds/sfx",
            ".agents/skills",
            ".claude/skills",
        ] {
            let dir = ws.root.join(dir);
            fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        write_new(
            &ws.root.join(MARKER),
            "# Marks this folder as a VeeCut workspace.\nversion = 1\n",
        )?;
        if !ws.style_path().exists() {
            Style::default().save(&ws.style_path())?;
        }
        write_new(&ws.rules_path(), RULES_HEADER)?;
        write_new(&ws.root.join(".env"), ENV_TEMPLATE)?;
        write_new(&ws.root.join(".gitignore"), GITIGNORE)?;
        upsert_block(&ws.root.join("AGENTS.md"), AGENTS_BLOCK)?;
        ensure_line(&ws.root.join("CLAUDE.md"), "@AGENTS.md")?;
        // Gemini CLI reads GEMINI.md and follows its @file imports.
        ensure_line(&ws.root.join("GEMINI.md"), "@./AGENTS.md")?;
        register_opencode_mcp(&ws.root.join("opencode.json"))?;
        ws.refresh_skill()?;
        Ok(ws)
    }

    /// The workspace holding `start`, found by walking up to the folder with `.veecut/workspace.toml`.
    pub fn open(start: &Path) -> Result<Workspace> {
        let start =
            std::path::absolute(start).with_context(|| format!("resolving {}", start.display()))?;
        match start.ancestors().find(|dir| dir.join(MARKER).is_file()) {
            Some(root) => Ok(Workspace {
                root: root.to_path_buf(),
            }),
            None => bail!(
                "{} is not inside a VeeCut workspace. Run `veecut init` in your videos folder first.",
                start.display()
            ),
        }
    }

    pub fn inbox(&self) -> PathBuf {
        self.root.join("inbox")
    }

    pub fn outbox(&self) -> PathBuf {
        self.root.join("outbox")
    }

    pub fn projects(&self) -> PathBuf {
        self.root.join("projects")
    }

    pub fn style_path(&self) -> PathBuf {
        self.root.join("my-style.toml")
    }

    pub fn rules_path(&self) -> PathBuf {
        self.root.join("my-rules.md")
    }

    /// Skill folders: `.agents/skills` (OpenCode, Codex, Gemini) and `.claude/skills` (Claude Code).
    pub fn skill_dirs(&self) -> [PathBuf; 2] {
        [
            self.root.join(".agents/skills"),
            self.root.join(".claude/skills"),
        ]
    }

    /// Rewrites `<skills dir>/veecut/` from the copy built into VeeCut, removing stale files.
    fn refresh_skill(&self) -> Result<()> {
        for base in self.skill_dirs() {
            let dir = base.join("veecut");
            match fs::remove_dir_all(&dir) {
                Err(e) if e.kind() != ErrorKind::NotFound => {
                    return Err(e).with_context(|| format!("clearing {}", dir.display()));
                }
                _ => {}
            }
            for (rel, text) in SKILL_FILES {
                let path = dir.join(rel);
                fs::create_dir_all(path.parent().expect("skill files sit in a folder"))?;
                fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
            }
        }
        Ok(())
    }

    /// A new, saved project in `projects/<slug>/`. The slug is the name plus today's date, with
    /// `-2`, `-3`... when taken. The workspace style is not applied; see [`Style::apply_to`].
    pub fn create_project(
        &self,
        name: &str,
        kind: VideoKind,
        canvas: Canvas,
    ) -> Result<(PathBuf, Project)> {
        let name = match name.trim() {
            "" => "Untitled",
            n => n,
        };
        let base = format!(
            "{}-{}",
            slugify(name),
            chrono::Local::now().format("%Y-%m-%d")
        );
        let projects = self.projects();
        fs::create_dir_all(&projects)
            .with_context(|| format!("creating {}", projects.display()))?;
        let mut n = 1;
        let dir = loop {
            let slug = if n == 1 {
                base.clone()
            } else {
                format!("{base}-{n}")
            };
            let dir = projects.join(slug);
            match fs::create_dir(&dir) {
                Ok(()) => break dir,
                Err(e) if e.kind() == ErrorKind::AlreadyExists => n += 1,
                Err(e) => return Err(e).with_context(|| format!("creating {}", dir.display())),
            }
        };
        let project = Project::new(name, kind, canvas);
        self.save_project(&dir, &project)?;
        Ok((dir, project))
    }

    /// Probes `file` and adds it to the project as `asset-N`; returns the id. Files inside the
    /// workspace are stored relative to the project folder so the workspace can move. Does not save.
    pub fn add_asset(
        &self,
        project: &mut Project,
        project_dir: &Path,
        file: &Path,
        role: AssetRole,
    ) -> Result<String> {
        let file =
            std::path::absolute(file).with_context(|| format!("resolving {}", file.display()))?;
        let info = media::probe(&file)?;
        let project_dir = std::path::absolute(project_dir)?;
        let path = if file.starts_with(&self.root) && project_dir.starts_with(&self.root) {
            relative(&project_dir, &file)
        } else {
            file
        };
        let id = project.new_id("asset");
        project.assets.push(Asset {
            id: id.clone(),
            path,
            kind: info.kind,
            role,
            duration: info.duration,
            width: info.width,
            height: info.height,
            fps: info.fps,
            has_audio: info.has_audio,
        });
        Ok(id)
    }

    /// Writes `project.json`, first copying the previous version into `.history/` (newest 50 kept).
    /// A save that changes nothing is skipped; any real change clears the redo steps.
    pub fn save_project(&self, project_dir: &Path, project: &Project) -> Result<()> {
        let file = project_dir.join(PROJECT_FILE);
        if let Ok(old) = fs::read(&file) {
            if old == (serde_json::to_string_pretty(project)? + "\n").into_bytes() {
                return Ok(());
            }
            let history = project_dir.join(".history");
            push_snapshot(&history, &file)?;
            match fs::remove_dir_all(history.join("redo")) {
                Err(e) if e.kind() != ErrorKind::NotFound => {
                    return Err(e).context("clearing redo steps");
                }
                _ => {}
            }
        }
        project.save(&file)
    }

    /// Restores the newest snapshot. The replaced version becomes a redo step.
    pub fn undo(&self, project_dir: &Path) -> Result<Project> {
        let history = project_dir.join(".history");
        step(project_dir, &history, &history.join("redo"), "undo")
    }

    /// Brings back the version the last undo replaced.
    pub fn redo(&self, project_dir: &Path) -> Result<Project> {
        let history = project_dir.join(".history");
        step(project_dir, &history.join("redo"), &history, "redo")
    }

    /// (slug, folder) of every project, most recently saved first.
    pub fn list_projects(&self) -> Result<Vec<(String, PathBuf)>> {
        let mut found = Vec::new();
        for entry in read_dir_or_empty(&self.projects())? {
            let dir = entry.path();
            let Ok(meta) = fs::metadata(dir.join(PROJECT_FILE)) else {
                continue;
            };
            let slug = entry.file_name().to_string_lossy().into_owned();
            found.push((meta.modified().ok(), slug, dir));
        }
        found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        Ok(found
            .into_iter()
            .map(|(_, slug, dir)| (slug, dir))
            .collect())
    }

    /// Resolves a slug, project folder or `project.json` path. Relative paths may start at the
    /// current folder or workspace root. Without a selector, uses the most recently saved project.
    pub fn resolve_project(&self, selector: Option<&str>) -> Result<PathBuf> {
        let Some(selector) = selector else {
            return self
                .list_projects()?
                .into_iter()
                .next()
                .map(|(_, dir)| dir)
                .context("no projects yet; run `veecut edit` first")
                .and_then(|dir| self.checked_project_dir(dir));
        };
        let path = Path::new(selector);
        if !selector.trim().is_empty() {
            for candidate in [
                self.projects().join(path),
                self.root.join(path),
                path.to_path_buf(),
            ] {
                let dir = if candidate
                    .file_name()
                    .is_some_and(|name| name == PROJECT_FILE)
                {
                    candidate.parent().unwrap_or(Path::new(".")).to_path_buf()
                } else {
                    candidate
                };
                if dir.join(PROJECT_FILE).is_file() {
                    return self.checked_project_dir(dir);
                }
            }
        }
        bail!(
            "no project {selector:?}; use a slug, project folder or project.json path, for example `veecut status -p my-edit`, `veecut status -p projects/my-edit` or `veecut status -p projects/my-edit/project.json`. Available projects are in {}",
            self.projects().display()
        )
    }

    fn checked_project_dir(&self, dir: PathBuf) -> Result<PathBuf> {
        let dir = dir
            .canonicalize()
            .with_context(|| format!("resolving project {}", dir.display()))?;
        let owner = Workspace::open(&dir)?;
        if owner.root.canonicalize()? != self.root.canonicalize()? {
            bail!(
                "project {} belongs to another workspace ({}); select that workspace with --workspace, or omit --workspace and pass its project.json path with -p",
                dir.display(),
                owner.root.display()
            );
        }
        Ok(dir)
    }

    /// The most recently modified video file directly in `inbox/`.
    pub fn newest_inbox_video(&self) -> Option<PathBuf> {
        read_dir_or_empty(&self.inbox())
            .ok()?
            .into_iter()
            .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
            .map(|e| e.path())
            .filter(|p| {
                p.extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| VIDEO_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
            })
            .filter_map(|p| {
                Some((
                    fs::metadata(&p)
                        .ok()
                        .filter(|m| m.is_file())?
                        .modified()
                        .ok()?,
                    p,
                ))
            })
            .max_by(|a, b| a.0.cmp(&b.0))
            .map(|(_, p)| p)
    }

    /// `outbox/<slug>-v<n>.mp4` with `n` one past the highest draft already there.
    pub fn next_draft_path(&self, slug: &str) -> PathBuf {
        let prefix = format!("{slug}-v");
        let last = read_dir_or_empty(&self.outbox())
            .unwrap_or_default()
            .iter()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                name.strip_prefix(&prefix)?
                    .strip_suffix(".mp4")?
                    .parse::<u32>()
                    .ok()
            })
            .max()
            .unwrap_or(0);
        self.outbox().join(format!("{prefix}{}.mp4", last + 1))
    }
}

/// Moves the newest snapshot in `from` into project.json after saving the current version to `to`.
fn step(project_dir: &Path, from: &Path, to: &Path, what: &str) -> Result<Project> {
    let Some(snapshot) = snapshots(from)?.pop() else {
        bail!("nothing to {what} in {}", project_dir.display())
    };
    let project = Project::load(&snapshot)?;
    let file = project_dir.join(PROJECT_FILE);
    if file.exists() {
        push_snapshot(to, &file)?;
    }
    fs::rename(&snapshot, &file).with_context(|| format!("restoring {}", file.display()))?;
    Ok(project)
}

/// Snapshot files (`000001.json`, ...) oldest first.
fn snapshots(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut found: Vec<(u64, PathBuf)> = read_dir_or_empty(dir)?
        .into_iter()
        .filter_map(|e| {
            let n = e
                .file_name()
                .to_str()?
                .strip_suffix(".json")?
                .parse()
                .ok()?;
            Some((n, e.path()))
        })
        .collect();
    found.sort();
    Ok(found.into_iter().map(|(_, p)| p).collect())
}

fn push_snapshot(dir: &Path, file: &Path) -> Result<()> {
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let existing = snapshots(dir)?;
    let next = existing
        .last()
        .and_then(|p| p.file_stem()?.to_str()?.parse::<u64>().ok())
        .unwrap_or(0)
        + 1;
    let target = dir.join(format!("{next:06}.json"));
    fs::copy(file, &target)
        .with_context(|| format!("saving undo snapshot {}", target.display()))?;
    let over = (existing.len() + 1).saturating_sub(HISTORY_KEEP);
    for old in &existing[..over] {
        fs::remove_file(old).with_context(|| format!("pruning {}", old.display()))?;
    }
    Ok(())
}

fn read_dir_or_empty(dir: &Path) -> Result<Vec<fs::DirEntry>> {
    match fs::read_dir(dir) {
        Ok(entries) => Ok(entries.filter_map(Result::ok).collect()),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e).with_context(|| format!("reading {}", dir.display())),
    }
}

/// Writes `text` only when `path` does not exist yet.
fn write_new(path: &Path, text: &str) -> Result<()> {
    use std::io::Write;
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(mut f) => f
            .write_all(text.as_bytes())
            .with_context(|| format!("writing {}", path.display())),
        Err(e) if e.kind() == ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e).with_context(|| format!("creating {}", path.display())),
    }
}

/// Replaces VeeCut's marked block in `path`, or appends it, keeping everything else.
fn upsert_block(path: &Path, block: &str) -> Result<()> {
    let old = fs::read_to_string(path).unwrap_or_default();
    let new = match (old.find(BLOCK_START), old.find(BLOCK_END)) {
        (Some(start), Some(end)) if end > start => {
            let mut end = end + BLOCK_END.len();
            if old[end..].starts_with('\n') {
                end += 1;
            }
            format!("{}{block}{}", &old[..start], &old[end..])
        }
        _ if old.trim().is_empty() => block.to_string(),
        _ => format!("{}\n\n{block}", old.trim_end()),
    };
    if new != old {
        fs::write(path, new).with_context(|| format!("writing {}", path.display()))?;
    }
    Ok(())
}

/// Appends `line` to `path` unless a line already equals it.
fn ensure_line(path: &Path, line: &str) -> Result<()> {
    let old = fs::read_to_string(path).unwrap_or_default();
    if old.lines().any(|l| l.trim() == line) {
        return Ok(());
    }
    let new = if old.trim().is_empty() {
        format!("{line}\n")
    } else {
        format!("{}\n\n{line}\n", old.trim_end())
    };
    fs::write(path, new).with_context(|| format!("writing {}", path.display()))
}

/// Adds `veecut mcp` as a local MCP server (https://opencode.ai/docs/mcp-servers) to opencode.json,
/// keeping the rest of the file.
fn register_opencode_mcp(path: &Path) -> Result<()> {
    let old = fs::read_to_string(path).unwrap_or_default();
    let mut config = if old.trim().is_empty() {
        serde_json::json!({ "$schema": "https://opencode.ai/config.json" })
    } else {
        match serde_json::from_str::<serde_json::Value>(&old) {
            Ok(v) if v.is_object() => v,
            // Comments (JSONC) or something else the user wrote: leave their file alone.
            _ => return Ok(()),
        }
    };
    let mcp = config
        .as_object_mut()
        .expect("checked above")
        .entry("mcp")
        .or_insert_with(|| serde_json::json!({}));
    let Some(mcp) = mcp.as_object_mut() else {
        return Ok(());
    };
    mcp.insert(
        "veecut".into(),
        serde_json::json!({ "type": "local", "command": ["veecut", "mcp"], "enabled": true }),
    );
    let new = serde_json::to_string_pretty(&config)? + "\n";
    if new != old {
        fs::write(path, new).with_context(|| format!("writing {}", path.display()))?;
    }
    Ok(())
}

/// Lowercase ASCII words joined by dashes, at most 40 characters; `video` when nothing is left.
fn slugify(name: &str) -> String {
    let mut slug = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    slug.truncate(40);
    let slug = slug.trim_end_matches('-');
    if slug.is_empty() {
        "video".into()
    } else {
        slug.into()
    }
}

/// `to` written relative to the folder `from` (both absolute and sharing a root).
fn relative(from: &Path, to: &Path) -> PathBuf {
    let from: Vec<_> = from.components().collect();
    let to: Vec<_> = to.components().collect();
    let common = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    let mut out = PathBuf::new();
    for _ in common..from.len() {
        out.push("..");
    }
    out.extend(&to[common..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_selectors_accept_slugs_folders_and_project_files() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = Workspace {
            root: tmp.path().to_path_buf(),
        };
        fs::create_dir_all(ws.root.join(".veecut")).unwrap();
        fs::write(ws.root.join(MARKER), "version = 1\n").unwrap();
        let dir = ws.projects().join("my-edit");
        fs::create_dir_all(&dir).unwrap();
        Project::new("My edit", VideoKind::default(), Canvas::new(1080, 1920))
            .save(&dir.join(PROJECT_FILE))
            .unwrap();
        let expected = dir.canonicalize().unwrap();
        for selector in [
            "my-edit".to_string(),
            "projects/my-edit".into(),
            "projects/my-edit/project.json".into(),
            dir.display().to_string(),
            dir.join(PROJECT_FILE).display().to_string(),
        ] {
            assert_eq!(
                ws.resolve_project(Some(&selector)).unwrap(),
                expected,
                "{selector}"
            );
        }
        assert_eq!(
            ws.resolve_project(None).unwrap().canonicalize().unwrap(),
            expected
        );
        fs::write(dir.join("other.json"), "{}").unwrap();
        for selector in ["", "missing", "projects/my-edit/other.json"] {
            let error = ws.resolve_project(Some(selector)).unwrap_err().to_string();
            assert!(
                error.contains("veecut status -p projects/my-edit/project.json"),
                "{error}"
            );
        }
    }

    #[test]
    fn init_is_idempotent_and_keeps_user_files() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = Workspace::init(tmp.path()).unwrap();
        for kind in VideoKind::ALL {
            assert!(
                ws.root
                    .join(format!(
                        ".agents/skills/veecut/workflows/{}.md",
                        kind.slug()
                    ))
                    .is_file()
            );
        }
        fs::write(ws.style_path(), "pace = \"tight\"\n").unwrap();
        fs::write(ws.root.join(".env"), "GROQ_API_KEY=mine\n").unwrap();
        fs::write(ws.root.join(".gitignore"), "custom\n").unwrap();
        fs::write(
            ws.root.join("AGENTS.md"),
            format!("My notes.\n\n{AGENTS_BLOCK}"),
        )
        .unwrap();
        let skill = ws.root.join(".claude/skills/veecut/SKILL.md");
        fs::write(&skill, "edited").unwrap();
        fs::write(ws.root.join(".agents/skills/veecut/stale.md"), "old").unwrap();

        let again = Workspace::init(&ws.root).unwrap();
        assert_eq!(again, ws);
        assert_eq!(
            fs::read_to_string(ws.style_path()).unwrap(),
            "pace = \"tight\"\n"
        );
        assert_eq!(
            fs::read_to_string(ws.root.join(".env")).unwrap(),
            "GROQ_API_KEY=mine\n"
        );
        assert_eq!(
            fs::read_to_string(ws.root.join(".gitignore")).unwrap(),
            "custom\n"
        );
        let agents = fs::read_to_string(ws.root.join("AGENTS.md")).unwrap();
        assert!(agents.starts_with("My notes."));
        assert_eq!(agents.matches(BLOCK_START).count(), 1);
        assert_eq!(
            fs::read_to_string(ws.root.join("CLAUDE.md")).unwrap(),
            "@AGENTS.md\n"
        );
        assert_eq!(fs::read_to_string(&skill).unwrap(), SKILL_FILES[0].1);
        assert!(!ws.root.join(".agents/skills/veecut/stale.md").exists());
        let opencode: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(ws.root.join("opencode.json")).unwrap())
                .unwrap();
        assert_eq!(
            opencode["mcp"]["veecut"]["command"],
            serde_json::json!(["veecut", "mcp"])
        );
        assert_eq!(Workspace::open(&ws.inbox()).unwrap(), ws);
    }

    #[test]
    fn undo_then_redo_walks_the_history() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = Workspace::init(tmp.path()).unwrap();
        let (dir, mut project) = ws
            .create_project("My Talk!", VideoKind::Podcast, Canvas::landscape())
            .unwrap();
        assert!(
            dir.file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("my-talk-")
        );
        assert!(ws.undo(&dir).is_err());
        for name in ["two", "three"] {
            project.name = name.into();
            ws.save_project(&dir, &project).unwrap();
            ws.save_project(&dir, &project).unwrap(); // unchanged: no extra undo step
        }
        assert_eq!(ws.undo(&dir).unwrap().name, "two");
        assert_eq!(ws.undo(&dir).unwrap().name, "My Talk!");
        assert!(ws.undo(&dir).is_err());
        assert_eq!(ws.redo(&dir).unwrap().name, "two");
        assert_eq!(ws.redo(&dir).unwrap().name, "three");
        assert!(ws.redo(&dir).is_err());
        assert_eq!(ws.undo(&dir).unwrap().name, "two");
        project.name = "four".into();
        ws.save_project(&dir, &project).unwrap();
        assert!(ws.redo(&dir).is_err(), "a new save clears redo");
        assert_eq!(Project::load(&dir.join(PROJECT_FILE)).unwrap().name, "four");
        assert_eq!(
            ws.list_projects().unwrap(),
            vec![(
                dir.file_name().unwrap().to_string_lossy().into_owned(),
                dir.clone()
            )]
        );
        assert_eq!(ws.next_draft_path("talk"), ws.outbox().join("talk-v1.mp4"));
        fs::write(ws.outbox().join("talk-v3.mp4"), "").unwrap();
        assert_eq!(ws.next_draft_path("talk"), ws.outbox().join("talk-v4.mp4"));
    }

    #[test]
    fn inbox_assets_get_paths_relative_to_the_project() {
        let Ok(ffmpeg) = media::ffmpeg() else { return };
        let tmp = tempfile::tempdir().unwrap();
        let ws = Workspace::init(tmp.path()).unwrap();
        let clip = ws.inbox().join("clip.mp4");
        let made = std::process::Command::new(ffmpeg)
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=64x64:duration=1",
                "-f",
                "lavfi",
                "-i",
                "sine=duration=1",
            ])
            .args(["-shortest", "-pix_fmt", "yuv420p"])
            .arg(&clip)
            .status()
            .unwrap();
        assert!(made.success());
        assert_eq!(ws.newest_inbox_video(), Some(clip.clone()));
        let (dir, mut project) = ws
            .create_project("a", VideoKind::Vlog, Canvas::vertical())
            .unwrap();
        let id = ws
            .add_asset(&mut project, &dir, &clip, AssetRole::Source)
            .unwrap();
        let asset = project.asset(&id).unwrap();
        assert_eq!(asset.path, Path::new("../../inbox/clip.mp4"));
        assert!(project.asset_path(&dir, asset).is_file());
        assert!(asset.has_audio && asset.duration > 0.9);
    }

    #[test]
    fn history_keeps_the_newest_fifty() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("p.json");
        fs::write(&file, "{}").unwrap();
        let dir = tmp.path().join("h");
        for _ in 0..HISTORY_KEEP + 3 {
            push_snapshot(&dir, &file).unwrap();
        }
        let kept = snapshots(&dir).unwrap();
        assert_eq!(kept.len(), HISTORY_KEEP);
        assert!(kept[0].ends_with("000004.json"));
    }
}
