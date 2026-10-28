//! One pipeline per video kind: from raw footage to a finished draft. Recipes make every
//! decision a careful editor would make by rule (cuts, framing, punch-ins, captions, music,
//! look), so an agent, even a small free model, only has to review the draft and pass on the
//! user's notes. The rules come from Herk Edit Kit's editing playbook and free247editor (both
//! MIT); see CREDITS.md.

mod beat;
mod speech;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::analyze::{self, CutSettings};
use crate::config::Config;
use crate::ops::{self, Op};
use crate::project::{
    Anim, AssetKind, AssetRole, Canvas, CaptionMode, ItemBody, KenBurns, Point, Project,
    TextPreset, TrackKind, VideoKind,
};
use crate::render::{self, Quality, RenderOptions};
use crate::style::{Format, Pace, Sound, Style};
use crate::transcribe::{self, Provider, TranscribeOptions};
use crate::transcript::Transcript;
use crate::workspace::Workspace;
use crate::{captions, media, qa};

/// What to edit.
#[derive(Clone, Debug)]
pub struct EditRequest {
    pub kind: VideoKind,
    /// The main recordings: the talking head, the screen recording, podcast cameras, the
    /// footage for a montage.
    pub sources: Vec<PathBuf>,
    /// Cutaways and images.
    pub broll: Vec<PathBuf>,
    pub music: Option<PathBuf>,
    /// Narration or a separate microphone recording.
    pub voice: Option<PathBuf>,
    /// Text the edit shows: promo lines, list items, or lyrics (plain lines or LRC).
    pub copy: Option<String>,
    pub canvas: Option<Canvas>,
    pub name: Option<String>,
    /// Hook text for the first seconds; the recipe picks one from the first sentence when unset.
    pub hook: Option<String>,
    /// Spoken language code (`en`, `tl`); falls back to the style, then auto-detect.
    pub language: Option<String>,
    /// How many clips the `clips` recipe makes.
    pub clips: usize,
    /// Render a draft at the end.
    pub render: bool,
}

impl EditRequest {
    pub fn new(kind: VideoKind) -> Self {
        EditRequest {
            kind,
            sources: Vec::new(),
            broll: Vec::new(),
            music: None,
            voice: None,
            copy: None,
            canvas: None,
            name: None,
            hook: None,
            language: None,
            clips: 3,
            render: true,
        }
    }
}

/// What a recipe made.
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct EditReport {
    /// Project folders (one, or one per clip for the `clips` recipe).
    pub projects: Vec<PathBuf>,
    /// Rendered drafts.
    pub drafts: Vec<PathBuf>,
    /// What the recipe did, in a few plain lines.
    pub summary: Vec<String>,
    /// Decisions left to a person or an agent, such as possible retakes the recipe kept.
    pub review: Vec<String>,
}

/// Runs the recipe for `req.kind`.
pub fn edit(
    ws: &Workspace,
    req: &EditRequest,
    progress: &mut dyn FnMut(&str),
) -> Result<EditReport> {
    let style = Style::load(&ws.style_path())?;
    let mut job = Job {
        ws,
        req,
        style,
        report: EditReport::default(),
    };
    match req.kind {
        VideoKind::TalkingHeadShort | VideoKind::TalkingHeadLong | VideoKind::Tutorial => {
            speech::talking_head(&mut job, progress)?
        }
        VideoKind::Podcast => speech::podcast(&mut job, progress)?,
        VideoKind::Clips => speech::clips(&mut job, progress)?,
        VideoKind::Explainer => speech::explainer(&mut job, progress)?,
        VideoKind::Vlog | VideoKind::Promo | VideoKind::MusicVideo => {
            beat::edit(&mut job, progress)?
        }
    }
    Ok(job.report)
}

pub(crate) struct Job<'a> {
    pub ws: &'a Workspace,
    pub req: &'a EditRequest,
    pub style: Style,
    pub report: EditReport,
}

