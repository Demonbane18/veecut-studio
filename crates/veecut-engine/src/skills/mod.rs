//! Skills catalog synced from awesome-claude-video-skills, with install, update and auto-update.
//!
//! Installing never runs anything from a skill repo: VeeCut downloads the tarball at a pinned
//! commit, copies the folders that hold a `SKILL.md` and records the commit in `skills.lock.json`.
//! Lock-file skill names must be safe direct children of the skill roots, excluding built-in
//! `veecut`; the entire lock is checked before removal or replacement can delete any directory.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{Read, Seek};
use std::path::{Component, Path, PathBuf};
use std::sync::LazyLock;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::config::{self, Config};
use crate::project::VideoKind;
use crate::workspace::Workspace;

/// The awesome list the catalog comes from.
pub const LIST_REPO: &str = "zhuyansen/awesome-claude-video-skills";
pub const LOCK_FILE: &str = "skills.lock.json";
const MAX_ARCHIVE: u64 = 200 * 1024 * 1024;
const MAX_FILE: u64 = 25 * 1024 * 1024;

/// The synced catalog, saved as `~/.veecut/catalog.json`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Catalog {
    /// Commit of the awesome list the catalog was read from.
    pub source_commit: String,
    pub fetched_at: DateTime<Utc>,
    pub categories: Vec<Category>,
    pub entries: Vec<Entry>,
}

/// A section of the list, like `editing` titled "✂️ Editing".
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Category {
    pub id: String,
    pub title: String,
}

/// One repo on the list.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    /// `owner/name`.
    pub repo: String,
    pub url: String,
    pub stars: u32,
    pub description: String,
    /// Agent Skills Hub grade: `SAFE`, `CAUTION`, `pending`...
    pub security: String,
    /// Category ids.
    pub categories: Vec<String>,
    /// Made a film in the list's end-to-end test.
    pub tested: bool,
}

impl Entry {
    pub fn is_safe(&self) -> bool {
        is_safe(&self.security)
    }
}

fn is_safe(grade: &str) -> bool {
    grade.eq_ignore_ascii_case("SAFE")
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SyncReport {
    pub commit: String,
    pub entries: usize,
    pub categories: usize,
}

/// Installed catalog skills, saved as `skills.lock.json` in the workspace.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Lock {
    pub repos: Vec<LockedRepo>,
}

/// A repo installed at a pinned commit and the skill folders it added.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LockedRepo {
    pub repo: String,
    pub commit: String,
    /// Folder names under `.agents/skills/` and `.claude/skills/`.
    pub skills: Vec<String>,
    /// SPDX id from GitHub, if it detected one.
    pub license: Option<String>,
    pub security: String,
    pub installed_at: DateTime<Utc>,
}

