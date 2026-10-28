//! One output frame: background, main track, overlays, look, graphics.

use std::path::Path;

use anyhow::{Result, anyhow, bail};
use tiny_skia::{Color, Pixmap};

use crate::graphics::Graphics;
use crate::project::{Asset, Clip, Item, Look, Point, Project, Rect, TrackKind, TransitionKind};

use super::decode::{self, DecoderPool, FrameReq};
use super::math::{self, channel_curves, color_unchanged, separable};

pub(crate) struct Grade {
    skip: bool,
    curves: Option<[[u8; 256]; 3]>,
    look: Look,
    vignette: f32,
}

impl Grade {
    pub fn new(look: &Look) -> Self {
        let skip = color_unchanged(look) && look.vignette <= 0.0;
        let curves = if skip || !separable(look) {
            None
        } else {
            Some(channel_curves(look))
        };
        Grade {
            skip,
            curves,
            look: look.clone(),
            vignette: look.vignette.max(0.0),
        }
    }
}

pub(crate) struct Scratch {
    pub a: Vec<u8>,
    pub b: Vec<u8>,
}

impl Scratch {
    pub fn new() -> Self {
        Scratch {
            a: Vec::new(),
            b: Vec::new(),
        }
    }
}

pub(crate) fn render_at(
    project: &Project,
    project_dir: &Path,
    t: f64,
    width: u32,
    height: u32,
    pool: &mut DecoderPool,
    scratch: &mut Scratch,
    gfx: &mut Graphics,
    grade: &Grade,
) -> Result<Pixmap> {
    if width < 2 || height < 2 {
        bail!("render size {width}x{height} is too small; use at least 2x2");
    }
    let mut pixmap = Pixmap::new(width, height)
        .ok_or_else(|| anyhow!("cannot allocate a {width}x{height} frame"))?;
    let bg = project.canvas.background;
    pixmap.fill(Color::from_rgba8(bg.r, bg.g, bg.b, 255));

    if let Some(main) = project.tracks.iter().find(|tr| tr.kind == TrackKind::Main) {
        if !main.hidden {
            draw_main(&mut pixmap, project, project_dir, t, pool, scratch)?;
        }
    }
    for track in &project.tracks {
        if track.hidden || track.kind != TrackKind::Overlay {
            continue;
        }
        for item in &track.items {
            if !item.contains(t) {
                continue;
            }
            let Some(clip) = item.clip() else { continue };
            draw_overlay(
                &mut pixmap,
                project,
                project_dir,
                item,
                clip,
                t,
                pool,
                scratch,
            )?;
        }
    }
    let (black, white) = dip_flash(project, t);
    if black > 0.001 || white > 0.001 {
        apply_dip_flash(pixmap.data_mut(), black, white);
    }
    apply_grade(pixmap.data_mut(), width, height, grade);
    gfx.draw(project, t, &mut pixmap.as_mut());
    pool.end_frame();
    Ok(pixmap)
}

