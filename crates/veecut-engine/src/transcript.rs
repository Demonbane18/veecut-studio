//! Word-level transcripts of a source file. Times are seconds in the source, not the timeline;
//! `Project::source_to_timeline` maps them through the cuts.

use std::path::Path;

use anyhow::{Context, Result, bail};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::captions;
use crate::ops::{Applied, Op, apply};
use crate::project::Project;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Transcript {
    /// Asset id this transcript belongs to.
    pub asset: String,
    /// Which service or model produced it, for example `groq:whisper-large-v3` or `local:large-v3-turbo`.
    pub provider: String,
    /// ISO 639-1 code when known, for example `en` or `tl`.
    #[serde(default)]
    pub language: Option<String>,
    /// Length of the audio in seconds.
    pub duration: f64,
    pub words: Vec<TranscriptWord>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TranscriptWord {
    pub text: String,
    pub start: f64,
    pub end: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
}

/// A run of words ending in sentence punctuation or a long pause.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Sentence {
    /// Index of the first word.
    pub first: usize,
    /// Index one past the last word.
    pub end_word: usize,
    pub start: f64,
    pub end: f64,
    pub text: String,
}

/// A pause at least this long ends a sentence even without punctuation.
pub const SENTENCE_GAP: f64 = 0.7;

impl Transcript {
    pub fn load(path: &Path) -> Result<Self> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        serde_json::from_str(&text)
            .with_context(|| format!("{} is not a VeeCut transcript", path.display()))
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, serde_json::to_string_pretty(self)? + "\n")
            .with_context(|| format!("writing {}", path.display()))
    }

    pub fn text(&self) -> String {
        self.words
            .iter()
            .map(|w| w.text.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    }

    pub fn sentences(&self) -> Vec<Sentence> {
        let mut out = Vec::new();
        let mut first = 0;
        for (i, w) in self.words.iter().enumerate() {
            let ends_sentence = w.text.ends_with(['.', '?', '!', '…']);
            let long_pause = self
                .words
                .get(i + 1)
                .is_some_and(|n| n.start - w.end >= SENTENCE_GAP);
            if ends_sentence || long_pause || i + 1 == self.words.len() {
                let words = &self.words[first..=i];
                out.push(Sentence {
                    first,
                    end_word: i + 1,
                    start: words[0].start,
                    end: w.end,
                    text: words
                        .iter()
                        .map(|w| w.text.as_str())
                        .collect::<Vec<_>>()
                        .join(" "),
                });
                first = i + 1;
            }
        }
        out
    }
}

const EPS: f64 = 1e-6;

fn ms(t: f64) -> f64 {
    (t * 1000.0).round() / 1000.0
}

/// Lowercase letters and digits only, so "Hello," and "hello" are the same spoken word.
fn norm(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// A transcript word that plays on the timeline. Seconds, rounded to the millisecond.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PlacedWord {
    /// `w` plus the word's index in the transcript, for example `w12`.
    pub id: String,
    pub text: String,
    pub source_start: f64,
    pub source_end: f64,
    pub timeline_start: f64,
    pub timeline_end: f64,
}

/// One place a phrase is spoken in the edit as it plays now.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PhraseMatch {
    /// 1-based, in timeline order.
    pub occurrence: usize,
    pub asset: String,
    pub words: Vec<PlacedWord>,
    pub source_start: f64,
    pub source_end: f64,
    pub timeline_start: f64,
    pub timeline_end: f64,
    /// The transcript sentence around the phrase.
    pub sentence: String,
}

/// What `cut_phrase` removed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PhraseCut {
    pub phrase: String,
    /// The occurrence that was cut.
    pub cut: PhraseMatch,
    /// The `cut_range` op that was applied.
    pub op: Op,
    pub applied: Applied,
}

/// A kept word with unrounded times. `joined` means it follows the previous kept word with
/// nothing between them: the next transcript word, and no other footage.
struct Raw {
    index: usize,
    span: usize,
    t_start: f64,
    t_end: f64,
    joined: bool,
}

/// True when a main-track item of another asset plays between `from` and `to`.
fn foreign_between(p: &Project, asset: &str, from: f64, to: f64) -> bool {
    p.main().items.iter().any(|it| {
        it.clip().map_or(true, |c| c.asset != asset) && it.start < to - EPS && it.end() > from + EPS
    })
}

