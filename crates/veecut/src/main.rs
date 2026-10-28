//! `veecut`: the command line for VeeCut Studio. Agents and people run the same commands;
//! `--json` gives agents structured output, and `veecut mcp` serves the same commands as MCP tools.

mod commands;
mod mcp;

use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use commands::*;

#[derive(Parser)]
#[command(
    name = "veecut",
    version,
    about = "VeeCut Studio: an AI video editor. Coding agents and a desktop editor share one timeline.",
    after_help = "Start with `veecut init` in your videos folder, put a recording in inbox/, then `veecut edit`."
)]
struct Cli {
    /// Print JSON instead of text.
    #[arg(long, global = true)]
    json: bool,
    /// The workspace folder. Default: the workspace holding the current folder.
    #[arg(long, global = true, value_name = "DIR")]
    workspace: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Set up a workspace: inbox, outbox, projects, your style and rules, the agent skill.
    Init {
        /// Folder to set up. Default: the current folder.
        dir: Option<PathBuf>,
    },
    /// Check ffmpeg, transcription, agents and the skills catalog.
    Doctor,
    /// Edit recordings with the recipe for a kind of video and render a draft.
    Edit(EditArgs),
    /// What a project holds: length, tracks, drafts, open notes.
    Status(ProjectArg),
    /// The transcript, or the captions as SRT or VTT.
    Transcript(TranscriptArgs),
    /// Apply Editor API operations (JSON) atomically.
    Ops(OpsArgs),
    /// JSON schema of the Editor API operations.
    Schema,
    /// Restore the project as it was before the last change.
    Undo(ProjectArg),
    /// Bring back what the last undo replaced.
    Redo(ProjectArg),
    /// Render a draft, or the final video with --final.
    Render(RenderArgs),
    /// Save one finished frame as PNG, to look at the edit.
    Frame(FrameArgs),
    /// Save a contact sheet of frames across the edit.
    Frames(FramesArgs),
    /// Change the caption mode or rebuild captions from the transcript.
    Captions(CaptionsArgs),
    /// Pin a note to a moment; --rule also saves it as a lasting rule.
    Note(NoteArgs),
    /// List or add lasting rules (my-rules.md).
    Rules {
        #[command(subcommand)]
        command: Option<RulesCommand>,
    },
    /// Show or change your style (my-style.toml).
    Style {
        #[command(subcommand)]
        command: Option<StyleCommand>,
    },
    /// The skills catalog from awesome-claude-video-skills: sync, search, install, update.
    Skills {
        #[command(subcommand)]
        command: Option<SkillsCommand>,
    },
    /// Check a rendered video: size, length, loudness, black frames.
    Qa {
        file: PathBuf,
    },
    /// Hand a request to a coding agent (OpenCode with a free model by default).
    Agent(AgentArgs),
    /// Run the MCP server on stdio for agents.
    Mcp,
    /// Open the desktop editor.
    Studio(ProjectArg),
    /// Fonts available for captions and text.
    Fonts,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let ws = cli.workspace.as_deref();
    let mut progress = |msg: &str| {
        let _ = writeln!(std::io::stderr(), "  {msg}");
    };
    let result = match cli.command {
        Command::Init { dir } => init(dir.as_deref()),
        Command::Doctor => doctor(ws),
        Command::Edit(args) => Ctx::open(ws).and_then(|c| c.edit(args, &mut progress)),
        Command::Status(args) => Ctx::open(ws).and_then(|c| c.status(args)),
        Command::Transcript(args) => Ctx::open(ws).and_then(|c| c.transcript(args)),
        Command::Ops(args) => Ctx::open(ws).and_then(|c| c.ops(args)),
        Command::Schema => schema(),
        Command::Undo(args) => Ctx::open(ws).and_then(|c| c.undo(args, false)),
        Command::Redo(args) => Ctx::open(ws).and_then(|c| c.undo(args, true)),
        Command::Render(args) => Ctx::open(ws).and_then(|c| c.render(args, &mut progress)),
        Command::Frame(args) => Ctx::open(ws).and_then(|c| c.frame(args)),
        Command::Frames(args) => Ctx::open(ws).and_then(|c| c.frames(args)),
        Command::Captions(args) => Ctx::open(ws).and_then(|c| c.captions(args)),
        Command::Note(args) => Ctx::open(ws).and_then(|c| c.note(args)),
        Command::Rules { command } => Ctx::open(ws).and_then(|c| c.rules(command)),
        Command::Style { command } => Ctx::open(ws).and_then(|c| c.style(command)),
        Command::Skills { command } => skills(ws, command, &mut progress),
        Command::Qa { file } => qa(&file),
        Command::Agent(args) => Ctx::open(ws).and_then(|c| c.agent(args)),
        Command::Mcp => mcp::serve(cli.workspace.clone()).map(|()| Out::quiet()),
        Command::Studio(args) => Ctx::open(ws).and_then(|c| c.studio(args)),
        Command::Fonts => fonts(),
    };
    match result {
        Ok(out) => {
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&out.json).unwrap_or_default());
            } else if !out.text.is_empty() {
                println!("{}", out.text.trim_end());
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            if cli.json {
                println!("{}", serde_json::json!({ "error": format!("{e:#}") }));
            } else {
                eprintln!("error: {e:#}");
            }
            ExitCode::FAILURE
        }
    }
}
