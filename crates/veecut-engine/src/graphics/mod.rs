//! Shared Rust text renderer for preview and export. Layouts and finished layers are cached.
//!
//! Caption lead, phone-safe region and edged caption boxes follow Herk Edit Kit (MIT).
//! Caption choices follow free247editor (MIT). Font files are OFL; licenses sit beside them.
//! Montserrat comes from JulietaUla/Montserrat/fonts/ttf; other families from google/fonts/ofl.
//! Inter is a static instance of that upstream font at wght=600, opsz=14 (fontTools 4.68).
//! Run the ignored `visual_proof` test for PNG samples and a warm-frame draw benchmark.

mod raster;
#[cfg(test)]
mod tests;
mod timing;

use std::{
    collections::{BTreeSet, HashMap, hash_map::DefaultHasher},
    hash::{Hash, Hasher},
    ops::Range,
    sync::Arc,
};

use cosmic_text::{FontSystem, SwashCache, fontdb::Source};
use tiny_skia::{
    FillRule, FilterQuality, Mask, PathBuilder, PixmapMut, PixmapPaint, Stroke, Transform,
};

use crate::project::{
    Align, Anim, Canvas, CaptionMode, CaptionStyle, Color, Item, ItemBody, Point, Project, Rect,
    Text, TextPreset, TrackKind, Word,
};
use raster::{ACCENT, DARK, Layer, Span, Spec};
use timing::{Motion, accents, active_caption, active_word, caption_pages, motion};

const FONTS: &[&[u8]] = &[
    include_bytes!("../../assets/fonts/Montserrat/Montserrat-ExtraBold.ttf"),
    include_bytes!("../../assets/fonts/Montserrat/Montserrat-Bold.ttf"),
    include_bytes!("../../assets/fonts/AbrilFatface/AbrilFatface-Regular.ttf"),
    include_bytes!("../../assets/fonts/Anton/Anton-Regular.ttf"),
    include_bytes!("../../assets/fonts/BebasNeue/BebasNeue-Regular.ttf"),
    include_bytes!("../../assets/fonts/Poppins/Poppins-Bold.ttf"),
    include_bytes!("../../assets/fonts/ArchivoBlack/ArchivoBlack-Regular.ttf"),
    include_bytes!("../../assets/fonts/Inter/Inter-SemiBold.ttf"),
];

/// Shaping and raster caches shared across frames. Keep one instance per playback/export.
pub struct Graphics {
    fonts: FontSystem,
    swash: SwashCache,
    families: Vec<String>,
    layers: HashMap<u64, Layer>,
    pages: HashMap<u64, Vec<(Range<usize>, usize)>>,
    bytes: usize,
    mask: Option<Mask>,
}

impl Graphics {
    /// Loads bundled OFL faces and system fonts for non-Latin and emoji fallback.
    pub fn new() -> Self {
        let mut fonts = FontSystem::new();
        // Prefer our exact bundled faces over different versions installed on the host.
        let bundled = [
            "Montserrat",
            "Abril Fatface",
            "Anton",
            "Bebas Neue",
            "Poppins",
            "Archivo Black",
            "Inter",
        ];
        let replaced: Vec<_> = fonts
            .db()
            .faces()
            .filter(|face| {
                face.families
                    .iter()
                    .any(|(name, _)| bundled.contains(&name.as_str()))
            })
            .map(|face| face.id)
            .collect();
        let db = fonts.db_mut();
        for id in replaced {
            db.remove_face(id);
        }
        for bytes in FONTS {
            db.load_font_source(Source::Binary(Arc::new(*bytes)));
        }
        let families = fonts
            .db()
            .faces()
            .flat_map(|face| face.families.iter().map(|(name, _)| name.clone()))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        Self {
            fonts,
            swash: SwashCache::new(),
            families,
            layers: HashMap::new(),
            pages: HashMap::new(),
            bytes: 0,
            mask: None,
        }
    }

