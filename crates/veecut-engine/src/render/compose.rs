//! One output frame: background, main track, mega text, overlays, cut effects, look, graphics.

use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow, bail};
use tiny_skia::{Color, Pixmap};

use crate::graphics::Graphics;
use crate::project::{
    Asset, Clip, Item, Look, Mask, Point, Project, Rect, TrackKind, TransitionKind,
};

use super::decode::{self, DecoderPool, FrameReq};
use super::fx::{self, par_rows};
use super::grade::Treat;
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
    mask: Vec<u8>,
    work: fx::Work,
}

impl Scratch {
    pub fn new() -> Self {
        Scratch {
            a: Vec::new(),
            b: Vec::new(),
            mask: Vec::new(),
            work: fx::Work::new(),
        }
    }
}

/// What every drawing step of one frame needs.
struct Frame<'a> {
    project: &'a Project,
    dir: &'a Path,
    t: f64,
    w: u32,
    h: u32,
    /// One frame, in seconds.
    step: f64,
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
    let f = Frame {
        project,
        dir: project_dir,
        t: t + fx::EPS,
        w: width,
        h: height,
        step: 1.0 / project.canvas.fps.max(1.0),
    };

    if let Some(main) = project
        .tracks
        .iter()
        .find(|tr| tr.kind == TrackKind::Main && !tr.hidden)
    {
        draw_main(&mut pixmap, &f, &main.items, pool, scratch)?;
    }
    // Mega text goes right under the first overlay track with a cut-out, so the speaker stands
    // in front of it over the world below; with no cut-out it goes over every overlay.
    let overlays = || {
        project
            .tracks
            .iter()
            .enumerate()
            .filter(|(_, tr)| !tr.hidden && tr.kind == TrackKind::Overlay)
    };
    let behind = overlays()
        .find(|(_, tr)| {
            tr.items
                .iter()
                .any(|i| i.clip().is_some_and(|c| c.mask.is_some()))
        })
        .map(|(i, _)| i);
    for (ti, track) in overlays() {
        if behind == Some(ti) {
            gfx.draw_behind(project, f.t, &mut pixmap.as_mut());
        }
        for (i, item) in track.items.iter().enumerate() {
            if item.contains(f.t) && item.clip().is_some() {
                draw_overlay(&mut pixmap, &f, &track.items, i, pool, scratch)?;
            }
        }
    }
    if behind.is_none() {
        gfx.draw_behind(project, f.t, &mut pixmap.as_mut());
    }
    cut_effects(&mut pixmap, &f);
    apply_grade(pixmap.data_mut(), width, height, grade);
    gfx.draw(project, f.t, &mut pixmap.as_mut());
    pool.end_frame();
    Ok(pixmap)
}

fn draw_main(
    pixmap: &mut Pixmap,
    f: &Frame,
    items: &[Item],
    pool: &mut DecoderPool,
    s: &mut Scratch,
) -> Result<()> {
    let visible: Vec<usize> = (0..items.len())
        .filter(|&i| items[i].contains(f.t) && items[i].clip().is_some())
        .collect();
    let (first, last) = match visible.as_slice() {
        [] => return Ok(()),
        [i] => return draw_main_clip(pixmap, f, items, *i, pool, s),
        [first, .., last] => (&items[*first], &items[*last]),
    };
    // A crossfade: the clips overlap.
    let (out_clip, in_clip) = (first.clip().unwrap(), last.clip().unwrap());
    let overlap = (first.end() - last.start).max(1e-6);
    let in_w = math::crossfade_incoming(f.t, last.start, overlap);
    let out_gain = fade_gain(first, out_clip, f.t) * opacity(out_clip) * (1.0 - in_w);
    let in_gain = fade_gain(last, in_clip, f.t) * opacity(in_clip) * in_w;
    let Scratch { a, b, mask, work } = s;
    let full = (f.w, f.h);
    layer(
        f, first, out_clip, f.t, 1.0, full, full, pool, a, mask, work,
    )?;
    layer(f, last, in_clip, f.t, 1.0, full, full, pool, b, mask, work)?;
    mix2(pixmap.data_mut(), a, out_gain, b, in_gain);
    Ok(())
}

