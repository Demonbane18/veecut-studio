//! Recipes cut to music: vlog and B-roll montages, product promos, music and lyric videos.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use super::documentary::{black_at, past_opening};
use super::{Job, SAMPLE_RATE, add_captions, audio_files, audio_track, pcm, place_audio, text_sfx};
use crate::analyze;
use crate::media;
use crate::ops::{self, Op};
use crate::project::{
    Anim, AssetKind, AssetRole, Caption, Item, ItemBody, KenBurns, Point, Project, TextPreset,
    TrackKind, Transition, TransitionKind, VideoKind, Word,
};

/// The beat grid of the music, or a steady 120 BPM grid when no beat is found.
fn grid(beats: &analyze::Beats, length: f64) -> (Vec<f64>, f64) {
    if beats.beats.len() >= 8 && beats.tempo > 0.0 {
        return (beats.beats.clone(), 60.0 / beats.tempo);
    }
    let step = 0.5;
    (
        (0..((length / step) as usize))
            .map(|i| i as f64 * step)
            .collect(),
        step,
    )
}

/// The number of beats per shot closest to `target` seconds.
fn beats_per_shot(spb: f64, target: f64, choices: &[usize]) -> usize {
    *choices
        .iter()
        .min_by(|a, b| {
            ((**a as f64 * spb) - target)
                .abs()
                .total_cmp(&((**b as f64 * spb) - target).abs())
        })
        .expect("choices are not empty")
}

/// Crossfade between vlog shots, in seconds.
const CROSSFADE: f64 = 0.3;
/// Silence between one promo line leaving and the next arriving, in seconds.
const LINE_GAP: f64 = 0.05;

/// Where to start looking for a `len`-second shot in a video of `duration` seconds. A video
/// not yet used opens past its opening, but never so late that the shot cannot fit: a short
/// phone clip would otherwise be skipped for a shot it could hold.
fn search_from(cursor: f64, len: f64, duration: f64, fresh: bool) -> f64 {
    if fresh {
        cursor.min((duration - 0.05 - len).max(0.0))
    } else {
        cursor
    }
}

/// Where a `len`-second shot can start at or after `from` in a video of `duration` seconds
/// without a black frame at its start, middle or end; None when the rest of the video has none.
/// Fades and slates are black, and a black beat looks like a fault and fails the render check.
fn lit_run(path: &Path, from: f64, len: f64, duration: f64) -> Option<f64> {
    let mut at = from;
    // ponytail: probes every half second with up to three frame grabs; a faster scan if montages get slow
    while at + len <= duration - 0.05 + 1e-9 {
        if [
            0.25f64.min(len / 2.0),
            len / 2.0,
            (len - 0.25).max(len / 2.0),
        ]
        .iter()
        .all(|d| !black_at(path, at + d))
        {
            return Some(at);
        }
        at += 0.5;
    }
    None
}

/// Start and length of each promo line: line `i` starts on beat `8 i` and is gone `LINE_GAP`
/// before the next one arrives (the last one holds to `total`). The beat grid can be tighter
/// than the nominal tempo, so the next start, not `8 * spb`, ends a line.
fn promo_spans(grid: &[f64], spb: f64, n: usize, total: f64) -> Vec<(f64, f64)> {
    let starts: Vec<f64> = (0..n)
        .map(|i| {
            grid.get(i * 8)
                .copied()
                .unwrap_or(i as f64 * 8.0 * spb)
                .min(total - 0.5 * (n - i) as f64)
        })
        .collect();
    (0..n)
        .map(|i| {
            let end = starts.get(i + 1).map_or(total, |s| s - LINE_GAP);
            (starts[i], end - starts[i])
        })
        .collect()
}

