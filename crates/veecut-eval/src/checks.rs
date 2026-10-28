//! Semantic checks for every eval task (see the Model eval plan, "Tasks").
//!
//! `check` grades the projects an agent left behind against the frozen baseline projects and
//! baseline transcripts. It never trusts what the agent says about its own work, never calls a
//! model, and never panics: an invalid or odd project is a failed check.
//!
//! Failure classes: a missing task prerequisite or missing ground truth is `Some("check")`
//! (the task cannot be graded); an agent that simply did the wrong thing leaves `None` so the
//! harness (or a human) decides between model, app and environment.
//!
//! Conventions:
//! - Levels 2 to 4 compare against `before[0]` by index and require the same number of projects;
//!   every other baseline project must be byte-for-byte unchanged.
//! - "Nothing else changed" is a JSON diff of the whole project. `Project` holds no timestamps
//!   or other volatile fields, so nothing is excluded from the diff.
//! - Source/timeline reasoning uses the main track only (source ranges of main clips, the way
//!   `Project::source_to_timeline` does) and finds assets by file name, because an agent may
//!   re-add a file under a new asset id.

use std::collections::{BTreeMap, BTreeSet};
use std::panic::{AssertUnwindSafe, catch_unwind};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use veecut_engine::{
    graphics::{CaptionPlacement, FaceFit, Graphics},
    ops::{Op, apply, apply_all},
    project::{
        Asset, AssetKind, AssetRole, CaptionMode, Clip, Color, FaceSample, Item, ItemBody, Project,
        Rect, Text, TextPreset, TrackKind, VideoKind,
    },
    render::canvas_rect,
    transcript::Transcript,
};

use crate::tasks::{Check, Task, canvas as canvas_of, filename, words};

/// Source seconds of a phrase that may still be on the timeline after "cutting" it.
const PHRASE_TOL: f64 = 0.05;
/// Seconds of slack on measured lengths.
const LENGTH_TOL: f64 = 0.05;
/// Multicam sync tolerance in seconds.
const SYNC_TOL: f64 = 0.1;
/// How far a cut may sit from a beat, in seconds.
const BEAT_TOL: f64 = 0.12;
/// Detector slack around known silences, in seconds.
const SILENCE_TOL: f64 = 0.2;
/// Slower than this is not a "speed-up" of a silent stretch.
const MIN_SPEEDUP: f64 = 1.5;
/// Smallest caption text, as a fraction of the canvas height, that still counts as readable.
const MIN_CAPTION_SIZE: f32 = 0.03;
/// Mouth and chin zone below the detected face box top / past its bottom (engine geometry,
/// `graphics/placement.rs`; a detector-derived proxy, not hand-annotated mouth truth).
const MOUTH_TOP: f32 = 0.55;
const CHIN_MARGIN: f32 = 0.12;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CheckResult {
    pub pass: bool,
    pub detail: String,
    pub evidence: Value,
    pub failure_class: Option<String>,
}

#[derive(Debug)]
struct Fail {
    detail: String,
    evidence: Value,
    class: Option<&'static str>,
}

type Verdict = Result<(String, Value), Fail>;

/// The agent's work is wrong.
fn bad(detail: impl Into<String>, evidence: Value) -> Fail {
    Fail {
        detail: detail.into(),
        evidence,
        class: None,
    }
}

/// The task cannot be graded: missing prerequisite or ground truth.
fn miss(detail: impl Into<String>) -> Fail {
    Fail {
        detail: detail.into(),
        evidence: Value::Null,
        class: Some("check"),
    }
}

fn pass(detail: impl Into<String>, evidence: Value) -> Verdict {
    Ok((detail.into(), evidence))
}

macro_rules! need {
    ($cond:expr, $detail:expr) => {
        need!($cond, $detail, Value::Null)
    };
    ($cond:expr, $detail:expr, $evidence:expr) => {
        if !($cond) {
            return Err(bad($detail, $evidence));
        }
    };
}

/// Grades one run. `before` are the frozen baseline projects (empty at level 1), `after` every
/// project the run left, `transcripts` the frozen baseline transcripts, `inbox_unchanged`
/// whether the harness found `inbox/` byte-identical to its snapshot.
pub fn check(
    task: &Task,
    before: &[Project],
    after: &[Project],
    transcripts: &[Transcript],
    inbox_unchanged: bool,
) -> CheckResult {
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        evaluate(task, before, after, transcripts, inbox_unchanged)
    }))
    .unwrap_or_else(|payload| {
        let message = payload
            .downcast_ref::<&str>()
            .map(|s| (*s).to_owned())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_default();
        Err(Fail {
            detail: format!("the check could not evaluate this project (panic: {message})"),
            evidence: Value::Null,
            class: Some("check"),
        })
    });
    let (pass, detail, observed, class) = match outcome {
        Ok((detail, observed)) => (true, detail, observed, None),
        Err(f) => (false, f.detail, f.evidence, f.class.map(str::to_owned)),
    };
    CheckResult {
        pass,
        detail,
        failure_class: class,
        evidence: json!({
            "task": task.id,
            "level": task.level,
            "expected": task.check,
            "provenance": task.provenance,
            "tolerances": tolerances(&task.check),
            "observed": observed,
        }),
    }
}

/// The concrete thresholds each check applies beyond the plan's wording, recorded with every
/// result so a reader can see them.
fn tolerances(check: &Check) -> Value {
    match check {
        Check::Run { .. } => {
            json!({"every_result_project_must_match": true, "sfx_assets_ignored": true})
        }
        Check::CaptionSize { .. } => json!({"size_abs": 1e-5}),
        Check::Highlight { .. } => json!({}),
        Check::Hook { .. } => {
            json!({"new_title_start_max_s": 1.0, "new_title_min_duration_s": 1.0})
        }
        Check::Cut { .. } => {
            json!({"phrase_left_max_s": PHRASE_TOL, "removed_slack_s": LENGTH_TOL, "removed_footage_within": "max_removed of the phrase"})
        }
        Check::Start { .. } => {
            json!({"opening_window_s": [-0.75, 0.25], "earlier_left_max_s": PHRASE_TOL, "lost_after_max_s": 0.5})
        }
        Check::Broll { .. } => {
            json!({"word_distance_s": 1.0, "duration_s": [2.0, 4.0], "duration_slack": "one frame"})
        }
        Check::LowerThird { .. } => json!({"start_before_s": 3.0, "min_duration_s": 1.0}),
        Check::Music { .. } => {
            json!({"coverage_min": 0.9, "quiet_volume_max": 0.5, "volume_min": 0.01})
        }
        Check::Clips { .. } => json!({"clips": 3, "seconds": [20, 60], "max_shared_footage": 0.5}),
        Check::Podcast { .. } => json!({"sync_s": SYNC_TOL, "min_switches": 2}),
        Check::Tutorial { .. } => {
            json!({"silence_edge_s": SILENCE_TOL, "min_speedup": MIN_SPEEDUP, "min_silence_sped": 0.5, "speech_lost_max_s": 0.1})
        }
        Check::Vlog { .. } => {
            json!({"seconds": [50, 70], "beat_s": BEAT_TOL, "cuts_on_beat": "all"})
        }
        Check::Explainer { .. } => {
            json!({"sentence_slack": 0.2, "distinct_start_share": 0.8, "start_slot_s": 0.1})
        }
        Check::Promo { .. } => json!({"beat_s": BEAT_TOL}),
        Check::Lyric { .. } => json!({"start_s": 0.3, "span_s": 0.1, "baseline_search_s": 3.0}),
        Check::Mouth { .. } => {
            json!({"min_pages_share": 0.8, "min_caption_size": MIN_CAPTION_SIZE, "mouth_top": MOUTH_TOP, "chin_margin": CHIN_MARGIN})
        }
    }
}

fn evaluate(
    task: &Task,
    before: &[Project],
    after: &[Project],
    transcripts: &[Transcript],
    inbox_unchanged: bool,
) -> Verdict {
    if let Some(reason) = &task.prerequisite {
        return Err(miss(reason.clone()));
    }
    for p in before {
        if let Err(e) = p.validate() {
            return Err(miss(format!(
                "baseline project {:?} is invalid: {e:#}",
                p.name
            )));
        }
    }
    for p in after {
        if let Err(e) = p.validate() {
            return Err(bad(
                format!("agent project {:?} is invalid: {e:#}", p.name),
                Value::Null,
            ));
        }
    }
    need!(
        inbox_unchanged,
        "files in inbox/ changed; originals must never be modified",
        json!({"inbox_unchanged": false})
    );
    match &task.check {
        Check::Run {
            kind,
            sources,
            width,
            height,
        } => new_project(*kind, sources, *width, *height, after),
        Check::CaptionSize { size } => caption_size(*size, before, after),
        Check::Highlight { color } => highlight(color, before, after),
        Check::Hook { item, text } => hook(item.as_deref(), text, before, after),
        Check::Cut {
            asset,
            start,
            end,
            max_removed,
        } => cut(asset, *start, *end, *max_removed, before, after),
        Check::Start { asset, start } => start_at(asset, *start, before, after),
        Check::Broll {
            file,
            asset,
            source_time,
        } => broll(file, asset, *source_time, before, after),
        Check::LowerThird { name, role } => lower_third(name, role, before, after),
        Check::Music { file } => music(file, before, after),
        Check::Clips { topic } => clips(topic, before, after, transcripts),
        Check::Podcast {
            cam_a,
            cam_b,
            offset,
        } => podcast(cam_a, cam_b, *offset, before, after),
        Check::Tutorial { silences } => tutorial(silences, before, after),
        Check::Vlog { beats } => vlog(beats, before, after),
        Check::Explainer { sentences } => explainer(*sentences, before, after),
        Check::Promo { item, text, beats } => promo(item, text, beats, before, after),
        Check::Lyric { at, text } => lyric(*at, text, before, after),
        Check::Mouth { asset, samples } => mouth(asset, samples, before, after),
    }
}

// ---------------------------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------------------------

fn list(v: &[String]) -> String {
    v.iter().take(8).cloned().collect::<Vec<_>>().join(", ")
}

fn to_json(p: &Project) -> Result<Value, Fail> {
    serde_json::to_value(p)
        .map_err(|e| bad(format!("project cannot be serialized: {e}"), Value::Null))
}

/// Numbers closer than this are the same (JSON round trips and ripple recomputation leave
/// noise of about 1e-14).
const NUMBER_TOL: f64 = 1e-6;

fn diff(a: &Value, b: &Value, path: String, out: &mut Vec<String>) {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            for k in x.keys().chain(y.keys().filter(|k| !x.contains_key(*k))) {
                let p = format!("{path}/{k}");
                match (x.get(k), y.get(k)) {
                    (Some(u), Some(v)) => diff(u, v, p, out),
                    _ => out.push(p),
                }
            }
        }
        (Value::Array(x), Value::Array(y)) if x.len() == y.len() => {
            for (i, (u, v)) in x.iter().zip(y).enumerate() {
                diff(u, v, format!("{path}/{i}"), out);
            }
        }
        (Value::Number(x), Value::Number(y)) => {
            let same = match (x.as_f64(), y.as_f64()) {
                (Some(u), Some(v)) => (u - v).abs() <= NUMBER_TOL,
                _ => x == y,
            };
            if !same {
                out.push(path);
            }
        }
        _ => {
            if a != b {
                out.push(path);
            }
        }
    }
}

/// `/tracks/0/items/<n>/start`: the main track's starts are computed from the durations and
/// the order by `Project::normalize` after every op, so they are not an edit of their own.
fn computed_main_start(path: &str) -> bool {
    path.strip_prefix("/tracks/0/items/")
        .and_then(|rest| rest.strip_suffix("/start"))
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|c| c.is_ascii_digit()))
}

/// JSON pointers of every field that differs between two projects, with numbers compared to
/// within `NUMBER_TOL` and the computed main-track starts left out (durations, order, source
/// ranges and every other field still count).
fn changed(a: &Project, b: &Project) -> Result<Vec<String>, Fail> {
    let mut out = vec![];
    diff(&to_json(a)?, &to_json(b)?, String::new(), &mut out);
    out.retain(|p| !computed_main_start(p));
    Ok(out)
}

/// Equal by the same comparator as `changed`. Serialization trouble counts as different.
fn same(a: &Project, b: &Project) -> bool {
    changed(a, b).is_ok_and(|c| c.is_empty())
}

/// The baseline project to compare with and the agent's counterpart. Every other baseline
/// project must be unchanged and no project may appear or vanish.
fn pair<'a>(
    before: &'a [Project],
    after: &'a [Project],
) -> Result<(&'a Project, &'a Project), Fail> {
    let Some(b) = before.first() else {
        return Err(miss("no baseline project to compare with"));
    };
    need!(
        after.len() == before.len(),
        format!(
            "project count changed from {} to {}",
            before.len(),
            after.len()
        ),
        json!({"before": before.iter().map(|p| &p.name).collect::<Vec<_>>(), "after": after.iter().map(|p| &p.name).collect::<Vec<_>>()})
    );
    for (x, y) in before.iter().zip(after).skip(1) {
        need!(
            same(x, y),
            format!(
                "project {:?}, which the task did not ask to change, was modified",
                x.name
            )
        );
    }
    Ok((b, &after[0]))
}

/// Items and assets the agent added, after proving everything else is unchanged.
#[derive(Default)]
struct Added {
    items: Vec<(String, TrackKind, Item)>,
    assets: Vec<Asset>,
}

fn additions(before: &Project, after: &Project) -> Result<Added, Fail> {
    let mut stripped = after.clone();
    let mut added = Added::default();
    for track in &mut stripped.tracks {
        let (old, new): (Vec<Item>, Vec<Item>) = std::mem::take(&mut track.items)
            .into_iter()
            .partition(|i| before.find(&i.id).is_some());
        track.items = old;
        added
            .items
            .extend(new.into_iter().map(|i| (track.id.clone(), track.kind, i)));
    }
    let (old, new): (Vec<Asset>, Vec<Asset>) = std::mem::take(&mut stripped.assets)
        .into_iter()
        .partition(|a| before.asset(&a.id).is_some());
    stripped.assets = old;
    added.assets = new;
    stripped.tracks.retain(|t| before.track(&t.id).is_some());
    let paths = changed(before, &stripped)?;
    need!(
        paths.is_empty(),
        format!(
            "beyond the requested additions, these project fields changed: {}",
            list(&paths)
        ),
        json!({"changed": paths})
    );
    Ok(added)
}