fn draw_main_clip(
    pixmap: &mut Pixmap,
    f: &Frame,
    items: &[Item],
    i: usize,
    pool: &mut DecoderPool,
    s: &mut Scratch,
) -> Result<()> {
    let item = &items[i];
    let clip = item.clip().expect("visible main items are clips");
    let enter = entering(item, clip, f.t);
    let whip = whip_out(items, i, f.t);
    let Scratch { a, b, mask, work } = s;
    if matches!(enter, Some((TransitionKind::Tiles, _))) && i > 0 {
        // The previous picture, held on its last frame, shows in the panes not yet switched.
        let prev = &items[i - 1];
        if let Some(pc) = prev.clip() {
            let hold = prev.end() - f.step * 0.5;
            let full = (f.w, f.h);
            let alpha = layer(f, prev, pc, hold, 1.0, full, full, pool, b, mask, work)?;
            put(pixmap, b, alpha, fade_gain(prev, pc, hold) * opacity(pc));
        }
    }
    let mut alpha = clip_layer(f, item, clip, whip, f.w, f.h, pool, a, b, mask, work)?;
    let gain = layer_transition(f, item, a, f.w, f.h, &mut alpha, enter, false, work)?;
    put(
        pixmap,
        a,
        alpha,
        gain * fade_gain(item, clip, f.t) * opacity(clip),
    );
    Ok(())
}

fn draw_overlay(
    pixmap: &mut Pixmap,
    f: &Frame,
    items: &[Item],
    i: usize,
    pool: &mut DecoderPool,
    s: &mut Scratch,
) -> Result<()> {
    let item = &items[i];
    let clip = item.clip().expect("caller checked for a clip");
    let (x, y, w, h) = frame_px(clip.frame, f.w, f.h);
    if w < 1 || h < 1 {
        return Ok(());
    }
    let whip = whip_out(items, i, f.t);
    let Scratch { a, b, mask, work } = s;
    let mut alpha = clip_layer(f, item, clip, whip, w, h, pool, a, b, mask, work)?;
    let enter = entering(item, clip, f.t);
    let gain = layer_transition(f, item, a, w, h, &mut alpha, enter, true, work)?
        * fade_gain(item, clip, f.t)
        * opacity(clip);
    if gain > 0.001 {
        let radius = clip.radius * f.w as f32;
        blit(pixmap.data_mut(), f.w, f.h, a, w, h, x, y, gain, radius);
    }
    Ok(())
}

/// The clip's layer at the frame time. During a whip out it is zoomed by the whip scale and
/// zoom-blurred at half size in `spare`, then scaled back up into `dest`.
fn clip_layer(
    f: &Frame,
    item: &Item,
    clip: &Clip,
    whip: Option<(f64, f64)>,
    w: u32,
    h: u32,
    pool: &mut DecoderPool,
    dest: &mut Vec<u8>,
    spare: &mut Vec<u8>,
    mask_buf: &mut Vec<u8>,
    work: &mut fx::Work,
) -> Result<bool> {
    let full = (w, h);
    let Some((p, d)) = whip else {
        return layer(
            f, item, clip, f.t, 1.0, full, full, pool, dest, mask_buf, work,
        );
    };
    let half = ((w / 2).max(1), (h / 2).max(1));
    let zoom = fx::whip_scale(p);
    let alpha = layer(
        f, item, clip, f.t, zoom, full, half, pool, spare, mask_buf, work,
    )?;
    // Blur along the zoom this frame adds over the previous one.
    let amount = 1.0 - fx::whip_scale(p - f.step / d) / zoom;
    dest.resize(w as usize * h as usize * 4, 0);
    work.zoom_blur(spare, half.0, half.1, dest, w, h, amount);
    Ok(alpha)
}

