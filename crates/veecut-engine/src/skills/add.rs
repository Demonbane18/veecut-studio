//! Human-initiated imports. Content is inspected and copied, never executed.
use super::*;
use crate::look::{LOOK_FILE, LookPack};
use sha2::{Digest, Sha256};
use std::io::Cursor;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    BuiltIn,
    Catalog(String),
    Added(String),
    Own,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InstalledSkill {
    pub name: String,
    pub description: String,
    pub dir: PathBuf,
    pub origin: Origin,
    pub has_look: bool,
    pub notes: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CheckReport {
    pub name: String,
    pub has_look: bool,
    pub kinds: Vec<VideoKind>,
    pub fonts: Vec<String>,
    pub notes: Vec<String>,
}

pub(super) fn validate_name(name: &str) -> Result<()> {
    if built_in(name) || safe_name(name).as_deref() != Some(name) {
        bail!(
            "{name:?} is unsafe or reserved; use letters, digits, dots, underscores and hyphens without leading dots or hyphens"
        );
    }
    Ok(())
}

pub(super) fn validate_roots(ws: &Workspace) -> Result<()> {
    for root in ws.skill_dirs() {
        for path in [
            root.parent().expect("skill root has a parent"),
            root.as_path(),
        ] {
            match fs::symlink_metadata(path) {
                Ok(meta) if meta.file_type().is_symlink() || !meta.is_dir() => {
                    bail!("{} must be a directory, not a link", path.display());
                }
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
    }
    Ok(())
}

/// Adds a folder, archive, Drive file, GitHub repo or HTTPS archive. No code is run.
pub fn add(ws: &Workspace, source: &str) -> Result<InstallReport> {
    let source = source.trim();
    let path = Path::new(source);
    if !path.exists() {
        if source.starts_with("https://github.com/")
            || source.starts_with("github.com/")
            || (!source.contains("://")
                && normalize_repo(source).is_ok()
                && ![".zip", ".tgz", ".gz"]
                    .iter()
                    .any(|ext| source.ends_with(*ext)))
        {
            return install(ws, source, false);
        }
        if !source.starts_with("https://") {
            bail!("{source:?} is not an existing folder/archive, GitHub repository or HTTPS URL");
        }
    }
    validate_roots(ws)?;
    let mut lock = installed(ws)?;
    let staging = tempfile::tempdir().context("creating a skill import staging folder")?;
    let content = staging.path().join("content");
    fs::create_dir(&content)?;
    let mut skipped = Vec::new();
    let mut total = 0;
    let (source, digest) = if path.exists() {
        let meta = fs::symlink_metadata(path)?;
        if meta.file_type().is_symlink() {
            bail!("skill source must not be a link");
        }
        if !meta.is_dir() && !meta.is_file() {
            bail!("skill source must be a regular file or directory");
        }
        let path = path.canonicalize()?;
        let digest = if meta.is_dir() {
            copy_source(&path, &content, &mut total, &mut skipped)?;
            folder_digest(&content)?
        } else {
            let bytes = read_archive(fs::File::open(&path)?)?;
            let digest = format!("{:x}", Sha256::digest(&bytes));
            unpack(&bytes, &content, &mut total, &mut skipped)?;
            digest
        };
        (format!("path:{}", path.display()), digest)
    } else {
        let url = drive_download_url(source).unwrap_or_else(|| source.to_string());
        let bytes = read_archive(
            fetch(&url, "application/octet-stream")?
                .into_body()
                .into_reader(),
        )?;
        let digest = format!("{:x}", Sha256::digest(&bytes));
        unpack(&bytes, &content, &mut total, &mut skipped)?;
        (format!("url:{source}"), digest)
    };
    expand_nested(&content, &mut total, &mut skipped, 0)?;
    let files = files_in(&content)?;
    let mut skills = Vec::new();
    for rel in &files {
        if rel.file_name().is_some_and(|n| n == "SKILL.md") {
            let text = fs::read_to_string(content.join(rel))
                .with_context(|| format!("reading {}", rel.display()))?;
            skills.push((rel.parent().unwrap_or(Path::new("")).to_path_buf(), text));
        }
    }
    if skills.is_empty() {
        bail!("source has no SKILL.md; add a folder or archive containing an agent skill");
    }
    let mine: HashSet<String> = lock
        .repos
        .iter()
        .filter(|r| r.source.as_deref() == Some(source.as_str()))
        .flat_map(|r| r.skills.iter().cloned())
        .collect();
    let mut taken: HashSet<String> = BUILT_IN_SKILLS
        .iter()
        .map(|name| name.to_string())
        .collect();
    for root in ws.skill_dirs() {
        if let Ok(entries) = fs::read_dir(root) {
            for entry in entries {
                taken.insert(entry?.file_name().to_string_lossy().into_owned());
            }
        }
    }
    taken.extend(lock.repos.iter().flat_map(|r| r.skills.iter().cloned()));
    taken.retain(|name| !mine.contains(name));
    let mut names = Vec::new();
    for (dir, text) in &skills {
        let base = frontmatter_name(text)
            .or_else(|| {
                dir.file_name()
                    .and_then(|n| safe_name(&n.to_string_lossy()))
            })
            .unwrap_or_else(|| "skill".to_string());
        let mut name = base.clone();
        let mut suffix = 2;
        while taken.iter().any(|n| n.eq_ignore_ascii_case(&name)) {
            name = format!("{base}-{suffix}");
            suffix += 1;
        }
        validate_name(&name)?;
        taken.insert(name.clone());
        names.push(name);
    }
    let notes = tree_notes(&content)?;
    let license = notes
        .iter()
        .find_map(|note| note.strip_prefix("Licence files: ").map(str::to_string));
    let prepared = staging.path().join("skills");
    for ((dir, _), name) in skills.iter().zip(&names) {
        copy_dir(&content.join(dir), &prepared.join(name))?;
    }
    // All untrusted content has passed validation before touching installed folders.
    remove_skill_dirs(ws, &mine.into_iter().collect::<Vec<_>>())?;
    for root in ws.skill_dirs() {
        for name in &names {
            copy_dir(&prepared.join(name), &root.join(name))?;
        }
    }
    let repo = lock
        .repos
        .iter()
        .find(|r| r.source.as_deref() == Some(source.as_str()))
        .map(|r| r.repo.clone())
        .unwrap_or_else(|| format!("local/{}", names[0]));
    lock.repos
        .retain(|r| r.source.as_deref() != Some(source.as_str()));
    lock.repos.push(LockedRepo {
        repo: repo.clone(),
        commit: digest.clone(),
        skills: names.clone(),
        license: license.clone(),
        security: String::new(),
        installed_at: Utc::now(),
        source: Some(source),
        sha256: Some(digest.clone()),
        notes: notes.clone(),
    });
    lock.save(&ws.root.join(LOCK_FILE))?;
    Ok(InstallReport {
        repo,
        commit: digest,
        skills: names,
        license,
        skipped,
        notes,
    })
}

/// Parses both Drive sharing URL shapes without making a request.
fn drive_download_url(source: &str) -> Option<String> {
    let rest = source.strip_prefix("https://drive.google.com/")?;
    let id = if let Some(path) = rest.strip_prefix("file/d/") {
        path.split(['/', '?', '#']).next()?
    } else {
        rest.split_once('?')?
            .1
            .split('#')
            .next()?
            .split('&')
            .find_map(|part| part.strip_prefix("id="))?
    };
    if id.is_empty()
        || !id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
    {
        return None;
    }
    Some(format!(
        "https://drive.usercontent.google.com/download?id={id}&export=download&confirm=t"
    ))
}

fn read_archive(reader: impl Read) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader.take(MAX_ARCHIVE + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_ARCHIVE {
        bail!("skill archive is over 200 MB");
    }
    Ok(bytes)
}

pub(super) fn safe_rel(path: &Path) -> Result<bool> {
    let text = path.to_string_lossy();
    if path.as_os_str().is_empty()
        || text.contains('\\')
        || text.split('/').any(|part| part == "..")
        || text.as_bytes().get(1) == Some(&b':')
        || path
            .components()
            .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
    {
        bail!("unsafe archive path {}", path.display());
    }
    Ok(!path
        .components()
        .any(|c| matches!(c, Component::Normal(n) if n == ".git" || n == "node_modules")))
}

fn write_entry(
    rel: &Path,
    size: u64,
    reader: &mut dyn Read,
    dest: &Path,
    total: &mut u64,
    skipped: &mut Vec<String>,
) -> Result<()> {
    if !safe_rel(rel)? {
        return Ok(());
    }
    if size > MAX_FILE {
        skipped.push(rel.display().to_string());
        return Ok(());
    }
    *total = total
        .checked_add(size)
        .ok_or_else(|| anyhow!("skill archive size overflow"))?;
    if *total > MAX_ARCHIVE {
        bail!("expanded skill content is over 200 MB");
    }
    let target = dest.join(rel);
    fs::create_dir_all(target.parent().expect("file has a parent"))?;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&target)
        .with_context(|| {
            format!(
                "creating {} (duplicate entries are not allowed)",
                rel.display()
            )
        })?;
    let written = std::io::copy(&mut reader.take(MAX_FILE + 1), &mut file)?;
    if written != size {
        bail!("archive entry {} has an invalid size", rel.display());
    }
    Ok(())
}

fn unpack(bytes: &[u8], dest: &Path, total: &mut u64, skipped: &mut Vec<String>) -> Result<()> {
    if bytes.starts_with(b"PK") {
        let mut archive =
            zip::ZipArchive::new(Cursor::new(bytes)).context("reading zip archive")?;
        for i in 0..archive.len() {
            let mut entry = archive.by_index(i).context("reading zip entry")?;
            let rel = PathBuf::from(entry.name());
            let include = safe_rel(&rel)?;
            if entry.unix_mode().is_some_and(|mode| {
                let kind = mode & 0o170000;
                kind != 0 && kind != 0o100000 && kind != 0o040000
            }) {
                bail!("links and special files are not allowed: {}", rel.display());
            }
            if entry.is_dir() || !include {
                continue;
            }
            write_entry(&rel, entry.size(), &mut entry, dest, total, skipped)?;
        }
    } else if bytes.starts_with(&[0x1f, 0x8b]) {
        let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(bytes));
        for entry in archive.entries().context("reading tar.gz archive")? {
            let mut entry = entry?;
            let rel = entry.path()?.into_owned();
            let include = safe_rel(&rel)?;
            let kind = entry.header().entry_type();
            if kind.is_dir() || !include {
                continue;
            }
            if !kind.is_file() {
                bail!("links and special files are not allowed: {}", rel.display());
            }
            write_entry(&rel, entry.size(), &mut entry, dest, total, skipped)?;
        }
    } else {
        bail!("source is not a zip or tar.gz archive (Drive may require public download access)");
    }
    Ok(())
}

fn files_in(root: &Path) -> Result<Vec<PathBuf>> {
    fn visit(root: &Path, rel: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
        for entry in fs::read_dir(root.join(rel))? {
            let entry = entry?;
            let path = rel.join(entry.file_name());
            if !safe_rel(&path)? {
                continue;
            }
            let kind = entry.file_type()?;
            if kind.is_symlink() {
                bail!("links are not allowed: {}", path.display());
            }
            if kind.is_dir() {
                visit(root, &path, files)?;
            } else if kind.is_file() {
                files.push(path);
            } else {
                bail!("special files are not allowed: {}", path.display());
            }
        }
        Ok(())
    }
    let mut files = Vec::new();
    visit(root, Path::new(""), &mut files)?;
    files.sort();
    Ok(files)
}

fn copy_source(
    source: &Path,
    dest: &Path,
    total: &mut u64,
    skipped: &mut Vec<String>,
) -> Result<()> {
    for rel in files_in(source)? {
        let mut file = fs::File::open(source.join(&rel))?;
        let size = file.metadata()?.len();
        write_entry(&rel, size, &mut file, dest, total, skipped)?;
    }
    Ok(())
}

fn folder_digest(root: &Path) -> Result<String> {
    let mut hash = Sha256::new();
    for rel in files_in(root)? {
        let name = rel.to_string_lossy();
        hash.update((name.len() as u64).to_le_bytes());
        hash.update(name.as_bytes());
        let mut file = fs::File::open(root.join(rel))?;
        hash.update(file.metadata()?.len().to_le_bytes());
        let mut buf = [0; 64 * 1024];
        loop {
            let n = file.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hash.update(&buf[..n]);
        }
    }
    Ok(format!("{:x}", hash.finalize()))
}

fn expand_nested(
    root: &Path,
    total: &mut u64,
    skipped: &mut Vec<String>,
    depth: usize,
) -> Result<()> {
    for rel in files_in(root)? {
        if !rel
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("zip"))
        {
            continue;
        }
        // ponytail: four nested wrappers maximum; raise only for real deeper skill bundles.
        if depth == 4 {
            bail!("skill archives are nested more than four levels");
        }
        let bytes = read_archive(fs::File::open(root.join(&rel))?)?;
        let dest = root.join(&rel).with_extension("zip-unpacked");
        fs::create_dir(&dest).context("creating nested archive folder")?;
        unpack(&bytes, &dest, total, skipped)?;
        expand_nested(&dest, total, skipped, depth + 1)?;
    }
    Ok(())
}

