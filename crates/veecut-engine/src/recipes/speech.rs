//! Recipes driven by speech: talking heads (short and long), screen tutorials, podcasts, long
//! videos cut into clips, and faceless explainers.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use super::{
    Job, SAMPLE_RATE, add_captions, audio_track, hook, hook_from, lay_main, music_bed, pcm,
    place_audio, place_broll, text_sfx, track_face,
};
use crate::analyze::{self, CutReason};
use crate::media;
use crate::ops::{self, Op};
use crate::project::{
    Anim, AssetKind, AssetRole, CaptionMode, KenBurns, Point, Project, TextPreset, VideoKind, Zoom,
};
use crate::transcript::Transcript;

/// Herk Edit Kit's camera for talking heads: alternate wide and punched-in framing, switching
/// only at cuts and phrase boundaries, so no shot runs past `max_shot` (its measured reels: shots
/// 1.1-1.4 s on average, none over about 2.8 s). Long-form uses slower numbers.
pub(super) fn punch_ins(
    project: &mut Project,
    transcript: &Transcript,
    min_shot: f64,
    target: f64,
    max_shot: f64,
    scale: f32,
) {
    let asset = transcript.asset.as_str();
    let mut points: Vec<(f64, bool)> = project
        .main()
        .items
        .iter()
        .skip(1)
        .map(|i| (i.start, true))
        .collect();
    for pair in transcript.words.windows(2) {
        let (a, b) = (&pair[0], &pair[1]);
        let phrase_end =
            a.text.ends_with([',', '.', ';', ':', '?', '!']) || b.start - a.end >= 0.12;
        if !phrase_end {
            continue;
        }
        let ta = project.source_to_timeline(asset, (a.end - 0.01).max(a.start));
        let tb = project.source_to_timeline(asset, (b.start + 0.01).min(b.end));
        if let (Some(ta), Some(tb)) = (ta, tb) {
            if (0.0..0.6).contains(&(tb - ta)) {
                points.push(((ta + tb) / 2.0, false));
            }
        }
    }
    points.sort_by(|x, y| x.0.total_cmp(&y.0));
    let end = project.duration();
    let (mut punched, mut shot_start) = (false, 0.0);
    let mut intervals = Vec::new();
    for (i, &(t, is_cut)) in points.iter().enumerate() {
        let len = t - shot_start;
        if len <= 0.05 {
            continue;
        }
        let next = points.get(i + 1).map(|p| p.0).unwrap_or(end);
        let change = if is_cut {
            len >= min_shot * 0.5
        } else {
            len >= target || (len >= min_shot && next - shot_start > max_shot)
        };
        if change {
            if punched {
                intervals.push((shot_start, t));
            }
            punched = !punched;
            shot_start = t;
        }
    }
    if punched {
        intervals.push((shot_start, end));
    }
    for item in &mut project.main_mut().items {
        let (s, e, dur) = (item.start, item.end(), item.duration);
        let Some(clip) = item.clip_mut() else {
            continue;
        };
        for &(a, b) in &intervals {
            let (from, to) = (a.max(s), b.min(e));
            if to - from > 0.1 {
                let at = from - s;
                clip.zooms.push(Zoom {
                    at,
                    duration: (to - from).min(dur - at),
                    scale,
                    focus: None,
                    ease: 0.0,
                });
            }
        }
    }
}

fn source_video(job: &Job) -> Result<PathBuf> {
    job.req
        .sources
        .first()
        .cloned()
        .or_else(|| job.ws.newest_inbox_video())
        .context("no video to edit: put a recording in inbox/ or pass its path")
}

fn stem(path: &Path) -> String {
    path.file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .replace(['_', '.'], " ")
}

fn note_reviews(job: &mut Job, removed: &[analyze::Removal]) {
    for r in removed.iter().filter(|r| r.review) {
        job.report.review.push(format!(
            "Possible {:?} kept at {:.2}-{:.2}s of the source: \"{}\". If it is a mistake, cut it.",
            r.reason, r.start, r.end, r.text
        ));
    }
}

fn cut_summary(job: &mut Job, removed: &[analyze::Removal], before: f64, after: f64) {
    let applied = removed.iter().filter(|r| !r.review);
    let (mut pauses, mut mistakes) = (0, 0);
    for r in applied {
        match r.reason {
            CutReason::Silence => pauses += 1,
            _ => mistakes += 1,
        }
    }
    job.report.summary.push(format!(
        "Cut {pauses} pauses and {mistakes} mistakes: {before:.1}s to {after:.1}s"
    ));
}

