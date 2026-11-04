//! Per-clip colour treatments, a halftone dot screen, vignette, hue-band recolour and pixelation.
//! Documentary looks, dot pitch and vignette follow NextPhase Editor V1's published measurements.
//! Timings and sizes follow published measurements in Jad M.H's (NextPhase AI) Arcade Motion
//! Graphics / Motion Graphics V3 skills; code written fresh. SoftMono alone lifts blacks.

use crate::project::{Recolor, TreatLook, Treatment};

use super::fx::{par_rows, pixelate};

/// Recolour bands fade in over this many degrees inside each edge.
const FEATHER: f32 = 12.0;

/// A treatment ready to apply: every look is `out = tone[luma] + chroma * (channel - luma)`,
/// so one luma, three table lookups and three multiply-adds per pixel.
pub(crate) struct Treat {
    tone: [[i16; 256]; 3],
    /// Fixed point, 256 = 1. Negative for the invert look.
    chroma: i32,
    halftone: f32,
    vignette: f32,
    pixelate: f32,
    recolor: Option<Band>,
}

impl Treat {
    pub fn new(t: &Treatment) -> Self {
        let (tone, chroma) = look_tables(t.look);
        Treat {
            tone,
            chroma,
            halftone: t.halftone.clamp(0.0, 1.0),
            vignette: t.vignette.clamp(0.0, 1.0),
            pixelate: t.pixelate.clamp(0.0, 1.0),
            recolor: t.recolor.and_then(Band::new),
        }
    }

    /// Grades a straight-alpha RGBA layer in place, then pixelates the finished colours.
    /// `canvas_h` sets the dot pitch and pixel block size consistently across layers.
    pub fn apply(&self, px: &mut [u8], w: u32, h: u32, canvas_h: u32) {
        let (w, h) = (w as usize, h as usize);
        if w == 0 || h == 0 {
            return;
        }
        let pitch = (0.0075 * canvas_h as f32).max(2.0);
        let inv_cell = 1.0 / (std::f32::consts::SQRT_2 * pitch);
        let (sx, sy) = (2.0 / w as f32, 2.0 / h as f32);
        par_rows(px, w * 4, |y0, band| {
            for (k, row) in band.chunks_exact_mut(w * 4).enumerate() {
                let y = y0 + k;
                let ny = (y as f32 + 0.5) * sy - 1.0;
                for (x, p) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                    if let Some(band) = &self.recolor {
                        let [r, g, b] = band.apply([p[0], p[1], p[2]]);
                        (p[0], p[1], p[2]) = (r, g, b);
                    }
                    self.tone(p);
                    let mut gain = 1.0f32;
                    if self.halftone > 0.0 {
                        gain *= halftone(p, x, y, pitch, inv_cell, self.halftone);
                    }
                    if self.vignette > 0.0 {
                        let nx = (x as f32 + 0.5) * sx - 1.0;
                        gain *= vignette_gain(nx * nx + ny * ny, self.vignette);
                    }
                    if gain < 1.0 {
                        for c in &mut p[..3] {
                            *c = (*c as f32 * gain) as u8;
                        }
                    }
                }
            }
        });
        pixelate(
            px,
            w,
            h,
            (self.pixelate * canvas_h as f32).round().max(1.0) as usize,
        );
    }

    #[inline]
    fn tone(&self, p: &mut [u8]) {
        let (r, g, b) = (p[0] as i32, p[1] as i32, p[2] as i32);
        let y = (54 * r + 183 * g + 19 * b + 128) >> 8;
        let l = y as usize;
        p[0] = (self.tone[0][l] as i32 + ((self.chroma * (r - y)) >> 8)).clamp(0, 255) as u8;
        p[1] = (self.tone[1][l] as i32 + ((self.chroma * (g - y)) >> 8)).clamp(0, 255) as u8;
        p[2] = (self.tone[2][l] as i32 + ((self.chroma * (b - y)) >> 8)).clamp(0, 255) as u8;
    }
}

