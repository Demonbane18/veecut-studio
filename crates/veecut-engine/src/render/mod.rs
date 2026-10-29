//! Rust compositor: decodes clips through ffmpeg, applies fit, zooms, per-clip grades, masks,
//! transitions and the color look, draws graphics, mixes audio and encodes through ffmpeg.

mod audio;
mod compose;
mod decode;
mod fx;
mod grade;
mod math;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{LazyLock, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};

use crate::graphics::Graphics;
use crate::media::ffmpeg;
use crate::project::{Canvas, Project};

use compose::{Grade, Scratch};
use decode::DecoderPool;
pub use decode::canvas_rect;

pub use audio::mix_audio;

/// Encode speed. Draft is a fast preview file; Final is the delivered file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Quality {
    Draft,
    Final,
}

/// Size, range and quality for [`export`].
#[derive(Clone, Debug)]
pub struct RenderOptions {
    pub width: u32,
    pub height: u32,
    pub range: Option<(f64, f64)>,
    pub quality: Quality,
    pub audio: bool,
}

impl RenderOptions {
    pub fn for_canvas(canvas: &Canvas, quality: Quality) -> Self {
        RenderOptions {
            width: canvas.width,
            height: canvas.height,
            range: None,
            quality,
            audio: true,
        }
    }
}

/// Passed to the export progress callback about every half second. Return false to cancel.
#[derive(Clone, Copy, Debug)]
pub struct Progress {
    pub stage: &'static str,
    pub done: f64,
    pub total: f64,
}

/// What [`export`] wrote.
#[derive(Clone, Debug)]
pub struct ExportReport {
    pub path: PathBuf,
    pub duration: f64,
    pub width: u32,
    pub height: u32,
    pub frames: u64,
    pub seconds_taken: f64,
}

/// Renders the project to an MP4 (H.264 + AAC). Returning false from `progress` cancels
/// and deletes the partial file.
pub fn export(
    project: &Project,
    project_dir: &Path,
    out: &Path,
    opts: &RenderOptions,
    progress: &mut dyn FnMut(&Progress) -> bool,
) -> Result<ExportReport> {
    let started = Instant::now();
    let (start, end) = timeline_range(project, opts.range)?;
    let fps = project.canvas.fps;
    if !(1.0..=120.0).contains(&fps) {
        bail!("canvas fps {fps} must be between 1 and 120");
    }
    let width = math::round_even(opts.width);
    let height = math::round_even(opts.height);
    let frames = ((end - start) * fps).round().max(1.0) as u64;
    let duration = frames as f64 / fps;

    if !progress(&Progress {
        stage: "audio",
        done: 0.0,
        total: 1.0,
    }) {
        bail!("export cancelled");
    }
    let wav = if opts.audio {
        let tmp = tempfile::NamedTempFile::new().context("creating a temp wav for the export")?;
        mix_audio(
            project,
            project_dir,
            tmp.path(),
            Some((start, start + duration)),
            true,
        )
        .context("mixing audio for the export")?;
        Some(tmp)
    } else {
        None
    };
    if !progress(&Progress {
        stage: "audio",
        done: 1.0,
        total: 1.0,
    }) {
        bail!("export cancelled");
    }

    if let Some(parent) = out.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
    }
    let mut partial = PartialFile {
        path: out.to_path_buf(),
        keep: false,
    };
    let mut enc = spawn_encoder(
        out,
        width,
        height,
        fps,
        wav.as_ref().map(|w| w.path()),
        opts.quality,
    )?;
    let mut stream = FrameStream::new(project, project_dir, start, width, height)?;
    let mut last_tick = Instant::now();
    for i in 0..frames {
        let Some(item) = stream.next() else {
            enc.fail(out)?;
            bail!("renderer ended at frame {i} of {frames}");
        };
        let (_t, pixmap) = match item {
            Ok(v) => v,
            Err(e) => {
                enc.fail(out)?;
                return Err(e);
            }
        };
        if let Err(e) = enc.write_frame(pixmap.data()) {
            let detail = enc.fail(out).unwrap_or_default();
            bail!("encoder stopped at frame {i}: {e}{detail}");
        }
        if last_tick.elapsed() >= Duration::from_millis(500) || i + 1 == frames {
            last_tick = Instant::now();
            if !progress(&Progress {
                stage: "video",
                done: (i + 1) as f64,
                total: frames as f64,
            }) {
                enc.fail(out)?;
                bail!("export cancelled");
            }
        }
    }
    enc.finish()
        .with_context(|| format!("encoding {}", out.display()))?;
    if let Some(wav) = &wav {
        audio::fit_true_peak(out, wav.path(), project.audio.true_peak_db)
            .context("bringing the exported audio under its true-peak ceiling")?;
    }
    partial.keep = true;
    Ok(ExportReport {
        path: out.to_path_buf(),
        duration,
        width,
        height,
        frames,
        seconds_taken: started.elapsed().as_secs_f64(),
    })
}

