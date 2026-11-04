//! Footage analysis: cut decisions, beats, speaker activity, face focus, highlight moments, emphasis words.

mod beats;
mod cuts;
mod face;
mod highlights;

use std::cmp::Reverse;

pub use beats::{Beats, beats};
pub use cuts::{CutPlan, CutReason, CutSettings, Removal, plan_cuts};
pub use face::{face_track, focus};
pub use highlights::{Highlight, highlights, highlights_with_cuts};

use crate::transcript::TranscriptWord;

/// Loudness windows for silence detection are this long, in seconds.
const WINDOW: f64 = 0.02;

/// Samples per loudness window.
fn window_len(sample_rate: u32) -> usize {
    ((sample_rate as f64 * WINDOW).round() as usize).max(1)
}

/// Loudness in dBFS of consecutive windows of `n` samples.
fn levels(pcm: &[f32], n: usize) -> Vec<f32> {
    pcm.chunks(n).map(rms_db).collect()
}

fn rms_db(samples: &[f32]) -> f32 {
    let mean_square = samples.iter().map(|s| s * s).sum::<f32>() / samples.len().max(1) as f32;
    10.0 * (mean_square + 1e-12).log10()
}

/// -40 dBFS, or 18 dB under the median speech level when that is higher (noisy rooms).
fn threshold_from(mut speech_db: Vec<f32>) -> f32 {
    if speech_db.is_empty() {
        return -40.0;
    }
    let mid = speech_db.len() / 2;
    let median = *speech_db.select_nth_unstable_by(mid, f32::total_cmp).1;
    (median - 18.0).max(-40.0)
}

/// The level under which a recording counts as quiet: -40 dBFS, or 18 dB under the median
/// speech level when that is higher. Speech here is every 20 ms window louder than -55 dBFS.
pub fn quiet_threshold(pcm: &[f32], sample_rate: u32) -> f32 {
    let db = levels(pcm, window_len(sample_rate));
    threshold_from(db.into_iter().filter(|d| *d > -55.0).collect())
}

/// Quiet stretches of at least `min_len` seconds, measured in 20 ms windows: (start, end) in
/// seconds. [`quiet_threshold`] gives a good `threshold_db`.
pub fn silences(pcm: &[f32], sample_rate: u32, threshold_db: f32, min_len: f64) -> Vec<(f64, f64)> {
    if sample_rate == 0 || pcm.is_empty() {
        return Vec::new();
    }
    let n = window_len(sample_rate);
    let end = pcm.len() as f64 / sample_rate as f64;
    quiet_runs(
        &levels(pcm, n),
        n as f64 / sample_rate as f64,
        threshold_db,
        min_len,
    )
    .into_iter()
    .map(|(a, b)| (a, b.min(end)))
    .collect()
}

/// Runs of windows under `threshold` lasting at least `min_len` seconds.
fn quiet_runs(db: &[f32], hop: f64, threshold: f32, min_len: f64) -> Vec<(f64, f64)> {
    let mut out = Vec::new();
    let mut start = None;
    for (i, &d) in db.iter().chain([f32::INFINITY].iter()).enumerate() {
        match (d < threshold, start) {
            (true, None) => start = Some(i),
            (false, Some(s)) => {
                let (a, b) = (s as f64 * hop, i as f64 * hop);
                if b - a >= min_len - 1e-9 {
                    out.push((a, b));
                }
                start = None;
            }
            _ => {}
        }
    }
    out
}

/// A word lowercased with leading and trailing punctuation removed (`$` and `%` stay), the way
/// Herk Edit Kit's cut-mistakes compares tokens.
fn clean_token(text: &str) -> String {
    let keep = |c: char| c.is_alphanumeric() || c == '$' || c == '%';
    text.trim_matches(|c: char| !keep(c)).to_lowercase()
}

fn ends_sentence(text: &str) -> bool {
    text.trim_end().ends_with(['.', '!', '?', '…'])
}