fn draw_main(
    pixmap: &mut Pixmap,
    project: &Project,
    project_dir: &Path,
    t: f64,
    pool: &mut DecoderPool,
    scratch: &mut Scratch,
) -> Result<()> {
    let main = project.tracks.iter().find(|tr| tr.kind == TrackKind::Main);
    let Some(main) = main else { return Ok(()) };
    let visible: Vec<&Item> = main
        .items
        .iter()
        .filter(|i| i.contains(t) && i.clip().is_some())
        .collect();
    if visible.is_empty() {
        return Ok(());
    }
    let w = pixmap.width();
    let h = pixmap.height();
    if visible.len() == 1 {
        let item = visible[0];
        let clip = item.clip().unwrap();
        let gain = fade_gain(item, clip, t) * clip.opacity.clamp(0.0, 1.0);
        fill_clip(
            project,
            project_dir,
            item,
            clip,
            t,
            w,
            h,
            pool,
            &mut scratch.a,
        )?;
        replace_weighted(pixmap.data_mut(), &scratch.a, gain);
        return Ok(());
    }
    let outgoing = visible[0];
    let incoming = visible[visible.len() - 1];
    let out_clip = outgoing.clip().unwrap();
    let in_clip = incoming.clip().unwrap();
    let overlap = (outgoing.end() - incoming.start).max(1e-6);
    let in_w = math::crossfade_incoming(t, incoming.start, overlap);
    let out_w = 1.0 - in_w;
    let out_gain = fade_gain(outgoing, out_clip, t) * out_clip.opacity.clamp(0.0, 1.0) * out_w;
    let in_gain = fade_gain(incoming, in_clip, t) * in_clip.opacity.clamp(0.0, 1.0) * in_w;
    fill_clip(
        project,
        project_dir,
        outgoing,
        out_clip,
        t,
        w,
        h,
        pool,
        &mut scratch.a,
    )?;
    fill_clip(
        project,
        project_dir,
        incoming,
        in_clip,
        t,
        w,
        h,
        pool,
        &mut scratch.b,
    )?;
    mix2(pixmap.data_mut(), &scratch.a, out_gain, &scratch.b, in_gain);
    Ok(())
}

fn draw_overlay(
    pixmap: &mut Pixmap,
    project: &Project,
    project_dir: &Path,
    item: &Item,
    clip: &Clip,
    t: f64,
    pool: &mut DecoderPool,
    scratch: &mut Scratch,
) -> Result<()> {
    let (x, y, w, h) = frame_px(clip.frame, pixmap.width(), pixmap.height());
    if w < 1 || h < 1 {
        return Ok(());
    }
    fill_clip(
        project,
        project_dir,
        item,
        clip,
        t,
        w,
        h,
        pool,
        &mut scratch.a,
    )?;
    let gain = fade_gain(item, clip, t) * clip.opacity.clamp(0.0, 1.0);
    if gain <= 0.001 {
        return Ok(());
    }
    let radius = clip.radius * pixmap.width() as f32;
    let cw = pixmap.width();
    let ch = pixmap.height();
    blit(
        pixmap.data_mut(),
        cw,
        ch,
        &scratch.a,
        w,
        h,
        x,
        y,
        gain,
        radius,
    );
    Ok(())
}

fn fill_clip(
    project: &Project,
    project_dir: &Path,
    item: &Item,
    clip: &Clip,
    t: f64,
    dest_w: u32,
    dest_h: u32,
    pool: &mut DecoderPool,
    dest: &mut Vec<u8>,
) -> Result<()> {
    let (asset, path) = clip_source(project, project_dir, clip)?;
    let (src_w, src_h) = if asset.width > 0 && asset.height > 0 {
        (asset.width, asset.height)
    } else {
        pool.probe_size(&path)?
    };
    let local = (t - item.start).clamp(0.0, item.duration);
    let (scale, focus) = math::view_at(clip, local, item.duration);
    let (dec_w, dec_h) = math::decode_size(clip, dest_w, dest_h);
    let bg = project.canvas.background;
    let req = FrameReq {
        id: item.id.clone(),
        path,
        image: decode::is_image(asset.kind),
        src_w,
        src_h,
        src_t: clip.src_in + local * clip.speed,
        src_out: clip.src_out(item.duration),
        speed: clip.speed.max(0.01),
        fps: project.canvas.fps.max(1.0),
        fit: clip.fit,
        focus: clip.focus.unwrap_or(Point::CENTER),
        dec_w,
        dec_h,
        pad: [bg.r, bg.g, bg.b],
    };
    let _opaque = pool.fill(&req, scale, focus, dest_w, dest_h, dest)?;
    Ok(())
}

fn clip_source<'a>(
    project: &'a Project,
    dir: &Path,
    clip: &Clip,
) -> Result<(&'a Asset, std::path::PathBuf)> {
    let asset = project.asset(&clip.asset).ok_or_else(|| {
        anyhow!(
            "clip uses asset {:?}, which the project does not have",
            clip.asset
        )
    })?;
    let path = project.asset_path(dir, asset);
    if !path.is_file() {
        bail!("{} does not exist (asset {})", path.display(), asset.id);
    }
    Ok((asset, path))
}

