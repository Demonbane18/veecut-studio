//! Look packs (`veecut.toml`, schema 1). `apply` plans Editor API ops and runs them as one batch.
//!
//! Text overrides patch every existing text item of that preset. They are not stored on the
//! project, so a text item added later keeps the preset default until the look is applied again.
//!
//! Clip grade, motion and fit hit image clips, B-roll or source videos on overlay tracks,
//! explainer main-track clips, and documentary main-track videos that are not the speaker. The
//! speaker is a main-track video on a talking-head or documentary project whose asset has a
//! transcript (`transcript.json` or `transcript-<asset>.json`).
//!
//! `transitions.every` (default 1) marks main-track cuts whose 1-based item index is divisible
//! by `every`. Motion capture is Hold, Push or Glide, not the original Ken Burns path.
//! Look-added overlays and audio use reserved `look-` item IDs and replace the previous look's
//! items on reapply. Capture keeps these overlays; ordinary story text stays in the project.
//!
//! Timings and sizes follow published measurements in Jad M.H's (NextPhase AI) Arcade Motion
//! Graphics / Motion Graphics V3 skills; code written fresh.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::ops::{self, Op};
use crate::project::{
    Anim, Asset, AssetKind, AssetRole, Canvas, Clip, Color, Fit, Item, ItemBody, KenBurns, Look,
    Point, Project, TextPreset, TextStyle, Track, TrackKind, Transition, TransitionKind, Treatment,
    VideoKind,
};
use crate::recipes::ExplainerMotion;
use crate::sfx;
use crate::transcript::Transcript;
use crate::workspace::Workspace;

pub const LOOK_FILE: &str = "veecut.toml";

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LookPack {
    pub schema: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub kinds: Vec<VideoKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aspect: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub captions: Option<CaptionPatch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub look: Option<Look>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<Color>,
    #[serde(
        default,
        skip_serializing_if = "BTreeMap::is_empty",
        serialize_with = "text_overrides::serialize",
        deserialize_with = "text_overrides::deserialize"
    )]
    #[schemars(with = "BTreeMap<String, crate::project::TextStyle>")]
    pub text: BTreeMap<TextPreset, TextStyle>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub overlays: Vec<OverlayLook>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clips: Option<ClipLook>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transitions: Option<TransitionLook>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sound: Option<SoundLook>,
    /// Files relative to the skill folder. Empty means every `fonts/*.{ttf,otf}`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fonts: Vec<String>,
}

/// Text decoration added by a look. An omitted end follows the project's end on each apply.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OverlayLook {
    pub preset: TextPreset,
    #[serde(default)]
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<Point>,
    #[serde(default)]
    pub style: TextStyle,
    #[serde(default)]
    pub start: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anim_in: Option<Anim>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anim_out: Option<Anim>,
}

/// Every `CaptionStyle` field, all optional, so a pack can change one without resetting the rest.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CaptionPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<crate::project::CaptionMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<Color>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub highlight: Option<Color>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stroke: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shadow: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boxed: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ink_on_light: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uppercase: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_words: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_chars: Option<u16>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ClipLook {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grade: Option<Treatment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub motion: Option<ExplainerMotion>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fit: Option<Fit>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TransitionLook {
    pub kind: TransitionKind,
    pub duration: f64,
    #[serde(default = "one_usize")]
    pub every: usize,
}

fn one_usize() -> usize {
    1
}

/// Sound refs are a file inside the skill folder or `kit:<sound id>` (`sfx::Sound` snake_case slug).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SoundLook {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cut: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub music: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub music_db: Option<f32>,
}

mod text_overrides {
    use std::collections::BTreeMap;

    use serde::ser::SerializeMap;
    use serde::{Deserialize, Deserializer, Serializer};

    use crate::project::{TextPreset, TextStyle};

