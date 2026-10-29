//! Free footage browser: search the licence-tracked providers and show thumbnails.
//!
//! Everything slow runs off the UI thread: one `footage::search` worker at a time and at most
//! [`MAX_THUMBS`] `ureq` thumbnail downloads. Workers answer over one channel; `show` (or `poll`)
//! drains it. Answers carry the search generation, so a slow old search can never overwrite a
//! newer one. The panel only returns the action the user asked for; the app does the work.

use std::collections::VecDeque;
use std::io::Cursor;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::LazyLock;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use eframe::egui::{
    self, Align2, Button, Checkbox, Color32, FontId, Label, Rect, RichText, ScrollArea, Sense,
    TextEdit, TextureHandle, TextureOptions, Ui, pos2, vec2,
};
use veecut_engine::footage::{self, Candidate, MediaKind, Provider, SearchOptions, SearchResult};

/// Thumbnail downloads running at the same time.
const MAX_THUMBS: usize = 3;
/// Results asked of each provider. Worst case, 5 providers x 4 thumbnails / 3 workers x
/// [`THUMB_SECS`] stays under the capture hook's 90 s wait, even when every download times out.
const PER_PROVIDER: usize = 4;
/// Deadline for one thumbnail download.
const THUMB_SECS: u64 = 6;
/// Largest thumbnail file read.
const THUMB_BYTES: u64 = 4 * 1024 * 1024;
/// Largest thumbnail width or height the decoder accepts, and its memory budget.
const MAX_DIM: u32 = 6000;
const MAX_ALLOC: u64 = 192 * 1024 * 1024;
/// Longest edge kept after decoding.
const THUMB_EDGE: u32 = 320;
const GAP: f32 = 8.0;
const MIN_CARD: f32 = 150.0;

#[derive(Clone, Copy, PartialEq)]
enum KindFilter {
    Any,
    Videos,
    Images,
}

enum Phase {
    Idle,
    Searching,
    Done,
    Failed(String),
}

enum Thumb {
    Queued,
    Loading,
    Ready(TextureHandle),
    Failed(String),
    /// The provider gave no thumbnail.
    Missing,
}

impl Thumb {
    fn settled(&self) -> bool {
        !matches!(self, Thumb::Queued | Thumb::Loading)
    }
}

struct Item {
    cand: Candidate,
    thumb: Thumb,
}

enum Msg {
    Search {
        generation: u64,
        result: Result<SearchResult, String>,
    },
    Thumb {
        generation: u64,
        idx: usize,
        result: Result<egui::ColorImage, String>,
    },
}

pub(crate) struct FootagePanel {
    query: String,
    kind: KindFilter,
    /// Checked providers, in `Provider::ALL` order.
    providers: [bool; 5],
    /// Providers that can be used (Pexels needs its key).
    available: [bool; 5],
    /// Bumped by every search; answers from older ones are dropped.
    generation: u64,
    phase: Phase,
    /// The query the shown results belong to.
    searched: String,
    warnings: Vec<String>,
    items: Vec<Item>,
    /// Indices into `items` still waiting for a download slot.
    queue: VecDeque<usize>,
    /// Running thumbnail workers, stale ones included.
    in_flight: usize,
    search_busy: bool,
    /// The newest search, waiting for the running worker to end.
    queued: Option<(String, SearchOptions)>,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
}

impl Default for FootagePanel {
    fn default() -> Self {
        let live = Provider::available();
        let available = Provider::ALL.map(|p| live.contains(&p));
        let (tx, rx) = mpsc::channel();
        FootagePanel {
            query: String::new(),
            kind: KindFilter::Any,
            providers: available,
            available,
            generation: 0,
            phase: Phase::Idle,
            searched: String::new(),
            warnings: Vec::new(),
            items: Vec::new(),
            queue: VecDeque::new(),
            in_flight: 0,
            search_busy: false,
            queued: None,
            tx,
            rx,
        }
    }
}

