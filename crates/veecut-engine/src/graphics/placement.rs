//! Keeps caption boxes off the speaker's mouth and chin.
//!
//! Faces come from the project (`Project::faces`, boxes in source coordinates over source time).
//! For each caption page the mouth and chin zone is carried through the main-track clip's crop,
//! focus and zooms into canvas space (the same math the compositor draws with) at the page's
//! start, end, every zoom edge and every face sample inside it. The page's box is then tested
//! against that zone and against the boxes of any text on screen (the hook): the requested
//! position first, then the lowest spot of the safe area at full, 0.85 and 0.7 size, then the same
//! in the upper slot. Text boxes come from the same layers and geometry `draw` uses, with room for
//! a pop-in or slide. Face and text never give way: when no slot is clear of both, the page is
//! not drawn, and the report says so.
//!
//! Limits: the detector only sees frontal faces, so a gap in the track means "not checked", never
//! "clear". Pages are not split, so one-line pages are not a fallback. Overlays (B-roll) are not
//! considered. The mouth and chin zone (`MOUTH_TOP`, `CHIN_MARGIN`) was checked by eye on the NASA
//! test clip at 45.9, 89.1 and 94.2 s and has not been tested on other footage.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::ops::Range;

use serde::Serialize;

use super::timing::{active_caption, caption_pages, page_span, pop_peak};
use super::{Graphics, Preset, layer_box, requested_y, safe_area, text_place};
use crate::project::{
    Anim, AssetKind, CaptionMode, CaptionStyle, FaceSample, Item, ItemBody, Point, Project, Rect,
    TextPreset, TrackKind, Word,
};
use crate::render::canvas_rect;

/// A face detection speaks for this many source seconds on each side of it.
const HOLD: f64 = 1.0;
/// The mouth and chin zone starts this far down the detected box (a fraction of its height) ...
const MOUTH_TOP: f32 = 0.55;
/// ... and ends this far below the box bottom.
const CHIN_MARGIN: f32 = 0.12;
/// Clear space kept between a caption box and the zone, as a fraction of the canvas height.
const GAP: f32 = 0.006;
/// Smaller caption text than this (a fraction of the canvas height) is not a placement fallback.
const MIN_SIZE: f32 = 0.035;
/// Fractions of the requested size a fallback may shrink the text to.
const SCALES: [f32; 3] = [1.0, 0.85, 0.7];

/// Where a caption page ended up.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Slot {
    /// At `captions.position` (the safe area may still clamp it, see `clamped`).
    Requested,
    /// Bottom of the safe area.
    Lower,
    /// Top of the safe area.
    Upper,
}

/// What the face check concluded for a caption page.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FaceFit {
    /// The page's clip has no face data, so nothing was checked.
    Off,
    /// Face data exists, but no detection is within a second of this page: not checked.
    Unchecked,
    /// The requested box is clear of the mouth and chin.
    Clear,
    /// Moved to another slot or shrunk to get clear of the face and any text.
    Moved,
    /// No slot was clear of the face and the text on screen, so the page is not drawn.
    Hidden,
}

/// One caption page as laid out: the box `Graphics::draw` fills (fractions of the canvas) and
/// why it is there.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CaptionPlacement {
    /// The caption item's id.
    pub item: String,
    /// Timeline seconds the page is on screen, including the two-frame lead and 300 ms tail.
    pub start: f64,
    pub end: f64,
    /// Where `captions.position` aims, as a fraction of the height.
    pub requested: f32,
    /// The largest box drawn (a word-pop entrance's overshoot included). For a `hidden` page,
    /// the box it would have at the requested position.
    pub bounds: Rect,
    /// Text height actually drawn, a fraction of the canvas height (after fitting the box).
    pub size: f32,
    pub slot: Slot,
    /// The safe area stopped the box from sitting where `requested` points.
    pub clamped: bool,
    pub face: FaceFit,
    /// The face was not found at some moment of this page, so the check covers the rest only.
    pub face_gaps: bool,
}

