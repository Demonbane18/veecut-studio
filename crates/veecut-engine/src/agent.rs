//! Runs a coding agent (OpenCode, Claude Code, Codex, Gemini CLI) headless inside a workspace.

use std::collections::HashMap;
use std::ffi::OsString;
use std::fmt;
use std::io::{self, BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, mpsc};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(windows)]
use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE},
    System::{
        Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
        },
        JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject, TerminateJobObject,
        },
        Threading::{CREATE_SUSPENDED, OpenThread, ResumeThread, THREAD_SUSPEND_RESUME},
    },
};

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

/// The one MCP server a Claude run may load: `veecut mcp`, as the workspace's opencode.json
/// registers it for OpenCode.
const VEECUT_MCP_JSON: &str = r#"{"mcpServers":{"veecut":{"command":"veecut","args":["mcp"]}}}"#;

/// The same server for Codex, as a `-c` override: Codex runs with `--ignore-user-config`, so
/// nothing else registers it. The tools are pre-approved like Claude's `mcp__veecut` rule,
/// because an `exec` run has nobody to ask.
const CODEX_VEECUT_MCP: &str =
    r#"mcp_servers.veecut={command="veecut",args=["mcp"],default_tools_approval_mode="approve"}"#;

/// OpenCode's free model, used when no model is chosen so a run never spends money by accident.
pub const OPENCODE_DEFAULT_MODEL: &str = "opencode/glm-5.3-flash-free";

/// The Claude Code effort choice that turns on ultracode: `--effort max`, the `ultracode`
/// setting, and `Workflow` among the allowed tools.
pub const ULTRACODE: &str = "ultracode";

/// Which model and effort an agent runs with. `None` leaves the choice to the agent's CLI
/// (except OpenCode, whose model falls back to [`OPENCODE_DEFAULT_MODEL`]). Callers resolve it
/// (usually `Config::agent_selection`) and pass it to [`run`]; [`run`] never reads it from the
/// config. A custom command override ignores it. Gemini has no effort control.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Selection {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
}

/// A model an agent accepts: `id` goes after `--model`/`-m`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelChoice {
    pub id: String,
    /// One line for a picker; empty for models found only by discovery.
    pub note: String,
}

/// An effort level an agent accepts for a model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EffortChoice {
    pub id: &'static str,
    pub note: &'static str,
}

/// Tokens and cost one run reported. `input_tokens` leaves out cached tokens, so
/// [`Usage::total_tokens`] counts every token once. [`Output::Usage`] carries running totals
/// for the run so far.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    /// Fresh input tokens (not read from or written to the prompt cache).
    pub input_tokens: u64,
    /// Output tokens, including reasoning tokens.
    pub output_tokens: u64,
    /// Prompt tokens read from or written to the cache.
    pub cached_tokens: u64,
    /// Reported or API-equivalent cost in US dollars; `Some(0.0)` is free, `None` unreported.
    pub cost_usd: Option<f64>,
    /// Model turns (OpenCode steps, Codex turns, Claude `num_turns`) when reported.
    pub turns: Option<u64>,
    /// Prompt size of the run's first model request: fresh plus cached input tokens, counted
    /// once. It is the fixed context the agent loads before any work, not a total. `None` when
    /// the agent does not report it (Gemini, custom commands, or a Codex session file that
    /// cannot be read).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_request_input_tokens: Option<u64>,
}

impl Usage {
    /// Input, output and cached tokens, each counted once.
    pub fn total_tokens(&self) -> u64 {
        self.input_tokens + self.output_tokens + self.cached_tokens
    }

    /// One line for a person, such as `42 s, 61k tokens, free`. Cost shows only when reported;
    /// tokens only when counted.
    pub fn summary(&self, seconds: f64) -> String {
        let seconds = seconds.max(0.0).round() as u64;
        let mut parts = vec![if seconds < 60 {
            format!("{seconds} s")
        } else {
            format!("{} min {} s", seconds / 60, seconds % 60)
        }];
        let tokens = self.total_tokens();
        if tokens > 0 {
            parts.push(format!(
                "{} tokens",
                match tokens {
                    0..1_000 => tokens.to_string(),
                    1_000..10_000 => format!("{:.1}k", tokens as f64 / 1e3),
                    10_000..999_500 => format!("{}k", (tokens + 500) / 1_000),
                    _ => format!("{:.1}M", tokens as f64 / 1e6),
                }
            ));
        }
        match self.cost_usd {
            Some(cost) if cost <= 0.0 => parts.push("free".into()),
            Some(cost) if cost < 0.01 => parts.push(format!("${cost:.3}")),
            Some(cost) => parts.push(format!("${cost:.2}")),
            None => {}
        }
        parts.join(", ")
    }
}

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

    /// The headless command for `selection`, checked against each CLI's `--help`. `{prompt}`
    /// marks the prompt.
    fn default_command(self, selection: &Selection) -> Vec<String> {
        let clean = |value: &Option<String>| {
            value
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
        };
        let (model, effort) = (clean(&selection.model), clean(&selection.effort));
        let mut argv: Vec<String> = Vec::new();
        let mut add = |args: &[&str]| argv.extend(args.iter().map(|arg| arg.to_string()));
        match self {
            // The free model needs no account.
            AgentKind::OpenCode => {
                add(&["opencode", "run", "--format", "json"]);
                add(self.lean_args());
                add(&["--model"]);
                add(&[model.as_deref().unwrap_or(OPENCODE_DEFAULT_MODEL)]);
                if let Some(effort) = &effort {
                    add(&["--variant", effort]);
                }
                add(&[PROMPT]);
            }
            // Edits are accepted; the only shell command allowed without asking is veecut.
            // stream-json with --verbose reports usage; ultracode also needs Workflow allowed,
            // because a -p run never prompts for the workflow launch.
            AgentKind::Claude => {
                let ultracode = effort.as_deref() == Some(ULTRACODE);
                add(&[
                    "claude",
                    "-p",
                    "--output-format",
                    "stream-json",
                    "--verbose",
                    "--permission-mode",
                    "acceptEdits",
                    "--allowedTools",
                    if ultracode {
                        "Bash(veecut),Bash(veecut *),mcp__veecut,Workflow"
                    } else {
                        "Bash(veecut),Bash(veecut *),mcp__veecut"
                    },
                ]);
                add(self.lean_args());
                if let Some(model) = &model {
                    add(&["--model", model]);
                }
                if ultracode {
                    add(&["--effort", "max", "--settings", r#"{"ultracode":true}"#]);
                } else if let Some(effort) = &effort {
                    add(&["--effort", effort]);
                }
                add(&["--", PROMPT]);
            }
            AgentKind::Codex => {
                add(&[
                    "codex",
                    "exec",
                    "--json",
                    "--sandbox",
                    "workspace-write",
                    "--skip-git-repo-check",
                ]);
                add(self.lean_args());
                if let Some(model) = &model {
                    add(&["-m", model]);
                }
                if let Some(effort) = &effort {
                    // A TOML string, so the value stays one value whatever it contains.
                    let value = serde_json::to_string(effort).expect("a string serializes");
                    add(&["-c", &format!("model_reasoning_effort={value}")]);
                }
                add(&["--", PROMPT]);
            }
            // --skip-trust: Gemini resets the approval mode in folders it has not been told to trust.
            // ponytail: --allowed-tools is deprecated for policy files (--policy); switch before Gemini CLI 1.0 drops it.
            // Gemini has no effort setting; the effort is ignored.
            AgentKind::Gemini => {
                add(&[
                    "gemini",
                    "--skip-trust",
                    "--approval-mode",
                    "auto_edit",
                    "--allowed-tools",
                    "run_shell_command(veecut)",
                ]);
                add(self.lean_args());
                if let Some(model) = &model {
                    add(&["-m", model]);
                }
                add(&["-p", PROMPT]);
            }
        }
        argv
    }

    /// Per-run flags that keep a default command to VeeCut's own context instead of loading
    /// the owner's global skills, plugins and MCP servers into every request. Each is a
    /// documented CLI option; none writes settings, and sign-in is never touched (Claude's
    /// `--bare` would refuse the OAuth login, so it is not used).
    ///
    /// - OpenCode: `--pure` skips external plugins; `--print-logs --log-level ERROR` sends
    ///   provider errors (such as a used-up free allowance) to stderr, where
    ///   [`opencode_failure`] reads them. Skills come from [`AgentKind::lean_env`].
    /// - Claude: user settings (plugins, hooks, user skills and CLAUDE.md) are skipped; with
    ///   `--strict-mcp-config` only the `veecut mcp` server in [`VEECUT_MCP_JSON`] loads, and
    ///   its tools are allowed in the command's `--allowedTools`.
    ///   ponytail: user-level `apiKeyHelper` or provider `env` settings are skipped too; OAuth
    ///   and `ANTHROPIC_API_KEY` still work. Add `user` back to the sources if that bites.
    /// - Codex: `--ignore-user-config` skips `config.toml` (its MCP servers, plugins and hooks;
    ///   the login in `auth.json` stays), so `veecut mcp` is registered again with an `-c`
    ///   override and its tools are pre-approved, as Claude's are. `skills.include_instructions`
    ///   drops the catalog of every installed skill; the workspace AGENTS.md names the veecut
    ///   skill by path.
    ///   ponytail: model, network and service-tier defaults from `config.toml` do not apply;
    ///   `~/.codex/AGENTS.md` still loads.
    /// - Gemini: no extensions, and only a server named `veecut` may start. Needs no sign-in.
    ///   ponytail: Gemini has no flag for its own skills or hooks.
    fn lean_args(self) -> &'static [&'static str] {
        match self {
            AgentKind::OpenCode => &["--pure", "--print-logs", "--log-level", "ERROR"],
            AgentKind::Claude => &[
                "--setting-sources",
                "project,local",
                "--strict-mcp-config",
                "--mcp-config",
                VEECUT_MCP_JSON,
            ],
            AgentKind::Codex => &[
                "--ignore-user-config",
                "-c",
                "skills.include_instructions=false",
                "-c",
                CODEX_VEECUT_MCP,
            ],
            AgentKind::Gemini => &["-e", "none", "--allowed-mcp-server-names", "veecut"],
        }
    }

    /// Environment that goes with [`AgentKind::lean_args`] for a run in `root`.
    ///
    /// OpenCode reads skills from `~/.agents/skills` and `~/.claude/skills` (472 on the
    /// owner's machine) plus `.agents/skills` and `.claude/skills` in the project.
    /// `OPENCODE_DISABLE_EXTERNAL_SKILLS` turns all of those off, project folders included,
    /// so the workspace's own `.agents/skills` is added back through the inline config's
    /// `skills.paths`; `OPENCODE_DISABLE_CLAUDE_CODE` also drops `~/.claude/CLAUDE.md`.
    /// OpenCode's global config folder (`$XDG_CONFIG_HOME/opencode`: plugins, MCP servers,
    /// providers, AGENTS.md) is pointed at the workspace's own `.veecut/agent-config`, which
    /// OpenCode creates empty; its data folder, where the sign-in lives, is untouched, and the
    /// workspace's `opencode.json` still registers the veecut MCP server.
    /// ponytail: custom providers defined only in the person's global config are not seen.
    ///
    /// Claude's auto-memory is switched off so no per-user memory enters the prompt.
    fn lean_env(self, root: &Path) -> Vec<(&'static str, String)> {
        match self {
            AgentKind::OpenCode => vec![
                ("OPENCODE_DISABLE_CLAUDE_CODE", "1".into()),
                ("OPENCODE_DISABLE_EXTERNAL_SKILLS", "1".into()),
                (
                    "XDG_CONFIG_HOME",
                    root.join(".veecut/agent-config")
                        .to_string_lossy()
                        .into_owned(),
                ),
                (
                    "OPENCODE_CONFIG_CONTENT",
                    serde_json::json!({
                        "skills": { "paths": [root.join(".agents/skills").to_string_lossy()] }
                    })
                    .to_string(),
                ),
            ],
            AgentKind::Claude => vec![("CLAUDE_CODE_DISABLE_AUTO_MEMORY", "1".into())],
            AgentKind::Codex | AgentKind::Gemini => Vec::new(),
        }
    }
}