    pub fn serialize<S: Serializer>(
        map: &BTreeMap<TextPreset, TextStyle>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        let mut out = serializer.serialize_map(Some(map.len()))?;
        for (preset, style) in map {
            let key = serde_json::to_value(preset).map_err(serde::ser::Error::custom)?;
            let key = key
                .as_str()
                .ok_or_else(|| serde::ser::Error::custom("text preset"))?;
            out.serialize_entry(key, style)?;
        }
        out.end()
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<BTreeMap<TextPreset, TextStyle>, D::Error> {
        let raw = BTreeMap::<String, TextStyle>::deserialize(deserializer)?;
        raw.into_iter()
            .map(|(key, style)| {
                let preset = serde_json::from_value(serde_json::Value::String(key.clone()))
                    .map_err(|_| {
                        serde::de::Error::custom(format!("unknown text preset {key:?}"))
                    })?;
                Ok((preset, style))
            })
            .collect()
    }
}

/// Parses `veecut.toml` and checks schema 1, unknown keys, and refs that stay inside the folder.
pub fn load(skill_dir: &Path) -> Result<LookPack> {
    let skill_dir = std::path::absolute(skill_dir)
        .with_context(|| format!("resolving {}", skill_dir.display()))?;
    let file = skill_dir.join(LOOK_FILE);
    let text = std::fs::read_to_string(&file)
        .with_context(|| format!("skill folder has no {LOOK_FILE}"))?;
    reject_unknown_look(&text)?;
    let mut pack: LookPack = toml::from_str(&text)
        .with_context(|| format!("{} is not a valid look pack", file.display()))?;
    if pack.schema != 1 {
        bail!(
            "{LOOK_FILE} schema is {}; this VeeCut reads schema 1",
            pack.schema
        );
    }
    if let Some(aspect) = &pack.aspect
        && Canvas::from_aspect(aspect).is_none()
    {
        bail!("aspect {aspect:?} must be 9:16, 16:9, 4:5 or 1:1");
    }
    if let Some(tr) = &pack.transitions {
        if !tr.duration.is_finite() || tr.duration <= 0.0 {
            bail!("transition duration must be a positive number of seconds");
        }
        if tr.every == 0 {
            bail!("transitions.every must be at least 1");
        }
    }
    for overlay in &pack.overlays {
        if !overlay.start.is_finite()
            || overlay.start < 0.0
            || overlay
                .end
                .is_some_and(|end| !end.is_finite() || end <= overlay.start)
        {
            bail!("overlay times must be finite, with 0 <= start < end");
        }
    }
    if let Some(sound) = &pack.sound {
        for spec in [&sound.text, &sound.cut, &sound.music]
            .into_iter()
            .flatten()
        {
            check_sound_ref(&skill_dir, spec)?;
        }
        if sound.music_db.is_some_and(|db| !db.is_finite()) {
            bail!("music_db must be a finite number of dB");
        }
    }
    if pack.fonts.is_empty() {
        pack.fonts = scan_fonts(&skill_dir)?;
    } else {
        for rel in &pack.fonts {
            contained_file(&skill_dir, rel)?;
        }
    }
    Ok(pack)
}

/// Editor API ops for `pack`. Does not change `project`. Kit cues go through `sfx::place` on a
/// scratch copy; music gain uses `sfx::bed_volume` unless `music_db` is set.
pub fn ops_for(
    ws: &Workspace,
    project: &Project,
    project_dir: &Path,
    skill_dir: &Path,
    pack: &LookPack,
) -> Result<Vec<Op>> {
    let skill_dir = std::path::absolute(skill_dir)
        .with_context(|| format!("resolving {}", skill_dir.display()))?;
    let mut ops = Vec::new();
    let mut clean = project.clone();
    for (index, track) in clean.tracks.iter_mut().enumerate() {
        let before = track.items.len();
        track.items.retain(|item| !item.id.starts_with("look-"));
        if track.items.len() != before {
            ops.push(set(&format!("/tracks/{index}/items"), &track.items)?);
        }
    }
    let project = &clean;
    if let Some(captions) = &pack.captions {
        caption_ops(captions, &mut ops)?;
    }
    if let Some(look) = &pack.look {
        ops.push(set("/look", look)?);
    }
    if let Some(color) = pack.background {
        ops.push(set("/canvas/background", &color)?);
    }
    if let Some(aspect) = &pack.aspect {
        let canvas = Canvas::from_aspect(aspect)
            .with_context(|| format!("aspect {aspect:?} must be 9:16, 16:9, 4:5 or 1:1"))?;
        ops.push(set("/canvas/width", &canvas.width)?);
        ops.push(set("/canvas/height", &canvas.height)?);
    }
    for (preset, style) in &pack.text {
        if *style == TextStyle::default() {
            continue;
        }
        for track in &project.tracks {
            for item in &track.items {
                if let ItemBody::Text(text) = &item.body
                    && text.preset == *preset
                {
                    ops.push(Op::Patch {
                        item: item.id.clone(),
                        patch: json!({ "style": style }),
                    });
                }
            }
        }
    }
    if let Some(clips) = &pack.clips {
        let patch = clip_patch(clips)?;
        if patch != json!({}) {
            for track in &project.tracks {
                for item in &track.items {
                    let Some(clip) = item.clip() else {
                        continue;
                    };
                    if takes_clip_look(project, project_dir, track, clip) {
                        ops.push(Op::Patch {
                            item: item.id.clone(),
                            patch: patch.clone(),
                        });
                    }
                }
            }
        }
    }
    if let Some(tr) = &pack.transitions {
        let value = serde_json::to_value(Transition {
            kind: tr.kind,
            duration: tr.duration,
        })?;
        for (index, item) in project.main().items.iter().enumerate().skip(1) {
            if index % tr.every == 0 && item.clip().is_some() {
                ops.push(Op::Patch {
                    item: item.id.clone(),
                    patch: json!({ "transition": value }),
                });
            }
        }
    }
    if !pack.fonts.is_empty() {
        let mut fonts = project.fonts.clone();
        for rel in &pack.fonts {
            let path = skill_dir.join(rel).to_string_lossy().to_string();
            if !fonts.iter().any(|have| have == &path) {
                fonts.push(path);
            }
        }
        if fonts != project.fonts {
            ops.push(set("/fonts", &fonts)?);
        }
    }
    let mut decorated = clean;
    ops::apply_all(&mut decorated, &ops)?;
    let end = decorated.duration();
    for overlay in &pack.overlays {
        let until = overlay.end.unwrap_or(end);
        if !overlay.start.is_finite()
            || overlay.start < 0.0
            || !until.is_finite()
            || until <= overlay.start
        {
            bail!("overlay times must be finite, with 0 <= start < end");
        }
        let mut style =
            serde_json::to_value(pack.text.get(&overlay.preset).cloned().unwrap_or_default())?;
        if let Value::Object(overrides) = serde_json::to_value(&overlay.style)? {
            style
                .as_object_mut()
                .expect("text style is an object")
                .extend(overrides);
        }
        let add = Op::AddText {
            text: overlay.text.clone(),
            start: overlay.start,
            duration: until - overlay.start,
            preset: overlay.preset,
            position: overlay.position,
            style: Some(serde_json::from_value(style)?),
            anim_in: overlay.anim_in,
            anim_out: overlay.anim_out,
            track: None,
        };
        let added = ops::apply(&mut decorated, &add)?;
        ops.push(add);
        let id = &added.items[0];
        let (ti, ii) = decorated
            .tracks
            .iter()
            .enumerate()
            .find_map(|(ti, track)| {
                track
                    .items
                    .iter()
                    .position(|item| &item.id == id)
                    .map(|ii| (ti, ii))
            })
            .expect("add_text returned its item");
        let tag = set(
            &format!("/tracks/{ti}/items/{ii}/id"),
            &decorated.new_id("look-overlay"),
        )?;
        ops::apply(&mut decorated, &tag)?;
        ops.push(tag);
    }
    ops.extend(sound_ops(ws, &decorated, project_dir, &skill_dir, pack)?);
    Ok(ops)
}

/// Loads the pack, plans ops, and applies them atomically. Returns the op count.
pub fn apply(
    ws: &Workspace,
    project: &mut Project,
    project_dir: &Path,
    skill_dir: &Path,
) -> Result<usize> {
    let pack = load(skill_dir)?;
    let ops = ops_for(ws, project, project_dir, skill_dir, &pack)?;
    let n = ops.len();
    ops::apply_all(project, &ops)?;
    project.normalize();
    Ok(n)
}

/// The project's look, reversed into a pack. No font or sound refs (those files may not live in
/// a skill folder). Motion is the nearest Hold / Push / Glide, taken from the first visual clip.
pub fn capture(project: &Project) -> LookPack {
    let mut text = BTreeMap::new();
    for track in &project.tracks {
        for item in &track.items {
            if let ItemBody::Text(body) = &item.body
                && body.style != TextStyle::default()
            {
                text.entry(body.preset)
                    .or_insert_with(|| body.style.clone());
            }
        }
    }
    let visual = project.tracks.iter().find_map(|track| {
        track.items.iter().find_map(|item| {
            let clip = item.clip()?;
            takes_clip_look(project, Path::new(""), track, clip).then_some(clip)
        })
    });
    LookPack {
        schema: 1,
        kinds: Vec::new(),
        aspect: None,
        captions: Some(caption_patch(&project.captions)),
        look: Some(project.look.clone()),
        background: Some(project.canvas.background),
        text,
        overlays: project
            .tracks
            .iter()
            .flat_map(|track| &track.items)
            .filter_map(|item| {
                let ItemBody::Text(body) = &item.body else {
                    return None;
                };
                item.id.starts_with("look-").then(|| OverlayLook {
                    preset: body.preset,
                    text: body.text.clone(),
                    position: body.position,
                    style: body.style.clone(),
                    start: item.start,
                    end: (item.end() < project.duration() - 0.001).then_some(item.end()),
                    anim_in: Some(body.anim_in),
                    anim_out: Some(body.anim_out),
                })
            })
            .collect(),
        clips: visual.map(|clip| ClipLook {
            grade: clip.grade.clone(),
            motion: Some(motion_of(clip)),
            fit: Some(clip.fit),
        }),
        transitions: capture_transition(project),
        sound: None,
        fonts: Vec::new(),
    }
}

fn caption_patch(style: &crate::project::CaptionStyle) -> CaptionPatch {
    CaptionPatch {
        mode: Some(style.mode),
        font: Some(style.font.clone()),
        size: Some(style.size),
        color: Some(style.color),
        highlight: Some(style.highlight),
        stroke: Some(style.stroke),
        shadow: Some(style.shadow),
        boxed: Some(style.boxed),
        ink_on_light: Some(style.ink_on_light),
        uppercase: Some(style.uppercase),
        position: Some(style.position),
        max_words: Some(style.max_words),
        max_chars: Some(style.max_chars),
    }
}

fn caption_ops(patch: &CaptionPatch, ops: &mut Vec<Op>) -> Result<()> {
    let mut push = |field: &str, value: Option<Value>| -> Result<()> {
        if let Some(value) = value {
            ops.push(Op::Set {
                path: format!("/captions/{field}"),
                value,
            });
        }
        Ok(())
    };
    push("mode", patch.mode.map(|v| json!(v)))?;
    push("font", patch.font.as_ref().map(|v| json!(v)))?;
    push("size", patch.size.map(|v| json!(v)))?;
    push("color", patch.color.map(|v| json!(v)))?;
    push("highlight", patch.highlight.map(|v| json!(v)))?;
    push("stroke", patch.stroke.map(|v| json!(v)))?;
    push("shadow", patch.shadow.map(|v| json!(v)))?;
    push("boxed", patch.boxed.map(|v| json!(v)))?;
    push("ink_on_light", patch.ink_on_light.map(|v| json!(v)))?;
    push("uppercase", patch.uppercase.map(|v| json!(v)))?;
    push("position", patch.position.map(|v| json!(v)))?;
    push("max_words", patch.max_words.map(|v| json!(v)))?;
    push("max_chars", patch.max_chars.map(|v| json!(v)))?;
    Ok(())
}

fn clip_patch(clips: &ClipLook) -> Result<Value> {
    let mut patch = serde_json::Map::new();
    if let Some(grade) = &clips.grade {
        patch.insert("grade".into(), serde_json::to_value(grade)?);
    }
    if let Some(fit) = clips.fit {
        patch.insert("fit".into(), serde_json::to_value(fit)?);
    }
    if let Some(motion) = clips.motion {
        patch.insert("ken_burns".into(), serde_json::to_value(burns_for(motion))?);
    }
    Ok(Value::Object(patch))
}

fn set(path: &str, value: &impl Serialize) -> Result<Op> {
    Ok(Op::Set {
        path: path.into(),
        value: serde_json::to_value(value)?,
    })
}

fn takes_clip_look(project: &Project, project_dir: &Path, track: &Track, clip: &Clip) -> bool {
    let Some(asset) = project.asset(&clip.asset) else {
        return false;
    };
    if speaker_clip(project, project_dir, track, asset) {
        return false;
    }
    if asset.kind == AssetKind::Image || matches!(asset.role, AssetRole::Image | AssetRole::Logo) {
        return track.kind.is_visual();
    }
    if asset.kind != AssetKind::Video {
        return false;
    }
    let role_ok = matches!(asset.role, AssetRole::Broll | AssetRole::Source);
    if track.kind == TrackKind::Overlay && role_ok {
        return true;
    }
    if project.kind == VideoKind::Explainer && track.kind == TrackKind::Main {
        return true;
    }
    project.kind == VideoKind::Documentary && track.kind == TrackKind::Main && role_ok
}

fn speaker_clip(project: &Project, project_dir: &Path, track: &Track, asset: &Asset) -> bool {
    matches!(
        project.kind,
        VideoKind::TalkingHeadShort | VideoKind::TalkingHeadLong | VideoKind::Documentary
    ) && track.kind == TrackKind::Main
        && asset.kind == AssetKind::Video
        && has_transcript(project_dir, &asset.id)
}

fn has_transcript(project_dir: &Path, asset_id: &str) -> bool {
    if project_dir
        .join(format!("transcript-{asset_id}.json"))
        .is_file()
    {
        return true;
    }
    Transcript::load(&project_dir.join("transcript.json"))
        .ok()
        .is_some_and(|t| t.asset == asset_id)
}

fn motion_of(clip: &Clip) -> ExplainerMotion {
    let Some(kb) = &clip.ken_burns else {
        return ExplainerMotion::Hold;
    };
    match (kb.from_focus, kb.to_focus) {
        (Some(a), Some(b)) if (a.x - b.x).abs() > 0.001 || (a.y - b.y).abs() > 0.001 => {
            ExplainerMotion::Glide
        }
        (None, Some(_)) | (Some(_), None) => ExplainerMotion::Glide,
        _ => ExplainerMotion::Push,
    }
}

#[allow(unreachable_patterns)]
fn burns_for(motion: ExplainerMotion) -> Option<KenBurns> {
    let push = KenBurns {
        from_scale: 1.0,
        to_scale: 1.12,
        from_focus: Some(Point::CENTER),
        to_focus: Some(Point::CENTER),
        smooth: true,
        phase: [0.0, 1.0],
    };
    match motion {
        ExplainerMotion::Hold => None,
        ExplainerMotion::Push => Some(push.clone()),
        ExplainerMotion::Glide => Some(KenBurns {
            from_scale: 1.06,
            to_scale: 1.12,
            from_focus: Some(Point::new(0.45, 0.5)),
            to_focus: Some(Point::new(0.55, 0.5)),
            smooth: true,
            phase: [0.0, 1.0],
        }),
        // ponytail: a new motion maps to a centered push until this match grows a case
        _ => Some(push),
    }
}

fn capture_transition(project: &Project) -> Option<TransitionLook> {
    let cuts: Vec<Option<&Transition>> = project
        .main()
        .items
        .iter()
        .skip(1)
        .filter_map(|item| item.clip().map(|c| c.transition.as_ref()))
        .collect();
    let (first_at, first) = cuts
        .iter()
        .enumerate()
        .find_map(|(i, t)| t.map(|t| (i, t)))?;
    for every in 1..=cuts.len().max(1) {
        let fits = cuts.iter().enumerate().all(|(i, t)| {
            let on = (i + 1) % every == 0;
            if on {
                t.is_some_and(|t| {
                    t.kind == first.kind && (t.duration - first.duration).abs() < 1e-6
                })
            } else {
                t.is_none()
            }
        });
        if fits {
            return Some(TransitionLook {
                kind: first.kind,
                duration: first.duration,
                every,
            });
        }
    }
    Some(TransitionLook {
        kind: first.kind,
        duration: first.duration,
        every: first_at + 1,
    })
}

fn reject_unknown_look(text: &str) -> Result<()> {
    let table: toml::Table = toml::from_str(text)?;
    let Some(look) = table.get("look").and_then(|v| v.as_table()) else {
        return Ok(());
    };
    for key in look.keys() {
        if !matches!(
            key.as_str(),
            "preset" | "brightness" | "contrast" | "saturation" | "temperature" | "vignette"
        ) {
            bail!("{LOOK_FILE} has unknown look field {key:?}");
        }
    }
    Ok(())
}

fn scan_fonts(skill_dir: &Path) -> Result<Vec<String>> {
    let dir = skill_dir.join("fonts");
    let Ok(read) = std::fs::read_dir(&dir) else {
        return Ok(Vec::new());
    };
    let mut names = Vec::new();
    for entry in read {
        let entry = entry?;
        let path = entry.path();
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if ext.eq_ignore_ascii_case("ttf") || ext.eq_ignore_ascii_case("otf") {
            names.push(format!("fonts/{}", entry.file_name().to_string_lossy()));
        }
    }
    names.sort();
    Ok(names)
}

fn contained_file(skill_dir: &Path, rel: &str) -> Result<PathBuf> {
    let rel_path = Path::new(rel);
    if rel.is_empty()
        || rel_path.is_absolute()
        || rel_path.components().any(|c| {
            matches!(
                c,
                Component::ParentDir | Component::Prefix(_) | Component::RootDir
            )
        })
    {
        bail!("{rel} escapes the skill folder");
    }
    let full = skill_dir.join(rel_path);
    let root = skill_dir
        .canonicalize()
        .unwrap_or_else(|_| skill_dir.to_path_buf());
    if let Ok(real) = full.canonicalize()
        && !real.starts_with(&root)
    {
        bail!("{rel} escapes the skill folder");
    }
    if !full.is_file() {
        bail!("{rel} is missing from the skill folder");
    }
    Ok(full)
}

enum SoundRef {
    Kit(sfx::Sound),
    File(String),
}

fn parse_sound_ref(spec: &str) -> Result<SoundRef> {
    if let Some(id) = spec.strip_prefix("kit:") {
        if id.is_empty() {
            bail!("kit sound id is empty");
        }
        let sound: sfx::Sound = serde_json::from_value(serde_json::Value::String(id.to_string()))
            .with_context(|| format!("unknown kit sound {id:?}"))?;
        return Ok(SoundRef::Kit(sound));
    }
    if spec.is_empty() {
        bail!("sound ref is empty");
    }
    Ok(SoundRef::File(spec.to_string()))
}

fn check_sound_ref(skill_dir: &Path, spec: &str) -> Result<()> {
    if let SoundRef::File(rel) = parse_sound_ref(spec)? {
        contained_file(skill_dir, &rel)?;
    }
    Ok(())
}

fn sound_ops(
    ws: &Workspace,
    project: &Project,
    project_dir: &Path,
    skill_dir: &Path,
    pack: &LookPack,
) -> Result<Vec<Op>> {
    let Some(sound) = &pack.sound else {
        return Ok(Vec::new());
    };
    if sound.text.is_none() && sound.cut.is_none() && sound.music.is_none() {
        return Ok(Vec::new());
    }
    let mut scratch = project.clone();
    let mut cues = Vec::new();
    if let Some(spec) = &sound.text {
        let times = text_starts(project);
        push_cues(
            ws,
            &mut scratch,
            project_dir,
            skill_dir,
            spec,
            &times,
            &mut cues,
        )?;
    }
    if let Some(spec) = &sound.cut {
        let times = cut_times(project);
        push_cues(
            ws,
            &mut scratch,
            project_dir,
            skill_dir,
            spec,
            &times,
            &mut cues,
        )?;
    }
    if !cues.is_empty() {
        sfx::place(ws, &mut scratch, project_dir, &cues, None)?;
    }
    if let Some(spec) = &sound.music {
        add_music(
            ws,
            &mut scratch,
            project_dir,
            skill_dir,
            spec,
            sound.music_db,
        )?;
    }
    for ti in 0..scratch.tracks.len() {
        for ii in 0..scratch.tracks[ti].items.len() {
            let item = &scratch.tracks[ti].items[ii];
            if project.item(&item.id).is_none() {
                scratch.tracks[ti].items[ii].id = scratch.new_id("look-sound");
            }
        }
    }
    Ok(audio_diff(project, &scratch))
}

fn text_starts(project: &Project) -> Vec<f64> {
    project
        .tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Text && !t.hidden)
        .flat_map(|t| t.items.iter())
        .filter(|i| matches!(i.body, ItemBody::Text(_)))
        .map(|i| i.start)
        .filter(|t| t.is_finite())
        .collect()
}

