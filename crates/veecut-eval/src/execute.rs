use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, ensure};
use serde_json::json;
use veecut_engine::{
    agent::{self, AgentKind, Outcome, Output, Selection, Stop, Usage},
    analyze, media,
    project::Canvas,
    recipes::{self, EditRequest},
    transcribe::{Provider, TranscribeOptions},
    workspace::Workspace,
};

use crate::{
    checks::{self, CheckResult},
    data::{self, Baseline, Fixture, FixtureFile, Row},
    ladder,
    tasks::{self, Task},
};

/// Held across the complete batch: a second process must not start another model concurrently.
pub struct BatchLock(PathBuf);
impl BatchLock {
    pub fn acquire(root: &Path) -> Result<Self> {
        let dir = root.join("work");
        fs::create_dir_all(&dir)?;
        let path = dir.join("harness.lock");
        let mut file=fs::OpenOptions::new().write(true).create_new(true).open(&path).with_context(||format!("Another harness owns {}. If its process died, inspect the PID and remove only this stale lock before resuming.",path.display()))?;
        writeln!(file, "{}", std::process::id())?;
        Ok(Self(path))
    }
}
impl Drop for BatchLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

pub fn baseline_dir(root: &Path, fixture: &Fixture) -> PathBuf {
    root.join("work/baselines").join(&fixture.id)
}

pub fn complete_baseline(root: &Path, fixture: &Fixture) -> Option<Baseline> {
    let dir = baseline_dir(root, fixture);
    let baseline = data::read_json::<Baseline>(&dir.join("baseline.json")).ok()?;
    (baseline.status == "complete"
        && !baseline.projects.is_empty()
        && baseline.projects.iter().all(|p| dir.join(p).is_file()))
    .then_some(baseline)
}

pub fn baselines(root: &Path, manifest_dir: &Path, fixtures: &[&Fixture]) -> Result<()> {
    for fixture in fixtures {
        if complete_baseline(root, fixture).is_some() {
            eprintln!("{}: complete baseline, skipped", fixture.id);
            continue;
        }
        let dir = baseline_dir(root, fixture);
        if dir.exists() {
            let archived = dir.with_file_name(format!("{}-failed-{}", fixture.id, unique_id()));
            fs::rename(&dir, archived).context("preserving incomplete baseline before retry")?;
        }
        fs::create_dir_all(&dir)?;
        let start = Instant::now();
        let mut progress = Vec::new();
        data::write_json(
            &dir.join("baseline.json"),
            &Baseline {
                fixture: fixture.id.clone(),
                status: "running".into(),
                projects: vec![],
                seconds: 0.0,
                report: json!({}),
                error: None,
                failure_class: None,
                beats: vec![],
            },
        )?;
        let result = (|| -> Result<Baseline> {
            let ws = Workspace::init(&dir)?;
            for file in &fixture.files {
                link_input(manifest_dir, file, &ws.inbox())?;
            }
            let mut req = EditRequest::new(fixture.video_kind());
            req.render = false;
            req.name = Some(fixture.id.clone());
            req.language = fixture.language.clone();
            let (w, h) = tasks::canvas(fixture.video_kind());
            req.canvas = Some(Canvas::new(w, h));
            req.transcribe = Some(TranscribeOptions {
                provider: Some(Provider::Local),
                cpu_only: true,
                ..Default::default()
            });
            for file in &fixture.files {
                let path = ws.inbox().join(tasks::filename(&file.path));
                match file.role.as_str() {
                    "source" | "cam-a" | "cam-b" => req.sources.push(path),
                    "broll" | "visual" => req.broll.push(path),
                    "music" => req.music = Some(path),
                    "narration" | "voice" => req.voice = Some(path),
                    "copy" | "lyrics" => req.copy = Some(fs::read_to_string(path)?),
                    _ => {}
                }
            }
            let report = recipes::edit(&ws, &req, &mut |message| {
                eprintln!("{}: {message}", fixture.id);
                progress.push(message.to_string());
            })?;
            let paths = report
                .projects
                .iter()
                .map(|p| {
                    p.join("project.json")
                        .strip_prefix(&dir)
                        .map(Path::to_path_buf)
                })
                .collect::<std::result::Result<Vec<_>, _>>()?;
            ensure!(!paths.is_empty(), "Recipe returned no projects");
            let beats = if let Some(music) = req.music {
                analyze::beats(&media::pcm_mono(&music, 16000, None)?, 16000).beats
            } else {
                vec![]
            };
            let baseline = Baseline {
                fixture: fixture.id.clone(),
                status: "complete".into(),
                projects: paths,
                seconds: start.elapsed().as_secs_f64(),
                report: serde_json::to_value(report)?,
                error: None,
                failure_class: None,
                beats,
            };
            data::write_json(&dir.join("report.json"), &baseline.report)?;
            Ok(baseline)
        })();
        let baseline = match result {
            Ok(b) => b,
            Err(e) => {
                let detail = format!("{e:#}");
                let class = if classify(&detail, false) == "environment" {
                    "environment"
                } else {
                    "app"
                };
                eprintln!("{}: FAILED: {detail}", fixture.id);
                Baseline {
                    fixture: fixture.id.clone(),
                    status: "failed".into(),
                    projects: vec![],
                    seconds: start.elapsed().as_secs_f64(),
                    report: json!({"progress":progress}),
                    error: Some(detail.clone()),
                    failure_class: Some(class.into()),
                    beats: vec![],
                }
            }
        };
        fs::write(dir.join("baseline.log"), progress.join("\n"))?;
        data::write_json(&dir.join("report.json"), &baseline.report)?;
        data::write_json(&dir.join("baseline.json"), &baseline)?;
    }
    Ok(())
}

