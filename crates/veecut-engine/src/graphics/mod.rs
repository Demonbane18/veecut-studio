//! Shared Rust text renderer for preview and export. Layouts and finished layers are cached.
//!
//! Caption lead, phone-safe region and edged caption boxes follow Herk Edit Kit (MIT).
//! Unboxed copy samples the underlying footage and gets a neutral scrim below 3:1 contrast.
//! Caption choices follow free247editor (MIT). Font files are OFL; licenses sit beside them.
//! Montserrat comes from JulietaUla/Montserrat/fonts/ttf; other families from google/fonts/ofl.
//! Inter is a static instance of that upstream font at wght=600, opsz=14 (fontTools 4.68).
//! Playfair Display Bold Italic is Google's official static release at google/fonts commit
//! d08233f9c581786e8c834a1f0695a4f27869c119. Bodoni Moda is a fontTools 4.68 static
//! instance (wght=700, opsz=96) from google/fonts/ofl/bodonimoda at
//! 634558fcbd9225a88591dec164f9fdb2716d0bf5. Montserrat Black is the designer's static TTF
//! at 679ead9bcd0d6cef6d3712369dc49fe72e8bd8d7. All three retain their upstream OFL text.
//! Documentary type sizes and six-frame keyword blur are from NextPhase Editor V1's published
//! measurements. Reveal shapes the complete page, then hides future words without moving slots.
//! `ink_on_light` changes white unboxed copy to dark ink on sampled light pictures; custom
//! colours remain intact. Placement and caption reports use the same complete-page bounds.
//! Run ignored `documentary_proof` for documentary PNGs and a warm draw benchmark.
//! Run the ignored `visual_proof` test for PNG samples and a warm-frame draw benchmark.
//! Caption pages keep off the speaker's eyes, mouth and chin when the project has face boxes; see
//! `placement.rs` for the rule and `Graphics::caption_placements` for what was drawn where.
//!
//! Timings and sizes follow published measurements in Jad M.H's (NextPhase AI) Arcade Motion
//! Graphics / Motion Graphics V3 skills; code written fresh. Press Start 2P is the upstream OFL
//! TTF from https://github.com/google/fonts/tree/main/ofl/pressstart2p.

mod contrast;
mod placement;
mod raster;
#[cfg(test)]
mod tests;
mod timing;

use std::{
    collections::{BTreeSet, HashMap, hash_map::DefaultHasher},
    hash::{Hash, Hasher},
    ops::Range,
    path::{Path, PathBuf},
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
use placement::Choice;
pub use placement::{CaptionPlacement, CaptionReport, FaceFit, Slot, caption_report};
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
    include_bytes!("../../assets/fonts/Montserrat/Montserrat-Black.ttf"),
    include_bytes!("../../assets/fonts/PlayfairDisplay/PlayfairDisplay-BoldItalic.ttf"),
    include_bytes!("../../assets/fonts/BodoniModa/BodoniModa-Bold.ttf"),
    include_bytes!("../../assets/fonts/PressStart2P/PressStart2P-Regular.ttf"),
];

