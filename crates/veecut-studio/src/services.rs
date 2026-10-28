use std::{collections::{HashSet, VecDeque}, path::Path, sync::mpsc, thread};

use eframe::egui;
use veecut_engine::{agent::{self, AgentKind}, config::Config, project::VideoKind, skills, workspace::Workspace};

pub(crate) enum Event {
    Catalog(Result<Option<skills::Catalog>, String>, HashSet<String>),
    Agents(Vec<(AgentKind, Option<String>)>, AgentKind),
    Line(String),
    AgentDone(Result<(), String>),
    SkillsDone(Result<(), String>),
}

pub(crate) struct Services {
    pub prompt: String,
    pub agent: AgentKind,
    agents: Vec<(AgentKind, Option<String>)>,
    pub agent_busy: bool,
    log: VecDeque<String>,
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
            let agent = config.as_ref().map(Config::default_agent).unwrap_or(AgentKind::OpenCode);
            let _ = worker.send(Event::Agents(agent::detect(), agent));
            send_catalog(&worker, &Workspace { root: root.clone() });
            let result = match config {
                Ok(config) => skills::auto_sync_if_stale(&config).transpose().map(|_| ()),
                Err(error) => Err(error.context("loading config")),
            }.map_err(|error| format!("{error:#}"));
            send_catalog(&worker, &Workspace { root });
            let _ = worker.send(Event::SkillsDone(result));
        });
        Self { prompt: String::new(), agent: AgentKind::OpenCode, agents: Vec::new(), agent_busy: false,
            log: VecDeque::new(), catalog: None, installed: HashSet::new(), query: String::new(),
            category: None, skills_busy: true, notice: None, tx, rx }
    }

    pub fn poll(&mut self) -> bool {
        let mut reload = false;
        while let Ok(event) = self.rx.try_recv() {
            match event {
                Event::Catalog(result, installed) => {
                    match result { Ok(catalog) => self.catalog = catalog, Err(error) => self.notice = Some(error) }
                    self.installed = installed;
                }
                Event::Agents(agents, selected) => { self.agents = agents; self.agent = selected; }
                Event::Line(line) => {
                    // ponytail: retain 2,000 log lines in memory; add a disk log when longer sessions need it.
                    if self.log.len() == 2_000 { self.log.pop_front(); }
                    self.log.push_back(line);
                }
                Event::AgentDone(result) => {
                    self.agent_busy = false;
                    reload = true;
                    self.notice = Some(match result { Ok(()) => "Agent finished.".into(), Err(error) => error });
                }
                Event::SkillsDone(result) => {
                    self.skills_busy = false;
                    self.notice = Some(match result { Ok(()) => "Skills ready.".into(), Err(error) => error });
                }
            }
        }
        reload
    }

    pub fn run_agent(&mut self, ws: &Workspace, project: &Path) {
        if self.agent_busy || self.prompt.trim().is_empty() { return; }
        let prompt = format!("Project: {}\n{}", project.display(), self.prompt.trim());
        self.log.push_back(format!("{}: {}", self.agent.label(), self.prompt));
        self.prompt.clear();
        self.agent_busy = true;
        let kind = self.agent;
        let root = ws.root.clone();
        let tx = self.tx.clone();
        thread::spawn(move || {
            let result = agent::run(&Workspace { root }, kind, &prompt, &mut |line| { let _ = tx.send(Event::Line(line.to_owned())); })
                .and_then(|status| if status.success() { Ok(()) } else { anyhow::bail!("agent exited with {status}") })
                .map_err(|error| format!("{error:#}"));
            let _ = tx.send(Event::AgentDone(result));
        });
    }

    pub fn agent_ui(&mut self, ui: &mut egui::Ui) -> bool {
        ui.heading("Coding agent");
        egui::ComboBox::from_id_salt("agent_kind").selected_text(self.agent.label()).show_ui(ui, |ui| {
            for (kind, version) in &self.agents {
                ui.add_enabled_ui(version.is_some(), |ui| {
                    ui.selectable_value(&mut self.agent, *kind, format!("{}{}", kind.label(), if version.is_none() { " (not installed)" } else { "" }));
                });
            }
        });
        if let Some((_, Some(version))) = self.agents.iter().find(|(kind, _)| *kind == self.agent) { ui.small(version); }
        ui.label("Agent edits the same project.json. Review its changes with Undo.");
        let mut run = false;
        ui.add_enabled_ui(!self.agent_busy, |ui| {
            for prompt in ["edit this", "tighter cuts", "bigger captions", "add a hook"] {
                if ui.button(prompt).clicked() { self.prompt = prompt.into(); run = true; }
            }
            ui.text_edit_multiline(&mut self.prompt);
            if ui.button("Ask agent").clicked() { run = true; }
        });
        if self.agent_busy { ui.spinner(); ui.label("Agent running..."); }
        ui.separator();
        egui::ScrollArea::vertical().id_salt("agent_log").stick_to_bottom(true).max_height(ui.available_height()).show(ui, |ui| {
            for line in &self.log { ui.monospace(line); }
        });
        run
    }

    pub fn skills_ui(&mut self, ui: &mut egui::Ui, ws: &Workspace, kind: VideoKind) {
        ui.heading("Video skills");
        ui.label("Community skills run code. Install only sources you trust.");
        if ui.add_enabled(!self.skills_busy, egui::Button::new("Sync now")).clicked() { self.skill_job(ws, None, "sync"); }
        if self.skills_busy { ui.spinner(); }
        if let Some(catalog) = &self.catalog { ui.small(format!("Last sync: {}", catalog.fetched_at.format("%Y-%m-%d %H:%M UTC"))); }
        ui.add(egui::TextEdit::singleline(&mut self.query).hint_text("Search skills"));
        let mut action = None;
        if let Some(catalog) = &self.catalog {
            egui::ComboBox::from_id_salt("skill_category").selected_text(self.category.as_deref().unwrap_or("All categories")).show_ui(ui, |ui| {
                ui.selectable_value(&mut self.category, None, "All categories");
                for category in &catalog.categories { ui.selectable_value(&mut self.category, Some(category.id.clone()), &category.title); }
            });
            ui.separator();
            ui.strong("Recommended for this video");
            for entry in skills::recommended(catalog, kind, 3) {
                if let Some(job) = skill_entry(ui, entry, self.installed.contains(&entry.repo), self.skills_busy) { action = Some(job); }
            }
            ui.separator();
            ui.strong("Catalog");
            for entry in skills::search(catalog, &self.query, self.category.as_deref()) {
                if let Some(job) = skill_entry(ui, entry, self.installed.contains(&entry.repo), self.skills_busy) { action = Some(job); }
            }
        } else { ui.label("No catalog yet. Sync to load available skills."); }
        if let Some((repo, action)) = action { self.skill_job(ws, Some(repo), action); }
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
                        if let Some(error) = report.error { anyhow::bail!("{}: {error}", report.repo); }
                    }
                    Ok(())
                }),
                ("remove", Some(repo)) => skills::remove(&ws, repo),
                _ => skills::sync().map(|_| ()),
            }.map_err(|error| format!("{error:#}"));
            send_catalog(&tx, &ws);
            let _ = tx.send(Event::SkillsDone(result));
        });
    }
}

