//! Runs a coding agent (OpenCode, Claude Code, Codex, Gemini CLI) headless inside a workspace.

use std::ffi::OsString;
use std::fmt;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::workspace::Workspace;

/// A coding agent VeeCut can drive.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentKind {
    OpenCode,
    Claude,
    Codex,
    Gemini,
}

/// Stands for the prompt in agent commands, including overrides in config.toml.
const PROMPT: &str = "{prompt}";

impl AgentKind {
    pub const ALL: [AgentKind; 4] = [
        AgentKind::OpenCode,
        AgentKind::Claude,
        AgentKind::Codex,
        AgentKind::Gemini,
    ];

    /// Short name used in config.toml and on the command line.
    pub fn name(self) -> &'static str {
        match self {
            AgentKind::OpenCode => "opencode",
            AgentKind::Claude => "claude",
            AgentKind::Codex => "codex",
            AgentKind::Gemini => "gemini",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            AgentKind::OpenCode => "OpenCode",
            AgentKind::Claude => "Claude Code",
            AgentKind::Codex => "Codex",
            AgentKind::Gemini => "Gemini CLI",
        }
    }

    /// Accepts the short name or the label, in any case.
    pub fn from_name(s: &str) -> Option<Self> {
        let s = s.trim().to_ascii_lowercase().replace([' ', '_'], "-");
        let s = s
            .strip_suffix("-code")
            .or_else(|| s.strip_suffix("-cli"))
            .unwrap_or(&s);
        Self::ALL.into_iter().find(|k| k.name() == s)
    }

    /// How to install the agent.
    pub fn install_hint(self) -> &'static str {
        match self {
            AgentKind::OpenCode => "curl -fsSL https://opencode.ai/install | bash",
            AgentKind::Claude => "curl -fsSL https://claude.ai/install.sh | bash",
            AgentKind::Codex => "npm install -g @openai/codex",
            AgentKind::Gemini => "npm install -g @google/gemini-cli",
        }
    }

    /// The headless command, checked against each CLI's `--help`. `{prompt}` marks the prompt.
    fn default_command(self) -> &'static [&'static str] {
        match self {
            // The free model needs no account.
            AgentKind::OpenCode => &[
                "opencode",
                "run",
                "--model",
                "opencode/glm-5.3-flash-free",
                PROMPT,
            ],
            // Edits are accepted; the only shell command allowed without asking is veecut.
            AgentKind::Claude => &[
                "claude",
                "-p",
                "--permission-mode",
                "acceptEdits",
                "--allowedTools",
                "Bash(veecut),Bash(veecut *)",
                "--",
                PROMPT,
            ],
            AgentKind::Codex => &[
                "codex",
                "exec",
                "--sandbox",
                "workspace-write",
                "--skip-git-repo-check",
                "--",
                PROMPT,
            ],
            // --skip-trust: Gemini resets the approval mode in folders it has not been told to trust.
            // ponytail: --allowed-tools is deprecated for policy files (--policy); switch before Gemini CLI 1.0 drops it.
            AgentKind::Gemini => &[
                "gemini",
                "--skip-trust",
                "--approval-mode",
                "auto_edit",
                "--allowed-tools",
                "run_shell_command(veecut)",
                "-p",
                PROMPT,
            ],
        }
    }
}