fn norm(s: &str) -> String {
    s.replace('*', "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Exact text, ignoring `*accent*` markers and whitespace layout.
fn text_eq(actual: &str, want: &str) -> bool {
    norm(actual) == norm(want)
}

fn text_of(item: &Item) -> Option<&Text> {
    match &item.body {
        ItemBody::Text(t) => Some(t),
        _ => None,
    }
}

fn has_run(hay: &[String], needle: &[String]) -> Option<usize> {
    if needle.is_empty() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

fn file_of(before: &[Project], id: &str) -> Option<String> {
    before
        .iter()
        .find_map(|p| p.asset(id))
        .map(|a| filename(&a.path))
}

fn ids_for(p: &Project, file: &str) -> Vec<String> {
    p.assets
        .iter()
        .filter(|a| filename(&a.path) == file)
        .map(|a| a.id.clone())
        .collect()
}

fn main_ids(p: &Project) -> Vec<String> {
    let set: BTreeSet<String> = p
        .main()
        .items
        .iter()
        .filter_map(|i| i.clip().map(|c| c.asset.clone()))
        .collect();
    set.into_iter().collect()
}

fn main_end(p: &Project) -> f64 {
    p.main().items.iter().map(Item::end).fold(0.0, f64::max)
}

fn merge(mut v: Vec<(f64, f64)>) -> Vec<(f64, f64)> {
    v.retain(|(a, b)| b > a);
    v.sort_by(|x, y| x.0.total_cmp(&y.0));
    let mut out: Vec<(f64, f64)> = vec![];
    for (a, b) in v {
        match out.last_mut() {
            Some(last) if a <= last.1 + 1e-6 => last.1 = last.1.max(b),
            _ => out.push((a, b)),
        }
    }
    out
}

fn overlap_len(v: &[(f64, f64)], a: f64, b: f64) -> f64 {
    v.iter().map(|&(s, e)| (e.min(b) - s.max(a)).max(0.0)).sum()
}

fn total(v: &[(f64, f64)]) -> f64 {
    v.iter().map(|&(s, e)| e - s).sum()
}

/// `a` minus `b`; both sorted and merged.
fn subtract(a: &[(f64, f64)], b: &[(f64, f64)]) -> Vec<(f64, f64)> {
    let mut out = vec![];
    for &(s, e) in a {
        let mut cur = s;
        for &(bs, be) in b {
            if be <= cur || bs >= e {
                continue;
            }
            if bs > cur {
                out.push((cur, bs));
            }
            cur = cur.max(be);
        }
        if cur < e {
            out.push((cur, e));
        }
    }
    out
}

/// Source ranges of the main-track clips of these assets (merged).
fn kept(p: &Project, ids: &[String]) -> Vec<(f64, f64)> {
    merge(
        p.main()
            .items
            .iter()
            .filter_map(|item| {
                let clip = item.clip()?;
                ids.contains(&clip.asset)
                    .then(|| (clip.src_in, clip.src_out(item.duration)))
            })
            .collect(),
    )
}

/// Timeline seconds of every beat that the project's music clips play.
fn beat_times(p: &Project, beats: &[f64]) -> Vec<f64> {
    let mut out = vec![];
    for track in p
        .tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Audio && !t.muted && !t.hidden)
    {
        for item in &track.items {
            let Some(clip) = item.clip() else { continue };
            if clip.muted
                || !p
                    .asset(&clip.asset)
                    .is_some_and(|a| a.role == AssetRole::Music)
            {
                continue;
            }
            let end = clip.src_out(item.duration);
            out.extend(
                beats
                    .iter()
                    .filter(|&&b| b >= clip.src_in && b < end)
                    .map(|&b| item.start + (b - clip.src_in) / clip.speed),
            );
        }
    }
    out.sort_by(f64::total_cmp);
    out
}

fn near(times: &[f64], t: f64, tol: f64) -> bool {
    times.iter().any(|b| (b - t).abs() <= tol)
}

// ---------------------------------------------------------------------------------------------
// Level 1: run
// ---------------------------------------------------------------------------------------------

/// Expected files actually in use: pictures and video on a visible main or overlay track,
/// audio on a visible audio track. An asset that is only listed does not count.
fn used_files(p: &Project) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for track in p.tracks.iter().filter(|t| !t.hidden) {
        for item in &track.items {
            let Some(clip) = item.clip() else { continue };
            let Some(asset) = p.asset(&clip.asset) else {
                continue;
            };
            let fits = match asset.kind {
                AssetKind::Audio => track.kind == TrackKind::Audio,
                _ => matches!(track.kind, TrackKind::Main | TrackKind::Overlay),
            };
            if fits {
                out.insert(filename(&asset.path));
            }
        }
    }
    out
}

/// Every project the run left must be a right result: right kind and canvas, exactly the
/// expected source files, each of them used. A wrong or stray project fails the task.
fn new_project(
    kind: VideoKind,
    sources: &[String],
    width: u32,
    height: u32,
    after: &[Project],
) -> Verdict {
    need!(!after.is_empty(), "the run left no project");
    let want: BTreeSet<String> = sources.iter().cloned().collect();
    let mut wrong = vec![];
    for p in after {
        let mut problems = vec![];
        if p.kind != kind {
            problems.push(format!("kind is {} instead of {kind}", p.kind));
        }
        if (p.canvas.width, p.canvas.height) != (width, height) {
            problems.push(format!(
                "canvas is {}x{} instead of {width}x{height}",
                p.canvas.width, p.canvas.height
            ));
        }
        let files: BTreeSet<String> = p
            .assets
            .iter()
            .filter(|a| a.role != AssetRole::Sfx)
            .map(|a| filename(&a.path))
            .collect();
        let missing: Vec<_> = want.difference(&files).cloned().collect();
        let extra: Vec<_> = files.difference(&want).cloned().collect();
        if !missing.is_empty() {
            problems.push(format!("sources missing: {}", missing.join(", ")));
        }
        if !extra.is_empty() {
            problems.push(format!("unexpected sources: {}", extra.join(", ")));
        }
        let used = used_files(p);
        let idle: Vec<_> = want
            .intersection(&files)
            .filter(|f| !used.contains(*f))
            .cloned()
            .collect();
        if !idle.is_empty() {
            problems.push(format!(
                "sources not used on a visible track of the right kind: {}",
                idle.join(", ")
            ));
        }
        if !p.main().items.iter().any(|i| i.clip().is_some()) {
            problems.push("the main track has no clip".into());
        }
        if !problems.is_empty() {
            wrong.push(json!({"project": p.name, "problems": problems}));
        }
    }
    need!(
        wrong.is_empty(),
        format!(
            "wrong result projects: {}",
            wrong
                .iter()
                .map(|w| w.to_string())
                .collect::<Vec<_>>()
                .join(" | ")
        ),
        json!({"projects": wrong})
    );
    pass(
        format!(
            "{} project(s) with the right kind, canvas and used sources",
            after.len()
        ),
        json!({"projects": after.iter().map(|p| &p.name).collect::<Vec<_>>()}),
    )
}

// ---------------------------------------------------------------------------------------------
// Level 2: style
// ---------------------------------------------------------------------------------------------

fn caption_size(size: f32, before: &[Project], after: &[Project]) -> Verdict {
    let (b, a) = pair(before, after)?;
    let paths = changed(b, a)?;
    need!(
        paths == ["/captions/size"],
        format!("only /captions/size may change; changed: {}", list(&paths)),
        json!({"changed": paths})
    );
    need!(
        (a.captions.size - size).abs() <= 1e-5,
        format!("caption size is {} instead of {size}", a.captions.size),
        json!({"size": a.captions.size})
    );
    pass(
        format!("caption size {} and nothing else changed", a.captions.size),
        json!({"size": a.captions.size}),
    )
}

fn highlight(color: &str, before: &[Project], after: &[Project]) -> Verdict {
    let (b, a) = pair(before, after)?;
    let want = Color::parse(color).map_err(|e| miss(format!("expected color {color:?}: {e:#}")))?;
    let paths = changed(b, a)?;
    need!(
        paths == ["/captions/highlight"],
        format!(
            "only /captions/highlight may change; changed: {}",
            list(&paths)
        ),
        json!({"changed": paths})
    );
    need!(
        a.captions.highlight == want,
        format!("highlight is {} instead of {color}", a.captions.highlight),
        json!({"highlight": a.captions.highlight.to_string()})
    );
    pass(
        format!("highlight is {color} and nothing else changed"),
        json!({"highlight": a.captions.highlight.to_string()}),
    )
}

fn hook(item: Option<&str>, text: &str, before: &[Project], after: &[Project]) -> Verdict {
    let (b, a) = pair(before, after)?;
    if let Some(id) = item {
        let Some((t, i)) = b.find(id) else {
            return Err(miss(format!("baseline hook item {id:?} not found")));
        };
        let want = format!("/tracks/{t}/items/{i}/text");
        let paths = changed(b, a)?;
        need!(
            paths.len() == 1 && paths[0] == want,
            format!(
                "only the hook text may change ({want}); changed: {}",
                list(&paths)
            ),
            json!({"changed": paths})
        );
        let now = a.item(id).and_then(text_of).map(|t| t.text.clone());
        need!(
            now.as_deref().is_some_and(|s| text_eq(s, text)),
            format!("hook text is {now:?}, expected {text:?}"),
            json!({"text": now})
        );
        return pass(
            "hook text changed and nothing else",
            json!({"item": id, "text": now}),
        );
    }
    let added = additions(b, a)?;
    need!(
        added.items.len() == 1 && added.assets.is_empty(),
        format!(
            "expected exactly one new title item and no new assets, found {} items and {} assets",
            added.items.len(),
            added.assets.len()
        )
    );
    let (track, kind, new) = &added.items[0];
    let Some(t) = text_of(new) else {
        return Err(bad("the added item is not text", Value::Null));
    };
    need!(
        *kind == TrackKind::Text
            && matches!(
                t.preset,
                TextPreset::Title | TextPreset::Card | TextPreset::Hook
            ),
        format!(
            "the title card must be a title, card or hook text item on the text track, got {:?} on {track}",
            t.preset
        )
    );
    need!(
        text_eq(&t.text, text),
        format!("title text is {:?}, expected {text:?}", t.text)
    );
    need!(
        new.start <= 1.0 && new.duration >= 1.0,
        format!(
            "title card must start within 1 s and last 1 s or more (start {:.2}, {:.2} s long)",
            new.start, new.duration
        )
    );
    need!(
        a.track(track).is_some_and(|t| !t.hidden),
        "title card sits on a hidden track"
    );
    pass(
        "title card added at the start and nothing else changed",
        json!({"item": new.id, "start": new.start, "duration": new.duration}),
    )
}

// ---------------------------------------------------------------------------------------------
// Level 3: content
// ---------------------------------------------------------------------------------------------

/// Seconds to add to a source time of each file to get the clock of the transcribed
/// recording. Other files (a second camera) are aligned with the baseline's continuous speech
/// track; a file whose offsets disagree is not the same recording and is left out.
type Frame = BTreeMap<String, f64>;

/// Audio-track clips of one file that are audible, with their items.
fn voices<'a>(p: &'a Project, file: &str) -> Vec<(&'a Item, &'a Clip)> {
    p.tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Audio && !t.muted && !t.hidden)
        .flat_map(|t| &t.items)
        .filter_map(|i| {
            let c = i.clip()?;
            (!c.muted && p.asset(&c.asset).is_some_and(|a| filename(&a.path) == file))
                .then_some((i, c))
        })
        .collect()
}

fn frame_of(b: &Project, file: &str) -> Frame {
    let mut out = Frame::new();
    out.insert(file.to_owned(), 0.0);
    let speech = voices(b, file);
    let mut samples: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    for item in &b.main().items {
        let Some(c) = item.clip() else { continue };
        let Some(f) = b.asset(&c.asset).map(|a| filename(&a.path)) else {
            continue;
        };
        if f == file {
            continue;
        }
        for t in [item.start + 0.01, item.end() - 0.01] {
            if let Some(&(vi, vc)) = speech.iter().find(|v| v.0.contains(t)) {
                let heard = vc.src_in + (t - vi.start) * vc.speed;
                let shown = c.src_in + (t - item.start) * c.speed;
                samples.entry(f.clone()).or_default().push(heard - shown);
            }
        }
    }
    for (f, v) in samples {
        let (lo, hi) = v
            .iter()
            .fold((f64::MAX, f64::MIN), |(l, h), &x| (l.min(x), h.max(x)));
        if hi - lo <= 0.25 {
            out.insert(f, v.iter().sum::<f64>() / v.len() as f64);
        }
    }
    out
}

/// Everything of the recording on the timeline, in the transcript's clock: the picture of every
/// main-track clip of the recording (another camera included) and all audible speech clips.
fn content(p: &Project, frame: &Frame) -> Vec<(f64, f64)> {
    let mut out = vec![];
    for track in p.tracks.iter().filter(|t| !t.hidden) {
        let audio = track.kind == TrackKind::Audio;
        if !(audio || track.kind == TrackKind::Main) || (audio && track.muted) {
            continue;
        }
        for item in &track.items {
            let Some(c) = item.clip() else { continue };
            let Some(a) = p.asset(&c.asset) else { continue };
            if audio && (c.muted || matches!(a.role, AssetRole::Music | AssetRole::Sfx)) {
                continue;
            }
            if let Some(d) = frame.get(&filename(&a.path)) {
                out.push((c.src_in + d, c.src_out(item.duration) + d));
            }
        }
    }
    merge(out)
}

fn content_pair(
    asset: &str,
    before: &[Project],
    b: &Project,
    a: &Project,
) -> Result<(Frame, Vec<(f64, f64)>, Vec<(f64, f64)>), Fail> {
    if asset.is_empty() {
        return Err(miss("no baseline asset matched the phrase"));
    }
    let Some(file) = file_of(before, asset) else {
        return Err(miss(format!(
            "baseline asset {asset:?} is not in any baseline project"
        )));
    };
    let frame = frame_of(b, &file);
    let (kb, ka) = (content(b, &frame), content(a, &frame));
    Ok((frame, kb, ka))
}

fn cut(
    asset: &str,
    start: f64,
    end: f64,
    max_removed: f64,
    before: &[Project],
    after: &[Project],
) -> Verdict {
    let (b, a) = pair(before, after)?;
    let (_, kb, ka) = content_pair(asset, before, b, a)?;
    if !(end > start) || overlap_len(&kb, start, end) <= 0.0 {
        return Err(miss(
            "the phrase is not on the baseline timeline, so removing it cannot be graded",
        ));
    }
    need!(
        !ka.is_empty(),
        "nothing of the recording is left on the timeline"
    );
    let phrase_after = overlap_len(&ka, start, end);
    need!(
        phrase_after <= PHRASE_TOL,
        format!(
            "{phrase_after:.2} s of the phrase ({start:.2} to {end:.2} s) is still on the timeline (picture or speech)"
        ),
        json!({"kept": ka})
    );
    let removed = subtract(&kb, &ka);
    let gone = total(&removed);
    need!(
        gone <= max_removed + LENGTH_TOL,
        format!("{gone:.2} s were removed, at most {max_removed:.2} s allowed (sentence plus 2 s)"),
        json!({"removed": removed})
    );
    let far: Vec<_> = removed
        .iter()
        .filter(|&&(s, e)| s < start - max_removed || e > end + max_removed)
        .collect();
    need!(
        far.is_empty(),
        "footage far from the phrase was removed too",
        json!({"removed": removed})
    );
    pass(
        format!("phrase gone; {gone:.2} s removed of {max_removed:.2} s allowed"),
        json!({"removed": removed, "phrase_still_kept_s": phrase_after}),
    )
}

fn start_at(asset: &str, start: f64, before: &[Project], after: &[Project]) -> Verdict {
    let (b, a) = pair(before, after)?;
    let (frame, kb, ka) = content_pair(asset, before, b, a)?;
    if overlap_len(&kb, f64::MIN, start - 0.75) < 1.0 {
        return Err(miss(
            "the baseline already starts at the phrase, so starting there cannot be graded",
        ));
    }
    need!(
        !ka.is_empty(),
        "nothing of the recording is left on the timeline"
    );
    let opening = a.main().items.first().and_then(|i| {
        let c = i.clip()?;
        let file = filename(&a.asset(&c.asset)?.path);
        frame.get(&file).map(|d| c.src_in + d)
    });
    let Some(opening) = opening else {
        return Err(bad(
            "the video does not open with footage of the recording",
            Value::Null,
        ));
    };
    need!(
        opening >= start - 0.75 && opening <= start + 0.25,
        format!("the video opens at {opening:.2} s of the recording, expected about {start:.2} s"),
        json!({"opening": opening})
    );
    let early = overlap_len(&ka, f64::MIN, start - 0.75);
    need!(
        early <= PHRASE_TOL,
        format!("{early:.2} s before the phrase is still on the timeline (picture or speech)"),
        json!({"kept": ka})
    );
    let rest: Vec<_> = kb
        .iter()
        .map(|&(s, e)| (s.max(start), e))
        .filter(|&(s, e)| e > s)
        .collect();
    let lost = total(&subtract(&rest, &ka));
    need!(
        lost <= 0.5,
        format!("{lost:.2} s of footage after the phrase were dropped"),
        json!({"kept": ka})
    );
    pass(
        format!("video opens at {opening:.2} s of the recording and keeps everything after"),
        json!({"opening": opening, "lost_after_s": lost}),
    )
}

// ---------------------------------------------------------------------------------------------
// Level 4: assets
// ---------------------------------------------------------------------------------------------

fn broll(
    file: &str,
    asset: &str,
    source_time: f64,
    before: &[Project],
    after: &[Project],
) -> Verdict {
    let (b, a) = pair(before, after)?;
    let added = additions(b, a)?;
    need!(
        added.assets.len() == 1 && added.items.len() == 1,
        format!(
            "expected one new asset and one new item, found {} assets and {} items",
            added.assets.len(),
            added.items.len()
        )
    );
    let new_asset = &added.assets[0];
    need!(
        filename(&new_asset.path) == file,
        format!(
            "the new asset is {:?}, expected {file}",
            filename(&new_asset.path)
        )
    );
    let role_ok = new_asset.role == AssetRole::Broll
        || (new_asset.role == AssetRole::Image && new_asset.kind == AssetKind::Image);
    need!(
        role_ok,
        format!("asset role is {:?}, expected broll", new_asset.role),
        json!({"role": format!("{:?}", new_asset.role)})
    );
    let (track, kind, item) = &added.items[0];
    let Some(clip) = item.clip().filter(|c| c.asset == new_asset.id) else {
        return Err(bad(
            "the new item is not a clip of the new asset",
            Value::Null,
        ));
    };
    need!(
        *kind == TrackKind::Overlay && a.track(track).is_some_and(|t| !t.hidden),
        format!("B-roll must sit on a visible overlay track, not {track:?}")
    );
    need!(clip.opacity > 0.1, "the B-roll clip is transparent");
    let Some(file_name) = file_of(before, asset) else {
        return Err(miss(format!(
            "baseline asset {asset:?} is not in any baseline project"
        )));
    };
    let Some(at) = ids_for(a, &file_name)
        .iter()
        .find_map(|id| a.source_to_timeline(id, source_time))
    else {
        return Err(bad(
            "the spoken word is no longer on the main timeline",
            Value::Null,
        ));
    };
    need!(
        (item.start - at).abs() <= 1.0,
        format!(
            "B-roll starts at {:.2} s, the word is at {at:.2} s (limit 1 s)",
            item.start
        ),
        json!({"start": item.start, "word_at": at})
    );
    let frame = 1.0 / a.canvas.fps.max(1.0);
    need!(
        item.duration >= 2.0 - frame && item.duration <= 4.0 + frame,
        format!("B-roll lasts {:.2} s, expected 2 to 4 s", item.duration),
        json!({"duration": item.duration})
    );
    pass(
        format!(
            "B-roll at {:.2} s for {:.2} s, word at {at:.2} s",
            item.start, item.duration
        ),
        json!({"start": item.start, "duration": item.duration, "word_at": at}),
    )
}

fn lower_third(name: &str, role: &str, before: &[Project], after: &[Project]) -> Verdict {
    let (b, a) = pair(before, after)?;
    let added = additions(b, a)?;
    need!(
        added.items.len() == 1 && added.assets.is_empty(),
        format!(
            "expected exactly one new item and no new assets, found {} items and {} assets",
            added.items.len(),
            added.assets.len()
        )
    );
    let (track, _, item) = &added.items[0];
    let Some(t) = text_of(item) else {
        return Err(bad("the added item is not text", Value::Null));
    };
    need!(
        t.preset == TextPreset::LowerThird,
        format!("preset is {:?}, expected lower_third", t.preset)
    );
    let tokens = words(&t.text);
    need!(
        has_run(&tokens, &words(name)).is_some(),
        format!("the name {name:?} is not in {:?}", t.text)
    );
    need!(
        has_run(&tokens, &words(role)).is_some(),
        format!("the role {role:?} is not in {:?}", t.text)
    );
    need!(
        item.start < 3.0 && item.duration >= 1.0,
        format!(
            "lower third must start in the first 3 s and last 1 s or more (start {:.2}, {:.2} s long)",
            item.start, item.duration
        )
    );
    need!(
        a.track(track).is_some_and(|t| !t.hidden),
        "the lower third sits on a hidden track"
    );
    pass(
        "lower third with name and role in the first 3 s",
        json!({"item": item.id, "start": item.start, "text": t.text}),
    )
}

