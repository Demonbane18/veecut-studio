//! Cut decisions for talking footage: dead air, long pauses, fillers, stutters, retakes and false
//! starts.
//!
//! The rules and numbers are Herk Edit Kit's `cut-silences` and `cut-mistakes` skills (Nate Herk,
//! MIT) as used by free247editor (riecodes, MIT), reimplemented here. On top of them the audio
//! confirms every pause, so cuts land in real quiet even when transcript times are a little off.

use std::collections::HashSet;
use std::ops::Range;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{clean_token, ends_sentence, levels, quiet_runs, threshold_from, window_len};
use crate::transcript::{Transcript, TranscriptWord};

/// Silence kept before the first word (Herk Edit Kit: 0.22 s).
const HEAD_PAD: f64 = 0.22;
/// Silence kept after the last word (Herk Edit Kit: 0.34 s).
const TAIL_PAD: f64 = 0.34;
/// Keep ranges shorter than this are dropped (Herk Edit Kit: 0.04 s).
const MIN_KEEP: f64 = 0.04;
/// Cuts shorter than this are not worth a splice.
const MIN_CUT: f64 = 0.02;
/// Quiet shorter than this is a consonant, not a pause.
const MIN_QUIET: f64 = 0.04;
/// A filler is only removed with at least this much quiet on each side.
const FILLER_QUIET: f64 = 0.1;
/// How far quiet in the audio may sit from a transcript word edge and still belong to that gap.
const TOLERANCE: f64 = 0.05;
/// How far a pause found in the audio may reach past the transcript's word edges.
const SLACK: f64 = 0.15;
/// A high-confidence retake is cut on its own only when the later take starts within this many
/// seconds; further apart it may be a callback, so it goes to review.
const RETAKE_GAP: f64 = 12.0;
/// How far ahead a later take is still worth comparing. The marked span is never this gap.
const RETAKE_LOOK: f64 = 75.0;
/// A review or cut covers one repeated sentence or one near segment, not the speech between takes.
const RETAKE_SPAN: f64 = 16.0;

const FILLERS: &[&str] = &[
    "um", "umm", "uhm", "uh", "uhh", "erm", "er", "err", "ah", "ahh", "hmm", "hm", "mm", "mmm",
    "ano", "eh",
];
/// Doubled words that are usually deliberate ("very very", "no no"), so they go to review.
const DOUBLING: &[&str] = &[
    "very", "really", "no", "never", "bye", "ha", "haha", "more", "again", "yeah",
];
/// Herk Edit Kit's stop words for retake signatures.
const STOP: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "but", "by", "for", "from", "i", "if", "in", "is",
    "it", "of", "on", "or", "so", "that", "this", "to", "was", "we", "with", "you", "your", "they",
    "their", "there", "what", "when", "where", "why", "how", "like", "um", "uh",
];

/// How hard `plan_cuts` trims.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct CutSettings {
    /// Pauses shorter than this stay. 0.55 s keeps a natural rhythm; 0.2 s gives the tight reel style.
    pub min_silence: f64,
    /// Silence always kept after a word's end before a cut, so the last consonant decays
    /// (transcripts end words 40-80 ms early).
    pub pad: f64,
    /// Remove um, uh, erm, er, ah, hmm and Tagalog ano/eh when they stand alone between pauses.
    pub remove_fillers: bool,
    /// Remove stutters, retakes and false starts. Only high-confidence ones are cut; the others
    /// come back as removals flagged for review.
    pub remove_retakes: bool,
}

impl Default for CutSettings {
    fn default() -> Self {
        Self::natural()
    }
}

impl CutSettings {
    /// Herk Edit Kit's default: pauses under 0.55 s stay, which removes about 15-20% of a raw take.
    pub fn natural() -> Self {
        CutSettings {
            min_silence: 0.55,
            pad: 0.08,
            remove_fillers: true,
            remove_retakes: true,
        }
    }

    /// The tight reel style: every pause over 0.2 s comes down to a breath.
    pub fn tight() -> Self {
        CutSettings {
            min_silence: 0.2,
            ..Self::natural()
        }
    }
}

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum CutReason {
    /// Dead air at the ends, or a pause trimmed down to a breath.
    Silence,
    /// A standalone um, uh or similar.
    Filler,
    /// The same word twice in a row ("the the").
    Stutter,
    /// A short opening abandoned and restarted.
    FalseStart,
    /// A line said again; the last take stays.
    Retake,
}