impl Lock {
    pub fn load(path: &Path) -> Result<Lock> {
        match fs::read_to_string(path) {
            Ok(text) => {
                let lock: Lock = serde_json::from_str(&text)
                    .with_context(|| format!("{} is not valid JSON", path.display()))?;
                for repo in &lock.repos {
                    for name in &repo.skills {
                        let mut components = Path::new(name).components();
                        if name.eq_ignore_ascii_case("veecut")
                            || safe_name(name).as_deref() != Some(name.as_str())
                            || !matches!(
                                (components.next(), components.next()),
                                (Some(Component::Normal(_)), None)
                            )
                        {
                            bail!(
                                "{} has unsafe skill name {name:?} in {}: expected one safe directory name other than built-in veecut",
                                path.display(),
                                repo.repo
                            );
                        }
                    }
                }
                Ok(lock)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Lock::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        fs::write(path, serde_json::to_string_pretty(self)? + "\n")
            .with_context(|| format!("writing {}", path.display()))
    }

    pub fn get(&self, repo: &str) -> Option<&LockedRepo> {
        self.repos
            .iter()
            .find(|r| r.repo.eq_ignore_ascii_case(repo))
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InstallReport {
    pub repo: String,
    pub commit: String,
    /// Installed skill folder names.
    pub skills: Vec<String>,
    pub license: Option<String>,
    /// Files left out because they are over 25 MB.
    pub skipped: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UpdateReport {
    pub repo: String,
    pub from: String,
    pub to: String,
    pub updated: bool,
    /// Why this repo could not be checked or updated; the others still were.
    pub error: Option<String>,
}

/// Fetches the awesome list at its newest commit and writes `~/.veecut/catalog.json`.
pub fn sync() -> Result<SyncReport> {
    let commit = head_commit(LIST_REPO, "main")?;
    let url = format!("https://raw.githubusercontent.com/{LIST_REPO}/{commit}/README.md");
    let readme = fetch(&url, "text/plain")?
        .body_mut()
        .read_to_string()
        .with_context(|| format!("reading {url}"))?;
    let (categories, entries) = parse_readme(&readme);
    if entries.is_empty() {
        bail!(
            "no entries found in {url}; the list's format may have changed. Update VeeCut or report it."
        );
    }
    let catalog = Catalog {
        source_commit: commit,
        fetched_at: Utc::now(),
        categories,
        entries,
    };
    let path = catalog_path();
    fs::create_dir_all(config::home())
        .with_context(|| format!("creating {}", config::home().display()))?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_string_pretty(&catalog)?)
        .with_context(|| format!("writing {}", tmp.display()))?;
    fs::rename(&tmp, &path).with_context(|| format!("saving {}", path.display()))?;
    Ok(SyncReport {
        commit: catalog.source_commit,
        entries: catalog.entries.len(),
        categories: catalog.categories.len(),
    })
}

fn catalog_path() -> PathBuf {
    config::home().join("catalog.json")
}

/// The saved catalog; `None` before the first sync.
pub fn load_catalog() -> Result<Option<Catalog>> {
    let path = catalog_path();
    match fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text).map(Some).with_context(|| {
            format!(
                "{} is damaged; run `veecut skills sync` to rebuild it",
                path.display()
            )
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// Syncs when the catalog is missing or older than `auto_sync_hours`; `None` when it is fresh or
/// automatic syncing is off.
pub fn auto_sync_if_stale(config: &Config) -> Option<Result<SyncReport>> {
    let hours = config.skills.auto_sync_hours;
    if hours == 0 {
        return None;
    }
    let fresh = matches!(load_catalog(), Ok(Some(c)) if Utc::now() - c.fetched_at < chrono::TimeDelta::hours(hours.into()));
    (!fresh).then(sync)
}

/// [`auto_sync_if_stale`], then, when `auto_update` is on, updates the workspace's installed
/// SAFE skills whose upstream moved.
pub fn auto_sync_and_update(
    ws: &Workspace,
    config: &Config,
) -> Option<Result<(SyncReport, Vec<UpdateReport>)>> {
    let synced = auto_sync_if_stale(config)?;
    Some(synced.and_then(|report| {
        let updates = if config.skills.auto_update {
            update_where(ws, None, true)?
        } else {
            Vec::new()
        };
        Ok((report, updates))
    }))
}

/// Entries whose repo or description holds every word of `query`, optionally in one category
/// (id or part of the title). Tested first, then by stars.
pub fn search<'a>(catalog: &'a Catalog, query: &str, category: Option<&str>) -> Vec<&'a Entry> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    let category = category.map(|c| {
        let c = c.trim().to_lowercase();
        let c = c.strip_prefix("type-").unwrap_or(&c);
        catalog
            .categories
            .iter()
            .find(|k| k.id == c || k.title.to_lowercase().contains(c))
            .map_or_else(|| c.to_string(), |k| k.id.clone())
    });
    let mut found: Vec<&Entry> = catalog
        .entries
        .iter()
        .filter(|e| category.as_ref().is_none_or(|c| e.categories.contains(c)))
        .filter(|e| {
            let text = format!("{} {}", e.repo, e.description).to_lowercase();
            words.iter().all(|w| text.contains(w.as_str()))
        })
        .collect();
    found.sort_by(|a, b| b.tested.cmp(&a.tested).then(b.stars.cmp(&a.stars)));
    found
}

/// Best catalog entries for a kind of video: SAFE, tested first, then stars. Data-driven, so it
/// follows the catalog as it updates.
pub fn recommended<'a>(catalog: &'a Catalog, kind: VideoKind, n: usize) -> Vec<&'a Entry> {
    let wanted: &[&str] = match kind {
        VideoKind::TalkingHeadShort
        | VideoKind::TalkingHeadLong
        | VideoKind::Podcast
        | VideoKind::Clips => &["editing", "shorts"],
        VideoKind::Tutorial => &["explainer", "editing"],
        VideoKind::Explainer => &["explainer", "motion"],
        VideoKind::Promo => &["promo", "motion"],
        VideoKind::Vlog | VideoKind::Documentary => &["editing", "motion"],
        VideoKind::MusicVideo => &["music"],
    };
    let mut found: Vec<&Entry> = catalog
        .entries
        .iter()
        .filter(|e| e.is_safe() && e.categories.iter().any(|c| wanted.contains(&c.as_str())))
        .collect();
    found.sort_by(|a, b| b.tested.cmp(&a.tested).then(b.stars.cmp(&a.stars)));
    found.truncate(n);
    found
}

/// The workspace's `skills.lock.json`.
pub fn installed(ws: &Workspace) -> Result<Lock> {
    Lock::load(&ws.root.join(LOCK_FILE))
}

