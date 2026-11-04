use super::*;
use crate::project::{
    Asset, AssetKind, AssetRole, Caption, Clip, FaceSample, TextStyle, VideoKind, Zoom,
};
use tiny_skia::Pixmap;

#[test]
fn safe_areas_reserve_phone_controls() {
    let phone = safe_area(&Canvas::vertical());
    let wide = safe_area(&Canvas::landscape());
    assert!((phone.x * 1080.0 - 90.0).abs() < 0.001);
    assert!(((phone.x + phone.w) * 1080.0 - 930.0).abs() < 0.001);
    assert!((phone.y * 1920.0 - 200.0).abs() < 0.001);
    assert!(((phone.y + phone.h) * 1920.0 - 1600.0).abs() < 0.001);
    assert_eq!(
        wide,
        Rect {
            x: 0.05,
            y: 0.05,
            w: 0.9,
            h: 0.9
        }
    );
    assert!(safe_area(&Canvas::square()).h < wide.h);
}

#[test]
fn bundled_faces_are_static_true_type() {
    for bytes in FONTS {
        assert_eq!(&bytes[..4], &[0, 1, 0, 0]);
        let tables = u16::from_be_bytes([bytes[4], bytes[5]]) as usize;
        assert!(!(0..tables).any(|i| &bytes[12 + i * 16..16 + i * 16] == b"fvar"));
    }
}

fn sample(preset: TextPreset, text: &str) -> Item {
    Item {
        id: "sample".into(),
        start: 0.0,
        duration: 4.0,
        body: ItemBody::Text(Text {
            text: text.into(),
            preset,
            style: TextStyle::default(),
            position: None,
            anim_in: Anim::Fade,
            anim_out: Anim::Fade,
        }),
    }
}

fn gradient(width: u32, height: u32) -> Pixmap {
    let mut image = Pixmap::new(width, height).unwrap();
    for y in 0..height {
        for x in 0..width {
            let v =
                (40.0 + 170.0 * x as f32 / width as f32 + 20.0 * y as f32 / height as f32) as u8;
            let i = ((y * width + x) * 4) as usize;
            image.data_mut()[i..i + 4].copy_from_slice(&[v, v, v, 255]);
        }
    }
    image
}

#[test]
fn font_fallback_and_cached_draw_are_deterministic() {
    let mut g = Graphics::new();
    for family in [
        "Montserrat",
        "Abril Fatface",
        "Anton",
        "Bebas Neue",
        "Poppins",
        "Archivo Black",
        "Inter",
    ] {
        assert!(
            g.font_families().iter().any(|name| name == family),
            "missing {family}"
        );
    }
    assert_eq!(g.family("unknown family"), "Montserrat");
    let mut p = Project::new("test", VideoKind::default(), Canvas::vertical());
    p.tracks[2]
        .items
        .push(sample(TextPreset::Card, "Readable *text*"));
    let mut a = gradient(270, 480);
    g.draw(&p, 1.0, &mut a.as_mut());
    let count = g.layers.len();
    let mut b = gradient(270, 480);
    g.draw(&p, 1.0, &mut b.as_mut());
    assert_eq!(a.data(), b.data());
    assert_eq!(g.layers.len(), count);
    assert_ne!(a.data(), gradient(270, 480).data());
}

#[test]
fn presets_and_entrances_stay_inside_safe_area() {
    let mut graphics = Graphics::new();
    let presets = [
        TextPreset::Hook,
        TextPreset::Title,
        TextPreset::Card,
        TextPreset::LowerThird,
        TextPreset::Callout,
        TextPreset::Counter,
        TextPreset::Label,
        TextPreset::Quote,
        TextPreset::List,
        TextPreset::Kinetic,
        TextPreset::Cta,
    ];
    let safe = safe_area(&Canvas::vertical());
    for preset in presets {
        let mut project = Project::new("bounds", VideoKind::default(), Canvas::vertical());
        project.tracks[2]
            .items
            .push(sample(preset, "Make *it*\ncount"));
        for at in [0.05, 1.0, 3.95] {
            let mut frame = Pixmap::new(270, 480).unwrap();
            graphics.draw(&project, at, &mut frame.as_mut());
            let mut seen = false;
            for (i, rgba) in frame.data().chunks_exact(4).enumerate() {
                if rgba[3] == 0 {
                    continue;
                }
                seen = true;
                let (x, y) = ((i % 270) as f32, (i / 270) as f32);
                assert!(
                    x >= safe.x * 270.0 - 1.0
                        && x <= (safe.x + safe.w) * 270.0 + 1.0
                        && y >= safe.y * 480.0 - 1.0
                        && y <= (safe.y + safe.h) * 480.0 + 1.0,
                    "{preset:?} at {at}: pixel {x},{y} outside safe area"
                );
            }
            assert!(seen, "{preset:?} failed to draw");
        }
    }
}

