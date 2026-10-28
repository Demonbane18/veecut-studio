use std::collections::BTreeMap;

use crate::data::Row;
use veecut_engine::agent::{AgentKind, Selection};

pub const FREE_MODELS: [&str; 9] = [
    "opencode/glm-5.3-flash-free",
    "opencode/deepseek-v4.1-flash-free",
    "opencode/mimo-v2.6-flash-free",
    "opencode/nemotron-3.5-super-vl-free",
    "opencode/nemotron-3-ultra-free",
    "opencode/nemotron-3.5-lightning-free",
    "opencode/flint-40b-free",
    "opencode/beam-free",
    "opencode/big-pickle",
];
pub const SCREENING: [(&str, &str, u8); 6] = [
    ("ths-s-mark-pond-31s", "run", 1),
    ("ths-s-charles-precourt", "caption-size", 2),
    ("ths-m-mark-pond", "cut-phrase", 3),
    ("ths-m-katelynn-greer", "lower-third", 4),
    ("promo-nasa-launch", "promo-line", 5),
    ("thl-m-sandy-freund", "mouth-safe", 6),
];

pub fn image_capable(model: &str) -> bool {
    FREE_MODELS[..4].contains(&model) || !FREE_MODELS.contains(&model)
}
pub fn free(agent: AgentKind, model: &str) -> bool {
    agent == AgentKind::OpenCode && FREE_MODELS.contains(&model)
}
pub fn selection(model: &str, effort: Option<&str>) -> Selection {
    Selection {
        model: Some(model.into()),
        effort: effort.map(str::to_owned),
    }
}
pub fn provider(agent: &str) -> &str {
    match agent {
        "opencode" => "free",
        "claude" => "Claude",
        "codex" => "OpenAI",
        _ => agent,
    }
}
pub fn cell(fixture: &str, task: &str, agent: AgentKind, selection: &Selection) -> String {
    format!(
        "{fixture}|{task}|{}|{}|{}",
        agent.name(),
        selection.model.as_deref().unwrap_or("cli-default"),
        selection.effort.as_deref().unwrap_or("default")
    )
}
pub fn screening_cell(row: &Row) -> bool {
    row.screening
        || (row.agent == "opencode"
            && row.effort.is_none()
            && row.trial == 1
            && FREE_MODELS.contains(&row.model.as_str())
            && SCREENING.iter().any(|(fixture, task, level)| {
                *fixture == row.fixture && *task == row.task && *level == row.level
            })
            && (row.level != 6 || image_capable(&row.model)))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Trial(u32),
    Reliable,
    Unreliable,
}

pub fn decide(outcomes: &[bool]) -> Decision {
    match outcomes {
        [] => Decision::Trial(1),
        [_] => Decision::Trial(2),
        [true, true, ..] => Decision::Reliable,
        [false, false, ..] => Decision::Unreliable,
        [_, _] => Decision::Trial(3),
        rest => {
            if rest.iter().take(3).filter(|v| **v).count() >= 2 {
                Decision::Reliable
            } else {
                Decision::Unreliable
            }
        }
    }
}

pub fn generation(rows: &[Row], key: &str, rerun: bool) -> u32 {
    if rerun {
        return rows
            .iter()
            .filter(|r| r.cell() == key)
            .map(|r| r.generation)
            .max()
            .map_or(0, |g| g + 1);
    }
    rows.iter()
        .filter(|r| !r.rechecked && r.cell() == key)
        .map(|r| r.source_generation())
        .max()
        .unwrap_or(0)
}
pub fn outcomes(rows: &[Row], key: &str, generation: u32) -> Vec<bool> {
    let trials: BTreeMap<_, _> = crate::data::effective_rows(rows)
        .into_iter()
        .filter(|r| {
            r.cell() == key
                && r.source_generation() == generation
                && !crate::data::attempt_invalid(rows, r)
        })
        .map(|r| (r.trial, r.pass))
        .collect();
    (1..)
        .map_while(|trial| trials.get(&trial).copied())
        .collect()
}