fn music(file: &str, before: &[Project], after: &[Project]) -> Verdict {
    let (b, a) = pair(before, after)?;
    let added = additions(b, a)?;
    need!(
        added.assets.len() == 1,
        format!("expected one new asset, found {}", added.assets.len())
    );
    let new_asset = &added.assets[0];
    need!(
        filename(&new_asset.path) == file,
        format!(
            "the new asset is {:?}, expected {file}",
            filename(&new_asset.path)
        )
    );
    need!(
        new_asset.role == AssetRole::Music && new_asset.kind == AssetKind::Audio,
        format!(
            "asset is {:?} {:?}, expected audio with the music role",
            new_asset.kind, new_asset.role
        )
    );
    need!(
        !added.items.is_empty(),
        "the music asset is not placed on the timeline"
    );
    let mut spans = vec![];
    for (track, kind, item) in &added.items {
        let Some(clip) = item.clip().filter(|c| c.asset == new_asset.id) else {
            return Err(bad(
                "an added item is not a clip of the music asset",
                Value::Null,
            ));
        };
        need!(
            *kind == TrackKind::Audio && a.track(track).is_some_and(|t| !t.muted && !t.hidden),
            format!("music item {} is not on an audible audio track", item.id)
        );
        need!(
            !clip.muted && clip.volume > 0.01,
            format!("music item {} is silent", item.id)
        );
        need!(
            clip.duck || clip.volume <= 0.5,
            format!(
                "music item {} is loud (volume {}, not ducked)",
                item.id, clip.volume
            )
        );
        // What the clip really plays: it cannot run past the end of the audio file.
        let playable = if new_asset.duration > 0.0 {
            (new_asset.duration - clip.src_in).max(0.0) / clip.speed
        } else {
            item.duration
        };
        spans.push((item.start, item.start + item.duration.min(playable)));
    }
    let end = main_end(a);
    need!(end > 0.0, "the main track is empty");
    let covered = overlap_len(&merge(spans), 0.0, end);
    need!(
        covered >= 0.9 * end,
        format!(
            "music covers {:.0}% of the {end:.1} s timeline, 90% needed",
            100.0 * covered / end
        ),
        json!({"covered": covered, "timeline": end})
    );
    pass(
        format!(
            "quiet music covers {:.0}% of the timeline",
            100.0 * covered / end
        ),
        json!({"covered": covered, "timeline": end}),
    )
}

// ---------------------------------------------------------------------------------------------
// Level 5: kind-specific
// ---------------------------------------------------------------------------------------------

/// Main-track source ranges per asset file, merged.
fn spans(p: &Project) -> BTreeMap<String, Vec<(f64, f64)>> {
    let mut raw: BTreeMap<String, Vec<(f64, f64)>> = BTreeMap::new();
    for item in &p.main().items {
        let Some(clip) = item.clip() else { continue };
        let Some(asset) = p.asset(&clip.asset) else {
            continue;
        };
        raw.entry(filename(&asset.path))
            .or_default()
            .push((clip.src_in, clip.src_out(item.duration)));
    }
    raw.into_iter().map(|(k, v)| (k, merge(v))).collect()
}

/// Baseline-transcript words (as single normalized tokens) kept on the main timeline.
fn kept_tokens(p: &Project, before: &[Project], transcripts: &[Transcript]) -> Vec<String> {
    let mut out = vec![];
    for t in transcripts {
        let Some(file) = file_of(before, &t.asset) else {
            continue;
        };
        let k = kept(p, &ids_for(p, &file));
        out.extend(
            t.words
                .iter()
                .filter(|w| {
                    let mid = (w.start + w.end) / 2.0;
                    k.iter().any(|&(s, e)| mid >= s && mid < e)
                })
                .map(|w| words(&w.text).join(""))
                .filter(|w| !w.is_empty()),
        );
    }
    out
}

fn clips(
    topic: &[String],
    before: &[Project],
    after: &[Project],
    transcripts: &[Transcript],
) -> Verdict {
    let topic: Vec<String> = topic.iter().flat_map(|t| words(t)).collect();
    if topic.is_empty() {
        return Err(miss("no topic words to grade the clips against"));
    }
    if transcripts.is_empty() {
        return Err(miss("no baseline transcript to measure the topic against"));
    }
    let fresh: Vec<&Project> = after
        .iter()
        .filter(|p| !before.iter().any(|q| same(p, q)))
        .collect();
    need!(
        fresh.len() == 3,
        format!(
            "expected exactly 3 new or changed clip projects, found {}",
            fresh.len()
        ),
        json!({"projects": fresh.iter().map(|p| &p.name).collect::<Vec<_>>()})
    );
    let (width, height) = canvas_of(VideoKind::Clips);
    let mut report = vec![];
    for p in &fresh {
        need!(
            p.kind == VideoKind::Clips,
            format!("project {:?} has kind {} instead of clips", p.name, p.kind)
        );
        need!(
            (p.canvas.width, p.canvas.height) == (width, height),
            format!(
                "project {:?} canvas is {}x{}",
                p.name, p.canvas.width, p.canvas.height
            )
        );
        let len = main_end(p);
        need!(
            (20.0..=60.0).contains(&len),
            format!(
                "project {:?} is {len:.1} s long, expected 20 to 60 s",
                p.name
            ),
            json!({"project": p.name, "seconds": len})
        );
        let tokens = kept_tokens(p, before, transcripts);
        let hits: Vec<&String> = topic.iter().filter(|w| tokens.contains(w)).collect();
        need!(
            !hits.is_empty(),
            format!(
                "project {:?} never says any of the topic words {topic:?}",
                p.name
            ),
            json!({"project": p.name, "kept_words": tokens.len()})
        );
        report.push(json!({"project": p.name, "seconds": len, "topic_words": hits}));
    }
    let maps: Vec<_> = fresh.iter().map(|p| spans(p)).collect();
    for i in 0..maps.len() {
        for j in i + 1..maps.len() {
            let (mut shared, mut left) = (0.0_f64, 0.0_f64);
            for (file, segs) in &maps[i] {
                left += total(segs);
                if let Some(other) = maps[j].get(file) {
                    shared += segs
                        .iter()
                        .map(|&(s, e)| overlap_len(other, s, e))
                        .sum::<f64>();
                }
            }
            let right: f64 = maps[j].values().map(|v| total(v)).sum();
            let smaller = left.min(right).max(1e-9);
            need!(
                shared / smaller <= 0.5,
                format!(
                    "clips {:?} and {:?} are mostly the same footage",
                    fresh[i].name, fresh[j].name
                ),
                json!({"shared_s": shared})
            );
        }
    }
    pass(
        "3 distinct topical clips of 20 to 60 s",
        json!({"clips": report}),
    )
}

/// Runs a property check on the agent's project and records whether the untouched baseline
/// already satisfied it (`baseline_passes`, `unchanged_from_baseline`), so a report can flag a
/// task that offers no change challenge. The verdict never depends on the project having
/// changed.
fn with_baseline(b: &Project, a: &Project, f: impl Fn(&Project) -> Verdict) -> Verdict {
    let baseline_passes = f(b).is_ok();
    let note = |ev: &mut Value| {
        if ev.is_null() {
            *ev = json!({});
        }
        if let Some(o) = ev.as_object_mut() {
            o.insert("baseline_passes".into(), json!(baseline_passes));
            o.insert("unchanged_from_baseline".into(), json!(same(b, a)));
        }
    };
    match f(a) {
        Ok((detail, mut ev)) => {
            note(&mut ev);
            Ok((detail, ev))
        }
        Err(mut fail) => {
            note(&mut fail.evidence);
            Err(fail)
        }
    }
}

fn podcast(
    cam_a: &str,
    cam_b: &str,
    offset: f64,
    before: &[Project],
    after: &[Project],
) -> Verdict {
    let (b, a) = pair(before, after)?;
    with_baseline(b, a, |p| podcast_ok(cam_a, cam_b, offset, p))
}

/// Both cameras are cut between, with camera B's clips shifted by the known signed offset
/// (`cam_b_time = cam_a_time - offset`).
fn podcast_ok(cam_a: &str, cam_b: &str, offset: f64, p: &Project) -> Verdict {
    let a_ids = ids_for(p, cam_a);
    let b_ids = ids_for(p, cam_b);
    need!(
        !a_ids.is_empty() && !b_ids.is_empty(),
        "both camera files must be assets of the project"
    );
    let cams: Vec<(&Item, &Clip, bool)> = p
        .main()
        .items
        .iter()
        .filter_map(|i| {
            let c = i.clip()?;
            if a_ids.contains(&c.asset) {
                Some((i, c, false))
            } else if b_ids.contains(&c.asset) {
                Some((i, c, true))
            } else {
                None
            }
        })
        .collect();
    need!(
        cams.iter().any(|c| !c.2) && cams.iter().any(|c| c.2),
        "both cameras must appear on the main track"
    );
    let switches = cams.windows(2).filter(|w| w[0].2 != w[1].2).count();
    need!(
        switches >= 2,
        format!("only {switches} camera switches; expected at least 2"),
        json!({"switches": switches})
    );
    // Moment in camera A's clock that a clip plays at source time `src`.
    let content = |is_b: bool, src: f64| if is_b { src + offset } else { src };
    // The continuous speech track, when there is one, is the independent sync reference.
    let voices: Vec<(&Item, &Clip, bool)> = p
        .tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Audio && !t.muted && !t.hidden)
        .flat_map(|t| &t.items)
        .filter_map(|i| {
            let c = i.clip()?;
            if c.muted {
                return None;
            }
            let f = p.asset(&c.asset).map(|a| filename(&a.path))?;
            if f == cam_a {
                Some((i, c, false))
            } else if f == cam_b {
                Some((i, c, true))
            } else {
                None
            }
        })
        .collect();
    let mut verified = [0usize; 2];
    let mut wrong = vec![];
    for &(item, clip, is_b) in &cams {
        // A camera B start clamped at the file start cannot carry the exact offset.
        if is_b && clip.src_in <= 1e-9 {
            continue;
        }
        for t in [item.start + 0.01, item.end() - 0.01] {
            let Some(&(vi, vc, vb)) = voices.iter().find(|v| v.0.contains(t)) else {
                continue;
            };
            let want = content(vb, vc.src_in + (t - vi.start) * vc.speed);
            let got = content(is_b, clip.src_in + (t - item.start) * clip.speed);
            verified[usize::from(is_b)] += 1;
            if (got - want).abs() > SYNC_TOL {
                wrong.push(json!({"item": item.id, "timeline": t, "off_by_s": got - want}));
            }
        }
    }
    need!(
        wrong.is_empty(),
        "camera clips are out of sync with the speech track",
        json!({"offset": offset, "mismatches": wrong})
    );
    if verified[1] > 0 {
        return pass(
            format!(
                "{switches} camera switches, camera B synced to the speech within {SYNC_TOL} s"
            ),
            json!({"mode": "speech_track", "verified": verified, "switches": switches}),
        );
    }
    // No speech track to compare with: switches must continue the same moment.
    let (mut continuous, mut backwards) = (0, vec![]);
    for w in cams.windows(2) {
        if w[0].2 == w[1].2 {
            continue;
        }
        let end = content(w[0].2, w[0].1.src_in + w[0].1.src_len(w[0].0.duration));
        let begin = content(w[1].2, w[1].1.src_in);
        let jump = begin - end;
        if jump.abs() <= SYNC_TOL {
            continuous += 1;
        } else if jump < -SYNC_TOL {
            backwards.push(jump);
        }
    }
    need!(
        backwards.is_empty(),
        "a camera switch jumps back in time; the offset is wrong",
        json!({"offset": offset, "jumps": backwards})
    );
    need!(
        continuous >= 1,
        "no camera switch continues the same moment, so the sync offset is not applied",
        json!({"offset": offset})
    );
    pass(
        format!("{switches} camera switches, {continuous} continuous at the signed offset"),
        json!({"mode": "continuity", "switches": switches, "continuous": continuous}),
    )
}

fn tutorial(silences: &[(f64, f64)], before: &[Project], after: &[Project]) -> Verdict {
    let (b, a) = pair(before, after)?;
    with_baseline(b, a, |p| tutorial_ok(silences, b, p))
}

/// Speed-ups only inside known silences, speech untouched and kept.
fn tutorial_ok(silences: &[(f64, f64)], b: &Project, a: &Project) -> Verdict {
    if silences.is_empty() {
        return Err(miss("no known silent stretches"));
    }
    let silent = merge(silences.to_vec());
    let wide = merge(
        silent
            .iter()
            .map(|&(s, e)| (s - SILENCE_TOL, e + SILENCE_TOL))
            .collect(),
    );
    let kb = kept(b, &main_ids(b));
    let ka = kept(a, &main_ids(a));
    let silent_before: f64 = silent.iter().map(|&(s, e)| overlap_len(&kb, s, e)).sum();
    if silent_before < 0.5 {
        return Err(miss(
            "the baseline keeps (almost) none of the known silence",
        ));
    }
    let (mut sped, mut slow, mut leaks) = (vec![], vec![], vec![]);
    for item in &a.main().items {
        let Some(c) = item.clip() else { continue };
        if (c.speed - 1.0).abs() <= 0.01 {
            continue;
        }
        if c.speed < MIN_SPEEDUP {
            slow.push(json!({"item": item.id, "speed": c.speed}));
            continue;
        }
        let range = (c.src_in, c.src_out(item.duration));
        let outside = (range.1 - range.0) - overlap_len(&wide, range.0, range.1);
        if outside > LENGTH_TOL {
            leaks.push(json!({"item": item.id, "speed": c.speed, "source": range, "outside_silence_s": outside}));
        }
        sped.push(range);
    }
    need!(
        slow.is_empty(),
        format!("clips must run at normal speed or at least {MIN_SPEEDUP}x"),
        json!({"clips": slow})
    );
    need!(
        leaks.is_empty(),
        "speech was sped up: sped clips reach outside the known silences",
        json!({"clips": leaks})
    );
    let sped = merge(sped);
    let sped_silence: f64 = silent.iter().map(|&(s, e)| overlap_len(&sped, s, e)).sum();
    need!(
        sped_silence >= 0.5 * silent_before,
        format!("only {sped_silence:.1} s of the {silent_before:.1} s of silence were sped up"),
        json!({"sped_silence": sped_silence, "silence": silent_before})
    );
    let lost = total(&subtract(&subtract(&kb, &wide), &ka));
    need!(
        lost <= 0.1,
        format!("{lost:.2} s of talking were deleted"),
        json!({"lost_speech_s": lost})
    );
    pass(
        format!("{sped_silence:.1} s of {silent_before:.1} s silence sped up; speech untouched"),
        json!({"sped_silence": sped_silence, "silence": silent_before}),
    )
}

fn vlog(beats: &[f64], before: &[Project], after: &[Project]) -> Verdict {
    let (b, a) = pair(before, after)?;
    with_baseline(b, a, |p| vlog_ok(beats, p))
}

fn vlog_ok(beats: &[f64], p: &Project) -> Verdict {
    if beats.is_empty() {
        return Err(miss("no independently analyzed beats"));
    }
    let len = main_end(p);
    need!(
        (50.0..=70.0).contains(&len),
        format!("the vlog is {len:.1} s long, expected 50 to 70 s"),
        json!({"seconds": len})
    );
    let times = beat_times(p, beats);
    need!(
        !times.is_empty(),
        "no music clip plays any of the beats, so cuts cannot be placed on them"
    );
    let cuts: Vec<f64> = p
        .main()
        .items
        .iter()
        .skip(1)
        .map(|i| i.start + i.clip().map_or(0.0, Clip::overlap) / 2.0)
        .collect();
    need!(!cuts.is_empty(), "the vlog has no cuts");
    let off: Vec<f64> = cuts
        .iter()
        .copied()
        .filter(|&c| !near(&times, c, BEAT_TOL))
        .collect();
    need!(
        off.is_empty(),
        format!(
            "{} of {} cuts are not on a beat (tolerance {BEAT_TOL} s)",
            off.len(),
            cuts.len()
        ),
        json!({"cuts": cuts, "off_beat": off, "tolerance_s": BEAT_TOL})
    );
    pass(
        format!(
            "{len:.1} s, all {} cuts on beats (tolerance {BEAT_TOL} s)",
            cuts.len()
        ),
        json!({"seconds": len, "cuts": cuts.len(), "tolerance_s": BEAT_TOL}),
    )
}

fn explainer(sentences: usize, before: &[Project], after: &[Project]) -> Verdict {
    let (b, a) = pair(before, after)?;
    with_baseline(b, a, |p| explainer_ok(sentences, p))
}

/// Pictures on screen: still images (not logos) and B-roll video on visible main or overlay
/// tracks, opaque enough to be seen.
fn pictures(p: &Project) -> Vec<&Item> {
    let picture = |a: &Asset| {
        (a.kind == AssetKind::Image && a.role != AssetRole::Logo)
            || (a.kind == AssetKind::Video && a.role == AssetRole::Broll)
    };
    p.tracks
        .iter()
        .filter(|t| !t.hidden && matches!(t.kind, TrackKind::Main | TrackKind::Overlay))
        .flat_map(|t| t.items.iter())
        .filter(|i| {
            i.clip()
                .is_some_and(|c| c.opacity > 0.1 && p.asset(&c.asset).is_some_and(picture))
        })
        .collect()
}

fn explainer_ok(sentences: usize, p: &Project) -> Verdict {
    if sentences == 0 {
        return Err(miss("no sentence ground truth"));
    }
    let shown = pictures(p);
    let n = shown.len();
    need!(
        (n as f64 - sentences as f64).abs() <= 0.2 * sentences as f64 + 1e-9,
        format!("{n} pictures for {sentences} sentences; within 20% needed"),
        json!({"pictures": n, "sentences": sentences})
    );
    let mut starts: Vec<i64> = shown
        .iter()
        .map(|i| (i.start * 10.0).round() as i64)
        .collect();
    starts.sort();
    starts.dedup();
    need!(
        starts.len() * 5 >= n * 4,
        "the pictures are stacked at the same moments instead of following the narration",
        json!({"distinct_starts": starts.len(), "pictures": n})
    );
    pass(
        format!("{n} pictures for {sentences} sentences"),
        json!({"pictures": n, "sentences": sentences}),
    )
}