/// Decodes `clip` at `t` with its grade, mask and outline, zoomed by `zoom` on top of its own
/// moves. It is framed and decoded for a `size` layer and drawn into `dest` at `out` (the same
/// size, or smaller for effects worked at reduced size). Returns whether it has transparency.
fn layer(
    f: &Frame,
    item: &Item,
    clip: &Clip,
    t: f64,
    zoom: f32,
    size: (u32, u32),
    out: (u32, u32),
    pool: &mut DecoderPool,
    dest: &mut Vec<u8>,
    mask_buf: &mut Vec<u8>,
    work: &mut fx::Work,
) -> Result<bool> {
    let (w, h) = out;
    // Canvas pixels to layer pixels, for the dot pitch and the outline width.
    let canvas_h = f.h as f32 * w as f32 / size.0.max(1) as f32;
    let (req, scale, focus) = clip_req(f, item, clip, t, size.0, size.1, pool)?;
    let scale = scale * zoom;
    let alpha = pool.fill(&req, scale, focus, w, h, dest)?;
    if let Some(g) = &clip.grade {
        Treat::new(g).apply(dest, w, h, canvas_h.round() as u32);
    }
    let Some(m) = &clip.mask else {
        return Ok(alpha);
    };
    if mask_frame(f, m, &req, scale, focus, w, h, pool, mask_buf)? {
        fx::apply_mask(dest, mask_buf, w as usize);
        if m.outline > 0.0 {
            work.outline(dest, w, h, m.outline * canvas_h);
        }
    } else {
        dest.iter_mut().skip(3).step_by(4).for_each(|a| *a = 0);
    }
    Ok(true)
}

/// The decode request, zoom and focus that put `clip` at `t` into a `w` x `h` layer.
fn clip_req(
    f: &Frame,
    item: &Item,
    clip: &Clip,
    t: f64,
    w: u32,
    h: u32,
    pool: &mut DecoderPool,
) -> Result<(FrameReq, f32, Point)> {
    let (asset, path) = asset_source(f.project, f.dir, &clip.asset, "clip")?;
    let (src_w, src_h) = asset_size(asset, &path, pool)?;
    let local = (t - item.start).clamp(0.0, item.duration);
    let (scale, focus) = math::view_at(clip, local, item.duration);
    let (dec_w, dec_h) = math::decode_size(clip, w, h);
    let bg = f.project.canvas.background;
    let req = FrameReq {
        id: item.id.clone(),
        path,
        image: decode::is_image(asset.kind),
        src_w,
        src_h,
        src_t: clip.src_in + local * clip.speed,
        src_out: clip.src_out(item.duration),
        speed: clip.speed.max(0.01),
        fps: f.project.canvas.fps.max(1.0),
        fit: clip.fit,
        focus: clip.focus.unwrap_or(Point::CENTER),
        dec_w,
        dec_h,
        pad: [bg.r, bg.g, bg.b],
        stretch: false,
        optional: false,
    };
    Ok((req, scale, focus))
}

/// Decodes the mask framed exactly like its clip (same request, zoom and focus) into `dest`.
/// Mask time is the clip's source time minus `offset`. False before the mask starts or past its
/// end, where the clip is fully transparent.
fn mask_frame(
    f: &Frame,
    m: &Mask,
    clip: &FrameReq,
    scale: f32,
    focus: Point,
    w: u32,
    h: u32,
    pool: &mut DecoderPool,
    dest: &mut Vec<u8>,
) -> Result<bool> {
    let (asset, path) = asset_source(f.project, f.dir, &m.asset, "mask")?;
    let image = decode::is_image(asset.kind);
    let src_t = clip.src_t - m.offset;
    let before = src_t < -0.5 * clip.speed / clip.fps;
    if !image && (before || (asset.duration > 0.0 && src_t >= asset.duration)) {
        return Ok(false);
    }
    let size = asset_size(asset, &path, pool)?;
    let req = FrameReq {
        id: format!("{}#mask", clip.id),
        path,
        image,
        src_t: src_t.max(0.0),
        src_out: clip.src_out - m.offset,
        pad: [0, 0, 0],
        stretch: size != (clip.src_w, clip.src_h),
        optional: true,
        ..clip.clone()
    };
    pool.fill(&req, scale, focus, w, h, dest)?;
    Ok(true)
}

/// The kind and 0..1 progress of the transition `clip` is entering with at `t`, if any.
fn entering(item: &Item, clip: &Clip, t: f64) -> Option<(TransitionKind, f64)> {
    let tr = clip.transition.as_ref()?;
    Some((tr.kind, fx::progress(t, item.start, tr.duration)?))
}

