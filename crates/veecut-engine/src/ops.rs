//! The Editor API. Every change to a project, whether it comes from the CLI, the MCP server, the
//! desktop editor or an agent, is one of these operations. Ops are plain JSON, for example
//! `{"op":"cut_range","from":2.0,"to":3.5}`, and atomic: an op either applies fully and leaves a
//! valid project, or returns an error and changes nothing.

use anyhow::{Context, Result, anyhow, bail};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::project::{
    Anim, AssetKind, AssetRole, Clip, Item, ItemBody, Marker, Point, Project, Rect, Text,
    TextPreset, TextStyle, Track, TrackKind, Word, Zoom,
};

const EPS: f64 = 1e-6;

fn punch() -> f32 {
    1.12
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    /// Remove the timeline range [from, to) from every track and close the gap (ripple).
    /// Use it for pauses, mistakes and anything the viewer should not see.
    CutRange { from: f64, to: f64 },
    /// Split items at a timeline time. Without `track`, every item crossing `at` is split.
    Split {
        at: f64,
        #[serde(default)]
        track: Option<String>,
    },
    /// Move an item's timeline edges. Main-track clips ripple: later clips shift.
    Trim {
        item: String,
        #[serde(default)]
        start: Option<f64>,
        #[serde(default)]
        end: Option<f64>,
    },
    /// Move an item. Main-track clips move by `index` (their position in the sequence); other
    /// items by `start`. `track` moves the item to another track.
    Move {
        item: String,
        #[serde(default)]
        start: Option<f64>,
        #[serde(default)]
        track: Option<String>,
        #[serde(default)]
        index: Option<usize>,
    },
    /// Delete an item. The main track closes the gap; other tracks keep it.
    Delete { item: String },
    /// Copy an item and place the copy right after it.
    Duplicate { item: String },
    /// Put part of an asset on a track. Defaults: the track that suits the asset's role, the
    /// end of the main track (or `start`), and the rest of the asset (3 s for images).
    AddClip {
        asset: String,
        #[serde(default)]
        track: Option<String>,
        #[serde(default)]
        start: Option<f64>,
        #[serde(default)]
        index: Option<usize>,
        #[serde(default)]
        src_in: f64,
        #[serde(default)]
        duration: Option<f64>,
        /// Placement on the canvas (overlay tracks); omit to fill the canvas.
        #[serde(default)]
        frame: Option<Rect>,
        #[serde(default)]
        volume: Option<f32>,
        /// Music beds: dip under speech.
        #[serde(default)]
        duck: Option<bool>,
    },
    /// Add on-screen text. Wrap words in *asterisks* to draw them in the accent color.
    AddText {
        text: String,
        start: f64,
        duration: f64,
        #[serde(default)]
        preset: TextPreset,
        #[serde(default)]
        position: Option<Point>,
        #[serde(default)]
        style: Option<TextStyle>,
        #[serde(default)]
        anim_in: Option<Anim>,
        #[serde(default)]
        anim_out: Option<Anim>,
        #[serde(default)]
        track: Option<String>,
    },
    /// Zoom the main-track picture at a timeline time. `ease` 0 is an instant punch-in.
    AddZoom {
        at: f64,
        duration: f64,
        #[serde(default = "punch")]
        scale: f32,
        #[serde(default)]
        focus: Option<Point>,
        #[serde(default)]
        ease: f64,
    },
    /// JSON merge patch (RFC 7386) on one item, for example
    /// `{"op":"patch","item":"clip-3","patch":{"volume":0.5,"speed":1.25}}`.
    Patch { item: String, patch: Value },
    /// Set any project value by JSON pointer, for example
    /// `{"op":"set","path":"/captions/position","value":0.3}`.
    Set { path: String, value: Value },
    /// Pin a note to a moment.
    AddMarker { at: f64, note: String },
    /// Add a track. The new track draws on top of the existing ones.
    AddTrack {
        kind: TrackKind,
        #[serde(default)]
        name: Option<String>,
    },
}