#[test]
fn wrapped_list_reveals_whole_entries_and_typewriter_reveals_clusters() {
    let mut graphics = Graphics::new();
    let mut project = Project::new("reveal", VideoKind::default(), Canvas::vertical());
    project.tracks[2].items.push(sample(
        TextPreset::List,
        "A long first entry that wraps across multiple lines\nSecond entry\nLast entry",
    ));
    let mut frame = Pixmap::new(270, 480).unwrap();
    graphics.draw(&project, 3.4, &mut frame.as_mut());
    assert_eq!(
        graphics.layers.values().next().unwrap().line_bottoms.len(),
        3
    );
    project.tracks[2].items = vec![sample(TextPreset::Title, "Café *story*")];
    if let ItemBody::Text(text) = &mut project.tracks[2].items[0].body {
        text.anim_in = Anim::Typewriter;
        text.anim_out = Anim::Typewriter;
    }
    let coverage = |g: &mut Graphics, at| {
        let mut f = Pixmap::new(270, 480).unwrap();
        g.draw(&project, at, &mut f.as_mut());
        f.data().chunks_exact(4).filter(|p| p[3] != 0).count()
    };
    assert!(coverage(&mut graphics, 0.025) < coverage(&mut graphics, 1.0));
    assert!(coverage(&mut graphics, 3.975) < coverage(&mut graphics, 1.0));
}

/// Reproducible visual proof, intentionally opt-in (43 PNGs, no committed media).
/// `cargo test -p veecut-engine --release --no-default-features graphics::tests::visual_proof -- --ignored --nocapture`
/// Outputs /tmp/veecut-shots/graphics; reports warm graphics-only ms/frame, excluding clear/decode.
#[test]
#[ignore]
fn visual_proof() {
    let dir = std::path::Path::new("/tmp/veecut-shots/graphics");
    std::fs::create_dir_all(dir).unwrap();
    let samples = [
        (TextPreset::Hook, "Stop scrolling.\n*Start creating.*"),
        (TextPreset::Title, "Small ideas.\n*Big stories.*"),
        (TextPreset::Card, "Make room for\n*your next idea.*"),
        (TextPreset::LowerThird, "Maya Santos\nEditor & filmmaker"),
        (TextPreset::Callout, "*Look here*"),
        (TextPreset::Counter, "STEP *2/5*"),
        (TextPreset::Label, "Behind the scenes"),
        (TextPreset::Quote, "The best story is\nthe one you *tell.*"),
        (
            TextPreset::List,
            "Find your story\nCut the noise\n*Make it count*",
        ),
        (TextPreset::Kinetic, "*MAKE*\nIT"),
        (TextPreset::Cta, "Your story\n*starts now.*"),
    ];
    let mut g = Graphics::new();
    for canvas in [Canvas::vertical(), Canvas::landscape()] {
        for (preset, text) in samples {
            let mut p = Project::new("visual proof", VideoKind::default(), canvas.clone());
            p.tracks[2].items.push(sample(preset, text));
            let mut image = gradient(canvas.width, canvas.height);
            g.draw(&p, 3.0, &mut image.as_mut());
            let name = format!("text-{preset:?}-{}x{}.png", canvas.width, canvas.height);
            image.save_png(dir.join(name)).unwrap();
        }
        for mode in [
            CaptionMode::WordPop,
            CaptionMode::Highlight,
            CaptionMode::Pills,
            CaptionMode::Subtitle,
            CaptionMode::Off,
        ] {
            for boxed in [false, true] {
                let mut p = Project::new("caption proof", VideoKind::default(), canvas.clone());
                p.captions.mode = mode;
                p.captions.boxed = boxed;
                p.captions.size = 0.033;
                p.tracks[3].items.push(Item {
                    id: "caption".into(),
                    start: 0.0,
                    duration: 3.0,
                    body: ItemBody::Caption(Caption {
                        words: ["Make", "every", "word", "count"]
                            .iter()
                            .enumerate()
                            .map(|(i, s)| Word {
                                text: (*s).into(),
                                start: i as f64 * 0.5,
                                end: (i + 1) as f64 * 0.5,
                                emphasis: i == 3,
                            })
                            .collect(),
                    }),
                });
                let mut image = gradient(canvas.width, canvas.height);
                g.draw(&p, 1.7, &mut image.as_mut());
                image
                    .save_png(dir.join(format!(
                        "caption-{mode:?}-boxed-{boxed}-{}x{}.png",
                        canvas.width, canvas.height
                    )))
                    .unwrap();
            }
        }
    }
    let mut p = Project::new("preview scale", VideoKind::default(), Canvas::vertical());
    p.tracks[2]
        .items
        .push(sample(TextPreset::Hook, samples[0].1));
    let mut preview = gradient(540, 960);
    g.draw(&p, 3.0, &mut preview.as_mut());
    preview.save_png(dir.join("text-Hook-540x960.png")).unwrap();
    let mut image = gradient(1080, 1920);
    g.draw(&p, 3.0, &mut image.as_mut());
    let now = std::time::Instant::now();
    for _ in 0..300 {
        g.draw(&p, 3.0, &mut image.as_mut());
    }
    println!(
        "graphics warm hook: {:.3} ms/frame (300 frames, 1080x1920)",
        now.elapsed().as_secs_f64() * 1000.0 / 300.0
    );
    p.tracks[2].items.clear();
    p.captions.boxed = true;
    p.tracks[3].items.push(Item {
        id: "benchmark".into(),
        start: 0.0,
        duration: 5.0,
        body: ItemBody::Caption(Caption {
            words: vec![Word {
                text: "MAKE EVERY WORD COUNT".into(),
                start: 0.0,
                end: 5.0,
                emphasis: true,
            }],
        }),
    });
    g.draw(&p, 2.0, &mut image.as_mut());
    let now = std::time::Instant::now();
    for _ in 0..300 {
        g.draw(&p, 2.0, &mut image.as_mut());
    }
    println!(
        "graphics warm boxed caption: {:.3} ms/frame (300 frames, 1080x1920)",
        now.elapsed().as_secs_f64() * 1000.0 / 300.0
    );
    for (suffix, tw, th) in [("1080x1920.png", 360, 640), ("1920x1080.png", 480, 270)] {
        for prefix in ["text-", "caption-"] {
            let mut paths = std::fs::read_dir(dir)
                .unwrap()
                .map(|p| p.unwrap().path())
                .filter(|p| {
                    p.file_name().unwrap().to_string_lossy().starts_with(prefix)
                        && p.file_name().unwrap().to_string_lossy().ends_with(suffix)
                })
                .collect::<Vec<_>>();
            paths.sort();
            for (sheet, chunk) in paths.chunks(6).enumerate() {
                let mut atlas = image::RgbaImage::new(tw * 3, (th + 36) * 2);
                for (index, path) in chunk.iter().enumerate() {
                    let thumb = image::open(path).unwrap().thumbnail(tw, th).to_rgba8();
                    let (x, y) = (index as u32 % 3 * tw, index as u32 / 3 * (th + 36));
                    image::imageops::overlay(&mut atlas, &thumb, i64::from(x), i64::from(y + 36));
                    let mut title =
                        Project::new("sheet label", VideoKind::default(), Canvas::new(tw, 36));
                    let name = path
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .replace(&format!("-{suffix}"), "");
                    let mut item = sample(TextPreset::Label, &name);
                    if let ItemBody::Text(t) = &mut item.body {
                        t.style.size = Some(0.42);
                        t.position = Some(Point::CENTER);
                    }
                    title.tracks[2].items.push(item);
                    let mut label = gradient(tw, 36);
                    g.draw(&title, 1.0, &mut label.as_mut());
                    let label = image::RgbaImage::from_raw(tw, 36, label.data().to_vec()).unwrap();
                    image::imageops::overlay(&mut atlas, &label, i64::from(x), i64::from(y));
                }
                atlas
                    .save(dir.join(format!("sheet-{prefix}{suffix}-{sheet}.png")))
                    .unwrap();
            }
        }
    }
}

