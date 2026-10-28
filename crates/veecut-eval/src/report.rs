use crate::{
    data::Row,
    ladder::{self, Decision},
};
use std::{collections::BTreeMap, fmt::Write, path::Path};

pub fn median(mut values: Vec<f64>) -> Option<f64> {
    values.retain(|v| v.is_finite());
    if values.is_empty() {
        return None;
    }
    values.sort_by(f64::total_cmp);
    let i = values.len() / 2;
    Some(if values.len() % 2 == 0 {
        (values[i - 1] + values[i]) / 2.0
    } else {
        values[i]
    })
}
fn number(value: Option<f64>, precision: usize) -> String {
    value.map_or_else(|| "not reported".into(), |v| format!("{v:.precision$}"))
}
fn cell(text: &str) -> String {
    text.replace('|', "\\|").replace(['\n', '\r'], " ")
}
fn link(path: &Path, label: &str) -> String {
    format!("[{label}](<{}>)", path.display())
}
fn model(row: &Row) -> String {
    format!(
        "{} / {}",
        row.model,
        row.effort.as_deref().unwrap_or("default")
    )
}

fn unassessed_legacy_visual(row: &Row) -> bool {
    row.level == 6 && !row.rechecked && row.frozen_inputs.is_none()
}

fn state(rows: &[Row], row: &Row) -> &'static str {
    if crate::data::attempt_invalid(rows, row) {
        "invalid; live rerun required"
    } else if unassessed_legacy_visual(row) {
        "legacy visual without prepared challenge; live rerun required"
    } else if row.timed_out
        || row.exit_code != Some(0)
        || row.failure_class.as_deref() == Some("environment")
    {
        "execution failed; live rerun required"
    } else if row.rechecked {
        if row.pass {
            "clean offline recheck"
        } else {
            "offline recheck failed"
        }
    } else if row.pass {
        "clean live pass"
    } else {
        "clean live attempt; check failed"
    }
}

fn evidence_text(rows: &[Row], row: &Row) -> String {
    let key = row.attempt_key();
    let original = rows
        .iter()
        .find(|candidate| !candidate.rechecked && candidate.attempt_key() == key);
    let invalid = rows
        .iter()
        .rev()
        .find(|candidate| candidate.invalid && candidate.attempt_key() == key);
    let value = |pointer: &str| {
        row.evidence
            .pointer(pointer)
            .or_else(|| row.evidence.get("original_evidence")?.pointer(pointer))
            .or_else(|| original?.evidence.pointer(pointer))
            .or_else(|| invalid?.evidence.pointer(pointer))
    };
    let text = |value: Option<&serde_json::Value>| {
        value
            .filter(|v| !v.is_null())
            .map(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| v.to_string())
            })
            .filter(|v| !v.trim().is_empty())
    };
    let reviewed = text(value("/failure_review/evidence"));
    let has_review = reviewed.is_some();
    let differs_from_check = reviewed.as_deref() != Some(row.check_detail.as_str());
    let mut detail = reviewed.unwrap_or_else(|| row.check_detail.clone());
    if has_review {
        if let Some(citation) = text(value("/failure_review/citation")) {
            let _ = write!(detail, " Citation: {citation}.");
        }
        if row.rechecked && differs_from_check {
            let _ = write!(detail, " Current check: {}", row.check_detail);
        }
    }
    if crate::data::attempt_invalid(rows, row) {
        let reason = text(value("/invalid_reason")).or_else(|| {
            (value("/baseline_already_satisfied").and_then(serde_json::Value::as_bool) == Some(true))
                .then(|| "The original baseline already satisfied the request; no change challenge was tested.".into())
        });
        if let Some(reason) = reason {
            let _ = write!(detail, " Invalid: {reason}");
        }
    }
    detail
}

