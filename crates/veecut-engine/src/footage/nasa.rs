//! NASA Image and Video Library (`images-api.nasa.gov`). NASA media is public domain unless a
//! credit names someone else; files are listed in each item's `collection.json`.

use std::sync::LazyLock;

use anyhow::{Context, Result};
use regex::Regex;
use serde::Deserialize;

use super::{Candidate, MediaKind, Provider, get_json, plain};

const API: &str = "https://images-api.nasa.gov/search";
/// NASA's media usage guidelines.
const GUIDELINES: &str = "https://www.nasa.gov/nasa-brand-center/images-and-media/";

#[derive(Deserialize)]
struct Search {
    collection: Collection,
}

#[derive(Deserialize)]
struct Collection {
    #[serde(default)]
    items: Vec<Item>,
}

#[derive(Deserialize)]
struct Item {
    /// The item's `collection.json`.
    href: String,
    #[serde(default)]
    data: Vec<Data>,
    #[serde(default)]
    links: Vec<Link>,
}

#[derive(Deserialize)]
struct Data {
    nasa_id: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    media_type: String,
    description: Option<String>,
    photographer: Option<String>,
    secondary_creator: Option<String>,
}

#[derive(Deserialize)]
struct Link {
    href: String,
    rel: Option<String>,
}

pub(super) fn search(query: &str, kind: Option<MediaKind>, count: usize) -> Result<Vec<Candidate>> {
    let media = match kind {
        None => "image,video",
        Some(MediaKind::Image) => "image",
        Some(MediaKind::Video) => "video",
    };
    let size = count.to_string();
    let answer: Search = get_json(
        API,
        &[("q", query), ("media_type", media), ("page_size", &size)],
        &[],
    )?;
    Ok(parse(answer).into_iter().map(|(c, _)| c).collect())
}

/// The candidates and their `collection.json` addresses.
fn parse(answer: Search) -> Vec<(Candidate, String)> {
    answer
        .collection
        .items
        .into_iter()
        .filter_map(|item| {
            let d = item.data.into_iter().next()?;
            let kind = match d.media_type.as_str() {
                "image" => MediaKind::Image,
                "video" => MediaKind::Video,
                _ => return None,
            };
            let mut c = Candidate::new(Provider::Nasa, &d.nasa_id, kind);
            c.title = if d.title.trim().is_empty() {
                d.nasa_id.clone()
            } else {
                d.title.trim().to_string()
            };
            c.page_url = format!(
                "https://images.nasa.gov/details/{}",
                super::encode(&d.nasa_id)
            );
            c.thumb_url = item
                .links
                .iter()
                .find(|l| l.rel.as_deref() == Some("preview"))
                .or(item.links.first())
                .map(|l| l.href.clone())
                .unwrap_or_default();
            c.author = Some(
                d.photographer
                    .clone()
                    .or(d.secondary_creator.clone())
                    .map(|a| plain(&a))
                    .filter(|a| !a.is_empty())
                    .unwrap_or_else(|| "NASA".into()),
            );
            c.license = "public domain".into();
            c.license_url = Some(GUIDELINES.into());
            c.flag_for(third_party(&d));
            Some((c, item.href))
        })
        .collect()
}

pub(super) fn resolve(id: &str) -> Result<(Candidate, Vec<String>)> {
    let answer: Search = get_json(API, &[("nasa_id", id)], &[])?;
    let (c, collection) = parse(answer)
        .into_iter()
        .find(|(c, _)| c.id == format!("nasa:{id}"))
        .with_context(|| format!("NASA has no item {id:?}"))?;
    let files: Vec<String> = get_json(&https(&collection), &[], &[])?;
    Ok((c.clone(), pick(&files, c.kind)))
}

/// Prefer the 720p medium rendition, then smaller files, before 1080p or an original.
/// The download cap still applies to every fallback.
fn pick(files: &[String], kind: MediaKind) -> Vec<String> {
    let order: &[&str] = match kind {
        MediaKind::Video => &[
            "~medium.mp4",
            "~small.mp4",
            "~mobile.mp4",
            "~large.mp4",
            "~orig.mp4",
            "~orig.mov",
        ],
        MediaKind::Image => &[
            "~large.jpg",
            "~orig.jpg",
            "~orig.jpeg",
            "~orig.png",
            "~medium.jpg",
            "~large.png",
        ],
    };
    let mut out = Vec::new();
    for suffix in order {
        for f in files {
            if f.to_lowercase().ends_with(suffix) && !out.contains(&https(f)) {
                out.push(https(f));
            }
        }
    }
    out
}

/// The library lists `http://` links that redirect to `https://`.
fn https(url: &str) -> String {
    match url.strip_prefix("http://") {
        Some(rest) => format!("https://{rest}"),
        None => url.to_string(),
    }
}

static CREDIT_LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\bcredits?\s*:\s*([^\n<]{1,200})").unwrap());
static PARTS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\s*(?:/|,|;|&|\band\b|\(|\)|\|)\s*").unwrap());

