//! A numbered contact sheet of candidates, so a person or a vision model can pick by number.

use std::path::Path;

use anyhow::{Context, Result};
use cosmic_text::{
    Attrs, Buffer, Color, Family, FontSystem, Metrics, Shaping, SwashCache, Weight, fontdb,
};
use image::{Rgba, RgbaImage, imageops};

use super::{Candidate, MediaKind, Provider, clip_text, get_bytes};

const INTER: &[u8] = include_bytes!("../../assets/fonts/Inter/Inter-SemiBold.ttf");
const CELL_W: u32 = 320;
const THUMB_H: u32 = 180;
const LABEL_H: u32 = 70;
const PAD: u32 = 10;
const COLS: u32 = 5;
const RED: [u8; 3] = [214, 38, 38];

/// Writes a PNG grid: each thumbnail with its 1-based number in a big badge (red when flagged),
/// the provider, kind and size, the licence and why it is flagged. Missing thumbnails get a grey
/// box.
pub fn contact_sheet(candidates: &[Candidate], out: &Path) -> Result<()> {
    let n = candidates.len().max(1) as u32;
    let cols = n.min(COLS);
    let rows = n.div_ceil(cols);
    let mut sheet = RgbaImage::from_pixel(
        PAD + cols * (CELL_W + PAD),
        PAD + rows * (THUMB_H + LABEL_H + PAD),
        Rgba([20, 20, 20, 255]),
    );
    let thumbs: Vec<Option<RgbaImage>> = std::thread::scope(|s| {
        let jobs: Vec<_> = candidates
            .iter()
            .map(|c| s.spawn(move || thumbnail(&c.thumb_url)))
            .collect();
        jobs.into_iter().map(|j| j.join().ok().flatten()).collect()
    });
    let mut text = Text::new();
    if candidates.is_empty() {
        text.draw(
            &mut sheet,
            "No results",
            PAD + 8,
            PAD + 8,
            24.0,
            [230, 230, 230],
        );
    }
    for (i, (c, thumb)) in candidates.iter().zip(&thumbs).enumerate() {
        let x = PAD + (i as u32 % cols) * (CELL_W + PAD);
        let y = PAD + (i as u32 / cols) * (THUMB_H + LABEL_H + PAD);
        fill(&mut sheet, x, y, CELL_W, THUMB_H, [42, 42, 42]);
        match thumb {
            Some(t) => {
                let scale =
                    (CELL_W as f32 / t.width() as f32).min(THUMB_H as f32 / t.height() as f32);
                let (w, h) = (
                    ((t.width() as f32 * scale) as u32).max(1),
                    ((t.height() as f32 * scale) as u32).max(1),
                );
                let fitted = imageops::resize(t, w, h, imageops::FilterType::Triangle);
                imageops::overlay(
                    &mut sheet,
                    &fitted,
                    i64::from(x + (CELL_W - w) / 2),
                    i64::from(y + (THUMB_H - h) / 2),
                );
            }
            None => text.draw(
                &mut sheet,
                "no preview",
                x + 110,
                y + 80,
                18.0,
                [150, 150, 150],
            ),
        }
        if c.flag {
            frame(&mut sheet, x, y, CELL_W, THUMB_H, RED);
        }
        let number = (i + 1).to_string();
        let badge_w = 18 + 24 * number.len() as u32;
        fill(
            &mut sheet,
            x,
            y,
            badge_w,
            50,
            if c.flag { RED } else { [0, 0, 0] },
        );
        text.draw(&mut sheet, &number, x + 9, y + 2, 40.0, [255, 255, 255]);
        let what = match (c.kind, c.duration > 0.0, c.width > 0) {
            (MediaKind::Video, true, _) => format!("video {:.0} s", c.duration),
            (MediaKind::Video, false, _) => "video".to_string(),
            (MediaKind::Image, _, true) => format!("picture {}x{}", c.width, c.height),
            (MediaKind::Image, _, false) => "picture".to_string(),
        };
        let provider = match c.provider {
            Provider::Commons => "Commons",
            Provider::Archive => "Archive",
            p => p.label(),
        };
        let line1 = format!("{provider} · {what}");
        text.draw(
            &mut sheet,
            &clip_text(&line1, 36),
            x + 2,
            y + THUMB_H + 4,
            16.0,
            [235, 235, 235],
        );
        let green = [150, 210, 150];
        text.draw(
            &mut sheet,
            &clip_text(&c.license, 36),
            x + 2,
            y + THUMB_H + 25,
            16.0,
            if c.flag { [235, 235, 235] } else { green },
        );
        if c.flag {
            let why = format!(
                "FLAGGED: {}",
                c.flag_reason.as_deref().unwrap_or("check the rights")
            );
            text.draw(
                &mut sheet,
                &clip_text(&why, 40),
                x + 2,
                y + THUMB_H + 46,
                15.0,
                RED,
            );
        }
    }
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    sheet
        .save_with_format(out, image::ImageFormat::Png)
        .with_context(|| format!("writing {}", out.display()))
}