pub(super) fn tree_notes(root: &Path) -> Result<Vec<String>> {
    Ok(notes_for_files(files_in(root)?))
}

pub(super) fn notes_for_files(files: impl IntoIterator<Item = PathBuf>) -> Vec<String> {
    let mut scripts = Vec::new();
    let mut licenses = Vec::new();
    for rel in files {
        let name = rel
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_lowercase();
        if name == "package.json"
            || rel.extension().is_some_and(|ext| {
                matches!(
                    ext.to_string_lossy().to_lowercase().as_str(),
                    "sh" | "py" | "js" | "mjs" | "ps1"
                )
            })
        {
            scripts.push(rel.display().to_string());
        }
        if name == "license"
            || name == "licence"
            || name == "copying"
            || name == "ofl.txt"
            || name.starts_with("license.")
            || name.starts_with("licence.")
            || name.starts_with("copying.")
            || name.starts_with("license-")
            || name.starts_with("licence-")
        {
            licenses.push(rel.display().to_string());
        }
    }
    vec![
        if scripts.is_empty() {
            "No scripts found. VeeCut never runs these".to_string()
        } else {
            format!(
                "Scripts found: {}. VeeCut never runs these",
                scripts.join(", ")
            )
        },
        if licenses.is_empty() {
            "No licence file: personal use only; do not redistribute".to_string()
        } else {
            format!("Licence files: {}", licenses.join(", "))
        },
    ]
}

