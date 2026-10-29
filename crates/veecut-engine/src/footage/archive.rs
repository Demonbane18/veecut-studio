//! Internet Archive: `advancedsearch` for items, the metadata API for their files. The licence is
//! the item's `licenseurl`; the Prelinger collection is public domain.

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value;

use super::{Candidate, MediaKind, Provider, clip_text, encode, get_json, plain};

#[derive(Deserialize)]
struct Search {
    response: Response,
}

#[derive(Deserialize)]
struct Response {
    #[serde(default)]
    docs: Vec<Doc>,
}

/// An item's metadata; the same fields come from `advancedsearch` and the metadata API.
#[derive(Deserialize, Default)]
#[serde(default)]
struct Doc {
    identifier: String,
    title: Value,
    licenseurl: Option<String>,
    creator: Value,
    collection: Value,
    mediatype: String,
}

#[derive(Deserialize)]
struct Metadata {
    metadata: Option<Doc>,
    #[serde(default)]
    files: Vec<File>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct File {
    name: String,
    format: String,
    size: Option<String>,
    width: Option<String>,
    height: Option<String>,
    length: Option<String>,
}

/// One string from a field that is sometimes a string and sometimes a list.
fn first(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Array(a) => a.iter().find_map(first),
        _ => None,
    }
}

fn all(v: &Value) -> Vec<String> {
    match v {
        Value::String(s) => vec![s.clone()],
        Value::Array(a) => a.iter().flat_map(all).collect(),
        _ => Vec::new(),
    }
}

pub(super) fn search(query: &str, kind: Option<MediaKind>, count: usize) -> Result<Vec<Candidate>> {
    // Keep Lucene syntax out of the user's words.
    let words: String = query
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' {
                c
            } else {
                ' '
            }
        })
        .collect();
    let media = match kind {
        None => "(movies OR image)",
        Some(MediaKind::Video) => "movies",
        Some(MediaKind::Image) => "image",
    };
    // Items with a licence (or Prelinger) only; the rest would all be flagged.
    let q = format!(
        "({}) AND mediatype:{media} AND (collection:prelinger OR licenseurl:*) AND -collection:deemphasize",
        words.split_whitespace().collect::<Vec<_>>().join(" ")
    );
    let rows = count.to_string();
    let answer: Search = get_json(
        "https://archive.org/advancedsearch.php",
        &[
            ("q", q.as_str()),
            ("fl[]", "identifier"),
            ("fl[]", "title"),
            ("fl[]", "licenseurl"),
            ("fl[]", "creator"),
            ("fl[]", "collection"),
            ("fl[]", "mediatype"),
            ("rows", rows.as_str()),
            ("output", "json"),
        ],
        &[],
    )?;
    Ok(answer.response.docs.iter().filter_map(candidate).collect())
}

fn candidate(d: &Doc) -> Option<Candidate> {
    let kind = match d.mediatype.as_str() {
        "movies" => MediaKind::Video,
        "image" => MediaKind::Image,
        _ => return None,
    };
    if d.identifier.is_empty() {
        return None;
    }
    let mut c = Candidate::new(Provider::Archive, &d.identifier, kind);
    c.title = clip_text(
        &plain(&first(&d.title).unwrap_or_else(|| d.identifier.clone())),
        120,
    );
    c.page_url = format!("https://archive.org/details/{}", encode(&d.identifier));
    c.thumb_url = format!("https://archive.org/services/img/{}", encode(&d.identifier));
    c.author = first(&d.creator).map(|a| clip_text(&plain(&a), 120));
    let prelinger = all(&d.collection)
        .iter()
        .any(|c| c.eq_ignore_ascii_case("prelinger"));
    match d.licenseurl.as_deref().filter(|u| !u.trim().is_empty()) {
        Some(url) => {
            c.license_url = Some(url.to_string());
            c.set_rights(url);
        }
        None if prelinger => c.license = "public domain".into(),
        None => c.set_rights(""),
    }
    if c.flag && d.licenseurl.is_none() {
        c.flag_reason = Some("this Archive item has no licence and is not in the Prelinger collection; check who owns it".into());
    }
    Some(c)
}

pub(super) fn resolve(id: &str) -> Result<(Candidate, Vec<String>)> {
    let answer: Metadata = get_json(
        &format!("https://archive.org/metadata/{}", encode(id)),
        &[],
        &[],
    )?;
    let doc = answer
        .metadata
        .with_context(|| format!("the Internet Archive has no item {id:?}"))?;
    let mut c = candidate(&doc)
        .with_context(|| format!("Archive item {id:?} is neither a movie nor an image"))?;
    let files = pick(&answer.files, c.kind);
    if let Some(best) = files.first() {
        c.width = num(&best.width) as u32;
        c.height = num(&best.height) as u32;
        c.duration = best.length.as_deref().map(seconds).unwrap_or(0.0);
    }
    let urls = files
        .iter()
        .map(|f| {
            let path: Vec<String> = f.name.split('/').map(encode).collect();
            format!(
                "https://archive.org/download/{}/{}",
                encode(id),
                path.join("/")
            )
        })
        .collect();
    Ok((c, urls))
}

fn num(v: &Option<String>) -> f64 {
    v.as_deref()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0.0)
}