/// One fully composited frame at timeline time `t`. The last decoder is kept so a nearby
/// scrub does not start ffmpeg again.
pub fn render_frame(
    project: &Project,
    project_dir: &Path,
    t: f64,
    width: u32,
    height: u32,
) -> Result<tiny_skia::Pixmap> {
    let mut guard = scrub().lock().unwrap_or_else(|e| e.into_inner());
    let grade = Grade::new(&project.look);
    let ScrubState { pool, scratch, gfx } = &mut *guard;
    compose::render_at(
        project,
        project_dir,
        t,
        width,
        height,
        pool,
        scratch,
        gfx,
        &grade,
    )
}

struct ScrubState {
    pool: DecoderPool,
    scratch: Scratch,
    gfx: Graphics,
}

fn scrub() -> &'static Mutex<ScrubState> {
    static SCRUB: LazyLock<Mutex<ScrubState>> = LazyLock::new(|| {
        Mutex::new(ScrubState {
            pool: DecoderPool::new(false),
            scratch: Scratch::new(),
            gfx: Graphics::new(),
        })
    });
    &SCRUB
}

/// Composited frames from `from` onward at the canvas frame rate, for playback.
/// Decoders stay open between frames.
pub struct FrameStream {
    project: Project,
    project_dir: PathBuf,
    t: f64,
    end: f64,
    dt: f64,
    width: u32,
    height: u32,
    pool: DecoderPool,
    scratch: Scratch,
    gfx: Graphics,
    grade: Grade,
}

impl FrameStream {
    pub fn new(
        project: &Project,
        project_dir: &Path,
        from: f64,
        width: u32,
        height: u32,
    ) -> Result<Self> {
        if width < 2 || height < 2 {
            bail!("render size {width}x{height} is too small; use at least 2x2");
        }
        let fps = project.canvas.fps;
        if !(1.0..=120.0).contains(&fps) {
            bail!("canvas fps {fps} must be between 1 and 120");
        }
        Ok(FrameStream {
            project: project.clone(),
            project_dir: project_dir.to_path_buf(),
            t: from.max(0.0),
            end: project.duration().max(0.0),
            dt: 1.0 / fps,
            width,
            height,
            pool: DecoderPool::new(true),
            scratch: Scratch::new(),
            gfx: Graphics::new(),
            grade: Grade::new(&project.look),
        })
    }
}

impl Iterator for FrameStream {
    type Item = Result<(f64, tiny_skia::Pixmap)>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.t >= self.end - 1e-9 {
            return None;
        }
        let t = self.t;
        let frame = compose::render_at(
            &self.project,
            &self.project_dir,
            t,
            self.width,
            self.height,
            &mut self.pool,
            &mut self.scratch,
            &mut self.gfx,
            &self.grade,
        );
        self.t += self.dt;
        Some(frame.map(|pixmap| (t, pixmap)))
    }
}

fn timeline_range(project: &Project, range: Option<(f64, f64)>) -> Result<(f64, f64)> {
    let dur = project.duration();
    if dur <= 1e-6 {
        bail!("the project has nothing to render");
    }
    let (a, b) = range.unwrap_or((0.0, dur));
    if !a.is_finite() || !b.is_finite() || b <= a {
        bail!("render range {a:.3}..{b:.3} is empty or not a number");
    }
    if a < -1e-3 || b > dur + 1e-2 {
        bail!("render range {a:.3}..{b:.3} is outside the project (0..{dur:.3}s)");
    }
    Ok((a.max(0.0), b.min(dur)))
}