pub(super) fn license_note(notes: &mut Vec<String>, license: Option<&str>) {
    if let Some(license) =
        license.filter(|license| !license.is_empty() && *license != "NOASSERTION")
    {
        notes.retain(|note| !note.starts_with("No licence file:"));
        let note = format!("Licence: {license}");
        if !notes.contains(&note) {
            notes.push(note);
        }
    }
}

pub fn installed_skills(ws: &Workspace) -> Result<Vec<InstalledSkill>> {
    validate_roots(ws)?;
    let lock = installed(ws)?;
    let root = ws.skill_dirs()[0].clone();
    let entries = match fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut result = Vec::new();
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_dir() || !entry.path().join("SKILL.md").is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if safe_name(&name).as_deref() != Some(name.as_str()) {
            continue;
        }
        let text = fs::read_to_string(entry.path().join("SKILL.md"))?;
        let locked = lock.repos.iter().find(|r| r.skills.contains(&name));
        let origin = if built_in(&name) {
            Origin::BuiltIn
        } else if let Some(repo) = locked {
            match &repo.source {
                Some(source) => Origin::Added(source.clone()),
                None => Origin::Catalog(repo.repo.clone()),
            }
        } else {
            Origin::Own
        };
        let mut notes = tree_notes(&entry.path())?;
        if origin == Origin::BuiltIn {
            license_note(&mut notes, Some("MIT (VeeCut)"));
        } else if let Some(repo) = locked {
            license_note(&mut notes, repo.license.as_deref());
            for note in &repo.notes {
                // Fresh scans use paths relative to this skill, not the imported archive root.
                if !note.starts_with("Scripts found:")
                    && !note.starts_with("No scripts found.")
                    && !note.starts_with("Licence")
                    && !note.starts_with("No licence file:")
                    && !notes.contains(note)
                {
                    notes.push(note.clone());
                }
            }
        }
        result.push(InstalledSkill {
            name,
            description: description(&text),
            dir: entry.path(),
            origin,
            has_look: entry.path().join(LOOK_FILE).is_file(),
            notes,
        });
    }
    result.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(result)
}

