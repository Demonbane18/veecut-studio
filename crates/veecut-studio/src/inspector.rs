//! Right-hand panels: item inspector, caption style and look/audio settings.
//!
//! Each panel clones what it edits, lets the widgets change the clone and returns ops built from
//! the difference. Nothing here mutates the project; the app applies the ops.

use std::ops::RangeInclusive;

use eframe::egui::{self, Color32, ComboBox, DragValue, Slider, TextEdit, Ui, emath::Numeric};
use serde_json::{Map, Value, json};
use veecut_engine::ops::Op;
use veecut_engine::project::*;

use crate::app::{AUTO_REVIEW_AT, AUTO_REVIEW_PREFIX};

const LOOKS: [(LookPreset, &str); 7] = [
    (LookPreset::Natural, "Natural"),
    (LookPreset::Warm, "Warm"),
    (LookPreset::Cool, "Cool"),
    (LookPreset::BlackWhite, "Black and white"),
    (LookPreset::TealOrange, "Teal and orange"),
    (LookPreset::Vivid, "Vivid"),
    (LookPreset::Matte, "Matte"),
];
const FITS: [(Fit, &str); 2] = [
    (Fit::Cover, "Cover (fill and crop)"),
    (Fit::Contain, "Contain (letterbox)"),
];
const TRANSITIONS: [(TransitionKind, &str); 3] = [
    (TransitionKind::Crossfade, "Crossfade"),
    (TransitionKind::Dip, "Dip to black"),
    (TransitionKind::Flash, "Flash"),
];
const PRESETS: [(TextPreset, &str); 11] = [
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
];
const ANIMS: [(Anim, &str); 9] = [
    (Anim::None, "None"),
    (Anim::Fade, "Fade"),
    (Anim::Pop, "Pop"),
    (Anim::SlideUp, "Slide up"),
    (Anim::SlideDown, "Slide down"),
    (Anim::SlideLeft, "Slide left"),
    (Anim::SlideRight, "Slide right"),
    (Anim::Typewriter, "Typewriter"),
    (Anim::Wipe, "Wipe"),
];
const ALIGNS: [(Align, &str); 3] = [
    (Align::Left, "Left"),
    (Align::Center, "Center"),
    (Align::Right, "Right"),
];
const MODES: [(CaptionMode, &str); 5] = [
    (CaptionMode::WordPop, "Word pop"),
    (CaptionMode::Highlight, "Highlight"),
    (CaptionMode::Pills, "Pills"),
    (CaptionMode::Subtitle, "Subtitle"),
    (CaptionMode::Off, "Off"),
];

/// Inspector for the selected item, or project settings when nothing is selected.
pub(crate) fn show(ui: &mut Ui, project: &Project, selected: Option<&str>) -> Vec<Op> {
    let mut ops = Vec::new();
    scroll(ui, "inspector", |ui| {
        needs_review(ui, project, &mut ops);
        let found = selected
            .and_then(|id| project.find(id))
            .map(|(t, i)| (&project.tracks[t], &project.tracks[t].items[i]));
        match found {
            Some((track, item)) => {
                ui.push_id(item.id.as_str(), |ui| {
                    item_inspector(ui, project, track, item, &mut ops)
                });
            }
            None => project_inspector(ui, project, &mut ops),
        }
    });
    ops
}

fn needs_review(ui: &mut Ui, project: &Project, ops: &mut Vec<Op>) {
    let mut pending = project
        .markers
        .iter()
        .enumerate()
        .filter(|(_, marker)| {
            marker.at == AUTO_REVIEW_AT
                && !marker.done
                && marker.note.starts_with(AUTO_REVIEW_PREFIX)
        })
        .peekable();
    if pending.peek().is_none() {
        return;
    }
    ui.heading("Needs review");
    ui.weak(
        "Auto-edit checks. Source times refer to raw footage; timeline times refer to this edit.",
    );
    for (index, marker) in pending {
        ui.push_id(("auto_review", index), |ui| {
            ui.group(|ui| {
                ui.label(marker.note.trim_start_matches(AUTO_REVIEW_PREFIX));
                if ui.button("Dismiss").clicked() {
                    ops.push(set(&format!("/markers/{index}/done"), json!(true)));
                }
            });
        });
    }
    ui.add_space(8.0);
    ui.separator();
}

