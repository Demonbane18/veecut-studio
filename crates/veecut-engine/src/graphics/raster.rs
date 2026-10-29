use cosmic_text::{
    Attrs, Buffer, Family, FontSystem, Metrics, Shaping, Style, SwashCache, Weight, Wrap,
};
use tiny_skia::{FillRule, Paint, Path, PathBuilder, Pixmap, Stroke, Transform};

use super::contrast::ContrastGuard;
use crate::project::{Align, Color, Rect, TextPreset};

pub(super) const DARK: Color = Color::rgba(13, 18, 25, 238);
pub(super) const ACCENT: Color = Color::rgb(104, 240, 202);

pub(super) struct Span {
    pub text: String,
    pub color: Color,
    pub scale: f32,
    pub pill: bool,
    pub italic: bool,
    pub chrome: bool,
}

impl Span {
    pub fn new(text: String, color: Color) -> Self {
        Self {
            text,
            color,
            scale: 1.0,
            pill: false,
            italic: false,
            chrome: false,
        }
    }
}

pub(super) struct Spec<'a> {
    pub spans: Vec<Span>,
    pub font: &'a str,
    pub size: f32,
    pub width: f32,
    pub height: f32,
    pub max_lines: usize,
    pub align: Align,
    pub stroke: f32,
    pub shadow: bool,
    pub background: Option<Color>,
    pub edge: bool,
    pub accent: Color,
    pub preset: Option<TextPreset>,
    pub weight: u16,
    pub reveal: bool,
    pub ink: bool,
}

pub(super) struct Layer {
    pub image: Pixmap,
    pub glyphs: Vec<Rect>,
    pub line_bottoms: Vec<f32>,
    pub contrast: Option<ContrastGuard>,
    /// The font size in pixels after shrinking to fit; below the requested size when it was cut.
    pub size: f32,
}

pub(super) fn paint(color: Color) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color_rgba8(color.r, color.g, color.b, color.a);
    p
}

fn rounded_path(rect: Rect, radius: f32) -> Option<Path> {
    let r = radius.min(rect.w / 2.0).min(rect.h / 2.0).max(0.0);
    let (x, y, w, h) = (rect.x, rect.y, rect.w, rect.h);
    let mut p = PathBuilder::new();
    p.move_to(x + r, y);
    p.line_to(x + w - r, y);
    p.quad_to(x + w, y, x + w, y + r);
    p.line_to(x + w, y + h - r);
    p.quad_to(x + w, y + h, x + w - r, y + h);
    p.line_to(x + r, y + h);
    p.quad_to(x, y + h, x, y + h - r);
    p.line_to(x, y + r);
    p.quad_to(x, y, x + r, y);
    p.close();
    p.finish()
}

pub(super) fn rounded(pix: &mut Pixmap, rect: Rect, radius: f32, color: Color) {
    if let Some(path) = rounded_path(rect, radius) {
        pix.fill_path(
            &path,
            &paint(color),
            FillRule::Winding,
            Transform::identity(),
            None,
        );
    }
}

fn shape(fonts: &mut FontSystem, spec: &Spec<'_>, size: f32, width: f32) -> Buffer {
    let mut b = Buffer::new(fonts, Metrics::new(size, size * 1.25));
    b.set_size(Some(width.max(1.0)), None);
    // Words stay whole; `render` shrinks the text until the widest word fits.
    b.set_wrap(Wrap::Word);
    // cosmic-text requires an exact weight for named families, even for static faces.
    let weight = fonts
        .db()
        .faces()
        .filter(|f| f.families.iter().any(|(name, _)| name == spec.font))
        .min_by_key(|f| f.weight.0.abs_diff(spec.weight))
        .map_or(spec.weight, |f| f.weight.0);
    let attrs = Attrs::new()
        .family(Family::Name(spec.font))
        .style(if spec.font == "Playfair Display" {
            Style::Italic
        } else {
            Style::Normal
        })
        .weight(Weight(weight));
    b.set_rich_text(
        spec.spans.iter().enumerate().map(|(i, s)| {
            (
                s.text.as_str(),
                (if s.italic {
                    attrs
                        .clone()
                        .family(Family::Name("Playfair Display"))
                        .style(Style::Italic)
                        .weight(Weight(700))
                } else {
                    attrs.clone()
                })
                .metadata(i)
                .metrics(Metrics::new(size * s.scale, size * 1.25 * s.scale)),
            )
        }),
        &attrs,
        Shaping::Advanced,
        Some(match spec.align {
            Align::Left => cosmic_text::Align::Left,
            Align::Center => cosmic_text::Align::Center,
            Align::Right => cosmic_text::Align::Right,
        }),
    );
    b.shape_until_scroll(fonts, false);
    b
}