/// Splits `[0, length)` into shots that start and end on beats, `per_shot` beats long, starting
/// at the first downbeat when there is one.
fn shot_bounds(beats: &[f64], downbeats: &[f64], per_shot: usize, length: f64) -> Vec<(f64, f64)> {
    let first = downbeats
        .first()
        .copied()
        .filter(|d| *d < 4.0)
        .unwrap_or(0.0);
    let mut marks: Vec<f64> = vec![0.0];
    let from = beats.iter().position(|b| *b >= first - 1e-3).unwrap_or(0);
    marks.extend(
        beats[from..]
            .iter()
            .step_by(per_shot.max(1))
            .copied()
            .filter(|b| *b > 0.3 && *b < length - 0.3),
    );
    marks.push(length);
    marks.dedup_by(|a, b| (*a - *b).abs() < 0.3);
    marks.windows(2).map(|w| (w[0], w[1])).collect()
}

/// Parses LRC lyrics (`[mm:ss.xx] line`) into (time, text) pairs; None when the text has no timestamps.
fn parse_lrc(copy: &str) -> Option<Vec<(f64, String)>> {
    let re = regex::Regex::new(r"^\[(\d+):(\d+(?:\.\d+)?)\]\s*(.*)$").expect("valid regex");
    let lines: Vec<(f64, String)> = copy
        .lines()
        .filter_map(|l| {
            let c = re.captures(l.trim())?;
            let t = c[1].parse::<f64>().ok()? * 60.0 + c[2].parse::<f64>().ok()?;
            Some((t, c[3].trim().to_string()))
        })
        .collect();
    (!lines.is_empty()).then_some(lines)
}

/// Lyric lines as caption pages, words spread evenly across each line.
fn lyric_captions(p: &mut Project, lines: &[(f64, String)], length: f64) -> usize {
    let Some(track) = p.tracks.iter().position(|t| t.kind == TrackKind::Captions) else {
        return 0;
    };
    let mut items = Vec::new();
    for (i, (t, text)) in lines.iter().enumerate() {
        let end = lines
            .get(i + 1)
            .map(|l| l.0)
            .unwrap_or(length)
            .min(t + 6.0)
            .min(length);
        let words: Vec<&str> = text.split_whitespace().collect();
        if words.is_empty() || end - t < 0.3 {
            continue;
        }
        let step = (end - t) / words.len() as f64;
        let words = words
            .iter()
            .enumerate()
            .map(|(k, w)| Word {
                text: (*w).into(),
                start: k as f64 * step,
                end: (k as f64 + 0.9) * step,
                emphasis: false,
            })
            .collect();
        items.push((*t, end - t, words));
    }
    let count = items.len();
    p.tracks[track].items.clear();
    for (n, (start, duration, words)) in items.into_iter().enumerate() {
        p.tracks[track].items.push(Item {
            id: format!("cap-{}", n + 1),
            start,
            duration,
            body: ItemBody::Caption(Caption { words }),
        });
    }
    count
}

