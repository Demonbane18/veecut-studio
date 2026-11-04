//! The words check: transcribe the delivered render again and compare what is heard with the
//! words the edit should contain. A word masked by music or a sound effect, or clipped by a cut,
//! shows up here. Herk Edit Kit and NextPhase Editor V1 both insist on this check before a video
//! ships (Editor V1's example: an impact on "Claude did." came back as "Flaw dead").

use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::project::{Project, TrackKind};
use crate::transcribe::{self, TranscribeOptions};
use crate::transcript::Transcript;

/// A main-track cut this close to a word may have clipped it (seconds).
const NEAR_CUT: f64 = 0.25;
/// A sound starting this close to a word may have masked it (seconds).
const NEAR_SOUND: f64 = 0.15;
/// Expected and heard words further apart than this never pair up (seconds).
const WINDOW: f64 = 10.0;
/// Alignment cost per second between paired words, so a repeated word pairs with the closest one.
const TIME_COST: f64 = 0.05;

/// An expected word the render does not say as written.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WordIssue {
    /// Timeline seconds where the expected word starts.
    pub at: f64,
    pub expected: String,
    /// What the render was heard saying instead; None when nothing was heard there.
    pub heard: Option<String>,
    pub missing: bool,
    /// A main-track cut lies within 0.25 s of the word.
    pub near_cut: bool,
    /// Item id of a sound effect or music clip that starts within 0.15 s of the word.
    pub near_sound: Option<String>,
}

/// What `check_words` found.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WordsReport {
    /// Transcript words that play on the timeline.
    pub expected: usize,
    /// Expected words heard as written.
    pub matched: usize,
    /// Expected words with nothing heard in their place.
    pub missing: usize,
    /// Expected words heard as something else.
    pub changed: usize,
    pub issues: Vec<WordIssue>,
    /// Who transcribed the render, for example `local:large-v3-turbo`.
    pub provider: String,
    /// One plain sentence about the result.
    pub verdict: String,
}

/// A word with timeline times.
#[derive(Clone)]
struct Spoken {
    text: String,
    start: f64,
    end: f64,
}

/// One normalized piece of a word ("25" is two tokens: "twenty", "five").
struct Token {
    word: usize,
    text: String,
    at: f64,
}

/// Transcribes `render` and compares it with every transcript word kept on the timeline.
///
/// Expected words come from `transcript.json` and every `transcript-*.json` in `project_dir`,
/// mapped through the cuts the way captions and phrase search map them. Without a language in
/// `opts`, the main transcript's language is used.
pub fn check_words(
    project: &Project,
    project_dir: &Path,
    render: &Path,
    opts: &TranscribeOptions,
    progress: &mut dyn FnMut(&str),
) -> Result<WordsReport> {
    if !render.is_file() {
        bail!(
            "{} does not exist; render the project first",
            render.display()
        );
    }
    let transcripts = transcripts(project_dir)?;
    let mut expected: Vec<Spoken> = transcripts
        .iter()
        .flat_map(|t| t.placed_words(project))
        .map(|w| Spoken {
            text: w.text,
            start: w.timeline_start,
            end: w.timeline_end,
        })
        .collect();
    expected.sort_by(|a, b| a.start.total_cmp(&b.start));
    if expected.is_empty() {
        bail!(
            "no transcript words play on this project's timeline; the words check needs the transcript.json that `veecut edit` writes"
        );
    }
    let mut opts = opts.clone();
    if opts.language.is_none() {
        opts.language = transcripts[0].language.clone();
    }
    progress("Transcribing the render to check its words");
    let heard = transcribe::transcribe(render, "render", &opts, progress)
        .with_context(|| format!("transcribing {}", render.display()))?;
    let heard_words: Vec<Spoken> = heard
        .words
        .iter()
        .map(|w| Spoken {
            text: w.text.clone(),
            start: w.start,
            end: w.end,
        })
        .collect();
    let voices: Vec<&str> = transcripts.iter().map(|t| t.asset.as_str()).collect();
    let cuts: Vec<f64> = project
        .main()
        .items
        .iter()
        .skip(1)
        .map(|i| i.start)
        .collect();
    let issues = compare(&expected, &heard_words, &cuts, &sounds(project, &voices));
    let missing = issues.iter().filter(|i| i.missing).count();
    Ok(WordsReport {
        expected: expected.len(),
        matched: expected.len() - issues.len(),
        missing,
        changed: issues.len() - missing,
        verdict: verdict(expected.len(), &issues),
        issues,
        provider: heard.provider,
    })
}