impl fmt::Display for AgentKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// Models VeeCut knows for `kind`, best starting point first. Instant and offline; for the full
/// OpenCode list use [`models`].
pub fn known_models(kind: AgentKind) -> Vec<ModelChoice> {
    let list: &[(&str, &str)] = match kind {
        AgentKind::OpenCode => &[
            (
                OPENCODE_DEFAULT_MODEL,
                "Free, 200k context, reads images (default)",
            ),
            (
                "opencode/deepseek-v4.1-flash-free",
                "Free, 200k context, reads images",
            ),
            (
                "opencode/mimo-v2.6-flash-free",
                "Free, 200k context, reads images",
            ),
            (
                "opencode/nemotron-3.5-super-vl-free",
                "Free, 1M context, reads images",
            ),
            (
                "opencode/nemotron-3-ultra-free",
                "Free, 1M context, text only",
            ),
            (
                "opencode/nemotron-3.5-lightning-free",
                "Free, 262k context, text only",
            ),
            ("opencode/flint-40b-free", "Free, 262k context, text only"),
            ("opencode/beam-free", "Free, 1M context, text only"),
            ("opencode/big-pickle", "Free, 200k context, text only"),
        ],
        AgentKind::Claude => &[
            ("sonnet", "Balanced (claude-sonnet-5-5)"),
            ("haiku", "Fastest and cheapest (claude-haiku-5-5)"),
            ("opus", "Strongest everyday model (claude-opus-5-5)"),
            (
                "fable",
                "Most capable and most expensive (claude-fable-5-1)",
            ),
        ],
        AgentKind::Codex => &[
            ("gpt-5.6-terra", "Balanced"),
            ("gpt-5.6-luna", "Fast and efficient"),
            ("gpt-reserve", "Fast and affordable"),
            ("gpt-5.6-sol", "Workhorse"),
            ("gpt-6-astra", "Frontier"),
        ],
        AgentKind::Gemini => &[
            ("gemini-2.5-pro", "Most capable"),
            ("gemini-2.5-flash", "Fast"),
            ("gemini-2.5-flash-lite", "Fastest and cheapest"),
        ],
    };
    list.iter()
        .map(|(id, note)| ModelChoice {
            id: (*id).into(),
            note: (*note).into(),
        })
        .collect()
}

/// [`known_models`] plus, for OpenCode, every model `opencode models` lists. Runs the CLI for
/// up to 15 seconds, so call it off the UI thread; fall back to [`known_models`] on error.
pub fn models(kind: AgentKind) -> Result<Vec<ModelChoice>> {
    let mut list = known_models(kind);
    if kind == AgentKind::OpenCode {
        for id in opencode_models()? {
            if !list.iter().any(|model| model.id == id) {
                list.push(ModelChoice {
                    id,
                    note: String::new(),
                });
            }
        }
    }
    Ok(list)
}

/// The `provider/model` ids `opencode models` prints.
pub fn opencode_models() -> Result<Vec<String>> {
    let program = find_program("opencode").ok_or_else(|| {
        anyhow!(
            "OpenCode is not installed. Install it with: {}",
            AgentKind::OpenCode.install_hint()
        )
    })?;
    let root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let output = check_command(
        &program,
        &["models".into()],
        &root,
        Duration::from_secs(15),
        1 << 20,
    )?;
    if !output.success {
        let error = output.stderr.trim();
        anyhow::bail!(
            "opencode models failed: {}",
            if error.is_empty() {
                "no diagnostic output"
            } else {
                error
            }
        );
    }
    Ok(output
        .stdout
        .lines()
        .map(str::trim)
        .filter(|line| line.contains('/') && !line.contains(char::is_whitespace))
        .map(str::to_owned)
        .collect())
}

/// Effort levels `kind` accepts for `model` (`None` is the agent's own pick), lowest first.
/// Empty means no effort control. Claude ends with [`ULTRACODE`], the most expensive choice.
/// Any effort can still be passed to [`run`]; this list only drives pickers.
pub fn efforts(kind: AgentKind, model: Option<&str>) -> Vec<EffortChoice> {
    fn level(id: &'static str) -> EffortChoice {
        EffortChoice {
            id,
            note: match id {
                "none" => "No extra thinking",
                "low" => "Least thinking, fastest",
                "medium" => "Balanced",
                "high" => "More thinking",
                "xhigh" => "Much more thinking",
                "max" => "Most thinking",
                "ultra" => "Even more thinking than max",
                _ => "",
            },
        }
    }
    let levels = |ids: &[&'static str]| ids.iter().copied().map(level).collect::<Vec<_>>();
    match kind {
        AgentKind::Gemini => Vec::new(),
        AgentKind::Claude => {
            // The plan lists these levels for every Claude model; a model that rejects one makes
            // the CLI say so. Ultracode needs a model that supports xhigh.
            let mut list = levels(&["low", "medium", "high", "xhigh", "max"]);
            list.push(EffortChoice {
                id: ULTRACODE,
                note: "Most expensive: about 5x the tokens. Claude plans a workflow with its own subagents. Needs a model that supports xhigh.",
            });
            list
        }
        AgentKind::Codex => {
            let mut list = levels(&["low", "medium", "high", "xhigh", "max"]);
            // ponytail: only gpt-6-astra is known to take ultra.
            if model == Some("gpt-6-astra") {
                list.push(level("ultra"));
            }
            list
        }
        // Variants differ by model; this is what the free models report. Other OpenCode models
        // list none here, and --effort still passes any variant through.
        AgentKind::OpenCode => match model.unwrap_or(OPENCODE_DEFAULT_MODEL) {
            "opencode/glm-5.3-flash-free" | "opencode/deepseek-v4.1-flash-free" => {
                levels(&["low", "high", "max"])
            }
            "opencode/nemotron-3.5-super-vl-free" => {
                levels(&["none", "low", "medium", "high", "xhigh"])
            }
            "opencode/flint-40b-free" | "opencode/beam-free" => {
                levels(&["low", "medium", "high", "xhigh", "max"])
            }
            _ => Vec::new(),
        },
    }
}

/// The prompt for a note typed in Studio. The editor previews every change as it lands, so the
/// agent has no reason to render or open a video unless the note asks for one.
pub fn studio_prompt(project: &Path, note: &str) -> String {
    format!(
        "This note comes from VeeCut Studio. The editor is open on this project and previews every change as soon as you apply it.\n\
         Apply the note with veecut commands. To remove spoken words, run veecut cut-phrase \"exact words\" (add --occurrence N when the words repeat; veecut transcript-search \"exact words\" lists the matches; the MCP tools are cut_phrase and transcript_search). Use veecut ops for every other edit. To check the result you may run veecut frame, which saves a PNG in the project's .cache folder.\n\
         Do not run veecut render and do not open any file unless the note asks for a video.\n\
         Answer in one or two lines, then stop.\n\
         Project: {}\n{note}",
        project.display()
    )
}

/// Read-only readiness, separate from whether a CLI can print a version.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Readiness {
    Ready,
    NotInstalled,
    SignedOut,
    CheckFailed,
    /// Installed, but this CLI or custom command has no supported sign-in check.
    Unverified,
}

#[derive(Clone, Debug, Serialize)]
pub struct AgentStatus {
    pub kind: AgentKind,
    pub readiness: Readiness,
    pub detail: String,
    pub fix: Option<String>,
    pub command_override: bool,
}

impl AgentStatus {
    pub fn can_run(&self) -> bool {
        matches!(self.readiness, Readiness::Ready | Readiness::Unverified)
    }

    fn new(kind: AgentKind, readiness: Readiness, detail: impl Into<String>) -> Self {
        Self {
            kind,
            readiness,
            detail: detail.into(),
            fix: None,
            command_override: false,
        }
    }
}

