use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::Path,
    sync::mpsc,
    thread,
    time::Instant,
};

use eframe::egui;
use veecut_engine::{
    agent::{self, AgentKind, AgentStatus, ModelChoice, Selection, Usage},
    config::Config,
    project::VideoKind,
    skills,
    workspace::Workspace,
};

pub(crate) enum Event {
    Catalog(Result<Option<skills::Catalog>, String>, HashSet<String>),
    Agents(Vec<AgentStatus>, Option<AgentKind>),
    Output(agent::Output),
    Models(AgentKind, Result<Vec<ModelChoice>, String>),
    Picks(Picks),
    AgentDone(Result<(), String>),
    AgentStopped,
    SkillsDone(Result<(), String>),
}

#[derive(Default)]
struct AgentTranscript {
    note: String,
    answer: String,
    result: Option<String>,
    /// The latest cumulative usage the agent reported during this run.
    usage: Option<Usage>,
    /// Time, tokens and cost of the finished run.
    summary: Option<String>,
    log: VecDeque<String>,
}

impl AgentTranscript {
    fn record(&mut self, output: agent::Output) {
        match output {
            agent::Output::Answer(text) => {
                if !self.answer.is_empty() {
                    self.answer.push('\n');
                }
                self.answer.push_str(&text);
            }
            agent::Output::Log(line) => {
                // ponytail: retain 2,000 raw records; use a disk log if longer tool history matters.
                if self.log.len() == 2_000 {
                    self.log.pop_front();
                }
                self.log.push_back(line);
            }
            agent::Output::Usage(usage) => self.usage = Some(usage),
        }
    }
}

/// The model and effort chosen for each agent, as kept in config.toml.
#[derive(Default)]
pub(crate) struct Picks(HashMap<AgentKind, Selection>);

impl Picks {
    fn from_config(config: &Config) -> Self {
        Self(
            AgentKind::ALL
                .into_iter()
                .map(|kind| (kind, config.agent_selection(kind)))
                .collect(),
        )
    }

    fn get(&self, kind: AgentKind) -> Selection {
        self.0.get(&kind).cloned().unwrap_or_default()
    }

    /// Remembers `new` for `kind`. A new model drops an effort it does not offer.
    fn set(&mut self, kind: AgentKind, mut new: Selection) -> Selection {
        if new.model != self.get(kind).model
            && let Some(effort) = &new.effort
            && !agent::efforts(kind, new.model.as_deref())
                .iter()
                .any(|choice| choice.id == effort)
        {
            new.effort = None;
        }
        self.0.insert(kind, new.clone());
        new
    }

    fn save(&self, kind: AgentKind) -> anyhow::Result<()> {
        let mut config = Config::load()?;
        config.set_agent_selection(kind, self.get(kind));
        config.save()
    }
}

pub(crate) struct Services {
    pub prompt: String,
    pub agent: AgentKind,
    picks: Picks,
    /// Known and discovered models per agent.
    models: HashMap<AgentKind, Vec<ModelChoice>>,
    model_errors: HashMap<AgentKind, String>,
    custom_model: String,
    custom_error: Option<&'static str>,
    agents: Vec<AgentStatus>,
    agents_busy: bool,
    pub agent_busy: bool,
    /// Ends the running agent and everything it started.
    agent_stop: Option<agent::Stop>,
    agent_started: Option<Instant>,
    transcript: AgentTranscript,
    catalog: Option<skills::Catalog>,
    installed: HashSet<String>,
    query: String,
    category: Option<String>,
    skills_busy: bool,
    pub notice: Option<String>,
    tx: mpsc::Sender<Event>,
    rx: mpsc::Receiver<Event>,
}

