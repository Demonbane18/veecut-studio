//! The documentary edit: a talking head cut into a fast stream of found material. Every found
//! picture is re-coloured while the speaker's own camera stays raw, cut effects sit between
//! pictures, small words appear as they are said, every picture change gets a click, and the
//! music bed is cut under the punchline instead of ducked. The pacing, layering and sound numbers
//! are NextPhase Editor V1's published measurements (see CREDITS.md); the code is VeeCut's own.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use anyhow::{Result, bail};

use super::speech::{cut_summary, note_reviews, set_focus, source_video, stem};
use super::{Job, SAMPLE_RATE, add_captions, audio_files, pcm, track_face};
use crate::analyze;
use crate::footage::{self, MediaKind, Provider, SearchOptions};
use crate::matte;
use crate::media;
use crate::ops::{self, Op};
use crate::project::{
    AssetKind, AssetRole, CaptionMode, Color, KenBurns, Point, Project, TextPreset, Transition,
    TransitionKind, TreatLook, Treatment,
};
use crate::sfx::{self, Cue, Sound};
use crate::style::Sound as StyleSound;
use crate::transcript::{PlacedWord, Transcript, TranscriptWord};

/// Seconds per found picture (shortest, aim, longest). Editor V1's references change the picture
/// every 0.6 to 1.5 s (held-shot medians 0.56 to 0.79 s).
const FAST: (f64, f64, f64) = (0.6, 0.95, 1.5);
/// Long videos keep the cold open at full pace and let the body breathe to 1.2 to 2.0 s.
const BODY: (f64, f64, f64) = (1.2, 1.6, 2.0);
/// The speaker's own face between runs of pictures: 1.2 to 2.5 s stretches.
const FACE: (f64, f64, f64) = (1.2, 1.8, 2.5);
/// Seconds of pictures per run. With face beats of about 1.8 s this puts the face on screen
/// about 30 percent of the time (Editor V1 aims for 25 to 35).
const RUN: f64 = 4.0;
/// Videos longer than this are long-form.
const LONG_FORM: f64 = 180.0;
/// The cold open keeps the fast pace even in long-form.
const COLD_OPEN: f64 = 10.0;
/// A steady push on found material: 4 to 7.5 percent per second.
const PUSH_PER_SECOND: f64 = 0.055;
/// The music bed sits this many LU under the voice, cut on structure rather than ducked.
const BED_UNDER_LU: f32 = 5.5;
/// At most this many files are downloaded by `--find` for one edit.
const FIND_LIMIT: usize = 12;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BeatKind {
    Face,
    Material,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Beat {
    kind: BeatKind,
    start: f64,
    end: f64,
    /// Which run of pictures (material) or which gap between runs (face) this beat belongs to.
    run: usize,
    /// First picture of its run.
    first_in_run: bool,
}

impl Beat {
    fn len(&self) -> f64 {
        self.end - self.start
    }
}

/// The next cut after `from`: the word onset inside [min, max] closest to `aim`, or `aim` itself
/// when no word starts in that window. A last piece shorter than `min` joins the one before.
fn next_cut(onsets: &[f64], from: f64, (min, aim, max): (f64, f64, f64), total: f64) -> f64 {
    let want = from + aim;
    let best = onsets
        .iter()
        .copied()
        .filter(|&o| o >= from + min - 1e-9 && o <= from + max + 1e-9)
        .min_by(|a, b| (a - want).abs().total_cmp(&(b - want).abs()));
    let t = best.unwrap_or(want).min(total);
    if total - t < min { total } else { t }
}

/// Splits the timeline into runs of found pictures and face beats, cutting on word onsets. It
/// opens on pictures (the cold open), never puts two face beats in a row, and without any
/// material keeps the speaker on screen throughout.
fn plan_beats(onsets: &[f64], total: f64, has_material: bool) -> Vec<Beat> {
    if !has_material || total <= 0.0 {
        return vec![Beat {
            kind: BeatKind::Face,
            start: 0.0,
            end: total.max(0.0),
            run: 0,
            first_in_run: true,
        }];
    }
    let long = total > LONG_FORM;
    let mut beats = Vec::new();
    let (mut t, mut run, mut face_next) = (0.0, 0usize, false);
    while total - t > 1e-6 {
        if face_next {
            let end = next_cut(onsets, t, FACE, total);
            beats.push(Beat {
                kind: BeatKind::Face,
                start: t,
                end,
                run,
                first_in_run: true,
            });
            t = end;
            face_next = false;
            continue;
        }
        let pace = if !long || t < COLD_OPEN { FAST } else { BODY };
        let goal = t + RUN;
        let mut first = true;
        loop {
            let end = next_cut(onsets, t, pace, total);
            beats.push(Beat {
                kind: BeatKind::Material,
                start: t,
                end,
                run,
                first_in_run: first,
            });
            first = false;
            t = end;
            if t >= goal - pace.0 * 0.5 || total - t <= 1e-6 {
                break;
            }
        }
        run += 1;
        face_next = true;
    }
    beats
}

/// Lower-case letters and digits only, for matching words against the lists below.
fn norm(word: &str) -> String {
    word.chars()
        .filter(|c| c.is_alphanumeric() || *c == '\'')
        .collect::<String>()
        .to_lowercase()
}

const WARM_WORDS: &[&str] = &[
    "money", "dollar", "dollars", "cash", "price", "prices", "gold", "fire", "burn", "burning",
    "history", "war", "old", "million", "millions", "billion", "billions", "rich", "pay", "paid",
    "cost", "costs", "pera", "presyo",
];
const MONO_WORDS: &[&str] = &[
    "document",
    "documents",
    "paper",
    "law",
    "report",
    "data",
    "news",
    "archive",
    "contract",
    "rule",
    "rules",
    "study",
    "research",
    "fact",
    "facts",
    "percent",
    "number",
    "numbers",
    "serious",
    "truth",
];
const COLD_WORDS: &[&str] = &[
    "code",
    "computer",
    "server",
    "servers",
    "tech",
    "technology",
    "ai",
    "model",
    "models",
    "machine",
    "night",
    "space",
    "digital",
    "internet",
    "online",
    "software",
    "app",
    "apps",
    "phone",
    "robot",
    "agent",
    "agents",
    "editor",
];
const TEAL_WORDS: &[&str] = &[
    "city",
    "place",
    "office",
    "building",
    "company",
    "street",
    "country",
    "world",
    "home",
    "house",
    "town",
    "headquarters",
    "market",
    "school",
    "studio",
];
const GLITCH_WORDS: &[&str] = &[
    "broke", "broken", "break", "leak", "leaked", "leaks", "glitch", "crash", "crashed", "hack",
    "hacked", "error", "bug", "bugs", "fail", "failed", "failure", "wrong", "sira",
];
const BLACK_WORDS: &[&str] = &["nothing", "nobody", "none", "wala"];
const CTA_WORDS: &[&str] = &[
    "follow",
    "subscribe",
    "comment",
    "comments",
    "link",
    "like",
    "share",
    "bio",
];
const NUMBER_WORDS: &[&str] = &[
    "zero",
    "hundred",
    "hundreds",
    "thousand",
    "thousands",
    "million",
    "millions",
    "billion",
    "billions",
    "percent",
    "half",
    "double",
    "triple",
];
const STOP_WORDS: &[&str] = &[
    "the",
    "and",
    "but",
    "that",
    "this",
    "these",
    "those",
    "with",
    "from",
    "into",
    "about",
    "what",
    "when",
    "where",
    "which",
    "while",
    "there",
    "their",
    "they",
    "them",
    "then",
    "than",
    "have",
    "just",
    "really",
    "very",
    "like",
    "will",
    "would",
    "could",
    "should",
    "because",
    "been",
    "being",
    "were",
    "your",
    "yours",
    "ours",
    "it's",
    "i'm",
    "mine",
    "here",
    "also",
    "only",
    "even",
    "still",
    "make",
    "made",
    "going",
    "gonna",
    "want",
    "know",
    "think",
    "thing",
    "things",
    "something",
    "anything",
    "everything",
    "today",
    "right",
    "okay",
    "well",
    "yeah",
    "actually",
    "basically",
    "literally",
    "let's",
    "lets",
    "every",
    "much",
    "many",
    "more",
    "most",
    "some",
    "does",
    "doesn't",
    "don't",
    "didn't",
    "can't",
    "won't",
    "you're",
    "we're",
    "they're",
    "that's",
    "what's",
    "there's",
    "first",
    "second",
    "third",
    "next",
    "last",
    "time",
    "people",
    "yung",
    "naman",
    "talaga",
    "kasi",
    "pero",
];

fn has_any(words: &[&str], list: &[&str]) -> bool {
    words.iter().any(|w| list.contains(w))
}

/// The colour treatment for a found picture: chosen from what the line says when it names a
/// kind of thing, otherwise rotated so roughly 40 percent stay natural (Editor V1 measured teal
/// about 22, mono about 18, warm 8 to 11 and natural 32 to 42 percent).
fn grade_for(words: &[&str], shot: usize, is_image: bool) -> Treatment {
    const ROTATION: [TreatLook; 8] = [
        TreatLook::Natural,
        TreatLook::Teal,
        TreatLook::Natural,
        TreatLook::Mono,
        TreatLook::Natural,
        TreatLook::Teal,
        TreatLook::Warm,
        TreatLook::Mono,
    ];
    let look = if has_any(words, WARM_WORDS) {
        TreatLook::Warm
    } else if has_any(words, MONO_WORDS) {
        TreatLook::Mono
    } else if has_any(words, COLD_WORDS) {
        TreatLook::Cold
    } else if has_any(words, TEAL_WORDS) {
        TreatLook::Teal
    } else {
        ROTATION[shot % ROTATION.len()]
    };
    Treatment {
        look,
        halftone: if is_image && look != TreatLook::Mono {
            0.22
        } else {
            0.0
        },
        vignette: if look == TreatLook::Natural {
            0.35
        } else {
            0.25
        },
        recolor: None,
        pixelate: 0.0,
    }
}

/// How a picture comes in: tiles for the very first frame and every third run, a double
/// negative into a new chapter, a rack focus at other run starts, a datamosh on "broke" or
/// "leaked", a one-frame white box on every third cut inside a run, and now and then a whip
/// out of a long shot. Lengths are Editor V1's frame counts.
fn transition_for(
    beat: &Beat,
    index_in_run: usize,
    first_picture: bool,
    chapter: bool,
    glitch: bool,
    whip: bool,
    fps: f64,
) -> Option<Transition> {
    let frames = |n: f64| n / fps;
    let t = |kind, n| {
        Some(Transition {
            kind,
            duration: frames(n),
        })
    };
    if first_picture {
        return t(TransitionKind::Tiles, 9.0);
    }
    if glitch {
        return t(TransitionKind::Glitch, 4.0);
    }
    if beat.first_in_run {
        return if chapter {
            t(TransitionKind::Negative, 4.0)
        } else if beat.run % 3 == 2 {
            t(TransitionKind::Tiles, 9.0)
        } else {
            t(TransitionKind::Focus, 9.0)
        };
    }
    if whip {
        return t(TransitionKind::Whip, 4.0);
    }
    if index_in_run % 3 == 2 {
        return t(TransitionKind::Flash, 1.0);
    }
    None
}

/// Words that say how, how much or what someone did rather than what is there: no footage
/// shows them, so they never go into a search.
const VAGUE_WORDS: &[&str] = &[
    "ability",
    "able",
    "important",
    "mean",
    "means",
    "kind",
    "sort",
    "part",
    "point",
    "idea",
    "fact",
    "case",
    "lots",
    "reason",
    "reasons",
    "stuff",
    "exciting",
    "amazing",
    "great",
    "good",
    "better",
    "best",
    "bigger",
    "huge",
    "little",
    "small",
    "whole",
    "different",
    "same",
    "other",
    "another",
    "certain",
    "sure",
    "probably",
    "maybe",
    "always",
    "never",
    "often",
    "ever",
    "already",
    "again",
    "around",
    "through",
    "across",
    "toward",
    "towards",
    "until",
    "since",
    "farther",
    "further",
    "enough",
    "need",
    "needs",
    "took",
    "take",
    "takes",
    "give",
    "gives",
    "going",
    "doing",
    "done",
    "said",
    "says",
    "tell",
    "told",
    "look",
    "looks",
    "start",
    "starts",
    "started",
    "work",
    "works",
    "worked",
    "come",
    "comes",
    "came",
    "back",
    "conditions",
    "precautions",
    "everybody",
    "everyone",
    "someone",
    "somebody",
    "anyone",
    "prove",
    "proves",
    "proved",
    "show",
    "shows",
    "showed",
    "make",
    "makes",
    "made",
    "want",
    "wants",
    "wanted",
    "know",
    "knows",
    "knew",
    "think",
    "thinks",
    "thought",
    "carry",
    "carries",
    "help",
    "helps",
    "keep",
    "keeps",
    "kept",
    "lets",
    "uses",
    "used",
    "tries",
    "tried",
    "find",
    "finds",
    "found",
    "become",
    "becomes",
    "became",
    "seem",
    "seems",
    "feel",
    "feels",
    "felt",
    "call",
    "calls",
    "called",
    "happen",
    "happens",
    "happened",
    "allow",
    "allows",
    "provide",
    "provides",
    "bring",
    "brings",
    "brought",
    "move",
    "moves",
    "turn",
    "turns",
    "talk",
    "talks",
    "goes",
    "went",
    "gone",
    "gets",
    "getting",
    "having",
    "being",
    "trying",
    "making",
    "taking",
    "coming",
    "looking",
    "thing",
    "things",
    "something",
    "anything",
    "everything",
    "nothing",
    "pretty",
    "quite",
    "right",
    "okay",
    "yeah",
    "time",
    "times",
    "today",
    "year",
    "years",
    "people",
    "guys",
    "folks",
    "ways",
];

/// A word that names something a camera could show: not a stop word, not vague, not an
/// adverb, at least four letters (or an acronym like SLS).
fn concrete(text: &str) -> Option<String> {
    let n = norm(text);
    let letters = text.chars().filter(|c| c.is_alphabetic()).count();
    let acronym = letters >= 3
        && text
            .chars()
            .filter(|c| c.is_alphabetic())
            .all(char::is_uppercase);
    if n.chars().any(|c| c.is_ascii_digit()) || text.contains(['\'', '’']) {
        return None;
    }
    if acronym {
        return Some(n);
    }
    let adverb = n.ends_with("ly") && n.len() > 5;
    let adjective = ["able", "ible", "ous", "ful"]
        .iter()
        .any(|s| n.ends_with(s) && n.len() > 6);
    (n.len() >= 4
        && !adverb
        && !adjective
        && !STOP_WORDS.contains(&n.as_str())
        && !VAGUE_WORDS.contains(&n.as_str()))
    .then_some(n)
}

/// The video's subject: acronyms (even once), then repeated concrete words, best first.
fn topic_terms(words: &[PlacedWord]) -> Vec<String> {
    // ponytail: lexical subject ranking; semantic search can replace it when keyword recall falls short.
    // word -> (acronym, times said, first index)
    let mut seen: BTreeMap<String, (bool, usize, usize)> = BTreeMap::new();
    for (k, w) in words.iter().enumerate() {
        let Some(n) = concrete(&w.text) else { continue };
        let acronym = w
            .text
            .chars()
            .filter(|c| c.is_alphabetic())
            .all(char::is_uppercase);
        let e = seen.entry(n).or_insert((false, 0, k));
        e.0 |= acronym;
        e.1 += 1;
    }
    let mut ranked: Vec<_> = seen
        .into_iter()
        .filter(|(_, (acronym, said, _))| *acronym || *said >= 2)
        .collect();
    ranked.sort_by(|a, b| {
        b.1.0
            .cmp(&a.1.0)
            .then(b.1.1.cmp(&a.1.1))
            .then(a.1.2.cmp(&b.1.2))
    });
    ranked.into_iter().take(4).map(|(n, _)| n).collect()
}

/// A short search query for a run: its own name or stressed concrete word, anchored with the
/// video's main subject, so a line about "the test" finds rockets, not gyms. Vague runs
/// rotate secondary terms while keeping the same subject anchor.
fn query_for(
    words: &[&PlacedWord],
    emphasis: &[bool],
    topic: &[String],
    run: usize,
) -> Option<String> {
    let mut scored: Vec<(i32, String)> = Vec::new();
    for (k, w) in words.iter().enumerate() {
        let Some(n) = concrete(&w.text) else { continue };
        let capital = k > 0 && w.text.chars().next().is_some_and(char::is_uppercase);
        let in_topic = topic.contains(&n);
        let score = 10 * capital as i32
            + 6 * in_topic as i32
            + 5 * emphasis.get(k).copied().unwrap_or(false) as i32
            + n.len().min(9) as i32;
        if !scored.iter().any(|(_, s)| *s == n) {
            scored.push((score, n));
        }
    }
    scored.sort_by(|a, b| b.0.cmp(&a.0));
    match scored.into_iter().next().map(|(_, s)| s) {
        Some(own) => Some(match topic.first() {
            Some(main) if *main != own => format!("{main} {own}"),
            _ => own,
        }),
        None => topic.first().map(|main| {
            let other = &topic[run % topic.len()];
            if main == other {
                main.clone()
            } else {
                format!("{main} {other}")
            }
        }),
    }
}

/// Title words that keep a result out of an automatic edit whatever the query was (matched at
/// the start of a title word). The user can still find and add anything by hand.
const UNSAFE_WORDS: &[&str] = &[
    "molest", "abuse", "rape", "porn", "nude", "naked", "sexual", "murder", "suicide", "corpse",
    "behead", "tortur", "massacre", "genocide", "lynch",
];

// ponytail: common English plural suffixes only; keep whole-word matching for unrelated stems.
fn singular(word: &str) -> &str {
    if word.len() > 3 {
        word.strip_suffix("es")
            .or_else(|| word.strip_suffix('s'))
            .unwrap_or(word)
    } else {
        word
    }
}

/// Requires the query's subject (its first word) as a whole title word and rejects unsafe titles.
/// Full-text search on Commons and the Archive also matches descriptions and unrelated names.
fn relevant(title: &str, query: &str) -> bool {
    let title = title.to_lowercase();
    let words: Vec<&str> = title
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 3)
        .collect();
    if words
        .iter()
        .any(|w| UNSAFE_WORDS.iter().any(|u| w.starts_with(u)))
    {
        return false;
    }
    query
        .split_whitespace()
        .next()
        .is_some_and(|subject| words.iter().any(|word| singular(word) == singular(subject)))
}