fn link_input(manifest_dir: &Path, file: &FixtureFile, inbox: &Path) -> Result<()> {
    let source = manifest_dir
        .join(&file.path)
        .canonicalize()
        .with_context(|| format!("missing fixture file {}", file.path.display()))?;
    let dest = inbox.join(tasks::filename(&file.path));
    #[cfg(unix)]
    std::os::unix::fs::symlink(source, dest)?;
    #[cfg(windows)]
    std::os::windows::fs::symlink_file(source, dest)?;
    Ok(())
}

pub fn unique_id() -> String {
    format!(
        "{}-{}",
        chrono::Utc::now().format("%Y%m%dT%H%M%S%.9f"),
        std::process::id()
    )
}

pub fn timeout_seconds(level: u8, length: &str, explicit: Option<u64>) -> u64 {
    explicit.unwrap_or(if level == 1 && matches!(length, "L" | "XL") {
        1200
    } else {
        600
    })
}

fn watchdog(
    stop: Stop,
    timeout: Duration,
) -> (mpsc::Sender<()>, thread::JoinHandle<()>, Arc<AtomicBool>) {
    let (send, recv) = mpsc::channel();
    let timed_out = Arc::new(AtomicBool::new(false));
    let flag = timed_out.clone();
    let worker = thread::spawn(move || {
        if recv.recv_timeout(timeout) == Err(mpsc::RecvTimeoutError::Timeout) {
            flag.store(true, Ordering::SeqCst);
            stop.stop();
        }
    });
    (send, worker, timed_out)
}

pub fn claim(answer: &str) -> (bool, String) {
    let sentences = answer
        .lines()
        .flat_map(|line| line.split(['.', '!']))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>();
    for sentence in &sentences {
        let lower = sentence.to_lowercase();
        if [
            "could not",
            "couldn't",
            "unable to",
            "not completed",
            "not done",
            "not changed",
            "not updated",
            "not saved",
            "failed to",
            "cannot ",
            "can't ",
        ]
        .iter()
        .any(|s| lower.contains(s))
        {
            return (false, (*sentence).into());
        }
    }
    // Decimal points split numeric ranges, so this completed-cut form uses the unsplit answer.
    // Its bare imperative/future forms are deliberately not accepted.
    if completed_numeric_cut(answer.trim()) {
        return (true, answer.trim().into());
    }
    for sentence in &sentences {
        let lower = sentence.to_lowercase();
        if lower.contains("will be") || lower.contains("would be") {
            continue;
        }
        if [
            "done",
            "completed",
            "updated",
            "changed",
            "added",
            "removed",
            "saved",
            "finished",
            "fixed",
            "created",
        ]
        .iter()
        .any(|s| {
            lower
                .split(|c: char| !c.is_alphanumeric())
                .any(|word| word == *s)
        }) {
            return (true, (*sentence).into());
        }
    }
    (false, "No explicit completion claim in final answer".into())
}

