//! Development launcher: `cargo run -p veecut-studio --example studio -- <workspace> [project.json]`.
//! Set `VEECUT_STUDIO_SHOTS=<empty-directory>` to capture this editor's framebuffer once
//! paused and twice during playback, then quit. Requires a project at least three seconds long.
//!
fn main() -> anyhow::Result<()> {
    let mut args = std::env::args_os().skip(1);
    let workspace = args
        .next()
        .map(std::path::PathBuf::from)
        .unwrap_or(std::env::current_dir()?);
    let project = args.next().map(std::path::PathBuf::from);
    veecut_studio::run(veecut_studio::StudioOptions { workspace, project })
}
