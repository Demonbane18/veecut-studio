//! Groq's hosted Whisper (free tier, the user's own key). The upload approach follows
//! free247editor's `tools/transcribe.mjs` (MIT): mono 16 kHz 64 kbps MP3, verbose JSON, word
//! timestamps.

use std::io::Write;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::analyze;
use crate::media;
use crate::transcript::{Transcript, TranscriptWord};

const URL: &str = "https://api.groq.com/openai/v1/audio/transcriptions";
const DEFAULT_MODEL: &str = "whisper-large-v3";
/// Longer audio goes up in parts split at pauses. At 64 kbps a part is about 9.6 MB, well under
/// Groq's 25 MB free-tier limit, so this also keeps every upload under 20 MB.
const PART_SECONDS: f64 = 20.0 * 60.0;
const MAX_UPLOAD: u64 = 20 * 1024 * 1024;
const ATTEMPTS: u32 = 6;

/// The user's Groq key, if set. Never printed.
pub(super) fn api_key() -> Option<String> {
    std::env::var("GROQ_API_KEY")
        .ok()
        .map(|k| k.trim().to_string())
        .filter(|k| !k.is_empty())
}

pub(super) fn transcribe(
    media: &Path,
    duration: f64,
    model: Option<&str>,
    language: Option<&str>,
    progress: &mut dyn FnMut(&str),
) -> Result<Transcript> {
    let Some(key) = api_key() else {
        bail!(
            "GROQ_API_KEY is not set. Get a free key at https://console.groq.com/keys and put GROQ_API_KEY=... in the workspace .env, or transcribe locally"
        );
    };
    let model = model.unwrap_or(DEFAULT_MODEL);
    let parts = parts(media, duration, progress)?;
    let dir = tempfile::tempdir().context("creating a temporary folder for the upload")?;
    let mut words = Vec::new();
    let mut detected = None;
    for (k, &(from, to)) in parts.iter().enumerate() {
        let mp3 = dir.path().join(format!("part{k}.mp3"));
        let mut cmd = media::ffmpeg_command()?;
        if parts.len() > 1 {
            cmd.arg("-ss")
                .arg(format!("{from:.3}"))
                .arg("-t")
                .arg(format!("{:.3}", to - from));
        }
        cmd.arg("-i")
            .arg(media)
            .args([
                "-vn", "-ac", "1", "-ar", "16000", "-b:a", "64k", "-f", "mp3", "-y",
            ])
            .arg(&mp3);
        media::run(&mut cmd).context("preparing audio for Groq")?;
        let audio = std::fs::read(&mp3)?;
        if audio.len() as u64 > MAX_UPLOAD {
            bail!(
                "the audio part is {:.1} MB, over the 20 MB upload limit",
                audio.len() as f64 / 1e6
            );
        }
        let label = if parts.len() > 1 {
            format!(" part {}/{}", k + 1, parts.len())
        } else {
            String::new()
        };
        progress(&format!(
            "Groq {model}: uploading{label} ({:.1} MB)",
            audio.len() as f64 / 1e6
        ));
        let reply = post(&key, &audio, model, language, progress)?;
        detected = detected.or(reply.language.as_deref().and_then(iso_language));
        words.extend(
            punctuate(reply.words, &reply.segments)
                .into_iter()
                .map(|mut w| {
                    w.start += from;
                    w.end += from;
                    w
                }),
        );
    }
    Ok(Transcript {
        asset: String::new(),
        provider: format!("groq:{model}"),
        language: language.map(str::to_string).or(detected),
        duration,
        words,
    })
}

/// Upload parts: the whole file when short, else pieces split at pauses.
fn parts(media: &Path, duration: f64, progress: &mut dyn FnMut(&str)) -> Result<Vec<(f64, f64)>> {
    if duration <= PART_SECONDS {
        return Ok(vec![(0.0, duration)]);
    }
    progress("Finding pauses to split the long recording");
    let pcm = media::pcm_mono(media, 8000, None)?;
    Ok(split(
        duration,
        &analyze::silences(&pcm, 8000, analyze::quiet_threshold(&pcm, 8000), 0.3),
    ))
}

