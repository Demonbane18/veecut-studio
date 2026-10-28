//! Timeline gestures preview locally and emit editor operations only on release.

use std::collections::{HashMap, HashSet};

use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, Stroke, StrokeKind, Vec2};
use veecut_engine::{ops::Op, project::*};

const GUTTER: f32 = 124.0;
const RULER: f32 = 34.0;
const ROW: f32 = 66.0;
const BASE_PPS: f32 = 50.0;
const EDGE: f32 = 7.0;
const ACCENT: Color32 = Color32::from_rgb(116, 194, 255);

pub(crate) struct Timeline {
    pub zoom: f32,
    pub snapping: bool,
    scroll_x: f32,
    gesture: Option<Gesture>,
    scrubbing: bool,
}

impl Default for Timeline {
    fn default() -> Self {
        Self {
            zoom: 1.0,
            snapping: true,
            scroll_x: 0.0,
            gesture: None,
            scrubbing: false,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum EditMode {
    Move,
    Left,
    Right,
}

struct Gesture {
    item: String,
    mode: EditMode,
    press: Pos2,
    press_time: f64,
    active: bool,
}

impl Timeline {
    pub(crate) fn show(
        &mut self,
        ui: &mut egui::Ui,
        project: &Project,
        selection: &mut HashSet<String>,
        playhead: &mut f64,
        waveforms: &HashMap<String, Vec<f32>>,
    ) -> Vec<Op> {
        let mut ops = Vec::new();
        let old_zoom = self.zoom.clamp(0.1, 100.0);
        self.zoom = old_zoom;
        ui.horizontal_wrapped(|ui| {
            ui.strong("Timeline");
            if ui
                .button("Split")
                .on_hover_text("Split tracks at playhead")
                .clicked()
                && project
                    .tracks
                    .iter()
                    .flat_map(|t| &t.items)
                    .any(|i| i.start + 1e-6 < *playhead && i.end() - 1e-6 > *playhead)
            {
                ops.push(Op::Split {
                    at: *playhead,
                    track: None,
                });
                self.gesture = None;
            }
            if ui
                .add_enabled(!selection.is_empty(), egui::Button::new("Delete"))
                .clicked()
            {
                ops.extend(
                    project
                        .tracks
                        .iter()
                        .flat_map(|t| &t.items)
                        .filter(|i| selection.contains(&i.id))
                        .map(|i| Op::Delete { item: i.id.clone() }),
                );
                selection.clear();
                self.gesture = None;
            }
            ui.separator();
            ui.checkbox(&mut self.snapping, "Snap")
                .on_hover_text("Hold Shift to disable snapping");
            if ui.button("/ 1.7").on_hover_text("Zoom out").clicked() {
                self.zoom /= 1.7;
            }
            if ui.button("x 1.7").on_hover_text("Zoom in").clicked() {
                self.zoom *= 1.7;
            }
            ui.label(format!("{:.1}x", self.zoom.clamp(0.1, 100.0)));
            ui.monospace(timecode(*playhead));
        });

        let area = ui.available_rect_before_wrap();
        let zooming = ui.input(|i| i.modifiers.ctrl || i.modifiers.command);
        if zooming && ui.rect_contains_pointer(area) && self.gesture.is_none() && !self.scrubbing {
            let wheel = ui.input(|i| {
                i.events
                    .iter()
                    .filter_map(|event| match event {
                        egui::Event::MouseWheel {
                            delta, modifiers, ..
                        } if modifiers.ctrl || modifiers.command => Some(delta.y),
                        _ => None,
                    })
                    .sum::<f32>()
            });
            self.zoom *= (wheel * 0.005).exp();
        }
        self.zoom = self.zoom.clamp(0.1, 100.0);
        if self.zoom != old_zoom {
            let anchor = ui
                .input(|i| i.pointer.hover_pos())
                .filter(|p| area.contains(*p))
                .map_or((area.width() - GUTTER).max(0.0) * 0.5, |p| {
                    (p.x - area.left() - GUTTER).max(0.0)
                });
            self.scroll_x = ((self.scroll_x + anchor) * self.zoom / old_zoom - anchor).max(0.0);
        }
        let pps = BASE_PPS * self.zoom;
        let width = (GUTTER + time_to_x(project.duration().max(*playhead) + 10.0, 0.0, pps))
            .max(area.width());
        let height = RULER + ROW * project.tracks.len() as f32;
        let scroll = egui::ScrollArea::both()
            .id_salt("timeline-scroll")
            .auto_shrink([false, false])
            .horizontal_scroll_offset(self.scroll_x)
            .scroll_source(egui::scroll_area::ScrollSource {
                mouse_wheel: !zooming,
                ..egui::scroll_area::ScrollSource::SCROLL_BAR
            })
            .show_viewport(ui, |ui, viewport| {
                let (canvas, response) = ui.allocate_exact_size(
                    Vec2::new(width, height.max(viewport.height())),
                    egui::Sense::click_and_drag(),
                );
                let origin = canvas.left() + GUTTER;
                let gutter_left = canvas.left() + viewport.left();
                let visible = canvas.intersect(ui.clip_rect());
                let time_clip = Rect::from_min_max(
                    Pos2::new((gutter_left + GUTTER).max(visible.left()), visible.top()),
                    visible.max,
                );
                let painter = ui.painter().with_clip_rect(time_clip);
                painter.rect_filled(canvas, 0.0, Color32::from_rgb(23, 26, 32));
                let pointer = ui.input(|i| i.pointer.interact_pos());
                let inside =
                    pointer.is_some_and(|p| time_clip.contains(p)) && response.contains_pointer();
                let pressed = ui.input(|i| i.pointer.primary_pressed());
                let released = ui.input(|i| i.pointer.primary_released());
                let down = ui.input(|i| i.pointer.primary_down());
                let shift = ui.input(|i| i.modifiers.shift);
                let additive = ui.input(|i| i.modifiers.shift || i.modifiers.command);
                let payload = egui::DragAndDrop::payload::<String>(ui.ctx());
                let mut hovered_item = None;
                let mut hovered_track = None;

                // Iteration order keeps the magnetic track first without copying tracks.
                let tracks = project
                    .tracks
                    .iter()
                    .filter(|t| t.kind == TrackKind::Main)
                    .chain(project.tracks.iter().filter(|t| t.kind != TrackKind::Main));
                for (row, track) in tracks.enumerate() {
                    let top = canvas.top() + RULER + row as f32 * ROW;
                    let row_rect = Rect::from_min_max(
                        Pos2::new(origin, top),
                        Pos2::new(canvas.right(), top + ROW),
                    );
                    if row_rect.intersects(time_clip) {
                        painter.rect_filled(
                            row_rect,
                            0.0,
                            if row % 2 == 0 {
                                Color32::from_rgb(29, 33, 40)
                            } else {
                                Color32::from_rgb(25, 29, 35)
                            },
                        );
                        painter.hline(
                            row_rect.x_range(),
                            row_rect.bottom(),
                            Stroke::new(1.0, Color32::from_gray(51)),
                        );
                    }
                    if inside && pointer.is_some_and(|p| row_rect.contains(p)) {
                        hovered_track = Some(track);
                    }
                    for item in &track.items {
                        let rect = item_rect(item.start, item.end(), top, origin, pps);
                        if rect.intersects(time_clip) {
                            paint_item(
                                &painter,
                                rect,
                                item,
                                project,
                                selection.contains(&item.id),
                                pps,
                                waveforms,
                            );
                        }
                        if inside && pointer.is_some_and(|p| rect.contains(p)) {
                            hovered_item = Some((track, item, rect));
                        }
                    }
                }

                paint_ruler(&painter, canvas, origin, pps, project);
                if pressed && inside && payload.is_none() {
                    if pointer.is_some_and(|p| p.y < canvas.top() + RULER) {
                        self.scrubbing = true;
                        self.gesture = None;
                    } else if !self.scrubbing {
                        if let (Some((_, item, rect)), Some(pos)) = (hovered_item, pointer) {
                            if additive {
                                if !selection.insert(item.id.clone()) {
                                    selection.remove(&item.id);
                                }
                            } else if !selection.contains(&item.id) || selection.len() != 1 {
                                selection.clear();
                                selection.insert(item.id.clone());
                            }
                            self.gesture = Some(Gesture {
                                item: item.id.clone(),
                                mode: edge_mode(rect, pos.x),
                                press: pos,
                                press_time: x_to_time(pos.x, origin, pps),
                                active: false,
                            });
                        } else if !additive {
                            selection.clear();
                        }
                    }
                }

                if self.scrubbing {
                    if (down || released)
                        && let Some(pos) = pointer
                    {
                        *playhead = x_to_time(pos.x, origin, pps);
                    }
                    if !down {
                        self.scrubbing = false;
                    }
                    ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                } else if let Some(gesture) = &mut self.gesture {
                    if let Some(pos) = pointer {
                        gesture.active |= pos.distance(gesture.press) >= 5.0;
                        if gesture.active
                            && let Some((ti, ii)) = project.find(&gesture.item)
                        {
                            let track = &project.tracks[ti];
                            let item = &track.items[ii];
                            let delta = x_to_time(pos.x, origin, pps) - gesture.press_time;
                            let snap = self.snapping && !shift;
                            let (start, end) = edit_bounds(
                                project,
                                item,
                                gesture.mode,
                                delta,
                                *playhead,
                                pps,
                                snap,
                            );
                            let row = if track.kind == TrackKind::Main {
                                0
                            } else {
                                project
                                    .tracks
                                    .iter()
                                    .filter(|t| t.kind == TrackKind::Main)
                                    .count()
                                    + project
                                        .tracks
                                        .iter()
                                        .filter(|t| t.kind != TrackKind::Main)
                                        .position(|t| t.id == track.id)
                                        .unwrap_or(0)
                            };
                            let top = canvas.top() + RULER + row as f32 * ROW;
                            let ghost = item_rect(start, end, top, origin, pps);
                            painter.rect(
                                ghost,
                                4.0,
                                Color32::from_rgba_unmultiplied(116, 194, 255, 65),
                                Stroke::new(2.0, ACCENT),
                                StrokeKind::Inside,
                            );
                            painter.with_clip_rect(ghost.intersect(time_clip)).text(
                                ghost.left_top() + Vec2::new(8.0, 7.0),
                                Align2::LEFT_TOP,
                                format!("{}  {}", timecode(start), timecode(end)),
                                FontId::monospace(11.0),
                                Color32::WHITE,
                            );
                            let mut index = None;
                            if gesture.mode == EditMode::Move && track.kind == TrackKind::Main {
                                let midpoint = x_to_time(pos.x, origin, pps);
                                let at = track
                                    .items
                                    .iter()
                                    .filter(|i| i.id != item.id)
                                    .take_while(|i| i.start + i.duration * 0.5 < midpoint)
                                    .count();
                                index = Some(at);
                                let insertion = track
                                    .items
                                    .iter()
                                    .filter(|i| i.id != item.id)
                                    .nth(at)
                                    .map_or_else(
                                        || {
                                            track
                                                .items
                                                .iter()
                                                .filter(|i| i.id != item.id)
                                                .last()
                                                .map_or(0.0, Item::end)
                                        },
                                        |i| i.start,
                                    );
                                painter.vline(
                                    time_to_x(insertion, origin, pps),
                                    top..=top + ROW,
                                    Stroke::new(3.0, ACCENT),
                                );
                            }
                            ui.ctx().set_cursor_icon(if gesture.mode == EditMode::Move {
                                egui::CursorIcon::Grabbing
                            } else {
                                egui::CursorIcon::ResizeHorizontal
                            });
                            if released {
                                match gesture.mode {
                                    EditMode::Move if index.is_some_and(|at| at != ii) => {
                                        ops.push(Op::Move {
                                            item: item.id.clone(),
                                            start: None,
                                            track: None,
                                            index,
                                        })
                                    }
                                    EditMode::Move
                                        if index.is_none() && (start - item.start).abs() > 1e-6 =>
                                    {
                                        ops.push(Op::Move {
                                            item: item.id.clone(),
                                            start: Some(start),
                                            track: None,
                                            index: None,
                                        })
                                    }
                                    EditMode::Left if (start - item.start).abs() > 1e-6 => ops
                                        .push(Op::Trim {
                                            item: item.id.clone(),
                                            start: Some(start),
                                            end: None,
                                        }),
                                    EditMode::Right if (end - item.end()).abs() > 1e-6 => {
                                        ops.push(Op::Trim {
                                            item: item.id.clone(),
                                            start: None,
                                            end: Some(end),
                                        })
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                    if !down {
                        self.gesture = None;
                    }
                } else if let Some((_, item, rect)) = hovered_item {
                    if let Some(pos) = pointer {
                        ui.ctx()
                            .set_cursor_icon(if edge_mode(rect, pos.x) == EditMode::Move {
                                egui::CursorIcon::Grab
                            } else {
                                egui::CursorIcon::ResizeHorizontal
                            });
                    }
                    response.clone().on_hover_text(format!(
                        "{}\n{} to {}\nDrag body to move; drag edges to trim",
                        item_label(item, project),
                        timecode(item.start),
                        timecode(item.end())
                    ));
                }

                if let (Some(asset_id), Some(track), Some(pos)) = (payload, hovered_track, pointer)
                {
                    if let Some(asset) = project.asset(asset_id.as_str()) {
                        let allowed = matches!(
                            track.kind,
                            TrackKind::Main | TrackKind::Overlay | TrackKind::Audio
                        ) && !(track.kind == TrackKind::Audio
                            && asset.kind == AssetKind::Image);
                        if allowed {
                            let at = x_to_time(pos.x, origin, pps);
                            painter.vline(
                                pos.x,
                                canvas.top() + RULER..=canvas.bottom(),
                                Stroke::new(2.0, ACCENT),
                            );
                            painter.text(
                                pos + Vec2::new(8.0, 8.0),
                                Align2::LEFT_TOP,
                                timecode(at),
                                FontId::monospace(12.0),
                                ACCENT,
                            );
                            if ui.input(|i| i.pointer.any_released()) {
                                egui::DragAndDrop::take_payload::<String>(ui.ctx());
                                ops.push(Op::AddClip {
                                    asset: asset_id.as_ref().clone(),
                                    track: Some(track.id.clone()),
                                    start: Some(at),
                                    index: None,
                                    src_in: 0.0,
                                    duration: None,
                                    frame: None,
                                    volume: None,
                                    duck: None,
                                });
                            }
                        } else {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::NotAllowed);
                            painter.text(
                                pos + Vec2::new(8.0, 8.0),
                                Align2::LEFT_TOP,
                                "Use a video or audio track",
                                FontId::proportional(12.0),
                                Color32::WHITE,
                            );
                        }
                    }
                }

                let x = time_to_x(*playhead, origin, pps);
                let red = Color32::from_rgb(255, 108, 119);
                painter.vline(x, canvas.y_range(), Stroke::new(1.5, red));
                painter.add(egui::Shape::convex_polygon(
                    vec![
                        Pos2::new(x - 6.0, canvas.top()),
                        Pos2::new(x + 6.0, canvas.top()),
                        Pos2::new(x + 6.0, canvas.top() + 8.0),
                        Pos2::new(x, canvas.top() + 14.0),
                        Pos2::new(x - 6.0, canvas.top() + 8.0),
                    ],
                    red,
                    Stroke::NONE,
                ));
                paint_gutter(ui.painter(), canvas, gutter_left, project);
            });
        self.scroll_x = scroll.state.clamped_offset().x;
        ops
    }
}

fn time_to_x(time: f64, origin: f32, pps: f32) -> f32 {
    origin + (time * f64::from(pps)) as f32
}

fn x_to_time(x: f32, origin: f32, pps: f32) -> f64 {
    (f64::from(x - origin) / f64::from(pps)).max(0.0)
}

fn snap_time(time: f64, targets: impl Iterator<Item = f64>, pps: f32) -> f64 {
    let mut best = time;
    let mut distance = 10.0 / f64::from(pps);
    for target in targets {
        let d = (time - target).abs();
        if target >= 0.0 && d <= distance + 1e-9 {
            best = target;
            distance = d;
        }
    }
    best
}

fn edit_bounds(
    project: &Project,
    item: &Item,
    mode: EditMode,
    delta: f64,
    playhead: f64,
    pps: f32,
    snapping: bool,
) -> (f64, f64) {
    let targets = || {
        project
            .tracks
            .iter()
            .flat_map(|t| &t.items)
            .filter(|i| i.id != item.id)
            .flat_map(|i| [i.start, i.end()])
            .chain([0.0, playhead])
    };
    let snap = |t| {
        if snapping {
            snap_time(t, targets(), pps)
        } else {
            t
        }
    };
    match mode {
        EditMode::Move => {
            let raw = (item.start + delta).max(0.0);
            let start = if snapping {
                snap_time(raw, targets().flat_map(|t| [t, t - item.duration]), pps)
            } else {
                raw
            };
            (start, start + item.duration)
        }
        EditMode::Left => {
            let min_duration = (1.0 / project.canvas.fps.max(1.0)).min(item.duration);
            let min = item
                .clip()
                .map_or(0.0, |c| (item.start - c.src_in / c.speed).max(0.0));
            let mut max = item.end() - min_duration;
            if let ItemBody::Caption(cap) = &item.body {
                if let Some(last) = cap.words.last() {
                    max = max.min(item.start + (last.start + last.end) * 0.5 - 1e-6);
                }
            }
            (
                snap(item.start + delta).clamp(min, max.max(min)),
                item.end(),
            )
        }
        EditMode::Right => {
            let min_duration = (1.0 / project.canvas.fps.max(1.0)).min(item.duration);
            let mut min = item.start + min_duration;
            if let ItemBody::Caption(cap) = &item.body {
                if let Some(first) = cap.words.first() {
                    min = min.max(item.start + (first.start + first.end) * 0.5 + 1e-6);
                }
            }
            let max = item
                .clip()
                .and_then(|c| project.asset(&c.asset).map(|a| (a, c)))
                .filter(|(a, _)| a.kind != AssetKind::Image && a.duration > 0.0)
                .map_or(f64::INFINITY, |(a, c)| {
                    item.start + (a.duration - c.src_in) / c.speed
                });
            (
                item.start,
                snap(item.end() + delta).clamp(min, max.max(min)),
            )
        }
    }
}

fn item_rect(start: f64, end: f64, top: f32, origin: f32, pps: f32) -> Rect {
    let left = time_to_x(start, origin, pps);
    Rect::from_min_max(
        Pos2::new(left, top + 5.0),
        Pos2::new(time_to_x(end, origin, pps).max(left + 3.0), top + ROW - 5.0),
    )
}

fn edge_mode(rect: Rect, x: f32) -> EditMode {
    let edge = EDGE.min(rect.width() / 3.0);
    if x <= rect.left() + edge {
        EditMode::Left
    } else if x >= rect.right() - edge {
        EditMode::Right
    } else {
        EditMode::Move
    }
}

fn item_label<'a>(item: &'a Item, project: &'a Project) -> &'a str {
    match &item.body {
        ItemBody::Text(text) => &text.text,
        ItemBody::Caption(caption) => caption.words.first().map_or("Caption", |w| w.text.as_str()),
        ItemBody::Clip(clip) => project
            .asset(&clip.asset)
            .and_then(|a| a.path.file_name())
            .and_then(|s| s.to_str())
            .unwrap_or(&clip.asset),
    }
}

fn paint_item(
    painter: &egui::Painter,
    rect: Rect,
    item: &Item,
    project: &Project,
    selected: bool,
    pps: f32,
    waveforms: &HashMap<String, Vec<f32>>,
) {
    let asset = item.clip().and_then(|c| project.asset(&c.asset));
    let color = match &item.body {
        ItemBody::Text(_) => Color32::from_rgb(93, 65, 124),
        ItemBody::Caption(_) => Color32::from_rgb(112, 83, 38),
        ItemBody::Clip(_) if asset.is_some_and(|a| a.kind == AssetKind::Audio) => {
            Color32::from_rgb(38, 91, 76)
        }
        ItemBody::Clip(_) => Color32::from_rgb(43, 71, 110),
    };
    painter.rect(
        rect,
        4.0,
        color,
        Stroke::new(
            if selected { 2.0 } else { 1.0 },
            if selected {
                ACCENT
            } else {
                color.gamma_multiply(1.5)
            },
        ),
        StrokeKind::Inside,
    );
    let clipped = painter.with_clip_rect(rect.shrink(3.0).intersect(painter.clip_rect()));
    clipped.text(
        rect.left_top() + Vec2::new(8.0, 7.0),
        Align2::LEFT_TOP,
        item_label(item, project),
        FontId::proportional(12.0),
        Color32::WHITE,
    );
    for x in [rect.left() + 3.0, rect.right() - 3.0] {
        painter.vline(
            x,
            rect.center().y - 6.0..=rect.center().y + 6.0,
            Stroke::new(
                1.0,
                if selected {
                    ACCENT
                } else {
                    Color32::from_gray(163)
                },
            ),
        );
    }
    if let (Some(clip), Some(asset)) = (item.clip(), asset) {
        if asset.kind == AssetKind::Audio || (asset.kind == AssetKind::Video && asset.has_audio) {
            if let Some(peaks) = waveforms.get(&clip.asset) {
                let wave = Rect::from_min_max(
                    Pos2::new(rect.left() + 4.0, rect.top() + 26.0),
                    Pos2::new(rect.right() - 4.0, rect.bottom() - 4.0),
                );
                let visible = wave.intersect(clipped.clip_rect());
                let mut x = visible.left();
                while x < visible.right() {
                    // Peaks cover 10 ms of source, not timeline time. Aggregate each screen column.
                    let from =
                        clip.src_in + f64::from((x - rect.left()).max(0.0) / pps) * clip.speed;
                    let to = clip.src_in
                        + f64::from((x + 2.0 - rect.left()).max(0.0) / pps) * clip.speed;
                    let a = ((from * 100.0).floor() as usize).min(peaks.len());
                    let b = ((to * 100.0).ceil() as usize)
                        .max(a.saturating_add(1))
                        .min(peaks.len());
                    let peak = peaks[a..b]
                        .iter()
                        .copied()
                        .filter(|v| v.is_finite())
                        .map(f32::abs)
                        .fold(0.0, f32::max)
                        .clamp(0.0, 1.0);
                    let half = peak * wave.height().max(0.0) * 0.5;
                    clipped.vline(
                        x,
                        wave.center().y - half..=wave.center().y + half,
                        Stroke::new(1.0, Color32::from_rgb(148, 226, 196)),
                    );
                    x += 2.0;
                }
            }
        }
    }
}

fn paint_ruler(painter: &egui::Painter, canvas: Rect, origin: f32, pps: f32, project: &Project) {
    let ruler = Rect::from_min_max(canvas.min, Pos2::new(canvas.right(), canvas.top() + RULER));
    painter.rect_filled(ruler, 0.0, Color32::from_rgb(34, 38, 46));
    let desired = 90.0 / f64::from(pps);
    let power = 10.0_f64.powf(desired.log10().floor());
    let step = [1.0, 2.0, 5.0, 10.0]
        .into_iter()
        .map(|n| n * power)
        .find(|s| *s >= desired)
        .unwrap_or(power * 10.0);
    let first = (x_to_time(painter.clip_rect().left(), origin, pps) / step).floor() as u64;
    let last = (x_to_time(painter.clip_rect().right(), origin, pps) / step).ceil() as u64;
    for tick in first..=last {
        let time = tick as f64 * step;
        let x = time_to_x(time, origin, pps);
        painter.vline(
            x,
            canvas.top() + 22.0..=canvas.top() + RULER,
            Stroke::new(1.0, Color32::from_gray(115)),
        );
        painter.text(
            Pos2::new(x + 4.0, canvas.top() + 3.0),
            Align2::LEFT_TOP,
            timecode(time),
            FontId::monospace(11.0),
            Color32::from_gray(210),
        );
    }
    for marker in &project.markers {
        let x = time_to_x(marker.at, origin, pps);
        if x < painter.clip_rect().left() || x > painter.clip_rect().right() {
            continue;
        }
        let color = if marker.done {
            Color32::from_gray(133)
        } else {
            Color32::from_rgb(247, 197, 91)
        };
        painter.vline(
            x,
            canvas.top() + 22.0..=canvas.bottom(),
            Stroke::new(0.5, color.gamma_multiply(0.6)),
        );
        painter.circle_filled(Pos2::new(x, canvas.top() + 26.0), 4.0, color);
        painter
            .with_clip_rect(
                Rect::from_min_max(
                    Pos2::new(x + 5.0, canvas.top() + 19.0),
                    Pos2::new((x + 150.0).min(canvas.right()), canvas.top() + RULER),
                )
                .intersect(painter.clip_rect()),
            )
            .text(
                Pos2::new(x + 7.0, canvas.top() + 21.0),
                Align2::LEFT_TOP,
                &marker.note,
                FontId::proportional(10.0),
                color,
            );
    }
}

fn paint_gutter(painter: &egui::Painter, canvas: Rect, left: f32, project: &Project) {
    let gutter = Rect::from_min_max(
        Pos2::new(left, canvas.top()),
        Pos2::new(left + GUTTER, canvas.bottom()),
    );
    let painter = painter.with_clip_rect(gutter.intersect(painter.clip_rect()));
    painter.rect_filled(gutter, 0.0, Color32::from_rgb(32, 36, 43));
    painter.text(
        Pos2::new(left + 10.0, canvas.top() + 10.0),
        Align2::LEFT_TOP,
        "Tracks",
        FontId::proportional(12.0),
        Color32::from_gray(210),
    );
    let tracks = project
        .tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Main)
        .chain(project.tracks.iter().filter(|t| t.kind != TrackKind::Main));
    for (row, track) in tracks.enumerate() {
        let top = canvas.top() + RULER + row as f32 * ROW;
        painter.hline(
            left..=left + GUTTER,
            top,
            Stroke::new(1.0, Color32::from_gray(54)),
        );
        painter.text(
            Pos2::new(left + 10.0, top + 13.0),
            Align2::LEFT_TOP,
            &track.name,
            FontId::proportional(12.0),
            Color32::from_gray(232),
        );
        let kind = match track.kind {
            TrackKind::Main => "Main video",
            TrackKind::Overlay => "Video overlay",
            TrackKind::Text => "Text",
            TrackKind::Captions => "Captions",
            TrackKind::Audio => "Audio",
        };
        let status = if track.hidden {
            "Hidden"
        } else if track.muted {
            "Muted"
        } else {
            kind
        };
        painter.text(
            Pos2::new(left + 10.0, top + 34.0),
            Align2::LEFT_TOP,
            status,
            FontId::proportional(11.0),
            Color32::from_gray(157),
        );
    }
    painter.vline(
        left + GUTTER - 1.0,
        canvas.y_range(),
        Stroke::new(1.0, Color32::from_gray(66)),
    );
}

fn timecode(time: f64) -> String {
    let millis = (time.max(0.0) * 1000.0).round() as u64;
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        millis / 3_600_000,
        millis / 60_000 % 60,
        millis / 1000 % 60,
        millis % 1000
    )
}

#[cfg(test)]
mod tests {
    use super::{snap_time, time_to_x, x_to_time};

    #[test]
    fn time_pixel_mapping_round_trips_zoom_and_scroll() {
        for zoom in [0.1, 1.0, 100.0] {
            for origin in [-143.0, 124.0] {
                for time in [0.0, 0.125, 12.5, 3600.0] {
                    let pps = 50.0 * zoom;
                    let actual = x_to_time(time_to_x(time, origin, pps), origin, pps);
                    assert!((actual - time).abs() < 0.001);
                }
                assert_eq!(x_to_time(origin - 20.0, origin, 50.0 * zoom), 0.0);
            }
        }
    }

    #[test]
    fn snapping_uses_nearest_edge_within_ten_pixels_at_every_zoom() {
        assert_eq!(snap_time(4.12, [4.0, 4.15].into_iter(), 50.0), 4.15);
        assert_eq!(snap_time(4.0, [4.2].into_iter(), 50.0), 4.2);
        assert_eq!(snap_time(4.0, [4.21].into_iter(), 50.0), 4.0);
        assert_eq!(snap_time(4.12, [4.0].into_iter(), 5000.0), 4.12);
        assert_eq!(snap_time(4.12, [4.0].into_iter(), 5.0), 4.0);
        assert_eq!(snap_time(0.05, [-0.05].into_iter(), 50.0), 0.05);
        assert_eq!(snap_time(3.0, [].into_iter(), 50.0), 3.0);
    }
}
