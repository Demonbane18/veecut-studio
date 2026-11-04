//! A synthesized sound-effects kit and the placement grammar for clicks, hits and the music bed.
//!
//! Every sound is synthesized here (see `synth.rs`) and written to `<workspace>/.veecut/sfx/v<KIT_VERSION>/`.
//! The roles, the click lag (4 to 14 ms after the picture change), the click level (2 to 7 dB
//! under the voice) and the bed rules (a fixed trim under the voice, cut rather than ducked) come
//! from NextPhase Editor V1's published measurements. None of its sounds or code are used.

mod synth;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::LazyLock;

use anyhow::{Context, Result, anyhow, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::media;
use crate::ops::{self, Op};
use crate::project::{Asset, AssetKind, AssetRole, Item, Project, TrackKind};
use crate::qa;
use crate::workspace::{Workspace, relative};

/// Bump when any sound changes: the kit then regenerates into a new folder.
pub const KIT_VERSION: u32 = 2;
/// Sample peak of every kit file, in dBFS.
pub const PEAK_DB: f32 = -3.0;
/// How long after its cue a sound's hit lands, in seconds.
pub const LAG: f64 = 0.008;
/// Voice loudness `place` assumes when the caller has not measured it.
pub const DEFAULT_VOICE_LUFS: f32 = -20.0;
/// Fade on each side of a bed move, in seconds.
const BED_FADE: f64 = 0.02;
/// The audio track sound effects go on.
const TRACK: &str = "sfx";
const EPS: f64 = 1e-6;

/// One kind of sound in the kit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sound {
    /// About 70 ms; four variants that `place` rotates.
    Click,
    /// About 25 ms.
    Tick,
    /// 16 ms, for tile panes at one per frame.
    PaneClick,
    /// 0.38 s, its hit at its end.
    Swish,
    /// 0.35 s, stuttered and bit-crushed.
    Glitch,
    /// 1.5 s, peaking at its end.
    Riser,
    /// 2 s low pad.
    SubTone,
    /// 2 s, a 55 to 40 Hz drop.
    SubBoom,
    /// 0.9 s.
    Impact,
    /// 1.6 s.
    Hit,
    /// 2.5 s.
    Outro,
    /// 27.4 s chiptune loop (16 bars at 140 BPM, seamless): a music bed, `kit:chiptune` in a look pack.
    Chiptune,
    /// 0.25 s, two square blips stepping up from 988 to 1319 Hz.
    Coin,
    /// 45 ms square pip for a text pop.
    Blip,
}

impl Sound {
    /// What the sound is for, in a few words.
    pub fn role(self) -> &'static str {
        match self {
            Sound::Click => "a picture change, a pop-in, a cut in a run",
            Sound::Tick => "a small marker or a strobe image",
            Sound::PaneClick => "one tile pane, one per frame",
            Sound::Swish => "into a title; its hit is the cut",
            Sound::Glitch => "under a datamosh",
            Sound::Riser => "builds into a cut and peaks on it",
            Sound::SubTone => "very low under a black or UI card",
            Sound::SubBoom => "very low drop under a card",
            Sound::Impact => "the beat slamming back after a silence",
            Sound::Hit => "one cut that needs weight (rare)",
            Sound::Outro => "under the end card",
            Sound::Chiptune => "a loopable arcade music bed under the whole video",
            Sound::Coin => "a reward, a counter step, a text landing in an arcade look",
            Sound::Blip => "a small text pop or cut in an arcade look",
        }
    }

    /// Where its loudest 50 ms sits against the voice's integrated loudness, in dB.
    fn under_voice(self) -> f32 {
        match self {
            Sound::Click => -4.5,
            Sound::Tick | Sound::Swish | Sound::Glitch => -5.5,
            // Nine panes land inside a third of a second, so each one is quieter.
            Sound::PaneClick => -9.0,
            Sound::Riser | Sound::Impact | Sound::Hit => -1.5,
            Sound::Outro => -8.0,
            Sound::Chiptune => -18.0,
            Sound::Coin => -5.5,
            Sound::Blip => -6.0,
            Sound::SubBoom => -15.0,
            Sound::SubTone => -18.0,
        }
    }

    fn is_click(self) -> bool {
        matches!(self, Sound::Click | Sound::Tick | Sound::PaneClick)
    }
}