/// Cache this complete layer: shaping, glyph rasterization, outlines and cards are cold work.
pub(super) fn render(
    fonts: &mut FontSystem,
    swash: &mut SwashCache,
    spec: Spec<'_>,
) -> Option<Layer> {
    let mut size = spec.size.max(1.0);
    let mut padding = size * 0.38 + size * spec.stroke.clamp(0.0, 0.5);
    let mut b = shape(fonts, &spec, size, spec.width - 2.0 * padding);
    // Fit whole text, never crop it or split a word. Preset line limits include explicit newlines.
    // ponytail: a single unbreakable string wider than ~35 box widths still overflows after the
    // last step; add a glyph-wrap fallback if such text ever shows up.
    for _ in 0..28 {
        let lines = b.layout_runs().count();
        let height = b
            .layout_runs()
            .map(|r| r.line_top + r.line_height)
            .fold(0.0_f32, f32::max);
        let widest = b.layout_runs().map(|r| r.line_w).fold(0.0_f32, f32::max);
        if lines <= spec.max_lines.max(1)
            && height + padding * 2.0 <= spec.height
            && widest <= spec.width - 2.0 * padding
        {
            break;
        }
        size *= 0.88;
        padding = size * (0.38 + spec.stroke.clamp(0.0, 0.5));
        b = shape(fonts, &spec, size, spec.width - 2.0 * padding);
    }
    let text_width = b.layout_runs().map(|r| r.line_w).fold(1.0_f32, f32::max);
    // Re-layout at the measured width so cards hug text and alignment remains real.
    b.set_size(Some((text_width + 1.0).max(1.0)), None);
    b.shape_until_scroll(fonts, false);
    let text_height = b
        .layout_runs()
        .map(|r| r.line_top + r.line_height)
        .fold(size, f32::max);
    let width = (text_width + padding * 2.0 + 1.0).ceil().max(1.0) as u32;
    let height = (text_height + padding * 2.0).ceil().max(1.0) as u32;
    let mut image = Pixmap::new(width, height)?;
    let outer = Rect {
        x: 1.0,
        y: 1.0,
        w: width as f32 - 2.0,
        h: height as f32 - 2.0,
    };
    let radius = if spec.preset == Some(TextPreset::Label) {
        outer.h / 2.0
    } else {
        size * 0.22
    };
    if let Some(background) = spec.background {
        if spec.edge {
            rounded(&mut image, outer, radius, Color::rgba(240, 245, 250, 185));
        }
        let edge = if spec.edge {
            (size * 0.025).max(1.0)
        } else {
            0.0
        };
        rounded(
            &mut image,
            Rect {
                x: outer.x + edge,
                y: outer.y + edge,
                w: outer.w - edge * 2.0,
                h: outer.h - edge * 2.0,
            },
            radius,
            background,
        );
    }
    match spec.preset {
        Some(TextPreset::LowerThird | TextPreset::Quote | TextPreset::List) => rounded(
            &mut image,
            Rect {
                x: size * 0.1,
                y: padding * 0.65,
                w: size * 0.065,
                h: text_height + padding * 0.7,
            },
            size * 0.03,
            spec.accent,
        ),
        Some(TextPreset::Title | TextPreset::Counter | TextPreset::Cta) => rounded(
            &mut image,
            Rect {
                x: width as f32 * 0.32,
                y: height as f32 - padding * 0.52,
                w: width as f32 * 0.36,
                h: size * 0.055,
            },
            size * 0.025,
            spec.accent,
        ),
        _ => {}
    }
    let mut outlines = PathBuilder::new();
    let mut glyphs = Vec::new();
    let mut line_bottoms = Vec::new();
    for run in b.layout_runs() {
        if line_bottoms.len() <= run.line_i {
            line_bottoms.resize(run.line_i + 1, 0.0);
        }
        line_bottoms[run.line_i] = padding + run.line_top + run.line_height + size * 0.12;
        // Adjacent glyphs sharing a marked span form one rounded pill, including ligatures.
        let mut pill: Option<(usize, f32, f32)> = None;
        let flush = |image: &mut Pixmap, group: Option<(usize, f32, f32)>| {
            if let Some((_, left, right)) = group {
                rounded(
                    image,
                    Rect {
                        x: left + padding - size * 0.075,
                        y: padding + run.line_top + size * 0.07,
                        w: right - left + size * 0.15,
                        h: run.line_height - size * 0.08,
                    },
                    size * 0.16,
                    spec.accent,
                );
            }
        };
        for glyph in run.glyphs {
            let marked = spec.spans[glyph.metadata].pill;
            if marked {
                if let Some((id, left, right)) = pill {
                    if id == glyph.metadata {
                        pill = Some((id, left.min(glyph.x), right.max(glyph.x + glyph.w)));
                    } else {
                        flush(&mut image, pill);
                        pill = Some((glyph.metadata, glyph.x, glyph.x + glyph.w));
                    }
                } else {
                    pill = Some((glyph.metadata, glyph.x, glyph.x + glyph.w));
                }
            } else {
                flush(&mut image, pill.take());
            }
            glyphs.push((
                run.line_i,
                glyph.start,
                Rect {
                    x: padding + glyph.x - size * 0.14,
                    y: padding + run.line_top - size * 0.12,
                    w: glyph.w + size * 0.28,
                    h: run.line_height + size * 0.24,
                },
            ));
            // Dark pill lettering needs no dark outline: it would fill the letter counters.
            if spec.spans[glyph.metadata].color.a == 0
                || marked
                || (spec.stroke <= 0.0 && !spec.shadow)
            {
                continue;
            }
            let physical = glyph.physical((padding, run.line_y + padding), 1.0);
            if let Some(commands) = swash.get_outline_commands(fonts, physical.cache_key) {
                for cmd in commands {
                    let (x, y) = (physical.x as f32, physical.y as f32);
                    match cmd {
                        cosmic_text::Command::MoveTo(p) => outlines.move_to(x + p.x, y - p.y),
                        cosmic_text::Command::LineTo(p) => outlines.line_to(x + p.x, y - p.y),
                        cosmic_text::Command::QuadTo(p, q) => {
                            outlines.quad_to(x + p.x, y - p.y, x + q.x, y - q.y)
                        }
                        cosmic_text::Command::CurveTo(p, q, r) => {
                            outlines.cubic_to(x + p.x, y - p.y, x + q.x, y - q.y, x + r.x, y - r.y)
                        }
                        cosmic_text::Command::Close => outlines.close(),
                    }
                }
            }
        }
        flush(&mut image, pill);
    }
    if let Some(path) = outlines.finish() {
        if spec.shadow
            && (spec.reveal
                || matches!(
                    spec.preset,
                    Some(TextPreset::Chapter | TextPreset::Shock | TextPreset::Mega)
                )
                || spec.ink)
        {
            let mut shadow = Pixmap::new(width, height)?;
            shadow.fill_path(
                &path,
                &paint(if spec.ink {
                    Color::rgba(255, 255, 255, 210)
                } else {
                    Color::rgba(0, 0, 0, 180)
                }),
                FillRule::Winding,
                Transform::from_translate(0.0, size * 0.035),
                None,
            );
            let shadow = blurred(&shadow, (size * 0.055).max(1.0))?;
            image.draw_pixmap(
                0,
                0,
                shadow.as_ref(),
                &tiny_skia::PixmapPaint::default(),
                Transform::identity(),
                None,
            );
            if spec.reveal && !spec.ink {
                let mut bloom = Pixmap::new(width, height)?;
                bloom.fill_path(
                    &path,
                    &paint(Color::rgba(255, 255, 255, 50)),
                    FillRule::Winding,
                    Transform::identity(),
                    None,
                );
                let bloom = blurred(&bloom, (size * 0.035).max(1.0))?;
                image.draw_pixmap(
                    0,
                    0,
                    bloom.as_ref(),
                    &tiny_skia::PixmapPaint::default(),
                    Transform::identity(),
                    None,
                );
            }
        } else if spec.shadow {
            let transform = Transform::from_translate(size * 0.025, size * 0.045);
            image.fill_path(
                &path,
                &paint(Color::rgba(0, 0, 0, 145)),
                FillRule::Winding,
                transform,
                None,
            );
            image.stroke_path(
                &path,
                &paint(Color::rgba(0, 0, 0, 100)),
                &Stroke {
                    width: size * 0.055,
                    ..Stroke::default()
                },
                transform,
                None,
            );
        }
        if spec.stroke > 0.0 {
            image.stroke_path(
                &path,
                &paint(Color::rgba(7, 10, 14, 245)),
                &Stroke {
                    width: size * spec.stroke.clamp(0.0, 0.5) * 2.0,
                    line_join: tiny_skia::LineJoin::Round,
                    ..Stroke::default()
                },
                Transform::identity(),
                None,
            );
        }
    }
    // Raster glyphs keep color-emoji fallback; outlines above are drawn only once per layout.
    for run in b.layout_runs() {
        for glyph in run.glyphs {
            let physical = glyph.physical((padding, run.line_y + padding), 1.0);
            let span = &spec.spans[glyph.metadata];
            let color = if spec.ink && span.color == Color::WHITE {
                Color::rgb(17, 17, 17)
            } else {
                span.color
            };
            if color.a == 0 {
                continue;
            }
            swash.with_pixels(
                fonts,
                physical.cache_key,
                cosmic_text::Color::rgba(color.r, color.g, color.b, color.a),
                |x, y, pixel| {
                    let (x, y) = (x + physical.x, y + physical.y);
                    if x < 0 || y < 0 || x >= width as i32 || y >= height as i32 {
                        return;
                    }
                    let a = (u16::from(pixel.a()) * u16::from(color.a) / 255) as u8;
                    let index = (y as usize * width as usize + x as usize) * 4;
                    let dst = &mut image.data_mut()[index..index + 4];
                    let rgb = if span.chrome && !spec.ink {
                        let p = ((y as f32 - padding) / text_height).clamp(0.0, 1.0);
                        let grey = if p < 0.45 {
                            180.0 + 75.0 * p / 0.45
                        } else {
                            255.0 - 130.0 * (p - 0.45) / 0.55
                        } as u8;
                        [grey; 3]
                    } else {
                        [pixel.r(), pixel.g(), pixel.b()]
                    };
                    for (d, c) in dst[..3].iter_mut().zip(rgb) {
                        *d = ((u16::from(c) * u16::from(a)
                            + u16::from(*d) * u16::from(255 - a)
                            + 127)
                            / 255) as u8;
                    }
                    dst[3] = a.saturating_add((u16::from(dst[3]) * u16::from(255 - a) / 255) as u8);
                },
            );
        }
    }
    glyphs.sort_by_key(|(line, start, _)| (*line, *start));
    // Red display tiers sit directly on the picture, including behind a cut-out head.
    let contrast = if !spec.ink
        && !matches!(
            spec.preset,
            Some(TextPreset::Shock | TextPreset::Mega | TextPreset::BlackCard)
        )
        && spec.background.is_none_or(|color| color.a == 0)
    {
        rounded_path(outer, radius).and_then(|path| {
            ContrastGuard::new(
                path,
                spec.spans
                    .iter()
                    .filter(|span| !span.pill && !span.text.trim().is_empty())
                    .map(|span| span.color),
            )
        })
    } else {
        None
    };
    Some(Layer {
        image,
        glyphs: glyphs.into_iter().map(|(_, _, rect)| rect).collect(),
        line_bottoms,
        contrast,
        size,
    })
}