fn cut_times(project: &Project) -> Vec<f64> {
    project
        .main()
        .items
        .iter()
        .skip(1)
        .filter(|i| i.clip().is_some())
        .map(|i| i.start)
        .filter(|t| t.is_finite())
        .collect()
}

fn push_cues(
    ws: &Workspace,
    scratch: &mut Project,
    project_dir: &Path,
    skill_dir: &Path,
    spec: &str,
    times: &[f64],
    cues: &mut Vec<sfx::Cue>,
) -> Result<()> {
    if times.is_empty() {
        return Ok(());
    }
    match parse_sound_ref(spec)? {
        SoundRef::Kit(sound) => {
            cues.extend(times.iter().map(|&at| sfx::Cue { at, sound }));
        }
        SoundRef::File(rel) => {
            let file = skill_dir.join(rel);
            let id = asset_for_file(ws, scratch, project_dir, &file, AssetRole::Sfx)?;
            let len = scratch.asset(&id).map(|a| a.duration).unwrap_or(0.0);
            let track = ensure_audio(scratch, "sfx", "Sound effects");
            let end = scratch.duration();
            for &at in times {
                let start = at.max(0.0);
                let duration = len.min(end - start);
                if duration >= 0.001 {
                    push_clip(
                        scratch, &track, &id, start, 0.0, duration, 1.0, false, 0.0, 0.0,
                    );
                }
            }
        }
    }
    Ok(())
}

