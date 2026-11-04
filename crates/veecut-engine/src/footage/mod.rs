//! Free, licence-tracked footage and pictures: search, contact sheets, download and provenance.
//!
//! Providers: NASA Image and Video Library, Wikimedia Commons, Internet Archive, Openverse (all
//! keyless) and Pexels (only with `PEXELS_API_KEY`). Every candidate carries its licence and a
//! flag when a person must check the rights; a final render refuses flagged files nobody
//! approved. The source list and the traps (NASA links that redirect, check pages that look
//! like files) are ideas from NextPhase Editor V1's published notes; the code is VeeCut's own.

mod archive;
mod commons;
mod nasa;
mod openverse;
mod pexels;
mod rights;
mod sheet;

use std::collections::BTreeSet;
use std::fs;
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail, ensure};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::media;
use crate::ops::{self, Op};
use crate::project::{AssetKind, AssetRole, Project, Source};
use crate::workspace::Workspace;

pub use sheet::contact_sheet;

/// Largest video file `fetch` downloads. Documentary shots use a few seconds of each file, so
/// bigger renditions only slow a first `--find` down; a skipped file falls through to the next.
pub const VIDEO_CAP: u64 = 150 * 1024 * 1024;
/// Largest picture file `fetch` downloads.
pub const IMAGE_CAP: u64 = 40 * 1024 * 1024;

/// Wikimedia asks every client for a descriptive User-Agent with a contact.
const USER_AGENT: &str = "VeeCutStudio/0.1 (https://github.com/Demonbane18/veecut-studio)";

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    Nasa,
    Commons,
    Archive,
    Openverse,
    /// Only with `PEXELS_API_KEY`.
    Pexels,
}

impl Provider {
    pub const ALL: [Provider; 5] = [
        Provider::Nasa,
        Provider::Commons,
        Provider::Archive,
        Provider::Openverse,
        Provider::Pexels,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Provider::Nasa => "nasa",
            Provider::Commons => "commons",
            Provider::Archive => "archive",
            Provider::Openverse => "openverse",
            Provider::Pexels => "pexels",
        }
    }

    /// The name used in credit lines.
    pub fn label(self) -> &'static str {
        match self {
            Provider::Nasa => "NASA",
            Provider::Commons => "Wikimedia Commons",
            Provider::Archive => "Internet Archive",
            Provider::Openverse => "Openverse",
            Provider::Pexels => "Pexels",
        }
    }

    /// Every keyless provider, plus Pexels when `PEXELS_API_KEY` is set.
    pub fn available() -> Vec<Provider> {
        Provider::ALL
            .into_iter()
            .filter(|p| *p != Provider::Pexels || pexels::key().is_some())
            .collect()
    }

    pub fn parse(name: &str) -> Result<Provider> {
        let name = name.trim().to_lowercase();
        Provider::ALL
            .into_iter()
            .find(|p| p.as_str() == name)
            .ok_or_else(|| {
                anyhow!(
                    "unknown provider {name:?}; use nasa, commons, archive, openverse or pexels"
                )
            })
    }

    fn search(self, query: &str, kind: Option<MediaKind>, count: usize) -> Result<Vec<Candidate>> {
        let list = match self {
            Provider::Nasa => nasa::search(query, kind, count),
            Provider::Commons => commons::search(query, kind, count),
            Provider::Archive => archive::search(query, kind, count),
            Provider::Openverse => openverse::search(query, kind, count),
            Provider::Pexels => pexels::search(query, kind, count),
        }?;
        Ok(list.into_iter().map(Candidate::tidy).collect())
    }

    /// The candidate with fresh rights, and its downloadable files, best first.
    fn resolve(self, id: &str) -> Result<(Candidate, Vec<String>)> {
        let (c, files) = match self {
            Provider::Nasa => nasa::resolve(id),
            Provider::Commons => commons::resolve(id),
            Provider::Archive => archive::resolve(id),
            Provider::Openverse => openverse::resolve(id),
            Provider::Pexels => pexels::resolve(id),
        }?;
        Ok((c.tidy(), files.iter().map(|f| safe_url(f)).collect()))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    Image,
    Video,
}