/// A stretch of source taken out, or proposed for review.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Removal {
    pub start: f64,
    pub end: f64,
    pub reason: CutReason,
    /// The words removed; empty for silence.
    pub text: String,
    /// Only a suggestion: `keep` still contains this range. Medium and low confidence stutters,
    /// retakes and false starts come back this way for an agent or the user to decide.
    #[serde(default)]
    pub review: bool,
    /// Why, in plain words.
    #[serde(default)]
    pub note: String,
}

/// What to keep of a source, in source seconds, and what was taken out.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CutPlan {
    pub keep: Vec<(f64, f64)>,
    /// Applied removals and review suggestions, in time order.
    pub removed: Vec<Removal>,
}

/// Plans the cuts for one source. `pcm` is the source's mono audio from time 0 (16 kHz is
/// plenty); with no usable audio, transcript times are trusted as they are.
///
/// Pauses longer than `min_silence` come down to a breath: 0.24 s after a pause of 2 s or more,
/// 0.20 s after a sentence, else 0.14 s, split 55% after the previous word and 45% before the
/// next. Dead air before the first word keeps 0.22 s, after the last 0.34 s. Cuts only fall in
/// pauses, never inside a phrase.
pub fn plan_cuts(
    transcript: &Transcript,
    pcm: &[f32],
    sample_rate: u32,
    settings: &CutSettings,
) -> CutPlan {
    let mut words: Vec<&TranscriptWord> = transcript
        .words
        .iter()
        .filter(|w| !w.text.trim().is_empty() && w.start.is_finite() && w.end >= w.start)
        .collect();
    words.sort_by(|a, b| a.start.total_cmp(&b.start));
    let audio = if sample_rate > 0 {
        pcm.len() as f64 / sample_rate as f64
    } else {
        0.0
    };
    let duration = transcript
        .duration
        .max(audio)
        .max(words.last().map_or(0.0, |w| w.end));
    let quiet = audio_quiet(&words, pcm, sample_rate);
    Planner {
        words,
        duration,
        settings,
        quiet,
    }
    .plan()
}

/// Quiet stretches of the audio, or None when there is no audio or no real pause anywhere in it
/// (a music bed or a loud room), in which case transcript gaps are trusted instead.
fn audio_quiet(
    words: &[&TranscriptWord],
    pcm: &[f32],
    sample_rate: u32,
) -> Option<Vec<(f64, f64)>> {
    if sample_rate == 0 || pcm.is_empty() {
        return None;
    }
    let n = window_len(sample_rate);
    let hop = n as f64 / sample_rate as f64;
    let db = levels(pcm, n);
    let speech: Vec<f32> = words
        .iter()
        .flat_map(|w| {
            let end = ((w.end / hop).ceil() as usize).min(db.len());
            ((w.start / hop) as usize).min(end)..end
        })
        .map(|i| db[i])
        .collect();
    let runs = quiet_runs(&db, hop, threshold_from(speech), MIN_QUIET);
    runs.iter()
        .any(|(a, b)| b - a >= FILLER_QUIET)
        .then_some(runs)
}

struct Mistake {
    words: Range<usize>,
    reason: CutReason,
    score: f64,
    sure: bool,
    note: String,
}

struct Planner<'a> {
    words: Vec<&'a TranscriptWord>,
    duration: f64,
    settings: &'a CutSettings,
    quiet: Option<Vec<(f64, f64)>>,
}

