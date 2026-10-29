//! Pexels photos and videos under the Pexels License. Needs a free key in `PEXELS_API_KEY`.

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use super::{Candidate, MediaKind, Provider, clip_text, get_json};

const LICENSE_URL: &str = "https://www.pexels.com/license/";

pub(super) fn key() -> Option<String> {
    std::env::var("PEXELS_API_KEY")
        .ok()
        .map(|k| k.trim().to_string())
        .filter(|k| !k.is_empty())
}

fn need_key() -> Result<String> {
    key().context(
        "Pexels needs a free key: add PEXELS_API_KEY=<key> to .env (https://www.pexels.com/api/)",
    )
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Photos {
    photos: Vec<Photo>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Photo {
    id: u64,
    width: u32,
    height: u32,
    url: String,
    photographer: Option<String>,
    alt: Option<String>,
    src: PhotoSrc,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct PhotoSrc {
    original: Option<String>,
    large2x: Option<String>,
    medium: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Videos {
    videos: Vec<Video>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Video {
    id: u64,
    width: u32,
    height: u32,
    url: String,
    image: Option<String>,
    duration: f64,
    user: Option<User>,
    video_files: Vec<VideoFile>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct User {
    name: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct VideoFile {
    file_type: String,
    width: u32,
    height: u32,
    link: String,
}

pub(super) fn search(query: &str, kind: Option<MediaKind>, count: usize) -> Result<Vec<Candidate>> {
    let key = need_key()?;
    let auth = [("Authorization", key.as_str())];
    let per = count.to_string();
    let q = [("query", query), ("per_page", per.as_str())];
    let mut out = Vec::new();
    if kind != Some(MediaKind::Video) {
        let p: Photos = get_json("https://api.pexels.com/v1/search", &q, &auth)?;
        out.extend(p.photos.iter().map(|p| photo(p).0));
    }
    if kind != Some(MediaKind::Image) {
        let v: Videos = get_json("https://api.pexels.com/v1/videos/search", &q, &auth)?;
        out.extend(v.videos.iter().map(|v| video(v).0));
    }
    Ok(out)
}

/// Ids are `photo-<n>` or `video-<n>`.
pub(super) fn resolve(id: &str) -> Result<(Candidate, Vec<String>)> {
    let key = need_key()?;
    let auth = [("Authorization", key.as_str())];
    let Some((what, n)) = id.split_once('-').filter(|(_, n)| n.parse::<u64>().is_ok()) else {
        bail!("{id:?} is not a Pexels id; they look like pexels:photo-123 or pexels:video-123");
    };
    match what {
        "photo" => Ok(photo(&get_json(
            &format!("https://api.pexels.com/v1/photos/{n}"),
            &[],
            &auth,
        )?)),
        "video" => Ok(video(&get_json(
            &format!("https://api.pexels.com/v1/videos/videos/{n}"),
            &[],
            &auth,
        )?)),
        _ => {
            bail!("{id:?} is not a Pexels id; they look like pexels:photo-123 or pexels:video-123")
        }
    }
}

fn base(c: &mut Candidate, page: &str, author: Option<String>) {
    c.page_url = page.to_string();
    c.author = author.filter(|a| !a.trim().is_empty());
    c.license = "Pexels License".into();
    c.license_url = Some(LICENSE_URL.into());
}

fn photo(p: &Photo) -> (Candidate, Vec<String>) {
    let mut c = Candidate::new(
        Provider::Pexels,
        &format!("photo-{}", p.id),
        MediaKind::Image,
    );
    base(&mut c, &p.url, p.photographer.clone());
    c.title = clip_text(
        p.alt
            .as_deref()
            .filter(|a| !a.trim().is_empty())
            .unwrap_or(&format!("Pexels photo {}", p.id)),
        120,
    );
    c.thumb_url = p.src.medium.clone().unwrap_or_default();
    c.width = p.width;
    c.height = p.height;
    let files = [&p.src.original, &p.src.large2x]
        .into_iter()
        .flatten()
        .cloned()
        .collect();
    (c, files)
}

fn video(v: &Video) -> (Candidate, Vec<String>) {
    let mut c = Candidate::new(
        Provider::Pexels,
        &format!("video-{}", v.id),
        MediaKind::Video,
    );
    base(&mut c, &v.url, v.user.as_ref().map(|u| u.name.clone()));
    // The page address ends in a readable slug: /video/video-of-forest-1448735/.
    let slug = v.url.trim_end_matches('/').rsplit('/').next().unwrap_or("");
    let words: Vec<&str> = slug
        .split('-')
        .filter(|w| w.parse::<u64>().is_err())
        .collect();
    c.title = if words.is_empty() {
        format!("Pexels video {}", v.id)
    } else {
        clip_text(&words.join(" "), 120)
    };
    c.thumb_url = v.image.clone().unwrap_or_default();
    c.width = v.width;
    c.height = v.height;
    c.duration = v.duration;
    // 720p to 1080p first (by the short side, so portrait clips count too), then smaller.
    let mut files: Vec<&VideoFile> = v
        .video_files
        .iter()
        .filter(|f| f.file_type == "video/mp4")
        .collect();
    files.sort_by_key(|f| {
        let p = i64::from(f.width.min(f.height));
        (!(720..=1080).contains(&p), if p > 1080 { p } else { -p })
    });
    (c, files.into_iter().map(|f| f.link.clone()).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// From the documented examples (pexels.com/api/documentation, 2026-10-29), trimmed.
    const PHOTOS: &str = r##"{"total_results":10000,"page":1,"per_page":1,"photos":[{"id":3573351,"width":3066,"height":3968,
      "url":"https://www.pexels.com/photo/trees-during-day-3573351/","photographer":"Lukas Rodriguez",
      "photographer_url":"https://www.pexels.com/@lukas-rodriguez-1845331","photographer_id":1845331,"avg_color":"#374824",
      "src":{"original":"https://images.pexels.com/photos/3573351/pexels-photo-3573351.png",
        "large2x":"https://images.pexels.com/photos/3573351/pexels-photo-3573351.png?auto=compress&cs=tinysrgb&dpr=2&h=650&w=940",
        "medium":"https://images.pexels.com/photos/3573351/pexels-photo-3573351.png?auto=compress&cs=tinysrgb&h=350"},
      "liked":false,"alt":"Brown Rocks During Golden Hour"}],"next_page":"https://api.pexels.com/v1/search/?page=2&per_page=1&query=nature"}"##;

    const VIDEOS: &str = r#"{"page":1,"per_page":1,"total_results":20475,"url":"https://www.pexels.com/videos/","videos":[{"id":1448735,
      "width":4096,"height":2160,"url":"https://www.pexels.com/video/video-of-forest-1448735/",
      "image":"https://images.pexels.com/videos/1448735/free-video-1448735.jpg","duration":32,
      "user":{"id":574687,"name":"Ruvim Miksanskiy","url":"https://www.pexels.com/@digitech"},
      "video_files":[
        {"id":58649,"quality":"sd","file_type":"video/mp4","width":640,"height":338,"fps":25.0,"link":"https://videos.pexels.com/sd.mp4"},
        {"id":58650,"quality":"uhd","file_type":"video/mp4","width":4096,"height":2160,"fps":25.0,"link":"https://videos.pexels.com/uhd.mp4"},
        {"id":58651,"quality":"hd","file_type":"video/mp4","width":2048,"height":1080,"fps":25.0,"link":"https://videos.pexels.com/hd.mp4"},
        {"id":58652,"quality":"hd","file_type":"video/mp4","width":1366,"height":720,"fps":25.0,"link":"https://videos.pexels.com/720.mp4"}]}]}"#;

    #[test]
    fn parses_photos_and_videos() {
        let p: Photos = serde_json::from_str(PHOTOS).unwrap();
        let (c, files) = photo(&p.photos[0]);
        assert_eq!(c.id, "pexels:photo-3573351");
        assert_eq!(c.title, "Brown Rocks During Golden Hour");
        assert_eq!((c.license.as_str(), c.flag), ("Pexels License", false));
        assert_eq!(c.author.as_deref(), Some("Lukas Rodriguez"));
        assert_eq!(
            files[0],
            "https://images.pexels.com/photos/3573351/pexels-photo-3573351.png"
        );

        let v: Videos = serde_json::from_str(VIDEOS).unwrap();
        let (c, files) = video(&v.videos[0]);
        assert_eq!(c.id, "pexels:video-1448735");
        assert_eq!(c.title, "video of forest");
        assert_eq!(c.duration, 32.0);
        assert_eq!(c.author.as_deref(), Some("Ruvim Miksanskiy"));
        assert_eq!(
            files,
            [
                "https://videos.pexels.com/hd.mp4",
                "https://videos.pexels.com/720.mp4",
                "https://videos.pexels.com/sd.mp4",
                "https://videos.pexels.com/uhd.mp4"
            ]
        );
    }
}