struct PartialFile {
    path: PathBuf,
    keep: bool,
}

impl Drop for PartialFile {
    fn drop(&mut self) {
        if !self.keep {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

struct Encoder {
    child: Child,
    stdin: Option<std::process::ChildStdin>,
    stderr: Option<JoinHandle<String>>,
}

impl Encoder {
    fn write_frame(&mut self, data: &[u8]) -> std::io::Result<()> {
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| std::io::Error::other("encoder stdin is closed"))?;
        stdin.write_all(data)
    }

    fn finish(&mut self) -> Result<()> {
        self.stdin.take();
        let status = self.child.wait().context("waiting for the encoder")?;
        let detail = self.stderr_tail();
        if !status.success() {
            bail!("ffmpeg encoder failed{}", detail_suffix(&detail));
        }
        Ok(())
    }

    fn fail(&mut self, out: &Path) -> Result<String> {
        self.stdin.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
        let detail = self.stderr_tail();
        let _ = std::fs::remove_file(out);
        Ok(detail_suffix(&detail))
    }

    fn stderr_tail(&mut self) -> String {
        self.stderr
            .take()
            .and_then(|h| h.join().ok())
            .unwrap_or_default()
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        self.stdin.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn spawn_encoder(
    out: &Path,
    width: u32,
    height: u32,
    fps: f64,
    wav: Option<&Path>,
    quality: Quality,
) -> Result<Encoder> {
    let enc = encoder_args(quality)?;
    let mut cmd = Command::new(ffmpeg()?);
    cmd.args(["-y", "-hide_banner", "-nostdin", "-loglevel", "error"]);
    cmd.args([
        "-f",
        "rawvideo",
        "-pix_fmt",
        "rgba",
        "-s",
        &format!("{width}x{height}"),
    ]);
    cmd.arg("-r").arg(fps_arg(fps));
    cmd.args(["-i", "pipe:0"]);
    if let Some(wav) = wav {
        cmd.arg("-i").arg(wav);
    }
    cmd.args(["-map", "0:v:0"]);
    if wav.is_some() {
        cmd.args(["-map", "1:a:0"]);
    }
    cmd.args(enc.iter().map(String::as_str));
    if wav.is_some() {
        cmd.args(["-c:a", "aac", "-b:a", "192k", "-shortest"]);
    }
    cmd.args(["-movflags", "+faststart"]);
    cmd.arg(out);
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let mut child = cmd
        .spawn()
        .with_context(|| format!("starting ffmpeg to encode {}", out.display()))?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| anyhow!("encoder stdin was not piped"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow!("encoder stderr was not piped"))?;
    let stderr = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let mut tmp = [0u8; 1024];
        let mut stderr = stderr;
        use std::io::Read;
        loop {
            match stderr.read(&mut tmp) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    buf.extend_from_slice(&tmp[..n]);
                    if buf.len() > 4096 {
                        buf.drain(0..buf.len() - 2048);
                    }
                }
            }
        }
        String::from_utf8_lossy(&buf).trim().to_string()
    });
    Ok(Encoder {
        child,
        stdin: Some(stdin),
        stderr: Some(stderr),
    })
}

fn encoder_args(quality: Quality) -> Result<Vec<String>> {
    let kind = detect_encoder()?;
    let args: &[&str] = match (kind, quality) {
        (Enc::X264, Quality::Draft) => &[
            "-c:v", "libx264", "-preset", "veryfast", "-crf", "23", "-pix_fmt", "yuv420p",
        ],
        (Enc::X264, Quality::Final) => &[
            "-c:v", "libx264", "-preset", "slow", "-crf", "18", "-pix_fmt", "yuv420p",
        ],
        (Enc::Vt, Quality::Draft) => &[
            "-c:v",
            "h264_videotoolbox",
            "-b:v",
            "4M",
            "-pix_fmt",
            "yuv420p",
        ],
        (Enc::Vt, Quality::Final) => &[
            "-c:v",
            "h264_videotoolbox",
            "-b:v",
            "12M",
            "-pix_fmt",
            "yuv420p",
        ],
        (Enc::Mpeg4, Quality::Draft) => &["-c:v", "mpeg4", "-q:v", "7", "-pix_fmt", "yuv420p"],
        (Enc::Mpeg4, Quality::Final) => &["-c:v", "mpeg4", "-q:v", "3", "-pix_fmt", "yuv420p"],
    };
    Ok(args.iter().map(|s| (*s).to_string()).collect())
}

