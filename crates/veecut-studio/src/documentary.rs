//! Documentary imports and speaker masks run away from the UI; completed edits use its undo path.
use super::*;
use veecut_engine::{footage, matte};

pub(super) struct Work {
    path: PathBuf,
    before: Project,
    rx: mpsc::Receiver<Message>,
    pub(super) progress: String,
}
enum Message {
    Progress(String),
    Done(Box<Result<Project, String>>),
}
pub(super) struct Cutout {
    seconds: f64,
    outline: f32,
    word: String,
    model_ready: bool,
}
impl Default for Cutout {
    fn default() -> Self {
        Self {
            seconds: 2.0,
            outline: 0.008,
            word: String::new(),
            model_ready: matte::ready(),
        }
    }
}

pub(super) fn source_set(asset: &str, field: &str, value: serde_json::Value) -> Op {
    Op::Set {
        path: format!(
            "/sources/{}/{field}",
            asset.replace('~', "~0").replace('/', "~1")
        ),
        value,
    }
}

// Merge only worker-owned fields. Editing while a download runs never overwrites newer work.
fn completed_ops(before: &Project, current: &Project, next: &Project) -> Result<Vec<Op>> {
    let mut changes = Vec::new();
    for (field, old, now, new) in [
        (
            "assets",
            json!(before.assets),
            json!(current.assets),
            json!(next.assets),
        ),
        (
            "sources",
            json!(before.sources),
            json!(current.sources),
            json!(next.sources),
        ),
        (
            "tracks",
            json!(before.tracks),
            json!(current.tracks),
            json!(next.tracks),
        ),
    ] {
        if old != new {
            anyhow::ensure!(
                old == now,
                "The {field} changed while this job ran. Your edits are safe; run the action again. Downloaded files remain in the workspace."
            );
            changes.push(Op::Set {
                path: format!("/{field}"),
                value: new,
            });
        }
    }
    Ok(changes)
}

impl Studio {
    fn start_documentary(
        &mut self,
        label: &str,
        work: impl FnOnce(Workspace, Project, PathBuf, &mut dyn FnMut(&str)) -> Result<Project>
        + Send
        + 'static,
    ) {
        if self.doc_work.is_some() {
            return;
        }
        if let Err(error) = self.save() {
            self.notice(format!("{error:#}"));
            return;
        }
        let Some(doc) = &self.document else {
            return;
        };
        let before = doc.project.clone();
        let project = before.clone();
        let dir = doc.dir().to_owned();
        let workspace = self.workspace.clone();
        let (tx, rx) = mpsc::channel();
        self.doc_work = Some(Work {
            path: doc.path.clone(),
            before,
            rx,
            progress: label.into(),
        });
        thread::spawn(move || {
            let result = work(workspace, project, dir, &mut |line| {
                let _ = tx.send(Message::Progress(line.into()));
            })
            .map_err(|error| format!("{error:#}"));
            let _ = tx.send(Message::Done(Box::new(result)));
        });
    }

