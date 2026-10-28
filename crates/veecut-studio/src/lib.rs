//! Native desktop editor for the project files shared with VeeCut's CLI and agents.

mod app;
mod capture;
mod inspector;
mod playback;
mod services;
mod timeline;

use std::path::PathBuf;

/// Workspace and optional project to open in the editor.
pub struct StudioOptions {
    pub workspace: PathBuf,
    pub project: Option<PathBuf>,
}

/// Opens the native editor and runs until its window closes.
///
/// For documentation, set `VEECUT_STUDIO_SHOTS` to an empty directory and open a project
/// at least three seconds long. The app saves `00-paused.png`, `01-playback.png` and
/// `02-playback.png` from its own framebuffer, then closes. Playback captures are one second
/// apart. The hook is inactive when the variable is unset and never captures other windows.
pub fn run(opts: StudioOptions) -> anyhow::Result<()> {
    let app = app::Studio::new(opts)?;
    let native = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("VeeCut Studio")
            .with_inner_size([1440.0, 960.0])
            .with_min_inner_size([960.0, 640.0]),
        ..Default::default()
    };
    eframe::run_native(
        "VeeCut Studio",
        native,
        Box::new(move |cc| {
            cc.egui_ctx.set_visuals(eframe::egui::Visuals::dark());
            Ok(Box::new(app))
        }),
    )
    .map_err(|error| anyhow::anyhow!("opening the editor: {error}"))
}