/// `transcript.json` first, then every `transcript-*.json`, sorted by name.
fn transcripts(dir: &Path) -> Result<Vec<Transcript>> {
    let mut extra: Vec<_> = std::fs::read_dir(dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("transcript-") && n.ends_with(".json"))
        })
        .collect();
    extra.sort();
    let main = dir.join("transcript.json");
    let mut paths = Vec::new();
    if main.is_file() {
        paths.push(main);
    }
    paths.append(&mut extra);
    if paths.is_empty() {
        bail!(
            "{} has no transcript.json; the words check compares the render with the transcript `veecut edit` writes",
            dir.display()
        );
    }
    paths.iter().map(|p| Transcript::load(p)).collect()
}

/// (start, item id) of every clip the render mixes in on top of the voices: music, sound effects
/// and anything else on an audible audio track. Overlay tracks are not mixed, so they never mask.
fn sounds(project: &Project, voices: &[&str]) -> Vec<(f64, String)> {
    project
        .tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Audio && !t.muted && !t.hidden)
        .flat_map(|t| &t.items)
        .filter_map(|item| {
            let clip = item.clip()?;
            let audible =
                !clip.muted && clip.volume > 0.0 && !voices.contains(&clip.asset.as_str());
            audible.then(|| (item.start, item.id.clone()))
        })
        .collect()
}

/// Seconds from `t` to the word, 0 inside it.
fn distance(w: &Spoken, t: f64) -> f64 {
    (w.start - t).max(t - w.end).max(0.0)
}

/// Aligns expected and heard words and lists the expected ones not heard as written.
fn compare(
    expected: &[Spoken],
    heard: &[Spoken],
    cuts: &[f64],
    sounds: &[(f64, String)],
) -> Vec<WordIssue> {
    let e = tokens(expected);
    let h = tokens(heard);
    let pairs = align(&e, &h);
    // Per expected word: everything fine so far, and the heard words paired with its tokens.
    let mut ok = vec![true; expected.len()];
    let mut partners: Vec<Vec<usize>> = vec![Vec::new(); expected.len()];
    for (tok, pair) in e.iter().zip(&pairs) {
        match *pair {
            Some((j, same)) => {
                ok[tok.word] &= same;
                if !partners[tok.word].contains(&h[j].word) {
                    partners[tok.word].push(h[j].word);
                }
            }
            None => ok[tok.word] = false,
        }
    }
    expected
        .iter()
        .enumerate()
        .filter(|(i, _)| !ok[*i])
        .map(|(i, w)| {
            let heard_text = (!partners[i].is_empty()).then(|| {
                partners[i]
                    .iter()
                    .map(|&k| heard[k].text.trim())
                    .collect::<Vec<_>>()
                    .join(" ")
            });
            let near_sound = sounds
                .iter()
                .map(|(t, id)| (distance(w, *t), id))
                .filter(|(d, _)| *d <= NEAR_SOUND)
                .min_by(|a, b| a.0.total_cmp(&b.0))
                .map(|(_, id)| id.clone());
            WordIssue {
                at: (w.start * 1000.0).round() / 1000.0,
                expected: w.text.trim().to_string(),
                missing: heard_text.is_none(),
                heard: heard_text,
                near_cut: cuts.iter().any(|&c| distance(w, c) <= NEAR_CUT),
                near_sound,
            }
        })
        .collect()
}

/// The tokens of every word, in order.
fn tokens(words: &[Spoken]) -> Vec<Token> {
    words
        .iter()
        .enumerate()
        .flat_map(|(word, w)| {
            norm(&w.text).into_iter().map(move |text| Token {
                word,
                text,
                at: w.start,
            })
        })
        .collect()
}

/// Lowercase letters and digits; numbers written in digits become spelled words, so "25",
/// "twenty-five" and "Twenty five" all read `twenty five`. "50%" adds `percent`.
fn norm(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let dash = |c: char| matches!(c, '-' | '\u{2010}' | '\u{2011}' | '–' | '—' | '/');
    for piece in text.split(|c: char| c.is_whitespace() || dash(c)) {
        let core = piece.trim_matches(|c: char| !c.is_alphanumeric());
        let number: String = core.chars().filter(|&c| c != ',').collect();
        let numeric = number.starts_with(|c: char| c.is_ascii_digit())
            && number.chars().all(|c| c.is_ascii_digit() || c == '.')
            && number.matches('.').count() <= 1;
        if numeric {
            let (whole, frac) = number.split_once('.').unwrap_or((&number, ""));
            match whole.parse::<u64>() {
                Ok(n)
                    if n < 1_000_000_000_000_000
                        && !(whole.len() > 1 && whole.starts_with('0')) =>
                {
                    spell(n, &mut out)
                }
                _ => digits(whole, &mut out),
            }
            if !frac.is_empty() {
                out.push("point".into());
                digits(frac, &mut out);
            }
        } else {
            let word: String = core
                .chars()
                .filter(|c| c.is_alphanumeric())
                .flat_map(char::to_lowercase)
                .collect();
            if !word.is_empty() {
                out.push(word);
            }
        }
        if piece.contains('%') {
            out.push("percent".into());
        }
    }
    out
}