fn send_catalog(tx: &mpsc::Sender<Event>, ws: &Workspace) {
    let installed = match skills::installed(ws) {
        Ok(lock) => lock.repos.into_iter().map(|entry| entry.repo).collect(),
        Err(error) => {
            let _ = tx.send(Event::SkillsDone(Err(format!("reading installed skills: {error:#}"))));
            return;
        }
    };
    let _ = tx.send(Event::Catalog(skills::load_catalog().map_err(|error| format!("{error:#}")), installed));
}

fn skill_entry(ui: &mut egui::Ui, entry: &skills::Entry, installed: bool, busy: bool) -> Option<(String, &'static str)> {
    let mut action = None;
    ui.group(|ui| {
        ui.hyperlink_to(&entry.repo, &entry.url);
        ui.small(format!("{} stars · {} · {}", entry.stars, entry.security, if entry.tested { "Tested" } else { "Untested" }));
        ui.label(&entry.description);
        ui.add_enabled_ui(!busy, |ui| {
            ui.horizontal(|ui| {
                if installed {
                    if ui.button("Update").clicked() { action = Some((entry.repo.clone(), "update")); }
                    if ui.button("Remove").clicked() { action = Some((entry.repo.clone(), "remove")); }
                } else if ui.button("Install").clicked() { action = Some((entry.repo.clone(), "install")); }
            });
        });
    });
    action
}