fn add_music(
    ws: &Workspace,
    scratch: &mut Project,
    project_dir: &Path,
    skill_dir: &Path,
    spec: &str,
    music_db: Option<f32>,
) -> Result<()> {
    let file = match parse_sound_ref(spec)? {
        SoundRef::File(rel) => skill_dir.join(rel),
        SoundRef::Kit(sound) => sfx::kit(ws)?
            .into_iter()
            .find(|k| k.sound == sound)
            .map(|k| k.path)
            .with_context(|| format!("kit has no file for {sound:?}"))?,
    };
    let id = asset_for_file(ws, scratch, project_dir, &file, AssetRole::Music)?;
    let len = scratch.asset(&id).map(|a| a.duration).unwrap_or(0.0);
    let total = scratch.duration();
    if len < 1.0 || total <= 0.0 {
        return Ok(());
    }
    let volume = if let Some(db) = music_db {
        10f32.powf(db / 20.0).max(0.0)
    } else {
        let music_lufs = sfx::loudness(&file, None).unwrap_or(-14.0);
        sfx::bed_volume(sfx::DEFAULT_VOICE_LUFS, music_lufs, 11.0)
    };
    let track = ensure_audio(scratch, "music", "Music");
    let mut t = 0.0;
    let mut ids = Vec::new();
    while total - t > 0.05 {
        let piece = len.min(total - t);
        ids.push(push_clip(
            scratch, &track, &id, t, 0.0, piece, volume, false, 0.0, 0.0,
        ));
        t += piece;
    }
    if let Some(id) = ids.first()
        && let Some(clip) = scratch.item_mut(id).and_then(|i| i.clip_mut())
    {
        clip.fade_in = 1.5;
    }
    if let Some(id) = ids.last()
        && let Some(clip) = scratch.item_mut(id).and_then(|i| i.clip_mut())
    {
        clip.fade_out = 1.5;
    }
    Ok(())
}