/// Project-wide caption style.
pub(crate) fn captions(ui: &mut Ui, project: &Project) -> Vec<Op> {
    let mut ops = Vec::new();
    scroll(ui, "inspector_captions", |ui| {
        ui.heading("Captions");
        ui.weak("Applies to every caption on the captions track.");
        let mut s = project.captions.clone();
        let mut ch = false;
        ch |= pick(ui, "mode", "Mode", &mut s.mode, &MODES);
        ui.label("Font");
        ch |= ui
            .add(TextEdit::singleline(&mut s.font).desired_width(f32::INFINITY))
            .changed();
        ch |= slider(ui, "Size (fraction of height)", &mut s.size, 0.01..=0.2);
        ch |= color(ui, "Text color", &mut s.color);
        ch |= color(ui, "Highlight color", &mut s.highlight);
        ch |= slider(ui, "Outline (fraction of size)", &mut s.stroke, 0.0..=0.5);
        ch |= ui.checkbox(&mut s.shadow, "Shadow").changed();
        ch |= ui.checkbox(&mut s.uppercase, "Uppercase").changed();
        ch |= slider(
            ui,
            "Vertical position (0 top, 1 bottom)",
            &mut s.position,
            0.0..=1.0,
        );
        ch |= drag(ui, "Most words at once", &mut s.max_words, 1..=12, 0.1, "");
        ch |= drag(
            ui,
            "Most characters per line",
            &mut s.max_chars,
            6..=60,
            0.2,
            "",
        );
        if ch {
            ops.push(set("/captions", json!(s)));
        }
    });
    ops
}

/// Look preset with adjustments, and the final audio mix.
pub(crate) fn look_audio(ui: &mut Ui, project: &Project) -> Vec<Op> {
    let mut ops = Vec::new();
    scroll(ui, "inspector_look_audio", |ui| {
        ui.heading("Look");
        let mut look = project.look.clone();
        let mut ch = pick(ui, "look", "Preset", &mut look.preset, &LOOKS);
        ch |= slider(ui, "Brightness", &mut look.brightness, -1.0..=1.0);
        ch |= slider(ui, "Contrast", &mut look.contrast, -1.0..=1.0);
        ch |= slider(ui, "Saturation", &mut look.saturation, -1.0..=1.0);
        ch |= slider(
            ui,
            "Temperature (cool to warm)",
            &mut look.temperature,
            -1.0..=1.0,
        );
        ch |= slider(ui, "Vignette", &mut look.vignette, 0.0..=1.0);
        if ui.small_button("Reset look").clicked() && look != Look::default() {
            look = Look::default();
            ch = true;
        }
        if ch {
            ops.push(set("/look", json!(look)));
        }

        ui.add_space(8.0);
        ui.separator();
        ui.heading("Audio mix");
        let mut mix = project.audio.clone();
        let mut ch = drag(
            ui,
            "Loudness target",
            &mut mix.target_lufs,
            -30.0..=-6.0,
            0.1,
            " LUFS",
        );
        ch |= drag(
            ui,
            "True-peak ceiling",
            &mut mix.true_peak_db,
            -9.0..=0.0,
            0.05,
            " dBTP",
        );
        ch |= drag(
            ui,
            "Music duck amount",
            &mut mix.duck_db,
            0.0..=30.0,
            0.1,
            " dB",
        );
        ch |= ui
            .checkbox(
                &mut mix.voice_cleanup,
                "Clean up voice (high-pass, noise reduction)",
            )
            .changed();
        if ch {
            ops.push(set("/audio", json!(mix)));
        }
    });
    ops
}

fn scroll(ui: &mut Ui, id: &str, body: impl FnOnce(&mut Ui)) {
    egui::ScrollArea::vertical()
        .id_salt(id)
        .auto_shrink([false, true])
        .show(ui, body);
}

// ---------- project settings ----------