fn add_hook(
    job: &mut Job,
    project: &mut Project,
    transcript: &Transcript,
    face: Option<Point>,
) -> Result<()> {
    let picked = job
        .req
        .hook
        .clone()
        .map(|text| (text, 3.0))
        .or_else(|| hook_from(transcript));
    if let Some((text, src_end)) = picked {
        let until = project
            .source_to_timeline(&transcript.asset, src_end)
            .unwrap_or(3.0);
        hook(project, &text, until, face)?;
        if job.req.hook.is_none() {
            job.report.review.push(format!(
                "Automatic hook: \"{text}\". Check that it works as the opener."
            ));
        }
    } else {
        job.report.review.push(
            "No complete short sentence fit an automatic hook, so the hook was left out. Add your own hook."
                .into(),
        );
    }
    Ok(())
}

/// Talking head (short or long) and screen tutorials.
pub(super) fn talking_head(job: &mut Job, progress: &mut dyn FnMut(&str)) -> Result<()> {
    let kind = job.req.kind;
    let source = source_video(job)?;
    let info = media::probe(&source)?;
    if info.kind != AssetKind::Video {
        bail!("{} is not a video", source.display());
    }
    let name = job.req.name.clone().unwrap_or_else(|| stem(&source));
    let canvas = job.canvas(info.fps);
    let (dir, mut p) = job.start(&name, canvas)?;
    let src = job.ws.add_asset(&mut p, &dir, &source, AssetRole::Source)?;
    let transcript = job.transcript(&dir, &p, &src, progress)?;
    progress("Planning cuts");
    let samples = pcm(&p, &dir, &src)?;
    let plan = analyze::plan_cuts(&transcript, &samples, SAMPLE_RATE, &job.cut_settings());
    note_reviews(job, &plan.removed);
    if kind == VideoKind::Tutorial {
        lay_tutorial(&mut p, &src, &plan)?;
    } else {
        lay_main(&mut p, &src, &plan.keep)?;
    }
    cut_summary(job, &plan.removed, info.duration, p.duration());

    // Framing: keep the face in a cover crop and at the center of punch-ins, and track it so
    // captions stay off the mouth and chin. Screen recordings have no face to find.
    let face = if kind != VideoKind::Tutorial {
        progress("Finding the face");
        track_face(job, &mut p, &src, &source, (0.0, info.duration))
    } else {
        None
    };
    set_focus(&mut p, face);
    match kind {
        VideoKind::TalkingHeadShort => punch_ins(&mut p, &transcript, 1.2, 2.0, 2.8, 1.15),
        VideoKind::TalkingHeadLong => punch_ins(&mut p, &transcript, 4.0, 7.0, 10.0, 1.10),
        _ => {}
    }
    if kind == VideoKind::Tutorial && p.captions.mode != CaptionMode::Off {
        // The screen is the content: plain subtitles low in the frame.
        p.captions.mode = CaptionMode::Subtitle;
        p.captions.position = 0.88;
        p.captions.uppercase = false;
        p.captions.max_words = 10;
        p.captions.max_chars = 42;
    }
    if kind == VideoKind::TalkingHeadShort && (job.req.hook.is_some() || job.style.hook()) {
        add_hook(job, &mut p, &transcript, face)?;
    }
    place_broll(job, &dir, &mut p, &transcript)?;
    add_captions(job, &mut p, &transcript)?;
    music_bed(job, &dir, &mut p, true)?;
    text_sfx(job, &dir, &mut p)?;
    job.finish(&dir, &mut p, progress)
}

