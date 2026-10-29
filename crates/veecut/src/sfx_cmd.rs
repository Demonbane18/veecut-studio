//! `veecut sfx`: the built-in synthesized sound kit.

use std::fmt::Write as _;

use anyhow::Result;
use serde_json::json;
use veecut_engine::sfx;

use crate::commands::{Ctx, Out, rel};

impl Ctx {
    /// Lists the kit (writing any missing file first); `regenerate` writes every file again.
    pub fn sfx(&self, regenerate: bool) -> Result<Out> {
        let kit = if regenerate {
            sfx::regenerate(&self.ws)?
        } else {
            sfx::kit(&self.ws)?
        };
        let folder = rel(&self.ws, &sfx::kit_dir(&self.ws));
        let mut text = format!(
            "Sound kit v{} in {folder} ({} files, 48 kHz, peak {} dBFS)\n",
            sfx::KIT_VERSION,
            kit.len(),
            sfx::PEAK_DB
        );
        for k in &kit {
            let _ = writeln!(
                text,
                "  {:<11} {:>5.3}s  hit at {:>6.1} ms  {}",
                k.name,
                k.duration,
                k.transient * 1000.0,
                k.sound.role()
            );
        }
        let sounds: Vec<_> = kit
            .iter()
            .map(|k| {
                json!({
                    "name": k.name,
                    "sound": k.sound,
                    "role": k.sound.role(),
                    "path": rel(&self.ws, &k.path),
                    "duration": k.duration,
                    "transient": k.transient,
                    "level_lufs": k.level,
                })
            })
            .collect();
        Ok(Out::new(
            text,
            json!({
                "folder": folder,
                "version": sfx::KIT_VERSION,
                "regenerated": regenerate,
                "sounds": sounds,
            }),
        ))
    }
}