fn asset_for_file(
    ws: &Workspace,
    project: &mut Project,
    project_dir: &Path,
    file: &Path,
    role: AssetRole,
) -> Result<String> {
    let real = std::fs::canonicalize(file).ok();
    if let Some(existing) = project.assets.iter().find(|asset| {
        real.is_some() && std::fs::canonicalize(project.asset_path(project_dir, asset)).ok() == real
    }) {
        return Ok(existing.id.clone());
    }
    ws.add_asset(project, project_dir, file, role)
}

fn ensure_audio(project: &mut Project, id: &str, name: &str) -> String {
    if let Some(track) = project
        .tracks
        .iter()
        .find(|t| t.kind == TrackKind::Audio && (t.id == id || t.name == name))
    {
        return track.id.clone();
    }
    let id = if project.tracks.iter().any(|t| t.id == id) {
        project.new_id("track")
    } else {
        id.to_string()
    };
    project.tracks.push(Track {
        id: id.clone(),
        kind: TrackKind::Audio,
        name: name.into(),
        muted: false,
        hidden: false,
        items: Vec::new(),
    });
    id
}

fn push_clip(
    project: &mut Project,
    track_id: &str,
    asset: &str,
    start: f64,
    src_in: f64,
    duration: f64,
    volume: f32,
    duck: bool,
    fade_in: f64,
    fade_out: f64,
) -> String {
    let id = project.new_id("clip");
    let mut clip = Clip::new(asset, src_in);
    clip.volume = volume;
    clip.duck = duck;
    clip.fade_in = fade_in;
    clip.fade_out = fade_out;
    let item = Item {
        id: id.clone(),
        start,
        duration,
        body: ItemBody::Clip(clip),
    };
    if let Some(track) = project.tracks.iter_mut().find(|t| t.id == track_id) {
        track.items.push(item);
    }
    id
}