impl fmt::Display for AgentKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// Every agent in [`AgentKind::ALL`] order with its version; `None` when it is not installed.
pub fn detect() -> Vec<(AgentKind, Option<String>)> {
    std::thread::scope(|s| {
        let handles: Vec<_> = AgentKind::ALL
            .into_iter()
            .map(|kind| {
                s.spawn(move || {
                    (
                        kind,
                        find_program(kind.default_command()[0]).map(|p| version(&p)),
                    )
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("version check panicked"))
            .collect()
    })
}

/// Runs the agent with `prompt` in the workspace root and passes every line it prints (stdout
/// and stderr) to `on_line`. Blocks until the agent exits; call it from a worker thread in a UI.
pub fn run(
    ws: &Workspace,
    kind: AgentKind,
    prompt: &str,
    on_line: &mut dyn FnMut(&str),
) -> Result<ExitStatus> {
    let config = Config::load()?;
    let mut argv: Vec<String> = match config.agent.command(kind) {
        Some(cmd) if !cmd.is_empty() => cmd.to_vec(),
        _ => kind
            .default_command()
            .iter()
            .map(|s| s.to_string())
            .collect(),
    };
    match argv.iter().position(|a| a == PROMPT) {
        Some(i) => argv[i] = prompt.to_string(),
        None => argv.push(prompt.to_string()),
    }
    let program = find_program(&argv[0]).ok_or_else(|| {
        anyhow!(
            "{} ({}) is not installed. Install it with: {}",
            kind.label(),
            argv[0],
            kind.install_hint()
        )
    })?;
    let mut child = Command::new(&program)
        .args(&argv[1..])
        .current_dir(&ws.root)
        // OpenCode takes its project folder from PWD, which the child would otherwise inherit
        // from wherever the editor or CLI was started.
        .env("PWD", &ws.root)
        .env("PATH", search_path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("starting {}", program.display()))?;
    let streams: [Box<dyn Read + Send>; 2] = [
        Box::new(child.stdout.take().expect("stdout is piped")),
        Box::new(child.stderr.take().expect("stderr is piped")),
    ];
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    std::thread::scope(|s| {
        for stream in streams {
            let tx = tx.clone();
            s.spawn(move || {
                let mut reader = BufReader::new(stream);
                let mut buf = Vec::new();
                while matches!(reader.read_until(b'\n', &mut buf), Ok(n) if n > 0) {
                    let line = String::from_utf8_lossy(&buf)
                        .trim_end_matches(['\r', '\n'])
                        .to_string();
                    buf.clear();
                    if tx.send(line).is_err() {
                        break;
                    }
                }
            });
        }
        drop(tx);
        for line in rx {
            on_line(&line);
        }
    });
    child
        .wait()
        .with_context(|| format!("waiting for {}", kind.label()))
}

/// First version-looking word of `<program> --version`, else its first line.
fn version(program: &Path) -> String {
    let out = Command::new(program)
        .arg("--version")
        .env("PATH", search_path())
        .stdin(Stdio::null())
        .output();
    let Ok(out) = out else {
        return "unknown".into();
    };
    let text = if out.stdout.iter().any(|b| !b.is_ascii_whitespace()) {
        out.stdout
    } else {
        out.stderr
    };
    let text = String::from_utf8_lossy(&text);
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("unknown");
    line.split_whitespace()
        .map(|w| w.trim_start_matches('v'))
        .find(|w| w.starts_with(|c: char| c.is_ascii_digit()) && w.contains('.'))
        .unwrap_or(line)
        .to_string()
}

/// PATH plus the folder of the running veecut and the usual install folders, so agents and the
/// tools they need are found even when a desktop app starts with a minimal PATH.
fn search_dirs() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(Path::to_path_buf))
    {
        out.push(dir);
    }
    if let Some(path) = std::env::var_os("PATH") {
        out.extend(std::env::split_paths(&path));
    }
    if let Some(home) = dirs::home_dir() {
        for dir in [
            ".opencode/bin",
            ".local/bin",
            ".bun/bin",
            ".npm-global/bin",
            ".volta/bin",
            ".cargo/bin",
        ] {
            out.push(home.join(dir));
        }
    }
    out.extend(["/opt/homebrew/bin", "/usr/local/bin"].map(PathBuf::from));
    let mut seen = std::collections::HashSet::new();
    out.retain(|d| seen.insert(d.clone()));
    out
}

fn search_path() -> OsString {
    std::env::join_paths(search_dirs())
        .unwrap_or_else(|_| std::env::var_os("PATH").unwrap_or_default())
}

pub(crate) fn find_program(name: &str) -> Option<PathBuf> {
    let direct = Path::new(name);
    if direct.components().count() > 1 {
        return direct.is_file().then(|| direct.to_path_buf());
    }
    let exts: &[&str] = if cfg!(windows) {
        &[".exe", ".cmd", ".bat", ""]
    } else {
        &[""]
    };
    search_dirs()
        .into_iter()
        .flat_map(|dir| exts.iter().map(move |ext| dir.join(format!("{name}{ext}"))))
        .find(|p| p.is_file())
}