impl Services {
    pub fn new(root: &Path) -> Self {
        let (tx, rx) = mpsc::channel();
        let worker = tx.clone();
        let root = root.to_owned();
        thread::spawn(move || {
            let config = Config::load();
            let agent = config
                .as_ref()
                .map(Config::default_agent)
                .unwrap_or(AgentKind::OpenCode);
            if let Ok(config) = &config {
                let _ = worker.send(Event::Picks(Picks::from_config(config)));
            }
            let _ = worker.send(Event::Agents(agent::detect(), Some(agent)));
            send_catalog(&worker, &Workspace { root: root.clone() });
            let result = match config {
                Ok(config) => skills::auto_sync_if_stale(&config).transpose().map(|_| ()),
                Err(error) => Err(error.context("loading config")),
            }
            .map_err(|error| format!("{error:#}"));
            send_catalog(&worker, &Workspace { root });
            let _ = worker.send(Event::SkillsDone(result));
        });
        for kind in AgentKind::ALL {
            fetch_models(&tx, kind);
        }
        Self {
            prompt: String::new(),
            agent: AgentKind::OpenCode,
            picks: Picks::default(),
            models: AgentKind::ALL
                .into_iter()
                .map(|kind| (kind, agent::known_models(kind)))
                .collect(),
            model_errors: HashMap::new(),
            custom_model: String::new(),
            custom_error: None,
            agents: Vec::new(),
            agents_busy: true,
            agent_busy: false,
            agent_stop: None,
            agent_started: None,
            transcript: AgentTranscript::default(),
            catalog: None,
            installed: HashSet::new(),
            query: String::new(),
            category: None,
            skills_busy: true,
            notice: None,
            tx,
            rx,
        }
    }

    pub fn poll(&mut self) -> bool {
        let mut reload = false;
        while let Ok(event) = self.rx.try_recv() {
            match event {
                Event::Catalog(result, installed) => {
                    match result {
                        Ok(catalog) => self.catalog = catalog,
                        Err(error) => self.notice = Some(error),
                    }
                    self.installed = installed;
                }
                Event::Agents(agents, selected) => {
                    self.agents = agents;
                    self.agents_busy = false;
                    if let Some(selected) = selected {
                        self.agent = selected;
                    }
                }
                Event::Picks(picks) => self.picks = picks,
                Event::Models(kind, result) => match result {
                    Ok(models) => {
                        self.models.insert(kind, models);
                        self.model_errors.remove(&kind);
                    }
                    Err(error) => {
                        self.model_errors.insert(kind, error);
                    }
                },
                Event::Output(output) => self.transcript.record(output),
                Event::AgentDone(result) => {
                    let usage = self.agent_finished();
                    reload = true;
                    let result = match result {
                        Ok(()) => "Agent finished.".into(),
                        Err(error) => error,
                    };
                    self.transcript.result = Some(result.clone());
                    self.notice = Some(format!("{result} {usage}"));
                }
                Event::AgentStopped => {
                    let usage = self.agent_finished();
                    reload = true;
                    self.transcript.result = Some("Agent stopped.".into());
                    self.notice = Some(format!("Agent stopped. {usage}"));
                }
                Event::SkillsDone(result) => {
                    self.skills_busy = false;
                    self.notice = Some(match result {
                        Ok(()) => "Skills ready.".into(),
                        Err(error) => error,
                    });
                }
            }
        }
        reload
    }

    fn refresh_agents(&mut self) {
        self.agents_busy = true;
        fetch_models(&self.tx, self.agent);
        let tx = self.tx.clone();
        thread::spawn(move || {
            let _ = tx.send(Event::Agents(agent::detect(), None));
        });
    }

    /// Ends the run's bookkeeping and returns its time, tokens and cost.
    fn agent_finished(&mut self) -> String {
        self.agent_busy = false;
        self.agent_stop = None;
        let seconds = self
            .agent_started
            .take()
            .map_or(0.0, |start| start.elapsed().as_secs_f64());
        let line = usage_line(self.transcript.usage.as_ref(), seconds);
        self.transcript.summary = Some(line.clone());
        line
    }

