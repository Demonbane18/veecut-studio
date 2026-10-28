//! Where the face is, so crops and zooms keep the speaker in frame and captions stay off the mouth.
//! Uses rustface (BSD-2) with the SeetaFace frontal model bundled in `assets/models` (BSD-2,
//! VIPL group, ICT, CAS). The model finds frontal faces only: a turned head or a small face in a
//! wide shot leaves a gap in the track, and callers must treat a gap as "not checked".

use std::io::Read;
use std::path::Path;
use std::process::Stdio;

use anyhow::{Context, Result, bail};
use rustface::ImageData;

use crate::media;
use crate::project::{AssetKind, FaceSample, Point};

static MODEL: &[u8] = include_bytes!("../../assets/models/seeta_fd_frontal_v1.0.bin");

/// Frames are scaled down to this width before detection.
const DETECT_WIDTH: u32 = 480;

/// Seconds between detection frames.
const STEP: f64 = 0.5;

/// Most detection frames per recording; longer ones are sampled further apart.
const MAX_FRAMES: f64 = 1200.0;

/// The largest face about every half second of `range` (source seconds), as boxes normalized to
/// the source picture. One ffmpeg pass decodes the range; frames with no face add nothing.
pub fn face_track(path: &Path, range: (f64, f64)) -> Result<Vec<FaceSample>> {
    let info = media::probe(path)?;
    if info.kind != AssetKind::Video || info.width == 0 || info.height == 0 {
        return Ok(Vec::new());
    }
    let start = range.0.max(0.0);
    let end = range.1.min(info.duration);
    if end - start < 0.1 {
        return Ok(Vec::new());
    }
    let step = STEP.max((end - start) / MAX_FRAMES);
    let width = info.width.min(DETECT_WIDTH) & !1;
    let height =
        ((info.height as f64 * width as f64 / info.width as f64).round() as u32).max(2) & !1;

    let model = rustface::read_model(MODEL)
        .context("the bundled face model is damaged; reinstall VeeCut")?;
    let mut detector = rustface::create_detector_with_model(model);
    detector.set_min_face_size(24);
    detector.set_score_thresh(2.0);
    detector.set_pyramid_scale_factor(0.8);
    detector.set_slide_window_step(4, 4);

    let mut cmd = media::ffmpeg_command()?;
    cmd.arg("-ss")
        .arg(format!("{start:.6}"))
        .arg("-t")
        .arg(format!("{:.6}", end - start))
        .arg("-i")
        .arg(path)
        .args([
            "-vf",
            &format!("fps={:.6},scale={width}:{height}:flags=bicubic", 1.0 / step),
            "-an",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "gray",
            "-",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = cmd
        .spawn()
        .with_context(|| format!("starting ffmpeg to find faces in {}", path.display()))?;
    let mut frames = child.stdout.take().context("ffmpeg gave no output pipe")?;

    let (w, h) = (width as f32, height as f32);
    let mut buf = vec![0u8; width as usize * height as usize];
    let mut track = Vec::new();
    let mut k = 0u32;
    while frames.read_exact(&mut buf).is_ok() {
        let t = start + f64::from(k) * step;
        k += 1;
        let faces = detector.detect(&ImageData::new(&buf, width, height));
        let Some(face) = faces
            .iter()
            .max_by_key(|f| f.bbox().width() * f.bbox().height())
        else {
            continue;
        };
        let b = face.bbox();
        let (x0, y0) = ((b.x() as f32).max(0.0), (b.y() as f32).max(0.0));
        let x1 = (b.x() as f32 + b.width() as f32).min(w);
        let y1 = (b.y() as f32 + b.height() as f32).min(h);
        if x1 > x0 && y1 > y0 {
            track.push(FaceSample {
                t: (t * 100.0).round() / 100.0,
                x: round3(x0 / w),
                y: round3(y0 / h),
                w: round3((x1 - x0) / w),
                h: round3((y1 - y0) / h),
            });
        }
    }
    drop(frames);
    let _ = child.kill();
    let _ = child.wait();
    if k == 0 {
        bail!("ffmpeg returned no frames from {}", path.display());
    }
    Ok(track)
}

/// Median face center of a track, where framing crops and punch-ins aim.
pub fn focus(track: &[FaceSample]) -> Option<Point> {
    let mut xs: Vec<f32> = track.iter().map(|s| s.x + s.w / 2.0).collect();
    let mut ys: Vec<f32> = track.iter().map(|s| s.y + s.h / 2.0).collect();
    median(&mut xs)
        .zip(median(&mut ys))
        .map(|(x, y)| Point::new(x, y))
}

fn round3(v: f32) -> f32 {
    (v * 1000.0).round() / 1000.0
}

fn median(v: &mut [f32]) -> Option<f32> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(f32::total_cmp);
    Some(v[v.len() / 2])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(t: f64, x: f32, y: f32, size: f32) -> FaceSample {
        FaceSample {
            t,
            x,
            y,
            w: size,
            h: size,
        }
    }

    #[test]
    fn focus_is_the_median_box_center_and_empty_without_faces() {
        assert_eq!(focus(&[]), None);
        let track = [
            at(0.0, 0.4, 0.2, 0.2),
            at(0.5, 0.4, 0.3, 0.2),
            at(1.0, 0.9, 0.9, 0.1),
        ];
        let f = focus(&track).unwrap();
        assert!((f.x - 0.5).abs() < 1e-6 && (f.y - 0.4).abs() < 1e-6);
    }
}