fn project_inspector(ui: &mut Ui, project: &Project, ops: &mut Vec<Op>) {
    ui.heading("Project");
    ui.weak(format!(
        "{:.1} s, {} tracks, {} assets",
        project.duration(),
        project.tracks.len(),
        project.assets.len()
    ));
    let mut name = project.name.clone();
    ui.label("Name");
    if ui
        .add(TextEdit::singleline(&mut name).desired_width(f32::INFINITY))
        .changed()
    {
        ops.push(set("/name", json!(name)));
    }

    let kinds: Vec<(VideoKind, &str)> = VideoKind::ALL.iter().map(|k| (*k, k.slug())).collect();
    let mut kind = project.kind;
    if pick(ui, "kind", "Video kind", &mut kind, &kinds) {
        ops.push(set("/kind", json!(kind)));
    }
    ui.weak(kind.label());

    ui.add_space(8.0);
    ui.separator();
    ui.strong("Canvas");
    let mut canvas = project.canvas.clone();
    let mut ch = false;
    ui.horizontal_wrapped(|ui| {
        for name in ["9:16", "16:9", "4:5", "1:1"] {
            let Some(preset) = Canvas::from_aspect(name) else {
                continue;
            };
            let same = (preset.width, preset.height) == (canvas.width, canvas.height);
            if ui.selectable_label(same, name).clicked() && !same {
                canvas.width = preset.width;
                canvas.height = preset.height;
                ch = true;
            }
        }
    });
    if drag(ui, "Width", &mut canvas.width, 16..=8192, 2.0, " px") {
        canvas.width &= !1;
        ch = true;
    }
    if drag(ui, "Height", &mut canvas.height, 16..=8192, 2.0, " px") {
        canvas.height &= !1;
        ch = true;
    }
    ch |= drag(ui, "Frame rate", &mut canvas.fps, 1.0..=120.0, 0.1, " fps");
    ch |= color(ui, "Background", &mut canvas.background);
    if ch {
        ops.push(set("/canvas", json!(canvas)));
    }
}

// ---------- items ----------

fn item_inspector(ui: &mut Ui, project: &Project, track: &Track, item: &Item, ops: &mut Vec<Op>) {
    ui.heading(match &item.body {
        ItemBody::Clip(_) => "Clip",
        ItemBody::Text(_) => "Text",
        ItemBody::Caption(_) => "Caption",
    });
    ui.weak(format!("{} on {}", item.id, track.name));
    ui.weak(format!(
        "{:.2} s to {:.2} s on the timeline",
        item.start,
        item.end()
    ));
    let mut edited = item.clone();
    let changed = match &mut edited.body {
        ItemBody::Clip(c) => clip_fields(ui, project, item, track.kind, c),
        ItemBody::Text(t) => text_fields(ui, t),
        ItemBody::Caption(c) => caption_fields(ui, item.duration, c),
    };
    if changed {
        ops.extend(patch_op(item, &edited));
    }
}

fn clip_fields(ui: &mut Ui, project: &Project, item: &Item, kind: TrackKind, c: &mut Clip) -> bool {
    let dur = item.duration;
    let asset = project.asset(&c.asset);
    let name = asset
        .and_then(|a| a.path.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| c.asset.clone());
    let src_max = asset
        .map(|a| a.duration)
        .filter(|d| *d > 0.0)
        .unwrap_or(1e6);
    let visual = kind.is_visual();
    let mut ch = false;

    ui.weak(format!(
        "Source {name}, {:.2} s to {:.2} s",
        c.src_in,
        c.src_out(dur)
    ));
    ch |= section(ui, "Timing and audio", true, |ui| {
        let mut ch = drag(ui, "Source start", &mut c.src_in, 0.0..=src_max, 0.02, " s");
        ch |= drag(ui, "Speed", &mut c.speed, 0.1..=16.0, 0.01, "x");
        ch |= slider(ui, "Volume (1 is unchanged)", &mut c.volume, 0.0..=2.0);
        ch |= ui.checkbox(&mut c.muted, "Mute").changed();
        if kind == TrackKind::Audio {
            ch |= ui
                .checkbox(&mut c.duck, "Lower while someone speaks")
                .changed();
        }
        ch
    });
    if !visual {
        return ch;
    }
    ch |= section(ui, "Picture", true, |ui| {
        let mut ch = slider(ui, "Opacity", &mut c.opacity, 0.0..=1.0);
        ch |= pick(ui, "fit", "Fit", &mut c.fit, &FITS);
        ch |= optional(ui, "Subject focus", &mut c.focus, Point::CENTER, point);
        ch
    });
    ch |= section(ui, "Zooms", true, |ui| {
        let mut ch = false;
        let mut remove = None;
        for (i, z) in c.zooms.iter_mut().enumerate() {
            ui.push_id(i, |ui| {
                ui.separator();
                ui.horizontal_wrapped(|ui| {
                    ui.strong(format!("Zoom {}", i + 1));
                    if ui.small_button("Remove").clicked() {
                        remove = Some(i);
                    }
                });
                ch |= drag(ui, "Starts at", &mut z.at, 0.0..=dur, 0.02, " s");
                ch |= drag(
                    ui,
                    "Length",
                    &mut z.duration,
                    0.05..=dur.max(0.05),
                    0.02,
                    " s",
                );
                ch |= drag(ui, "Scale", &mut z.scale, 1.0..=4.0, 0.01, "x");
                ch |= drag(ui, "Ease in and out", &mut z.ease, 0.0..=dur, 0.02, " s");
                ch |= optional(ui, "Own focus", &mut z.focus, Point::CENTER, point);
            });
        }
        if let Some(i) = remove {
            c.zooms.remove(i);
            ch = true;
        }
        if ui.button("Add zoom").clicked() {
            c.zooms.push(Zoom {
                at: 0.0,
                duration: dur.min(1.0),
                scale: 1.12,
                focus: None,
                ease: 0.0,
            });
            ch = true;
        }
        ch
    });
    ch |= section(ui, "Fades and transition", true, |ui| {
        let mut ch = drag(ui, "Fade in", &mut c.fade_in, 0.0..=dur, 0.02, " s");
        ch |= drag(ui, "Fade out", &mut c.fade_out, 0.0..=dur, 0.02, " s");
        if kind == TrackKind::Main {
            let default = Transition {
                kind: TransitionKind::Crossfade,
                duration: 0.5,
            };
            ch |= optional(
                ui,
                "Transition from previous clip",
                &mut c.transition,
                default,
                |ui, t| {
                    let mut ch = combo(ui, "transition", &mut t.kind, &TRANSITIONS);
                    ch |= dv(ui, &mut t.duration, 0.0..=10.0, 0.02, " s");
                    ch
                },
            );
        }
        ch
    });
    ch |= section(ui, "Frame (picture in picture)", false, |ui| {
        let default = Rect {
            x: 0.55,
            y: 0.55,
            w: 0.4,
            h: 0.4,
        };
        let mut ch = optional(ui, "Place in a frame", &mut c.frame, default, rect);
        if c.frame.is_some() {
            ch |= slider(
                ui,
                "Corner radius (fraction of width)",
                &mut c.radius,
                0.0..=0.5,
            );
        }
        ch
    });
    ch
}