/// Tutorials keep long silent stretches (typing, loading) but play them fast and muted, with
/// a small speed label, instead of cutting what the viewer needs to see.
fn lay_tutorial(p: &mut Project, src: &str, plan: &analyze::CutPlan) -> Result<()> {
    let mut segments: Vec<(f64, f64, f64)> = plan.keep.iter().map(|&(a, b)| (a, b, 1.0)).collect();
    for r in plan
        .removed
        .iter()
        .filter(|r| r.reason == CutReason::Silence && !r.review)
    {
        let len = r.end - r.start - 0.4;
        if len >= 2.0 {
            let speed = (len / 1.5).clamp(2.0, 8.0).round();
            segments.push((r.start + 0.2, r.end - 0.2, speed));
        }
    }
    segments.sort_by(|a, b| a.0.total_cmp(&b.0));
    lay_main(
        p,
        src,
        &segments.iter().map(|&(a, b, _)| (a, b)).collect::<Vec<_>>(),
    )?;
    let fast: Vec<(String, f64, f64)> = p
        .main()
        .items
        .iter()
        .filter_map(|item| {
            let clip = item.clip()?;
            let seg = segments
                .iter()
                .find(|s| (s.0 - clip.src_in).abs() < 1e-6 && s.2 > 1.0)?;
            Some((item.id.clone(), seg.2, seg.1 - seg.0))
        })
        .collect();
    for (id, speed, src_len) in fast {
        ops::apply(
            p,
            &Op::Patch {
                item: id.clone(),
                patch: serde_json::json!({ "speed": speed, "duration": src_len / speed, "muted": true }),
            },
        )?;
        let item = p.item(&id).context("sped-up clip vanished")?;
        let (start, duration) = (item.start, item.duration);
        ops::apply(
            p,
            &Op::AddText {
                text: format!("{speed}× SPEED"),
                start,
                duration,
                preset: TextPreset::Label,
                position: Some(Point::new(0.88, 0.1)),
                style: None,
                anim_in: Some(Anim::Fade),
                anim_out: Some(Anim::Fade),
                track: None,
            },
        )?;
    }
    Ok(())
}

/// 100 Hz loudness envelope in dB (floored at -70). Decibels make a microphone's quiet bleed
/// of the other speaker count almost as much as its own speaker, so two microphones that hear
/// different people loudest still line up on the shared pattern of speech and silence.
fn envelope(samples: &[f32]) -> Vec<f32> {
    let hop = (SAMPLE_RATE / 100) as usize;
    samples
        .chunks(hop)
        .map(|c| {
            (10.0 * (c.iter().map(|s| s * s).sum::<f32>() / c.len() as f32 + 1e-9).log10())
                .max(-70.0)
        })
        .collect()
}

/// Seconds to add to a time in `reference` to get the same moment in `other`, found by
/// cross-correlating loudness envelopes over the first minute (search within 10 s).
fn sync_offset(reference: &[f32], other: &[f32]) -> f64 {
    let (a, b) = (envelope(reference), envelope(other));
    let mean = |v: &[f32]| v.iter().sum::<f32>() / v.len().max(1) as f32;
    let (ma, mb) = (mean(&a), mean(&b));
    let window = a.len().min(6000);
    let mut best = (f32::MIN, 0i64);
    for lag in -1000i64..=1000 {
        let mut sum = 0.0f32;
        for i in 0..window {
            let j = i as i64 + lag;
            if j >= 0 && (j as usize) < b.len() {
                sum += (a[i] - ma) * (b[j as usize] - mb);
            }
        }
        if sum > best.0 {
            best = (sum, lag);
        }
    }
    best.1 as f64 / 100.0
}