fn completed_numeric_cut(answer: &str) -> bool {
    if answer.contains(['\n', '\r']) {
        return false;
    }
    let Some(rest) = answer.strip_prefix("Cut ") else {
        return false;
    };
    let Some((range, detail)) = rest.split_once(' ') else {
        return false;
    };
    if !detail.starts_with("(exactly ")
        || !(detail.ends_with(") from every track.") || detail.ends_with(") from all tracks."))
    {
        return false;
    }
    let Some((from, to)) = range.split_once(['–', '—', '-']) else {
        return false;
    };
    let seconds = |value: &str| value.strip_suffix('s')?.parse::<f64>().ok();
    matches!((seconds(from), seconds(to)), (Some(from), Some(to)) if from.is_finite() && to.is_finite() && from >= 0.0 && to > from)
}

pub fn classify(detail: &str, timed_out: bool) -> &'static str {
    let text = detail.to_lowercase();
    if text.contains("opencode's free allowance is used up")
        || (text.contains("opencode says the model ") && text.contains(" is unavailable"))
        || [
            "unauthorized",
            "not logged in",
            "sign in",
            "authentication",
            "quota",
            "rate limit",
            "429",
            "free usage limit reached",
            "freeusagelimiterror",
            "model is unavailable",
            "network",
            "connection refused",
            "timed out connecting",
            "not installed",
            "no such file",
            "not found in path",
            "disk full",
            "no space left",
        ]
        .iter()
        .any(|x| text.contains(x))
    {
        "environment"
    } else if [
        "unknown command",
        "unrecognized subcommand",
        "invalid operation",
        "unknown field",
        "schema",
        "panic",
        "invalid veecut",
        "built without",
        "permission denied",
        "output too large",
    ]
    .iter()
    .any(|x| text.contains(x))
    {
        "app"
    } else if timed_out {
        "model"
    } else {
        "model"
    }
}

pub struct RunSpec<'a> {
    pub root: &'a Path,
    pub manifest_dir: &'a Path,
    pub fixture: &'a Fixture,
    pub task: &'a Task,
    pub agent: AgentKind,
    pub selection: &'a Selection,
    pub trial: u32,
    pub timeout: Option<u64>,
    pub screening: bool,
    pub generation: u32,
}