/// "910.78" or "15:10" (minutes:seconds) or "1:02:03".
fn seconds(s: &str) -> f64 {
    s.split(':')
        .try_fold(0.0, |acc, part| {
            part.trim().parse::<f64>().map(|v| acc * 60.0 + v)
        })
        .unwrap_or(0.0)
}

/// MP4 videos, 720p to 1080p first and then smaller; JPEG or PNG pictures, largest first.
fn pick(files: &[File], kind: MediaKind) -> Vec<&File> {
    let mut out: Vec<&File> = files
        .iter()
        .filter(|f| {
            let name = f.name.to_lowercase();
            let thumb = name.contains(".thumbs/")
                || name.starts_with("__ia_thumb")
                || f.format == "Thumbnail"
                || f.format == "Item Tile";
            !thumb
                && match kind {
                    MediaKind::Video => name.ends_with(".mp4"),
                    MediaKind::Image => [".jpg", ".jpeg", ".png"].iter().any(|e| name.ends_with(e)),
                }
        })
        .collect();
    match kind {
        MediaKind::Video => out.sort_by_key(|f| {
            let h = num(&f.height) as i64;
            (!(720..=1080).contains(&h), if h > 1080 { h } else { -h })
        }),
        MediaKind::Image => out.sort_by_key(|f| std::cmp::Reverse(num(&f.size) as u64)),
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Excerpts of live answers (2026-10-29), trimmed.
    const SEARCH: &str = r#"{"responseHeader":{"status":0},"response":{"numFound":390,"start":0,"docs":[
      {"collection":["opensource_movies"],"identifier":"china-cz-6a-rocket-launch","licenseurl":"https://creativecommons.org/licenses/by-nc-nd/4.0/","mediatype":"movies","title":"China Long March CZ-6A Rocket Launch"},
      {"collection":["universal_newsreels","newsandpublicaffairs"],"identifier":"1957-10-28_Space_Race","licenseurl":"http://creativecommons.org/licenses/publicdomain/","mediatype":"movies","title":"Space Race"},
      {"collection":"prelinger","identifier":"Operatio1955","mediatype":"movies","title":"Operation Cue (1955)","creator":"U.S. Federal Civil Defense Administration"},
      {"collection":["community"],"identifier":"someones-clip","mediatype":"movies","title":"Someone's clip","creator":["TheSpaceDevs","SpaceX"]},
      {"identifier":"a-book","mediatype":"texts","title":"A book"}
    ]}}"#;

    const METADATA: &str = r#"{"metadata":{"mediatype":"movies","identifier":"Operatio1955","creator":"U.S. Federal Civil Defense Administration",
      "licenseurl":"http://creativecommons.org/licenses/publicdomain/","collection":"prelinger","title":"Operation Cue (1955)"},
      "files":[
        {"name":"Operatio1955.avi","format":"Cinepack","size":"47438686","width":"320","height":"240","length":"911.07","source":"original"},
        {"name":"Operatio1955.mp4","format":"h.264","size":"94652878","width":"640","height":"480","length":"910.78","source":"derivative"},
        {"name":"Operatio1955_512kb.mp4","format":"512Kb MPEG4","size":"64667095","width":"320","height":"240","length":"910.77"},
        {"name":"Operatio1955_hd.mp4","format":"h.264 HD","size":"194652878","width":"1280","height":"720","length":"15:10"},
        {"name":"Operatio1955.thumbs/Operatio1955_000001.jpg","format":"Thumbnail","size":"1669"},
        {"name":"__ia_thumb.jpg","format":"Item Tile","size":"3978"}
      ]}"#;

    #[test]
    fn parses_search_and_classifies_rights() {
        let answer: Search = serde_json::from_str(SEARCH).unwrap();
        let list: Vec<_> = answer.response.docs.iter().filter_map(candidate).collect();
        assert_eq!(list.len(), 4, "texts are skipped");
        assert!(list[0].flag && list[0].license == "CC BY-NC-ND 4.0");
        assert_eq!(
            (list[1].license.as_str(), list[1].flag),
            ("public domain", false)
        );
        assert_eq!(
            (list[2].license.as_str(), list[2].flag),
            ("public domain", false),
            "Prelinger"
        );
        assert_eq!(
            list[2].author.as_deref(),
            Some("U.S. Federal Civil Defense Administration")
        );
        assert_eq!(list[2].page_url, "https://archive.org/details/Operatio1955");
        assert!(
            list[3].flag
                && list[3]
                    .flag_reason
                    .as_deref()
                    .unwrap()
                    .contains("Prelinger")
        );
        assert_eq!(list[3].author.as_deref(), Some("TheSpaceDevs"));
    }

    #[test]
    fn picks_an_hd_mp4_from_the_metadata() {
        let m: Metadata = serde_json::from_str(METADATA).unwrap();
        let names: Vec<_> = pick(&m.files, MediaKind::Video)
            .iter()
            .map(|f| f.name.as_str())
            .collect();
        assert_eq!(
            names,
            [
                "Operatio1955_hd.mp4",
                "Operatio1955.mp4",
                "Operatio1955_512kb.mp4"
            ]
        );
        assert_eq!(seconds("15:10"), 910.0);
        assert_eq!(seconds("910.78"), 910.78);
        assert!(
            pick(&m.files, MediaKind::Image).is_empty(),
            "thumbnails are not pictures to fetch"
        );
    }
}
