use anyhow::{Result, bail};
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

/// Every rung the ladder can climb: free default and extreme variants, then each paid provider's.
fn all_rungs() -> Vec<(AgentKind, Selection)> {
    let mut rungs = vec![];
    for model in FREE_MODELS {
        rungs.push((AgentKind::OpenCode, selection(model, None)));
        let (low, high) = extremes(model);
        for effort in [low, high].into_iter().flatten() {
            rungs.push((AgentKind::OpenCode, selection(model, Some(effort))));
        }
    }
    for agent in [AgentKind::Claude, AgentKind::Codex] {
        rungs.extend(paid_candidates(agent).into_iter().map(|s| (agent, s)));
    }
    rungs
}

/// `model@effort`; `@default` for no effort. The id of one concrete rung.
pub fn rung_id(selection: &Selection) -> String {
    format!(
        "{}@{}",
        selection.model.as_deref().unwrap_or("cli-default"),
        selection.effort.as_deref().unwrap_or("default")
    )
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Pick {
    Provider(AgentKind),
    /// `effort` None selects every effort of the model.
    Rung {
        agent: AgentKind,
        model: String,
        effort: Option<String>,
    },
}

/// The `--rungs` selection. Empty selects every rung.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Rungs(Vec<Pick>);

impl Rungs {
    pub fn parse(values: &[String]) -> Result<Self> {
        let known = all_rungs();
        let mut picks = vec![];
        for value in values {
            let text = value.trim();
            let lower = text.to_ascii_lowercase();
            picks.push(match lower.as_str() {
                "free" | "opencode" => Pick::Provider(AgentKind::OpenCode),
                "claude" => Pick::Provider(AgentKind::Claude),
                "codex" => Pick::Provider(AgentKind::Codex),
                _ => {
                    let (model, effort) = match text.split_once('@') {
                        Some((model, effort)) => (model, Some(effort)),
                        None => (text, None),
                    };
                    let matches = |(_, s): &&(AgentKind, Selection)| {
                        s.model.as_deref() == Some(model)
                            && effort.is_none_or(|e| e == s.effort.as_deref().unwrap_or("default"))
                    };
                    let Some((agent, _)) = known.iter().find(matches) else {
                        let ids: Vec<_> = known.iter().map(|(_, s)| rung_id(s)).collect();
                        bail!(
                            "Unknown --rungs value {text:?}. Use providers free, opencode, claude, codex, or rung ids MODEL or MODEL@EFFORT (@default for no effort): {}",
                            ids.join(", ")
                        );
                    };
                    Pick::Rung {
                        agent: *agent,
                        model: model.into(),
                        effort: effort.map(str::to_owned),
                    }
                }
            });
        }
        Ok(Self(picks))
    }

    pub fn allows(&self, agent: AgentKind, selection: &Selection) -> bool {
        self.0.is_empty()
            || self.0.iter().any(|pick| match pick {
                Pick::Provider(a) => *a == agent,
                Pick::Rung {
                    agent: a,
                    model,
                    effort,
                } => {
                    *a == agent
                        && selection.model.as_deref() == Some(model)
                        && effort
                            .as_deref()
                            .is_none_or(|e| e == selection.effort.as_deref().unwrap_or("default"))
                }
            })
    }

    /// True when the selection names any Claude or Codex rung, which needs --allow-paid.
    pub fn selects_paid(&self) -> bool {
        self.0.iter().any(|pick| match pick {
            Pick::Provider(a) | Pick::Rung { agent: a, .. } => *a != AgentKind::OpenCode,
        })
    }

    /// Free models named by id (not merely by provider); the ladder must try these even when
    /// screening would not have picked them.
    pub fn named_free_models(&self) -> Vec<&str> {
        self.0
            .iter()
            .filter_map(|pick| match pick {
                Pick::Rung {
                    agent: AgentKind::OpenCode,
                    model,
                    ..
                } => Some(model.as_str()),
                _ => None,
            })
            .collect()
    }
}

/// A ladder rung's result. Deferred means it was not run, so it is neither pass nor fail.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cell {
    Pass,
    Fail,
    Deferred,
}

