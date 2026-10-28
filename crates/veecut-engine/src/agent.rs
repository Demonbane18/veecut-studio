//! Runs a coding agent (OpenCode, Claude Code, Codex, Gemini CLI) headless inside a workspace.

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
                "--format",
                "json",
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
                "--json",
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
    match check_command(&program, &args, &root, Duration::from_secs(5)) {
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
    let result = launch(program, args, root, &stop, &mut |stream, line| {
        if output.stdout.len() + output.stderr.len() + line.len() + 1 > 64 * 1024 {
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
        anyhow::bail!("sign-in status exceeded 64 KiB");
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
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Output {
    Answer(String),
    Log(String),
}

impl Output {
    pub fn text(&self) -> &str {
        match self {
            Self::Answer(text) | Self::Log(text) => text,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stream {
    Stdout,
    Stderr,
}

#[derive(Clone, Copy)]
enum OutputFormat {
    Plain,
    Codex,
    OpenCode,
}

impl OutputFormat {
    fn decode(self, stream: Stream, line: String) -> Output {
        if stream == Stream::Stderr {
            return Output::Log(line);
        }
        if matches!(self, Self::Plain) {
            return Output::Answer(line);
        }
        let Ok(mut json) = serde_json::from_str::<serde_json::Value>(&line) else {
            return Output::Log(line);
        };
        let path = match self {
            Self::Codex
                if json["type"] == "item.completed" && json["item"]["type"] == "agent_message" =>
            {
                "/item/text"
            }
            Self::OpenCode if json["type"] == "text" => "/part/text",
            _ => return Output::Log(line),
        };
        match json.pointer_mut(path).map(serde_json::Value::take) {
            Some(serde_json::Value::String(text)) => Output::Answer(text),
            _ => Output::Log(line),
        }
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

/// Runs the agent with `prompt` in the workspace root. Assistant replies and raw logs stay
/// separate; custom commands keep plain stdout as the reply without assuming a JSON format.
/// Blocks until exit or Stop; call from a worker thread in a UI.
pub fn run(
    ws: &Workspace,
    kind: AgentKind,
    prompt: &str,
    stop: &Stop,
    on_output: &mut dyn FnMut(Output),
) -> Result<Outcome> {
    let config = Config::load()?;
    let command = config.agent.command(kind).filter(|cmd| !cmd.is_empty());
    let format = if command.is_some() {
        OutputFormat::Plain
    } else {
        match kind {
            AgentKind::Codex => OutputFormat::Codex,
            AgentKind::OpenCode => OutputFormat::OpenCode,
            _ => OutputFormat::Plain,
        }
    };
    let mut argv: Vec<String> = match command {
        Some(cmd) => cmd.to_vec(),
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
    launch(&program, &argv[1..], &ws.root, stop, &mut |stream, line| {
        on_output(format.decode(stream, line));
    })
}

/// What the threads around one agent tell [`launch`].
enum Msg {
    Line(Stream, String),
    /// The agent was reaped; true when [`Stop::stop`] was called while it ran.
    Exit(io::Result<ExitStatus>, bool),
}

/// How long output that is still in the pipes may take to arrive after the agent is gone.
const DRAIN: Duration = Duration::from_secs(2);

fn launch(
    program: &Path,
    args: &[String],
    root: &Path,
    stop: &Stop,
    on_line: &mut dyn FnMut(Stream, String),
) -> Result<Outcome> {
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(root)
        // OpenCode takes its project folder from PWD, which the child would otherwise inherit
        // from wherever the editor or CLI was started.
        .env("PWD", root)
        .env("PATH", search_path())
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
            let result = launch(Path::new("/bin/sh"), &args, &root, &stop, &mut |_, line| {
                on_line(&line)
            });
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
mod output_tests {
    use super::*;

    #[test]
    fn codex_tool_records_and_noise_are_not_assistant_answers() {
        let format = OutputFormat::Codex;
        let records = [
            r#"{"type":"thread.started","thread_id":"thread-1"}"#,
            r#"{"type":"item.completed","item":{"id":"item_1","type":"command_execution","command":"veecut status --json","aggregated_output":"{\"duration\":124.5}","exit_code":0,"status":"completed"}}"#,
            "hook output before final answer",
            r#"{"type":"item.completed","item":{"id":"item_2","type":"agent_message","text":"Captions are bigger. Review them in Studio."}}"#,
            r#"{"type":"turn.completed","usage":{"input_tokens":1200,"cached_input_tokens":1000,"output_tokens":20}}"#,
        ];
        let output: Vec<_> = records
            .iter()
            .map(|line| format.decode(Stream::Stdout, (*line).into()))
            .collect();
        assert_eq!(
            output[3],
            Output::Answer("Captions are bigger. Review them in Studio.".into())
        );
        assert!(output[..3].iter().all(|out| matches!(out, Output::Log(_))));
        assert!(matches!(&output[4], Output::Log(_)));
        assert!(matches!(
            format.decode(Stream::Stderr, records[3].into()),
            Output::Log(_)
        ));
    }

    #[test]
    fn opencode_text_parts_survive_tool_output_and_malformed_records() {
        let format = OutputFormat::OpenCode;
        let records = [
            r#"{"type":"tool_use","timestamp":1793200000000,"sessionID":"ses_1","part":{"type":"tool","tool":"veecut_ops","state":{"status":"completed","output":"{\"ok\":true}"}}}"#,
            "{broken json",
            r#"{"type":"text","timestamp":1793200000001,"sessionID":"ses_1","part":{"id":"prt_1","type":"text","text":"Applied the note."}}"#,
            r#"{"type":"step_finish","part":{"type":"step-finish","reason":"stop","cost":0}}"#,
        ];
        assert!(matches!(
            format.decode(Stream::Stdout, records[0].into()),
            Output::Log(_)
        ));
        assert!(matches!(
            format.decode(Stream::Stdout, records[1].into()),
            Output::Log(_)
        ));
        assert_eq!(
            format.decode(Stream::Stdout, records[2].into()),
            Output::Answer("Applied the note.".into())
        );
        assert!(matches!(
            format.decode(Stream::Stdout, records[3].into()),
            Output::Log(_)
        ));
    }

    #[test]
    fn plain_stdout_is_reply_without_json_guessing_and_stderr_is_log() {
        let format = OutputFormat::Plain;
        for line in [
            "Applied the note.",
            r#"{"type":"text","part":{"text":"literal JSON"}}"#,
        ] {
            assert_eq!(
                format.decode(Stream::Stdout, line.into()),
                Output::Answer(line.into())
            );
            assert_eq!(
                format.decode(Stream::Stderr, line.into()),
                Output::Log(line.into())
            );
        }
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
        );
        assert!(result.unwrap_err().to_string().contains("timed out"));
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