/// Pieces of at most 20 minutes, each ending in the middle of the longest pause of its last
/// 10 minutes (or at 20 minutes when that stretch has no pause).
fn split(duration: f64, pauses: &[(f64, f64)]) -> Vec<(f64, f64)> {
    let mut out = Vec::new();
    let mut start = 0.0;
    while duration - start > PART_SECONDS {
        let (lo, hi) = (start + PART_SECONDS / 2.0, start + PART_SECONDS);
        let cut = pauses
            .iter()
            .filter(|(a, b)| (a + b) / 2.0 > lo && (a + b) / 2.0 < hi)
            .max_by(|x, y| (x.1 - x.0).total_cmp(&(y.1 - y.0)))
            .map_or(hi, |(a, b)| (a + b) / 2.0);
        out.push((start, cut));
        start = cut;
    }
    out.push((start, duration));
    out
}

#[derive(Deserialize)]
struct Reply {
    #[serde(default)]
    language: Option<String>,
    #[serde(default)]
    segments: Vec<Segment>,
    #[serde(default)]
    words: Vec<Word>,
}

#[derive(Deserialize)]
struct Segment {
    text: String,
}

#[derive(Deserialize)]
struct Word {
    word: String,
    start: f64,
    end: f64,
}

/// Posts one audio file, retrying 429 and 5xx with backoff (honoring `retry-after`) and network
/// failures.
fn post(
    key: &str,
    audio: &[u8],
    model: &str,
    language: Option<&str>,
    progress: &mut dyn FnMut(&str),
) -> Result<Reply> {
    let boundary = format!(
        "veecut{:016x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos() as u64)
    );
    let mut fields = vec![
        ("model", model),
        ("response_format", "verbose_json"),
        ("timestamp_granularities[]", "word"),
        ("timestamp_granularities[]", "segment"),
        ("temperature", "0"),
    ];
    if let Some(l) = language {
        fields.push(("language", l));
    }
    let body = form(&boundary, &fields, audio);
    let mut delay = 2.0f64;
    for attempt in 1..=ATTEMPTS {
        let sent = ureq::post(URL)
            .header("Authorization", &format!("Bearer {key}"))
            .header(
                "Content-Type",
                &format!("multipart/form-data; boundary={boundary}"),
            )
            .config()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(600)))
            .build()
            .send(&body[..]);
        let wait = match sent {
            Ok(mut reply) => {
                let status = reply.status().as_u16();
                if status == 200 {
                    return reply
                        .body_mut()
                        .read_json()
                        .context("Groq sent a reply VeeCut could not read");
                }
                let retry_after = reply
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.trim().parse::<f64>().ok());
                let message = error_message(&reply.body_mut().read_to_string().unwrap_or_default());
                match status {
                    429 | 500.. if attempt < ATTEMPTS => {
                        let wait = retry_after.unwrap_or(delay).clamp(1.0, 120.0);
                        progress(&format!(
                            "Groq is busy (HTTP {status}); trying again in {wait:.0} s"
                        ));
                        wait
                    }
                    401 | 403 => bail!(
                        "Groq refused GROQ_API_KEY (HTTP {status}: {message}). Check the key at https://console.groq.com/keys"
                    ),
                    413 => bail!("Groq says the audio is too large (HTTP 413: {message})"),
                    _ => bail!("Groq transcription failed (HTTP {status}: {message})"),
                }
            }
            Err(e) if attempt < ATTEMPTS => {
                progress(&format!(
                    "Could not reach Groq ({e}); trying again in {delay:.0} s"
                ));
                delay
            }
            Err(e) => {
                return Err(e).context("could not reach Groq; check your internet connection");
            }
        };
        std::thread::sleep(Duration::from_secs_f64(wait));
        delay *= 2.0;
    }
    bail!(
        "Groq stayed busy after {ATTEMPTS} attempts; try again in a few minutes or transcribe locally"
    )
}

/// A multipart/form-data body with text fields and the MP3 as `file`.
fn form(boundary: &str, fields: &[(&str, &str)], audio: &[u8]) -> Vec<u8> {
    let mut body = Vec::with_capacity(audio.len() + 1024);
    for (name, value) in fields {
        let _ = write!(
            body,
            "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n"
        );
    }
    let _ = write!(
        body,
        "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"audio.mp3\"\r\nContent-Type: audio/mpeg\r\n\r\n"
    );
    body.extend_from_slice(audio);
    let _ = write!(body, "\r\n--{boundary}--\r\n");
    body
}