/// Every agent in [`AgentKind::ALL`] order. Checks never sign in or edit settings.
/// Supported auth checks take at most five seconds, plus the bounded pipe drain.
pub fn detect() -> Vec<AgentStatus> {
    let config = Config::load().map_err(|error| format!("{error:#}"));
    thread::scope(|scope| {
        let handles: Vec<_> = AgentKind::ALL
            .into_iter()
            .map(|kind| {
                let config = config.as_ref().map_err(String::as_str);
                scope.spawn(move || detect_kind(kind, config))
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("agent readiness check panicked"))
            .collect()
    })
}

fn detect_kind(kind: AgentKind, config: std::result::Result<&Config, &str>) -> AgentStatus {
    let config = match config {
        Ok(config) => config,
        Err(error) => {
            let mut status = AgentStatus::new(
                kind,
                Readiness::CheckFailed,
                format!("Cannot read VeeCut configuration: {error}"),
            );
            status.fix = Some(format!("Fix {}", Config::path().display()));
            return status;
        }
    };
    if let Some(command) = config.agent.command(kind).filter(|cmd| !cmd.is_empty()) {
        let mut status = if find_program(&command[0]).is_some() {
            AgentStatus::new(
                kind,
                Readiness::Unverified,
                "Custom command found; sign-in and output format are not verified.",
            )
        } else {
            let mut status = AgentStatus::new(
                kind,
                Readiness::NotInstalled,
                format!("Custom command not found: {}", command[0]),
            );
            status.fix = Some(format!(
                "Install {} or fix the command override in {}",
                command[0],
                Config::path().display()
            ));
            status
        };
        status.command_override = true;
        return status;
    }
    let Some(program) = find_program(kind.name()) else {
        let mut status = AgentStatus::new(kind, Readiness::NotInstalled, "Not installed.");
        status.fix = Some(kind.install_hint().into());
        return status;
    };
    let args = match kind {
        AgentKind::Claude => &["auth", "status"][..],
        AgentKind::Codex => &["login", "status"][..],
        AgentKind::OpenCode => {
            return AgentStatus::new(
                kind,
                Readiness::Ready,
                "Default free model needs no sign-in. Provider availability is not checked.",
            );
        }
        _ => {
            return AgentStatus::new(
                kind,
                Readiness::Unverified,
                "Installed; sign-in and provider access are not verified.",
            );
        }
    };
    let args: Vec<_> = args.iter().map(|arg| (*arg).to_owned()).collect();
    let root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    match check_command(&program, &args, &root, Duration::from_secs(5), 64 * 1024) {
        Ok(output) => auth_status(kind, &output),
        Err(error) => {
            let mut status = AgentStatus::new(
                kind,
                Readiness::CheckFailed,
                format!("{} sign-in check failed: {error:#}", kind.label()),
            );
            status.fix = Some(format!(
                "Run {} {} status to inspect the error; fix CLI configuration and retry.",
                kind.name(),
                if kind == AgentKind::Claude {
                    "auth"
                } else {
                    "login"
                }
            ));
            status
        }
    }
}

#[derive(Debug)]
struct CheckOutput {
    success: bool,
    stdout: String,
    stderr: String,
}

fn auth_status(kind: AgentKind, output: &CheckOutput) -> AgentStatus {
    let signed_in = match kind {
        AgentKind::Claude => serde_json::from_str::<serde_json::Value>(&output.stdout)
            .ok()
            .and_then(|json| json.get("loggedIn").and_then(|value| value.as_bool())),
        AgentKind::Codex => {
            let text = format!("{}\n{}", output.stdout, output.stderr).to_ascii_lowercase();
            if text.lines().any(|line| line.trim() == "not logged in") {
                Some(false)
            } else if text
                .lines()
                .any(|line| line.trim().starts_with("logged in using "))
            {
                Some(true)
            } else {
                None
            }
        }
        _ => None,
    };
    match signed_in {
        Some(true) if output.success => AgentStatus::new(
            kind,
            Readiness::Ready,
            "Signed in. Usage limits and provider access are not checked.",
        ),
        Some(false) => {
            let mut status = AgentStatus::new(kind, Readiness::SignedOut, "Signed out.");
            status.fix = Some(
                if kind == AgentKind::Claude {
                    "claude auth login"
                } else {
                    "codex login"
                }
                .into(),
            );
            status
        }
        _ => {
            let detail = if output.success {
                "Unrecognized sign-in status output.".into()
            } else {
                let error = output.stderr.trim();
                format!(
                    "Sign-in check failed: {}",
                    if error.is_empty() {
                        "no diagnostic output".into()
                    } else {
                        error.chars().take(600).collect::<String>()
                    }
                )
            };
            let mut status = AgentStatus::new(kind, Readiness::CheckFailed, detail);
            status.fix = Some(format!(
                "Run {} {} status; fix CLI configuration or check errors, then retry.",
                kind.name(),
                if kind == AgentKind::Claude {
                    "auth"
                } else {
                    "login"
                }
            ));
            status
        }
    }
}

fn check_command(
    program: &Path,
    args: &[String],
    root: &Path,
    timeout: Duration,
    max_output: usize,
) -> Result<CheckOutput> {
    let stop = Stop::default();
    let timer_stop = stop.clone();
    let (done, timer) = mpsc::channel();
    thread::spawn(move || {
        if matches!(
            timer.recv_timeout(timeout),
            Err(mpsc::RecvTimeoutError::Timeout)
        ) {
            timer_stop.stop();
        }
    });
    let mut output = CheckOutput {
        success: false,
        stdout: String::new(),
        stderr: String::new(),
    };
    let mut too_large = false;
    let spawn = Spawn {
        program,
        args,
        root,
        env: &[],
    };
    let result = launch(&spawn, &stop, &mut |stream, line| {
        if output.stdout.len() + output.stderr.len() + line.len() + 1 > max_output {
            too_large = true;
            stop.stop();
            return;
        }
        let text = match stream {
            Stream::Stdout => &mut output.stdout,
            Stream::Stderr => &mut output.stderr,
        };
        text.push_str(&line);
        text.push('\n');
    });
    let _ = done.send(());
    if too_large {
        anyhow::bail!("command output exceeded {} KiB", max_output / 1024);
    }
    match result? {
        Outcome::Finished(status) => output.success = status.success(),
        Outcome::Stopped => anyhow::bail!(
            "sign-in check timed out after {} seconds",
            timeout.as_secs_f32()
        ),
    }
    Ok(output)
}

/// Assistant text is kept separately from tool output and diagnostics.
#[derive(Clone, Debug, PartialEq)]
pub enum Output {
    Answer(String),
    Log(String),
    /// Running totals for this run so far, sent after each line that reports usage. The last
    /// one is the run's usage.
    Usage(Usage),
}

impl Output {
    /// The text of an answer or log line; empty for [`Output::Usage`].
    pub fn text(&self) -> &str {
        match self {
            Self::Answer(text) | Self::Log(text) => text,
            Self::Usage(_) => "",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stream {
    Stdout,
    Stderr,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OutputFormat {
    Plain,
    Codex,
    OpenCode,
    Claude,
}

/// Turns an agent's output lines into [`Output`] and keeps the run's running usage.
struct Decoder {
    format: OutputFormat,
    usage: Usage,
    /// Claude: the latest usage per assistant message id, so a stopped run still counts.
    claude_messages: HashMap<String, Usage>,
    /// Claude: the `result` event has set the final totals.
    claude_done: bool,
    /// Codex: the thread id from `thread.started`.
    codex_thread: Option<String>,
}

fn count(value: &serde_json::Value) -> u64 {
    value.as_u64().unwrap_or(0)
}

/// Claude's `usage` object (also the shape inside each assistant message).
fn claude_usage(usage: &serde_json::Value) -> Usage {
    Usage {
        input_tokens: count(&usage["input_tokens"]),
        output_tokens: count(&usage["output_tokens"]),
        cached_tokens: count(&usage["cache_creation_input_tokens"])
            + count(&usage["cache_read_input_tokens"]),
        ..Usage::default()
    }
}

impl Decoder {
    fn new(format: OutputFormat) -> Self {
        Decoder {
            format,
            usage: Usage::default(),
            claude_messages: HashMap::new(),
            claude_done: false,
            codex_thread: None,
        }
    }

    /// Sends what `line` means: its answer or else the line as a log, then the running usage
    /// when the line reported any.
    fn decode(&mut self, stream: Stream, line: String, emit: &mut dyn FnMut(Output)) {
        if stream == Stream::Stderr {
            return emit(Output::Log(line));
        }
        if self.format == OutputFormat::Plain {
            return emit(Output::Answer(line));
        }
        let Ok(mut json) = serde_json::from_str::<serde_json::Value>(&line) else {
            return emit(Output::Log(line));
        };
        let reported = self.record(&json);
        let answer = match self.format {
            OutputFormat::Codex
                if json["type"] == "item.completed" && json["item"]["type"] == "agent_message" =>
            {
                json.pointer_mut("/item/text").map(serde_json::Value::take)
            }
            OutputFormat::OpenCode if json["type"] == "text" => {
                json.pointer_mut("/part/text").map(serde_json::Value::take)
            }
            // Subagent (sidechain) text stays in the log.
            OutputFormat::Claude
                if json["type"] == "assistant" && json["parent_tool_use_id"].is_null() =>
            {
                let text: Vec<&str> = json["message"]["content"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|block| block["type"] == "text")
                    .filter_map(|block| block["text"].as_str())
                    .filter(|text| !text.is_empty())
                    .collect();
                (!text.is_empty()).then(|| text.join("\n\n").into())
            }
            _ => None,
        };
        emit(match answer {
            Some(serde_json::Value::String(text)) => Output::Answer(text),
            _ => Output::Log(line),
        });
        if reported {
            emit(Output::Usage(self.usage.clone()));
        }
    }

    /// Adds the usage `json` reports to the running totals; false when it reports none.
    fn record(&mut self, json: &serde_json::Value) -> bool {
        let usage = &mut self.usage;
        match (self.format, json["type"].as_str().unwrap_or("")) {
            (OutputFormat::OpenCode, "step_finish") => {
                let tokens = &json["part"]["tokens"];
                usage.input_tokens += count(&tokens["input"]);
                usage.output_tokens += count(&tokens["output"]) + count(&tokens["reasoning"]);
                usage.cached_tokens +=
                    count(&tokens["cache"]["read"]) + count(&tokens["cache"]["write"]);
                if let Some(cost) = json["part"]["cost"].as_f64() {
                    *usage.cost_usd.get_or_insert(0.0) += cost;
                }
                *usage.turns.get_or_insert(0) += 1;
                let prompt = count(&tokens["input"])
                    + count(&tokens["cache"]["read"])
                    + count(&tokens["cache"]["write"]);
                if usage.first_request_input_tokens.is_none() && prompt > 0 {
                    usage.first_request_input_tokens = Some(prompt);
                }
            }
            (OutputFormat::Codex, "thread.started") => {
                // The native session file for this thread holds the first request's size.
                self.codex_thread = json["thread_id"].as_str().map(str::to_owned);
                return false;
            }
            (OutputFormat::Codex, "turn.completed") => {
                // Codex counts cached tokens inside input_tokens.
                let reported = &json["usage"];
                let cached = count(&reported["cached_input_tokens"]);
                usage.input_tokens += count(&reported["input_tokens"]).saturating_sub(cached);
                usage.output_tokens += count(&reported["output_tokens"]);
                usage.cached_tokens += cached;
                *usage.turns.get_or_insert(0) += 1;
            }
            (OutputFormat::Claude, "assistant") if !self.claude_done => {
                let message = &json["message"];
                let Some(id) = message["id"]
                    .as_str()
                    .filter(|_| message["usage"].is_object())
                else {
                    return false;
                };
                self.claude_messages
                    .insert(id.to_owned(), claude_usage(&message["usage"]));
                usage.input_tokens = self.claude_messages.values().map(|u| u.input_tokens).sum();
                usage.output_tokens = self.claude_messages.values().map(|u| u.output_tokens).sum();
                usage.cached_tokens = self.claude_messages.values().map(|u| u.cached_tokens).sum();
                let first = claude_usage(&message["usage"]);
                if usage.first_request_input_tokens.is_none()
                    && json["parent_tool_use_id"].is_null()
                    && first.input_tokens + first.cached_tokens > 0
                {
                    usage.first_request_input_tokens =
                        Some(first.input_tokens + first.cached_tokens);
                }
            }
            (OutputFormat::Claude, "result") => {
                // modelUsage covers subagents too (ultracode); usage is the main thread.
                let mut total = Usage::default();
                for model in json["modelUsage"]
                    .as_object()
                    .into_iter()
                    .flat_map(|m| m.values())
                {
                    total.input_tokens += count(&model["inputTokens"]);
                    total.output_tokens += count(&model["outputTokens"]);
                    total.cached_tokens += count(&model["cacheReadInputTokens"])
                        + count(&model["cacheCreationInputTokens"]);
                }
                if total.total_tokens() == 0 {
                    total = claude_usage(&json["usage"]);
                }
                if total.total_tokens() > 0 {
                    usage.input_tokens = total.input_tokens;
                    usage.output_tokens = total.output_tokens;
                    usage.cached_tokens = total.cached_tokens;
                }
                usage.cost_usd = json["total_cost_usd"].as_f64().or(usage.cost_usd);
                usage.turns = json["num_turns"].as_u64().or(usage.turns);
                self.claude_done = true;
            }
            _ => return false,
        }
        true
    }
}

/// How [`run`] ended.
#[derive(Debug)]
pub enum Outcome {
    /// The agent exited on its own.
    Finished(ExitStatus),
    /// [`Stop::stop`] ended it, with every process it had started.
    Stopped,
}

/// Ends a [`run`] from any thread. Clones share one run: create one per run, hand a clone to
/// the thread that wants to stop it and pass the other to [`run`].
///
/// Calling [`Stop::stop`] before `run` starts the agent means the agent never starts. Calling it
/// after the run ended does nothing.
#[derive(Clone, Default)]
pub struct Stop(Arc<Mutex<Tree>>);

#[derive(Default)]
struct Tree {
    stopped: bool,
    /// Everything the agent starts, while it runs.
    group: Option<Group>,
}

impl Stop {
    fn lock(&self) -> MutexGuard<'_, Tree> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Kills the agent and everything it started (MCP servers, hooks, shells), then returns.
    /// [`run`] returns [`Outcome::Stopped`] once the agent is reaped and its output drained.
    pub fn stop(&self) {
        let mut tree = self.lock();
        tree.stopped = true;
        if let Some(group) = tree.group.take() {
            group.kill();
        }
    }
}

/// The command for a run: the user's override as written (the selection is ignored and stdout
/// is the reply), else the default command for `selection`. `{prompt}` is replaced with
/// `prompt`, which is appended when absent.
fn build_command(
    kind: AgentKind,
    custom: Option<&[String]>,
    selection: &Selection,
    prompt: &str,
) -> (Vec<String>, OutputFormat) {
    let (mut argv, format) = match custom {
        Some(command) => (command.to_vec(), OutputFormat::Plain),
        None => (
            kind.default_command(selection),
            match kind {
                AgentKind::Codex => OutputFormat::Codex,
                AgentKind::OpenCode => OutputFormat::OpenCode,
                AgentKind::Claude => OutputFormat::Claude,
                AgentKind::Gemini => OutputFormat::Plain,
            },
        ),
    };
    match argv.iter().position(|arg| arg == PROMPT) {
        Some(i) => argv[i] = prompt.to_string(),
        None => argv.push(prompt.to_string()),
    }
    (argv, format)
}

/// Runs the agent with `prompt` in the workspace root, with the model and effort in
/// `selection`. Assistant replies and raw logs stay separate; custom commands keep plain stdout
/// as the reply without assuming a JSON format. [`Output::Usage`] reports tokens and cost as the
/// agent gives them. Blocks until exit or Stop; call from a worker thread in a UI.
///
/// The default commands load only VeeCut's context (see [`AgentKind::lean_args`]): the global
/// skills, plugins and MCP servers of the person's agent setup stay out of every request, and
/// their sign-in is untouched. A custom command gets none of that and runs as written.
///
/// The default OpenCode command also stops at once with an error when OpenCode says its free
/// allowance is used up or the model is unavailable. It runs with `--print-logs --log-level
/// ERROR`, so provider errors reach stderr, where they are caught; a run that ends without
/// edits or an answer can still be a refused model.
pub fn run(
    ws: &Workspace,
    kind: AgentKind,
    prompt: &str,
    selection: &Selection,
    stop: &Stop,
    on_output: &mut dyn FnMut(Output),
) -> Result<Outcome> {
    let config = Config::load()?;
    let custom = config.agent.command(kind).filter(|cmd| !cmd.is_empty());
    let (argv, format) = build_command(kind, custom, selection, prompt);
    let program = find_program(&argv[0]).ok_or_else(|| {
        anyhow!(
            "{} ({}) is not installed. Install it with: {}",
            kind.label(),
            argv[0],
            kind.install_hint()
        )
    })?;
    // Only the command VeeCut builds is known to speak OpenCode's output.
    let opencode_model = (kind == AgentKind::OpenCode && custom.is_none()).then(|| {
        selection
            .model
            .as_deref()
            .map(str::trim)
            .filter(|model| !model.is_empty())
            .unwrap_or(OPENCODE_DEFAULT_MODEL)
    });
    let env = if custom.is_none() {
        kind.lean_env(&ws.root)
    } else {
        Vec::new()
    };
    drive(
        &Spawn {
            program: &program,
            args: &argv[1..],
            root: &ws.root,
            env: &env,
        },
        stop,
        format,
        opencode_model,
        on_output,
    )
}

/// Launches the agent and decodes its output. With `opencode_model` set, a failure OpenCode
/// reports stops the run and becomes the error, after the output and usage seen so far.
fn drive(
    spawn: &Spawn,
    stop: &Stop,
    format: OutputFormat,
    opencode_model: Option<&str>,
    on_output: &mut dyn FnMut(Output),
) -> Result<Outcome> {
    let mut decoder = Decoder::new(format);
    let mut failure: Option<String> = None;
    let started = chrono::Local::now().date_naive();
    let outcome = launch(spawn, stop, &mut |stream, line| {
        let found = match opencode_model {
            Some(model) if failure.is_none() => opencode_failure(stream, &line, model),
            _ => None,
        };
        decoder.decode(stream, line, on_output);
        if found.is_some() {
            failure = found;
            stop.stop();
        }
    });
    // Codex's JSON stream sums the turn's requests; the first request's size is only in the
    // session file Codex keeps for the thread.
    if let Some(thread) = &decoder.codex_thread
        && let Some(dir) = codex_sessions_dir()
        && let Some(first) =
            codex_first_request(&dir, thread, [started, chrono::Local::now().date_naive()])
    {
        decoder.usage.first_request_input_tokens = Some(first);
        on_output(Output::Usage(decoder.usage.clone()));
    }
    match failure {
        Some(message) => Err(anyhow!(message)),
        None => outcome,
    }
}

/// `$CODEX_HOME/sessions` (`~/.codex/sessions` unless `CODEX_HOME` is set), where Codex writes
/// one `rollout-<time>-<thread id>.jsonl` per thread under `YYYY/MM/DD`.
fn codex_sessions_dir() -> Option<PathBuf> {
    let home = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".codex")))?;
    Some(home.join("sessions"))
}

/// The prompt size of the first model request in the Codex session file for `thread`: the
/// first `token_count` event's `last_token_usage.input_tokens`, which already includes the
/// cached tokens. Only that number is read from the file. `days` are the local dates the run
/// may have been filed under (start and end, in case it crossed midnight).
fn codex_first_request(sessions: &Path, thread: &str, days: [chrono::NaiveDate; 2]) -> Option<u64> {
    use chrono::Datelike;
    let suffix = format!("-{thread}.jsonl");
    let file = days.iter().enumerate().find_map(|(i, day)| {
        if i == 1 && days[0] == days[1] {
            return None;
        }
        let dir = sessions
            .join(format!("{:04}", day.year()))
            .join(format!("{:02}", day.month()))
            .join(format!("{:02}", day.day()));
        std::fs::read_dir(dir).ok()?.flatten().find_map(|entry| {
            let name = entry.file_name();
            let name = name.to_str()?;
            (name.starts_with("rollout-") && name.ends_with(&suffix)).then(|| entry.path())
        })
    })?;
    BufReader::new(std::fs::File::open(file).ok()?)
        .lines()
        .map_while(Result::ok)
        // The file also holds the conversation; skip every line that is not a token count
        // before parsing it.
        .filter(|line| line.contains("\"token_count\""))
        .find_map(|line| {
            let json: serde_json::Value = serde_json::from_str(&line).ok()?;
            (json["payload"]["type"] == "token_count")
                .then(|| json["payload"]["info"]["last_token_usage"]["input_tokens"].as_u64())?
        })
}

/// The error for a line in which OpenCode reports that the free allowance is used up or the
/// model is unavailable. On stdout only OpenCode's own `error` events and non-JSON lines count,
/// so a reply or tool result that merely quotes the words does not stop the run.
fn opencode_failure(stream: Stream, line: &str, model: &str) -> Option<String> {
    if stream == Stream::Stdout
        && let Ok(json) = serde_json::from_str::<serde_json::Value>(line)
        && json.is_object()
        && json["type"] != "error"
    {
        return None;
    }
    let line = line.to_ascii_lowercase();
    if line.contains("free usage limit reached") || line.contains("freeusagelimiterror") {
        Some(
            "OpenCode's free allowance is used up. It can take a day or more to come back; pick another agent or model."
                .into(),
        )
    } else if line.contains("model is unavailable") {
        Some(format!(
            "OpenCode says the model {model} is unavailable. Pick another model or agent."
        ))
    } else {
        None
    }
}

/// What the threads around one agent tell [`launch`].
enum Msg {
    Line(Stream, String),
    /// The agent was reaped; true when [`Stop::stop`] was called while it ran.
    Exit(io::Result<ExitStatus>, bool),
}

/// How long output that is still in the pipes may take to arrive after the agent is gone.
const DRAIN: Duration = Duration::from_secs(2);

/// The process one run starts: `program args` in `root`, plus `env` on top of the inherited
/// environment.
struct Spawn<'a> {
    program: &'a Path,
    args: &'a [String],
    root: &'a Path,
    env: &'a [(&'static str, String)],
}