impl FootagePanel {
    /// Draws the panel. Returns `(candidate id, true)` for "Add at playhead" and
    /// `(candidate id, false)` for "Add" (to the project without placing it); it changes nothing else.
    pub(crate) fn show(&mut self, ui: &mut Ui, can_add: bool) -> Option<(String, bool)> {
        self.poll(ui.ctx());
        self.controls(ui);
        ui.separator();
        self.status(ui);
        let width = ui.available_width();
        let cols = columns(width);
        let card_w = ((width - GAP * (cols - 1) as f32) / cols as f32).max(1.0);
        let mut action = None;
        ScrollArea::vertical()
            .id_salt("footage_results")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                egui::Grid::new("footage_grid")
                    .num_columns(cols)
                    .spacing([GAP, GAP])
                    .show(ui, |ui| {
                        for (i, item) in self.items.iter().enumerate() {
                            ui.vertical(|ui| {
                                if let Some(a) = card(ui, item, card_w, can_add) {
                                    action = Some(a);
                                }
                            });
                            if (i + 1) % cols == 0 {
                                ui.end_row();
                            }
                        }
                    });
            });
        action
    }

    /// Starts a search with the current kind and provider choices. Results replace the old ones.
    pub(crate) fn search(&mut self, query: String) {
        self.query = query;
        self.start();
    }

    /// True once a search has finished (results or an error) and every thumbnail is settled
    /// (shown, failed or missing), so a capture shows the final picture.
    pub(crate) fn ready(&self) -> bool {
        matches!(self.phase, Phase::Done | Phase::Failed(_))
            && self.items.iter().all(|i| i.thumb.settled())
    }

    /// Takes finished worker answers and starts queued downloads. `show` calls this.
    pub(crate) fn poll(&mut self, ctx: &egui::Context) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Search { generation, result } => self.search_done(generation, result),
                Msg::Thumb {
                    generation,
                    idx,
                    result,
                } => {
                    self.in_flight = self.in_flight.saturating_sub(1);
                    if generation == self.generation
                        && let Some(item) = self.items.get_mut(idx)
                    {
                        item.thumb = match result {
                            Ok(img) => Thumb::Ready(ctx.load_texture(
                                format!("footage-thumb:{}", item.cand.id),
                                img,
                                TextureOptions::LINEAR,
                            )),
                            Err(e) => Thumb::Failed(e),
                        };
                    }
                }
            }
        }
        self.pump();
        if self.busy() {
            ctx.request_repaint_after(Duration::from_millis(50));
        }
    }

    fn busy(&self) -> bool {
        matches!(self.phase, Phase::Searching) || self.in_flight > 0 || !self.queue.is_empty()
    }

    fn controls(&mut self, ui: &mut Ui) {
        let mut submit = false;
        ui.horizontal(|ui| {
            let button_w = 70.0;
            let r = ui.add(
                TextEdit::singleline(&mut self.query)
                    .hint_text("Search free footage and pictures")
                    .desired_width((ui.available_width() - button_w - GAP).max(60.0)),
            );
            submit |= r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            submit |= ui
                .add_sized([button_w, 0.0], Button::new("Search"))
                .clicked();
        });
        ui.horizontal_wrapped(|ui| {
            for (k, label) in [
                (KindFilter::Any, "Any"),
                (KindFilter::Videos, "Videos"),
                (KindFilter::Images, "Images"),
            ] {
                ui.selectable_value(&mut self.kind, k, label);
            }
        });
        ui.horizontal_wrapped(|ui| {
            for (i, p) in Provider::ALL.into_iter().enumerate() {
                let name = match p {
                    Provider::Nasa => "NASA",
                    Provider::Commons => "Commons",
                    Provider::Archive => "Archive",
                    Provider::Openverse => "Openverse",
                    Provider::Pexels => "Pexels",
                };
                let r = ui.add_enabled(
                    self.available[i],
                    Checkbox::new(&mut self.providers[i], name),
                );
                if !self.available[i] {
                    r.on_disabled_hover_text("Set PEXELS_API_KEY to search Pexels");
                }
            }
        });
        if submit {
            self.start();
        }
    }

    fn status(&self, ui: &mut Ui) {
        let warn = ui.visuals().warn_fg_color;
        let error = ui.visuals().error_fg_color;
        match &self.phase {
            Phase::Idle => {
                ui.weak("Type what to look for, pick providers, then press Search.");
            }
            Phase::Searching => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(format!("Searching for \"{}\"...", self.searched));
                });
            }
            Phase::Failed(e) => {
                ui.colored_label(error, e);
            }
            Phase::Done if self.items.is_empty() => {
                ui.label(format!(
                    "No results for \"{}\". Try other words, another kind or more providers.",
                    self.searched
                ));
            }
            Phase::Done => {
                let waiting = self.items.iter().filter(|i| !i.thumb.settled()).count();
                let mut line = format!("{} results for \"{}\"", self.items.len(), self.searched);
                if waiting > 0 {
                    line.push_str(&format!(" - loading {waiting} thumbnails"));
                }
                ui.label(line);
            }
        }
        for w in &self.warnings {
            ui.label(RichText::new(w).small().color(warn));
        }
    }

    fn start(&mut self) {
        // A new search always retires the old one, even when it cannot start.
        self.generation += 1;
        self.items.clear();
        self.queue.clear();
        self.warnings.clear();
        self.queued = None;
        let query = self.query.trim().to_string();
        if query.is_empty() {
            self.phase =
                Phase::Failed("Say what to look for, for example \"rocket launch\".".into());
            return;
        }
        let providers: Vec<Provider> = Provider::ALL
            .into_iter()
            .enumerate()
            .filter(|(i, _)| self.providers[*i] && self.available[*i])
            .map(|(_, p)| p)
            .collect();
        if providers.is_empty() {
            self.phase = Phase::Failed("Pick at least one provider.".into());
            return;
        }
        let opts = SearchOptions {
            kind: match self.kind {
                KindFilter::Any => None,
                KindFilter::Videos => Some(MediaKind::Video),
                KindFilter::Images => Some(MediaKind::Image),
            },
            providers,
            per_provider: PER_PROVIDER,
        };
        self.searched = query.clone();
        self.phase = Phase::Searching;
        // ponytail: one search worker at a time, so spamming Search cannot pile up threads;
        // a newer search waits for the running one (it ends within the engine's HTTP timeouts).
        if self.search_busy {
            self.queued = Some((query, opts));
        } else {
            self.spawn_search(query, opts);
        }
    }

    fn spawn_search(&mut self, query: String, opts: SearchOptions) {
        let (tx, generation) = (self.tx.clone(), self.generation);
        let spawned = std::thread::Builder::new()
            .name("footage-search".into())
            .spawn(move || {
                let result = catch_unwind(AssertUnwindSafe(|| footage::search(&query, &opts)))
                    .unwrap_or_else(|_| Err(anyhow::anyhow!("the search crashed")))
                    .map_err(|e| format!("{e:#}"));
                let _ = tx.send(Msg::Search { generation, result });
            });
        match spawned {
            Ok(_) => self.search_busy = true,
            Err(e) => self.phase = Phase::Failed(format!("Could not start the search: {e}")),
        }
    }

    fn search_done(&mut self, generation: u64, result: Result<SearchResult, String>) {
        self.search_busy = false;
        if generation == self.generation {
            match result {
                Ok(res) => {
                    self.warnings = res.warnings;
                    for (i, cand) in res.candidates.into_iter().enumerate() {
                        let thumb = if cand.thumb_url.is_empty() {
                            Thumb::Missing
                        } else if !is_http(&cand.thumb_url) {
                            Thumb::Failed("the thumbnail link is not an http(s) address".into())
                        } else {
                            self.queue.push_back(i);
                            Thumb::Queued
                        };
                        self.items.push(Item { cand, thumb });
                    }
                    self.phase = Phase::Done;
                }
                Err(e) => self.phase = Phase::Failed(e),
            }
        }
        if let Some((query, opts)) = self.queued.take() {
            self.spawn_search(query, opts);
        }
    }

    /// Starts downloads until [`MAX_THUMBS`] workers run.
    fn pump(&mut self) {
        while self.in_flight < MAX_THUMBS {
            let Some(idx) = self.queue.pop_front() else {
                break;
            };
            let Some(item) = self.items.get_mut(idx) else {
                continue;
            };
            let (url, generation, tx) = (
                item.cand.thumb_url.clone(),
                self.generation,
                self.tx.clone(),
            );
            let spawned = std::thread::Builder::new()
                .name("footage-thumb".into())
                .spawn(move || {
                    let result = catch_unwind(AssertUnwindSafe(|| {
                        fetch_thumb(&url).and_then(|b| decode(&b))
                    }))
                    .unwrap_or_else(|_| Err("the thumbnail worker crashed".into()));
                    let _ = tx.send(Msg::Thumb {
                        generation,
                        idx,
                        result,
                    });
                });
            match spawned {
                Ok(_) => {
                    item.thumb = Thumb::Loading;
                    self.in_flight += 1;
                }
                Err(e) => item.thumb = Thumb::Failed(format!("could not start a download: {e}")),
            }
        }
    }
}

