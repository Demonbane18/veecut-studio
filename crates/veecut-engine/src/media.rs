//! ffmpeg and ffprobe, the only programs VeeCut runs besides itself. They decode and encode;
//! everything else (cutting, zooms, color, text, captions) happens in Rust, so any stock ffmpeg
//! build works, including ones without libass or drawtext.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::LazyLock;

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

use crate::project::AssetKind;

static FFMPEG: LazyLock<Option<PathBuf>> = LazyLock::new(|| find_tool("VEECUT_FFMPEG", "ffmpeg"));

static FFPROBE: LazyLock<Option<PathBuf>> = LazyLock::new(|| {
    find_tool("VEECUT_FFPROBE", "ffprobe").or_else(|| {
        let ff = std::env::var_os("VEECUT_FFMPEG").map(PathBuf::from)?;
        let probe = ff.with_file_name(format!("ffprobe{}", std::env::consts::EXE_SUFFIX));
        probe.is_file().then_some(probe)
    })
});

/// `ffmpeg` from `VEECUT_FFMPEG` or the PATH.
pub fn ffmpeg() -> Result<PathBuf> {
    FFMPEG.clone().ok_or_else(|| anyhow!("ffmpeg is not installed. Install it (macOS: brew install ffmpeg, Windows: winget install Gyan.FFmpeg, Linux: your package manager), or set VEECUT_FFMPEG to its path."))
}

/// `ffprobe` from `VEECUT_FFPROBE`, next to ffmpeg, or the PATH.
pub fn ffprobe() -> Result<PathBuf> {
    FFPROBE
        .clone()
        .ok_or_else(|| anyhow!("ffprobe is not installed; it ships with ffmpeg"))
}

fn find_tool(env: &str, name: &str) -> Option<PathBuf> {
    if let Some(p) = std::env::var_os(env).map(PathBuf::from) {
        return p.is_file().then_some(p);
    }
    let exe = format!("{name}{}", std::env::consts::EXE_SUFFIX);
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(&exe))
        .find(|p| p.is_file())
}

/// A quiet ffmpeg command: no banner, errors only, never waits for keyboard input.
pub fn ffmpeg_command() -> Result<Command> {
    let mut cmd = Command::new(ffmpeg()?);
    cmd.args(["-hide_banner", "-nostdin", "-loglevel", "error"]);
    Ok(cmd)
}

/// Runs a command to completion and returns stdout. Failures carry the last lines of stderr.
pub fn run(cmd: &mut Command) -> Result<Vec<u8>> {
    let out = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| format!("could not start {:?}", cmd.get_program()))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let tail: Vec<&str> = err.lines().rev().take(6).collect();
        bail!(
            "{} failed: {}",
            Path::new(cmd.get_program())
                .file_name()
                .unwrap_or_default()
                .to_string_lossy(),
            tail.into_iter().rev().collect::<Vec<_>>().join("\n")
        );
    }
    Ok(out.stdout)
}

/// What ffprobe knows about a file. Width and height are as displayed (rotation applied).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MediaInfo {
    pub kind: AssetKind,
    pub duration: f64,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub has_video: bool,
    pub has_audio: bool,
    pub audio_channels: u32,
    pub audio_rate: u32,
}

#[derive(Deserialize)]
struct ProbeJson {
    #[serde(default)]
    streams: Vec<ProbeStream>,
    format: Option<ProbeFormat>,
}

#[derive(Deserialize)]
struct ProbeStream {
    codec_type: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    avg_frame_rate: Option<String>,
    r_frame_rate: Option<String>,
    duration: Option<String>,
    channels: Option<u32>,
    sample_rate: Option<String>,
    #[serde(default)]
    side_data_list: Vec<serde_json::Value>,
    #[serde(default)]
    tags: serde_json::Map<String, serde_json::Value>,
    #[serde(default)]
    disposition: serde_json::Map<String, serde_json::Value>,
}

#[derive(Deserialize)]
struct ProbeFormat {
    duration: Option<String>,
    format_name: Option<String>,
}

fn rate(s: Option<&str>) -> Option<f64> {
    let s = s?;
    let (n, d) = s.split_once('/').unwrap_or((s, "1"));
    let (n, d): (f64, f64) = (n.parse().ok()?, d.parse().ok()?);
    (d > 0.0 && n > 0.0).then(|| n / d)
}

const IMAGE_EXTENSIONS: [&str; 6] = ["png", "jpg", "jpeg", "webp", "bmp", "tiff"];