fn launch(spawn: &Spawn, stop: &Stop, on_line: &mut dyn FnMut(Stream, String)) -> Result<Outcome> {
    let Spawn {
        program,
        args,
        root,
        env,
    } = *spawn;
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(root)
        // OpenCode takes its project folder from PWD, which the child would otherwise inherit
        // from wherever the editor or CLI was started.
        .env("PWD", root)
        .env("PATH", search_path())
        .envs(env.iter().map(|(key, value)| (key, value)))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // The agent starts in a group that Stop can kill as a whole: a process group of its own on
    // Unix, a job object on Windows (the agent starts suspended and joins it before it runs).
    // ponytail: on Unix a helper that makes its own group or session escapes Stop; the upgrade
    // is walking the process tree before the kill.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_SUSPENDED);
    }
    let mut child = {
        // Spawning under the lock means a concurrent stop() either wins and nothing starts,
        // or finds the group registered and kills it.
        let mut tree = stop.lock();
        if tree.stopped {
            return Ok(Outcome::Stopped);
        }
        let mut child = command
            .spawn()
            .with_context(|| format!("starting {}", program.display()))?;
        match Group::adopt(&child) {
            Ok(group) => tree.group = Some(group),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error).context("grouping the agent's processes");
            }
        }
        child
    };
    // Bounded, so a flood of output waits in the pipe instead of in front of the Exit message.
    let (tx, rx) = mpsc::sync_channel(256);
    forward(
        child.stdout.take().expect("stdout is piped"),
        Stream::Stdout,
        tx.clone(),
    );
    forward(
        child.stderr.take().expect("stderr is piped"),
        Stream::Stderr,
        tx.clone(),
    );
    let watcher = stop.clone();
    thread::spawn(move || {
        wait_for_exit(&mut child);
        let stopped = {
            let mut tree = watcher.lock();
            // An agent that exits on its own can leave helpers behind that hold its pipes open.
            if let Some(group) = tree.group.take() {
                group.kill();
            }
            tree.stopped
        };
        let _ = tx.send(Msg::Exit(child.wait(), stopped));
    });
    // The readers and the watcher own their threads: a helper that escaped the kill and still
    // holds a pipe cannot block this function, it only keeps one idle reader alive.
    let mut exit = None;
    let mut deadline: Option<Instant> = None;
    loop {
        let msg = match deadline {
            None => rx.recv().ok(),
            Some(deadline) => {
                // Queued lines would satisfy even a zero timeout, so check the clock first.
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    None
                } else {
                    rx.recv_timeout(left).ok()
                }
            }
        };
        match msg {
            Some(Msg::Line(stream, line)) => on_line(stream, line),
            Some(Msg::Exit(status, stopped)) => {
                exit = Some((status, stopped));
                deadline = Some(Instant::now() + DRAIN);
            }
            None => break,
        }
    }
    let (status, stopped) = exit.ok_or_else(|| anyhow!("lost track of {}", program.display()))?;
    let status = status.with_context(|| format!("waiting for {}", program.display()))?;
    Ok(if stopped {
        Outcome::Stopped
    } else {
        Outcome::Finished(status)
    })
}