pub(super) fn edit(job: &mut Job, progress: &mut dyn FnMut(&str)) -> Result<()> {
    let kind = job.req.kind;
    let mut visuals: Vec<PathBuf> = Vec::new();
    let mut music = job.req.music.clone();
    for s in &job.req.sources {
        if media::probe(s)?.kind == AssetKind::Audio {
            music.get_or_insert_with(|| s.clone());
        } else {
            visuals.push(s.clone());
        }
    }
    visuals.extend(job.req.broll.iter().cloned());
    let music = music
        .or_else(|| {
            audio_files(&job.ws.root.join("my-sounds/music"))
                .into_iter()
                .next()
        })
        .context(
            "this recipe cuts to music: pass --music FILE or put a track in my-sounds/music",
        )?;
    if visuals.is_empty() && kind != VideoKind::Promo {
        bail!(
            "a {} needs footage or images: pass them as files or with --broll",
            kind.label()
        );
    }
    let lines: Vec<String> = job
        .req
        .copy
        .as_deref()
        .unwrap_or("")
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect();
    if kind == VideoKind::Promo && lines.is_empty() {
        bail!(
            "a promo needs its lines of copy: pass them with --copy \"Line one\\nLine two\" or a text file"
        );
    }
    let name = job.req.name.clone().unwrap_or_else(|| job.name());
    let canvas = job.canvas(30.0);
    let (dir, mut p) = job.start(&name, canvas)?;
    let music_id = job.ws.add_asset(&mut p, &dir, &music, AssetRole::Music)?;
    let mut visual_ids = Vec::new();
    for v in &visuals {
        visual_ids.push(job.ws.add_asset(&mut p, &dir, v, AssetRole::Broll)?);
    }
    progress("Finding the beat");
    let samples = pcm(&p, &dir, &music_id)?;
    let beats = analyze::beats(&samples, SAMPLE_RATE);
    let music_len = p.asset(&music_id).map(|a| a.duration).unwrap_or(0.0);
    let (grid, spb) = grid(&beats, music_len);
    let tempo = 60.0 / spb;

    let length = match kind {
        // Each line holds two bars, plus a bar for the end card to land.
        VideoKind::Promo => grid
            .get(lines.len() * 8 + 4)
            .copied()
            .unwrap_or((lines.len() as f64 * 8.0 + 4.0) * spb),
        _ => music_len,
    }
    .min(music_len);
    let per_shot = match kind {
        VideoKind::MusicVideo => beats_per_shot(spb, 1.5, &[1, 2, 4]),
        VideoKind::Promo => 4,
        _ => beats_per_shot(spb, 2.0, &[2, 4, 8]),
    };
    let bounds = shot_bounds(&grid, &beats.downbeats, per_shot, length);

    // Fill the shots with footage, each source range used once (Herk Edit Kit: every B-roll
    // scene once per video).
    let mut used = 0.0;
    if !visual_ids.is_empty() {
        let mut cursor: Vec<f64> = visual_ids
            .iter()
            .map(|id| p.asset(id).map(|a| past_opening(a.duration)).unwrap_or(0.0))
            .collect();
        let mut exhausted = vec![false; visual_ids.len()];
        let mut fresh = vec![true; visual_ids.len()];
        let mut ops = Vec::new();
        let mut kb = Vec::new();
        let mut k = 0;
        for &(s, e) in &bounds {
            // A crossfaded shot starts early by the overlap; lengthen it so its end, and every
            // later cut, stays on the beat.
            let crossfaded = kind == VideoKind::Vlog && !kb.is_empty() && kb.len() % 8 == 0;
            let len = e - s + if crossfaded { CROSSFADE } else { 0.0 };
            let mut placed = false;
            for _ in 0..visual_ids.len() {
                let i = k % visual_ids.len();
                k += 1;
                if exhausted[i] {
                    continue;
                }
                let asset = p.asset(&visual_ids[i]).context("visual vanished")?.clone();
                if asset.kind == AssetKind::Image {
                    ops.push(Op::AddClip {
                        asset: asset.id.clone(),
                        track: Some("main".into()),
                        start: None,
                        index: None,
                        src_in: 0.0,
                        duration: Some(len),
                        frame: None,
                        volume: None,
                        duck: None,
                    });
                    kb.push(true);
                    exhausted[i] = true;
                } else {
                    let path = p.asset_path(&dir, &asset);
                    let from = search_from(cursor[i], len, asset.duration, fresh[i]);
                    let Some(at) = lit_run(&path, from, len, asset.duration) else {
                        exhausted[i] = true;
                        continue;
                    };
                    ops.push(Op::AddClip {
                        asset: asset.id.clone(),
                        track: Some("main".into()),
                        start: None,
                        index: None,
                        src_in: at,
                        duration: Some(len),
                        frame: None,
                        volume: None,
                        duck: None,
                    });
                    kb.push(false);
                    cursor[i] = at + len;
                    fresh[i] = false;
                }
                placed = true;
                break;
            }
            if !placed {
                break;
            }
        }
        ops::apply_all(&mut p, &ops)?;
        let ids: Vec<String> = p.main().items.iter().map(|i| i.id.clone()).collect();
        for (n, id) in ids.iter().enumerate() {
            let mut patch = serde_json::json!({ "muted": true });
            if kb.get(n) == Some(&true) {
                let (a, b) = if n % 2 == 0 {
                    (Point::new(0.45, 0.5), Point::new(0.55, 0.5))
                } else {
                    (Point::new(0.55, 0.48), Point::new(0.45, 0.52))
                };
                patch["ken_burns"] = serde_json::to_value(KenBurns {
                    from_scale: 1.0,
                    to_scale: 1.1,
                    from_focus: Some(a),
                    to_focus: Some(b),
                    smooth: false,
                    phase: [0.0, 1.0],
                })?;
            }
            if kind == VideoKind::Vlog && n > 0 && n % 8 == 0 {
                patch["transition"] = serde_json::to_value(Transition {
                    kind: TransitionKind::Crossfade,
                    duration: CROSSFADE,
                })?;
            }
            if n == 0 {
                patch["fade_in"] = serde_json::json!(0.3);
            }
            if n + 1 == ids.len() {
                patch["fade_out"] = serde_json::json!(0.8);
            }
            ops::apply(
                &mut p,
                &Op::Patch {
                    item: id.clone(),
                    patch,
                },
            )?;
        }
        used = p.duration();
        // Checked after the crossfade overlaps are applied, so they cannot hide a short timeline.
        if used < length - 0.5 {
            job.report.review.push(format!(
                "The footage ran out at {used:.1}s of {length:.1}s of music; the video ends there. Add more clips to fill the song."
            ));
        }
    }
    let total = if used > 0.0 { used } else { length };

    // Music under everything, full level: there is no voice to duck under.
    let track = audio_track(&mut p, "music", "Music")?;
    let clip = place_audio(&mut p, &track, &music_id, 0.0, 0.0, total, 1.0, false)?;
    ops::apply(
        &mut p,
        &Op::Patch {
            item: clip,
            patch: serde_json::json!({ "fade_out": 1.5f64.min(total / 4.0) }),
        },
    )?;

    match kind {
        VideoKind::Promo => {
            let spans = promo_spans(&grid, spb, lines.len(), total);
            for (n, (line, &(start, duration))) in lines.iter().zip(&spans).enumerate() {
                let last = n + 1 == lines.len();
                let preset = if last {
                    TextPreset::Cta
                } else if line.split_whitespace().count() <= 3 {
                    TextPreset::Kinetic
                } else {
                    TextPreset::Title
                };
                let anim = if n % 2 == 0 { Anim::Pop } else { Anim::SlideUp };
                ops::apply(
                    &mut p,
                    &Op::AddText {
                        text: line.clone(),
                        start,
                        duration,
                        preset,
                        position: None,
                        style: None,
                        anim_in: Some(anim),
                        anim_out: Some(Anim::Fade),
                        track: None,
                    },
                )?;
            }
            job.report
                .summary
                .push(format!("{} lines of copy, two bars each", lines.len()));
        }
        VideoKind::Vlog => {
            if let Some(title) = lines.first() {
                ops::apply(
                    &mut p,
                    &Op::AddText {
                        text: title.clone(),
                        start: 0.0,
                        duration: (8.0 * spb).min(total),
                        preset: TextPreset::Title,
                        position: None,
                        style: None,
                        anim_in: Some(Anim::Fade),
                        anim_out: Some(Anim::Fade),
                        track: None,
                    },
                )?;
            }
        }
        VideoKind::MusicVideo => {
            let lyrics = job.req.copy.as_deref().and_then(parse_lrc);
            match lyrics {
                Some(l) => {
                    let n = lyric_captions(&mut p, &l, total);
                    job.report.summary.push(format!("{n} timed lyric lines"));
                }
                None => match job.transcript(&dir, &p, &music_id, progress) {
                    Ok(t) if !t.words.is_empty() => add_captions(job, &mut p, &t)?,
                    Ok(_) | Err(_) => job.report.review.push(
                        "No lyrics: transcription found no clear vocals. Pass timed lyrics as LRC with --copy.".into(),
                    ),
                },
            }
        }
        _ => {}
    }
    job.report.summary.push(format!(
        "Cut to the beat at {tempo:.0} BPM, {per_shot} beats per shot"
    ));
    text_sfx(job, &dir, &mut p)?;
    job.finish(&dir, &mut p, progress)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shots_land_on_beats() {
        let beats: Vec<f64> = (0..40).map(|i| 0.25 + i as f64 * 0.5).collect();
        let downbeats: Vec<f64> = beats.iter().step_by(4).copied().collect();
        let shots = shot_bounds(&beats, &downbeats, 4, 12.0);
        assert_eq!(shots.first().unwrap().0, 0.0);
        assert_eq!(shots.last().unwrap().1, 12.0);
        for (s, _) in shots.iter().skip(1) {
            assert!(
                beats.iter().any(|b| (b - s).abs() < 1e-9),
                "shot starts on a beat: {s}"
            );
        }
        assert_eq!(beats_per_shot(0.5, 2.0, &[2, 4, 8]), 4);
    }

    #[test]
    fn lrc_parses_and_spreads_words() {
        let lines =
            parse_lrc("[00:01.00] hello there\n[00:03.50] second line here\nno stamp").unwrap();
        assert_eq!(lines.len(), 2);
        assert!((lines[1].0 - 3.5).abs() < 1e-9);
        assert!(parse_lrc("plain words").is_none());
        let mut p = Project::new(
            "t",
            VideoKind::MusicVideo,
            crate::project::Canvas::vertical(),
        );
        assert_eq!(lyric_captions(&mut p, &lines, 6.0), 2);
        let cap = &p.tracks[3].items[1];
        assert!((cap.start - 3.5).abs() < 1e-9 && (cap.duration - 2.5).abs() < 1e-9);
    }

    #[test]
    fn a_short_clip_still_holds_its_first_shot() {
        // 2.5 s phone clip: past_opening is 0.625 s, and 0.625 + 2 s overruns the clip.
        let from = search_from(0.625, 2.0, 2.5, true);
        assert!((from - 0.45).abs() < 1e-9, "from {from}");
        assert!(from + 2.0 <= 2.5 - 0.05 + 1e-9, "the shot fits");
        assert_eq!(
            search_from(5.0, 2.0, 60.0, true),
            5.0,
            "a long clip starts past its opening"
        );
        assert_eq!(
            search_from(0.625, 2.0, 2.5, false),
            0.625,
            "a used clip carries on"
        );
        assert_eq!(
            search_from(0.5, 4.0, 3.0, true),
            0.0,
            "a clip shorter than the shot"
        );
    }

    #[test]
    fn promo_lines_end_before_the_next_starts() {
        // The detected grid is tighter (0.4 s) than the nominal 0.5 s per beat.
        let grid: Vec<f64> = (0..60).map(|i| 0.2 + i as f64 * 0.4).collect();
        let spans = promo_spans(&grid, 0.5, 3, 12.0);
        assert_eq!(spans.len(), 3);
        for w in spans.windows(2) {
            assert!(w[0].0 + w[0].1 < w[1].0, "{w:?} overlap");
        }
        let (s, d) = spans[2];
        assert!(
            (s + d - 12.0).abs() < 1e-9,
            "the last line holds to the end"
        );
    }

    #[test]
    fn shots_skip_black_stretches() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fade.mp4");
        let mut cmd = media::ffmpeg_command().unwrap();
        cmd.args([
            "-y",
            "-f",
            "lavfi",
            "-i",
            "color=c=black:s=160x90:r=30:d=4",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=s=160x90:r=30:d=4",
        ])
        .args([
            "-filter_complex",
            "[0:v][1:v]concat=n=2:v=1[v]",
            "-map",
            "[v]",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
        ])
        .arg(&path);
        media::run(&mut cmd).unwrap();
        let at = lit_run(&path, 0.0, 1.0, 8.0).expect("the lit half has room");
        assert!(at >= 3.9 && at + 1.0 <= 8.0, "start {at}");
        assert_eq!(
            lit_run(&path, 5.0, 1.0, 8.0),
            Some(5.0),
            "a lit start stays"
        );
        assert_eq!(lit_run(&path, 7.5, 1.0, 8.0), None, "no room left");
        assert_eq!(lit_run(&path, 0.0, 3.0, 4.0), None, "all black");
    }
}