pub fn free_candidates(rows: &[Row], visual: bool) -> Vec<Selection> {
    let valid: Vec<_> = crate::data::current_rows(rows)
        .into_iter()
        .filter(|r| !crate::data::attempt_invalid(rows, r))
        .collect();
    let mut others: Vec<_> = FREE_MODELS[2..]
        .iter()
        .copied()
        .filter(|m| {
            (!visual || image_capable(m))
                && valid.iter().any(|r| screening_cell(r) && r.model == *m)
        })
        .collect();
    others.sort_by(|a, b| {
        let score = |model: &str| {
            let selected: Vec<_> = valid
                .iter()
                .filter(|r| screening_cell(r) && r.model == model)
                .collect();
            (
                selected.iter().filter(|r| r.pass).count() as f64 / selected.len() as f64,
                selected.iter().map(|r| r.seconds).sum::<f64>() / selected.len() as f64,
            )
        };
        let x = score(a);
        let y = score(b);
        y.0.total_cmp(&x.0).then(x.1.total_cmp(&y.1)).then(a.cmp(b))
    });
    let mut models = vec![
        selection(FREE_MODELS[0], None),
        selection(FREE_MODELS[1], None),
    ];
    if let Some(other) = others.first() {
        models.push(selection(other, None));
    }
    models
}

pub fn extremes(model: &str) -> (Option<&'static str>, Option<&'static str>) {
    match model {
        "opencode/glm-5.3-flash-free"
        | "opencode/deepseek-v4.1-flash-free"
        | "opencode/flint-40b-free"
        | "opencode/beam-free" => (Some("low"), Some("max")),
        "opencode/nemotron-3.5-super-vl-free" => (Some("none"), Some("xhigh")),
        _ => (None, None),
    }
}

pub fn paid_candidates(agent: AgentKind) -> Vec<Selection> {
    match agent {
        AgentKind::Claude => vec![
            selection("haiku", Some("low")),
            selection("sonnet", Some("medium")),
            selection("opus", Some("high")),
            selection("fable", Some("high")),
            selection("opus", Some("ultracode")),
        ],
        AgentKind::Codex => vec![
            selection("gpt-5.6-luna", Some("low")),
            selection("gpt-reserve", Some("low")),
            selection("gpt-6-astra", Some("medium")),
            selection("gpt-6-astra", Some("high")),
            selection("gpt-6-astra", Some("xhigh")),
        ],
        _ => vec![],
    }
}

pub fn rank(row: &Row) -> usize {
    if row.agent == "opencode" {
        let base = FREE_MODELS
            .iter()
            .position(|m| *m == row.model)
            .unwrap_or(99)
            * 4;
        base + match row.effort.as_deref() {
            Some("low" | "none") => 0,
            None => 1,
            Some("high") => 2,
            _ => 3,
        }
    } else {
        let agent = if row.agent == "claude" {
            AgentKind::Claude
        } else {
            AgentKind::Codex
        };
        paid_candidates(agent)
            .iter()
            .position(|s| s.model.as_deref() == Some(row.model.as_str()) && s.effort == row.effort)
            .unwrap_or(99)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn trials_require_two_passes_and_split_tiebreak() {
        assert_eq!(decide(&[]), Decision::Trial(1));
        assert_eq!(decide(&[true]), Decision::Trial(2));
        assert_eq!(decide(&[true, true]), Decision::Reliable);
        assert_eq!(decide(&[false, false]), Decision::Unreliable);
        assert_eq!(decide(&[true, false]), Decision::Trial(3));
        assert_eq!(decide(&[true, false, true]), Decision::Reliable);
        assert_eq!(decide(&[false, true, false]), Decision::Unreliable);
    }
    #[test]
    fn screening_is_exactly_49_cells() {
        let count = SCREENING
            .iter()
            .map(|(_, _, level)| {
                FREE_MODELS
                    .iter()
                    .filter(|m| *level != 6 || image_capable(m))
                    .count()
            })
            .sum::<usize>();
        assert_eq!(count, 49);
        assert!(!free(
            AgentKind::OpenCode,
            "some-provider/free-looking-paid-model"
        ));
        assert!(!free(AgentKind::Claude, FREE_MODELS[0]));
    }
    #[test]
    fn resume_uses_completed_trials_and_latest_generation() {
        let first = crate::data::synthetic_row();
        let key = first.cell();
        let mut second = first.clone();
        second.trial = 2;
        second.pass = false;
        let mut rows = vec![first.clone(), second];
        assert_eq!(generation(&rows, &key, false), 0);
        assert_eq!(generation(&rows, &key, true), 1);
        assert_eq!(decide(&outcomes(&rows, &key, 0)), Decision::Trial(3));
        let mut rerun = first;
        rerun.generation = 1;
        rerun.pass = false;
        rows.push(rerun);
        assert_eq!(generation(&rows, &key, false), 1);
        assert_eq!(outcomes(&rows, &key, 1), vec![false]);
        let tmp = tempfile::tempdir().unwrap();
        for row in &rows {
            crate::data::append_row(tmp.path(), row).unwrap();
        }
        let loaded = crate::data::rows(tmp.path()).unwrap();
        assert_eq!(loaded.len(), 3);
        assert_eq!(outcomes(&loaded, &key, 1), vec![false]);
    }
}
