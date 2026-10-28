//! Offline grading of retained artifacts. This module never launches an agent or a process.
use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    path::{Path, PathBuf},
    time::Instant,
};

use anyhow::{Context, Result, ensure};
use ring::digest::{Context as DigestContext, SHA256};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use veecut_engine::{project::Project, transcript::Transcript};

use crate::{
    checks::{self, CheckResult},
    data::{self, Baseline, Fixture, Row, RunReference},
    execute,
    tasks::{self, Task},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FrozenInputs {
    pub origin: RunReference,
    pub task: Task,
    pub before: Vec<Project>,
    pub transcripts: Vec<Transcript>,
    pub inbox: BTreeMap<PathBuf, (u64, u64)>,
    pub provenance: String,
}

pub fn freeze(
    row: &Row,
    task: &Task,
    before: &[Project],
    transcripts: &[Transcript],
    inbox: &BTreeMap<PathBuf, (u64, u64)>,
) -> Result<PathBuf> {
    let path = row
        .task_path
        .parent()
        .context("Task artifact has no parent")?
        .join("frozen-inputs.json");
    ensure!(
        !path.exists(),
        "Frozen inputs already exist: {}",
        path.display()
    );
    data::write_json(&path,&FrozenInputs{origin:row.reference(),task:task.clone(),before:before.to_vec(),transcripts:transcripts.to_vec(),inbox:inbox.clone(),provenance:"Captured after task preparation and before the original model run; stored outside its workspace".into()})?;
    Ok(path)
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut value = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        value.push(DIGITS[(b >> 4) as usize] as char);
        value.push(DIGITS[(b & 15) as usize] as char);
    }
    value
}

/// Fingerprints compiled evaluator code, not mutable runtime source files.
pub fn checker_fingerprint() -> String {
    let mut context = DigestContext::new(&SHA256);
    context.update(include_bytes!("checks.rs"));
    context.update(include_bytes!("recheck.rs"));
    hex(context.finish().as_ref())
}

pub fn already_checked(row: &Row, fingerprint: &str) -> bool {
    row.rechecked && row.checker_fingerprint.as_deref() == Some(fingerprint)
}

fn source_fingerprint(path: &Path, expected: &str) -> Result<(u64, u64)> {
    ensure!(
        expected.len() == 64 && expected.bytes().all(|b| b.is_ascii_hexdigit()),
        "Missing usable SHA256 for {}",
        path.display()
    );
    let mut file = fs::File::open(path)?;
    let mut context = DigestContext::new(&SHA256);
    let mut bytes = [0u8; 65536];
    let mut length = 0u64;
    let mut hash = 0xcbf29ce484222325u64;
    loop {
        let n = file.read(&mut bytes)?;
        if n == 0 {
            break;
        }
        context.update(&bytes[..n]);
        length += n as u64;
        for b in &bytes[..n] {
            hash = (hash ^ u64::from(*b)).wrapping_mul(0x100000001b3);
        }
    }
    ensure!(
        hex(context.finish().as_ref()).eq_ignore_ascii_case(expected),
        "Immutable fixture SHA256 mismatch: {}",
        path.display()
    );
    Ok((length, hash))
}

fn original_check(row: &Row) -> Option<&Value> {
    row.evidence
        .get("expected")
        .or_else(|| row.evidence.pointer("/check/expected"))
}