fn promo(item: &str, text: &str, beats: &[f64], before: &[Project], after: &[Project]) -> Verdict {
    let (b, a) = pair(before, after)?;
    if item.is_empty() || beats.is_empty() {
        return Err(miss(
            "missing baseline text item or independently analyzed beats",
        ));
    }
    let Some((t, i)) = b.find(item) else {
        return Err(miss(format!("baseline text item {item:?} not found")));
    };
    let prefix = format!("/tracks/{t}/items/{i}/");
    let paths = changed(b, a)?;
    let stray: Vec<&String> = paths
        .iter()
        .filter(|p| {
            !p.strip_prefix(&prefix)
                .is_some_and(|f| matches!(f, "text" | "start" | "duration"))
        })
        .collect();
    need!(
        stray.is_empty() && paths.iter().any(|p| p.ends_with("/text")),
        format!(
            "only the text, start and duration of {item} may change, and the text must; changed: {}",
            list(&paths)
        ),
        json!({"changed": paths})
    );
    let Some(new) = a.item(item) else {
        return Err(bad("the promo line vanished", Value::Null));
    };
    let now = text_of(new).map(|t| t.text.clone());
    need!(
        now.as_deref().is_some_and(|s| text_eq(s, text)),
        format!("promo line is {now:?}, expected {text:?}"),
        json!({"text": now})
    );
    let times = beat_times(a, beats);
    need!(
        !times.is_empty(),
        "no music clip plays any of the beats, so the line cannot be placed on one"
    );
    need!(
        near(&times, new.start, BEAT_TOL),
        format!(
            "the line starts at {:.2} s, which is not on a beat (within {BEAT_TOL} s)",
            new.start
        ),
        json!({"start": new.start})
    );
    pass(
        format!("line changed and starts on a beat at {:.2} s", new.start),
        json!({"start": new.start, "text": now}),
    )
}

struct Tok {
    text: String,
    start: f64,
    item: String,
}

/// Every spoken or sung word of caption and text items, in time order.
fn stream(p: &Project) -> Vec<Tok> {
    let mut items: Vec<&Item> = p
        .tracks
        .iter()
        .filter(|t| !t.hidden && matches!(t.kind, TrackKind::Captions | TrackKind::Text))
        .flat_map(|t| t.items.iter())
        .collect();
    items.sort_by(|a, b| a.start.total_cmp(&b.start));
    let mut out = vec![];
    for item in items {
        match &item.body {
            ItemBody::Caption(c) => {
                for w in &c.words {
                    let text = words(&w.text).join("");
                    if !text.is_empty() {
                        out.push(Tok {
                            text,
                            start: item.start + w.start,
                            item: item.id.clone(),
                        });
                    }
                }
            }
            ItemBody::Text(t) => {
                for text in words(&t.text) {
                    out.push(Tok {
                        text,
                        start: item.start,
                        item: item.id.clone(),
                    });
                }
            }
            ItemBody::Clip(_) => {}
        }
    }
    out
}

/// Start indexes in `stream` where `needle` occurs.
fn runs(stream: &[Tok], needle: &[String]) -> Vec<usize> {
    if needle.is_empty() {
        return vec![];
    }
    (0..stream.len().saturating_sub(needle.len() - 1))
        .filter(|&i| {
            stream[i..i + needle.len()]
                .iter()
                .map(|t| &t.text)
                .eq(needle.iter())
        })
        .collect()
}

fn without(p: &Project, ids: &BTreeSet<String>) -> Project {
    let mut q = p.clone();
    for t in &mut q.tracks {
        t.items.retain(|i| !ids.contains(&i.id));
    }
    q
}

fn lyric(at: f64, text: &str, before: &[Project], after: &[Project]) -> Verdict {
    let (b, a) = pair(before, after)?;
    let expected = words(text);
    if expected.is_empty() {
        return Err(miss("no replacement lyric text"));
    }
    let original: Vec<String> = match expected.split_last() {
        Some((last, rest)) if last == "chorus" => rest.to_vec(),
        _ => expected.clone(),
    };
    let old = stream(b);
    let Some(&bi) = runs(&old, &original).iter().min_by(|&&x, &&y| {
        (old[x].start - at)
            .abs()
            .total_cmp(&(old[y].start - at).abs())
    }) else {
        return Err(miss("the original lyric is not in the baseline captions"));
    };
    if (old[bi].start - at).abs() > 3.0 {
        return Err(miss("the baseline lyric is not near the requested time"));
    }
    let first = old[bi].start;
    let old_items: BTreeSet<String> = old[bi..bi + original.len()]
        .iter()
        .map(|t| t.item.clone())
        .collect();
    let now = stream(a);
    let Some(&ai) = runs(&now, &expected).iter().min_by(|&&x, &&y| {
        (now[x].start - first)
            .abs()
            .total_cmp(&(now[y].start - first).abs())
    }) else {
        return Err(bad(
            format!("the lyric {text:?} is not in the project's captions"),
            Value::Null,
        ));
    };
    let new_items: BTreeSet<String> = now[ai..ai + expected.len()]
        .iter()
        .map(|t| t.item.clone())
        .collect();
    need!(
        (now[ai].start - first).abs() <= 0.3,
        format!(
            "the changed lyric moved: it starts at {:.2} s, it was at {first:.2} s",
            now[ai].start
        ),
        json!({"start": now[ai].start, "was": first})
    );
    let span = |p: &Project, ids: &BTreeSet<String>| {
        ids.iter()
            .filter_map(|id| p.item(id))
            .fold((f64::MAX, f64::MIN), |(s, e), i| {
                (s.min(i.start), e.max(i.end()))
            })
    };
    let (bs, be) = span(b, &old_items);
    let (ns, ne) = span(a, &new_items);
    need!(
        (bs - ns).abs() <= 0.1 && (be - ne).abs() <= 0.1,
        format!("the lyric's timing moved from {bs:.2}-{be:.2} s to {ns:.2}-{ne:.2} s"),
        json!({"was": [bs, be], "now": [ns, ne]})
    );
    let rest = changed(&without(b, &old_items), &without(a, &new_items))?;
    need!(
        rest.is_empty(),
        format!("other parts of the project changed: {}", list(&rest)),
        json!({"changed": rest})
    );
    pass(
        format!("lyric changed at {first:.2} s with its timing kept"),
        json!({"at": first, "items": new_items}),
    )
}

// ---------------------------------------------------------------------------------------------
// Level 6: visual
// ---------------------------------------------------------------------------------------------

fn hit(a: Rect, b: Rect) -> bool {
    a.x < b.x + b.w && a.x + a.w > b.x && a.y < b.y + b.h && a.y + a.h > b.y
}

/// Where the frozen mouth zones of the retained close-ups land on the canvas, with their
/// timeline seconds. Zones come from the frozen samples and the project's own crop, focus and
/// zooms, never from `Project::faces`.
fn mouth_zones(
    p: &Project,
    ids: &[String],
    samples: &[FaceSample],
) -> Result<Vec<(f64, Rect)>, Fail> {
    let mut zones = vec![];
    for s in samples
        .iter()
        .filter(|s| s.t.is_finite() && s.w > 0.0 && s.h > 0.0 && s.x.is_finite() && s.y.is_finite())
    {
        let found = p.main().items.iter().find_map(|item| {
            let clip = item.clip()?;
            (ids.contains(&clip.asset) && s.t >= clip.src_in && s.t < clip.src_out(item.duration))
                .then_some((item, clip))
        });
        let Some((item, clip)) = found else { continue };
        let Some(asset) = p
            .asset(&clip.asset)
            .filter(|a| a.kind == AssetKind::Video && a.width > 0 && a.height > 0)
        else {
            return Err(miss(
                "the source asset has no picture size, so the mouth cannot be mapped to the canvas",
            ));
        };
        let local = (s.t - clip.src_in) / clip.speed;
        let face = Rect {
            x: s.x,
            y: s.y + s.h * MOUTH_TOP,
            w: s.w,
            h: s.h * (1.0 - MOUTH_TOP + CHIN_MARGIN),
        };
        let zone = canvas_rect(
            clip,
            (asset.width, asset.height),
            (p.canvas.width, p.canvas.height),
            local,
            item.duration,
            face,
        );
        zones.push((item.start + local, zone));
    }
    Ok(zones)
}

fn mouth(asset: &str, samples: &[FaceSample], before: &[Project], after: &[Project]) -> Verdict {
    let (b, a) = pair(before, after)?;
    if asset.is_empty() || samples.is_empty() {
        return Err(miss(
            "no usable independent close-up face detections; mouth safety cannot be certified",
        ));
    }
    let Some(file) = file_of(before, asset) else {
        return Err(miss(format!(
            "baseline asset {asset:?} is not in any baseline project"
        )));
    };
    let ids = ids_for(a, &file);
    need!(
        !ids.is_empty(),
        "the source video is no longer an asset of the project"
    );
    let baseline_pages = b
        .tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Captions)
        .flat_map(|t| &t.items)
        .count();
    // The placements are those of the agent's project as it stands, face data and all.
    let placements = if a.captions.mode == CaptionMode::Off {
        vec![]
    } else {
        Graphics::new().caption_placements(a)
    };
    judge_mouth(a, &ids, samples, &placements, baseline_pages)
}

/// Frozen face truth against the caption boxes actually laid out.
fn judge_mouth(
    p: &Project,
    ids: &[String],
    samples: &[FaceSample],
    placements: &[CaptionPlacement],
    baseline_pages: usize,
) -> Verdict {
    if baseline_pages == 0 {
        return Err(miss("the baseline has no caption pages to protect"));
    }
    need!(
        p.captions.mode != CaptionMode::Off,
        "captions are turned off"
    );
    need!(
        !placements.is_empty(),
        "no caption page is drawn (captions missing, empty or on a hidden track)"
    );
    let hidden: Vec<&str> = placements
        .iter()
        .filter(|x| x.face == FaceFit::Hidden)
        .map(|x| x.item.as_str())
        .collect();
    need!(
        hidden.is_empty(),
        format!(
            "{} caption pages are hidden instead of placed",
            hidden.len()
        ),
        json!({"hidden": hidden})
    );
    need!(
        placements.len() * 5 >= baseline_pages * 4,
        format!(
            "only {} of {baseline_pages} caption pages remain",
            placements.len()
        )
    );
    need!(
        placements.iter().all(|x| x.size >= MIN_CAPTION_SIZE),
        "captions were shrunk below a readable size"
    );
    let zones = mouth_zones(p, ids, samples)?;
    need!(
        !zones.is_empty(),
        "none of the frozen close-ups is still on the main timeline"
    );
    let (mut verified, mut covered) = (0usize, vec![]);
    for x in placements {
        for &(t, zone) in zones.iter().filter(|(t, _)| *t >= x.start && *t <= x.end) {
            verified += 1;
            if hit(x.bounds, zone) {
                covered.push(json!({"page": x.item, "timeline": t, "face_fit": x.face}));
            }
        }
    }
    need!(
        covered.is_empty(),
        format!(
            "{} caption boxes cover the mouth at close-up moments",
            covered.len()
        ),
        json!({"covered": covered})
    );
    need!(
        verified > 0,
        "no caption page is on screen at any retained close-up, so mouth safety is not certified"
    );
    pass(
        format!("{verified} caption/close-up pairs checked, none covers the mouth"),
        json!({"verified": verified, "pages": placements.len()}),
    )
}

// ---------------------------------------------------------------------------------------------
// Task setup: a start state that fails the check
// ---------------------------------------------------------------------------------------------

/// How far camera B's clips are moved to put it out of sync, in seconds (8 times `SYNC_TOL`).
const DESYNC: f64 = 0.8;
/// Where `prepare` cuts a vlog that already has the right length, in seconds.
const VLOG_SHORT: f64 = 45.0;

/// Puts copies of the frozen baseline projects into the state the agent starts from, so that a
/// run which changes nothing cannot pass. Changes go through the Editor API ops, never around
/// them. On `Ok` the untouched projects were checked with `check(task, projects, projects, ..)`
/// and failed it for a task reason (for the mouth task: by really covering the frozen mouth).
/// On `Err` the projects are left as they were and the run must not start: the fixture cannot
/// give this task a valid failing start.
///
/// - Podcast: camera B's clips are moved 0.8 s out of sync.
/// - Tutorial: sped-up clips are put back to normal speed over the same source.
/// - Vlog: a vlog of 50 to 70 s loses its tail so that it is 45 s long.
/// - Explainer: a picture count within 20% of the sentences gets extra pictures on an overlay.
/// - Mouth: the face data is removed and the caption position and size are chosen so a caption
///   box overlaps a frozen close-up mouth zone, proven with the real caption layout.
/// - Every other task already starts from a state that fails (the requested change is missing).
pub fn prepare(
    task: &Task,
    projects: &mut [Project],
    transcripts: &[Transcript],
) -> Result<(), String> {
    if let Some(reason) = &task.prerequisite {
        return Err(reason.clone());
    }
    let mut next = projects.to_vec();
    setup(&task.check, &mut next)
        .map_err(|e| format!("cannot build a failing start state: {e}"))?;
    let start = check(task, &next, &next, transcripts, true);
    if start.pass {
        return Err(format!(
            "the untouched start state already passes the check ({}); no challenge could be built",
            start.detail
        ));
    }
    if start.failure_class.is_some() {
        return Err(format!(
            "the check cannot grade this fixture: {}",
            start.detail
        ));
    }
    if matches!(task.check, Check::Mouth { .. }) && !start.detail.contains("cover the mouth") {
        return Err(format!(
            "the start state fails for another reason than covering the mouth: {}",
            start.detail
        ));
    }
    projects.clone_from_slice(&next);
    Ok(())
}

/// Whether the projects as given, untouched, already satisfy the task. For auditing legacy runs
/// that started from an unprepared baseline: it is `check(task, before, before, ..).pass`, with
/// no change required (a `check`-class result, for example a missing prerequisite, is `false`).
pub fn baseline_satisfied(task: &Task, before: &[Project], transcripts: &[Transcript]) -> bool {
    check(task, before, before, transcripts, true).pass
}

fn first(ps: &mut [Project]) -> Result<&mut Project, String> {
    ps.first_mut()
        .ok_or_else(|| "there is no baseline project".to_owned())
}

fn setup(check: &Check, ps: &mut [Project]) -> Result<(), String> {
    match check {
        Check::Podcast { cam_b, .. } => desync(first(ps)?, cam_b),
        Check::Tutorial { .. } => normal_speed(first(ps)?),
        Check::Vlog { .. } => shorten(first(ps)?),
        Check::Explainer { sentences } => crowd(first(ps)?, *sentences),
        Check::Mouth { asset, samples } => {
            let Some(file) = file_of(ps, asset) else {
                return Err(format!("baseline asset {asset:?} is not in any project"));
            };
            cover_mouth(first(ps)?, &file, samples)
        }
        _ => Ok(()),
    }
}

fn run_ops(p: &mut Project, ops: &[Op]) -> Result<(), String> {
    apply_all(p, ops).map(|_| ()).map_err(|e| format!("{e:#}"))
}

fn desync(p: &mut Project, cam_b: &str) -> Result<(), String> {
    let ids = ids_for(p, cam_b);
    let mut ops = vec![];
    for item in &p.main().items {
        let Some(c) = item.clip().filter(|c| ids.contains(&c.asset)) else {
            continue;
        };
        let limit = p.asset(&c.asset).map_or(0.0, |a| a.duration);
        let src_in = if limit <= 0.0 || c.src_out(item.duration) + DESYNC <= limit {
            c.src_in + DESYNC
        } else if c.src_in >= DESYNC {
            c.src_in - DESYNC
        } else {
            continue;
        };
        ops.push(Op::Patch {
            item: item.id.clone(),
            patch: json!({"src_in": src_in}),
        });
    }
    if ops.is_empty() {
        return Err(format!(
            "camera B ({cam_b}) has no movable clip on the main track"
        ));
    }
    run_ops(p, &ops)
}

fn normal_speed(p: &mut Project) -> Result<(), String> {
    let ops: Vec<Op> = p
        .main()
        .items
        .iter()
        .filter_map(|i| {
            let c = i.clip()?;
            ((c.speed - 1.0).abs() > 0.01).then(|| Op::Patch {
                item: i.id.clone(),
                patch: json!({"speed": 1.0, "duration": i.duration * c.speed}),
            })
        })
        .collect();
    if ops.is_empty() {
        return Ok(());
    }
    run_ops(p, &ops)
}

fn shorten(p: &mut Project) -> Result<(), String> {
    let len = main_end(p);
    if !(50.0..=70.0).contains(&len) {
        return Ok(());
    }
    run_ops(
        p,
        &[Op::CutRange {
            from: VLOG_SHORT,
            to: len,
        }],
    )
}

fn crowd(p: &mut Project, sentences: usize) -> Result<(), String> {
    if sentences == 0 {
        return Err("no sentence ground truth".into());
    }
    let n = pictures(p).len();
    if (n as f64 - sentences as f64).abs() > 0.2 * sentences as f64 + 1e-9 {
        return Ok(());
    }
    let extra = (((1.2 * sentences as f64).floor() as usize) + 2)
        .saturating_sub(n)
        .max(1);
    let Some(asset) = p
        .assets
        .iter()
        .find(|a| a.kind == AssetKind::Image && a.role != AssetRole::Logo)
        .map(|a| a.id.clone())
    else {
        return Err("the project has no picture asset to add".into());
    };
    let existing = p
        .tracks
        .iter()
        .find(|t| t.kind == TrackKind::Overlay && !t.hidden)
        .map(|t| t.id.clone());
    let track = match existing {
        Some(id) => id,
        None => {
            let done = apply(
                p,
                &Op::AddTrack {
                    kind: TrackKind::Overlay,
                    name: None,
                },
            )
            .map_err(|e| format!("{e:#}"))?;
            done.items
                .first()
                .cloned()
                .ok_or("no overlay track was added")?
        }
    };
    let ops: Vec<Op> = (0..extra)
        .map(|i| Op::AddClip {
            asset: asset.clone(),
            track: Some(track.clone()),
            start: Some(i as f64 * 0.6),
            index: None,
            src_in: 0.0,
            duration: Some(0.5),
            frame: None,
            volume: None,
            duck: None,
        })
        .collect();
    run_ops(p, &ops)
}

