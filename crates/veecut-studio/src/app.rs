use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant, SystemTime},
};

use anyhow::{Context, Result};
use eframe::egui::{self, Color32, Key};
use serde_json::json;
use veecut_engine::{
    media,
    ops::{self, Op},
    project::*,
    recipes::{self, EditReport, EditRequest},
    render::{self, Quality, RenderOptions},
    workspace::Workspace,
};

use crate::{
    StudioOptions,
    capture::{self, Capture},
    inspector,
    playback::{self, Playback},
    services::Services,
    timeline::Timeline,
};

#[path = "documentary.rs"]
mod documentary;
#[path = "footage_panel.rs"]
mod footage_panel;

// Unpinned review records stay off the timeline and survive ripple cuts.
pub(crate) const AUTO_REVIEW_AT: f64 = -1.0;
pub(crate) const AUTO_REVIEW_PREFIX: &str = "Auto-edit review: ";

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Media,
    Footage,
    Sources,
    Text,
    Captions,
    LookAudio,
    Skills,
    Agent,
}

struct Document {
    project: Project,
    path: PathBuf,
    modified: Option<SystemTime>,
    dirty: Option<Instant>,
    undo: Vec<Project>,
    redo: Vec<Project>,
}
impl Document {
    fn dir(&self) -> &Path {
        self.path.parent().unwrap_or(Path::new("."))
    }

    fn apply(&mut self, changes: &[Op]) -> Result<bool> {
        if changes.is_empty() {
            return Ok(false);
        }
        let before = self.project.clone();
        ops::apply_all(&mut self.project, changes)?;
        if self.project == before {
            return Ok(false);
        }
        // ponytail: 100 in-memory undo steps; older saves remain in the workspace history.
        if self.undo.len() == 100 {
            self.undo.remove(0);
        }
        self.undo.push(before);
        self.redo.clear();
        self.dirty = Some(Instant::now());
        Ok(true)
    }

    fn add_reviews(&mut self, review: &[String]) -> Result<bool> {
        let changes: Vec<_> = review
            .iter()
            .map(|text| Op::AddMarker {
                at: AUTO_REVIEW_AT,
                note: format!("{AUTO_REVIEW_PREFIX}{text}"),
            })
            .collect();
        self.apply(&changes)
    }

    fn undo(&mut self, redo: bool) -> bool {
        let snapshot = if redo {
            self.redo.pop()
        } else {
            self.undo.pop()
        };
        let Some(snapshot) = snapshot else {
            return false;
        };
        let current = std::mem::replace(&mut self.project, snapshot);
        if redo {
            self.undo.push(current);
        } else {
            self.redo.push(current);
        }
        self.dirty = Some(Instant::now());
        true
    }
}

struct Export {
    open: bool,
    canvas: Canvas,
    final_quality: bool,
    path: String,
    running: bool,
    progress: Arc<std::sync::Mutex<(String, f64)>>,
    cancel: Arc<AtomicBool>,
    rx: Option<mpsc::Receiver<Result<PathBuf, String>>>,
    result: Option<Result<PathBuf, String>>,
}
impl Default for Export {
    fn default() -> Self {
        Self {
            open: false,
            canvas: Canvas::vertical(),
            final_quality: false,
            path: String::new(),
            running: false,
            progress: Arc::new(std::sync::Mutex::new((String::new(), 0.0))),
            cancel: Arc::new(AtomicBool::new(false)),
            rx: None,
            result: None,
        }
    }
}

/// A recipe running on a worker thread from the welcome screen.
struct AutoEdit {
    rx: mpsc::Receiver<AutoMessage>,
    last: String,
}

enum AutoMessage {
    Progress(String),
    Done(Result<EditReport, String>),
}

type WaveJob = (u64, String, PathBuf);
type WaveResult = (u64, String, Result<Vec<f32>, String>);

pub(crate) struct Studio {
    workspace: Workspace,
    document: Option<Document>,
    selection: HashSet<String>,
    clipboard: Vec<(String, Item)>,
    tab: Tab,
    timeline: Timeline,
    playhead: f64,
    timecode: String,
    playback: Playback,
    texture: Option<egui::TextureHandle>,
    preview_due: Option<Instant>,
    preview_size: [u32; 2],
    last_poll: Instant,
    notice: Option<(String, Instant)>,
    services: Services,
    export: Export,
    waveforms: HashMap<String, Vec<f32>>,
    wave_pending: HashSet<String>,
    wave_tx: mpsc::Sender<WaveJob>,
    wave_rx: mpsc::Receiver<WaveResult>,
    epoch: u64,
    projects: Vec<(String, PathBuf)>,
    inbox: Vec<PathBuf>,
    new_name: String,
    new_kind: VideoKind,
    new_canvas: Canvas,
    new_source: Option<PathBuf>,
    new_find: bool,
    new_broll: Vec<PathBuf>,
    footage: footage_panel::FootagePanel,
    doc_work: Option<documentary::Work>,
    cutout: documentary::Cutout,
    source_reasons: HashMap<String, String>,
    close_error: Option<String>,
    capture: Option<Capture>,
    auto: Option<AutoEdit>,
}