fn is_http(url: &str) -> bool {
    url.starts_with("https://") || url.starts_with("http://")
}

static HTTP: LazyLock<ureq::Agent> = LazyLock::new(|| {
    ureq::Agent::config_builder()
        .user_agent("VeeCutStudio/0.1 (https://github.com/Demonbane18/veecut-studio)")
        .timeout_connect(Some(Duration::from_secs(4)))
        .timeout_global(Some(Duration::from_secs(THUMB_SECS)))
        .max_redirects(3)
        .build()
        .into()
});

/// Downloads one thumbnail: http(s) only, redirects capped, [`THUMB_SECS`] deadline, at most
/// [`THUMB_BYTES`] read.
fn fetch_thumb(url: &str) -> Result<Vec<u8>, String> {
    let mut resp = HTTP.get(url).call().map_err(|e| match e {
        ureq::Error::StatusCode(code) => format!("the server answered HTTP {code}"),
        ureq::Error::Timeout(_) => "the server did not answer in time".into(),
        e => format!("download failed: {e}"),
    })?;
    let bytes = resp
        .body_mut()
        .with_config()
        .limit(THUMB_BYTES)
        .read_to_vec()
        .map_err(|e| match e {
            ureq::Error::BodyExceedsLimit(_) => {
                format!("thumbnail is over {} MB", THUMB_BYTES / (1024 * 1024))
            }
            e => format!("download failed: {e}"),
        })?;
    if bytes.is_empty() {
        return Err("the server sent an empty file".into());
    }
    Ok(bytes)
}