/// Contrast proof on white, pale cyan footage and black. Files are temporary.
/// `cargo test -p veecut-engine --no-default-features graphics::tests::contrast_proof -- --ignored --nocapture`
#[test]
#[ignore]
fn contrast_proof() {
    let dir = std::path::Path::new("/tmp/veecut-shots/graphics");
    std::fs::create_dir_all(dir).unwrap();
    let mut graphics = Graphics::new();
    for canvas in [Canvas::vertical(), Canvas::landscape()] {
        let mut kinetic = Project::new("contrast", VideoKind::default(), canvas.clone());
        kinetic.tracks[2]
            .items
            .push(sample(TextPreset::Kinetic, "*MAKE*\nIT"));
        let mut caption = Project::new("contrast", VideoKind::default(), canvas.clone());
        caption.captions.size = 0.033;
        caption.captions.boxed = false;
        caption.tracks[3].items.push(Item {
            id: "contrast-caption".into(),
            start: 0.0,
            duration: 4.0,
            body: ItemBody::Caption(Caption {
                words: vec![
                    Word {
                        text: "Make".into(),
                        start: 0.0,
                        end: 0.5,
                        emphasis: false,
                    },
                    Word {
                        text: "every".into(),
                        start: 0.5,
                        end: 1.0,
                        emphasis: false,
                    },
                    Word {
                        text: "word".into(),
                        start: 1.0,
                        end: 2.0,
                        emphasis: false,
                    },
                    Word {
                        text: "count".into(),
                        start: 2.0,
                        end: 3.0,
                        emphasis: true,
                    },
                ],
            }),
        });
        for background in ["white", "cyan", "dark"] {
            let mut base = Pixmap::new(canvas.width, canvas.height).unwrap();
            for y in 0..canvas.height {
                for x in 0..canvas.width {
                    let rgba = match background {
                        "white" => [255, 255, 255, 255],
                        "cyan" => [200 + (45 * x / canvas.width) as u8, 245, 255, 255],
                        _ => [15, 20, 27, 255],
                    };
                    let index = ((y * canvas.width + x) * 4) as usize;
                    base.data_mut()[index..index + 4].copy_from_slice(&rgba);
                }
            }
            for (name, project) in [("kinetic", &kinetic), ("caption", &caption)] {
                let mut frame = base.clone();
                graphics.draw(project, 1.6, &mut frame.as_mut());
                frame
                    .save_png(dir.join(format!(
                        "contrast-{name}-{background}-{}x{}.png",
                        canvas.width, canvas.height
                    )))
                    .unwrap();
            }
            if canvas.width == 1080 && background == "white" {
                let mut frame = base.clone();
                let mut elapsed = std::time::Duration::ZERO;
                for _ in 0..300 {
                    frame.data_mut().copy_from_slice(base.data());
                    let now = std::time::Instant::now();
                    graphics.draw(&kinetic, 1.6, &mut frame.as_mut());
                    elapsed += now.elapsed();
                }
                println!(
                    "graphics contrast + scrim: {:.3} ms/frame (300 fresh white frames, 1080x1920, background copy excluded)",
                    elapsed.as_secs_f64() * 1000.0 / 300.0
                );
            }
        }
    }
}

