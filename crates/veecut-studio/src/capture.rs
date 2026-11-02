//! Opt-in documentation capture of this app's own framebuffer, without desktop automation.

use std::{
    fs::OpenOptions,
    io::Write,
    path::PathBuf,
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};
use eframe::egui;

pub(crate) enum Action {
    None,
    Play,
    /// Open the Agent tab, which shows the model and effort pickers.
    ShowAgent,
    Close,
}
enum Stage {
    Preview,
    AwaitPaused,
    First,
    AwaitFirst(f64),
    Second(f64),
    AwaitLast,
    Agent,
    AwaitAgent,
    Done,
}

pub(crate) struct Capture {
    dir: PathBuf,
    stage: Stage,
    started: Instant,
    tx: mpsc::Sender<Result<()>>,
    rx: mpsc::Receiver<Result<()>>,
    single: bool,
}

impl Capture {
    pub fn from_env(duration: Option<f64>) -> Result<Option<Self>> {
        let Some(dir) = std::env::var_os("VEECUT_STUDIO_SHOTS") else {
            return Ok(None);
        };
        let welcome = std::env::var_os("VEECUT_STUDIO_NEW_KIND").is_some();
        ensure!(
            welcome || duration.is_some_and(|duration| duration >= 3.0),
            "VEECUT_STUDIO_SHOTS needs a project at least 3 seconds long, or VEECUT_STUDIO_NEW_KIND for the new-edit form"
        );
        ensure!(
            !dir.is_empty(),
            "VEECUT_STUDIO_SHOTS must name an output directory"
        );
        let dir = PathBuf::from(dir);
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("creating screenshot directory {}", dir.display()))?;
        let (tx, rx) = mpsc::channel();
        Ok(Some(Self {
            dir,
            stage: Stage::Preview,
            started: Instant::now(),
            tx,
            rx,
            single: welcome || std::env::var_os("VEECUT_STUDIO_TAB").is_some(),
        }))
    }

    pub fn advance(
        &mut self,
        ctx: &egui::Context,
        preview_ready: bool,
        position: Option<f64>,
        agent_ready: bool,
    ) -> Result<Action> {
        if matches!(self.stage, Stage::Done) {
            return Ok(Action::None);
        }
        ensure!(
            self.started.elapsed() < Duration::from_secs(90),
            "documentation capture timed out; check preview rendering and the audio output device"
        );
        if let Ok(saved) = self.rx.try_recv() {
            saved?;
            match self.stage {
                Stage::AwaitPaused if self.single => {
                    self.stage = Stage::Done;
                    return Ok(Action::Close);
                }
                Stage::AwaitPaused => {
                    self.stage = Stage::First;
                    return Ok(Action::Play);
                }
                Stage::AwaitFirst(at) => self.stage = Stage::Second(at + 1.0),
                Stage::AwaitLast => {
                    self.stage = Stage::Agent;
                    return Ok(Action::ShowAgent);
                }
                Stage::AwaitAgent => {
                    self.stage = Stage::Done;
                    return Ok(Action::Close);
                }
                _ => bail!("received an unexpected documentation screenshot"),
            }
        }
        match self.stage {
            Stage::Preview if preview_ready => {
                self.request(ctx, "00-paused.png", position.unwrap_or_default());
                self.stage = Stage::AwaitPaused;
            }
            Stage::First if position.is_some_and(|at| at >= 1.0) => {
                let at = position.unwrap_or(1.0);
                self.request(ctx, "01-playback.png", at);
                self.stage = Stage::AwaitFirst(at);
            }
            Stage::Second(at) if position.is_some_and(|position| position >= at) => {
                self.request(ctx, "02-playback.png", position.unwrap_or(at));
                self.stage = Stage::AwaitLast;
            }
            Stage::Agent if agent_ready => {
                self.request(ctx, "03-agent.png", position.unwrap_or_default());
                self.stage = Stage::AwaitAgent;
            }
            _ => {}
        }
        Ok(Action::None)
    }

    fn request(&self, ctx: &egui::Context, name: &str, at: f64) {
        let path = self.dir.join(name);
        let tx = self.tx.clone();
        let context = ctx.clone();
        ctx.request_screenshot(move |image| {
            let result = save_png(&image, &path);
            if result.is_ok() {
                eprintln!(
                    "Studio screenshot saved: {} (playhead {at:.3}s)",
                    path.display()
                );
            }
            let _ = tx.send(result);
            context.request_repaint();
        });
    }
}

fn save_png(image: &Arc<egui::ColorImage>, path: &std::path::Path) -> Result<()> {
    let mut pixmap = tiny_skia::Pixmap::new(image.size[0] as u32, image.size[1] as u32)
        .context("invalid screenshot dimensions")?;
    for (rgba, pixel) in pixmap.data_mut().chunks_exact_mut(4).zip(&image.pixels) {
        rgba.copy_from_slice(&pixel.to_array());
    }
    let bytes = pixmap.encode_png()?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| {
            format!(
                "creating {} (choose an empty screenshot directory)",
                path.display()
            )
        })?;
    file.write_all(&bytes)
        .with_context(|| format!("saving screenshot {}", path.display()))
}