/// One file of the kit.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct KitSound {
    /// File stem, such as `click-2`.
    pub name: String,
    pub sound: Sound,
    pub path: PathBuf,
    /// Seconds from the start of the file to its hit (its loudest sample).
    pub transient: f64,
    /// Seconds.
    pub duration: f64,
    /// Loudness of its loudest 50 ms, K-weighted, on the LUFS scale. `place` sets gains from it.
    pub level: f32,
}

/// A sound for one picture change.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Cue {
    /// Timeline time of the picture change the sound belongs to.
    pub at: f64,
    pub sound: Sound,
}

/// Folder of the current kit version.
pub fn kit_dir(ws: &Workspace) -> PathBuf {
    ws.root.join(".veecut/sfx").join(format!("v{KIT_VERSION}"))
}

/// The kit, writing any missing file first. The files are deterministic, so a deleted one comes
/// back byte for byte.
pub fn kit(ws: &Workspace) -> Result<Vec<KitSound>> {
    write_kit(ws, false)
}

/// Writes every kit file again, then returns the kit.
pub fn regenerate(ws: &Workspace) -> Result<Vec<KitSound>> {
    write_kit(ws, true)
}

/// The synthesized kit with each sound's loudness, made once per process: synthesis is
/// deterministic and the 27 s chiptune loop is not free.
static SYNTHESIZED: LazyLock<Vec<(synth::Made, f32)>> = LazyLock::new(|| {
    synth::all(PEAK_DB)
        .into_iter()
        .map(|m| {
            let level = synth::level(&m.samples);
            (m, level)
        })
        .collect()
});

fn write_kit(ws: &Workspace, force: bool) -> Result<Vec<KitSound>> {
    let dir = kit_dir(ws);
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    SYNTHESIZED
        .iter()
        .map(|(made, level)| {
            let path = dir.join(format!("{}.wav", made.name));
            if force || !path.is_file() {
                let tmp = dir.join(format!(".{}.wav.tmp", made.name));
                std::fs::write(&tmp, synth::wav(&made.samples)?)
                    .with_context(|| format!("writing {}", tmp.display()))?;
                std::fs::rename(&tmp, &path)
                    .with_context(|| format!("writing {}", path.display()))?;
            }
            Ok(KitSound {
                level: *level,
                duration: made.samples.len() as f64 / synth::RATE as f64,
                name: made.name.clone(),
                sound: made.sound,
                path,
                transient: made.transient,
            })
        })
        .collect()
}

