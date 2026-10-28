mod checks;
mod data;
mod execute;
mod ladder;
mod recheck;
mod report;
mod tasks;

use crate::{
    data::{Fixture, Row},
    ladder::Cell,
    tasks::Task,
};
use anyhow::{Context, Result, ensure};
use clap::{Args, Parser, Subcommand};
use std::{
    fs,
    path::{Path, PathBuf},
};
use veecut_engine::agent::{AgentKind, Selection};

#[derive(Parser)]
#[command(
    about = "Local, resumable VeeCut fixture evaluations. Never uploads transcription media."
)]
struct Cli {
    #[arg(long, global = true)]
    root: Option<PathBuf>,
    #[arg(long, global = true)]
    manifest: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    /// Build deterministic recipe baselines with local CPU transcription, without rendering.
    Baselines {
        #[command(flatten)]
        filter: Filter,
        #[arg(long)]
        screening: bool,
    },
    /// Run explicit cells, or the fixed 49-cell free screening batch.
    Run(RunArgs),
    /// Try each provider's cheapest rungs; two passes, third trial on a split.
    Ladder(LadderArgs),
    /// Regrade retained artifacts offline; never launches a model or prepares a new challenge.
    Recheck(RecheckArgs),
    /// Markdown recommendations and complete artifact-linked screening/failure tables.
    Report {
        #[arg(long)]
        out: Option<PathBuf>,
    },
}
#[derive(Args, Default)]
struct Filter {
    #[arg(long, value_delimiter = ',')]
    fixture: Vec<String>,
    #[arg(long, value_delimiter = ',')]
    kind: Vec<String>,
    #[arg(long, value_delimiter = ',')]
    length: Vec<String>,
}
impl Filter {
    fn matches(&self, f: &Fixture) -> bool {
        (self.fixture.is_empty() || self.fixture.contains(&f.id))
            && (self.kind.is_empty() || self.kind.contains(&f.kind))
            && (self.length.is_empty() || self.length.contains(&f.length))
    }
    fn empty(&self) -> bool {
        self.fixture.is_empty() && self.kind.is_empty() && self.length.is_empty()
    }
}
#[derive(Args)]
struct TaskFilter {
    #[command(flatten)]
    filter: Filter,
    #[arg(long, value_delimiter = ',')]
    task: Vec<String>,
    #[arg(long,value_delimiter=',',value_parser=clap::value_parser!(u8).range(1..=6))]
    level: Vec<u8>,
}
impl TaskFilter {
    fn matches(&self, t: &Task) -> bool {
        (self.task.is_empty() || self.task.contains(&t.id))
            && (self.level.is_empty() || self.level.contains(&t.level))
    }
}
#[derive(Args)]
struct RunArgs {
    #[command(flatten)]
    filter: TaskFilter,
    #[arg(long,default_value="opencode",value_parser=["opencode","claude","codex","gemini"])]
    agent: String,
    #[arg(long)]
    model: Option<String>,
    /// Omit for agent default; no saved effort is inherited.
    #[arg(long)]
    effort: Option<String>,
    #[arg(long,default_value_t=1,value_parser=clap::value_parser!(u32).range(1..))]
    trials: u32,
    #[arg(long,value_parser=clap::value_parser!(u64).range(1..))]
    timeout: Option<u64>,
    #[arg(long)]
    rerun: bool,
    #[arg(long)]
    allow_paid: bool,
    #[arg(long)]
    screening: bool,
}
#[derive(Args)]
struct LadderArgs {
    #[command(flatten)]
    filter: TaskFilter,
    #[arg(long,value_parser=clap::value_parser!(u64).range(1..))]
    timeout: Option<u64>,
    #[arg(long)]
    rerun: bool,
    /// Only orchestrators may enable paid rungs; monitor weekly budgets between batches.
    #[arg(long)]
    allow_paid: bool,
    /// Rungs to try: providers free/opencode, claude, codex, or rung ids MODEL[@EFFORT]
    /// (e.g. claude,codex or opencode/flint-40b-free@max). Omit for the whole ladder.
    #[arg(long, value_delimiter = ',')]
    rungs: Vec<String>,
    /// Seconds allowed for the once-per-model free availability probe.
    #[arg(long,default_value_t=20,value_parser=clap::value_parser!(u64).range(5..=600))]
    probe_timeout: u64,
}

