//! Local transcription with whisper.cpp (through whisper-rs), behind the `whisper` cargo feature.
//! Models come from https://huggingface.co/ggerganov/whisper.cpp and live in `~/.veecut/models`.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use crate::config;
use crate::transcript::Transcript;

/// A ggml model VeeCut knows how to fetch.
struct Model {
    name: &'static str,
    file: &'static str,
    megabytes: u32,
}

const DEFAULT_MODEL: &str = "large-v3-turbo";
const MODELS: [Model; 4] = [
    Model {
        name: "large-v3-turbo",
        file: "ggml-large-v3-turbo-q5_0.bin",
        megabytes: 574,
    },
    Model {
        name: "base",
        file: "ggml-base.bin",
        megabytes: 148,
    },
    Model {
        name: "small",
        file: "ggml-small.bin",
        megabytes: 488,
    },
    Model {
        name: "medium",
        file: "ggml-medium.bin",
        megabytes: 1533,
    },
];

fn model(name: &str) -> Result<&'static Model> {
    match MODELS.iter().find(|m| m.name == name) {
        Some(m) => Ok(m),
        None => bail!(
            "unknown local whisper model \"{name}\"; choose one of {}",
            MODELS.iter().map(|m| m.name).collect::<Vec<_>>().join(", ")
        ),
    }
}

fn model_path(m: &Model) -> PathBuf {
    config::home().join("models").join(m.file)
}

pub(super) fn status() -> String {
    let m = &MODELS[0];
    let path = model_path(m);
    if !cfg!(feature = "whisper") {
        return "local: this VeeCut was built without whisper.cpp (cargo feature `whisper`); use Groq or rebuild with it".into();
    }
    if path.is_file() {
        format!("local: ready ({} at {})", m.name, path.display())
    } else {
        format!(
            "local: ready; the {} model (about {} MB) downloads to {} on first use",
            m.name,
            m.megabytes,
            path.display()
        )
    }
}

#[cfg(not(feature = "whisper"))]
pub(super) fn transcribe(
    _: &Path,
    model: Option<&str>,
    _: Option<&str>,
    _: &mut dyn FnMut(&str),
) -> Result<Transcript> {
    self::model(model.unwrap_or(DEFAULT_MODEL))?;
    bail!(
        "this VeeCut was built without whisper.cpp; set GROQ_API_KEY to use Groq, or rebuild with the `whisper` feature"
    )
}

#[cfg(feature = "whisper")]
pub(super) fn transcribe(
    media: &Path,
    model: Option<&str>,
    language: Option<&str>,
    progress: &mut dyn FnMut(&str),
) -> Result<Transcript> {
    let m = self::model(model.unwrap_or(DEFAULT_MODEL))?;
    let path = model_path(m);
    if !path.is_file() {
        download(m, &path, progress)?;
    }
    let pcm = crate::media::pcm_mono(media, whisper::SAMPLE_RATE, None)?;
    let (words, detected) = whisper::run(&path, m.name, &pcm, language, progress)?;
    Ok(Transcript {
        asset: String::new(),
        provider: format!("local:{}", m.name),
        language: language.map(str::to_string).or(detected),
        duration: pcm.len() as f64 / whisper::SAMPLE_RATE as f64,
        words,
    })
}

/// Fetches a model into `path` through a `.part` file, so an interrupted download never looks
/// complete.
#[cfg(feature = "whisper")]
fn download(m: &Model, path: &Path, progress: &mut dyn FnMut(&str)) -> Result<()> {
    use std::io::{Read, Write};

    use anyhow::{Context, ensure};

    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let url = format!(
        "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/{}",
        m.file
    );
    progress(&format!(
        "Downloading the {} whisper model (about {} MB, first use only) to {}",
        m.name,
        m.megabytes,
        dir.display()
    ));
    let reply = ureq::get(&url)
        .call()
        .with_context(|| format!("downloading {url}; check your internet connection"))?;
    let total = reply.body().content_length();
    let mut body = reply.into_body().into_reader();
    let part = path.with_extension("bin.part");
    let mut file = std::io::BufWriter::new(
        std::fs::File::create(&part).with_context(|| format!("creating {}", part.display()))?,
    );
    let mut buf = vec![0u8; 1 << 16];
    let (mut done, mut next_report) = (0u64, 0.1f64);
    loop {
        let n = body
            .read(&mut buf)
            .context("the model download was interrupted; run again to retry")?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])?;
        done += n as u64;
        if let Some(t) = total.filter(|t| *t > 0)
            && done as f64 / t as f64 >= next_report
        {
            progress(&format!("Model download {:.0}%", next_report * 100.0));
            next_report += 0.1;
        }
    }
    file.flush()?;
    drop(file);
    if let Some(t) = total {
        ensure!(
            done == t,
            "the model download stopped early ({done} of {t} bytes); run again to retry"
        );
    }
    std::fs::rename(&part, path).with_context(|| format!("saving {}", path.display()))?;
    Ok(())
}