impl Studio {
    pub fn new(opts: StudioOptions) -> Result<Self> {
        let workspace =
            Workspace::open(&opts.workspace).or_else(|_| Workspace::init(&opts.workspace))?;
        let services = Services::new(&workspace.root);
        let (wave_tx, wave_jobs) = mpsc::channel::<WaveJob>();
        let (wave_results, wave_rx) = mpsc::channel();
        thread::spawn(move || {
            while let Ok((epoch, id, path)) = wave_jobs.recv() {
                let result = media::pcm_mono(&path, 8_000, None)
                    .map(|pcm| {
                        pcm.chunks(80)
                            .map(|chunk| {
                                chunk
                                    .iter()
                                    .fold(0.0_f32, |peak, sample| peak.max(sample.abs()))
                            })
                            .collect()
                    })
                    .map_err(|error| format!("waveform for {}: {error:#}", path.display()));
                if wave_results.send((epoch, id, result)).is_err() {
                    break;
                }
            }
        });
        let projects = workspace.list_projects()?;
        let inbox = inbox_files(&workspace);
        let mut studio = Self {
            workspace,
            document: None,
            selection: HashSet::new(),
            clipboard: Vec::new(),
            tab: Tab::Media,
            timeline: Timeline::default(),
            playhead: 0.0,
            timecode: String::new(),
            playback: Playback::new(),
            texture: None,
            preview_due: None,
            preview_size: [480, 270],
            last_poll: Instant::now(),
            notice: None,
            services,
            export: Export::default(),
            waveforms: HashMap::new(),
            wave_pending: HashSet::new(),
            wave_tx,
            wave_rx,
            epoch: 0,
            projects,
            inbox,
            new_name: "My edit".into(),
            new_kind: VideoKind::default(),
            new_canvas: Canvas::vertical(),
            new_source: None,
            new_find: false,
            new_broll: Vec::new(),
            footage: footage_panel::FootagePanel::default(),
            doc_work: None,
            cutout: documentary::Cutout::default(),
            source_reasons: HashMap::new(),
            close_error: None,
            capture: None,
            auto: None,
        };
        if let Some(path) = opts.project {
            let path = if path.is_absolute() || path.exists() {
                path
            } else {
                studio.workspace.root.join(path)
            };
            studio.open_project(path)?;
        }
        studio.capture =
            Capture::from_env(studio.document.as_ref().map(|doc| doc.project.duration()))?;
        if studio.capture.is_some() {
            studio.tab = match std::env::var("VEECUT_STUDIO_TAB").as_deref() {
                Ok("footage") => Tab::Footage,
                Ok("sources") => Tab::Sources,
                _ => Tab::Media,
            };
            if let Ok(item) = std::env::var("VEECUT_STUDIO_SELECT") {
                studio.selection.insert(item);
            }
            if let Ok(at) = std::env::var("VEECUT_STUDIO_AT") {
                let at: f64 = at.parse().context("VEECUT_STUDIO_AT must be seconds")?;
                anyhow::ensure!(
                    at.is_finite() && at >= 0.0,
                    "VEECUT_STUDIO_AT must be finite and nonnegative"
                );
                studio.seek(at);
            }
            if studio.tab == Tab::Footage {
                studio
                    .footage
                    .search(std::env::var("VEECUT_STUDIO_QUERY").unwrap_or_else(|_| "moon".into()));
            }
        }
        Ok(studio)
    }

    fn notice(&mut self, message: impl Into<String>) {
        self.notice = Some((message.into(), Instant::now()));
    }

    fn open_project(&mut self, path: PathBuf) -> Result<()> {
        self.save()?;
        let path = if path.is_dir() {
            path.join(PROJECT_FILE)
        } else {
            path
        };
        let path = path
            .canonicalize()
            .with_context(|| format!("finding project {}", path.display()))?;
        let project = Project::load(&path)?;
        project.validate()?;
        self.playback.stop();
        self.selection.clear();
        self.clipboard.clear();
        self.waveforms.clear();
        self.wave_pending.clear();
        self.epoch += 1;
        self.texture = None;
        self.playhead = 0.0;
        self.document = Some(Document {
            project,
            modified: mtime(&path),
            path,
            dirty: None,
            undo: Vec::new(),
            redo: Vec::new(),
        });
        self.invalidate_preview();
        self.queue_waveforms();
        Ok(())
    }

    fn invalidate_preview(&mut self) {
        self.playback.stop();
        self.preview_due = Some(Instant::now() + Duration::from_millis(100));
    }

    fn queue_waveforms(&mut self) {
        if let Some(doc) = &self.document {
            for asset in &doc.project.assets {
                if (asset.has_audio || asset.kind == AssetKind::Audio)
                    && self.wave_pending.insert(asset.id.clone())
                {
                    let path = if asset.path.is_absolute() {
                        asset.path.clone()
                    } else {
                        doc.dir().join(&asset.path)
                    };
                    let _ = self.wave_tx.send((self.epoch, asset.id.clone(), path));
                }
            }
        }
    }

    fn apply(&mut self, changes: Vec<Op>) {
        let Some(doc) = &mut self.document else {
            return;
        };
        match doc.apply(&changes) {
            Ok(true) => {
                self.selection.retain(|id| doc.project.find(id).is_some());
                self.playhead = self.playhead.min(doc.project.duration());
                self.invalidate_preview();
                self.queue_waveforms();
            }
            Ok(false) => {}
            Err(error) => self.notice(format!("{error:#}")),
        }
    }

    fn undo(&mut self, redo: bool) {
        let Some(doc) = &mut self.document else {
            return;
        };
        if doc.undo(redo) {
            self.selection.retain(|id| doc.project.find(id).is_some());
            self.playhead = self.playhead.min(doc.project.duration());
            self.invalidate_preview();
        }
    }

    fn save(&mut self) -> Result<()> {
        self.reload_external(false)?;
        if let Some(doc) = &mut self.document {
            if doc.dirty.is_some() {
                self.workspace.save_project(doc.dir(), &doc.project)?;
                doc.modified = mtime(&doc.path);
                doc.dirty = None;
            }
        }
        Ok(())
    }