impl Transcript {
    /// Words that play on the timeline, in timeline order. A word is kept when its midpoint plays
    /// (the rule captions use); clip speed and repeated clips of the same footage are mapped, and
    /// times are clamped to the clip so every kept word has a start and an end.
    fn raw_placed(&self, project: &Project) -> Vec<Raw> {
        let mut spans = captions::spans(project, &self.asset);
        spans.sort_by(|a, b| a.start.total_cmp(&b.start));
        let on_main = project
            .main()
            .items
            .iter()
            .any(|i| i.clip().is_some_and(|c| c.asset == self.asset));
        let mut out: Vec<Raw> = Vec::new();
        for (si, s) in spans.iter().enumerate() {
            let at = |t: f64| (s.start + (t - s.src_in) / s.speed).clamp(s.start, s.end);
            for (index, w) in self.words.iter().enumerate() {
                let mid = (w.start + w.end) / 2.0;
                if !(mid >= s.src_in && mid < s.src_out) {
                    continue;
                }
                // Joined: the very next transcript word, with no other footage between clips.
                let joined = match out.last() {
                    None => false,
                    Some(prev) if prev.index + 1 != index => false,
                    Some(prev) if prev.span == si => true,
                    Some(prev) => {
                        !on_main
                            || !foreign_between(project, &self.asset, spans[prev.span].end, s.start)
                    }
                };
                out.push(Raw {
                    index,
                    span: si,
                    t_start: at(w.start),
                    t_end: at(w.end).max(at(w.start)),
                    joined,
                });
            }
        }
        out
    }

    fn placed_word(&self, r: &Raw) -> PlacedWord {
        let w = &self.words[r.index];
        PlacedWord {
            id: format!("w{}", r.index),
            text: w.text.clone(),
            source_start: ms(w.start),
            source_end: ms(w.end),
            timeline_start: ms(r.t_start),
            timeline_end: ms(r.t_end),
        }
    }

    /// The words of this transcript that play on the timeline, in timeline order.
    pub fn placed_words(&self, project: &Project) -> Vec<PlacedWord> {
        self.raw_placed(project)
            .iter()
            .map(|r| self.placed_word(r))
            .collect()
    }

    /// Each match with its exact (unrounded) timeline range.
    fn matches(&self, project: &Project, phrase: &str) -> Result<Vec<(PhraseMatch, f64, f64)>> {
        let query: Vec<String> = phrase
            .split_whitespace()
            .map(norm)
            .filter(|t| !t.is_empty())
            .collect();
        if query.is_empty() {
            bail!("the phrase has no words; give the spoken words to find");
        }
        let raw = self.raw_placed(project);
        let tokens: Vec<(usize, String)> = raw
            .iter()
            .enumerate()
            .filter_map(|(k, r)| {
                let n = norm(&self.words[r.index].text);
                (!n.is_empty()).then_some((k, n))
            })
            .collect();
        let sentences = self.sentences();
        let sentence_of = |i: usize| sentences.iter().find(|s| i >= s.first && i < s.end_word);
        let mut out = Vec::new();
        for win in tokens.windows(query.len()) {
            let same = win.iter().zip(&query).all(|((_, n), q)| n == q);
            let joined = win
                .windows(2)
                .all(|p| raw[p[0].0 + 1..=p[1].0].iter().all(|r| r.joined));
            if !(same && joined) {
                continue;
            }
            let (first, last) = (&raw[win[0].0], &raw[win[win.len() - 1].0]);
            let sentence = match (sentence_of(first.index), sentence_of(last.index)) {
                (Some(a), Some(b)) if a.first <= b.end_word => self.words[a.first..b.end_word]
                    .iter()
                    .map(|w| w.text.as_str())
                    .collect::<Vec<_>>()
                    .join(" "),
                (Some(a), Some(b)) => format!("{} … {}", a.text, b.text),
                _ => String::new(),
            };
            let words: Vec<PlacedWord> = win
                .iter()
                .map(|(k, _)| self.placed_word(&raw[*k]))
                .collect();
            out.push((
                PhraseMatch {
                    occurrence: out.len() + 1,
                    asset: self.asset.clone(),
                    source_start: words[0].source_start,
                    source_end: words[words.len() - 1].source_end,
                    timeline_start: ms(first.t_start),
                    timeline_end: ms(last.t_end),
                    words,
                    sentence,
                },
                first.t_start,
                last.t_end,
            ));
        }
        Ok(out)
    }