/// Blur premultiplied RGBA, so transparent pixels cannot leave coloured fringes.
pub(super) fn blurred(image: &Pixmap, sigma: f32) -> Option<Pixmap> {
    use image::imageops::{FilterType, blur, resize};
    let rgba = image::RgbaImage::from_raw(image.width(), image.height(), image.data().to_vec())?;
    let blurred = if sigma >= 2.0 {
        let small = resize(
            &rgba,
            image.width().div_ceil(4),
            image.height().div_ceil(4),
            FilterType::Triangle,
        );
        resize(
            &blur(&small, sigma / 4.0),
            image.width(),
            image.height(),
            FilterType::Triangle,
        )
    } else {
        blur(&rgba, sigma)
    };
    Pixmap::from_vec(
        blurred.into_raw(),
        tiny_skia::IntSize::from_wh(image.width(), image.height())?,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_families_keep_their_face_when_weight_differs() {
        let mut graphics = crate::graphics::Graphics::new();
        for font in [
            "Anton",
            "Abril Fatface",
            "Bebas Neue",
            "Archivo Black",
            "Inter",
            "Playfair Display",
            "Bodoni Moda",
            "Montserrat",
        ] {
            let spec = Spec {
                spans: vec![Span::new("Story".into(), Color::WHITE)],
                font,
                size: 60.0,
                width: 800.0,
                height: 300.0,
                max_lines: 2,
                align: Align::Center,
                stroke: 0.0,
                shadow: false,
                background: None,
                edge: false,
                accent: ACCENT,
                preset: None,
                weight: if font == "Montserrat" { 900 } else { 800 },
                reveal: false,
                ink: false,
            };
            let buffer = shape(&mut graphics.fonts, &spec, 60.0, 800.0);
            for glyph in buffer.layout_runs().flat_map(|run| run.glyphs) {
                let face = graphics.fonts.db().face(glyph.font_id).unwrap();
                assert!(
                    face.families.iter().any(|(name, _)| name == font),
                    "{font} silently fell back to {:?}",
                    face.families
                );
                if font == "Montserrat" {
                    assert_eq!(face.weight.0, 900);
                }
                if font == "Playfair Display" {
                    assert_eq!(face.style, cosmic_text::fontdb::Style::Italic);
                }
            }
        }
    }

    #[test]
    fn long_words_shrink_to_fit_instead_of_breaking() {
        let mut graphics = crate::graphics::Graphics::new();
        // A 9:16 caption at size 0.06: "OPPORTUNITY" alone is wider than the caption box.
        let spec = Spec {
            spans: vec![Span::new("OPPORTUNITY TO FLY".into(), Color::WHITE)],
            font: "Montserrat",
            size: 115.0,
            width: 778.0,
            height: 1296.0,
            max_lines: 2,
            align: Align::Center,
            stroke: 0.12,
            shadow: true,
            background: None,
            edge: false,
            accent: ACCENT,
            preset: None,
            weight: 800,
            reveal: false,
            ink: false,
        };
        let layer = render(&mut graphics.fonts, &mut graphics.swash, spec).unwrap();
        let first_line = layer
            .glyphs
            .iter()
            .filter(|glyph| glyph.y == layer.glyphs[0].y)
            .count();
        assert!(
            first_line >= "OPPORTUNITY".len(),
            "the word was split after {first_line} letters"
        );
        assert!(layer.image.width() <= 779);
    }
}