pub fn run(spec: RunSpec<'_>) -> Result<Row> {
    let RunSpec {
        root,
        manifest_dir,
        fixture,
        task,
        agent,
        selection,
        trial,
        timeout,
        screening,
        generation,
    } = spec;
    let workspace =
        root.join("work/runs")
            .join(format!("{}-{}-{}", fixture.id, task.id, unique_id()));
    let artifacts = root
        .join("results/artifacts")
        .join(workspace.file_name().context("Run workspace has no name")?);
    fs::create_dir_all(&artifacts)?;
    let mut row = Row {
        schema_version: 2,
        timestamp: chrono::Utc::now().to_rfc3339(),
        fixture: fixture.id.clone(),
        kind: fixture.kind.clone(),
        length: fixture.length.clone(),
        task: task.id.clone(),
        level: task.level,
        agent: agent.name().into(),
        model: selection
            .model
            .clone()
            .unwrap_or_else(|| "cli-default".into()),
        effort: selection.effort.clone(),
        trial,
        pass: false,
        check_detail: String::new(),
        evidence: json!({}),
        seconds: 0.0,
        tokens_in: None,
        tokens_out: None,
        tokens_cached: None,
        cost_usd: None,
        turns: None,
        first_request_input_tokens: None,
        claimed_success: false,
        claim_evidence: String::new(),
        failure_class: None,
        timed_out: false,
        exit_code: None,
        workspace: workspace.clone(),
        project_paths: vec![],
        prompt_path: artifacts.join("prompt.txt"),
        answer_path: artifacts.join("answer.txt"),
        log_path: artifacts.join("agent.log"),
        task_path: artifacts.join("task.json"),
        provenance: task.provenance.clone(),
        screening,
        generation,
        invalid: false,
        rechecked: false,
        original_run: None,
        checker_fingerprint: None,
        frozen_inputs: None,
    };
    let mut answer = String::new();
    let mut logs = String::new();
    let mut usage = None::<Usage>;
    let result = (|| -> Result<CheckResult> {
        let config = veecut_engine::config::Config::load()?;
        if config.agent.command(agent).is_some_and(|c| !c.is_empty()) {
            return Ok(CheckResult{pass:false,detail:"Configured custom agent command refused: selected model and price cannot be verified. Remove the override explicitly before evaluating this provider.".into(),failure_class:Some("environment".into()),evidence:json!({"command_override":true,"spawned":false})});
        }
        let baseline = baseline_dir(root, fixture);
        if task.level == 1 {
            Workspace::init(&workspace)?;
            for file in &fixture.files {
                copy_input(manifest_dir, file, &workspace.join("inbox"))?;
            }
        } else {
            ensure!(
                complete_baseline(root, fixture).is_some(),
                "Missing complete baseline: {}",
                baseline.display()
            );
            data::copy_tree(&baseline, &workspace)?;
            let mut mapping = vec![];
            for file in &fixture.files {
                let source = manifest_dir.join(&file.path).canonicalize()?;
                let dest = workspace.join("inbox").join(tasks::filename(&file.path));
                mapping.push((source, dest));
            }
            data::rebase(&workspace, &baseline, &mapping)?;
        }
        for file in &task.inputs {
            if let Some(existing) = fixture
                .files
                .iter()
                .find(|f| tasks::filename(&f.path) == tasks::filename(&file.path))
            {
                ensure!(
                    existing.path == file.path,
                    "Auxiliary input basename collision: {}",
                    file.path.display()
                );
            } else {
                copy_input(manifest_dir, file, &workspace.join("inbox"))?;
            }
        }
        data::write_json(&row.task_path, task)?;
        let (paths, mut before, transcripts) = data::projects(&workspace)?;
        let prompt = if task.level == 1 {
            task.prompt.clone()
        } else {
            agent::studio_prompt(paths.first().context("No baseline project")?, &task.prompt)
        };
        fs::write(&row.prompt_path, &prompt)?;
        fs::write(&row.answer_path, "")?;
        fs::write(&row.log_path, "")?;
        if let Some(reason) = &task.prerequisite {
            return Ok(CheckResult {
                pass: false,
                detail: reason.clone(),
                failure_class: Some("check".into()),
                evidence: json!({"prerequisite":reason,"provenance":task.provenance}),
            });
        }
        if let Err(reason) = checks::prepare(task, &mut before, &transcripts) {
            return Ok(CheckResult {
                pass: false,
                detail: format!("Task preparation failed: {reason}"),
                failure_class: Some("check".into()),
                evidence: json!({"preparation_error":reason,"spawned":false}),
            });
        }
        for (path, project) in paths.iter().zip(&before) {
            project.save(path)?;
        }
        // Frozen snapshots remain outside the agent-readable workspace.
        let inbox_before = data::inbox_snapshot(&workspace.join("inbox"))?;
        row.frozen_inputs = Some(crate::recheck::freeze(
            &row,
            task,
            &before,
            &transcripts,
            &inbox_before,
        )?);
        let ws = Workspace::open(&workspace)?;
        let mut log_file = fs::OpenOptions::new().append(true).open(&row.log_path)?;
        let stop = Stop::default();
        let (cancel, watch, deadline) = watchdog(
            stop.clone(),
            Duration::from_secs(timeout_seconds(task.level, &fixture.length, timeout)),
        );
        let started = Instant::now();
        let outcome = agent::run(
            &ws,
            agent,
            &prompt,
            selection,
            &stop,
            &mut |event| match event {
                Output::Answer(text) => {
                    let _ = writeln!(log_file, "[assistant] {text}");
                    if agent == AgentKind::Gemini {
                        answer.push_str(&text);
                        answer.push('\n');
                    } else {
                        answer = text;
                    }
                }
                Output::Log(text) => {
                    logs.push_str(&text);
                    logs.push('\n');
                    let _ = writeln!(log_file, "{text}");
                    if logs.len() > 131072 {
                        let mut cut = logs.len() - 65536;
                        while !logs.is_char_boundary(cut) {
                            cut += 1;
                        }
                        logs.drain(..cut);
                    }
                }
                Output::Usage(snapshot) => usage = Some(snapshot),
            },
        );
        let _ = cancel.send(());
        let _ = watch.join();
        row.timed_out = deadline.load(Ordering::SeqCst);
        row.seconds = started.elapsed().as_secs_f64();
        if let Ok(Outcome::Finished(status)) = &outcome {
            row.exit_code = status.code();
        }
        fs::write(&row.answer_path, &answer)?;
        let (paths, after, _) = data::projects(&workspace)?;
        row.project_paths = paths;
        let inbox_unchanged =
            data::inbox_snapshot(&workspace.join("inbox")).is_ok_and(|after| after == inbox_before);
        let checked = checks::check(task, &before, &after, &transcripts, inbox_unchanged);
        match outcome {
            Ok(Outcome::Finished(status)) => {
                row.exit_code = status.code();
                if !status.success() {
                    return Ok(CheckResult {
                        pass: false,
                        detail: format!("Agent exited {status}; {}", checked.detail),
                        failure_class: Some(classify(&logs, false).into()),
                        evidence: json!({"check":checked.evidence,"agent_log":row.log_path}),
                    });
                }
            }
            Ok(Outcome::Stopped) => {
                return Ok(CheckResult {
                    pass: false,
                    detail: format!(
                        "{}; {}",
                        if row.timed_out {
                            "Run deadline reached; process group stopped"
                        } else {
                            "Agent stopped"
                        },
                        checked.detail
                    ),
                    failure_class: Some(classify(&logs, row.timed_out).into()),
                    evidence: json!({"check":checked.evidence,"agent_log":row.log_path}),
                });
            }
            Err(error) => {
                return Ok(CheckResult {
                    pass: false,
                    detail: format!("Agent error: {error:#}; {}", checked.detail),
                    failure_class: Some(classify(&format!("{error:#}\n{logs}"), false).into()),
                    evidence: json!({"check":checked.evidence,"agent_log":row.log_path}),
                });
            }
        }
        Ok(checked)
    })();
    let usage_evidence = usage.as_ref().and_then(|u| serde_json::to_value(u).ok());
    if let Some(usage) = &usage {
        row.first_request_input_tokens = usage.first_request_input_tokens;
        row.tokens_in = Some(usage.input_tokens);
        row.tokens_out = Some(usage.output_tokens);
        row.tokens_cached = Some(usage.cached_tokens);
        row.cost_usd = usage.cost_usd;
        row.turns = usage.turns;
    }
    (row.claimed_success, row.claim_evidence) = claim(&answer);
    let checked = match result {
        Ok(c) => c,
        Err(error) => {
            let detail = format!("{error:#}");
            CheckResult {
                pass: false,
                detail: detail.clone(),
                evidence: json!({"error":detail}),
                failure_class: Some(
                    if detail.contains("Missing complete baseline") {
                        "app"
                    } else {
                        classify(&detail, false)
                    }
                    .into(),
                ),
            }
        }
    };
    row.pass = checked.pass;
    row.check_detail = checked.detail;
    row.evidence = checked.evidence;
    // Keep the CLI's complete usage report next to the verdict, whatever fields it carries.
    if let (Some(usage), Some(evidence)) = (usage_evidence, row.evidence.as_object_mut()) {
        evidence.insert("usage".into(), usage);
    }
    row.failure_class = checked
        .failure_class
        .or_else(|| (!row.pass).then(|| "model".into()));
    fs::create_dir_all(&workspace)?;
    if !row.task_path.exists() {
        data::write_json(&row.task_path, task)?;
    }
    if !row.prompt_path.exists() {
        fs::write(&row.prompt_path, &task.prompt)?;
    }
    if !row.answer_path.exists() {
        fs::write(&row.answer_path, &answer)?;
    }
    if !row.log_path.exists() {
        fs::write(&row.log_path, &logs)?;
    }
    data::write_json(&artifacts.join("result.json"), &row)?;
    data::append_row(&root.join("results"), &row)?;
    eprintln!(
        "{} / {} / {} trial {}: {} ({:.1}s) {}",
        fixture.id,
        task.id,
        row.model,
        trial,
        if row.pass { "PASS" } else { "FAIL" },
        row.seconds,
        row.check_detail
    );
    Ok(row)
}