/// Places cues as clips on the `sfx` audio track (created when missing), one asset per kit file
/// used. Each sound's hit lands [`LAG`] after its cue (a riser starts early; a start before 0 is
/// trimmed off its head), clicks closer than one frame merge into the first, click variants
/// rotate, and tails stop at the end of the main track. Gains put each sound's loudest 50 ms a
/// fixed distance under the voice (`voice_lufs`, default [`DEFAULT_VOICE_LUFS`]). Atomic through
/// ops. Returns the number of clips placed.
pub fn place(
    ws: &Workspace,
    project: &mut Project,
    project_dir: &Path,
    cues: &[Cue],
    voice_lufs: Option<f32>,
) -> Result<usize> {
    if let Some(c) = cues.iter().find(|c| !c.at.is_finite()) {
        bail!("cue time {} for {:?} is not a number", c.at, c.sound);
    }
    if cues.is_empty() {
        return Ok(0);
    }
    let kit = kit(ws)?;
    let voice = voice_lufs.unwrap_or(DEFAULT_VOICE_LUFS);
    let mut next = project.clone();
    match next.track(TRACK) {
        Some(t) if t.kind != TrackKind::Audio => {
            bail!("track {TRACK:?} is not an audio track, so sound effects cannot go on it")
        }
        Some(_) => {}
        None => {
            ops::apply(
                &mut next,
                &Op::Set {
                    path: "/tracks/-".into(),
                    value: json!({ "id": TRACK, "kind": "audio", "name": "Sound effects" }),
                },
            )?;
        }
    }
    let end = next.main().items.last().map(Item::end).filter(|&e| e > 0.0);
    let frame = 1.0 / next.canvas.fps;
    let clicks: Vec<&KitSound> = kit.iter().filter(|k| k.sound == Sound::Click).collect();
    let mut sorted = cues.to_vec();
    sorted.sort_by(|a, b| a.at.total_cmp(&b.at));
    let mut last_click = f64::NEG_INFINITY;
    let mut rotation = 0;
    let mut assets = HashMap::new();
    let mut adds = Vec::new();
    for cue in sorted {
        if cue.sound.is_click() {
            if cue.at - last_click < frame - EPS {
                continue;
            }
            last_click = cue.at;
        }
        let k = if cue.sound == Sound::Click {
            rotation += 1;
            clicks[(rotation - 1) % clicks.len()]
        } else {
            kit.iter()
                .find(|k| k.sound == cue.sound)
                .expect("the kit has every sound")
        };
        let start = cue.at + LAG - k.transient;
        let src_in = (-start).max(0.0);
        let start = start.max(0.0);
        let mut duration = k.duration - src_in;
        if let Some(end) = end {
            duration = duration.min(end - start);
        }
        if duration < 0.001 {
            continue;
        }
        let asset = kit_asset(ws, &mut next, project_dir, k, &mut assets)?;
        adds.push(Op::AddClip {
            asset,
            track: Some(TRACK.into()),
            start: Some(start),
            index: None,
            src_in,
            duration: Some(duration),
            frame: None,
            volume: Some(gain(k, voice)),
            duck: Some(false),
        });
    }
    ops::apply_all(&mut next, &adds)?;
    *project = next;
    Ok(adds.len())
}

/// The project's asset for a kit file, added through ops the first time it is used.
fn kit_asset(
    ws: &Workspace,
    p: &mut Project,
    project_dir: &Path,
    k: &KitSound,
    cache: &mut HashMap<String, String>,
) -> Result<String> {
    if let Some(id) = cache.get(&k.name) {
        return Ok(id.clone());
    }
    let root = std::path::absolute(&ws.root)?;
    let dir = std::path::absolute(project_dir)?;
    let file = std::path::absolute(&k.path)?;
    let real = std::fs::canonicalize(&file).ok();
    let existing = p.assets.iter().find(|a| {
        a.role == AssetRole::Sfx
            && real.is_some()
            && std::fs::canonicalize(p.asset_path(&dir, a)).ok() == real
    });
    let id = match existing {
        Some(a) => a.id.clone(),
        None => {
            let path = if file.starts_with(&root) && dir.starts_with(&root) {
                relative(&dir, &file)
            } else {
                file
            };
            let id = p.new_id("asset");
            let asset = Asset {
                id: id.clone(),
                path,
                kind: AssetKind::Audio,
                role: AssetRole::Sfx,
                duration: k.duration,
                width: 0,
                height: 0,
                fps: 0.0,
                has_audio: true,
            };
            ops::apply(
                p,
                &Op::Set {
                    path: "/assets/-".into(),
                    value: serde_json::to_value(&asset)?,
                },
            )?;
            id
        }
    };
    cache.insert(k.name.clone(), id.clone());
    Ok(id)
}

/// Linear gain that puts the sound's loudest 50 ms where it belongs under the voice.
// ponytail: capped so the sound's own peak stays under -1 dBFS; with a very loud voice the
// effects then sit a little lower than planned. Lift the cap if the mixer gets a limiter.
fn gain(k: &KitSound, voice: f32) -> f32 {
    db_gain(voice + k.sound.under_voice() - k.level).min(db_gain(-1.0 - PEAK_DB))
}

