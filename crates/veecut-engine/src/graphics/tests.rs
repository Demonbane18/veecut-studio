use super::*;
use crate::project::{Caption, TextStyle, VideoKind};
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