/// One search result.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Candidate {
    /// `<provider>:<the provider's id>`, for example `nasa:PIA01637`.
    pub id: String,
    pub provider: Provider,
    pub kind: MediaKind,
    pub title: String,
    /// The page that describes the file.
    pub page_url: String,
    pub thumb_url: String,
    /// Pixels; 0 when the provider does not say before download.
    pub width: u32,
    pub height: u32,
    /// Seconds; 0 for pictures or when unknown before download.
    pub duration: f64,
    /// VeeCut's short form: "public domain", "CC0", "CC BY 4.0", "Pexels License", "unknown"...
    pub license: String,
    pub license_url: Option<String>,
    pub author: Option<String>,
    /// The rights are unclear, so a person must check before a final render.
    pub flag: bool,
    pub flag_reason: Option<String>,
}

impl Candidate {
    /// Fills the licence fields from any licence text (see [`rights::classify`]).
    fn set_rights(&mut self, text: &str) {
        let r = rights::classify(text);
        self.license = r.license;
        self.flag = r.flag.is_some();
        self.flag_reason = r.flag;
    }

    /// Flags the candidate for a reason besides its licence (keeps the first reason).
    fn flag_for(&mut self, reason: Option<String>) {
        if let Some(reason) = reason
            && !self.flag
        {
            self.flag = true;
            self.flag_reason = Some(reason);
        }
    }

    /// Addresses as valid URIs: some providers send spaces and non-ASCII characters raw
    /// (NASA ids like "Moon and Saturn").
    fn tidy(mut self) -> Candidate {
        self.page_url = safe_url(&self.page_url);
        self.thumb_url = safe_url(&self.thumb_url);
        self.license_url = self.license_url.as_deref().map(safe_url);
        self
    }

    /// The provenance record for the project.
    pub fn source(&self) -> Source {
        Source {
            url: self.page_url.clone(),
            title: self.title.clone(),
            author: self.author.clone(),
            license: self.license.clone(),
            license_url: self.license_url.clone(),
            credit: Some(rights::credit(self)),
            provider: self.provider.as_str().to_string(),
            flag: self.flag,
            flag_reason: self.flag_reason.clone(),
            approved: false,
            retrieved: chrono::Local::now().format("%Y-%m-%d").to_string(),
        }
    }