/// A 16:9 clip in a 9:16 canvas, aimed at (0.5, 0.45) and punched in 1.15x from `zoom_at`, with
/// one face box (x, y, w, h as fractions of the source) every half second. By hand: a source row
/// `y` lands on the canvas at `1.15 * y - 0.0175` once punched in, and at `y` before.
fn face_project(face: (f32, f32, f32, f32), zoom_at: f64) -> Project {
    let mut p = Project::new("faces", VideoKind::default(), Canvas::vertical());
    p.assets.push(Asset {
        id: "a".into(),
        path: "a.mp4".into(),
        kind: AssetKind::Video,
        role: AssetRole::Source,
        duration: 10.0,
        width: 1920,
        height: 1080,
        fps: 30.0,
        has_audio: true,
    });
    let mut clip = Clip::new("a", 0.0);
    clip.focus = Some(Point::new(0.5, 0.45));
    clip.zooms.push(Zoom {
        at: zoom_at,
        duration: 10.0 - zoom_at,
        scale: 1.15,
        focus: None,
        ease: 0.0,
    });
    p.main_mut().items.push(Item {
        id: "clip-1".into(),
        start: 0.0,
        duration: 10.0,
        body: ItemBody::Clip(clip),
    });
    p.faces.insert(
        "a".into(),
        (0..20)
            .map(|k| FaceSample {
                t: k as f64 * 0.5,
                x: face.0,
                y: face.1,
                w: face.2,
                h: face.3,
            })
            .collect(),
    );
    p.captions.size = 0.06;
    p.captions.position = 0.74;
    p
}

/// A three-word caption page, 1.2 s long.
fn page(id: &str, start: f64) -> Item {
    Item {
        id: id.into(),
        start,
        duration: 1.2,
        body: ItemBody::Caption(Caption {
            words: ["Make", "every", "word"]
                .iter()
                .enumerate()
                .map(|(i, s)| Word {
                    text: (*s).into(),
                    start: i as f64 * 0.4,
                    end: (i + 1) as f64 * 0.4,
                    emphasis: false,
                })
                .collect(),
        }),
    }
}

/// Canvas rows (top, bottom) of the mouth-to-chin zone of `face` in a punched-in `face_project`:
/// from 55% of the box height to 12% below its bottom.
fn punched_in_zone(face: (f32, f32, f32, f32)) -> (f32, f32) {
    let on_canvas = |y: f32| 1.15 * y - 0.0175;
    (
        on_canvas(face.1 + face.3 * 0.55),
        on_canvas(face.1 + face.3 * 1.12),
    )
}

/// First and last pixel row with any paint.
fn painted_rows(frame: &Pixmap) -> Option<(u32, u32)> {
    let rows: Vec<u32> = frame
        .data()
        .chunks_exact(frame.width() as usize * 4)
        .enumerate()
        .filter(|(_, row)| row.chunks_exact(4).any(|px| px[3] != 0))
        .map(|(y, _)| y as u32)
        .collect();
    Some((*rows.first()?, *rows.last()?))
}

#[test]
fn captions_keep_off_the_eyes_not_only_the_mouth() {
    // A 16:9 interview cropped to 9:16: the eyes (from 0.26 of the canvas) sit inside the default
    // upper-third caption box, while the mouth and chin (from 0.365) are well clear of it.
    let face = (0.40, 0.20, 0.20, 0.30);
    let mut project = face_project(face, 9.0);
    project.captions.size = 0.045;
    project.captions.position = 0.25;
    project.tracks[3].items.push(page("cap-1", 1.0));
    let placed = Graphics::new().caption_placement_at(&project, 1.3).unwrap();
    assert_eq!(placed.face, FaceFit::Moved, "{placed:?}");
    // Eyes start just below the brows (20% down the box) on this unzoomed crop.
    let eyes_top = face.1 + face.3 * 0.2;
    let chin = face.1 + face.3 * 1.12;
    let b = placed.bounds;
    assert!(
        b.y + b.h <= eyes_top || b.y >= chin,
        "caption {b:?} overlaps the face band {eyes_top}..{chin}"
    );
}

#[test]
fn a_face_filling_the_frame_keeps_captions_off_the_mouth_instead_of_hiding_them() {
    // Eyes to chin cover nearly the whole safe area, so no slot clears them. The words must
    // still show, clear of the mouth and chin.
    let face = (0.25, 0.05, 0.50, 0.70);
    let mut project = face_project(face, 9.0);
    project.captions.size = 0.045;
    project.captions.position = 0.25;
    project.tracks[3].items.push(page("cap-1", 1.0));
    let placed = Graphics::new().caption_placement_at(&project, 1.3).unwrap();
    assert_ne!(placed.face, FaceFit::Hidden, "{placed:?}");
    let mouth_top = face.1 + face.3 * 0.55;
    assert!(
        placed.bounds.y + placed.bounds.h <= mouth_top,
        "caption {:?} reaches the mouth at {mouth_top}",
        placed.bounds
    );
}