/// Installs every skill in `repo` (`owner/name` or a GitHub URL) at its default branch's newest
/// commit. Entries not graded SAFE are refused unless `force` is set or `safe_only` is off.
pub fn install(ws: &Workspace, repo: &str, force: bool) -> Result<InstallReport> {
    let repo = normalize_repo(repo)?;
    let config = Config::load()?;
    let catalog = load_catalog()?;
    let entry = catalog.as_ref().and_then(|c| {
        c.entries
            .iter()
            .find(|e| e.repo.eq_ignore_ascii_case(&repo))
    });
    let security = entry.map(|e| e.security.clone()).unwrap_or_default();
    if config.skills.safe_only && !force && !is_safe(&security) {
        match entry {
            None => bail!(
                "{repo} is not in the skills catalog, so it has no security grade. Run `veecut skills sync`, or install it anyway with --force if you trust it."
            ),
            Some(e) => bail!(
                "{repo} is graded {} on Agent Skills Hub, not SAFE (https://agentskillshub.top/skill/{repo}/). Install it anyway with --force only if you trust it.",
                if e.security.is_empty() {
                    "nothing"
                } else {
                    &e.security
                }
            ),
        }
    }
    let info = repo_info(&repo)?;
    let commit = head_commit(&info.full_name, &info.default_branch)?;
    install_at(
        ws,
        &info.full_name,
        &commit,
        info.license.map(|l| l.spdx_id),
        security,
    )
}

/// Reinstalls installed repos (all, or one) whose default branch moved past the locked commit.
pub fn update(ws: &Workspace, repo: Option<&str>) -> Result<Vec<UpdateReport>> {
    update_where(ws, repo, false)
}

fn update_where(ws: &Workspace, repo: Option<&str>, safe_only: bool) -> Result<Vec<UpdateReport>> {
    let lock = installed(ws)?;
    let wanted: Vec<LockedRepo> = match repo {
        Some(r) => {
            let r = normalize_repo(r)?;
            vec![
                lock.get(&r)
                    .cloned()
                    .ok_or_else(|| anyhow!("{r} is not installed in this workspace"))?,
            ]
        }
        None => lock.repos.clone(),
    };
    let catalog = load_catalog().ok().flatten();
    let mut reports = Vec::new();
    for locked in wanted {
        let security = catalog
            .as_ref()
            .and_then(|c| {
                c.entries
                    .iter()
                    .find(|e| e.repo.eq_ignore_ascii_case(&locked.repo))
            })
            .map_or_else(|| locked.security.clone(), |e| e.security.clone());
        if safe_only && !is_safe(&security) {
            continue;
        }
        let mut report = UpdateReport {
            repo: locked.repo.clone(),
            from: locked.commit.clone(),
            to: locked.commit.clone(),
            updated: false,
            error: None,
        };
        let result = (|| -> Result<()> {
            let info = repo_info(&locked.repo)?;
            report.to = head_commit(&locked.repo, &info.default_branch)?;
            if report.to != locked.commit {
                install_at(
                    ws,
                    &locked.repo,
                    &report.to,
                    info.license.map(|l| l.spdx_id),
                    security,
                )?;
                report.updated = true;
            }
            Ok(())
        })();
        if let Err(e) = result {
            if repo.is_some() {
                return Err(e);
            }
            report.error = Some(format!("{e:#}"));
        }
        reports.push(report);
    }
    Ok(reports)
}

/// Deletes the repo's skill folders and its lock entry.
pub fn remove(ws: &Workspace, repo: &str) -> Result<()> {
    let repo = normalize_repo(repo)?;
    let path = ws.root.join(LOCK_FILE);
    let mut lock = Lock::load(&path)?;
    let Some(i) = lock
        .repos
        .iter()
        .position(|r| r.repo.eq_ignore_ascii_case(&repo))
    else {
        bail!("{repo} is not installed in this workspace");
    };
    let locked = lock.repos.remove(i);
    remove_skill_dirs(ws, &locked.skills)?;
    lock.save(&path)
}

fn remove_skill_dirs(ws: &Workspace, names: &[String]) -> Result<()> {
    for base in ws.skill_dirs() {
        for name in names {
            let dir = base.join(name);
            match fs::remove_dir_all(&dir) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                    return Err(e).with_context(|| format!("removing {}", dir.display()));
                }
                _ => {}
            }
        }
    }
    Ok(())
}