    pub(super) fn poll_documentary(&mut self) {
        let mut done = None;
        if let Some(work) = &mut self.doc_work {
            loop {
                match work.rx.try_recv() {
                    Ok(Message::Progress(line)) => work.progress = line,
                    Ok(Message::Done(result)) => {
                        done = Some(*result);
                        break;
                    }
                    Err(mpsc::TryRecvError::Disconnected) => {
                        done = Some(Err("Background job stopped before completing".into()));
                        break;
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                }
            }
        }
        let Some(result) = done else {
            return;
        };
        let work = self.doc_work.take().expect("completed job");
        let result = result.map_err(anyhow::Error::msg).and_then(|next| {
            self.reload_external(false)?;
            let doc = self.document.as_mut().context("Project was closed; downloaded files remain in the workspace")?;
            anyhow::ensure!(doc.path == work.path, "Project changed; downloaded files remain in the workspace. Reopen the original project and run the action again.");
            let changes = completed_ops(&work.before, &doc.project, &next)?;
            // Document::apply is the inspector's validated undo/autosave path.
            if doc.apply(&changes)? {
                self.selection.retain(|id| doc.project.find(id).is_some());
                self.playhead = self.playhead.min(doc.project.duration());
                self.invalidate_preview();
                self.queue_waveforms();
            }
            self.save()?;
            self.reload_external(true)?;
            Ok(())
        });
        match result {
            Ok(()) => {
                self.cutout.model_ready = matte::ready();
                self.notice("Added to the project. Undo is available.");
            }
            Err(error) => self.notice(format!("{error:#}")),
        }
    }

    pub(super) fn fetch_footage(&mut self, id: String, at_playhead: bool) {
        let start = self.playhead;
        self.start_documentary(
            "Downloading footage",
            move |workspace, mut project, dir, progress| {
                progress("Downloading and checking footage");
                let fetched = footage::fetch(&workspace, &id)?;
                progress("Adding licence and source record");
                let asset = footage::add_to_project(
                    &workspace,
                    &mut project,
                    &dir,
                    &fetched,
                    AssetRole::Broll,
                )?;
                if at_playhead {
                    let media = project
                        .asset(&asset)
                        .context("Downloaded asset is missing")?;
                    let duration = if media.kind == AssetKind::Image {
                        2.0
                    } else {
                        media.duration.min(3.0)
                    };
                    let track = project
                        .tracks
                        .iter()
                        .find(|t| t.id == "broll" && t.kind == TrackKind::Overlay)
                        .or_else(|| project.tracks.iter().find(|t| t.kind == TrackKind::Overlay))
                        .map(|t| t.id.clone());
                    let track = match track {
                        Some(track) => track,
                        None => ops::apply(
                            &mut project,
                            &Op::AddTrack {
                                kind: TrackKind::Overlay,
                                name: Some("B-roll".into()),
                            },
                        )?
                        .items
                        .first()
                        .cloned()
                        .context("B-roll track was not created")?,
                    };
                    let mut op = add_clip(asset, start);
                    if let Op::AddClip {
                        track: target,
                        duration: length,
                        ..
                    } = &mut op
                    {
                        *target = Some(track);
                        *length = Some(duration);
                    }
                    ops::apply(&mut project, &op)?;
                }
                Ok(project)
            },
        );
    }

    pub(super) fn sources_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("Sources");
        ui.small("Check each page and its licence before approving rights. Credits belong in the video description.");
        let Some(doc) = &self.document else {
            return;
        };
        let mut changes = Vec::new();
        let mut open_page = None;
        if doc.project.sources.is_empty() {
            ui.label("No found footage in this project yet.");
        }
        for (asset, source) in &doc.project.sources {
            ui.push_id(asset, |ui| {
                ui.group(|ui| {
                    ui.strong(&source.title);
                    let name = doc
                        .project
                        .asset(asset)
                        .map(|a| filename(&a.path))
                        .unwrap_or_else(|| asset.clone());
                    ui.label(format!("{name} ({asset})"));
                    ui.label(format!("{} · {}", source.provider, source.license));
                    if let Some(author) = &source.author {
                        ui.label(format!("Author: {author}"));
                    }
                    if ui.button("Open source page").clicked() {
                        open_page = Some(source.url.clone());
                    }
                    if source.flag {
                        ui.colored_label(
                            if source.approved {
                                Color32::LIGHT_GREEN
                            } else {
                                Color32::LIGHT_RED
                            },
                            source
                                .flag_reason
                                .as_deref()
                                .unwrap_or("Rights need review"),
                        );
                    }
                    // Approving only means something for a flagged file; clear licences need no click.
                    if source.flag && source.approved {
                        ui.label("Rights approved by you");
                    }
                    if source.flag
                        && !source.approved
                        && ui.button("Approve (I checked the rights)").clicked()
                    {
                        changes.push(source_set(asset, "approved", json!(true)));
                    }
                    let reason = self.source_reasons.entry(asset.clone()).or_default();
                    ui.add(
                        egui::TextEdit::singleline(reason)
                            .hint_text("Reason to flag")
                            .desired_width(f32::INFINITY),
                    );
                    if ui
                        .add_enabled(!reason.trim().is_empty(), egui::Button::new("Flag"))
                        .clicked()
                    {
                        changes.extend([
                            source_set(asset, "approved", json!(false)),
                            source_set(asset, "flag", json!(true)),
                            source_set(asset, "flag_reason", json!(reason.trim())),
                        ]);
                    }
                });
            });
        }
        let mut credits = doc
            .project
            .sources
            .values()
            .filter_map(|source| source.credit.as_deref())
            .collect::<Vec<_>>()
            .join("\n");
        ui.separator();
        ui.strong("Credits for video description");
        if credits.is_empty() {
            ui.weak("No credit lines recorded.");
        } else {
            if ui.button("Copy credits").clicked() {
                ui.ctx().copy_text(credits.clone());
            }
            ui.add(
                egui::TextEdit::multiline(&mut credits)
                    .desired_width(f32::INFINITY)
                    .desired_rows(6)
                    .interactive(false),
            );
        }
        self.apply(changes);
        if let Some(url) = open_page {
            match url.split_once(':') {
                Some(("https" | "http", _)) => {
                    if let Err(error) = open::that(url) {
                        self.notice(format!("Could not open source page: {error:#}"));
                    }
                }
                _ => self.notice("Source page must be an HTTP or HTTPS address"),
            }
        }
    }