#[test]
fn large_face_moves_the_caption_off_mouth_and_chin_in_canvas_space() {
    let face = (0.38, 0.22, 0.24, 0.43);
    let (top, bottom) = punched_in_zone(face);
    let mut project = face_project(face, 0.0);
    project.tracks[3].items.push(page("cap-1", 1.0));
    let mut g = Graphics::new();
    let at = 1.3;

    // Without face data the page sits on the chin: the bug.
    let mut blind = project.clone();
    blind.faces.clear();
    let before = g.caption_placement_at(&blind, at).unwrap();
    assert_eq!(before.face, FaceFit::Off);
    assert!(
        before.bounds.y < bottom && before.bounds.y + before.bounds.h > top,
        "{before:?} vs zone {top}..{bottom}"
    );

    let placed = g.caption_placement_at(&project, at).unwrap();
    assert_eq!(placed.face, FaceFit::Moved, "{placed:?}");
    let (a, b) = (placed.bounds.y, placed.bounds.y + placed.bounds.h);
    assert!(
        b <= top || a >= bottom,
        "{placed:?} vs zone {top}..{bottom}"
    );

    // The pixels agree with the report: painted inside the reported box, clear of the zone.
    let mut frame = Pixmap::new(1080, 1920).unwrap();
    g.draw(&project, at, &mut frame.as_mut());
    let (first, last) = painted_rows(&frame).expect("the caption is drawn");
    assert!(
        first as f32 >= a * 1920.0 - 2.0 && last as f32 <= b * 1920.0 + 2.0,
        "{first}..{last} vs {placed:?}"
    );
    assert!(last as f32 / 1920.0 <= top || first as f32 / 1920.0 >= bottom);
}

#[test]
fn a_wide_shot_keeps_the_requested_position() {
    let mut project = face_project((0.45, 0.30, 0.08, 0.14), 0.0);
    project.tracks[3].items.push(page("cap-1", 1.0));
    let mut g = Graphics::new();
    let placed = g.caption_placement_at(&project, 1.3).unwrap();
    assert_eq!(
        (placed.face, placed.slot),
        (FaceFit::Clear, Slot::Requested),
        "{placed:?}"
    );
    project.faces.clear();
    assert_eq!(
        placed.bounds,
        g.caption_placement_at(&project, 1.3).unwrap().bounds
    );
}

#[test]
fn a_punch_in_inside_a_page_counts_for_the_whole_page() {
    let mut project = face_project((0.38, 0.0, 0.24, 0.2), 2.0);
    for (id, start) in [("early", 0.0), ("straddle", 1.6), ("late", 4.0)] {
        project.tracks[3].items.push(page(id, start));
    }
    let mut g = Graphics::new();
    let mut blind = project.clone();
    blind.faces.clear();
    let home = g.caption_placement_at(&blind, 0.5).unwrap().bounds;
    // Put the zone's bottom (y + 1.12 h at 1x) 0.04 above the requested box.
    for s in project.faces.get_mut("a").unwrap() {
        s.y = home.y - 0.04 - 1.12 * s.h;
    }
    let early = g.caption_placement_at(&project, 0.5).unwrap();
    assert_eq!(early.face, FaceFit::Clear, "{early:?}");
    assert_eq!(early.bounds, home);
    // The 1.15x punch-in at 2.0 s lifts the zone onto the box for the page that spans it ...
    let straddle = g.caption_placement_at(&project, 2.5).unwrap();
    assert_ne!(straddle.face, FaceFit::Clear, "{straddle:?}");
    // ... and for the pages after it.
    let late = g.caption_placement_at(&project, 4.5).unwrap();
    assert_ne!(late.face, FaceFit::Clear, "{late:?}");
}

#[test]
fn missing_detections_are_reported_and_never_counted_as_clear() {
    let mut project = face_project((0.45, 0.30, 0.08, 0.14), 0.0);
    project.faces.get_mut("a").unwrap().retain(|s| s.t <= 2.0);
    for (id, start) in [("seen", 0.0), ("partly", 2.4), ("blind", 5.0)] {
        project.tracks[3].items.push(page(id, start));
    }
    let mut g = Graphics::new();
    let seen = g.caption_placement_at(&project, 0.5).unwrap();
    assert_eq!((seen.face, seen.face_gaps), (FaceFit::Clear, false));
    let partly = g.caption_placement_at(&project, 2.8).unwrap();
    assert!(partly.face_gaps, "{partly:?}");
    let blind = g.caption_placement_at(&project, 5.5).unwrap();
    assert_eq!(
        (blind.face, blind.slot, blind.face_gaps),
        (FaceFit::Unchecked, Slot::Requested, true)
    );
    let report = caption_report(&mut g, &project, 8);
    assert_eq!(
        (report.pages, report.unchecked, report.gaps),
        (3, 1, 2),
        "{report:?}"
    );
    assert_eq!(report.examples[0].item, "blind");
}

#[test]
fn a_page_with_no_clear_slot_is_not_drawn_and_is_reported() {
    // The face is high in the frame for the first half of the page and low for the second, so
    // the zone spans both slots and nothing is clear.
    let mut project = face_project((0.38, 0.22, 0.24, 0.43), 0.0);
    project.tracks[3].items.push(page("cap-1", 1.0));
    let low_only = project.clone();
    for s in project
        .faces
        .get_mut("a")
        .unwrap()
        .iter_mut()
        .filter(|s| s.t < 1.75)
    {
        (s.y, s.h) = (0.0, 0.3);
    }
    let mut g = Graphics::new();
    let placed = g.caption_placement_at(&project, 1.3).unwrap();
    assert_eq!(placed.face, FaceFit::Hidden, "{placed:?}");
    let report = caption_report(&mut g, &project, 5);
    assert_eq!(report.hidden, 1);
    assert_eq!(report.examples[0].item, "cap-1");
    let frame = |g: &mut Graphics, p: &Project| {
        let mut f = Pixmap::new(270, 480).unwrap();
        g.draw(p, 1.3, &mut f.as_mut());
        f
    };
    let mut without = project.clone();
    without.tracks[3].items.clear();
    assert_eq!(
        frame(&mut g, &project).data(),
        frame(&mut g, &without).data()
    );
    // With the face only low, the upper slot is clear, so the page moves there instead.
    let placed = g.caption_placement_at(&low_only, 1.3).unwrap();
    assert_eq!((placed.face, placed.slot), (FaceFit::Moved, Slot::Upper));
}