#[derive(Clone, Copy)]
enum Enc {
    X264,
    Vt,
    Mpeg4,
}

fn detect_encoder() -> Result<Enc> {
    let mut cmd = Command::new(ffmpeg()?);
    cmd.args(["-hide_banner", "-encoders"]);
    let out = cmd
        .output()
        .context("asking ffmpeg which encoders it has")?;
    let text = String::from_utf8_lossy(&out.stdout);
    if text.contains(" libx264 ") {
        Ok(Enc::X264)
    } else if text.contains(" h264_videotoolbox ") {
        Ok(Enc::Vt)
    } else if text.contains(" mpeg4 ") {
        Ok(Enc::Mpeg4)
    } else {
        bail!("ffmpeg has no libx264, h264_videotoolbox, or mpeg4 encoder")
    }
}

fn fps_arg(fps: f64) -> String {
    if (fps - fps.round()).abs() < 1e-6 {
        format!("{}", fps.round() as i64)
    } else {
        format!("{fps:.6}")
    }
}

fn detail_suffix(detail: &str) -> String {
    if detail.is_empty() {
        String::new()
    } else {
        format!("\n{detail}")
    }
}

#[cfg(test)]
mod smoke {
    use super::*;
    use crate::media;
    use crate::project::{
        Asset, AssetKind, AssetRole, Clip, Item, ItemBody, LookPreset, Rect, Transition,
        TransitionKind, VideoKind, Zoom,
    };
    use std::path::PathBuf;

