//! `veecut qa FILE --words` and the MCP tool `verify_words`: transcribe a render again and list
//! the words that did not come through.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use schemars::JsonSchema;
use serde::Deserialize;
use veecut_engine::config::Config;
use veecut_engine::qa;
use veecut_engine::transcribe::{Provider, TranscribeOptions};

use crate::commands::{Ctx, Out, ProjectArg, rel};

#[derive(Deserialize, JsonSchema)]
pub struct VerifyWordsArgs {
    /// The rendered video: a path, or a name in outbox/ such as my-edit-v2.mp4.
    pub file: PathBuf,
    #[serde(flatten)]
    pub project: ProjectArg,
}

impl Ctx {
    /// Transcribes a render again and compares it with the project's transcript. Uses local
    /// whisper.cpp unless the user's config picked Groq, so nothing is uploaded by default.
    pub fn verify_words(
        &self,
        args: VerifyWordsArgs,
        progress: &mut dyn FnMut(&str),
    ) -> Result<Out> {
        let file = self.render_file(&args.file)?;
        let (dir, project) = self.project(&args.project)?;
        let config = Config::load()?;
        let opts = TranscribeOptions {
            provider: Some(match config.transcribe.provider.as_str() {
                "groq" => Provider::Groq,
                _ => Provider::Local,
            }),
            model: Some(config.transcribe.local_model).filter(|m| !m.is_empty()),
            ..Default::default()
        };
        let r = qa::check_words(&project, &dir, &file, &opts, progress)?;
        let mut text = format!(
            "Words in {} ({}): {} of {} heard as written.",
            rel(&self.ws, &file),
            r.provider,
            r.matched,
            r.expected
        );
        for i in &r.issues {
            let _ = write!(text, "\n  {}  {:?} ", clock(i.at), i.expected);
            let _ = match &i.heard {
                Some(h) => write!(text, "heard as {h:?}"),
                None => write!(text, "missing"),
            };
            if let Some(s) = &i.near_sound {
                let _ = write!(text, ", sound {s} starts there");
            }
            if i.near_cut {
                let _ = write!(text, ", next to a cut");
            }
        }
        let _ = write!(text, "\n{}", r.verdict);
        Ok(Out::new(text, serde_json::to_value(&r)?))
    }

    /// A path as given, or a name inside outbox/ or the workspace.
    fn render_file(&self, path: &Path) -> Result<PathBuf> {
        for candidate in [
            path.to_path_buf(),
            self.ws.outbox().join(path),
            self.ws.root.join(path),
        ] {
            if candidate.is_file() {
                return Ok(std::path::absolute(candidate)?);
            }
        }
        bail!("{} not found (also looked in outbox/)", path.display())
    }
}

/// `m:ss.mmm`.
fn clock(t: f64) -> String {
    let ms = (t.max(0.0) * 1000.0).round() as u64;
    format!("{}:{:02}.{:03}", ms / 60_000, ms / 1000 % 60, ms % 1000)
}
