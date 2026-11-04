//! Highlight moments for short clips: self-contained transcript windows that open strong.

use std::collections::HashMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{clean_token, ends_sentence};
use crate::transcript::Transcript;

/// A candidate clip, in source seconds.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Highlight {
    pub start: f64,
    pub end: f64,
    /// 0 to 1; higher is better.
    pub score: f32,
    /// The opening sentence, shortened.
    pub title: String,
    /// What made it score, in plain words.
    pub reason: String,
}

const CONNECTORS: &[&str] = &[
    "and", "but", "so", "because", "also", "then", "or", "um", "uh", "like", "which", "it", "that",
    "this", "they", "he", "she", "those", "these",
];
const CLAIMS: &[&str] = &[
    "never", "always", "best", "worst", "secret", "mistake", "mistakes", "biggest", "nobody",
    "everyone", "stop", "truth", "why", "how", "here's", "most", "only", "every", "wrong", "need",
    "must",
];
const NUMBER_WORDS: &[&str] = &[
    "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten", "hundred",
    "thousand", "million", "billion", "half", "double",
];
const STOP: &[&str] = &[
    "the", "and", "that", "this", "with", "have", "from", "they", "were", "what", "when", "your",
    "just", "like", "about", "there", "their", "then", "than", "into", "some", "because", "really",
    "going", "know", "it's", "that's", "don't",
];

