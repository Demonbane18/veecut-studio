//! Zoom, crop, transition, loudness-chain and look math. No ffmpeg here.

use crate::project::{Clip, Look, LookPreset, Point, Zoom};

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct CropRect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

pub(crate) fn smoothstep(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// 0 outside the zoom, 1 during the hold. `ease` 0 is an instant punch-in.
pub(crate) fn zoom_amount(zoom: &Zoom, t: f64) -> f32 {
    if !(t >= zoom.at && t < zoom.at + zoom.duration) {
        return 0.0;
    }
    let ease = zoom.ease.max(0.0).min(zoom.duration * 0.5);
    if ease <= 1e-9 {
        return 1.0;
    }
    let local = t - zoom.at;
    let amt = if local < ease {
        smoothstep(local / ease)
    } else if local > zoom.duration - ease {
        smoothstep((zoom.duration - local) / ease)
    } else {
        1.0
    };
    amt as f32
}

pub(crate) fn zoom_scale(zoom: &Zoom, t: f64) -> f32 {
    1.0 + (zoom.scale - 1.0) * zoom_amount(zoom, t)
}

/// Scale and focus at a time inside a clip. Zooms multiply Ken Burns.
pub(crate) fn view_at(clip: &Clip, local_t: f64, duration: f64) -> (f32, Point) {
    let base = clip.focus.unwrap_or(Point::CENTER);
    let (mut scale, mut focus) = if let Some(kb) = &clip.ken_burns {
        let p = if duration > 1e-9 {
            (local_t / duration).clamp(0.0, 1.0) as f32
        } else {
            0.0
        };
        let from = kb.from_focus.unwrap_or(base);
        let to = kb.to_focus.unwrap_or(base);
        (
            (kb.from_scale + (kb.to_scale - kb.from_scale) * p).max(0.01),
            Point {
                x: from.x + (to.x - from.x) * p,
                y: from.y + (to.y - from.y) * p,
            },
        )
    } else {
        (1.0, base)
    };
    for z in &clip.zooms {
        let amt = zoom_amount(z, local_t);
        if amt <= 0.0 {
            continue;
        }
        scale *= zoom_scale(z, local_t);
        if let Some(zf) = z.focus {
            focus.x += (zf.x - focus.x) * amt;
            focus.y += (zf.y - focus.y) * amt;
        }
    }
    (scale.max(0.01), focus)
}

/// Largest scale the decoder must cover so a punch-in is a crop, not an upscale.
/// Capped at 4: a wider Ken Burns times a punch-in would allocate a huge frame.
pub(crate) fn max_decode_scale(clip: &Clip) -> f32 {
    let kb = clip
        .ken_burns
        .as_ref()
        .map(|k| k.from_scale.max(k.to_scale).max(1.0))
        .unwrap_or(1.0);
    let z = clip
        .zooms
        .iter()
        .map(|z| z.scale.max(1.0))
        .fold(1.0, f32::max);
    (kb * z).clamp(1.0, 4.0)
}

/// Cover crop of a source picture into an output aspect, centered on `focus` and clamped
/// so the window stays inside the source.
pub(crate) fn cover_crop(
    src_w: f64,
    src_h: f64,
    out_w: f64,
    out_h: f64,
    focus_x: f64,
    focus_y: f64,
) -> CropRect {
    let src_w = src_w.max(1.0);
    let src_h = src_h.max(1.0);
    let out_aspect = out_w.max(1.0) / out_h.max(1.0);
    let src_aspect = src_w / src_h;
    let (cw, ch) = if src_aspect > out_aspect {
        (src_h * out_aspect, src_h)
    } else {
        (src_w, src_w / out_aspect)
    };
    let cw = cw.min(src_w);
    let ch = ch.min(src_h);
    let fx = focus_x.clamp(0.0, 1.0) * src_w;
    let fy = focus_y.clamp(0.0, 1.0) * src_h;
    CropRect {
        x: (fx - cw * 0.5).clamp(0.0, src_w - cw),
        y: (fy - ch * 0.5).clamp(0.0, src_h - ch),
        w: cw,
        h: ch,
    }
}

pub(crate) fn crop_px(c: CropRect, src_w: u32, src_h: u32) -> (u32, u32, u32, u32) {
    let sw = src_w.max(2) as i32;
    let sh = src_h.max(2) as i32;
    let w = (c.w.round() as i32).clamp(2, sw);
    let h = (c.h.round() as i32).clamp(2, sh);
    let x = (c.x.round() as i32).clamp(0, sw - w);
    let y = (c.y.round() as i32).clamp(0, sh - h);
    (x as u32, y as u32, w as u32, h as u32)
}

/// Window inside a decoded frame for `scale` (1 = the whole frame), centered on a pixel.
pub(crate) fn zoom_window(
    dec_w: f64,
    dec_h: f64,
    scale: f64,
    fx: f64,
    fy: f64,
) -> (f64, f64, f64, f64) {
    let scale = scale.max(1.0);
    let w = (dec_w / scale).clamp(1.0, dec_w.max(1.0));
    let h = (dec_h / scale).clamp(1.0, dec_h.max(1.0));
    let x = (fx - w * 0.5).clamp(0.0, (dec_w - w).max(0.0));
    let y = (fy - h * 0.5).clamp(0.0, (dec_h - h).max(0.0));
    (x, y, w, h)
}

pub(crate) fn crossfade_incoming(t: f64, start: f64, duration: f64) -> f32 {
    if duration <= 1e-9 {
        return 1.0;
    }
    ((t - start) / duration).clamp(0.0, 1.0) as f32
}

/// Triangle that peaks at 1 on `cut` and is 0 at `cut ± duration/2`. Dip and flash share it.
pub(crate) fn peak_at_cut(t: f64, cut: f64, duration: f64) -> f32 {
    if duration <= 1e-9 {
        return 0.0;
    }
    let half = duration * 0.5;
    let d = (t - cut).abs();
    if d >= half {
        0.0
    } else {
        (1.0 - d / half) as f32
    }
}

pub(crate) fn fade_gain(local: f64, duration: f64, fade_in: f64, fade_out: f64) -> f32 {
    let mut g = 1.0f64;
    if fade_in > 1e-6 && local < fade_in {
        g *= (local / fade_in).clamp(0.0, 1.0);
    }
    if fade_out > 1e-6 {
        let start = duration - fade_out;
        if local > start {
            g *= ((duration - local) / fade_out).clamp(0.0, 1.0);
        }
    }
    g.clamp(0.0, 1.0) as f32
}

/// `atempo` stages in 0.5..=2 whose product is `speed`. Empty means speed 1 (no filter).
/// Portable ffmpeg only accepts 0.5..=2 per stage; this build allows up to 100, but the chain
/// stays inside 2 so an older ffmpeg still works.
pub(crate) fn atempo_chain(speed: f64) -> Vec<f64> {
    if !speed.is_finite() || speed <= 0.0 || (speed - 1.0).abs() < 1e-6 {
        return Vec::new();
    }
    let mut stages = Vec::new();
    let mut remain = speed;
    while remain > 2.0 + 1e-8 {
        stages.push(2.0);
        remain /= 2.0;
    }
    while remain < 0.5 - 1e-8 {
        stages.push(0.5);
        remain *= 2.0;
    }
    if (remain - 1.0).abs() > 1e-6 {
        stages.push(remain);
    }
    stages
}

pub(crate) fn round_even(n: u32) -> u32 {
    let n = n.max(2);
    if n % 2 == 0 { n } else { n + 1 }
}

pub(crate) fn color_unchanged(look: &Look) -> bool {
    look.preset == LookPreset::Natural
        && look.brightness == 0.0
        && look.contrast == 0.0
        && look.saturation == 0.0
        && look.temperature == 0.0
}

fn channel(c: f32) -> u8 {
    (c.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// Maps one pixel through the look. Natural with zero adjustments is exact identity.
/// Black and white forces equal channels after the other adjustments.
pub(crate) fn map_color(look: &Look, px: [u8; 3]) -> [u8; 3] {
    if color_unchanged(look) {
        return px;
    }
    let mut r = px[0] as f32 / 255.0;
    let mut g = px[1] as f32 / 255.0;
    let mut b = px[2] as f32 / 255.0;
    match look.preset {
        LookPreset::Warm => {
            r = (r * 1.06 + 0.02).min(1.0);
            g = (g * 1.01).min(1.0);
            b = (b * 0.92).min(1.0);
        }
        LookPreset::Cool => {
            r = (r * 0.92).min(1.0);
            g = (g * 1.01).min(1.0);
            b = (b * 1.06 + 0.02).min(1.0);
        }
        LookPreset::Matte => {
            r = r * 0.82 + 0.10;
            g = g * 0.82 + 0.10;
            b = b * 0.82 + 0.10;
        }
        LookPreset::TealOrange => {
            let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
            r += 0.12 * y - 0.04 * (1.0 - y);
            g += 0.02 * (1.0 - y) - 0.02 * y;
            b += 0.10 * (1.0 - y) - 0.08 * y;
        }
        LookPreset::Natural | LookPreset::BlackWhite | LookPreset::Vivid => {}
    }
    let mut contrast = look.contrast;
    let mut sat = look.saturation;
    let mut temp = look.temperature;
    match look.preset {
        LookPreset::Vivid => {
            contrast += 0.18;
            sat += 0.35;
        }
        LookPreset::Matte => {
            contrast -= 0.15;
            sat -= 0.12;
        }
        LookPreset::Warm => temp += 0.2,
        LookPreset::Cool => temp -= 0.2,
        _ => {}
    }
    let bright = look.brightness * 0.25;
    let ck = 1.0 + contrast;
    r = (r - 0.5) * ck + 0.5 + bright;
    g = (g - 0.5) * ck + 0.5 + bright;
    b = (b - 0.5) * ck + 0.5 + bright;
    let t = temp * 0.12;
    r += t;
    b -= t;
    r = r.clamp(0.0, 1.0);
    g = g.clamp(0.0, 1.0);
    b = b.clamp(0.0, 1.0);
    let sat_k = (1.0 + sat).max(0.0);
    let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    r = y + (r - y) * sat_k;
    g = y + (g - y) * sat_k;
    b = y + (b - y) * sat_k;
    if look.preset == LookPreset::BlackWhite {
        let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        return [channel(y), channel(y), channel(y)];
    }
    [channel(r), channel(g), channel(b)]
}

pub(crate) fn separable(look: &Look) -> bool {
    look.saturation == 0.0
        && !matches!(
            look.preset,
            LookPreset::BlackWhite | LookPreset::TealOrange | LookPreset::Vivid | LookPreset::Matte
        )
}

/// Per-channel curves when the look does not mix channels. Exact, and cheap to apply.
pub(crate) fn channel_curves(look: &Look) -> [[u8; 256]; 3] {
    let mut curves = [[0u8; 256]; 3];
    for i in 0..256 {
        let u = i as u8;
        curves[0][i] = map_color(look, [u, 0, 0])[0];
        curves[1][i] = map_color(look, [0, u, 0])[1];
        curves[2][i] = map_color(look, [0, 0, u])[2];
    }
    curves
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::LookPreset;

    fn zoom(at: f64, duration: f64, scale: f32, ease: f64) -> Zoom {
        Zoom {
            at,
            duration,
            scale,
            focus: None,
            ease,
        }
    }

    #[test]
    fn zoom_ease_ramps_and_punch() {
        let punch = zoom(1.0, 2.0, 1.5, 0.0);
        assert_eq!(zoom_scale(&punch, 0.9), 1.0);
        assert_eq!(zoom_scale(&punch, 1.0), 1.5);
        assert_eq!(zoom_scale(&punch, 2.9), 1.5);
        assert_eq!(zoom_scale(&punch, 3.0), 1.0);

        let eased = zoom(0.0, 2.0, 2.0, 0.5);
        assert!((zoom_scale(&eased, 0.0) - 1.0).abs() < 1e-5);
        // Halfway up the ramp: smoothstep(0.5) = 0.5, so scale is 1.5.
        assert!((zoom_scale(&eased, 0.25) - 1.5).abs() < 1e-5);
        assert!((zoom_scale(&eased, 1.0) - 2.0).abs() < 1e-5);
        assert!((zoom_scale(&eased, 1.75) - 1.5).abs() < 1e-5);
        assert!((zoom_scale(&eased, 2.0) - 1.0).abs() < 1e-5);
    }

    #[test]
    fn cover_crop_clamps_focus_near_edge() {
        let edge = cover_crop(1920.0, 1080.0, 1080.0, 1920.0, 0.95, 0.5);
        let aspect = 1080.0 / 1920.0;
        assert!(
            (edge.w / edge.h - aspect).abs() < 1e-6,
            "crop aspect {}",
            edge.w / edge.h
        );
        assert!(
            (edge.x + edge.w - 1920.0).abs() < 1e-4,
            "focus 0.95 should pin the crop to the right edge, got x={}",
            edge.x
        );
        assert!(edge.y.abs() < 1e-4);
        let mid = cover_crop(1920.0, 1080.0, 1080.0, 1920.0, 0.5, 0.5);
        assert!(
            mid.x > 1.0 && mid.x + mid.w < 1919.0,
            "center crop should have side margins"
        );
    }

    #[test]
    fn look_natural_is_identity_and_black_white_is_gray() {
        let natural = Look::default();
        for c in [0u8, 1, 127, 128, 254, 255] {
            assert_eq!(map_color(&natural, [c, c, c]), [c, c, c]);
            assert_eq!(map_color(&natural, [c, 20, 200]), [c, 20, 200]);
        }
        let bw = Look {
            preset: LookPreset::BlackWhite,
            ..Look::default()
        };
        for rgb in [
            [0, 0, 0],
            [255, 0, 0],
            [0, 255, 0],
            [10, 20, 30],
            [255, 128, 16],
        ] {
            let out = map_color(&bw, rgb);
            assert_eq!(out[0], out[1], "{rgb:?} -> {out:?}");
            assert_eq!(out[1], out[2]);
        }
    }

    #[test]
    fn transition_weights() {
        assert!((crossfade_incoming(0.0, 0.0, 1.0) - 0.0).abs() < 1e-6);
        assert!((crossfade_incoming(0.5, 0.0, 1.0) - 0.5).abs() < 1e-6);
        assert!((crossfade_incoming(1.0, 0.0, 1.0) - 1.0).abs() < 1e-6);
        assert!((peak_at_cut(1.0, 1.0, 0.4) - 1.0).abs() < 1e-6);
        assert!((peak_at_cut(0.8, 1.0, 0.4) - 0.0).abs() < 1e-6);
        assert!((peak_at_cut(0.9, 1.0, 0.4) - 0.5).abs() < 1e-6);
        assert!((peak_at_cut(1.2, 1.0, 0.4) - 0.0).abs() < 1e-6);
    }

    #[test]
    fn atempo_chain_covers_quarter_to_eight() {
        for speed in [0.25, 0.3, 0.5, 1.0, 1.5, 2.0, 3.0, 4.0, 6.0, 8.0] {
            let chain = atempo_chain(speed);
            let product: f64 = chain.iter().product();
            assert!(
                (product - speed).abs() < 1e-6,
                "{speed} -> {chain:?} = {product}"
            );
            for s in &chain {
                assert!((0.5..=2.0).contains(s), "{speed} stage {s}");
            }
        }
    }
}