fn audio_diff(before: &Project, after: &Project) -> Vec<Op> {
    let mut ops = Vec::new();
    if before.assets != after.assets
        && let Ok(value) = serde_json::to_value(&after.assets)
    {
        ops.push(Op::Set {
            path: "/assets".into(),
            value,
        });
    }
    if before.sources != after.sources
        && let Ok(value) = serde_json::to_value(&after.sources)
    {
        ops.push(Op::Set {
            path: "/sources".into(),
            value,
        });
    }
    let shared = before.tracks.len().min(after.tracks.len());
    for i in 0..shared {
        if after.tracks[i].kind == TrackKind::Audio
            && before.tracks[i].items != after.tracks[i].items
            && let Ok(value) = serde_json::to_value(&after.tracks[i].items)
        {
            ops.push(Op::Set {
                path: format!("/tracks/{i}/items"),
                value,
            });
        }
    }
    for track in after.tracks.iter().skip(before.tracks.len()) {
        if let Ok(value) = serde_json::to_value(track) {
            ops.push(Op::Set {
                path: "/tracks/-".into(),
                value,
            });
        }
    }
    ops
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::{Text, TreatLook};
    use crate::transcript::TranscriptWord;

    fn ws(root: &Path) -> Workspace {
        Workspace {
            root: root.to_path_buf(),
        }
    }

    fn write_pack(dir: &Path, body: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(LOOK_FILE), body).unwrap();
    }

    fn asset(id: &str, kind: AssetKind, role: AssetRole) -> Asset {
        Asset {
            id: id.into(),
            path: format!("{id}.bin").into(),
            kind,
            role,
            duration: 8.0,
            width: 1920,
            height: 1080,
            fps: 30.0,
            has_audio: kind == AssetKind::Video,
        }
    }

    fn clip(id: &str, asset_id: &str, duration: f64) -> Item {
        Item {
            id: id.into(),
            start: 0.0,
            duration,
            body: ItemBody::Clip(Clip::new(asset_id, 0.0)),
        }
    }

    fn hook(id: &str, font: Option<&str>) -> Item {
        let mut style = TextStyle::default();
        style.font = font.map(str::to_string);
        Item {
            id: id.into(),
            start: 0.2,
            duration: 2.0,
            body: ItemBody::Text(Text {
                text: "Hello".into(),
                preset: TextPreset::Hook,
                style,
                position: None,
                anim_in: crate::project::Anim::Fade,
                anim_out: crate::project::Anim::Fade,
            }),
        }
    }

    #[test]
    fn load_rejects_schema_unknown_keys_and_escaping_refs() {
        let tmp = tempfile::tempdir().unwrap();
        let skill = tmp.path().join("skill");
        write_pack(&skill, "schema = 2\n");
        let err = load(&skill).unwrap_err().to_string();
        assert!(err.contains("schema"), "{err}");

        write_pack(&skill, "schema = 1\nnope = 1\n");
        let err = format!("{:#}", load(&skill).unwrap_err());
        assert!(err.contains("unknown") || err.contains("nope"), "{err}");

        write_pack(&skill, "schema = 1\n[look]\nnope = 1\n");
        let err = load(&skill).unwrap_err().to_string();
        assert!(err.contains("unknown look field"), "{err}");

        write_pack(&skill, "schema = 1\nfonts = [\"../secret.ttf\"]\n");
        let err = load(&skill).unwrap_err().to_string();
        assert!(err.contains("escapes"), "{err}");

        write_pack(&skill, "schema = 1\nfonts = [\"/etc/passwd\"]\n");
        let err = load(&skill).unwrap_err().to_string();
        assert!(err.contains("escapes"), "{err}");

        write_pack(&skill, "schema = 1\nfonts = [\"fonts/missing.ttf\"]\n");
        let err = load(&skill).unwrap_err().to_string();
        assert!(err.contains("missing"), "{err}");

        std::fs::create_dir(skill.join("fonts")).unwrap();
        let ttf =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/fonts/Anton/Anton-Regular.ttf");
        std::fs::copy(&ttf, skill.join("fonts/Anton-Regular.ttf")).unwrap();
        write_pack(&skill, "schema = 1\n[sound]\ntext = \"kit:tick\"\n");
        let pack = load(&skill).unwrap();
        assert_eq!(pack.schema, 1);
        assert_eq!(pack.fonts, vec!["fonts/Anton-Regular.ttf".to_string()]);
        assert_eq!(pack.sound.unwrap().text.as_deref(), Some("kit:tick"));
    }

    #[test]
    fn apply_is_one_atomic_batch_and_skips_the_speaker() {
        let tmp = tempfile::tempdir().unwrap();
        let skill = tmp.path().join("skill");
        write_pack(
            &skill,
            r##"
schema = 1
background = "#112233"
[captions]
size = 0.08
color = "#00FF00"
[text.hook]
font = "Anton"
[clips]
fit = "contain"
[clips.grade]
look = "mono"
"##,
        );
        let mut project = Project::new("talk", VideoKind::TalkingHeadShort, Canvas::vertical());
        project
            .assets
            .push(asset("speaker", AssetKind::Video, AssetRole::Source));
        project
            .assets
            .push(asset("still", AssetKind::Image, AssetRole::Broll));
        project.main_mut().items.push(clip("talk", "speaker", 4.0));
        project.tracks[1].items.push(clip("still-1", "still", 2.0));
        project.tracks[2].items.push(hook("hook-1", None));
        Transcript {
            asset: "speaker".into(),
            provider: "test".into(),
            language: None,
            duration: 4.0,
            words: vec![TranscriptWord {
                text: "hi".into(),
                start: 0.0,
                end: 0.4,
                confidence: None,
            }],
        }
        .save(&tmp.path().join("transcript.json"))
        .unwrap();

        let workspace = ws(tmp.path());
        let ops = ops_for(
            &workspace,
            &project,
            tmp.path(),
            &skill,
            &load(&skill).unwrap(),
        )
        .unwrap();
        assert!(ops.len() > 1, "{ops:?}");
        let mut doomed = project.clone();
        let mut bad = ops.clone();
        bad.push(Op::Set {
            path: "/canvas/width".into(),
            value: json!("nope"),
        });
        assert!(crate::ops::apply_all(&mut doomed, &bad).is_err());
        assert_eq!(doomed, project);

        let n = apply(&workspace, &mut project, tmp.path(), &skill).unwrap();
        assert_eq!(n, ops.len());
        assert_eq!(project.captions.size, 0.08);
        assert_eq!(project.captions.color, Color::parse("#00FF00").unwrap());
        assert_eq!(project.canvas.background, Color::parse("#112233").unwrap());
        let styled = match &project.tracks[2].items[0].body {
            ItemBody::Text(text) => text.style.font.as_deref(),
            _ => None,
        };
        assert_eq!(styled, Some("Anton"));
        let still = project.tracks[1].items[0].clip().unwrap();
        assert_eq!(still.fit, Fit::Contain);
        assert_eq!(still.grade.as_ref().map(|g| g.look), Some(TreatLook::Mono));
        let speaker = project.main().items[0].clip().unwrap();
        assert!(speaker.grade.is_none());
        assert_eq!(speaker.fit, Fit::Cover);

        project.tracks[2].items.push(hook("hook-2", Some("Inter")));
        apply(&workspace, &mut project, tmp.path(), &skill).unwrap();
        for item in &project.tracks[2].items {
            let ItemBody::Text(text) = &item.body else {
                continue;
            };
            assert_eq!(text.style.font.as_deref(), Some("Anton"));
        }

        let mut doc = Project::new("doc", VideoKind::Documentary, Canvas::vertical());
        doc.assets
            .push(asset("speaker", AssetKind::Video, AssetRole::Source));
        doc.assets
            .push(asset("found", AssetKind::Video, AssetRole::Broll));
        doc.main_mut().items.push(clip("talk", "speaker", 3.0));
        doc.main_mut().items.push(clip("found-1", "found", 2.0));
        Transcript {
            asset: "speaker".into(),
            provider: "test".into(),
            language: None,
            duration: 3.0,
            words: vec![TranscriptWord {
                text: "hi".into(),
                start: 0.0,
                end: 0.3,
                confidence: None,
            }],
        }
        .save(&tmp.path().join("transcript.json"))
        .unwrap();
        apply(&workspace, &mut doc, tmp.path(), &skill).unwrap();
        assert!(doc.main().items[0].clip().unwrap().grade.is_none());
        assert_eq!(
            doc.main().items[1]
                .clip()
                .unwrap()
                .grade
                .as_ref()
                .map(|g| g.look),
            Some(TreatLook::Mono)
        );
    }

    #[test]
    fn capture_apply_round_trip_reproduces_the_look() {
        let tmp = tempfile::tempdir().unwrap();
        let mut source = Project::new("a", VideoKind::Explainer, Canvas::square());
        source
            .assets
            .push(asset("pic", AssetKind::Image, AssetRole::Image));
        source.main_mut().items.push(clip("c1", "pic", 2.0));
        source.main_mut().items.push(clip("c2", "pic", 2.0));
        source.tracks[2].items.push(hook("hook", Some("Anton")));
        source.captions.size = 0.125;
        source.captions.color = Color::parse("#ABCDEF").unwrap();
        source.canvas.background = Color::parse("#010203").unwrap();
        source.look.preset = crate::project::LookPreset::Warm;
        source.look.contrast = 0.25;
        let visual = source.main_mut().items[0].clip_mut().unwrap();
        visual.fit = Fit::Contain;
        visual.grade = Some(Treatment {
            look: TreatLook::Teal,
            ..Treatment::default()
        });
        visual.ken_burns = burns_for(ExplainerMotion::Push);
        source.main_mut().items[1].clip_mut().unwrap().transition = Some(Transition {
            kind: TransitionKind::Dip,
            duration: 0.4,
        });

        let pack = capture(&source);
        let skill = tmp.path().join("skill");
        std::fs::create_dir(&skill).unwrap();
        std::fs::write(
            skill.join(LOOK_FILE),
            toml::to_string_pretty(&pack).unwrap(),
        )
        .unwrap();
        let loaded = load(&skill).unwrap();
        assert_eq!(loaded.schema, 1);
        assert!(loaded.fonts.is_empty());
        assert!(loaded.sound.is_none());

        let mut other = Project::new("b", VideoKind::Explainer, Canvas::square());
        other
            .assets
            .push(asset("pic", AssetKind::Image, AssetRole::Image));
        other.main_mut().items.push(clip("c1", "pic", 2.0));
        other.main_mut().items.push(clip("c2", "pic", 2.0));
        other.tracks[2].items.push(hook("hook", Some("Inter")));
        other.captions.size = 0.02;
        other.look.preset = crate::project::LookPreset::Cool;
        apply(&ws(tmp.path()), &mut other, tmp.path(), &skill).unwrap();

        assert_eq!(other.captions, source.captions);
        assert_eq!(other.look, source.look);
        assert_eq!(other.canvas.background, source.canvas.background);
        let font = match &other.tracks[2].items[0].body {
            ItemBody::Text(text) => text.style.font.clone(),
            _ => None,
        };
        assert_eq!(font.as_deref(), Some("Anton"));
        let first = other.main().items[0].clip().unwrap();
        assert_eq!(first.fit, Fit::Contain);
        assert_eq!(first.grade.as_ref().map(|g| g.look), Some(TreatLook::Teal));
        assert_eq!(first.ken_burns, burns_for(ExplainerMotion::Push));
        assert_eq!(
            other.main().items[1].clip().unwrap().transition,
            Some(Transition {
                kind: TransitionKind::Dip,
                duration: 0.4,
            })
        );
        let again = capture(&other);
        assert_eq!(again.clips, pack.clips);
        assert_eq!(again.transitions, pack.transitions);
        assert_eq!(again.background, pack.background);
    }

    #[test]
    fn overlays_parse_apply_and_capture_with_open_project_end() {
        let tmp = tempfile::tempdir().unwrap();
        let skill = tmp.path().join("look");
        write_pack(
            &skill,
            r##"
schema = 1
[text.marker]
font = "Inter"
accent = "#D97757"
[[overlays]]
preset = "marker"
text = "Evidence"
start = 1.0
end = 2.0
position = { x = 0.5, y = 0.3 }
anim_in = "wipe"
anim_out = "fade"
[overlays.style]
color = "#1C1B1E"
[[overlays]]
preset = "meter"
text = "HP"
"##,
        );
        let pack = load(&skill).unwrap();
        assert_eq!(pack.overlays.len(), 2);
        let mut project = Project::new("test", VideoKind::Explainer, Canvas::vertical());
        project
            .assets
            .push(asset("pic", AssetKind::Image, AssetRole::Image));
        project.main_mut().items.push(clip("c1", "pic", 4.0));
        project.tracks[2].items.push(hook("manual-hook", None));
        apply(&ws(tmp.path()), &mut project, tmp.path(), &skill).unwrap();
        let overlays: Vec<_> = project
            .tracks
            .iter()
            .flat_map(|track| &track.items)
            .filter(|item| item.id.starts_with("look-overlay-"))
            .collect();
        assert_eq!(overlays.len(), 2);
        let marker = overlays.iter().find(|item| item.start == 1.0).unwrap();
        let ItemBody::Text(text) = &marker.body else {
            panic!("expected text")
        };
        assert_eq!(text.position, Some(Point { x: 0.5, y: 0.3 }));
        assert_eq!(text.anim_in, Anim::Wipe);
        assert_eq!(text.style.font.as_deref(), Some("Inter"));
        assert_eq!(text.style.color, Some(Color::parse("#1C1B1E").unwrap()));
        let captured = capture(&project);
        assert_eq!(
            captured.overlays.len(),
            2,
            "ordinary story text must not become decoration"
        );
        assert!(
            captured
                .overlays
                .iter()
                .any(|o| o.preset == TextPreset::Meter && o.end.is_none())
        );
        assert!(
            captured
                .overlays
                .iter()
                .any(|o| o.preset == TextPreset::Marker && o.end == Some(2.0))
        );
        let json = serde_json::to_value(&project).unwrap();
        write_pack(
            &skill,
            "schema = 1\n[[overlays]]\npreset = \"meter\"\nstart = -1.0\n",
        );
        assert!(apply(&ws(tmp.path()), &mut project, tmp.path(), &skill).is_err());
        assert_eq!(serde_json::to_value(&project).unwrap(), json);
    }

    #[test]
    fn reapply_replaces_generated_overlays_cues_and_looped_kit_bed_only() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = ws(tmp.path());
        let skill = tmp.path().join("look");
        write_pack(&skill, include_str!("../skill/looks/arcade/veecut.toml"));
        let mut project = Project::new("test", VideoKind::Explainer, Canvas::vertical());
        let mut pic = asset("pic", AssetKind::Image, AssetRole::Image);
        pic.duration = 40.0;
        project.assets.push(pic);
        project.main_mut().items.push(clip("c1", "pic", 18.0));
        project.main_mut().items.push(clip("c2", "pic", 18.0));
        project.tracks[2].items.push(hook("manual-hook", None));
        project.normalize();
        // Register the generated bed without a media probe: no fixture media or ffmpeg needed.
        let bed = sfx::kit(&workspace)
            .unwrap()
            .into_iter()
            .find(|entry| entry.sound == sfx::Sound::Chiptune)
            .unwrap();
        let mut music = asset("bed", AssetKind::Audio, AssetRole::Music);
        music.path = bed.path;
        music.duration = bed.duration;
        project.assets.push(music);
        apply(&workspace, &mut project, tmp.path(), &skill).unwrap();
        let generated: Vec<_> = project
            .tracks
            .iter()
            .flat_map(|track| &track.items)
            .filter(|item| item.id.starts_with("look-"))
            .cloned()
            .collect();
        assert!(
            generated
                .iter()
                .any(|item| matches!(item.body, ItemBody::Text(_)))
        );
        let beds: Vec<_> = generated
            .iter()
            .filter(|item| item.clip().is_some_and(|c| c.asset == "bed"))
            .collect();
        assert_eq!(beds.len(), 2, "a long kit sound loops as a bed, not a cue");
        assert!((beds.iter().map(|item| item.duration).sum::<f64>() - 36.0).abs() < 0.001);
        assert!(
            beds.iter()
                .all(|item| (item.clip().unwrap().volume - 0.1).abs() < 0.0001)
        );
        let mut manual = generated
            .iter()
            .find(|item| item.clip().is_some())
            .unwrap()
            .clone();
        manual.id = "manual-audio".into();
        let track = project
            .tracks
            .iter_mut()
            .find(|track| track.kind == TrackKind::Audio)
            .unwrap();
        track.items.push(manual.clone());
        let asset_count = project.assets.len();
        apply(&workspace, &mut project, tmp.path(), &skill).unwrap();
        let again: Vec<_> = project
            .tracks
            .iter()
            .flat_map(|track| &track.items)
            .filter(|item| item.id.starts_with("look-"))
            .cloned()
            .collect();
        assert_eq!(again, generated);
        assert_eq!(project.assets.len(), asset_count);
        assert_eq!(project.item("manual-audio"), Some(&manual));
        assert!(project.item("manual-hook").is_some());
        write_pack(&skill, "schema = 1\n");
        apply(&workspace, &mut project, tmp.path(), &skill).unwrap();
        assert!(
            project
                .tracks
                .iter()
                .flat_map(|track| &track.items)
                .all(|item| !item.id.starts_with("look-"))
        );
        assert_eq!(project.item("manual-audio"), Some(&manual));
    }
}