    pub fn run_agent(&mut self, ws: &Workspace, project: &Path) {
        if self.agent_busy || self.prompt.trim().is_empty() {
            return;
        }
        let Some(status) = self.agents.iter().find(|status| status.kind == self.agent) else {
            self.notice = Some("Agent readiness is still being checked.".into());
            return;
        };
        if !status.can_run() {
            self.notice = Some(format!(
                "{}: {} {}",
                self.agent.label(),
                status.detail,
                status.fix.as_deref().unwrap_or("")
            ));
            return;
        }
        let prompt = agent::studio_prompt(project, self.prompt.trim());
        let selection = self.picks.get(self.agent);
        self.transcript = AgentTranscript {
            note: self.prompt.trim().into(),
            ..Default::default()
        };
        self.prompt.clear();
        self.agent_busy = true;
        self.agent_started = Some(Instant::now());
        let stop = agent::Stop::default();
        self.agent_stop = Some(stop.clone());
        let kind = self.agent;
        let root = ws.root.clone();
        let tx = self.tx.clone();
        thread::spawn(move || {
            let done = match agent::run(
                &Workspace { root },
                kind,
                &prompt,
                &selection,
                &stop,
                &mut |output| {
                    let _ = tx.send(Event::Output(output));
                },
            ) {
                Ok(agent::Outcome::Stopped) => Event::AgentStopped,
                Ok(agent::Outcome::Finished(status)) if status.success() => {
                    Event::AgentDone(Ok(()))
                }
                Ok(agent::Outcome::Finished(status)) => {
                    Event::AgentDone(Err(format!("agent exited with {status}")))
                }
                Err(error) => Event::AgentDone(Err(format!("{error:#}"))),
            };
            let _ = tx.send(done);
        });
    }

    /// Ends the running agent and its helpers, and returns once they are killed. The worker
    /// then reports `Event::AgentStopped`. Does nothing when no agent runs.
    pub fn stop_agent(&self) {
        if let Some(stop) = &self.agent_stop {
            stop.stop();
        }
    }

    /// False while the agent list is still being checked.
    pub fn agents_ready(&self) -> bool {
        !self.agents_busy
    }

