//! `veecut`: the command line for VeeCut Studio. Agents and people run the same commands;
//! `--json` gives agents structured output, and `veecut mcp` serves the same commands as MCP tools.

mod commands;
mod cutout_cmd;
mod footage_cmd;
mod mcp;
mod sfx_cmd;
mod verify_cmd;

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
    /// Project slug, folder or project.json path. Paths also select their workspace unless --workspace is set.
    #[arg(short, long, global = true, value_name = "P")]
    project: Option<String>,
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
    /// Bring files (B-roll, music, sound effects, images) into a project so ops can place them.
    Add(AddArgs),
    /// The transcript, or the captions as SRT or VTT.
    Transcript(TranscriptArgs),
    /// Find exact spoken words and their source/timeline ranges.
    TranscriptSearch(TranscriptSearchArgs),
    /// Cut an exact spoken phrase; choose --occurrence when it repeats.
    CutPhrase(CutPhraseArgs),
    /// Cut the speaker out over a picture, with an optional word behind their head.
    Cutout(cutout_cmd::CutoutArgs),
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
        /// Transcribe the render again and list the project's words that were masked or clipped.
        #[arg(long)]
        words: bool,
    },
    /// Hand a request to a coding agent (OpenCode with a free model by default).
    Agent(AgentArgs),
    /// Run the MCP server on stdio for agents.
    Mcp,
    /// Open the desktop editor.
    Studio(ProjectArg),
    /// Fonts available for captions and text.
    Fonts,
    /// The built-in synthesized sound kit: clicks, swishes, hits, pads.
    Sfx {
        /// Write every kit file again.
        #[arg(long)]
        regenerate: bool,
    },
    /// Search free, licence-tracked footage and pictures; numbered results and a contact sheet.
    Find(footage_cmd::FindArgs),
    /// Download a found file into footage/ and add it to the project with its licence.
    Fetch(footage_cmd::FetchArgs),
    /// The rights table of found files; --credits for the description; --approve after the user checked.
    Sources(footage_cmd::SourcesArgs),
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let ws = cli.workspace.as_deref();
    let open_project = || Ctx::open_project(ws, cli.project.as_deref());
    let mut progress = |msg: &str| {
        let _ = writeln!(std::io::stderr(), "  {msg}");
    };
    let result = match cli.command {
        Command::Init { dir } => init(dir.as_deref()),
        Command::Doctor => doctor(ws),
        Command::Edit(args) => Ctx::open(ws).and_then(|c| c.edit(args, &mut progress)),
        Command::Status(args) => open_project().and_then(|c| c.status(args)),
        Command::Transcript(args) => open_project().and_then(|c| c.transcript(args)),
        Command::TranscriptSearch(args) => open_project().and_then(|c| c.transcript_search(args)),
        Command::CutPhrase(args) => open_project().and_then(|c| c.cut_phrase(args)),
        Command::Cutout(args) => open_project().and_then(|c| c.cutout(args, &mut progress)),
        Command::Ops(args) => open_project().and_then(|c| c.ops(args)),
        Command::Schema => schema(),
        Command::Undo(args) => open_project().and_then(|c| c.undo(args, false)),
        Command::Add(args) => open_project().and_then(|c| c.add(args)),
        Command::Redo(args) => open_project().and_then(|c| c.undo(args, true)),
        Command::Render(args) => open_project().and_then(|c| c.render(args, &mut progress)),
        Command::Frame(args) => open_project().and_then(|c| c.frame(args)),
        Command::Frames(args) => open_project().and_then(|c| c.frames(args)),
        Command::Captions(args) => open_project().and_then(|c| c.captions(args)),
        Command::Note(args) => open_project().and_then(|c| c.note(args)),
        Command::Rules { command } => Ctx::open(ws).and_then(|c| c.rules(command)),
        Command::Style { command } => Ctx::open(ws).and_then(|c| c.style(command)),
        Command::Skills { command } => skills(ws, command, &mut progress),
        Command::Sfx { regenerate } => Ctx::open(ws).and_then(|c| c.sfx(regenerate)),
        Command::Find(args) => Ctx::open(ws).and_then(|c| c.find(args)),
        Command::Fetch(args) => open_project().and_then(|c| c.fetch(args, &mut progress)),
        Command::Sources(args) => open_project().and_then(|c| c.sources(args)),
        Command::Qa { file, words: false } => qa(&file),
        Command::Qa { file, words: true } => open_project().and_then(|c| {
            let project = ProjectArg {
                project: cli.project.clone(),
            };
            c.verify_words(verify_cmd::VerifyWordsArgs { file, project }, &mut progress)
        }),
        Command::Agent(args) => agent_command(ws, args),
        Command::Mcp => mcp::serve(cli.workspace.clone()).map(|()| Out::quiet()),
        Command::Studio(args) => open_project().and_then(|c| c.studio(args)),
        Command::Fonts => fonts(),
    };
    match result {
        Ok(out) => {
            if cli.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&out.json).unwrap_or_default()
                );
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_paths_establish_workspace_but_explicit_workspace_cannot_cross_roots() {
        let (_temp, ctx, dir) = commands::tests::speech_project();
        for selector in [
            dir.display().to_string(),
            dir.join("project.json").display().to_string(),
        ] {
            for argv in [
                vec!["veecut", "-p", &selector, "status"],
                vec!["veecut", "status", "-p", &selector],
            ] {
                let cli = Cli::try_parse_from(argv).unwrap();
                let selected =
                    Ctx::open_project(cli.workspace.as_deref(), cli.project.as_deref()).unwrap();
                assert_eq!(
                    selected.ws.root.canonicalize().unwrap(),
                    ctx.ws.root.canonicalize().unwrap()
                );
                let Command::Status(args) = cli.command else {
                    panic!("expected status")
                };
                assert_eq!(selected.status(args).unwrap().json["name"], "My edit");
            }
        }
        let (_other_temp, other, other_dir) = commands::tests::speech_project();
        let before = std::fs::read(other_dir.join("project.json")).unwrap();
        let selected =
            Ctx::open_project(Some(&ctx.ws.root), Some(&other_dir.display().to_string())).unwrap();
        let error = selected
            .cut_phrase(CutPhraseArgs {
                phrase: "keep".into(),
                project: ProjectArg {
                    project: Some(other_dir.display().to_string()),
                },
                ..Default::default()
            })
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains("another workspace"), "{error}");
        assert_eq!(
            before,
            std::fs::read(other_dir.join("project.json")).unwrap()
        );
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&other_dir, ctx.ws.projects().join("foreign")).unwrap();
            let error = ctx
                .status(ProjectArg {
                    project: Some("foreign".into()),
                })
                .err()
                .unwrap()
                .to_string();
            assert!(error.contains("another workspace"), "{error}");
        }
        assert_ne!(other.ws.root, ctx.ws.root);
    }

    #[test]
    fn cli_project_paths_and_phrase_commands_reach_shared_handlers() {
        let (_temp, ctx, dir) = commands::tests::speech_project();
        for selector in [
            "my-edit".to_string(),
            "projects/my-edit".into(),
            "projects/my-edit/project.json".into(),
            dir.display().to_string(),
            dir.join("project.json").display().to_string(),
        ] {
            let cli = Cli::try_parse_from(["veecut", "status", "-p", &selector]).unwrap();
            let Command::Status(args) = cli.command else {
                panic!("expected status")
            };
            assert_eq!(ctx.status(args).unwrap().json["name"], "My edit");
        }
        let cli = Cli::try_parse_from([
            "veecut",
            "transcript-search",
            "HELLO THERE",
            "-p",
            "projects/my-edit",
        ])
        .unwrap();
        let Command::TranscriptSearch(args) = cli.command else {
            panic!("expected transcript-search")
        };
        let found = ctx.transcript_search(args).unwrap();
        assert_eq!(found.json["matches"].as_array().unwrap().len(), 2);
        let before = std::fs::read(dir.join("project.json")).unwrap();
        let cli = Cli::try_parse_from([
            "veecut",
            "cut-phrase",
            "Keep",
            "-p",
            "projects/my-edit/project.json",
        ])
        .unwrap();
        let Command::CutPhrase(args) = cli.command else {
            panic!("expected cut-phrase")
        };
        let cut = ctx.cut_phrase(args).unwrap();
        assert_eq!(cut.json["cut"]["occurrence"], 1);
        assert_ne!(before, std::fs::read(dir.join("project.json")).unwrap());
        assert_eq!(std::fs::read_dir(dir.join(".history")).unwrap().count(), 1);
        ctx.undo(ProjectArg::default(), false).unwrap();
        assert_eq!(before, std::fs::read(dir.join("project.json")).unwrap());
        let cli = Cli::try_parse_from(["veecut", "status", "-p", "missing"]).unwrap();
        let Command::Status(args) = cli.command else {
            panic!("expected status")
        };
        let error = ctx.status(args).err().unwrap().to_string();
        assert!(
            error.contains("veecut status -p projects/my-edit/project.json"),
            "{error}"
        );
    }

    #[test]
    fn frame_defaults_and_project_selectors_match_mcp() {
        for selector in [
            "my-edit",
            "projects/my-edit",
            "projects/my-edit/project.json",
        ] {
            let cli = Cli::try_parse_from(["veecut", "frame", "-p", selector]).unwrap();
            let Command::Frame(cli) = cli.command else {
                panic!("expected frame")
            };
            let mcp: FrameArgs =
                serde_json::from_value(serde_json::json!({ "project": selector })).unwrap();
            assert_eq!(cli.project.project, mcp.project.project);
            assert_eq!(cli.at, mcp.at);
            assert_eq!(cli.at, 0.0);
            assert_eq!(cli.width, mcp.width);
        }
    }

    #[test]
    fn phrase_commands_use_shared_argument_names_and_defaults() {
        let cli = Cli::try_parse_from([
            "veecut",
            "transcript-search",
            "hello there",
            "-p",
            "my-edit",
        ])
        .unwrap();
        let Command::TranscriptSearch(cli) = cli.command else {
            panic!("expected transcript-search")
        };
        let mcp: TranscriptSearchArgs = serde_json::from_value(serde_json::json!({
            "phrase": "hello there", "project": "my-edit"
        }))
        .unwrap();
        assert_eq!(cli.phrase, mcp.phrase);
        assert_eq!(cli.project.project, mcp.project.project);
        let cli = Cli::try_parse_from(["veecut", "cut-phrase", "hello there", "--occurrence", "2"])
            .unwrap();
        let Command::CutPhrase(cli) = cli.command else {
            panic!("expected cut-phrase")
        };
        let mcp: CutPhraseArgs = serde_json::from_value(serde_json::json!({
            "phrase": "hello there", "occurrence": 2
        }))
        .unwrap();
        assert_eq!(cli.phrase, mcp.phrase);
        assert_eq!(cli.occurrence, mcp.occurrence);
        let cli = Cli::try_parse_from(["veecut", "cut-phrase", "hello there"]).unwrap();
        let Command::CutPhrase(cli) = cli.command else {
            panic!("expected cut-phrase")
        };
        let mcp: CutPhraseArgs =
            serde_json::from_value(serde_json::json!({ "phrase": "hello there" })).unwrap();
        assert_eq!(cli.occurrence, mcp.occurrence);
        assert_eq!(cli.occurrence, None);
        assert!(Cli::try_parse_from(["veecut", "transcript-search"]).is_err());
    }

    fn agent_args(args: &[&str]) -> Result<AgentArgs, clap::Error> {
        let argv = ["veecut", "agent"].into_iter().chain(args.iter().copied());
        match Cli::try_parse_from(argv)?.command {
            Command::Agent(args) => Ok(args),
            _ => unreachable!("parsed an agent command"),
        }
    }

    #[test]
    fn agent_takes_model_and_effort() {
        let args = agent_args(&[
            "tighter cuts",
            "--with",
            "claude",
            "--model",
            "opus",
            "--effort",
            "ultracode",
        ])
        .unwrap();
        assert_eq!(args.prompt.as_deref(), Some("tighter cuts"));
        assert_eq!(args.with.as_deref(), Some("claude"));
        assert_eq!(args.model.as_deref(), Some("opus"));
        assert_eq!(args.effort.as_deref(), Some("ultracode"));
        assert!(!args.models);
    }

    #[test]
    fn agent_lists_models_without_a_prompt_but_runs_need_one() {
        let args = agent_args(&["--models", "--with", "opencode"]).unwrap();
        assert!(args.models && args.prompt.is_none());
        assert!(agent_args(&["--with", "opencode"]).is_err());
    }
}