fn description(text: &str) -> String {
    frontmatter_value(text, "description").unwrap_or_default()
}

pub fn new_skill(
    ws: &Workspace,
    name: &str,
    description: &str,
    look: Option<LookPack>,
) -> Result<PathBuf> {
    validate_name(name)?;
    validate_roots(ws)?;
    let lock = installed(ws)?;
    if lock
        .repos
        .iter()
        .any(|r| r.skills.iter().any(|n| n.eq_ignore_ascii_case(name)))
        || ws
            .skill_dirs()
            .iter()
            .any(|root| root.join(name).symlink_metadata().is_ok())
    {
        bail!("skill {name:?} already exists");
    }
    let staging = tempfile::tempdir()?;
    fs::write(
        staging.path().join("SKILL.md"),
        format!(
            "---\nname: {name}\ndescription: {}\n---\n\n# {name}\n\nDescribe when to use this skill, then write the editing steps an agent should follow.\n\nUse veecut.toml for the native look. Keep scripts as reference only: VeeCut never runs them.\n",
            serde_json::to_string(description)?
        ),
    )?;
    let toml = match look { Some(look) => toml::to_string_pretty(&look)?, None =>
        "# VeeCut native look. Set optional fields to match your style.\nschema = 1\n# kinds = [\"talking_head_short\"]\n# aspect = \"9:16\"\n\n# [captions]\n# font = \"Inter\"\n".to_string() };
    fs::write(staging.path().join(LOOK_FILE), toml)?;
    // Validate supplied looks (including their file references) before installing anything.
    crate::look::load(staging.path())?;
    for root in ws.skill_dirs() {
        copy_dir(staging.path(), &root.join(name))?;
    }
    Ok(ws.skill_dirs()[0].join(name))
}