const PROBE_PROMPT: &str = "Reply with the single word OK. Do not use tools or edit files.";
const PROBE_LOG_BYTES: usize = 8192;

/// A free model that cannot be used right now, with what this probe's own process showed.
pub struct Probe {
    pub reason: String,
    pub seconds: f64,
    pub workspace: PathBuf,
    pub log: String,
}

fn keep_tail(text: &mut String, max: usize) {
    if text.len() > max {
        let mut cut = text.len() - max;
        while !text.is_char_boundary(cut) {
            cut += 1;
        }
        text.drain(..cut);
    }
}

/// Why this probe's own output says the model is unusable now. `None` when it answered, or when
/// the failure says nothing about availability: that is left to a real, recorded trial.
/// `deadline` is the probe limit in seconds when it was reached.
pub fn probe_verdict(
    failure: Option<&str>,
    deadline: Option<u64>,
    answer: &str,
    logs: &str,
) -> Option<String> {
    if !answer.trim().is_empty() {
        return None;
    }
    let short = |text: &str| text.trim().chars().take(300).collect::<String>();
    if let Some(error) = failure {
        return (classify(&format!("{error}\n{logs}"), false) == "environment")
            .then(|| short(error.lines().next().unwrap_or(error)));
    }
    if let Some(seconds) = deadline {
        return Some(format!(
            "No reply within the {seconds}-second probe deadline"
        ));
    }
    logs.lines()
        .map(str::trim)
        .find(|line| classify(line, false) == "environment")
        .map(short)
}