    /// Model and effort pickers for the chosen agent; each change is saved to config.toml.
    fn pick_ui(&mut self, ui: &mut egui::Ui, custom_command: bool) {
        let kind = self.agent;
        let mut selection = self.picks.get(kind);
        let before = selection.clone();
        if custom_command {
            ui.small(
                "Custom command in config.toml runs as written. Model and effort are not added.",
            );
        }
        let models = self.models.get(&kind).map_or(&[][..], Vec::as_slice);
        let efforts = agent::efforts(kind, selection.model.as_deref());
        let enabled = !custom_command && !self.agent_busy;
        ui.add_enabled_ui(enabled, |ui| {
            let custom = selection
                .model
                .as_deref()
                .is_some_and(|id| !models.iter().any(|choice| choice.id == id));
            ui.horizontal(|ui| {
                ui.label("Model");
                egui::ComboBox::from_id_salt("agent_model")
                    .width(220.0)
                    .selected_text(match &selection.model {
                        Some(id) if custom => format!("{id} (custom)"),
                        Some(id) => id.clone(),
                        None => "Agent default".into(),
                    })
                    .show_ui(ui, |ui| {
                        // OpenCode always runs a model: an empty choice would reset to the free one.
                        if kind != AgentKind::OpenCode {
                            ui.selectable_value(&mut selection.model, None, "Agent default");
                        }
                        for choice in models {
                            let label = if choice.note.is_empty() {
                                choice.id.clone()
                            } else {
                                format!("{} · {}", choice.id, choice.note)
                            };
                            ui.selectable_value(
                                &mut selection.model,
                                Some(choice.id.clone()),
                                label,
                            );
                        }
                    });
            });
            ui.horizontal(|ui| {
                let edit = ui.add(
                    egui::TextEdit::singleline(&mut self.custom_model)
                        .hint_text("Custom model id")
                        .desired_width(160.0),
                );
                let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if ui.button("Use").clicked() || enter {
                    match custom_model_id(&self.custom_model) {
                        Ok(id) => {
                            selection.model = Some(id);
                            self.custom_model.clear();
                            self.custom_error = None;
                        }
                        Err(error) => self.custom_error = Some(error),
                    }
                }
            });
            if let Some(error) = self.custom_error {
                ui.colored_label(egui::Color32::LIGHT_RED, error);
            }
            if efforts.is_empty() {
                ui.small(format!("{} has no effort setting.", kind.label()));
                return;
            }
            ui.horizontal(|ui| {
                ui.label("Effort");
                egui::ComboBox::from_id_salt("agent_effort")
                    .width(220.0)
                    .selected_text(selection.effort.as_deref().unwrap_or("Agent default"))
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut selection.effort, None, "Agent default");
                        for choice in &efforts {
                            let mut label =
                                egui::RichText::new(format!("{} · {}", choice.id, choice.note));
                            if choice.id == agent::ULTRACODE {
                                label = label.color(egui::Color32::from_rgb(255, 170, 60));
                            }
                            ui.selectable_value(
                                &mut selection.effort,
                                Some(choice.id.to_string()),
                                label,
                            );
                        }
                    });
            });
            if let Some(choice) = efforts.iter().find(|choice| {
                selection.effort.as_deref() == Some(choice.id) && choice.id == agent::ULTRACODE
            }) {
                ui.colored_label(egui::Color32::from_rgb(255, 170, 60), choice.note);
            }
        });
        if let Some(error) = self.model_errors.get(&kind)
            && !custom_command
        {
            ui.small(format!("Showing built-in models only: {error}"));
        }
        if selection != before {
            self.picks.set(kind, selection);
            if let Err(error) = self.picks.save(kind) {
                self.notice = Some(format!("Could not save {} choice: {error:#}", kind.label()));
            }
        }
    }

    pub fn agent_ui(&mut self, ui: &mut egui::Ui) -> bool {
        ui.heading("Coding agent");
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("agent_kind")
                .selected_text(self.agent.label())
                .show_ui(ui, |ui| {
                    for status in &self.agents {
                        ui.add_enabled_ui(status.can_run() && !self.agent_busy, |ui| {
                            ui.selectable_value(&mut self.agent, status.kind, status.kind.label());
                        });
                        ui.small(&status.detail);
                        if let Some(fix) = &status.fix {
                            ui.small(format!("Fix: {fix}"));
                        }
                        ui.separator();
                    }
                });
            if ui
                .add_enabled(
                    !self.agents_busy && !self.agent_busy,
                    egui::Button::new("Refresh"),
                )
                .on_hover_text("Check installation and sign-in again after logging in.")
                .clicked()
            {
                self.refresh_agents();
            }
            if self.agents_busy {
                ui.spinner();
            }
        });
        let ready = self.agents.iter().find(|status| status.kind == self.agent);
        if let Some(status) = ready {
            ui.label(&status.detail);
            if let Some(fix) = &status.fix {
                ui.label(format!("Fix: {fix}"));
            }
        }
        let custom_command = ready.is_some_and(|status| status.command_override);
        let can_run = ready.is_some_and(AgentStatus::can_run);
        self.pick_ui(ui, custom_command);
        ui.label("Agent edits the same project.json. Review its changes with Undo.");
        let mut run = false;
        ui.add_enabled_ui(!self.agent_busy && can_run, |ui| {
            for prompt in ["edit this", "tighter cuts", "bigger captions", "add a hook"] {
                if ui.button(prompt).clicked() {
                    self.prompt = prompt.into();
                    run = true;
                }
            }
            ui.text_edit_multiline(&mut self.prompt);
            if ui.button("Ask agent").clicked() {
                run = true;
            }
        });
        if self.agent_busy {
            ui.horizontal(|ui| {
                ui.spinner();
                let seconds = self
                    .agent_started
                    .map_or(0, |start| start.elapsed().as_secs());
                ui.label(format!(
                    "Agent running... {}:{:02}",
                    seconds / 60,
                    seconds % 60
                ));
                if ui.button("Stop").clicked() {
                    self.stop_agent();
                }
            });
        }
        ui.separator();
        egui::ScrollArea::vertical()
            .id_salt("agent_conversation")
            .max_height(ui.available_height())
            .show(ui, |ui| {
                if !self.transcript.note.is_empty() {
                    ui.strong("Your note");
                    ui.label(&self.transcript.note);
                }
                if !self.transcript.answer.is_empty() {
                    ui.strong("Assistant reply");
                    ui.label(&self.transcript.answer);
                }
                if let Some(result) = &self.transcript.result {
                    ui.label(result);
                }
                if let Some(summary) = &self.transcript.summary {
                    ui.small(summary);
                }
                egui::CollapsingHeader::new(format!(
                    "Tool output and logs ({})",
                    self.transcript.log.len()
                ))
                .id_salt("agent_raw_log")
                .default_open(false)
                .show(ui, |ui| {
                    for line in &self.transcript.log {
                        ui.monospace(line);
                    }
                });
            });
        run
    }

    pub fn skills_ui(&mut self, ui: &mut egui::Ui, ws: &Workspace, kind: VideoKind) {
        ui.heading("Video skills");
        ui.label("Community skills run code. Install only sources you trust.");
        if ui
            .add_enabled(!self.skills_busy, egui::Button::new("Sync now"))
            .clicked()
        {
            self.skill_job(ws, None, "sync");
        }
        if self.skills_busy {
            ui.spinner();
        }
        if let Some(catalog) = &self.catalog {
            ui.small(format!(
                "Last sync: {}",
                catalog.fetched_at.format("%Y-%m-%d %H:%M UTC")
            ));
        }
        ui.add(egui::TextEdit::singleline(&mut self.query).hint_text("Search skills"));
        let mut action = None;
        if let Some(catalog) = &self.catalog {
            egui::ComboBox::from_id_salt("skill_category")
                .selected_text(self.category.as_deref().unwrap_or("All categories"))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.category, None, "All categories");
                    for category in &catalog.categories {
                        ui.selectable_value(
                            &mut self.category,
                            Some(category.id.clone()),
                            &category.title,
                        );
                    }
                });
            ui.separator();
            ui.strong("Recommended for this video");
            for entry in skills::recommended(catalog, kind, 3) {
                if let Some(job) = skill_entry(
                    ui,
                    entry,
                    self.installed.contains(&entry.repo),
                    self.skills_busy,
                ) {
                    action = Some(job);
                }
            }
            ui.separator();
            ui.strong("Catalog");
            for entry in skills::search(catalog, &self.query, self.category.as_deref()) {
                if let Some(job) = skill_entry(
                    ui,
                    entry,
                    self.installed.contains(&entry.repo),
                    self.skills_busy,
                ) {
                    action = Some(job);
                }
            }
        } else {
            ui.label("No catalog yet. Sync to load available skills.");
        }
        if let Some((repo, action)) = action {
            self.skill_job(ws, Some(repo), action);
        }
    }

    fn skill_job(&mut self, ws: &Workspace, repo: Option<String>, action: &'static str) {
        self.skills_busy = true;
        let root = ws.root.clone();
        let tx = self.tx.clone();
        thread::spawn(move || {
            let ws = Workspace { root };
            let result = match (action, repo.as_deref()) {
                ("install", Some(repo)) => skills::install(&ws, repo, false).map(|_| ()),
                ("update", Some(repo)) => skills::update(&ws, Some(repo)).and_then(|reports| {
                    for report in reports {
                        if let Some(error) = report.error {
                            anyhow::bail!("{}: {error}", report.repo);
                        }
                    }
                    Ok(())
                }),
                ("remove", Some(repo)) => skills::remove(&ws, repo),
                _ => skills::sync().map(|_| ()),
            }
            .map_err(|error| format!("{error:#}"));
            send_catalog(&tx, &ws);
            let _ = tx.send(Event::SkillsDone(result));
        });
    }
}