impl Op {
    pub fn name(&self) -> &'static str {
        match self {
            Op::CutRange { .. } => "cut_range",
            Op::Split { .. } => "split",
            Op::Trim { .. } => "trim",
            Op::Move { .. } => "move",
            Op::Delete { .. } => "delete",
            Op::Duplicate { .. } => "duplicate",
            Op::AddClip { .. } => "add_clip",
            Op::AddText { .. } => "add_text",
            Op::AddZoom { .. } => "add_zoom",
            Op::Patch { .. } => "patch",
            Op::Set { .. } => "set",
            Op::AddMarker { .. } => "add_marker",
            Op::AddTrack { .. } => "add_track",
        }
    }
}

/// What an op did, in words an agent can relay, plus the ids it created or changed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Applied {
    pub summary: String,
    pub items: Vec<String>,
}

fn applied(summary: impl Into<String>, items: Vec<String>) -> Applied {
    Applied { summary: summary.into(), items }
}

/// Applies one op atomically.
pub fn apply(project: &mut Project, op: &Op) -> Result<Applied> {
    let mut next = project.clone();
    let done = apply_inner(&mut next, op)?;
    next.normalize();
    next.validate().with_context(|| format!("{} would leave the project invalid", op.name()))?;
    *project = next;
    Ok(done)
}

/// Applies a batch atomically: all of them or none.
pub fn apply_all(project: &mut Project, ops: &[Op]) -> Result<Vec<Applied>> {
    let mut next = project.clone();
    let mut out = Vec::with_capacity(ops.len());
    for (i, op) in ops.iter().enumerate() {
        out.push(apply(&mut next, op).with_context(|| format!("op {} ({})", i + 1, op.name()))?);
    }
    *project = next;
    Ok(out)
}

/// Parses one op, a JSON array of ops, or JSON lines.
pub fn parse_ops(text: &str) -> Result<Vec<Op>> {
    let text = text.trim();
    if text.starts_with('[') {
        return serde_json::from_str(text).context("ops must be a JSON array of op objects");
    }
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str(l).with_context(|| format!("not a valid op: {l}")))
        .collect()
}