/// Sends every line of `stream` to `tx` until the stream closes or nobody listens.
fn forward(stream: impl Read + Send + 'static, source: Stream, tx: mpsc::SyncSender<Msg>) {
    thread::spawn(move || {
        let mut reader = BufReader::new(stream);
        let mut buf = Vec::new();
        while matches!(reader.read_until(b'\n', &mut buf), Ok(n) if n > 0) {
            let line = String::from_utf8_lossy(&buf)
                .trim_end_matches(['\r', '\n'])
                .to_string();
            buf.clear();
            if tx.send(Msg::Line(source, line)).is_err() {
                break;
            }
        }
    });
}

/// Everything one agent starts, so that Stop and a normal exit can end it all.
#[cfg(unix)]
struct Group(libc::pid_t);

#[cfg(unix)]
impl Group {
    /// The agent leads a process group of its own (see `launch`), which its pid names.
    fn adopt(child: &Child) -> io::Result<Self> {
        Ok(Group(child.id() as libc::pid_t))
    }

    fn kill(&self) {
        // SAFETY: kill(2) takes plain integers. The negative pid names the process group; it
        // outlives a leader that exited first, and `wait_for_exit` keeps the leader unreaped
        // until this ran, so the number cannot belong to another process by now.
        unsafe {
            libc::kill(-self.0, libc::SIGKILL);
        }
    }
}

/// Waits for the agent to exit without reaping it, so that its pid stays reserved while the
/// group is killed.
#[cfg(unix)]
fn wait_for_exit(child: &mut Child) {
    // SAFETY: waitid only writes the zeroed siginfo_t it is given.
    unsafe {
        let mut info: libc::siginfo_t = std::mem::zeroed();
        while libc::waitid(
            libc::P_PID,
            child.id() as libc::id_t,
            &mut info,
            libc::WEXITED | libc::WNOWAIT,
        ) == -1
            && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted
        {}
    }
}

/// A job object that kills every process in it when it is terminated or its last handle closes.
#[cfg(windows)]
struct Group(HANDLE);

// SAFETY: a job handle can be used from any thread.
#[cfg(windows)]
unsafe impl Send for Group {}

#[cfg(windows)]
impl Group {
    /// Puts `child`, which `launch` started suspended, in a new job and lets it run, so it
    /// cannot start a helper before it belongs to the job.
    fn adopt(child: &Child) -> io::Result<Self> {
        use std::os::windows::io::AsRawHandle;
        // SAFETY: plain Win32 calls on handles owned here; `Group`'s Drop closes the job on
        // every early return.
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return Err(io::Error::last_os_error());
            }
            let group = Group(job);
            let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                (&raw const limits).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            ) == 0
                || AssignProcessToJobObject(job, child.as_raw_handle()) == 0
            {
                return Err(io::Error::last_os_error());
            }
            resume(child.id())?;
            Ok(group)
        }
    }

    fn kill(&self) {
        // SAFETY: the handle is a job object owned by `self`.
        unsafe {
            TerminateJobObject(self.0, 1);
        }
    }
}

#[cfg(windows)]
impl Drop for Group {
    fn drop(&mut self) {
        // SAFETY: the handle is owned by `self` and closed once.
        unsafe {
            CloseHandle(self.0);
        }
    }
}

/// Resumes the thread of the suspended process `pid`; std does not hand out its thread handle.
#[cfg(windows)]
fn resume(pid: u32) -> io::Result<()> {
    // SAFETY: plain Win32 calls; every handle opened here is closed here.
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        let mut entry: THREADENTRY32 = std::mem::zeroed();
        entry.dwSize = size_of::<THREADENTRY32>() as u32;
        let mut resumed = false;
        let mut more = Thread32First(snapshot, &mut entry);
        while more != 0 {
            if entry.th32OwnerProcessID == pid {
                let thread = OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID);
                if !thread.is_null() {
                    resumed |= ResumeThread(thread) != u32::MAX;
                    CloseHandle(thread);
                }
            }
            more = Thread32Next(snapshot, &mut entry);
        }
        CloseHandle(snapshot);
        if resumed {
            Ok(())
        } else {
            Err(io::Error::other("the agent's thread could not be resumed"))
        }
    }
}

/// Waits for the agent to exit. std remembers the status, so reaping later is instant.
#[cfg(windows)]
fn wait_for_exit(child: &mut Child) {
    let _ = child.wait();
}

/// PATH plus the folder of the running veecut and the usual install folders, so agents and the
/// tools they need are found even when a desktop app starts with a minimal PATH.
fn search_dirs() -> Vec<PathBuf> {
    search_dirs_for(std::env::current_exe().ok().as_deref())
}

/// The folder next to `exe` when `exe` is a VeeCut product executable (the CLI or Studio),
/// which ships its helpers beside it. Any other executable that links the engine, such as the
/// evaluation harness or a test binary, has no bundled helpers; its folder (`target/debug`)
/// can hold a stale `veecut` that must not shadow the installed one.
fn bundled_dir(exe: &Path) -> Option<&Path> {
    let product = matches!(
        exe.file_stem().and_then(|name| name.to_str()),
        Some("veecut" | "veecut-studio")
    );
    product.then(|| exe.parent()).flatten()
}

fn search_dirs_for(exe: Option<&Path>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(dir) = exe.and_then(bundled_dir) {
        out.push(dir.to_path_buf());
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

#[cfg(test)]
mod search_tests {
    use super::*;

    #[test]
    fn only_a_product_executable_puts_its_folder_ahead_of_the_installed_tools() {
        let build = Path::new("/nowhere/target/debug");
        for product in ["veecut", "veecut-studio", "veecut.exe", "veecut-studio.exe"] {
            let dirs = search_dirs_for(Some(&build.join(product)));
            assert_eq!(dirs.first().map(PathBuf::as_path), Some(build), "{product}");
        }
        // The harness and test binaries sit beside whatever `cargo build` left there, which can
        // be an older `veecut` than the installed one, so their folder is not searched.
        for helper in ["veecut-eval", "veecut-eval.exe", "veecut_engine-3f9a1c2b7d"] {
            let dirs = search_dirs_for(Some(&build.join(helper)));
            assert!(!dirs.iter().any(|dir| dir == build), "{helper}: {dirs:?}");
        }
        assert!(!search_dirs_for(None).iter().any(|dir| dir == build));
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// Runs `launch` on a fake agent (`sh -c script`); fails instead of hanging when it does.
    fn run_sh(
        root: &Path,
        script: &str,
        stop: &Stop,
        mut on_line: impl FnMut(&str) + Send + 'static,
    ) -> Outcome {
        let (root, script, stop) = (root.to_owned(), script.to_owned(), stop.clone());
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let args = ["-c".to_string(), script];
            let spawn = Spawn {
                program: Path::new("/bin/sh"),
                args: &args,
                root: &root,
                env: &[],
            };
            let result = launch(&spawn, &stop, &mut |_, line| on_line(&line));
            let _ = tx.send(result);
        });
        rx.recv_timeout(Duration::from_secs(20))
            .expect("run did not return")
            .unwrap()
    }

    /// The fake agents print `helper <pid>` for the `sleep 300` they leave in the background.
    fn note_helper(pid: &AtomicU32, line: &str) {
        if let Some(n) = line.strip_prefix("helper ") {
            pid.store(n.trim().parse().unwrap(), Ordering::SeqCst);
        }
    }

    /// A zombie that init has not reaped yet counts as gone.
    fn alive(pid: u32) -> bool {
        let out = Command::new("ps")
            .args(["-o", "stat=", "-p", &pid.to_string()])
            .output()
            .unwrap();
        let stat = String::from_utf8_lossy(&out.stdout);
        !stat.trim().is_empty() && !stat.trim().starts_with('Z')
    }

    fn assert_gone(pid: u32) {
        assert_ne!(pid, 0, "the agent printed no helper pid");
        for _ in 0..100 {
            if !alive(pid) {
                return;
            }
            thread::sleep(Duration::from_millis(50));
        }
        let _ = Command::new("kill").arg(pid.to_string()).status();
        panic!("helper {pid} is still running");
    }

    #[test]
    fn stop_ends_the_agent_and_its_helpers() {
        let tmp = tempfile::tempdir().unwrap();
        let stop = Stop::default();
        let pid = Arc::new(AtomicU32::new(0));
        let (stopper, seen) = (stop.clone(), pid.clone());
        let outcome = run_sh(
            tmp.path(),
            "sleep 300 & echo helper $!; wait",
            &stop,
            move |line| {
                note_helper(&seen, line);
                if seen.load(Ordering::SeqCst) != 0 {
                    stopper.stop();
                }
            },
        );
        assert!(matches!(outcome, Outcome::Stopped));
        assert_gone(pid.load(Ordering::SeqCst));
    }

    #[test]
    fn stop_from_another_thread_ends_a_silent_agent() {
        let tmp = tempfile::tempdir().unwrap();
        let stop = Stop::default();
        let stopper = stop.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(300));
            stopper.stop();
        });
        let outcome = run_sh(tmp.path(), "sleep 300", &stop, |_| {});
        assert!(matches!(outcome, Outcome::Stopped));
    }

    #[test]
    fn stop_before_the_agent_starts_means_it_never_starts() {
        let tmp = tempfile::tempdir().unwrap();
        let marker = tmp.path().join("ran");
        let stop = Stop::default();
        stop.stop();
        let outcome = run_sh(
            tmp.path(),
            &format!("touch '{}'", marker.display()),
            &stop,
            |_| {},
        );
        assert!(matches!(outcome, Outcome::Stopped));
        assert!(!marker.exists());
    }

    #[test]
    fn an_agent_that_exits_cannot_leave_helpers_holding_its_output() {
        let tmp = tempfile::tempdir().unwrap();
        let pid = Arc::new(AtomicU32::new(0));
        let seen = pid.clone();
        let outcome = run_sh(
            tmp.path(),
            "sleep 300 & echo helper $!",
            &Stop::default(),
            move |line| note_helper(&seen, line),
        );
        assert!(matches!(outcome, Outcome::Finished(status) if status.success()));
        assert_gone(pid.load(Ordering::SeqCst));
    }

    fn drive_sh(
        root: &Path,
        script: &str,
        format: OutputFormat,
        opencode_model: Option<&str>,
        on_output: &mut dyn FnMut(Output),
    ) -> Result<Outcome> {
        let args = ["-c".to_string(), script.to_string()];
        let spawn = Spawn {
            program: Path::new("/bin/sh"),
            args: &args,
            root,
            env: &[],
        };
        drive(&spawn, &Stop::default(), format, opencode_model, on_output)
    }

    #[test]
    fn the_run_environment_reaches_the_agent_and_stderr_error_logs_stop_opencode() {
        let tmp = tempfile::tempdir().unwrap();
        let args = ["-c".to_string(), r#"echo "$LEAN_PROBE""#.to_string()];
        let env = [("LEAN_PROBE", "on".to_string())];
        let spawn = Spawn {
            program: Path::new("/bin/sh"),
            args: &args,
            root: tmp.path(),
            env: &env,
        };
        let mut seen = Vec::new();
        drive(
            &spawn,
            &Stop::default(),
            OutputFormat::Plain,
            None,
            &mut |output| seen.push(output),
        )
        .unwrap();
        assert_eq!(seen, [Output::Answer("on".into())]);

        // `--print-logs --log-level ERROR` puts OpenCode's private error log on stderr.
        let error = drive_sh(
            tmp.path(),
            r#"echo 'ERROR 2026-10-29T01:00:00 +12ms service=llm providerID=opencode error=FreeUsageLimitError: Free usage limit reached' >&2; sleep 300"#,
            OutputFormat::OpenCode,
            Some(OPENCODE_DEFAULT_MODEL),
            &mut |_| {},
        )
        .unwrap_err();
        assert!(error.to_string().contains("free allowance is used up"));
    }

    #[test]
    fn opencode_free_limit_stops_the_run_and_keeps_earlier_usage() {
        let tmp = tempfile::tempdir().unwrap();
        let script = r#"echo '{"type":"step_finish","part":{"cost":0,"tokens":{"input":5,"output":2,"reasoning":0,"cache":{"read":0,"write":0}}}}'; echo 'Error: FreeUsageLimitError' >&2; sleep 300"#;
        let mut seen = Vec::new();
        let started = Instant::now();
        let error = drive_sh(
            tmp.path(),
            script,
            OutputFormat::OpenCode,
            Some(OPENCODE_DEFAULT_MODEL),
            &mut |output| seen.push(output),
        )
        .unwrap_err();
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "the run was not stopped"
        );
        assert_eq!(
            error.to_string(),
            "OpenCode's free allowance is used up. It can take a day or more to come back; pick another agent or model."
        );
        assert!(
            seen.iter()
                .any(|o| matches!(o, Output::Usage(u) if u.total_tokens() == 7))
        );
        assert!(seen.contains(&Output::Log("Error: FreeUsageLimitError".into())));

        // The same text from a custom command (no OpenCode watch) is only output.
        let outcome = drive_sh(
            tmp.path(),
            "echo 'Free usage limit reached'",
            OutputFormat::Plain,
            None,
            &mut |_| {},
        )
        .unwrap();
        assert!(matches!(outcome, Outcome::Finished(status) if status.success()));
    }

    #[test]
    fn opencode_unavailable_model_stops_the_run_with_its_name() {
        let tmp = tempfile::tempdir().unwrap();
        let error = drive_sh(
            tmp.path(),
            r#"echo '{"type":"error","error":{"data":{"message":"Model is unavailable"}}}'; sleep 300"#,
            OutputFormat::OpenCode,
            Some("opencode/big-pickle"),
            &mut |_| {},
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "OpenCode says the model opencode/big-pickle is unavailable. Pick another model or agent."
        );
    }

    /// Kills a helper that escaped the group on purpose, even when the test fails.
    struct KillOnDrop(Arc<AtomicU32>);

    impl Drop for KillOnDrop {
        fn drop(&mut self) {
            let pid = self.0.load(Ordering::SeqCst);
            if pid != 0 {
                let _ = Command::new("kill").arg(pid.to_string()).status();
            }
        }
    }

    #[test]
    fn a_helper_outside_the_group_cannot_hold_run_open() {
        let tmp = tempfile::tempdir().unwrap();
        let pid = Arc::new(AtomicU32::new(0));
        let _cleanup = KillOnDrop(pid.clone());
        let seen = pid.clone();
        // `set -m` gives the background job a process group of its own, out of reach of Stop.
        let outcome = run_sh(
            tmp.path(),
            "set -m; sleep 300 & echo helper $!",
            &Stop::default(),
            move |line| note_helper(&seen, line),
        );
        assert!(matches!(outcome, Outcome::Finished(status) if status.success()));
    }
}