    fn reload_external(&mut self, force: bool) -> Result<()> {
        let Some(doc) = &mut self.document else {
            return Ok(());
        };
        let modified = mtime(&doc.path);
        if !force && modified == doc.modified {
            return Ok(());
        }
        let next = Project::load(&doc.path)?;
        next.validate()?;
        doc.modified = modified;
        if next == doc.project {
            return Ok(());
        }
        let mut message = "Updated by agent or CLI".to_owned();
        if doc.dirty.is_some() {
            let stamp = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)?
                .as_nanos();
            let recovery = doc
                .dir()
                .join(".history")
                .join(format!("studio-unsaved-{stamp}.json"));
            doc.project.save(&recovery)?;
            message = format!(
                "Updated by agent. Your unsaved edit is in {}",
                recovery.display()
            );
        }
        if doc.undo.len() == 100 {
            doc.undo.remove(0);
        }
        doc.undo.push(std::mem::replace(&mut doc.project, next));
        doc.redo.clear();
        doc.dirty = None;
        self.selection.retain(|id| doc.project.find(id).is_some());
        self.playhead = self.playhead.min(doc.project.duration());
        self.invalidate_preview();
        self.queue_waveforms();
        self.notice(message);
        Ok(())
    }

    fn seek(&mut self, at: f64) {
        if let Some(doc) = &self.document {
            self.playhead = doc
                .project
                .canvas
                .snap(at)
                .clamp(0.0, doc.project.duration());
            self.invalidate_preview();
        }
    }

    fn toggle_play(&mut self, ctx: &egui::Context) {
        if self.playback.playing {
            if let Some(at) = self.playback.position() {
                self.playhead = at;
            }
            self.invalidate_preview();
        } else if let Some(doc) = &self.document {
            if doc.project.duration() <= 0.0 {
                return;
            }
            if self.playhead >= doc.project.duration() {
                self.playhead = 0.0;
            }
            self.preview_due = None;
            self.playback.request(
                &doc.project,
                doc.dir(),
                self.playhead,
                self.preview_size,
                true,
                ctx,
            );
        }
    }

    fn poll(&mut self, ctx: &egui::Context) {
        self.poll_documentary();
        let reload = self.services.poll();
        if let Some(notice) = self.services.notice.take() {
            self.notice(notice);
        }
        if reload {
            if let Err(error) = self.reload_external(true) {
                self.notice(format!("{error:#}"));
            }
        }
        while let Ok((epoch, id, result)) = self.wave_rx.try_recv() {
            if epoch == self.epoch {
                match result {
                    Ok(peaks) => {
                        self.waveforms.insert(id, peaks);
                    }
                    Err(error) => self.notice(error),
                }
            }
        }
        while let Ok(event) = self.playback.events.try_recv() {
            match event {
                playback::Event::Ready(generation, player, from)
                    if generation == self.playback.generation() =>
                {
                    self.playback.player = Some(player);
                    self.playback.from = from;
                }
                playback::Event::End(generation, at)
                    if generation == self.playback.generation() =>
                {
                    self.playhead = at;
                    self.playback.stop();
                }
                playback::Event::Error(generation, error)
                    if generation == self.playback.generation() =>
                {
                    self.playback.stop();
                    self.notice(error);
                }
                _ => {}
            }
        }
        let frame = self
            .playback
            .frame
            .lock()
            .ok()
            .and_then(|mut frame| frame.take());
        if let Some(frame) = frame {
            if frame.generation == self.playback.generation() {
                let image = egui::ColorImage::from_rgba_premultiplied(
                    [
                        frame.pixels.width() as usize,
                        frame.pixels.height() as usize,
                    ],
                    frame.pixels.data(),
                );
                if let Some(texture) = &mut self.texture {
                    texture.set(image, egui::TextureOptions::LINEAR);
                } else {
                    self.texture =
                        Some(ctx.load_texture("preview", image, egui::TextureOptions::LINEAR));
                }
            }
        }
        if let Some(at) = self.playback.position() {
            self.playhead = self
                .document
                .as_ref()
                .map(|doc| at.min(doc.project.duration()))
                .unwrap_or(at);
        }
        if self.last_poll.elapsed() >= Duration::from_millis(500) {
            self.last_poll = Instant::now();
            if let Err(error) = self.reload_external(false) {
                self.notice(format!("{error:#}"));
            }
        }
        if self
            .document
            .as_ref()
            .and_then(|doc| doc.dirty)
            .is_some_and(|dirty| dirty.elapsed() >= Duration::from_millis(800))
        {
            if let Err(error) = self.save() {
                self.notice(format!("Autosave failed: {error:#}"));
            }
        }
        if self.preview_due.is_some_and(|due| Instant::now() >= due) {
            self.preview_due = None;
            if let Some(doc) = &self.document {
                self.playback.request(
                    &doc.project,
                    doc.dir(),
                    self.playhead,
                    self.preview_size,
                    false,
                    ctx,
                );
            }
        }
        let auto = self.auto.as_mut().map(|auto| {
            let mut done = None;
            while let Ok(message) = auto.rx.try_recv() {
                match message {
                    AutoMessage::Progress(line) => auto.last = line,
                    AutoMessage::Done(result) => done = Some(result),
                }
            }
            done
        });
        if let Some(Some(result)) = auto {
            self.auto = None;
            self.projects = self.workspace.list_projects().unwrap_or_default();
            match result {
                Ok(report) => {
                    let mut message = report.summary.join(" | ");
                    if !report.review.is_empty() {
                        message += &format!(
                            " | {} item(s) in Needs review in the right inspector",
                            report.review.len()
                        );
                    }
                    if let Some(dir) = report.projects.first() {
                        let result = self.open_project(dir.clone()).and_then(|_| {
                            if let Some(doc) = &mut self.document {
                                doc.add_reviews(&report.review)?;
                            }
                            self.save()
                        });
                        if let Err(error) = result {
                            message = format!(
                                "{message} | Could not open or save Auto-edit review: {error:#}"
                            );
                        }
                    }
                    self.notice(message);
                }
                Err(error) => self.notice(format!("Auto-edit failed: {error}")),
            }
        }
        if self.auto.is_some() {
            ctx.request_repaint_after(Duration::from_millis(200));
        }
        if let Some(rx) = &self.export.rx {
            if let Ok(result) = rx.try_recv() {
                self.export.running = false;
                self.export.result = Some(result);
                self.export.rx = None;
            }
        }
        ctx.request_repaint_after(Duration::from_millis(if self.playback.playing {
            16
        } else {
            100
        }));
    }

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top("top").show(ui, |ui| {
            ui.horizontal(|ui| {
                if ui.button("VeeCut Studio").clicked() {
                    match self.save() {
                        Ok(()) => {
                            self.playback.stop();
                            self.document = None;
                            self.projects = self.workspace.list_projects().unwrap_or_default();
                            self.inbox = inbox_files(&self.workspace);
                        }
                        Err(error) => self.notice(format!("{error:#}")),
                    }
                }
                if let Some(doc) = &self.document {
                    ui.separator();
                    ui.strong(&doc.project.name);
                    let mut canvas = doc.project.canvas.clone();
                    if canvas_combo(ui, "top_canvas", &mut canvas) {
                        self.apply(vec![Op::Set {
                            path: "/canvas".into(),
                            value: json!(canvas),
                        }]);
                    }
                    let undo = self
                        .document
                        .as_ref()
                        .is_some_and(|doc| !doc.undo.is_empty());
                    let redo = self
                        .document
                        .as_ref()
                        .is_some_and(|doc| !doc.redo.is_empty());
                    if ui.add_enabled(undo, egui::Button::new("Undo")).clicked() {
                        self.undo(false);
                    }
                    if ui.add_enabled(redo, egui::Button::new("Redo")).clicked() {
                        self.undo(true);
                    }
                    if ui.button("Export").clicked() {
                        self.open_export();
                    }
                    ui.separator();
                    let response = ui.add_sized(
                        [ui.available_width().max(120.0) - 60.0, 24.0],
                        egui::TextEdit::singleline(&mut self.services.prompt)
                            .hint_text("Ask AI to edit this video..."),
                    );
                    let enter =
                        response.lost_focus() && ui.input(|input| input.key_pressed(Key::Enter));
                    if ui
                        .add_enabled(!self.services.agent_busy, egui::Button::new("Ask AI"))
                        .clicked()
                        || enter
                    {
                        self.run_agent();
                    }
                }
            });
            if let Some((message, when)) = &self.notice {
                if when.elapsed() < Duration::from_secs(12) {
                    ui.colored_label(Color32::from_rgb(240, 202, 125), message);
                }
            }
            if let Some(work) = &self.doc_work {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(&work.progress);
                });
            }
        });
    }

    fn welcome(&mut self, ui: &mut egui::Ui) {
        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.add_space(24.0); ui.heading("Your editing workspace"); ui.label(self.workspace.root.display().to_string());
                ui.separator();
                ui.columns(2, |columns| {
                    let ui = &mut columns[0];
                    ui.heading("Projects");
                    let mut open = None;
                    for (name, path) in &self.projects { if ui.button(name).clicked() { open = Some(path.clone()); } }
                    if self.projects.is_empty() { ui.label("No projects yet."); }
                    if ui.button("Open project.json...").clicked() {
                        open = rfd::FileDialog::new().add_filter("VeeCut project", &["json"]).pick_file();
                    }
                    if let Some(path) = open { if let Err(error) = self.open_project(path) { self.notice(format!("{error:#}")); } }
                    let ui = &mut columns[1];
                    ui.heading("New edit");
                    ui.label("Project name"); ui.text_edit_singleline(&mut self.new_name);
                    egui::ComboBox::from_id_salt("new_kind").selected_text(self.new_kind.label()).show_ui(ui, |ui| {
                        for kind in VideoKind::ALL { ui.selectable_value(&mut self.new_kind, kind, kind.label()); }
                    });
                    canvas_combo(ui, "new_canvas", &mut self.new_canvas);
                    if ui.button("Choose source file...").clicked() { self.new_source = rfd::FileDialog::new().add_filter("Video or audio", &["mp4", "mov", "mkv", "webm", "m4v", "mp3", "wav", "m4a", "aiff", "flac"]).pick_file(); }
                    if let Some(path) = &self.new_source { ui.label(path.display().to_string()); }
                    if self.new_kind == VideoKind::Documentary {
                        ui.checkbox(&mut self.new_find, "Find footage online (free, licence-tracked)");
                        if ui.button("Choose B-roll files...").clicked() {
                            if let Some(paths) = rfd::FileDialog::new().add_filter("Video or pictures", &["mp4", "mov", "mkv", "webm", "png", "jpg", "jpeg", "webp"]).pick_files() {
                                self.new_broll.extend(paths);
                            }
                        }
                        let mut remove = None;
                        for (index, path) in self.new_broll.iter().enumerate() {
                            ui.horizontal_wrapped(|ui| {
                                ui.label(filename(path));
                                if ui.small_button("Remove").clicked() { remove = Some(index); }
                            });
                        }
                        if let Some(index) = remove { self.new_broll.remove(index); }
                    }
                    let ready = self.new_source.is_some() && !self.new_name.trim().is_empty() && self.auto.is_none();
                    ui.horizontal(|ui| {
                        if ui.add_enabled(ready, egui::Button::new("Auto-edit with the recipe")).on_hover_text("Transcribe, cut, frame, caption and mix by the rules for this kind of video, then open the result").clicked() {
                            self.start_auto_edit();
                        }
                        if ui.add_enabled(ready, egui::Button::new("Create empty edit")).clicked() {
                            if let Err(error) = self.create_project() { self.notice(format!("{error:#}")); }
                        }
                    });
                    if let Some(auto) = &self.auto {
                        ui.horizontal(|ui| { ui.spinner(); ui.label(format!("Auto-editing: {}", auto.last)); });
                    }
                    ui.separator(); ui.heading("Inbox videos");
                    for path in &self.inbox {
                        let selected = self.new_source.as_ref() == Some(path);
                        if ui.selectable_label(selected, filename(path)).clicked() { self.new_source = Some(path.clone()); }
                    }
                });
            });
        });
    }

    fn create_project(&mut self) -> Result<()> {
        let source = self.new_source.as_ref().context("choose a source video")?;
        let (dir, mut project) = self.workspace.create_project(
            self.new_name.trim(),
            self.new_kind,
            self.new_canvas.clone(),
        )?;
        let asset = self
            .workspace
            .add_asset(&mut project, &dir, source, AssetRole::Source)?;
        ops::apply(&mut project, &add_clip(asset, 0.0))?;
        self.workspace.save_project(&dir, &project)?;
        self.open_project(dir)
    }

    /// Runs the recipe for the chosen kind on a worker thread; `poll` opens the result.
    fn start_auto_edit(&mut self) {
        let Some(source) = self.new_source.clone() else {
            return;
        };
        let mut request = EditRequest::new(self.new_kind);
        request.sources = vec![source];
        request.canvas = Some(self.new_canvas.clone());
        request.name = Some(self.new_name.trim().to_string());
        if self.new_kind == VideoKind::Documentary {
            request.find = self.new_find;
            request.broll = self.new_broll.clone();
        }
        // The editor previews in place; render from Export when the edit looks right.
        request.render = false;
        let workspace = self.workspace.clone();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let progress_tx = tx.clone();
            let result = recipes::edit(&workspace, &request, &mut |line| {
                let _ = progress_tx.send(AutoMessage::Progress(line.to_string()));
            })
            .map_err(|error| format!("{error:#}"));
            let _ = tx.send(AutoMessage::Done(result));
        });
        self.auto = Some(AutoEdit {
            rx,
            last: "starting".into(),
        });
    }

    fn left_panel(&mut self, ui: &mut egui::Ui) {
        egui::Panel::left("library")
            .default_size(260.0)
            .size_range(210.0..=400.0)
            .resizable(true)
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    for (tab, label) in [
                        (Tab::Media, "Media"),
                        (Tab::Footage, "Footage"),
                        (Tab::Sources, "Sources"),
                        (Tab::Text, "Text"),
                        (Tab::Captions, "Captions"),
                        (Tab::LookAudio, "Look & Audio"),
                        (Tab::Skills, "Skills"),
                        (Tab::Agent, "Agent"),
                    ] {
                        ui.selectable_value(&mut self.tab, tab, label);
                    }
                });
                ui.separator();
                egui::ScrollArea::vertical()
                    .id_salt("library_scroll")
                    .show(ui, |ui| match self.tab {
                        Tab::Media => self.media_ui(ui),
                        Tab::Footage => {
                            let can_add = self.doc_work.is_none();
                            if let Some((id, at_playhead)) = self.footage.show(ui, can_add) {
                                self.fetch_footage(id, at_playhead);
                            }
                        }
                        Tab::Sources => self.sources_ui(ui),
                        Tab::Text => {
                            ui.heading("Text presets");
                            for (preset, label) in text_presets() {
                                if ui.button(label).clicked() {
                                    self.apply(vec![Op::AddText {
                                        text: label.into(),
                                        start: self.playhead,
                                        duration: 3.0,
                                        preset,
                                        position: None,
                                        style: None,
                                        anim_in: None,
                                        anim_out: None,
                                        track: None,
                                    }]);
                                }
                            }
                        }
                        Tab::Captions => {
                            if let Some(doc) = &self.document {
                                let changes = inspector::captions(ui, &doc.project);
                                self.apply(changes);
                            }
                        }
                        Tab::LookAudio => {
                            if let Some(doc) = &self.document {
                                let changes = inspector::look_audio(ui, &doc.project);
                                self.apply(changes);
                            }
                        }
                        Tab::Skills => {
                            if let Some(doc) = &self.document {
                                self.services
                                    .skills_ui(ui, &self.workspace, doc.project.kind);
                            }
                        }
                        Tab::Agent => {
                            if self.services.agent_ui(ui) {
                                self.run_agent();
                            }
                        }
                    });
            });
    }

    fn media_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("Media library");
        if ui.button("Import files...").clicked() {
            if let Some(paths) = rfd::FileDialog::new()
                .add_filter(
                    "Media",
                    &[
                        "mp4", "mov", "mkv", "webm", "m4v", "mp3", "wav", "m4a", "aac", "flac",
                        "png", "jpg", "jpeg", "webp",
                    ],
                )
                .pick_files()
            {
                if let Err(error) = self.import_files(paths) {
                    self.notice(format!("{error:#}"));
                }
            }
        }
        ui.small("Drag a file onto a track, or add at the playhead.");
        let mut add = None;
        if let Some(doc) = &self.document {
            for asset in &doc.project.assets {
                ui.group(|ui| {
                    ui.dnd_drag_source(
                        egui::Id::unique(("asset", &asset.id)),
                        asset.id.clone(),
                        |ui| {
                            ui.strong(filename(&asset.path));
                        },
                    );
                    ui.small(format!(
                        "{:?} · {:.1}s · {}x{}",
                        asset.kind, asset.duration, asset.width, asset.height
                    ));
                    if ui.button("Add at playhead").clicked() {
                        add = Some(asset.id.clone());
                    }
                });
            }
        }
        if let Some(asset) = add {
            self.apply(vec![add_clip(asset, self.playhead)]);
        }
    }

    fn import_files(&mut self, paths: Vec<PathBuf>) -> Result<()> {
        let Some(doc) = &self.document else {
            return Ok(());
        };
        let mut next = doc.project.clone();
        for path in paths {
            let role = match path
                .extension()
                .and_then(|extension| extension.to_str())
                .unwrap_or("")
                .to_ascii_lowercase()
                .as_str()
            {
                "wav" | "mp3" | "m4a" | "aac" | "flac" => AssetRole::Music,
                "png" | "jpg" | "jpeg" | "webp" => AssetRole::Image,
                _ => AssetRole::Source,
            };
            self.workspace
                .add_asset(&mut next, doc.dir(), &path, role)?;
        }
        self.apply(vec![Op::Set {
            path: "/assets".into(),
            value: json!(next.assets),
        }]);
        Ok(())
    }

    fn preview(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        egui::CentralPanel::default().show(ui, |ui| {
            let Some(doc) = &self.document else {
                return;
            };
            let aspect = doc.project.canvas.width as f32 / doc.project.canvas.height as f32;
            let fps = doc.project.canvas.fps;
            let duration = doc.project.duration();
            let available = egui::vec2(
                ui.available_width(),
                (ui.available_height() - 48.0).max(40.0),
            );
            let width = available.x.min(available.y * aspect);
            let size = egui::vec2(width, width / aspect);
            let render_width = size.x.min(640.0).max(64.0) as u32;
            let render_height = (render_width as f32 / aspect).max(64.0) as u32;
            let new_size = [render_width, render_height];
            if self.preview_size != new_size && !self.playback.playing {
                self.preview_size = new_size;
                self.preview_due = Some(Instant::now() + Duration::from_millis(120));
            }
            ui.allocate_ui_with_layout(
                available,
                egui::Layout::centered_and_justified(egui::Direction::TopDown),
                |ui| {
                    if let Some(texture) = &self.texture {
                        ui.add(egui::Image::new(texture).fit_to_exact_size(size));
                    } else {
                        ui.label("Preparing preview...");
                    }
                },
            );
            ui.horizontal(|ui| {
                if ui.button("|<").on_hover_text("Go to start").clicked() {
                    self.seek(0.0);
                }
                if ui.button("<").on_hover_text("Previous frame").clicked() {
                    self.seek(self.playhead - 1.0 / fps);
                }
                if ui
                    .button(if self.playback.playing {
                        "Pause"
                    } else {
                        "Play"
                    })
                    .clicked()
                {
                    self.toggle_play(&ctx);
                }
                if ui.button(">").on_hover_text("Next frame").clicked() {
                    self.seek(self.playhead + 1.0 / fps);
                }
                let id = egui::Id::unique("timecode");
                if !ui.memory(|memory| memory.has_focus(id)) {
                    self.timecode = format_timecode(self.playhead, fps);
                }
                let response = ui.add(
                    egui::TextEdit::singleline(&mut self.timecode)
                        .id(id)
                        .desired_width(105.0),
                );
                if response.lost_focus() {
                    match parse_timecode(&self.timecode, fps) {
                        Some(at) => self.seek(at),
                        None => self.notice("Use timecode HH:MM:SS:FF"),
                    }
                }
                ui.label(format!("/ {}", format_timecode(duration, fps)));
                if self.playback.playing && self.playback.player.is_none() {
                    ui.spinner();
                    ui.small("Preparing audio...");
                }
            });
        });
    }

    fn shortcuts(&mut self, ctx: &egui::Context) {
        if self.document.is_none() {
            return;
        }
        let (mut pressed, modifiers, copied, pasted) = ctx.input(|input| {
            (
                input
                    .events
                    .iter()
                    .filter_map(|event| match event {
                        egui::Event::Key {
                            key, pressed: true, ..
                        } => Some(*key),
                        _ => None,
                    })
                    .collect::<Vec<_>>(),
                input.modifiers,
                input
                    .events
                    .iter()
                    .any(|event| matches!(event, egui::Event::Copy)),
                input
                    .events
                    .iter()
                    .any(|event| matches!(event, egui::Event::Paste(_))),
            )
        });
        if pressed.is_empty() && !copied && !pasted {
            return;
        }
        if ctx
            .memory(|memory| memory.focused())
            .is_some_and(|id| egui::text_edit::TextEditState::load(ctx, id).is_some())
        {
            return;
        }
        if copied {
            self.copy();
            pressed.retain(|key| *key != Key::C);
        }
        if pasted {
            self.paste();
            pressed.retain(|key| *key != Key::V);
        }
        let fps = self
            .document
            .as_ref()
            .map(|doc| doc.project.canvas.fps)
            .unwrap_or(30.0);
        for key in pressed {
            if modifiers.command {
                match key {
                    Key::Z => self.undo(modifiers.shift),
                    Key::Y => self.undo(true),
                    Key::A => {
                        if let Some(doc) = &self.document {
                            self.selection = doc
                                .project
                                .tracks
                                .iter()
                                .flat_map(|track| track.items.iter().map(|item| item.id.clone()))
                                .collect();
                        }
                    }
                    Key::C => self.copy(),
                    Key::V => self.paste(),
                    Key::D => self.apply(
                        self.selection
                            .iter()
                            .map(|item| Op::Duplicate { item: item.clone() })
                            .collect(),
                    ),
                    _ => {}
                }
            } else {
                match key {
                    Key::Space | Key::K => self.toggle_play(ctx),
                    Key::J => self.seek(self.playhead - 1.0),
                    Key::L => self.seek(self.playhead + 1.0),
                    Key::ArrowLeft => {
                        self.seek(self.playhead - if modifiers.shift { 5.0 } else { 1.0 / fps })
                    }
                    Key::ArrowRight => {
                        self.seek(self.playhead + if modifiers.shift { 5.0 } else { 1.0 / fps })
                    }
                    Key::Home | Key::Enter => self.seek(0.0),
                    Key::End => {
                        if let Some(doc) = &self.document {
                            self.seek(doc.project.duration());
                        }
                    }
                    Key::S => self.split(None),
                    Key::Q => self.split(Some(true)),
                    Key::W => self.split(Some(false)),
                    Key::Delete | Key::Backspace => self.apply(
                        self.selection
                            .iter()
                            .map(|item| Op::Delete { item: item.clone() })
                            .collect(),
                    ),
                    Key::N => self.timeline.snapping = !self.timeline.snapping,
                    Key::Escape => self.selection.clear(),
                    _ => {}
                }
            }
        }
    }

    fn copy(&mut self) {
        if let Some(doc) = &self.document {
            self.clipboard = doc
                .project
                .tracks
                .iter()
                .flat_map(|track| {
                    track
                        .items
                        .iter()
                        .filter(|item| self.selection.contains(&item.id))
                        .map(|item| (track.id.clone(), item.clone()))
                })
                .collect();
        }
    }

    fn paste(&mut self) {
        let Some(doc) = &self.document else {
            return;
        };
        let mut scratch = doc.project.clone();
        let start = self
            .clipboard
            .iter()
            .map(|(_, item)| item.start)
            .fold(f64::INFINITY, f64::min);
        let mut changes = Vec::new();
        let mut ids = HashSet::new();
        for (track, original) in &self.clipboard {
            let Some(index) = scratch.track_index(track) else {
                continue;
            };
            let mut copy = original.clone();
            copy.id = scratch.new_id("copy");
            copy.start = self.playhead + original.start - start;
            let id = copy.id.clone();
            let insertion = scratch.tracks[index]
                .items
                .iter()
                .position(|item| item.start >= copy.start)
                .unwrap_or(scratch.tracks[index].items.len());
            let op = Op::Set {
                path: format!("/tracks/{index}/items/-"),
                value: json!(copy),
            };
            if let Err(error) = ops::apply(&mut scratch, &op) {
                self.notice(format!("{error:#}"));
                return;
            }
            changes.push(op);
            if index == 0 {
                let op = Op::Move {
                    item: id.clone(),
                    start: None,
                    track: None,
                    index: Some(insertion),
                };
                if let Err(error) = ops::apply(&mut scratch, &op) {
                    self.notice(format!("{error:#}"));
                    return;
                }
                changes.push(op);
            }
            ids.insert(id);
        }
        self.apply(changes);
        self.selection = ids;
    }

    fn split(&mut self, delete_left: Option<bool>) {
        let Some(doc) = &self.document else {
            return;
        };
        let mut scratch = doc.project.clone();
        let mut changes = Vec::new();
        for track in &doc.project.tracks {
            let crossing: Vec<_> = track
                .items
                .iter()
                .filter(|item| {
                    item.start < self.playhead
                        && item.end() > self.playhead
                        && (self.selection.is_empty() || self.selection.contains(&item.id))
                })
                .map(|item| item.id.clone())
                .collect();
            if crossing.is_empty() {
                continue;
            }
            let op = Op::Split {
                at: self.playhead,
                track: Some(track.id.clone()),
            };
            let applied = match ops::apply(&mut scratch, &op) {
                Ok(applied) => applied,
                Err(error) => {
                    self.notice(format!("{error:#}"));
                    return;
                }
            };
            changes.push(op);
            if let Some(left) = delete_left {
                let delete = if left { crossing } else { applied.items };
                for item in delete {
                    let op = Op::Delete { item };
                    if let Err(error) = ops::apply(&mut scratch, &op) {
                        self.notice(format!("{error:#}"));
                        return;
                    }
                    changes.push(op);
                }
            }
        }
        self.apply(changes);
    }

    fn run_agent(&mut self) {
        if let Err(error) = self.save() {
            self.notice(format!("Save before asking the agent: {error:#}"));
            return;
        }
        if let Some(doc) = &self.document {
            self.services.run_agent(&self.workspace, &doc.path);
            self.tab = Tab::Agent;
        }
    }

    fn open_export(&mut self) {
        if self.export.running {
            self.export.open = true;
            return;
        }
        if let Some(doc) = &self.document {
            let slug = doc
                .dir()
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("edit");
            self.export = Export {
                open: true,
                canvas: doc.project.canvas.clone(),
                path: self.workspace.next_draft_path(slug).display().to_string(),
                ..Export::default()
            };
        }
    }

    fn export_dialog(&mut self, ctx: &egui::Context) {
        if !self.export.open {
            return;
        }
        let mut open = true;
        let mut start = false;
        egui::Window::new("Export video")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.add_enabled_ui(!self.export.running, |ui| {
                    ui.label("Canvas preset");
                    canvas_combo(ui, "export_canvas", &mut self.export.canvas);
                    ui.small("9:16 Reels/TikTok/Shorts · 16:9 YouTube · 4:5 Feed · 1:1 Square");
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut self.export.final_quality, false, "Draft");
                        ui.radio_value(&mut self.export.final_quality, true, "Final");
                    });
                    ui.label("Output MP4");
                    ui.text_edit_singleline(&mut self.export.path);
                    if ui.button("Choose output...").clicked() {
                        if let Some(path) = rfd::FileDialog::new()
                            .set_file_name("edit.mp4")
                            .add_filter("MP4 video", &["mp4"])
                            .save_file()
                        {
                            self.export.path = path.display().to_string();
                        }
                    }
                    let blockers = if self.export.final_quality {
                        self.document
                            .as_ref()
                            .map(|doc| veecut_engine::footage::blocking_sources(&doc.project))
                            .unwrap_or_default()
                    } else {
                        Vec::new()
                    };
                    for (asset, source) in &blockers {
                        ui.colored_label(
                            Color32::LIGHT_RED,
                            format!(
                                "{}: {}",
                                source.title,
                                source
                                    .flag_reason
                                    .as_deref()
                                    .unwrap_or("Rights need review")
                            ),
                        );
                        if ui
                            .button(format!("Approve (I checked the rights): {asset}"))
                            .clicked()
                        {
                            self.apply(vec![documentary::source_set(
                                asset,
                                "approved",
                                json!(true),
                            )]);
                        }
                    }
                    if ui
                        .add_enabled(blockers.is_empty(), egui::Button::new("Export"))
                        .clicked()
                    {
                        start = true;
                    }
                });
                if self.export.running {
                    if let Ok(progress) = self.export.progress.lock() {
                        ui.add(
                            egui::ProgressBar::new(progress.1 as f32)
                                .show_percentage()
                                .text(&progress.0),
                        );
                    }
                    if ui.button("Cancel export").clicked() {
                        self.export.cancel.store(true, Ordering::Relaxed);
                    }
                }
                if let Some(result) = self.export.result.clone() {
                    match result {
                        Ok(path) => {
                            ui.label(format!("Saved {}", path.display()));
                            if ui.button("Open video").clicked() {
                                if let Err(error) = open::that(&path) {
                                    self.notice(format!("{error:#}"));
                                }
                            }
                            if ui.button("Show in folder").clicked() {
                                if let Err(error) = reveal(&path) {
                                    self.notice(format!("{error:#}"));
                                }
                            }
                        }
                        Err(error) => {
                            ui.colored_label(Color32::LIGHT_RED, error);
                        }
                    }
                }
            });
        self.export.open = open;
        if start {
            self.start_export(ctx);
        }
    }

    fn start_export(&mut self, ctx: &egui::Context) {
        let Some(doc) = &self.document else {
            return;
        };
        if self.export.final_quality {
            if let Err(error) = veecut_engine::footage::check_final(&doc.project) {
                self.notice(format!("{error:#}"));
                return;
            }
        }
        let out = PathBuf::from(self.export.path.trim());
        if out.as_os_str().is_empty() {
            self.notice("Choose an output path");
            return;
        }
        if out.exists() {
            self.notice(
                "Output already exists. Choose a new filename to keep the previous export.",
            );
            return;
        }
        let project = doc.project.clone();
        let dir = doc.dir().to_owned();
        let opts = RenderOptions::for_canvas(
            &self.export.canvas,
            if self.export.final_quality {
                Quality::Final
            } else {
                Quality::Draft
            },
        );
        let (tx, rx) = mpsc::channel();
        self.export.rx = Some(rx);
        self.export.cancel.store(false, Ordering::Relaxed);
        self.export.result = None;
        self.export.running = true;
        let cancel = Arc::clone(&self.export.cancel);
        let progress = Arc::clone(&self.export.progress);
        let ctx = ctx.clone();
        thread::spawn(move || {
            let result = render::export(&project, &dir, &out, &opts, &mut |update| {
                if let Ok(mut state) = progress.lock() {
                    *state = (
                        update.stage.into(),
                        if update.total > 0.0 {
                            (update.done / update.total).clamp(0.0, 1.0)
                        } else {
                            0.0
                        },
                    );
                }
                ctx.request_repaint();
                !cancel.load(Ordering::Relaxed)
            })
            .map(|report| report.path)
            .map_err(|error| format!("{error:#}"));
            let _ = tx.send(result);
            ctx.request_repaint();
        });
    }
}