/// Totals over every page, with the pages most worth a look first.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CaptionReport {
    pub requested: f32,
    pub pages: usize,
    pub clamped: usize,
    pub moved: usize,
    pub hidden: usize,
    /// Pages whose face check did not run: no face data (`off`) or no detection nearby
    /// (`unchecked`).
    pub unchecked: usize,
    pub gaps: usize,
    /// Hidden, unchecked, moved and clamped pages, at most `limit` of them.
    pub examples: Vec<CaptionPlacement>,
}

/// The decision `draw` needs: where to center the page and how much to shrink it.
#[derive(Clone, Copy)]
pub(super) struct Choice {
    pub slot: Slot,
    /// Center as a fraction of the height; 1 and 0 pin the box to the safe area's edges.
    pub anchor: f32,
    /// Fraction of `captions.size`.
    pub scale: f32,
    pub face: FaceFit,
    pub gaps: bool,
}

/// Mouth and chin zones of one page, united, in canvas space.
#[derive(Default)]
struct Zone {
    rect: Option<Rect>,
    /// Some probe had no detection within `HOLD`.
    gaps: bool,
    /// A clip under the page has face data at all.
    data: bool,
}

fn union(a: Rect, b: Rect) -> Rect {
    let (x, y) = (a.x.min(b.x), a.y.min(b.y));
    Rect {
        x,
        y,
        w: (a.x + a.w).max(b.x + b.w) - x,
        h: (a.y + a.h).max(b.y + b.h) - y,
    }
}

/// Whether `b`, with `GAP` above and below, touches the zone.
fn touches(b: Rect, zone: Rect) -> bool {
    b.x < zone.x + zone.w
        && b.x + b.w > zone.x
        && b.y - GAP < zone.y + zone.h
        && b.y + b.h + GAP > zone.y
}

fn nearest(faces: &[FaceSample], t: f64) -> Option<&FaceSample> {
    let i = faces.partition_point(|s| s.t < t);
    faces[i.saturating_sub(1)..(i + 1).min(faces.len())]
        .iter()
        .min_by(|p, q| (p.t - t).abs().total_cmp(&(q.t - t).abs()))
        .filter(|s| (s.t - t).abs() <= HOLD)
}

/// The mouth and chin zone of every face the page can show between timeline seconds `a` and
/// `b`, on the canvas.
fn face_zone(project: &Project, a: f64, b: f64) -> Zone {
    let mut zone = Zone::default();
    let canvas = (project.canvas.width, project.canvas.height);
    for item in &project.main().items {
        if item.end() <= a || item.start >= b {
            continue;
        }
        let Some(clip) = item.clip() else { continue };
        let Some(faces) = project.faces.get(&clip.asset).filter(|f| !f.is_empty()) else {
            continue;
        };
        let Some(asset) = project.asset(&clip.asset) else {
            continue;
        };
        if asset.kind != AssetKind::Video || asset.width == 0 || asset.height == 0 {
            continue;
        }
        zone.data = true;
        let (from, to) = (a.max(item.start), b.min(item.end()));
        let source = |t: f64| clip.src_in + (t - item.start) * clip.speed;
        let mut probe = |t: f64| {
            let Some(s) = nearest(faces, source(t)) else {
                zone.gaps = true;
                return;
            };
            let mouth = Rect {
                x: s.x,
                y: s.y + s.h * MOUTH_TOP,
                w: s.w,
                h: s.h * (1.0 - MOUTH_TOP + CHIN_MARGIN),
            };
            let r = canvas_rect(
                clip,
                (asset.width, asset.height),
                canvas,
                t - item.start,
                item.duration,
                mouth,
            );
            zone.rect = Some(zone.rect.map_or(r, |u| union(u, r)));
        };
        // The view changes at zoom edges, the face between samples: probe both.
        probe(from);
        probe(to);
        for z in &clip.zooms {
            for edge in [0.0, z.ease, z.duration - z.ease, z.duration] {
                let t = item.start + z.at + edge;
                if t > from && t < to {
                    probe(t);
                }
            }
        }
        let first = faces.partition_point(|s| s.t < source(from));
        for s in faces[first..].iter().take_while(|s| s.t <= source(to)) {
            probe(item.start + (s.t - clip.src_in) / clip.speed);
        }
    }
    zone
}