    /// Every place `phrase` is spoken in the edit as it plays now, numbered from 1 in timeline
    /// order. Matching is on whole spoken words: case and punctuation are ignored, nothing else.
    /// The words must be neighbours in the transcript and follow each other on the timeline,
    /// across cuts and clip joins, with no other footage between. Overlapping matches ("go go"
    /// in "go go go") are all listed.
    pub fn find_phrase(&self, project: &Project, phrase: &str) -> Result<Vec<PhraseMatch>> {
        Ok(self
            .matches(project, phrase)?
            .into_iter()
            .map(|m| m.0)
            .collect())
    }

    /// Cuts one occurrence of `phrase` from every track and closes the gap, atomically through
    /// `ops::apply`. With more than one match `occurrence` (1-based) is required; otherwise
    /// nothing changes.
    pub fn cut_phrase(
        &self,
        project: &mut Project,
        phrase: &str,
        occurrence: Option<usize>,
    ) -> Result<PhraseCut> {
        let found = self.matches(project, phrase)?;
        let pick = match (occurrence, found.len()) {
            (_, 0) => bail!(
                "{phrase:?} is not spoken in the part of {} that plays",
                self.asset
            ),
            (Some(n), len) if n == 0 || n > len => bail!(
                "{phrase:?} is spoken {len} time(s); occurrence {n} does not exist (use 1..{len})"
            ),
            (Some(n), _) => n - 1,
            (None, 1) => 0,
            (None, len) => {
                let at: Vec<String> = found
                    .iter()
                    .map(|m| format!("{} at {:.3}s", m.0.occurrence, m.0.timeline_start))
                    .collect();
                bail!(
                    "{phrase:?} is spoken {len} times ({}); pass an occurrence (1..{len})",
                    at.join(", ")
                )
            }
        };
        let (cut, from, to) = found.into_iter().nth(pick).expect("pick is in range");
        let op = Op::CutRange { from, to };
        let applied = apply(project, &op).with_context(|| format!("cutting {phrase:?}"))?;
        Ok(PhraseCut {
            phrase: phrase.to_string(),
            cut,
            op,
            applied,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sentences_split_on_punctuation_and_pauses() {
        let w = |text: &str, start: f64, end: f64| TranscriptWord {
            text: text.into(),
            start,
            end,
            confidence: None,
        };
        let t = Transcript {
            asset: "a".into(),
            provider: "test".into(),
            language: Some("en".into()),
            duration: 10.0,
            words: vec![
                w("Hello", 0.0, 0.4),
                w("there.", 0.5, 0.9),
                w("So", 1.0, 1.2),
                w("um", 2.5, 2.7),
                w("yes", 2.8, 3.0),
            ],
        };
        let s = t.sentences();
        assert_eq!(
            s.iter().map(|s| s.text.as_str()).collect::<Vec<_>>(),
            ["Hello there.", "So", "um yes"]
        );
        assert_eq!((s[2].first, s[2].end_word), (3, 5));
    }

    use crate::project::{Asset, AssetKind, AssetRole, Canvas, VideoKind};

    fn tr(words: &[(&str, f64, f64)]) -> Transcript {
        Transcript {
            asset: "a".into(),
            provider: "test".into(),
            language: None,
            duration: 60.0,
            words: words
                .iter()
                .map(|&(text, start, end)| TranscriptWord {
                    text: text.into(),
                    start,
                    end,
                    confidence: None,
                })
                .collect(),
        }
    }

    fn asset(id: &str) -> Asset {
        Asset {
            id: id.into(),
            path: format!("{id}.mp4").into(),
            kind: AssetKind::Video,
            role: AssetRole::Source,
            duration: 60.0,
            width: 1920,
            height: 1080,
            fps: 30.0,
            has_audio: true,
        }
    }

    fn add(asset: &str, src_in: f64, duration: f64) -> Op {
        Op::AddClip {
            asset: asset.into(),
            track: None,
            start: None,
            index: None,
            src_in,
            duration: Some(duration),
            frame: None,
            volume: None,
            duck: None,
        }
    }

    /// Project with assets `a` and `b` and the given main-track clips (asset, src_in, duration).
    fn project(clips: &[(&str, f64, f64)]) -> Project {
        let mut p = Project::new("t", VideoKind::TalkingHeadShort, Canvas::vertical());
        p.assets.extend([asset("a"), asset("b")]);
        for &(a, src_in, dur) in clips {
            crate::ops::apply(&mut p, &add(a, src_in, dur)).unwrap();
        }
        p
    }

    fn near(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-9, "{a} != {b}");
    }

    fn so_yes() -> Transcript {
        tr(&[
            ("So", 1.0, 1.3),
            ("yes.", 1.4, 1.8),
            ("No", 2.0, 2.3),
            ("so,", 3.0, 3.3),
            ("YES", 3.4, 3.8),
        ])
    }

    #[test]
    fn search_matches_whole_spoken_words_only() {
        let p = project(&[("a", 0.0, 20.0)]);
        let t = tr(&[
            ("Hello,", 0.0, 0.4),
            ("there.", 0.5, 0.9),
            ("Hello", 5.0, 5.4),
            ("there", 5.5, 5.9),
            ("friend", 6.0, 6.4),
            ("Hellothere", 8.0, 8.5),
        ]);
        assert_eq!(t.find_phrase(&p, "hello THERE!").unwrap().len(), 2);
        assert_eq!(t.find_phrase(&p, "hello there friend").unwrap().len(), 1);
        assert!(t.find_phrase(&p, "hell").unwrap().is_empty());
        assert!(t.find_phrase(&p, "ello there").unwrap().is_empty());
        // "there." then "Hello" are neighbours in the transcript (across a sentence end), so it is
        // spoken once; reversed order and a word that was never next to another are not.
        assert_eq!(t.find_phrase(&p, "there hello").unwrap().len(), 1);
        assert!(t.find_phrase(&p, "friend there").unwrap().is_empty());
        assert!(t.find_phrase(&p, "friend hello").unwrap().is_empty());
        assert!(t.find_phrase(&p, " ... ").is_err());
        let m = &t.find_phrase(&p, "hello there").unwrap()[1];
        assert_eq!(m.occurrence, 2);
        assert_eq!(
            m.words.iter().map(|w| w.id.as_str()).collect::<Vec<_>>(),
            ["w2", "w3"]
        );
        assert_eq!(m.sentence, "Hello there friend");
        near(m.source_start, 5.0);
        near(m.timeline_end, 5.9);
    }

    #[test]
    fn ambiguous_phrase_changes_nothing_and_occurrence_picks_one() {
        let mut p = project(&[("a", 0.0, 20.0)]);
        let t = so_yes();
        let before = p.clone();
        let err = t
            .cut_phrase(&mut p, "so yes", None)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("2 times") && err.contains("occurrence"),
            "{err}"
        );
        assert_eq!(p, before);
        assert!(t.cut_phrase(&mut p, "so yes", Some(3)).is_err());
        assert!(t.cut_phrase(&mut p, "so yes", Some(0)).is_err());
        assert!(t.cut_phrase(&mut p, "nothing here", None).is_err());
        assert_eq!(p, before);

        let cut = t.cut_phrase(&mut p, "so yes", Some(2)).unwrap();
        near(cut.cut.timeline_start, 3.0);
        near(p.duration(), 20.0 - 0.8);
        let left = t.find_phrase(&p, "so yes").unwrap();
        assert_eq!(left.len(), 1);
        near(left[0].timeline_start, 1.0);
        assert_eq!(left[0].words[0].id, "w0");
        // A single match needs no occurrence.
        t.cut_phrase(&mut p, "so yes", None).unwrap();
        near(p.duration(), 20.0 - 1.6);
        p.validate().unwrap();
    }

    #[test]
    fn overlapping_matches_are_all_listed_and_need_an_occurrence() {
        let mut p = project(&[("a", 0.0, 20.0)]);
        let t = tr(&[("go", 1.0, 1.2), ("go", 1.3, 1.5), ("go", 1.6, 1.8)]);
        assert_eq!(t.find_phrase(&p, "go go").unwrap().len(), 2);
        assert!(t.cut_phrase(&mut p, "go go", None).is_err());
        let cut = t.cut_phrase(&mut p, "go go", Some(2)).unwrap();
        near(cut.cut.timeline_start, 1.3);
        near(p.duration(), 20.0 - 0.5);
        // The recorded op replays on a fresh project to the same result.
        let mut q = project(&[("a", 0.0, 20.0)]);
        crate::ops::apply(&mut q, &cut.op).unwrap();
        assert_eq!(p, q);
    }

    #[test]
    fn speed_and_source_offset_map_to_the_timeline() {
        let mut p = project(&[("a", 4.0, 5.0)]);
        crate::ops::apply(
            &mut p,
            &Op::Patch {
                item: "clip-1".into(),
                patch: serde_json::json!({"speed": 2.0}),
            },
        )
        .unwrap();
        let t = tr(&[
            ("before", 1.0, 1.4),
            ("alpha", 6.0, 6.4),
            ("beta", 6.5, 7.0),
            ("after", 20.0, 20.4),
        ]);
        assert_eq!(t.placed_words(&p).len(), 2);
        let m = &t.find_phrase(&p, "alpha beta").unwrap()[0];
        near(m.source_start, 6.0);
        near(m.source_end, 7.0);
        near(m.timeline_start, 1.0);
        near(m.timeline_end, 1.5);
        t.cut_phrase(&mut p, "alpha beta", None).unwrap();
        near(p.duration(), 4.5);
    }

    #[test]
    fn repeated_source_clips_are_separate_occurrences() {
        let mut p = project(&[("a", 0.0, 4.0), ("a", 0.0, 4.0)]);
        let t = tr(&[("so", 1.0, 1.3), ("yes", 1.4, 1.8)]);
        let found = t.find_phrase(&p, "so yes").unwrap();
        assert_eq!(found.len(), 2);
        near(found[0].timeline_start, 1.0);
        near(found[1].timeline_start, 5.0);
        near(found[1].source_start, 1.0);
        t.cut_phrase(&mut p, "so yes", Some(2)).unwrap();
        near(p.main().items[0].duration, 4.0);
        assert_eq!(t.find_phrase(&p, "so yes").unwrap().len(), 1);
        near(p.duration(), 8.0 - 0.8);
    }

    #[test]
    fn phrases_cross_cuts_but_not_removed_words_or_other_footage() {
        // A cut between two neighbouring words keeps the phrase intact.
        let mut p = project(&[("a", 0.0, 20.0)]);
        crate::ops::apply(&mut p, &Op::CutRange { from: 2.0, to: 3.0 }).unwrap();
        let t = tr(&[("so", 1.5, 1.9), ("yes", 3.2, 3.6)]);
        let m = &t.find_phrase(&p, "so yes").unwrap()[0];
        near(m.timeline_start, 1.5);
        near(m.timeline_end, 2.6);
        t.cut_phrase(&mut p, "so yes", None).unwrap();
        near(p.duration(), 19.0 - 1.1);

        // A word removed between them means the viewer never hears the phrase.
        let mut p = project(&[("a", 0.0, 20.0)]);
        crate::ops::apply(&mut p, &Op::CutRange { from: 2.0, to: 3.0 }).unwrap();
        let t = tr(&[("so", 1.5, 1.9), ("um", 2.4, 2.7), ("yes", 3.2, 3.6)]);
        let before = p.clone();
        assert!(t.find_phrase(&p, "so yes").unwrap().is_empty());
        assert!(t.cut_phrase(&mut p, "so yes", None).is_err());
        assert_eq!(p, before);

        // Footage from another asset between the two words breaks the phrase.
        let q = project(&[("a", 0.0, 4.0), ("b", 0.0, 2.0), ("a", 4.0, 4.0)]);
        let t = tr(&[("so", 3.5, 3.9), ("yes", 4.1, 4.5)]);
        assert!(t.find_phrase(&q, "so yes").unwrap().is_empty());
        // Two touching clips of the same footage do not.
        let q = project(&[("a", 0.0, 4.0), ("a", 4.0, 4.0)]);
        assert_eq!(t.find_phrase(&q, "so yes").unwrap().len(), 1);
        // Replayed footage is not one phrase: "yes" at the end of a clip then "so" at its start.
        let q = project(&[("a", 0.0, 4.0), ("a", 0.0, 4.0)]);
        let t = tr(&[("so", 0.5, 0.9), ("yes", 3.0, 3.5)]);
        assert!(t.find_phrase(&q, "yes so").unwrap().is_empty());
    }
}
