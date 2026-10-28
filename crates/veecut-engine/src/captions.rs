//! Builds caption pages on the captions track from a transcript mapped through the timeline, and exports SRT/VTT.

use anyhow::Result;

use crate::analyze;
use crate::project::{Caption, Item, ItemBody, Project, Track, TrackKind, Word};
use crate::transcript::Transcript;

/// A pause longer than this between two words starts a new page.
const PAUSE: f64 = 0.4;

/// One clip of the transcript's asset: where it sits on the timeline and which source it plays.
struct Span {
    start: f64,
    end: f64,
    src_in: f64,
    src_out: f64,
    speed: f64,
}

/// A transcript word placed on the timeline.
struct Placed<'a> {
    text: &'a str,
    start: f64,
    end: f64,
    span: usize,
    src_start: f64,
    src_end: f64,
    emphasis: bool,
}

/// Clips of `asset` on the main track or, when it is not there (narration on an audio track in
/// faceless videos), its clips on audio tracks.
fn spans(project: &Project, asset: &str) -> Vec<Span> {
    let of = |kind: TrackKind| -> Vec<Span> {
        project
            .tracks
            .iter()
            .filter(|t| t.kind == kind)
            .flat_map(|t| &t.items)
            .filter_map(|item| {
                let clip = item.clip().filter(|c| c.asset == asset)?;
                Some(Span {
                    start: item.start,
                    end: item.end(),
                    src_in: clip.src_in,
                    src_out: clip.src_out(item.duration),
                    speed: clip.speed,
                })
            })
            .collect()
    };
    let main = of(TrackKind::Main);
    if main.is_empty() {
        of(TrackKind::Audio)
    } else {
        main
    }
}

fn breaks_after(text: &str) -> bool {
    text.trim_end()
        .ends_with(['.', '!', '?', '…', ',', ';', ':'])
}

/// Caption pages for every transcript word kept on the main track, grouped by the project's caption style.
///
/// A word is kept when its midpoint plays on the timeline. Pages hold at most `max_words` words
/// and `2 * max_chars` characters, and break after sentence ends and commas, at pauses over
/// 0.4 s and at every clip boundary, so no page spans a cut. Each page lasts until its last
/// word ends.
pub fn build(project: &Project, transcript: &Transcript) -> Vec<Item> {
    let spans = spans(project, &transcript.asset);
    let emphasis = analyze::emphasis(&transcript.words);
    let mut placed: Vec<Placed> = Vec::new();
    for (w, &emphasis) in transcript.words.iter().zip(&emphasis) {
        let text = w.text.trim();
        let mid = (w.start + w.end) / 2.0;
        let Some((span, s)) = spans
            .iter()
            .enumerate()
            .find(|(_, s)| mid >= s.src_in && mid < s.src_out)
        else {
            continue;
        };
        if text.is_empty() {
            continue;
        }
        let at = |t: f64| (s.start + (t - s.src_in) / s.speed).clamp(s.start, s.end);
        placed.push(Placed {
            text,
            start: at(w.start),
            end: at(w.end).max(at(w.start)),
            span,
            src_start: w.start,
            src_end: w.end,
            emphasis,
        });
    }
    placed.sort_by(|a, b| a.start.total_cmp(&b.start));

    let max_words = (project.captions.max_words as usize).max(1);
    let max_chars = (project.captions.max_chars as usize * 2).max(1);
    let mut pages: Vec<Vec<Placed>> = Vec::new();
    for word in placed {
        let fits = pages.last().is_some_and(|page| {
            let prev = &page[page.len() - 1];
            let chars: usize = page
                .iter()
                .map(|w| w.text.chars().count() + 1)
                .sum::<usize>()
                + word.text.chars().count();
            page.len() < max_words
                && chars <= max_chars
                && prev.span == word.span
                && !breaks_after(prev.text)
                && word.src_start - prev.src_end <= PAUSE
        });
        match pages.last_mut() {
            Some(page) if fits => page.push(word),
            _ => pages.push(vec![word]),
        }
    }
    pages
        .into_iter()
        .enumerate()
        .map(|(k, page)| {
            let start = page[0].start;
            let end = page.iter().map(|w| w.end).fold(start, f64::max);
            let words = page
                .iter()
                .map(|w| Word {
                    text: w.text.to_string(),
                    start: w.start - start,
                    end: w.end - start,
                    emphasis: w.emphasis,
                })
                .collect();
            Item {
                id: format!("cap-{}", k + 1),
                start,
                duration: end - start,
                body: ItemBody::Caption(Caption { words }),
            }
        })
        .collect()
}