const ONES: [&str; 20] = [
    "zero",
    "one",
    "two",
    "three",
    "four",
    "five",
    "six",
    "seven",
    "eight",
    "nine",
    "ten",
    "eleven",
    "twelve",
    "thirteen",
    "fourteen",
    "fifteen",
    "sixteen",
    "seventeen",
    "eighteen",
    "nineteen",
];
const TENS: [&str; 10] = [
    "", "", "twenty", "thirty", "forty", "fifty", "sixty", "seventy", "eighty", "ninety",
];

/// English words for `n` without "and": 1205 is `one thousand two hundred five`.
fn spell(n: u64, out: &mut Vec<String>) {
    if n < 20 {
        out.push(ONES[n as usize].into());
    } else if n < 100 {
        out.push(TENS[(n / 10) as usize].into());
        if !n.is_multiple_of(10) {
            out.push(ONES[(n % 10) as usize].into());
        }
    } else {
        let (scale, name) = [
            (1_000_000_000_000, "trillion"),
            (1_000_000_000, "billion"),
            (1_000_000, "million"),
            (1_000, "thousand"),
            (100, "hundred"),
        ]
        .into_iter()
        .find(|(s, _)| n >= *s)
        .expect("n is at least 100");
        spell(n / scale, out);
        out.push(name.into());
        if !n.is_multiple_of(scale) {
            spell(n % scale, out);
        }
    }
}

/// Digit by digit, for decimals, leading zeros and very long numbers.
fn digits(s: &str, out: &mut Vec<String>) {
    for d in s.chars().filter_map(|c| c.to_digit(10)) {
        out.push(ONES[d as usize].into());
    }
}

/// How an alignment cell was reached.
#[derive(Clone, Copy, PartialEq)]
enum Step {
    Start,
    /// An expected token with nothing heard.
    Drop,
    /// A heard token nothing expected (music lyrics, a repeated syllable).
    Extra,
    /// One expected token heard as one token, the same or not.
    Pair,
    /// Two expected tokens heard as one ("every one" heard as "everyone").
    Join,
    /// One expected token heard as two ("cannot" heard as "can not").
    Split,
}