/// Shaping and raster caches shared across frames. Keep one instance per playback/export.
pub struct Graphics {
    fonts: FontSystem,
    swash: SwashCache,
    families: Vec<String>,
    /// Canonical paths already read, so a project font is parsed once.
    loaded_fonts: BTreeSet<PathBuf>,
    layers: HashMap<u64, Layer>,
    pages: HashMap<u64, Vec<(Range<usize>, usize)>>,
    placements: HashMap<u64, Choice>,
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
            "Playfair Display",
            "Bodoni Moda",
            "Press Start 2P",
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
        let families = family_list(&fonts);
        Self {
            fonts,
            swash: SwashCache::new(),
            families,
            loaded_fonts: BTreeSet::new(),
            layers: HashMap::new(),
            pages: HashMap::new(),
            placements: HashMap::new(),
            bytes: 0,
            mask: None,
        }
    }

    /// Loads each new file in `project.fonts` into the font database. A missing file is skipped.
    /// Paths are absolute, or relative to `project_dir`, the same way asset paths are.
    pub fn load_project_fonts(&mut self, project: &Project, project_dir: &Path) {
        let mut added = false;
        for font in &project.fonts {
            let path = Path::new(font);
            let path = if path.is_absolute() {
                path.to_path_buf()
            } else {
                project_dir.join(path)
            };
            let key = std::fs::canonicalize(&path).unwrap_or(path);
            if !self.loaded_fonts.insert(key.clone()) {
                continue;
            }
            let Ok(bytes) = std::fs::read(&key) else {
                self.loaded_fonts.remove(&key);
                continue;
            };
            if bytes.is_empty() {
                continue;
            }
            self.fonts
                .db_mut()
                .load_font_source(Source::Binary(Arc::new(bytes)));
            added = true;
        }
        if added {
            self.families = family_list(&self.fonts);
        }
    }

    /// Draws only `mega` text. The compositor calls this before its first masked overlay track
    /// (or after all overlays when none is masked), so a cut-out can stand in front of the word.
    pub fn draw_behind(&mut self, project: &Project, t: f64, pixmap: &mut PixmapMut<'_>) {
        if !t.is_finite() {
            return;
        }
        let safe = safe_area(&project.canvas);
        for track in &project.tracks {
            if track.hidden || !matches!(track.kind, TrackKind::Text | TrackKind::Overlay) {
                continue;
            }
            for item in &track.items {
                if item.contains(t)
                    && let ItemBody::Text(text) = &item.body
                    && text.preset == TextPreset::Mega
                {
                    self.text(
                        item,
                        text,
                        t,
                        project.canvas.fps,
                        project.captions.ink_on_light,
                        safe,
                        pixmap,
                    );
                }
            }
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
                        // Placement is planned at canvas size so preview and export agree.
                        let choice = self.choose(
                            project,
                            item,
                            words,
                            safe,
                            project.canvas.width,
                            project.canvas.height,
                        );
                        if choice.face != FaceFit::Hidden {
                            self.caption(
                                item,
                                words,
                                &project.captions,
                                choice,
                                t,
                                project.canvas.fps,
                                safe,
                                (project.canvas.width, project.canvas.height),
                                pixmap,
                            );
                        }
                    }
                }
                TrackKind::Text | TrackKind::Overlay => {
                    for item in &track.items {
                        if item.contains(t)
                            && let ItemBody::Text(text) = &item.body
                            && text.preset != TextPreset::Mega
                        {
                            self.text(
                                item,
                                text,
                                t,
                                project.canvas.fps,
                                project.captions.ink_on_light,
                                safe,
                                pixmap,
                            );
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
        choice: Choice,
        t: f64,
        fps: f64,
        safe: Rect,
        canvas: (u32, u32),
        target: &mut PixmapMut<'_>,
    ) {
        if words.is_empty() {
            return;
        }
        let mut hash = DefaultHasher::new();
        item.id.hash(&mut hash);
        // Paged at canvas size, as placement measures it, so previews and frames at any size
        // page exactly like the export.
        let line_limit = caption_line_chars(style, safe, canvas.0, canvas.1);
        line_limit.hash(&mut hash);
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
            self.pages
                .insert(page_key, caption_pages(words, line_limit, style.uppercase));
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
        let visible = if style.mode == CaptionMode::Reveal {
            if relative >= words[range.end - 1].end {
                return;
            }
            words[range.clone()]
                .iter()
                .take_while(|word| relative >= word.start - 2.0 / fps.max(1.0))
                .count()
        } else {
            usize::MAX
        };
        let Some(mut built) = self.caption_layer(
            words,
            shown.clone(),
            line_break,
            spoken,
            style,
            style.size * choice.scale,
            safe,
            target.width(),
            target.height(),
            visible,
            false,
        ) else {
            return;
        };
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
        let position = Point::new(safe.x + safe.w / 2.0, choice.anchor);
        if style.ink_on_light
            && !style.boxed
            && style.mode != CaptionMode::Subtitle
            && light_behind(target, built, position, safe, m.scale)
            && let Some(ink) = self.caption_layer(
                words,
                shown,
                line_break,
                spoken,
                style,
                style.size * choice.scale,
                safe,
                target.width(),
                target.height(),
                visible,
                true,
            )
        {
            built = ink;
        }
        if let Some(layer) = self.layers.get(&built.key) {
            draw_layer(
                layer,
                target,
                &mut self.mask,
                position,
                safe,
                m,
                Anim::None,
                usize::MAX,
                None,
            );
        }
    }

    /// Finds or renders the layer for `words[shown]` at text size `size` (a fraction of the
    /// canvas height) on a `w` x `h` px target. Placement measures candidates through this too,
    /// so what it measures is what draws.
    #[allow(clippy::too_many_arguments)]
    fn caption_layer(
        &mut self,
        words: &[Word],
        shown: Range<usize>,
        line_break: usize,
        spoken: usize,
        style: &CaptionStyle,
        size: f32,
        safe: Rect,
        w: u32,
        h: u32,
        visible: usize,
        ink: bool,
    ) -> Option<Built> {
        let mut hash = DefaultHasher::new();
        visible.hash(&mut hash);
        ink.hash(&mut hash);
        (style.mode as u8).hash(&mut hash);
        style.uppercase.hash(&mut hash);
        for word in &words[shown.clone()] {
            word.text.hash(&mut hash);
            word.emphasis.hash(&mut hash);
        }
        (line_break > shown.start && line_break < shown.end)
            .then(|| line_break - shown.start)
            .hash(&mut hash);
        if style.mode == CaptionMode::Highlight {
            (spoken - shown.start).hash(&mut hash);
        }
        style.font.hash(&mut hash);
        size.to_bits().hash(&mut hash);
        style.color.hash(&mut hash);
        style.highlight.hash(&mut hash);
        style.stroke.to_bits().hash(&mut hash);
        style.shadow.hash(&mut hash);
        style.boxed.hash(&mut hash);
        w.hash(&mut hash);
        h.hash(&mut hash);
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
                let color = if style.mode == CaptionMode::Reveal && index - shown.start >= visible {
                    Color::rgba(255, 255, 255, 0)
                } else if pill {
                    DARK
                } else if style.mode != CaptionMode::Reveal
                    && (word.emphasis || (style.mode == CaptionMode::Highlight && index == spoken))
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
                    italic: style.mode == CaptionMode::Reveal && word.emphasis,
                    chrome: false,
                });
            }
            let font = self
                .family(
                    if style.mode == CaptionMode::Reveal
                        && style.font == CaptionStyle::default().font
                    {
                        "Inter"
                    } else {
                        &style.font
                    },
                )
                .to_owned();
            let subtitle = style.mode == CaptionMode::Subtitle;
            let size = fraction(size, 0.033) * h as f32 * if subtitle { 0.72 } else { 1.0 };
            let spec = Spec {
                spans,
                font: &font,
                size,
                width: safe.w * w as f32 / 1.08,
                height: safe.h * h as f32 / 1.08,
                max_lines: 2,
                align: Align::Center,
                stroke: if style.mode == CaptionMode::Reveal || ink {
                    0.0
                } else {
                    style.stroke
                },
                shadow: style.shadow || style.mode == CaptionMode::Reveal || ink,
                background: (style.boxed || subtitle).then_some(if style.boxed {
                    DARK
                } else {
                    Color::rgba(10, 14, 20, 190)
                }),
                edge: style.boxed,
                accent: style.highlight,
                preset: None,
                weight: if style.mode == CaptionMode::Reveal {
                    600
                } else {
                    800
                },
                reveal: style.mode == CaptionMode::Reveal,
                ink,
            };
            let layer = raster::render(&mut self.fonts, &mut self.swash, spec)?;
            self.cache(key, layer);
        }
        let layer = self.layers.get(&key)?;
        Some(Built {
            key,
            width: layer.image.width(),
            height: layer.image.height(),
            size: layer.size,
        })
    }

    /// Finds or renders the layer for a text item on a `w` x `h` px target. Caption placement
    /// measures text through this too, so the hook's box it avoids is the one that draws.
    #[allow(clippy::too_many_arguments)]
    fn text_layer(
        &mut self,
        item: &Item,
        text: &Text,
        defaults: &Preset,
        safe: Rect,
        w: u32,
        h: u32,
        ink: bool,
    ) -> Option<Built> {
        let mut hash = DefaultHasher::new();
        ink.hash(&mut hash);
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
        w.hash(&mut hash);
        h.hash(&mut hash);
        let key = hash.finish();
        if !self.layers.contains_key(&key) {
            let color = text.style.color.unwrap_or(defaults.color);
            let accent = text
                .style
                .accent
                .unwrap_or(if text.preset == TextPreset::Marker {
                    Color::rgb(217, 119, 87)
                } else {
                    ACCENT
                });
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
                    span.chrome = text.preset == TextPreset::Chapter
                        && text.style.color.is_none()
                        && !accented;
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
            let size = fraction(text.style.size.unwrap_or(defaults.size), defaults.size) * h as f32;
            let spec = Spec {
                spans,
                font: &font,
                size,
                width: safe.w * w as f32 / 1.08,
                height: safe.h * h as f32 / 1.08,
                max_lines: defaults.lines.max(content.lines().count()),
                align: text.style.align.unwrap_or(defaults.align),
                stroke: if ink {
                    0.0
                } else {
                    text.style.stroke.unwrap_or(defaults.stroke)
                },
                shadow: !matches!(text.preset, TextPreset::Marker | TextPreset::Meter),
                background: if matches!(text.preset, TextPreset::Marker | TextPreset::Meter) {
                    None
                } else if text.preset == TextPreset::BlackCard {
                    Some(Color::rgba(0, 0, 0, 0))
                } else {
                    text.style.background.or(defaults.background)
                },
                edge: matches!(
                    text.preset,
                    TextPreset::Card | TextPreset::Callout | TextPreset::Cta
                ),
                accent,
                preset: Some(text.preset),
                weight: defaults.weight,
                reveal: false,
                ink,
            };
            let layer = raster::render(&mut self.fonts, &mut self.swash, spec)?;
            self.cache(key, layer);
        }
        let layer = self.layers.get(&key)?;
        Some(Built {
            key,
            width: layer.image.width(),
            height: layer.image.height(),
            size: layer.size,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn text(
        &mut self,
        item: &Item,
        text: &Text,
        t: f64,
        fps: f64,
        ink_on_light: bool,
        safe: Rect,
        target: &mut PixmapMut<'_>,
    ) {
        if text.preset == TextPreset::Meter {
            self.meter(item, text, t, safe, target);
            return;
        }
        if text.text.trim().is_empty() {
            return;
        }
        let defaults = Preset::for_kind(text.preset, safe);
        let Some(built) = self.text_layer(
            item,
            text,
            &defaults,
            safe,
            target.width(),
            target.height(),
            false,
        ) else {
            return;
        };
        let mut key = built.key;
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
        let (position, bounds) = text_place(text, &defaults, safe);
        if text.preset == TextPreset::BlackCard {
            let background = text.style.background.unwrap_or(Color::BLACK);
            target.fill_rect(
                tiny_skia::Rect::from_xywh(0.0, 0.0, target.width() as f32, target.height() as f32)
                    .unwrap(),
                &raster::paint(background.with_alpha((f32::from(background.a) * m.alpha) as u8)),
                Transform::identity(),
                None,
            );
        } else if ink_on_light
            && text.style.background.or(defaults.background).is_none()
            && text.style.color.unwrap_or(defaults.color) == Color::WHITE
            && light_behind(target, built, position, bounds, m.scale)
            && let Some(ink) = self.text_layer(
                item,
                text,
                &defaults,
                safe,
                target.width(),
                target.height(),
                true,
            )
        {
            key = ink.key;
        }
        // Cache the six blur steps, never blur full video frames.
        if text.preset == TextPreset::Keyword && text.anim_in != Anim::None && !exit {
            let frame = ((t - item.start) * fps.max(1.0)).floor().max(0.0) as u8;
            if frame < 6 {
                let mut hash = DefaultHasher::new();
                ("keyword-blur", key, frame).hash(&mut hash);
                let blurred_key = hash.finish();
                if !self.layers.contains_key(&blurred_key)
                    && let Some(layer) = self.layers.get(&key)
                    && let Some(image) = raster::blurred(
                        &layer.image,
                        layer.size * 0.055 * f32::from(6 - frame) / 6.0,
                    )
                {
                    let blurred = Layer {
                        image,
                        glyphs: layer.glyphs.clone(),
                        line_bottoms: layer.line_bottoms.clone(),
                        contrast: None,
                        size: layer.size,
                    };
                    self.cache(blurred_key, blurred);
                }
                if self.layers.contains_key(&blurred_key) {
                    key = blurred_key;
                }
            }
        }
        if text.preset == TextPreset::Callout {
            let point = callout_point(text, safe);
            if let Some(layer) = self.layers.get(&key) {
                let (cx, cy) = center(
                    layer.image.width() as f32,
                    layer.image.height() as f32,
                    target.width() as f32,
                    target.height() as f32,
                    position,
                    safe,
                    m.scale,
                );
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
                (text.preset == TextPreset::Marker).then(|| {
                    let p = ((t - item.start) / 0.35).clamp(0.0, 1.0) as f32;
                    let eased = if p < 0.5 {
                        2.0 * p * p
                    } else {
                        1.0 - 2.0 * (1.0 - p).powi(2)
                    };
                    (text.style.accent.unwrap_or(Color::rgb(217, 119, 87)), eased)
                }),
            );
        }
    }

    /// Draws the HUD in shared canvas coordinates; the item's own clock drives whole segments.
    fn meter(&mut self, item: &Item, text: &Text, t: f64, safe: Rect, target: &mut PixmapMut<'_>) {
        let (w, h) = (target.width() as f32, target.height() as f32);
        let bounds = meter_box(text, safe);
        let mut bar = Rect {
            x: bounds.x * w,
            y: bounds.y * h,
            w: bounds.w * w,
            h: bounds.h * h,
        };
        if !text.text.trim().is_empty() {
            let defaults = Preset::for_kind(TextPreset::Meter, safe);
            if let Some(built) = self.text_layer(
                item,
                text,
                &defaults,
                safe,
                target.width(),
                target.height(),
                false,
            ) && let Some(label) = self.layers.get(&built.key)
            {
                let label_w = bar.w * 0.22;
                draw_layer(
                    label,
                    target,
                    &mut self.mask,
                    Point::new((bar.x + label_w / 2.0) / w, (bar.y + bar.h / 2.0) / h),
                    Rect::FULL,
                    Motion {
                        scale: (label_w / built.width as f32).min(bar.h / built.height as f32),
                        ..motion(Anim::None, 1.0, false)
                    },
                    Anim::None,
                    usize::MAX,
                    None,
                );
                let offset = label_w + w * 0.01;
                bar.x += offset;
                bar.w -= offset;
            }
        }
        let fill = |target: &mut PixmapMut<'_>, r: Rect, color: Color| {
            if let Some(rect) = tiny_skia::Rect::from_xywh(r.x, r.y, r.w, r.h) {
                target.fill_rect(rect, &raster::paint(color), Transform::identity(), None);
            }
        };
        fill(
            target,
            bar,
            text.style.color.unwrap_or(Color::rgb(233, 227, 245)),
        );
        let border = (w * 2.0 / 1080.0).max(1.0).min(bar.h * 0.15);
        let inset = border * 2.0;
        fill(
            target,
            Rect {
                x: bar.x + border,
                y: bar.y + border,
                w: bar.w - 2.0 * border,
                h: bar.h - 2.0 * border,
            },
            Color::rgb(18, 14, 30),
        );
        let gap = (w * 6.0 / 1080.0).min(bar.w / 30.0);
        let seg_w = (bar.w - inset * 2.0 - gap * 11.0) / 12.0;
        let lit =
            (((t - item.start) / item.duration.max(0.001)).clamp(0.0, 1.0) * 12.0).floor() as usize;
        for i in 0..12 {
            let p = i as f32 / 11.0;
            let color = if i < lit {
                text.style.accent.unwrap_or(Color::rgb(
                    (255.0 - 13.0 * p).round() as u8,
                    (90.0 + 71.0 * p).round() as u8,
                    (60.0 - 16.0 * p).round() as u8,
                ))
            } else {
                text.style.background.unwrap_or(Color::rgb(43, 50, 72))
            };
            fill(
                target,
                Rect {
                    x: bar.x + inset + i as f32 * (seg_w + gap),
                    y: bar.y + inset,
                    w: seg_w,
                    h: bar.h - inset * 2.0,
                },
                color,
            );
        }
    }
}

fn family_list(fonts: &FontSystem) -> Vec<String> {
    fonts
        .db()
        .faces()
        .flat_map(|face| face.families.iter().map(|(name, _)| name.clone()))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

impl Default for Graphics {
    fn default() -> Self {
        Self::new()
    }
}

fn light_behind(
    target: &PixmapMut<'_>,
    built: Built,
    position: Point,
    safe: Rect,
    scale: f32,
) -> bool {
    let rect = layer_box(
        built.width,
        built.height,
        target.width(),
        target.height(),
        position,
        safe,
        scale,
    );
    contrast::light_picture(
        target,
        Rect {
            x: rect.x * target.width() as f32,
            y: rect.y * target.height() as f32,
            w: rect.w * target.width() as f32,
            h: rect.h * target.height() as f32,
        },
    )
}

fn fraction(value: f32, fallback: f32) -> f32 {
    if value.is_finite() {
        value.clamp(0.001, 0.6)
    } else {
        fallback
    }
}

/// Characters per caption line: the style's `max_chars`, lowered to what fits the safe width at
/// the style's own size, so a long word starts the next page instead of shrinking the whole page.
/// Draw and placement both page through this, so what placement measures is what draws.
pub(super) fn caption_line_chars(style: &CaptionStyle, safe: Rect, w: u32, h: u32) -> usize {
    let subtitle = style.mode == CaptionMode::Subtitle;
    let size = fraction(style.size, 0.033) * h as f32 * if subtitle { 0.72 } else { 1.0 };
    // The renderer pads each side by about half the size. Heavy capitals (Montserrat 800)
    // advance ~0.73 to 0.83 em, lower case ~0.62 em. Overestimating pages sooner instead of
    // shrinking; a single word wider than the line is still fitted by the renderer.
    let room = safe.w * w as f32 / 1.08 - size;
    let em = if style.uppercase { 0.80 } else { 0.62 };
    let fit = (room / (size * em)).floor().max(1.0) as usize;
    usize::from(style.max_chars).max(1).min(fit)
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
            // Sizes and six-frame keyword entrance from NextPhase Editor V1's published measurements.
            TextPreset::Keyword => {
                p.size = 0.14;
                p.stroke = 0.0;
                p.lines = 1;
            }
            TextPreset::Chapter => {
                p.font = "Playfair Display";
                p.size = 0.126;
                p.weight = 700;
                p.stroke = 0.0;
                p.lines = 2;
            }
            TextPreset::Shock => {
                p.font = "Bodoni Moda";
                p.size = 0.163;
                p.weight = 700;
                p.color = Color::rgb(242, 3, 10);
                p.uppercase = true;
                p.stroke = 0.0;
                p.lines = 1;
            }
            TextPreset::BlackCard => {
                p.font = "Inter";
                p.size = 0.05;
                p.weight = 600;
                p.stroke = 0.0;
                p.position = Point::new(0.5, 0.5);
                p.lines = 1;
            }
            TextPreset::Mega => {
                p.size = 0.52;
                p.weight = 900;
                p.color = Color::rgb(242, 3, 10);
                p.uppercase = true;
                p.stroke = 0.0;
                p.position = Point::new(0.5, 0.36);
                p.lines = 1;
            }
            TextPreset::Marker => {
                p.font = "Inter";
                p.size = 0.055;
                p.color = Color::rgb(28, 27, 30);
                p.stroke = 0.0;
                p.weight = 600;
            }
            TextPreset::Meter => {
                p.font = "Press Start 2P";
                p.size = 0.018;
                p.stroke = 0.0;
                p.lines = 1;
                p.weight = 400;
            }
        }
        p
    }
}