/// Replaces the items on the first captions track with `build`, adding a captions track when
/// the project has none. Returns the number of pages.
pub fn apply(project: &mut Project, transcript: &Transcript) -> Result<usize> {
    let mut items = build(project, transcript);
    let ti = match project.track_of_kind(TrackKind::Captions) {
        Some(ti) => ti,
        None => {
            let id = if project.track("captions").is_none() {
                "captions".to_string()
            } else {
                project.new_id("captions")
            };
            let at = project
                .tracks
                .iter()
                .position(|t| t.kind == TrackKind::Audio)
                .unwrap_or(project.tracks.len());
            let track = Track {
                id,
                kind: TrackKind::Captions,
                name: "Captions".into(),
                muted: false,
                hidden: false,
                items: Vec::new(),
            };
            project.tracks.insert(at, track);
            at
        }
    };
    project.tracks[ti].items.clear();
    // Every number past the highest `cap-N` in use is free.
    let first: u64 = project
        .new_id("cap")
        .rsplit('-')
        .next()
        .and_then(|n| n.parse().ok())
        .unwrap_or(1);
    for (k, item) in items.iter_mut().enumerate() {
        item.id = format!("cap-{}", first + k as u64);
    }
    let count = items.len();
    project.tracks[ti].items = items;
    Ok(count)
}

/// Caption pages of the first captions track: (start, end, text).
fn cues(project: &Project) -> Vec<(f64, f64, String)> {
    let Some(ti) = project.track_of_kind(TrackKind::Captions) else {
        return Vec::new();
    };
    let mut cues: Vec<(f64, f64, String)> = project.tracks[ti]
        .items
        .iter()
        .filter_map(|item| match &item.body {
            ItemBody::Caption(c) if !c.words.is_empty() => Some((
                item.start,
                item.end(),
                c.words
                    .iter()
                    .map(|w| w.text.as_str())
                    .collect::<Vec<_>>()
                    .join(" "),
            )),
            _ => None,
        })
        .collect();
    cues.sort_by(|a, b| a.0.total_cmp(&b.0));
    cues
}

fn stamp(t: f64, separator: char) -> String {
    let ms = (t.max(0.0) * 1000.0).round() as u64;
    format!(
        "{:02}:{:02}:{:02}{separator}{:03}",
        ms / 3_600_000,
        ms / 60_000 % 60,
        ms / 1000 % 60,
        ms % 1000
    )
}

/// The caption pages as SubRip subtitles.
pub fn to_srt(project: &Project) -> String {
    cues(project)
        .into_iter()
        .enumerate()
        .map(|(i, (a, b, text))| {
            format!(
                "{}\n{} --> {}\n{text}\n\n",
                i + 1,
                stamp(a, ','),
                stamp(b, ',')
            )
        })
        .collect()
}