#[cfg(feature = "whisper")]
mod whisper {
    use std::path::Path;
    use std::sync::mpsc;
    use std::time::Duration;

    use anyhow::{Result, anyhow};
    use whisper_rs::{
        DtwMode, DtwModelPreset, DtwParameters, FullParams, SamplingStrategy, WhisperContext,
        WhisperContextParameters,
    };

    use crate::analyze;
    use crate::transcript::TranscriptWord;

    pub(super) const SAMPLE_RATE: u32 = 16_000;

    /// A word as whisper.cpp hands it over: UTF-8 bytes and the DTW time of its first token.
    struct Piece {
        bytes: Vec<u8>,
        at: f64,
        probability: f32,
        tokens: u32,
    }

    /// Runs whisper.cpp over 16 kHz mono audio. Returns words and the detected language.
    pub(super) fn run(
        model: &Path,
        name: &str,
        pcm: &[f32],
        language: Option<&str>,
        progress: &mut dyn FnMut(&str),
    ) -> Result<(Vec<TranscriptWord>, Option<String>)> {
        if pcm.is_empty() {
            return Ok((Vec::new(), None));
        }
        whisper_rs::install_logging_hooks();
        progress(&format!("Loading the {name} whisper model"));
        let preset = match name {
            "base" => DtwModelPreset::Base,
            "small" => DtwModelPreset::Small,
            "medium" => DtwModelPreset::Medium,
            _ => DtwModelPreset::LargeV3Turbo,
        };
        let mut params = WhisperContextParameters::default();
        params.dtw_parameters(DtwParameters {
            mode: DtwMode::ModelPreset {
                model_preset: preset,
            },
            ..Default::default()
        });
        let ctx = WhisperContext::new_with_params(model, params).map_err(|e| {
            anyhow!(
                "could not load {} ({e}); delete it to download it again",
                model.display()
            )
        })?;
        let mut state = ctx
            .create_state()
            .map_err(|e| anyhow!("whisper.cpp could not start: {e}"))?;

        let mut full = FullParams::new(SamplingStrategy::BeamSearch {
            beam_size: 5,
            patience: -1.0,
        });
        full.set_language(Some(language.unwrap_or("auto")));
        full.set_token_timestamps(true);
        full.set_print_progress(false);
        full.set_print_realtime(false);
        full.set_print_special(false);
        full.set_print_timestamps(false);
        full.set_suppress_nst(true);
        full.set_n_threads(
            std::thread::available_parallelism().map_or(4, |n| n.get().min(8)) as i32,
        );
        let (tx, rx) = mpsc::channel::<i32>();
        full.set_progress_callback_safe(move |p: i32| {
            let _ = tx.send(p);
        });
        progress(&format!(
            "Transcribing {:.0} s of audio with whisper.cpp ({name})",
            pcm.len() as f64 / SAMPLE_RATE as f64
        ));
        let result = std::thread::scope(|s| {
            let job = s.spawn(|| state.full(full, pcm));
            let mut last = -1;
            while !job.is_finished() {
                if let Ok(p) = rx.recv_timeout(Duration::from_millis(200))
                    && p >= last + 10
                {
                    last = p;
                    progress(&format!("Transcribing: {p}%"));
                }
            }
            job.join().map_err(|_| anyhow!("whisper.cpp crashed"))
        })?;
        result.map_err(|e| anyhow!("whisper.cpp failed: {e}"))?;

        let eot = ctx.token_eot();
        let mut pieces: Vec<Piece> = Vec::new();
        for segment in state.as_iter() {
            let mut first = true;
            for k in 0..segment.n_tokens() {
                let Some(token) = segment.get_token(k) else {
                    continue;
                };
                if token.token_id() >= eot {
                    continue; // timestamps and other special tokens
                }
                let Ok(bytes) = token.to_bytes() else {
                    continue;
                };
                let data = token.token_data();
                let at = if data.t_dtw >= 0 { data.t_dtw } else { data.t0 } as f64 / 100.0;
                match pieces.last_mut() {
                    Some(p) if !first && !bytes.starts_with(b" ") => {
                        p.bytes.extend_from_slice(bytes);
                        p.probability += token.token_probability();
                        p.tokens += 1;
                    }
                    _ => pieces.push(Piece {
                        bytes: bytes.to_vec(),
                        at,
                        probability: token.token_probability(),
                        tokens: 1,
                    }),
                }
                first = false;
            }
        }
        let detected =
            whisper_rs::get_lang_str(state.full_lang_id_from_state()).map(str::to_string);
        Ok((settle(pieces, pcm), detected))
    }