fn fade_gain(item: &Item, clip: &Clip, t: f64) -> f32 {
    math::fade_gain(t - item.start, item.duration, clip.fade_in, clip.fade_out)
}

fn frame_px(frame: Option<Rect>, canvas_w: u32, canvas_h: u32) -> (i32, i32, u32, u32) {
    let r = frame.unwrap_or(Rect::FULL);
    let x = (r.x * canvas_w as f32).round() as i32;
    let y = (r.y * canvas_h as f32).round() as i32;
    let w = (r.w * canvas_w as f32).round().max(1.0) as u32;
    let h = (r.h * canvas_h as f32).round().max(1.0) as u32;
    (x, y, w, h)
}

fn dip_flash(project: &Project, t: f64) -> (f32, f32) {
    let Some(main) = project.tracks.iter().find(|tr| tr.kind == TrackKind::Main) else {
        return (0.0, 0.0);
    };
    if main.hidden {
        return (0.0, 0.0);
    }
    let mut black = 0.0f32;
    let mut white = 0.0f32;
    for item in &main.items {
        let Some(clip) = item.clip() else { continue };
        let Some(tr) = &clip.transition else { continue };
        let peak = math::peak_at_cut(t, item.start, tr.duration);
        match tr.kind {
            TransitionKind::Dip => black = black.max(peak),
            TransitionKind::Flash => white = white.max(peak),
            TransitionKind::Crossfade => {}
        }
    }
    (black, white)
}

fn replace_weighted(dst: &mut [u8], src: &[u8], gain: f32) {
    let gain = gain.clamp(0.0, 1.0);
    if gain >= 0.999 && src.len() >= dst.len() {
        let n = dst.len() / 4 * 4;
        dst[..n].copy_from_slice(&src[..n]);
        for px in dst[..n].chunks_exact_mut(4) {
            px[3] = 255;
        }
        return;
    }
    let w = (gain * 256.0).round() as u16;
    for (d, s) in dst.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
        d[0] = ((s[0] as u16 * w) >> 8) as u8;
        d[1] = ((s[1] as u16 * w) >> 8) as u8;
        d[2] = ((s[2] as u16 * w) >> 8) as u8;
        d[3] = 255;
    }
}

fn mix2(dst: &mut [u8], a: &[u8], wa: f32, b: &[u8], wb: f32) {
    let wa = (wa.clamp(0.0, 1.0) * 256.0).round() as u16;
    let wb = (wb.clamp(0.0, 1.0) * 256.0).round() as u16;
    for ((d, pa), pb) in dst
        .chunks_exact_mut(4)
        .zip(a.chunks_exact(4))
        .zip(b.chunks_exact(4))
    {
        d[0] = ((pa[0] as u16 * wa + pb[0] as u16 * wb) >> 8).min(255) as u8;
        d[1] = ((pa[1] as u16 * wa + pb[1] as u16 * wb) >> 8).min(255) as u8;
        d[2] = ((pa[2] as u16 * wa + pb[2] as u16 * wb) >> 8).min(255) as u8;
        d[3] = 255;
    }
}