/// The caption pages as WebVTT subtitles.
pub fn to_vtt(project: &Project) -> String {
    let mut out = String::from("WEBVTT\n\n");
    for (a, b, text) in cues(project) {
        out += &format!("{} --> {}\n{text}\n\n", stamp(a, '.'), stamp(b, '.'));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::{Op, apply as apply_op};
    use crate::project::{Asset, AssetKind, AssetRole, Canvas, Clip, VideoKind};
    use crate::transcript::TranscriptWord;

    fn project_with(asset: &str) -> Project {
        let mut p = Project::new("t", VideoKind::TalkingHeadShort, Canvas::vertical());
        p.assets.push(Asset {
            id: asset.into(),
            path: "a.mp4".into(),
            kind: AssetKind::Video,
            role: AssetRole::Source,
            duration: 60.0,
            width: 1920,
            height: 1080,
            fps: 30.0,
            has_audio: true,
        });
        p
    }

    fn transcript(words: &[(&str, f64, f64)]) -> Transcript {
        let words = words
            .iter()
            .map(|&(t, s, e)| TranscriptWord {
                text: t.into(),
                start: s,
                end: e,
                confidence: None,
            })
            .collect();
        Transcript {
            asset: "a".into(),
            provider: "test".into(),
            language: None,
            duration: 60.0,
            words,
        }
    }

    fn pages(items: &[Item]) -> Vec<String> {
        items
            .iter()
            .map(|i| match &i.body {
                ItemBody::Caption(c) => c
                    .words
                    .iter()
                    .map(|w| w.text.as_str())
                    .collect::<Vec<_>>()
                    .join(" "),
                _ => String::new(),
            })
            .collect()
    }

    #[test]
    fn captions_follow_the_cuts() {
        let mut p = project_with("a");
        let add = Op::AddClip {
            asset: "a".into(),
            track: None,
            start: None,
            index: None,
            src_in: 0.0,
            duration: Some(20.0),
            frame: None,
            volume: None,
            duck: None,
        };
        apply_op(&mut p, &add).unwrap();
        apply_op(&mut p, &Op::CutRange { from: 2.9, to: 4.0 }).unwrap();
        let t = transcript(&[
            ("Hello", 1.0, 1.3),
            ("world.", 1.4, 1.8),
            ("This", 2.0, 2.2),
            ("is", 2.3, 2.4),
            ("cut", 3.0, 3.3),
            ("away", 3.4, 3.7),
            ("and", 5.0, 5.2),
            ("this", 5.3, 5.5),
            ("stays", 5.6, 6.0),
            ("here", 6.05, 6.3),
            ("too", 6.35, 6.6),
            ("later", 7.2, 7.5),
        ]);
        let items = build(&p, &t);
        assert_eq!(
            pages(&items),
            [
                "Hello world.",
                "This is",
                "and this stays",
                "here too",
                "later"
            ]
        );
        let third = &items[2];
        assert!(
            (third.start - 3.9).abs() < 1e-9,
            "source 5.0 lands at 3.9 after the 1.1 s cut"
        );
        let ItemBody::Caption(c) = &third.body else {
            panic!()
        };
        assert!((c.words[1].start - 0.3).abs() < 1e-9 && (third.duration - 1.0).abs() < 1e-9);

        apply_op(
            &mut p,
            &Op::AddText {
                text: "x".into(),
                start: 0.0,
                duration: 1.0,
                preset: Default::default(),
                position: None,
                style: None,
                anim_in: None,
                anim_out: None,
                track: None,
            },
        )
        .unwrap();
        assert_eq!(apply(&mut p, &t).unwrap(), 5);
        assert_eq!(
            apply(&mut p, &t).unwrap(),
            5,
            "applying again replaces the pages"
        );
        let caps = &p.tracks[p.track_of_kind(TrackKind::Captions).unwrap()].items;
        assert_eq!(caps.len(), 5);
        assert!(caps.iter().all(|i| i.id.starts_with("cap-")));
        p.validate().unwrap();
    }

    #[test]
    fn narration_on_an_audio_track_is_captioned() {
        let mut p = project_with("a");
        let music = p.track_of_kind(TrackKind::Audio).unwrap();
        for (id, start, src_in) in [("n1", 0.0, 0.0), ("n2", 2.0, 3.0)] {
            p.tracks[music].items.push(Item {
                id: id.into(),
                start,
                duration: 2.0,
                body: ItemBody::Clip(Clip::new("a", src_in)),
            });
        }
        let t = transcript(&[
            ("One", 0.2, 0.5),
            ("two", 0.6, 0.9),
            ("gone", 2.2, 2.6),
            ("three", 3.2, 3.5),
        ]);
        let items = build(&p, &t);
        assert_eq!(pages(&items), ["One two", "three"]);
        assert!((items[1].start - 2.2).abs() < 1e-9);
    }

    #[test]
    fn srt_and_vtt_format() {
        let mut p = project_with("a");
        let words = |list: &[&str]| {
            list.iter()
                .map(|w| Word {
                    text: w.to_string(),
                    start: 0.0,
                    end: 0.1,
                    emphasis: false,
                })
                .collect()
        };
        let caps = p.track_of_kind(TrackKind::Captions).unwrap();
        p.tracks[caps].items = vec![
            Item {
                id: "cap-1".into(),
                start: 0.5,
                duration: 1.25,
                body: ItemBody::Caption(Caption {
                    words: words(&["Hello", "world."]),
                }),
            },
            Item {
                id: "cap-2".into(),
                start: 3661.0,
                duration: 0.5,
                body: ItemBody::Caption(Caption {
                    words: words(&["Bye"]),
                }),
            },
        ];
        assert_eq!(
            to_srt(&p),
            "1\n00:00:00,500 --> 00:00:01,750\nHello world.\n\n2\n01:01:01,000 --> 01:01:01,500\nBye\n\n"
        );
        assert_eq!(
            to_vtt(&p),
            "WEBVTT\n\n00:00:00.500 --> 00:00:01.750\nHello world.\n\n01:01:01.000 --> 01:01:01.500\nBye\n\n"
        );
    }
}