#[derive(Args)]
struct RecheckArgs {
    #[command(flatten)]
    filter: TaskFilter,
    #[arg(long, value_delimiter = ',')]
    model: Vec<String>,
    #[arg(long, value_delimiter = ',')]
    failure_class: Vec<String>,
    /// Select one retained original attempt, including an older live-run generation.
    #[arg(long)]
    workspace: Option<PathBuf>,
    /// Recheck even when this compiled checker already graded the retained attempt.
    #[arg(long)]
    rerun: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .context("Home directory not set; pass --root")?;
    let root = cli.root.unwrap_or_else(|| home.join("veecut-eval"));
    let root = safe_output(&root, &home)?;
    if let Command::Report { out } = cli.command {
        let results = root.join("results");
        let text = report::markdown(&data::rows(&results)?, &data::deferrals(&results)?);
        if let Some(path) = out {
            fs::write(safe_output(&path, &home)?, text)?;
        } else {
            print!("{text}");
        }
        return Ok(());
    }
    let manifest = cli.manifest.unwrap_or_else(|| root.join("manifest.json"));
    let manifest = manifest
        .canonicalize()
        .with_context(|| format!("opening manifest {}", manifest.display()))?;
    let manifest_dir = manifest.parent().context("Manifest has no parent")?;
    let fixtures = data::manifest(&manifest)?;
    for output in [root.join("work"), root.join("results")] {
        let output = safe_output(&output, &home)?;
        ensure!(
            !output.starts_with(
                manifest_dir
                    .join("media")
                    .canonicalize()
                    .unwrap_or_else(|_| manifest_dir.join("media"))
            ),
            "Refusing manifest media output path"
        );
    }
    // This executable is single-threaded here. Children inherit the override; user config stays untouched.
    unsafe {
        std::env::set_var("VEECUT_LOCAL_CPU_ONLY", "1");
    }
    let _lock = execute::BatchLock::acquire(&root)?;
    let mut rows = data::rows(&root.join("results"))?;
    match cli.command {
        Command::Baselines { filter, screening } => {
            let selected: Vec<_> = fixtures
                .iter()
                .filter(|f| {
                    filter.matches(f)
                        && (!screening || ladder::SCREENING.iter().any(|(id, _, _)| *id == f.id))
                })
                .collect();
            ensure!(!selected.is_empty(), "No fixtures matched filters");
            execute::baselines(&root, manifest_dir, &selected)?;
        }
        Command::Run(args) => {
            if args.screening {
                ensure!(
                    args.filter.filter.empty()
                        && args.filter.task.is_empty()
                        && args.filter.level.is_empty()
                        && args.model.is_none()
                        && args.effort.is_none()
                        && args.agent == "opencode"
                        && args.trials == 1,
                    "--screening fixes six fixture tasks, nine free models, default efforts and one trial; no selection filters allowed"
                );
                for (id, task_id, level) in ladder::SCREENING {
                    let fixture = fixtures
                        .iter()
                        .find(|f| f.id == id)
                        .with_context(|| format!("Screening fixture {id} missing from manifest"))?;
                    let task = fixture_tasks(&root, fixture, &fixtures)?
                        .into_iter()
                        .find(|t| t.id == task_id)
                        .with_context(|| format!("Screening task {task_id} missing for {id}"))?;
                    for model in ladder::FREE_MODELS {
                        if level == 6 && !ladder::image_capable(model) {
                            continue;
                        }
                        run_trials(
                            &root,
                            manifest_dir,
                            fixture,
                            &task,
                            AgentKind::OpenCode,
                            &ladder::selection(model, None),
                            1,
                            args.timeout,
                            args.rerun,
                            true,
                            false,
                            &mut rows,
                        )?;
                    }
                }
            } else {
                let agent = parse_agent(&args.agent);
                let selection = Selection {
                    model: args.model.or_else(|| {
                        (agent == AgentKind::OpenCode).then(|| ladder::FREE_MODELS[0].into())
                    }),
                    effort: args.effort,
                };
                paid_guard(agent, &selection, args.allow_paid)?;
                let mut count = 0;
                for fixture in fixtures.iter().filter(|f| args.filter.filter.matches(f)) {
                    for task in fixture_tasks(&root, fixture, &fixtures)?
                        .into_iter()
                        .filter(|t| args.filter.matches(t))
                    {
                        if task.level == 6
                            && !ladder::image_capable(selection.model.as_deref().unwrap_or(""))
                        {
                            continue;
                        }
                        count += 1;
                        run_trials(
                            &root,
                            manifest_dir,
                            fixture,
                            &task,
                            agent,
                            &selection,
                            args.trials,
                            args.timeout,
                            args.rerun,
                            false,
                            args.allow_paid,
                            &mut rows,
                        )?;
                    }
                }
                ensure!(count > 0, "No tasks matched filters");
            }
        }
        Command::Ladder(args) => {
            let rungs = ladder::Rungs::parse(&args.rungs)?;
            ensure!(
                !rungs.selects_paid() || args.allow_paid,
                "--rungs selects paid rungs (claude/codex); they also require --allow-paid from the orchestrator"
            );
            let mut probes = execute::Probes::new(args.probe_timeout);
            let mut count = 0;
            for fixture in fixtures.iter().filter(|f| args.filter.filter.matches(f)) {
                for task in fixture_tasks(&root, fixture, &fixtures)?
                    .into_iter()
                    .filter(|t| args.filter.matches(t))
                {
                    count += 1;
                    let mut defaults = ladder::free_candidates(&rows, task.level == 6);
                    for model in rungs.named_free_models() {
                        if !defaults.iter().any(|s| s.model.as_deref() == Some(model))
                            && (task.level != 6 || ladder::image_capable(model))
                        {
                            defaults.push(ladder::selection(model, None));
                        }
                    }
                    let (free_pass, deferred) =
                        ladder::climb_free(&defaults, &rungs, &mut |selection| {
                            ladder_cell(
                                &root,
                                manifest_dir,
                                fixture,
                                &task,
                                AgentKind::OpenCode,
                                selection,
                                &args,
                                &mut probes,
                                &mut rows,
                            )
                        })?;
                    if !free_pass && args.allow_paid {
                        if deferred {
                            // A deferred free rung proves nothing, so it must not trigger paid spend.
                            eprintln!(
                                "{} / {}: paid rungs withheld; a free rung was deferred, not failed",
                                fixture.id, task.id
                            );
                            continue;
                        }
                        for agent in [AgentKind::Claude, AgentKind::Codex] {
                            for selection in ladder::paid_candidates(agent) {
                                if !rungs.allows(agent, &selection) {
                                    continue;
                                }
                                if ladder_cell(
                                    &root,
                                    manifest_dir,
                                    fixture,
                                    &task,
                                    agent,
                                    &selection,
                                    &args,
                                    &mut probes,
                                    &mut rows,
                                )? == Cell::Pass
                                {
                                    break;
                                }
                            }
                        }
                    }
                }
            }
            ensure!(count > 0, "No tasks matched filters");
        }
        Command::Recheck(args) => {
            let workspace = args
                .workspace
                .as_ref()
                .map(|p| safe_output(p, &home))
                .transpose()?;
            let candidates = if workspace.is_some() {
                data::effective_rows(&rows)
            } else {
                data::current_rows(&rows)
            };
            let selected: Vec<Row> = candidates
                .into_iter()
                .filter(|row| {
                    (args.filter.filter.fixture.is_empty()
                        || args.filter.filter.fixture.contains(&row.fixture))
                        && (args.filter.filter.kind.is_empty()
                            || args.filter.filter.kind.contains(&row.kind))
                        && (args.filter.filter.length.is_empty()
                            || args.filter.filter.length.contains(&row.length))
                        && (args.filter.task.is_empty() || args.filter.task.contains(&row.task))
                        && (args.filter.level.is_empty() || args.filter.level.contains(&row.level))
                        && (args.model.is_empty() || args.model.contains(&row.model))
                        && (args.failure_class.is_empty()
                            || row
                                .failure_class
                                .as_ref()
                                .is_some_and(|class| args.failure_class.contains(class)))
                        && workspace.as_ref().is_none_or(|path| {
                            row.workspace.canonicalize().is_ok_and(|p| &p == path)
                        })
                })
                .cloned()
                .collect();
            ensure!(
                !selected.is_empty(),
                "No retained attempts matched recheck filters"
            );
            let fingerprint = recheck::checker_fingerprint();
            for previous in selected {
                let invalid = data::attempt_invalid(&rows, &previous);
                if !args.rerun
                    && recheck::already_checked(&previous, &fingerprint)
                    && (!invalid || previous.invalid)
                {
                    continue;
                }
                let origin = previous.origin();
                let source = rows
                    .iter()
                    .find(|row| !row.rechecked && row.reference() == origin)
                    .cloned()
                    .context("Original model attempt row is missing")?;
                let fixture = fixtures
                    .iter()
                    .find(|f| f.id == source.fixture)
                    .context("Original fixture is missing from manifest")?;
                let generation = rows
                    .iter()
                    .filter(|row| row.cell() == source.cell())
                    .map(|row| row.generation)
                    .max()
                    .unwrap_or(0)
                    + 1;
                let row = recheck::evaluate(
                    &root,
                    manifest_dir,
                    fixture,
                    &source,
                    &previous,
                    generation,
                    invalid,
                )?;
                eprintln!(
                    "{} / {} / {}: offline {}{}",
                    row.fixture,
                    row.task,
                    row.model,
                    if row.pass { "PASS" } else { "FAIL" },
                    if row.invalid {
                        " (invalid; live rerun required)"
                    } else {
                        ""
                    }
                );
                rows.push(row);
            }
        }
        Command::Report { .. } => unreachable!(),
    }
    Ok(())
}