fn text_fields(ui: &mut Ui, t: &mut Text) -> bool {
    ui.label("Content");
    let mut ch = ui
        .add(
            TextEdit::multiline(&mut t.text)
                .desired_rows(3)
                .desired_width(f32::INFINITY)
                .hint_text("Put *stars* around words to use the accent color"),
        )
        .changed();
    ch |= pick(ui, "preset", "Preset", &mut t.preset, &PRESETS);
    ch |= section(ui, "Style overrides", true, |ui| {
        let s = &mut t.style;
        let mut ch = false;
        ui.weak("Unchecked fields use the preset.");
        ch |= optional(
            ui,
            "Font",
            &mut s.font,
            "Montserrat".to_string(),
            |ui, f| {
                ui.add(TextEdit::singleline(f).desired_width(120.0))
                    .changed()
            },
        );
        ch |= optional(ui, "Size", &mut s.size, 0.05, |ui, v| {
            dv(ui, v, 0.005..=0.5, 0.002, "")
        });
        ch |= optional(ui, "Text color", &mut s.color, Color::WHITE, color_button);
        ch |= optional(
            ui,
            "Accent color",
            &mut s.accent,
            Color::YELLOW,
            color_button,
        );
        ch |= optional(
            ui,
            "Background color",
            &mut s.background,
            Color::BLACK.with_alpha(160),
            color_button,
        );
        ch |= optional(ui, "Outline", &mut s.stroke, 0.1, |ui, v| {
            dv(ui, v, 0.0..=1.0, 0.005, "")
        });
        ch |= optional(ui, "Uppercase", &mut s.uppercase, true, |ui, v| {
            ui.checkbox(v, "on").changed()
        });
        ch |= optional(ui, "Align", &mut s.align, Align::Center, |ui, a| {
            combo(ui, "align", a, &ALIGNS)
        });
        if ui.small_button("Reset all overrides").clicked() && *s != TextStyle::default() {
            *s = TextStyle::default();
            ch = true;
        }
        ch
    });
    ch |= section(ui, "Position and animation", true, |ui| {
        let mut ch = optional(ui, "Own position", &mut t.position, Point::CENTER, point);
        if t.position.is_none() {
            ui.weak("Using the preset's place inside the safe area.");
        }
        ch |= pick(ui, "anim_in", "Animate in", &mut t.anim_in, &ANIMS);
        ch |= pick(ui, "anim_out", "Animate out", &mut t.anim_out, &ANIMS);
        ch
    });
    ch
}