/// One tiny real call through `agent::run`, bounded by a watchdog. Only this call's own
/// callbacks are read; OpenCode's shared log file never is.
fn probe(root: &Path, model: &str, seconds: u64) -> Result<Option<Probe>> {
    let config = veecut_engine::config::Config::load()?;
    if config
        .agent
        .command(AgentKind::OpenCode)
        .is_some_and(|c| !c.is_empty())
    {
        return Ok(None); // run() refuses overrides with its own recorded failure
    }
    let workspace =
        root.join("work/probes")
            .join(format!("{}-{}", model.replace('/', "_"), unique_id()));
    Workspace::init(&workspace)?;
    let ws = Workspace::open(&workspace)?;
    let stop = Stop::default();
    let (cancel, watch, deadline) = watchdog(stop.clone(), Duration::from_secs(seconds));
    let started = Instant::now();
    let (mut answer, mut logs) = (String::new(), String::new());
    let outcome = agent::run(
        &ws,
        AgentKind::OpenCode,
        PROBE_PROMPT,
        &ladder::selection(model, None),
        &stop,
        &mut |event| match event {
            Output::Answer(text) => answer.push_str(&text),
            Output::Log(text) => {
                logs.push_str(&text);
                logs.push('\n');
                keep_tail(&mut logs, 65536);
            }
            Output::Usage(_) => {}
        },
    );
    let _ = cancel.send(());
    let _ = watch.join();
    let failure = outcome.as_ref().err().map(|e| format!("{e:#}"));
    let deadline = deadline.load(Ordering::SeqCst).then_some(seconds);
    Ok(
        probe_verdict(failure.as_deref(), deadline, &answer, &logs).map(|reason| {
            let mut log = failure.map_or(logs.clone(), |e| format!("{logs}\n{e}"));
            keep_tail(&mut log, PROBE_LOG_BYTES);
            Probe {
                reason,
                seconds: started.elapsed().as_secs_f64(),
                workspace,
                log,
            }
        }),
    )
}