fn apply_inner(p: &mut Project, op: &Op) -> Result<Applied> {
    match op {
        Op::CutRange { from, to } => cut_range(p, *from, *to),
        Op::Split { at, track } => {
            let tracks: Vec<usize> = match track {
                Some(id) => vec![p.track_index(id).ok_or_else(|| no_track(id))?],
                None => (0..p.tracks.len()).collect(),
            };
            let mut ids = Vec::new();
            for ti in tracks {
                ids.extend(split_track_at(p, ti, *at));
            }
            if ids.is_empty() {
                bail!("nothing crosses {at:.3}s, so there is nothing to split");
            }
            Ok(applied(format!("split {} item(s) at {at:.3}s", ids.len()), ids))
        }
        Op::Trim { item, start, end } => {
            let (ti, ii) = p.find(item).ok_or_else(|| no_item(item))?;
            let it = &p.tracks[ti].items[ii];
            let (s, e) = (start.unwrap_or(it.start), end.unwrap_or(it.end()));
            if e - s <= EPS {
                bail!("trim would leave {item} with no length");
            }
            let trimmed = slice(it, s, e);
            p.tracks[ti].items[ii] = trimmed;
            Ok(applied(format!("trimmed {item} to {s:.3}s–{e:.3}s"), vec![item.clone()]))
        }
        Op::Move { item, start, track, index } => move_item(p, item, *start, track.as_deref(), *index),
        Op::Delete { item } => {
            let (ti, ii) = p.find(item).ok_or_else(|| no_item(item))?;
            p.tracks[ti].items.remove(ii);
            Ok(applied(format!("deleted {item}"), vec![item.clone()]))
        }
        Op::Duplicate { item } => {
            let (ti, ii) = p.find(item).ok_or_else(|| no_item(item))?;
            let mut copy = p.tracks[ti].items[ii].clone();
            copy.id = p.new_id(prefix(&copy));
            let id = copy.id.clone();
            if ti == 0 {
                if let Some(c) = copy.clip_mut() {
                    c.transition = None;
                }
                p.tracks[0].items.insert(ii + 1, copy);
            } else {
                copy.start = p.tracks[ti].items[ii].end();
                p.tracks[ti].items.push(copy);
            }
            Ok(applied(format!("duplicated {item} as {id}"), vec![id]))
        }
        Op::AddClip { asset, track, start, index, src_in, duration, frame, volume, duck } => {
            let a = p.asset(asset).ok_or_else(|| anyhow!("no asset {asset:?}; add the file first"))?.clone();
            let ti = match track {
                Some(id) => p.track_index(id).ok_or_else(|| no_track(id))?,
                None => default_track(p, a.kind, a.role)?,
            };
            let dur = match (duration, a.kind) {
                (Some(d), _) => *d,
                (None, AssetKind::Image) => 3.0,
                (None, _) => a.duration - src_in,
            };
            if dur <= EPS {
                bail!("the clip would have no length (src_in {src_in}s, asset {:.3}s)", a.duration);
            }
            let mut clip = Clip::new(asset.clone(), *src_in);
            clip.frame = *frame;
            if let Some(v) = volume {
                clip.volume = *v;
            }
            clip.duck = duck.unwrap_or(a.role == AssetRole::Music);
            let id = p.new_id("clip");
            let item = Item { id: id.clone(), start: start.unwrap_or(0.0), duration: dur, body: ItemBody::Clip(clip) };
            if ti == 0 {
                let items = &p.tracks[0].items;
                let at = match (index, start) {
                    (Some(i), _) => (*i).min(items.len()),
                    (None, Some(s)) => items.iter().position(|it| it.start >= s - EPS).unwrap_or(items.len()),
                    (None, None) => items.len(),
                };
                p.tracks[0].items.insert(at, item);
            } else {
                p.tracks[ti].items.push(item);
            }
            let track_id = p.tracks[ti].id.clone();
            Ok(applied(format!("added {id} ({dur:.3}s of {asset}) on {track_id}"), vec![id]))
        }
        Op::AddText { text, start, duration, preset, position, style, anim_in, anim_out, track } => {
            let ti = match track {
                Some(id) => p.track_index(id).ok_or_else(|| no_track(id))?,
                None => p.track_of_kind(TrackKind::Text).map(Ok).unwrap_or_else(|| add_track(p, TrackKind::Text, None))?,
            };
            let id = p.new_id("text");
            p.tracks[ti].items.push(Item {
                id: id.clone(),
                start: *start,
                duration: *duration,
                body: ItemBody::Text(Text {
                    text: text.clone(),
                    preset: *preset,
                    style: style.clone().unwrap_or_default(),
                    position: *position,
                    anim_in: anim_in.unwrap_or_default(),
                    anim_out: anim_out.unwrap_or_default(),
                }),
            });
            Ok(applied(format!("added {id} at {start:.3}s for {duration:.3}s"), vec![id]))
        }
        Op::AddZoom { at, duration, scale, focus, ease } => {
            let (ii, item) = p.main_at(*at).ok_or_else(|| anyhow!("no main-track clip at {at:.3}s"))?;
            let (rel, room) = (at - item.start, item.end() - at);
            let id = item.id.clone();
            let zoom = Zoom { at: rel, duration: duration.min(room), scale: *scale, focus: *focus, ease: *ease };
            p.tracks[0].items[ii].clip_mut().expect("main track holds clips").zooms.push(zoom);
            Ok(applied(format!("zoom {scale}x on {id} at {at:.3}s"), vec![id]))
        }
        Op::Patch { item, patch } => {
            let (ti, ii) = p.find(item).ok_or_else(|| no_item(item))?;
            let mut value = serde_json::to_value(&p.tracks[ti].items[ii])?;
            merge_patch(&mut value, patch);
            let mut patched: Item = serde_json::from_value(value).context("the patch does not fit this item")?;
            patched.id = item.clone();
            p.tracks[ti].items[ii] = patched;
            Ok(applied(format!("patched {item}"), vec![item.clone()]))
        }
        Op::Set { path, value } => {
            let mut doc = serde_json::to_value(&*p)?;
            set_pointer(&mut doc, path, value.clone())?;
            *p = serde_json::from_value(doc).with_context(|| format!("{path} cannot be {value}"))?;
            Ok(applied(format!("set {path} = {value}"), Vec::new()))
        }
        Op::AddMarker { at, note } => {
            p.markers.push(Marker { at: *at, note: note.clone(), done: false });
            Ok(applied(format!("marker at {at:.3}s"), Vec::new()))
        }
        Op::AddTrack { kind, name } => {
            let ti = add_track(p, *kind, name.as_deref())?;
            let id = p.tracks[ti].id.clone();
            Ok(applied(format!("added track {id}"), vec![id]))
        }
    }
}

