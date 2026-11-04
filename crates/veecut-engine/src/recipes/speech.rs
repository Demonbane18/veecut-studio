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
/// at cuts and phrase boundaries, falling back to word gaps before `max_shot` (its measured
/// reels: shots 1.1-1.4 s on average, none over about 2.8 s). Long-form uses slower numbers.
pub(super) fn punch_ins(
    project: &mut Project,
    transcript: &Transcript,
    min_shot: f64,
    target: f64,
    max_shot: f64,
    scale: f32,
) {
    let asset = transcript.asset.as_str();
    let mut points: Vec<(f64, bool, bool)> = project
        .main()
        .items
        .iter()
        .skip(1)
        .map(|i| (i.start, true, true))
        .collect();
    for pair in transcript.words.windows(2) {
        let (a, b) = (&pair[0], &pair[1]);
        let phrase_end =
            a.text.ends_with([',', '.', ';', ':', '?', '!']) || b.start - a.end >= 0.12;
        if b.start < a.end {
            continue;
        }
        let ta = project.source_to_timeline(asset, a.end);
        let tb = project.source_to_timeline(asset, b.start);
        if let (Some(ta), Some(tb)) = (ta, tb) {
            if (0.0..0.6).contains(&(tb - ta)) {
                points.push(((ta + tb) / 2.0, false, phrase_end));
            }
        }
    }
    points.sort_by(|x, y| x.0.total_cmp(&y.0));
    let end = project.duration();
    let (mut punched, mut shot_start) = (false, 0.0);
    let mut intervals = Vec::new();
    for (i, &(t, is_cut, phrase_end)) in points.iter().enumerate() {
        let len = t - shot_start;
        if len <= 0.05 {
            continue;
        }
        let next = points.get(i + 1).map(|p| p.0).unwrap_or(end);
        let change = if is_cut {
            len >= min_shot * 0.5
        } else if phrase_end {
            let next_phrase = points[i + 1..]
                .iter()
                .find(|p| p.1 || p.2)
                .map(|p| p.0)
                .unwrap_or(end);
            len >= target || (len >= min_shot && next_phrase - shot_start > max_shot)
        } else {
            len >= min_shot && next - shot_start > max_shot
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

pub(super) fn source_video(job: &Job) -> Result<PathBuf> {
    job.req
        .sources
        .first()
        .cloned()
        .or_else(|| job.ws.newest_inbox_video())
        .context("no video to edit: put a recording in inbox/ or pass its path")
}

pub(super) fn stem(path: &Path) -> String {
    path.file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .replace(['_', '.'], " ")
}

pub(super) fn note_reviews(job: &mut Job, removed: &[analyze::Removal]) {
    for r in removed.iter().filter(|r| r.review) {
        job.report.review.push(format!(
            "Possible {:?} kept at {:.2}-{:.2}s of the source: \"{}\". {} Listen to both takes before editing; remove only a confirmed repeated phrase, preserving unique speech.",
            r.reason, r.start, r.end, r.text, r.note
        ));
    }
}

pub(super) fn cut_summary(job: &mut Job, removed: &[analyze::Removal], before: f64, after: f64) {
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
    if let Some((text, _)) = picked {
        hook(project, &text, 3.0, face)?;
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
        lay_tutorial(&mut p, &src, &plan, &transcript)?;
    } else {
        lay_main(&mut p, &src, &plan.keep)?;
    }
    cut_summary(job, &plan.removed, info.duration, p.duration());

    // Framing: keep the face in a cover crop and at the center of punch-ins, and track it so
    // captions stay off the eyes, mouth and chin. Screen recordings have no face to find.
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
fn lay_tutorial(
    p: &mut Project,
    src: &str,
    plan: &analyze::CutPlan,
    transcript: &Transcript,
) -> Result<()> {
    let mut segments: Vec<(f64, f64, f64)> = plan.keep.iter().map(|&(a, b)| (a, b, 1.0)).collect();
    for r in plan
        .removed
        .iter()
        .filter(|r| r.reason == CutReason::Silence && !r.review)
    {
        // Transcript words win over an audio-only silence estimate, even in an old cut plan.
        if transcript
            .words
            .iter()
            .any(|w| w.start < r.end && w.end > r.start)
        {
            segments.push((r.start, r.end, 1.0));
            continue;
        }
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
                position: Some(Point::new(0.5, 0.92)),
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
pub(super) fn set_focus(p: &mut Project, face: Option<Point>) {
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
    let moments =
        analyze::highlights_with_cuts(&transcript, &plan.keep, 20.0, 60.0, job.req.clips.max(1));
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
        t.words.retain(|w| w.start >= m.start && w.end <= m.end);
        t.save(&dir.join("transcript.json"))?;
        let keeps: Vec<(f64, f64)> = plan
            .keep
            .iter()
            .map(|&(a, b)| (a.max(m.start), b.min(m.end)))
            .filter(|(a, b)| b > a)
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
            p.duration(),
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
    let mut first_video = None;
    for s in &job.req.sources {
        let info = media::probe(s)?;
        if info.kind == AssetKind::Audio && voice.is_none() {
            voice = Some(s.clone());
        } else if info.kind != AssetKind::Audio {
            if info.kind == AssetKind::Video && first_video.is_none() {
                first_video = Some(s.clone());
            }
            visuals.push(s.clone());
        }
    }
    if voice.is_none() {
        voice = first_video;
        if !job.req.broll.is_empty() {
            visuals.retain(|path| Some(path) != voice.as_ref());
        }
    }
    visuals.extend(job.req.broll.iter().cloned());
    let voice = voice.context(
        "an explainer needs narration: pass --voice FILE, an audio source, or a video with audio",
    )?;
    let voice_info = media::probe(&voice)?;
    if !voice_info.has_audio {
        bail!(
            "narration source {} has no audio; pass --voice FILE or an audio source",
            voice.display()
        );
    }
    // Reject bad B-roll before creating a project that could become the default.
    for visual in &visuals {
        if media::probe(visual)?.kind == AssetKind::Audio {
            bail!(
                "visual {} has no picture; pass an image or video",
                visual.display()
            );
        }
    }
    let synced_picture =
        voice_info.kind == AssetKind::Video && visuals.len() == 1 && visuals[0] == voice;
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
    explainer_picture_style(&mut p, !visual_ids.is_empty());
    if visual_ids.is_empty() {
        // The words are the picture.
        job.report.review.push("No images or footage were given, so the captions carry the picture. Add visuals with --broll for a richer explainer.".into());
    } else if synced_picture {
        lay_explainer_synced_video(&mut p, &visual_ids[0], &plan.keep, job.req.motion)?;
        job.report
            .summary
            .push("Narration video's picture follows the same source cuts as its voice".into());
        job.report
            .review
            .extend(explainer_visual_reviews(&p, &visual_ids, 1));
    } else {
        lay_explainer_visuals(&mut p, &visual_ids, &shots, job.req.motion)?;
        job.report.summary.push(format!(
            "{} sentence shots from {} visuals (short footage repeats to fill each shot)",
            shots.len(),
            visual_ids.len()
        ));
        job.report
            .review
            .extend(explainer_visual_reviews(&p, &visual_ids, shots.len()));
    }
    if let Some(copy) = &job.req.copy {
        // Heading cards sit at the top; captions move below them when they would collide.
        if p.captions.mode != CaptionMode::Off && p.captions.position < 0.4 {
            p.captions.position = 0.74;
            job.report
                .summary
                .push("Captions moved to the lower third, under the heading cards".into());
        }
        job.report
            .review
            .extend(add_explainer_headings(&mut p, &transcript, copy)?);
    }
    cut_summary(job, &plan.removed, transcript.duration, total);
    add_captions(job, &mut p, &transcript)?;
    music_bed(job, &dir, &mut p, true)?;
    text_sfx(job, &dir, &mut p)?;
    job.finish(&dir, &mut p, progress)
}

fn explainer_picture_style(p: &mut Project, has_visuals: bool) {
    if !has_visuals {
        // Clear blackdetect's 10% threshold even between caption pages.
        p.canvas.background = crate::project::Color::rgb(40, 48, 64);
        p.captions.position = 0.5;
        p.captions.size = p.captions.size.max(0.08);
        p.captions.max_words = 1;
        if matches!(p.captions.mode, CaptionMode::Off | CaptionMode::Subtitle) {
            p.captions.mode = CaptionMode::Highlight;
        }
    } else if p.canvas.width > p.canvas.height && p.captions.position < 0.4 {
        p.captions.position = 0.8;
    }
}

fn explainer_visual_reviews(p: &Project, visuals: &[String], shots: usize) -> Vec<String> {
    let mut reviews = Vec::new();
    let reused = shots.saturating_sub(visuals.len());
    if reused > 0 {
        reviews.push(format!(
            "{reused} shots reuse earlier visuals in the supplied order, without matching their subject to the narration. Add {reused} more visuals in story order or replace those shots."
        ));
    }
    // One line for all heavy crops: a vertical edit of landscape pictures crops every one of them.
    let canvas = p.canvas.width as f64 / p.canvas.height as f64;
    let cropped: Vec<String> = visuals
        .iter()
        .filter_map(|id| p.asset(id))
        .filter_map(|asset| {
            let source = asset.width.max(1) as f64 / asset.height.max(1) as f64;
            let kept = (source / canvas).min(canvas / source);
            (kept < 0.7).then(|| {
                let name = asset.path.file_name().unwrap_or(asset.path.as_os_str());
                format!("{} ({:.0}%)", name.to_string_lossy(), (1.0 - kept) * 100.0)
            })
        })
        .collect();
    if !cropped.is_empty() {
        reviews.push(format!(
            "{} of {} visuals lose over 30% of the picture to the cover crop: {}. Check that subjects and baked-in labels stay in frame; patch a clip with {{\"fit\":\"contain\",\"ken_burns\":null}} to show its whole picture with bars.",
            cropped.len(),
            visuals.len(),
            cropped.join(", ")
        ));
    }
    reviews
}

fn add_explainer_headings(
    p: &mut Project,
    transcript: &Transcript,
    copy: &str,
) -> Result<Vec<String>> {
    let mut headings = Vec::new();
    let mut reviews = Vec::new();
    let placed = transcript.placed_words(p);
    let mut after = -1.0;
    for line in copy.lines().map(str::trim).filter(|line| !line.is_empty()) {
        let exact = transcript
            .find_phrase(p, line)
            .ok()
            .and_then(|matches| matches.into_iter().find(|m| m.timeline_start > after))
            .map(|m| m.timeline_start);
        // ponytail: literal topic words only; unmatched paraphrases need a manual heading op.
        let words: Vec<_> = line
            .split_whitespace()
            .filter_map(explainer_topic_word)
            .collect();
        let matched = exact.or_else(|| {
            if words.is_empty() {
                return None;
            }
            transcript.sentences().iter().find_map(|sentence| {
                let spoken: Vec<_> = placed
                    .iter()
                    .filter(|word| {
                        word.source_start >= sentence.start
                            && word.source_start <= sentence.end
                            && word.timeline_start > after
                    })
                    .filter_map(|word| {
                        Some((explainer_topic_word(&word.text)?, word.timeline_start))
                    })
                    .collect();
                if words
                    .iter()
                    .all(|word| spoken.iter().any(|(text, _)| text == word))
                {
                    spoken
                        .iter()
                        .find(|(word, _)| words.contains(word))
                        .map(|(_, at)| *at)
                } else {
                    None
                }
            })
        });
        if let Some(start) = matched {
            headings.push((line, start));
            after = start;
        } else {
            reviews.push(format!(
                "Heading {line:?} did not match a spoken topic in order, so it was not placed. Add it at its intended beat with add_text."
            ));
        }
    }
    for (index, &(line, start)) in headings.iter().enumerate() {
        let end = headings.get(index + 1).map_or(p.duration(), |(_, at)| *at);
        ops::apply(
            p,
            &Op::AddText {
                text: line.into(),
                start,
                duration: (end - start).min(4.0),
                preset: TextPreset::Card,
                position: Some(Point::new(0.5, 0.18)),
                style: None,
                anim_in: Some(Anim::SlideUp),
                anim_out: Some(Anim::Fade),
                track: None,
            },
        )?;
    }
    Ok(reviews)
}

fn explainer_topic_word(text: &str) -> Option<String> {
    let word = text
        .trim_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase();
    (!word.is_empty()
        && !matches!(
            word.as_str(),
            "a" | "an"
                | "the"
                | "our"
                | "of"
                | "in"
                | "on"
                | "to"
                | "is"
                | "are"
                | "and"
                | "or"
                | "like"
        ))
    .then_some(word)
}

fn lay_explainer_synced_video(
    p: &mut Project,
    visual: &str,
    keep: &[(f64, f64)],
    motion: Option<super::ExplainerMotion>,
) -> Result<()> {
    let mut cursor = 0.0;
    let shots: Vec<_> = keep
        .iter()
        .filter(|(start, end)| end - start > 0.1)
        .map(|(start, end)| {
            let at = cursor;
            cursor += end - start;
            (at, cursor)
        })
        .collect();
    lay_explainer_visuals(p, &[visual.into()], &shots, motion)?;
    let ids: Vec<_> = p.main().items.iter().map(|item| item.id.clone()).collect();
    for (id, &(source_start, _)) in ids
        .into_iter()
        .zip(keep.iter().filter(|(start, end)| end - start > 0.1))
    {
        ops::apply(
            p,
            &Op::Patch {
                item: id,
                patch: serde_json::json!({ "src_in": source_start }),
            },
        )?;
    }
    Ok(())
}

/// Fill every sentence interval before appending the next magnetic main-track shot.
fn lay_explainer_visuals(
    p: &mut Project,
    visuals: &[String],
    shots: &[(f64, f64)],
    motion: Option<super::ExplainerMotion>,
) -> Result<()> {
    use super::ExplainerMotion;

    if visuals.is_empty() {
        bail!("explainer visuals cannot be empty");
    }
    let mut cursor = vec![0.0; visuals.len()];
    let mut edits = Vec::new();
    let mut motions = Vec::new();
    for (n, &(start, end)) in shots.iter().enumerate() {
        let k = n % visuals.len();
        let asset = p.asset(&visuals[k]).context("visual vanished")?;
        let video = asset.kind == AssetKind::Video;
        if video && (!asset.duration.is_finite() || asset.duration <= 1e-6) {
            bail!(
                "visual {} has no usable video duration",
                asset.path.display()
            );
        }
        let duration = end - start;
        if !duration.is_finite() || duration <= 1e-6 {
            bail!("explainer shot must have a positive finite duration");
        }
        let camera = match motion {
            Some(ExplainerMotion::Hold) => None,
            None if video => None,
            mode => {
                // At 1x the decoded frame has no pan room, even when cover crops the source.
                // Reserve room first, then express its focus offsets in source coordinates.
                let push = mode == Some(ExplainerMotion::Push);
                let from_scale = if push { 1.0 } else { 1.06 };
                let room = (1.0 - 1.0 / from_scale) * 0.4;
                let source = asset.width.max(1) as f32 / asset.height.max(1) as f32;
                let canvas = p.canvas.width as f32 / p.canvas.height as f32;
                let dx = room * (canvas / source).min(1.0);
                let dy = room * (source / canvas).min(1.0);
                let direction = if n % 2 == 0 { 1.0 } else { -1.0 };
                let from = Point::new(0.5 - direction * dx, 0.5 - direction * dy);
                let to = Point::new(0.5 + direction * dx, 0.5 + direction * dy);
                Some(KenBurns {
                    from_scale,
                    to_scale: 1.12,
                    from_focus: Some(from),
                    to_focus: Some(to),
                    smooth: mode.is_some(),
                    phase: [0.0, 1.0],
                })
            }
        };
        if video && cursor[k] + duration > asset.duration {
            cursor[k] = 0.0;
        }
        // Bound allocation even for corrupt metadata claiming microsecond-long footage.
        let segments = if video {
            (duration / asset.duration).ceil()
        } else {
            1.0
        };
        if segments > 10_000.0 {
            bail!(
                "visual {} is too short for this shot; use a longer video or a still",
                asset.path.display()
            );
        }
        let mut elapsed = 0.0;
        for segment in 0..segments as usize {
            // Equal segments avoid an unusably tiny final clip near a whole-loop boundary.
            let len = if segment + 1 == segments as usize {
                duration - elapsed
            } else {
                duration / segments
            };
            if segments > 1.0 {
                cursor[k] = 0.0;
            }
            edits.push(Op::AddClip {
                asset: asset.id.clone(),
                track: Some("main".into()),
                start: None,
                index: None,
                src_in: cursor[k],
                duration: Some(len),
                frame: None,
                volume: Some(0.0),
                duck: None,
            });
            let mut segment_camera = camera.clone();
            if let Some(camera) = segment_camera.as_mut() {
                camera.phase = [
                    (elapsed / duration).clamp(0.0, 1.0) as f32,
                    ((elapsed + len) / duration).clamp(0.0, 1.0) as f32,
                ];
            }
            motions.push(segment_camera);
            elapsed += len;
            if video {
                cursor[k] += len;
                if cursor[k] >= asset.duration - 1e-6 {
                    cursor[k] = 0.0;
                }
            }
        }
    }
    let added = ops::apply_all(p, &edits)?;
    for (id, camera) in added.iter().flat_map(|op| &op.items).zip(motions) {
        ops::apply(
            p,
            &Op::Patch {
                item: id.clone(),
                patch: serde_json::json!({ "muted": true, "ken_burns": camera }),
            },
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::apply;
    use crate::project::{Asset, Canvas, ItemBody};
    use crate::transcript::TranscriptWord;

    fn explainer_project(video_duration: f64) -> Project {
        let mut p = Project::new("explainer", VideoKind::Explainer, Canvas::vertical());
        for (id, kind, duration) in [
            ("short", AssetKind::Video, video_duration),
            ("still", AssetKind::Image, 0.0),
            ("voice", AssetKind::Audio, 10.0),
        ] {
            p.assets.push(Asset {
                id: id.into(),
                path: format!("{id}.test").into(),
                kind,
                role: if id == "voice" {
                    AssetRole::Voice
                } else {
                    AssetRole::Source
                },
                duration,
                width: if kind == AssetKind::Audio { 0 } else { 640 },
                height: if kind == AssetKind::Audio { 0 } else { 360 },
                fps: 30.0,
                has_audio: kind != AssetKind::Image,
            });
        }
        p
    }

    #[test]
    fn short_explainer_video_keeps_sentence_boundaries_and_full_voice() {
        use super::super::ExplainerMotion;
        for motion in [
            None,
            Some(ExplainerMotion::Hold),
            Some(ExplainerMotion::Push),
            Some(ExplainerMotion::Glide),
        ] {
            let mut p = explainer_project(1.0);
            lay_voice(&mut p, "voice", &[(0.0, 10.0)]).unwrap();
            lay_explainer_visuals(
                &mut p,
                &["short".into(), "still".into()],
                &[(0.0, 2.9), (2.9, 6.12), (6.12, 10.0)],
                motion,
            )
            .unwrap();
            p.validate().unwrap();
            let main = &p.main().items;
            let still = main
                .iter()
                .find(|i| i.clip().unwrap().asset == "still")
                .unwrap();
            assert!((still.start - 2.9).abs() < 1e-9);
            assert!((still.end() - 6.12).abs() < 1e-9);
            assert!((main.last().unwrap().end() - 10.0).abs() < 1e-9);
            let voice = p
                .tracks
                .iter()
                .flat_map(|t| &t.items)
                .find(|i| i.clip().is_some_and(|c| c.asset == "voice"))
                .unwrap();
            assert_eq!(voice.end(), 10.0);
            for pair in main.windows(2) {
                assert!((pair[0].end() - pair[1].start).abs() < 1e-9);
                let (a, b) = (pair[0].clip().unwrap(), pair[1].clip().unwrap());
                if a.asset == b.asset {
                    if let (Some(a), Some(b)) = (&a.ken_burns, &b.ken_burns) {
                        assert_eq!(a.phase[1], b.phase[0], "camera must not reset on repeat");
                    }
                }
            }
            for item in main {
                let clip = item.clip().unwrap();
                assert!(clip.muted);
                assert_eq!(clip.volume, 0.0);
                assert_eq!(
                    clip.ken_burns.is_some(),
                    match motion {
                        Some(ExplainerMotion::Hold) => false,
                        None => clip.asset == "still",
                        _ => true,
                    }
                );
                if let Some(camera) = &clip.ken_burns {
                    assert_eq!(camera.smooth, motion.is_some());
                    if motion == Some(ExplainerMotion::Push) {
                        assert_eq!(camera.from_focus, Some(Point::new(0.5, 0.5)));
                        assert_eq!(camera.to_focus, camera.from_focus);
                    }
                }
            }
        }
    }

    #[test]
    fn explainer_rejects_unusable_visuals_and_handles_near_whole_repeats() {
        for duration in [0.0, 1e-7] {
            let mut p = explainer_project(duration);
            let error = lay_explainer_visuals(&mut p, &["short".into()], &[(0.0, 3.0)], None)
                .unwrap_err()
                .to_string();
            assert!(error.contains("no usable video duration"), "{error}");
            assert!(p.main().items.is_empty());
        }
        let mut p = explainer_project(1.0);
        lay_explainer_visuals(&mut p, &["short".into()], &[(0.0, 2.0000001)], None).unwrap();
        assert!((p.main().items.last().unwrap().end() - 2.0000001).abs() < 1e-9);
    }

    #[test]
    fn explainer_glide_never_clamps_or_lurches_on_vertical_canvas() {
        use super::super::ExplainerMotion;
        use crate::project::Rect;

        for (width, height) in [(1920, 1080), (1080, 1920), (900, 1800)] {
            for shot in 0..2 {
                let mut p = explainer_project(1.0);
                p.assets[1].width = width;
                p.assets[1].height = height;
                let shots = vec![(0.0, 10.0); shot + 1];
                lay_explainer_visuals(
                    &mut p,
                    &["still".into()],
                    &shots,
                    Some(ExplainerMotion::Glide),
                )
                .unwrap();
                let clip = p.main().items[shot].clip().unwrap();
                let camera = clip.ken_burns.as_ref().unwrap();
                let mut previous: Option<(f32, f32)> = None;
                let mut previous_step: Option<(f32, f32)> = None;
                for frame in 0..=300 {
                    let t = frame as f64 / 30.0;
                    let transform = crate::render::canvas_rect(
                        clip,
                        (width, height),
                        (1080, 1920),
                        t,
                        10.0,
                        Rect::FULL,
                    );
                    let phase = camera.progress(t / 10.0);
                    let from = camera.from_focus.unwrap();
                    let to = camera.to_focus.unwrap();
                    let focus = Point::new(
                        from.x + (to.x - from.x) * phase,
                        from.y + (to.y - from.y) * phase,
                    );
                    // A free (unclamped) zoom window puts its requested focus at canvas center.
                    assert!((transform.x + focus.x * transform.w - 0.5).abs() < 1e-5);
                    assert!((transform.y + focus.y * transform.h - 0.5).abs() < 1e-5);
                    let center = (
                        (transform.x + 0.5 * transform.w) * 1080.0,
                        (transform.y + 0.5 * transform.h) * 1920.0,
                    );
                    if let Some((x, y)) = previous {
                        let step = (center.0 - x, center.1 - y);
                        if let Some((dx, dy)) = previous_step {
                            assert!((step.0 - dx).abs() < 0.1, "horizontal lurch at {frame}");
                            assert!((step.1 - dy).abs() < 0.1, "vertical lurch at {frame}");
                        }
                        previous_step = Some(step);
                    }
                    previous = Some(center);
                }
                assert_ne!(camera.from_focus, camera.to_focus);
            }
        }
    }

    #[test]
    fn explainer_headings_follow_spoken_topics_and_report_unmatched_lines() {
        let mut p = explainer_project(1.0);
        let mut t = transcript(
            "Our solar system has many rocks. Another sentence comes first. \
             This force is called the Yarkovsky effect. More follows.",
        );
        t.asset = "voice".into();
        lay_voice(&mut p, "voice", &[(0.0, 10.0)]).unwrap();
        let reviews = add_explainer_headings(
            &mut p,
            &t,
            "Our solar system\nThe Yarkovsky effect\nUnspoken topic",
        )
        .unwrap();
        let headings: Vec<_> = p
            .tracks
            .iter()
            .flat_map(|track| &track.items)
            .filter(|item| matches!(&item.body, ItemBody::Text(_)))
            .collect();
        assert_eq!(headings.len(), 2);
        assert_eq!(headings[0].start, 0.0);
        let spoken = t
            .words
            .iter()
            .find(|word| word.text == "the")
            .unwrap()
            .start;
        assert!((headings[1].start - spoken).abs() < 1e-9);
        assert!(headings[0].end() <= headings[1].start);
        assert_eq!(reviews.len(), 1);
        assert!(reviews[0].contains("Unspoken topic"));
    }

    #[test]
    fn explainer_topic_heading_matches_kept_voice_after_source_cuts() {
        let mut p = explainer_project(1.0);
        let mut t = transcript("Discard this. Asteroids such as Bennu are rocky.");
        t.asset = "voice".into();
        lay_voice(&mut p, "voice", &[(0.8, 8.0)]).unwrap();
        let reviews = add_explainer_headings(&mut p, &t, "Asteroids and Bennu").unwrap();
        assert!(reviews.is_empty());
        let heading = p
            .tracks
            .iter()
            .flat_map(|track| &track.items)
            .find(|item| matches!(&item.body, ItemBody::Text(_)))
            .unwrap();
        assert_eq!(heading.start, 0.0);
    }

    #[test]
    fn explainer_narration_video_picture_follows_every_voice_cut() {
        let mut p = explainer_project(30.0);
        let keep = [(5.4, 8.4), (15.0, 19.0), (22.0, 26.0)];
        lay_voice(&mut p, "short", &keep).unwrap();
        lay_explainer_synced_video(&mut p, "short", &keep, None).unwrap();
        let voice = p.tracks.iter().find(|track| track.name == "Voice").unwrap();
        assert_eq!(p.main().items.len(), voice.items.len());
        for (picture, voice) in p.main().items.iter().zip(&voice.items) {
            assert_eq!(picture.start, voice.start);
            assert_eq!(picture.duration, voice.duration);
            assert_eq!(picture.clip().unwrap().src_in, voice.clip().unwrap().src_in);
            assert!(picture.clip().unwrap().muted);
        }
        p.validate().unwrap();
    }

    #[test]
    fn explainer_caption_picture_uses_nonblack_background_and_large_centered_words() {
        let mut p = explainer_project(1.0);
        p.captions.mode = CaptionMode::Off;
        explainer_picture_style(&mut p, false);
        let bg = p.canvas.background;
        let luma = 0.2126 * bg.r as f64 + 0.7152 * bg.g as f64 + 0.0722 * bg.b as f64;
        assert!(
            luma > 255.0 * 0.1,
            "background must clear QA's black threshold"
        );
        assert_eq!(p.captions.position, 0.5);
        assert!(p.captions.size >= 0.08);
        assert_eq!(p.captions.max_words, 1);
        assert_eq!(p.captions.mode, CaptionMode::Highlight);
    }

    #[test]
    fn explainer_landscape_captions_default_below_picture_subject() {
        let mut p = explainer_project(1.0);
        p.canvas = Canvas::landscape();
        p.captions.position = 0.25;
        explainer_picture_style(&mut p, true);
        assert_eq!(p.captions.position, 0.8);
        p.captions.position = 0.65;
        explainer_picture_style(&mut p, true);
        assert_eq!(
            p.captions.position, 0.65,
            "keep already safe style position"
        );
    }

    #[test]
    fn explainer_reviews_count_reused_shots() {
        let p = explainer_project(1.0);
        let reviews = explainer_visual_reviews(&p, &["still".into()], 4);
        assert!(reviews.iter().any(|note| note.contains("3 shots reuse")));
        let reviews = explainer_visual_reviews(&p, &["still".into()], 1);
        assert!(reviews.iter().all(|note| !note.contains("reuse")));
    }

    #[test]
    fn explainer_reviews_warn_about_heavy_crop_and_offer_native_contain() {
        let mut p = explainer_project(1.0);
        let reviews = explainer_visual_reviews(&p, &["still".into()], 1);
        assert!(
            reviews
                .iter()
                .any(|note| note.contains("\"fit\":\"contain\""))
        );
        p.assets[1].width = p.canvas.width;
        p.assets[1].height = p.canvas.height;
        assert!(explainer_visual_reviews(&p, &["still".into()], 1).is_empty());
    }

    #[test]
    fn explainer_invalid_broll_does_not_create_project() {
        let dir = tempfile::tempdir().unwrap();
        let ws = crate::workspace::Workspace {
            root: dir.path().into(),
        };
        let voice = dir.path().join("voice.wav");
        // A one-second silent PCM WAV: no render, model, network or fixture media.
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&32036u32.to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&16000u32.to_le_bytes());
        wav.extend_from_slice(&32000u32.to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&32000u32.to_le_bytes());
        wav.resize(32044, 0);
        std::fs::write(&voice, wav).unwrap();
        let bad = dir.path().join("headings.txt");
        std::fs::write(&bad, "Not media").unwrap();
        let mut req = super::super::EditRequest::new(VideoKind::Explainer);
        req.voice = Some(voice);
        req.broll.push(bad);
        let mut job = Job {
            ws: &ws,
            req: &req,
            style: crate::style::Style::default(),
            report: super::super::EditReport::default(),
        };
        assert!(explainer(&mut job, &mut |_| {}).is_err());
        assert!(!dir.path().join("projects").exists());
    }

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

    fn speech_project() -> Project {
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
        p
    }

    #[test]
    fn clips_keep_twenty_seconds_after_pause_cuts_and_short_keeps() {
        let mut t = transcript(&"Creators can learn. ".repeat(67));
        for word in &mut t.words {
            word.end = word.start + 0.15;
        }
        t.duration = t.words.last().unwrap().end;
        let keep: Vec<_> = t.words.iter().map(|w| (w.start, w.end)).collect();
        let moments = analyze::highlights_with_cuts(&t, &keep, 20.0, 60.0, 1);
        assert_eq!(moments.len(), 1);
        let m = &moments[0];
        let ranges: Vec<_> = keep
            .iter()
            .map(|&(a, b)| (a.max(m.start), b.min(m.end)))
            .filter(|(a, b)| b > a)
            .collect();
        let mut p = speech_project();
        p.main_mut().items.clear();
        p.assets[0].duration = t.duration;
        lay_main(&mut p, "a", &ranges).unwrap();
        assert!((20.0..=60.0).contains(&p.duration()), "{}", p.duration());
        assert!(p.main().items.iter().all(|i| i.duration < 0.2));
    }

    #[test]
    fn tutorial_restores_mislabeled_speech_and_speeds_only_silence() {
        let mut p = speech_project();
        p.main_mut().items.clear();
        let mut t = transcript("Quiet narration.");
        for (i, word) in t.words.iter_mut().enumerate() {
            word.start = 2.0 + i as f64;
            word.end = word.start + 0.4;
        }
        let plan = analyze::CutPlan {
            keep: vec![(0.0, 1.0), (5.0, 6.0), (10.0, 12.0)],
            removed: [(1.0, 5.0), (6.0, 10.0)]
                .into_iter()
                .map(|(start, end)| analyze::Removal {
                    start,
                    end,
                    reason: CutReason::Silence,
                    text: String::new(),
                    review: false,
                    note: String::new(),
                })
                .collect(),
        };
        lay_tutorial(&mut p, "a", &plan, &t).unwrap();
        for word in &t.words {
            assert!(
                p.main().items.iter().any(|item| {
                    let c = item.clip().unwrap();
                    c.src_in <= word.start
                        && c.src_out(item.duration) >= word.end
                        && c.speed == 1.0
                        && !c.muted
                }),
                "{word:?}"
            );
        }
        let fast: Vec<_> = p
            .main()
            .items
            .iter()
            .filter(|i| i.clip().unwrap().speed > 1.0)
            .collect();
        assert_eq!(fast.len(), 1);
        let c = fast[0].clip().unwrap();
        assert!(c.src_in >= 6.0 && c.src_out(fast[0].duration) <= 10.0 && c.muted);
        let label = p
            .tracks
            .iter()
            .flat_map(|t| &t.items)
            .find_map(|i| {
                if let ItemBody::Text(text) = &i.body {
                    Some(text)
                } else {
                    None
                }
            })
            .unwrap();
        assert_eq!(label.position, Some(Point::new(0.5, 0.92)));
    }

    #[test]
    fn short_automatic_hook_holds_for_three_seconds() {
        let ws = crate::workspace::Workspace { root: ".".into() };
        let req = super::super::EditRequest::new(VideoKind::TalkingHeadShort);
        let mut job = Job {
            ws: &ws,
            req: &req,
            style: crate::style::Style::default(),
            report: super::super::EditReport::default(),
        };
        let mut p = speech_project();
        add_hook(&mut job, &mut p, &transcript("Kumusta ka."), None).unwrap();
        let hook = p
            .tracks
            .iter()
            .flat_map(|t| &t.items)
            .find(|i| matches!(i.body, ItemBody::Text(_)))
            .unwrap();
        assert_eq!(hook.start, 0.0);
        assert_eq!(hook.duration, 3.0);
    }

    #[test]
    fn mistake_reviews_require_listening_and_protect_unique_speech() {
        let ws = crate::workspace::Workspace { root: ".".into() };
        let req = super::super::EditRequest::new(VideoKind::TalkingHeadLong);
        let mut job = Job {
            ws: &ws,
            req: &req,
            style: crate::style::Style::default(),
            report: super::super::EditReport::default(),
        };
        note_reviews(
            &mut job,
            &[analyze::Removal {
                start: 75.0,
                end: 78.0,
                reason: CutReason::Retake,
                text: "So the surface was unexpected.".into(),
                review: true,
                note: "Opening repeats later with a different continuation".into(),
            }],
        );
        let review = &job.report.review[0];
        assert!(review.contains("Listen"));
        assert!(review.contains("unique speech"));
        assert!(review.contains("Opening repeats later with a different continuation"));
        assert!(!review.contains("If it is a mistake, cut it"));
    }

    #[test]
    fn punch_ins_use_word_gaps_and_bound_wide_and_punched_shots() {
        let p = speech_project();
        // Dense narration without punctuation or a 0.12 s phrase gap.
        let words = (0..30)
            .map(|i| {
                let start = i as f64 * 0.4;
                TranscriptWord {
                    text: "word".into(),
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
        for punctuated in [false, true] {
            let mut p = p.clone();
            let mut t = t.clone();
            if punctuated {
                for word in t.words.iter_mut().skip(4).step_by(5) {
                    word.text.push(',');
                }
            }
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
            let mut cursor = 0.0;
            for zoom in zooms {
                assert!(zoom.at - cursor <= 2.8 + 1e-6, "{zooms:?}");
                for boundary in [zoom.at, zoom.at + zoom.duration] {
                    assert!(
                        !t.words
                            .iter()
                            .any(|w| w.start < boundary && boundary < w.end),
                        "switch inside a word at {boundary}"
                    );
                }
                cursor = zoom.at + zoom.duration;
            }
            assert!(p.duration() - cursor <= 2.8 + 1e-6, "{zooms:?}");
        }
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