/// What `draw` shows of a page: whole pages in word-pop, else each wrapped phrase. Each is
/// (words shown, where line two starts, the word being spoken).
fn units(words: &[Word], style: &CaptionStyle) -> Vec<(Range<usize>, usize, usize)> {
    let mut out = Vec::new();
    for (range, line_break) in caption_pages(words, usize::from(style.max_chars), style.uppercase) {
        if style.mode == CaptionMode::WordPop {
            out.extend(range.map(|i| (i..i + 1, i + 1, i)));
        } else {
            out.push((range.clone(), line_break, range.start));
        }
    }
    out
}

impl Graphics {
    /// Boxes every phrase of the page fills when centered at `anchor` at `scale` times the
    /// requested size, and the smallest text size drawn (fractions of the canvas height).
    #[allow(clippy::too_many_arguments)]
    fn measure(
        &mut self,
        style: &CaptionStyle,
        words: &[Word],
        anchor: f32,
        scale: f32,
        safe: Rect,
        w: u32,
        h: u32,
    ) -> (Vec<Rect>, f32) {
        let mut boxes = Vec::new();
        let mut text = f32::MAX;
        // A word pops in a little larger than at rest; that box must stay clear too.
        let pop = if style.mode == CaptionMode::WordPop {
            pop_peak()
        } else {
            1.0
        };
        for (shown, line_break, spoken) in units(words, style) {
            if let Some(layer) = self.caption_layer(
                words,
                shown,
                line_break,
                spoken,
                style,
                style.size * scale,
                safe,
                w,
                h,
                usize::MAX,
                false,
            ) {
                boxes.push(layer_box(
                    layer.width,
                    layer.height,
                    w,
                    h,
                    Point::new(safe.x + safe.w / 2.0, anchor),
                    safe,
                    pop,
                ));
                text = text.min(layer.size / h as f32);
            }
        }
        (boxes, text)
    }

    /// Calls `f` with the largest box (fractions of the canvas) of every text item on screen
    /// between timeline seconds `a` and `b`: the box `draw` fills, grown for a pop-in's overshoot
    /// and for the travel of a slide.
    #[allow(clippy::too_many_arguments)]
    fn text_boxes(
        &mut self,
        project: &Project,
        a: f64,
        b: f64,
        safe: Rect,
        w: u32,
        h: u32,
        f: &mut dyn FnMut(Rect),
    ) {
        for track in &project.tracks {
            if track.hidden || !matches!(track.kind, TrackKind::Text | TrackKind::Overlay) {
                continue;
            }
            for item in &track.items {
                let ItemBody::Text(text) = &item.body else {
                    continue;
                };
                if item.start >= b || item.end() <= a || text.text.trim().is_empty() {
                    continue;
                }
                let defaults = Preset::for_kind(text.preset, safe);
                let Some(layer) = self.text_layer(item, text, &defaults, safe, w, h, false) else {
                    continue;
                };
                let (position, bounds) = text_place(text, &defaults, safe);
                let anims = [text.anim_in, text.anim_out];
                let mut peak = 1.0f32;
                if anims.contains(&Anim::Pop) {
                    peak = pop_peak();
                }
                if text.preset == TextPreset::Kinetic {
                    peak = peak.max(1.28);
                }
                let mut r = layer_box(layer.width, layer.height, w, h, position, bounds, peak);
                // Slides travel up to 0.08 of the width and 0.055 of the height; lower thirds
                // enter from the left, up to 0.24 of the width.
                let slide = anims.iter().any(|a| {
                    matches!(
                        a,
                        Anim::SlideUp | Anim::SlideDown | Anim::SlideLeft | Anim::SlideRight
                    )
                });
                let dx = if text.preset == TextPreset::LowerThird {
                    0.24
                } else if slide {
                    0.08
                } else {
                    0.0
                };
                let dy = if slide { 0.055 } else { 0.0 };
                r = Rect {
                    x: r.x - dx,
                    y: r.y - dy,
                    w: r.w + 2.0 * dx,
                    h: r.h + 2.0 * dy,
                };
                f(r);
            }
        }
    }

