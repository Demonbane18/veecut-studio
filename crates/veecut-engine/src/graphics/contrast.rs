//! Herk Edit Kit's contrast rule: sample the footage behind text, never white on light.

use std::sync::LazyLock;

use tiny_skia::{FillRule, Path, PixmapMut, Transform};

use super::raster::{DARK, paint};
use crate::project::{Color, Rect};

/// The path and fill luminances are layout work; each frame only samples 64 pixels.
pub(super) struct ContrastGuard {
    path: Path,
    fills: Vec<f32>,
    scrim: Color,
}

impl ContrastGuard {
    pub fn new(path: Path, colors: impl Iterator<Item = Color>) -> Option<Self> {
        let mut fills = Vec::new();
        for color in colors.filter(|c| c.a > 0) {
            let value = luminance(color.r, color.g, color.b);
            if !fills.contains(&value) {
                fills.push(value);
            }
        }
        if fills.is_empty() {
            return None;
        }
        // Dark text needs a light backdrop; mixed light/dark copy uses a middle gray.
        // All candidates are nearly opaque so video cannot erase their contrast.
        let scrim = [
            DARK,
            Color::rgba(245, 247, 250, 238),
            Color::rgba(117, 120, 125, 250),
        ]
        .into_iter()
        .max_by(|a, b| {
            let score = |c: Color| {
                fills
                    .iter()
                    .map(|f| ratio(*f, luminance(c.r, c.g, c.b)))
                    .fold(f32::INFINITY, f32::min)
            };
            score(*a).total_cmp(&score(*b))
        })?;
        Some(Self { path, fills, scrim })
    }

    pub fn draw(&self, target: &mut PixmapMut<'_>, region: Rect, transform: Transform, alpha: f32) {
        if needs_scrim(sample_luminance(target, region), &self.fills) {
            let color = self
                .scrim
                .with_alpha((f32::from(self.scrim.a) * alpha).round() as u8);
            target.fill_path(
                &self.path,
                &paint(color),
                FillRule::Winding,
                transform,
                None,
            );
        }
    }
}

fn luminance(r: u8, g: u8, b: u8) -> f32 {
    static LINEAR: LazyLock<[f32; 256]> = LazyLock::new(|| {
        std::array::from_fn(|index| {
            let c = index as f32 / 255.0;
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        })
    });
    0.2126 * LINEAR[usize::from(r)]
        + 0.7152 * LINEAR[usize::from(g)]
        + 0.0722 * LINEAR[usize::from(b)]
}

fn ratio(a: f32, b: f32) -> f32 {
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

fn needs_scrim(background: f32, fills: &[f32]) -> bool {
    fills.iter().any(|fill| ratio(*fill, background) < 3.0)
}

pub(super) fn light_picture(target: &PixmapMut<'_>, region: Rect) -> bool {
    sample_luminance(target, region) > 0.45
}

fn sample_luminance(target: &PixmapMut<'_>, region: Rect) -> f32 {
    let (width, height) = (target.width() as usize, target.height() as usize);
    let pixels = target.as_ref();
    let mut sum = 0.0;
    for row in 0..8 {
        for col in 0..8 {
            let x = (region.x + (col as f32 + 0.5) * region.w / 8.0).clamp(0.0, (width - 1) as f32)
                as usize;
            let y = (region.y + (row as f32 + 0.5) * region.h / 8.0).clamp(0.0, (height - 1) as f32)
                as usize;
            let pixel = &pixels.data()[(y * width + x) * 4..][..4];
            sum += luminance(pixel[0], pixel[1], pixel[2]);
        }
    }
    sum / 64.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use tiny_skia::{PathBuilder, Pixmap};

    #[test]
    fn bright_and_dark_footage_choose_scrim_by_fill_contrast() {
        let region = Rect {
            x: 16.0,
            y: 16.0,
            w: 64.0,
            h: 64.0,
        };
        let mut background = Pixmap::new(96, 96).unwrap();
        background.fill(tiny_skia::Color::WHITE);
        let white = [luminance(255, 255, 255)];
        assert!(needs_scrim(
            sample_luminance(&background.as_mut(), region),
            &white
        ));
        background.fill(tiny_skia::Color::BLACK);
        assert!(!needs_scrim(
            sample_luminance(&background.as_mut(), region),
            &white
        ));
        assert!(needs_scrim(
            luminance(200, 255, 255),
            &[luminance(104, 240, 202)]
        ));
        assert!(needs_scrim(0.0, &[0.0]));
        assert!(!needs_scrim(1.0, &[0.0]));
    }

    #[test]
    fn guard_keeps_dark_user_colors_readable_and_samples_only_its_region() {
        let path =
            PathBuilder::from_rect(tiny_skia::Rect::from_xywh(0.0, 0.0, 32.0, 32.0).unwrap());
        let white = ContrastGuard::new(path.clone(), [Color::WHITE].into_iter()).unwrap();
        let black = ContrastGuard::new(path, [Color::BLACK].into_iter()).unwrap();
        assert_eq!(white.scrim, DARK);
        assert!(luminance(black.scrim.r, black.scrim.g, black.scrim.b) > 0.9);
        let mut image = Pixmap::new(64, 32).unwrap();
        image.fill(tiny_skia::Color::WHITE);
        image.fill_rect(
            tiny_skia::Rect::from_xywh(0.0, 0.0, 32.0, 32.0).unwrap(),
            &paint(Color::BLACK),
            Transform::identity(),
            None,
        );
        assert_eq!(
            sample_luminance(
                &image.as_mut(),
                Rect {
                    x: 0.0,
                    y: 0.0,
                    w: 32.0,
                    h: 32.0
                }
            ),
            0.0
        );
        assert_eq!(
            sample_luminance(
                &image.as_mut(),
                Rect {
                    x: 32.0,
                    y: 0.0,
                    w: 32.0,
                    h: 32.0
                }
            ),
            1.0
        );
    }
}