    /// Draws visible text and captions at timeline time `t`, scaled to the pixmap size.
    pub fn draw(&mut self, project: &Project, t: f64, pixmap: &mut PixmapMut<'_>) {
        if !t.is_finite() {
            return;
        }
        let safe = safe_area(&project.canvas);
        for track in &project.tracks {
            if track.hidden {
                continue;
            }
            match track.kind {
                TrackKind::Captions if project.captions.mode != CaptionMode::Off => {
                    if let Some((item, words)) = active_caption(&track.items, t, project.canvas.fps)
                    {
                        self.caption(
                            item,
                            words,
                            &project.captions,
                            t,
                            project.canvas.fps,
                            safe,
                            pixmap,
                        );
                    }
                }
                TrackKind::Text | TrackKind::Overlay => {
                    for item in &track.items {
                        if item.contains(t)
                            && let ItemBody::Text(text) = &item.body
                        {
                            self.text(item, text, t, safe, pixmap);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    /// Usable bundled and system font family names, sorted without duplicates.
    pub fn font_families(&self) -> Vec<String> {
        self.families.clone()
    }

    fn family<'a>(&'a self, requested: &'a str) -> &'a str {
        self.families
            .iter()
            .find(|name| name.eq_ignore_ascii_case(requested))
            .map(String::as_str)
            .unwrap_or("Montserrat")
    }

    fn cache(&mut self, key: u64, layer: Layer) {
        // ponytail: 64 MiB raster cache flushes as a batch; use LRU if seeking churn becomes costly.
        if self.bytes + layer.image.data().len() > 64 * 1024 * 1024 || self.layers.len() >= 128 {
            self.layers.clear();
            self.bytes = 0;
            self.swash.image_cache.clear();
            self.swash.outline_command_cache.clear();
        }
        self.bytes += layer.image.data().len();
        self.layers.insert(key, layer);
    }

    #[allow(clippy::too_many_arguments)]
    fn caption(
        &mut self,
        item: &Item,
        words: &[Word],
        style: &CaptionStyle,
        t: f64,
        fps: f64,
        safe: Rect,
        target: &mut PixmapMut<'_>,
    ) {
        if words.is_empty() {
            return;
        }
        let mut hash = DefaultHasher::new();
        item.id.hash(&mut hash);
        style.max_chars.hash(&mut hash);
        style.uppercase.hash(&mut hash);
        for w in words {
            w.text.hash(&mut hash);
            w.start.to_bits().hash(&mut hash);
            w.end.to_bits().hash(&mut hash);
            w.emphasis.hash(&mut hash);
        }
        let page_key = hash.finish();
        if !self.pages.contains_key(&page_key) {
            if self.pages.len() >= 256 {
                self.pages.clear();
            }
            self.pages.insert(
                page_key,
                caption_pages(words, usize::from(style.max_chars), style.uppercase),
            );
        }
        let relative = t - item.start;
        let pages = &self.pages[&page_key];
        let (range, line_break) = pages
            .iter()
            .rev()
            .find(|(r, _)| relative >= words[r.start].start - 2.0 / fps.max(1.0))
            .unwrap_or(&pages[0]);
        let (range, line_break) = (range.clone(), *line_break);
        let spoken = active_word(&words[range.clone()], relative) + range.start;
        let shown = if style.mode == CaptionMode::WordPop {
            spoken..spoken + 1
        } else {
            range.clone()
        };
        (style.mode as u8).hash(&mut hash);
        shown.start.hash(&mut hash);
        shown.end.hash(&mut hash);
        if style.mode == CaptionMode::Highlight {
            spoken.hash(&mut hash);
        }
        style.font.hash(&mut hash);
        style.size.to_bits().hash(&mut hash);
        style.color.hash(&mut hash);
        style.highlight.hash(&mut hash);
        style.stroke.to_bits().hash(&mut hash);
        style.shadow.hash(&mut hash);
        style.boxed.hash(&mut hash);
        target.width().hash(&mut hash);
        target.height().hash(&mut hash);
        let key = hash.finish();
        if !self.layers.contains_key(&key) {
            let mut spans = Vec::new();
            for index in shown.clone() {
                let word = &words[index];
                if index > shown.start {
                    spans.push(Span::new(
                        if index == line_break { "\n" } else { " " }.into(),
                        style.color,
                    ));
                }
                let pill = style.mode == CaptionMode::Pills && word.emphasis;
                let color = if pill {
                    DARK
                } else if word.emphasis || (style.mode == CaptionMode::Highlight && index == spoken)
                {
                    style.highlight
                } else {
                    style.color
                };
                let text = if style.uppercase {
                    word.text.to_uppercase()
                } else {
                    word.text.clone()
                };
                spans.push(Span {
                    text,
                    color,
                    scale: 1.0,
                    pill,
                });
            }
            let font = self.family(&style.font).to_owned();
            let subtitle = style.mode == CaptionMode::Subtitle;
            let size = fraction(style.size, 0.033)
                * target.height() as f32
                * if subtitle { 0.72 } else { 1.0 };
            let spec = Spec {
                spans,
                font: &font,
                size,
                width: safe.w * target.width() as f32 / 1.08,
                height: safe.h * target.height() as f32 / 1.08,
                max_lines: 2,
                align: Align::Center,
                stroke: style.stroke,
                shadow: style.shadow,
                background: (style.boxed || subtitle).then_some(if style.boxed {
                    DARK
                } else {
                    Color::rgba(10, 14, 20, 190)
                }),
                edge: style.boxed,
                accent: style.highlight,
                preset: None,
                weight: 800,
            };
            if let Some(layer) = raster::render(&mut self.fonts, &mut self.swash, spec) {
                self.cache(key, layer);
            }
        }
        let mut m = Motion::default();
        if style.mode == CaptionMode::WordPop {
            m = motion(
                Anim::Pop,
                ((relative - words[spoken].start) / 0.12) as f32,
                false,
            );
            // The word is already readable during Herk's two-frame lead; only its size pops.
            m.alpha = 1.0;
        }
        let position = Point::new(
            safe.x + safe.w / 2.0,
            if style.mode == CaptionMode::Subtitle && style.position == 0.70 {
                safe.y + safe.h - 0.06
            } else {
                style.position
            },
        );
        if let Some(layer) = self.layers.get(&key) {
            draw_layer(
                layer,
                target,
                &mut self.mask,
                position,
                safe,
                m,
                Anim::None,
                usize::MAX,
            );
        }
    }

    fn text(&mut self, item: &Item, text: &Text, t: f64, safe: Rect, target: &mut PixmapMut<'_>) {
        if text.text.trim().is_empty() {
            return;
        }
        let defaults = Preset::for_kind(text.preset, safe);
        let mut hash = DefaultHasher::new();
        item.id.hash(&mut hash);
        text.text.hash(&mut hash);
        (text.preset as u8).hash(&mut hash);
        text.style.font.hash(&mut hash);
        text.style.size.map(f32::to_bits).hash(&mut hash);
        text.style.color.hash(&mut hash);
        text.style.accent.hash(&mut hash);
        text.style.background.hash(&mut hash);
        text.style.stroke.map(f32::to_bits).hash(&mut hash);
        text.style.uppercase.hash(&mut hash);
        text.style.align.map(|a| a as u8).hash(&mut hash);
        target.width().hash(&mut hash);
        target.height().hash(&mut hash);
        let key = hash.finish();
        if !self.layers.contains_key(&key) {
            let color = text.style.color.unwrap_or(defaults.color);
            let accent = text.style.accent.unwrap_or(ACCENT);
            let upper = text.style.uppercase.unwrap_or(defaults.uppercase);
            let content = match text.preset {
                TextPreset::Quote => format!("“{}”", text.text),
                TextPreset::List => text
                    .text
                    .lines()
                    .map(|s| format!("• {s}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
                _ => text.text.clone(),
            };
            let mut spans = Vec::new();
            let mut line = 0;
            for (value, accented) in accents(&content) {
                for s in value.split_inclusive('\n') {
                    let mut span = Span::new(
                        if upper {
                            s.to_uppercase()
                        } else {
                            s.to_owned()
                        },
                        if accented { accent } else { color },
                    );
                    if text.preset == TextPreset::LowerThird && line > 0 {
                        span.scale = 0.57;
                    }
                    spans.push(span);
                    if s.ends_with('\n') {
                        line += 1;
                    }
                }
            }
            let font = self
                .family(text.style.font.as_deref().unwrap_or(defaults.font))
                .to_owned();
            let size = fraction(text.style.size.unwrap_or(defaults.size), defaults.size)
                * target.height() as f32;
            let spec = Spec {
                spans,
                font: &font,
                size,
                width: safe.w * target.width() as f32 / 1.08,
                height: safe.h * target.height() as f32 / 1.08,
                max_lines: defaults.lines.max(content.lines().count()),
                align: text.style.align.unwrap_or(defaults.align),
                stroke: text.style.stroke.unwrap_or(defaults.stroke),
                shadow: true,
                background: text.style.background.or(defaults.background),
                edge: matches!(
                    text.preset,
                    TextPreset::Card | TextPreset::Callout | TextPreset::Cta
                ),
                accent,
                preset: Some(text.preset),
                weight: defaults.weight,
            };
            if let Some(layer) = raster::render(&mut self.fonts, &mut self.swash, spec) {
                self.cache(key, layer);
            }
        }
        let duration = item.duration.min(0.6) as f32 / 2.0;
        let entering = ((t - item.start) as f32 / duration.max(0.001)).clamp(0.0, 1.0);
        let leaving =
            ((t - item.end() + f64::from(duration)) as f32 / duration.max(0.001)).clamp(0.0, 1.0);
        let (anim, progress, exit) = if leaving > 0.0 {
            (text.anim_out, leaving, true)
        } else {
            (text.anim_in, entering, false)
        };
        let mut m = motion(anim, progress, exit);
        // Lower thirds always enter from the left; explicit none still means no motion.
        if text.preset == TextPreset::LowerThird && anim == Anim::Fade && !exit {
            m.x = -(1.0 - entering).powi(3) * 3.0;
        }
        if text.preset == TextPreset::Kinetic && anim == Anim::Fade && !exit {
            m.scale = 1.0 + 0.28 * (1.0 - entering).powi(3);
        }
        let mut position = text.position.unwrap_or(defaults.position);
        let bounds = if text.position.is_some() && text.preset != TextPreset::Callout {
            Rect::FULL
        } else {
            safe
        };
        if text.preset == TextPreset::Callout {
            let point = text
                .position
                .unwrap_or(Point::new(safe.x + safe.w * 0.67, safe.y + safe.h * 0.56));
            position = Point::new(point.x - safe.w * 0.20, point.y - safe.h * 0.14);
            if let Some(layer) = self.layers.get(&key) {
                let (cx, cy) = center(layer, target, position, safe, m.scale);
                let mut path = PathBuilder::new();
                path.move_to(cx, cy + layer.image.height() as f32 * 0.4);
                path.line_to(
                    point.x * target.width() as f32,
                    point.y * target.height() as f32,
                );
                if let Some(path) = path.finish() {
                    let accent = text
                        .style
                        .accent
                        .unwrap_or(ACCENT)
                        .with_alpha((255.0 * m.alpha) as u8);
                    target.stroke_path(
                        &path,
                        &raster::paint(accent),
                        &Stroke {
                            width: (target.height() as f32 * 0.002).max(1.0),
                            ..Stroke::default()
                        },
                        Transform::identity(),
                        None,
                    );
                }
            }
        }
        let visible_lines = if text.preset == TextPreset::List {
            (((t - item.start) / item.duration.max(0.001) * text.text.lines().count() as f64)
                .floor() as usize
                + 1)
            .min(text.text.lines().count())
        } else {
            usize::MAX
        };
        if let Some(layer) = self.layers.get(&key) {
            draw_layer(
                layer,
                target,
                &mut self.mask,
                position,
                bounds,
                m,
                anim,
                visible_lines,
            );
        }
    }
}

impl Default for Graphics {
    fn default() -> Self {
        Self::new()
    }
}

fn fraction(value: f32, fallback: f32) -> f32 {
    if value.is_finite() {
        value.clamp(0.001, 0.5)
    } else {
        fallback
    }
}

/// Normalized safe region. Herk Edit Kit reserves x=90..930, y=200..1600 at 1080×1920.
pub fn safe_area(canvas: &Canvas) -> Rect {
    let aspect = canvas.width as f32 / canvas.height.max(1) as f32;
    if aspect < 0.7 {
        Rect {
            x: 90.0 / 1080.0,
            y: 200.0 / 1920.0,
            w: 840.0 / 1080.0,
            h: 1400.0 / 1920.0,
        }
    } else {
        Rect {
            x: 0.05,
            y: 0.05,
            w: 0.90,
            h: if aspect <= 1.05 { 0.85 } else { 0.90 },
        }
    }
}

struct Preset {
    font: &'static str,
    size: f32,
    color: Color,
    background: Option<Color>,
    uppercase: bool,
    align: Align,
    position: Point,
    lines: usize,
    stroke: f32,
    weight: u16,
}

impl Preset {
    fn for_kind(kind: TextPreset, safe: Rect) -> Self {
        let mut p = Self {
            font: "Montserrat",
            size: 0.055,
            color: Color::WHITE,
            background: None,
            uppercase: false,
            align: Align::Center,
            position: Point::new(safe.x + safe.w / 2.0, safe.y + safe.h * 0.44),
            lines: 3,
            stroke: 0.025,
            weight: 800,
        };
        match kind {
            TextPreset::Hook => {
                p.font = "Anton";
                p.size = 0.085;
                p.uppercase = true;
                p.position.y = safe.y + safe.h * 0.25;
            }
            TextPreset::Title => {
                p.font = "Montserrat";
                p.size = 0.065;
                p.weight = 700;
            }
            TextPreset::Card => {
                p.font = "Poppins";
                p.size = 0.043;
                p.background = Some(DARK);
                p.weight = 700;
            }
            TextPreset::LowerThird => {
                p.size = 0.037;
                p.align = Align::Left;
                p.background = Some(DARK);
                p.position = Point::new(safe.x + safe.w * 0.35, safe.y + safe.h * 0.83);
                p.lines = 2;
                p.weight = 700;
            }
            TextPreset::Callout => {
                p.font = "Inter";
                p.size = 0.030;
                p.background = Some(DARK);
                p.lines = 2;
                p.weight = 600;
            }
            TextPreset::Counter => {
                p.font = "Bebas Neue";
                p.size = 0.115;
                p.uppercase = true;
                p.color = ACCENT;
                p.lines = 2;
            }
            TextPreset::Label => {
                p.font = "Inter";
                p.size = 0.025;
                p.color = DARK;
                p.background = Some(ACCENT);
                p.uppercase = true;
                p.position.y = safe.y + safe.h * 0.15;
                p.stroke = 0.0;
                p.weight = 600;
            }
            TextPreset::Quote => {
                p.font = "Abril Fatface";
                p.size = 0.052;
                p.align = Align::Left;
                p.lines = 5;
            }
            TextPreset::List => {
                p.font = "Poppins";
                p.size = 0.038;
                p.align = Align::Left;
                p.background = Some(DARK);
                p.lines = 12;
                p.weight = 700;
            }
            TextPreset::Kinetic => {
                p.font = "Archivo Black";
                p.size = 0.15;
                p.color = ACCENT;
                p.uppercase = true;
                p.lines = 2;
            }
            TextPreset::Cta => {
                p.size = 0.060;
                p.background = Some(DARK);
                p.position.y = safe.y + safe.h * 0.53;
                p.uppercase = true;
            }
        }
        p
    }
}

fn center(
    layer: &Layer,
    target: &PixmapMut<'_>,
    position: Point,
    bounds: Rect,
    scale: f32,
) -> (f32, f32) {
    let (w, h) = (target.width() as f32, target.height() as f32);
    let (half_w, half_h) = (
        layer.image.width() as f32 * scale / 2.0,
        layer.image.height() as f32 * scale / 2.0,
    );
    let x = if position.x.is_finite() {
        position.x * w
    } else {
        w / 2.0
    };
    let y = if position.y.is_finite() {
        position.y * h
    } else {
        h / 2.0
    };
    (
        x.clamp(
            bounds.x * w + half_w,
            ((bounds.x + bounds.w) * w - half_w).max(bounds.x * w + half_w),
        ),
        y.clamp(
            bounds.y * h + half_h,
            ((bounds.y + bounds.h) * h - half_h).max(bounds.y * h + half_h),
        ),
    )
}

#[allow(clippy::too_many_arguments)]
fn draw_layer(
    layer: &Layer,
    target: &mut PixmapMut<'_>,
    mask: &mut Option<Mask>,
    position: Point,
    bounds: Rect,
    m: Motion,
    anim: Anim,
    visible_lines: usize,
) {
    if m.alpha <= 0.0 || m.reveal <= 0.0 {
        return;
    }
    let (width, height) = (layer.image.width() as f32, layer.image.height() as f32);
    let scale = m
        .scale
        .min(bounds.w * target.width() as f32 / width)
        .min(bounds.h * target.height() as f32 / height);
    let shifted = Point::new(position.x + m.x * 0.08, position.y + m.y * 0.055);
    let (cx, cy) = center(layer, target, shifted, bounds, scale);
    let transform = Transform::from_row(
        scale,
        0.0,
        0.0,
        scale,
        cx - width * scale / 2.0,
        cy - height * scale / 2.0,
    );
    let clipped = m.reveal < 1.0 || visible_lines < layer.line_bottoms.len();
    if clipped {
        if mask
            .as_ref()
            .is_none_or(|m| m.width() != target.width() || m.height() != target.height())
        {
            *mask = Mask::new(target.width(), target.height());
        }
        if let Some(mask) = mask.as_mut() {
            mask.clear();
            let bottom = layer
                .line_bottoms
                .get(visible_lines.saturating_sub(1))
                .copied()
                .unwrap_or(height);
            let mut path = PathBuilder::new();
            if anim == Anim::Typewriter && m.reveal < 1.0 {
                for r in layer
                    .glyphs
                    .iter()
                    .take((layer.glyphs.len() as f32 * m.reveal).ceil() as usize)
                    .filter(|r| r.y < bottom)
                {
                    if let Some(rect) =
                        tiny_skia::Rect::from_xywh(r.x, r.y, r.w, r.h.min(bottom - r.y))
                    {
                        path.push_rect(rect);
                    }
                }
            } else if let Some(rect) =
                tiny_skia::Rect::from_xywh(0.0, 0.0, width * m.reveal, bottom)
            {
                path.push_rect(rect);
            }
            if let Some(path) = path.finish() {
                mask.fill_path(&path, FillRule::Winding, true, transform);
            }
        }
    }
    target.draw_pixmap(
        0,
        0,
        layer.image.as_ref(),
        &PixmapPaint {
            opacity: m.alpha,
            quality: FilterQuality::Bilinear,
            ..PixmapPaint::default()
        },
        transform,
        if clipped { mask.as_ref() } else { None },
    );
}