/// Podcasts and interviews. One camera: natural cuts and gentle punch-ins. Two to four cameras
/// (each with its own microphone): cameras are synced by sound and the edit cuts to whoever is
/// talking, while the audio stays on one continuous voice track.
pub(super) fn podcast(job: &mut Job, progress: &mut dyn FnMut(&str)) -> Result<()> {
    let mut sources = job.req.sources.clone();
    if sources.is_empty() {
        sources.push(source_video(job)?);
    }
    sources.truncate(4);
    let first = media::probe(&sources[0])?;
    let name = job.req.name.clone().unwrap_or_else(|| stem(&sources[0]));
    let canvas = job.canvas(first.fps);
    let (dir, mut p) = job.start(&name, canvas)?;
    let mut cams = Vec::new();
    for s in &sources {
        cams.push(job.ws.add_asset(&mut p, &dir, s, AssetRole::Source)?);
    }
    let speech_file = job.req.voice.clone().unwrap_or_else(|| sources[0].clone());
    let multi = cams.len() > 1 && cams.iter().all(|c| p.asset(c).is_some_and(|a| a.has_audio));
    let speech = if multi || job.req.voice.is_some() {
        job.ws
            .add_asset(&mut p, &dir, &speech_file, AssetRole::Voice)?
    } else {
        cams[0].clone()
    };
    let transcript = job.transcript(&dir, &p, &speech, progress)?;
    progress("Planning cuts");
    let speech_pcm = pcm(&p, &dir, &speech)?;
    let plan = analyze::plan_cuts(&transcript, &speech_pcm, SAMPLE_RATE, &job.cut_settings());
    note_reviews(job, &plan.removed);

    if !multi {
        lay_main(&mut p, &cams[0], &plan.keep)?;
        if speech != cams[0] {
            mute_main(&mut p)?;
            lay_voice(&mut p, &speech, &plan.keep)?;
        }
        punch_ins(&mut p, &transcript, 5.0, 8.0, 12.0, 1.08);
        job.report
            .summary
            .push("One camera: natural cuts with gentle punch-ins".into());
    } else {
        progress("Syncing cameras");
        let mut offsets = Vec::new();
        let mut tracks = Vec::new();
        for cam in &cams {
            let cam_pcm = pcm(&p, &dir, cam)?;
            let off = sync_offset(&speech_pcm, &cam_pcm);
            // The camera's own microphone, aligned to the speech timeline.
            let shift = (off * SAMPLE_RATE as f64).round() as i64;
            tracks.push(
                (0..speech_pcm.len() as i64)
                    .map(|i| {
                        cam_pcm
                            .get((i + shift).max(0) as usize)
                            .copied()
                            .unwrap_or(0.0)
                    })
                    .collect::<Vec<f32>>(),
            );
            offsets.push(off);
        }
        let turns = analyze::speaker_turns(&tracks, SAMPLE_RATE, 2.0);
        let speaker_at = |t: f64| {
            turns
                .iter()
                .rev()
                .find(|(s, _)| *s <= t)
                .map(|&(_, k)| k)
                .unwrap_or(0)
        };
        let mut pieces = Vec::new();
        for &(a, b) in &plan.keep {
            let mut cuts: Vec<f64> = turns
                .iter()
                .map(|&(s, _)| s)
                .filter(|&s| s > a && s < b)
                .collect();
            cuts.insert(0, a);
            cuts.push(b);
            for w in cuts.windows(2) {
                pieces.push((w[0], w[1], speaker_at(w[0]).min(cams.len() - 1)));
            }
        }
        let ops: Vec<Op> = pieces
            .iter()
            .filter(|(a, b, _)| b - a > 0.1)
            .map(|&(a, b, k)| Op::AddClip {
                asset: cams[k].clone(),
                track: Some("main".into()),
                start: None,
                index: None,
                src_in: (a + offsets[k]).max(0.0),
                duration: Some(b - a),
                frame: None,
                volume: None,
                duck: None,
            })
            .collect();
        ops::apply_all(&mut p, &ops)?;
        mute_main(&mut p)?;
        lay_voice(&mut p, &speech, &plan.keep)?;
        job.report.summary.push(format!(
            "{} cameras synced by sound (offsets {:?} s); the edit cuts to whoever speaks",
            cams.len(),
            offsets
                .iter()
                .map(|o| (o * 100.0).round() / 100.0)
                .collect::<Vec<_>>()
        ));
    }
    // One camera: aim framing at its face. With several, each camera's face is tracked for the
    // captions, but there is no single place to aim.
    progress("Finding the faces");
    let mut face = None;
    for (k, (cam, source)) in cams.iter().zip(&sources).enumerate() {
        let duration = if k == 0 {
            first.duration
        } else {
            media::probe(source)?.duration
        };
        let found = track_face(job, &mut p, cam, source, (0.0, duration));
        if k == 0 {
            face = found;
        }
    }
    if !multi {
        set_focus(&mut p, face);
    }
    cut_summary(job, &plan.removed, first.duration, p.duration());
    place_broll(job, &dir, &mut p, &transcript)?;
    add_captions(job, &mut p, &transcript)?;
    music_bed(job, &dir, &mut p, true)?;
    text_sfx(job, &dir, &mut p)?;
    job.finish(&dir, &mut p, progress)
}

/// Points every main-track clip's framing and punch-ins at the face.
fn set_focus(p: &mut Project, face: Option<Point>) {
    let Some(f) = face else { return };
    for item in &mut p.main_mut().items {
        if let Some(c) = item.clip_mut() {
            c.focus = Some(f);
        }
    }
}