fn legacy_inputs(
    root: &Path,
    manifest_dir: &Path,
    fixture: &Fixture,
    source: &Row,
    task: Task,
) -> Result<FrozenInputs> {
    let expected: tasks::Check = serde_json::from_value(
        original_check(source)
            .context("Original row has no frozen expected check; cannot trust a mutable task file")?
            .clone(),
    )?;
    ensure!(
        serde_json::to_value(&expected)? == serde_json::to_value(&task.check)?,
        "Retained task check differs from original recorded check"
    );
    ensure!(
        task.id == source.task
            && task.level == source.level
            && task.provenance == source.provenance,
        "Retained task identity or provenance changed"
    );
    let mut inbox = BTreeMap::new();
    let mut source_paths = BTreeMap::new();
    for file in fixture.files.iter().chain(&task.inputs) {
        let name = PathBuf::from(tasks::filename(&file.path));
        let path = manifest_dir.join(&file.path).canonicalize()?;
        if let Some(previous) = source_paths.insert(name.clone(), path.clone()) {
            ensure!(
                previous == path,
                "Original input basename collision: {}",
                name.display()
            );
            continue;
        }
        let sha = file
            .sha256
            .as_deref()
            .context("Manifest input has no SHA256; original inbox cannot be reconstructed")?;
        inbox.insert(name, source_fingerprint(&path, sha)?);
    }
    let (mut before, transcripts) = if source.level == 1 {
        (vec![], vec![])
    } else {
        let baseline = execute::baseline_dir(root, fixture);
        let current: Baseline = data::read_json(&baseline.join("baseline.json"))?;
        let copied: Baseline = data::read_json(&source.workspace.join("baseline.json"))
            .context("Legacy workspace has no copied baseline record")?;
        ensure!(
            current.status == "complete"
                && current.fixture == source.fixture
                && current.fixture == copied.fixture
                && current.projects == copied.projects
                && current.seconds == copied.seconds
                && current.beats == copied.beats,
            "Retained and immutable baseline records differ"
        );
        let (paths, projects, transcripts) = data::projects(&baseline)?;
        let started =
            chrono::DateTime::parse_from_rfc3339(&source.timestamp)?.with_timezone(&chrono::Utc);
        for path in &paths {
            let dir = path.parent().context("Baseline project has no parent")?;
            for entry in fs::read_dir(dir)? {
                let path = entry?.path();
                if path.file_name().is_some_and(|n| {
                    n == "project.json" || n.to_string_lossy().starts_with("transcript")
                }) && path.is_file()
                {
                    let modified: chrono::DateTime<chrono::Utc> =
                        fs::metadata(&path)?.modified()?.into();
                    ensure!(
                        modified <= started,
                        "Baseline evidence was modified after original run: {}",
                        path.display()
                    );
                }
            }
        }
        let mut expected_paths: Vec<_> =
            current.projects.iter().map(|p| baseline.join(p)).collect();
        expected_paths.sort();
        ensure!(
            paths == expected_paths,
            "Baseline project roster differs from the copied baseline record"
        );
        (projects, transcripts)
    };
    let baseline = execute::baseline_dir(root, fixture);
    for project in &mut before {
        for asset in &mut project.assets {
            if !asset.path.is_absolute() {
                continue;
            }
            if let Ok(relative) = asset.path.strip_prefix(&baseline) {
                asset.path = source.workspace.join(relative);
            } else {
                let canonical = asset.path.canonicalize()?;
                let name = source_paths
                    .iter()
                    .find_map(|(name, path)| (path == &canonical).then_some(name))
                    .context("Absolute baseline asset cannot be rebased into retained workspace")?;
                asset.path = source.workspace.join("inbox").join(name);
            }
        }
    }
    Ok(FrozenInputs{origin:source.reference(),task,before,transcripts,inbox,provenance:"Reconstructed offline from the unchanged recipe baseline and retained task checked against the original row. Every immutable fixture input was SHA256-verified against the manifest before deriving original inbox fingerprints. This is reconstructed evidence, not a historical pre-run snapshot.".into()})
}

fn load_inputs(
    root: &Path,
    manifest_dir: &Path,
    fixture: &Fixture,
    source: &Row,
    previous: &Row,
) -> Result<FrozenInputs> {
    let task: Task = data::read_json(&source.task_path)?;
    if let Some(path) = previous
        .frozen_inputs
        .as_ref()
        .or(source.frozen_inputs.as_ref())
    {
        let canonical = path.canonicalize()?;
        ensure!(
            canonical.starts_with(root.join("results/artifacts").canonicalize()?),
            "Frozen evidence is outside results/artifacts"
        );
        let frozen: FrozenInputs = data::read_json(&canonical)?;
        ensure!(
            frozen.origin == source.reference(),
            "Frozen inputs belong to another model attempt"
        );
        ensure!(
            serde_json::to_value(&frozen.task)? == serde_json::to_value(&task)?,
            "Retained task differs from frozen task; refusing to regenerate it"
        );
        return Ok(frozen);
    }
    legacy_inputs(root, manifest_dir, fixture, source, task)
}