    /// Vertical 1080x1920 export: three cuts, a punch-in, a crossfade, a rounded B-roll
    /// overlay, ducked music, warm look. Checks size, duration, audio and loudness.
    /// Run with `cargo test -p veecut-engine --lib export_smoke -- --ignored --nocapture`.
    #[test]
    #[ignore = "renders a short project with ffmpeg"]
    fn export_smoke() {
        if media::ffmpeg().is_err() {
            return;
        }
        let dir = PathBuf::from("/tmp/veecut-fixtures");
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("src.mp4");
        let music = dir.join("music.wav");
        generate(&src, &music).unwrap();
        let src_info = media::probe(&src).unwrap();
        let music_info = media::probe(&music).unwrap();
        let mut project = Project::new("smoke", VideoKind::Clips, Canvas::vertical());
        project.look.preset = LookPreset::Warm;
        project.assets.push(Asset {
            id: "src".into(),
            path: src.clone(),
            kind: AssetKind::Video,
            role: AssetRole::Source,
            duration: src_info.duration,
            width: src_info.width,
            height: src_info.height,
            fps: src_info.fps,
            has_audio: src_info.has_audio,
        });
        project.assets.push(Asset {
            id: "music".into(),
            path: music,
            kind: AssetKind::Audio,
            role: AssetRole::Music,
            duration: music_info.duration,
            width: 0,
            height: 0,
            fps: 0.0,
            has_audio: true,
        });
        let clip = |id: &str, dur: f64, body: Clip| Item {
            id: id.into(),
            start: 0.0,
            duration: dur,
            body: ItemBody::Clip(body),
        };
        let mut b = Clip::new("src", 1.2);
        b.transition = Some(Transition {
            kind: TransitionKind::Crossfade,
            duration: 0.2,
        });
        b.zooms.push(Zoom {
            at: 0.15,
            duration: 0.5,
            scale: 1.3,
            focus: None,
            ease: 0.08,
        });
        project.main_mut().items = vec![
            clip("a", 1.0, Clip::new("src", 0.0)),
            clip("b", 1.0, b),
            clip("c", 1.0, Clip::new("src", 2.4)),
        ];
        let mut overlay = Clip::new("src", 0.3);
        overlay.frame = Some(Rect {
            x: 0.08,
            y: 0.62,
            w: 0.84,
            h: 0.28,
        });
        overlay.radius = 0.03;
        project
            .tracks
            .iter_mut()
            .find(|t| t.id == "broll")
            .unwrap()
            .items
            .push(Item {
                id: "overlay".into(),
                start: 0.4,
                duration: 1.2,
                body: ItemBody::Clip(overlay),
            });
        let mut bed = Clip::new("music", 0.0);
        bed.duck = true;
        bed.volume = 0.45;
        project
            .tracks
            .iter_mut()
            .find(|t| t.id == "music")
            .unwrap()
            .items
            .push(Item {
                id: "bed".into(),
                start: 0.0,
                duration: 2.8,
                body: ItemBody::Clip(bed),
            });
        project.normalize();
        let expected = project.duration();

        let draft_path = dir.join("draft.mp4");
        let final_path = dir.join("final.mp4");
        let mut keep_going = |_: &Progress| true;
        let draft = export(
            &project,
            &dir,
            &draft_path,
            &RenderOptions {
                quality: Quality::Draft,
                ..RenderOptions::for_canvas(&project.canvas, Quality::Draft)
            },
            &mut keep_going,
        )
        .unwrap();
        let final_report = export(
            &project,
            &dir,
            &final_path,
            &RenderOptions::for_canvas(&project.canvas, Quality::Final),
            &mut keep_going,
        )
        .unwrap();
        for report in [&draft, &final_report] {
            let info = media::probe(&report.path).unwrap();
            assert_eq!(info.width, 1080, "{}", report.path.display());
            assert_eq!(info.height, 1920);
            assert!(info.has_audio, "{}", report.path.display());
            assert!(
                (info.duration - expected).abs() < 0.15,
                "duration {} vs {expected}",
                info.duration
            );
            let qa = crate::qa::check(&report.path).unwrap();
            let lufs = qa.integrated_lufs.expect("lufs");
            assert!(
                (lufs - -14.0).abs() <= 1.0,
                "{lufs} LUFS, problems: {:?}",
                qa.problems
            );
            let speed = report.duration / report.seconds_taken.max(1e-6);
            println!(
                "{}: {:.2}s video in {:.2}s = {:.2}x, {}x{}, lufs {lufs:.2}",
                report.path.display(),
                report.duration,
                report.seconds_taken,
                speed,
                report.width,
                report.height
            );
        }
        let t0 = Instant::now();
        let cold = render_frame(&project, &dir, 0.5, 540, 960).unwrap();
        let cold_ms = t0.elapsed().as_secs_f64() * 1000.0;
        let t1 = Instant::now();
        let warm = render_frame(&project, &dir, 0.55, 540, 960).unwrap();
        let warm_ms = t1.elapsed().as_secs_f64() * 1000.0;
        assert_eq!((cold.width(), cold.height()), (540, 960));
        assert_eq!((warm.width(), warm.height()), (540, 960));
        println!("render_frame 540px cold {cold_ms:.0} ms, warm {warm_ms:.0} ms");
    }

    fn generate(src: &Path, music: &Path) -> Result<()> {
        let mut cmd = media::ffmpeg_command()?;
        cmd.args([
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=1920x1080:rate=30",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000",
            "-t",
            "5",
            "-c:v",
            "libx264",
            "-preset",
            "veryfast",
            "-crf",
            "23",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-shortest",
        ]);
        cmd.arg(src);
        media::run(&mut cmd).context("generating the source fixture")?;
        let mut cmd = media::ffmpeg_command()?;
        cmd.args([
            "-y",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=220:sample_rate=48000",
            "-t",
            "5",
            "-c:a",
            "pcm_s16le",
        ]);
        cmd.arg(music);
        media::run(&mut cmd).context("generating the music fixture")?;
        Ok(())
    }