impl Job<'_> {
    /// The canvas for this kind: the request's, else the kind's natural shape, else the style's format.
    fn canvas(&self, source_fps: f64) -> Canvas {
        let mut canvas = match (&self.req.canvas, self.req.kind) {
            (Some(c), _) => c.clone(),
            (None, VideoKind::TalkingHeadShort | VideoKind::Clips) => Canvas::vertical(),
            (None, VideoKind::TalkingHeadLong | VideoKind::Tutorial | VideoKind::Podcast) => {
                Canvas::landscape()
            }
            (None, _) => self
                .style
                .canvases()
                .into_iter()
                .next()
                .unwrap_or_else(Canvas::vertical),
        };
        if self.req.canvas.is_none() {
            canvas.fps = match source_fps {
                f if (f - 25.0).abs() < 0.6 => 25.0,
                f if (f - 24.0).abs() < 0.6 || (f - 23.976).abs() < 0.05 => 24.0,
                _ => 30.0,
            };
        }
        canvas
    }

    /// Creates the project with the user's style applied and the chosen canvas.
    fn start(&mut self, name: &str, canvas: Canvas) -> Result<(PathBuf, Project)> {
        let (dir, mut project) = self
            .ws
            .create_project(name, self.req.kind, canvas.clone())?;
        self.style.apply_to(&mut project);
        project.canvas = canvas;
        Ok((dir, project))
    }

    fn name(&self) -> String {
        self.req
            .name
            .clone()
            .or_else(|| {
                let first = self
                    .req
                    .sources
                    .first()
                    .or(self.req.voice.as_ref())
                    .or(self.req.music.as_ref())?;
                Some(
                    first
                        .file_stem()?
                        .to_string_lossy()
                        .replace(['_', '.'], " "),
                )
            })
            .unwrap_or_else(|| self.req.kind.slug().to_string())
    }

    fn cut_settings(&self) -> CutSettings {
        let tight = self.style.pace() == Pace::Tight || self.req.kind == VideoKind::Clips;
        // Herk Edit Kit: 0.55 s keeps natural rhythm; its measured reel style leaves pauses
        // between phrases under about 0.2 s.
        let min_silence = match (tight, self.req.kind) {
            (true, _) => 0.25,
            (false, VideoKind::Podcast) => 0.8,
            (false, _) => 0.55,
        };
        CutSettings {
            min_silence,
            ..CutSettings::default()
        }
    }

    fn language(&self) -> Option<String> {
        self.req.language.clone().or_else(|| {
            let l = self.style.language.trim();
            (!l.is_empty() && l != "auto").then(|| l.to_string())
        })
    }

    /// The transcript of an asset, cached in the project folder.
    fn transcript(
        &self,
        dir: &Path,
        project: &Project,
        asset_id: &str,
        progress: &mut dyn FnMut(&str),
    ) -> Result<Transcript> {
        let path = transcript_path(dir, asset_id);
        if let Ok(t) = Transcript::load(&path) {
            if t.asset == asset_id {
                return Ok(t);
            }
        }
        let asset = project
            .asset(asset_id)
            .context("transcript of an unknown asset")?;
        let file = project.asset_path(dir, asset);
        let config = Config::load()?;
        let provider = match config.transcribe.provider.as_str() {
            "groq" => Some(Provider::Groq),
            "local" => Some(Provider::Local),
            _ => None,
        };
        let opts = TranscribeOptions {
            provider,
            language: self.language(),
            model: Some(config.transcribe.local_model.clone()).filter(|m| !m.is_empty()),
        };
        progress(&format!(
            "Transcribing {}",
            file.file_name().unwrap_or_default().to_string_lossy()
        ));
        let transcript = transcribe::transcribe(&file, asset_id, &opts, progress)?;
        transcript.save(&path)?;
        Ok(transcript)
    }

    /// Saves, renders a draft when asked, checks it, and records the result.
    fn finish(
        &mut self,
        dir: &Path,
        project: &mut Project,
        progress: &mut dyn FnMut(&str),
    ) -> Result<()> {
        project.normalize();
        project
            .validate()
            .context("the recipe built an invalid project")?;
        self.ws.save_project(dir, project)?;
        self.report.projects.push(dir.to_path_buf());
        let length = project.duration();
        self.report.summary.push(format!(
            "{}: {} at {}x{}, {:.1} s, {} clips on the main track",
            project.name,
            project.kind.label(),
            project.canvas.width,
            project.canvas.height,
            length,
            project.main().items.len()
        ));
        if !self.req.render {
            return Ok(());
        }
        let slug = dir
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let out = self.ws.next_draft_path(&slug);
        self.render_draft(dir, project, &out, progress)?;
        // Style format `both`: the other shape too, from the same edit. Positions are fractions
        // of the frame and the crop follows the face, so text and framing re-place themselves.
        if self.style.format == Format::Both && self.req.canvas.is_none() {
            let mut other = project.clone();
            let flip = if other.canvas.height > other.canvas.width {
                Canvas::landscape()
            } else {
                Canvas::vertical()
            };
            other.canvas.width = flip.width;
            other.canvas.height = flip.height;
            let stem = out
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            let alt = out.with_file_name(format!(
                "{stem}-{}.mp4",
                flip.aspect_name().replace(':', "x")
            ));
            self.render_draft(dir, &other, &alt, progress)?;
        }
        Ok(())
    }

    fn render_draft(
        &mut self,
        dir: &Path,
        project: &Project,
        out: &Path,
        progress: &mut dyn FnMut(&str),
    ) -> Result<()> {
        progress(&format!("Rendering {}", out.display()));
        let opts = RenderOptions::for_canvas(&project.canvas, Quality::Draft);
        let mut last = -1.0;
        render::export(project, dir, out, &opts, &mut |p| {
            if p.total > 0.0 && p.done - last >= p.total / 10.0 {
                last = p.done;
                progress(&format!("{} {:.0}%", p.stage, 100.0 * p.done / p.total));
            }
            true
        })?;
        match qa::check(out) {
            Ok(report) => self
                .report
                .summary
                .extend(report.problems.iter().map(|p| format!("Check: {p}"))),
            Err(e) => self
                .report
                .summary
                .push(format!("Check could not run: {e:#}")),
        }
        self.report.drafts.push(out.to_path_buf());
        Ok(())
    }
}