/// A word worth a shock: money, a number with a unit, or a big quantity.
fn is_number(word: &str) -> bool {
    let n = norm(word);
    word.contains(['$', '%', '₱', '€', '£'])
        || n.chars().filter(|c| c.is_ascii_digit()).count() >= 2
        || NUMBER_WORDS.contains(&n.as_str())
}

/// A word strong enough to stand alone on screen: a number worth a shock, an acronym, or a
/// concrete word of five letters or more.
fn display_worthy(word: &str) -> bool {
    is_number(word)
        || concrete(word).is_some_and(|n| {
            n.len() >= 5
                || word
                    .chars()
                    .filter(|c| c.is_alphabetic())
                    .all(char::is_uppercase)
        })
}

#[derive(Clone, Debug, PartialEq)]
struct Display {
    preset: TextPreset,
    text: String,
    start: f64,
    end: f64,
}

/// Display words on the found pictures: one stressed word per run as a keyword, a shock word
/// once or twice a minute for money and numbers, and a black card when the speaker says
/// "nothing" or "no one". Never on a face beat, never two at once.
fn display_words(
    words: &[PlacedWord],
    emphasis: &[bool],
    beats: &[Beat],
    total: f64,
) -> Vec<Display> {
    let mut out: Vec<Display> = Vec::new();
    let shock_budget = ((total / 60.0) * 1.5).round().max(1.0) as usize;
    let mut black = 0usize;
    let material = |t: f64| {
        beats
            .iter()
            .find(|b| t >= b.start && t < b.end)
            .filter(|b| b.kind == BeatKind::Material)
    };
    let free = |out: &Vec<Display>, a: f64, b: f64| out.iter().all(|d| b <= d.start || a >= d.end);
    let mut runs_with_word = HashSet::new();
    for (k, w) in words.iter().enumerate() {
        let n = norm(&w.text);
        let next_is_one = words.get(k + 1).is_some_and(|x| norm(&x.text) == "one");
        let Some(beat) = material(w.timeline_start) else {
            continue;
        };
        let clean = w
            .text
            .trim_matches(|c: char| !c.is_alphanumeric() && !"$%₱€£".contains(c))
            .to_string();
        if clean.is_empty() {
            continue;
        }
        if black < 2 && (BLACK_WORDS.contains(&n.as_str()) || (n == "no" && next_is_one)) {
            let end = (w.timeline_start + 1.0)
                .min(beat.end.max(w.timeline_end + 0.3))
                .min(total);
            let text = if n == "no" {
                "no one".to_string()
            } else {
                clean.to_lowercase()
            };
            if free(&out, w.timeline_start, end) {
                out.push(Display {
                    preset: TextPreset::BlackCard,
                    text,
                    start: w.timeline_start,
                    end,
                });
                black += 1;
                runs_with_word.insert(beat.run);
            }
            continue;
        }
        if !emphasis.get(k).copied().unwrap_or(false)
            || runs_with_word.contains(&beat.run)
            || !display_worthy(&w.text)
        {
            continue;
        }
        let shocks = out.iter().filter(|d| d.preset == TextPreset::Shock).count();
        let (preset, len) = if is_number(&w.text) && shocks < shock_budget {
            (TextPreset::Shock, 0.8)
        } else {
            (TextPreset::Keyword, 1.2)
        };
        let end = (w.timeline_start + len).min(total).max(w.timeline_end);
        if end - w.timeline_start < 0.4 || !free(&out, w.timeline_start, end) {
            continue;
        }
        let text = if preset == TextPreset::Shock {
            clean.to_uppercase()
        } else {
            clean
        };
        out.push(Display {
            preset,
            text,
            start: w.timeline_start,
            end,
        });
        runs_with_word.insert(beat.run);
    }
    out
}

