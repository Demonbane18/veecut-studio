use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{BufRead, BufReader, Read, Write},
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use veecut_engine::{
    project::{PROJECT_FILE, Project, VideoKind},
    transcript::Transcript,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FixtureFile {
    pub path: PathBuf,
    pub role: String,
    #[serde(default)]
    pub duration_s: Option<f64>,
    #[serde(default)]
    pub sha256: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Fixture {
    pub id: String,
    pub kind: String,
    pub length: String,
    pub files: Vec<FixtureFile>,
    #[serde(default)]
    pub language: Option<String>,
    pub notes: Value,
}

impl Fixture {
    pub fn video_kind(&self) -> VideoKind {
        VideoKind::from_slug(&self.kind).expect("validated manifest kind")
    }
}

pub fn manifest(path: &Path) -> Result<Vec<Fixture>> {
    let fixtures: Vec<Fixture> = read_json(path)?;
    let mut ids = BTreeSet::new();
    for f in &fixtures {
        ensure!(
            !f.id.is_empty()
                && f.id
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "Unsafe fixture id {}",
            f.id
        );
        ensure!(ids.insert(&f.id), "Duplicate fixture id {}", f.id);
        ensure!(
            VideoKind::from_slug(&f.kind).is_some(),
            "Unknown kind {}",
            f.kind
        );
        ensure!(
            ["S", "M", "L", "XL"].contains(&f.length.as_str()),
            "Unknown length {}",
            f.length
        );
        ensure!(f.notes.is_object(), "Fixture {} notes must be object", f.id);
        let mut names = BTreeSet::new();
        for file in &f.files {
            ensure!(
                !file.path.is_absolute()
                    && file
                        .path
                        .components()
                        .all(|c| matches!(c, Component::Normal(_))),
                "Unsafe fixture path {}",
                file.path.display()
            );
            ensure!(
                names.insert(file.path.file_name()),
                "Colliding inbox filenames in {}",
                f.id
            );
        }
    }
    Ok(fixtures)
}

pub fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    serde_json::from_slice(&fs::read(path).with_context(|| format!("reading {}", path.display()))?)
        .with_context(|| format!("parsing {}", path.display()))
}

pub fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    fs::create_dir_all(path.parent().context("output has no parent")?)?;
    let temp = path.with_extension("json.tmp");
    fs::write(&temp, serde_json::to_vec_pretty(value)?)?;
    fs::rename(temp, path)?;
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Baseline {
    pub fixture: String,
    pub status: String,
    pub projects: Vec<PathBuf>,
    pub seconds: f64,
    pub report: Value,
    pub error: Option<String>,
    pub failure_class: Option<String>,
    pub beats: Vec<f64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Row {
    pub schema_version: u8,
    pub timestamp: String,
    pub fixture: String,
    pub kind: String,
    pub length: String,
    pub task: String,
    pub level: u8,
    pub agent: String,
    pub model: String,
    pub effort: Option<String>,
    pub trial: u32,
    pub pass: bool,
    pub check_detail: String,
    pub evidence: Value,
    pub seconds: f64,
    pub tokens_in: Option<u64>,
    pub tokens_out: Option<u64>,
    pub tokens_cached: Option<u64>,
    pub cost_usd: Option<f64>,
    pub turns: Option<u64>,
    pub claimed_success: bool,
    pub claim_evidence: String,
    pub failure_class: Option<String>,
    pub timed_out: bool,
    pub exit_code: Option<i32>,
    pub workspace: PathBuf,
    pub project_paths: Vec<PathBuf>,
    pub prompt_path: PathBuf,
    pub answer_path: PathBuf,
    pub log_path: PathBuf,
    pub task_path: PathBuf,
    pub provenance: String,
    pub screening: bool,
    pub generation: u32,
}

impl Row {
    pub fn cell(&self) -> String {
        format!(
            "{}|{}|{}|{}|{}",
            self.fixture,
            self.task,
            self.agent,
            self.model,
            self.effort.as_deref().unwrap_or("default")
        )
    }
    pub fn total_tokens(&self) -> Option<u64> {
        Some(self.tokens_in? + self.tokens_out? + self.tokens_cached?)
    }
}

pub fn rows(dir: &Path) -> Result<Vec<Row>> {
    if !dir.exists() {
        return Ok(vec![]);
    }
    let mut paths = fs::read_dir(dir)?
        .map(|e| e.map(|e| e.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    paths.sort();
    let mut out = vec![];
    for path in paths
        .into_iter()
        .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
    {
        let mut reader = BufReader::new(fs::File::open(&path)?);
        let mut line = String::new();
        let mut n = 0;
        loop {
            line.clear();
            if reader.read_line(&mut line)? == 0 {
                break;
            }
            n += 1;
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str(&line) {
                Ok(row) => out.push(row),
                Err(e) => bail!(
                    "{}:{n}: invalid result row ({e}); preserve and repair the interrupted record before continuing",
                    path.display()
                ),
            }
        }
    }
    Ok(out)
}

pub fn append_row(dir: &Path, row: &Row) -> Result<()> {
    fs::create_dir_all(dir)?;
    let path = dir.join(format!("{}.jsonl", chrono::Utc::now().format("%Y-%m-%d")));
    let mut file = fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)?;
    let mut bytes = serde_json::to_vec(row)?;
    bytes.push(b'\n');
    file.write_all(&bytes)?;
    file.sync_data()?;
    Ok(())
}

pub fn projects(root: &Path) -> Result<(Vec<PathBuf>, Vec<Project>, Vec<Transcript>)> {
    let dir = root.join("projects");
    if !dir.exists() {
        return Ok((vec![], vec![], vec![]));
    }
    let mut dirs = fs::read_dir(dir)?
        .map(|e| e.map(|e| e.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    dirs.sort();
    let mut paths = vec![];
    let mut projects = vec![];
    let mut transcripts = vec![];
    for dir in dirs {
        let path = dir.join(PROJECT_FILE);
        if !path.is_file() {
            continue;
        }
        // Validate raw agent output before any normalization can conceal edits or index empty tracks.
        let project: Project = read_json(&path)?;
        project.validate()?;
        for entry in fs::read_dir(&dir)? {
            let path = entry?.path();
            if path
                .file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("transcript"))
                && path.extension().is_some_and(|e| e == "json")
            {
                transcripts.push(Transcript::load(&path)?);
            }
        }
        paths.push(path);
        projects.push(project);
    }
    Ok((paths, projects, transcripts))
}

pub fn copy_tree(source: &Path, dest: &Path) -> Result<()> {
    ensure!(
        !dest.exists(),
        "Run destination already exists: {}",
        dest.display()
    );
    fs::create_dir_all(dest)?;
    copy_contents(source, dest)
}

fn copy_contents(source: &Path, dest: &Path) -> Result<()> {
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let from = entry.path();
        let to = dest.join(entry.file_name());
        let metadata = fs::metadata(&from)?;
        if metadata.is_dir() {
            ensure!(
                !entry.file_type()?.is_symlink(),
                "Refusing directory symlink {}",
                from.display()
            );
            fs::create_dir(&to)?;
            copy_contents(&from, &to)?;
        } else if metadata.is_file() {
            // fs::copy dereferences baseline inbox links and creates independent writable files.
            fs::copy(&from, &to).with_context(|| format!("copying {}", from.display()))?;
        } else {
            bail!("Unsupported file {}", from.display());
        }
    }
    Ok(())
}

pub fn rebase(root: &Path, old: &Path, mapping: &[(PathBuf, PathBuf)]) -> Result<()> {
    fn strings(value: &mut Value, old: &str, new: &str, mapping: &[(PathBuf, PathBuf)]) {
        match value {
            Value::String(s) => {
                if let Some((_, target)) = mapping
                    .iter()
                    .find(|(source, _)| s == source.to_string_lossy().as_ref())
                {
                    *s = target.to_string_lossy().into_owned();
                } else if s.starts_with(old)
                    && s.as_bytes().get(old.len()).is_none_or(|b| *b == b'/')
                {
                    *s = format!("{new}{}", &s[old.len()..]);
                }
            }
            Value::Array(v) => {
                for v in v {
                    strings(v, old, new, mapping)
                }
            }
            Value::Object(v) => {
                for v in v.values_mut() {
                    strings(v, old, new, mapping)
                }
            }
            _ => {}
        }
    }
    fn visit(dir: &Path, old: &str, new: &str, mapping: &[(PathBuf, PathBuf)]) -> Result<()> {
        for entry in fs::read_dir(dir)? {
            let path = entry?.path();
            if path.is_dir() {
                visit(&path, old, new, mapping)?;
            } else if path.extension().is_some_and(|e| e == "json") {
                let Ok(mut value) = read_json::<Value>(&path) else {
                    continue;
                };
                strings(&mut value, old, new, mapping);
                write_json(&path, &value)?;
            }
        }
        Ok(())
    }
    visit(
        root,
        &old.to_string_lossy(),
        &root.to_string_lossy(),
        mapping,
    )?;
    let (paths, ps, _) = projects(root)?;
    let canonical = root.canonicalize()?;
    for (path, p) in paths.iter().zip(ps) {
        for asset in &p.assets {
            let asset_path = p
                .asset_path(path.parent().unwrap(), asset)
                .canonicalize()
                .with_context(|| format!("resolving copied asset {}", asset.path.display()))?;
            ensure!(
                asset_path.starts_with(&canonical),
                "Copied project references outside run: {}",
                asset_path.display()
            );
        }
    }
    Ok(())
}

/// Streaming, non-cryptographic fingerprint detects accidental media edits without loading video into RAM.
pub fn inbox_snapshot(root: &Path) -> Result<BTreeMap<PathBuf, (u64, u64)>> {
    fn visit(base: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, (u64, u64)>) -> Result<()> {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            ensure!(
                !entry.file_type()?.is_symlink(),
                "Run inbox contains symlink: {}",
                path.display()
            );
            if path.is_dir() {
                visit(base, &path, out)?;
            } else {
                let mut file = fs::File::open(&path)?;
                let mut buffer = [0u8; 65536];
                let mut hash = 0xcbf29ce484222325u64;
                let mut length = 0;
                loop {
                    let n = file.read(&mut buffer)?;
                    if n == 0 {
                        break;
                    }
                    length += n as u64;
                    for b in &buffer[..n] {
                        hash = (hash ^ u64::from(*b)).wrapping_mul(0x100000001b3);
                    }
                }
                out.insert(path.strip_prefix(base)?.to_path_buf(), (length, hash));
            }
        }
        Ok(())
    }
    let mut out = BTreeMap::new();
    visit(root, root, &mut out)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn copied_inbox_cannot_mutate_fixture_and_paths_rebase() {
        let temp = tempfile::tempdir().unwrap();
        let fixture = temp.path().join("fixture");
        fs::write(&fixture, b"original").unwrap();
        let baseline = temp.path().join("baseline");
        fs::create_dir_all(baseline.join("inbox")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&fixture, baseline.join("inbox/media")).unwrap();
        #[cfg(not(unix))]
        fs::copy(&fixture, baseline.join("inbox/media")).unwrap();
        let run = temp.path().join("run");
        copy_tree(&baseline, &run).unwrap();
        let snapshot = inbox_snapshot(&run.join("inbox")).unwrap();
        fs::write(run.join("inbox/media"), b"changed").unwrap();
        assert_eq!(fs::read(&fixture).unwrap(), b"original");
        assert_ne!(snapshot, inbox_snapshot(&run.join("inbox")).unwrap());
        write_json(
            &run.join("transcript.json"),
            &serde_json::json!({"path":fixture,"root":baseline}),
        )
        .unwrap();
        rebase(&run, &baseline, &[(fixture, run.join("inbox/media"))]).unwrap();
        let v: Value = read_json(&run.join("transcript.json")).unwrap();
        assert_eq!(v["path"], run.join("inbox/media").to_str().unwrap());
    }
}

#[cfg(test)]
pub fn synthetic_row() -> Row {
    Row {
        schema_version: 1,
        timestamp: "2026-01-01T00:00:00Z".into(),
        fixture: "synthetic".into(),
        kind: "talking-head-short".into(),
        length: "S".into(),
        task: "caption-size".into(),
        level: 2,
        agent: "opencode".into(),
        model: crate::ladder::FREE_MODELS[0].into(),
        effort: None,
        trial: 1,
        pass: true,
        check_detail: "synthetic check".into(),
        evidence: serde_json::json!({}),
        seconds: 2.0,
        tokens_in: Some(5),
        tokens_out: Some(3),
        tokens_cached: Some(2),
        cost_usd: Some(0.0),
        turns: Some(1),
        claimed_success: true,
        claim_evidence: "Done".into(),
        failure_class: None,
        timed_out: false,
        exit_code: Some(0),
        workspace: "synthetic/run".into(),
        project_paths: vec![],
        prompt_path: "synthetic/prompt".into(),
        answer_path: "synthetic/answer".into(),
        log_path: "synthetic/log".into(),
        task_path: "synthetic/task".into(),
        provenance: "synthetic unit test".into(),
        screening: false,
        generation: 0,
    }
}