/// Progress and length of a whip out of `items[i]`: a clip on the same track starts where it
/// ends with a whip transition, and `t` is in its last `duration`.
fn whip_out(items: &[Item], i: usize, t: f64) -> Option<(f64, f64)> {
    let end = items[i].end();
    let d = items
        .iter()
        .filter(|n| (n.start - end).abs() < 1e-3)
        .find_map(|n| {
            let tr = n.clip()?.transition.as_ref()?;
            (tr.kind == TransitionKind::Whip).then_some(tr.duration)
        })?;
    Some((fx::progress(t, end - d, d)?, d))
}

/// The parts of an entering transition drawn on a layer: rack focus, tile reveal and, on
/// overlays, the fade-in, the dip from black and the white box. Returns extra opacity.
fn layer_transition(
    f: &Frame,
    item: &Item,
    px: &mut [u8],
    w: u32,
    h: u32,
    alpha: &mut bool,
    enter: Option<(TransitionKind, f64)>,
    overlay: bool,
    work: &mut fx::Work,
) -> Result<f32> {
    let Some((kind, p)) = enter else {
        return Ok(1.0);
    };
    match kind {
        TransitionKind::Focus => work.blur(px, w, h, fx::focus_radius(p, f.h), *alpha),
        TransitionKind::Pixelate => {
            fx::pixelate(px, w as usize, h as usize, fx::pixelate_size(p, f.h));
        }
        TransitionKind::Tiles => {
            let order = fx::tile_order(fx::seed(&item.id));
            fx::tiles(px, w as usize, h as usize, &order, fx::tile_step(p));
            *alpha = true;
        }
        TransitionKind::Crossfade if overlay => return Ok(p as f32),
        TransitionKind::Dip if overlay => {
            let k = (p * 256.0) as u32;
            par_rows(px, w as usize * 4, |_, band| {
                for c in band.as_chunks_mut::<4>().0 {
                    for v in &mut c[..3] {
                        *v = ((*v as u32 * k) >> 8) as u8;
                    }
                }
            });
        }
        TransitionKind::Flash if overlay => {
            px.fill(255);
            *alpha = false;
        }
        _ => {}
    }
    Ok(1.0)
}

/// Whole-frame cut effects: dip and flash on main-track cuts, negatives and datamosh on main
/// and overlay cuts.
fn cut_effects(pixmap: &mut Pixmap, f: &Frame) {
    let (mut black, mut white, mut negative) = (0f32, 0f32, false);
    let mut glitch = None;
    for track in &f.project.tracks {
        let main = track.kind == TrackKind::Main;
        if track.hidden || !(main || track.kind == TrackKind::Overlay) {
            continue;
        }
        for item in &track.items {
            let Some(tr) = item.clip().and_then(|c| c.transition.as_ref()) else {
                continue;
            };
            let (cut, d) = (item.start, tr.duration);
            match tr.kind {
                TransitionKind::Dip if main => black = black.max(math::peak_at_cut(f.t, cut, d)),
                TransitionKind::Flash if main => {
                    white = white.max(fx::flash_amount(f.t, cut, d, f.step));
                }
                TransitionKind::Negative => negative |= fx::negative_on(f.t, cut, d),
                TransitionKind::Glitch if glitch.is_none() => {
                    if let Some(p) = fx::progress(f.t, cut, d) {
                        // A new datamosh every frame, the same one on every render.
                        let frame = ((f.t - cut) / f.step).floor() as u64;
                        let seed = fx::seed(&item.id) ^ frame.wrapping_mul(0x9e37_79b9_7f4a_7c15);
                        glitch = Some((seed, p));
                    }
                }
                _ => {}
            }
        }
    }
    let (w, h) = (pixmap.width() as usize, pixmap.height() as usize);
    if let Some((seed, p)) = glitch {
        fx::glitch(pixmap.data_mut(), w, h, seed, 1.0 - 0.5 * p as f32);
    }
    if negative {
        fx::invert(pixmap.data_mut(), w);
    }
    if black > 0.001 || white > 0.001 {
        apply_dip_flash(pixmap.data_mut(), black, white);
    }
}

fn asset_source<'a>(
    project: &'a Project,
    dir: &Path,
    id: &str,
    what: &str,
) -> Result<(&'a Asset, PathBuf)> {
    let asset = project
        .asset(id)
        .ok_or_else(|| anyhow!("{what} uses asset {id:?}, which the project does not have"))?;
    let path = project.asset_path(dir, asset);
    if !path.is_file() {
        bail!("{} does not exist (asset {})", path.display(), asset.id);
    }
    Ok((asset, path))
}