/// The `error.message` of an OpenAI-style error reply, else the start of the body.
fn error_message(body: &str) -> String {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| {
            v.pointer("/error/message")
                .and_then(|m| m.as_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| body.chars().take(300).collect())
}

/// Groq's word list has no punctuation; the segment text does. Each word takes the matching
/// segment token (looking a few tokens ahead), so sentence ends and commas survive.
fn punctuate(words: Vec<Word>, segments: &[Segment]) -> Vec<TranscriptWord> {
    let norm = |s: &str| {
        s.chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect::<String>()
    };
    let tokens: Vec<&str> = segments
        .iter()
        .flat_map(|s| s.text.split_whitespace())
        .collect();
    let mut next = 0;
    words
        .into_iter()
        .filter(|w| !w.word.trim().is_empty() && w.end >= w.start)
        .map(|w| {
            let key = norm(&w.word);
            let found = (next..(next + 4).min(tokens.len()))
                .find(|&k| !key.is_empty() && norm(tokens[k]) == key);
            let text = match found {
                Some(k) => {
                    next = k + 1;
                    tokens[k].to_string()
                }
                None => w.word.trim().to_string(),
            };
            TranscriptWord {
                text,
                start: w.start,
                end: w.end,
                confidence: None,
            }
        })
        .collect()
}

/// Whisper reports languages by name ("english"); the transcript wants ISO 639-1.
fn iso_language(name: &str) -> Option<String> {
    let name = name.trim().to_lowercase();
    if name.len() == 2 {
        return Some(name);
    }
    // ponytail: common languages only; add names here when users hit others.
    let code = match name.as_str() {
        "english" => "en",
        "tagalog" | "filipino" => "tl",
        "spanish" => "es",
        "french" => "fr",
        "german" => "de",
        "portuguese" => "pt",
        "italian" => "it",
        "japanese" => "ja",
        "korean" => "ko",
        "chinese" => "zh",
        "hindi" => "hi",
        "indonesian" => "id",
        "malay" => "ms",
        "vietnamese" => "vi",
        "thai" => "th",
        "arabic" => "ar",
        "russian" => "ru",
        "dutch" => "nl",
        _ => return None,
    };
    Some(code.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_take_punctuation_from_segments() {
        let w = |word: &str, start: f64| Word {
            word: word.into(),
            start,
            end: start + 0.2,
        };
        let words = vec![
            w("It", 0.0),
            w("costs", 0.3),
            w("20", 0.6),
            w("dollars", 0.9),
            w("OK", 1.5),
            w("so", 1.8),
        ];
        let segments = vec![
            Segment {
                text: " It costs $20 dollars.".into(),
            },
            Segment {
                text: " OK, so".into(),
            },
        ];
        let got: Vec<String> = punctuate(words, &segments)
            .into_iter()
            .map(|w| w.text)
            .collect();
        assert_eq!(got, ["It", "costs", "$20", "dollars.", "OK,", "so"]);
    }

    #[test]
    fn form_body_has_fields_and_file() {
        let body = String::from_utf8(form("b", &[("model", "whisper-large-v3")], b"MP3")).unwrap();
        assert_eq!(
            body,
            "--b\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\nwhisper-large-v3\r\n\
             --b\r\nContent-Disposition: form-data; name=\"file\"; filename=\"audio.mp3\"\r\nContent-Type: audio/mpeg\r\n\r\nMP3\r\n--b--\r\n"
        );
    }

    #[test]
    fn long_audio_splits_at_the_longest_late_pause() {
        let pauses = [(799.5, 800.0), (1100.0, 1101.0), (1900.0, 1901.0)];
        assert_eq!(
            split(3000.0, &pauses),
            [(0.0, 1100.5), (1100.5, 1900.5), (1900.5, 3000.0)]
        );
        assert_eq!(
            split(2500.0, &[]),
            [(0.0, 1200.0), (1200.0, 2400.0), (2400.0, 2500.0)]
        );
    }
}