/// The first asset transcribed in a project owns `transcript.json` (what `veecut transcript` and
/// caption rebuilds read); any other asset gets `transcript-<asset>.json`.
pub(crate) fn transcript_path(dir: &Path, asset_id: &str) -> PathBuf {
    let main = dir.join("transcript.json");
    match Transcript::load(&main) {
        Ok(t) if t.asset != asset_id => dir.join(format!("transcript-{asset_id}.json")),
        _ => main,
    }
}

/// Lays source ranges back to back on the main track.
pub(crate) fn lay_main(project: &mut Project, asset: &str, keeps: &[(f64, f64)]) -> Result<()> {
    let ops: Vec<Op> = keeps
        .iter()
        .filter(|(a, b)| b - a > 1.0 / project.canvas.fps)
        .map(|&(a, b)| Op::AddClip {
            asset: asset.into(),
            track: Some("main".into()),
            start: None,
            index: None,
            src_in: a,
            duration: Some(b - a),
            frame: None,
            volume: None,
            duck: None,
        })
        .collect();
    if ops.is_empty() {
        bail!("nothing is left after cutting; is there speech in this recording?");
    }
    ops::apply_all(project, &ops)?;
    Ok(())
}

/// Places a clip on an audio track at an exact time.
pub(crate) fn place_audio(
    project: &mut Project,
    track: &str,
    asset: &str,
    start: f64,
    src_in: f64,
    duration: f64,
    volume: f32,
    duck: bool,
) -> Result<String> {
    let done = ops::apply(
        project,
        &Op::AddClip {
            asset: asset.into(),
            track: Some(track.into()),
            start: Some(start),
            index: None,
            src_in,
            duration: Some(duration),
            frame: None,
            volume: Some(volume),
            duck: Some(duck),
        },
    )?;
    Ok(done.items[0].clone())
}

/// A track id for an audio track named `name`, created when missing.
pub(crate) fn audio_track(project: &mut Project, id_hint: &str, name: &str) -> Result<String> {
    if let Some(t) = project
        .tracks
        .iter()
        .find(|t| t.kind == TrackKind::Audio && (t.id == id_hint || t.name == name))
    {
        return Ok(t.id.clone());
    }
    let done = ops::apply(
        project,
        &Op::AddTrack {
            kind: TrackKind::Audio,
            name: Some(name.into()),
        },
    )?;
    Ok(done.items[0].clone())
}

/// The music bed: the requested file, or the first file in `my-sounds/music` when the style asks
/// for music. Herk Edit Kit: the bed sits about 11 dB under the voice and ducks under speech; the
/// first seconds are quieter.
pub(crate) fn music_bed(
    job: &mut Job,
    dir: &Path,
    project: &mut Project,
    ducked: bool,
) -> Result<()> {
    let file = match &job.req.music {
        Some(f) => Some(f.clone()),
        None if job.style.sound() == Sound::SfxMusic => {
            first_audio(&job.ws.root.join("my-sounds/music"))
        }
        None => None,
    };
    let Some(file) = file else { return Ok(()) };
    let id = job.ws.add_asset(project, dir, &file, AssetRole::Music)?;
    let asset_len = project.asset(&id).map(|a| a.duration).unwrap_or(0.0);
    let total = project.duration();
    if asset_len < 1.0 || total <= 0.0 {
        return Ok(());
    }
    let volume = if ducked {
        10f32.powf(-11.0 / 20.0)
    } else {
        1.0
    };
    let track = audio_track(project, "music", "Music")?;
    let mut t = 0.0;
    let mut ids = Vec::new();
    while total - t > 0.05 {
        let len = asset_len.min(total - t);
        ids.push(place_audio(
            project, &track, &id, t, 0.0, len, volume, ducked,
        )?);
        t += len;
    }
    if let (Some(first), Some(last)) = (ids.first().cloned(), ids.last().cloned()) {
        ops::apply(
            project,
            &Op::Patch {
                item: first,
                patch: serde_json::json!({ "fade_in": 1.5 }),
            },
        )?;
        ops::apply(
            project,
            &Op::Patch {
                item: last,
                patch: serde_json::json!({ "fade_out": 1.5 }),
            },
        )?;
    }
    job.report.summary.push(format!(
        "Music: {}",
        file.file_name().unwrap_or_default().to_string_lossy()
    ));
    Ok(())
}