    /// A blank candidate the provider parsers fill in.
    fn new(provider: Provider, id: &str, kind: MediaKind) -> Candidate {
        Candidate {
            id: format!("{}:{id}", provider.as_str()),
            provider,
            kind,
            title: String::new(),
            page_url: String::new(),
            thumb_url: String::new(),
            width: 0,
            height: 0,
            duration: 0.0,
            license: "unknown".into(),
            license_url: None,
            author: None,
            flag: false,
            flag_reason: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SearchOptions {
    /// Pictures, videos, or both (`None`).
    pub kind: Option<MediaKind>,
    /// Empty means [`Provider::available`].
    pub providers: Vec<Provider>,
    /// Results asked of each provider (1 to 20).
    pub per_provider: usize,
}

impl Default for SearchOptions {
    fn default() -> Self {
        SearchOptions {
            kind: None,
            providers: Provider::available(),
            per_provider: 4,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SearchResult {
    pub candidates: Vec<Candidate>,
    /// One line per provider that failed or was skipped; the others still answer.
    pub warnings: Vec<String>,
}

/// Searches the providers at the same time. One provider failing never fails the search: its
/// error becomes a warning.
pub fn search(query: &str, opts: &SearchOptions) -> Result<SearchResult> {
    let query = query.trim();
    ensure!(
        !query.is_empty(),
        "say what to look for, for example \"rocket launch\""
    );
    let providers = if opts.providers.is_empty() {
        Provider::available()
    } else {
        opts.providers.clone()
    };
    let count = opts.per_provider.clamp(1, 20);
    let answers: Vec<(Provider, Result<Vec<Candidate>>)> = std::thread::scope(|s| {
        let jobs: Vec<_> = providers
            .iter()
            .map(|&p| (p, s.spawn(move || p.search(query, opts.kind, count))))
            .collect();
        jobs.into_iter()
            .map(|(p, job)| {
                let answer = job
                    .join()
                    .unwrap_or_else(|_| Err(anyhow!("the search crashed")));
                (p, answer)
            })
            .collect()
    });
    let mut out = SearchResult::default();
    let mut seen = BTreeSet::new();
    for (p, answer) in answers {
        match answer {
            Ok(list) => out.candidates.extend(
                list.into_iter()
                    .filter(|c| opts.kind.is_none_or(|k| k == c.kind))
                    .filter(|c| seen.insert(c.id.clone()))
                    .take(count),
            ),
            Err(e) => out.warnings.push(format!("{}: {e:#}", p.label())),
        }
    }
    Ok(out)
}

/// A downloaded file with its provenance.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Fetched {
    pub path: PathBuf,
    pub source: Source,
    pub candidate: Candidate,
}

/// What `<file>.source.json` holds: the source record plus the candidate it came from.
#[derive(Serialize, Deserialize)]
struct Sidecar {
    #[serde(flatten)]
    source: Source,
    candidate: Candidate,
}

/// Downloads a candidate into `<ws>/footage/<provider>-<id>.<ext>` through a `.part` file,
/// within [`VIDEO_CAP`] or [`IMAGE_CAP`], checks that ffprobe reads it as the expected kind
/// and writes `<file>.source.json`. An earlier download of the same id is reused.
pub fn fetch(ws: &Workspace, id: &str) -> Result<Fetched> {
    let (provider, key) = id
        .split_once(':')
        .with_context(|| format!("{id:?} is not a footage id; ids look like nasa:PIA01637"))?;
    let provider = Provider::parse(provider)?;
    ensure!(!key.trim().is_empty(), "{id:?} has no id after the colon");
    let dir = ws.footage();
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let stem = format!("{}-{}", provider.as_str(), file_safe(key));
    if let Some(done) = existing(&dir, &stem)? {
        return Ok(done);
    }
    let (mut candidate, urls) = provider.resolve(key)?;
    ensure!(
        !urls.is_empty(),
        "{id} has no file VeeCut can download (it wants jpg, png or webp pictures and mp4, webm or ogv videos); pick another candidate"
    );
    let cap = match candidate.kind {
        MediaKind::Video => VIDEO_CAP,
        MediaKind::Image => IMAGE_CAP,
    };
    let part = dir.join(format!("{stem}.part"));
    let mut errors = Vec::new();
    let mut got = None;
    for url in &urls {
        match download(url, &part, cap, candidate.kind) {
            Ok(ext) => {
                got = Some(ext);
                break;
            }
            Err(e) => {
                let _ = fs::remove_file(&part);
                errors.push(format!("{e:#}"));
            }
        }
    }
    let Some(ext) = got else {
        // One line per size tried would repeat the long address; the preferred size's error
        // and the count say enough.
        bail!(
            "could not download {id} (tried {} files; the first: {})",
            errors.len(),
            errors.first().map(String::as_str).unwrap_or("no answer")
        );
    };
    let path = dir.join(format!("{stem}.{ext}"));
    fs::rename(&part, &path).with_context(|| format!("saving {}", path.display()))?;
    let want = match candidate.kind {
        MediaKind::Image => AssetKind::Image,
        MediaKind::Video => AssetKind::Video,
    };
    let info = match media::probe(&path) {
        Ok(info) if info.kind == want => info,
        Ok(info) => {
            let _ = fs::remove_file(&path);
            bail!(
                "{id} downloaded as {:?}, not {:?}; pick another candidate",
                info.kind,
                want
            );
        }
        Err(e) => {
            let _ = fs::remove_file(&path);
            return Err(e.context(format!("{id} did not download as a readable file")));
        }
    };
    candidate.width = info.width;
    candidate.height = info.height;
    candidate.duration = info.duration;
    let source = candidate.source();
    let sidecar = sidecar_path(&path);
    let text = serde_json::to_string_pretty(&Sidecar {
        source: source.clone(),
        candidate: candidate.clone(),
    })?;
    fs::write(&sidecar, text + "\n").with_context(|| format!("writing {}", sidecar.display()))?;
    Ok(Fetched {
        path,
        source,
        candidate,
    })
}

fn sidecar_path(file: &Path) -> PathBuf {
    let mut name = file.as_os_str().to_owned();
    name.push(".source.json");
    PathBuf::from(name)
}

/// Imports adjacent fetch provenance; approval must be granted in the receiving project.
pub(crate) fn source_for(file: &Path) -> Result<Option<Source>> {
    let path = sidecar_path(file);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let mut source: Source = serde_json::from_slice(&bytes)
        .with_context(|| format!("reading source record {}", path.display()))?;
    source.approved = false;
    Ok(Some(source))
}

/// An earlier, still readable download of `stem` with its sidecar.
fn existing(dir: &Path, stem: &str) -> Result<Option<Fetched>> {
    let prefix = format!("{stem}.");
    for entry in fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(file) = name
            .strip_suffix(".source.json")
            .filter(|f| f.starts_with(&prefix))
        else {
            continue;
        };
        let path = dir.join(file);
        let Ok(side) = fs::read(entry.path())
            .map_err(anyhow::Error::from)
            .and_then(|b| Ok(serde_json::from_slice::<Sidecar>(&b)?))
        else {
            continue;
        };
        if path.is_file() && media::probe(&path).is_ok() {
            return Ok(Some(Fetched {
                path,
                source: side.source,
                candidate: side.candidate,
            }));
        }
    }
    Ok(None)
}

/// Provider ids as file names: letters, digits, `.`, `_` and `-`; at most 100 characters.
fn file_safe(id: &str) -> String {
    let safe: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .take(100)
        .collect();
    safe.trim_matches('.').to_string()
}

/// Adds a fetched file to a project as an asset and records its source through ops. A file the
/// project already uses keeps its asset id and any approval the user gave. Does not save.
pub fn add_to_project(
    ws: &Workspace,
    project: &mut Project,
    project_dir: &Path,
    fetched: &Fetched,
    role: AssetRole,
) -> Result<String> {
    let file = fs::canonicalize(&fetched.path).ok();
    let known = project
        .assets
        .iter()
        .find(|a| file.is_some() && fs::canonicalize(project_dir.join(&a.path)).ok() == file)
        .map(|a| a.id.clone());
    let id = match known {
        Some(id) => id,
        None => ws.add_asset(project, project_dir, &fetched.path, role)?,
    };
    let mut source = fetched.source.clone();
    if let Some(old) = project.sources.get(&id) {
        source.approved = old.approved;
    }
    ops::apply(project, &source_op(project, &id, &source)?)?;
    Ok(id)
}

/// The op that records `source` for `asset`. `set` needs the parent object to exist, and an
/// empty source list is left out of the saved project, so the first source sets the whole map.
pub fn source_op(project: &Project, asset: &str, source: &Source) -> Result<Op> {
    let value = serde_json::to_value(source)?;
    Ok(if project.sources.is_empty() {
        Op::Set {
            path: "/sources".into(),
            value: serde_json::json!({ asset: value }),
        }
    } else {
        Op::Set {
            path: format!("/sources/{}", asset.replace('~', "~0").replace('/', "~1")),
            value,
        }
    })
}

/// Asset ids that clips on any track use.
pub fn used_assets(project: &Project) -> BTreeSet<&str> {
    project
        .tracks
        .iter()
        .flat_map(|t| &t.items)
        .filter_map(|i| i.clip())
        .map(|c| c.asset.as_str())
        .collect()
}

/// Flagged sources nobody approved that the project's clips use.
pub fn blocking_sources(project: &Project) -> Vec<(String, Source)> {
    let used = used_assets(project);
    project
        .sources
        .iter()
        .filter(|(id, s)| s.blocks_final() && used.contains(id.as_str()))
        .map(|(id, s)| (id.clone(), s.clone()))
        .collect()
}

/// Refuses a final render while [`blocking_sources`] is not empty. Drafts never call this.
pub fn check_final(project: &Project) -> Result<()> {
    let blocking = blocking_sources(project);
    if blocking.is_empty() {
        return Ok(());
    }
    let mut text = String::from(
        "the final render is blocked: the edit uses found files whose rights a person must check first.\n",
    );
    for (id, s) in &blocking {
        text += &format!(
            "- {id} {:?} ({}, {}): {}\n",
            s.title,
            s.license,
            if s.url.is_empty() {
                "no source page"
            } else {
                &s.url
            },
            s.flag_reason.as_deref().unwrap_or("flagged")
        );
    }
    text += "The user must check each licence and then run `veecut sources --approve <asset>` for every file they have the right to use, or replace it. Agents must not approve sources on their own: show this list to the user and ask. Drafts still render.";
    bail!(text)
}

static HTTP: LazyLock<ureq::Agent> = LazyLock::new(|| {
    ureq::Agent::config_builder()
        .user_agent(USER_AGENT)
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_recv_response(Some(Duration::from_secs(30)))
        .timeout_per_read(Some(Duration::from_secs(60)))
        .build()
        .into()
});

/// A failed request as a readable error.
fn http_error(url: &str, e: ureq::Error) -> anyhow::Error {
    let host = url
        .split("://")
        .nth(1)
        .and_then(|r| r.split('/').next())
        .unwrap_or(url);
    match e {
        ureq::Error::StatusCode(429) => {
            anyhow!("{host} is rate limiting VeeCut; wait a few minutes and search again")
        }
        ureq::Error::StatusCode(401 | 403) => anyhow!("{host} refused the request (HTTP 403)"),
        ureq::Error::StatusCode(code) => anyhow!("{host} answered HTTP {code}"),
        ureq::Error::Timeout(_) => anyhow!("{host} did not answer in time"),
        e => anyhow!("could not reach {host}: {e}"),
    }
}

/// GETs JSON with query parameters and extra headers, within 20 seconds, so one slow provider
/// does not hold a search for long.
fn get_json<T: DeserializeOwned>(
    url: &str,
    query: &[(&str, &str)],
    headers: &[(&str, &str)],
) -> Result<T> {
    let mut req = HTTP
        .get(&safe_url(url))
        .header("Accept", "application/json")
        .query_pairs(query.iter().copied());
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let mut resp = req
        .config()
        .timeout_global(Some(Duration::from_secs(20)))
        .build()
        .call()
        .map_err(|e| http_error(url, e))?;
    resp.body_mut()
        .with_config()
        .limit(16 * 1024 * 1024)
        .read_json()
        .with_context(|| format!("{url} sent an answer VeeCut could not read"))
}

/// GETs a small file (a thumbnail) into memory.
fn get_bytes(url: &str, cap: u64) -> Result<Vec<u8>> {
    let mut resp = HTTP
        .get(&safe_url(url))
        .config()
        .timeout_global(Some(Duration::from_secs(20)))
        .build()
        .call()
        .map_err(|e| http_error(url, e))?;
    Ok(resp.body_mut().with_config().limit(cap).read_to_vec()?)
}

/// Streams `url` into `part`, at most `cap` bytes, and returns the file extension its first
/// bytes show. Redirects are followed; an HTML check page or the wrong kind of file is refused.
fn download(url: &str, part: &Path, cap: u64, kind: MediaKind) -> Result<&'static str> {
    let resp = HTTP
        .get(&safe_url(url))
        .call()
        .map_err(|e| http_error(url, e))?;
    let mb = |b: u64| b / (1024 * 1024);
    if let Some(len) = resp.body().content_length() {
        ensure!(
            len <= cap,
            "{url} is {} MB, over the {} MB limit",
            mb(len),
            mb(cap)
        );
    }
    let mut body = resp.into_body().into_reader();
    let mut file = BufWriter::new(
        fs::File::create(part).with_context(|| format!("creating {}", part.display()))?,
    );
    let mut buf = vec![0u8; 1 << 16];
    let mut head = Vec::with_capacity(16);
    let mut total = 0u64;
    loop {
        let n = body
            .read(&mut buf)
            .with_context(|| format!("the download of {url} was interrupted; run fetch again"))?;
        if n == 0 {
            break;
        }
        total += n as u64;
        ensure!(total <= cap, "{url} is over the {} MB limit", mb(cap));
        if head.len() < 16 {
            head.extend_from_slice(&buf[..n.min(16 - head.len())]);
        }
        file.write_all(&buf[..n])?;
    }
    file.flush()?;
    match sniff(&head) {
        Some((ext, k)) if k == kind => Ok(ext),
        Some((ext, _)) => bail!("{url} sent a {ext} file, not the expected {kind:?}"),
        None => {
            bail!("{url} did not send a picture or video (it may be a check page or an error page)")
        }
    }
}

/// The file type from its first bytes.
fn sniff(head: &[u8]) -> Option<(&'static str, MediaKind)> {
    use MediaKind::{Image, Video};
    let at = |i: usize, s: &[u8]| head.get(i..i + s.len()) == Some(s);
    Some(if at(0, &[0xFF, 0xD8, 0xFF]) {
        ("jpg", Image)
    } else if at(0, b"\x89PNG") {
        ("png", Image)
    } else if at(0, b"RIFF") && at(8, b"WEBP") {
        ("webp", Image)
    } else if at(4, b"ftyp") && at(8, b"qt  ") {
        ("mov", Video)
    } else if at(4, b"ftyp") {
        ("mp4", Video)
    } else if at(0, &[0x1A, 0x45, 0xDF, 0xA3]) {
        ("webm", Video)
    } else if at(0, b"OggS") {
        ("ogv", Video)
    } else if at(0, &[0, 0, 1, 0xBA]) || at(0, &[0, 0, 1, 0xB3]) {
        ("mpg", Video)
    } else if at(0, b"RIFF") && at(8, b"AVI ") {
        ("avi", Video)
    } else {
        return None;
    })
}

/// Percent-encodes a URL path segment.
fn encode(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    for b in segment.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out += &format!("%{b:02X}");
        }
    }
    out
}

/// A whole URL with every byte a URI may not hold raw (spaces, quotes, non-ASCII) percent-encoded.
/// Reserved characters and existing `%XX` escapes stay, so it is safe to apply twice.
pub fn safe_url(url: &str) -> String {
    let mut out = String::with_capacity(url.len());
    for b in url.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~:/?#[]@!$&'()*+,;=%".contains(&b) {
            out.push(b as char);
        } else {
            out += &format!("%{b:02X}");
        }
    }
    out
}