impl Planner<'_> {
    fn plan(&self) -> CutPlan {
        let w = &self.words;
        if w.is_empty() {
            let keep = if self.duration > 0.0 {
                vec![(0.0, self.duration)]
            } else {
                Vec::new()
            };
            return CutPlan {
                keep,
                removed: Vec::new(),
            };
        }
        let tokens: Vec<String> = w.iter().map(|x| clean_token(&x.text)).collect();
        let mut reason: Vec<Option<CutReason>> = vec![None; w.len()];
        let mut applied = Vec::new();
        let mut suggested = Vec::new();
        if self.settings.remove_retakes {
            let mut found = mistakes(w, &tokens);
            found.sort_by(|a, b| b.score.total_cmp(&a.score));
            for m in found {
                if m.sure {
                    for r in &mut reason[m.words.clone()] {
                        r.get_or_insert(m.reason);
                    }
                    applied.push(m);
                } else {
                    suggested.push(m);
                }
            }
        }
        if self.settings.remove_fillers {
            for i in 0..w.len() {
                if reason[i].is_none()
                    && FILLERS.contains(&tokens[i].as_str())
                    && self.quiet_around(i)
                {
                    reason[i] = Some(CutReason::Filler);
                }
            }
        }

        // One cut between each pair of kept words (and at both ends).
        let kept: Vec<usize> = (0..w.len()).filter(|&i| reason[i].is_none()).collect();
        let prevs = std::iter::once(None).chain(kept.iter().copied().map(Some));
        let nexts = kept.iter().copied().map(Some).chain(std::iter::once(None));
        let mut removed = Vec::new();
        for (p, n) in prevs.zip(nexts) {
            let range = p.map_or(0, |i| i + 1)..n.unwrap_or(w.len());
            let Some((start, end, pause)) = self.region(p, range.clone(), n) else {
                continue;
            };
            let why = range
                .clone()
                .filter_map(|i| reason[i])
                .max()
                .unwrap_or(CutReason::Silence);
            let note = match why {
                CutReason::Silence if p.is_none() => "dead air before the first word".to_string(),
                CutReason::Silence if n.is_none() => "dead air after the last word".to_string(),
                CutReason::Silence => format!("pause of {pause:.2} s trimmed to a breath"),
                CutReason::Filler => "filler with silence on both sides".to_string(),
                _ => applied
                    .iter()
                    .filter(|m| range.contains(&m.words.start))
                    .map(|m| m.note.as_str())
                    .collect::<Vec<_>>()
                    .join("; "),
            };
            removed.push(Removal {
                start,
                end,
                reason: why,
                text: self.text(range),
                review: false,
                note,
            });
        }

        // Suggestions, strongest first, skipping any already cut or overlapping a stronger one.
        let mut taken: Vec<Range<usize>> = Vec::new();
        for m in suggested {
            let r = m.words.clone();
            if r.clone().all(|i| reason[i].is_some())
                || taken.iter().any(|t| t.start < r.end && r.start < t.end)
            {
                continue;
            }
            let p = r.start.checked_sub(1);
            let n = (r.end < w.len()).then_some(r.end);
            if let Some((start, end, _)) = self.region(p, r.clone(), n) {
                removed.push(Removal {
                    start,
                    end,
                    reason: m.reason,
                    text: self.text(r.clone()),
                    review: true,
                    note: m.note,
                });
                taken.push(r);
            }
        }
        removed.sort_by(|a, b| a.start.total_cmp(&b.start));

        let mut keep = Vec::new();
        let mut cursor = 0.0f64;
        for r in removed.iter().filter(|r| !r.review) {
            if r.start > cursor {
                keep.push((cursor, r.start));
            }
            cursor = cursor.max(r.end);
        }
        if cursor < self.duration {
            keep.push((cursor, self.duration));
        }
        keep.retain(|(a, b)| b - a >= MIN_KEEP);
        CutPlan { keep, removed }
    }

    fn text(&self, range: Range<usize>) -> String {
        self.words[range]
            .iter()
            .map(|w| w.text.trim())
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Quiet stretches of at least `min_len` seconds that belong to the transcript gap
    /// `lo..hi`, trimmed to at most `SLACK` past its edges. Without audio, the gap itself.
    fn quiet_near(&self, lo: f64, hi: f64, min_len: f64) -> Vec<(f64, f64)> {
        let Some(runs) = &self.quiet else {
            return if hi - lo >= min_len {
                vec![(lo, hi)]
            } else {
                Vec::new()
            };
        };
        runs.iter()
            .filter(|(a, b)| *b > lo - TOLERANCE && *a < hi + TOLERANCE)
            .map(|(a, b)| (a.max(lo - SLACK), b.min(hi + SLACK)))
            .filter(|(a, b)| b - a >= min_len - 1e-9)
            .collect()
    }

    /// A filler counts as standalone with quiet before and after it (or a file edge).
    fn quiet_around(&self, i: usize) -> bool {
        let w = &self.words;
        let before = i == 0
            || !self
                .quiet_near(w[i - 1].end, w[i].start, FILLER_QUIET)
                .is_empty();
        let after = i + 1 == w.len()
            || !self
                .quiet_near(w[i].end, w[i + 1].start, FILLER_QUIET)
                .is_empty();
        before && after
    }

    /// The cut between kept word `p` and kept word `n` (None at the file's ends) that removes the
    /// words in `removed` and brings the pause down to a breath: (start, end, pause length).
    fn region(
        &self,
        p: Option<usize>,
        removed: Range<usize>,
        n: Option<usize>,
    ) -> Option<(f64, f64, f64)> {
        let w = &self.words;
        let lo = p.map_or(0.0, |i| w[i].end);
        let hi = n.map_or(self.duration, |i| w[i].start);
        let (left_hi, right_lo) = if removed.is_empty() {
            (hi, lo)
        } else {
            (w[removed.start].start, w[removed.end - 1].end)
        };
        let left = p.and_then(|_| self.quiet_near(lo, left_hi, MIN_QUIET).first().copied());
        let right = n.and_then(|_| self.quiet_near(right_lo, hi, MIN_QUIET).last().copied());
        let pause = match (left, right) {
            (Some(l), Some(r)) => r.1 - l.0,
            _ => hi - lo,
        };
        if removed.is_empty()
            && p.is_some()
            && n.is_some()
            && (left.is_none() || right.is_none() || pause < self.settings.min_silence)
        {
            return None;
        }
        let breath = if pause >= 2.0 {
            0.24
        } else if p.is_some_and(|i| ends_sentence(&w[i].text)) {
            0.20
        } else {
            0.14
        };
        let start = match p {
            None => 0.0,
            Some(i) => {
                let keep = if n.is_some() { 0.55 * breath } else { TAIL_PAD };
                // Removed words must go entirely, so the cut starts no later than their sound.
                let (from, limit) = match left {
                    Some((a, b)) => (a, if removed.is_empty() { f64::INFINITY } else { b }),
                    None => (lo, left_hi),
                };
                (from + keep).max(w[i].end + self.settings.pad).min(limit)
            }
        };
        let end = match n {
            None => self.duration,
            Some(_) => {
                let keep = if p.is_some() { 0.45 * breath } else { HEAD_PAD };
                let (to, limit) = match right {
                    Some((a, b)) => (
                        b,
                        if removed.is_empty() {
                            f64::NEG_INFINITY
                        } else {
                            a
                        },
                    ),
                    None => (hi, right_lo),
                };
                (to - keep).max(limit)
            }
        };
        // Quiet may be associated up to SLACK past the next word. Never cut into that word.
        let end = end.min(hi);
        (end - start >= MIN_CUT).then_some((start, end, pause))
    }
}

