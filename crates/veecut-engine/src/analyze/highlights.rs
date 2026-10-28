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

const CONNECTORS: &[&str] = &["and", "but", "so", "because", "also", "then", "or", "um", "uh", "like", "which", "it", "that", "this", "they", "he", "she", "those", "these"];
const CLAIMS: &[&str] = &[
    "never", "always", "best", "worst", "secret", "mistake", "mistakes", "biggest", "nobody", "everyone", "stop", "truth",
    "why", "how", "here's", "most", "only", "every", "wrong", "need", "must",
];
const NUMBER_WORDS: &[&str] = &["one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten", "hundred", "thousand", "million", "billion", "half", "double"];
const STOP: &[&str] = &[
    "the", "and", "that", "this", "with", "have", "from", "they", "were", "what", "when", "your", "just", "like", "about",
    "there", "their", "then", "than", "into", "some", "because", "really", "going", "know", "it's", "that's", "don't",
];

/// The best `count` clips between `min_len` and `max_len` seconds, best first, not overlapping.
///
/// Windows start and end on sentence boundaries. They score for a strong first sentence (a
/// question, a number, a bold claim, talking to "you"; not opening on "and" or "it"), density of
/// the video's topic words (content words said more than once), lively delivery (words per second
/// against the video's average, exclamations, no long dead air) and ending on a full sentence.
pub fn highlights(transcript: &Transcript, min_len: f64, max_len: f64, count: usize) -> Vec<Highlight> {
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
            let content: Vec<String> = window.iter().map(|w| clean_token(&w.text)).filter(|t| is_content(t)).collect();
            let mut topics: Vec<&str> = content.iter().filter(|t| freq[*t] >= 2).map(String::as_str).collect();
            let density = topics.len() as f64 / content.len().max(1) as f64;
            let dead_air: f64 = window.windows(2).map(|p| (p[1].start - p[0].end - 1.0).max(0.0)).sum();
            let rate = window.len() as f64 / len.max(1e-6);
            let shouts = window.iter().filter(|w| w.text.ends_with('!')).count() as f64;
            let energy = ((rate / mean_rate - 0.7) / 0.6 + 0.1 * shouts - dead_air / len).clamp(0.0, 1.0);
            let complete = if ends_sentence(&last.text) { 1.0 } else { 0.0 };
            let score = 0.45 * hook.0 + 0.25 * density.min(1.0) + 0.2 * energy + 0.1 * complete;

            let mut reason = hook.1.clone();
            topics.sort_unstable();
            topics.dedup();
            if !topics.is_empty() {
                reason.push(format!("topic words: {}", topics.iter().take(4).copied().collect::<Vec<_>>().join(", ")));
            }
            if energy >= 0.6 {
                reason.push("lively delivery".into());
            }
            candidates.push(Highlight {
                start: first.start,
                end: last.end,
                score: score as f32,
                title: title(&first.text),
                reason: if reason.is_empty() { "steady section".into() } else { reason.join("; ") },
            });
        }
    }
    candidates.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.start.total_cmp(&b.start)));
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
    if tokens.first().is_some_and(|t| CONNECTORS.contains(&t.as_str())) {
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
            .map(|(i, w)| TranscriptWord { text: w.into(), start: i as f64 * 0.4, end: i as f64 * 0.4 + 0.3, confidence: None })
            .collect();
        let t = Transcript { asset: "a".into(), provider: "test".into(), language: None, duration: 20.0, words };
        let found = highlights(&t, 3.0, 8.0, 2);
        assert_eq!(found.len(), 2);
        assert!(found[0].title.starts_with("Why do 90%"), "{found:?}");
        assert!(found[0].end <= found[1].start || found[1].end <= found[0].start);
    }
}
