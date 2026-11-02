//! Local portrait cut-outs. MODNet runs in pure Rust; ffmpeg only decodes and encodes.

mod filter;
mod model;

use crate::ops::{self, Op};
use crate::project::{AssetKind, AssetRole, Clip, Item, ItemBody, Mask, Project, Track, TrackKind};
use crate::{media, workspace::Workspace};
use anyhow::{Context, Result, bail, ensure};
use serde::Serialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::time::Instant;

pub fn status() -> String {
    model::status()
}

/// True only for a locally installed model with the pinned size and SHA-256. No network.
pub fn ready() -> bool {
    model::ready()
}
#[derive(Debug, Serialize)]
pub struct CutoutReport {
    pub mask_asset: String,
    pub overlay_item: String,
    pub frames: usize,
    pub seconds_taken: f64,
}

/// Masks the main-track range and places its silent overlay copy. Saves no project file.
pub fn cutout(
    ws: &Workspace,
    project: &mut Project,
    project_dir: &Path,
    from: f64,
    to: f64,
    outline: f32,
    progress: &mut dyn FnMut(&str),
) -> Result<CutoutReport> {
    let started = Instant::now();
    ensure!(
        (0.0..=0.05).contains(&outline),
        "outline must be between 0 and 0.05 of canvas height"
    );
    let main = covering(project, from, to)?;
    let mut clip = subclip(main, from, to)?;
    let asset = project
        .asset(&clip.asset)
        .context("main clip's asset is missing")?;
    ensure!(
        asset.kind == AssetKind::Video,
        "cutout needs a video on the main track"
    );
    let source = project.asset_path(project_dir, asset);
    let source_from = clip.src_in;
    let source_to = clip.src_out(to - from);
    let (mask_path, frames) = make_mask(&source, project_dir, source_from, source_to, progress)?;
    let mut next = project.clone();
    let mask_asset = if let Some(existing) = next
        .assets
        .iter()
        .find(|a| a.role == AssetRole::Mask && next.asset_path(project_dir, a) == mask_path)
    {
        existing.id.clone()
    } else {
        ws.add_asset(&mut next, project_dir, &mask_path, AssetRole::Mask)?
    };
    clip.mask = Some(Mask {
        asset: mask_asset.clone(),
        offset: source_from,
        outline,
    });
    let overlay_item = next.new_id("cutout");
    let item = Item {
        id: overlay_item.clone(),
        start: from,
        duration: to - from,
        body: ItemBody::Clip(clip),
    };
    let mut track = if let Some(index) = next
        .tracks
        .iter()
        .position(|t| t.kind == TrackKind::Overlay && t.name == "Cut-out")
    {
        next.tracks.remove(index)
    } else {
        Track {
            id: next.new_id("track"),
            kind: TrackKind::Overlay,
            name: "Cut-out".into(),
            muted: false,
            hidden: false,
            items: Vec::new(),
        }
    };
    track.hidden = false;
    track.items.push(item);
    next.tracks.push(track); // Later tracks draw above B-roll, including after a reused track moved.
    ops::apply_all(
        project,
        &[
            Op::Set {
                path: "/assets".into(),
                value: json!(next.assets),
            },
            Op::Set {
                path: "/tracks".into(),
                value: json!(next.tracks),
            },
        ],
    )?;
    Ok(CutoutReport {
        mask_asset,
        overlay_item,
        frames,
        seconds_taken: started.elapsed().as_secs_f64(),
    })
}

fn covering(project: &Project, from: f64, to: f64) -> Result<&Item> {
    ensure!(
        from.is_finite() && to.is_finite() && from >= 0.0 && to > from,
        "cutout needs a non-negative start and a positive duration"
    );
    let (_, item) = project
        .main_at(from)
        .context("cutout starts outside the main track")?;
    ensure!(
        to <= item.end() + 1e-7
            && !project
                .main()
                .items
                .iter()
                .any(|other| other.id != item.id && other.start > from && other.start < to - 1e-7),
        "cutout range crosses a cut; choose a range within one main-track clip"
    );
    Ok(item)
}

fn subclip(main: &Item, from: f64, to: f64) -> Result<Clip> {
    let mut clip = main
        .clip()
        .context("cutout needs a video clip on the main track")?
        .clone();
    let offset = from - main.start;
    clip.src_in += offset * clip.speed;
    clip.muted = true;
    clip.transition = None;
    clip.fade_in = 0.0;
    clip.fade_out = 0.0;
    clip.zooms
        .retain(|z| z.at < to - main.start && z.at + z.duration > offset);
    for zoom in &mut clip.zooms {
        zoom.at -= offset;
    }
    if let Some(kb) = &mut clip.ken_burns {
        kb.retain_phase(offset / main.duration, (to - main.start) / main.duration);
    }
    Ok(clip)
}