fn send_catalog(tx: &mpsc::Sender<Event>, ws: &Workspace) {
    let installed = match skills::installed(ws) {
        Ok(lock) => lock.repos.into_iter().map(|entry| entry.repo).collect(),
        Err(error) => {
            let _ = tx.send(Event::SkillsDone(Err(format!(
                "reading installed skills: {error:#}"
            ))));
            return;
        }
    };
    let _ = tx.send(Event::Catalog(
        skills::load_catalog().map_err(|error| format!("{error:#}")),
        installed,
    ));
}

fn fetch_models(tx: &mpsc::Sender<Event>, kind: AgentKind) {
    let tx = tx.clone();
    thread::spawn(move || {
        let _ = tx.send(Event::Models(
            kind,
            agent::models(kind).map_err(|error| format!("{error:#}")),
        ));
    });
}

/// "42 s, 61k tokens, free"; just the time when the agent reported no usage.
fn usage_line(usage: Option<&Usage>, seconds: f64) -> String {
    match usage {
        Some(usage) => usage.summary(seconds),
        None => format!("{seconds:.0} s"),
    }
}

fn custom_model_id(text: &str) -> Result<String, &'static str> {
    let id = text.trim();
    if id.is_empty() {
        Err("Type a model id.")
    } else if id.starts_with('-') || id.chars().any(|c| c.is_whitespace() || c.is_control()) {
        Err("A model id has no spaces and does not start with -.")
    } else {
        Ok(id.to_string())
    }
}