/// Edit-distance alignment of expected tokens `e` with heard tokens `h`, preferring pairs that
/// are close in time. Returns, per expected token, its heard partner and whether they match.
///
/// Each row only holds heard tokens within `WINDOW` seconds of the expected words around it,
/// so memory stays small on long edits.
fn align(e: &[Token], h: &[Token]) -> Vec<Option<(usize, bool)>> {
    let (n, m) = (e.len(), h.len());
    let before = |t: f64| h.partition_point(|x| x.at < t);
    let upto = |t: f64| h.partition_point(|x| x.at <= t);
    // Row i (i expected tokens used) spans heard counts from near the last used expected token
    // to near the next one. Both ends only grow, so neighbouring rows always overlap.
    let mut lo = vec![0usize; n + 1];
    let mut hi = vec![m; n + 1];
    for i in 0..=n {
        if i > 0 {
            lo[i] = before(e[i - 1].at - WINDOW).max(lo[i - 1]);
        }
        if i < n {
            hi[i] = upto(e[i].at + WINDOW).max(lo[i]);
        }
        if i > 0 {
            hi[i] = hi[i].max(hi[i - 1]);
        }
    }
    let mut offset = vec![0usize; n + 2];
    for i in 0..=n {
        offset[i + 1] = offset[i] + hi[i] - lo[i] + 1;
    }
    let cell = |i: usize, j: usize| (j >= lo[i] && j <= hi[i]).then(|| offset[i] + j - lo[i]);
    let mut cost = vec![f64::INFINITY; offset[n + 1]];
    let mut step = vec![Step::Start; offset[n + 1]];
    cost[0] = 0.0;
    let gap = |i: usize, j: usize| (e[i].at - h[j].at).abs();
    for i in 0..=n {
        for j in lo[i]..=hi[i] {
            let here = offset[i] + j - lo[i];
            let mut best = (cost[here], step[here]);
            let mut offer = |from: Option<usize>, add: f64, s: Step| {
                if let Some(c) = from.map(|k| cost[k] + add)
                    && c < best.0
                {
                    best = (c, s);
                }
            };
            if i > 0 && j > 0 && gap(i - 1, j - 1) <= WINDOW {
                let miss = if e[i - 1].text == h[j - 1].text {
                    0.0
                } else {
                    1.0
                };
                offer(
                    cell(i - 1, j - 1),
                    miss + TIME_COST * gap(i - 1, j - 1),
                    Step::Pair,
                );
            }
            if i > 1 && j > 0 && joins(&e[i - 2].text, &e[i - 1].text, &h[j - 1].text) {
                offer(
                    cell(i - 2, j - 1),
                    TIME_COST * gap(i - 2, j - 1),
                    Step::Join,
                );
            }
            if i > 0 && j > 1 && joins(&h[j - 2].text, &h[j - 1].text, &e[i - 1].text) {
                offer(
                    cell(i - 1, j - 2),
                    TIME_COST * gap(i - 1, j - 2),
                    Step::Split,
                );
            }
            if i > 0 {
                offer(cell(i - 1, j), 1.0, Step::Drop);
            }
            if j > 0 {
                offer(cell(i, j - 1), 1.0, Step::Extra);
            }
            (cost[here], step[here]) = best;
        }
    }
    let mut out = vec![None; n];
    let (mut i, mut j) = (n, m);
    while i > 0 || j > 0 {
        let k = cell(i, j).expect("the path stays inside the band");
        match step[k] {
            Step::Start => break,
            Step::Drop => i -= 1,
            Step::Extra => j -= 1,
            Step::Pair => {
                out[i - 1] = Some((j - 1, e[i - 1].text == h[j - 1].text));
                i -= 1;
                j -= 1;
            }
            Step::Join => {
                out[i - 1] = Some((j - 1, true));
                out[i - 2] = Some((j - 1, true));
                i -= 2;
                j -= 1;
            }
            Step::Split => {
                out[i - 1] = Some((j - 2, true));
                i -= 1;
                j -= 2;
            }
        }
    }
    out
}

/// True when `a` followed by `b` spells `whole`.
fn joins(a: &str, b: &str, whole: &str) -> bool {
    whole.len() == a.len() + b.len() && whole.starts_with(a) && whole.ends_with(b)
}