/// Sound effects on visible actions (text landing), never on every cut, cycling through the
/// files in `my-sounds/sfx` so repeats do not sound like a loop. Skipped for voice-only styles.
pub(crate) fn text_sfx(job: &mut Job, dir: &Path, project: &mut Project) -> Result<()> {
    if job.style.sound() == Sound::VoiceOnly {
        return Ok(());
    }
    let files = audio_files(&job.ws.root.join("my-sounds/sfx"));
    if files.is_empty() {
        return Ok(());
    }
    let starts: Vec<f64> = project
        .tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Text && !t.hidden)
        .flat_map(|t| t.items.iter())
        .filter(|i| matches!(i.body, ItemBody::Text(_)))
        .map(|i| i.start)
        .collect();
    if starts.is_empty() {
        return Ok(());
    }
    let mut ids = Vec::new();
    for file in &files {
        ids.push(job.ws.add_asset(project, dir, file, AssetRole::Sfx)?);
    }
    let track = audio_track(project, "sfx", "Sound effects")?;
    for (n, start) in starts.iter().enumerate() {
        let id = &ids[n % ids.len()];
        let len = project
            .asset(id)
            .map(|a| a.duration)
            .unwrap_or(0.0)
            .min(2.0);
        if len > 0.05 {
            place_audio(
                project,
                &track,
                id,
                (start - 0.05).max(0.0),
                0.0,
                len,
                0.5,
                false,
            )?;
        }
    }
    job.report
        .summary
        .push(format!("Sound effects on {} text moments", starts.len()));
    Ok(())
}

/// Big hook text over the first seconds, placed clear of the captions and, when known, the face.
pub(crate) fn hook(
    project: &mut Project,
    text: &str,
    until: f64,
    face: Option<Point>,
) -> Result<()> {
    let captions = if project.captions.mode == CaptionMode::Off {
        -1.0
    } else {
        project.captions.position
    };
    let clear =
        |y: f32| (y - captions).abs() >= 0.16 && face.is_none_or(|f| (y - f.y).abs() >= 0.2);
    let y = [0.52, 0.30, 0.15, 0.82]
        .into_iter()
        .find(|&y| clear(y))
        .unwrap_or(0.52);
    ops::apply(
        project,
        &Op::AddText {
            text: text.into(),
            start: 0.0,
            duration: until.clamp(1.5, 3.0),
            preset: TextPreset::Hook,
            position: Some(Point::new(0.5, y)),
            style: None,
            anim_in: Some(Anim::Pop),
            anim_out: Some(Anim::Fade),
            track: None,
        },
    )?;
    Ok(())
}

/// A hook from the first sentence: the whole sentence when it is short, else its first seven
/// words, with the strongest word in the accent color.
pub(crate) fn hook_from(transcript: &Transcript) -> Option<(String, f64)> {
    let first = transcript.sentences().into_iter().next()?;
    let take = if first.end_word - first.first <= 10 {
        first.end_word
    } else {
        first.first + 7
    };
    let words = &transcript.words[first.first..take];
    let emphasis = analyze::emphasis(words);
    let mut marked = false;
    let text: Vec<String> = words
        .iter()
        .zip(emphasis)
        .map(|(w, e)| {
            let clean = w
                .text
                .trim_matches(|c: char| ",.;:!?".contains(c))
                .to_string();
            if e && !marked && !clean.is_empty() {
                marked = true;
                format!("*{clean}*")
            } else {
                clean
            }
        })
        .filter(|w| !w.is_empty())
        .collect();
    (!text.is_empty()).then(|| (text.join(" "), words.last().map(|w| w.end).unwrap_or(3.0)))
}