/// Words that mark NASA itself or one of its centres and labs.
const NASA_WORDS: &[&str] = &[
    "nasa",
    "jpl",
    "caltech",
    "gsfc",
    "goddard",
    "jsc",
    "ksc",
    "msfc",
    "arc",
    "grc",
    "larc",
    "afrc",
    "ssc",
    "ames",
    "glenn",
    "langley",
    "marshall",
    "kennedy",
    "johnson",
    "stennis",
    "armstrong",
    "wallops",
    "svs",
];
/// Words that mark an organisation rather than a person's name.
const ORG_WORDS: &[&str] = &[
    "university",
    "institute",
    "agency",
    "inc",
    "corp",
    "corporation",
    "company",
    "lab",
    "laboratory",
    "college",
    "observatory",
    "society",
    "foundation",
    "ltd",
    "llc",
    "press",
    "news",
    "getty",
    "reuters",
    "esa",
    "spacex",
    "boeing",
    "rocket",
    "group",
    "studio",
    "studios",
    "center",
    "centre",
    "museum",
];

/// Why a NASA item may not be NASA's own work: its description mentions copyright, or its
/// credit names someone besides NASA and NASA photographers ("NASA/Bill Ingalls" is fine,
/// "NASA/JPL/University of Arizona" or "Rocket Lab" is not).
fn third_party(d: &Data) -> Option<String> {
    let desc = plain(d.description.as_deref().unwrap_or(""));
    if desc.to_lowercase().contains("copyright") || desc.contains('©') {
        return Some("the NASA description mentions copyright; check who owns it".into());
    }
    let raw = d.description.as_deref().unwrap_or("");
    // NASA fills empty credit fields with placeholders such as "N/A", which would split into
    // the names "N" and "A".
    let empty = |c: &String| {
        matches!(
            c.trim().to_lowercase().as_str(),
            "" | "n/a" | "na" | "none" | "unknown" | "-"
        )
    };
    let credits = [d.photographer.as_deref(), d.secondary_creator.as_deref()]
        .into_iter()
        .flatten()
        .map(plain)
        .chain(CREDIT_LINE.captures_iter(raw).map(|m| plain(&m[1])))
        .filter(|c| !empty(c));
    for credit in credits {
        if let Some(name) = outside_nasa(&credit) {
            return Some(format!(
                "the credit names {name:?}, not only NASA; check who owns it"
            ));
        }
    }
    None
}

/// The first part of a credit that is neither NASA nor a person credited after NASA.
// ponytail: word lists, not a registry of NASA partners; widen NASA_WORDS if real NASA credits
// keep getting flagged.
fn outside_nasa(credit: &str) -> Option<String> {
    let mut after_nasa = false;
    for part in PARTS.split(credit) {
        let part = part
            .trim()
            .trim_matches(|c: char| c == '.' || c == ':')
            .trim();
        if part.is_empty() {
            continue;
        }
        let words: Vec<String> = part
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .map(str::to_lowercase)
            .collect();
        if words.iter().any(|w| NASA_WORDS.contains(&w.as_str())) {
            after_nasa = true;
            continue;
        }
        if after_nasa && is_person(part, &words) {
            continue;
        }
        return Some(part.to_string());
    }
    None
}