    /// Where to draw `item`: the requested spot unless a face or a text box (the hook) is in the
    /// way.
    pub(super) fn choose(
        &mut self,
        project: &Project,
        item: &Item,
        words: &[Word],
        safe: Rect,
        w: u32,
        h: u32,
    ) -> Choice {
        let style = &project.captions;
        let requested = requested_y(style, safe);
        let home = Choice {
            slot: Slot::Requested,
            anchor: requested,
            scale: 1.0,
            face: FaceFit::Off,
            gaps: false,
        };
        let Some((a, b)) = page_span(item, words, project.canvas.fps) else {
            return home;
        };
        let zone = if project.faces.is_empty() {
            Zone::default()
        } else {
            face_zone(project, a, b)
        };
        // Hashed so a moved or edited hook replans the page.
        let mut text_hash = DefaultHasher::new();
        let mut texts = 0u32;
        self.text_boxes(project, a, b, safe, w, h, &mut |r| {
            texts += 1;
            [r.x, r.y, r.w, r.h].map(f32::to_bits).hash(&mut text_hash);
        });
        let unchecked = if zone.data {
            FaceFit::Unchecked
        } else {
            FaceFit::Off
        };
        if zone.rect.is_none() && texts == 0 {
            return Choice {
                face: unchecked,
                gaps: zone.gaps,
                ..home
            };
        }

        let mut hash = DefaultHasher::new();
        item.id.hash(&mut hash);
        a.to_bits().hash(&mut hash);
        b.to_bits().hash(&mut hash);
        for word in words {
            word.text.hash(&mut hash);
            word.emphasis.hash(&mut hash);
        }
        (style.mode as u8).hash(&mut hash);
        style.font.hash(&mut hash);
        style.size.to_bits().hash(&mut hash);
        style.stroke.to_bits().hash(&mut hash);
        style.shadow.hash(&mut hash);
        style.boxed.hash(&mut hash);
        style.uppercase.hash(&mut hash);
        style.max_chars.hash(&mut hash);
        requested.to_bits().hash(&mut hash);
        zone.rect
            .map(|r| [r.x, r.y, r.w, r.h].map(f32::to_bits))
            .hash(&mut hash);
        (zone.gaps, text_hash.finish(), w, h).hash(&mut hash);
        let key = hash.finish();
        if let Some(choice) = self.placements.get(&key) {
            return *choice;
        }

        let mut obstacles = Vec::new();
        self.text_boxes(project, a, b, safe, w, h, &mut |r| obstacles.push(r));
        let tries = std::iter::once((Slot::Requested, requested, 1.0))
            .chain(SCALES.map(|scale| (Slot::Lower, 1.0, scale)))
            .chain(SCALES.map(|scale| (Slot::Upper, 0.0, scale)));
        // The first try clear of the face and of every text box wins; with none, the page is
        // not drawn.
        let mut found = None;
        for (k, (slot, anchor, scale)) in tries.enumerate() {
            let (boxes, text) = self.measure(style, words, anchor, scale, safe, w, h);
            if scale < 1.0 && text < MIN_SIZE {
                continue;
            }
            let blocked = zone
                .rect
                .is_some_and(|f| boxes.iter().any(|b| touches(*b, f)))
                || boxes
                    .iter()
                    .any(|b| obstacles.iter().any(|o| touches(*b, *o)));
            if !blocked {
                found = Some((k, slot, anchor, scale));
                break;
            }
        }
        let choice = match found {
            Some((k, slot, anchor, scale)) => Choice {
                slot,
                anchor,
                scale,
                face: match zone.rect {
                    Some(_) if k == 0 => FaceFit::Clear,
                    Some(_) => FaceFit::Moved,
                    None => unchecked,
                },
                gaps: zone.gaps,
            },
            None => Choice {
                face: FaceFit::Hidden,
                gaps: zone.gaps,
                ..home
            },
        };
        if self.placements.len() >= 512 {
            self.placements.clear();
        }
        self.placements.insert(key, choice);
        choice
    }