#[cfg(test)]
mod opencode_failure_tests {
    use super::*;

    #[test]
    fn only_opencode_errors_and_plain_lines_count_not_quoted_text() {
        let m = OPENCODE_DEFAULT_MODEL;
        let text =
            r#"{"type":"text","part":{"text":"The Free usage limit reached message means..."}}"#;
        let tool = r#"{"type":"tool_use","part":{"state":{"output":"FreeUsageLimitError"}}}"#;
        let error = r#"{"type":"error","error":{"name":"FreeUsageLimitError"}}"#;
        assert_eq!(opencode_failure(Stream::Stdout, text, m), None);
        assert_eq!(opencode_failure(Stream::Stdout, tool, m), None);
        assert!(opencode_failure(Stream::Stdout, error, m).is_some());
        assert!(opencode_failure(Stream::Stdout, "Free usage limit reached", m).is_some());
        assert!(opencode_failure(Stream::Stderr, tool, m).is_some());
        assert_eq!(opencode_failure(Stream::Stdout, "all good", m), None);
    }
}

#[cfg(test)]
mod output_tests {
    use super::*;

    /// Decodes every line as stdout, in order.
    fn decode_all(format: OutputFormat, lines: &[&str]) -> Vec<Output> {
        let mut decoder = Decoder::new(format);
        let mut out = Vec::new();
        for line in lines {
            decoder.decode(Stream::Stdout, (*line).into(), &mut |o| out.push(o));
        }
        out
    }

    fn last_usage(out: &[Output]) -> Usage {
        out.iter()
            .rev()
            .find_map(|o| match o {
                Output::Usage(usage) => Some(usage.clone()),
                _ => None,
            })
            .expect("a usage snapshot")
    }

    #[test]
    fn codex_tool_records_and_noise_are_not_assistant_answers() {
        let records = [
            r#"{"type":"thread.started","thread_id":"thread-1"}"#,
            r#"{"type":"item.completed","item":{"id":"item_1","type":"command_execution","command":"veecut status --json","aggregated_output":"{\"duration\":124.5}","exit_code":0,"status":"completed"}}"#,
            "hook output before final answer",
            r#"{"type":"item.completed","item":{"id":"item_2","type":"agent_message","text":"Captions are bigger. Review them in Studio."}}"#,
            r#"{"type":"turn.completed","usage":{"input_tokens":1200,"cached_input_tokens":1000,"output_tokens":20}}"#,
        ];
        let output = decode_all(OutputFormat::Codex, &records);
        assert_eq!(output.len(), 6);
        assert_eq!(
            output[3],
            Output::Answer("Captions are bigger. Review them in Studio.".into())
        );
        assert!(output[..3].iter().all(|out| matches!(out, Output::Log(_))));
        assert!(matches!(&output[4], Output::Log(_)));
        // Codex counts cached tokens inside input_tokens; the total counts them once.
        let usage = last_usage(&output);
        assert_eq!(
            usage,
            Usage {
                input_tokens: 200,
                output_tokens: 20,
                cached_tokens: 1000,
                cost_usd: None,
                turns: Some(1),
                // Codex's stream sums the turn; the first request is read from its session file.
                first_request_input_tokens: None,
            }
        );
        assert_eq!(usage.total_tokens(), 1220);
        let mut stderr = Vec::new();
        Decoder::new(OutputFormat::Codex)
            .decode(Stream::Stderr, records[3].into(), &mut |o| stderr.push(o));
        assert!(matches!(stderr.as_slice(), [Output::Log(_)]));
    }

    #[test]
    fn opencode_text_parts_survive_tool_output_and_malformed_records() {
        let records = [
            r#"{"type":"tool_use","timestamp":1793200000000,"sessionID":"ses_1","part":{"type":"tool","tool":"veecut_ops","state":{"status":"completed","output":"{\"ok\":true}"}}}"#,
            "{broken json",
            r#"{"type":"text","timestamp":1793200000001,"sessionID":"ses_1","part":{"id":"prt_1","type":"text","text":"Applied the note."}}"#,
            r#"{"type":"step_finish","part":{"type":"step-finish","reason":"stop","cost":0}}"#,
        ];
        let output = decode_all(OutputFormat::OpenCode, &records);
        assert!(matches!(output[0], Output::Log(_)));
        assert!(matches!(output[1], Output::Log(_)));
        assert_eq!(output[2], Output::Answer("Applied the note.".into()));
        assert!(matches!(output[3], Output::Log(_)));
        // A free step: cost reported as zero, no tokens in the record.
        let usage = last_usage(&output);
        assert_eq!(usage.cost_usd, Some(0.0));
        assert_eq!(usage.turns, Some(1));
    }

    #[test]
    fn opencode_steps_add_up_run_by_run() {
        let records = [
            r#"{"type":"step_finish","part":{"type":"step-finish","cost":0,"tokens":{"input":100,"output":10,"reasoning":5,"cache":{"read":50,"write":0}}}}"#,
            r#"{"type":"step_finish","part":{"type":"step-finish","cost":0.25,"tokens":{"input":20,"output":3,"reasoning":0,"cache":{"read":70,"write":5}}}}"#,
        ];
        let output = decode_all(OutputFormat::OpenCode, &records);
        let snapshots: Vec<_> = output
            .iter()
            .filter_map(|o| match o {
                Output::Usage(usage) => Some(usage.total_tokens()),
                _ => None,
            })
            .collect();
        assert_eq!(snapshots, [165, 263]);
        assert_eq!(
            last_usage(&output),
            Usage {
                input_tokens: 120,
                output_tokens: 18,
                cached_tokens: 125,
                cost_usd: Some(0.25),
                turns: Some(2),
                // The first step only: 100 fresh + 50 read + 0 written.
                first_request_input_tokens: Some(150),
            }
        );
    }

    #[test]
    fn claude_stream_json_gives_answers_and_usage_that_survives_a_stop() {
        let init = r#"{"type":"system","subtype":"init","session_id":"s1","tools":["Bash"]}"#;
        let text = r#"{"type":"assistant","message":{"id":"m1","role":"assistant","content":[{"type":"text","text":"Working."}],"usage":{"input_tokens":10,"cache_creation_input_tokens":20,"cache_read_input_tokens":30,"output_tokens":5}},"parent_tool_use_id":null}"#;
        // The same message id again (one event per content block) with its final output count.
        let tool = r#"{"type":"assistant","message":{"id":"m1","role":"assistant","content":[{"type":"tool_use","name":"Bash","input":{}}],"usage":{"input_tokens":10,"cache_creation_input_tokens":20,"cache_read_input_tokens":30,"output_tokens":8}},"parent_tool_use_id":null}"#;
        let sub = r#"{"type":"assistant","message":{"id":"s9","role":"assistant","content":[{"type":"text","text":"subagent chatter"}],"usage":{"input_tokens":1,"output_tokens":1}},"parent_tool_use_id":"toolu_1"}"#;
        let result = r#"{"type":"result","subtype":"success","is_error":false,"result":"Done.","total_cost_usd":0.2,"num_turns":2,"usage":{"input_tokens":15,"cache_creation_input_tokens":20,"cache_read_input_tokens":80,"output_tokens":20}}"#;

        let output = decode_all(OutputFormat::Claude, &[init, text, tool, sub]);
        assert!(matches!(output[0], Output::Log(_)));
        assert_eq!(output[1], Output::Answer("Working.".into()));
        // A stop here still has the tokens spent so far, counted once per message.
        assert_eq!(
            output[2],
            Output::Usage(Usage {
                input_tokens: 10,
                output_tokens: 5,
                cached_tokens: 50,
                first_request_input_tokens: Some(60),
                ..Usage::default()
            })
        );
        assert!(matches!(output[3], Output::Log(_)));
        assert_eq!(last_usage(&output[..5]).output_tokens, 8);
        assert!(
            matches!(output[5], Output::Log(_)),
            "subagent text is a log"
        );
        assert_eq!(last_usage(&output).output_tokens, 9);

        let output = decode_all(OutputFormat::Claude, &[text, tool, result]);
        assert_eq!(
            last_usage(&output),
            Usage {
                input_tokens: 15,
                output_tokens: 20,
                cached_tokens: 100,
                cost_usd: Some(0.2),
                turns: Some(2),
                // The first message: 10 fresh + 20 written + 30 read.
                first_request_input_tokens: Some(60),
            }
        );
    }