pub fn check(ws: &Workspace, name: &str) -> Result<CheckReport> {
    let skill = installed_skills(ws)?
        .into_iter()
        .find(|skill| skill.name == name)
        .ok_or_else(|| anyhow!("skill {name:?} is not installed"))?;
    let notes = skill.notes;
    let mut report = CheckReport {
        name: skill.name,
        has_look: skill.has_look,
        kinds: Vec::new(),
        fonts: Vec::new(),
        notes,
    };
    if skill.has_look {
        let pack = crate::look::load(&skill.dir)?;
        let mut db = cosmic_text::fontdb::Database::new();
        for font in &pack.fonts {
            let before = db.faces().count();
            db.load_font_data(fs::read(skill.dir.join(font))?);
            if db.faces().count() == before {
                bail!("font {font:?} could not be loaded");
            }
        }
        report.kinds = pack.kinds;
        report.fonts = pack.fonts;
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn workspace(root: &Path) -> Workspace {
        let ws = Workspace {
            root: root.join("workspace"),
        };
        fs::create_dir_all(&ws.root).unwrap();
        ws
    }

    fn skill(root: &Path, name: &str) {
        fs::create_dir_all(root).unwrap();
        fs::write(
            root.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {name} description\n---\nInstructions\n"),
        )
        .unwrap();
    }

    #[test]
    fn folder_import_copies_all_skills_and_records_source_safety_and_updates() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = workspace(tmp.path());
        let source = tmp.path().join("bundle");
        skill(&source.join("one"), "one");
        skill(&source.join("two"), "two");
        fs::write(source.join("setup.sh"), "do not run this").unwrap();
        let report = add(&ws, source.to_str().unwrap()).unwrap();
        assert_eq!(report.skills, ["one", "two"]);
        assert!(
            report
                .notes
                .iter()
                .any(|note| note.contains("setup.sh") && note.contains("VeeCut never runs these"))
        );
        assert!(
            report
                .notes
                .iter()
                .any(|note| note == "No licence file: personal use only; do not redistribute")
        );
        for root in ws.skill_dirs() {
            for name in &report.skills {
                assert!(root.join(name).join("SKILL.md").is_file());
            }
        }
        let lock = installed(&ws).unwrap();
        assert_eq!(
            lock.repos[0].source.as_deref(),
            Some(format!("path:{}", source.canonicalize().unwrap().display()).as_str())
        );
        assert_eq!(lock.repos[0].sha256.as_ref().unwrap().len(), 64);
        fs::write(source.join("one/extra.txt"), "new").unwrap();
        let updates = update(&ws, Some("one")).unwrap();
        assert!(updates[0].updated);
        assert_eq!(installed(&ws).unwrap().repos.len(), 1);
        for root in ws.skill_dirs() {
            assert!(root.join("one/extra.txt").is_file());
        }
        remove(&ws, "one").unwrap();
        for root in ws.skill_dirs() {
            assert!(!root.join("one").exists());
            assert!(root.join("two/SKILL.md").is_file());
        }
    }

    fn zip_file(path: &Path, entries: &[(&str, &str)]) {
        let mut zip = zip::ZipWriter::new(fs::File::create(path).unwrap());
        for (name, text) in entries {
            zip.start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(text.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }

    #[test]
    fn wrapped_zip_and_nested_zip_install_but_unsafe_paths_leave_workspace_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = workspace(tmp.path());
        let archive = tmp.path().join("bundle.zip");
        zip_file(
            &archive,
            &[("wrapper/editor/SKILL.md", "---\nname: editor\n---\n")],
        );
        add(&ws, archive.to_str().unwrap()).unwrap();
        for root in ws.skill_dirs() {
            assert!(root.join("editor/SKILL.md").is_file());
        }
        let outer = tmp.path().join("outer.zip");
        let mut zip = zip::ZipWriter::new(fs::File::create(&outer).unwrap());
        zip.start_file(
            "wrapper/inner.zip",
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
        zip.write_all(&fs::read(&archive).unwrap()).unwrap();
        zip.finish().unwrap();
        assert_eq!(
            add(&ws, outer.to_str().unwrap()).unwrap().skills,
            ["editor-2"]
        );
        let before = fs::read(ws.root.join(LOCK_FILE)).unwrap();
        let outside = tmp.path().join("escape.txt");
        for bad in [
            "../escape.txt".to_string(),
            outside.display().to_string(),
            "C:/escape.txt".to_string(),
            "..\\escape.txt".to_string(),
        ] {
            zip_file(
                &archive,
                &[("new/SKILL.md", "---\nname: new\n---\n"), (&bad, "bad")],
            );
            assert!(
                add(&ws, archive.to_str().unwrap())
                    .unwrap_err()
                    .to_string()
                    .contains("unsafe archive path")
            );
            assert!(!outside.exists());
            assert_eq!(fs::read(ws.root.join(LOCK_FILE)).unwrap(), before);
            for root in ws.skill_dirs() {
                assert!(!root.join("new").exists());
            }
        }
    }

    #[test]
    fn drive_links_parse_without_network() {
        let expected =
            "https://drive.usercontent.google.com/download?id=ab_CD-12&export=download&confirm=t";
        assert_eq!(
            drive_download_url("https://drive.google.com/file/d/ab_CD-12/view?usp=sharing")
                .as_deref(),
            Some(expected)
        );
        assert_eq!(
            drive_download_url("https://drive.google.com/open?usp=sharing&id=ab_CD-12").as_deref(),
            Some(expected)
        );
        assert_eq!(
            drive_download_url("https://drive.google.com/?id=ab_CD-12").as_deref(),
            Some(expected)
        );
        for invalid in [
            "https://evil.example/?id=ab_CD-12",
            "https://drive.google.com/?id=../x",
            "https://drive.google.com/file/d//view",
        ] {
            assert!(drive_download_url(invalid).is_none());
        }
    }

    #[test]
    fn scaffold_list_and_check_distinguish_origins_and_protect_existing_skills() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = workspace(tmp.path());
        for root in ws.skill_dirs() {
            skill(&root.join("veecut"), "veecut");
        }
        let source = tmp.path().join("added");
        skill(&source, "added");
        add(&ws, source.to_str().unwrap()).unwrap();
        let dir = new_skill(&ws, "my-look", "My own look", None).unwrap();
        for root in ws.skill_dirs() {
            assert!(root.join("my-look/SKILL.md").is_file());
            assert!(root.join("my-look/veecut.toml").is_file());
        }
        assert!(new_skill(&ws, "my-look", "replace", None).is_err());
        for unsafe_name in [
            "../escape",
            "/absolute",
            "veecut",
            "VeeCut",
            "nested/name",
            "bad name",
            "",
        ] {
            assert!(new_skill(&ws, unsafe_name, "", None).is_err());
        }
        let list = installed_skills(&ws).unwrap();
        assert_eq!(
            list.iter()
                .map(|s| (s.name.as_str(), &s.origin))
                .collect::<Vec<_>>(),
            vec![
                (
                    "added",
                    &Origin::Added(format!("path:{}", source.canonicalize().unwrap().display()))
                ),
                ("my-look", &Origin::Own),
                ("veecut", &Origin::BuiltIn),
            ]
        );
        assert_eq!(list[1].description, "My own look");
        assert!(check(&ws, "my-look").unwrap().has_look);
        fs::write(dir.join("setup.py"), "never executed").unwrap();
        assert!(
            check(&ws, "my-look")
                .unwrap()
                .notes
                .iter()
                .any(|note| note.contains("setup.py"))
        );
        fs::write(dir.join(LOOK_FILE), "schema = 99").unwrap();
        assert!(check(&ws, "my-look").is_err());
        assert!(remove(&ws, "veecut").is_err());
        remove(&ws, "my-look").unwrap();
        for root in ws.skill_dirs() {
            assert!(!root.join("my-look").exists());
        }
    }

    #[test]
    fn old_lock_without_source_hash_or_notes_loads() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(LOCK_FILE);
        fs::write(&path, r#"{"repos":[{"repo":"owner/repo","commit":"abc","skills":["example"],"license":null,"security":"SAFE","installed_at":"2026-11-05T00:00:00Z"}]}"#).unwrap();
        let lock = Lock::load(&path).unwrap();
        assert_eq!(lock.repos[0].source, None);
        assert_eq!(lock.repos[0].sha256, None);
        assert!(lock.repos[0].notes.is_empty());
    }

    #[test]
    fn folder_links_and_archive_links_are_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = workspace(tmp.path());
        let archive = tmp.path().join("link.tar.gz");
        let encoder = flate2::write::GzEncoder::new(
            fs::File::create(&archive).unwrap(),
            flate2::Compression::default(),
        );
        let mut tar = tar::Builder::new(encoder);
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_size(0);
        header.set_mode(0o777);
        header.set_link_name("/outside").unwrap();
        header.set_cksum();
        tar.append_data(&mut header, "wrapper/link", std::io::empty())
            .unwrap();
        tar.into_inner().unwrap().finish().unwrap();
        assert!(add(&ws, archive.to_str().unwrap()).is_err());
        #[cfg(unix)]
        {
            let source = tmp.path().join("source");
            skill(&source, "link-skill");
            std::os::unix::fs::symlink(tmp.path(), source.join("link")).unwrap();
            assert!(add(&ws, source.to_str().unwrap()).is_err());
        }
        assert!(!ws.root.join(LOCK_FILE).exists());
    }

    #[test]
    fn tar_gz_import_keeps_licence_and_checks_fonts() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = workspace(tmp.path());
        let archive = tmp.path().join("bundle.tgz");
        let encoder = flate2::write::GzEncoder::new(
            fs::File::create(&archive).unwrap(),
            flate2::Compression::default(),
        );
        let mut tar = tar::Builder::new(encoder);
        for (name, text) in [
            ("wrapper/font-look/SKILL.md", "---\nname: font-look\n---\n"),
            ("wrapper/font-look/LICENSE", "MIT"),
            (
                "wrapper/font-look/veecut.toml",
                "schema = 1\nfonts = [\"fonts/invalid.ttf\"]\n",
            ),
            ("wrapper/font-look/fonts/invalid.ttf", "not a font"),
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_size(text.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tar.append_data(&mut header, name, text.as_bytes()).unwrap();
        }
        tar.into_inner().unwrap().finish().unwrap();
        let report = add(&ws, archive.to_str().unwrap()).unwrap();
        assert!(
            report
                .notes
                .iter()
                .any(|note| note.contains("Licence files:") && note.contains("LICENSE"))
        );
        assert!(
            check(&ws, "font-look")
                .unwrap_err()
                .to_string()
                .contains("could not be loaded")
        );
        for root in ws.skill_dirs() {
            assert!(root.join("font-look/LICENSE").is_file());
        }
    }

    #[test]
    fn init_embeds_refreshes_and_protects_both_native_looks() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = Workspace::init(tmp.path()).unwrap();
        for root in ws.skill_dirs() {
            assert!(root.join("veecut/reference/skills.md").is_file());
            for name in ["veecut-arcade", "veecut-editorial"] {
                let pack = crate::look::load(&root.join(name)).unwrap();
                assert_eq!(pack.schema, 1);
                assert!(!pack.kinds.is_empty());
                assert!(root.join(name).join("SKILL.md").is_file());
            }
        }
        let installed = installed_skills(&ws).unwrap();
        for name in BUILT_IN_SKILLS {
            let skill = installed.iter().find(|skill| skill.name == *name).unwrap();
            assert_eq!(skill.origin, Origin::BuiltIn);
            assert!(
                skill
                    .notes
                    .iter()
                    .any(|note| note == "Licence: MIT (VeeCut)")
            );
            assert!(
                !skill
                    .notes
                    .iter()
                    .any(|note| note.starts_with("No licence"))
            );
            assert!(
                !check(&ws, name)
                    .unwrap()
                    .notes
                    .iter()
                    .any(|note| note.starts_with("No licence"))
            );
            assert!(remove(&ws, name).is_err());
            assert!(new_skill(&ws, name, "", None).is_err());
        }
        for root in ws.skill_dirs() {
            fs::write(root.join("veecut-arcade/veecut.toml"), "stale").unwrap();
            fs::write(root.join("veecut-editorial/old.txt"), "stale").unwrap();
        }
        Workspace::init(tmp.path()).unwrap();
        for root in ws.skill_dirs() {
            assert!(crate::look::load(&root.join("veecut-arcade")).is_ok());
            assert!(!root.join("veecut-editorial/old.txt").exists());
        }
        // A forged lock cannot turn a built-in into a removable imported folder.
        fs::write(ws.root.join(LOCK_FILE), r#"{"repos":[{"repo":"owner/repo","commit":"abc","skills":["veecut-arcade"],"license":"MIT","security":"SAFE","installed_at":"2026-11-05T00:00:00Z"}]}"#).unwrap();
        assert!(remove(&ws, "owner/repo").is_err());
        assert!(ws.skill_dirs()[0].join("veecut-arcade/SKILL.md").is_file());
    }

    #[test]
    fn imported_folded_and_literal_descriptions_become_joined_summaries() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = workspace(tmp.path());
        let source = tmp.path().join("bundle");
        for (index, scalar) in [">", ">-", ">+", "|", "|-", "|+"].iter().enumerate() {
            let dir = source.join(format!("skill-{index}"));
            skill(&dir, &format!("skill-{index}"));
            fs::write(dir.join("SKILL.md"), format!(
                "---\nname: skill-{index}\ndescription: {scalar}\n  First line\n  second line.\n\nlicense: MIT\n---\nBody\n"
            )).unwrap();
        }
        add(&ws, source.to_str().unwrap()).unwrap();
        for skill in installed_skills(&ws).unwrap() {
            assert_eq!(skill.description, "First line second line.");
        }
        assert_eq!(
            description("---\ndescription: \"Quoted text\"\n---\n"),
            "Quoted text"
        );
        assert_eq!(
            description("---\ndescription: 'Single quoted'\n---\n"),
            "Single quoted"
        );
    }

    #[test]
    fn check_uses_one_fresh_relative_scan_and_catalog_spdx() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = workspace(tmp.path());
        let source = tmp.path().join("bundle");
        let original = source.join("wrapper/editor");
        skill(&original, "editor");
        fs::create_dir_all(original.join("tools")).unwrap();
        fs::write(original.join("tools/setup.py"), "never run").unwrap();
        add(&ws, source.to_str().unwrap()).unwrap();
        let added = check(&ws, "editor").unwrap();
        assert_eq!(
            added
                .notes
                .iter()
                .filter(|note| note.starts_with("Scripts found:"))
                .count(),
            1
        );
        assert!(
            added
                .notes
                .iter()
                .any(|note| note == "Scripts found: tools/setup.py. VeeCut never runs these")
        );
        assert!(
            !added
                .notes
                .iter()
                .any(|note| note.contains("wrapper/editor"))
        );
        let mut lock = installed(&ws).unwrap();
        lock.repos[0].source = None;
        lock.repos[0].license = Some("Apache-2.0".into());
        lock.save(&ws.root.join(LOCK_FILE)).unwrap();
        let report = check(&ws, "editor").unwrap();
        assert_eq!(
            report
                .notes
                .iter()
                .filter(|note| note.starts_with("Scripts found:"))
                .count(),
            1
        );
        assert_eq!(
            report
                .notes
                .iter()
                .filter(|note| note.starts_with("Licence:"))
                .count(),
            1
        );
        assert!(
            report
                .notes
                .iter()
                .any(|note| note == "Licence: Apache-2.0")
        );
        assert!(
            !report
                .notes
                .iter()
                .any(|note| note.starts_with("No licence"))
        );
        fs::write(ws.skill_dirs()[0].join("editor/LICENSE"), "Apache-2.0").unwrap();
        assert!(
            check(&ws, "editor")
                .unwrap()
                .notes
                .iter()
                .any(|note| note == "Licence files: LICENSE")
        );
    }
}