fn db_gain(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

/// Integrated loudness (LUFS) of a file's audio, or of `range` (start, end seconds) of it, from
/// ffmpeg's `ebur128`. Sounds shorter than 400 ms measure -70.
pub fn loudness(path: &Path, range: Option<(f64, f64)>) -> Result<f32> {
    if !path.is_file() {
        bail!("{} does not exist", path.display());
    }
    let mut cmd = Command::new(media::ffmpeg()?);
    cmd.args(["-hide_banner", "-nostdin"]);
    if let Some((from, to)) = range {
        ensure!(
            from.is_finite() && to.is_finite() && to > from,
            "loudness range {from}..{to} is empty"
        );
        let from = from.max(0.0);
        cmd.arg("-ss").arg(format!("{from:.6}"));
        cmd.arg("-t").arg(format!("{:.6}", to - from));
    }
    cmd.arg("-i").arg(path);
    cmd.args(["-vn", "-af", "ebur128", "-f", "null", "-"]);
    let out = cmd
        .output()
        .with_context(|| format!("measuring the loudness of {}", path.display()))?;
    let err = String::from_utf8_lossy(&out.stderr);
    if !out.status.success() {
        bail!(
            "measuring the loudness of {} failed: {}",
            path.display(),
            qa::tail(&err)
        );
    }
    qa::section_value(&err, "Integrated loudness:", "I:")
        .ok_or_else(|| anyhow!("ffmpeg found no audio to measure in {}", path.display()))
}

/// Linear gain for a music bed so it sits `under_lu` below the voice. Never above 1.0: the bed
/// is trimmed with a fixed gain, never boosted or loudness-normalized.
pub fn bed_volume(voice_lufs: f32, music_lufs: f32, under_lu: f32) -> f32 {
    db_gain(voice_lufs - under_lu - music_lufs).min(1.0)
}

/// Cuts or pulls down a music bed. For each (from, to, dB) the clips on `track` are split at
/// both edges and the pieces in between are lowered by dB (-60 or lower is silence), with
/// 20 ms fades on each side of the move. Pull-downs dip briefly to silence at their edges
/// because the fades meet. Atomic through ops: every move applies or none does.
pub fn shape_bed(project: &mut Project, track: &str, moves: &[(f64, f64, f32)]) -> Result<()> {
    let ti = project
        .track_index(track)
        .ok_or_else(|| anyhow!("no track {track:?} in this project"))?;
    ensure!(
        project.tracks[ti].kind == TrackKind::Audio,
        "track {track:?} is not an audio track"
    );
    let mut next = project.clone();
    for &(from, to, db) in moves {
        ensure!(
            from.is_finite() && to.is_finite() && to - from > EPS && !db.is_nan(),
            "bed move {from}..{to} ({db} dB) needs from < to and a level in dB"
        );
        for at in [from, to] {
            let crosses = next.tracks[ti]
                .items
                .iter()
                .any(|it| it.start < at - EPS && it.end() > at + EPS);
            if crosses {
                ops::apply(
                    &mut next,
                    &Op::Split {
                        at,
                        track: Some(track.into()),
                    },
                )?;
            }
        }
        let gain = if db <= -60.0 { 0.0 } else { db_gain(db) };
        let patches: Vec<Op> = next.tracks[ti]
            .items
            .iter()
            .filter_map(|it| {
                let c = it.clip()?;
                let fade = |old: f64| json!(old.max(BED_FADE));
                let mut patch = Map::new();
                if it.start >= from - EPS && it.end() <= to + EPS {
                    patch.insert("volume".into(), json!(c.volume * gain));
                    if gain > 0.0 && (it.start - from).abs() <= EPS {
                        patch.insert("fade_in".into(), fade(c.fade_in));
                    }
                    if gain > 0.0 && (it.end() - to).abs() <= EPS {
                        patch.insert("fade_out".into(), fade(c.fade_out));
                    }
                } else if (it.end() - from).abs() <= EPS {
                    patch.insert("fade_out".into(), fade(c.fade_out));
                } else if (it.start - to).abs() <= EPS {
                    patch.insert("fade_in".into(), fade(c.fade_in));
                }
                (!patch.is_empty()).then(|| Op::Patch {
                    item: it.id.clone(),
                    patch: Value::Object(patch),
                })
            })
            .collect();
        ops::apply_all(&mut next, &patches)?;
    }
    *project = next;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::{Canvas, Clip, ItemBody, VideoKind};

    fn workspace() -> (tempfile::TempDir, Workspace, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let ws = Workspace {
            root: tmp.path().to_path_buf(),
        };
        let dir = tmp.path().join("projects/p");
        std::fs::create_dir_all(&dir).unwrap();
        (tmp, ws, dir)
    }

    fn clips(p: &Project, track: &str) -> Vec<(f64, f64, Clip)> {
        p.track(track)
            .unwrap()
            .items
            .iter()
            .map(|it| (it.start, it.duration, it.clip().unwrap().clone()))
            .collect()
    }

    #[test]
    fn kit_files_are_the_same_bytes_every_time() {
        let (_tmp, ws, _) = workspace();
        let read = |kit: &[KitSound]| -> Vec<Vec<u8>> {
            kit.iter()
                .map(|k| std::fs::read(&k.path).unwrap())
                .collect()
        };
        let first = read(&kit(&ws).unwrap());
        let again = read(&regenerate(&ws).unwrap());
        assert_eq!(first.len(), 17);
        assert!(first == again, "regenerating changed the kit");
        let names: Vec<String> = kit(&ws).unwrap().into_iter().map(|k| k.name).collect();
        assert!(names.contains(&"click-4".to_string()) && names.contains(&"outro".to_string()));
    }

    #[test]
    fn every_sound_peaks_at_its_declared_hit() {
        for m in synth::all(PEAK_DB) {
            let (at, peak) = m
                .samples
                .iter()
                .enumerate()
                .fold((0, 0.0f32), |(bi, bv), (i, s)| {
                    if s.abs() > bv { (i, s.abs()) } else { (bi, bv) }
                });
            let measured = at as f64 / synth::RATE as f64;
            // A loop has no single loudest moment: its declared hit is the downbeat.
            if m.sound == Sound::Chiptune {
                assert_eq!(m.transient, 0.0);
                assert!((20.0 * peak.log10() - PEAK_DB).abs() < 0.01);
                assert!(m.samples[0].abs() < 1e-3 && m.samples.last().unwrap().abs() < 1e-3);
                continue;
            }
            assert!(
                (measured - m.transient).abs() <= 0.002,
                "{}: loudest sample at {measured:.4}s, declared {:.4}s",
                m.name,
                m.transient
            );
            assert!((20.0 * peak.log10() - PEAK_DB).abs() < 0.01, "{}", m.name);
            assert!(m.samples[0].abs() < 1e-3 && m.samples.last().unwrap().abs() < 1e-3);
        }
    }

    fn made(sound: Sound) -> Vec<f32> {
        synth::all(PEAK_DB)
            .into_iter()
            .find(|m| m.sound == sound)
            .unwrap()
            .samples
    }

    /// Frequency of the strongest bin of `x` (DC excluded), in Hz.
    fn peak_hz(x: &[f32]) -> f64 {
        let fft = realfft::RealFftPlanner::<f32>::new().plan_fft_forward(x.len());
        let mut input = x.to_vec();
        let mut spectrum = fft.make_output_vec();
        fft.process(&mut input, &mut spectrum).unwrap();
        let bin = (1..spectrum.len())
            .max_by(|&a, &b| spectrum[a].norm().total_cmp(&spectrum[b].norm()))
            .unwrap();
        bin as f64 * synth::RATE as f64 / x.len() as f64
    }

    #[test]
    fn chiptune_is_a_seamless_sixteen_bar_loop_at_140_bpm() {
        let x = made(Sound::Chiptune);
        assert!(x == made(Sound::Chiptune), "the loop is not deterministic");
        let bars = x.len() as f64 / synth::RATE as f64 / (4.0 * 60.0 / 140.0);
        assert!((bars - 16.0).abs() < 1e-4, "{bars} bars");
        // Wrapping end to start must not click: both ends sit at zero, and so does the step.
        let (first, last) = (x[0], *x.last().unwrap());
        assert!(first.abs() < 1e-3 && last.abs() < 1e-3 && (first - last).abs() < 1e-3);
        // Every bar has music in it, and the peak is the kit's.
        let bar = x.len() / 16;
        for (i, chunk) in x.chunks(bar).take(16).enumerate() {
            let rms = (chunk.iter().map(|s| s * s).sum::<f32>() / chunk.len() as f32).sqrt();
            assert!(rms > 0.02, "bar {i} is nearly silent ({rms})");
        }
        let peak = x.iter().fold(0.0f32, |p, s| p.max(s.abs()));
        assert!((20.0 * peak.log10() - PEAK_DB).abs() < 0.01);
    }

    #[test]
    fn a_second_kit_call_rewrites_nothing() {
        let (_tmp, ws, _) = workspace();
        let first = kit(&ws).unwrap();
        let mtimes = |kit: &[KitSound]| -> Vec<_> {
            kit.iter()
                .map(|k| std::fs::metadata(&k.path).unwrap().modified().unwrap())
                .collect()
        };
        let before = mtimes(&first);
        std::thread::sleep(std::time::Duration::from_millis(50));
        let second = kit(&ws).unwrap();
        assert_eq!(first, second);
        assert!(
            before == mtimes(&second),
            "kit() rewrote a file that existed"
        );
    }

    #[test]
    fn coin_steps_up_in_pitch_and_blip_is_a_short_pip() {
        let coin = made(Sound::Coin);
        assert!(coin == made(Sound::Coin) && made(Sound::Blip) == made(Sound::Blip));
        let rate = synth::RATE as f64;
        assert!((coin.len() as f64 / rate - 0.25).abs() < 0.01);
        let at = |s: f64| (s * rate) as usize;
        let early = peak_hz(&coin[at(0.005)..at(0.065)]);
        let late = peak_hz(&coin[at(0.10)..at(0.20)]);
        assert!((early - 988.0).abs() < 40.0, "first blip at {early} Hz");
        assert!((late - 1319.0).abs() < 40.0, "second blip at {late} Hz");
        assert!(late > early * 1.25);
        let blip = made(Sound::Blip);
        assert!(blip.len() as f64 / rate < 0.08, "{} samples", blip.len());
    }

    #[test]
    fn place_lags_rotates_merges_and_trims_head() {
        let (_tmp, ws, dir) = workspace();
        let mut p = Project::new("t", VideoKind::Documentary, Canvas::landscape());
        p.tracks.retain(|t| t.id != TRACK);
        let frame = 1.0 / p.canvas.fps;
        let cue = |at: f64, sound| Cue { at, sound };
        let cues = [
            cue(3.0, Sound::Click),
            cue(1.0, Sound::Click),
            cue(1.0 + frame * 0.5, Sound::Click), // merges into the click at 1.0
            cue(2.0, Sound::Click),
            cue(4.0, Sound::Click),
            cue(5.0, Sound::Click),
            cue(6.0, Sound::PaneClick),
            cue(6.0 + frame, Sound::PaneClick), // exactly a frame later: kept
            cue(1.0, Sound::Riser),
        ];
        let placed = place(&ws, &mut p, &dir, &cues, Some(-18.0)).unwrap();
        assert_eq!(placed, 8);
        let kit = kit(&ws).unwrap();
        let by_asset = |c: &Clip| {
            let path = p.asset_path(&dir, p.asset(&c.asset).unwrap());
            let real = std::fs::canonicalize(path).unwrap();
            kit.iter()
                .find(|k| std::fs::canonicalize(&k.path).unwrap() == real)
                .unwrap()
                .clone()
        };
        let placed = clips(&p, TRACK);
        let click_runs: Vec<_> = placed
            .iter()
            .filter(|(_, _, c)| by_asset(c).sound == Sound::Click)
            .collect();
        assert_eq!(click_runs.len(), 5);
        for (i, (start, _, c)) in click_runs.iter().enumerate() {
            let k = by_asset(c);
            assert!((start + k.transient - (1.0 + i as f64 + LAG)).abs() < 1e-9);
            if i > 0 {
                assert_ne!(
                    c.asset,
                    click_runs[i - 1].2.asset,
                    "neighbour clicks repeat"
                );
            }
        }
        let variants: std::collections::HashSet<_> =
            click_runs.iter().map(|(_, _, c)| c.asset.clone()).collect();
        assert_eq!(variants.len(), 4);
        let (start, dur, riser) = placed
            .iter()
            .find(|(_, _, c)| by_asset(c).sound == Sound::Riser)
            .unwrap();
        let k = by_asset(riser);
        assert_eq!(*start, 0.0);
        assert!((riser.src_in - (k.transient - 1.0 - LAG)).abs() < 1e-9);
        assert!((dur + riser.src_in - k.duration).abs() < 1e-9);
        assert!(placed.iter().all(|(_, _, c)| c.volume > 0.0 && !c.duck));
        // A second pass reuses the kit assets.
        let assets = p.assets.len();
        place(&ws, &mut p, &dir, &[cue(9.0, Sound::Riser)], None).unwrap();
        assert_eq!(p.assets.len(), assets);
    }

    #[test]
    fn bed_volume_sits_under_the_voice_and_never_boosts() {
        // -16 voice, 5.5 LU under = -21.5; a -12 LUFS song needs -9.5 dB.
        assert!((bed_volume(-16.0, -12.0, 5.5) - 10f32.powf(-9.5 / 20.0)).abs() < 1e-6);
        assert_eq!(bed_volume(-14.0, -30.0, 5.5), 1.0);
    }

    #[test]
    fn shape_bed_cuts_and_pulls_down_with_fades() {
        let mut p = Project::new("t", VideoKind::Documentary, Canvas::landscape());
        p.assets.push(Asset {
            id: "song".into(),
            path: "song.mp3".into(),
            kind: AssetKind::Audio,
            role: AssetRole::Music,
            duration: 30.0,
            width: 0,
            height: 0,
            fps: 0.0,
            has_audio: true,
        });
        let mut clip = Clip::new("song", 0.0);
        clip.volume = 0.5;
        let music = p.track_index("music").unwrap();
        p.tracks[music].items.push(Item {
            id: "bed".into(),
            start: 0.0,
            duration: 20.0,
            body: ItemBody::Clip(clip),
        });
        shape_bed(&mut p, "music", &[(5.0, 8.0, -70.0), (12.0, 14.0, -8.0)]).unwrap();
        let pull = 0.5 * 10f32.powf(-8.0 / 20.0);
        // (start, duration, src_in, volume, fade_in, fade_out)
        let want = [
            (0.0, 5.0, 0.0, 0.5, 0.0, 0.02),
            (5.0, 3.0, 5.0, 0.0, 0.0, 0.0),
            (8.0, 4.0, 8.0, 0.5, 0.02, 0.02),
            (12.0, 2.0, 12.0, pull, 0.02, 0.02),
            (14.0, 6.0, 14.0, 0.5, 0.02, 0.0),
        ];
        let got = clips(&p, "music");
        assert_eq!(got.len(), want.len());
        for ((start, dur, c), w) in got.iter().zip(want) {
            assert!(
                (start - w.0).abs() < 1e-9 && (dur - w.1).abs() < 1e-9,
                "{start} {dur}"
            );
            assert!((c.src_in - w.2).abs() < 1e-9);
            assert!(
                (c.volume - w.3).abs() < 1e-6,
                "{start}: volume {}",
                c.volume
            );
            assert!(
                (c.fade_in - w.4).abs() < 1e-9 && (c.fade_out - w.5).abs() < 1e-9,
                "{start}"
            );
        }
        assert!(shape_bed(&mut p, "main", &[(1.0, 2.0, -8.0)]).is_err());
    }
}