/// Stutters, retakes and false starts, by Herk Edit Kit's `cut-mistakes` rules.
fn mistakes(w: &[&TranscriptWord], tokens: &[String]) -> Vec<Mistake> {
    let mut out = Vec::new();
    for i in 1..w.len() {
        let (a, b) = (w[i - 1], w[i]);
        if !tokens[i].is_empty() && tokens[i - 1] == tokens[i] && b.start - a.end < 0.5 {
            let emphatic = a.text.trim_end().ends_with([',', ';', '!', '.', '?', '…'])
                || DOUBLING.contains(&tokens[i].as_str());
            let note = if emphatic {
                format!(
                    "\"{} {}\" may be deliberate emphasis",
                    a.text.trim(),
                    b.text.trim()
                )
            } else {
                format!("stutter: repeated \"{}\"", tokens[i])
            };
            out.push(Mistake {
                words: i - 1..i,
                reason: CutReason::Stutter,
                score: 1.0,
                sure: !emphatic,
                note,
            });
        }
    }
    let segments = segments(w);
    let sigs: Vec<Vec<String>> = segments.iter().map(|s| signature(&w[s.clone()])).collect();
    for i in 0..segments.len() {
        for j in i + 1..segments.len() {
            let (si, sj) = (&segments[i], &segments[j]);
            let gap = w[sj.start].start - w[si.end - 1].end;
            if gap > RETAKE_LOOK {
                break;
            }
            let score = jaccard(&sigs[i], &sigs[j]);
            // A shared tail ("web browser") is not an opening. Only a prefix of the
            // content words counts, and the abandoned take must not be the longer one.
            let prefix = opening_prefix(&sigs[i], &sigs[j]);
            let prefix_restart = prefix >= 2 && sigs[i].len() <= sigs[j].len();
            let repeated = repeated_sentences(w, si, sj);
            if !repeated.is_empty() {
                for range in repeated {
                    push_retake(&mut out, w, range, gap, score, prefix_restart, false);
                }
                continue;
            }
            // Far overlap with no repeated sentence is shared vocabulary, not a retake.
            // A trailing phrase in common is not a retake even when the takes are close.
            if gap > RETAKE_GAP || !prefix_restart {
                continue;
            }
            // A prefix restart is a strong hint, so the looser same-opening threshold applies.
            if score < 0.24 {
                continue;
            }
            let span = w[si.end - 1].end - w[si.start].start;
            if span <= RETAKE_SPAN {
                push_retake(&mut out, w, si.clone(), gap, score, true, true);
            }
        }
    }
    out
}