fn safe_output(path: &Path, home: &Path) -> Result<PathBuf> {
    let absolute = std::path::absolute(path)?;
    let mut suffix = vec![];
    let mut existing = absolute.as_path();
    while !existing.exists() {
        suffix.push(
            existing
                .file_name()
                .context("Output path has no existing ancestor")?
                .to_owned(),
        );
        existing = existing.parent().context("Output path has no parent")?;
    }
    let mut resolved = existing.canonicalize()?;
    for part in suffix.iter().rev() {
        resolved.push(part);
    }
    for protected in [home.join("veecut-test"), home.join("veecut-eval/media")] {
        let protected = protected.canonicalize().unwrap_or(protected);
        ensure!(
            !resolved.starts_with(&protected),
            "Refusing protected fixture/test output path: {}",
            resolved.display()
        );
    }
    Ok(resolved)
}

fn parse_agent(agent: &str) -> AgentKind {
    match agent {
        "claude" => AgentKind::Claude,
        "codex" => AgentKind::Codex,
        "gemini" => AgentKind::Gemini,
        _ => AgentKind::OpenCode,
    }
}
fn paid_guard(agent: AgentKind, selection: &Selection, allow_paid: bool) -> Result<()> {
    ensure!(
        allow_paid
            || selection
                .model
                .as_deref()
                .is_some_and(|m| ladder::free(agent, m)),
        "Paid or unknown model requires explicit --allow-paid from orchestrator"
    );
    Ok(())
}
fn fixture_tasks(root: &Path, fixture: &Fixture, all: &[Fixture]) -> Result<Vec<Task>> {
    let dir = execute::baseline_dir(root, fixture);
    let (before, transcripts) = if execute::complete_baseline(root, fixture).is_some() {
        let (_, p, t) = data::projects(&dir)?;
        (p, t)
    } else {
        (vec![], vec![])
    };
    let beats = data::read_json::<data::Baseline>(&dir.join("baseline.json"))
        .map(|b| b.beats)
        .unwrap_or_default();
    Ok(tasks::templates(
        fixture,
        all,
        &before,
        &transcripts,
        &beats,
    ))
}

