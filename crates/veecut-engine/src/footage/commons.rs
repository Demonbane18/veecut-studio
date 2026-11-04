//! Wikimedia Commons through the MediaWiki API. Every file has its own licence in
//! `extmetadata`; pictures download as a 2560-pixel rendition, videos as a 720p–1080p
//! transcode when one exists.

use std::collections::HashMap;

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value;

use super::{Candidate, MediaKind, Provider, clip_text, get_json, plain};

const API: &str = "https://commons.wikimedia.org/w/api.php";
const META: &str = "ObjectName|Artist|LicenseShortName|LicenseUrl|License|UsageTerms|NonFree";
const IMAGE_MIMES: [&str; 3] = ["image/jpeg", "image/png", "image/webp"];

#[derive(Deserialize)]
struct Answer {
    query: Option<Query>,
    /// The API reports problems in the body with HTTP 200, for example "search is too busy".
    error: Option<ApiError>,
}

#[derive(Deserialize)]
struct ApiError {
    #[serde(default)]
    code: String,
    #[serde(default)]
    info: String,
}

#[derive(Deserialize)]
struct Query {
    #[serde(default)]
    pages: Vec<Page>,
}

#[derive(Deserialize)]
struct Page {
    pageid: Option<u64>,
    #[serde(default)]
    title: String,
    /// Search rank; pages come back unordered.
    #[serde(default)]
    index: u32,
    #[serde(default)]
    imageinfo: Vec<Info>,
    #[serde(default)]
    videoinfo: Vec<VideoInfo>,
}

#[derive(Deserialize)]
struct Info {
    #[serde(default)]
    url: String,
    #[serde(default)]
    descriptionurl: String,
    thumburl: Option<String>,
    #[serde(default)]
    width: u32,
    #[serde(default)]
    height: u32,
    #[serde(default)]
    mime: String,
    duration: Option<f64>,
    #[serde(default)]
    extmetadata: HashMap<String, Meta>,
}

#[derive(Deserialize)]
struct Meta {
    value: Value,
}

#[derive(Deserialize)]
struct VideoInfo {
    #[serde(default)]
    derivatives: Vec<Derivative>,
}

#[derive(Deserialize)]
struct Derivative {
    src: String,
    #[serde(default)]
    height: u32,
    transcodekey: Option<String>,
}

fn base(width: &str) -> Vec<(&str, &str)> {
    vec![
        ("action", "query"),
        ("format", "json"),
        ("formatversion", "2"),
        ("prop", "imageinfo|videoinfo"),
        ("iiprop", "url|size|mime|extmetadata"),
        ("iiextmetadatafilter", META),
        ("iiurlwidth", width),
        ("viprop", "derivatives"),
    ]
}

pub(super) fn search(query: &str, kind: Option<MediaKind>, count: usize) -> Result<Vec<Candidate>> {
    let kinds = match kind {
        Some(k) => vec![k],
        None => vec![MediaKind::Image, MediaKind::Video],
    };
    let mut out = Vec::new();
    for k in &kinds {
        let filter = match k {
            MediaKind::Image => "filetype:bitmap",
            MediaKind::Video => "filetype:video",
        };
        let text = format!("{query} {filter}");
        let limit = count.div_ceil(kinds.len()).max(1).to_string();
        let mut params = base("330");
        params.extend([
            ("generator", "search"),
            ("gsrsearch", text.as_str()),
            ("gsrnamespace", "6"),
            ("gsrlimit", limit.as_str()),
        ]);
        let answer = ask(&params)?;
        out.extend(parse(answer).into_iter().map(|(c, _)| c));
    }
    Ok(out)
}

pub(super) fn resolve(id: &str) -> Result<(Candidate, Vec<String>)> {
    let mut params = base("2560");
    params.push(("pageids", id));
    let answer = ask(&params)?;
    parse(answer)
        .into_iter()
        .next()
        .with_context(|| format!("Wikimedia Commons has no usable file with page id {id}"))
}

/// One API call; a busy search is retried once after a short pause.
fn ask(params: &[(&str, &str)]) -> Result<Answer> {
    let mut answer: Answer = get_json(API, params, &[])?;
    if answer
        .error
        .as_ref()
        .is_some_and(|e| e.code == "cirrussearch-too-busy-error")
    {
        std::thread::sleep(std::time::Duration::from_millis(1500));
        answer = get_json(API, params, &[])?;
    }
    match answer.error {
        Some(e) => anyhow::bail!("Wikimedia Commons said: {} ({})", e.info, e.code),
        None => Ok(answer),
    }
}

