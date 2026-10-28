//! Word-level transcription: Groq's free Whisper API or local whisper.cpp.

mod groq;
mod local;

use std::path::Path;

use anyhow::{Result, bail};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::media;
use crate::transcript::Transcript;

/// Who turns speech into words.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    /// Groq's Whisper API with the user's free key (`GROQ_API_KEY`). Uploads the audio.
    Groq,
    /// whisper.cpp on this computer; the model downloads to `~/.veecut/models` on first use.
    Local,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TranscribeOptions {
    /// None picks Groq when `GROQ_API_KEY` is set, else local.
    #[serde(default)]
    pub provider: Option<Provider>,
    /// ISO 639-1 code such as `en` or `tl` (Tagalog and Taglish); None or `auto` detects it.
    #[serde(default)]
    pub language: Option<String>,
    /// Groq: `whisper-large-v3` (default) or `whisper-large-v3-turbo`. Local: `large-v3-turbo`
    /// (default), `base`, `small` or `medium`. A name meant for the other provider is ignored.
    #[serde(default)]
    pub model: Option<String>,
}

/// Transcribes the speech in `media` with word times in source seconds.
/// Auto provider: Groq when GROQ_API_KEY is set, else local whisper.cpp (model downloaded to ~/.veecut/models on first use).
pub fn transcribe(
    media: &Path,
    asset_id: &str,
    opts: &TranscribeOptions,
    progress: &mut dyn FnMut(&str),
) -> Result<Transcript> {
    let info = media::probe(media)?;
    if !info.has_audio {
        bail!("{} has no audio to transcribe", media.display());
    }
    let language = opts
        .language
        .as_deref()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.eq_ignore_ascii_case("auto"));
    let model = opts
        .model
        .as_deref()
        .map(str::trim)
        .filter(|m| !m.is_empty());
    let mut transcript = match opts.provider.unwrap_or_else(auto_provider) {
        Provider::Groq => groq::transcribe(
            media,
            info.duration,
            model.filter(|m| m.starts_with("whisper-")),
            language,
            progress,
        )?,
        Provider::Local => local::transcribe(
            media,
            model.map(|m| m.trim_start_matches("whisper-")),
            language,
            progress,
        )?,
    };
    transcript.asset = asset_id.to_string();
    transcript.duration = transcript.duration.max(info.duration);
    if transcript.words.is_empty() {
        bail!("no speech was found in {}", media.display());
    }
    Ok(transcript)
}

fn auto_provider() -> Provider {
    if groq::api_key().is_some() {
        Provider::Groq
    } else {
        Provider::Local
    }
}

/// One line per provider: ready, or what is missing.
pub fn status() -> Vec<String> {
    let auto = auto_provider();
    let mark = |p: Provider| if p == auto { " (used by default)" } else { "" };
    let ffmpeg = media::ffmpeg().err().map(|e| format!(" but {e}"));
    let groq = match groq::api_key() {
        Some(_) => format!("groq: ready (GROQ_API_KEY is set; uploads audio to Groq){}", ffmpeg.as_deref().unwrap_or("")),
        None => "groq: GROQ_API_KEY is not set; get a free key at https://console.groq.com/keys and put GROQ_API_KEY=... in the workspace .env".to_string(),
    };
    vec![
        groq + mark(Provider::Groq),
        local::status() + ffmpeg.as_deref().unwrap_or("") + mark(Provider::Local),
    ]
}