    /// The layout of one caption page as `draw` would make it.
    fn placement(
        &mut self,
        project: &Project,
        item: &Item,
        words: &[Word],
    ) -> Option<CaptionPlacement> {
        let safe = safe_area(&project.canvas);
        let (w, h) = (project.canvas.width, project.canvas.height);
        let (start, mut end) = page_span(item, words, project.canvas.fps)?;
        if project.captions.mode == CaptionMode::Reveal {
            end = end.min(item.start + words.last()?.end);
        }
        let choice = self.choose(project, item, words, safe, w, h);
        let style = &project.captions;
        let requested = requested_y(style, safe);
        let (home, home_text) = self.measure(style, words, requested, 1.0, safe, w, h);
        let clamped = home
            .iter()
            .any(|b| (b.y + b.h / 2.0 - requested).abs() > 0.002);
        let (boxes, size) = if choice.slot == Slot::Requested {
            (home, home_text)
        } else {
            self.measure(style, words, choice.anchor, choice.scale, safe, w, h)
        };
        Some(CaptionPlacement {
            item: item.id.clone(),
            start,
            end,
            requested,
            bounds: boxes.into_iter().reduce(union)?,
            size,
            slot: choice.slot,
            clamped,
            face: choice.face,
            face_gaps: choice.gaps,
        })
    }

    /// Every caption page as drawn, in time order. Pages are not drawn at all while captions
    /// are off.
    pub fn caption_placements(&mut self, project: &Project) -> Vec<CaptionPlacement> {
        let mut out = Vec::new();
        if project.captions.mode == CaptionMode::Off {
            return out;
        }
        for track in &project.tracks {
            if track.kind != TrackKind::Captions || track.hidden {
                continue;
            }
            for item in &track.items {
                if let ItemBody::Caption(c) = &item.body
                    && let Some(p) = self.placement(project, item, &c.words)
                {
                    out.push(p);
                }
            }
        }
        out.sort_by(|a, b| a.start.total_cmp(&b.start));
        out
    }

    /// The caption page on screen at timeline second `t`, laid out as `draw` lays it out.
    pub fn caption_placement_at(&mut self, project: &Project, t: f64) -> Option<CaptionPlacement> {
        if project.captions.mode == CaptionMode::Off {
            return None;
        }
        for track in &project.tracks {
            if track.kind != TrackKind::Captions || track.hidden {
                continue;
            }
            if let Some((item, words)) = active_caption(&track.items, t, project.canvas.fps) {
                let placement = self.placement(project, item, words)?;
                if t >= placement.end {
                    continue;
                }
                return Some(placement);
            }
        }
        None
    }
}

/// Counts over every caption page and the `limit` pages that most need a look.
pub fn caption_report(graphics: &mut Graphics, project: &Project, limit: usize) -> CaptionReport {
    let placements = graphics.caption_placements(project);
    let count = |f: &dyn Fn(&CaptionPlacement) -> bool| placements.iter().filter(|p| f(*p)).count();
    let rank = |p: &CaptionPlacement| match p.face {
        FaceFit::Hidden => 5,
        FaceFit::Unchecked | FaceFit::Off => 3,
        _ if p.slot != Slot::Requested => 2,
        _ => u8::from(p.clamped),
    };
    let mut examples: Vec<CaptionPlacement> = placements
        .iter()
        .filter(|p| rank(*p) > 0)
        .cloned()
        .collect();
    examples.sort_by_key(|p| std::cmp::Reverse(rank(p)));
    examples.truncate(limit);
    CaptionReport {
        requested: requested_y(&project.captions, safe_area(&project.canvas)),
        pages: placements.len(),
        clamped: count(&|p| p.clamped),
        moved: count(&|p| p.slot != Slot::Requested),
        hidden: count(&|p| p.face == FaceFit::Hidden),
        unchecked: count(&|p| matches!(p.face, FaceFit::Unchecked | FaceFit::Off)),
        gaps: count(&|p| p.face_gaps),
        examples,
    }
}