pub fn markdown(rows: &[Row]) -> String {
    let mut out = String::from(
        "# VeeCut model evaluation (exploratory)\n\nRecommendations require two independent passing model trials (third trial after a split). An offline recheck revises the verdict for the same original trial; it is never another model run or additional spend. Single screening passes are not reliability recommendations. Invalid attempts are excluded. Failed workspaces are retained. Token input excludes cached tokens; totals include each once. Missing usage remains unreported. Costs are CLI-reported values, not inferred API prices.\n\n",
    );
    let originals: Vec<_> = rows.iter().filter(|row| !row.rechecked).collect();
    let rechecks = rows.iter().filter(|row| row.rechecked).count();
    let _ = writeln!(
        out,
        "{} original model attempts + {rechecks} offline recheck records. Rechecks inherit their original attempt's seconds, tokens and cost; those fields are not new usage.\n",
        originals.len()
    );
    let _ = writeln!(
        out,
        "Original-run totals only: {:.1} seconds; {} reported tokens ({} of {} attempts reported usage); ${:.5} reported cost ({} of {} attempts reported cost).\n",
        originals.iter().map(|r| r.seconds).sum::<f64>(),
        originals
            .iter()
            .filter_map(|r| r.total_tokens())
            .sum::<u64>(),
        originals
            .iter()
            .filter(|r| r.total_tokens().is_some())
            .count(),
        originals.len(),
        originals.iter().filter_map(|r| r.cost_usd).sum::<f64>(),
        originals.iter().filter(|r| r.cost_usd.is_some()).count(),
        originals.len()
    );
    let active: Vec<_> = crate::data::current_rows(rows)
        .into_iter()
        .filter(|row| !crate::data::attempt_invalid(rows, row) && !unassessed_legacy_visual(row))
        .collect();
    let mut cells = BTreeMap::<String, Vec<&Row>>::new();
    for row in &active {
        cells.entry(row.cell()).or_default().push(row);
    }
    type Group = (String, String, u8, String, String, String);
    let mut groups = BTreeMap::<Group, Vec<Vec<&Row>>>::new();
    for mut trials in cells.into_values() {
        trials.sort_by_key(|r| r.trial);
        trials.dedup_by_key(|r| r.trial);
        let row = trials[0];
        groups
            .entry((
                row.kind.clone(),
                row.length.clone(),
                row.level,
                row.fixture.clone(),
                row.task.clone(),
                ladder::provider(&row.agent).into(),
            ))
            .or_default()
            .push(trials);
    }
    out.push_str("## Cheapest reliable rung by kind, length and level\n\n| Kind | Length | Level | Fixture / task | Provider | Rung | Trials | Median seconds | Median tokens | Median USD | Artifacts |\n| --- | --- | --- | --- | --- | --- | --- | ---: | ---: | ---: | --- |\n");
    for ((kind, length, level, fixture, task, provider), mut options) in groups {
        options.sort_by_key(|runs| ladder::rank(runs[0]));
        let reliable = options.iter().find(|runs| {
            let outcomes: Vec<_> = (1..)
                .map_while(|trial| runs.iter().find(|r| r.trial == trial).map(|r| r.pass))
                .collect();
            ladder::decide(&outcomes) == Decision::Reliable
        });
        if let Some(runs) = reliable {
            let row = runs[0];
            let pass = runs.iter().filter(|r| r.pass).count();
            let _ = writeln!(
                out,
                "| {} | {length} | {level} | {} / {} | {provider} | {} | {pass}/{} | {} | {} | {} | {} |",
                cell(&kind),
                cell(&fixture),
                cell(&task),
                cell(&model(row)),
                runs.len(),
                number(median(runs.iter().map(|r| r.seconds).collect()), 1),
                number(
                    median(
                        runs.iter()
                            .filter_map(|r| r.total_tokens().map(|v| v as f64))
                            .collect()
                    ),
                    0
                ),
                number(median(runs.iter().filter_map(|r| r.cost_usd).collect()), 5),
                link(&row.workspace, "workspace")
            );
        } else {
            let trials: usize = options.iter().map(Vec::len).sum();
            let reason = if options.iter().any(|runs| runs.len() < 2) {
                "insufficient trials"
            } else {
                "not reliable yet"
            };
            let _ = writeln!(
                out,
                "| {} | {length} | {level} | {} / {} | {provider} | {reason} | {trials} | | | | |",
                cell(&kind),
                cell(&fixture),
                cell(&task)
            );
        }
    }
    out.push_str("\n## Free model-by-task screening\n\nFixed set: six distinct S/M fixture tasks, nine default-variant free models at levels 1 to 5, four image-capable models at level 6. Expected total: 49 original attempts. Text-only models have no level-6 cell. The latest verdict for each cell is shown below; offline rows reuse the original run's usage. Legacy visual runs without a prepared failing start need fresh live challenge trials, even if their retained geometry is clear.\n\n| Model | Fixture / task | Level | Recorded pass | State | Original seconds | Input | Output | Cached | Total | USD | Artifacts |\n| --- | --- | ---: | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | --- |\n");
    let mut screening_cells = BTreeMap::<String, &Row>::new();
    for row in crate::data::effective_rows(rows)
        .into_iter()
        .filter(|r| ladder::screening_cell(r))
    {
        let previous = screening_cells.get(&row.cell());
        if previous.is_none_or(|p| {
            (p.source_generation(), p.generation) <= (row.source_generation(), row.generation)
        }) {
            screening_cells.insert(row.cell(), row);
        }
    }
    let mut screening: Vec<_> = screening_cells.into_values().collect();
    screening.sort_by(|a, b| a.model.cmp(&b.model).then(a.level.cmp(&b.level)));
    for row in &screening {
        let projects = row
            .project_paths
            .iter()
            .enumerate()
            .map(|(i, p)| link(p, &format!("project {}", i + 1)))
            .collect::<Vec<_>>()
            .join(" ");
        let _ = writeln!(
            out,
            "| {} | {} / {} | {} | {} | {} | {:.1} | {} | {} | {} | {} | {} | {} {} {} {} {} |",
            cell(&model(row)),
            cell(&row.fixture),
            cell(&row.task),
            row.level,
            row.pass,
            state(rows, row),
            row.seconds,
            number(row.tokens_in.map(|n| n as f64), 0),
            number(row.tokens_out.map(|n| n as f64), 0),
            number(row.tokens_cached.map(|n| n as f64), 0),
            number(row.total_tokens().map(|n| n as f64), 0),
            number(row.cost_usd, 5),
            link(&row.workspace, "workspace"),
            link(&row.answer_path, "answer"),
            link(&row.log_path, "log"),
            link(&row.task_path, "task/check"),
            projects
        );
    }
    let _ = writeln!(
        out,
        "\nScreening records in latest generations: {} / 49.\n",
        screening.len()
    );
    out.push_str("## Failures, invalid attempts and claims\n\nFailure classes are evidence-based heuristics, not an independent model judgment. Inspect attached logs before assigning blame. Check-class failures mean missing or invalid evaluator evidence, not model failure. Historical records remain below; an offline verdict does not erase its original row.\n\n| Fixture / task | Model | Trial / generation | State | Class | Claimed success | Evidence | Workspace |\n| --- | --- | --- | --- | --- | --- | --- | --- |\n");
    for row in rows
        .iter()
        .filter(|r| !r.pass || crate::data::attempt_invalid(rows, r))
    {
        let _ = writeln!(
            out,
            "| {} / {} | {} | {} / {} | {} | {} | {} ({}) | {} | {} {} |",
            cell(&row.fixture),
            cell(&row.task),
            cell(&model(row)),
            row.trial,
            row.generation,
            state(rows, row),
            row.failure_class.as_deref().unwrap_or("unclassified"),
            row.claimed_success,
            cell(&row.claim_evidence),
            cell(&evidence_text(rows, row)),
            link(&row.workspace, "workspace"),
            link(&row.log_path, "log")
        );
    }
    out.push_str("\n## Ground truth and operational limits\n\nContent checks use frozen baseline word times, with phrases selected from manifest notes. Unmatched words, absent beat samples and missing usable mouth/close-up detections fail as check errors. Visual checks use frozen baseline face samples as a detector-derived mouth/chin proxy; these are not hand-labelled mouth annotations. They cannot establish accuracy outside sampled close-ups.\n\nBaselines use the engine recipe, local CPU transcription and no render. Runs materialize independent inbox files and rebase asset paths; no writable hardlinks or media symlinks point to original fixtures. The harness preserves all run workspaces, including passes. Paid rungs require --allow-paid. Configured custom agent commands are refused to prevent mislabeled or accidental paid runs.\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn medians_preserve_missing_values() {
        assert_eq!(median(vec![]), None);
        assert_eq!(median(vec![5., 1., 3.]), Some(3.));
        assert_eq!(median(vec![1., 5.]), Some(3.));
        assert_eq!(number(None, 0), "not reported");
        assert!(markdown(&[]).contains("0 / 49"));
    }
    #[test]
    fn a_screening_pass_is_not_a_recommendation() {
        let mut first = crate::data::synthetic_row();
        first.screening = true;
        assert!(markdown(std::slice::from_ref(&first)).contains("insufficient trials"));
        let mut second = first.clone();
        second.trial = 2;
        second.screening = false;
        let text = markdown(&[first.clone(), second]);
        assert!(text.contains("2/2"));
        assert!(text.contains("1 / 49"));
        let mut rerun = first.clone();
        rerun.screening = false;
        rerun.generation = 1;
        assert!(markdown(&[first, rerun]).contains("1 / 49"));
    }
    #[test]
    fn rechecks_do_not_add_trials_or_spend_and_mark_invalid_cells() {
        let mut original = crate::data::synthetic_row();
        original.screening = true;
        let mut checked = original.clone();
        checked.rechecked = true;
        checked.original_run = Some(original.reference());
        checked.generation = 1;
        let text = markdown(&[original.clone(), checked.clone()]);
        assert!(text.contains("1 original model attempts + 1 offline recheck records"));
        assert!(text.contains("Original-run totals only: 2.0 seconds; 10 reported tokens"));
        assert!(text.contains("insufficient trials"));
        assert!(!text.contains("2/2"));
        assert!(text.contains("clean offline recheck"));
        let mut second = original.clone();
        second.trial = 2;
        assert!(markdown(&[original.clone(), checked.clone(), second]).contains("2/2"));
        original.invalid = true;
        let invalid = markdown(&[original, checked]);
        assert!(invalid.contains("invalid; live rerun required"));
        assert!(!invalid.contains("2/2"));
    }
    #[test]
    fn reviewed_root_cause_and_citation_replace_generic_timeout_evidence() {
        let mut original = crate::data::synthetic_row();
        original.pass = false;
        original.check_detail = "Run deadline reached after 600 seconds".into();
        original.failure_class = Some("environment".into());
        original.evidence = serde_json::json!({"failure_review":{"evidence":"Free usage limit reached","citation":"R22.log:12"}});
        let text = markdown(std::slice::from_ref(&original));
        assert!(text.contains("Free usage limit reached"));
        assert!(text.contains("Citation: R22.log:12"));
        assert!(!text.contains("Run deadline reached after 600 seconds"));
        original.invalid = true;
        original.evidence["invalid_reason"] = "Read eval-task.json before editing".into();
        let mut rechecked = original.clone();
        rechecked.rechecked = true;
        rechecked.generation = 1;
        rechecked.original_run = Some(original.reference());
        rechecked.check_detail = "Offline artifact check passed; original timeout retained".into();
        rechecked.evidence = serde_json::json!({"original_evidence":original.evidence});
        let text = evidence_text(&[original, rechecked.clone()], &rechecked);
        assert!(text.contains("Free usage limit reached"));
        assert!(text.contains("R22.log:12"));
        assert!(text.contains("Current check: Offline artifact check passed"));
        assert!(text.contains("Invalid: Read eval-task.json before editing"));
    }
}