/// At most one bounded probe per free model per batch, made only just before a free rung would
/// spend. Paid and unlisted models are refused: a probe is never a paid request. OpenCode's
/// free allowance is shared by every free model, so one exhaustion answer covers the batch;
/// "model unavailable" stays scoped to its model.
pub struct Probes {
    seconds: u64,
    exhausted: Option<Probe>,
    done: BTreeMap<String, Option<Probe>>,
}
impl Probes {
    pub fn new(seconds: u64) -> Self {
        Self {
            seconds,
            exhausted: None,
            done: BTreeMap::new(),
        }
    }
    /// `Some` when the free model cannot be used right now.
    pub fn unavailable(
        &mut self,
        root: &Path,
        agent: AgentKind,
        selection: &Selection,
    ) -> Result<Option<&Probe>> {
        let model = selection
            .model
            .as_deref()
            .filter(|m| ladder::free(agent, m))
            .context("Only listed free models may be probed; a probe never spends on a paid or unknown model")?;
        if self.exhausted.is_none() && !self.done.contains_key(model) {
            match probe(root, model, self.seconds)? {
                Some(found) if allowance_exhausted(&found.reason) => self.exhausted = Some(found),
                result => {
                    self.done.insert(model.to_owned(), result);
                }
            }
        }
        Ok(self
            .exhausted
            .as_ref()
            .or(self.done.get(model).and_then(Option::as_ref)))
    }
}

/// OpenCode's exact messages for a used-up free allowance, which belongs to the account, not a model.
pub fn allowance_exhausted(text: &str) -> bool {
    let text = text.to_lowercase();
    [
        "free allowance is used up",
        "free usage limit reached",
        "freeusagelimiterror",
    ]
    .iter()
    .any(|m| text.contains(m))
}

fn copy_input(manifest_dir: &Path, file: &FixtureFile, inbox: &Path) -> Result<()> {
    let source = manifest_dir.join(&file.path).canonicalize()?;
    let dest = inbox.join(tasks::filename(&file.path));
    ensure!(
        !dest.exists(),
        "Inbox basename collision: {}",
        dest.display()
    );
    fs::copy(source, dest)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn timeout_policy_and_watchdog() {
        assert_eq!(timeout_seconds(1, "XL", None), 1200);
        assert_eq!(timeout_seconds(2, "XL", None), 600);
        assert_eq!(timeout_seconds(1, "S", Some(9)), 9);
        let stop = Stop::default();
        let (cancel, worker, flag) = watchdog(stop, Duration::ZERO);
        worker.join().unwrap();
        assert!(flag.load(Ordering::SeqCst));
        drop(cancel);
        let (cancel, worker, flag) = watchdog(Stop::default(), Duration::from_secs(60));
        cancel.send(()).unwrap();
        worker.join().unwrap();
        assert!(!flag.load(Ordering::SeqCst));
    }
    #[test]
    fn claims_do_not_imply_artifact_success() {
        assert!(claim("Done. Updated the caption size.").0);
        assert!(!claim("I could not save the project. Nothing was updated.").0);
        assert!(!claim("I would change the captions.").0);
        assert!(!claim("The project was not changed.").0);
        assert!(!claim("The captions will be updated.").0);
        assert!(!claim("").0);
    }
    #[test]
    fn completed_numeric_cut_claim_preserves_the_full_range() {
        let answer = "Cut 14.2s–20.2s (exactly the \"from Utah to Kennedy Space Center, Florida.\" sentence, no extra seconds) from every track.";
        assert_eq!(claim(answer), (true, answer.into()));
        assert!(claim("Cut 2s-4s (exactly the requested sentence) from all tracks.").0);
        for future in [
            "I will cut 14.2s–20.2s from every track.",
            "I would cut 14.2s–20.2s from every track.",
            "Please cut 14.2s–20.2s (exactly the requested sentence) from every track.",
            "Cut 14.2s–20.2s from every track.",
            "Next, cut 14.2s–20.2s (exactly the requested sentence) from every track.",
            "Plan:\nCut 14.2s–20.2s (exactly the requested sentence) from every track.",
            "Cut 20.2s–14.2s (exactly the requested sentence) from every track.",
        ] {
            assert!(!claim(future).0, "{future}");
        }
    }
    #[test]
    fn opencode_exhaustion_and_unavailable_errors_are_environment_failures() {
        for message in [
            "Free usage limit reached",
            "FreeUsageLimitError",
            "Model is unavailable",
            "OpenCode's free allowance is used up. It can take a day or more to come back; pick another agent or model.",
            "OpenCode says the model opencode/big-pickle is unavailable. Pick another model or agent.",
        ] {
            assert_eq!(classify(message, false), "environment");
        }
        assert_eq!(
            classify("The requested caption size was not changed", false),
            "model"
        );
    }
}