impl eframe::App for Studio {
    fn on_exit(&mut self) {
        self.services.stop_agent();
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll(&ctx);
        self.shortcuts(&ctx);
        self.top_bar(ui);
        if self.document.is_some() {
            egui::Panel::bottom("timeline")
                .default_size(320.0)
                .min_size(170.0)
                .resizable(true)
                .show(ui, |ui| {
                    if let Some(doc) = &self.document {
                        let before = self.playhead;
                        let changes = self.timeline.show(
                            ui,
                            &doc.project,
                            &mut self.selection,
                            &mut self.playhead,
                            &self.waveforms,
                        );
                        self.apply(changes);
                        if self.playhead != before {
                            self.invalidate_preview();
                        }
                    }
                });
            self.left_panel(ui);
            egui::Panel::right("inspector")
                .default_size(270.0)
                .size_range(220.0..=420.0)
                .resizable(true)
                .show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt("inspector_scroll")
                        .show(ui, |ui| {
                            self.cutout_ui(ui);
                            if let Some(doc) = &self.document {
                                let selected = self.selection.iter().next().map(String::as_str);
                                let changes = inspector::show(ui, &doc.project, selected);
                                self.apply(changes);
                            }
                        });
                });
            self.preview(ui);
        } else {
            self.welcome(ui);
        }
        self.export_dialog(&ctx);
        if let Some(capture) = &mut self.capture {
            let preview_ready = (self.tab != Tab::Footage || self.footage.ready())
                && self.texture.is_some()
                && self
                    .wave_pending
                    .iter()
                    .all(|id| self.waveforms.contains_key(id));
            let agent_ready = self.services.agents_ready();
            match capture.advance(&ctx, preview_ready, Some(self.playhead), agent_ready) {
                Ok(capture::Action::Play) => self.toggle_play(&ctx),
                Ok(capture::Action::ShowAgent) => {
                    self.tab = Tab::Agent;
                    ctx.request_repaint();
                }
                Ok(capture::Action::Close) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                Ok(capture::Action::None) => {}
                Err(error) => {
                    eprintln!("Studio documentation capture failed: {error:#}");
                    self.capture = None;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }
        if ctx.input(|input| input.viewport().close_requested()) {
            if let Err(error) = self.save() {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.close_error = Some(format!("{error:#}"));
            } else {
                self.playback.stop();
                self.export.cancel.store(true, Ordering::Relaxed);
            }
        }
        if let Some(error) = self.close_error.clone() {
            egui::Window::new("Project was not saved")
                .collapsible(false)
                .show(&ctx, |ui| {
                    ui.label(error);
                    ui.label("Keep the editor open to preserve your changes.");
                    if ui.button("Keep editing").clicked() {
                        self.close_error = None;
                    }
                });
        }
    }
}