fn push_retake(
    out: &mut Vec<Mistake>,
    w: &[&TranscriptWord],
    range: Range<usize>,
    gap: f64,
    score: f64,
    same_opening: bool,
    allow_false_start: bool,
) {
    if range.is_empty() {
        return;
    }
    let span = w[range.end - 1].end - w[range.start].start;
    if span > RETAKE_SPAN {
        return;
    }
    let false_start = allow_false_start && same_opening && span < 6.0 && gap <= RETAKE_GAP;
    let level = if score >= 0.45 {
        "high"
    } else if score >= 0.34 {
        "medium"
    } else {
        "low"
    };
    let phrase: Vec<&str> = w[range.clone()]
        .iter()
        .take(8)
        .map(|x| x.text.trim())
        .collect();
    out.push(Mistake {
        words: range,
        reason: if false_start {
            CutReason::FalseStart
        } else {
            CutReason::Retake
        },
        score,
        sure: same_opening && score >= 0.45 && gap <= RETAKE_GAP,
        note: format!(
            "{} ({level} confidence, similarity {score:.2}{}); repeated phrase \"{}\"",
            if false_start { "false start" } else { "retake" },
            if same_opening { ", same opening" } else { "" },
            phrase.join(" "),
        ),
    });
}

/// Sentences inside `earlier` whose words occur, in order, inside `later`.
fn repeated_sentences(
    w: &[&TranscriptWord],
    earlier: &Range<usize>,
    later: &Range<usize>,
) -> Vec<Range<usize>> {
    let hay = word_tokens(w, later.clone());
    sentence_ranges(w, earlier)
        .into_iter()
        .filter(|sentence| {
            let needle = word_tokens(w, sentence.clone());
            needle.len() >= 4 && contains_sequence(&hay, &needle)
        })
        .collect()
}

fn sentence_ranges(w: &[&TranscriptWord], seg: &Range<usize>) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut start = seg.start;
    for i in seg.start..seg.end {
        let pause = i + 1 < seg.end && w[i + 1].start - w[i].end > 0.85;
        if ends_sentence(&w[i].text) || pause || i + 1 == seg.end {
            out.push(start..i + 1);
            start = i + 1;
        }
    }
    out
}

fn word_tokens(w: &[&TranscriptWord], range: Range<usize>) -> Vec<String> {
    w[range]
        .iter()
        .map(|x| clean_token(&x.text))
        .filter(|t| !t.is_empty())
        .collect()
}

fn contains_sequence(hay: &[String], needle: &[String]) -> bool {
    !needle.is_empty() && hay.windows(needle.len()).any(|win| win == needle)
}

/// Herk Edit Kit's segments: break on a pause over 0.85 s, at a sentence end once the segment is
/// 7.5 s long, or at 16 s.
fn segments(w: &[&TranscriptWord]) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut start = 0;
    for i in 1..w.len() {
        let gap = w[i].start - w[i - 1].end;
        let elapsed = w[i].end - w[start].start;
        if gap > 0.85 || (ends_sentence(&w[i - 1].text) && elapsed >= 7.5) || elapsed >= 16.0 {
            out.push(start..i);
            start = i;
        }
    }
    if !w.is_empty() {
        out.push(start..w.len());
    }
    out
}

/// Lowercase content words longer than two letters, first 18.
fn signature(words: &[&TranscriptWord]) -> Vec<String> {
    words
        .iter()
        .flat_map(|w| {
            w.text
                .to_lowercase()
                .split(|c: char| !(c.is_alphanumeric() || c == '$' || c == '%'))
                .filter(|t| t.chars().count() > 2 && !STOP.contains(t))
                .map(String::from)
                .collect::<Vec<_>>()
        })
        .take(18)
        .collect()
}