/// Downloads `repo` at `commit`, swaps in its skill folders and records it in the lock file.
fn install_at(
    ws: &Workspace,
    repo: &str,
    commit: &str,
    license: Option<String>,
    security: String,
) -> Result<InstallReport> {
    let mut archive = tempfile::tempfile().context("creating a temporary file for the download")?;
    let url = format!("https://codeload.github.com/{repo}/tar.gz/{commit}");
    let mut body = fetch(&url, "application/octet-stream")?
        .into_body()
        .into_reader()
        .take(MAX_ARCHIVE + 1);
    let size =
        std::io::copy(&mut body, &mut archive).with_context(|| format!("downloading {url}"))?;
    if size > MAX_ARCHIVE {
        bail!("{repo} is over 200 MB as an archive; VeeCut does not install repos that large");
    }

    // Pass 1: find every folder holding a SKILL.md.
    archive.rewind()?;
    let mut skills: Vec<(PathBuf, String)> = Vec::new();
    for_each_file(&archive, |rel, size, reader| {
        if rel.file_name().is_some_and(|n| n == "SKILL.md") && size <= MAX_FILE {
            let mut text = String::new();
            reader.read_to_string(&mut text).ok();
            skills.push((rel.parent().unwrap_or(Path::new("")).to_path_buf(), text));
        }
        Ok(())
    })?;
    if skills.is_empty() {
        bail!(
            "{repo} has no SKILL.md, so it is not an agent skill VeeCut can install. See how to use it: https://github.com/{repo}#readme"
        );
    }

    // Folder names: frontmatter `name:`, else the folder name; `<owner>-<name>` on a clash.
    let lock_path = ws.root.join(LOCK_FILE);
    let mut lock = Lock::load(&lock_path)?;
    let mine: HashSet<String> = lock
        .get(repo)
        .map(|r| r.skills.iter().cloned().collect())
        .unwrap_or_default();
    let mut taken: HashSet<String> = HashSet::from(["veecut".to_string()]);
    for base in ws.skill_dirs() {
        if let Ok(entries) = fs::read_dir(&base) {
            taken.extend(entries.filter_map(|e| e.ok()?.file_name().into_string().ok()));
        }
    }
    taken.extend(
        lock.repos
            .iter()
            .filter(|r| !r.repo.eq_ignore_ascii_case(repo))
            .flat_map(|r| r.skills.iter().cloned()),
    );
    taken.retain(|n| !mine.contains(n));
    let (owner, repo_name) = repo
        .split_once('/')
        .expect("normalized repos are owner/name");
    let mut names = Vec::new();
    for (dir, text) in &skills {
        let base = frontmatter_name(text)
            .or_else(|| {
                dir.file_name()
                    .and_then(|n| safe_name(&n.to_string_lossy()))
            })
            .or_else(|| safe_name(repo_name))
            .unwrap_or_else(|| "skill".into());
        let mut name = base.clone();
        if name.eq_ignore_ascii_case("veecut") || taken.contains(&name) {
            name = format!("{}-{base}", safe_name(owner).unwrap_or_default());
        }
        let mut n = 2;
        while taken.contains(&name) {
            name = format!("{}-{base}-{n}", safe_name(owner).unwrap_or_default());
            n += 1;
        }
        taken.insert(name.clone());
        names.push(name);
    }

    // Pass 2: stage the skill folders, then swap them in.
    let staging_root = ws.root.join(".veecut");
    fs::create_dir_all(&staging_root)?;
    let staging = tempfile::tempdir_in(&staging_root).context("creating a staging folder")?;
    let mut skipped = Vec::new();
    archive.rewind()?;
    for_each_file(&archive, |rel, size, reader| {
        let owners: Vec<usize> = (0..skills.len())
            .filter(|&i| rel.starts_with(&skills[i].0))
            .collect();
        if owners.is_empty() {
            return Ok(());
        }
        if size > MAX_FILE {
            skipped.push(rel.display().to_string());
            return Ok(());
        }
        let mut data = Vec::new();
        reader.read_to_end(&mut data)?;
        for i in owners {
            let dest = staging
                .path()
                .join(&names[i])
                .join(rel.strip_prefix(&skills[i].0).expect("checked above"));
            fs::create_dir_all(dest.parent().expect("files sit in a folder"))?;
            fs::write(&dest, &data).with_context(|| format!("writing {}", dest.display()))?;
        }
        Ok(())
    })?;
    remove_skill_dirs(ws, &mine.into_iter().collect::<Vec<_>>())?;
    for base in ws.skill_dirs() {
        for name in &names {
            copy_dir(&staging.path().join(name), &base.join(name))?;
        }
    }

    lock.repos.retain(|r| !r.repo.eq_ignore_ascii_case(repo));
    lock.repos.push(LockedRepo {
        repo: repo.to_string(),
        commit: commit.to_string(),
        skills: names.clone(),
        license: license.clone(),
        security,
        installed_at: Utc::now(),
    });
    lock.save(&lock_path)?;
    Ok(InstallReport {
        repo: repo.to_string(),
        commit: commit.to_string(),
        skills: names,
        license,
        skipped,
    })
}