fn mute_main(p: &mut Project) -> Result<()> {
    let ops: Vec<Op> = p
        .main()
        .items
        .iter()
        .map(|i| Op::Patch {
            item: i.id.clone(),
            patch: serde_json::json!({ "muted": true }),
        })
        .collect();
    ops::apply_all(p, &ops)?;
    Ok(())
}

/// The voice as back-to-back clips on a "Voice" audio track, matching the main track's cuts.
fn lay_voice(p: &mut Project, voice: &str, keeps: &[(f64, f64)]) -> Result<()> {
    let track = audio_track(p, "voice", "Voice")?;
    let mut t = 0.0;
    for &(a, b) in keeps.iter().filter(|(a, b)| b - a > 0.1) {
        place_audio(p, &track, voice, t, a, b - a, 1.0, false)?;
        t += b - a;
    }
    Ok(())
}

/// A long recording cut into short vertical clips around its strongest self-contained moments.
pub(super) fn clips(job: &mut Job, progress: &mut dyn FnMut(&str)) -> Result<()> {
    let source = source_video(job)?;
    let info = media::probe(&source)?;
    let base = job.req.name.clone().unwrap_or_else(|| stem(&source));
    let canvas = job.canvas(info.fps);

    // Transcribe and plan once, in a cache folder shared by every clip.
    let cache = job.ws.projects().join(".sources").join(slug(&base));
    std::fs::create_dir_all(&cache)?;
    let mut scratch = Project::new(&base, VideoKind::Clips, canvas.clone());
    let src_id = job
        .ws
        .add_asset(&mut scratch, &cache, &source, AssetRole::Source)?;
    let transcript = job.transcript(&cache, &scratch, &src_id, progress)?;
    let samples = pcm(&scratch, &cache, &src_id)?;
    let plan = analyze::plan_cuts(&transcript, &samples, SAMPLE_RATE, &job.cut_settings());
    let moments = analyze::highlights(&transcript, 20.0, 60.0, job.req.clips.max(1));
    if moments.is_empty() {
        bail!(
            "found no self-contained moment between 20 and 60 seconds; is the recording long enough?"
        );
    }
    for (n, m) in moments.iter().enumerate() {
        progress(&format!(
            "Clip {} of {}: {:.0}-{:.0}s, \"{}\"",
            n + 1,
            moments.len(),
            m.start,
            m.end,
            m.title
        ));
        let (dir, mut p) = job.start(&format!("{base} clip {}", n + 1), canvas.clone())?;
        let src = job.ws.add_asset(&mut p, &dir, &source, AssetRole::Source)?;
        let mut t = transcript.clone();
        t.asset = src.clone();
        t.save(&dir.join("transcript.json"))?;
        t.words.retain(|w| w.start >= m.start && w.end <= m.end);
        let keeps: Vec<(f64, f64)> = plan
            .keep
            .iter()
            .map(|&(a, b)| (a.max(m.start), b.min(m.end)))
            .filter(|(a, b)| b - a > 0.2)
            .collect();
        lay_main(&mut p, &src, &keeps)?;
        let face = track_face(job, &mut p, &src, &source, (m.start, m.end));
        set_focus(&mut p, face);
        punch_ins(&mut p, &t, 1.2, 2.0, 2.8, 1.15);
        add_hook(job, &mut p, &t, face)?;
        job.report.summary.push(format!(
            "Clip {}: {} ({:.0} s), score {:.2}: {}",
            n + 1,
            m.title,
            m.end - m.start,
            m.score,
            m.reason
        ));
        add_captions(job, &mut p, &t)?;
        music_bed(job, &dir, &mut p, true)?;
        text_sfx(job, &dir, &mut p)?;
        job.finish(&dir, &mut p, progress)?;
    }
    Ok(())
}