fn asset_size(asset: &Asset, path: &Path, pool: &mut DecoderPool) -> Result<(u32, u32)> {
    if asset.width > 0 && asset.height > 0 {
        Ok((asset.width, asset.height))
    } else {
        pool.probe_size(path)
    }
}

fn fade_gain(item: &Item, clip: &Clip, t: f64) -> f32 {
    math::fade_gain(t - item.start, item.duration, clip.fade_in, clip.fade_out)
}

fn opacity(clip: &Clip) -> f32 {
    clip.opacity.clamp(0.0, 1.0)
}

fn frame_px(frame: Option<Rect>, canvas_w: u32, canvas_h: u32) -> (i32, i32, u32, u32) {
    let r = frame.unwrap_or(Rect::FULL);
    let x = (r.x * canvas_w as f32).round() as i32;
    let y = (r.y * canvas_h as f32).round() as i32;
    let w = (r.w * canvas_w as f32).round().max(1.0) as u32;
    let h = (r.h * canvas_h as f32).round().max(1.0) as u32;
    (x, y, w, h)
}

/// Draws a full-canvas main-track layer: replaced when opaque, composited over what is already
/// there when it has transparency.
fn put(pixmap: &mut Pixmap, layer: &[u8], alpha: bool, gain: f32) {
    let (w, h) = (pixmap.width(), pixmap.height());
    if alpha {
        blit(pixmap.data_mut(), w, h, layer, w, h, 0, 0, gain, 0.0);
    } else {
        replace_weighted(pixmap.data_mut(), layer, gain);
    }
}