fn blit(
    dst: &mut [u8],
    dst_w: u32,
    dst_h: u32,
    src: &[u8],
    sw: u32,
    sh: u32,
    x: i32,
    y: i32,
    opacity: f32,
    radius: f32,
) {
    let opacity = opacity.clamp(0.0, 1.0);
    if opacity <= 0.0 || sw == 0 || sh == 0 {
        return;
    }
    let r = radius.max(0.0).min(sw as f32 * 0.5).min(sh as f32 * 0.5);
    for row in 0..sh {
        let dy = y + row as i32;
        if dy < 0 || dy >= dst_h as i32 {
            continue;
        }
        for col in 0..sw {
            let dx = x + col as i32;
            if dx < 0 || dx >= dst_w as i32 {
                continue;
            }
            let si = (row as usize * sw as usize + col as usize) * 4;
            if si + 3 >= src.len() {
                continue;
            }
            let mut a = opacity * (src[si + 3] as f32 / 255.0);
            if r >= 0.5 {
                a *= corner_coverage(col, row, sw, sh, r);
            }
            if a <= 0.001 {
                continue;
            }
            let di = (dy as usize * dst_w as usize + dx as usize) * 4;
            let w = (a * 256.0).round() as u16;
            let iw = 256 - w;
            dst[di] = ((src[si] as u16 * w + dst[di] as u16 * iw) >> 8) as u8;
            dst[di + 1] = ((src[si + 1] as u16 * w + dst[di + 1] as u16 * iw) >> 8) as u8;
            dst[di + 2] = ((src[si + 2] as u16 * w + dst[di + 2] as u16 * iw) >> 8) as u8;
            dst[di + 3] = 255;
        }
    }
}

fn corner_coverage(x: u32, y: u32, w: u32, h: u32, radius: f32) -> f32 {
    let px = x as f32 + 0.5;
    let py = y as f32 + 0.5;
    let cx = if px < radius {
        radius
    } else if px > w as f32 - radius {
        w as f32 - radius
    } else {
        return 1.0;
    };
    let cy = if py < radius {
        radius
    } else if py > h as f32 - radius {
        h as f32 - radius
    } else {
        return 1.0;
    };
    let dx = px - cx;
    let dy = py - cy;
    let dist = (dx * dx + dy * dy).sqrt();
    if dist <= radius - 0.5 {
        1.0
    } else if dist >= radius + 0.5 {
        0.0
    } else {
        (radius + 0.5 - dist).clamp(0.0, 1.0)
    }
}

fn apply_dip_flash(data: &mut [u8], black: f32, white: f32) {
    let keep = (1.0 - black).clamp(0.0, 1.0) * (1.0 - white).clamp(0.0, 1.0);
    let add = (white.clamp(0.0, 1.0) * 255.0).round() as u16;
    let k = (keep * 256.0).round() as u16;
    for px in data.chunks_exact_mut(4) {
        px[0] = (((px[0] as u16 * k) >> 8) + add).min(255) as u8;
        px[1] = (((px[1] as u16 * k) >> 8) + add).min(255) as u8;
        px[2] = (((px[2] as u16 * k) >> 8) + add).min(255) as u8;
        px[3] = 255;
    }
}

fn apply_grade(data: &mut [u8], width: u32, height: u32, grade: &Grade) {
    if grade.skip {
        return;
    }
    if let Some(curves) = grade.curves {
        if grade.vignette <= 0.001 {
            for px in data.chunks_exact_mut(4) {
                px[0] = curves[0][px[0] as usize];
                px[1] = curves[1][px[1] as usize];
                px[2] = curves[2][px[2] as usize];
            }
            return;
        }
    }
    let cx = width as f32 * 0.5;
    let cy = height as f32 * 0.5;
    let vig = grade.vignette;
    for y in 0..height {
        for x in 0..width {
            let i = (y as usize * width as usize + x as usize) * 4;
            if i + 2 >= data.len() {
                return;
            }
            let rgb = if let Some(curves) = grade.curves {
                [
                    curves[0][data[i] as usize],
                    curves[1][data[i + 1] as usize],
                    curves[2][data[i + 2] as usize],
                ]
            } else {
                math::map_color(&grade.look, [data[i], data[i + 1], data[i + 2]])
            };
            let m = if vig <= 0.001 {
                1.0
            } else {
                let dx = (x as f32 - cx) / cx.max(1.0);
                let dy = (y as f32 - cy) / cy.max(1.0);
                let d = (dx * dx + dy * dy).sqrt();
                let v = ((d - 0.45) / 0.9).clamp(0.0, 1.0);
                1.0 - vig * v * v
            };
            data[i] = (rgb[0] as f32 * m) as u8;
            data[i + 1] = (rgb[1] as f32 * m) as u8;
            data[i + 2] = (rgb[2] as f32 * m) as u8;
        }
    }
}