#[test]
fn captions_avoid_the_hook_box_where_it_really_is_and_hide_when_nothing_is_clear() {
    let hook_at = |y: f32| {
        let mut item = sample(TextPreset::Hook, "The hook");
        if let ItemBody::Text(text) = &mut item.body {
            text.position = Some(Point::new(0.5, y));
        }
        item
    };
    let mut project = face_project((0.38, 0.22, 0.24, 0.43), 0.0);
    project.tracks[3].items.push(page("cap-1", 1.0));
    project.tracks[2].items.push(hook_at(0.74));
    let at = 1.3;
    let mut g = Graphics::new();

    // The rows the low hook really paints.
    let mut only_hook = project.clone();
    only_hook.tracks[3].items.clear();
    let mut frame = Pixmap::new(1080, 1920).unwrap();
    g.draw(&only_hook, at, &mut frame.as_mut());
    let (first, last) = painted_rows(&frame).expect("the hook is drawn");
    let (top, bottom) = (first as f32 / 1920.0, (last + 1) as f32 / 1920.0);
    let overlaps = |b: Rect| b.y < bottom && b.y + b.h > top;

    // No face: captions asked for the low spot sit on a low hook, so they leave it.
    let mut no_face = project.clone();
    no_face.faces.clear();
    let mut no_hook = no_face.clone();
    no_hook.tracks[2].items.clear();
    let free = g.caption_placement_at(&no_hook, at).unwrap();
    assert!(overlaps(free.bounds), "{free:?} vs hook {top}..{bottom}");
    let placed = g.caption_placement_at(&no_face, at).unwrap();
    assert_eq!(placed.slot, Slot::Upper, "{placed:?}");
    assert!(!overlaps(placed.bounds), "{placed:?}");

    // A large face blocks the low slots; the upper slot is clear of the low hook, so it is used
    // rather than hiding the page because some text is on screen.
    let placed = g.caption_placement_at(&project, at).unwrap();
    assert_eq!((placed.face, placed.slot), (FaceFit::Moved, Slot::Upper));
    assert!(!overlaps(placed.bounds), "{placed:?}");

    // With the hook up there too, no slot is clear of both: the page is not drawn.
    project.tracks[2].items = vec![hook_at(0.2)];
    let placed = g.caption_placement_at(&project, at).unwrap();
    assert_eq!(placed.face, FaceFit::Hidden, "{placed:?}");
    assert_eq!(caption_report(&mut g, &project, 5).hidden, 1);
}

#[test]
fn a_position_the_safe_area_overrides_is_reported_with_the_box_actually_drawn() {
    let mut project = Project::new("clamp", VideoKind::default(), Canvas::vertical());
    project.captions.size = 0.06;
    project.tracks[3].items.push(page("cap-1", 0.0));
    let mut g = Graphics::new();
    let mut at = |position: f32| {
        project.captions.position = position;
        g.caption_placement_at(&project, 0.5).unwrap()
    };
    let (low, lower) = (at(0.78), at(0.84));
    assert!(low.clamped && lower.clamped, "{low:?} {lower:?}");
    assert_eq!(lower.requested, 0.84);
    // Both land on the same box: the safe area's bottom edge (y 1600 of 1920).
    assert_eq!(low.bounds, lower.bounds);
    assert!(lower.bounds.y + lower.bounds.h <= 1600.0 / 1920.0 + 1e-4);
    assert!(lower.bounds.y + lower.bounds.h / 2.0 < 0.84 - 0.01);
    assert_eq!(lower.face, FaceFit::Off);
    assert!(!at(0.5).clamped);
}

#[test]
fn long_words_page_on_instead_of_shrinking_a_vertical_caption() {
    // Default style on 9:16: "SHADOWED LUNAR CRATERS." is three words, inside the 20-character
    // budget, but at the requested size only one or two of them fit on a line.
    let mut project = Project::new("long", VideoKind::default(), Canvas::vertical());
    project.tracks[3].items.push(Item {
        id: "cap-1".into(),
        start: 0.0,
        duration: 1.2,
        body: ItemBody::Caption(Caption {
            words: ["Shadowed", "lunar", "craters."]
                .iter()
                .enumerate()
                .map(|(i, s)| Word {
                    text: (*s).into(),
                    start: i as f64 * 0.4,
                    end: (i + 1) as f64 * 0.4,
                    emphasis: false,
                })
                .collect(),
        }),
    });
    // `size` is the smallest text drawn across every phrase of the page.
    let placed = Graphics::new().caption_placement_at(&project, 0.2).unwrap();
    assert!(
        placed.size >= project.captions.size * 0.99,
        "shrunk: {placed:?}"
    );
}