    /// Loud kicks and speech, printed so a true-peak regression is visible.
    /// `cargo test -p veecut-engine --lib peak_ceiling -- --ignored --nocapture`
    #[test]
    #[ignore = "exports a kick track and speech to check true peak"]
    fn peak_ceiling() {
        if media::ffmpeg().is_err() {
            return;
        }
        let dir = PathBuf::from("/tmp/veecut-fixtures");
        std::fs::create_dir_all(&dir).unwrap();
        let kicks = dir.join("kicks.wav");
        let speech = dir.join("speech.wav");
        let picture = dir.join("blank.mp4");
        let expr = "0.9*sin(2*PI*(45+90*exp(-35*mod(t\\,0.5)))*t)*exp(-9*mod(t\\,0.5))";
        let mut cmd = media::ffmpeg_command().unwrap();
        cmd.args([
            "-y",
            "-f",
            "lavfi",
            "-i",
            &format!("aevalsrc={expr}:s=48000:d=37"),
            "-c:a",
            "pcm_s16le",
        ]);
        cmd.arg(&kicks);
        media::run(&mut cmd).unwrap();
        let aiff = dir.join("speech.aiff");
        std::process::Command::new("say")
            .args(["-o"])
            .arg(&aiff)
            .arg("This is a spoken sentence for the loudness check. The voice should stay near minus fourteen and under the true peak ceiling.")
            .status()
            .unwrap();
        let mut cmd = media::ffmpeg_command().unwrap();
        cmd.args(["-y", "-i"])
            .arg(&aiff)
            .args(["-ar", "48000", "-ac", "2", "-c:a", "pcm_s16le"]);
        cmd.arg(&speech);
        media::run(&mut cmd).unwrap();
        let mut cmd = media::ffmpeg_command().unwrap();
        cmd.args([
            "-y",
            "-f",
            "lavfi",
            "-i",
            "color=c=black:s=320x180:r=30",
            "-t",
            "37",
            "-c:v",
            "libx264",
            "-preset",
            "veryfast",
            "-pix_fmt",
            "yuv420p",
        ]);
        cmd.arg(&picture);
        media::run(&mut cmd).unwrap();
        export_bed(&dir, &picture, &kicks, &dir.join("kicks.mp4"), 37.0);
        let speech_len = media::probe(&speech).unwrap().duration.min(30.0);
        export_bed(&dir, &picture, &speech, &dir.join("speech.mp4"), speech_len);
    }

    fn export_bed(dir: &Path, picture: &Path, audio: &Path, out: &Path, seconds: f64) {
        let pic = media::probe(picture).unwrap();
        let aud = media::probe(audio).unwrap();
        let mut project = Project::new("peak", VideoKind::Vlog, Canvas::new(pic.width, pic.height));
        project.canvas.fps = 30.0;
        project.assets.push(Asset {
            id: "pic".into(),
            path: picture.to_path_buf(),
            kind: AssetKind::Video,
            role: AssetRole::Source,
            duration: pic.duration,
            width: pic.width,
            height: pic.height,
            fps: pic.fps,
            has_audio: false,
        });
        project.assets.push(Asset {
            id: "bed".into(),
            path: audio.to_path_buf(),
            kind: AssetKind::Audio,
            role: AssetRole::Music,
            duration: aud.duration,
            width: 0,
            height: 0,
            fps: 0.0,
            has_audio: true,
        });
        project.main_mut().items.push(Item {
            id: "v".into(),
            start: 0.0,
            duration: seconds,
            body: ItemBody::Clip(Clip::new("pic", 0.0)),
        });
        let mut bed = Clip::new("bed", 0.0);
        bed.volume = 1.0;
        project
            .tracks
            .iter_mut()
            .find(|t| t.id == "music")
            .unwrap()
            .items
            .push(Item {
                id: "m".into(),
                start: 0.0,
                duration: seconds,
                body: ItemBody::Clip(bed),
            });
        project.normalize();
        let wav = dir
            .join(out.file_stem().unwrap())
            .with_extension("mixed.wav");
        mix_audio(&project, dir, &wav, None, true).unwrap();
        let wav_qa = crate::qa::check(&wav).unwrap();
        let mut go = |_: &Progress| true;
        export(
            &project,
            dir,
            out,
            &RenderOptions::for_canvas(&project.canvas, Quality::Draft),
            &mut go,
        )
        .unwrap();
        let mp4 = crate::qa::check(out).unwrap();
        println!(
            "{} wav lufs {:?} tp {:?} | mp4 lufs {:?} tp {:?}",
            out.display(),
            wav_qa.integrated_lufs,
            wav_qa.true_peak_db,
            mp4.integrated_lufs,
            mp4.true_peak_db
        );
        let lufs = mp4.integrated_lufs.expect("lufs");
        let tp = mp4.true_peak_db.expect("true peak");
        assert!(tp <= -1.0 + 0.05, "{tp} dBTP");
        assert!((lufs - -14.0).abs() <= 1.0, "{lufs} LUFS");
    }
}