fn caption_fields(ui: &mut Ui, dur: f64, c: &mut Caption) -> bool {
    ui.weak("Times are seconds from the start of this caption.");
    let mut ch = false;
    for (i, w) in c.words.iter_mut().enumerate() {
        ui.push_id(i, |ui| {
            ui.separator();
            ui.horizontal_wrapped(|ui| {
                ui.label(format!("{}.", i + 1));
                ch |= ui
                    .add(TextEdit::singleline(&mut w.text).desired_width(90.0))
                    .changed();
                ch |= ui.checkbox(&mut w.emphasis, "Emphasis").changed();
            });
            ch |= drag(ui, "Start", &mut w.start, 0.0..=dur, 0.01, " s");
            ch |= drag(ui, "End", &mut w.end, 0.0..=dur, 0.01, " s");
        });
    }
    ch
}

// ---------- ops ----------

fn set(path: &str, value: Value) -> Op {
    Op::Set {
        path: path.into(),
        value,
    }
}

/// Merge patch that turns `old` into `new`; None when they are equal.
fn patch_op(old: &Item, new: &Item) -> Option<Op> {
    let (o, n) = (
        serde_json::to_value(old).ok()?,
        serde_json::to_value(new).ok()?,
    );
    diff(&o, &n).map(|patch| Op::Patch {
        item: old.id.clone(),
        patch,
    })
}

/// RFC 7386 merge patch from `old` to `new`. Keys that vanish become explicit nulls, so a
/// cleared optional field or style override is really removed. Arrays are replaced whole.
fn diff(old: &Value, new: &Value) -> Option<Value> {
    if old == new {
        return None;
    }
    let (Value::Object(o), Value::Object(n)) = (old, new) else {
        return Some(new.clone());
    };
    let mut patch = Map::new();
    for (k, v) in n {
        match o.get(k) {
            Some(ov) => {
                if let Some(d) = diff(ov, v) {
                    patch.insert(k.clone(), d);
                }
            }
            None => {
                patch.insert(k.clone(), v.clone());
            }
        }
    }
    for k in o.keys().filter(|k| !n.contains_key(*k)) {
        patch.insert(k.clone(), Value::Null);
    }
    Some(Value::Object(patch))
}

// ---------- small field widgets; each returns true when the user changed the value ----------

fn section(ui: &mut Ui, title: &str, open: bool, body: impl FnOnce(&mut Ui) -> bool) -> bool {
    egui::CollapsingHeader::new(title)
        .default_open(open)
        .show(ui, body)
        .body_returned
        .unwrap_or(false)
}

/// A number box without a label.
fn dv<T: Numeric>(
    ui: &mut Ui,
    v: &mut T,
    range: RangeInclusive<T>,
    speed: f64,
    suffix: &str,
) -> bool {
    ui.add(DragValue::new(v).range(range).speed(speed).suffix(suffix))
        .changed()
}

/// Label and number box on one line that wraps when the panel is narrow.
fn drag<T: Numeric>(
    ui: &mut Ui,
    label: &str,
    v: &mut T,
    range: RangeInclusive<T>,
    speed: f64,
    suffix: &str,
) -> bool {
    ui.horizontal_wrapped(|ui| {
        ui.label(label);
        dv(ui, v, range, speed, suffix)
    })
    .inner
}

/// Label above a slider that fills the panel width.
fn slider<T: Numeric>(ui: &mut Ui, label: &str, v: &mut T, range: RangeInclusive<T>) -> bool {
    ui.label(label);
    let width = (ui.available_width() - 64.0).clamp(40.0, 260.0);
    ui.scope(|ui| {
        ui.spacing_mut().slider_width = width;
        ui.add(Slider::new(v, range)).changed()
    })
    .inner
}

fn combo<T: Copy + PartialEq>(ui: &mut Ui, id: &str, value: &mut T, options: &[(T, &str)]) -> bool {
    let mut changed = false;
    let current = options
        .iter()
        .find(|(v, _)| *v == *value)
        .map_or("", |(_, n)| *n);
    ComboBox::from_id_salt(id)
        .selected_text(current)
        .show_ui(ui, |ui| {
            for (v, name) in options {
                changed |= ui.selectable_value(value, *v, *name).changed();
            }
        });
    changed
}

fn pick<T: Copy + PartialEq>(
    ui: &mut Ui,
    id: &str,
    label: &str,
    value: &mut T,
    options: &[(T, &str)],
) -> bool {
    ui.horizontal_wrapped(|ui| {
        ui.label(label);
        combo(ui, id, value, options)
    })
    .inner
}