fn smoothstep(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Contrast as an S-curve around mid-grey; `amount` 0 is flat, 1 is a full smoothstep.
fn s_curve(y: f32, amount: f32) -> f32 {
    y + amount * (smoothstep(y) - y)
}

/// Tone tables per channel (indexed by luma) and the chroma factor for a look.
fn look_tables(look: TreatLook) -> ([[i16; 256]; 3], i32) {
    let mut tone = [[0i16; 256]; 3];
    if look == TreatLook::Invert {
        for (l, v) in tone.iter_mut().flat_map(|t| t.iter_mut().enumerate()) {
            *v = 255 - l as i16;
        }
        return (tone, -256);
    }
    // Tints fade out toward black and white, so blacks stay black and whites stay white.
    let mid = |y: f32| 4.0 * y * (1.0 - y);
    let shadow = |y: f32| 6.75 * y * (1.0 - y) * (1.0 - y);
    let light = |y: f32| 6.75 * y * y * (1.0 - y);
    let (chroma, f): (f32, &dyn Fn(f32) -> [f32; 3]) = match look {
        TreatLook::Natural => (1.15, &|y: f32| [s_curve(y, 0.35); 3]),
        TreatLook::Teal => (0.55, &|y: f32| {
            let v = s_curve(y, 0.3);
            let (s, l) = (shadow(y), light(y));
            [
                v - 0.10 * s + 0.05 * l,
                v + 0.04 * s + 0.025 * l,
                v + 0.06 * s - 0.05 * l,
            ]
        }),
        TreatLook::Mono => (0.0, &|y: f32| {
            let v = smoothstep((y - 0.04) / 0.92);
            [0.5 * (v + smoothstep(v)); 3]
        }),
        TreatLook::SoftMono => (0.0, &|y: f32| [0.10 + 0.80 * s_curve(y, 0.25); 3]),
        TreatLook::Warm => (0.5, &|y: f32| {
            let (v, m) = (s_curve(y, 0.25), mid(y));
            [v + 0.11 * m, v + 0.03 * m, v - 0.12 * m]
        }),
        TreatLook::Cold => (0.5, &|y: f32| {
            let (v, m) = (s_curve(y, 0.3), mid(y));
            [v - 0.07 * m, v + 0.005 * m, v + 0.09 * m]
        }),
        TreatLook::Invert => unreachable!(),
    };
    for l in 0..256 {
        let rgb = f(l as f32 / 255.0);
        for c in 0..3 {
            tone[c][l] = (rgb[c].clamp(0.0, 1.0) * 255.0).round() as i16;
        }
    }
    (tone, (chroma * 256.0).round() as i32)
}

/// Ink gain of a 45-degree dot screen that only prints in the shadows: darker pixels get
/// bigger dots. Dots stop short of merging so paper still shows between them in deep shadow.
#[inline]
fn halftone(p: &[u8], x: usize, y: usize, pitch: f32, inv_cell: f32, strength: f32) -> f32 {
    let luma = (0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32) / 255.0;
    let shade = ((0.6 - luma) / 0.45).clamp(0.0, 1.0);
    if shade <= 0.0 {
        return 1.0;
    }
    let (x, y) = (x as f32, y as f32);
    let u = (x + y) * inv_cell;
    let v = (x - y) * inv_cell;
    let (fu, fv) = (u - u.round(), v - v.round());
    let dist = (fu * fu + fv * fv).sqrt() * pitch;
    let radius = pitch * 0.62 * shade.sqrt();
    let ink = (radius - dist + 0.5).clamp(0.0, 1.0);
    1.0 - strength * 0.85 * ink
}

/// Vignette gain at squared normalised radius `d2` (1 at the edge midpoints, 2 in the
/// corners). At strength 1 the outer 5% of the frame averages 0.59 of the centre's luma.
fn vignette_gain(d2: f32, strength: f32) -> f32 {
    1.0 - strength * 0.537 * smoothstep((d2 - 0.15) / 1.45)
}

/// A recolour band prepared for lookups: hues `from..from+len` (degrees) move by `shift`.
#[derive(Clone, Copy)]
struct Band {
    from: f32,
    len: f32,
    shift: f32,
}

impl Band {
    fn new(r: Recolor) -> Option<Band> {
        let span = r.to - r.from;
        let len = if span.abs() >= 360.0 {
            360.0
        } else {
            span.rem_euclid(360.0)
        };
        if len <= 0.0 {
            return None;
        }
        let from = r.from.rem_euclid(360.0);
        let shift = (r.target - (from + len * 0.5)).rem_euclid(360.0);
        Some(Band { from, len, shift })
    }

    /// 1 inside the band, easing to 0 over `FEATHER` degrees inside each edge, 0 outside.
    fn weight(&self, hue: f32) -> f32 {
        if self.len >= 360.0 {
            return 1.0;
        }
        let a = (hue - self.from).rem_euclid(360.0);
        if a > self.len {
            return 0.0;
        }
        smoothstep(a.min(self.len - a) / FEATHER)
    }

    /// Recolours the whole pixel, then blends by the band weight in RGB. Blending rotated hues
    /// by weight would paint every hue in between (rainbow fringes) at the band edges.
    fn apply(&self, px: [u8; 3]) -> [u8; 3] {
        let Some((h, s, v)) = hsv(px) else { return px };
        let w = self.weight(h);
        if w <= 0.0 {
            return px;
        }
        let q = rgb_of(h + self.shift, s, v);
        let mut out = [0u8; 3];
        for c in 0..3 {
            let o = px[c] as f32 / 255.0;
            out[c] = ((o + (q[c] - o) * w).clamp(0.0, 1.0) * 255.0).round() as u8;
        }
        out
    }
}

/// Hue (degrees), saturation and value of a pixel; None for greys, which have no hue.
fn hsv(px: [u8; 3]) -> Option<(f32, f32, f32)> {
    let [r, g, b] = px.map(|c| c as f32 / 255.0);
    let max = r.max(g).max(b);
    let d = max - r.min(g).min(b);
    if d <= 0.0 {
        return None;
    }
    let h = if max == r {
        60.0 * ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    Some((h, d / max, max))
}

fn rgb_of(h: f32, s: f32, v: f32) -> [f32; 3] {
    let h = h.rem_euclid(360.0) / 60.0;
    let c = v * s;
    let x = c * (1.0 - (h.rem_euclid(2.0) - 1.0).abs());
    let (r, g, b) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    [r + m, g + m, b + m]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn treat(look: TreatLook) -> Treat {
        Treat::new(&Treatment {
            look,
            ..Treatment::default()
        })
    }

    fn graded(t: &Treat, rgb: [u8; 3]) -> [u8; 3] {
        let mut px = [rgb[0], rgb[1], rgb[2], 255];
        t.apply(&mut px, 1, 1, 1080);
        [px[0], px[1], px[2]]
    }

    fn samples() -> impl Iterator<Item = [u8; 3]> {
        (0..=255u16).step_by(15).flat_map(|r| {
            (0..=255u16).step_by(15).flat_map(move |g| {
                (0..=255u16)
                    .step_by(15)
                    .map(move |b| [r as u8, g as u8, b as u8])
            })
        })
    }

    #[test]
    fn pixelate_grade_makes_flat_canvas_sized_blocks() {
        let (w, h, size) = (19usize, 13usize, 4usize);
        let mut px = Vec::new();
        for y in 0..h {
            for x in 0..w {
                px.extend_from_slice(&[(x * 11) as u8, (y * 17) as u8, 90, 255]);
            }
        }
        Treat::new(&Treatment {
            pixelate: 0.04,
            ..Treatment::default()
        })
        .apply(&mut px, w as u32, h as u32, 100);
        let at = |x: usize, y: usize| &px[(y * w + x) * 4..(y * w + x + 1) * 4];
        for y in 0..h {
            for x in 0..w {
                assert_eq!(at(x, y), at(x / size * size, y / size * size));
            }
        }
        assert_ne!(at(0, 0), at(size, 0));
        assert_ne!(at(0, 0), at(0, size));
    }

    #[test]
    fn soft_mono_lifts_blacks_and_softens_white_without_chroma() {
        let t = treat(TreatLook::SoftMono);
        for rgb in samples() {
            let m = graded(&t, rgb);
            assert_eq!(m[0], m[1]);
            assert_eq!(m[1], m[2]);
        }
        assert!((18..=41).contains(&graded(&t, [0; 3])[0]));
        assert!((220..=240).contains(&graded(&t, [255; 3])[0]));
        assert!((126..=130).contains(&graded(&t, [128; 3])[0]));
    }

    #[test]
    fn looks_keep_their_promises() {
        let mono = treat(TreatLook::Mono);
        let invert = treat(TreatLook::Invert);
        for rgb in samples() {
            let m = graded(&mono, rgb);
            assert!(m[0] == m[1] && m[1] == m[2], "mono {rgb:?} -> {m:?}");
            assert_eq!(graded(&invert, graded(&invert, rgb)), rgb, "invert twice");
        }
        let natural = treat(TreatLook::Natural);
        let grey = graded(&natural, [128, 128, 128]);
        assert!(grey.iter().all(|&c| c.abs_diff(128) <= 2), "{grey:?}");
        // No lifted blacks, no dirty whites.
        for look in [
            TreatLook::Natural,
            TreatLook::Teal,
            TreatLook::Mono,
            TreatLook::Warm,
            TreatLook::Cold,
        ] {
            let t = treat(look);
            assert!(
                graded(&t, [0, 0, 0]).iter().all(|&c| c <= 2),
                "{look:?} black"
            );
            assert!(
                graded(&t, [255, 255, 255]).iter().all(|&c| c >= 253),
                "{look:?} white"
            );
        }
        // Teal: shadows lean blue-green, highlights lean warm.
        let teal = treat(TreatLook::Teal);
        let s = graded(&teal, [60, 60, 60]);
        let l = graded(&teal, [190, 190, 190]);
        assert!(s[2] > s[0] && s[1] > s[0], "{s:?}");
        assert!(l[0] > l[2], "{l:?}");
    }

    #[test]
    fn vignette_edge_to_centre_is_about_059() {
        let (w, h) = (200usize, 300usize);
        let t = Treat::new(&Treatment {
            vignette: 1.0,
            ..Treatment::default()
        });
        let mut px = vec![200u8; w * h * 4];
        // Natural pushes contrast; measure the gain on a flat field instead.
        let flat = t.tone[0][200] as f32;
        t.apply(&mut px, w as u32, h as u32, 1920);
        let (mut edge, mut ne, mut centre, mut nc) = (0.0, 0, 0.0, 0);
        for y in 0..h {
            for x in 0..w {
                let nx = (x as f32 + 0.5) / w as f32 * 2.0 - 1.0;
                let ny = (y as f32 + 0.5) / h as f32 * 2.0 - 1.0;
                let v = px[(y * w + x) * 4] as f32 / flat;
                if nx.abs() >= 0.9 || ny.abs() >= 0.9 {
                    edge += v;
                    ne += 1;
                } else if nx.abs() <= 0.2 && ny.abs() <= 0.2 {
                    centre += v;
                    nc += 1;
                }
            }
        }
        let ratio = (edge / ne as f32) / (centre / nc as f32);
        assert!((ratio - 0.59).abs() < 0.03, "edge/centre {ratio}");
    }

    #[test]
    fn recolor_changes_only_its_band_without_fringes() {
        // Reds (340..20, wrapping) become blue (240).
        let band = Band::new(Recolor {
            from: 340.0,
            to: 20.0,
            target: 240.0,
        })
        .unwrap();
        for h in 0..360 {
            let px = rgb_of(h as f32, 0.9, 0.8).map(|c| (c * 255.0).round() as u8);
            let out = band.apply(px);
            let inside = !(20..340).contains(&h);
            if !inside || h == 20 || h == 340 {
                assert_eq!(out, px, "hue {h} is outside the band");
                continue;
            }
            // Every channel lies between the pixel and its full recolour: a two-colour mix,
            // never a hue sweep. Rotating by weight would paint yellows and greens here.
            let full = Band {
                from: 0.0,
                len: 360.0,
                shift: band.shift,
            }
            .apply(px);
            for c in 0..3 {
                let (lo, hi) = (px[c].min(full[c]), px[c].max(full[c]));
                assert!(
                    (lo..=hi).contains(&out[c]),
                    "hue {h}: {px:?} -> {out:?} vs {full:?}"
                );
            }
            if let Some((oh, os, _)) = hsv(out) {
                assert!(
                    os < 0.15 || !(45.0..200.0).contains(&oh),
                    "hue {h} fringed to {oh}"
                );
            }
        }
        // The middle of the band lands on the target.
        let mid = band.apply(rgb_of(0.0, 1.0, 1.0).map(|c| (c * 255.0).round() as u8));
        assert_eq!(hsv(mid).unwrap().0.round(), 240.0);
    }
}