// Reap both ffmpeg children on inference, read, write and early-exit errors.
struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn make_mask(
    source: &Path,
    dir: &Path,
    from: f64,
    to: f64,
    progress: &mut dyn FnMut(&str),
) -> Result<(PathBuf, usize)> {
    let source = source.canonicalize().context("finding cutout source")?;
    let meta = fs::metadata(&source)?;
    let info = media::probe(&source)?;
    ensure!(
        info.width > 0 && info.height > 0 && info.fps.is_finite() && info.fps > 0.0,
        "cutout source has no usable video dimensions or frame rate"
    );
    ensure!(
        to <= info.duration + 1.0 / info.fps,
        "cutout source range runs past the video"
    );
    let mut hash = Sha256::new();
    hash.update(source.as_os_str().as_encoded_bytes());
    hash.update(format!(
        "|{}|{:?}|{from:.9}|{to:.9}|{}|guided-v2",
        meta.len(),
        meta.modified()?,
        model::SHA
    ));
    let cache = dir.join(".cache/masks");
    fs::create_dir_all(&cache)?;
    let path = cache.join(format!("{:x}.mkv", hash.finalize()));
    if path.is_file() {
        let cached = media::probe(&path)?;
        ensure!(
            cached.width == info.width
                && cached.height == info.height
                && (cached.fps - info.fps).abs() < 0.001,
            "cached mask metadata is invalid: {}",
            path.display()
        );
        progress("Reusing cached cut-out mask");
        return Ok((path, (cached.duration * cached.fps).round() as usize));
    }
    let mut model = model::Model::load(info.width, info.height, progress)?;
    let part = tempfile::Builder::new()
        .prefix("mask-")
        .suffix(".part")
        .tempfile_in(&cache)?;
    let duration = to - from;
    let mut decoder = Process(
        media::ffmpeg_command()?
            .args(["-ss", &from.to_string(), "-i"])
            .arg(&source)
            .args([
                "-t",
                &duration.to_string(),
                "-map",
                "0:v:0",
                "-an",
                "-r",
                &info.fps.to_string(),
                "-f",
                "rawvideo",
                "-pix_fmt",
                "rgb24",
                "pipe:1",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?,
    );
    let mut encoder = Process(
        media::ffmpeg_command()?
            .args([
                "-y",
                "-f",
                "rawvideo",
                "-pix_fmt",
                "gray",
                "-s",
                &format!("{}x{}", info.width, info.height),
                "-r",
                &info.fps.to_string(),
                "-i",
                "pipe:0",
                "-an",
                "-c:v",
                "ffv1",
                "-level",
                "3",
                "-pix_fmt",
                "gray",
                "-f",
                "matroska",
            ])
            .arg(part.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()?,
    );
    let mut input = decoder
        .0
        .stdout
        .take()
        .context("ffmpeg decoder has no output")?;
    let mut output = encoder
        .0
        .stdin
        .take()
        .context("ffmpeg encoder has no input")?;
    let mut rgb = image::RgbImage::new(info.width, info.height);
    let n = (info.width * info.height) as usize;
    let mut luma = vec![0.0f32; n];
    let mut full = vec![0.0f32; n];
    let mut gray = vec![0u8; n];
    let mut previous = Vec::new();
    let mut mask = Vec::with_capacity((model.width * model.height) as usize);
    let mut guided = filter::Guided::new(info.width as usize, info.height as usize);
    let mut frames = 0;
    loop {
        let bytes: &mut [u8] = rgb.as_mut();
        let first = input.read(&mut bytes[..1])?;
        if first == 0 {
            break;
        }
        input
            .read_exact(&mut bytes[1..])
            .context("ffmpeg returned a partial source frame")?;
        model.infer(&rgb, &mut mask)?;
        filter::smooth(&mut mask, &mut previous);
        for (i, p) in rgb.pixels().enumerate() {
            luma[i] = (0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32) / 255.0;
        }
        upsample(
            &mask,
            model.width as usize,
            model.height as usize,
            &mut full,
            info.width as usize,
            info.height as usize,
        );
        guided.refine(
            &luma,
            &mut full,
            (info.width / model.width).max(1) as usize * 4,
        );
        for (dst, value) in gray.iter_mut().zip(&full) {
            *dst = (value * 255.0).round() as u8;
        }
        output.write_all(&gray)?;
        frames += 1;
        if frames == 1 || frames % 10 == 0 {
            progress(&format!(
                "Cut-out mask: {frames}/{} frames",
                (duration * info.fps).ceil() as usize
            ));
        }
    }
    drop(output);
    ensure!(
        decoder.0.wait()?.success(),
        "ffmpeg could not decode the cutout range"
    );
    ensure!(
        encoder.0.wait()?.success(),
        "ffmpeg could not encode the cutout mask"
    );
    if frames == 0 {
        bail!("cutout range contains no video frames");
    }
    ensure!(
        (frames as f64 / info.fps - duration).abs() <= 1.1 / info.fps,
        "decoded cutout range is shorter than requested"
    );
    part.persist(&path)
        .context("saving completed cutout mask")?;
    Ok((path, frames))
}

fn upsample(src: &[f32], sw: usize, sh: usize, dst: &mut [f32], dw: usize, dh: usize) {
    for y in 0..dh {
        let sy = ((y as f32 + 0.5) * sh as f32 / dh as f32 - 0.5).clamp(0.0, (sh - 1) as f32);
        let y0 = sy as usize;
        let y1 = (y0 + 1).min(sh - 1);
        let fy = sy - y0 as f32;
        for x in 0..dw {
            let sx = ((x as f32 + 0.5) * sw as f32 / dw as f32 - 0.5).clamp(0.0, (sw - 1) as f32);
            let x0 = sx as usize;
            let x1 = (x0 + 1).min(sw - 1);
            let fx = sx - x0 as f32;
            let a = src[y0 * sw + x0] * (1.0 - fx) + src[y0 * sw + x1] * fx;
            let b = src[y1 * sw + x0] * (1.0 - fx) + src[y1 * sw + x1] * fx;
            dst[y * dw + x] = a * (1.0 - fy) + b * fy;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::{Canvas, Fit, KenBurns, Point, VideoKind, Zoom};
    #[test]
    fn rejects_cut_crossings_and_preserves_subrange_framing() {
        let mut p = Project::new("test", VideoKind::Documentary, Canvas::landscape());
        let mut clip = Clip::new("camera", 10.0);
        clip.speed = 2.0;
        clip.fit = Fit::Contain;
        clip.focus = Some(Point { x: 0.3, y: 0.6 });
        clip.zooms.push(Zoom {
            at: 1.0,
            duration: 4.0,
            scale: 1.5,
            focus: None,
            ease: 1.0,
        });
        clip.ken_burns = Some(KenBurns {
            from_scale: 1.0,
            to_scale: 2.0,
            from_focus: None,
            to_focus: Some(Point::CENTER),
            smooth: false,
            phase: [0.0, 1.0],
        });
        p.main_mut().items.push(Item {
            id: "one".into(),
            start: 0.0,
            duration: 8.0,
            body: ItemBody::Clip(clip.clone()),
        });
        p.main_mut().items.push(Item {
            id: "two".into(),
            start: 8.0,
            duration: 2.0,
            body: ItemBody::Clip(clip),
        });
        assert!(
            covering(&p, 7.0, 9.0)
                .unwrap_err()
                .to_string()
                .contains("crosses a cut")
        );
        assert!(covering(&p, 0.0, 8.0).is_ok());
        assert!(covering(&p, f64::NAN, 2.0).is_err());
        for smooth in [false, true] {
            p.main_mut().items[0]
                .clip_mut()
                .unwrap()
                .ken_burns
                .as_mut()
                .unwrap()
                .smooth = smooth;
            let main = covering(&p, 2.0, 4.0).unwrap();
            let cut = subclip(main, 2.0, 4.0).unwrap();
            assert_eq!(cut.asset, "camera");
            assert_eq!(cut.src_in, 14.0);
            assert_eq!(cut.speed, 2.0);
            assert_eq!(cut.fit, Fit::Contain);
            assert_eq!(cut.focus, main.clip().unwrap().focus);
            assert_eq!(cut.zooms[0].at, -1.0);
            assert_eq!(cut.zooms[0].duration, 4.0);
            assert_eq!(cut.ken_burns.as_ref().unwrap().from_scale, 1.0);
            assert_eq!(cut.ken_burns.as_ref().unwrap().to_scale, 2.0);
            assert_eq!(cut.ken_burns.as_ref().unwrap().phase, [0.25, 0.5]);
            for t in [0.0, 0.2, 0.8, 1.9] {
                let rect = crate::project::Rect {
                    x: 0.2,
                    y: 0.2,
                    w: 0.3,
                    h: 0.3,
                };
                let a = crate::render::canvas_rect(&cut, (1280, 720), (1280, 720), t, 2.0, rect);
                let b = crate::render::canvas_rect(
                    main.clip().unwrap(),
                    (1280, 720),
                    (1280, 720),
                    t + 2.0,
                    8.0,
                    rect,
                );
                assert!(
                    [a.x - b.x, a.y - b.y, a.w - b.w, a.h - b.h]
                        .iter()
                        .all(|d| d.abs() < 1e-6)
                );
            }
        }
    }
}