/// Plain text from a provider's HTML snippet: tags dropped, common entities decoded,
/// whitespace collapsed.
fn plain(html: &str) -> String {
    let mut text = String::with_capacity(html.len());
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => text.push(c),
            _ => {}
        }
    }
    let text = text
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#039;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&nbsp;", " ");
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `text` cut to `max` characters on a character boundary, with an ellipsis when cut.
fn clip_text(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut cut: String = text.chars().take(max.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::{Asset, Canvas, Item, VideoKind};

    fn flagged(reason: &str) -> Source {
        Source {
            url: "https://example.org/a".into(),
            title: "A tap".into(),
            license: "CC BY-NC 2.0".into(),
            flag: true,
            flag_reason: Some(reason.into()),
            provider: "openverse".into(),
            ..Source::default()
        }
    }

    fn project_with(sources: &[(&str, Source, bool)]) -> Project {
        let mut p = Project::new("t", VideoKind::Documentary, Canvas::vertical());
        for (id, source, used) in sources {
            p.assets.push(Asset {
                id: id.to_string(),
                path: PathBuf::from(format!("{id}.jpg")),
                kind: AssetKind::Image,
                role: AssetRole::Broll,
                duration: 0.0,
                width: 100,
                height: 100,
                fps: 0.0,
                has_audio: false,
            });
            if *used {
                let item: Item = serde_json::from_value(serde_json::json!({
                    "id": format!("clip-{id}"), "start": 0.0, "duration": 1.0, "type": "clip", "asset": id
                }))
                .unwrap();
                p.tracks[1].items.push(item);
            }
            let op = source_op(&p, id, source).unwrap();
            ops::apply(&mut p, &op).unwrap();
        }
        p
    }

    #[test]
    fn final_render_blocks_only_used_flagged_unapproved_sources() {
        let ok = Source {
            license: "CC0".into(),
            ..Source::default()
        };
        let mut approved = flagged("no commercial use (NC)");
        approved.approved = true;
        let p = project_with(&[
            ("asset-1", flagged("no commercial use (NC)"), true),
            ("asset-2", flagged("unused"), false),
            ("asset-3", approved, true),
            ("asset-4", ok, true),
        ]);
        let blocking = blocking_sources(&p);
        assert_eq!(
            blocking
                .iter()
                .map(|(id, _)| id.as_str())
                .collect::<Vec<_>>(),
            ["asset-1"]
        );
        let message = format!("{:#}", check_final(&p).unwrap_err());
        assert!(message.contains("asset-1") && message.contains("CC BY-NC 2.0"));
        assert!(message.contains("veecut sources --approve <asset>"));
        assert!(message.contains("Agents must not approve"));
        assert!(!message.contains("asset-2"));

        let mut cleared = p.clone();
        let approve = Op::Set {
            path: "/sources/asset-1/approved".into(),
            value: serde_json::json!(true),
        };
        ops::apply(&mut cleared, &approve).unwrap();
        assert!(check_final(&cleared).is_ok());
    }

    #[test]
    fn recipe_asset_import_keeps_sidecar_credits_and_blocks_flagged_finals() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = Workspace::init(tmp.path()).unwrap();
        fs::create_dir_all(ws.footage()).unwrap();
        let file = ws.footage().join("nasa-synthetic.png");
        image::RgbaImage::new(32, 32).save(&file).unwrap();
        let mut source = flagged("third-party credit needs review");
        source.credit = Some("Synthetic credit, CC BY-NC 2.0".into());
        source.approved = true;
        fs::write(
            sidecar_path(&file),
            serde_json::to_vec(&Sidecar {
                source,
                candidate: Candidate::new(Provider::Nasa, "synthetic", MediaKind::Image),
            })
            .unwrap(),
        )
        .unwrap();
        let (dir, mut project) = ws
            .create_project("recipe", VideoKind::Explainer, Canvas::vertical())
            .unwrap();
        // Recipes and `veecut add` share this import path, not `add_to_project`.
        let id = ws
            .add_asset(&mut project, &dir, &file, AssetRole::Broll)
            .unwrap();
        assert_eq!(
            project.sources[&id].credit.as_deref(),
            Some("Synthetic credit, CC BY-NC 2.0")
        );
        assert!(
            !project.sources[&id].approved,
            "approval belongs to this project"
        );
        assert!(check_final(&project).is_ok(), "unused files do not block");
        let clip: Op = serde_json::from_value(serde_json::json!({
            "op": "add_clip", "asset": id, "track": "main", "duration": 2.0
        }))
        .unwrap();
        ops::apply(&mut project, &clip).unwrap();
        ws.save_project(&dir, &project).unwrap();
        let saved = Project::load(&dir.join(crate::project::PROJECT_FILE)).unwrap();
        assert!(check_final(&saved).unwrap_err().to_string().contains(&id));
    }

    #[test]
    fn sniff_names_files_by_their_first_bytes() {
        assert_eq!(
            sniff(b"\xFF\xD8\xFF\xE0rest"),
            Some(("jpg", MediaKind::Image))
        );
        assert_eq!(
            sniff(b"\x00\x00\x00\x20ftypisom"),
            Some(("mp4", MediaKind::Video))
        );
        assert_eq!(sniff(b"\x1A\x45\xDF\xA3"), Some(("webm", MediaKind::Video)));
        assert_eq!(
            sniff(b"RIFF\x00\x00\x00\x00WEBPVP8 "),
            Some(("webp", MediaKind::Image))
        );
        assert_eq!(sniff(b"<!DOCTYPE html>"), None);
        assert_eq!(sniff(b""), None);
    }

    #[test]
    fn helpers_clean_ids_and_html() {
        assert_eq!(file_safe("File:A b/c.jpg"), "File-A-b-c.jpg");
        assert_eq!(encode("a b&c.mp4"), "a%20b%26c.mp4");
        let raw = "https://images-assets.nasa.gov/video/Moon and Saturn/Moon and Saturn~thumb.jpg";
        let safe = "https://images-assets.nasa.gov/video/Moon%20and%20Saturn/Moon%20and%20Saturn~thumb.jpg";
        assert_eq!(safe_url(raw), safe);
        assert_eq!(safe_url(safe), safe, "applying twice changes nothing");
        assert_eq!(
            safe_url("https://x.org/ü?q=a\"b"),
            "https://x.org/%C3%BC?q=a%22b"
        );
        assert_eq!(
            plain("<a href=\"x\">United States Air Force</a> photo by &amp; <b>Joe</b>\n  Smith"),
            "United States Air Force photo by & Joe Smith"
        );
        assert_eq!(clip_text("abcdef", 4), "abc…");
    }
}