/// Calls `f(path inside the repo, size, contents)` for every regular file in a GitHub tarball,
/// skipping `.git`, `node_modules`, links and any path that tries to leave the repo.
fn for_each_file(
    archive: &fs::File,
    mut f: impl FnMut(&Path, u64, &mut dyn Read) -> Result<()>,
) -> Result<()> {
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(archive));
    for entry in tar
        .entries()
        .context("the download is not a valid tar.gz archive")?
    {
        let mut entry = entry.context("reading the archive")?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let path = entry
            .path()
            .context("reading a path in the archive")?
            .into_owned();
        // The first component is the `<repo>-<commit>/` folder GitHub wraps everything in.
        let rel: PathBuf = path.components().skip(1).collect();
        let safe = rel
            .components()
            .all(|c| matches!(c, Component::Normal(n) if n != ".git" && n != "node_modules"));
        if !safe || rel.as_os_str().is_empty() {
            continue;
        }
        let size = entry.size();
        f(&rel, size, &mut entry)?;
    }
    Ok(())
}

fn copy_dir(from: &Path, to: &Path) -> Result<()> {
    fs::create_dir_all(to).with_context(|| format!("creating {}", to.display()))?;
    for entry in fs::read_dir(from).with_context(|| format!("reading {}", from.display()))? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), &target)
                .with_context(|| format!("copying to {}", target.display()))?;
        }
    }
    Ok(())
}

/// `name:` from a SKILL.md's YAML frontmatter, made safe as a folder name.
fn frontmatter_name(text: &str) -> Option<String> {
    let mut lines = text.trim_start_matches('\u{feff}').lines();
    if lines.next()?.trim() != "---" {
        return None;
    }
    lines
        .take_while(|l| l.trim() != "---")
        .find_map(|l| l.strip_prefix("name:"))
        .and_then(|v| safe_name(v.trim().trim_matches(['"', '\''])))
}

/// Letters, digits, `.`, `_` and `-` only, without leading dots or dashes.
fn safe_name(raw: &str) -> Option<String> {
    let name: String = raw
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let name = name.trim_matches(['-', '.']);
    (!name.is_empty()).then(|| name.to_string())
}

/// `owner/name` from `owner/name`, a GitHub URL or `owner/name.git`.
fn normalize_repo(input: &str) -> Result<String> {
    let s = input.trim();
    let s = s
        .strip_prefix("https://")
        .or_else(|| s.strip_prefix("http://"))
        .unwrap_or(s);
    let s = s.strip_prefix("www.").unwrap_or(s);
    let s = s.strip_prefix("github.com/").unwrap_or(s);
    let mut parts = s.split('/');
    let valid = |p: &str| {
        !p.is_empty()
            && p != "."
            && p != ".."
            && p.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    match (
        parts.next(),
        parts.next().map(|r| r.trim_end_matches(".git")),
    ) {
        (Some(owner), Some(name)) if valid(owner) && valid(name) => Ok(format!("{owner}/{name}")),
        _ => bail!("{input:?} is not a GitHub repository; write it as owner/name"),
    }
}

static HTTP: LazyLock<ureq::Agent> = LazyLock::new(|| {
    ureq::Agent::config_builder()
        .user_agent("veecut")
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_recv_response(Some(Duration::from_secs(30)))
        .timeout_per_read(Some(Duration::from_secs(60)))
        .build()
        .into()
});

/// GET with readable errors. Sends `GITHUB_TOKEN` to api.github.com when it is set.
fn fetch(url: &str, accept: &str) -> Result<ureq::http::Response<ureq::Body>> {
    let mut req = HTTP.get(url).header("Accept", accept);
    if url.starts_with("https://api.github.com/") {
        if let Some(token) = std::env::var("GITHUB_TOKEN")
            .ok()
            .filter(|t| !t.trim().is_empty())
        {
            req = req.header("Authorization", format!("Bearer {}", token.trim()));
        }
    }
    req.call().map_err(|e| match e {
        ureq::Error::StatusCode(403 | 429) => anyhow!(
            "GitHub refused {url}, most likely its hourly rate limit. Add GITHUB_TOKEN=<a token> to .env, or try again in an hour."
        ),
        ureq::Error::StatusCode(404) => anyhow!("{url} was not found; check the repository name"),
        ureq::Error::StatusCode(code) => anyhow!("{url} answered HTTP {code}"),
        e => anyhow!("could not reach {url}: {e}. Check the internet connection."),
    })
}

/// The commit sha a branch or tag points to.
fn head_commit(repo: &str, git_ref: &str) -> Result<String> {
    let url = format!("https://api.github.com/repos/{repo}/commits/{git_ref}");
    let sha = fetch(&url, "application/vnd.github.sha")?
        .body_mut()
        .read_to_string()?;
    let sha = sha.trim();
    if sha.len() != 40 || !sha.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("GitHub returned an unexpected commit id for {repo}");
    }
    Ok(sha.to_string())
}

#[derive(Deserialize)]
struct RepoInfo {
    full_name: String,
    default_branch: String,
    license: Option<License>,
}

#[derive(Deserialize)]
struct License {
    spdx_id: String,
}