fn cover_mouth(p: &mut Project, file: &str, samples: &[FaceSample]) -> Result<(), String> {
    let ids = ids_for(p, file);
    let zones = mouth_zones(p, &ids, samples).map_err(|f| f.detail)?;
    if zones.is_empty() {
        return Err("none of the frozen close-ups is on the main timeline".into());
    }
    // Without face data the engine cannot dodge, so the caption stays where it is told to be.
    let mut base = p.clone();
    if !base.faces.is_empty() {
        run_ops(
            &mut base,
            &[Op::Set {
                path: "/faces".into(),
                value: json!({}),
            }],
        )?;
    }
    let mut graphics = Graphics::new();
    let step = (zones.len() / 24).max(1);
    for &(t, zone) in zones.iter().step_by(step) {
        let centre = f64::from(zone.y + zone.h / 2.0);
        for size in [base.captions.size, 0.06, 0.08] {
            let mut trial = base.clone();
            run_ops(
                &mut trial,
                &[
                    Op::Set {
                        path: "/captions/position".into(),
                        value: json!(centre),
                    },
                    Op::Set {
                        path: "/captions/size".into(),
                        value: json!(size),
                    },
                ],
            )?;
            let covered = graphics
                .caption_placements(&trial)
                .iter()
                .any(|x| t >= x.start && t <= x.end && hit(x.bounds, zone));
            if covered {
                *p = trial;
                return Ok(());
            }
        }
    }
    Err("no caption position and size put a caption box over a retained close-up mouth".into())
}