    #[test]
    fn claude_result_counts_subagents_through_model_usage() {
        let result = r#"{"type":"result","subtype":"success","total_cost_usd":1.5,"num_turns":9,"usage":{"input_tokens":1,"output_tokens":1},"modelUsage":{"claude-opus-5-5":{"inputTokens":7,"outputTokens":3,"cacheReadInputTokens":10,"cacheCreationInputTokens":2,"costUSD":1.0},"claude-haiku-5-5":{"inputTokens":1,"outputTokens":2,"cacheReadInputTokens":0,"cacheCreationInputTokens":0,"costUSD":0.5}}}"#;
        let usage = last_usage(&decode_all(OutputFormat::Claude, &[result]));
        assert_eq!(
            (usage.input_tokens, usage.output_tokens, usage.cached_tokens),
            (8, 5, 12)
        );
        assert_eq!(usage.cost_usd, Some(1.5));
    }

    #[test]
    fn plain_stdout_is_reply_without_json_guessing_and_stderr_is_log() {
        for line in [
            "Applied the note.",
            r#"{"type":"text","part":{"text":"literal JSON"}}"#,
        ] {
            assert_eq!(
                decode_all(OutputFormat::Plain, &[line]),
                [Output::Answer(line.into())]
            );
            let mut out = Vec::new();
            Decoder::new(OutputFormat::Plain)
                .decode(Stream::Stderr, line.into(), &mut |o| out.push(o));
            assert_eq!(out, [Output::Log(line.into())]);
        }
    }

    #[test]
    fn usage_summary_reads_like_a_receipt() {
        let usage = |input, cost| Usage {
            input_tokens: input,
            cost_usd: cost,
            ..Usage::default()
        };
        assert_eq!(
            usage(61_000, Some(0.0)).summary(42.0),
            "42 s, 61k tokens, free"
        );
        assert_eq!(
            usage(1_234, Some(0.2)).summary(125.0),
            "2 min 5 s, 1.2k tokens, $0.20"
        );
        assert_eq!(
            usage(500, Some(0.009)).summary(3.0),
            "3 s, 500 tokens, $0.009"
        );
        assert_eq!(usage(2_500_000, None).summary(0.0), "0 s, 2.5M tokens");
        assert_eq!(Usage::default().summary(9.4), "9 s");
    }

    fn argv(kind: AgentKind, model: Option<&str>, effort: Option<&str>) -> Vec<String> {
        let selection = Selection {
            model: model.map(Into::into),
            effort: effort.map(Into::into),
        };
        build_command(kind, None, &selection, "go").0
    }

    #[test]
    fn opencode_command_pins_the_free_model_and_passes_the_variant() {
        assert_eq!(
            argv(AgentKind::OpenCode, None, None),
            [
                "opencode",
                "run",
                "--format",
                "json",
                "--pure",
                "--print-logs",
                "--log-level",
                "ERROR",
                "--model",
                "opencode/glm-5.3-flash-free",
                "go"
            ]
        );
        assert_eq!(
            argv(AgentKind::OpenCode, Some("opencode/beam-free"), Some("max")),
            [
                "opencode",
                "run",
                "--format",
                "json",
                "--pure",
                "--print-logs",
                "--log-level",
                "ERROR",
                "--model",
                "opencode/beam-free",
                "--variant",
                "max",
                "go"
            ]
        );
    }

    #[test]
    fn claude_command_streams_json_and_takes_model_and_effort() {
        assert_eq!(
            argv(AgentKind::Claude, None, None),
            [
                "claude",
                "-p",
                "--output-format",
                "stream-json",
                "--verbose",
                "--permission-mode",
                "acceptEdits",
                "--allowedTools",
                "Bash(veecut),Bash(veecut *),mcp__veecut",
                "--setting-sources",
                "project,local",
                "--strict-mcp-config",
                "--mcp-config",
                VEECUT_MCP_JSON,
                "--",
                "go"
            ]
        );
        assert_eq!(
            argv(AgentKind::Claude, Some("sonnet"), Some("high")),
            [
                "claude",
                "-p",
                "--output-format",
                "stream-json",
                "--verbose",
                "--permission-mode",
                "acceptEdits",
                "--allowedTools",
                "Bash(veecut),Bash(veecut *),mcp__veecut",
                "--setting-sources",
                "project,local",
                "--strict-mcp-config",
                "--mcp-config",
                VEECUT_MCP_JSON,
                "--model",
                "sonnet",
                "--effort",
                "high",
                "--",
                "go"
            ]
        );
    }

    #[test]
    fn ultracode_is_max_effort_with_the_setting_and_the_workflow_rule() {
        assert_eq!(
            argv(AgentKind::Claude, Some("opus"), Some(ULTRACODE)),
            [
                "claude",
                "-p",
                "--output-format",
                "stream-json",
                "--verbose",
                "--permission-mode",
                "acceptEdits",
                "--allowedTools",
                "Bash(veecut),Bash(veecut *),mcp__veecut,Workflow",
                "--setting-sources",
                "project,local",
                "--strict-mcp-config",
                "--mcp-config",
                VEECUT_MCP_JSON,
                "--model",
                "opus",
                "--effort",
                "max",
                "--settings",
                r#"{"ultracode":true}"#,
                "--",
                "go"
            ]
        );
        // Only Claude knows the word.
        assert!(
            argv(AgentKind::Codex, None, Some(ULTRACODE))
                .contains(&"model_reasoning_effort=\"ultracode\"".to_string())
        );
    }

    #[test]
    fn codex_command_takes_model_and_reasoning_effort() {
        assert_eq!(
            argv(AgentKind::Codex, None, None),
            [
                "codex",
                "exec",
                "--json",
                "--sandbox",
                "workspace-write",
                "--skip-git-repo-check",
                "--ignore-user-config",
                "-c",
                "skills.include_instructions=false",
                "-c",
                CODEX_VEECUT_MCP,
                "--",
                "go"
            ]
        );
        assert_eq!(
            argv(AgentKind::Codex, Some("gpt-5.6-terra"), Some("high")),
            [
                "codex",
                "exec",
                "--json",
                "--sandbox",
                "workspace-write",
                "--skip-git-repo-check",
                "--ignore-user-config",
                "-c",
                "skills.include_instructions=false",
                "-c",
                CODEX_VEECUT_MCP,
                "-m",
                "gpt-5.6-terra",
                "-c",
                "model_reasoning_effort=\"high\"",
                "--",
                "go"
            ]
        );
    }