fn replace_weighted(dst: &mut [u8], src: &[u8], gain: f32) {
    let gain = gain.clamp(0.0, 1.0);
    if gain >= 0.999 && src.len() >= dst.len() {
        let n = dst.len() / 4 * 4;
        dst[..n].copy_from_slice(&src[..n]);
        for px in dst[..n].as_chunks_mut::<4>().0 {
            px[3] = 255;
        }
        return;
    }
    let w = (gain * 256.0).round() as u16;
    for (d, s) in dst
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(src.chunks_exact(4))
    {
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
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(a.chunks_exact(4))
        .zip(b.chunks_exact(4))
    {
        d[0] = ((pa[0] as u16 * wa + pb[0] as u16 * wb) >> 8).min(255) as u8;
        d[1] = ((pa[1] as u16 * wa + pb[1] as u16 * wb) >> 8).min(255) as u8;
        d[2] = ((pa[2] as u16 * wa + pb[2] as u16 * wb) >> 8).min(255) as u8;
        d[3] = 255;
    }
}

/// `v / 255`, rounded, for v up to 255 * 255.
fn div255(v: u32) -> u32 {
    let v = v + 128;
    (v + (v >> 8)) >> 8
}

/// Straight-alpha source over an opaque destination at (`x`, `y`), with rounded corners.
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
    let op = (opacity.clamp(0.0, 1.0) * 255.0).round() as u32;
    if op == 0 || sw == 0 || sh == 0 || src.len() < sw as usize * sh as usize * 4 {
        return;
    }
    let r = radius.max(0.0).min(sw as f32 * 0.5).min(sh as f32 * 0.5);
    let rc = if r >= 0.5 { r.ceil() as i64 } else { 0 };
    let (x, y) = (x as i64, y as i64);
    let (x0, x1) = (x.max(0), (x + sw as i64).min(dst_w as i64));
    let (y0, y1) = (y.max(0), (y + sh as i64).min(dst_h as i64));
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    let stride = dst_w as usize * 4;
    let rows = &mut dst[y0 as usize * stride..y1 as usize * stride];
    par_rows(rows, stride, |first, band| {
        for (k, line) in band.chunks_exact_mut(stride).enumerate() {
            let row = y0 + (first + k) as i64 - y;
            let corner_row = row < rc || row >= sh as i64 - rc;
            let src_row =
                &src[row as usize * sw as usize * 4..(row as usize + 1) * sw as usize * 4];
            for dx in x0..x1 {
                let col = dx - x;
                let s = &src_row[col as usize * 4..col as usize * 4 + 4];
                let mut a = s[3] as u32 * op;
                if corner_row && (col < rc || col >= sw as i64 - rc) {
                    a = (a as f32 * corner_coverage(col as u32, row as u32, sw, sh, r)) as u32;
                }
                if a == 0 {
                    continue;
                }
                let d = &mut line[dx as usize * 4..dx as usize * 4 + 4];
                if a >= 255 * 255 {
                    d[..3].copy_from_slice(&s[..3]);
                } else {
                    let wa = div255(a);
                    for c in 0..3 {
                        d[c] = div255(s[c] as u32 * wa + d[c] as u32 * (255 - wa)) as u8;
                    }
                }
                d[3] = 255;
            }
        }
    });
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
    for px in data.as_chunks_mut::<4>().0 {
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
    if let Some(curves) = grade.curves
        && grade.vignette <= 0.001
    {
        for px in data.as_chunks_mut::<4>().0 {
            px[0] = curves[0][px[0] as usize];
            px[1] = curves[1][px[1] as usize];
            px[2] = curves[2][px[2] as usize];
        }
        return;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media;
    use crate::project::{AssetKind, AssetRole, Canvas, ItemBody, KenBurns, VideoKind, Zoom};

    #[test]
    fn pixelate_transition_holds_steps_and_finishes_on_original_clip() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (120u32, 180u32);
        let path = dir.path().join("gradient.png");
        image::RgbaImage::from_fn(w, h, |x, y| image::Rgba([(x * 2) as u8, y as u8, 90, 255]))
            .save(&path)
            .unwrap();
        let mut project = Project::new("pixels", VideoKind::default(), Canvas::new(w, h));
        project.assets.push(Asset {
            id: "gradient".into(),
            path,
            kind: AssetKind::Image,
            role: AssetRole::Broll,
            duration: 0.0,
            width: w,
            height: h,
            fps: 0.0,
            has_audio: false,
        });
        let mut clip = Clip::new("gradient", 0.0);
        clip.transition = Some(crate::project::Transition {
            kind: TransitionKind::Pixelate,
            duration: 0.4,
        });
        project.main_mut().items.push(Item {
            id: "pixels".into(),
            start: 0.0,
            duration: 1.0,
            body: ItemBody::Clip(clip),
        });
        let render = |p: &Project, t| super::super::render_frame(p, dir.path(), t, w, h).unwrap();
        let early = render(&project, 0.1);
        let held = render(&project, 0.11);
        let late = render(&project, 0.3);
        assert_eq!(
            early.data(),
            held.data(),
            "size must hold between step boundaries"
        );
        let run = |frame: &Pixmap| {
            let first = &frame.data()[..4];
            frame.data()[..w as usize * 4]
                .chunks_exact(4)
                .take_while(|p| *p == first)
                .count()
        };
        assert_eq!(run(&early), fx::pixelate_size(0.25, h));
        assert_eq!(run(&late), fx::pixelate_size(0.75, h));
        assert!(run(&early) > run(&late));
        let end = render(&project, 0.4);
        project.main_mut().items[0].clip_mut().unwrap().transition = None;
        assert_eq!(end.data(), render(&project, 0.4).data());
    }
    /// A white box moving right, drawn by ffmpeg at `w` x `h` (geometry scales with the size).
    fn box_video(path: &Path, w: u32, h: u32, gray: bool) -> Asset {
        let k = w as f64 / 160.0;
        let filter = format!(
            "color=black:s={w}x{h}:r=30:d=1[bg];color=white:s={bw}x{bh}:r=30[b];[bg][b]overlay=x={x}+t*{v}:y={y}:shortest=1",
            x = 20.0 * k,
            v = 60.0 * k,
            y = 25.0 * k,
            bw = 40.0 * k,
            bh = 30.0 * k
        );
        let mut cmd = media::ffmpeg_command().unwrap();
        cmd.args([
            "-y", "-f", "lavfi", "-i", &filter, "-c:v", "ffv1", "-pix_fmt",
        ])
        .arg(if gray { "gray" } else { "yuv444p" })
        .arg(path);
        media::run(&mut cmd).unwrap();
        let id = path.file_stem().unwrap().to_string_lossy().into_owned();
        Asset {
            id,
            path: path.to_path_buf(),
            kind: AssetKind::Video,
            role: if gray {
                AssetRole::Mask
            } else {
                AssetRole::Broll
            },
            duration: 1.0,
            width: w,
            height: h,
            fps: 30.0,
            has_audio: false,
        }
    }

    fn white_mask(frame: &Pixmap) -> Vec<bool> {
        frame.data().chunks_exact(4).map(|p| p[1] >= 128).collect()
    }

    /// A white clip cut out by a half-size mask of a moving box must land exactly where the
    /// full-size box video lands with the same framing, zoom and Ken Burns, including the
    /// mask offset. Past the mask's end the clip is transparent.
    #[test]
    fn mask_frames_exactly_like_its_clip() {
        if media::ffmpeg().is_err() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let boxes = box_video(&dir.path().join("box.mkv"), 320, 180, false);
        let mask = box_video(&dir.path().join("mask.mkv"), 160, 90, true);
        let white_path = dir.path().join("white.png");
        image::RgbaImage::from_pixel(320, 180, image::Rgba([255, 255, 255, 255]))
            .save(&white_path)
            .unwrap();
        let white = Asset {
            id: "white".into(),
            path: white_path,
            kind: AssetKind::Image,
            role: AssetRole::Broll,
            duration: 0.0,
            width: 320,
            height: 180,
            fps: 0.0,
            has_audio: false,
        };
        let framed = |asset: &str, src_in: f64| {
            let mut c = Clip::new(asset, src_in);
            c.frame = Some(Rect {
                x: 0.05,
                y: 0.2,
                w: 0.9,
                h: 0.5,
            });
            c.focus = Some(Point::new(0.4, 0.45));
            c.zooms.push(Zoom {
                at: 0.1,
                duration: 2.0,
                scale: 1.3,
                focus: None,
                ease: 0.2,
            });
            c.ken_burns = Some(KenBurns {
                from_scale: 1.0,
                to_scale: 1.15,
                from_focus: None,
                to_focus: Some(Point::new(0.55, 0.5)),
                smooth: false,
                phase: [0.0, 1.0],
            });
            c
        };
        let project_with = |clip: Clip| {
            let mut p = Project::new("mask", VideoKind::Documentary, Canvas::new(180, 320));
            p.canvas.fps = 30.0;
            p.assets = vec![boxes.clone(), mask.clone(), white.clone()];
            p.tracks
                .iter_mut()
                .find(|t| t.id == "broll")
                .unwrap()
                .items
                .push(Item {
                    id: "o".into(),
                    start: 0.0,
                    duration: 1.6,
                    body: ItemBody::Clip(clip),
                });
            p
        };
        // The box video plays from 0; the masked clip starts 0.5 s into its (static) source
        // and its mask starts there too.
        let reference = project_with(framed("box", 0.0));
        let mut cut = framed("white", 0.5);
        cut.mask = Some(Mask {
            asset: "mask".into(),
            offset: 0.5,
            outline: 0.0,
        });
        let masked = project_with(cut);
        for t in [0.0, 0.2, 0.5, 0.8] {
            let a = white_mask(
                &super::super::render_frame(&reference, dir.path(), t, 180, 320).unwrap(),
            );
            let b =
                white_mask(&super::super::render_frame(&masked, dir.path(), t, 180, 320).unwrap());
            let lit = a.iter().filter(|&&v| v).count();
            let differ = a.iter().zip(&b).filter(|(x, y)| x != y).count();
            assert!(lit > 200, "t={t}: the box should be visible ({lit} px)");
            // Only edge pixels may round differently (the mask is scaled up from half size).
            assert!(
                differ * 12 < lit,
                "t={t}: {differ} of {lit} pixels disagree"
            );
        }
        // The mask is one second long: from source 1.5 s on the clip is fully transparent.
        let late = render_frame_ok(&masked, dir.path(), 1.2);
        assert!(
            white_mask(&late).iter().all(|&v| !v),
            "past the mask end must be transparent"
        );
    }

    fn render_frame_ok(p: &Project, dir: &Path, t: f64) -> Pixmap {
        super::super::render_frame(p, dir, t, 180, 320).unwrap()
    }
}