#[test]
fn word_pop_overshoot_is_part_of_the_reported_box() {
    let mut project = Project::new("pop", VideoKind::default(), Canvas::vertical());
    project.tracks[3].items.push(Item {
        id: "cap-1".into(),
        start: 1.0,
        duration: 0.5,
        body: ItemBody::Caption(Caption {
            words: vec![Word {
                text: "Hello".into(),
                start: 0.0,
                end: 0.5,
                emphasis: false,
            }],
        }),
    });
    let mut g = Graphics::new();
    project.captions.mode = CaptionMode::Highlight;
    let rest = g.caption_placement_at(&project, 1.2).unwrap().bounds;
    project.captions.mode = CaptionMode::WordPop;
    let popped = g.caption_placement_at(&project, 1.2).unwrap().bounds;
    // The same word, grown to the top of its pop-in so the face check covers it too.
    let peak = timing::pop_peak();
    assert!(peak > 1.0);
    assert!(
        (popped.h / rest.h - peak).abs() < 1e-3 && (popped.w / rest.w - peak).abs() < 1e-3,
        "{rest:?} {popped:?}"
    );
}

#[test]
fn pages_without_any_face_data_count_as_unchecked() {
    let mut project = face_project((0.45, 0.30, 0.08, 0.14), 0.0);
    project.faces.clear();
    for (id, start) in [("first", 0.0), ("second", 3.0)] {
        project.tracks[3].items.push(page(id, start));
    }
    let report = caption_report(&mut Graphics::new(), &project, 5);
    assert_eq!(
        (report.pages, report.unchecked, report.moved, report.hidden),
        (2, 2, 0, 0)
    );
    assert_eq!(report.examples.len(), 2);
    assert!(report.examples.iter().all(|p| p.face == FaceFit::Off));
}

fn documentary_caption(canvas: Canvas) -> Project {
    let mut p = Project::new("Documentary captions", VideoKind::Documentary, canvas);
    p.captions.mode = CaptionMode::Reveal;
    p.captions.size = 0.057;
    p.captions.position = 0.52;
    p.captions.uppercase = false;
    p.captions.boxed = false;
    p.tracks[3].items.push(Item {
        id: "reveal".into(),
        start: 0.0,
        duration: 2.0,
        body: ItemBody::Caption(Caption {
            words: ["Every", "word", "matters"]
                .into_iter()
                .enumerate()
                .map(|(i, text)| Word {
                    text: text.into(),
                    start: i as f64 * 0.5,
                    end: (i + 1) as f64 * 0.5,
                    emphasis: i == 2,
                })
                .collect(),
        }),
    });
    p
}

#[test]
fn reveal_keeps_final_slots_leads_each_word_and_clears_at_page_end() {
    let mut g = Graphics::new();
    let p = documentary_caption(Canvas::vertical());
    let draw = |g: &mut Graphics, t| {
        let mut image = Pixmap::new(270, 480).unwrap();
        image.fill(tiny_skia::Color::BLACK);
        g.draw(&p, t, &mut image.as_mut());
        image
    };
    let first = draw(&mut g, 0.2);
    assert_eq!(first.data(), draw(&mut g, 0.42).data());
    let second = draw(&mut g, 0.44);
    assert_ne!(first.data(), second.data());
    assert_eq!(second.data(), draw(&mut g, 0.9).data());
    let final_page = draw(&mut g, 0.94);
    assert_ne!(second.data(), final_page.data());
    assert_eq!(final_page.data(), draw(&mut g, 1.49).data());
    let clear = draw(&mut g, 1.5);
    assert!(clear.data().chunks_exact(4).all(|p| p == [0, 0, 0, 255]));
    let ItemBody::Caption(caption) = &p.tracks[3].items[0].body else {
        unreachable!()
    };
    let mut dimensions = Vec::new();
    let mut positions = Vec::new();
    for visible in 0..=3 {
        let layer = g
            .caption_layer(
                &caption.words,
                0..3,
                3,
                0,
                &p.captions,
                p.captions.size,
                safe_area(&p.canvas),
                270,
                480,
                visible,
                false,
            )
            .unwrap();
        dimensions.push((layer.width, layer.height));
        positions.push(g.layers[&layer.key].glyphs.clone());
    }
    assert!(dimensions.windows(2).all(|pair| pair[0] == pair[1]));
    assert!(positions.windows(2).all(|pair| pair[0] == pair[1]));
    assert!(g.caption_placement_at(&p, 0.7).is_some());
    assert!(g.caption_placement_at(&p, 1.5).is_none());
    assert_eq!(g.caption_placements(&p)[0].end, 1.5);
}

#[test]
fn mega_draws_only_behind_with_visibility_and_animation() {
    let mut g = Graphics::new();
    let mut p = Project::new("Mega", VideoKind::Documentary, Canvas::vertical());
    p.tracks[2].items.push(sample(TextPreset::Mega, "WORLD"));
    let mut frame = Pixmap::new(270, 480).unwrap();
    g.draw(&p, 1.0, &mut frame.as_mut());
    assert!(frame.data().iter().all(|&b| b == 0));
    g.draw_behind(&p, 1.0, &mut frame.as_mut());
    assert!(frame.data().chunks_exact(4).any(|p| p[3] > 0));
    for at in [-0.1, 0.0, 4.0, f64::NAN] {
        frame.fill(tiny_skia::Color::TRANSPARENT);
        g.draw_behind(&p, at, &mut frame.as_mut());
        assert!(frame.data().iter().all(|&b| b == 0));
    }
    p.tracks[2].hidden = true;
    g.draw_behind(&p, 1.0, &mut frame.as_mut());
    assert!(frame.data().iter().all(|&b| b == 0));
}