    /// Gives each word times that match the audio. whisper.cpp's DTW time marks a point inside a
    /// token, usually 0.1-0.2 s after it starts, so it says which stretch of sound a word belongs
    /// to but not where the word begins. The audio does: the first word of a stretch starts
    /// where the sound starts, the last ends where it stops, and words in between split at their
    /// DTW times moved back by the usual lag, while every word keeps room for its letters (the
    /// last word's DTW time often lands near its end). A DTW time in a pause belongs to the sound
    /// just before it (DTW runs late); a word far from any sound is a hallucination and goes.
    fn settle(pieces: Vec<Piece>, pcm: &[f32]) -> Vec<TranscriptWord> {
        const LAG: f64 = 0.1;
        const REACH: f64 = 0.5;
        // Shortest a word can be, per letter, between these bounds.
        const PER_LETTER: f64 = 0.05;
        const WORD_BOUNDS: (f64, f64) = (0.05, 0.3);
        let audio_end = pcm.len() as f64 / SAMPLE_RATE as f64;
        let pauses = analyze::silences(
            pcm,
            SAMPLE_RATE,
            analyze::quiet_threshold(pcm, SAMPLE_RATE),
            0.1,
        );
        let mut sound = Vec::new();
        let mut at = 0.0;
        for &(a, b) in &pauses {
            if a > at {
                sound.push((at, a));
            }
            at = b;
        }
        if at < audio_end {
            sound.push((at, audio_end));
        }

        let mut groups: Vec<(usize, Vec<Piece>)> = Vec::new();
        for p in pieces {
            if String::from_utf8_lossy(&p.bytes).trim().is_empty() {
                continue;
            }
            // Distance to each stretch of sound; time after a stretch counts half, since DTW runs late.
            let distance = |&(a, b): &(f64, f64)| {
                if p.at < a {
                    a - p.at
                } else {
                    (p.at - b).max(0.0) * 0.5
                }
            };
            let nearest = sound
                .iter()
                .enumerate()
                .min_by(|x, y| distance(x.1).total_cmp(&distance(y.1)));
            let Some((k, s)) = nearest else { continue };
            if distance(s) > REACH {
                continue;
            }
            let k = groups.last().map_or(k, |g| k.max(g.0));
            match groups.last_mut() {
                Some(g) if g.0 == k => g.1.push(p),
                _ => groups.push((k, vec![p])),
            }
        }

        let mut words = Vec::new();
        for (k, group) in groups {
            let (a, b) = sound[k];
            let letters = |p: &Piece| {
                String::from_utf8_lossy(&p.bytes)
                    .chars()
                    .filter(|c| c.is_alphanumeric())
                    .count()
            };
            let need: Vec<f64> = group
                .iter()
                .map(|p| (letters(p) as f64 * PER_LETTER).clamp(WORD_BOUNDS.0, WORD_BOUNDS.1))
                .collect();
            let scale = ((b - a) / need.iter().sum::<f64>()).min(1.0);
            let mut starts = vec![a];
            for (j, p) in group.iter().enumerate().skip(1) {
                let lo = starts[j - 1] + need[j - 1] * scale;
                let hi = b - need[j..].iter().sum::<f64>() * scale;
                starts.push((p.at - LAG).clamp(lo, hi.max(lo)));
            }
            for (j, p) in group.iter().enumerate() {
                words.push(TranscriptWord {
                    text: String::from_utf8_lossy(&p.bytes).trim().to_string(),
                    start: starts[j],
                    end: starts.get(j + 1).copied().unwrap_or(b),
                    confidence: Some(p.probability / p.tokens as f32),
                });
            }
        }
        words
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn words_snap_to_the_sound_they_belong_to() {
            let mut pcm = vec![0f32; SAMPLE_RATE as usize * 4];
            for (a, b) in [(0.0, 0.5), (1.0, 1.6)] {
                for s in &mut pcm[(a * 16_000.0) as usize..(b * 16_000.0) as usize] {
                    *s = 0.3;
                }
            }
            let piece = |text: &str, at: f64| Piece {
                bytes: format!(" {text}").into_bytes(),
                at,
                probability: 0.9,
                tokens: 1,
            };
            let pieces = vec![
                piece("Hello", 0.2),
                piece("there.", 0.55),
                piece("big", 1.15),
                piece("world.", 1.45),
                piece("Thanks.", 3.0),
            ];
            let got: Vec<(String, f64, f64)> = settle(pieces, &pcm)
                .into_iter()
                .map(|w| (w.text, w.start, w.end))
                .collect();
            let want = [
                ("Hello", 0.0, 0.25),
                ("there.", 0.25, 0.5),
                ("big", 1.0, 1.35),
                ("world.", 1.35, 1.6),
            ];
            assert_eq!(got.len(), want.len(), "{got:?}");
            for (g, w) in got.iter().zip(want) {
                assert!(
                    g.0 == w.0 && (g.1 - w.1).abs() < 1e-6 && (g.2 - w.2).abs() < 1e-6,
                    "{got:?}"
                );
            }
        }
    }
}