#[allow(clippy::too_many_arguments)]
fn run_trials(
    root: &Path,
    manifest_dir: &Path,
    fixture: &Fixture,
    task: &Task,
    agent: AgentKind,
    selection: &Selection,
    trials: u32,
    timeout: Option<u64>,
    rerun: bool,
    screening: bool,
    allow_paid: bool,
    rows: &mut Vec<Row>,
) -> Result<()> {
    paid_guard(agent, selection, allow_paid)?;
    let key = ladder::cell(&fixture.id, &task.id, agent, selection);
    let generation = ladder::generation(rows, &key, rerun);
    for trial in 1..=trials {
        if rows.iter().any(|r| {
            !r.rechecked
                && r.cell() == key
                && r.source_generation() == generation
                && r.trial == trial
        }) {
            continue;
        }
        rows.push(execute::run(execute::RunSpec {
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
        })?);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn ladder_cell(
    root: &Path,
    manifest_dir: &Path,
    fixture: &Fixture,
    task: &Task,
    agent: AgentKind,
    selection: &Selection,
    args: &LadderArgs,
    probes: &mut execute::Probes,
    rows: &mut Vec<Row>,
) -> Result<Cell> {
    paid_guard(agent, selection, args.allow_paid)?;
    let key = ladder::cell(&fixture.id, &task.id, agent, selection);
    let generation = ladder::generation(rows, &key, args.rerun);
    loop {
        match ladder::decide(&ladder::outcomes(rows, &key, generation)) {
            ladder::Decision::Reliable => return Ok(Cell::Pass),
            ladder::Decision::Unreliable => return Ok(Cell::Fail),
            ladder::Decision::Trial(trial) => {
                // Free rungs: one bounded probe per model per batch, before the first spend.
                if selection
                    .model
                    .as_deref()
                    .is_some_and(|m| ladder::free(agent, m))
                    && let Some(probe) = probes.unavailable(root, agent, selection)?
                {
                    let deferral = data::Deferral {
                        timestamp: chrono::Utc::now().to_rfc3339(),
                        fixture: fixture.id.clone(),
                        task: task.id.clone(),
                        level: task.level,
                        agent: agent.name().into(),
                        model: selection.model.clone().unwrap_or_default(),
                        effort: selection.effort.clone(),
                        reason: probe.reason.clone(),
                        probe_seconds: probe.seconds,
                        probe_workspace: probe.workspace.clone(),
                        probe_log: probe.log.clone(),
                    };
                    data::append_deferral(&root.join("results"), &deferral)?;
                    eprintln!(
                        "{} / {} / {}: DEFERRED, not a model trial ({}). Rerun the same command later to retry.",
                        fixture.id,
                        task.id,
                        ladder::rung_id(selection),
                        deferral.reason
                    );
                    return Ok(Cell::Deferred);
                }
                rows.push(execute::run(execute::RunSpec {
                    root,
                    manifest_dir,
                    fixture,
                    task,
                    agent,
                    selection,
                    trial,
                    timeout: args.timeout,
                    screening: false,
                    generation,
                })?)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn paid_gate_never_accepts_unknown_free_names() {
        assert!(
            paid_guard(
                AgentKind::OpenCode,
                &ladder::selection(ladder::FREE_MODELS[0], None),
                false
            )
            .is_ok()
        );
        assert!(
            paid_guard(
                AgentKind::Claude,
                &ladder::selection("haiku", Some("low")),
                false
            )
            .is_err()
        );
        assert!(
            paid_guard(
                AgentKind::OpenCode,
                &ladder::selection("vendor/free", None),
                false
            )
            .is_err()
        );
        assert!(
            paid_guard(
                AgentKind::Claude,
                &ladder::selection("haiku", Some("low")),
                true
            )
            .is_ok()
        );
    }
    #[test]
    fn protected_outputs_reject_paths_and_symlink_aliases() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        fs::create_dir_all(home.join("veecut-eval/media")).unwrap();
        fs::create_dir_all(home.join("veecut-test")).unwrap();
        assert!(safe_output(&home.join("veecut-test/new"), home).is_err());
        assert!(safe_output(&home.join("veecut-eval/media/new"), home).is_err());
        assert!(safe_output(&home.join("veecut-eval/work"), home).is_ok());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(home.join("veecut-eval/media"), home.join("alias")).unwrap();
            assert!(safe_output(&home.join("alias/new"), home).is_err());
        }
    }
}