fn no_item(id: &str) -> anyhow::Error {
    anyhow!("no item {id:?} in this project")
}

fn no_track(id: &str) -> anyhow::Error {
    anyhow!("no track {id:?} in this project")
}

fn prefix(item: &Item) -> &'static str {
    match item.body {
        ItemBody::Clip(_) => "clip",
        ItemBody::Text(_) => "text",
        ItemBody::Caption(_) => "cap",
    }
}

fn add_track(p: &mut Project, kind: TrackKind, name: Option<&str>) -> Result<usize> {
    let (stem, default_name) = match kind {
        TrackKind::Main => bail!("a project has exactly one main track"),
        TrackKind::Overlay => ("overlay", "Overlay"),
        TrackKind::Text => ("text-track", "Text"),
        TrackKind::Captions => ("captions-track", "Captions"),
        TrackKind::Audio => ("audio", "Audio"),
    };
    let id = p.new_id(stem);
    p.tracks.push(Track {
        id,
        kind,
        name: name.unwrap_or(default_name).into(),
        muted: false,
        hidden: false,
        items: Vec::new(),
    });
    Ok(p.tracks.len() - 1)
}

/// The track a new clip of this asset goes on when the caller does not say.
fn default_track(p: &mut Project, kind: AssetKind, role: AssetRole) -> Result<usize> {
    let named = |p: &Project, id: &str, kind: TrackKind| {
        p.track_index(id).filter(|&i| p.tracks[i].kind == kind).or_else(|| p.track_of_kind(kind))
    };
    let found = match (kind, role) {
        (AssetKind::Video | AssetKind::Image, AssetRole::Source) => Some(0),
        (AssetKind::Video | AssetKind::Image, _) => named(p, "broll", TrackKind::Overlay),
        (AssetKind::Audio, AssetRole::Sfx) => named(p, "sfx", TrackKind::Audio),
        (AssetKind::Audio, _) => named(p, "music", TrackKind::Audio),
    };
    match found {
        Some(i) => Ok(i),
        None if kind == AssetKind::Audio => add_track(p, TrackKind::Audio, None),
        None => add_track(p, TrackKind::Overlay, None),
    }
}