pub fn evaluate(
    root: &Path,
    manifest_dir: &Path,
    fixture: &Fixture,
    source: &Row,
    previous: &Row,
    generation: u32,
    invalid: bool,
) -> Result<Row> {
    ensure!(
        !source.rechecked,
        "Recheck source must be an original model attempt"
    );
    let started = Instant::now();
    let mut row = source.clone();
    row.schema_version = 2;
    row.timestamp = chrono::Utc::now().to_rfc3339();
    row.generation = generation;
    row.rechecked = true;
    row.invalid = invalid || source.invalid || previous.invalid;
    row.original_run = Some(source.reference());
    row.checker_fingerprint = Some(checker_fingerprint());
    let run_id = source
        .workspace
        .file_name()
        .context("Run workspace has no name")?;
    let artifacts = root.join("results/artifacts").join(run_id);
    let mut reconstructed = None;
    let mut baseline_already_satisfied = None;
    let checked = (|| -> Result<(CheckResult, String)> {
        let canonical = source.workspace.canonicalize()?;
        ensure!(
            canonical.starts_with(root.join("work/runs").canonicalize()?),
            "Retained workspace is outside work/runs"
        );
        let frozen = load_inputs(root, manifest_dir, fixture, source, previous)?;
        let provenance = frozen.provenance.clone();
        let satisfied =
            checks::baseline_satisfied(&frozen.task, &frozen.before, &frozen.transcripts);
        baseline_already_satisfied = Some(satisfied);
        row.invalid |= satisfied;
        let (paths, after, _) = match data::projects(&source.workspace) {
            Ok(projects) => projects,
            Err(error) => {
                reconstructed = Some(frozen);
                return Ok((
                    CheckResult {
                        pass: false,
                        detail: format!("Retained model output is invalid: {error:#}"),
                        evidence: json!({"project_error":format!("{error:#}")}),
                        failure_class: Some("model".into()),
                    },
                    provenance,
                ));
            }
        };
        row.project_paths = paths;
        let unchanged = data::inbox_snapshot(&source.workspace.join("inbox"))
            .is_ok_and(|snapshot| snapshot == frozen.inbox);
        let checked = checks::check(
            &frozen.task,
            &frozen.before,
            &after,
            &frozen.transcripts,
            unchanged,
        );
        reconstructed = Some(frozen);
        Ok((checked, provenance))
    })();
    let (checked, provenance) = match checked {
        Ok(value) => value,
        Err(error) => (
            CheckResult {
                pass: false,
                detail: format!("Retained evidence cannot be rechecked: {error:#}"),
                evidence: json!({"error":format!("{error:#}")}),
                failure_class: Some("check".into()),
            },
            "Required frozen or reconstructable evidence unavailable".into(),
        ),
    };
    if let Some(frozen) = reconstructed {
        if let Some(path) = previous
            .frozen_inputs
            .as_ref()
            .or(source.frozen_inputs.as_ref())
        {
            row.frozen_inputs = Some(path.clone());
        } else {
            let path = artifacts.join(format!("reconstructed-inputs-{generation}.json"));
            ensure!(
                !path.exists(),
                "Reconstruction artifact already exists: {}",
                path.display()
            );
            data::write_json(&path, &frozen)?;
            row.frozen_inputs = Some(path);
        }
    }
    let execution_ok = source.exit_code == Some(0)
        && !source.timed_out
        && source.failure_class.as_deref() != Some("environment");
    row.pass = checked.pass && execution_ok && !row.invalid;
    row.failure_class = if !execution_ok {
        source
            .failure_class
            .clone()
            .or_else(|| Some("model".into()))
    } else if row.invalid {
        Some("check".into())
    } else {
        checked
            .failure_class
            .clone()
            .or_else(|| (!checked.pass).then(|| "model".into()))
    };
    row.check_detail = format!(
        "Offline recheck: {}{}{}{}",
        checked.detail,
        if !execution_ok {
            "; original execution failed/stopped or timed out, so overall failure is retained"
        } else {
            ""
        },
        if baseline_already_satisfied == Some(true) {
            "; original baseline already satisfied the request, so no challenge was tested"
        } else {
            ""
        },
        if row.invalid {
            "; original attempt is invalid and requires a fresh live run"
        } else {
            ""
        }
    );
    let result_path = artifacts.join(format!(
        "recheck-{generation}-{}.json",
        execute::unique_id()
    ));
    row.evidence = json!({"check":checked.evidence,"artifact_check_pass":checked.pass,"original_run":source.reference(),"previous_verdict":previous.reference(),"original_evidence":source.evidence,"original_check_detail":source.check_detail,"ground_truth":provenance,"baseline_already_satisfied":baseline_already_satisfied,"live_rerun_required":row.invalid||!execution_ok,"offline":true,"new_model_calls":0,"new_tokens":0,"new_cost_usd":0.0,"inherited_source_run_seconds_tokens_and_cost":true,"offline_seconds":started.elapsed().as_secs_f64(),"result_path":result_path,"invalid":row.invalid});
    data::write_json(&result_path, &row)?;
    data::append_row(&root.join("results"), &row)?;
    Ok(row)
}