fn thumbnail(url: &str) -> Option<RgbaImage> {
    if url.is_empty() {
        return None;
    }
    let bytes = get_bytes(url, 8 * 1024 * 1024).ok()?;
    Some(image::load_from_memory(&bytes).ok()?.to_rgba8())
}

fn fill(img: &mut RgbaImage, x: u32, y: u32, w: u32, h: u32, rgb: [u8; 3]) {
    for py in y..(y + h).min(img.height()) {
        for px in x..(x + w).min(img.width()) {
            img.put_pixel(px, py, Rgba([rgb[0], rgb[1], rgb[2], 255]));
        }
    }
}

fn frame(img: &mut RgbaImage, x: u32, y: u32, w: u32, h: u32, rgb: [u8; 3]) {
    let t = 4;
    fill(img, x, y, w, t, rgb);
    fill(img, x, y + h - t, w, t, rgb);
    fill(img, x, y, t, h, rgb);
    fill(img, x + w - t, y, t, h, rgb);
}

/// Inter SemiBold (bundled, OFL) drawn with cosmic-text, without loading system fonts.
struct Text {
    fonts: FontSystem,
    swash: SwashCache,
}

impl Text {
    fn new() -> Self {
        let mut db = fontdb::Database::new();
        db.load_font_data(INTER.to_vec());
        Text {
            fonts: FontSystem::new_with_locale_and_db("en-US".into(), db),
            swash: SwashCache::new(),
        }
    }

    /// Draws one line with its top-left corner at (x, y).
    fn draw(&mut self, img: &mut RgbaImage, s: &str, x: u32, y: u32, size: f32, rgb: [u8; 3]) {
        let mut b = Buffer::new(&mut self.fonts, Metrics::new(size, size * 1.2));
        b.set_text(
            s,
            &Attrs::new()
                .family(Family::Name("Inter"))
                .weight(Weight::SEMIBOLD),
            Shaping::Advanced,
            None,
        );
        let (w, h) = (img.width() as i32, img.height() as i32);
        b.draw(
            &mut self.fonts,
            &mut self.swash,
            Color::rgb(rgb[0], rgb[1], rgb[2]),
            |gx, gy, gw, gh, color| {
                let a = color.a() as u32;
                if a == 0 {
                    return;
                }
                for py in gy..gy + gh as i32 {
                    for px in gx..gx + gw as i32 {
                        let (px, py) = (px + x as i32, py + y as i32);
                        if px < 0 || py < 0 || px >= w || py >= h {
                            continue;
                        }
                        let p = img.get_pixel_mut(px as u32, py as u32);
                        for (k, c) in [color.r(), color.g(), color.b()].into_iter().enumerate() {
                            p[k] = ((c as u32 * a + p[k] as u32 * (255 - a)) / 255) as u8;
                        }
                    }
                }
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sheet_numbers_every_candidate_without_network() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("sheet.png");
        let mut c = Candidate::new(Provider::Nasa, "x", MediaKind::Video);
        c.license = "public domain".into();
        let mut flagged = c.clone();
        flagged.flag = true;
        flagged.license = "CC BY-NC 2.0".into();
        contact_sheet(&[c.clone(), flagged, c], &out).unwrap();
        let img = image::open(&out).unwrap().to_rgba8();
        assert_eq!(img.width(), PAD + 3 * (CELL_W + PAD));
        // The badge of the flagged second cell is red and has white number pixels on it.
        let x0 = PAD + CELL_W + PAD;
        let badge: Vec<_> = (x0..x0 + 40)
            .flat_map(|x| (PAD..PAD + 50).map(move |y| (x, y)))
            .map(|(x, y)| *img.get_pixel(x, y))
            .collect();
        assert!(badge.iter().any(|p| p.0 == [RED[0], RED[1], RED[2], 255]));
        assert!(
            badge.iter().any(|p| p[0] > 240 && p[1] > 240 && p[2] > 240),
            "the number is drawn"
        );
    }
}