/// A copy of `item` covering the timeline range [from, to). The range may extend past the
/// item's edges (trimming outward); validation then checks the source is long enough.
fn slice(item: &Item, from: f64, to: f64) -> Item {
    let off = from - item.start;
    let dur = to - from;
    let mut out = item.clone();
    out.start = from;
    out.duration = dur;
    let keeps_head = off.abs() <= EPS;
    let keeps_tail = (to - item.end()).abs() <= EPS;
    match &mut out.body {
        ItemBody::Clip(c) => {
            c.src_in = (c.src_in + off * c.speed).max(0.0);
            if !keeps_head {
                c.fade_in = 0.0;
                c.transition = None;
            }
            if !keeps_tail {
                c.fade_out = 0.0;
            }
            c.zooms = c
                .zooms
                .iter()
                .filter_map(|z| {
                    let (s, e) = ((z.at - off).max(0.0), (z.at - off + z.duration).min(dur));
                    (e - s > EPS).then(|| Zoom { at: s, duration: e - s, ..z.clone() })
                })
                .collect();
            if let Some(kb) = &mut c.ken_burns {
                let lerp = |a: f32, b: f32, t: f64| a + (b - a) * t.clamp(0.0, 1.0) as f32;
                let (t0, t1) = (off / item.duration, (off + dur) / item.duration);
                let (s0, s1) = (kb.from_scale, kb.to_scale);
                kb.from_scale = lerp(s0, s1, t0);
                kb.to_scale = lerp(s0, s1, t1);
                if let (Some(f0), Some(f1)) = (kb.from_focus, kb.to_focus) {
                    kb.from_focus = Some(Point::new(lerp(f0.x, f1.x, t0), lerp(f0.y, f1.y, t0)));
                    kb.to_focus = Some(Point::new(lerp(f0.x, f1.x, t1), lerp(f0.y, f1.y, t1)));
                }
            }
        }
        ItemBody::Caption(cap) => {
            cap.words = cap
                .words
                .iter()
                .filter_map(|w| {
                    let (s, e) = (w.start - off, w.end - off);
                    let mid = (s + e) / 2.0;
                    (mid >= 0.0 && mid < dur).then(|| Word { start: s.max(0.0), end: e.min(dur), ..w.clone() })
                })
                .collect();
        }
        ItemBody::Text(_) => {}
    }
    out
}

fn is_empty_caption(item: &Item) -> bool {
    matches!(&item.body, ItemBody::Caption(c) if c.words.is_empty())
}

/// Splits every item on track `ti` that strictly crosses `at`. Returns the new right-hand ids.
fn split_track_at(p: &mut Project, ti: usize, at: f64) -> Vec<String> {
    let mut new_ids = Vec::new();
    let mut ii = 0;
    while ii < p.tracks[ti].items.len() {
        let item = &p.tracks[ti].items[ii];
        if item.start < at - EPS && item.end() > at + EPS {
            let left = slice(item, item.start, at);
            let mut right = slice(item, at, item.end());
            right.id = p.new_id(prefix(&right));
            if let Some(c) = right.clip_mut() {
                c.transition = None;
            }
            let (left_empty, right_empty) = (is_empty_caption(&left), is_empty_caption(&right));
            let track = &mut p.tracks[ti].items;
            track.remove(ii);
            if !right_empty {
                new_ids.push(right.id.clone());
                track.insert(ii, right);
            }
            if !left_empty {
                track.insert(ii, left);
                ii += 1;
            }
            if !right_empty {
                ii += 1;
            }
        } else {
            ii += 1;
        }
    }
    new_ids
}

fn cut_range(p: &mut Project, from: f64, to: f64) -> Result<Applied> {
    if !(to - from > EPS) || from < 0.0 {
        bail!("cut_range needs 0 <= from < to (got {from}..{to})");
    }
    let len = to - from;
    let mut removed = Vec::new();
    for ti in 0..p.tracks.len() {
        split_track_at(p, ti, from);
        split_track_at(p, ti, to);
        let track = &mut p.tracks[ti];
        track.items.retain(|it| {
            let inside = it.start >= from - EPS && it.end() <= to + EPS;
            if inside {
                removed.push(it.id.clone());
            }
            !inside
        });
        if ti != 0 {
            for it in &mut track.items {
                if it.start >= to - EPS {
                    it.start -= len;
                }
            }
        }
    }
    p.markers.retain(|m| !(m.at >= from && m.at < to));
    for m in &mut p.markers {
        if m.at >= to {
            m.at -= len;
        }
    }
    Ok(applied(format!("cut {from:.3}s–{to:.3}s ({len:.3}s) from every track"), removed))
}