fn center(
    layer_w: f32,
    layer_h: f32,
    w: f32,
    h: f32,
    position: Point,
    bounds: Rect,
    scale: f32,
) -> (f32, f32) {
    let (half_w, half_h) = (layer_w * scale / 2.0, layer_h * scale / 2.0);
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

/// A cached caption layer: its cache key, pixel size and the text size it was shrunk to fit.
#[derive(Clone, Copy)]
struct Built {
    key: u64,
    width: u32,
    height: u32,
    size: f32,
}

/// Where `captions.position` aims: subtitles keep their own low default until it is changed.
fn requested_y(style: &CaptionStyle, safe: Rect) -> f32 {
    if style.mode == CaptionMode::Subtitle && style.position == 0.70 {
        safe.y + safe.h - 0.06
    } else {
        style.position
    }
}

/// Where a text item's layer is centered and the area it may not leave, as `text` draws it.
fn text_place(text: &Text, defaults: &Preset, safe: Rect) -> (Point, Rect) {
    if text.preset == TextPreset::Callout {
        let point = callout_point(text, safe);
        return (
            Point::new(point.x - safe.w * 0.20, point.y - safe.h * 0.14),
            safe,
        );
    }
    (
        text.position.unwrap_or(defaults.position),
        if text.position.is_some() {
            Rect::FULL
        } else {
            safe
        },
    )
}

/// One footprint shared by HUD drawing and caption avoidance. Explicit positions are centres.
fn meter_box(text: &Text, safe: Rect) -> Rect {
    let height = fraction(text.style.size.unwrap_or(0.028), 0.028).min(safe.h);
    let width = 0.34f32.min(safe.w);
    let position = text.position.unwrap_or(Point::new(
        safe.x.max(0.06) + width / 2.0,
        safe.y.max(0.06) + height / 2.0,
    ));
    let bounds = if text.position.is_some() {
        Rect::FULL
    } else {
        safe
    };
    Rect {
        x: (position.x - width / 2.0).clamp(bounds.x, (bounds.x + bounds.w - width).max(bounds.x)),
        y: (position.y - height / 2.0)
            .clamp(bounds.y, (bounds.y + bounds.h - height).max(bounds.y)),
        w: width,
        h: height,
    }
}

/// The point a callout's leader line ends on.
fn callout_point(text: &Text, safe: Rect) -> Point {
    text.position
        .unwrap_or(Point::new(safe.x + safe.w * 0.67, safe.y + safe.h * 0.56))
}

/// The box (fractions of the canvas) a `layer_w` x `layer_h` px layer fills on a `w` x `h` px
/// target when `draw_layer` centers it at `position` inside `bounds`, at motion scale `scale`
/// (1 at rest; `pop_peak()` at the top of a pop-in).
fn layer_box(
    layer_w: u32,
    layer_h: u32,
    w: u32,
    h: u32,
    position: Point,
    bounds: Rect,
    scale: f32,
) -> Rect {
    let (lw, lh, wf, hf) = (layer_w as f32, layer_h as f32, w as f32, h as f32);
    let scale = scale.min(bounds.w * wf / lw).min(bounds.h * hf / lh);
    let (cx, cy) = center(lw, lh, wf, hf, position, bounds, scale);
    Rect {
        x: (cx - lw * scale / 2.0) / wf,
        y: (cy - lh * scale / 2.0) / hf,
        w: lw * scale / wf,
        h: lh * scale / hf,
    }
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
    marker: Option<(Color, f32)>,
) {
    if m.alpha <= 0.0 || m.reveal <= 0.0 {
        return;
    }
    let (width, height) = (layer.image.width() as f32, layer.image.height() as f32);
    let (target_w, target_h) = (target.width() as f32, target.height() as f32);
    let scale = m
        .scale
        .min(bounds.w * target_w / width)
        .min(bounds.h * target_h / height);
    let shifted = Point::new(position.x + m.x * 0.08, position.y + m.y * 0.055);
    let (cx, cy) = center(width, height, target_w, target_h, shifted, bounds, scale);
    let transform = Transform::from_row(
        scale,
        0.0,
        0.0,
        scale,
        cx - width * scale / 2.0,
        cy - height * scale / 2.0,
    );
    if let Some((color, progress)) = marker {
        // Raster padding is 0.38 em; inset 0.13 leaves 0.25 em around the shaped text.
        let inset = layer.size * 0.13;
        let rect = Rect {
            x: inset,
            y: inset,
            w: (width - inset * 2.0) * progress,
            h: height - inset * 2.0,
        };
        if progress > 0.0
            && let Some(path) = raster::rounded_path(rect, 4.0 * target_w / 1080.0 / scale)
        {
            target.fill_path(
                &path,
                &raster::paint(color.with_alpha((color.a as f32 * 0.62 * m.alpha) as u8)),
                FillRule::Winding,
                transform,
                None,
            );
        }
    }
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
    if let Some(guard) = &layer.contrast {
        guard.draw(
            target,
            Rect {
                x: cx - width * scale / 2.0,
                y: cy - height * scale / 2.0,
                w: width * scale,
                h: height * scale,
            },
            transform,
            m.alpha,
        );
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