/// "Bill Ingalls", "J. Nichols": two to four capitalised words, none an organisation word.
fn is_person(part: &str, words: &[String]) -> bool {
    let names: Vec<&str> = part.split_whitespace().collect();
    (2..=4).contains(&names.len())
        && names.iter().all(|n| {
            n.chars().next().is_some_and(char::is_uppercase)
                && (n.chars().any(char::is_lowercase) || n.ends_with('.'))
        })
        && !words.iter().any(|w| ORG_WORDS.contains(&w.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Excerpt of a live answer (2026-10-29), trimmed.
    const SAMPLE: &str = r#"{"collection":{"version":"1.0","href":"http://images-api.nasa.gov/search?q=aurora","items":[
      {"href":"https://images-assets.nasa.gov/image/KSC-20251111-PH-JBS01_0011/collection.json",
       "data":[{"center":"KSC","title":"Aurora Over Kennedy","nasa_id":"KSC-20251111-PH-JBS01_0011","media_type":"image",
         "photographer":"NASA/Ben Smegelsky","description":"Glowing lights stretch across the sky."}],
       "links":[{"href":"https://images-assets.nasa.gov/image/KSC-20251111-PH-JBS01_0011/KSC-20251111-PH-JBS01_0011~thumb.jpg","rel":"preview","render":"image"}]},
      {"href":"https://images-assets.nasa.gov/image/PIA01637/collection.json",
       "data":[{"center":"JPL","title":"Io Aurorae","nasa_id":"PIA01637","media_type":"image",
         "secondary_creator":"NASA/JPL/University of Arizona","description":"Io aurorae."}],
       "links":[{"href":"https://images-assets.nasa.gov/image/PIA01637/PIA01637~thumb.jpg","rel":"preview","render":"image"}]},
      {"href":"https://images-assets.nasa.gov/image/hubble-auroras/collection.json",
       "data":[{"center":"GSFC","title":"Hubble Captures Vivid Auroras","nasa_id":"hubble-auroras","media_type":"image",
         "description":"Jupiter's auroras.  Credits: NASA, ESA, and J. Nichols (University of Leicester)"}],
       "links":[]},
      {"href":"https://images-assets.nasa.gov/video/KSC-TROPICS/collection.json",
       "data":[{"center":"KSC","title":"TROPICS Rocket Launch","nasa_id":"KSC-TROPICS","media_type":"video",
         "photographer":"Rocket Lab","description":"TROPICS Rocket Launch"}],
       "links":[{"href":"https://images-assets.nasa.gov/video/KSC-TROPICS/KSC-TROPICS~thumb.jpg","rel":"preview","render":"image"}]},
      {"href":"https://images-assets.nasa.gov/audio/x/collection.json",
       "data":[{"title":"Sounds","nasa_id":"x","media_type":"audio"}],"links":[]}
    ],"metadata":{"total_hits":5}}}"#;

    #[test]
    fn parses_search_and_flags_third_party_credits() {
        let list = parse(serde_json::from_str(SAMPLE).unwrap());
        assert_eq!(list.len(), 4, "audio is skipped");
        let (own, collection) = &list[0];
        assert_eq!(own.id, "nasa:KSC-20251111-PH-JBS01_0011");
        assert_eq!(own.kind, MediaKind::Image);
        assert_eq!(
            own.page_url,
            "https://images.nasa.gov/details/KSC-20251111-PH-JBS01_0011"
        );
        assert!(own.thumb_url.ends_with("~thumb.jpg"));
        assert_eq!(own.author.as_deref(), Some("NASA/Ben Smegelsky"));
        assert_eq!(own.license, "public domain");
        assert!(!own.flag, "{:?}", own.flag_reason);
        assert!(collection.ends_with("/collection.json"));

        let flagged: Vec<_> = list[1..]
            .iter()
            .map(|(c, _)| c.flag_reason.clone().unwrap_or_default())
            .collect();
        assert!(flagged[0].contains("University of Arizona"), "{flagged:?}");
        assert!(flagged[1].contains("ESA"), "{flagged:?}");
        assert!(flagged[2].contains("Rocket Lab"), "{flagged:?}");
        assert_eq!(list[3].0.kind, MediaKind::Video);
    }

    #[test]
    fn copyright_in_the_description_is_flagged() {
        let d = Data {
            nasa_id: "a".into(),
            title: String::new(),
            media_type: "image".into(),
            description: Some("Image copyright Jane Doe, used with permission.".into()),
            photographer: None,
            secondary_creator: None,
        };
        assert!(third_party(&d).unwrap().contains("copyright"));
        assert_eq!(outside_nasa("NASA/Joel Kowsky"), None);
        assert_eq!(outside_nasa("NASA/JPL-Caltech"), None);
        assert_eq!(outside_nasa("NASA/JPL/ASU").as_deref(), Some("ASU"));
        assert_eq!(
            outside_nasa("Bill Ingalls").as_deref(),
            Some("Bill Ingalls")
        );
    }

    #[test]
    fn placeholder_credits_are_not_names() {
        let d = Data {
            nasa_id: "a".into(),
            title: String::new(),
            media_type: "video".into(),
            description: Some("Artemis I launches.".into()),
            photographer: Some("NASA/Sam Lott".into()),
            secondary_creator: Some("N/A".into()),
        };
        assert_eq!(third_party(&d), None);
    }

    #[test]
    fn picks_medium_and_smaller_renditions_before_large_or_original() {
        let base = "http://images-assets.nasa.gov/video/K/K";
        let files: Vec<String> = [
            "~orig.mp4",
            ".vtt",
            "~large.mp4",
            "~medium.mp4",
            "~mobile.mp4",
            "~small.mp4",
            "~thumb.jpg",
        ]
        .iter()
        .map(|s| format!("{base}{s}"))
        .collect();
        let picked = pick(&files, MediaKind::Video);
        assert_eq!(
            picked[0],
            "https://images-assets.nasa.gov/video/K/K~medium.mp4"
        );
        assert_eq!(
            picked[1],
            "https://images-assets.nasa.gov/video/K/K~small.mp4"
        );
        assert!(picked[2].ends_with("~mobile.mp4"));
        assert!(picked[3].ends_with("~large.mp4"));
        assert_eq!(
            picked.last().unwrap(),
            "https://images-assets.nasa.gov/video/K/K~orig.mp4"
        );
        let images = pick(
            &[format!("{base}~orig.jpg"), format!("{base}~large.jpg")],
            MediaKind::Image,
        );
        assert_eq!(
            images[0],
            "https://images-assets.nasa.gov/video/K/K~large.jpg"
        );
    }
}