fn move_item(p: &mut Project, id: &str, start: Option<f64>, track: Option<&str>, index: Option<usize>) -> Result<Applied> {
    let (ti, ii) = p.find(id).ok_or_else(|| no_item(id))?;
    let target = match track {
        Some(t) => p.track_index(t).ok_or_else(|| no_track(t))?,
        None => ti,
    };
    if ti == 0 && target == 0 {
        let Some(index) = index else {
            bail!("main-track clips sit back to back; move them with \"index\" (0 is first)");
        };
        let item = p.tracks[0].items.remove(ii);
        let at = index.min(p.tracks[0].items.len());
        p.tracks[0].items.insert(at, item);
        return Ok(applied(format!("moved {id} to position {at} on the main track"), vec![id.into()]));
    }
    let mut item = p.tracks[ti].items.remove(ii);
    if let Some(s) = start {
        item.start = s;
    }
    if target == 0 {
        let items = &p.tracks[0].items;
        let at = index
            .unwrap_or_else(|| items.iter().position(|it| it.start >= item.start - EPS).unwrap_or(items.len()))
            .min(items.len());
        p.tracks[0].items.insert(at, item);
    } else {
        p.tracks[target].items.push(item);
    }
    let name = p.tracks[target].id.clone();
    Ok(applied(format!("moved {id} on {name}"), vec![id.into()]))
}

/// RFC 7386 JSON merge patch.
pub fn merge_patch(target: &mut Value, patch: &Value) {
    let Value::Object(patch) = patch else {
        *target = patch.clone();
        return;
    };
    if !target.is_object() {
        *target = Value::Object(Default::default());
    }
    let map = target.as_object_mut().expect("just made an object");
    for (key, value) in patch {
        if value.is_null() {
            map.remove(key);
        } else {
            merge_patch(map.entry(key.clone()).or_insert(Value::Null), value);
        }
    }
}