/// The line the bed cuts out under: the last short sentence in the back half that is not a call
/// to action. Returns its timeline start, the end of the cut (the next line's start, or the end)
/// and its text.
fn punchline(sentences: &[(f64, f64, String)], total: f64) -> Option<(f64, f64, String)> {
    let candidate = sentences
        .iter()
        .enumerate()
        .skip(1)
        .rev()
        .find(|(_, (start, _, text))| {
            let words: Vec<String> = text.split_whitespace().map(norm).collect();
            *start >= total * 0.4
                && words.len() <= 12
                && !words.iter().any(|w| CTA_WORDS.contains(&w.as_str()))
        })?;
    let (i, (start, _, text)) = candidate;
    let cut_end = sentences.get(i + 1).map(|s| s.0 - 0.02).unwrap_or(total);
    (cut_end - start > 0.3).then(|| ((start - 0.05).max(0.0), cut_end, text.clone()))
}

/// Material for the picture beats: an asset and where to start in it.
struct Material {
    asset: String,
    is_image: bool,
    duration: f64,
    next_in: f64,
    uses: usize,
}

pub(super) fn edit(job: &mut Job, progress: &mut dyn FnMut(&str)) -> Result<()> {
    let source = source_video(job)?;
    let info = media::probe(&source)?;
    if info.kind != AssetKind::Video {
        bail!("{} is not a video", source.display());
    }
    let name = job.req.name.clone().unwrap_or_else(|| stem(&source));
    let canvas = job.canvas(info.fps);
    let fps = canvas.fps;
    let (dir, mut p) = job.start(&name, canvas)?;
    let src = job.ws.add_asset(&mut p, &dir, &source, AssetRole::Source)?;
    let transcript = job.transcript(&dir, &p, &src, progress)?;
    progress("Planning cuts");
    let samples = pcm(&p, &dir, &src)?;
    let plan = analyze::plan_cuts(&transcript, &samples, SAMPLE_RATE, &job.cut_settings());
    note_reviews(job, &plan.removed);
    super::lay_main(&mut p, &src, &plan.keep)?;
    cut_summary(job, &plan.removed, info.duration, p.duration());
    progress("Finding the face");
    let face = track_face(job, &mut p, &src, &source, (0.0, info.duration));
    set_focus(&mut p, face);
    let total = p.duration();

    let words = transcript.placed_words(&p);
    let as_transcript: Vec<TranscriptWord> = words
        .iter()
        .map(|w| TranscriptWord {
            text: w.text.clone(),
            start: w.timeline_start,
            end: w.timeline_end,
            confidence: None,
        })
        .collect();
    let emphasis = analyze::emphasis(&as_transcript);
    let onsets: Vec<f64> = words.iter().map(|w| w.timeline_start).collect();

    // Material: the user's B-roll in order, then footage found for each run.
    let mut materials: Vec<Material> = Vec::new();
    for file in &job.req.broll {
        let id = job.ws.add_asset(&mut p, &dir, file, AssetRole::Broll)?;
        materials.push(material(&p, &id));
    }
    let mut beats = plan_beats(&onsets, total, !materials.is_empty() || job.req.find);
    let mut by_run: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    let picture_beats = beats
        .iter()
        .filter(|b| b.kind == BeatKind::Material)
        .count();
    if job.req.find && materials.len() < picture_beats {
        progress("Finding footage for the pictures");
        find_material(
            job,
            &dir,
            &mut p,
            &beats,
            &words,
            &emphasis,
            &mut materials,
            &mut by_run,
            progress,
        );
        if materials.is_empty() {
            beats = plan_beats(&onsets, total, false);
        }
    }

    // Picture beats: overlays on the B-roll track, graded, pushed, with their cut effects.
    let sentences = timeline_sentences(&transcript, &words);
    let chapter_starts: Vec<f64> = chapter_starts(&transcript, &words);
    let mut general = (0..materials.len())
        .filter(|i| !by_run.values().any(|v| v.contains(i)))
        .collect::<Vec<_>>()
        .into_iter();
    let mut counts: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut looks: BTreeMap<String, usize> = BTreeMap::new();
    let mut cues: Vec<Cue> = Vec::new();
    let (mut shot, mut index_in_run, mut last_whip, mut first_picture, mut reused) =
        (0usize, 0usize, -100.0, true, 0usize);
    let mut prev_len = 0.0;
    for beat in &beats {
        if beat.kind == BeatKind::Face {
            if beat.start > 0.0 {
                cues.push(Cue {
                    at: beat.start,
                    sound: Sound::Click,
                });
            }
            continue;
        }
        if materials.is_empty() {
            break;
        }
        if beat.first_in_run {
            index_in_run = 0;
            prev_len = 0.0;
        }
        let pick = by_run
            .get_mut(&beat.run)
            .and_then(|v| (!v.is_empty()).then(|| v.remove(0)))
            .or_else(|| general.next())
            .unwrap_or_else(|| {
                reused += 1;
                least_used(&materials)
            });
        let m = &mut materials[pick];
        let beat_words: Vec<String> = words
            .iter()
            .filter(|w| w.timeline_start >= beat.start - 0.05 && w.timeline_start < beat.end)
            .map(|w| norm(&w.text))
            .collect();
        let word_refs: Vec<&str> = beat_words.iter().map(String::as_str).collect();
        let len = beat.len();
        let src_in = if m.is_image {
            0.0
        } else {
            if m.next_in + len > m.duration - 0.05 {
                m.next_in = past_opening(m.duration).min((m.duration - len).max(0.0));
            }
            let path = p.asset_path(&dir, p.asset(&m.asset).expect("material asset"));
            lit_start(&path, m.next_in, len, m.duration)
        };
        let len = if m.is_image {
            len
        } else {
            len.min(m.duration - src_in)
        };
        if len < 1.0 / fps {
            continue;
        }
        let chapter =
            beat.first_in_run && chapter_starts.iter().any(|c| (c - beat.start).abs() < 0.35);
        let glitch = has_any(&word_refs, GLITCH_WORDS);
        let whip = !beat.first_in_run && prev_len >= 1.2 && beat.start - last_whip >= 20.0;
        let transition = transition_for(
            beat,
            index_in_run,
            first_picture,
            chapter,
            glitch,
            whip,
            fps,
        );
        if transition
            .as_ref()
            .is_some_and(|t| t.kind == TransitionKind::Whip)
        {
            last_whip = beat.start;
        }
        let grade = grade_for(&word_refs, shot, m.is_image);
        *looks
            .entry(format!("{:?}", grade.look).to_lowercase())
            .or_default() += 1;
        let push = (1.0 + PUSH_PER_SECOND * len).min(1.2) as f32;
        let (from_focus, to_focus) = if shot % 2 == 0 {
            (Point::new(0.47, 0.5), Point::new(0.53, 0.5))
        } else {
            (Point::new(0.53, 0.48), Point::new(0.47, 0.52))
        };
        let added = ops::apply(
            &mut p,
            &Op::AddClip {
                asset: m.asset.clone(),
                track: Some("broll".into()),
                start: Some(beat.start),
                index: None,
                src_in,
                duration: Some(len),
                frame: None,
                volume: None,
                duck: None,
            },
        )?;
        let mut patch = serde_json::json!({
            "muted": true,
            "grade": grade,
            "ken_burns": KenBurns { from_scale: 1.0, to_scale: push, from_focus: Some(from_focus), to_focus: Some(to_focus), smooth: false, phase: [0.0, 1.0] },
        });
        if let Some(t) = &transition {
            patch["transition"] = serde_json::to_value(t)?;
            *counts.entry(transition_name(t.kind)).or_default() += 1;
        }
        ops::apply(
            &mut p,
            &Op::Patch {
                item: added.items[0].clone(),
                patch,
            },
        )?;
        match transition.as_ref().map(|t| t.kind) {
            Some(TransitionKind::Tiles) => {
                for k in 0..9 {
                    cues.push(Cue {
                        at: beat.start + k as f64 / fps,
                        sound: Sound::PaneClick,
                    });
                }
            }
            Some(TransitionKind::Glitch) => cues.push(Cue {
                at: beat.start,
                sound: Sound::Glitch,
            }),
            Some(TransitionKind::Whip) => cues.push(Cue {
                at: beat.start,
                sound: Sound::Swish,
            }),
            _ => cues.push(Cue {
                at: beat.start,
                sound: Sound::Click,
            }),
        }
        m.next_in = src_in + len + 0.5;
        m.uses += 1;
        shot += 1;
        index_in_run += 1;
        prev_len = len;
        first_picture = false;
    }
    let pictures = shot;

    // Captions: small words that appear as they are said (Editor V1: about 62 px at 1080p,
    // white, a soft shadow, ink on light pages).
    documentary_captions(&mut p);

    // Display words on the pictures.
    let displays = display_words(&words, &emphasis, &beats, total);
    let landscape = p.canvas.width >= p.canvas.height;
    for d in &displays {
        let position = match d.preset {
            TextPreset::BlackCard => None,
            _ => Some(Point::new(0.5, if landscape { 0.42 } else { 0.4 })),
        };
        ops::apply(
            &mut p,
            &Op::AddText {
                text: d.text.clone(),
                start: d.start,
                duration: d.end - d.start,
                preset: d.preset,
                position,
                style: None,
                anim_in: None,
                anim_out: None,
                track: None,
            },
        )?;
        cues.push(Cue {
            at: d.start,
            sound: if d.preset == TextPreset::BlackCard {
                Sound::SubTone
            } else {
                Sound::Tick
            },
        });
    }

    // Mega words behind the speaker on stressed face beats, when the cut-out model is ready.
    let megas = mega_words(
        job, &dir, &mut p, &beats, &words, &emphasis, total, progress,
    );

    if let Some(hook) = &job.req.hook {
        ops::apply(
            &mut p,
            &Op::AddText {
                text: hook.clone(),
                start: 0.0,
                duration: total.min(2.5),
                preset: TextPreset::Chapter,
                position: Some(Point::new(0.5, 0.3)),
                style: None,
                anim_in: None,
                anim_out: None,
                track: None,
            },
        )?;
    }
    add_captions(job, &dir, &mut p, &transcript)?;

    // The bed: Editor V1 keeps it about 5.5 LU under the voice from frame 0, cuts it out under the
    // punchline and slams it back on the next line, and pulls it down under black cards.
    let voice_lufs = sfx::loudness(&source, None).ok();
    let punch = punchline(&sentences, total);
    let pulls: Vec<(f64, f64)> = displays
        .iter()
        .filter(|d| d.preset == TextPreset::BlackCard)
        .map(|d| (d.start, d.end))
        .collect();
    let bed = music_bed(job, &dir, &mut p, voice_lufs, punch.as_ref(), &pulls)?;
    if bed.is_some()
        && let Some((_, cut_end, _)) = &punch
        && *cut_end < total - 0.3
    {
        cues.push(Cue {
            at: (cut_end - 0.06).max(0.0),
            sound: Sound::Impact,
        });
    }

    // A click on every change, at Editor V1's measured levels, unless the style is voice only.
    let placed = if job.style.sound() == StyleSound::VoiceOnly {
        0
    } else {
        sfx::place(job.ws, &mut p, &dir, &cues, voice_lufs)?
    };

    // Report.
    let face_time: f64 = beats
        .iter()
        .filter(|b| b.kind == BeatKind::Face)
        .map(Beat::len)
        .sum::<f64>()
        + if pictures == 0 { 0.0 } else { 0.0 };
    let face_share = if total > 0.0 {
        100.0 * face_time / total
    } else {
        100.0
    };
    if pictures > 0 {
        job.report.summary.push(format!(
            "Documentary edit: {pictures} found pictures in {} runs, average {:.2} s each; the face is on screen {face_share:.0}% of the time",
            beats.iter().filter(|b| b.kind == BeatKind::Material && b.first_in_run).count(),
            (total - face_time) / pictures as f64,
        ));
        job.report.summary.push(format!(
            "Grades: {}; cut effects: {}",
            looks
                .iter()
                .map(|(k, v)| format!("{k} {v}"))
                .collect::<Vec<_>>()
                .join(", "),
            counts
                .iter()
                .map(|(k, v)| format!("{k} {v}"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    } else {
        job.report.review.push(
            "No found material, so the speaker stays on screen. The documentary edit needs pictures: pass them with --broll, or add --find to search free, licence-tracked footage.".into(),
        );
    }
    if reused > 0 {
        job.report.review.push(format!(
            "{reused} picture beats reuse material because there were only {} files for {pictures} pictures. Add more with --broll or --find.",
            materials.len()
        ));
    }
    if !displays.is_empty() {
        job.report.summary.push(format!(
            "Display words: {}",
            displays
                .iter()
                .map(|d| format!(
                    "{} \"{}\" at {:.1} s",
                    preset_name(d.preset),
                    d.text,
                    d.start
                ))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    job.report.summary.extend(megas);
    if placed > 0 {
        job.report.summary.push(format!(
            "{placed} clicks and effects from the built-in sound kit"
        ));
    }
    match (&bed, &punch) {
        (Some(file), Some((_, _, line))) => job.report.summary.push(format!(
            "Music bed {file} about {BED_UNDER_LU} LU under the voice, cut out under the punchline \"{line}\""
        )),
        (Some(file), None) => job.report.summary.push(format!("Music bed {file} about {BED_UNDER_LU} LU under the voice")),
        (None, _) => job.report.review.push(
            "No music bed. The documentary style runs a bed under the voice and cuts it under the punchline: pass --music FILE (a track you have the rights to) or put one in my-sounds/music.".into(),
        ),
    }
    if let Some((_, _, line)) = &punch {
        job.report.review.push(format!("Punchline picked for the bed cut-out: \"{line}\". Change it if another line lands harder."));
    }
    job.finish(&dir, &mut p, progress)
}

fn material(p: &Project, id: &str) -> Material {
    let a = p.asset(id).expect("asset just added");
    Material {
        asset: id.to_string(),
        is_image: a.kind == AssetKind::Image,
        duration: a.duration,
        next_in: if a.kind == AssetKind::Image {
            0.0
        } else {
            past_opening(a.duration)
        },
        uses: 0,
    }
}

/// Where a video's own pictures start: past a tenth of it, and past the first 10 s (a quarter
/// of a short one), where B-roll packages put slates, titles and contact cards.
pub(super) fn past_opening(duration: f64) -> f64 {
    (duration * 0.1).max((duration * 0.25).min(10.0))
}

fn least_used(materials: &[Material]) -> usize {
    (0..materials.len())
        .min_by_key(|&i| (materials[i].uses, materials[i].is_image))
        .unwrap_or(0)
}

/// Where to start a `len`-second shot in a video at or after `start`: the first of four tries,
/// a step apart, whose middle frame does not read as black. Found videos open on slates and
/// fades, and space footage is often nearly black; a black picture beat looks like a fault and
/// fails the render check. Falls back to `start`.
fn lit_start(path: &Path, start: f64, len: f64, duration: f64) -> f64 {
    let span = (duration - len).max(0.0);
    let step = (duration * 0.13).max(len + 0.5);
    (0..4)
        .map(|k| {
            if span > 0.0 {
                (start + k as f64 * step) % span
            } else {
                0.0
            }
        })
        .find(|&t| !black_at(path, t + len / 2.0))
        .unwrap_or(start)
}

/// Whether the frame at `t` reads as black: 95 % of its pixels under 10 % luma, a little
/// stricter than the render check's blackdetect (98 %). An unreadable frame counts as lit.
pub(super) fn black_at(path: &Path, t: f64) -> bool {
    let Ok(img) = media::frame_rgba(path, t, 96) else {
        return false;
    };
    let luma = |p: &image::Rgba<u8>| {
        0.2126 * f32::from(p[0]) + 0.7152 * f32::from(p[1]) + 0.0722 * f32::from(p[2])
    };
    let dark = img.pixels().filter(|p| luma(p) < 25.5).count();
    dark * 100 >= (img.width() * img.height()).max(1) as usize * 95
}

fn transition_name(kind: TransitionKind) -> &'static str {
    match kind {
        TransitionKind::Crossfade => "crossfade",
        TransitionKind::Dip => "dip",
        TransitionKind::Flash => "white box",
        TransitionKind::Negative => "negative",
        TransitionKind::Glitch => "datamosh",
        TransitionKind::Tiles => "tile reveal",
        TransitionKind::Focus => "rack focus",
        TransitionKind::Whip => "whip",
        TransitionKind::Pixelate => "pixelate",
    }
}

fn preset_name(preset: TextPreset) -> &'static str {
    match preset {
        TextPreset::Keyword => "keyword",
        TextPreset::Shock => "shock",
        TextPreset::BlackCard => "black card",
        TextPreset::Mega => "mega",
        _ => "text",
    }
}

/// Sentences with their timeline start and end, for the ones that play.
fn timeline_sentences(t: &Transcript, words: &[PlacedWord]) -> Vec<(f64, f64, String)> {
    let at: BTreeMap<usize, &PlacedWord> = words
        .iter()
        .filter_map(|w| Some((w.id.strip_prefix('w')?.parse().ok()?, w)))
        .collect();
    t.sentences()
        .into_iter()
        .filter_map(|s| {
            let first = (s.first..s.end_word).find_map(|i| at.get(&i))?;
            let last = (s.first..s.end_word).rev().find_map(|i| at.get(&i))?;
            Some((first.timeline_start, last.timeline_end, s.text))
        })
        .collect()
}

/// Timeline starts of lines that follow a long pause in the recording: the speaker moving to a
/// new part, where the edit enters through a double negative.
fn chapter_starts(t: &Transcript, words: &[PlacedWord]) -> Vec<f64> {
    let at: BTreeMap<usize, &PlacedWord> = words
        .iter()
        .filter_map(|w| Some((w.id.strip_prefix('w')?.parse().ok()?, w)))
        .collect();
    t.words
        .windows(2)
        .enumerate()
        .filter(|(_, pair)| pair[1].start - pair[0].end >= 1.0)
        .filter_map(|(i, _)| at.get(&(i + 1)).map(|w| w.timeline_start))
        .collect()
}

fn documentary_captions(p: &mut Project) {
    if p.captions.mode == CaptionMode::Off {
        return;
    }
    let landscape = p.canvas.width >= p.canvas.height;
    let c = &mut p.captions;
    c.mode = CaptionMode::Reveal;
    c.font = "Inter".into();
    c.size = if landscape { 0.057 } else { 0.036 };
    c.color = Color::WHITE;
    c.stroke = 0.0;
    c.shadow = true;
    c.boxed = false;
    c.ink_on_light = true;
    c.uppercase = false;
    c.position = if landscape { 0.64 } else { 0.62 };
    c.max_words = if landscape { 6 } else { 4 };
    c.max_chars = if landscape { 32 } else { 20 };
}

/// Try a run's own subject only when the global subject finds no usable title matches.
fn search_with_fallback(
    query: &str,
    fallback: Option<&str>,
    seen: &HashSet<String>,
    mut search: impl FnMut(&str) -> Result<footage::SearchResult>,
) -> Result<(String, footage::SearchResult)> {
    let mut result = search(query)?;
    let keep = |result: &mut footage::SearchResult, query: &str| {
        result
            .candidates
            .retain(|c| !c.flag && !seen.contains(&c.id) && relevant(&c.title, query));
    };
    keep(&mut result, query);
    if result.candidates.is_empty()
        && let Some(fallback) = fallback.filter(|fallback| *fallback != query)
    {
        match search(fallback) {
            Ok(mut next) => {
                keep(&mut next, fallback);
                next.warnings.splice(0..0, result.warnings);
                return Ok((fallback.to_string(), next));
            }
            Err(error) => result.warnings.push(format!("\"{fallback}\": {error:#}")),
        }
    }
    Ok((query.to_string(), result))
}

/// Finds footage for runs that have none, one query per run, licence-clear results only.
#[allow(clippy::too_many_arguments)]
fn find_material(
    job: &mut Job,
    dir: &Path,
    p: &mut Project,
    beats: &[Beat],
    words: &[PlacedWord],
    emphasis: &[bool],
    materials: &mut Vec<Material>,
    by_run: &mut BTreeMap<usize, Vec<usize>>,
    progress: &mut dyn FnMut(&str),
) {
    let user_files = materials.len();
    let runs: Vec<(usize, f64, f64, usize)> = {
        let mut out: Vec<(usize, f64, f64, usize)> = Vec::new();
        for b in beats.iter().filter(|b| b.kind == BeatKind::Material) {
            match out.last_mut() {
                Some(last) if last.0 == b.run => {
                    last.2 = b.end;
                    last.3 += 1;
                }
                _ => out.push((b.run, b.start, b.end, 1)),
            }
        }
        out
    };
    let mut fetched = 0usize;
    let mut queries = Vec::new();
    let mut seen_ids = HashSet::new();
    let mut warnings = Vec::new();
    // A provider that fails twice (a timeout, an outage) is left out of the later searches.
    let mut failures = [0usize; Provider::ALL.len()];
    let topic = topic_terms(words);
    // Spread the user's own files over the runs first; find footage for the rest.
    let need_runs = runs.len().saturating_sub(user_files.div_ceil(3));
    for (i, &(run, a, b, shots)) in runs.iter().skip(runs.len() - need_runs).enumerate() {
        if fetched >= FIND_LIMIT {
            break;
        }
        let run_words: Vec<&PlacedWord> = words
            .iter()
            .filter(|w| w.timeline_start >= a && w.timeline_start < b)
            .collect();
        let run_emph: Vec<bool> = words
            .iter()
            .zip(emphasis)
            .filter(|(w, _)| w.timeline_start >= a && w.timeline_start < b)
            .map(|(_, e)| *e)
            .collect();
        let Some(query) = query_for(&run_words, &run_emph, &topic, i) else {
            continue;
        };
        let providers: Vec<Provider> = Provider::available()
            .into_iter()
            .filter(|p| {
                Provider::ALL
                    .iter()
                    .position(|q| q == p)
                    .is_none_or(|k| failures[k] < 2)
            })
            .collect();
        if providers.is_empty() {
            break;
        }
        let fallback = query_for(&run_words, &run_emph, &[], i);
        let opts = SearchOptions {
            providers,
            ..SearchOptions::default()
        };
        let (query, result) =
            match search_with_fallback(&query, fallback.as_deref(), &seen_ids, |query| {
                progress(&format!("Searching footage for \"{query}\""));
                footage::search(query, &opts)
            }) {
                Ok(r) => r,
                Err(e) => {
                    warnings.push(format!("\"{query}\": {e:#}"));
                    continue;
                }
            };
        for w in result.warnings {
            match Provider::ALL
                .iter()
                .position(|p| w.starts_with(&format!("{}:", p.label())))
            {
                Some(k) => {
                    failures[k] += 1;
                    if failures[k] == 1 {
                        warnings.push(w);
                    }
                }
                None => warnings.push(w),
            }
        }
        let mut picked = 0;
        // Moving footage first (Editor V1: 50 to 75 percent of the runtime moves), then stills.
        let mut candidates = result.candidates;
        candidates.sort_by_key(|c| {
            (
                c.kind != MediaKind::Video,
                c.kind == MediaKind::Video && c.duration > 0.0 && c.duration < 2.0,
            )
        });
        for c in candidates {
            if picked >= shots.min(2) || fetched >= FIND_LIMIT {
                break;
            }
            seen_ids.insert(c.id.clone());
            match footage::fetch(job.ws, &c.id)
                .and_then(|f| footage::add_to_project(job.ws, p, dir, &f, AssetRole::Broll))
            {
                Ok(id) => {
                    materials.push(material(p, &id));
                    by_run.entry(run).or_default().push(materials.len() - 1);
                    picked += 1;
                    fetched += 1;
                }
                Err(e) => warnings.push(format!("{}: {e:#}", c.id)),
            }
        }
        queries.push(format!("\"{query}\" ({picked})"));
    }
    if !queries.is_empty() {
        job.report
            .summary
            .push(format!("Found {fetched} files for: {}", queries.join(", ")));
        job.report.review.push(
            "Found footage is matched by keywords, so check that each picture fits its line; `veecut sources` lists every source and licence, and `veecut find` plus `veecut fetch` swap one in.".into(),
        );
    }
    for w in warnings.into_iter().take(4) {
        job.report.review.push(format!("Footage search: {w}"));
    }
}

/// Cut-outs with a mega word behind the speaker on face beats that hold a stressed word, when
/// the cut-out model is already downloaded. Returns summary lines.
#[allow(clippy::too_many_arguments)]
fn mega_words(
    job: &mut Job,
    dir: &Path,
    p: &mut Project,
    beats: &[Beat],
    words: &[PlacedWord],
    emphasis: &[bool],
    total: f64,
    progress: &mut dyn FnMut(&str),
) -> Vec<String> {
    let budget = ((total / 45.0).floor() as usize).max(1);
    // A cut-out needs the speaker in the picture: recordings often cut to their own B-roll
    // mid-sentence, so the face must have been found through the beat.
    let candidates: Vec<(f64, f64, String)> = beats
        .iter()
        .filter(|b| b.kind == BeatKind::Face && b.len() >= 1.2)
        .filter_map(|b| {
            let (_, w) = words.iter().enumerate().find(|(k, w)| {
                w.timeline_start >= b.start
                    && w.timeline_start < b.end - 0.4
                    && emphasis.get(*k).copied().unwrap_or(false)
                    && display_worthy(&w.text)
            })?;
            let text = w
                .text
                .trim_matches(|c: char| !c.is_alphanumeric())
                .to_uppercase();
            (!text.is_empty() && face_through(p, w.timeline_start, b.end)).then_some((
                w.timeline_start,
                b.end,
                text,
            ))
        })
        .take(budget)
        .collect();
    if candidates.is_empty() {
        return Vec::new();
    }
    if !matte::ready() {
        job.report.review.push(format!(
            "For the huge red word behind your head (Editor V1's mega word), run `veecut cutout --at {:.2} --dur {:.2} --mega \"{}\"` once; it downloads the cut-out model the first time.",
            candidates[0].0,
            candidates[0].1 - candidates[0].0,
            candidates[0].2
        ));
        return Vec::new();
    }
    let mut out = Vec::new();
    for (at, end, text) in candidates {
        // The cut-out must stay inside one main-track clip.
        let Some((_, clip)) = p.main_at(at + 0.01) else {
            continue;
        };
        let (from, to) = (at.max(clip.start), end.min(clip.end()));
        if to - from < 0.5 {
            continue;
        }
        match matte::cutout(job.ws, p, dir, from, to, 0.006, progress) {
            Ok(_) => {
                let done = ops::apply(
                    p,
                    &Op::AddText {
                        text: text.clone(),
                        start: from,
                        duration: to - from,
                        preset: TextPreset::Mega,
                        position: None,
                        style: None,
                        anim_in: None,
                        anim_out: None,
                        track: None,
                    },
                );
                if done.is_ok() {
                    out.push(format!(
                        "Mega word \"{text}\" behind the speaker at {from:.1} s"
                    ));
                }
            }
            Err(e) => job
                .report
                .review
                .push(format!("Cut-out at {from:.1} s failed: {e:#}")),
        }
    }
    out
}

/// Whether the face detector found the speaker at nearly every check from `from` to `to` on the
/// timeline, within the main-track clip playing at `from`. One missed check is allowed: the
/// detector loses turned heads.
fn face_through(p: &Project, from: f64, to: f64) -> bool {
    let Some((_, item)) = p.main_at(from + 0.01) else {
        return false;
    };
    let Some(clip) = item.clip() else {
        return false;
    };
    let Some(track) = p.faces.get(&clip.asset) else {
        return false;
    };
    let at = |t: f64| clip.src_in + (t - item.start) * clip.speed;
    let (s0, s1) = (at(from), at(to.min(item.end())));
    // Checks run on a fixed grid; the smallest gap between samples is its step.
    let step = track
        .windows(2)
        .map(|w| w[1].t - w[0].t)
        .filter(|d| *d > 1e-3)
        .fold(f64::INFINITY, f64::min);
    let step = if step.is_finite() { step } else { 0.5 };
    let found = track
        .iter()
        .filter(|f| f.t >= s0 - step / 2.0 && f.t <= s1 + step / 2.0)
        .count();
    found >= ((s1 - s0) / step).floor().max(1.0) as usize
}

/// The bed: the requested file, else the first file in my-sounds/music. Runs from frame 0 at a
/// fixed level under the voice (not ducked), with the punchline cut and the black-card pulls.
fn music_bed(
    job: &mut Job,
    dir: &Path,
    p: &mut Project,
    voice_lufs: Option<f32>,
    punch: Option<&(f64, f64, String)>,
    pulls: &[(f64, f64)],
) -> Result<Option<String>> {
    let file = match &job.req.music {
        Some(f) => Some(f.clone()),
        None => audio_files(&job.ws.root.join("my-sounds/music"))
            .into_iter()
            .next(),
    };
    let Some(file) = file else { return Ok(None) };
    let id = job.ws.add_asset(p, dir, &file, AssetRole::Music)?;
    let asset_len = p.asset(&id).map(|a| a.duration).unwrap_or(0.0);
    let total = p.duration();
    if asset_len < 1.0 || total <= 0.0 {
        return Ok(None);
    }
    let music_path = p.asset_path(dir, p.asset(&id).expect("asset just added"));
    let volume = match (voice_lufs, sfx::loudness(&music_path, None).ok()) {
        (Some(v), Some(m)) => sfx::bed_volume(v, m, BED_UNDER_LU),
        _ => 0.35,
    };
    let track = super::audio_track(p, "music", "Music")?;
    let mut t = 0.0;
    let mut ids = Vec::new();
    while total - t > 0.05 {
        let len = asset_len.min(total - t);
        ids.push(super::place_audio(
            p, &track, &id, t, 0.0, len, volume, false,
        )?);
        t += len;
    }
    if let (Some(first), Some(last)) = (ids.first().cloned(), ids.last().cloned()) {
        ops::apply(
            p,
            &Op::Patch {
                item: first,
                patch: serde_json::json!({ "fade_in": 0.03 }),
            },
        )?;
        ops::apply(
            p,
            &Op::Patch {
                item: last,
                patch: serde_json::json!({ "fade_out": 0.9 }),
            },
        )?;
    }
    let mut moves: Vec<(f64, f64, f32)> = pulls.iter().map(|&(a, b)| (a, b, -8.0)).collect();
    if let Some((a, b, _)) = punch {
        moves.push((*a, *b, -70.0));
    }
    moves.sort_by(|x, y| x.0.total_cmp(&y.0));
    moves.dedup_by(|b, a| b.0 < a.1);
    if !moves.is_empty() {
        sfx::shape_bed(p, &track, &moves)?;
    }
    Ok(Some(
        file.file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn onsets_every(step: f64, total: f64) -> Vec<f64> {
        (0..((total / step) as usize))
            .map(|i| i as f64 * step)
            .collect()
    }

    #[test]
    fn beats_alternate_cut_on_words_and_keep_the_face_near_a_third() {
        let onsets = onsets_every(0.31, 60.0);
        let beats = plan_beats(&onsets, 60.0, true);
        assert_eq!(
            beats.first().map(|b| b.kind),
            Some(BeatKind::Material),
            "opens on pictures"
        );
        assert!((beats.last().unwrap().end - 60.0).abs() < 1e-9);
        for pair in beats.windows(2) {
            assert!((pair[0].end - pair[1].start).abs() < 1e-9, "no gaps");
            assert!(
                !(pair[0].kind == BeatKind::Face && pair[1].kind == BeatKind::Face),
                "never two face beats in a row"
            );
        }
        for b in &beats[..beats.len() - 1] {
            assert!(b.end - b.start >= 0.6 - 1e-9, "{b:?}");
            assert!(
                onsets.iter().any(|o| (o - b.end).abs() < 1e-9) || b.end == 60.0,
                "cuts on a word: {b:?}"
            );
            if b.kind == BeatKind::Material {
                assert!(b.len() <= 1.5 + 1e-9, "{b:?}");
            }
        }
        let face: f64 = beats
            .iter()
            .filter(|b| b.kind == BeatKind::Face)
            .map(Beat::len)
            .sum();
        let share = face / 60.0;
        assert!((0.22..=0.4).contains(&share), "face share {share}");
    }

    #[test]
    fn long_form_body_breathes_after_the_cold_open() {
        let onsets = onsets_every(0.27, 400.0);
        let beats = plan_beats(&onsets, 400.0, true);
        let body: Vec<&Beat> = beats
            .iter()
            .filter(|b| b.kind == BeatKind::Material && b.start > 30.0)
            .collect();
        let avg = body.iter().map(|b| b.len()).sum::<f64>() / body.len() as f64;
        assert!((1.2..=2.0).contains(&avg), "body average {avg}");
        let open: Vec<&Beat> = beats
            .iter()
            .filter(|b| b.kind == BeatKind::Material && b.end <= 10.0)
            .collect();
        assert!(open.iter().all(|b| b.len() <= 1.5 + 1e-9));
    }

    #[test]
    fn no_material_keeps_the_speaker_on_screen() {
        let beats = plan_beats(&onsets_every(0.3, 20.0), 20.0, false);
        assert_eq!(beats.len(), 1);
        assert_eq!(beats[0].kind, BeatKind::Face);
    }

    #[test]
    fn transitions_follow_structure() {
        let b = |first, run| Beat {
            kind: BeatKind::Material,
            start: 5.0,
            end: 6.0,
            run,
            first_in_run: first,
        };
        let kind = |t: Option<Transition>| t.map(|t| t.kind);
        assert_eq!(
            kind(transition_for(
                &b(true, 0),
                0,
                true,
                false,
                false,
                false,
                30.0
            )),
            Some(TransitionKind::Tiles)
        );
        assert_eq!(
            kind(transition_for(
                &b(true, 1),
                0,
                false,
                true,
                false,
                false,
                30.0
            )),
            Some(TransitionKind::Negative)
        );
        assert_eq!(
            kind(transition_for(
                &b(true, 1),
                0,
                false,
                false,
                false,
                false,
                30.0
            )),
            Some(TransitionKind::Focus)
        );
        assert_eq!(
            kind(transition_for(
                &b(false, 1),
                1,
                false,
                false,
                true,
                false,
                30.0
            )),
            Some(TransitionKind::Glitch)
        );
        assert_eq!(
            kind(transition_for(
                &b(false, 1),
                2,
                false,
                false,
                false,
                false,
                30.0
            )),
            Some(TransitionKind::Flash)
        );
        assert_eq!(
            kind(transition_for(
                &b(false, 1),
                1,
                false,
                false,
                false,
                false,
                30.0
            )),
            None
        );
        let tiles = transition_for(&b(true, 0), 0, true, false, false, false, 30.0).unwrap();
        assert!((tiles.duration - 0.3).abs() < 1e-9);
    }

    #[test]
    fn grades_follow_words_and_spare_natural_share() {
        assert_eq!(grade_for(&["money"], 0, false).look, TreatLook::Warm);
        assert_eq!(grade_for(&["report"], 1, false).look, TreatLook::Mono);
        let natural = (0..40)
            .filter(|&i| grade_for(&["word"], i, false).look == TreatLook::Natural)
            .count();
        assert!((12..=20).contains(&natural), "{natural} of 40 natural");
        assert!(
            grade_for(&["word"], 1, true).halftone > 0.0,
            "stills get a dot screen"
        );
    }

    #[test]
    fn punchline_is_a_short_late_line_and_skips_calls_to_action() {
        let s = |a: f64, text: &str| (a, a + 2.0, text.to_string());
        let lines = vec![
            s(0.0, "Intro line here."),
            s(
                10.0,
                "A long build up that keeps going and going for many many words here.",
            ),
            s(20.0, "It cost me zero."),
            s(25.0, "Follow for more."),
        ];
        let (a, b, text) = punchline(&lines, 30.0).unwrap();
        assert_eq!(text, "It cost me zero.");
        assert!((a - 19.95).abs() < 1e-9 && (b - 24.98).abs() < 1e-9);
        assert!(punchline(&lines[..1], 30.0).is_none());
    }

    #[test]
    fn queries_prefer_names_and_skip_filler() {
        let w = |t: &str| PlacedWord {
            id: "w0".into(),
            text: t.into(),
            source_start: 0.0,
            source_end: 0.1,
            timeline_start: 0.0,
            timeline_end: 0.1,
        };
        let words = [
            w("so"),
            w("NASA"),
            w("really"),
            w("launched"),
            w("the"),
            w("rocket"),
        ];
        let refs: Vec<&PlacedWord> = words.iter().collect();
        let q = query_for(&refs, &[false; 6], &[], 0).unwrap();
        assert!(q.starts_with("nasa"), "{q}");
        assert!(!q.contains("really"));
    }

    #[test]
    fn display_words_one_per_run_numbers_shock_and_black_cards() {
        let w = |t: &str, at: f64| PlacedWord {
            id: "w0".into(),
            text: t.into(),
            source_start: at,
            source_end: at + 0.3,
            timeline_start: at,
            timeline_end: at + 0.3,
        };
        let words = vec![
            w("It", 0.1),
            w("cost", 0.4),
            w("$40,000", 0.7),
            w("and", 1.6),
            w("nothing", 2.0),
            w("changed", 2.4),
            w("today", 5.2),
        ];
        let emphasis = vec![false, false, true, false, false, false, true];
        let beats = vec![
            Beat {
                kind: BeatKind::Material,
                start: 0.0,
                end: 1.5,
                run: 0,
                first_in_run: true,
            },
            Beat {
                kind: BeatKind::Material,
                start: 1.5,
                end: 3.0,
                run: 1,
                first_in_run: true,
            },
            Beat {
                kind: BeatKind::Face,
                start: 3.0,
                end: 6.0,
                run: 1,
                first_in_run: true,
            },
        ];
        let d = display_words(&words, &emphasis, &beats, 6.0);
        assert_eq!(
            d.iter().map(|d| d.preset).collect::<Vec<_>>(),
            vec![TextPreset::Shock, TextPreset::BlackCard]
        );
        assert_eq!(d[0].text, "$40,000");
        assert_eq!(d[1].text, "nothing");
        assert!(d.windows(2).all(|p| p[0].end <= p[1].start));
    }

    #[test]
    fn weak_words_never_go_on_screen() {
        let w = |t: &str, at: f64| PlacedWord {
            id: "w0".into(),
            text: t.into(),
            source_start: at,
            source_end: at + 0.3,
            timeline_start: at,
            timeline_end: at + 0.3,
        };
        let words = vec![w("two", 0.2), w("2", 1.7), w("boosters", 3.2)];
        let beat = |start: f64, run| Beat {
            kind: BeatKind::Material,
            start,
            end: start + 1.5,
            run,
            first_in_run: true,
        };
        let beats = vec![beat(0.0, 0), beat(1.5, 1), beat(3.0, 2)];
        let d = display_words(&words, &[true, true, true], &beats, 4.5);
        assert_eq!(
            d.iter().map(|d| d.text.as_str()).collect::<Vec<_>>(),
            vec!["boosters"]
        );
    }

    #[test]
    fn searches_stay_on_the_subject_and_results_must_match_it() {
        let w = |t: &str| PlacedWord {
            id: "w0".into(),
            text: t.into(),
            source_start: 0.0,
            source_end: 0.1,
            timeline_start: 0.0,
            timeline_end: 0.1,
        };
        let talk: Vec<PlacedWord> =
            "The Artemis crew trains in Houston. Artemis flies on the SLS rocket, and the SLS boosters give the ability to go farther."
                .split_whitespace()
                .map(w)
                .collect();
        let topic = topic_terms(&talk);
        assert_eq!(topic, vec!["sls".to_string(), "artemis".to_string()]);
        let run: Vec<PlacedWord> = "that's the test properly with"
            .split_whitespace()
            .map(w)
            .collect();
        let refs: Vec<&PlacedWord> = run.iter().collect();
        assert_eq!(
            query_for(&refs, &[false; 5], &topic, 0).as_deref(),
            Some("sls test")
        );
        let vague: Vec<PlacedWord> = "it's really important".split_whitespace().map(w).collect();
        let refs: Vec<&PlacedWord> = vague.iter().collect();
        assert_eq!(
            query_for(&refs, &[false; 3], &topic, 1).as_deref(),
            Some("sls artemis")
        );
        assert!(relevant("Artemis I Launch Imagery", "artemis test"));
        assert!(!relevant("How to properly leg press", "sls test"));
        assert!(!relevant("Child Molester, The", "child safety"));
    }

    #[test]
    fn one_subject_acronym_beats_repeated_locations_and_titles_need_the_subject() {
        let talk: Vec<PlacedWord> =
            "The SLS booster segments travel from Utah to Kennedy Space Center. \
            Booster segments leave Utah. Utah ships booster segments for Artemis."
                .split_whitespace()
                .map(|text| PlacedWord {
                    id: "w0".into(),
                    text: text.into(),
                    source_start: 0.0,
                    source_end: 0.1,
                    timeline_start: 0.0,
                    timeline_end: 0.1,
                })
                .collect();
        let topic = topic_terms(&talk);
        assert_eq!(topic.first().map(String::as_str), Some("sls"));
        assert!(topic.iter().any(|t| t == "booster"));
        let refs: Vec<_> = talk[..15].iter().collect();
        let query = query_for(&refs, &[], &topic, 0).unwrap();
        assert!(query.starts_with("sls"), "{query}");
        assert!(relevant("SLS Booster Segments head to Kennedy", &query));
        for title in [
            "Senator John Kennedy speaks on the U.S. Senate floor",
            "Clip of the State Funeral of John Kennedy",
            "Utah01",
            "Space Station Astronauts Discuss Life In Space With Utah Students",
            "OSIRIS-REx Sample Landing and Recovery in Utah",
        ] {
            assert!(!relevant(title, "sls kennedy"), "{title}");
        }
        assert!(!relevant("Boosterism in Utah", "booster"));
        assert!(!relevant("Utah01", "utah"));
    }

    #[test]
    fn title_matching_tolerates_plural_suffixes_without_prefix_matches() {
        assert!(relevant("SLS Booster Segments", "boosters"));
        assert!(relevant("SLS Boosters", "booster"));
        assert!(relevant("Artemis launches", "launch"));
        assert!(relevant("Artemis launch", "launches"));
        assert!(!relevant("Boosterism in Utah", "boosters"));
        assert!(!relevant("Utah01", "utah"));
    }

    #[test]
    fn incidental_acronym_falls_back_to_the_runs_own_subject() {
        let words: Vec<PlacedWord> = "The Artemis boosters launch. Artemis flies with NASA."
            .split_whitespace()
            .map(|text| PlacedWord {
                id: "w0".into(),
                text: text.into(),
                source_start: 0.0,
                source_end: 0.1,
                timeline_start: 0.0,
                timeline_end: 0.1,
            })
            .collect();
        let topic = topic_terms(&words);
        assert_eq!(topic[0], "nasa");
        let run: Vec<_> = words[..4].iter().collect();
        let query = query_for(&run, &[], &topic, 0).unwrap();
        let fallback = query_for(&run, &[], &[], 0).unwrap();
        let mut calls = Vec::new();
        let (used, result) =
            search_with_fallback(&query, Some(&fallback), &HashSet::new(), |query| {
                calls.push(query.to_string());
                Ok(footage::SearchResult {
                    candidates: vec![footage::Candidate {
                        id: "nasa:artemis".into(),
                        provider: Provider::Nasa,
                        kind: MediaKind::Video,
                        title: "Artemis Booster Launch".into(),
                        page_url: String::new(),
                        thumb_url: String::new(),
                        width: 1280,
                        height: 720,
                        duration: 10.0,
                        license: "public domain".into(),
                        license_url: None,
                        author: None,
                        flag: false,
                        flag_reason: None,
                    }],
                    warnings: Vec::new(),
                })
            })
            .unwrap();
        assert_eq!(calls, ["nasa artemis", "artemis"]);
        assert_eq!(used, "artemis");
        assert_eq!(result.candidates.len(), 1);
    }

    #[test]
    fn shots_start_past_black_stretches() {
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
        assert!(black_at(&path, 1.0) && !black_at(&path, 6.0));
        let t = lit_start(&path, 0.0, 1.0, 8.0);
        assert!(t + 0.5 >= 4.0 && t + 1.0 <= 8.0, "start {t}");
        assert_eq!(lit_start(&path, 5.0, 1.0, 8.0), 5.0, "a lit start stays");
    }
}
