use std::path::Path;

use serde::{Deserialize, Serialize};
use veecut_engine::{
    project::{FaceSample, Project, VideoKind},
    transcript::Transcript,
};

use crate::data::{Fixture, FixtureFile};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub level: u8,
    pub prompt: String,
    pub check: Check,
    pub provenance: String,
    pub prerequisite: Option<String>,
    /// Extra fixture files copied into this run's inbox, never linked.
    pub inputs: Vec<FixtureFile>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Check {
    Run {
        kind: VideoKind,
        sources: Vec<String>,
        width: u32,
        height: u32,
    },
    CaptionSize {
        size: f32,
    },
    Highlight {
        color: String,
    },
    Hook {
        item: Option<String>,
        text: String,
    },
    Cut {
        asset: String,
        start: f64,
        end: f64,
        max_removed: f64,
    },
    Start {
        asset: String,
        start: f64,
    },
    Broll {
        file: String,
        asset: String,
        source_time: f64,
    },
    LowerThird {
        name: String,
        role: String,
    },
    Music {
        file: String,
    },
    Clips {
        topic: Vec<String>,
    },
    Podcast {
        cam_a: String,
        cam_b: String,
        offset: f64,
    },
    Tutorial {
        silences: Vec<(f64, f64)>,
    },
    Vlog {
        beats: Vec<f64>,
    },
    Explainer {
        sentences: usize,
    },
    Promo {
        item: String,
        text: String,
        beats: Vec<f64>,
    },
    Lyric {
        at: f64,
        text: String,
    },
    /// Frozen baseline detections, not the model-editable project faces. Source coordinates.
    Mouth {
        asset: String,
        samples: Vec<FaceSample>,
    },
}

pub fn words(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|w| {
            w.chars()
                .filter(|c| c.is_alphanumeric())
                .flat_map(char::to_lowercase)
                .collect::<String>()
        })
        .filter(|w| !w.is_empty())
        .collect()
}