/// The result in plain words, on one line.
fn verdict(expected: usize, issues: &[WordIssue]) -> String {
    if issues.is_empty() {
        return format!("All {expected} expected words are heard in the render.");
    }
    let missing = issues.iter().filter(|i| i.missing).count();
    let mut text = format!(
        "{} of {expected} words did not come through ({missing} missing, {} heard differently).",
        issues.len(),
        issues.len() - missing
    );
    let sound = issues.iter().filter(|i| i.near_sound.is_some()).count();
    let cut = issues.iter().filter(|i| i.near_cut).count();
    if sound > 0 {
        text += &format!(" A sound or music clip starts on {sound} of them: lower or move it.");
    }
    if cut > 0 {
        text +=
            &format!(" A cut sits next to {cut} of them: check that it does not clip the word.");
    }
    if sound + cut == 0 {
        text += " None is next to a cut or a sound start, so listen before changing anything: the transcriber can be wrong too.";
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Words a third of a second apart, starting at `t0`.
    fn words(text: &str, t0: f64) -> Vec<Spoken> {
        text.split_whitespace()
            .enumerate()
            .map(|(i, w)| Spoken {
                text: w.into(),
                start: t0 + i as f64 / 3.0,
                end: t0 + i as f64 / 3.0 + 0.25,
            })
            .collect()
    }

    #[test]
    fn identical_words_have_no_issues() {
        let said = words("Hello, world. This is a test of the words check!", 1.0);
        let heard = words("hello world this is a test of the words check", 1.05);
        assert!(compare(&said, &heard, &[], &[]).is_empty());
        assert_eq!(
            verdict(10, &[]),
            "All 10 expected words are heard in the render."
        );
    }

    #[test]
    fn a_dropped_word_is_missing_at_its_time() {
        let said = words("we built the rocket in a year", 2.0);
        let mut heard = said.clone();
        heard.remove(3);
        let issues = compare(&said, &heard, &[], &[]);
        assert_eq!(issues.len(), 1);
        let i = &issues[0];
        assert_eq!(
            (i.expected.as_str(), i.missing, i.heard.as_deref()),
            ("rocket", true, None)
        );
        assert_eq!(i.at, 3.0);
        assert!(!i.near_cut && i.near_sound.is_none());
    }

    #[test]
    fn a_substituted_word_near_a_cut_and_a_sound_is_flagged() {
        let said = words("and then Claude did.", 0.0);
        let mut heard = said.clone();
        heard[2].text = "Flaw".into();
        let cut = said[2].start - 0.2;
        let sounds = vec![
            (said[2].end + 0.5, "sfx-1".into()),
            (said[2].start + 0.05, "sfx-2".into()),
        ];
        let issues = compare(&said, &heard, &[cut, 9.0], &sounds);
        assert_eq!(issues.len(), 1);
        let i = &issues[0];
        assert_eq!(i.expected, "Claude");
        assert_eq!(i.heard.as_deref(), Some("Flaw"));
        assert!(!i.missing && i.near_cut);
        assert_eq!(i.near_sound.as_deref(), Some("sfx-2"));
        // A cut 0.3 s away is not near.
        let far = compare(&said, &heard, &[said[2].start - 0.3], &[]);
        assert!(!far[0].near_cut && far[0].near_sound.is_none());
    }

    #[test]
    fn a_repeated_word_drop_lands_on_the_right_one() {
        let said = words("go go go now", 0.0);
        // The last "go" (at 0.667 s) is masked; the others are heard where they were said.
        let heard: Vec<Spoken> = said
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != 2)
            .map(|(_, w)| w.clone())
            .collect();
        let issues = compare(&said, &heard, &[], &[]);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].at, 0.667);
        // Now the first one is masked.
        let heard: Vec<Spoken> = said[1..].to_vec();
        let issues = compare(&said, &heard, &[], &[]);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].at, 0.0);
    }

    #[test]
    fn numbers_match_spelled_words() {
        let said = words(
            "about 25 people paid $1,000 for 2.5 hours, 50% of them twice",
            0.0,
        );
        let heard = words(
            "about twenty-five people paid $1000 for two point five hours fifty percent of them twice",
            0.0,
        );
        assert!(compare(&said, &heard, &[], &[]).is_empty());
        assert_eq!(norm("1,205"), ["one", "thousand", "two", "hundred", "five"]);
        assert_eq!(norm("007"), ["zero", "zero", "seven"]);
        assert_eq!(norm("Twenty-Five!"), ["twenty", "five"]);
        // A different number is still a changed word.
        let heard = words("about 26 people", 0.0);
        let issues = compare(&words("about 25 people", 0.0), &heard, &[], &[]);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].heard.as_deref(), Some("26"));
    }

    #[test]
    fn joined_and_split_spellings_match() {
        let said = words("every one of us can not stop", 0.0);
        let heard = words("everyone of us cannot stop", 0.0);
        assert!(compare(&said, &heard, &[], &[]).is_empty());
        assert!(compare(&heard, &said, &[], &[]).is_empty());
    }

    #[test]
    fn only_audible_audio_tracks_count_as_sounds() {
        use crate::project::{Canvas, Clip, Item, ItemBody, VideoKind};
        let mut p = Project::new("t", VideoKind::Documentary, Canvas::landscape());
        let clip = |id: &str, asset: &str, start: f64| Item {
            id: id.into(),
            start,
            duration: 1.0,
            body: ItemBody::Clip(Clip::new(asset, 0.0)),
        };
        let (broll, music, sfx) = ["broll", "music", "sfx"]
            .map(|t| p.track_index(t).unwrap())
            .into();
        // Overlays are not mixed, the transcribed voice is not a sound, muted clips are silent.
        p.tracks[broll].items.push(clip("clip-1", "b", 1.0));
        p.tracks[music].items.push(clip("clip-2", "m", 2.0));
        p.tracks[sfx].items.push(clip("clip-3", "voice", 3.0));
        let mut muted = clip("clip-4", "s", 4.0);
        muted.clip_mut().unwrap().muted = true;
        p.tracks[sfx].items.push(muted);
        assert_eq!(sounds(&p, &["voice"]), [(2.0, "clip-2".to_string())]);
        p.tracks[music].muted = true;
        assert!(sounds(&p, &["voice"]).is_empty());
    }

    #[test]
    fn far_apart_words_never_pair_and_extra_words_are_ignored() {
        let said = words("the launch was delayed", 0.0);
        // The same words 30 s late never pair: all four are missing.
        let heard = words("the launch was delayed", 30.0);
        let issues = compare(&said, &heard, &[], &[]);
        assert_eq!(issues.len(), 4);
        assert!(issues.iter().all(|i| i.missing));
        // Extra heard words around the right ones change nothing.
        let mut heard = words("la la", 50.0);
        heard.splice(0..0, words("the launch was delayed", 0.1));
        assert!(compare(&said, &heard, &[], &[]).is_empty());
    }
}