fn repo_info(repo: &str) -> Result<RepoInfo> {
    let url = format!("https://api.github.com/repos/{repo}");
    fetch(&url, "application/vnd.github+json")?
        .body_mut()
        .read_json()
        .with_context(|| format!("unexpected answer from {url}"))
}

/// Categories and merged entries from the list's README: every `<a id="type-…">` section, the
/// tested table (`<a id="tested">`) and the "Made with Claude Opus" table.
fn parse_readme(text: &str) -> (Vec<Category>, Vec<Entry>) {
    enum Section {
        Other,
        Category(String),
        Tested,
        Opus,
    }
    let mut categories = Vec::new();
    let mut entries: Vec<Entry> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut anchor: Option<String> = None;
    let mut section = Section::Other;
    let mut columns: Option<Vec<String>> = None;
    for line in text.lines() {
        let line = line.trim();
        if let Some(id) = line
            .strip_prefix("<a id=\"")
            .and_then(|r| r.split('"').next())
        {
            anchor = Some(id.to_string());
            continue;
        }
        if let Some(title) = line.strip_prefix("## ") {
            section = match anchor.take() {
                Some(id) if id == "tested" => Section::Tested,
                Some(id) if id.starts_with("made-with") => Section::Opus,
                Some(id) if id.starts_with("type-") => {
                    let id = id["type-".len()..].to_string();
                    categories.push(Category {
                        id: id.clone(),
                        title: title.trim().to_string(),
                    });
                    Section::Category(id)
                }
                _ => Section::Other,
            };
            continue;
        }
        if !line.starts_with('|') {
            columns = None;
            continue;
        }
        if matches!(section, Section::Other) {
            continue;
        }
        let cells = split_row(line);
        let Some(cols) = &columns else {
            columns = Some(cells.iter().map(|c| c.to_lowercase()).collect());
            continue;
        };
        if cells
            .iter()
            .all(|c| !c.is_empty() && c.chars().all(|ch| matches!(ch, '-' | ':')))
        {
            continue;
        }
        let col = |names: &[&str]| {
            cols.iter()
                .position(|c| names.contains(&c.as_str()))
                .and_then(|i| cells.get(i))
        };
        let Some(url) = col(&["repo", "skill"]).and_then(|c| link_target(c)) else {
            continue;
        };
        let Ok(repo) = normalize_repo(&url) else {
            continue;
        };
        let entry = Entry {
            url: format!("https://github.com/{repo}"),
            repo,
            stars: col(&["stars", "★"])
                .and_then(|c| parse_stars(c))
                .unwrap_or(0),
            description: col(&["what it does", "description"])
                .cloned()
                .unwrap_or_default(),
            security: col(&["security"]).map(|c| grade(c)).unwrap_or_default(),
            categories: match &section {
                Section::Category(id) => vec![id.clone()],
                _ => Vec::new(),
            },
            tested: matches!(section, Section::Tested),
        };
        match index.get(&entry.repo.to_lowercase()) {
            Some(&i) => {
                let e = &mut entries[i];
                e.stars = e.stars.max(entry.stars);
                if e.description.is_empty() {
                    e.description = entry.description;
                }
                if e.security.is_empty() {
                    e.security = entry.security;
                }
                for c in entry.categories {
                    if !e.categories.contains(&c) {
                        e.categories.push(c);
                    }
                }
                e.tested |= entry.tested;
            }
            None => {
                index.insert(entry.repo.to_lowercase(), entries.len());
                entries.push(entry);
            }
        }
    }
    (categories, entries)
}

/// Cells of a markdown table row, with `\|` turned back into `|`.
fn split_row(line: &str) -> Vec<String> {
    let inner = line.trim().trim_start_matches('|');
    let inner = inner
        .strip_suffix('|')
        .filter(|s| !s.ends_with('\\'))
        .unwrap_or(inner);
    let mut cells = vec![String::new()];
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&'|') => {
                cells.last_mut().expect("never empty").push('|');
                chars.next();
            }
            '|' => cells.push(String::new()),
            c => cells.last_mut().expect("never empty").push(c),
        }
    }
    cells.into_iter().map(|c| c.trim().to_string()).collect()
}

/// The URL of the first markdown link in a cell.
fn link_target(cell: &str) -> Option<String> {
    let start = cell.find("](")? + 2;
    let end = cell[start..].find(')')? + start;
    Some(cell[start..end].to_string())
}

/// `[SAFE](…)` or `*pending*` to the grade text.
fn grade(cell: &str) -> String {
    let text = match (cell.find('['), cell.find("](")) {
        (Some(open), Some(close)) if open < close => &cell[open + 1..close],
        _ => cell,
    };
    text.trim_matches(['*', '_', ' ']).to_string()
}