pub fn filename(path: &Path) -> String {
    path.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

pub fn canvas(kind: VideoKind) -> (u32, u32) {
    match kind {
        VideoKind::TalkingHeadLong | VideoKind::Tutorial | VideoKind::Podcast => (1920, 1080),
        _ => (1080, 1920),
    }
}

pub fn templates(
    fixture: &Fixture,
    all: &[Fixture],
    before: &[Project],
    transcripts: &[Transcript],
    beats: &[f64],
) -> Vec<Task> {
    use veecut_engine::project::{ItemBody, TextPreset};
    let kind = fixture.video_kind();
    let project = before.first();
    let (width, height) = canvas(kind);
    let primary: Vec<_> = fixture
        .files
        .iter()
        .filter(|f| {
            matches!(
                f.role.as_str(),
                "source" | "cam-a" | "cam-b" | "narration" | "broll" | "visual" | "music"
            )
        })
        .collect();
    let description = match kind {
        VideoKind::TalkingHeadShort => "Make a Reel",
        VideoKind::TalkingHeadLong => "Edit my long talking-head video",
        VideoKind::Tutorial => "Edit my screen recording",
        VideoKind::Podcast => "Edit this podcast",
        VideoKind::Clips => "Make three short clips",
        VideoKind::Vlog => "Make a vlog from these clips with this song",
        VideoKind::Explainer => "Make an explainer from this narration and these pictures",
        VideoKind::Promo => "Make a product promo from these clips, music and copy",
        VideoKind::MusicVideo => "Make a music video with these visuals, song and lyrics",
        VideoKind::Documentary => "Make a fast documentary edit of my talking-head video",
    };
    let files = fixture
        .files
        .iter()
        .map(|f| format!("inbox/{} ({})", filename(&f.path), f.role))
        .collect::<Vec<_>>()
        .join(", ");
    let mut out = vec![];
    let mut add =
        |id: &str, level, prompt: String, check, provenance: String, prerequisite, inputs| {
            out.push(Task {
                id: id.into(),
                level,
                prompt,
                check,
                provenance,
                prerequisite,
                inputs,
            })
        };
    add(
        "run",
        1,
        format!(
            "{description} from {files}. Use the {kind} workflow and {width}x{height} canvas. Save the project; do not render. Use local CPU transcription only; never upload media."
        ),
        Check::Run {
            kind,
            sources: primary.iter().map(|f| filename(&f.path)).collect(),
            width,
            height,
        },
        "Manifest file roles; engine kind canvas defaults".into(),
        None,
        vec![],
    );
    let missing = project
        .is_none()
        .then(|| "No complete baseline project".into());
    let size = project.map_or(0.06, |p| p.captions.size * 1.25);
    add(
        "caption-size",
        2,
        format!("Make captions bigger: set caption size to {size:.6}. Change nothing else."),
        Check::CaptionSize { size },
        "Baseline caption size multiplied by 1.25".into(),
        missing.clone(),
        vec![],
    );
    let color = if project.is_some_and(|p| {
        serde_json::to_value(p.captions.highlight)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .is_some_and(|c| c.eq_ignore_ascii_case("#00ff00"))
    }) {
        "#00CC66"
    } else {
        "#00FF00"
    };
    add(
        "highlight",
        2,
        format!("Make the caption highlight green ({color}). Change nothing else."),
        Check::Highlight {
            color: color.into(),
        },
        "Explicit requested color".into(),
        missing.clone(),
        vec![],
    );
    let hook = project.and_then(|p| {
        p.tracks
            .iter()
            .flat_map(|t| &t.items)
            .find(|i| matches!(&i.body, ItemBody::Text(t) if t.preset == TextPreset::Hook))
    });
    let text = format!("A closer look: {}", fixture.id);
    add(
        "hook",
        2,
        if hook.is_some() {
            format!("Change the hook to '{text}'. Change nothing else.")
        } else {
            format!(
                "Add a title card at the start saying '{text}'. Keep existing content unchanged."
            )
        },
        Check::Hook {
            item: hook.map(|i| i.id.clone()),
            text,
        },
        "New requested title, not factual source truth".into(),
        missing.clone(),
        vec![],
    );

    let mut phrases = vec![];
    for key in ["phrases", "sentences", "lyric_checkpoints"] {
        if let Some(values) = fixture.notes[key].as_array() {
            phrases.extend(values.iter().filter_map(|p| {
                Some((
                    p["text"].as_str()?.to_owned(),
                    p["start_s"]
                        .as_f64()
                        .or_else(|| p["cam_a_s"].as_f64())
                        .unwrap_or(0.0),
                ))
            }));
        }
    }
    let matched = phrases
        .iter()
        .filter_map(|(phrase, hint)| {
            let wanted = words(phrase);
            if wanted.is_empty() {
                return None;
            }
            transcripts.iter().find_map(|t| {
                let tokens: Vec<_> = t.words.iter().map(|w| words(&w.text).join("")).collect();
                tokens
                    .windows(wanted.len())
                    .enumerate()
                    .filter(|(_, w)| *w == wanted.as_slice())
                    .min_by(|(a, _), (b, _)| {
                        (t.words[*a].start - hint)
                            .abs()
                            .total_cmp(&(t.words[*b].start - hint).abs())
                    })
                    .and_then(|(i, _)| {
                        let start = t.words[i].start;
                        let end = t.words[i + wanted.len() - 1].end;
                        let p = project?;
                        // A task must change something still present, not ask to remove a prior recipe cut.
                        if p.source_to_timeline(&t.asset, (start + end) / 2.0)
                            .is_none()
                        {
                            return None;
                        }
                        let sentence = t
                            .sentences()
                            .into_iter()
                            .find(|s| s.start <= start && s.end >= end);
                        Some((
                            phrase.clone(),
                            t.asset.clone(),
                            start,
                            end,
                            sentence.map_or(end - start + 2.0, |s| s.end - s.start + 2.0),
                        ))
                    })
            })
        })
        .take(2)
        .last();
    if !phrases.is_empty() {
        let (phrase, asset, start, end, max_removed) = matched
            .clone()
            .unwrap_or_else(|| (phrases[0].0.clone(), String::new(), 0.0, 0.0, 0.0));
        let unavailable = matched.is_none().then(|| "No manifest phrase matched baseline word-level transcript within kept footage; cue times alone are not word truth".into());
        let provenance = format!(
            "Manifest phrase matched verbatim (punctuation/case normalized) to frozen baseline transcript; {}",
            fixture.notes["phrase_timing"]
                .as_str()
                .unwrap_or("manifest sentence timing")
        );
        add(
            "cut-phrase",
            3,
            format!(
                "Cut the part where they say '{phrase}'. Remove only that sentence, with at most two seconds extra."
            ),
            Check::Cut {
                asset: asset.clone(),
                start,
                end,
                max_removed,
            },
            provenance.clone(),
            unavailable.clone(),
            vec![],
        );
        add(
            "start-phrase",
            3,
            format!("Start the video at '{phrase}'. Keep everything after it."),
            Check::Start {
                asset: asset.clone(),
                start,
            },
            provenance.clone(),
            unavailable.clone(),
            vec![],
        );
        let broll = fixture
            .files
            .iter()
            .chain(all.iter().flat_map(|f| &f.files))
            .find(|f| {
                matches!(f.role.as_str(), "broll" | "visual")
                    && project.is_none_or(|p| {
                        !p.assets
                            .iter()
                            .any(|a| filename(&a.path) == filename(&f.path))
                    })
            });
        if let Some(file) = broll {
            add(
                "broll",
                4,
                format!(
                    "Add inbox/{} as B-roll when they say '{phrase}', for about 3 seconds.",
                    filename(&file.path)
                ),
                Check::Broll {
                    file: filename(&file.path),
                    asset,
                    source_time: start,
                },
                provenance,
                unavailable,
                vec![file.clone()],
            );
        }
    }
    let (name, role) = match fixture.id.as_str() {
        "ths-m-katelynn-greer" => ("Katelynn Greer", "Research scientist"),
        "ths-s-mark-pond-31s" | "ths-m-mark-pond" => ("Mark Pond", "Northrop Grumman"),
        "ths-s-charles-precourt" => ("Charlie Precourt", "Northrop Grumman"),
        _ => ("VeeCut evaluation", "Source footage"),
    };
    add("lower-third", 4, format!("Add a lower third in the first three seconds: '{name}', '{role}'."), Check::LowerThird { name: name.into(), role: role.into() }, "Manifest speaker description where available; otherwise explicitly requested neutral label".into(), missing.clone(), vec![]);
    if let Some(file) = all.iter().flat_map(|f| &f.files).find(|f| {
        f.role == "music"
            && project.is_none_or(|p| {
                !p.assets
                    .iter()
                    .any(|a| filename(&a.path) == filename(&f.path))
            })
    }) {
        add(
            "music-bed",
            4,
            format!(
                "Put inbox/{} quietly under the whole video, looping if needed. Keep dialogue audible.",
                filename(&file.path)
            ),
            Check::Music {
                file: filename(&file.path),
            },
            "Manifest music input; coverage and gain measured on timeline".into(),
            missing.clone(),
            vec![file.clone()],
        );
    }
    match kind {
        VideoKind::Clips => {
            let topic: Vec<_> = phrases
                .iter()
                .flat_map(|(p, _)| words(p))
                .filter(|w| w.len() > 5)
                .take(3)
                .collect();
            let prerequisite = topic
                .is_empty()
                .then(|| "No topic words in manifest".into());
            add(
                "topic-clips",
                5,
                format!(
                    "Make exactly three distinct clips of 20 to 60 seconds about {}. Each clip must include at least one of these topic words.",
                    topic.join(", ")
                ),
                Check::Clips { topic },
                "Topic words taken from manifest phrases; checked against kept baseline transcript"
                    .into(),
                prerequisite,
                vec![],
            );
        }
        VideoKind::Podcast if fixture.notes["camera_offset_s"].is_number() => {
            let a = fixture.files.iter().find(|f| f.role == "cam-a");
            let b = fixture.files.iter().find(|f| f.role == "cam-b");
            if let (Some(a), Some(b)) = (a, b) {
                add("two-camera",5,"Edit this two-camera podcast: synchronize camera B to A and switch between both cameras.".into(),Check::Podcast {cam_a:filename(&a.path),cam_b:filename(&b.path),offset:fixture.notes["camera_offset_s"].as_f64().unwrap()},fixture.notes["offset_meaning"].as_str().unwrap_or("manifest synthetic camera offset").into(),missing.clone(),vec![]);
            }
        }
        VideoKind::Tutorial => {
            let silences = fixture.notes["silences_s"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|v| Some((v[0].as_f64()?, v[1].as_f64()?)))
                .collect::<Vec<_>>();
            let unavailable = silences
                .is_empty()
                .then(|| "No known silent source ranges".into());
            add(
                "silent-speed",
                5,
                "Speed up the silent parts, not the talking. Keep the speech at normal speed."
                    .into(),
                Check::Tutorial { silences },
                fixture.notes["silence_method"]
                    .as_str()
                    .unwrap_or("manifest silence ranges")
                    .into(),
                unavailable,
                vec![],
            );
        }
        VideoKind::Vlog => add(
            "beat-vlog",
            5,
            "Cut this vlog to the beat, about 60 seconds long.".into(),
            Check::Vlog {
                beats: beats.to_vec(),
            },
            "Frozen engine audio beat analysis of manifest music; not a zero-phase BPM assumption"
                .into(),
            beats
                .is_empty()
                .then(|| "No independently analyzed beat timestamps".into()),
            vec![],
        ),
        VideoKind::Explainer => {
            let sentences = fixture.notes["sentences"]
                .as_array()
                .map(Vec::len)
                .unwrap_or_else(|| transcripts.first().map_or(0, |t| t.sentences().len()));
            add("sentence-pictures",5,"Use one picture per sentence of narration.".into(),Check::Explainer {sentences},"Exact manifest sentence list when present; otherwise frozen baseline transcript sentence segmentation".into(),(sentences==0).then(||"No sentence ground truth".into()),vec![]);
        }
        VideoKind::Promo => {
            let item = project.and_then(|p| {
                p.tracks
                    .iter()
                    .flat_map(|t| &t.items)
                    .find(|i| matches!(&i.body, ItemBody::Text(_)))
            });
            let text = "Built for the next giant leap.".to_owned();
            add(
                "promo-line",
                5,
                format!(
                    "Change the first copy line to '{text}', keep it on the beat, and leave the other lines unchanged."
                ),
                Check::Promo {
                    item: item.map_or(String::new(), |i| i.id.clone()),
                    text,
                    beats: beats.to_vec(),
                },
                "Explicit new copy; frozen music beat timestamps".into(),
                (item.is_none() || beats.is_empty())
                    .then(|| "Missing baseline text item or independently analyzed beats".into()),
                vec![],
            );
        }
        VideoKind::MusicVideo => {
            if let Some(v) = fixture.notes["lyric_checkpoints"]
                .as_array()
                .and_then(|v| v.first())
            {
                let at = v["start_s"].as_f64().unwrap_or(0.0);
                let text = format!("{} [chorus]", v["text"].as_str().unwrap_or(""));
                add("lyric",5,format!("Change the lyric at {at:.2} seconds to '{text}'. Keep its timing."),Check::Lyric {at,text},"Explicit editorial replacement of manifest lyric checkpoint; not a transcription correction claim".into(),missing.clone(),vec![]);
            }
        }
        _ => {}
    }
    if matches!(
        kind,
        VideoKind::TalkingHeadShort | VideoKind::TalkingHeadLong | VideoKind::Podcast
    ) {
        let sample = project.and_then(|p| {
            p.faces.iter().find(|(_, v)| {
                v.iter()
                    .any(|s| s.h >= 0.15 && s.w > 0.0 && s.h.is_finite())
            })
        });
        let (asset, samples) = sample.map_or((String::new(), vec![]), |(a, s)| {
            (
                a.clone(),
                s.iter()
                    .copied()
                    .filter(|s| s.h >= 0.15 && s.w > 0.0 && s.h.is_finite())
                    .collect(),
            )
        });
        let unavailable = samples.is_empty().then(|| {
            "No usable independent close-up face detections; mouth safety cannot be certified"
                .into()
        });
        add("mouth-safe",6,"Make sure the captions never cover the mouth in close-up shots. Keep captions visible and preserve the spoken content.".into(),Check::Mouth {asset,samples},"Frozen baseline frontal-face detections, never model-written faces. Mouth/chin ROI uses engine geometry (lower 45% plus 12% chin margin); detector-derived proxy, not manually annotated mouth truth.".into(),unavailable,vec![]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use veecut_engine::project::{Asset, AssetKind, AssetRole, Canvas, Clip, Item, ItemBody};
    use veecut_engine::transcript::TranscriptWord;
    #[test]
    fn content_templates_need_real_kept_word_truth() {
        let fixture = Fixture {
            id: "synthetic".into(),
            kind: "talking-head-short".into(),
            length: "S".into(),
            files: vec![],
            language: Some("en".into()),
            notes: serde_json::json!({"phrases":[{"start_s":99.0,"text":"Hello Moon."}]}),
        };
        let mut project = Project::new("test", VideoKind::TalkingHeadShort, Canvas::vertical());
        project.assets.push(Asset {
            id: "a".into(),
            path: "source.mp4".into(),
            kind: AssetKind::Video,
            role: AssetRole::Source,
            duration: 20.0,
            width: 1080,
            height: 1920,
            fps: 30.0,
            has_audio: true,
        });
        project.main_mut().items.push(Item {
            id: "clip".into(),
            start: 0.0,
            duration: 20.0,
            body: ItemBody::Clip(Clip::new("a", 0.0)),
        });
        let transcript = Transcript {
            asset: "a".into(),
            provider: "synthetic".into(),
            language: Some("en".into()),
            duration: 20.0,
            words: vec![
                TranscriptWord {
                    text: "Hello".into(),
                    start: 5.0,
                    end: 5.4,
                    confidence: None,
                },
                TranscriptWord {
                    text: "Moon.".into(),
                    start: 5.5,
                    end: 6.0,
                    confidence: None,
                },
            ],
        };
        let tasks = templates(
            &fixture,
            std::slice::from_ref(&fixture),
            std::slice::from_ref(&project),
            &[transcript],
            &[],
        );
        let cut = tasks.iter().find(|t| t.id == "cut-phrase").unwrap();
        assert!(cut.prerequisite.is_none());
        assert!(matches!(
            cut.check,
            Check::Cut {
                start: 5.0,
                end: 6.0,
                ..
            }
        ));
        assert!(
            tasks
                .iter()
                .find(|t| t.id == "mouth-safe")
                .unwrap()
                .prerequisite
                .is_some()
        );
        let missing = templates(&fixture, &[], &[project], &[], &[]);
        assert!(
            missing
                .iter()
                .find(|t| t.id == "cut-phrase")
                .unwrap()
                .prerequisite
                .is_some()
        );
    }
}