fn skill_entry(
    ui: &mut egui::Ui,
    entry: &skills::Entry,
    installed: bool,
    busy: bool,
) -> Option<(String, &'static str)> {
    let mut action = None;
    ui.group(|ui| {
        ui.hyperlink_to(&entry.repo, &entry.url);
        ui.small(format!(
            "{} stars · {} · {}",
            entry.stars,
            entry.security,
            if entry.tested { "Tested" } else { "Untested" }
        ));
        ui.label(&entry.description);
        ui.add_enabled_ui(!busy, |ui| {
            ui.horizontal(|ui| {
                if installed {
                    if ui.button("Update").clicked() {
                        action = Some((entry.repo.clone(), "update"));
                    }
                    if ui.button("Remove").clicked() {
                        action = Some((entry.repo.clone(), "remove"));
                    }
                } else if ui.button("Install").clicked() {
                    action = Some((entry.repo.clone(), "install"));
                }
            });
        });
    });
    action
}

impl Drop for Services {
    /// Closing the editor must not leave an agent running unattended.
    fn drop(&mut self) {
        self.stop_agent();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_flood_cannot_evict_note_reply_or_run_result() {
        let mut transcript = AgentTranscript {
            note: "Make captions bigger.".into(),
            ..Default::default()
        };
        transcript.record(agent::Output::Answer("Updated captions.".into()));
        for index in 0..2_501 {
            transcript.record(agent::Output::Log(format!("tool result {index}")));
        }
        transcript.result = Some("stopped".into());
        assert_eq!(transcript.note, "Make captions bigger.");
        assert_eq!(transcript.answer, "Updated captions.");
        assert_eq!(transcript.log.len(), 2_000);
        assert_eq!(transcript.log.front().unwrap(), "tool result 501");
        assert_eq!(transcript.result.as_deref(), Some("stopped"));
        transcript.record(agent::Output::Answer("Review in Studio.".into()));
        assert_eq!(transcript.answer, "Updated captions.\nReview in Studio.");
    }

    #[test]
    fn latest_usage_snapshot_wins_and_next_run_starts_clean() {
        let mut transcript = AgentTranscript::default();
        for tokens in [10, 25] {
            transcript.record(agent::Output::Usage(Usage {
                output_tokens: tokens,
                ..Default::default()
            }));
        }
        assert_eq!(transcript.usage.as_ref().unwrap().output_tokens, 25);
        let next = AgentTranscript {
            note: "again".into(),
            ..Default::default()
        };
        assert!(next.usage.is_none() && next.summary.is_none());
        assert_eq!(usage_line(None, 41.6), "42 s");
    }

    #[test]
    fn picks_follow_config_and_stay_per_agent() {
        let mut config = Config::default();
        let saved = Selection {
            model: Some("opus".into()),
            effort: Some("high".into()),
        };
        config.set_agent_selection(AgentKind::Claude, saved.clone());
        let mut picks = Picks::from_config(&config);
        assert_eq!(picks.get(AgentKind::Claude), saved);
        assert_eq!(picks.get(AgentKind::Codex), Selection::default());
        assert_eq!(
            picks.get(AgentKind::OpenCode).model.as_deref(),
            Some("opencode/glm-5.3-flash-free")
        );
        // A model change keeps an effort the model offers.
        let kept = picks.set(
            AgentKind::Claude,
            Selection {
                model: Some("sonnet".into()),
                ..saved
            },
        );
        assert_eq!(kept.effort.as_deref(), Some("high"));
        // Gemini has no effort control, so a model change drops the effort.
        let dropped = picks.set(
            AgentKind::Gemini,
            Selection {
                model: Some("gemini-custom".into()),
                effort: Some("high".into()),
            },
        );
        assert_eq!(dropped.effort, None);
        assert_eq!(picks.get(AgentKind::Claude), kept);
    }

    #[test]
    fn custom_model_ids_are_checked() {
        assert_eq!(
            custom_model_id("  acme/model-1 "),
            Ok("acme/model-1".into())
        );
        for bad in ["", "  ", "-x", "two words"] {
            assert!(custom_model_id(bad).is_err(), "{bad:?}");
        }
    }
}