/// The free part of the ladder: each default rung in cheapest order (a pass is followed by its
/// cheaper low rung), then, with no pass, each model's high rung. Returns `(passed, deferred)`.
/// `rungs` filters what runs. A low rung selected without its default (`MODEL@low`) runs at the
/// default's position, since nothing else would ever reach it.
pub fn climb_free(
    defaults: &[Selection],
    rungs: &Rungs,
    run: &mut dyn FnMut(&Selection) -> Result<Cell>,
) -> Result<(bool, bool)> {
    let (mut pass, mut deferred) = (false, false);
    for default in defaults {
        let model = default.model.as_deref().unwrap_or_default();
        let low = extremes(model)
            .0
            .map(|effort| selection(model, Some(effort)))
            .filter(|low| rungs.allows(AgentKind::OpenCode, low));
        if rungs.allows(AgentKind::OpenCode, default) {
            match run(default)? {
                Cell::Pass => {
                    pass = true;
                    if let Some(low) = low {
                        run(&low)?;
                    }
                    break;
                }
                Cell::Deferred => deferred = true,
                Cell::Fail => {}
            }
        } else if let Some(low) = low {
            match run(&low)? {
                Cell::Pass => {
                    pass = true;
                    break;
                }
                Cell::Deferred => deferred = true,
                Cell::Fail => {}
            }
        }
    }
    if !pass {
        for default in defaults {
            let model = default.model.as_deref().unwrap_or_default();
            let Some(high) = extremes(model).1 else {
                continue;
            };
            let high = selection(model, Some(high));
            if !rungs.allows(AgentKind::OpenCode, &high) {
                continue;
            }
            match run(&high)? {
                Cell::Pass => {
                    pass = true;
                    break;
                }
                Cell::Deferred => deferred = true,
                Cell::Fail => {}
            }
        }
    }
    Ok((pass, deferred))
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

#[cfg(test)]
mod rung_tests {
    use super::*;
    fn parse(values: &[&str]) -> Result<Rungs> {
        Rungs::parse(&values.iter().map(|v| v.to_string()).collect::<Vec<_>>())
    }
    #[test]
    fn providers_and_rung_ids_select_only_what_they_name() {
        let all = Rungs::default();
        let flint = selection("opencode/flint-40b-free", None);
        assert!(all.allows(AgentKind::OpenCode, &flint));
        assert!(all.allows(AgentKind::Claude, &selection("haiku", Some("low"))));
        let paid = parse(&["Claude", "codex"]).unwrap();
        assert!(paid.selects_paid());
        assert!(!paid.allows(AgentKind::OpenCode, &flint));
        assert!(paid.allows(AgentKind::Claude, &selection("opus", Some("ultracode"))));
        assert!(paid.allows(AgentKind::Codex, &selection("gpt-6-astra", Some("high"))));
        let free = parse(&["free"]).unwrap();
        assert!(!free.selects_paid());
        assert_eq!(free, parse(&["opencode"]).unwrap());
        assert!(free.allows(AgentKind::OpenCode, &flint));
        assert!(!free.allows(AgentKind::Claude, &selection("haiku", Some("low"))));
        let exact = parse(&["opencode/flint-40b-free@max", "opus@high"]).unwrap();
        assert!(exact.allows(
            AgentKind::OpenCode,
            &selection("opencode/flint-40b-free", Some("max"))
        ));
        assert!(!exact.allows(AgentKind::OpenCode, &flint));
        assert!(exact.allows(AgentKind::Claude, &selection("opus", Some("high"))));
        assert!(!exact.allows(AgentKind::Claude, &selection("opus", Some("ultracode"))));
        assert_eq!(exact.named_free_models(), ["opencode/flint-40b-free"]);
        let whole = parse(&["opus"]).unwrap();
        assert!(whole.allows(AgentKind::Claude, &selection("opus", Some("ultracode"))));
        assert!(whole.allows(AgentKind::Claude, &selection("opus", Some("high"))));
        assert!(!whole.allows(AgentKind::Claude, &selection("sonnet", Some("medium"))));
        let plain = parse(&["opencode/flint-40b-free@default"]).unwrap();
        assert!(plain.allows(AgentKind::OpenCode, &flint));
        assert!(!plain.allows(
            AgentKind::OpenCode,
            &selection("opencode/flint-40b-free", Some("max"))
        ));
    }
    #[test]
    fn invalid_values_fail_helpfully() {
        for bad in [
            "gpt",
            "haiku@max",
            "free-ish",
            "",
            "opencode/flint-40b-free@fast",
        ] {
            let error = parse(&[bad]).unwrap_err().to_string();
            assert!(
                error.contains("--rungs")
                    && error.contains("claude")
                    && error.contains("haiku@low"),
                "{bad}: {error}"
            );
        }
    }
}

#[cfg(test)]
mod climb_tests {
    use super::*;
    const FLINT: &str = "opencode/flint-40b-free";
    const VL: &str = "opencode/nemotron-3.5-super-vl-free";
    fn schedule(
        select: &[&str],
        defaults: &[&str],
        results: &[(&str, Cell)],
    ) -> (Vec<String>, (bool, bool)) {
        let rungs =
            Rungs::parse(&select.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap();
        let defaults: Vec<_> = defaults.iter().map(|m| selection(m, None)).collect();
        let mut ran = vec![];
        let outcome = climb_free(&defaults, &rungs, &mut |s| {
            let id = rung_id(s);
            ran.push(id.clone());
            Ok(results
                .iter()
                .find(|(r, _)| *r == id)
                .map_or(Cell::Fail, |(_, c)| *c))
        })
        .unwrap();
        (ran, outcome)
    }
    #[test]
    fn explicit_low_runs_without_its_default_and_nothing_else() {
        let (ran, out) = schedule(&["opencode/flint-40b-free@low"], &[FLINT], &[]);
        assert_eq!(ran, ["opencode/flint-40b-free@low"]);
        assert_eq!(out, (false, false));
        let (ran, out) = schedule(
            &["opencode/flint-40b-free@low"],
            &[FLINT],
            &[("opencode/flint-40b-free@low", Cell::Pass)],
        );
        assert_eq!(ran, ["opencode/flint-40b-free@low"]);
        assert_eq!(out, (true, false));
        let (ran, _) = schedule(&["opencode/nemotron-3.5-super-vl-free@none"], &[VL], &[]);
        assert_eq!(ran, ["opencode/nemotron-3.5-super-vl-free@none"]);
    }
    #[test]
    fn explicit_high_runs_without_default_and_both_extremes_run_in_order() {
        let (ran, _) = schedule(&["opencode/flint-40b-free@max"], &[FLINT], &[]);
        assert_eq!(ran, ["opencode/flint-40b-free@max"]);
        let (ran, _) = schedule(
            &["opencode/flint-40b-free@max", "opencode/flint-40b-free@low"],
            &[FLINT],
            &[],
        );
        assert_eq!(
            ran,
            ["opencode/flint-40b-free@low", "opencode/flint-40b-free@max"]
        );
    }
    #[test]
    fn unfiltered_climb_keeps_cheapest_first_order() {
        let (ran, out) = schedule(
            &[],
            &[FLINT, VL],
            &[("opencode/nemotron-3.5-super-vl-free@default", Cell::Pass)],
        );
        assert_eq!(
            ran,
            [
                "opencode/flint-40b-free@default",
                "opencode/nemotron-3.5-super-vl-free@default",
                "opencode/nemotron-3.5-super-vl-free@none",
            ]
        );
        assert_eq!(out, (true, false));
        let (ran, out) = schedule(&[], &[FLINT, VL], &[]);
        assert_eq!(
            ran,
            [
                "opencode/flint-40b-free@default",
                "opencode/nemotron-3.5-super-vl-free@default",
                "opencode/flint-40b-free@max",
                "opencode/nemotron-3.5-super-vl-free@xhigh",
            ]
        );
        assert_eq!(out, (false, false));
    }
    #[test]
    fn a_provider_filter_runs_only_that_provider_and_deferrals_are_reported() {
        let (ran, out) = schedule(&["claude"], &[FLINT], &[]);
        assert!(ran.is_empty());
        assert_eq!(out, (false, false));
        let (ran, out) = schedule(
            &["free"],
            &[FLINT],
            &[("opencode/flint-40b-free@default", Cell::Deferred)],
        );
        assert_eq!(
            ran,
            [
                "opencode/flint-40b-free@default",
                "opencode/flint-40b-free@max"
            ]
        );
        assert_eq!(out.1, true);
    }
}