/// Index of the loudest speaker per window, for multi-mic podcasts: (start, speaker).
///
/// Each track is measured in 0.5 s windows and leveled against its own speaking level, so a hot
/// mic does not win every window. A new speaker takes over after leading by 3 dB for a full
/// second, and never before the current shot has lasted `min_shot` seconds. Windows where every
/// mic is quiet keep the current speaker.
pub fn speaker_turns(tracks: &[Vec<f32>], sample_rate: u32, min_shot: f64) -> Vec<(f64, usize)> {
    const WIN: f64 = 0.5;
    const LEAD_DB: f32 = 3.0;
    const SILENT_DB: f32 = -50.0;
    if tracks.is_empty() || sample_rate == 0 {
        return Vec::new();
    }
    let n = ((sample_rate as f64 * WIN) as usize).max(1);
    let db: Vec<Vec<f32>> = tracks.iter().map(|t| levels(t, n)).collect();
    // Each mic's speaking level: the 90th percentile of its active windows.
    let offsets: Vec<f32> = db
        .iter()
        .map(|d| {
            let mut active: Vec<f32> = d.iter().copied().filter(|v| *v > SILENT_DB).collect();
            if active.is_empty() {
                return 0.0;
            }
            let k = (active.len() * 9 / 10).min(active.len() - 1);
            *active.select_nth_unstable_by(k, f32::total_cmp).1
        })
        .collect();
    let windows = db.iter().map(Vec::len).max().unwrap_or(0);
    let mut out: Vec<(f64, usize)> = Vec::new();
    let mut shot_start = 0.0;
    let mut challenger: Option<(usize, usize)> = None;
    for w in 0..windows {
        let raw = |i: usize| db[i].get(w).copied().unwrap_or(f32::NEG_INFINITY);
        if (0..db.len()).all(|i| raw(i) <= SILENT_DB) {
            challenger = None;
            continue;
        }
        let level = |i: usize| {
            if raw(i) <= SILENT_DB {
                f32::NEG_INFINITY
            } else {
                raw(i) - offsets[i]
            }
        };
        let lead = (0..db.len())
            .max_by(|&a, &b| level(a).total_cmp(&level(b)))
            .unwrap_or(0);
        let Some(&(_, current)) = out.last() else {
            out.push((0.0, lead));
            continue;
        };
        if lead == current || level(lead) < level(current) + LEAD_DB {
            challenger = None;
            continue;
        }
        match challenger {
            Some((who, first)) if who == lead => {
                let now = w as f64 * WIN;
                if w > first && now >= shot_start + min_shot {
                    let at = (first as f64 * WIN).max(shot_start + min_shot);
                    out.push((at, lead));
                    shot_start = at;
                    challenger = None;
                }
            }
            _ => challenger = Some((lead, w)),
        }
    }
    out
}