#[test]
fn ink_switches_only_white_on_light_pictures() {
    let mut g = Graphics::new();
    let mut p = documentary_caption(Canvas::vertical());
    let render = |g: &mut Graphics, p: &Project, light: bool| {
        let mut frame = Pixmap::new(270, 480).unwrap();
        frame.fill(if light {
            tiny_skia::Color::WHITE
        } else {
            tiny_skia::Color::BLACK
        });
        g.draw(p, 1.0, &mut frame.as_mut());
        frame
    };
    let dark = render(&mut g, &p, false);
    let light_scrim = render(&mut g, &p, true);
    p.captions.ink_on_light = true;
    assert_eq!(dark.data(), render(&mut g, &p, false).data());
    let ink = render(&mut g, &p, true);
    assert_ne!(light_scrim.data(), ink.data());
    assert!(
        ink.data()
            .chunks_exact(4)
            .any(|p| p[0] == 17 && p[1] == 17 && p[2] == 17)
    );
    p.captions.color = Color::rgb(200, 20, 100);
    let custom = render(&mut g, &p, true);
    assert!(
        custom
            .data()
            .chunks_exact(4)
            .any(|p| p[..3] == [200, 20, 100])
    );
    p.tracks[3].items.clear();
    p.tracks[2]
        .items
        .push(sample(TextPreset::Keyword, "White *accent*"));
    let ink = render(&mut g, &p, true);
    assert!(ink.data().chunks_exact(4).any(|p| p[..3] == [17, 17, 17]));
    assert!(
        ink.data()
            .chunks_exact(4)
            .any(|p| p[..3] == [ACCENT.r, ACCENT.g, ACCENT.b])
    );
    let dark = render(&mut g, &p, false);
    assert!(
        dark.data()
            .chunks_exact(4)
            .any(|p| p[..3] == [255, 255, 255])
    );
}

/// Documentary presets, motion phases and caption contrast at full export sizes.
/// Run with `cargo test -p veecut-engine --no-default-features documentary_proof -- --ignored --nocapture`.
#[test]
#[ignore]
fn documentary_proof() {
    let dir = std::path::Path::new("/tmp/veecut-doc/Graphics");
    std::fs::create_dir_all(dir).unwrap();
    let mut g = Graphics::new();
    let mut preview = Canvas::vertical();
    preview.width = 540;
    preview.height = 960;
    for canvas in [Canvas::vertical(), Canvas::landscape(), preview] {
        for (preset, text) in [
            (TextPreset::Keyword, "TRUTH"),
            (TextPreset::Chapter, "New horizons"),
            (TextPreset::Shock, "EXPOSED"),
            (TextPreset::BlackCard, "nothing"),
            (TextPreset::Mega, "WORLD"),
        ] {
            let mut p = Project::new("Documentary type", VideoKind::Documentary, canvas.clone());
            let mut item = sample(preset, text);
            if let ItemBody::Text(text) = &mut item.body {
                text.anim_in = preset.default_anim();
                text.anim_out = preset.default_anim();
            }
            p.tracks[2].items.push(item);
            for (phase, t) in [("entrance", 1.0 / 30.0), ("settled", 1.0), ("exit", 3.95)] {
                let mut frame = gradient(canvas.width, canvas.height);
                g.draw_behind(&p, t, &mut frame.as_mut());
                g.draw(&p, t, &mut frame.as_mut());
                frame
                    .save_png(dir.join(format!(
                        "{preset:?}-{phase}-{}x{}.png",
                        canvas.width, canvas.height
                    )))
                    .unwrap();
            }
        }
        for light in [false, true] {
            for ink in [false, true] {
                let mut p = documentary_caption(canvas.clone());
                p.captions.ink_on_light = ink;
                for (phase, t) in [
                    ("first", 0.1),
                    ("second", 0.6),
                    ("all", 1.1),
                    ("clear", 1.6),
                ] {
                    let mut frame = Pixmap::new(canvas.width, canvas.height).unwrap();
                    let v = if light { 235 } else { 24 };
                    frame.fill(tiny_skia::Color::from_rgba8(v, v, v, 255));
                    g.draw(&p, t, &mut frame.as_mut());
                    frame
                        .save_png(dir.join(format!(
                            "Reveal-light{light}-ink{ink}-{phase}-{}x{}.png",
                            canvas.width, canvas.height
                        )))
                        .unwrap();
                }
            }
        }
    }
    let p = documentary_caption(Canvas::vertical());
    let mut frame = Pixmap::new(1080, 1920).unwrap();
    frame.fill(tiny_skia::Color::BLACK);
    g.draw(&p, 1.0, &mut frame.as_mut());
    let start = std::time::Instant::now();
    for _ in 0..300 {
        g.draw(&p, 1.0, &mut frame.as_mut());
    }
    println!(
        "Documentary Reveal warm draw at 1080x1920: {:.3} ms/frame (300 frames, excludes clear/decode)",
        start.elapsed().as_secs_f64() * 1000.0 / 300.0
    );
}
