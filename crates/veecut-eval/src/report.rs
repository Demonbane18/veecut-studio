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

pub fn markdown(rows: &[Row]) -> String {
    let mut out = String::from(
        "# VeeCut model evaluation\n\nRecommendations require two passing trials (third trial after a split). Single screening passes are not reliability recommendations. Failed workspaces are retained. Token input excludes cached tokens; totals include each once. Missing usage remains unreported. Costs are CLI-reported values, not inferred API prices.\n\n",
    );
    let mut latest = BTreeMap::<String, u32>::new();
    for row in rows {
        latest
            .entry(row.cell())
            .and_modify(|g| *g = (*g).max(row.generation))
            .or_insert(row.generation);
    }
    let active: Vec<_> = rows
        .iter()
        .filter(|r| latest.get(&r.cell()) == Some(&r.generation))
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
    out.push_str("\n## Free model-by-task screening\n\nFixed set: six distinct S/M fixture tasks, nine default-variant free models at levels 1 to 5, four image-capable models at level 6. Expected total: 49 rows. Text-only models have no level-6 cell. The table lists actual records only.\n\n| Model | Fixture / task | Level | Pass | Seconds | Input | Output | Cached | Total | USD | Artifacts |\n| --- | --- | ---: | --- | ---: | ---: | ---: | ---: | ---: | ---: | --- |\n");
    let mut screening_cells = BTreeMap::<String, &Row>::new();
    for row in rows.iter().filter(|r| ladder::screening_cell(r)) {
        let previous = screening_cells.get(&row.cell());
        if previous.is_none_or(|p| p.generation <= row.generation) {
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
            "| {} | {} / {} | {} | {} | {:.1} | {} | {} | {} | {} | {} | {} {} {} {} {} |",
            cell(&model(row)),
            cell(&row.fixture),
            cell(&row.task),
            row.level,
            row.pass,
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
    out.push_str("## Failures and claims\n\nFailure classes are evidence-based heuristics, not an independent model judgment. Inspect attached logs before assigning blame. Check-class failures mean missing or invalid evaluator evidence, not model failure.\n\n| Fixture / task | Model | Trial / generation | Class | Claimed success | Evidence | Workspace |\n| --- | --- | --- | --- | --- | --- | --- |\n");
    for row in rows.iter().filter(|r| !r.pass) {
        let _ = writeln!(
            out,
            "| {} / {} | {} | {} / {} | {} | {} ({}) | {} | {} {} |",
            cell(&row.fixture),
            cell(&row.task),
            cell(&model(row)),
            row.trial,
            row.generation,
            row.failure_class.as_deref().unwrap_or("unclassified"),
            row.claimed_success,
            cell(&row.claim_evidence),
            cell(&row.check_detail),
            link(&row.workspace, "workspace"),
            link(&row.log_path, "log")
        );
    }
    out.push_str("\n## Ground truth and operational limits\n\nContent checks use frozen baseline word times, with phrases selected from manifest notes. Unmatched words, absent beat samples and missing usable mouth/close-up detections fail as check errors. Visual checks use frozen baseline face samples as a detector-derived mouth/chin proxy; these are not hand-labelled mouth annotations. They cannot establish accuracy outside sampled close-ups.\n\nBaselines use the engine recipe, local CPU transcription and no render. Runs materialize independent inbox files and rebase asset paths; no writable hardlinks or media symlinks point to original fixtures. The harness preserves all run workspaces, including passes. Paid rungs require --allow-paid; the orchestrator must enforce weekly-plan budgets (15% maximum per plan, stop below 72% remaining) and request ultracode comparison runs explicitly. Configured custom agent commands are refused to prevent mislabeled or accidental paid runs.\n");
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
}
