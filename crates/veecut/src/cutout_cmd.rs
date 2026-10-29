//! Cut out a speaker, with an optional world picture and a word behind their head.

use crate::commands::{Ctx, Out, ProjectArg};
use anyhow::{Context, Result, ensure};
use clap::Args;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use std::path::Path;
use veecut_engine::project::{AssetKind, AssetRole, TextPreset, TrackKind};
use veecut_engine::{
    matte,
    ops::{self, Op},
};

#[derive(Args, Deserialize, JsonSchema)]
pub struct CutoutArgs {
    /// Timeline start in seconds. The range must stay inside one main-track clip.
    #[arg(long)]
    pub at: f64,
    /// Timeline duration in seconds.
    #[arg(long)]
    pub dur: f64,
    /// White outline width, as a fraction of canvas height (0..0.05).
    #[arg(long, default_value_t = 0.006)]
    #[serde(default = "default_outline")]
    pub outline: f32,
    /// A mega word behind the cut-out.
    #[arg(long)]
    pub mega: Option<String>,
    /// Picture path or asset id to place underneath the cut-out.
    #[arg(long)]
    pub world: Option<String>,
    #[command(flatten)]
    #[serde(flatten)]
    pub project: ProjectArg,
}

fn default_outline() -> f32 {
    0.006
}

impl Ctx {
    pub fn cutout(&self, args: CutoutArgs, progress: &mut dyn FnMut(&str)) -> Result<Out> {
        let (dir, mut project) = self.project(&args.project)?;
        ensure!(
            args.at.is_finite() && args.at >= 0.0 && args.dur.is_finite() && args.dur > 0.0,
            "cutout needs a non-negative --at and a positive --dur"
        );
        if let Some(word) = &args.mega {
            ensure!(!word.trim().is_empty(), "--mega needs a word");
        }
        let world =
            if let Some(world) = &args.world {
                let id = if project.asset(world).is_some() {
                    world.clone()
                } else {
                    let file = self.input(Path::new(world))?;
                    // Register on a scratch project; only validated ops reach the working project.
                    let mut next = project.clone();
                    let id = self
                        .ws
                        .add_asset(&mut next, &dir, &file, AssetRole::Broll)?;
                    ops::apply(
                        &mut project,
                        &Op::Set {
                            path: "/assets".into(),
                            value: json!(next.assets),
                        },
                    )?;
                    id
                };
                ensure!(
                    project.asset(&id).context("world asset missing")?.kind == AssetKind::Image,
                    "--world needs a picture, not a video or audio asset"
                );
                if !project.tracks.iter().any(|t| {
                    t.kind == TrackKind::Overlay && (t.id == "broll" || t.name == "B-roll")
                }) {
                    ops::apply(
                        &mut project,
                        &Op::AddTrack {
                            kind: TrackKind::Overlay,
                            name: Some("B-roll".into()),
                        },
                    )?;
                }
                Some(id)
            } else {
                None
            };
        let report = matte::cutout(
            &self.ws,
            &mut project,
            &dir,
            args.at,
            args.at + args.dur,
            args.outline,
            progress,
        )?;
        let mut edits = Vec::new();
        if let Some(asset) = world {
            let track = project
                .tracks
                .iter()
                .find(|t| t.kind == TrackKind::Overlay && (t.id == "broll" || t.name == "B-roll"))
                .context("project has no B-roll track")?
                .id
                .clone();
            edits.push(Op::AddClip {
                asset,
                track: Some(track),
                start: Some(args.at),
                index: None,
                src_in: 0.0,
                duration: Some(args.dur),
                frame: None,
                volume: Some(0.0),
                duck: None,
            });
        }
        if let Some(text) = args.mega {
            edits.push(Op::AddText {
                text,
                start: args.at,
                duration: args.dur,
                preset: TextPreset::Mega,
                position: None,
                style: None,
                anim_in: None,
                anim_out: None,
                track: None,
            });
        }
        ops::apply_all(&mut project, &edits)?;
        self.ws.save_project(&dir, &project)?; // One save, one undo snapshot, including world and word.
        Ok(Out::new(
            format!(
                "Cut-out {}: {} mask frames in {:.2}s",
                report.overlay_item, report.frames, report.seconds_taken
            ),
            json!(report),
        ))
    }
}