// ---------------------------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use veecut_engine::graphics::Slot;
    use veecut_engine::project::{Anim, Canvas, Caption, TextStyle, Track, Word};
    use veecut_engine::transcript::TranscriptWord;

    use super::*;

    fn proj(kind: VideoKind) -> Project {
        let (w, h) = canvas_of(kind);
        Project::new("p", kind, Canvas::new(w, h))
    }

    fn asset(
        p: &mut Project,
        id: &str,
        file: &str,
        kind: AssetKind,
        role: AssetRole,
        duration: f64,
    ) {
        p.assets.push(Asset {
            id: id.into(),
            path: PathBuf::from(format!("inbox/{file}")),
            kind,
            role,
            duration,
            width: 1920,
            height: 1080,
            fps: 30.0,
            has_audio: kind != AssetKind::Image,
        });
    }

    fn video(p: &mut Project, id: &str, file: &str) {
        asset(p, id, file, AssetKind::Video, AssetRole::Source, 1000.0);
    }

    fn clip_item(id: &str, start: f64, duration: f64, asset: &str, src_in: f64) -> Item {
        Item {
            id: id.into(),
            start,
            duration,
            body: ItemBody::Clip(Clip::new(asset, src_in)),
        }
    }

    fn text_item(id: &str, start: f64, duration: f64, text: &str, preset: TextPreset) -> Item {
        Item {
            id: id.into(),
            start,
            duration,
            body: ItemBody::Text(Text {
                text: text.into(),
                preset,
                style: TextStyle::default(),
                position: None,
                anim_in: Anim::Fade,
                anim_out: Anim::Fade,
            }),
        }
    }

    fn caps(id: &str, start: f64, duration: f64, ws: &[&str]) -> Item {
        let step = duration / ws.len() as f64;
        Item {
            id: id.into(),
            start,
            duration,
            body: ItemBody::Caption(Caption {
                words: ws
                    .iter()
                    .enumerate()
                    .map(|(k, w)| Word {
                        text: (*w).into(),
                        start: k as f64 * step,
                        end: (k as f64 + 0.9) * step,
                        emphasis: false,
                    })
                    .collect(),
            }),
        }
    }

    fn put(p: &mut Project, track: &str, item: Item) {
        let i = p.track_index(track).unwrap();
        p.tracks[i].items.push(item);
        p.normalize();
    }

    fn main_clip(p: &mut Project, id: &str, asset: &str, src_in: f64, duration: f64) {
        put(p, "main", clip_item(id, 0.0, duration, asset, src_in));
    }

    fn speed(p: &mut Project, id: &str, s: f64) {
        p.item_mut(id).and_then(Item::clip_mut).unwrap().speed = s;
    }

    fn task(check: Check) -> Task {
        Task {
            id: "t".into(),
            level: 1,
            prompt: String::new(),
            check,
            provenance: "synthetic".into(),
            prerequisite: None,
            inputs: vec![],
        }
    }

    fn go(check: Check, before: &[Project], after: &[Project]) -> CheckResult {
        super::check(&task(check), before, after, &[], true)
    }

    fn ok(r: &CheckResult) {
        assert!(r.pass, "expected pass, got: {} {}", r.detail, r.evidence);
    }

    fn fails(r: &CheckResult, why: &str) {
        assert!(
            !r.pass,
            "expected failure containing {why:?}, but it passed: {}",
            r.detail
        );
        assert!(
            r.detail.contains(why),
            "detail {:?} lacks {why:?}",
            r.detail
        );
    }

    // ---- level 1 ----

    fn made(kind: VideoKind, files: &[&str]) -> Project {
        let mut p = proj(kind);
        for (i, f) in files.iter().enumerate() {
            video(&mut p, &format!("a{i}"), f);
            main_clip(&mut p, &format!("m{}", i + 1), &format!("a{i}"), 0.0, 10.0);
        }
        p
    }

    fn run_check(kind: VideoKind) -> Check {
        let (width, height) = canvas_of(kind);
        Check::Run {
            kind,
            sources: vec!["a.mp4".into(), "b.mp4".into()],
            width,
            height,
        }
    }

    #[test]
    fn run_passes_and_rejects_wrong_kind_canvas_and_sources() {
        let good = made(VideoKind::Vlog, &["a.mp4", "b.mp4"]);
        ok(&go(run_check(VideoKind::Vlog), &[], &[good.clone()]));

        let wrong_kind = made(VideoKind::Promo, &["a.mp4", "b.mp4"]);
        fails(
            &go(run_check(VideoKind::Vlog), &[], &[wrong_kind]),
            "kind is",
        );

        let mut wrong_canvas = good.clone();
        wrong_canvas.canvas = Canvas::new(1920, 1080);
        fails(
            &go(run_check(VideoKind::Vlog), &[], &[wrong_canvas]),
            "canvas is",
        );

        let missing = made(VideoKind::Vlog, &["a.mp4"]);
        fails(
            &go(run_check(VideoKind::Vlog), &[], &[missing]),
            "sources missing",
        );

        let extra = made(VideoKind::Vlog, &["a.mp4", "b.mp4", "other.mp4"]);
        fails(
            &go(run_check(VideoKind::Vlog), &[], &[extra]),
            "unexpected sources",
        );

        let mut empty = good.clone();
        empty.tracks[0].items.clear();
        fails(&go(run_check(VideoKind::Vlog), &[], &[empty]), "not used");

        fails(&go(run_check(VideoKind::Vlog), &[], &[]), "no project");
    }

    #[test]
    fn run_rejects_asset_only_decoys_and_stray_projects() {
        let good = made(VideoKind::Vlog, &["a.mp4", "b.mp4"]);
        // b.mp4 is listed as an asset but never placed.
        let mut decoy = good.clone();
        decoy.tracks[0].items.retain(|i| i.id != "m2");
        decoy.normalize();
        fails(
            &go(run_check(VideoKind::Vlog), &[], &[decoy.clone()]),
            "not used",
        );
        // Placed only on a hidden track.
        let mut hidden = decoy.clone();
        put(&mut hidden, "broll", clip_item("o1", 0.0, 3.0, "a1", 0.0));
        hidden.tracks[1].hidden = true;
        fails(&go(run_check(VideoKind::Vlog), &[], &[hidden]), "not used");
        // A correct project does not excuse a wrong stray one, or the reverse.
        let wrong_kind = made(VideoKind::Promo, &["a.mp4", "b.mp4"]);
        fails(
            &go(
                run_check(VideoKind::Vlog),
                &[],
                &[good.clone(), wrong_kind.clone()],
            ),
            "kind is",
        );
        fails(
            &go(run_check(VideoKind::Vlog), &[], &[wrong_kind, good.clone()]),
            "kind is",
        );
        fails(
            &go(run_check(VideoKind::Vlog), &[], &[good.clone(), decoy]),
            "not used",
        );
        // Two right projects are fine (for example the clips kind).
        let mut second = good.clone();
        second.name = "second".into();
        ok(&go(run_check(VideoKind::Vlog), &[], &[good, second]));
    }

    #[test]
    fn run_requires_audio_sources_on_an_audio_track() {
        let want = Check::Run {
            kind: VideoKind::Vlog,
            sources: vec!["a.mp4".into(), "song.mp3".into()],
            width: 1080,
            height: 1920,
        };
        let mut good = made(VideoKind::Vlog, &["a.mp4"]);
        asset(
            &mut good,
            "m",
            "song.mp3",
            AssetKind::Audio,
            AssetRole::Music,
            100.0,
        );
        let mut on_main = good.clone();
        put(&mut on_main, "main", clip_item("bad", 0.0, 5.0, "m", 0.0));
        fails(&go(want.clone(), &[], &[on_main]), "not used");
        fails(&go(want.clone(), &[], &[good.clone()]), "not used");
        put(&mut good, "music", clip_item("song", 0.0, 10.0, "m", 0.0));
        ok(&go(want, &[], &[good]));
    }

    #[test]
    fn run_rejects_changed_inbox_and_sfx_assets_are_allowed() {
        let mut good = made(VideoKind::Vlog, &["a.mp4", "b.mp4"]);
        asset(
            &mut good,
            "s1",
            "pop.wav",
            AssetKind::Audio,
            AssetRole::Sfx,
            1.0,
        );
        ok(&go(run_check(VideoKind::Vlog), &[], &[good.clone()]));
        let r = super::check(&task(run_check(VideoKind::Vlog)), &[], &[good], &[], false);
        fails(&r, "inbox");
    }

    // ---- level 2 ----

    fn styled() -> Project {
        let mut p = made(VideoKind::TalkingHeadShort, &["a.mp4"]);
        put(
            &mut p,
            "text",
            text_item("hook-1", 0.0, 3.0, "Old hook", TextPreset::Hook),
        );
        p
    }

    #[test]
    fn caption_size_exact_field_only() {
        let before = styled();
        let mut good = before.clone();
        good.captions.size = 0.06;
        ok(&go(
            Check::CaptionSize { size: 0.06 },
            &[before.clone()],
            &[good.clone()],
        ));

        fails(
            &go(
                Check::CaptionSize { size: 0.07 },
                &[before.clone()],
                &[good.clone()],
            ),
            "instead of",
        );

        let mut extra = good.clone();
        extra.captions.size = 0.06;
        extra.captions.uppercase = false;
        fails(
            &go(
                Check::CaptionSize { size: 0.06 },
                &[before.clone()],
                &[extra],
            ),
            "/captions/uppercase",
        );

        let mut renamed = good.clone();
        renamed.name = "renamed".into();
        fails(
            &go(
                Check::CaptionSize { size: 0.06 },
                &[before.clone()],
                &[renamed],
            ),
            "/name",
        );

        fails(
            &go(
                Check::CaptionSize { size: 0.06 },
                &[before.clone()],
                &[before],
            ),
            "only /captions/size",
        );
    }

    #[test]
    fn other_projects_must_stay_identical() {
        let before = styled();
        let mut other = styled();
        other.name = "second".into();
        let mut good = before.clone();
        good.captions.size = 0.06;
        let mut touched = other.clone();
        touched.captions.size = 0.9;
        ok(&go(
            Check::CaptionSize { size: 0.06 },
            &[before.clone(), other.clone()],
            &[good.clone(), other.clone()],
        ));
        fails(
            &go(
                Check::CaptionSize { size: 0.06 },
                &[before.clone(), other.clone()],
                &[good.clone(), touched],
            ),
            "did not ask",
        );
        fails(
            &go(
                Check::CaptionSize { size: 0.06 },
                &[before.clone(), other],
                &[good],
            ),
            "project count",
        );
    }

    #[test]
    fn highlight_needs_the_exact_color_and_nothing_else() {
        let before = styled();
        let mut good = before.clone();
        good.captions.highlight = Color::parse("#00ff00").unwrap();
        ok(&go(
            Check::Highlight {
                color: "#00FF00".into(),
            },
            &[before.clone()],
            &[good.clone()],
        ));

        let mut other_color = good.clone();
        other_color.captions.highlight = Color::parse("#00CC66").unwrap();
        fails(
            &go(
                Check::Highlight {
                    color: "#00FF00".into(),
                },
                &[before.clone()],
                &[other_color],
            ),
            "instead of",
        );

        let mut extra = good.clone();
        extra.captions.color = Color::BLACK;
        fails(
            &go(
                Check::Highlight {
                    color: "#00FF00".into(),
                },
                &[before],
                &[extra],
            ),
            "/captions/color",
        );
    }

    #[test]
    fn hook_text_changes_only_the_text() {
        let before = styled();
        let want = Check::Hook {
            item: Some("hook-1".into()),
            text: "A closer look: x".into(),
        };
        let mut good = before.clone();
        if let ItemBody::Text(t) = &mut good.item_mut("hook-1").unwrap().body {
            t.text = "A closer look: *x*".into();
        }
        ok(&go(want.clone(), &[before.clone()], &[good.clone()]));

        let mut stretched = good.clone();
        stretched.item_mut("hook-1").unwrap().duration = 9.0;
        fails(
            &go(want.clone(), &[before.clone()], &[stretched]),
            "only the hook text",
        );

        let mut wrong = before.clone();
        if let ItemBody::Text(t) = &mut wrong.item_mut("hook-1").unwrap().body {
            t.text = "Something else".into();
        }
        fails(&go(want.clone(), &[before.clone()], &[wrong]), "expected");

        let mut replaced = before.clone();
        replaced.tracks[2].items.clear();
        put(
            &mut replaced,
            "text",
            text_item("hook-2", 0.0, 3.0, "A closer look: x", TextPreset::Hook),
        );
        fails(&go(want, &[before], &[replaced]), "only the hook text");
    }

    #[test]
    fn title_card_added_at_the_start_when_there_is_no_hook() {
        let before = made(VideoKind::TalkingHeadShort, &["a.mp4"]);
        let want = Check::Hook {
            item: None,
            text: "A closer look: x".into(),
        };
        let mut good = before.clone();
        put(
            &mut good,
            "text",
            text_item("t1", 0.0, 3.0, "A closer look: x", TextPreset::Title),
        );
        ok(&go(want.clone(), &[before.clone()], &[good.clone()]));

        let mut late = before.clone();
        put(
            &mut late,
            "text",
            text_item("t1", 5.0, 3.0, "A closer look: x", TextPreset::Title),
        );
        fails(&go(want.clone(), &[before.clone()], &[late]), "within 1 s");

        let mut wrong = before.clone();
        put(
            &mut wrong,
            "text",
            text_item("t1", 0.0, 3.0, "Hello", TextPreset::Title),
        );
        fails(&go(want.clone(), &[before.clone()], &[wrong]), "title text");

        let mut damaged = good.clone();
        damaged.captions.size = 0.2;
        fails(&go(want.clone(), &[before.clone()], &[damaged]), "changed");

        fails(
            &go(want, &[before.clone()], &[before]),
            "exactly one new title item",
        );
    }

    // ---- level 3 ----

    fn talk() -> Project {
        let mut p = proj(VideoKind::TalkingHeadLong);
        video(&mut p, "src", "talk.mp4");
        main_clip(&mut p, "m1", "src", 0.0, 100.0);
        p
    }

    fn split(ranges: &[(f64, f64)]) -> Project {
        let mut p = proj(VideoKind::TalkingHeadLong);
        video(&mut p, "src", "talk.mp4");
        for (i, &(s, e)) in ranges.iter().enumerate() {
            main_clip(&mut p, &format!("m{i}"), "src", s, e - s);
        }
        p
    }

    fn cut_check() -> Check {
        Check::Cut {
            asset: "src".into(),
            start: 30.0,
            end: 34.0,
            max_removed: 6.0,
        }
    }

    #[test]
    fn cut_removes_the_phrase_and_little_else() {
        let before = talk();
        ok(&go(
            cut_check(),
            &[before.clone()],
            &[split(&[(0.0, 30.0), (34.0, 100.0)])],
        ));
        ok(&go(
            cut_check(),
            &[before.clone()],
            &[split(&[(0.0, 29.5), (34.5, 100.0)])],
        ));

        fails(
            &go(cut_check(), &[before.clone()], &[before.clone()]),
            "still on the timeline",
        );
        fails(
            &go(
                cut_check(),
                &[before.clone()],
                &[split(&[(0.0, 31.0), (34.0, 100.0)])],
            ),
            "still on the timeline",
        );
        fails(
            &go(
                cut_check(),
                &[before.clone()],
                &[split(&[(0.0, 30.0), (33.0, 100.0)])],
            ),
            "still on the timeline",
        );
        fails(
            &go(
                cut_check(),
                &[before.clone()],
                &[split(&[(0.0, 25.0), (40.0, 100.0)])],
            ),
            "at most",
        );
        fails(
            &go(
                cut_check(),
                &[before.clone()],
                &[split(&[(0.0, 30.0), (34.0, 90.0)])],
            ),
            "at most",
        );
        // Right amount, wrong place: the phrase stays and unrelated footage goes.
        fails(
            &go(
                cut_check(),
                &[before.clone()],
                &[split(&[(0.0, 10.0), (14.0, 100.0)])],
            ),
            "still on the timeline",
        );
        // Phrase gone plus a far-away removal, within the total budget.
        fails(
            &go(
                cut_check(),
                &[before.clone()],
                &[split(&[(0.0, 30.0), (34.0, 50.0), (52.0, 100.0)])],
            ),
            "far",
        );
    }

    #[test]
    fn cut_cannot_pass_by_chopping_the_whole_video() {
        let before = talk();
        let mut empty = before.clone();
        empty.tracks[0].items.clear();
        fails(
            &go(cut_check(), &[before.clone()], &[empty]),
            "nothing of the recording",
        );
        fails(
            &go(cut_check(), &[before.clone()], &[split(&[(0.0, 5.0)])]),
            "at most",
        );
    }

    #[test]
    fn cut_without_ground_truth_is_a_check_failure() {
        let before = talk();
        let r = go(
            Check::Cut {
                asset: String::new(),
                start: 0.0,
                end: 0.0,
                max_removed: 0.0,
            },
            &[before.clone()],
            &[before.clone()],
        );
        assert!(!r.pass);
        assert_eq!(r.failure_class.as_deref(), Some("check"));
        // Phrase not in the baseline's kept footage.
        let trimmed = split(&[(40.0, 100.0)]);
        let r = go(cut_check(), &[trimmed.clone()], &[trimmed]);
        assert_eq!(r.failure_class.as_deref(), Some("check"));
    }

    #[test]
    fn start_trims_the_front_and_keeps_the_rest() {
        let before = talk();
        let want = Check::Start {
            asset: "src".into(),
            start: 20.0,
        };
        ok(&go(
            want.clone(),
            &[before.clone()],
            &[split(&[(19.8, 100.0)])],
        ));

        fails(
            &go(want.clone(), &[before.clone()], &[before.clone()]),
            "opens at",
        );
        fails(
            &go(want.clone(), &[before.clone()], &[split(&[(10.0, 100.0)])]),
            "opens at",
        );
        fails(
            &go(want.clone(), &[before.clone()], &[split(&[(25.0, 100.0)])]),
            "opens at",
        );
        fails(
            &go(want.clone(), &[before.clone()], &[split(&[(19.8, 60.0)])]),
            "dropped",
        );
        // Opens right, but the earlier footage is appended later on the timeline.
        fails(
            &go(
                want.clone(),
                &[before.clone()],
                &[split(&[(20.0, 100.0), (0.0, 20.0)])],
            ),
            "before the phrase",
        );
        // Baseline that already starts there cannot grade the task.
        let started = split(&[(20.0, 100.0)]);
        assert_eq!(
            go(want, &[started.clone()], &[started])
                .failure_class
                .as_deref(),
            Some("check")
        );
    }

    // ---- level 4 ----

    fn broll_check() -> Check {
        Check::Broll {
            file: "city.mp4".into(),
            asset: "src".into(),
            source_time: 30.0,
        }
    }

    fn with_broll(role: AssetRole, start: f64, duration: f64) -> Project {
        let mut p = talk();
        asset(&mut p, "b", "city.mp4", AssetKind::Video, role, 10.0);
        put(&mut p, "broll", clip_item("br", start, duration, "b", 0.0));
        p
    }

    #[test]
    fn broll_needs_role_timing_and_duration() {
        let before = talk();
        ok(&go(
            broll_check(),
            &[before.clone()],
            &[with_broll(AssetRole::Broll, 30.4, 3.0)],
        ));

        fails(
            &go(
                broll_check(),
                &[before.clone()],
                &[with_broll(AssetRole::Source, 30.4, 3.0)],
            ),
            "role",
        );
        fails(
            &go(
                broll_check(),
                &[before.clone()],
                &[with_broll(AssetRole::Broll, 35.0, 3.0)],
            ),
            "limit 1 s",
        );
        fails(
            &go(
                broll_check(),
                &[before.clone()],
                &[with_broll(AssetRole::Broll, 30.4, 6.0)],
            ),
            "2 to 4 s",
        );
        fails(
            &go(
                broll_check(),
                &[before.clone()],
                &[with_broll(AssetRole::Broll, 30.4, 1.0)],
            ),
            "2 to 4 s",
        );
        fails(
            &go(broll_check(), &[before.clone()], &[before.clone()]),
            "one new asset",
        );

        let mut wrong_file = with_broll(AssetRole::Broll, 30.4, 3.0);
        wrong_file.assets[1].path = PathBuf::from("inbox/other.mp4");
        fails(
            &go(broll_check(), &[before.clone()], &[wrong_file]),
            "expected city.mp4",
        );

        let mut damaged = with_broll(AssetRole::Broll, 30.4, 3.0);
        damaged.item_mut("m1").unwrap().duration = 90.0;
        fails(&go(broll_check(), &[before], &[damaged]), "changed");
    }

    fn lt_check() -> Check {
        Check::LowerThird {
            name: "Katelynn Greer".into(),
            role: "Research scientist".into(),
        }
    }

    fn with_lower_third(text: &str, start: f64) -> Project {
        let mut p = talk();
        put(
            &mut p,
            "text",
            text_item("lt", start, 4.0, text, TextPreset::LowerThird),
        );
        p
    }

    /// What a real op does to a multi-clip main track: starts recomputed, durations a float
    /// apart (34.111999999999995 against 34.112).
    fn noisy(p: &Project) -> Project {
        let mut n = p.clone();
        for (i, item) in n.tracks[0].items.iter_mut().enumerate().skip(1) {
            item.start += 1e-12 * i as f64;
            item.duration += 1e-13;
        }
        n
    }

    #[test]
    fn float_noise_and_recomputed_main_starts_are_not_changes() {
        let before = split(&[(0.0, 34.112), (34.112, 60.0), (60.0, 100.0)]);
        let name = "Katelynn Greer\nResearch scientist";

        // Noise plus the requested lower third passes.
        let mut good = noisy(&before);
        put(
            &mut good,
            "text",
            text_item("lt", 1.0, 4.0, name, TextPreset::LowerThird),
        );
        for item in good.tracks[0].items.iter_mut().skip(1) {
            item.start += 1e-9;
            item.duration += 1e-10;
        }
        ok(&go(lt_check(), &[before.clone()], &[good.clone()]));

        // Real changes next to the noise still fail: a main duration, an unrelated style field,
        // a source range, or an item that moved on a non-main track.
        let mut longer = good.clone();
        longer.tracks[0].items[1].duration += 0.5;
        fails(
            &go(lt_check(), &[before.clone()], &[longer]),
            "/tracks/0/items/1/duration",
        );
        let mut styled = good.clone();
        styled.captions.size += 0.01;
        fails(
            &go(lt_check(), &[before.clone()], &[styled]),
            "/captions/size",
        );
        let mut shifted = good.clone();
        shifted.tracks[0].items[2].body = ItemBody::Clip(Clip::new("src", 61.0));
        fails(&go(lt_check(), &[before.clone()], &[shifted]), "src_in");
        let mut base_text = before.clone();
        put(
            &mut base_text,
            "text",
            text_item("other", 20.0, 3.0, "Other", TextPreset::Card),
        );
        let mut moved = noisy(&base_text);
        put(
            &mut moved,
            "text",
            text_item("lt", 1.0, 4.0, name, TextPreset::LowerThird),
        );
        moved.item_mut("other").unwrap().start += 0.5;
        fails(&go(lt_check(), &[base_text], &[moved]), "changed");
    }

    #[test]
    fn style_checks_ignore_noise_but_catch_real_side_effects() {
        let before = split(&[(0.0, 34.112), (34.112, 100.0)]);
        let mut good = noisy(&before);
        good.captions.size = 0.06;
        ok(&go(
            Check::CaptionSize { size: 0.06 },
            &[before.clone()],
            &[good.clone()],
        ));
        let mut longer = good.clone();
        longer.tracks[0].items[1].duration += 0.5;
        fails(
            &go(Check::CaptionSize { size: 0.06 }, &[before], &[longer]),
            "only /captions/size",
        );
    }

    #[test]
    fn lower_third_has_name_role_and_starts_early() {
        let before = talk();
        ok(&go(
            lt_check(),
            &[before.clone()],
            &[with_lower_third("Katelynn Greer\nResearch scientist", 1.0)],
        ));
        fails(
            &go(
                lt_check(),
                &[before.clone()],
                &[with_lower_third("Katelynn Greer\nResearch scientist", 5.0)],
            ),
            "first 3 s",
        );
        fails(
            &go(
                lt_check(),
                &[before.clone()],
                &[with_lower_third("Katelynn Greer", 1.0)],
            ),
            "role",
        );
        fails(
            &go(
                lt_check(),
                &[before.clone()],
                &[with_lower_third("Research scientist", 1.0)],
            ),
            "name",
        );

        let mut card = before.clone();
        put(
            &mut card,
            "text",
            text_item(
                "lt",
                1.0,
                4.0,
                "Katelynn Greer Research scientist",
                TextPreset::Card,
            ),
        );
        fails(&go(lt_check(), &[before.clone()], &[card]), "lower_third");

        let mut damaged = with_lower_third("Katelynn Greer\nResearch scientist", 1.0);
        damaged.captions.size = 0.1;
        fails(&go(lt_check(), &[before], &[damaged]), "changed");
    }

    fn with_music(volume: f32, duck: bool, pieces: &[(f64, f64)], role: AssetRole) -> Project {
        let mut p = proj(VideoKind::TalkingHeadLong);
        video(&mut p, "src", "talk.mp4");
        main_clip(&mut p, "m1", "src", 0.0, 60.0);
        asset(&mut p, "m", "bed.mp3", AssetKind::Audio, role, 20.0);
        for (i, &(start, duration)) in pieces.iter().enumerate() {
            let mut item = clip_item(&format!("mu{i}"), start, duration, "m", 0.0);
            let c = item.clip_mut().unwrap();
            c.volume = volume;
            c.duck = duck;
            put(&mut p, "music", item);
        }
        p
    }

    fn music_base() -> Project {
        let mut p = proj(VideoKind::TalkingHeadLong);
        video(&mut p, "src", "talk.mp4");
        main_clip(&mut p, "m1", "src", 0.0, 60.0);
        p
    }

    #[test]
    fn music_must_cover_90_percent_and_be_quiet() {
        let before = music_base();
        let want = Check::Music {
            file: "bed.mp3".into(),
        };
        let looped = [(0.0, 20.0), (20.0, 20.0), (40.0, 20.0)];
        ok(&go(
            want.clone(),
            &[before.clone()],
            &[with_music(0.2, false, &looped, AssetRole::Music)],
        ));
        ok(&go(
            want.clone(),
            &[before.clone()],
            &[with_music(1.0, true, &looped, AssetRole::Music)],
        ));
        ok(&go(
            want.clone(),
            &[before.clone()],
            &[with_music(
                0.2,
                false,
                &[(0.0, 20.0), (20.0, 20.0), (40.0, 15.0)],
                AssetRole::Music,
            )],
        ));

        fails(
            &go(
                want.clone(),
                &[before.clone()],
                &[with_music(
                    0.2,
                    false,
                    &[(0.0, 20.0), (20.0, 10.0)],
                    AssetRole::Music,
                )],
            ),
            "90% needed",
        );
        fails(
            &go(
                want.clone(),
                &[before.clone()],
                &[with_music(0.2, false, &[(0.0, 20.0)], AssetRole::Music)],
            ),
            "90% needed",
        );
        fails(
            &go(
                want.clone(),
                &[before.clone()],
                &[with_music(1.0, false, &looped, AssetRole::Music)],
            ),
            "loud",
        );
        fails(
            &go(
                want.clone(),
                &[before.clone()],
                &[with_music(0.0, true, &looped, AssetRole::Music)],
            ),
            "silent",
        );
        fails(
            &go(
                want.clone(),
                &[before.clone()],
                &[with_music(0.2, false, &looped, AssetRole::Sfx)],
            ),
            "music role",
        );
        fails(
            &go(want.clone(), &[before.clone()], &[before.clone()]),
            "one new asset",
        );

        // Audible-track rule: the music sits on a new muted audio track.
        let mut muted = with_music(0.2, false, &looped, AssetRole::Music);
        let items = std::mem::take(&mut muted.tracks[4].items);
        muted.tracks.push(Track {
            id: "muted-music".into(),
            kind: TrackKind::Audio,
            name: "Muted".into(),
            muted: true,
            hidden: false,
            items,
        });
        fails(&go(want.clone(), &[before.clone()], &[muted]), "audible");

        // Overlapping copies must not be double counted toward coverage.
        fails(
            &go(
                want,
                &[before],
                &[with_music(
                    0.2,
                    false,
                    &[(0.0, 20.0), (0.0, 20.0), (5.0, 15.0)],
                    AssetRole::Music,
                )],
            ),
            "90% needed",
        );
    }

    // ---- level 5 ----

    fn clip_project(name: &str, src_in: f64, len: f64) -> Project {
        let mut p = Project::new(name, VideoKind::Clips, Canvas::new(1080, 1920));
        video(&mut p, "src", "talk.mp4");
        main_clip(&mut p, "m1", "src", src_in, len);
        p
    }

    fn transcript() -> Transcript {
        Transcript {
            asset: "src".into(),
            provider: "test".into(),
            language: None,
            duration: 1000.0,
            words: (0..100)
                .map(|i| TranscriptWord {
                    text: if i % 2 == 0 {
                        "Space.".into()
                    } else {
                        "filler".into()
                    },
                    start: i as f64 * 10.0,
                    end: i as f64 * 10.0 + 0.5,
                    confidence: None,
                })
                .collect(),
        }
    }

    fn clips_check(topic: &str) -> Task {
        task(Check::Clips {
            topic: vec![topic.into()],
        })
    }

    #[test]
    fn clips_need_three_distinct_topical_clips() {
        let base = clip_project("base", 500.0, 10.0);
        let c = |n: &str, s: f64, l: f64| clip_project(n, s, l);
        let tr = [transcript()];
        let run = |t: &Task, after: &[Project]| super::check(t, &[base.clone()], after, &tr, true);

        let good = [
            base.clone(),
            c("c1", 0.0, 30.0),
            c("c2", 100.0, 30.0),
            c("c3", 200.0, 30.0),
        ];
        let r = run(&clips_check("space"), &good);
        ok(&r);

        fails(&run(&clips_check("mars"), &good), "topic words");
        fails(&run(&clips_check("space"), &good[..3]), "found 2");
        let long = [
            base.clone(),
            c("c1", 0.0, 30.0),
            c("c2", 100.0, 30.0),
            c("c3", 200.0, 70.0),
        ];
        fails(&run(&clips_check("space"), &long), "20 to 60 s");
        let short = [
            base.clone(),
            c("c1", 0.0, 30.0),
            c("c2", 100.0, 30.0),
            c("c3", 200.0, 10.0),
        ];
        fails(&run(&clips_check("space"), &short), "20 to 60 s");
        let overlap = [
            base.clone(),
            c("c1", 0.0, 30.0),
            c("c2", 10.0, 30.0),
            c("c3", 200.0, 30.0),
        ];
        fails(
            &run(&clips_check("space"), &overlap),
            "mostly the same footage",
        );
        let mut wrong_kind = good.clone();
        wrong_kind[3].kind = VideoKind::Vlog;
        fails(&run(&clips_check("space"), &wrong_kind), "instead of clips");
        let mut wrong_canvas = good.clone();
        wrong_canvas[3].canvas = Canvas::new(1920, 1080);
        fails(&run(&clips_check("space"), &wrong_canvas), "canvas");
        // The unchanged baseline never counts as one of the clips.
        let four = [
            base.clone(),
            base.clone(),
            c("c1", 0.0, 30.0),
            c("c2", 100.0, 30.0),
        ];
        fails(&run(&clips_check("space"), &four), "found 2");

        let r = super::check(&clips_check("space"), &[base.clone()], &good, &[], true);
        assert_eq!(r.failure_class.as_deref(), Some("check"));
        let r = super::check(
            &task(Check::Clips { topic: vec![] }),
            &[base],
            &good,
            &tr,
            true,
        );
        assert_eq!(r.failure_class.as_deref(), Some("check"));
    }

    const OFFSET: f64 = 1.3;

    /// Camera A and B alternate; B is shifted by the signed offset (`b = a - offset`).
    fn two_cam(b_shift: f64, voice: bool) -> Project {
        let mut p = proj(VideoKind::Podcast);
        video(&mut p, "a", "cam-a.mp4");
        video(&mut p, "b", "cam-b.mp4");
        main_clip(&mut p, "m1", "a", 100.0, 10.0);
        main_clip(&mut p, "m2", "b", 110.0 + b_shift, 10.0);
        main_clip(&mut p, "m3", "a", 120.0, 10.0);
        main_clip(&mut p, "m4", "b", 130.0 + b_shift, 5.0);
        if voice {
            asset(
                &mut p,
                "v",
                "cam-a.mp4",
                AssetKind::Video,
                AssetRole::Voice,
                1000.0,
            );
            put(&mut p, "sfx", clip_item("voice", 0.0, 35.0, "v", 100.0));
        }
        p
    }

    fn podcast_check() -> Check {
        Check::Podcast {
            cam_a: "cam-a.mp4".into(),
            cam_b: "cam-b.mp4".into(),
            offset: OFFSET,
        }
    }

    #[test]
    fn podcast_checks_the_signed_offset_and_switching() {
        let mut one_cam = proj(VideoKind::Podcast);
        video(&mut one_cam, "a", "cam-a.mp4");
        video(&mut one_cam, "b", "cam-b.mp4");
        main_clip(&mut one_cam, "m1", "a", 100.0, 30.0);
        let before = [one_cam];
        for voice in [true, false] {
            ok(&go(podcast_check(), &before, &[two_cam(-OFFSET, voice)]));
            fails(
                &go(podcast_check(), &before, &[two_cam(OFFSET, voice)]),
                if voice { "out of sync" } else { "jumps back" },
            );
            fails(
                &go(podcast_check(), &before, &[two_cam(0.0, voice)]),
                if voice { "out of sync" } else { "jumps back" },
            );
            fails(
                &go(podcast_check(), &before, &[two_cam(-OFFSET + 0.3, voice)]),
                if voice { "out of sync" } else { "jumps back" },
            );
        }
        // Never switching: only camera A.
        let mut only_a = proj(VideoKind::Podcast);
        video(&mut only_a, "a", "cam-a.mp4");
        video(&mut only_a, "b", "cam-b.mp4");
        main_clip(&mut only_a, "m1", "a", 100.0, 20.0);
        main_clip(&mut only_a, "m2", "a", 120.0, 10.0);
        fails(&go(podcast_check(), &before, &[only_a]), "both cameras");
        // One cut between cameras is not switching.
        let mut once = proj(VideoKind::Podcast);
        video(&mut once, "a", "cam-a.mp4");
        video(&mut once, "b", "cam-b.mp4");
        main_clip(&mut once, "m1", "a", 100.0, 10.0);
        main_clip(&mut once, "m2", "b", 110.0 - OFFSET, 10.0);
        fails(&go(podcast_check(), &before, &[once]), "camera switches");
        // A baseline that already satisfies the task is not the agent's work.
        let done = two_cam(-OFFSET, true);
        let r = go(podcast_check(), &[done.clone()], &[done]);
        ok(&r);
        assert_eq!(r.evidence["observed"]["baseline_passes"], true);
        assert_eq!(r.evidence["observed"]["unchanged_from_baseline"], true);
    }

    /// Two cameras plus a speech track on the audio side (camera A's file, as the podcast recipe
    /// lays it). `main`: (asset id, source start, seconds); `speech`: (source start, seconds).
    fn cast(main: &[(&str, f64, f64)], speech: &[(f64, f64)]) -> Project {
        let mut p = proj(VideoKind::Podcast);
        video(&mut p, "a", "cam-a.mp4");
        video(&mut p, "b", "cam-b.mp4");
        asset(
            &mut p,
            "v",
            "cam-a.mp4",
            AssetKind::Video,
            AssetRole::Voice,
            1000.0,
        );
        for (i, &(id, s, d)) in main.iter().enumerate() {
            main_clip(&mut p, &format!("m{i}"), id, s, d);
        }
        let mut at = 0.0;
        for (i, &(s, d)) in speech.iter().enumerate() {
            put(&mut p, "sfx", clip_item(&format!("v{i}"), at, d, "v", s));
            at += d;
        }
        p
    }

    const WHOLE: [(&str, f64, f64); 4] = [
        ("a", 100.0, 10.0),
        ("b", 108.7, 10.0),
        ("a", 120.0, 10.0),
        ("b", 128.7, 5.0),
    ];

    #[test]
    fn cut_sees_the_phrase_on_another_camera_and_in_the_speech_track() {
        let before = [cast(&WHOLE, &[(100.0, 35.0)])];
        let want = Check::Cut {
            asset: "a".into(),
            start: 112.0,
            end: 114.0,
            max_removed: 4.0,
        };
        // Phrase (112 to 114 s, in camera B's piece) removed from picture and speech alike.
        let pieces = [
            ("a", 100.0, 10.0),
            ("b", 108.7, 2.0),
            ("b", 112.7, 6.0),
            ("a", 120.0, 10.0),
            ("b", 128.7, 5.0),
        ];
        ok(&go(
            want.clone(),
            &before,
            &[cast(&pieces, &[(100.0, 12.0), (114.0, 21.0)])],
        ));
        // Picture cut but the speech track still says it.
        fails(
            &go(want.clone(), &before, &[cast(&pieces, &[(100.0, 35.0)])]),
            "still on the timeline",
        );
        // Speech cut but camera B still shows the same words (offset-shifted source).
        fails(
            &go(
                want.clone(),
                &before,
                &[cast(&WHOLE, &[(100.0, 12.0), (114.0, 21.0)])],
            ),
            "still on the timeline",
        );
        // Camera A's piece removed while camera B repeats the dialogue and the speech is gone.
        let repeat = [("a", 100.0, 10.0), ("b", 108.7, 10.0), ("a", 120.0, 10.0)];
        fails(
            &go(
                want,
                &before,
                &[cast(&repeat, &[(100.0, 12.0), (114.0, 16.0)])],
            ),
            "still on the timeline",
        );
    }

    #[test]
    fn start_sees_earlier_speech_and_opens_in_the_recording_clock() {
        let before = [cast(&WHOLE, &[(100.0, 35.0)])];
        let want = Check::Start {
            asset: "a".into(),
            start: 112.0,
        };
        // Opens on camera B at 112 s of the recording (source 110.7 s) with the speech trimmed too.
        let trimmed = [("b", 110.7, 8.0), ("a", 120.0, 10.0), ("b", 128.7, 5.0)];
        ok(&go(
            want.clone(),
            &before,
            &[cast(&trimmed, &[(112.0, 23.0)])],
        ));
        // Speech still starts at the beginning.
        fails(
            &go(want.clone(), &before, &[cast(&trimmed, &[(100.0, 35.0)])]),
            "before the phrase",
        );
        // Camera B's source time is not the recording's clock: 112 s of source is 113.3 s of speech.
        let unshifted = [("b", 112.0, 8.0), ("a", 120.0, 10.0), ("b", 128.7, 5.0)];
        fails(
            &go(want.clone(), &before, &[cast(&unshifted, &[(112.0, 23.0)])]),
            "opens at",
        );
        // Opens at the very start.
        fails(
            &go(want, &before, &[cast(&WHOLE, &[(112.0, 23.0)])]),
            "opens at",
        );
    }

    fn tutorial_base() -> Project {
        let mut p = proj(VideoKind::Tutorial);
        video(&mut p, "src", "screen.mp4");
        main_clip(&mut p, "m1", "src", 0.0, 100.0);
        p
    }

    fn tutorial_check() -> Check {
        Check::Tutorial {
            silences: vec![(10.0, 14.0), (40.0, 46.0)],
        }
    }

    /// (source start, source end, speed)
    fn tutorial_edit(parts: &[(f64, f64, f64)]) -> Project {
        let mut p = proj(VideoKind::Tutorial);
        video(&mut p, "src", "screen.mp4");
        for (i, &(s, e, sp)) in parts.iter().enumerate() {
            main_clip(&mut p, &format!("m{i}"), "src", s, (e - s) / sp);
            speed(&mut p, &format!("m{i}"), sp);
        }
        p
    }

    #[test]
    fn tutorial_speeds_only_known_silences() {
        let before = [tutorial_base()];
        let good = tutorial_edit(&[
            (0.0, 10.0, 1.0),
            (10.0, 14.0, 4.0),
            (14.0, 40.0, 1.0),
            (40.0, 46.0, 4.0),
            (46.0, 100.0, 1.0),
        ]);
        ok(&go(tutorial_check(), &before, &[good]));
        // Edge padding from the detector is still fine.
        let padded = tutorial_edit(&[
            (0.0, 9.9, 1.0),
            (9.9, 14.1, 4.0),
            (14.1, 39.9, 1.0),
            (39.9, 46.1, 4.0),
            (46.1, 100.0, 1.0),
        ]);
        ok(&go(tutorial_check(), &before, &[padded]));

        let speech = tutorial_edit(&[
            (0.0, 10.0, 1.0),
            (10.0, 14.0, 4.0),
            (14.0, 40.0, 2.0),
            (40.0, 46.0, 4.0),
            (46.0, 100.0, 1.0),
        ]);
        fails(
            &go(tutorial_check(), &before, &[speech]),
            "speech was sped up",
        );
        let spill = tutorial_edit(&[(0.0, 5.0, 1.0), (5.0, 14.0, 4.0), (14.0, 100.0, 1.0)]);
        fails(
            &go(tutorial_check(), &before, &[spill]),
            "speech was sped up",
        );
        let r = go(tutorial_check(), &before, &[tutorial_base()]);
        fails(&r, "silence were sped up");
        assert_eq!(r.evidence["observed"]["baseline_passes"], false);
        let nothing = tutorial_edit(&[(0.0, 50.0, 1.0), (50.0, 100.0, 1.0)]);
        fails(
            &go(tutorial_check(), &before, &[nothing]),
            "silence were sped up",
        );
        let tiny = tutorial_edit(&[(0.0, 10.0, 1.0), (10.0, 11.0, 4.0), (11.0, 100.0, 1.0)]);
        fails(
            &go(tutorial_check(), &before, &[tiny]),
            "silence were sped up",
        );
        let slow = tutorial_edit(&[(0.0, 10.0, 1.0), (10.0, 14.0, 1.1), (14.0, 100.0, 1.0)]);
        fails(&go(tutorial_check(), &before, &[slow]), "at least");
        let deleted = tutorial_edit(&[
            (0.0, 10.0, 1.0),
            (10.0, 14.0, 4.0),
            (40.0, 46.0, 4.0),
            (46.0, 100.0, 1.0),
        ]);
        fails(
            &go(tutorial_check(), &before, &[deleted]),
            "talking were deleted",
        );
        let r = go(
            Check::Tutorial { silences: vec![] },
            &before,
            &[tutorial_edit(&[(0.0, 50.0, 1.0), (50.0, 100.0, 1.0)])],
        );
        assert_eq!(r.failure_class.as_deref(), Some("check"));
    }

    fn beats() -> Vec<f64> {
        (0..400).map(|i| f64::from(i) * 0.5).collect()
    }

    fn vlog_project(clips: usize, each: f64) -> Project {
        let mut p = proj(VideoKind::Vlog);
        video(&mut p, "v", "trip.mp4");
        asset(
            &mut p,
            "m",
            "song.mp3",
            AssetKind::Audio,
            AssetRole::Music,
            200.0,
        );
        for i in 0..clips {
            main_clip(&mut p, &format!("c{i}"), "v", i as f64 * 10.0, each);
        }
        put(
            &mut p,
            "music",
            clip_item("song", 0.0, each * clips as f64, "m", 0.0),
        );
        p
    }

    #[test]
    fn vlog_is_50_to_70_seconds_with_cuts_on_beats() {
        let before = [vlog_project(4, 10.0)];
        let vlog = Check::Vlog { beats: beats() };
        ok(&go(vlog.clone(), &before, &[vlog_project(10, 6.0)]));
        ok(&go(vlog.clone(), &before, &[vlog_project(8, 7.5)]));

        fails(
            &go(vlog.clone(), &before, &[vlog_project(10, 6.25)]),
            "on a beat",
        );
        fails(
            &go(vlog.clone(), &before, &[vlog_project(5, 8.0)]),
            "expected 50 to 70 s",
        );
        fails(
            &go(vlog.clone(), &before, &[vlog_project(12, 6.0)]),
            "expected 50 to 70 s",
        );
        // One cut, on a beat, is a valid 60 s vlog; the same cut off the beat is not.
        ok(&go(vlog.clone(), &before, &[vlog_project(2, 30.0)]));
        fails(
            &go(vlog.clone(), &before, &[vlog_project(2, 30.25)]),
            "not on a beat",
        );
        let r = go(vlog.clone(), &before, &[before[0].clone()]);
        fails(&r, "expected 50 to 70 s");
        assert_eq!(r.evidence["observed"]["baseline_passes"], false);
        // A baseline that already satisfies the request is a valid, flagged result.
        let met = [vlog_project(10, 6.0)];
        let r = go(vlog.clone(), &met, &met);
        ok(&r);
        assert_eq!(r.evidence["observed"]["baseline_passes"], true);

        let mut silent = vlog_project(10, 6.0);
        silent.tracks[4].items.clear();
        fails(&go(vlog.clone(), &before, &[silent]), "no music clip");
        fails(
            &go(
                Check::Vlog { beats: vec![] },
                &before,
                &[vlog_project(10, 6.0)],
            ),
            "beats",
        );
        let r = go(
            Check::Vlog { beats: vec![] },
            &before,
            &[vlog_project(10, 6.0)],
        );
        assert_eq!(r.failure_class.as_deref(), Some("check"));
    }

    fn explainer_project(pictures: usize, step: f64) -> Project {
        let mut p = proj(VideoKind::Explainer);
        asset(
            &mut p,
            "n",
            "voice.mp3",
            AssetKind::Audio,
            AssetRole::Voice,
            100.0,
        );
        for i in 0..pictures {
            asset(
                &mut p,
                &format!("i{i}"),
                &format!("pic{i}.png"),
                AssetKind::Image,
                AssetRole::Image,
                0.0,
            );
            main_clip(&mut p, &format!("c{i}"), &format!("i{i}"), 0.0, 3.0);
        }
        if step == 0.0 {
            // Pile every picture at time zero on an overlay track instead.
            p.tracks[0].items.clear();
            for i in 0..pictures {
                put(
                    &mut p,
                    "broll",
                    clip_item(&format!("o{i}"), 0.0, 3.0, &format!("i{i}"), 0.0),
                );
            }
        }
        p
    }

    #[test]
    fn explainer_needs_a_picture_per_sentence_within_20_percent() {
        let before = [explainer_project(1, 3.0)];
        let want = Check::Explainer { sentences: 10 };
        ok(&go(want.clone(), &before, &[explainer_project(10, 3.0)]));
        ok(&go(want.clone(), &before, &[explainer_project(8, 3.0)]));
        ok(&go(want.clone(), &before, &[explainer_project(12, 3.0)]));
        fails(
            &go(want.clone(), &before, &[explainer_project(7, 3.0)]),
            "within 20%",
        );
        fails(
            &go(want.clone(), &before, &[explainer_project(13, 3.0)]),
            "within 20%",
        );
        fails(
            &go(want.clone(), &before, &[explainer_project(10, 0.0)]),
            "stacked",
        );
        fails(
            &go(want.clone(), &before, &[before[0].clone()]),
            "within 20%",
        );
        let same = [explainer_project(10, 3.0)];
        let r = go(want.clone(), &same, &same);
        ok(&r);
        assert_eq!(r.evidence["observed"]["baseline_passes"], true);
        let r = go(
            Check::Explainer { sentences: 0 },
            &before,
            &[explainer_project(1, 3.0)],
        );
        assert_eq!(r.failure_class.as_deref(), Some("check"));
    }

    fn promo_project(first: &str, start: f64, second: &str) -> Project {
        let mut p = proj(VideoKind::Promo);
        video(&mut p, "v", "launch.mp4");
        asset(
            &mut p,
            "m",
            "song.mp3",
            AssetKind::Audio,
            AssetRole::Music,
            100.0,
        );
        main_clip(&mut p, "c1", "v", 0.0, 20.0);
        put(&mut p, "music", clip_item("song", 0.0, 20.0, "m", 0.0));
        put(
            &mut p,
            "text",
            text_item("t1", start, 4.0, first, TextPreset::Title),
        );
        put(
            &mut p,
            "text",
            text_item("t2", 8.0, 4.0, second, TextPreset::Title),
        );
        p
    }

    fn promo_check() -> Check {
        Check::Promo {
            item: "t1".into(),
            text: "Built for the next giant leap.".into(),
            beats: beats(),
        }
    }

    #[test]
    fn promo_line_changes_on_the_beat_and_others_stay() {
        let before = [promo_project("Old line", 2.0, "Second line")];
        ok(&go(
            promo_check(),
            &before,
            &[promo_project(
                "Built for the next giant leap.",
                2.0,
                "Second line",
            )],
        ));
        ok(&go(
            promo_check(),
            &before,
            &[promo_project(
                "Built for the next *giant* leap.",
                2.5,
                "Second line",
            )],
        ));

        fails(
            &go(
                promo_check(),
                &before,
                &[promo_project("Something else", 2.0, "Second line")],
            ),
            "expected",
        );
        fails(
            &go(
                promo_check(),
                &before,
                &[promo_project(
                    "Built for the next giant leap.",
                    2.25,
                    "Second line",
                )],
            ),
            "not on a beat",
        );
        fails(
            &go(
                promo_check(),
                &before,
                &[promo_project(
                    "Built for the next giant leap.",
                    2.0,
                    "Changed too",
                )],
            ),
            "only the text",
        );
        fails(&go(promo_check(), &before, &[before[0].clone()]), "must");
        let mut loud = promo_project("Built for the next giant leap.", 2.0, "Second line");
        loud.item_mut("t1").unwrap().duration = 6.0;
        ok(&go(promo_check(), &before, &[loud]));
        let mut restyled = promo_project("Built for the next giant leap.", 2.0, "Second line");
        if let ItemBody::Text(t) = &mut restyled.item_mut("t1").unwrap().body {
            t.preset = TextPreset::Cta;
        }
        fails(&go(promo_check(), &before, &[restyled]), "only the text");
        let r = go(
            Check::Promo {
                item: String::new(),
                text: "x".into(),
                beats: beats(),
            },
            &before,
            &before,
        );
        assert_eq!(r.failure_class.as_deref(), Some("check"));
        let r = go(
            Check::Promo {
                item: "t1".into(),
                text: "x".into(),
                beats: vec![],
            },
            &before,
            &before,
        );
        assert_eq!(r.failure_class.as_deref(), Some("check"));
    }

    fn lyric_project(first: &[&str], first_start: f64, second: &[&str]) -> Project {
        let mut p = proj(VideoKind::MusicVideo);
        video(&mut p, "v", "visual.mp4");
        asset(
            &mut p,
            "m",
            "song.mp3",
            AssetKind::Audio,
            AssetRole::Music,
            100.0,
        );
        main_clip(&mut p, "c1", "v", 0.0, 30.0);
        put(&mut p, "music", clip_item("song", 0.0, 30.0, "m", 0.0));
        put(&mut p, "captions", caps("cap-1", first_start, 2.0, first));
        put(&mut p, "captions", caps("cap-2", 12.0, 2.0, second));
        p
    }

    fn lyric_check() -> Check {
        Check::Lyric {
            at: 10.0,
            text: "Hold me tight [chorus]".into(),
        }
    }

    #[test]
    fn lyric_changes_text_in_place_with_its_timing() {
        let before = [lyric_project(
            &["Hold", "me", "tight"],
            10.0,
            &["never", "let", "go"],
        )];
        let good = lyric_project(
            &["Hold", "me", "tight", "[chorus]"],
            10.0,
            &["never", "let", "go"],
        );
        ok(&go(lyric_check(), &before, &[good]));

        fails(
            &go(
                lyric_check(),
                &before,
                &[lyric_project(
                    &["Hold", "me", "tight", "[chorus]"],
                    11.0,
                    &["never", "let", "go"],
                )],
            ),
            "moved",
        );
        fails(
            &go(
                lyric_check(),
                &before,
                &[lyric_project(
                    &["Hold", "me", "close", "[chorus]"],
                    10.0,
                    &["never", "let", "go"],
                )],
            ),
            "not in the project",
        );
        fails(
            &go(
                lyric_check(),
                &before,
                &[lyric_project(
                    &["Hold", "me", "tight", "[chorus]"],
                    10.0,
                    &["never", "let", "fall"],
                )],
            ),
            "other parts",
        );
        fails(
            &go(lyric_check(), &before, &[before[0].clone()]),
            "not in the project",
        );

        let mut damaged = lyric_project(
            &["Hold", "me", "tight", "[chorus]"],
            10.0,
            &["never", "let", "go"],
        );
        damaged.captions.size = 0.1;
        fails(&go(lyric_check(), &before, &[damaged]), "other parts");

        let mut dropped = lyric_project(
            &["Hold", "me", "tight", "[chorus]"],
            10.0,
            &["never", "let", "go"],
        );
        dropped.tracks[3].items.retain(|i| i.id != "cap-2");
        fails(&go(lyric_check(), &before, &[dropped]), "other parts");

        let absent = [lyric_project(
            &["Different", "words", "here"],
            10.0,
            &["never", "let", "go"],
        )];
        let r = go(lyric_check(), &absent, &absent);
        assert_eq!(r.failure_class.as_deref(), Some("check"));
    }

    // ---- level 6 ----

    fn face() -> FaceSample {
        FaceSample {
            t: 1.5,
            x: 0.45,
            y: 0.4,
            w: 0.1,
            h: 0.3,
        }
    }

    fn talking(faces: bool) -> Project {
        let mut p = proj(VideoKind::TalkingHeadShort);
        video(&mut p, "src", "talk.mp4");
        main_clip(&mut p, "m1", "src", 0.0, 10.0);
        put(
            &mut p,
            "captions",
            caps("cap-1", 0.5, 3.0, &["hello", "there", "friend"]),
        );
        if faces {
            p.faces.insert("src".into(), vec![face()]);
        }
        p
    }

    fn mouth_check() -> Check {
        Check::Mouth {
            asset: "src".into(),
            samples: vec![face()],
        }
    }

    fn placement(start: f64, end: f64, y: f32, fit: FaceFit) -> CaptionPlacement {
        CaptionPlacement {
            item: "cap-1".into(),
            start,
            end,
            requested: 0.7,
            bounds: Rect {
                x: 0.2,
                y,
                w: 0.6,
                h: 0.05,
            },
            size: 0.045,
            slot: Slot::Requested,
            clamped: false,
            face: fit,
            face_gaps: false,
        }
    }

    #[test]
    fn mouth_uses_actual_placements_against_frozen_faces() {
        let before = [talking(true)];
        ok(&go(mouth_check(), &before, &[talking(true)]));

        // The agent erased the face data: the engine no longer protects the mouth.
        fails(
            &go(mouth_check(), &before, &[talking(false)]),
            "cover the mouth",
        );
        // The agent wrote a decoy face far from the speaker instead of the real one.
        let mut decoy = talking(false);
        decoy.faces.insert(
            "src".into(),
            vec![FaceSample {
                t: 1.5,
                x: 0.05,
                y: 0.05,
                w: 0.1,
                h: 0.2,
            }],
        );
        fails(&go(mouth_check(), &before, &[decoy]), "cover the mouth");

        let mut off = talking(true);
        off.captions.mode = CaptionMode::Off;
        fails(&go(mouth_check(), &before, &[off]), "turned off");
        let mut hidden_track = talking(true);
        hidden_track.tracks[3].hidden = true;
        fails(
            &go(mouth_check(), &before, &[hidden_track]),
            "no caption page is drawn",
        );
        let mut no_captions = talking(true);
        no_captions.tracks[3].items.clear();
        fails(
            &go(mouth_check(), &before, &[no_captions]),
            "no caption page is drawn",
        );
        let mut cut_away = talking(true);
        cut_away.tracks[0].items.clear();
        main_clip(&mut cut_away, "m1", "src", 5.0, 5.0);
        fails(
            &go(mouth_check(), &before, &[cut_away]),
            "none of the frozen close-ups",
        );

        let r = go(
            Check::Mouth {
                asset: "src".into(),
                samples: vec![],
            },
            &before,
            &[talking(true)],
        );
        assert_eq!(r.failure_class.as_deref(), Some("check"));
        let r = go(
            mouth_check(),
            &[talking(true), talking(true)],
            &[talking(true)],
        );
        assert!(!r.pass);
    }

    #[test]
    fn mouth_judge_rejects_hidden_unchecked_covering_and_uncertified_pages() {
        let p = talking(true);
        let ids = vec!["src".to_string()];
        let samples = [face()];
        let judge = |pl: &[CaptionPlacement], pages| judge_mouth(&p, &ids, &samples, pl, pages);
        let clear = placement(0.4, 3.1, 0.8, FaceFit::Clear);
        assert!(judge(&[clear.clone()], 1).is_ok());

        let covering = placement(0.4, 3.1, 0.65, FaceFit::Clear);
        assert!(judge(&[covering], 1).is_err_and(|f| f.detail.contains("cover the mouth")));
        // The engine's own verdict is not trusted: an "off" or "unchecked" page that overlaps fails.
        for fit in [FaceFit::Off, FaceFit::Unchecked] {
            assert!(
                judge(&[placement(0.4, 3.1, 0.65, fit)], 1)
                    .is_err_and(|f| f.detail.contains("cover the mouth"))
            );
        }
        let hidden = placement(0.4, 3.1, 0.8, FaceFit::Hidden);
        assert!(judge(&[hidden], 1).is_err_and(|f| f.detail.contains("hidden")));
        // A page far from every close-up certifies nothing.
        let elsewhere = placement(6.0, 8.0, 0.8, FaceFit::Unchecked);
        assert!(judge(&[elsewhere], 1).is_err_and(|f| f.detail.contains("not certified")));
        // Most pages gone.
        assert!(judge(&[clear.clone()], 10).is_err_and(|f| f.detail.contains("remain")));
        let mut tiny = clear.clone();
        tiny.size = 0.01;
        assert!(judge(&[tiny], 1).is_err_and(|f| f.detail.contains("readable")));
        // No baseline pages: no ground truth to protect.
        assert!(judge(&[clear], 0).is_err_and(|f| f.class == Some("check")));
    }

    // ---- cross-cutting ----

    #[test]
    fn missing_prerequisite_is_a_check_failure() {
        let mut t = task(Check::CaptionSize { size: 0.06 });
        t.prerequisite = Some("No complete baseline project".into());
        let before = styled();
        let mut after = before.clone();
        after.captions.size = 0.06;
        let r = super::check(&t, &[before], &[after], &[], true);
        assert!(!r.pass);
        assert_eq!(r.failure_class.as_deref(), Some("check"));
        assert!(r.detail.contains("No complete baseline"));
        assert_eq!(r.evidence["expected"]["type"], "caption_size");
        assert_eq!(r.evidence["provenance"], "synthetic");
    }

    #[test]
    fn invalid_agent_projects_fail_without_panicking() {
        let before = styled();
        let mut empty = before.clone();
        empty.tracks.clear();
        let r = go(
            Check::CaptionSize { size: 0.06 },
            &[before.clone()],
            &[empty],
        );
        fails(&r, "is invalid");
        assert_eq!(r.failure_class, None);

        let mut dangling = before.clone();
        dangling.tracks[0].items[0].body = ItemBody::Clip(Clip::new("ghost", 0.0));
        fails(
            &go(
                Check::CaptionSize { size: 0.06 },
                &[before.clone()],
                &[dangling],
            ),
            "is invalid",
        );

        let mut no_main = before.clone();
        no_main.tracks.remove(0);
        for check in [
            Check::Run {
                kind: VideoKind::Vlog,
                sources: vec![],
                width: 1,
                height: 1,
            },
            Check::Mouth {
                asset: "src".into(),
                samples: vec![face()],
            },
            Check::Vlog { beats: beats() },
            Check::Tutorial {
                silences: vec![(0.0, 1.0)],
            },
        ] {
            assert!(!go(check, &[before.clone()], &[no_main.clone()]).pass);
        }
    }

    #[test]
    fn a_panic_inside_a_check_is_reported_as_a_failed_check() {
        // `Color::parse` slices the string by byte and panics inside a multi-byte character.
        let before = styled();
        let r = go(
            Check::Highlight {
                color: "€€".into()
            },
            &[before.clone()],
            &[before],
        );
        assert!(!r.pass);
        assert_eq!(r.failure_class.as_deref(), Some("check"));
        assert!(r.detail.contains("panic"));
    }

    #[test]
    fn baseline_satisfied_exposes_a_start_that_already_meets_the_task() {
        // The original (unprepared) mouth baseline: the engine already keeps captions clear.
        let safe = vec![talking(true)];
        assert!(baseline_satisfied(&task(mouth_check()), &safe, &[]));
        let covered = start_of(mouth_check(), safe, &[]);
        assert!(!baseline_satisfied(&task(mouth_check()), &covered, &[]));
        // Tasks that ask for a change are not met by their baseline.
        assert!(!baseline_satisfied(&task(cut_check()), &[talk()], &[]));
        // A prerequisite or missing truth is never "satisfied".
        let mut t = task(mouth_check());
        t.prerequisite = Some("none".into());
        assert!(!baseline_satisfied(&t, &[talking(true)], &[]));
    }

    // ---- prepared start states: the untouched project must never pass ----

    /// `prepare` must succeed and the untouched start must fail for a task reason.
    fn start_of(check: Check, mut ps: Vec<Project>, tr: &[Transcript]) -> Vec<Project> {
        let t = task(check);
        prepare(&t, &mut ps, tr).expect("a failing start state");
        let r = super::check(&t, &ps, &ps, tr, true);
        assert!(!r.pass && r.failure_class.is_none(), "{}", r.detail);
        ps
    }

    #[test]
    fn requested_change_tasks_fail_untouched_and_pass_with_the_edit() {
        let mut hooked = styled();
        if let ItemBody::Text(t) = &mut hooked.item_mut("hook-1").unwrap().body {
            t.text = "A closer look: x".into();
        }
        let mut sized = styled();
        sized.captions.size = 0.06;
        let mut green = styled();
        green.captions.highlight = Color::parse("#00ff00").unwrap();
        let no_hook = made(VideoKind::TalkingHeadShort, &["a.mp4"]);
        let mut titled = no_hook.clone();
        put(
            &mut titled,
            "text",
            text_item("t1", 0.0, 3.0, "A closer look: x", TextPreset::Title),
        );
        let looped = [(0.0, 20.0), (20.0, 20.0), (40.0, 20.0)];
        let cases = vec![
            (Check::CaptionSize { size: 0.06 }, styled(), sized),
            (
                Check::Highlight {
                    color: "#00FF00".into(),
                },
                styled(),
                green,
            ),
            (
                Check::Hook {
                    item: Some("hook-1".into()),
                    text: "A closer look: x".into(),
                },
                styled(),
                hooked,
            ),
            (
                Check::Hook {
                    item: None,
                    text: "A closer look: x".into(),
                },
                no_hook,
                titled,
            ),
            (cut_check(), talk(), split(&[(0.0, 30.0), (34.0, 100.0)])),
            (
                Check::Start {
                    asset: "src".into(),
                    start: 20.0,
                },
                talk(),
                split(&[(19.8, 100.0)]),
            ),
            (
                broll_check(),
                talk(),
                with_broll(AssetRole::Broll, 30.4, 3.0),
            ),
            (
                lt_check(),
                talk(),
                with_lower_third("Katelynn Greer\nResearch scientist", 1.0),
            ),
            (
                Check::Music {
                    file: "bed.mp3".into(),
                },
                music_base(),
                with_music(0.2, false, &looped, AssetRole::Music),
            ),
            (
                promo_check(),
                promo_project("Old line", 2.0, "Second line"),
                promo_project("Built for the next giant leap.", 2.0, "Second line"),
            ),
            (
                lyric_check(),
                lyric_project(&["Hold", "me", "tight"], 10.0, &["never", "let", "go"]),
                lyric_project(
                    &["Hold", "me", "tight", "[chorus]"],
                    10.0,
                    &["never", "let", "go"],
                ),
            ),
        ];
        for (check, base, good) in cases {
            let name = format!("{check:?}");
            let start = start_of(check.clone(), vec![base.clone()], &[]);
            assert_eq!(
                start,
                vec![base],
                "{name}: the start must stay the baseline"
            );
            let r = go(check, &start, &[good]);
            assert!(r.pass, "{name}: {}", r.detail);
        }
    }

    #[test]
    fn run_starts_with_no_project_and_clips_with_no_new_clips() {
        let good = made(VideoKind::Vlog, &["a.mp4", "b.mp4"]);
        let start = start_of(run_check(VideoKind::Vlog), vec![], &[]);
        assert!(start.is_empty());
        ok(&go(run_check(VideoKind::Vlog), &start, &[good]));

        let base = clip_project("base", 500.0, 10.0);
        let tr = [transcript()];
        let want = Check::Clips {
            topic: vec!["space".into()],
        };
        let start = start_of(want.clone(), vec![base.clone()], &tr);
        let three = [
            base,
            clip_project("c1", 0.0, 30.0),
            clip_project("c2", 100.0, 30.0),
            clip_project("c3", 200.0, 30.0),
        ];
        let r = super::check(&task(want), &start, &three, &tr, true);
        assert!(r.pass, "{}", r.detail);
    }

    #[test]
    fn podcast_starts_out_of_sync() {
        let good = two_cam(-OFFSET, true);
        let start = start_of(podcast_check(), vec![good.clone()], &[]);
        assert_ne!(start[0], good);
        fails(&go(podcast_check(), &start, &start), "out of sync");
        ok(&go(podcast_check(), &start, &[good]));
        // Without a speech track the continuity rule catches the same shift.
        let quiet = two_cam(-OFFSET, false);
        let start = start_of(podcast_check(), vec![quiet.clone()], &[]);
        ok(&go(podcast_check(), &start, &[quiet]));

        // Camera B never on the main track: no start state can be built, nothing is changed.
        let mut one_cam = proj(VideoKind::Podcast);
        video(&mut one_cam, "a", "cam-a.mp4");
        video(&mut one_cam, "b", "cam-b.mp4");
        main_clip(&mut one_cam, "m1", "a", 100.0, 30.0);
        let mut ps = vec![one_cam.clone()];
        assert!(prepare(&task(podcast_check()), &mut ps, &[]).is_err());
        assert_eq!(ps, vec![one_cam]);
    }

    #[test]
    fn tutorial_starts_with_the_silences_at_normal_speed() {
        let edit = tutorial_edit(&[
            (0.0, 10.0, 1.0),
            (10.0, 14.0, 4.0),
            (14.0, 40.0, 1.0),
            (40.0, 46.0, 4.0),
            (46.0, 100.0, 1.0),
        ]);
        let start = start_of(tutorial_check(), vec![edit.clone()], &[]);
        assert!(
            start[0]
                .main()
                .items
                .iter()
                .all(|i| i.clip().is_some_and(|c| (c.speed - 1.0).abs() < 1e-9))
        );
        let ids = vec!["src".to_string()];
        assert!((total(&kept(&start[0], &ids)) - total(&kept(&edit, &ids))).abs() < 1e-6);
        ok(&go(tutorial_check(), &start, &[edit]));
        // A baseline with nothing sped up already fails and is left as it is.
        let plain = start_of(tutorial_check(), vec![tutorial_base()], &[]);
        assert_eq!(plain, vec![tutorial_base()]);
    }

    #[test]
    fn vlog_starts_too_short_unless_it_already_fails() {
        let good = vlog_project(10, 6.0);
        let want = Check::Vlog { beats: beats() };
        let start = start_of(want.clone(), vec![good.clone()], &[]);
        assert!((main_end(&start[0]) - VLOG_SHORT).abs() < 1e-6);
        ok(&go(want.clone(), &start, &[good]));
        let short = vec![vlog_project(5, 8.0)];
        assert_eq!(start_of(want, short.clone(), &[]), short);
    }

    #[test]
    fn explainer_starts_with_too_many_pictures() {
        let good = explainer_project(10, 3.0);
        let want = Check::Explainer { sentences: 10 };
        let start = start_of(want.clone(), vec![good.clone()], &[]);
        assert!(pictures(&start[0]).len() > 12);
        ok(&go(want.clone(), &start, &[good]));
        let few = vec![explainer_project(5, 3.0)];
        assert_eq!(start_of(want, few.clone(), &[]), few);
        let mut ps = vec![explainer_project(10, 3.0)];
        assert!(prepare(&task(Check::Explainer { sentences: 0 }), &mut ps, &[]).is_err());
    }

    #[test]
    fn mouth_starts_really_covering_the_frozen_mouth() {
        let safe = talking(true);
        let start = start_of(mouth_check(), vec![safe.clone()], &[]);
        let p = &start[0];
        assert!(p.faces.is_empty(), "the engine must not be able to dodge");
        assert_ne!(p.captions.position, safe.captions.position);
        // Independent proof: real layout against the frozen face, not the project's faces.
        let zones = mouth_zones(p, &["src".to_string()], &[face()]).unwrap();
        let placements = Graphics::new().caption_placements(p);
        assert!(placements.iter().any(|x| {
            zones
                .iter()
                .any(|&(t, z)| t >= x.start && t <= x.end && hit(x.bounds, z))
        }));
        // Fixes: the engine's own protection, or moving the captions off the mouth.
        ok(&go(mouth_check(), &start, &[safe.clone()]));
        let mut moved = p.clone();
        apply(
            &mut moved,
            &Op::Set {
                path: "/captions/position".into(),
                value: json!(0.2),
            },
        )
        .unwrap();
        apply(
            &mut moved,
            &Op::Set {
                path: "/captions/size".into(),
                value: json!(0.04),
            },
        )
        .unwrap();
        ok(&go(mouth_check(), &start, &[moved]));

        // No usable close-up, unknown asset or captions off: no honest challenge exists.
        for (check, project) in [
            (
                Check::Mouth {
                    asset: "src".into(),
                    samples: vec![],
                },
                talking(true),
            ),
            (
                Check::Mouth {
                    asset: "ghost".into(),
                    samples: vec![face()],
                },
                talking(true),
            ),
            (mouth_check(), {
                let mut off = talking(true);
                off.captions.mode = CaptionMode::Off;
                off
            }),
        ] {
            let mut ps = vec![project.clone()];
            assert!(prepare(&task(check), &mut ps, &[]).is_err());
            assert_eq!(ps, vec![project]);
        }
    }

    #[test]
    fn prepare_refuses_tasks_it_cannot_grade() {
        let mut ps = vec![talk()];
        let ungradable = Check::Cut {
            asset: String::new(),
            start: 0.0,
            end: 0.0,
            max_removed: 0.0,
        };
        let e = prepare(&task(ungradable), &mut ps, &[]).unwrap_err();
        assert!(e.contains("cannot grade"), "{e}");
        let mut t = task(cut_check());
        t.prerequisite = Some("No phrase".into());
        assert_eq!(prepare(&t, &mut ps, &[]), Err("No phrase".to_owned()));
        assert_eq!(ps, vec![talk()]);
    }
}