/// Candidates in search order, with their downloads best first.
fn parse(answer: Answer) -> Vec<(Candidate, Vec<String>)> {
    let mut pages = answer.query.map(|q| q.pages).unwrap_or_default();
    pages.sort_by_key(|p| p.index);
    pages.into_iter().filter_map(candidate).collect()
}

fn candidate(page: Page) -> Option<(Candidate, Vec<String>)> {
    let id = page.pageid?;
    let info = page.imageinfo.into_iter().next()?;
    let kind = if IMAGE_MIMES.contains(&info.mime.as_str()) {
        MediaKind::Image
    } else if info.mime.starts_with("video/") || info.mime == "application/ogg" {
        MediaKind::Video
    } else {
        return None;
    };
    let meta = |key: &str| {
        info.extmetadata
            .get(key)
            .map(|m| match &m.value {
                Value::String(s) => plain(s),
                other => other.to_string(),
            })
            .unwrap_or_default()
    };
    let mut c = Candidate::new(Provider::Commons, &id.to_string(), kind);
    let name = page.title.trim_start_matches("File:");
    let stem = name.rsplit_once('.').map_or(name, |(s, _)| s);
    c.title = clip_text(
        &Some(meta("ObjectName"))
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| stem.to_string()),
        120,
    );
    c.page_url = info.descriptionurl.clone();
    c.thumb_url = info.thumburl.clone().unwrap_or_default();
    c.width = info.width;
    c.height = info.height;
    c.duration = info.duration.unwrap_or(0.0);
    c.author = Some(clip_text(&meta("Artist"), 120)).filter(|a| !a.is_empty());
    let url = meta("LicenseUrl");
    c.license_url = Some(url.clone()).filter(|u| !u.is_empty());
    let nonfree = if meta("NonFree") == "true" {
        " nonfree"
    } else {
        ""
    };
    c.set_rights(&format!(
        "{} {} {url}{nonfree}",
        meta("LicenseShortName"),
        meta("License")
    ));
    let downloads = match kind {
        // Above 2560 pixels wide `thumburl` is a 2560 rendition; below it is the file itself.
        MediaKind::Image => info.thumburl.iter().chain([&info.url]).cloned().collect(),
        MediaKind::Video => {
            let transcode = |d: &&Derivative| {
                d.transcodekey
                    .as_deref()
                    .is_some_and(|k| k.ends_with("vp9.webm"))
            };
            let mut good: Vec<&Derivative> = page
                .videoinfo
                .iter()
                .flat_map(|v| &v.derivatives)
                .filter(transcode)
                .collect();
            good.sort_by_key(|d| {
                (
                    !(720..=1080).contains(&d.height),
                    if d.height < 720 {
                        1080 - d.height
                    } else {
                        d.height
                    },
                )
            });
            let (hd, low): (Vec<_>, Vec<_>) = good
                .into_iter()
                .partition(|d| d.height >= 720 && d.height <= 1080);
            let mut list: Vec<String> = hd.iter().map(|d| d.src.clone()).collect();
            list.extend(low.iter().filter(|d| d.height < 720).map(|d| d.src.clone()));
            list.push(info.url.clone());
            list
        }
    };
    Some((c, downloads))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Excerpt of live answers (2026-10-29), trimmed.
    const SAMPLE: &str = r#"{"batchcomplete":true,"query":{"pages":[
      {"pageid":151478344,"ns":6,"title":"File:Aurora and perseids.jpg","index":2,"imagerepository":"local",
       "imageinfo":[{"size":23554936,"width":5978,"height":3977,
         "thumburl":"https://upload.wikimedia.org/wikipedia/commons/thumb/3/31/Aurora_and_perseids.jpg/330px-Aurora_and_perseids.jpg",
         "url":"https://upload.wikimedia.org/wikipedia/commons/3/31/Aurora_and_perseids.jpg",
         "descriptionurl":"https://commons.wikimedia.org/wiki/File:Aurora_and_perseids.jpg","mime":"image/jpeg",
         "extmetadata":{"ObjectName":{"value":"Aurora and perseids","source":"mediawiki-metadata"},
           "Artist":{"value":"<a href=\"//commons.wikimedia.org/wiki/User:Stephan_Sprinz\" title=\"User:Stephan Sprinz\">Stephan Sprinz</a>","source":"commons-desc-page"},
           "LicenseShortName":{"value":"CC BY 4.0"},"LicenseUrl":{"value":"https://creativecommons.org/licenses/by/4.0"},
           "License":{"value":"cc-by-4.0"}}}]},
      {"pageid":1234235,"ns":6,"title":"File:Aurora borealis over Eielson Air Force Base, Alaska.jpg","index":1,
       "imageinfo":[{"width":3008,"height":1960,"thumburl":"https://upload.wikimedia.org/t.jpg","url":"https://upload.wikimedia.org/o.jpg",
         "descriptionurl":"https://commons.wikimedia.org/wiki/File:Aurora_borealis_over_Eielson_Air_Force_Base,_Alaska.jpg","mime":"image/jpeg",
         "extmetadata":{"Artist":{"value":"<a href=\"//commons.wikimedia.org/wiki/United_States_Air_Force\">United States Air Force</a> photo by Senior Airman Joshua Strang"},
           "LicenseShortName":{"value":"Public domain"},"License":{"value":"pd"}}}]},
      {"pageid":5,"title":"File:Logo.jpg","index":3,
       "imageinfo":[{"width":300,"height":300,"url":"https://upload.wikimedia.org/l.jpg","descriptionurl":"https://commons.wikimedia.org/wiki/File:Logo.jpg","mime":"image/jpeg",
         "extmetadata":{"LicenseShortName":{"value":"Fair use"},"NonFree":{"value":"true"}}}]},
      {"pageid":7,"title":"File:Diagram.svg","index":4,"imageinfo":[{"mime":"image/svg+xml","url":"x","descriptionurl":"y"}]},
      {"pageid":9,"title":"File:Launch Replays of the Antares 230 Rocket with Cygnus.webm","index":5,
       "imageinfo":[{"width":1920,"height":1080,"duration":437.741,"thumburl":"https://upload.wikimedia.org/v.jpg",
         "url":"https://upload.wikimedia.org/wikipedia/commons/a/ab/Launch.webm","descriptionurl":"https://commons.wikimedia.org/wiki/File:Launch.webm","mime":"video/webm",
         "extmetadata":{"LicenseShortName":{"value":"Public domain"},"Artist":{"value":"NASA"}}}],
       "videoinfo":[{"derivatives":[
         {"src":"https://upload.wikimedia.org/wikipedia/commons/a/ab/Launch.webm","type":"video/webm","width":1920,"height":1080},
         {"src":"https://upload.wikimedia.org/t/Launch.webm.240p.vp9.webm","type":"video/webm","width":426,"height":240,"transcodekey":"240p.vp9.webm"},
         {"src":"https://upload.wikimedia.org/t/Launch.webm.360p.mpeg4.mov","type":"video/quicktime","width":640,"height":360,"transcodekey":"360p.mpeg4.mov"},
         {"src":"https://upload.wikimedia.org/t/Launch.webm.1080p.vp9.webm","type":"video/webm","width":1920,"height":1080,"transcodekey":"1080p.vp9.webm"},
         {"src":"https://upload.wikimedia.org/t/Launch.webm.720p.vp9.webm","type":"video/webm","width":1280,"height":720,"transcodekey":"720p.vp9.webm"}]}]}
    ]}}"#;

    #[test]
    fn parses_search_in_rank_order_with_licences() {
        let list = parse(serde_json::from_str(SAMPLE).unwrap());
        let ids: Vec<_> = list.iter().map(|(c, _)| c.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "commons:1234235",
                "commons:151478344",
                "commons:5",
                "commons:9"
            ],
            "svg skipped"
        );

        let (pd, pd_files) = &list[0];
        assert_eq!(pd.license, "public domain");
        assert!(!pd.flag);
        assert_eq!(
            pd.title, "Aurora borealis over Eielson Air Force Base, Alaska",
            "file name without extension"
        );
        assert_eq!(
            pd.author.as_deref(),
            Some("United States Air Force photo by Senior Airman Joshua Strang")
        );
        assert_eq!(
            pd_files,
            &[
                "https://upload.wikimedia.org/t.jpg",
                "https://upload.wikimedia.org/o.jpg"
            ]
        );

        let (by, _) = &list[1];
        assert_eq!((by.license.as_str(), by.flag), ("CC BY 4.0", false));
        assert_eq!(by.author.as_deref(), Some("Stephan Sprinz"));
        assert_eq!(
            by.license_url.as_deref(),
            Some("https://creativecommons.org/licenses/by/4.0")
        );
        assert_eq!((by.width, by.height), (5978, 3977));

        let (fair, _) = &list[2];
        assert!(fair.flag && fair.license == "fair use");

        let (video, files) = &list[3];
        assert_eq!(video.kind, MediaKind::Video);
        assert!((video.duration - 437.741).abs() < 1e-9);
        assert_eq!(
            files,
            &[
                "https://upload.wikimedia.org/t/Launch.webm.720p.vp9.webm",
                "https://upload.wikimedia.org/t/Launch.webm.1080p.vp9.webm",
                "https://upload.wikimedia.org/t/Launch.webm.240p.vp9.webm",
                "https://upload.wikimedia.org/wikipedia/commons/a/ab/Launch.webm",
            ]
        );
    }
}