fn slug(name: &str) -> String {
    let s: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    s.split('-')
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

/// Faceless explainer: narration on a voice track, one visual per sentence (images drift with a
/// slow push, videos play muted), captions as the main text, a ducked music bed.
pub(super) fn explainer(job: &mut Job, progress: &mut dyn FnMut(&str)) -> Result<()> {
    let mut visuals: Vec<PathBuf> = Vec::new();
    let mut voice = job.req.voice.clone();
    for s in &job.req.sources {
        let info = media::probe(s)?;
        if info.kind == AssetKind::Audio && voice.is_none() {
            voice = Some(s.clone());
        } else if info.kind != AssetKind::Audio {
            visuals.push(s.clone());
        }
    }
    visuals.extend(job.req.broll.iter().cloned());
    let voice =
        voice.context("an explainer needs a narration recording: pass it with --voice FILE")?;
    let name = job.req.name.clone().unwrap_or_else(|| stem(&voice));
    let canvas = job.canvas(30.0);
    let (dir, mut p) = job.start(&name, canvas)?;
    let voice_id = job.ws.add_asset(&mut p, &dir, &voice, AssetRole::Voice)?;
    let mut visual_ids = Vec::new();
    for v in &visuals {
        visual_ids.push(job.ws.add_asset(&mut p, &dir, v, AssetRole::Source)?);
    }
    let transcript = job.transcript(&dir, &p, &voice_id, progress)?;
    let samples = pcm(&p, &dir, &voice_id)?;
    let plan = analyze::plan_cuts(&transcript, &samples, SAMPLE_RATE, &job.cut_settings());
    note_reviews(job, &plan.removed);
    lay_voice(&mut p, &voice_id, &plan.keep)?;
    let total = plan
        .keep
        .iter()
        .map(|(a, b)| b - a)
        .filter(|l| *l > 0.1)
        .sum::<f64>();
    let to_timeline = |src_t: f64| -> Option<f64> {
        let mut t = 0.0;
        for &(a, b) in plan.keep.iter().filter(|(a, b)| b - a > 0.1) {
            if src_t >= a && src_t < b {
                return Some(t + src_t - a);
            }
            t += b - a;
        }
        None
    };
    // One shot per sentence, merging sentences shorter than 1.5 s into the next.
    let mut starts = vec![0.0];
    for s in transcript.sentences().iter().skip(1) {
        if let Some(t) = to_timeline(s.start) {
            if t - starts.last().copied().unwrap_or(0.0) >= 1.5 && total - t >= 1.0 {
                starts.push(t);
            }
        }
    }
    let shots: Vec<(f64, f64)> = starts
        .iter()
        .enumerate()
        .map(|(i, &s)| (s, starts.get(i + 1).copied().unwrap_or(total)))
        .collect();
    if visual_ids.is_empty() {
        // The words are the picture: big centered captions.
        p.captions.position = 0.5;
        p.captions.size = p.captions.size.max(0.06);
        if p.captions.mode == CaptionMode::Off || p.captions.mode == CaptionMode::Subtitle {
            p.captions.mode = CaptionMode::Highlight;
        }
        job.report.review.push("No images or footage were given, so the captions carry the picture. Add visuals with --broll for a richer explainer.".into());
    } else {
        let mut cursor = vec![0.0f64; visual_ids.len()];
        let mut ops = Vec::new();
        for (n, &(s, e)) in shots.iter().enumerate() {
            let k = n % visual_ids.len();
            let asset = p.asset(&visual_ids[k]).context("visual vanished")?.clone();
            let len = e - s;
            let src_in = if asset.kind == AssetKind::Video {
                if cursor[k] + len > asset.duration {
                    // ponytail: few visuals for a long narration reuse footage from the start; more visuals avoid it.
                    cursor[k] = 0.0;
                }
                let at = cursor[k];
                cursor[k] += len;
                at
            } else {
                0.0
            };
            let len = if asset.kind == AssetKind::Video {
                len.min(asset.duration - src_in)
            } else {
                len
            };
            ops.push(Op::AddClip {
                asset: asset.id.clone(),
                track: Some("main".into()),
                start: None,
                index: None,
                src_in,
                duration: Some(len),
                frame: None,
                volume: None,
                duck: None,
            });
        }
        ops::apply_all(&mut p, &ops)?;
        let ids: Vec<String> = p.main().items.iter().map(|i| i.id.clone()).collect();
        for (n, id) in ids.iter().enumerate() {
            let is_image = p
                .item(id)
                .and_then(|i| i.clip())
                .and_then(|c| p.asset(&c.asset))
                .is_some_and(|a| a.kind == AssetKind::Image);
            let patch = if is_image {
                let (from, to) = if n % 2 == 0 {
                    (Point::new(0.42, 0.5), Point::new(0.58, 0.5))
                } else {
                    (Point::new(0.58, 0.45), Point::new(0.42, 0.55))
                };
                serde_json::json!({ "ken_burns": KenBurns { from_scale: 1.0, to_scale: 1.12, from_focus: Some(from), to_focus: Some(to) } })
            } else {
                serde_json::json!({ "muted": true })
            };
            ops::apply(
                &mut p,
                &Op::Patch {
                    item: id.clone(),
                    patch,
                },
            )?;
        }
        job.report.summary.push(format!(
            "{} shots from {} visuals, one per sentence",
            ids.len(),
            visual_ids.len()
        ));
    }
    if let Some(copy) = &job.req.copy {
        // Heading cards sit at the top; captions move below them when they would collide.
        if p.captions.mode != CaptionMode::Off && p.captions.position < 0.4 {
            p.captions.position = 0.74;
            job.report
                .summary
                .push("Captions moved to the lower third, under the heading cards".into());
        }
        for (line, &(s, e)) in copy
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .zip(&shots)
        {
            ops::apply(
                &mut p,
                &Op::AddText {
                    text: line.into(),
                    start: s,
                    duration: (e - s).min(4.0),
                    preset: TextPreset::Card,
                    position: Some(Point::new(0.5, 0.18)),
                    style: None,
                    anim_in: Some(Anim::SlideUp),
                    anim_out: Some(Anim::Fade),
                    track: None,
                },
            )?;
        }
    }
    cut_summary(job, &plan.removed, transcript.duration, total);
    add_captions(job, &mut p, &transcript)?;
    music_bed(job, &dir, &mut p, true)?;
    text_sfx(job, &dir, &mut p)?;
    job.finish(&dir, &mut p, progress)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::apply;
    use crate::project::{Asset, Canvas, ItemBody};
    use crate::transcript::TranscriptWord;

    fn transcript(text: &str) -> Transcript {
        let words: Vec<_> = text
            .split_whitespace()
            .enumerate()
            .map(|(i, text)| TranscriptWord {
                text: text.into(),
                start: i as f64 * 0.4,
                end: i as f64 * 0.4 + 0.3,
                confidence: None,
            })
            .collect();
        Transcript {
            asset: "a".into(),
            provider: "test".into(),
            language: None,
            duration: words.last().map_or(0.0, |w| w.end),
            words,
        }
    }

    #[test]
    fn automatic_hook_keeps_complete_sentences_and_scores_later_candidates() {
        let t = transcript(
            "You know I got the opportunity to visit a place far away yesterday. \
             We walked home. Why do you always quit?",
        );
        let (text, end) = hook_from(&t).unwrap();
        assert_eq!(text.replace('*', ""), "Why do you always quit");
        assert_eq!(end, t.words.last().unwrap().end);

        let first = transcript("We walked home. Why do you always quit?");
        let (text, end) = hook_from(&first).unwrap();
        assert_eq!(text.replace('*', ""), "We walked home");
        assert_eq!(end, first.sentences()[0].end);

        let long_title = transcript(
            "You know I got the opportunity to visit a place far away yesterday. \
             Extraordinary discoveries change everything about interplanetary exploration.",
        );
        let (text, end) = hook_from(&long_title).unwrap();
        assert_eq!(
            text.replace('*', ""),
            "Extraordinary discoveries change everything about interplanetary exploration"
        );
        assert_eq!(end, long_title.words.last().unwrap().end);
    }

    #[test]
    fn automatic_hook_omits_long_or_unfinished_sentences_and_requests_review() {
        let ws = crate::workspace::Workspace { root: ".".into() };
        let req = super::super::EditRequest::new(VideoKind::TalkingHeadShort);
        for text in [
            "You know I got the opportunity to visit a place far away yesterday.",
            "You know I got the opportunity to",
        ] {
            let t = transcript(text);
            assert!(hook_from(&t).is_none());
            let mut job = Job {
                ws: &ws,
                req: &req,
                style: crate::style::Style::default(),
                report: super::super::EditReport::default(),
            };
            let mut p = Project::new("t", req.kind, Canvas::vertical());
            add_hook(&mut job, &mut p, &t, None).unwrap();
            assert!(p.tracks.iter().all(|track| track.items.is_empty()));
            assert_eq!(job.report.review.len(), 1);
        }
    }

    #[test]
    fn explicit_hook_is_preserved_without_automatic_review() {
        let ws = crate::workspace::Workspace { root: ".".into() };
        let mut req = super::super::EditRequest::new(VideoKind::TalkingHeadShort);
        let text = "You know I got the opportunity to choose my own *hook* unchanged";
        req.hook = Some(text.into());
        let mut job = Job {
            ws: &ws,
            req: &req,
            style: crate::style::Style::default(),
            report: super::super::EditReport::default(),
        };
        let mut p = Project::new("t", req.kind, Canvas::vertical());
        add_hook(&mut job, &mut p, &transcript("Nothing fits"), None).unwrap();
        let hooks: Vec<_> = p.tracks.iter().flat_map(|track| &track.items).collect();
        assert_eq!(hooks.len(), 1);
        let ItemBody::Text(hook) = &hooks[0].body else {
            panic!("hook must be text");
        };
        assert_eq!(hook.text, text);
        assert!(job.report.review.is_empty());
    }

    #[test]
    fn punch_ins_alternate_and_respect_max_shot() {
        let mut p = Project::new("t", VideoKind::TalkingHeadShort, Canvas::vertical());
        p.assets.push(Asset {
            id: "a".into(),
            path: "a.mp4".into(),
            kind: AssetKind::Video,
            role: AssetRole::Source,
            duration: 30.0,
            width: 1920,
            height: 1080,
            fps: 30.0,
            has_audio: true,
        });
        apply(
            &mut p,
            &Op::AddClip {
                asset: "a".into(),
                track: None,
                start: None,
                index: None,
                src_in: 0.0,
                duration: Some(12.0),
                frame: None,
                volume: None,
                duck: None,
            },
        )
        .unwrap();
        // A word every 0.4 s, a phrase boundary every 5 words (2 s).
        let words = (0..30)
            .map(|i| {
                let start = i as f64 * 0.4;
                let text = if i % 5 == 4 {
                    "word,".to_string()
                } else {
                    "word".to_string()
                };
                TranscriptWord {
                    text,
                    start,
                    end: start + 0.3,
                    confidence: None,
                }
            })
            .collect();
        let t = Transcript {
            asset: "a".into(),
            provider: "test".into(),
            language: None,
            duration: 12.0,
            words,
        };
        punch_ins(&mut p, &t, 1.2, 2.0, 2.8, 1.15);
        let zooms = &p.main().items[0].clip().unwrap().zooms;
        assert!(zooms.len() >= 2, "{zooms:?}");
        // Wide and punched shots alternate, so punched intervals never touch.
        for pair in zooms.windows(2) {
            assert!(
                pair[1].at > pair[0].at + pair[0].duration + 1.0,
                "{zooms:?}"
            );
        }
        assert!(
            zooms
                .iter()
                .all(|z| z.duration <= 2.8 + 1e-6 && z.scale == 1.15)
        );
    }

    #[test]
    fn sync_offset_finds_a_shift_despite_bleed() {
        // Two people talk in turns with irregular spurts; each microphone hears its own person
        // loud and the other at -15 dB.
        let len = SAMPLE_RATE as usize * 40;
        let (mut one, mut two) = (vec![0.0f32; len], vec![0.0f32; len]);
        let (mut t, mut seed, mut who) = (0.5f64, 7u64, 0);
        while t < 38.0 {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let talk = 0.6 + (seed >> 33) as f64 / (1u64 << 31) as f64 * 2.5;
            let target = if who == 0 { &mut one } else { &mut two };
            for i in (t * SAMPLE_RATE as f64) as usize
                ..((t + talk) * SAMPLE_RATE as f64).min(len as f64) as usize
            {
                target[i] = ((i as f32) * 0.3).sin();
            }
            t += talk + 0.3 + (seed >> 40) as f64 / (1u64 << 24) as f64 * 0.9;
            who = 1 - who;
        }
        let mic_a: Vec<f32> = one.iter().zip(&two).map(|(x, y)| x + 0.18 * y).collect();
        // Microphone B started recording 1.5 s earlier, so every moment comes 1.5 s later in it.
        let mut mic_b = vec![0.0f32; (1.5 * SAMPLE_RATE as f64) as usize];
        mic_b.extend(one.iter().zip(&two).map(|(x, y)| 0.18 * x + y));
        assert!(
            (sync_offset(&mic_a, &mic_b) - 1.5).abs() < 0.02,
            "{}",
            sync_offset(&mic_a, &mic_b)
        );
    }
}
