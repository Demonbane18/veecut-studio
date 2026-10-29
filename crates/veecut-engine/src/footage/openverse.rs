//! Openverse (`api.openverse.org`): openly licensed pictures gathered from Flickr, museums and
//! other sites. Anonymous use is limited to 20 results a page and a small hourly quota.

use anyhow::Result;
use serde::Deserialize;

use super::{Candidate, MediaKind, Provider, clip_text, encode, get_json, plain};

const API: &str = "https://api.openverse.org/v1/images/";

#[derive(Deserialize)]
struct Page {
    #[serde(default)]
    results: Vec<Image>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Image {
    id: String,
    title: Option<String>,
    foreign_landing_url: Option<String>,
    url: Option<String>,
    thumbnail: Option<String>,
    creator: Option<String>,
    license: String,
    license_version: Option<String>,
    license_url: Option<String>,
    filetype: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
}

pub(super) fn search(query: &str, kind: Option<MediaKind>, count: usize) -> Result<Vec<Candidate>> {
    if kind == Some(MediaKind::Video) {
        return Ok(Vec::new());
    }
    let size = count.min(20).to_string();
    let page: Page = get_json(
        API,
        &[
            ("q", query),
            ("page_size", size.as_str()),
            ("mature", "false"),
        ],
        &[],
    )?;
    Ok(page
        .results
        .iter()
        .filter_map(|i| candidate(i).map(|(c, _)| c))
        .collect())
}

pub(super) fn resolve(id: &str) -> Result<(Candidate, Vec<String>)> {
    let image: Image = get_json(&format!("{API}{}/", encode(id)), &[], &[])?;
    candidate(&image).ok_or_else(|| {
        anyhow::anyhow!("Openverse item {id} is not a jpg, png or webp picture; pick another")
    })
}

fn candidate(i: &Image) -> Option<(Candidate, Vec<String>)> {
    let url = i.url.clone().filter(|u| !u.is_empty())?;
    // Unknown file types are usually jpg; svg and gif cannot be used as pictures.
    let ext = i
        .filetype
        .clone()
        .unwrap_or_else(|| "jpg".into())
        .to_lowercase();
    if !["jpg", "jpeg", "png", "webp"].contains(&ext.as_str()) || i.id.is_empty() {
        return None;
    }
    let mut c = Candidate::new(Provider::Openverse, &i.id, MediaKind::Image);
    c.title = clip_text(&plain(i.title.as_deref().unwrap_or("Untitled")), 120);
    c.page_url = i
        .foreign_landing_url
        .clone()
        .unwrap_or_else(|| format!("https://openverse.org/image/{}", i.id));
    c.thumb_url = i.thumbnail.clone().unwrap_or_else(|| url.clone());
    c.width = i.width.unwrap_or(0);
    c.height = i.height.unwrap_or(0);
    c.author = i.creator.as_deref().map(plain).filter(|a| !a.is_empty());
    c.license_url = i.license_url.clone();
    // Openverse writes CC licences as codes: "by", "by-sa", "cc0", "pdm", "by-nc"...
    c.set_rights(&format!(
        "cc {} {} {}",
        i.license,
        i.license_version.as_deref().unwrap_or(""),
        i.license_url.as_deref().unwrap_or("")
    ));
    if i.license.trim().is_empty() {
        c.set_rights("");
    }
    Some((c, vec![url]))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The documented answer shape (api.openverse.org/v1/images/), trimmed.
    const SAMPLE: &str = r#"{"result_count":2,"page_count":1,"page_size":20,"page":1,"results":[
      {"id":"4bc43a04-ef46-4544-a0c1-63c63f56e276","title":"Dripping tap","indexed_on":"2020-04-19T00:00:00Z",
       "foreign_landing_url":"https://www.flickr.com/photos/someone/1234/","url":"https://live.staticflickr.com/1/1234_b.jpg",
       "creator":"Jane Doe","creator_url":"https://www.flickr.com/photos/someone","license":"by","license_version":"2.0",
       "license_url":"https://creativecommons.org/licenses/by/2.0/","provider":"flickr","source":"flickr","category":"photograph",
       "filesize":null,"filetype":"jpg","tags":[],"attribution":"\"Dripping tap\" by Jane Doe is licensed under CC BY 2.0.",
       "mature":false,"height":683,"width":1024,"thumbnail":"https://api.openverse.org/v1/images/4bc43a04-ef46-4544-a0c1-63c63f56e276/thumb/"},
      {"id":"aa","title":"Tap","foreign_landing_url":"https://example.org/tap","url":"https://example.org/tap.jpg","creator":"X",
       "license":"by-nc-sa","license_version":"4.0","license_url":"https://creativecommons.org/licenses/by-nc-sa/4.0/","filetype":null},
      {"id":"bb","title":"Drawing","url":"https://example.org/d.svg","license":"cc0","license_version":"1.0","filetype":"svg"},
      {"id":"cc","title":"Public","url":"https://example.org/p.png","license":"pdm","license_version":"1.0","filetype":"png"}
    ],"warnings":[]}"#;

    #[test]
    fn parses_results_and_licence_codes() {
        let page: Page = serde_json::from_str(SAMPLE).unwrap();
        let list: Vec<_> = page.results.iter().filter_map(candidate).collect();
        assert_eq!(list.len(), 3, "svg is skipped");
        let (by, files) = &list[0];
        assert_eq!(by.id, "openverse:4bc43a04-ef46-4544-a0c1-63c63f56e276");
        assert_eq!((by.license.as_str(), by.flag), ("CC BY 2.0", false));
        assert_eq!(by.page_url, "https://www.flickr.com/photos/someone/1234/");
        assert_eq!(by.author.as_deref(), Some("Jane Doe"));
        assert_eq!(files, &["https://live.staticflickr.com/1/1234_b.jpg"]);
        assert!(by.thumb_url.ends_with("/thumb/"));
        assert!(list[1].0.flag && list[1].0.license == "CC BY-NC-SA 4.0");
        assert_eq!(
            (list[2].0.license.as_str(), list[2].0.flag),
            ("public domain", false)
        );
    }
}