/// The best `count` clips between `min_len` and `max_len` seconds, best first, not overlapping.
///
/// Windows start and end on sentence boundaries. They score for a strong first sentence (a
/// question, a number, a bold claim, talking to "you"; not opening on "and" or "it"), density of
/// the video's topic words (content words said more than once), lively delivery (words per second
/// against the video's average, exclamations, no long dead air) and ending on a full sentence.
pub fn highlights(
    transcript: &Transcript,
    min_len: f64,
    max_len: f64,
    count: usize,
) -> Vec<Highlight> {
    let sentences = transcript.sentences();
    let words = &transcript.words;
    if sentences.is_empty() || count == 0 {
        return Vec::new();
    }
    let mut freq: HashMap<String, usize> = HashMap::new();
    for w in words {
        let t = clean_token(&w.text);
        if is_content(&t) {
            *freq.entry(t).or_default() += 1;
        }
    }
    let span = (words[words.len() - 1].end - words[0].start).max(1e-6);
    let mean_rate = words.len() as f64 / span;

    let mut candidates = Vec::new();
    for (i, first) in sentences.iter().enumerate() {
        let hook = hook(&first.text);
        for last in &sentences[i..] {
            let len = last.end - first.start;
            if len > max_len {
                break;
            }
            if len < min_len {
                continue;
            }
            let window = &words[first.first..last.end_word];
            let content: Vec<String> = window
                .iter()
                .map(|w| clean_token(&w.text))
                .filter(|t| is_content(t))
                .collect();
            let mut topics: Vec<&str> = content
                .iter()
                .filter(|t| freq[*t] >= 2)
                .map(String::as_str)
                .collect();
            let density = topics.len() as f64 / content.len().max(1) as f64;
            let dead_air: f64 = window
                .windows(2)
                .map(|p| (p[1].start - p[0].end - 1.0).max(0.0))
                .sum();
            let rate = window.len() as f64 / len.max(1e-6);
            let shouts = window.iter().filter(|w| w.text.ends_with('!')).count() as f64;
            let energy =
                ((rate / mean_rate - 0.7) / 0.6 + 0.1 * shouts - dead_air / len).clamp(0.0, 1.0);
            let complete = if ends_sentence(&last.text) { 1.0 } else { 0.0 };
            let score = 0.45 * hook.0 + 0.25 * density.min(1.0) + 0.2 * energy + 0.1 * complete;

            let mut reason = hook.1.clone();
            topics.sort_unstable();
            topics.dedup();
            if !topics.is_empty() {
                reason.push(format!(
                    "topic words: {}",
                    topics
                        .iter()
                        .take(4)
                        .copied()
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            if energy >= 0.6 {
                reason.push("lively delivery".into());
            }
            candidates.push(Highlight {
                start: first.start,
                end: last.end,
                score: score as f32,
                title: title(&first.text),
                reason: if reason.is_empty() {
                    "steady section".into()
                } else {
                    reason.join("; ")
                },
            });
        }
    }
    candidates.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then(a.start.total_cmp(&b.start))
    });
    let mut out: Vec<Highlight> = Vec::new();
    for c in candidates {
        if out.len() == count {
            break;
        }
        if out.iter().all(|h| c.end <= h.start || c.start >= h.end) {
            out.push(c);
        }
    }
    out
}

fn is_content(token: &str) -> bool {
    token.chars().count() > 3 && !STOP.contains(&token)
}

/// How well a first sentence works as a hook, 0 to 1, with the reasons.
fn hook(sentence: &str) -> (f64, Vec<String>) {
    let tokens: Vec<String> = sentence.split_whitespace().map(clean_token).collect();
    let has = |list: &[&str]| tokens.iter().any(|t| list.contains(&t.as_str()));
    let mut score = 0.0;
    let mut why = Vec::new();
    if sentence.trim_end().ends_with('?') {
        score += 0.35;
        why.push("opens with a question".to_string());
    }
    if tokens.iter().any(|t| t.chars().any(|c| c.is_ascii_digit())) || has(NUMBER_WORDS) {
        score += 0.25;
        why.push("opens with a number".into());
    }
    if has(&["you", "your", "you're", "you'll", "ikaw", "kayo", "mo"]) {
        score += 0.2;
        why.push("talks to the viewer".into());
    }
    if has(CLAIMS) {
        score += 0.25;
        why.push("bold claim".into());
    }
    if tokens
        .first()
        .is_some_and(|t| CONNECTORS.contains(&t.as_str()))
    {
        score -= 0.4;
        why.push("opens mid-thought".into());
    }
    (f64::clamp(score, 0.0, 1.0), why)
}

fn title(sentence: &str) -> String {
    const MAX: usize = 60;
    let text = sentence.trim();
    if text.chars().count() <= MAX {
        return text.to_string();
    }
    let mut out = String::new();
    for word in text.split_whitespace() {
        if out.chars().count() + word.chars().count() + 1 > MAX {
            break;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    out + "…"
}

/// Slivers shorter than one 30 fps frame are dropped by the clip laydown.
const FRAME: f64 = 1.0 / 30.0;

/// Like [`highlights`], but length is kept source time inside `keep` (a
/// `plan_cuts` keep list), not the raw span.
///
/// A window starts at a real sentence start (after punctuation, or the first
/// word), not at a pause split and not mid-sentence. A short sentence is
/// skipped only when the next one restarts it with the same opening words.
/// It grows by later sentences until kept time is at least `min_len`, and is
/// dropped if that would pass `max_len`.
pub fn highlights_with_cuts(
    transcript: &Transcript,
    keep: &[(f64, f64)],
    min_len: f64,
    max_len: f64,
    count: usize,
) -> Vec<Highlight> {
    let sentences = transcript.sentences();
    let words = &transcript.words;
    if sentences.is_empty() || count == 0 || max_len <= 0.0 {
        return Vec::new();
    }
    let mut freq: HashMap<String, usize> = HashMap::new();
    for w in words {
        let t = clean_token(&w.text);
        if is_content(&t) {
            *freq.entry(t).or_default() += 1;
        }
    }
    let span = (words[words.len() - 1].end - words[0].start).max(1e-6);
    let mean_rate = words.len() as f64 / span;

    let mut candidates = Vec::new();
    for (i, first) in sentences.iter().enumerate() {
        // A pause without punctuation is not a sentence start. "was | around 100 grams"
        // must not open a clip, and "And ..." is not a false start.
        if first.first > 0 && !ends_sentence(&words[first.first - 1].text) {
            continue;
        }
        if aborted_restart(words, &sentences, i) {
            continue;
        }
        let start = first.start;
        let opening = first.text.clone();
        let hook = hook(&opening);
        let mut chosen = None;
        for (k, last) in sentences.iter().enumerate().skip(i) {
            if !ends_sentence(&last.text) && k + 1 != sentences.len() {
                continue;
            }
            let kept = kept_len(keep, start, last.end);
            if kept > max_len {
                break;
            }
            if kept + 1e-6 >= min_len {
                chosen = Some((last.end, last.end_word, kept));
                break;
            }
        }
        let Some((end, end_word, kept)) = chosen else {
            continue;
        };
        let window = &words[first.first..end_word];
        let content: Vec<String> = window
            .iter()
            .map(|w| clean_token(&w.text))
            .filter(|t| is_content(t))
            .collect();
        let mut topics: Vec<&str> = content
            .iter()
            .filter(|t| freq.get(*t).copied().unwrap_or(0) >= 2)
            .map(String::as_str)
            .collect();
        let density = topics.len() as f64 / content.len().max(1) as f64;
        let dead_air: f64 = window
            .windows(2)
            .map(|p| (p[1].start - p[0].end - 1.0).max(0.0))
            .sum();
        let rate = window.len() as f64 / kept.max(1e-6);
        let shouts = window.iter().filter(|w| w.text.ends_with('!')).count() as f64;
        let energy = ((rate / mean_rate - 0.7) / 0.6 + 0.1 * shouts - dead_air / kept.max(1e-6))
            .clamp(0.0, 1.0);
        let score = 0.45 * hook.0 + 0.25 * density.min(1.0) + 0.2 * energy + 0.1;
        let mut reason = hook.1.clone();
        topics.sort_unstable();
        topics.dedup();
        if !topics.is_empty() {
            reason.push(format!(
                "topic words: {}",
                topics
                    .iter()
                    .take(4)
                    .copied()
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if energy >= 0.6 {
            reason.push("lively delivery".into());
        }
        candidates.push(Highlight {
            start,
            end,
            score: score as f32,
            title: title(&opening),
            reason: if reason.is_empty() {
                "steady section".into()
            } else {
                reason.join("; ")
            },
        });
    }
    candidates.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then(a.start.total_cmp(&b.start))
    });
    let mut out: Vec<Highlight> = Vec::new();
    for c in candidates {
        if out.len() == count {
            break;
        }
        if out.iter().all(|h| c.end <= h.start || c.start >= h.end) {
            out.push(c);
        }
    }
    out
}

fn kept_len(keep: &[(f64, f64)], start: f64, end: f64) -> f64 {
    keep.iter()
        .map(|(a, b)| {
            let d = b.min(end) - a.max(start);
            if d > FRAME { d } else { 0.0 }
        })
        .sum()
}

/// A short sentence the speaker immediately restarts with the same opening words, in a longer
/// take. A clause that merely starts with "And", a pause split, or a repeated line is not one.
fn aborted_restart(
    words: &[crate::transcript::TranscriptWord],
    sentences: &[crate::transcript::Sentence],
    i: usize,
) -> bool {
    let s = &sentences[i];
    if s.end_word - s.first > 6 || s.end - s.start > 6.0 {
        return false;
    }
    let Some(next) = sentences.get(i + 1) else {
        return false;
    };
    if next.start - s.end > 3.0 {
        return false;
    }
    let a = opening_words(words, s.first, s.end_word);
    let b = opening_words(words, next.first, next.end_word);
    // The aborted take is shorter than the restart; equal lines are repetition, not a false start.
    a.len() == 2 && a == b && next.end_word - next.first > s.end_word - s.first
}

fn opening_words(
    words: &[crate::transcript::TranscriptWord],
    from: usize,
    to: usize,
) -> Vec<String> {
    words[from..to]
        .iter()
        .map(|w| clean_token(&w.text))
        .filter(|t| !t.is_empty())
        .take(2)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transcript::TranscriptWord;

    #[test]
    fn strong_openings_win_and_windows_do_not_overlap() {
        let text = "And then we went home. It was late. Why do 90% of creators quit in year one? \
                    Creators quit because growth feels slow. Growth comes from posting every week. \
                    So that was the day. We ate dinner.";
        let words: Vec<TranscriptWord> = text
            .split_whitespace()
            .enumerate()
            .map(|(i, w)| TranscriptWord {
                text: w.into(),
                start: i as f64 * 0.4,
                end: i as f64 * 0.4 + 0.3,
                confidence: None,
            })
            .collect();
        let t = Transcript {
            asset: "a".into(),
            provider: "test".into(),
            language: None,
            duration: 20.0,
            words,
        };
        let found = highlights(&t, 3.0, 8.0, 2);
        assert_eq!(found.len(), 2);
        assert!(found[0].title.starts_with("Why do 90%"), "{found:?}");
        assert!(found[0].end <= found[1].start || found[1].end <= found[0].start);
    }

    fn words_of(text: &str) -> Transcript {
        let words: Vec<TranscriptWord> = text
            .split_whitespace()
            .enumerate()
            .map(|(i, w)| TranscriptWord {
                text: w.into(),
                start: i as f64 * 0.4,
                end: i as f64 * 0.4 + 0.15,
                confidence: None,
            })
            .collect();
        let duration = words.last().map_or(0.0, |w| w.end);
        Transcript {
            asset: "a".into(),
            provider: "test".into(),
            language: None,
            duration,
            words,
        }
    }

    #[test]
    fn window_starts_at_the_sentence_not_inside_a_false_start() {
        let t = words_of(
            "We have done, we just, we do two positions of the spacecraft. \
             We spin the spacecraft and learn its inertia. \
             We repeat that measurement before we go down.",
        );
        let keep = vec![(0.0, t.duration)];
        let found = highlights_with_cuts(&t, &keep, 1.0, 30.0, 1);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(
            found[0].title.starts_with("We have done"),
            "window left the sentence start: {found:?}"
        );
        assert!(found[0].start < 0.05, "{found:?}");
    }

    #[test]
    fn window_extends_until_kept_time_meets_the_floor() {
        let t = words_of(&"Creators can learn. ".repeat(50));
        let keep: Vec<_> = t.words.iter().map(|w| (w.start, w.end)).collect();
        let found = highlights_with_cuts(&t, &keep, 20.0, 60.0, 1);
        assert_eq!(found.len(), 1, "{found:?}");
        let kept: f64 = keep
            .iter()
            .map(|(a, b)| (b.min(found[0].end) - a.max(found[0].start)).max(0.0))
            .sum();
        assert!(
            (20.0..=60.0).contains(&kept),
            "kept {kept} from {:.2}-{:.2}",
            found[0].start,
            found[0].end
        );
    }

    #[test]
    fn pause_split_does_not_drop_the_sentence_head() {
        // Dale clip 2: whisper paused 0.82 s after "was", so "around 100 grams" looks
        // like its own sentence. The window must still start at "And".
        let raw = [
            ("And", 371.92, 372.52),
            ("the", 372.62, 373.06),
            ("estimate", 373.06, 373.36),
            ("that", 373.46, 373.60),
            ("came", 373.72, 373.92),
            ("in", 373.92, 374.16),
            ("after", 374.26, 374.37),
            ("match", 374.37, 374.48),
            ("point", 374.62, 374.86),
            ("rehearsal", 375.00, 375.46),
            ("from", 375.58, 375.79),
            ("the", 375.79, 375.94),
            ("science", 376.06, 376.34),
            ("team", 376.52, 376.78),
            ("was", 377.28, 378.02),
            ("around", 378.84, 379.54),
            ("100", 379.54, 380.06),
            ("grams.", 380.06, 380.38),
        ];
        let words: Vec<TranscriptWord> = raw
            .iter()
            .map(|&(text, start, end)| TranscriptWord {
                text: text.into(),
                start,
                end,
                confidence: None,
            })
            .collect();
        let t = Transcript {
            asset: "a".into(),
            provider: "test".into(),
            language: None,
            duration: 380.38,
            words,
        };
        let found = highlights_with_cuts(&t, &[(371.92, 380.38)], 1.0, 30.0, 1);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(
            found[0].title.starts_with("And"),
            "started mid-sentence: {found:?}"
        );
        assert!(found[0].start <= 371.92 + 0.01, "{found:?}");
        assert!(found[0].end >= 380.0, "dropped the rest: {found:?}");
    }
}