/// Decodes PNG, JPEG or WebP within [`MAX_DIM`] and [`MAX_ALLOC`], then shrinks it.
fn decode(bytes: &[u8]) -> Result<egui::ColorImage, String> {
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_DIM);
    limits.max_image_height = Some(MAX_DIM);
    limits.max_alloc = Some(MAX_ALLOC);
    let mut reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| format!("could not read the file: {e}"))?;
    reader.limits(limits);
    let img = reader
        .decode()
        .map_err(|e| format!("could not decode the picture: {e}"))?
        .thumbnail(THUMB_EDGE, THUMB_EDGE)
        .to_rgba8();
    let size = [img.width() as usize, img.height() as usize];
    Ok(egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw()))
}

/// Card columns for a panel width.
fn columns(width: f32) -> usize {
    if !width.is_finite() {
        return 1;
    }
    (((width + GAP) / (MIN_CARD + GAP)).floor() as usize).clamp(1, 8)
}

fn fmt_duration(secs: f64) -> String {
    let s = secs.max(0.0).round() as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

fn card(ui: &mut Ui, item: &Item, w: f32, can_add: bool) -> Option<(String, bool)> {
    let c = &item.cand;
    ui.set_width(w);
    let (rect, _) = ui.allocate_exact_size(vec2(w, (w * 9.0 / 16.0).round()), Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 4.0, ui.visuals().extreme_bg_color);
    let weak = ui.visuals().weak_text_color();
    let note = |text: &str, color: Color32| {
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            text,
            FontId::proportional(12.0),
            color,
        );
    };
    match &item.thumb {
        Thumb::Ready(tex) => {
            let t = tex.size_vec2();
            let scale = (rect.width() / t.x).min(rect.height() / t.y);
            let fitted = Rect::from_center_size(rect.center(), t * scale);
            painter.image(
                tex.id(),
                fitted,
                Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                Color32::WHITE,
            );
        }
        Thumb::Queued | Thumb::Loading => note("Loading...", weak),
        Thumb::Missing => note("No preview from provider", weak),
        Thumb::Failed(e) => {
            ui.put(
                Rect::from_center_size(rect.center(), rect.size() - vec2(8.0, 8.0)),
                Label::new(
                    RichText::new(format!("Thumbnail failed: {e}"))
                        .small()
                        .color(ui.visuals().error_fg_color),
                ),
            );
        }
    }
    if c.kind == MediaKind::Video {
        let text = if c.duration > 0.0 {
            fmt_duration(c.duration)
        } else {
            "video".into()
        };
        let galley = painter.layout_no_wrap(text, FontId::proportional(11.0), Color32::WHITE);
        let badge = Rect::from_min_size(
            rect.right_bottom() - galley.size() - vec2(10.0, 8.0),
            galley.size() + vec2(8.0, 4.0),
        );
        painter.rect_filled(badge, 3.0, Color32::from_black_alpha(170));
        painter.galley(badge.min + vec2(4.0, 2.0), galley, Color32::WHITE);
    }

    let title = if c.title.trim().is_empty() {
        "(untitled)"
    } else {
        c.title.trim()
    };
    ui.add(Label::new(RichText::new(title).strong()).truncate())
        .on_hover_text(title);
    let mut info = format!("{} - {}", c.provider.label(), c.license);
    if let Some(a) = c.author.as_deref().filter(|a| !a.trim().is_empty()) {
        info.push_str(&format!(" - {a}"));
    }
    ui.label(RichText::new(info).small().weak());
    let mut size = match c.kind {
        MediaKind::Video if c.duration > 0.0 => format!("Video, {}", fmt_duration(c.duration)),
        MediaKind::Video => "Video, length unknown".to_string(),
        MediaKind::Image => "Picture".to_string(),
    };
    if c.width > 0 && c.height > 0 {
        size.push_str(&format!(", {}x{}", c.width, c.height));
    }
    ui.label(RichText::new(size).small().weak());
    if c.flag {
        let reason = c.flag_reason.as_deref().unwrap_or("rights are unclear");
        ui.label(
            RichText::new(format!("Check rights: {reason}"))
                .small()
                .color(ui.visuals().warn_fg_color),
        );
    }

    let mut action = None;
    ui.horizontal_wrapped(|ui| {
        let add = ui
            .add_enabled(can_add, Button::new("Add at playhead"))
            .on_disabled_hover_text("Open a project first");
        if add.clicked() {
            action = Some((c.id.clone(), true));
        }
        if ui.add_enabled(can_add, Button::new("Add")).clicked() {
            action = Some((c.id.clone(), false));
        }
        if is_http(&c.page_url) {
            ui.hyperlink_to("Source", &c.page_url);
        }
    });
    action
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_and_duration_helpers() {
        assert_eq!(columns(100.0), 1);
        assert_eq!(columns(f32::INFINITY), 1);
        assert_eq!(columns(2.0 * MIN_CARD + GAP), 2);
        assert_eq!(columns(4000.0), 8);
        assert_eq!(fmt_duration(5.4), "0:05");
        assert_eq!(fmt_duration(125.0), "2:05");
        assert_eq!(fmt_duration(3725.0), "1:02:05");
    }
}