/// `73.6k`, `1,315`, `2.1m` or `5`.
fn parse_stars(cell: &str) -> Option<u32> {
    let s = cell.trim().replace(',', "").to_lowercase();
    let (num, scale) = match s.strip_suffix('k') {
        Some(n) => (n, 1e3),
        None => match s.strip_suffix('m') {
            Some(n) => (n, 1e6),
            None => (s.as_str(), 1.0),
        },
    };
    num.trim()
        .parse::<f64>()
        .ok()
        .map(|n| (n * scale).round() as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    const README: &str = r#"# Awesome Claude Video Skills

## Contents

- [Made with Claude Opus 5.5](#made-with-opus-55)

<a id="tested"></a>
## 🧪 Tested end to end

| # | Skill | ★ | Checks passed (of 4) | Lines of copy readable (of 5) | Sound | Look, by eye | Time to make |  |
|---|---|---|---|---|---|---|---|---|
| 1 | [explainroo](https://github.com/vincentsch/explainroo) | 523 | 4/4 | 5/5 | Yes | A look of its own | 2.0 min | [evidence](https://agentskillshub.top/best-runs/video/vincentsch__explainroo.html) |
| 3 | [skills](https://github.com/vibe-motion/skills) | 1,315 | 4/4 | 5/5 | Yes | A look of its own | 4.4 min | [evidence](https://agentskillshub.top/x.html) |

<a id="made-with-opus-55"></a>
## Made with Claude Opus 5.5

| Repo | Stars | What it does | Security |
|---|---:|---|---|
| [JohnHeibel/PDoomVideo](https://github.com/JohnHeibel/PDoomVideo) | 2.6k | Source code for the music video | [SAFE](https://agentskillshub.top/skill/JohnHeibel/PDoomVideo/) |

<a id="type-general"></a>
## 🧱 Frameworks & toolkits

<table><tr>
<td align="center" valign="top"><a href="https://github.com/heygen-com/hyperframes"><img src="x.jpg"></a></td>
</tr></table>

| Repo | Stars | What it does | Security |
|---|---:|---|---|
| [heygen-com/hyperframes](https://github.com/heygen-com/hyperframes) | 73.6k | Write HTML. Render video. Built for agents. | [SAFE](https://agentskillshub.top/skill/heygen-com/hyperframes/) |
| [vibe-motion/skills](https://github.com/vibe-motion/skills) | 1.4k | agent skills for vibe motion | [SAFE](https://agentskillshub.top/skill/vibe-motion/skills/) |
| [kangarooking/director-skills](https://github.com/kangarooking/director-skills) | 245 | 导演Skill \| Director Skills: Open-source Agent Skills | [SAFE](https://agentskillshub.top/skill/kangarooking/director-skills/) |

<a id="type-promo"></a>
## 📣 Promo & demos

| Repo | Stars | What it does | Security |
|---|---:|---|---|
| [vincentsch/explainroo](https://github.com/vincentsch/explainroo) | 600 | Brief to film in two minutes | [SAFE](https://agentskillshub.top/skill/vincentsch/explainroo/) |
| [vibe-motion/skills](https://github.com/vibe-motion/skills) | 1.4k | agent skills for vibe motion | [SAFE](https://agentskillshub.top/skill/vibe-motion/skills/) |
| [someone/risky](https://github.com/someone/risky) | 9,999 | Big but risky | [CAUTION](https://agentskillshub.top/skill/someone/risky/) |
| [someone/new](https://github.com/someone/new) | 7 | Not graded yet | *pending* |

**Security** is the grade of the repo's README.

## Related collections

| Repo | Stars | What it does | Security |
|---|---:|---|---|
| [not/listed](https://github.com/not/listed) | 1 | ignored | [SAFE](https://x) |
"#;

    #[test]
    fn parses_the_awesome_list_readme() {
        let (categories, entries) = parse_readme(README);
        assert_eq!(
            categories,
            vec![
                Category {
                    id: "general".into(),
                    title: "🧱 Frameworks & toolkits".into()
                },
                Category {
                    id: "promo".into(),
                    title: "📣 Promo & demos".into()
                },
            ]
        );
        let get = |repo: &str| entries.iter().find(|e| e.repo == repo).unwrap();
        assert_eq!(entries.len(), 7, "{entries:#?}");
        let hyper = get("heygen-com/hyperframes");
        assert_eq!(
            (hyper.stars, hyper.security.as_str(), hyper.tested),
            (73_600, "SAFE", false)
        );
        assert_eq!(hyper.url, "https://github.com/heygen-com/hyperframes");
        let vibe = get("vibe-motion/skills");
        assert_eq!((vibe.stars, vibe.tested), (1_400, true));
        assert_eq!(vibe.categories, vec!["general", "promo"]);
        assert_eq!(vibe.description, "agent skills for vibe motion");
        let explainroo = get("vincentsch/explainroo");
        assert_eq!(
            (
                explainroo.stars,
                explainroo.tested,
                explainroo.categories.clone()
            ),
            (600, true, vec!["promo".to_string()])
        );
        assert_eq!(
            get("kangarooking/director-skills").description,
            "导演Skill | Director Skills: Open-source Agent Skills"
        );
        assert_eq!(get("someone/risky").security, "CAUTION");
        assert_eq!(get("someone/new").security, "pending");
        let opus = get("JohnHeibel/PDoomVideo");
        assert_eq!((opus.stars, opus.categories.is_empty()), (2_600, true));
        assert_eq!(parse_stars("1,315"), Some(1_315));

        let catalog = Catalog {
            source_commit: "x".into(),
            fetched_at: Utc::now(),
            categories,
            entries,
        };
        let promo: Vec<&str> = recommended(&catalog, VideoKind::Promo, 10)
            .iter()
            .map(|e| e.repo.as_str())
            .collect();
        assert_eq!(
            promo,
            ["vibe-motion/skills", "vincentsch/explainroo"],
            "SAFE only, tested first, then stars"
        );
        let found: Vec<&str> = search(&catalog, "director", Some("frameworks"))
            .iter()
            .map(|e| e.repo.as_str())
            .collect();
        assert_eq!(found, ["kangarooking/director-skills"]);
    }

    #[test]
    fn lock_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(LOCK_FILE);
        assert_eq!(Lock::load(&path).unwrap(), Lock::default());
        let lock = Lock {
            repos: vec![LockedRepo {
                repo: "remotion-dev/skills".into(),
                commit: "0123456789abcdef0123456789abcdef01234567".into(),
                skills: vec!["remotion".into(), "remotion-dev-video".into()],
                license: Some("MIT".into()),
                security: "SAFE".into(),
                installed_at: Utc::now(),
            }],
        };
        lock.save(&path).unwrap();
        let loaded = Lock::load(&path).unwrap();
        assert_eq!(loaded, lock);
        assert!(loaded.get("Remotion-Dev/Skills").is_some());
    }

    #[test]
    fn unsafe_lock_names_fail_before_removing_any_skill_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = Workspace {
            root: tmp.path().join("workspace"),
        };
        fs::create_dir_all(&ws.root).unwrap();
        let outside = tmp.path().join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("sentinel"), "keep").unwrap();
        for base in ws.skill_dirs() {
            fs::create_dir_all(base.join("valid-skill")).unwrap();
            fs::create_dir_all(base.join("veecut")).unwrap();
            fs::create_dir_all(base.parent().unwrap().join("outside")).unwrap();
        }
        let mut lock = Lock {
            repos: vec![LockedRepo {
                repo: "owner/repo".into(),
                commit: "0123456789abcdef0123456789abcdef01234567".into(),
                skills: Vec::new(),
                license: None,
                security: "SAFE".into(),
                installed_at: Utc::now(),
            }],
        };
        let lock_path = ws.root.join(LOCK_FILE);
        for unsafe_name in [
            "../outside",
            outside.to_str().unwrap(),
            "",
            ".",
            "..",
            "nested/skill",
            "nested\\skill",
            "veecut",
            "VeeCut",
            "valid-skill/",
        ] {
            lock.repos[0].skills = vec!["valid-skill".into(), unsafe_name.into()];
            lock.save(&lock_path).unwrap();
            for result in [remove(&ws, "owner/repo"), update(&ws, None).map(|_| ())] {
                let error = result.unwrap_err();
                assert!(error.to_string().contains("unsafe skill name"), "{error:#}");
            }
            assert_eq!(
                fs::read_to_string(outside.join("sentinel")).unwrap(),
                "keep"
            );
            for base in ws.skill_dirs() {
                assert!(base.join("valid-skill").is_dir());
                assert!(base.join("veecut").is_dir());
                assert!(base.parent().unwrap().join("outside").is_dir());
            }
        }

        lock.repos[0].skills = vec!["valid-skill".into()];
        lock.save(&lock_path).unwrap();
        remove(&ws, "owner/repo").unwrap();
        for base in ws.skill_dirs() {
            assert!(!base.join("valid-skill").exists());
            assert!(base.join("veecut").is_dir());
        }
        assert!(outside.join("sentinel").is_file());
        assert!(installed(&ws).unwrap().repos.is_empty());
    }

    #[test]
    fn names_and_repos_are_sanitized() {
        assert_eq!(
            frontmatter_name("---\nname: \"My Skill/../x\"\ndescription: d\n---\n# x"),
            Some("My-Skill-..-x".into())
        );
        assert_eq!(frontmatter_name("# no frontmatter\nname: x"), None);
        assert_eq!(
            normalize_repo("https://github.com/remotion-dev/skills.git").unwrap(),
            "remotion-dev/skills"
        );
        assert_eq!(normalize_repo("github.com/a/b/tree/main").unwrap(), "a/b");
        assert!(normalize_repo("../etc").is_err());
        assert!(normalize_repo("justone").is_err());
    }
}