fn jaccard(a: &[String], b: &[String]) -> f64 {
    let a: HashSet<&str> = a.iter().map(String::as_str).collect();
    let b: HashSet<&str> = b.iter().map(String::as_str).collect();
    a.intersection(&b).count() as f64 / a.union(&b).count().max(1) as f64
}

/// How many content words the two signatures share from the start.
fn opening_prefix(a: &[String], b: &[String]) -> usize {
    a.iter().zip(b).take_while(|(x, y)| x == y).count()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 16_000;

    /// A transcript plus audio: a tone inside every word, near-silence elsewhere.
    fn take(words: &[(&str, f64, f64)], duration: f64) -> (Transcript, Vec<f32>) {
        let mut pcm: Vec<f32> = (0..(duration * SR as f64) as usize)
            .map(|i| if i % 2 == 0 { 1e-4 } else { -1e-4 })
            .collect();
        for &(_, start, end) in words {
            for i in (start * SR as f64).round() as usize..(end * SR as f64).round() as usize {
                pcm[i] = 0.3 * (i as f32 * 220.0 * std::f32::consts::TAU / SR as f32).sin();
            }
        }
        let words = words
            .iter()
            .map(|&(text, start, end)| TranscriptWord {
                text: text.into(),
                start,
                end,
                confidence: None,
            })
            .collect();
        (
            Transcript {
                asset: "a".into(),
                provider: "test".into(),
                language: Some("en".into()),
                duration,
                words,
            },
            pcm,
        )
    }

    fn close(a: (f64, f64), b: (f64, f64)) -> bool {
        (a.0 - b.0).abs() < 1e-6 && (a.1 - b.1).abs() < 1e-6
    }

    fn kept(plan: &CutPlan, from: f64, to: f64) -> bool {
        plan.keep.iter().any(|(a, b)| *a <= from && to <= *b)
    }

    fn cut(plan: &CutPlan, from: f64, to: f64) -> bool {
        plan.keep.iter().all(|(a, b)| *b <= from || to <= *a)
    }

    #[test]
    fn pauses_come_down_to_the_right_breath() {
        let (t, pcm) = take(
            &[
                ("Hello", 0.30, 0.70),
                ("there.", 0.75, 1.20),
                ("Next", 3.70, 4.10),
                ("one.", 4.15, 4.50),
                ("two", 5.50, 5.90),
                ("three", 6.30, 6.70),
                ("four.", 7.70, 8.10),
            ],
            9.0,
        );
        let plan = plan_cuts(&t, &pcm, SR, &CutSettings::natural());
        let got: Vec<(f64, f64)> = plan.removed.iter().map(|r| (r.start, r.end)).collect();
        let want = [
            (0.0, 0.08),                  // head pad 0.22
            (1.20 + 0.132, 3.70 - 0.108), // 2.5 s pause keeps 0.24
            (4.50 + 0.11, 5.50 - 0.09),   // after a sentence keeps 0.20
            (6.70 + 0.08, 7.70 - 0.063),  // 0.14 breath, but never closer than pad to a word end
            (8.10 + 0.34, 9.0),           // tail pad 0.34
        ];
        assert_eq!(got.len(), want.len(), "{got:?}");
        for (g, w) in got.iter().zip(want) {
            assert!(close(*g, w), "got {g:?}, want {w:?}");
        }
        assert!(
            plan.removed
                .iter()
                .all(|r| r.reason == CutReason::Silence && !r.review)
        );
        assert!(kept(&plan, 5.90, 6.30), "a 0.4 s pause stays");

        let tight = plan_cuts(&t, &pcm, SR, &CutSettings::tight());
        assert!(
            cut(&tight, 6.0, 6.2),
            "tight style trims the 0.4 s pause too"
        );
    }

    #[test]
    fn fillers_go_only_with_silence_on_both_sides() {
        let (t, pcm) = take(
            &[
                ("So", 0.20, 0.50),
                ("um", 0.90, 1.20),
                ("we", 1.60, 1.90),
                ("go.", 1.95, 2.30),
                ("Then", 3.00, 3.30),
                ("uh", 3.60, 3.90),
                ("this", 3.90, 4.20),
                ("works.", 4.25, 4.60),
            ],
            5.0,
        );
        let plan = plan_cuts(&t, &pcm, SR, &CutSettings::natural());
        let filler = plan
            .removed
            .iter()
            .find(|r| r.reason == CutReason::Filler)
            .expect("um removed");
        assert_eq!(filler.text, "um");
        assert!(
            close((filler.start, filler.end), (0.58, 1.6 - 0.063)),
            "{filler:?}"
        );
        assert!(cut(&plan, 0.9, 1.2));
        assert!(
            kept(&plan, 3.0, 4.6),
            "uh runs straight into the next word, so it stays"
        );
    }

    #[test]
    fn stutters_are_cut_and_emphasis_is_flagged() {
        let (t, pcm) = take(
            &[
                ("I", 0.20, 0.30),
                ("think", 0.30, 0.60),
                ("the", 0.65, 0.75),
                ("the", 0.85, 0.95),
                ("plan", 0.95, 1.30),
                ("works.", 1.35, 1.80),
                ("Never,", 2.00, 2.30),
                ("never", 2.40, 2.70),
                ("quit.", 2.75, 3.10),
            ],
            3.5,
        );
        let plan = plan_cuts(&t, &pcm, SR, &CutSettings::natural());
        let stutter = plan
            .removed
            .iter()
            .find(|r| r.reason == CutReason::Stutter && !r.review)
            .expect("stutter cut");
        assert!(
            stutter.start <= 0.65 && stutter.end >= 0.75 && stutter.end <= 0.85,
            "{stutter:?}"
        );
        assert!(cut(&plan, 0.65, 0.75) && kept(&plan, 0.85, 1.80));
        let flagged = plan
            .removed
            .iter()
            .find(|r| r.review)
            .expect("emphatic doubling flagged");
        assert_eq!(flagged.text, "Never,");
        assert!(kept(&plan, 2.0, 3.1));
    }

    fn sentence(words: &str, from: f64) -> Vec<(&str, f64, f64)> {
        words
            .split(' ')
            .enumerate()
            .map(|(i, w)| (w, from + i as f64 * 0.7, from + i as f64 * 0.7 + 0.5))
            .collect()
    }

    #[test]
    fn retakes_keep_the_last_take() {
        let line = "The new pricing starts next month for every team.";
        let mut words = sentence(line, 0.5);
        words.extend(sentence(line, 8.1));
        words.push(("Thanks.", 14.8, 15.2));
        let (t, pcm) = take(&words, 16.0);
        for audio in [&pcm[..], &[]] {
            let plan = plan_cuts(&t, audio, SR, &CutSettings::natural());
            let retake = plan
                .removed
                .iter()
                .find(|r| r.reason == CutReason::Retake)
                .expect("retake cut");
            assert!(
                !retake.review && retake.text.starts_with("The new pricing"),
                "{retake:?}"
            );
            assert!(cut(&plan, 0.5, 6.1), "first take gone: {:?}", plan.keep);
            assert!(
                kept(&plan, 8.1, 14.2) && kept(&plan, 14.8, 15.2),
                "last take stays: {:?}",
                plan.keep
            );
        }
    }

    #[test]
    fn medium_confidence_retakes_go_to_review() {
        let mut words = sentence("Alpha bravo charlie delta echo.", 0.5);
        words.extend(sentence("Alpha bravo charlie foxtrot golf hotel.", 5.0));
        let (t, pcm) = take(&words, 10.0);
        let plan = plan_cuts(&t, &pcm, SR, &CutSettings::natural());
        let flagged: Vec<&Removal> = plan.removed.iter().filter(|r| r.review).collect();
        assert_eq!(flagged.len(), 1, "{:?}", plan.removed);
        assert!(matches!(
            flagged[0].reason,
            CutReason::Retake | CutReason::FalseStart
        ));
        assert!(
            flagged[0].start <= 0.5 && flagged[0].end >= 3.3 && flagged[0].note.contains("medium")
        );
        assert!(
            kept(&plan, 0.5, 3.3),
            "not cut without review: {:?}",
            plan.keep
        );
    }

    #[test]
    fn retake_review_covers_only_the_repeated_sentence() {
        let repeated = "So the surface of Bennu was really unexpected.";
        let mut words = sentence("We can maintain our orbit.", 70.0);
        words.extend(sentence(repeated, 75.0));
        words.extend(sentence(
            "We had expected to find large areas of sand essentially a beach.",
            82.0,
        ));
        words.extend(sentence(
            "Finding a sample site was very difficult on that rocky surface.",
            95.0,
        ));
        words.extend(sentence(repeated, 140.0));
        words.extend(sentence(
            "We had expected sand but designed the head for a beach of pebbles.",
            148.0,
        ));
        let (t, pcm) = take(&words, 160.0);
        let plan = plan_cuts(&t, &pcm, SR, &CutSettings::natural());
        let retakes: Vec<_> = plan
            .removed
            .iter()
            .filter(|r| matches!(r.reason, CutReason::Retake | CutReason::FalseStart))
            .collect();
        assert!(!retakes.is_empty(), "repeated sentence should be flagged");
        for r in &retakes {
            assert!(
                r.end - r.start < 15.0,
                "span {:.1}s swallows unique speech: {r:?}",
                r.end - r.start
            );
            assert!(
                r.text.contains("surface of Bennu") && !r.text.contains("sample site"),
                "only the repeated sentence, got {}",
                r.text
            );
        }
        assert!(
            kept(&plan, 95.0, 102.0),
            "unique middle stays: {:?}",
            plan.keep
        );
    }

    #[test]
    fn shared_vocabulary_across_a_long_gap_is_not_a_retake() {
        let mut words = sentence(
            "NASA OSIRIS mission will help better answer this question about the asteroid surface.",
            29.0,
        );
        words.extend(sentence(
            "Radiation and heat on a small asteroid change its orbit over time.",
            40.0,
        ));
        words.extend(sentence(
            "Scientists hope the spacecraft data predicts how an asteroid drifts.",
            85.0,
        ));
        let (t, pcm) = take(&words, 100.0);
        let plan = plan_cuts(&t, &pcm, SR, &CutSettings::natural());
        let bad: Vec<_> = plan
            .removed
            .iter()
            .filter(|r| matches!(r.reason, CutReason::Retake | CutReason::FalseStart))
            .collect();
        assert!(
            bad.is_empty(),
            "shared topic words are not a retake: {bad:?}"
        );
    }

    #[test]
    fn silence_cuts_stop_at_the_next_word() {
        let words = [
            ("Here", 1.00, 1.20),
            ("I", 2.00, 2.03),
            ("am", 2.03, 2.06),
            ("using", 2.06, 2.12),
        ];
        let (t, mut pcm) = take(&words, 3.0);
        // Transcript times sit early of the tone, so the quiet run runs through those words.
        let from = (1.20 * SR as f64) as usize;
        let late = (2.12 * SR as f64) as usize;
        for (i, s) in pcm[from..late].iter_mut().enumerate() {
            *s = if i % 2 == 0 { 1e-4 } else { -1e-4 };
        }
        for i in late..(2.40 * SR as f64) as usize {
            pcm[i] = 0.3 * (i as f32 * 220.0 * std::f32::consts::TAU / SR as f32).sin();
        }
        let plan = plan_cuts(&t, &pcm, SR, &CutSettings::natural());
        for r in plan.removed.iter().filter(|r| !r.review) {
            for w in &t.words {
                let overlaps = r.start < w.end - 1e-3 && r.end > w.start + 1e-3;
                assert!(!overlaps, "cut {r:?} overlaps {}", w.text);
            }
        }
        assert!(kept(&plan, 2.00, 2.12), "{:?}", plan.removed);
    }

    #[test]
    fn shared_trailing_phrase_is_not_a_false_start() {
        // Tutorial: "open your web browser." then "Here I am using Chrome web browser."
        // The shared tail must not auto-cut the first sentence.
        let words = [
            ("First,", 29.38, 29.50),
            ("open", 30.60, 30.66),
            ("your", 30.78, 30.80),
            ("web", 30.94, 30.98),
            ("browser.", 31.18, 31.24),
            ("Here", 33.64, 33.78),
            ("I", 33.96, 33.9625),
            ("am", 33.9625, 33.9675),
            ("using", 33.9675, 33.98),
            ("Chrome", 34.84, 34.872),
            ("web", 34.872, 34.888),
            ("browser.", 34.888, 34.92),
        ];
        let (t, pcm) = take(&words, 36.0);
        let plan = plan_cuts(&t, &pcm, SR, &CutSettings::natural());
        assert!(
            kept(&plan, 30.60, 31.24),
            "first sentence cut: {:?}",
            plan.removed
        );
        assert!(
            plan.removed
                .iter()
                .filter(|r| !r.review)
                .all(|r| { !(r.start < 31.24 && r.end > 30.60) }),
            "auto-cut overlaps the sentence: {:?}",
            plan.removed
        );
    }
}