    pub(super) fn cutout_ui(&mut self, ui: &mut egui::Ui) {
        let Some(doc) = &self.document else {
            return;
        };
        let selected = self
            .selection
            .iter()
            .next()
            .and_then(|id| doc.project.find(id));
        let Some((track, index)) = selected else {
            return;
        };
        if doc.project.tracks[track].kind != TrackKind::Main {
            return;
        }
        let item = &doc.project.tracks[track].items[index];
        let Some(clip) = item.clip() else {
            return;
        };
        if !doc
            .project
            .asset(&clip.asset)
            .is_some_and(|a| a.kind == AssetKind::Video)
        {
            return;
        }
        let from = self.playhead.max(item.start);
        let available = (item.end() - from).max(0.0);
        ui.separator();
        ui.strong("Cut out the speaker");
        if !self.cutout.model_ready {
            ui.label("First use downloads the model (about 26 MB), once.");
        }
        ui.label("Seconds from playhead (clamped to this clip)");
        ui.add(
            egui::DragValue::new(&mut self.cutout.seconds)
                .range(0.1..=60.0)
                .speed(0.1)
                .suffix(" s"),
        );
        ui.add(egui::Slider::new(&mut self.cutout.outline, 0.0..=0.05).text("Outline"));
        ui.add(
            egui::TextEdit::singleline(&mut self.cutout.word)
                .hint_text("Optional mega word")
                .desired_width(f32::INFINITY),
        );
        let range = cutout_range(
            item.start,
            item.end(),
            self.playhead,
            self.cutout.seconds,
            doc.project.canvas.fps,
        );
        if let Some((from, to)) = range {
            ui.weak(format!("{from:.2} s to {to:.2} s"));
        }
        if available <= 0.0 || range.is_none() {
            ui.weak("Move the playhead inside this clip.");
        }
        if ui
            .add_enabled(
                self.doc_work.is_none() && range.is_some(),
                egui::Button::new("Cut out the speaker"),
            )
            .clicked()
        {
            let (from, to) = range.expect("enabled range");
            let outline = self.cutout.outline;
            let word = self.cutout.word.trim().to_owned();
            self.start_documentary(
                "Preparing speaker cut-out",
                move |workspace, mut project, dir, progress| {
                    matte::cutout(&workspace, &mut project, &dir, from, to, outline, progress)?;
                    if !word.is_empty() {
                        ops::apply(
                            &mut project,
                            &Op::AddText {
                                text: word,
                                start: from,
                                duration: to - from,
                                preset: TextPreset::Mega,
                                position: None,
                                style: None,
                                anim_in: None,
                                anim_out: None,
                                track: None,
                            },
                        )?;
                    }
                    Ok(project)
                },
            );
        }
    }
}

fn cutout_range(start: f64, end: f64, playhead: f64, seconds: f64, fps: f64) -> Option<(f64, f64)> {
    if playhead < start || playhead >= end {
        return None;
    }
    let to = (playhead + seconds).min(end);
    (to - playhead >= 1.0 / fps).then_some((playhead, to))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn worker_merge_keeps_newer_edits_and_cutout_stays_in_clip() {
        let before = Project::new("test", VideoKind::Documentary, Canvas::vertical());
        let mut next = before.clone();
        next.sources.insert("asset".into(), Source::default());
        let mut current = before.clone();
        current.name = "new name".into();
        let changes = completed_ops(&before, &current, &next).unwrap();
        assert_eq!(changes.len(), 1);
        assert_eq!(current.name, "new name");
        current.sources.insert(
            "asset".into(),
            Source {
                approved: true,
                ..Source::default()
            },
        );
        assert!(completed_ops(&before, &current, &next).is_err());
        assert_eq!(cutout_range(1.0, 3.0, 2.0, 2.0, 30.0), Some((2.0, 3.0)));
        assert_eq!(cutout_range(1.0, 3.0, 0.0, 2.0, 30.0), None);
        assert_eq!(cutout_range(1.0, 3.0, 3.0, 2.0, 30.0), None);
    }
}