pub fn probe(path: &Path) -> Result<MediaInfo> {
    if !path.is_file() {
        bail!("{} does not exist", path.display());
    }
    let out = run(Command::new(ffprobe()?)
        .args([
            "-v",
            "error",
            "-print_format",
            "json",
            "-show_format",
            "-show_streams",
        ])
        .arg(path))
    .with_context(|| format!("{} is not a media file ffmpeg can read", path.display()))?;
    let json: ProbeJson = serde_json::from_slice(&out).context("unexpected ffprobe output")?;
    let is_cover =
        |s: &ProbeStream| s.disposition.get("attached_pic").and_then(|v| v.as_i64()) == Some(1);
    let video = json
        .streams
        .iter()
        .find(|s| s.codec_type.as_deref() == Some("video") && !is_cover(s));
    let audio = json
        .streams
        .iter()
        .find(|s| s.codec_type.as_deref() == Some("audio"));
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let format = json.format.as_ref();
    let is_image = IMAGE_EXTENSIONS.contains(&ext.as_str())
        || format
            .and_then(|f| f.format_name.as_deref())
            .is_some_and(|f| f.ends_with("_pipe") || f == "image2");
    let duration = format
        .and_then(|f| f.duration.as_deref())
        .or_else(|| video.and_then(|v| v.duration.as_deref()))
        .or_else(|| audio.and_then(|a| a.duration.as_deref()))
        .and_then(|d| d.parse::<f64>().ok())
        .unwrap_or(0.0);
    let (mut width, mut height) = video
        .map(|v| (v.width.unwrap_or(0), v.height.unwrap_or(0)))
        .unwrap_or((0, 0));
    if let Some(v) = video {
        let side_rotation = v
            .side_data_list
            .iter()
            .find_map(|d| d.get("rotation")?.as_f64());
        let tag_rotation = v
            .tags
            .get("rotate")
            .and_then(|r| r.as_str()?.parse::<f64>().ok());
        if let Some(r) = side_rotation.or(tag_rotation) {
            if (r.abs() as i64) % 180 == 90 {
                std::mem::swap(&mut width, &mut height);
            }
        }
    }
    let kind = if is_image && video.is_some() {
        AssetKind::Image
    } else if video.is_some() {
        AssetKind::Video
    } else if audio.is_some() {
        AssetKind::Audio
    } else {
        bail!("{} has no video or audio", path.display());
    };
    let fps = video
        .and_then(|v| rate(v.avg_frame_rate.as_deref()).or_else(|| rate(v.r_frame_rate.as_deref())))
        .filter(|f| *f <= 240.0)
        .unwrap_or(0.0);
    Ok(MediaInfo {
        kind,
        duration: if kind == AssetKind::Image {
            0.0
        } else {
            duration
        },
        width,
        height,
        fps: if kind == AssetKind::Image { 0.0 } else { fps },
        has_video: video.is_some(),
        has_audio: audio.is_some(),
        audio_channels: audio.and_then(|a| a.channels).unwrap_or(0),
        audio_rate: audio
            .and_then(|a| a.sample_rate.as_deref()?.parse().ok())
            .unwrap_or(0),
    })
}

/// Decodes audio to mono f32 samples at `sample_rate`. `range` is (start, end) in source seconds.
pub fn pcm_mono(path: &Path, sample_rate: u32, range: Option<(f64, f64)>) -> Result<Vec<f32>> {
    let mut cmd = ffmpeg_command()?;
    if let Some((start, _)) = range {
        cmd.arg("-ss").arg(format!("{start:.6}"));
    }
    cmd.arg("-i").arg(path);
    if let Some((start, end)) = range {
        cmd.arg("-t").arg(format!("{:.6}", (end - start).max(0.0)));
    }
    cmd.args([
        "-vn",
        "-ac",
        "1",
        "-ar",
        &sample_rate.to_string(),
        "-f",
        "f32le",
        "-",
    ]);
    let bytes = run(&mut cmd).with_context(|| format!("decoding audio from {}", path.display()))?;
    Ok(bytes
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect())
}

/// One frame of a video or image at source time `t`, scaled down to at most `max_width` pixels
/// wide (aspect kept, even height).
pub fn frame_rgba(path: &Path, t: f64, max_width: u32) -> Result<image::RgbaImage> {
    let info = probe(path)?;
    if info.width == 0 || info.height == 0 {
        bail!("{} has no picture", path.display());
    }
    let width = info.width.min(max_width.max(2)) & !1;
    let height =
        ((info.height as f64 * width as f64 / info.width as f64).round() as u32).max(2) & !1;
    let mut cmd = ffmpeg_command()?;
    if info.kind == AssetKind::Video {
        cmd.arg("-ss").arg(format!(
            "{:.6}",
            t.clamp(0.0, (info.duration - 0.05).max(0.0))
        ));
    }
    cmd.arg("-i").arg(path).args([
        "-frames:v",
        "1",
        "-vf",
        &format!("scale={width}:{height}:flags=bicubic"),
        "-f",
        "rawvideo",
        "-pix_fmt",
        "rgba",
        "-",
    ]);
    let bytes = run(&mut cmd)?;
    image::RgbaImage::from_raw(width, height, bytes)
        .ok_or_else(|| anyhow!("ffmpeg returned a short frame from {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rates_parse() {
        assert_eq!(
            rate(Some("30000/1001")).map(|r| (r * 1000.0).round()),
            Some(29970.0)
        );
        assert_eq!(rate(Some("0/0")), None);
        assert_eq!(rate(Some("25")), Some(25.0));
    }
}
