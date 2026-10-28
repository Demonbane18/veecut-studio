//! Word-level transcripts of a source file. Times are seconds in the source, not the timeline;
//! `Project::source_to_timeline` maps them through the cuts.

use std::path::Path;

use anyhow::{Context, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

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
}