/// Sets `value` at a JSON pointer. The parent must exist; `-` appends to an array.
pub fn set_pointer(doc: &mut Value, path: &str, value: Value) -> Result<()> {
    if path.is_empty() || path == "/" {
        *doc = value;
        return Ok(());
    }
    let Some(rest) = path.strip_prefix('/') else {
        bail!("path {path:?} must start with /, for example /captions/size");
    };
    let mut keys: Vec<String> = rest.split('/').map(|k| k.replace("~1", "/").replace("~0", "~")).collect();
    let last = keys.pop().expect("split yields at least one piece");
    let mut node = doc;
    for key in &keys {
        node = match node {
            Value::Object(m) => m.get_mut(key),
            Value::Array(a) => key.parse::<usize>().ok().and_then(|i| a.get_mut(i)),
            _ => None,
        }
        .ok_or_else(|| anyhow!("{path}: nothing at {key:?}"))?;
    }
    match node {
        Value::Object(m) => {
            m.insert(last, value);
        }
        Value::Array(a) if last == "-" => a.push(value),
        Value::Array(a) => {
            let i: usize = last.parse().with_context(|| format!("{path}: {last:?} is not an index"))?;
            let slot = a.get_mut(i).ok_or_else(|| anyhow!("{path}: index {i} is past the end"))?;
            *slot = value;
        }
        _ => bail!("{path}: the parent is not an object or array"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::{Asset, Canvas, Caption, VideoKind};

    fn project() -> Project {
        let mut p = Project::new("t", VideoKind::TalkingHeadShort, Canvas::vertical());
        p.assets.push(Asset {
            id: "a".into(),
            path: "a.mp4".into(),
            kind: AssetKind::Video,
            role: AssetRole::Source,
            duration: 60.0,
            width: 1920,
            height: 1080,
            fps: 30.0,
            has_audio: true,
        });
        apply(&mut p, &Op::AddClip { asset: "a".into(), track: None, start: None, index: None, src_in: 0.0, duration: Some(10.0), frame: None, volume: None, duck: None }).unwrap();
        apply(&mut p, &Op::AddClip { asset: "a".into(), track: None, start: None, index: None, src_in: 20.0, duration: Some(10.0), frame: None, volume: None, duck: None }).unwrap();
        let words = (0..20).map(|i| Word { text: format!("w{i}"), start: i as f64, end: i as f64 + 0.8, emphasis: false }).collect();
        p.tracks[3].items.push(Item { id: "cap-1".into(), start: 0.0, duration: 20.0, body: ItemBody::Caption(Caption { words }) });
        apply(&mut p, &Op::AddText { text: "late".into(), start: 15.0, duration: 2.0, preset: TextPreset::Card, position: None, style: None, anim_in: None, anim_out: None, track: None }).unwrap();
        p
    }

    #[test]
    fn cut_range_ripples_every_track() {
        let mut p = project();
        apply(&mut p, &Op::CutRange { from: 8.0, to: 12.0 }).unwrap();
        let main = &p.main().items;
        assert_eq!(main.len(), 2);
        assert!((main[0].duration - 8.0).abs() < EPS);
        let second = main[1].clip().unwrap();
        assert!((second.src_in - 22.0).abs() < EPS && (main[1].start - 8.0).abs() < EPS);
        let text = &p.tracks[2].items[0];
        assert!((text.start - 11.0).abs() < EPS, "text shifted left by 4s");
        let caps: Vec<&str> = p.tracks[3].items.iter().flat_map(|i| match &i.body {
            ItemBody::Caption(c) => c.words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>(),
            _ => vec![],
        }).collect();
        assert!(!caps.contains(&"w9") && !caps.contains(&"w10") && caps.contains(&"w12"));
        assert!((p.duration() - 16.0).abs() < EPS);
    }

    #[test]
    fn failed_ops_change_nothing() {
        let mut p = project();
        let before = p.clone();
        assert!(apply(&mut p, &Op::Trim { item: "clip-2".into(), start: None, end: Some(60.0) }).is_err(), "runs past the source");
        assert!(apply_all(&mut p, &[Op::Delete { item: "clip-1".into() }, Op::Delete { item: "nope".into() }]).is_err());
        assert_eq!(p, before);
    }

    #[test]
    fn split_trim_move_patch_and_set() {
        let mut p = project();
        let done = apply(&mut p, &Op::Split { at: 5.0, track: Some("main".into()) }).unwrap();
        assert_eq!(p.main().items.len(), 3);
        let right = &done.items[0];
        assert!((p.item(right).unwrap().clip().unwrap().src_in - 5.0).abs() < EPS);
        apply(&mut p, &Op::Move { item: right.clone(), start: None, track: None, index: Some(0) }).unwrap();
        assert_eq!(&p.main().items[0].id, right);
        apply(&mut p, &Op::Trim { item: "clip-1".into(), start: Some(6.0), end: None }).unwrap();
        apply(&mut p, &Op::Patch { item: "clip-1".into(), patch: serde_json::json!({"volume": 0.5}) }).unwrap();
        assert_eq!(p.item("clip-1").unwrap().clip().unwrap().volume, 0.5);
        apply(&mut p, &Op::Set { path: "/captions/position".into(), value: serde_json::json!(0.3) }).unwrap();
        assert_eq!(p.captions.position, 0.3);
        assert!(apply(&mut p, &Op::Set { path: "/captions/position".into(), value: serde_json::json!("high") }).is_err());
        let ops = parse_ops(r#"{"op":"add_zoom","at":1.0,"duration":2.0}"#).unwrap();
        apply_all(&mut p, &ops).unwrap();
        assert_eq!(p.main().items[0].clip().unwrap().zooms[0].scale, 1.12);
    }
}