/// Words worth emphasizing in captions: numbers, money, names, strong words.
///
/// Money and percentages come first, then numbers, names mid-sentence and a short list of strong
/// words. At most one word in any three is emphasized, so emphasis keeps its punch.
pub fn emphasis(words: &[TranscriptWord]) -> Vec<bool> {
    const MONEY: [&str; 9] = [
        "dollar",
        "dollars",
        "bucks",
        "peso",
        "pesos",
        "euro",
        "euros",
        "percent",
        "porsyento",
    ];
    const NUMBERS: [&str; 44] = [
        "zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten",
        "eleven", "twelve", "fifteen", "twenty", "thirty", "forty", "fifty", "sixty", "seventy",
        "eighty", "ninety", "hundred", "thousand", "million", "billion", "half", "double",
        "triple", "twice", "first", "second", "third", "isa", "dalawa", "tatlo", "apat", "lima",
        "anim", "pito", "walo", "siyam", "sampu", "milyon",
    ];
    const STRONG: [&str; 36] = [
        "never",
        "always",
        "best",
        "worst",
        "free",
        "secret",
        "mistake",
        "mistakes",
        "huge",
        "massive",
        "insane",
        "crazy",
        "easy",
        "fast",
        "faster",
        "biggest",
        "only",
        "stop",
        "must",
        "nobody",
        "everyone",
        "everything",
        "nothing",
        "guaranteed",
        "important",
        "wrong",
        "instantly",
        "exactly",
        "grabe",
        "sobra",
        "libre",
        "sikreto",
        "mali",
        "lahat",
        "wala",
        "talaga",
    ];
    const NOT_NAMES: [&str; 7] = ["i", "i'm", "i'll", "i've", "i'd", "ok", "okay"];
    let rank: Vec<u8> = words
        .iter()
        .enumerate()
        .map(|(i, w)| {
            let raw = w.text.trim();
            let token = clean_token(raw);
            let name = i > 0
                && !ends_sentence(&words[i - 1].text)
                && raw
                    .trim_start_matches(|c: char| !c.is_alphanumeric())
                    .starts_with(char::is_uppercase)
                && !NOT_NAMES.contains(&token.as_str());
            if token.is_empty() {
                0
            } else if raw.contains(['$', '%', '₱', '€', '£']) || MONEY.contains(&token.as_str())
            {
                4
            } else if token.chars().any(|c| c.is_ascii_digit()) || NUMBERS.contains(&token.as_str())
            {
                3
            } else if name {
                2
            } else if STRONG.contains(&token.as_str()) {
                1
            } else {
                0
            }
        })
        .collect();
    let mut order: Vec<usize> = (0..words.len()).filter(|&i| rank[i] > 0).collect();
    order.sort_by_key(|&i| (Reverse(rank[i]), i));
    let mut out = vec![false; words.len()];
    for i in order {
        let near = i.saturating_sub(2)..(i + 3).min(words.len());
        if !out[near].contains(&true) {
            out[i] = true;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(text: &str) -> TranscriptWord {
        TranscriptWord {
            text: text.into(),
            start: 0.0,
            end: 0.0,
            confidence: None,
        }
    }

    #[test]
    fn emphasis_prefers_money_and_spaces_out() {
        let words: Vec<_> = "We saved $500 in two weeks with Notion, the best tool."
            .split(' ')
            .map(word)
            .collect();
        let marked: Vec<&str> = emphasis(&words)
            .iter()
            .zip(&words)
            .filter(|(e, _)| **e)
            .map(|(_, w)| w.text.as_str())
            .collect();
        // "two" is too close to "$500"; Notion is a name; "best" is two words after it.
        assert_eq!(marked, ["$500", "Notion,"]);
    }

    #[test]
    fn speaker_turns_follow_the_louder_mic_with_hysteresis() {
        let sr = 1000;
        let tone = |amp: f32, secs: usize| vec![amp; sr as usize * secs];
        // Mic A is 10 dB hotter overall; B talks from 4 s to 8 s, with a 0.5 s blip from A at 6 s.
        let mut a = tone(0.3, 4);
        a.extend(tone(0.03, 2));
        a.extend(tone(0.3, 1).into_iter().take(500));
        a.extend(tone(0.03, 2).into_iter().take(1500));
        a.extend(tone(0.3, 4));
        let mut b = tone(0.01, 4);
        b.extend(tone(0.1, 4));
        b.extend(tone(0.01, 4));
        let turns = speaker_turns(&[a, b], sr, 2.0);
        assert_eq!(turns, [(0.0, 0), (4.0, 1), (8.0, 0)]);
    }

    #[test]
    fn silences_find_quiet_runs() {
        let sr = 1000;
        let mut pcm = vec![0.5f32; 1000];
        pcm.extend(vec![0.0; 600]);
        pcm.extend(vec![0.5; 400]);
        let found = silences(&pcm, sr, quiet_threshold(&pcm, sr), 0.3);
        assert_eq!(found.len(), 1);
        assert!(
            (found[0].0 - 1.0).abs() < 1e-9 && (found[0].1 - 1.6).abs() < 1e-9,
            "{found:?}"
        );
    }
}