#[cfg(test)]
mod probe_tests {
    use super::*;
    #[test]
    fn unavailable_only_on_this_probes_own_evidence() {
        // The product path raises OpenCode's allowance error; that defers the rung.
        let reason = probe_verdict(
            Some(
                "OpenCode's free allowance is used up. It can take a day or more to come back; pick another agent or model.",
            ),
            None,
            "",
            "",
        );
        assert!(reason.unwrap().contains("free allowance is used up"));
        assert!(
            probe_verdict(
                Some("OpenCode says the model opencode/beam-free is unavailable."),
                None,
                "",
                ""
            )
            .is_some()
        );
        // A deadline without any reply defers; a deadline after an answer does not.
        assert!(
            probe_verdict(None, Some(60), "", "")
                .unwrap()
                .contains("60-second")
        );
        assert!(probe_verdict(None, Some(60), "OK", "").is_none());
        // Quota text only in logs, no answer: deferred. With an answer, log words prove nothing.
        assert!(probe_verdict(None, None, "", "starting\nFree usage limit reached\n").is_some());
        assert!(probe_verdict(None, None, "OK", "request 429 ms later").is_none());
        // Failures that say nothing about availability stay a recorded model/app trial.
        assert!(probe_verdict(Some("invalid veecut operation"), None, "", "").is_none());
        assert!(probe_verdict(None, None, "", "").is_none());
    }
    #[test]
    fn probes_refuse_paid_and_unknown_models_before_any_call() {
        let mut probes = Probes::new(1);
        let root = Path::new("/nonexistent");
        for (agent, model) in [
            (AgentKind::Claude, "haiku"),
            (AgentKind::OpenCode, "vendor/free-looking"),
            (AgentKind::Claude, ladder::FREE_MODELS[0]),
        ] {
            assert!(
                probes
                    .unavailable(root, agent, &ladder::selection(model, None))
                    .is_err()
            );
        }
    }
    #[test]
    fn keeps_probe_log_tail_on_char_boundary() {
        let mut text = "é".repeat(10);
        keep_tail(&mut text, 5);
        assert_eq!(text, "éé");
    }
}

#[cfg(test)]
mod allowance_tests {
    use super::*;
    #[test]
    fn only_exact_free_allowance_messages_are_account_wide() {
        assert!(allowance_exhausted(
            "OpenCode's free allowance is used up. It can take a day or more."
        ));
        assert!(allowance_exhausted("FreeUsageLimitError"));
        assert!(allowance_exhausted("Free usage limit reached"));
        assert!(!allowance_exhausted(
            "OpenCode says the model opencode/beam-free is unavailable."
        ));
        assert!(!allowance_exhausted(
            "No reply within the 20-second probe deadline"
        ));
        assert!(!allowance_exhausted("rate limit 429"));
    }
    #[test]
    fn one_exhaustion_probe_covers_every_free_model_without_more_calls() {
        // Pre-seeded cache: any model returns it and no process is started (root does not exist).
        let mut probes = Probes::new(1);
        probes.exhausted = Some(Probe {
            reason: "OpenCode's free allowance is used up.".into(),
            seconds: 3.0,
            workspace: "probe".into(),
            log: String::new(),
        });
        for model in ladder::FREE_MODELS {
            let probe = probes
                .unavailable(
                    Path::new("/nonexistent"),
                    AgentKind::OpenCode,
                    &ladder::selection(model, Some("max")),
                )
                .unwrap();
            assert!(probe.unwrap().reason.contains("used up"));
        }
    }
}