fn color_button(ui: &mut Ui, c: &mut Color) -> bool {
    let mut c32 = Color32::from_rgba_unmultiplied(c.r, c.g, c.b, c.a);
    if !ui.color_edit_button_srgba(&mut c32).changed() {
        return false;
    }
    let [r, g, b, a] = c32.to_srgba_unmultiplied();
    *c = Color { r, g, b, a };
    true
}

fn color(ui: &mut Ui, label: &str, c: &mut Color) -> bool {
    ui.horizontal_wrapped(|ui| {
        ui.label(label);
        color_button(ui, c)
    })
    .inner
}

/// Checkbox that turns an optional value on (with `default`) or off (None); the editor shows
/// while it is on. Switching off removes the field from the item.
fn optional<T>(
    ui: &mut Ui,
    label: &str,
    opt: &mut Option<T>,
    default: T,
    edit: impl FnOnce(&mut Ui, &mut T) -> bool,
) -> bool {
    ui.horizontal_wrapped(|ui| {
        let mut on = opt.is_some();
        let mut changed = false;
        if ui.checkbox(&mut on, label).changed() {
            *opt = on.then_some(default);
            changed = true;
        }
        if let Some(v) = opt {
            changed |= edit(ui, v);
        }
        changed
    })
    .inner
}

fn point(ui: &mut Ui, p: &mut Point) -> bool {
    let mut ch = ui
        .add(
            DragValue::new(&mut p.x)
                .range(0.0..=1.0)
                .speed(0.005)
                .fixed_decimals(3)
                .prefix("x "),
        )
        .changed();
    ch |= ui
        .add(
            DragValue::new(&mut p.y)
                .range(0.0..=1.0)
                .speed(0.005)
                .fixed_decimals(3)
                .prefix("y "),
        )
        .changed();
    ch
}

fn rect(ui: &mut Ui, r: &mut Rect) -> bool {
    let mut ch = ui
        .add(
            DragValue::new(&mut r.x)
                .range(0.0..=1.0)
                .speed(0.005)
                .fixed_decimals(3)
                .prefix("x "),
        )
        .changed();
    ch |= ui
        .add(
            DragValue::new(&mut r.y)
                .range(0.0..=1.0)
                .speed(0.005)
                .fixed_decimals(3)
                .prefix("y "),
        )
        .changed();
    ch |= ui
        .add(
            DragValue::new(&mut r.w)
                .range(0.01..=1.0)
                .speed(0.005)
                .fixed_decimals(3)
                .prefix("w "),
        )
        .changed();
    ch |= ui
        .add(
            DragValue::new(&mut r.h)
                .range(0.01..=1.0)
                .speed(0.005)
                .fixed_decimals(3)
                .prefix("h "),
        )
        .changed();
    ch
}

#[cfg(test)]
mod tests {
    use super::*;
    use veecut_engine::ops;

    fn text_item() -> Item {
        Item {
            id: "t1".into(),
            start: 0.0,
            duration: 2.0,
            body: ItemBody::Text(Text {
                text: "Hi".into(),
                preset: TextPreset::Card,
                style: TextStyle {
                    color: Some(Color::YELLOW),
                    size: Some(0.1),
                    ..Default::default()
                },
                position: Some(Point::CENTER),
                anim_in: Anim::Fade,
                anim_out: Anim::None,
            }),
        }
    }

    #[test]
    fn patch_removes_cleared_optionals_and_overrides() {
        let old = text_item();
        let mut p = Project::new("t", VideoKind::default(), Canvas::vertical());
        p.tracks[2].items.push(old.clone());

        let mut new = old.clone();
        let ItemBody::Text(t) = &mut new.body else {
            unreachable!()
        };
        t.text = "Yo".into();
        t.style.color = None;
        t.position = None;
        ops::apply(&mut p, &patch_op(&old, &new).unwrap()).unwrap();
        assert_eq!(p.tracks[2].items[0], new);

        // Clearing the last override drops the whole style key.
        let before = new.clone();
        let ItemBody::Text(t) = &mut new.body else {
            unreachable!()
        };
        t.style = TextStyle::default();
        ops::apply(&mut p, &patch_op(&before, &new).unwrap()).unwrap();
        assert_eq!(p.tracks[2].items[0], new);
        assert!(patch_op(&new, &new).is_none());
    }
}