#[cfg(test)]
mod tests {
    use super::*;
    use veecut_engine::project::{Canvas, VideoKind};

    fn retained() -> (tempfile::TempDir, Fixture, Row) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let mut row = data::synthetic_row();
        row.workspace = root.join("work/runs/synthetic");
        let artifact = root.join("results/artifacts/synthetic");
        fs::create_dir_all(&artifact).unwrap();
        row.task_path = artifact.join("task.json");
        row.answer_path = artifact.join("answer.txt");
        row.log_path = artifact.join("agent.log");
        row.prompt_path = artifact.join("prompt.txt");
        fs::create_dir_all(row.workspace.join("inbox")).unwrap();
        fs::write(row.workspace.join("inbox/source.mp4"), b"synthetic bytes").unwrap();
        let before = Project::new("synthetic", VideoKind::TalkingHeadShort, Canvas::vertical());
        let mut after = before.clone();
        after.captions.size = 0.06;
        let project_path = row.workspace.join("projects/test/project.json");
        after.save(&project_path).unwrap();
        row.project_paths = vec![project_path];
        let task = Task {
            id: row.task.clone(),
            level: 2,
            prompt: "Set caption size to 0.06".into(),
            check: tasks::Check::CaptionSize { size: 0.06 },
            provenance: "synthetic immutable truth".into(),
            prerequisite: None,
            inputs: vec![],
        };
        data::write_json(&row.task_path, &task).unwrap();
        row.provenance = task.provenance.clone();
        let inbox = data::inbox_snapshot(&row.workspace.join("inbox")).unwrap();
        row.frozen_inputs = Some(freeze(&row, &task, &[before], &[], &inbox).unwrap());
        row.pass = false;
        row.failure_class = Some("check".into());
        let fixture = Fixture {
            id: row.fixture.clone(),
            kind: row.kind.clone(),
            length: "S".into(),
            files: vec![],
            language: None,
            notes: json!({}),
        };
        (temp, fixture, row)
    }
    #[test]
    fn offline_recheck_links_original_and_does_not_create_model_spend() {
        let (temp, fixture, source) = retained();
        let row = evaluate(
            temp.path(),
            temp.path(),
            &fixture,
            &source,
            &source,
            1,
            false,
        )
        .unwrap();
        assert!(row.pass);
        assert!(row.rechecked);
        assert_eq!(row.original_run, Some(source.reference()));
        assert_eq!(row.generation, 1);
        assert_eq!(row.seconds, source.seconds);
        assert_eq!(row.tokens_in, source.tokens_in);
        assert_eq!(row.cost_usd, source.cost_usd);
        assert_eq!(row.evidence["new_model_calls"], 0);
        assert!(already_checked(&row, &checker_fingerprint()));
        assert!(!already_checked(&row, "different-checker"));
        let loaded = data::rows(&temp.path().join("results")).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(
            data::effective_rows(&[source.clone(), row.clone()]).len(),
            1
        );
        assert_eq!(
            crate::ladder::generation(&[source.clone(), row.clone()], &source.cell(), false),
            0
        );
        assert_eq!(
            crate::ladder::generation(&[source.clone(), row.clone()], &source.cell(), true),
            2
        );
        assert_eq!(
            crate::ladder::outcomes(&[source, row], &loaded[0].cell(), 0),
            vec![true]
        );
    }
    #[test]
    fn timeout_invalid_and_changed_inbox_cannot_be_promoted() {
        for mode in ["timeout", "invalid", "inbox"] {
            let (temp, fixture, mut source) = retained();
            if mode == "timeout" {
                source.timed_out = true;
                source.exit_code = None;
            }
            if mode == "invalid" {
                source.invalid = true;
            }
            if mode == "inbox" {
                fs::write(source.workspace.join("inbox/source.mp4"), b"altered").unwrap();
            }
            let row = evaluate(
                temp.path(),
                temp.path(),
                &fixture,
                &source,
                &source,
                1,
                false,
            )
            .unwrap();
            assert!(!row.pass, "{mode}");
            if mode == "invalid" {
                assert!(row.invalid);
            }
        }
    }
    #[test]
    fn changed_task_and_missing_truth_fail_without_regeneration() {
        let (temp, fixture, source) = retained();
        let mut task: Task = data::read_json(&source.task_path).unwrap();
        task.check = tasks::Check::CaptionSize { size: 0.08 };
        data::write_json(&source.task_path, &task).unwrap();
        let row = evaluate(
            temp.path(),
            temp.path(),
            &fixture,
            &source,
            &source,
            1,
            false,
        )
        .unwrap();
        assert_eq!(row.failure_class.as_deref(), Some("check"));
        assert!(row.check_detail.contains("differs from frozen"));
    }
    #[test]
    fn original_manifest_digest_is_checked() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("source");
        fs::write(&file, b"abc").unwrap();
        let expected = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        assert_eq!(source_fingerprint(&file, expected).unwrap().0, 3);
        assert!(source_fingerprint(&file, &"0".repeat(64)).is_err());
    }
    #[test]
    fn already_safe_visual_start_is_invalid_and_keeps_environment_timeout() {
        use veecut_engine::project::{
            Asset, AssetKind, AssetRole, Caption, Clip, FaceSample, Item, ItemBody, Word,
        };
        let (temp, fixture, mut source) = retained();
        let mut project = Project::new(
            "safe start",
            VideoKind::TalkingHeadShort,
            Canvas::vertical(),
        );
        project.assets.push(Asset {
            id: "a".into(),
            path: "../../inbox/source.mp4".into(),
            kind: AssetKind::Video,
            role: AssetRole::Source,
            duration: 10.0,
            width: 1080,
            height: 1920,
            fps: 30.0,
            has_audio: true,
        });
        project.main_mut().items.push(Item {
            id: "clip".into(),
            start: 0.0,
            duration: 10.0,
            body: ItemBody::Clip(Clip::new("a", 0.0)),
        });
        project.tracks[3].items.push(Item {
            id: "caption".into(),
            start: 2.0,
            duration: 1.0,
            body: ItemBody::Caption(Caption {
                words: vec![Word {
                    text: "Hello".into(),
                    start: 0.0,
                    end: 0.9,
                    emphasis: false,
                }],
            }),
        });
        let samples = vec![FaceSample {
            t: 2.5,
            x: 0.35,
            y: 0.1,
            w: 0.3,
            h: 0.3,
        }];
        project.faces.insert("a".into(), samples.clone());
        project.captions.position = 0.8;
        source.task = "mouth-safe".into();
        source.level = 6;
        source.timed_out = true;
        source.exit_code = None;
        source.failure_class = Some("environment".into());
        let task = Task {
            id: source.task.clone(),
            level: 6,
            prompt: "Keep captions off the mouth".into(),
            check: tasks::Check::Mouth {
                asset: "a".into(),
                samples,
            },
            provenance: "synthetic independent face samples".into(),
            prerequisite: None,
            inputs: vec![],
        };
        source.provenance = task.provenance.clone();
        data::write_json(&source.task_path, &task).unwrap();
        let frozen = FrozenInputs {
            origin: source.reference(),
            task,
            before: vec![project.clone()],
            transcripts: vec![],
            inbox: data::inbox_snapshot(&source.workspace.join("inbox")).unwrap(),
            provenance: "Reconstructed synthetic legacy safe start".into(),
        };
        data::write_json(source.frozen_inputs.as_ref().unwrap(), &frozen).unwrap();
        project.save(&source.project_paths[0]).unwrap();
        let row = evaluate(
            temp.path(),
            temp.path(),
            &fixture,
            &source,
            &source,
            1,
            false,
        )
        .unwrap();
        assert_eq!(row.evidence["baseline_already_satisfied"], true);
        assert!(row.invalid);
        assert!(!row.pass);
        assert_eq!(row.failure_class.as_deref(), Some("environment"));
        assert!(row.check_detail.contains("no challenge was tested"));
    }
}