    #[test]
    fn gemini_command_takes_a_model_and_ignores_effort() {
        let base = [
            "gemini",
            "--skip-trust",
            "--approval-mode",
            "auto_edit",
            "--allowed-tools",
            "run_shell_command(veecut)",
            "-e",
            "none",
            "--allowed-mcp-server-names",
            "veecut",
        ];
        let with = |tail: &[&'static str]| [&base[..], tail].concat();
        let expected = with(&["-m", "gemini-2.5-pro", "-p", "go"]);
        assert_eq!(
            argv(AgentKind::Gemini, Some("gemini-2.5-pro"), None),
            expected
        );
        assert_eq!(
            argv(AgentKind::Gemini, Some("gemini-2.5-pro"), Some("high")),
            expected
        );
        assert_eq!(
            argv(AgentKind::Gemini, Some("  "), Some("")),
            with(&["-p", "go"])
        );
    }

    #[test]
    fn a_custom_command_is_used_as_written_whatever_the_selection() {
        let selection = Selection {
            model: Some("opus".into()),
            effort: Some(ULTRACODE.into()),
        };
        let with_marker = ["my-agent".to_string(), "--x".into(), PROMPT.into()];
        let (argv, format) = build_command(AgentKind::Claude, Some(&with_marker), &selection, "go");
        assert_eq!(argv, ["my-agent", "--x", "go"]);
        assert_eq!(format, OutputFormat::Plain);
        let without_marker = ["my-agent".to_string()];
        let (argv, format) =
            build_command(AgentKind::Codex, Some(&without_marker), &selection, "go");
        assert_eq!(argv, ["my-agent", "go"]);
        assert_eq!(format, OutputFormat::Plain);
        assert_eq!(
            build_command(AgentKind::Claude, None, &selection, "go").1,
            OutputFormat::Claude
        );
    }

    #[test]
    fn studio_prompt_sends_spoken_phrase_cuts_to_the_phrase_tools() {
        let prompt = studio_prompt(
            Path::new("/work/projects/talk"),
            "cut the part about pricing",
        );
        for needle in [
            "veecut cut-phrase \"exact words\"",
            "--occurrence N",
            "veecut transcript-search",
            "cut_phrase and transcript_search",
            "Use veecut ops for every other edit",
            "Do not run veecut render",
            "Project: /work/projects/talk\ncut the part about pricing",
        ] {
            assert!(prompt.contains(needle), "missing {needle:?} in {prompt}");
        }
        // The old blanket instruction sent phrase cuts through manual ops.
        assert!(!prompt.contains("Apply the note with veecut ops"));
    }

    #[test]
    fn default_commands_load_only_veecut_context_and_keep_the_login() {
        let flags = |kind: AgentKind| argv(kind, None, None).join(" ");
        assert!(flags(AgentKind::OpenCode).contains("--pure --print-logs --log-level ERROR"));
        assert!(
            flags(AgentKind::Claude)
                .contains("--setting-sources project,local --strict-mcp-config")
        );
        assert!(
            flags(AgentKind::Codex)
                .contains("--ignore-user-config -c skills.include_instructions=false")
        );
        assert!(flags(AgentKind::Gemini).contains("-e none --allowed-mcp-server-names veecut"));
        for kind in AgentKind::ALL {
            // --bare would refuse Claude's OAuth login; nothing may copy or replace credentials.
            let command = flags(kind);
            assert!(!command.contains("--bare"), "{command}");
            assert!(!command.contains("CODEX_HOME"), "{command}");
        }

        let root = Path::new("/work/space");
        let env = |kind: AgentKind| kind.lean_env(root);
        let opencode = env(AgentKind::OpenCode);
        assert!(opencode.contains(&("OPENCODE_DISABLE_CLAUDE_CODE", "1".to_string())));
        assert!(opencode.contains(&("OPENCODE_DISABLE_EXTERNAL_SKILLS", "1".to_string())));
        let (_, config) = opencode
            .iter()
            .find(|(key, _)| *key == "OPENCODE_CONFIG_CONTENT")
            .expect("inline config");
        let config: serde_json::Value = serde_json::from_str(config).unwrap();
        let skills = Path::new("/work/space").join(".agents/skills");
        assert_eq!(
            config,
            serde_json::json!({ "skills": { "paths": [skills.to_string_lossy()] } })
        );
        assert_eq!(
            env(AgentKind::Claude),
            [("CLAUDE_CODE_DISABLE_AUTO_MEMORY", "1".to_string())]
        );
        assert!(env(AgentKind::Codex).is_empty() && env(AgentKind::Gemini).is_empty());
    }

    #[test]
    fn lean_runs_register_only_the_veecut_mcp_server() {
        let after = |kind: AgentKind, flag: &str| -> String {
            let args = argv(kind, None, None);
            let at = args.iter().position(|arg| arg == flag).unwrap();
            args[at + 1].clone()
        };
        // Claude: strict config, exactly one server, and its tools allowed without a prompt.
        assert!(argv(AgentKind::Claude, None, None).contains(&"--strict-mcp-config".into()));
        let claude: serde_json::Value =
            serde_json::from_str(&after(AgentKind::Claude, "--mcp-config")).unwrap();
        assert_eq!(
            claude,
            serde_json::json!({
                "mcpServers": { "veecut": { "command": "veecut", "args": ["mcp"] } }
            })
        );
        assert!(
            after(AgentKind::Claude, "--allowedTools")
                .split(',')
                .any(|rule| rule == "mcp__veecut")
        );
        // Codex: its config.toml is ignored, so the one server comes from the override.
        let codex = argv(AgentKind::Codex, None, None);
        assert!(codex.contains(&"--ignore-user-config".into()));
        let overrides: Vec<&String> = codex
            .iter()
            .enumerate()
            .filter(|(i, _)| *i > 0 && codex[*i - 1] == "-c")
            .map(|(_, value)| value)
            .filter(|value| value.starts_with("mcp_servers."))
            .collect();
        assert_eq!(overrides.len(), 1);
        let table: toml::Table = toml::from_str(overrides[0]).unwrap();
        let servers = table["mcp_servers"].as_table().unwrap();
        assert_eq!(servers.keys().collect::<Vec<_>>(), ["veecut"]);
        assert_eq!(servers["veecut"]["command"].as_str(), Some("veecut"));
        assert_eq!(servers["veecut"]["args"][0].as_str(), Some("mcp"));
        assert_eq!(
            servers["veecut"]["default_tools_approval_mode"].as_str(),
            Some("approve")
        );
        // OpenCode: the workspace's opencode.json carries the server and the person's global
        // config folder is replaced by one inside the workspace.
        let env = AgentKind::OpenCode.lean_env(Path::new("/work/space"));
        let (_, config_home) = env
            .iter()
            .find(|(key, _)| *key == "XDG_CONFIG_HOME")
            .expect("config home");
        assert_eq!(
            Path::new(config_home),
            Path::new("/work/space").join(".veecut/agent-config")
        );
        assert!(
            env.iter()
                .all(|(key, _)| *key != "XDG_DATA_HOME" && *key != "HOME")
        );
    }

    #[test]
    fn first_request_tokens_come_from_the_first_request_only() {
        let first =
            |format, lines: &[&str]| last_usage_of(format, lines).first_request_input_tokens;
        let step = |input: u64, read: u64| {
            format!(
                r#"{{"type":"step_finish","part":{{"cost":0,"tokens":{{"input":{input},"output":3,"reasoning":0,"cache":{{"read":{read},"write":10}}}}}}}}"#
            )
        };
        let (one, two) = (step(100, 40), step(500, 400));
        assert_eq!(
            first(OutputFormat::OpenCode, &[one.as_str(), two.as_str()]),
            Some(100 + 40 + 10)
        );

        // Claude: the first main-thread message, cache read and written counted once; a later
        // message, a subagent message and the final result do not replace it.
        let main = |id: &str, input: u64, read: u64, parent: &str| {
            format!(
                r#"{{"type":"assistant","parent_tool_use_id":{parent},"message":{{"id":"{id}","content":[],"usage":{{"input_tokens":{input},"cache_creation_input_tokens":7,"cache_read_input_tokens":{read},"output_tokens":2}}}}}}"#
            )
        };
        let result = r#"{"type":"result","usage":{"input_tokens":900,"output_tokens":9},"total_cost_usd":0.1,"num_turns":2}"#;
        let (side, a, b) = (
            main("s", 5, 5, r#""toolu_1""#),
            main("a", 20, 1000, "null"),
            main("b", 30, 5000, "null"),
        );
        assert_eq!(
            first(
                OutputFormat::Claude,
                &[side.as_str(), a.as_str(), b.as_str(), result]
            ),
            Some(20 + 1000 + 7)
        );
        assert_eq!(first(OutputFormat::Claude, &[result]), None);
        assert_eq!(
            first(
                OutputFormat::Codex,
                &[
                    r#"{"type":"turn.completed","usage":{"input_tokens":900,"cached_input_tokens":100,"output_tokens":9}}"#
                ]
            ),
            None
        );
        assert_eq!(Usage::default().first_request_input_tokens, None);
        assert!(
            !serde_json::to_string(&Usage::default())
                .unwrap()
                .contains("first_request")
        );
    }

    /// The running usage after every line, decoded as stdout.
    fn last_usage_of(format: OutputFormat, lines: &[&str]) -> Usage {
        let mut decoder = Decoder::new(format);
        for line in lines {
            decoder.decode(Stream::Stdout, (*line).into(), &mut |_| {});
        }
        decoder.usage
    }

    #[test]
    fn codex_first_request_is_read_from_its_own_session_file_only() {
        use chrono::NaiveDate;
        let tmp = tempfile::tempdir().unwrap();
        let day = NaiveDate::from_ymd_opt(2026, 10, 29).unwrap();
        let other = NaiveDate::from_ymd_opt(2026, 10, 28).unwrap();
        let dir = tmp.path().join("2026/10/29");
        std::fs::create_dir_all(&dir).unwrap();
        let rollout = |input: u64, last: u64| {
            [
                r#"{"type":"session_meta","payload":{"id":"t1"}}"#.to_string(),
                // Rate-limit-only event before any request: no info yet.
                r#"{"type":"event_msg","payload":{"type":"token_count","info":null}}"#.into(),
                format!(
                    r#"{{"type":"event_msg","payload":{{"type":"token_count","info":{{"total_token_usage":{{"input_tokens":{input}}},"last_token_usage":{{"input_tokens":{input},"cached_input_tokens":6528}}}}}}}}"#
                ),
                format!(
                    r#"{{"type":"event_msg","payload":{{"type":"token_count","info":{{"total_token_usage":{{"input_tokens":{last}}},"last_token_usage":{{"input_tokens":{last}}}}}}}}}"#
                ),
            ]
            .join("\n")
        };
        std::fs::write(
            dir.join("rollout-2026-10-29T01-36-15-thread-a.jsonl"),
            rollout(37_301, 80_487),
        )
        .unwrap();
        std::fs::write(
            dir.join("rollout-2026-10-29T01-40-00-thread-b.jsonl"),
            rollout(11, 22),
        )
        .unwrap();
        let find = |thread, days| codex_first_request(tmp.path(), thread, days);
        assert_eq!(find("thread-a", [day, day]), Some(37_301));
        assert_eq!(find("thread-b", [other, day]), Some(11));
        assert_eq!(find("thread-c", [day, day]), None);
        assert_eq!(find("thread-a", [other, other]), None);

        let mut decoder = Decoder::new(OutputFormat::Codex);
        decoder.decode(
            Stream::Stdout,
            r#"{"type":"thread.started","thread_id":"thread-a"}"#.into(),
            &mut |_| {},
        );
        assert_eq!(decoder.codex_thread.as_deref(), Some("thread-a"));
    }

    #[test]
    fn model_and_effort_lists_follow_the_agent_and_model() {
        let ids = |kind, model| -> Vec<&'static str> {
            efforts(kind, model).iter().map(|e| e.id).collect()
        };
        for kind in AgentKind::ALL {
            assert!(!known_models(kind).is_empty());
        }
        assert_eq!(
            known_models(AgentKind::OpenCode)[0].id,
            OPENCODE_DEFAULT_MODEL
        );
        assert_eq!(ids(AgentKind::Gemini, None), Vec::<&str>::new());
        assert_eq!(
            ids(AgentKind::Claude, Some("opus")),
            ["low", "medium", "high", "xhigh", "max", ULTRACODE]
        );
        assert!(ids(AgentKind::Claude, Some("haiku")).contains(&ULTRACODE));
        let ultracode = efforts(AgentKind::Claude, None).pop().unwrap();
        assert!(ultracode.note.contains("Most expensive") && ultracode.note.contains("5x"));
        assert_eq!(
            ids(AgentKind::Codex, Some("gpt-5.6-sol")).last(),
            Some(&"max")
        );
        assert_eq!(
            ids(AgentKind::Codex, Some("gpt-6-astra")).last(),
            Some(&"ultra")
        );
        assert_eq!(ids(AgentKind::OpenCode, None), ["low", "high", "max"]);
        assert!(ids(AgentKind::OpenCode, Some("opencode/big-pickle")).is_empty());
    }

    #[test]
    fn auth_records_distinguish_signed_out_config_failure_and_unknown_output() {
        let check = |success, stdout: &str, stderr: &str| CheckOutput {
            success,
            stdout: stdout.into(),
            stderr: stderr.into(),
        };
        for (kind, output, readiness) in [
            (
                AgentKind::Claude,
                check(true, r#"{"loggedIn":true,"authMethod":"claude.ai"}"#, ""),
                Readiness::Ready,
            ),
            (
                AgentKind::Claude,
                check(false, r#"{"loggedIn":false,"authMethod":"none"}"#, ""),
                Readiness::SignedOut,
            ),
            (
                AgentKind::Claude,
                check(true, r#"{"loggedIn":"yes"}"#, ""),
                Readiness::CheckFailed,
            ),
            (
                AgentKind::Claude,
                check(false, "", "Error: invalid settings JSON"),
                Readiness::CheckFailed,
            ),
            (
                AgentKind::Codex,
                check(true, "", "Logged in using ChatGPT"),
                Readiness::Ready,
            ),
            (
                AgentKind::Codex,
                check(false, "", "Not logged in"),
                Readiness::SignedOut,
            ),
            (
                AgentKind::Codex,
                check(false, "", "Error loading configuration: invalid TOML"),
                Readiness::CheckFailed,
            ),
            (
                AgentKind::Codex,
                check(true, "codex-cli 0.50.0", ""),
                Readiness::CheckFailed,
            ),
        ] {
            let status = auth_status(kind, &output);
            assert_eq!(status.readiness, readiness);
            assert_eq!(status.can_run(), readiness == Readiness::Ready);
            if readiness == Readiness::SignedOut {
                assert_eq!(
                    status.fix.as_deref(),
                    Some(if kind == AgentKind::Claude {
                        "claude auth login"
                    } else {
                        "codex login"
                    })
                );
            }
        }
    }

    #[test]
    fn configured_command_is_unverified_without_checking_default_cli_auth() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let mut config = Config::default();
        config.agent.claude = Some(vec![file.path().to_string_lossy().into_owned()]);
        let status = detect_kind(AgentKind::Claude, Ok(&config));
        assert_eq!(status.readiness, Readiness::Unverified);
        assert!(status.command_override);
        assert!(status.can_run());
        assert_eq!(
            detect_kind(AgentKind::Claude, Err("invalid TOML")).readiness,
            Readiness::CheckFailed
        );
        config.agent.claude = Some(vec![
            file.path().join("missing").to_string_lossy().into_owned(),
        ]);
        let status = detect_kind(AgentKind::Claude, Ok(&config));
        assert_eq!(status.readiness, Readiness::NotInstalled);
        assert!(!status.can_run());
    }

    #[cfg(unix)]
    #[test]
    fn readiness_check_stops_a_hung_cli_and_its_inherited_pipes() {
        let tmp = tempfile::tempdir().unwrap();
        let started = Instant::now();
        let result = check_command(
            Path::new("/bin/sh"),
            &["-c".into(), "sleep 30 & wait".into()],
            tmp.path(),
            Duration::from_millis(100),
            64 * 1024,
        );
        assert!(result.unwrap_err().to_string().contains("timed out"));
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