/// Moves captions off the face: Herk Edit Kit never puts captions over the mouth or chin, and
/// puts them on the shirt in a punch-in.
pub(crate) fn keep_captions_off_face(job: &mut Job, project: &mut Project, face: Option<Point>) {
    let Some(face) = face else { return };
    if project.captions.mode == CaptionMode::Off {
        return;
    }
    if (project.captions.position - face.y).abs() < 0.2 {
        let to = if face.y < 0.55 { 0.74 } else { 0.22 };
        job.report.summary.push(format!(
            "Captions moved from {:.2} to {to:.2} of the height to stay off the face",
            project.captions.position
        ));
        project.captions.position = to;
    }
}

/// Captions from a transcript, unless the style turns them off.
pub(crate) fn add_captions(
    job: &mut Job,
    project: &mut Project,
    transcript: &Transcript,
) -> Result<()> {
    if project.captions.mode == CaptionMode::Off {
        return Ok(());
    }
    let pages = captions::apply(project, transcript)?;
    job.report.summary.push(format!(
        "{pages} caption pages ({:?})",
        project.captions.mode
    ));
    Ok(())
}

/// B-roll cutaways from `--broll`, Herk Edit Kit style: each file used once, full frame over the
/// speaker at a sentence start after the hook, spread evenly through the edit, never longer than
/// 3 s so the speaker is not gone for long. Footage plays muted; stills get a slow push-in.
pub(crate) fn place_broll(
    job: &mut Job,
    dir: &Path,
    project: &mut Project,
    transcript: &Transcript,
) -> Result<()> {
    if job.req.broll.is_empty() {
        return Ok(());
    }
    let mut ids = Vec::new();
    for file in &job.req.broll {
        ids.push(job.ws.add_asset(project, dir, file, AssetRole::Broll)?);
    }
    let total = project.duration();
    let moments: Vec<(f64, f64)> = transcript
        .sentences()
        .iter()
        .filter_map(|s| {
            let a = project.source_to_timeline(&transcript.asset, s.start)?;
            let b = project
                .source_to_timeline(&transcript.asset, (s.end - 0.05).max(s.start))
                .unwrap_or(a + 3.0);
            (a >= 3.0 && a < total - 2.0).then_some((a, b.max(a + 1.5)))
        })
        .collect();
    let placed = ids.len().min(moments.len());
    for (k, id) in ids.iter().take(placed).enumerate() {
        let (a, b) = moments[k * moments.len() / placed];
        let asset = project.asset(id).context("B-roll asset vanished")?.clone();
        let is_image = asset.kind == AssetKind::Image;
        let len = (b - a)
            .clamp(1.5, 3.0)
            .min(if is_image { 3.0 } else { asset.duration });
        let src_in = if is_image {
            0.0
        } else {
            (asset.duration * 0.1).min(asset.duration - len).max(0.0)
        };
        let done = ops::apply(
            project,
            &Op::AddClip {
                asset: id.clone(),
                track: Some("broll".into()),
                start: Some(a),
                index: None,
                src_in,
                duration: Some(len),
                frame: None,
                volume: None,
                duck: None,
            },
        )?;
        let patch = if is_image {
            serde_json::json!({ "muted": true, "ken_burns": KenBurns { from_scale: 1.0, to_scale: 1.08, from_focus: None, to_focus: None } })
        } else {
            serde_json::json!({ "muted": true })
        };
        ops::apply(
            project,
            &Op::Patch {
                item: done.items[0].clone(),
                patch,
            },
        )?;
    }
    if placed < ids.len() {
        job.report.review.push(format!("{} B-roll file(s) were not placed: there are more files than sentences after the hook.", ids.len() - placed));
    }
    if placed > 0 {
        job.report
            .summary
            .push(format!("{placed} B-roll cutaways at sentence starts"));
    }
    Ok(())
}

pub(crate) fn audio_files(dir: &Path) -> Vec<PathBuf> {
    const EXT: [&str; 7] = ["mp3", "wav", "m4a", "aac", "flac", "ogg", "aiff"];
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.extension()
                        .and_then(|e| e.to_str())
                        .is_some_and(|e| EXT.contains(&e.to_ascii_lowercase().as_str()))
                })
                .collect()
        })
        .unwrap_or_default();
    files.sort();
    files
}

fn first_audio(dir: &Path) -> Option<PathBuf> {
    audio_files(dir).into_iter().next()
}

/// Mono PCM of an asset at 16 kHz, for analysis.
pub(crate) fn pcm(project: &Project, dir: &Path, asset_id: &str) -> Result<Vec<f32>> {
    let asset = project.asset(asset_id).context("unknown asset")?;
    media::pcm_mono(&project.asset_path(dir, asset), SAMPLE_RATE, None)
}

pub(crate) const SAMPLE_RATE: u32 = 16_000;