fn mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}
fn filename(path: &Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}
fn inbox_files(workspace: &Workspace) -> Vec<PathBuf> {
    let mut paths: Vec<_> = std::fs::read_dir(workspace.inbox())
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| {
                        ["mp4", "mov", "mkv", "webm", "m4v"]
                            .contains(&extension.to_ascii_lowercase().as_str())
                    })
        })
        .collect();
    paths.sort();
    paths
}
fn add_clip(asset: String, start: f64) -> Op {
    Op::AddClip {
        asset,
        track: None,
        start: Some(start),
        index: None,
        src_in: 0.0,
        duration: None,
        frame: None,
        volume: None,
        duck: None,
    }
}
fn canvas_combo(ui: &mut egui::Ui, id: &str, canvas: &mut Canvas) -> bool {
    let mut changed = false;
    egui::ComboBox::from_id_salt(id)
        .selected_text(canvas.aspect_name())
        .show_ui(ui, |ui| {
            for aspect in ["9:16", "16:9", "4:5", "1:1"] {
                if ui
                    .selectable_label(canvas.aspect_name() == aspect, aspect)
                    .clicked()
                {
                    let fps = canvas.fps;
                    let background = canvas.background;
                    *canvas = Canvas::from_aspect(aspect).expect("listed aspect");
                    canvas.fps = fps;
                    canvas.background = background;
                    changed = true;
                }
            }
        });
    changed
}
fn text_presets() -> [(TextPreset, &'static str); 16] {
    [
        (TextPreset::Hook, "Hook"),
        (TextPreset::Title, "Title"),
        (TextPreset::Card, "Card"),
        (TextPreset::LowerThird, "Lower third"),
        (TextPreset::Callout, "Callout"),
        (TextPreset::Counter, "Counter"),
        (TextPreset::Label, "Label"),
        (TextPreset::Quote, "Quote"),
        (TextPreset::List, "List"),
        (TextPreset::Kinetic, "Kinetic"),
        (TextPreset::Cta, "Call to action"),
        (TextPreset::Keyword, "Keyword"),
        (TextPreset::Chapter, "Chapter"),
        (TextPreset::Shock, "Shock"),
        (TextPreset::BlackCard, "Black card"),
        (TextPreset::Mega, "Mega"),
    ]
}
fn format_timecode(seconds: f64, fps: f64) -> String {
    let nominal = fps.round().max(1.0) as u64;
    let frames = (seconds.max(0.0) * fps).round() as u64;
    let seconds = frames / nominal;
    format!(
        "{:02}:{:02}:{:02}:{:02}",
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60,
        frames % nominal
    )
}
fn parse_timecode(text: &str, fps: f64) -> Option<f64> {
    let parts: Vec<u64> = text
        .split(':')
        .map(str::parse)
        .collect::<std::result::Result<_, _>>()
        .ok()?;
    if parts.len() != 4
        || parts[1] >= 60
        || parts[2] >= 60
        || parts[3] >= fps.round().max(1.0) as u64
    {
        return None;
    }
    let frames = parts[0]
        .checked_mul(3600)?
        .checked_add(parts[1] * 60)?
        .checked_add(parts[2])?
        .checked_mul(fps.round().max(1.0) as u64)?
        .checked_add(parts[3])?;
    Some(frames as f64 / fps)
}
fn reveal(path: &Path) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        let status = std::process::Command::new("open")
            .arg("-R")
            .arg(path)
            .status()?;
        anyhow::ensure!(status.success(), "Finder could not reveal the video");
    }
    #[cfg(not(target_os = "macos"))]
    {
        open::that(path.parent().unwrap_or(Path::new(".")))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn timecode_round_trip_and_invalid_fields() {
        for fps in [24.0, 29.97, 30.0, 60.0] {
            let at = 3_661.0 + 7.0 / fps;
            let roundtrip = parse_timecode(&format_timecode(at, fps), fps).unwrap();
            assert!((roundtrip - at).abs() <= 0.5 / fps + 1e-6);
        }
        assert_eq!(parse_timecode("00:60:00:00", 30.0), None);
        assert_eq!(parse_timecode("00:00:00:30", 30.0), None);
        assert_eq!(parse_timecode("invalid", 30.0), None);
    }

    #[test]
    fn auto_edit_review_survives_cuts_reopen_and_undoable_dismissal() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = Workspace {
            root: tmp.path().into(),
        };
        let (dir, project) = ws
            .create_project("review", VideoKind::TalkingHeadShort, Canvas::vertical())
            .unwrap();
        let path = dir.join(PROJECT_FILE);
        let document = |project| Document {
            project,
            path: path.clone(),
            modified: mtime(&path),
            dirty: None,
            undo: Vec::new(),
            redo: Vec::new(),
        };
        let mut doc = document(project);
        doc.apply(&[Op::AddMarker {
            at: 1.0,
            note: "Pinned user note".into(),
        }])
        .unwrap();
        let review = vec![
            "Possible retake kept at 17.00-19.00s of the source.".into(),
            "Caption hidden at timeline 2.0 s.".into(),
        ];
        doc.add_reviews(&review).unwrap();
        doc.apply(&[Op::CutRange { from: 0.0, to: 2.0 }]).unwrap();
        assert_eq!(doc.project.markers.len(), review.len());
        for (marker, text) in doc.project.markers.iter().zip(&review) {
            assert_eq!(marker.at, AUTO_REVIEW_AT);
            assert_eq!(
                marker.note.strip_prefix(AUTO_REVIEW_PREFIX),
                Some(text.as_str())
            );
            assert!(!marker.done);
        }
        ws.save_project(&dir, &doc.project).unwrap();

        let mut doc = document(Project::load(&path).unwrap());
        assert_eq!(doc.project.markers.len(), review.len());
        assert!(doc.project.markers.iter().all(|marker| !marker.done));
        doc.apply(&[Op::Set {
            path: "/markers/0/done".into(),
            value: json!(true),
        }])
        .unwrap();
        ws.save_project(&dir, &doc.project).unwrap();
        let reopened = Project::load(&path).unwrap();
        assert!(reopened.markers[0].done);
        assert!(!reopened.markers[1].done);

        assert!(doc.undo(false));
        assert!(doc.project.markers.iter().all(|marker| !marker.done));
        ws.save_project(&dir, &doc.project).unwrap();
        assert!(
            Project::load(&path)
                .unwrap()
                .markers
                .iter()
                .all(|marker| !marker.done)
        );
        assert!(doc.undo(true));
        assert!(doc.project.markers[0].done);
    }
}
