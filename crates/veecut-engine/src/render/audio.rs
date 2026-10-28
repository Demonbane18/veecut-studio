//! One ffmpeg `filter_complex`: trim, speed, fades, ducking, optional loudness.

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};

use crate::media::{self, ffmpeg};
use crate::project::{AssetKind, AssetRole, Clip, Item, Project, TrackKind};

use super::math::atempo_chain;

pub fn mix_audio(
    project: &Project,
    project_dir: &Path,
    out_wav: &Path,
    range: Option<(f64, f64)>,
    normalize: bool,
) -> Result<()> {
    let (start, end) = range_of(project, range)?;
    let dur = end - start;
    if let Some(parent) = out_wav.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
    }
    let clips = collect(project, project_dir, start, end)?;
    if clips.is_empty() {
        return write_silence(out_wav, dur);
    }
    if normalize {
        let tmp = tempfile::NamedTempFile::new().context("creating a temp wav for loudness")?;
        run_mix(&clips, tmp.path(), dur, project)?;
        let measured = measure(tmp.path(), project.audio.target_lufs, project.audio.true_peak_db)?;
        apply_loudnorm(tmp.path(), out_wav, &measured, project.audio.target_lufs, project.audio.true_peak_db)?;
    } else {
        run_mix(&clips, out_wav, dur, project)?;
    }
    Ok(())
}

struct Aud {
    path: std::path::PathBuf,
    src_in: f64,
    src_len: f64,
    speed: f64,
    volume: f32,
    delay: f64,
    fade_in: f64,
    fade_out_st: f64,
    fade_out: f64,
    duck: bool,
    voice: bool,
}

fn range_of(project: &Project, range: Option<(f64, f64)>) -> Result<(f64, f64)> {
    let dur = project.duration();
    let (a, b) = range.unwrap_or((0.0, dur));
    if !a.is_finite() || !b.is_finite() || b <= a {
        bail!("audio range {a:.3}..{b:.3} is empty or not a number");
    }
    Ok((a.max(0.0), b))
}

fn collect(project: &Project, dir: &Path, start: f64, end: f64) -> Result<Vec<Aud>> {
    let mut out = Vec::new();
    for track in &project.tracks {
        if track.hidden || track.muted {
            continue;
        }
        if !matches!(track.kind, TrackKind::Main | TrackKind::Audio) {
            continue;
        }
        let main = track.kind == TrackKind::Main;
        for (i, item) in track.items.iter().enumerate() {
            let Some(clip) = item.clip() else { continue };
            if clip.muted || clip.volume <= 0.0 {
                continue;
            }
            let Some(asset) = project.asset(&clip.asset) else {
                bail!("clip {} uses asset {:?}, which the project does not have", item.id, clip.asset);
            };
            if asset.kind == AssetKind::Image || (asset.kind == AssetKind::Video && !asset.has_audio) {
                continue;
            }
            let hear_start = item.start.max(start);
            let hear_end = item.end().min(end);
            if hear_end - hear_start < 1e-4 {
                continue;
            }
            let local = hear_start - item.start;
            let hear_dur = hear_end - hear_start;
            // Narration and podcast mics live on audio tracks with role Voice. They are speech:
            // side-chain key and voice cleanup, never ducked.
            let voice = main || asset.role == AssetRole::Voice;
            let (overlap_in, overlap_out) = if main { overlaps(&track.items, i) } else { (0.0, 0.0) };
            let (fade_in, fade_out_st, fade_out) = window_fades(item, clip, local, hear_dur, overlap_in, overlap_out);
            let path = project.asset_path(dir, asset);
            if !path.is_file() {
                bail!("{} does not exist (asset {})", path.display(), asset.id);
            }
            out.push(Aud {
                path,
                src_in: clip.src_in + local * clip.speed,
                src_len: (hear_dur * clip.speed).max(0.01),
                speed: clip.speed.max(0.01),
                volume: clip.volume,
                delay: hear_start - start,
                fade_in,
                fade_out_st,
                fade_out,
                duck: clip.duck && !voice,
                voice,
            });
        }
    }
    Ok(out)
}

fn overlaps(items: &[Item], i: usize) -> (f64, f64) {
    let item = &items[i];
    let inn = if i > 0 { (items[i - 1].end() - item.start).max(0.0) } else { 0.0 };
    let out = items.get(i + 1).map(|n| (item.end() - n.start).max(0.0)).unwrap_or(0.0);
    (inn, out)
}

fn window_fades(item: &Item, clip: &Clip, local: f64, hear_dur: f64, overlap_in: f64, overlap_out: f64) -> (f64, f64, f64) {
    let fade_in_len = clip.fade_in.max(overlap_in);
    let fade_out_len = clip.fade_out.max(overlap_out);
    let in_d = (fade_in_len - local).clamp(0.0, hear_dur);
    if fade_out_len <= 1e-6 {
        return (in_d, 0.0, 0.0);
    }
    let out_start = item.duration - fade_out_len - local;
    if out_start >= hear_dur {
        (in_d, 0.0, 0.0)
    } else if out_start <= 0.0 {
        (in_d, 0.0, hear_dur.min(fade_out_len))
    } else {
        (in_d, out_start, (hear_dur - out_start).min(fade_out_len))
    }
}

fn run_mix(clips: &[Aud], out: &Path, dur: f64, project: &Project) -> Result<()> {
    let graph = graph(clips, dur, project.audio.voice_cleanup, project.audio.duck_db);
    let mut cmd = media::ffmpeg_command()?;
    cmd.arg("-y");
    for c in clips {
        cmd.arg("-ss").arg(format!("{:.6}", c.src_in.max(0.0)));
        cmd.arg("-t").arg(format!("{:.6}", c.src_len));
        cmd.arg("-i").arg(&c.path);
    }
    cmd.args(["-filter_complex", &graph, "-map", "[out]"]);
    cmd.args(["-f", "wav", "-ar", "48000", "-ac", "2", "-c:a", "pcm_s16le"]);
    cmd.arg(out);
    media::run(&mut cmd).with_context(|| format!("mixing audio to {}", out.display()))?;
    Ok(())
}

fn graph(clips: &[Aud], dur: f64, cleanup: bool, duck_db: f32) -> String {
    let mut parts = Vec::new();
    let mut voice = Vec::new();
    let mut ducked = Vec::new();
    let mut dry = Vec::new();
    for (i, c) in clips.iter().enumerate() {
        let label = format!("[c{i}]");
        parts.push(clip_chain(i, c, &label));
        if c.voice {
            voice.push(label);
        } else if c.duck {
            ducked.push(label);
        } else {
            dry.push(label);
        }
    }
    let voice_l = mix_into(&mut parts, &voice, "voice");
    let voice_l = match voice_l {
        Some(v) if cleanup => {
            parts.push(format!("{v}highpass=f=80,afftdn=nr=8:nf=-45[vclean]"));
            Some("[vclean]".into())
        }
        other => other,
    };
    let duck_l = mix_into(&mut parts, &ducked, "duckbed");
    let dry_l = mix_into(&mut parts, &dry, "dry");
    let mut tails = Vec::new();
    if let (Some(d), Some(v)) = (&duck_l, &voice_l) {
        if duck_db > 0.1 {
            parts.push(format!("{v}asplit=2[vout][sc]"));
            let ratio = (duck_db as f64 / 2.0).clamp(1.5, 20.0);
            parts.push(format!(
                "{d}[sc]sidechaincompress=threshold=0.02:ratio={ratio:.2}:attack=15:release=280:makeup=1[dout]"
            ));
            tails.push("[vout]".into());
            tails.push("[dout]".into());
        } else {
            tails.push(v.clone());
            tails.push(d.clone());
        }
    } else {
        if let Some(v) = &voice_l {
            tails.push(v.clone());
        }
        if let Some(d) = &duck_l {
            tails.push(d.clone());
        }
    }
    if let Some(y) = dry_l {
        tails.push(y);
    }
    let mixed = mix_into(&mut parts, &tails, "mix").unwrap_or_else(|| "[c0]".into());
    parts.push(format!("{mixed}apad,atrim=start=0:end={dur:.6},asetpts=PTS-STARTPTS[out]"));
    parts.join(";")
}

fn mix_into(parts: &mut Vec<String>, labels: &[String], name: &str) -> Option<String> {
    match labels.len() {
        0 => None,
        1 => Some(labels[0].clone()),
        n => {
            let out = format!("[{name}]");
            parts.push(format!(
                "{}amix=inputs={n}:normalize=0:duration=longest:dropout_transition=0{out}",
                labels.join("")
            ));
            Some(out)
        }
    }
}

fn clip_chain(i: usize, c: &Aud, label: &str) -> String {
    let mut chain = vec![format!("[{i}:a]aformat=sample_rates=48000:channel_layouts=stereo")];
    for t in atempo_chain(c.speed) {
        chain.push(format!("atempo={t:.6}"));
    }
    chain.push(format!("volume={:.6}", c.volume.max(0.0)));
    if c.fade_in > 1e-4 {
        chain.push(format!("afade=t=in:st=0:d={:.6}", c.fade_in));
    }
    if c.fade_out > 1e-4 {
        chain.push(format!("afade=t=out:st={:.6}:d={:.6}", c.fade_out_st.max(0.0), c.fade_out));
    }
    let ms = (c.delay * 1000.0).round().max(0.0) as i64;
    if ms > 0 {
        chain.push(format!("adelay=delays={ms}:all=1"));
    }
    chain.push("asetpts=PTS-STARTPTS".into());
    format!("{}{label}", chain.join(","))
}

struct Measured {
    i: f64,
    tp: f64,
    lra: f64,
    thresh: f64,
    offset: f64,
}

fn measure(path: &Path, target: f32, tp: f32) -> Result<Measured> {
    let mut cmd = Command::new(ffmpeg()?);
    cmd.args(["-hide_banner", "-nostdin", "-i"]).arg(path);
    cmd.arg("-af").arg(format!("loudnorm=I={target}:TP={tp}:LRA=11:print_format=json"));
    cmd.args(["-f", "null", "-"]);
    let out = cmd.output().with_context(|| format!("measuring loudness of {}", path.display()))?;
    let err = String::from_utf8_lossy(&out.stderr);
    if !out.status.success() {
        bail!("measuring loudness of {} failed: {}", path.display(), tail(&err));
    }
    let start = err.find('{').ok_or_else(|| anyhow!("loudnorm did not print measurements for {}", path.display()))?;
    let end = err.rfind('}').ok_or_else(|| anyhow!("loudnorm measurements were cut off"))?;
    let json: serde_json::Value = serde_json::from_str(&err[start..=end])
        .context("loudnorm measurements were not valid JSON")?;
    let num = |k: &str| -> Result<f64> {
        json.get(k)
            .and_then(|v| v.as_str().and_then(|s| s.parse().ok()).or_else(|| v.as_f64()))
            .ok_or_else(|| anyhow!("loudnorm did not report {k}"))
    };
    Ok(Measured {
        i: num("input_i")?,
        tp: num("input_tp")?,
        lra: num("input_lra")?,
        thresh: num("input_thresh")?,
        offset: num("target_offset").unwrap_or(0.0),
    })
}

fn apply_loudnorm(src: &Path, dst: &Path, m: &Measured, target: f32, tp: f32) -> Result<()> {
    let filter = format!(
        "loudnorm=I={target}:TP={tp}:LRA=11:measured_I={}:measured_TP={}:measured_LRA={}:measured_thresh={}:offset={}:linear=true",
        m.i, m.tp, m.lra, m.thresh, m.offset
    );
    let mut cmd = media::ffmpeg_command()?;
    cmd.args(["-y", "-i"]).arg(src);
    cmd.args(["-af", &filter, "-f", "wav", "-ar", "48000", "-ac", "2", "-c:a", "pcm_s16le"]);
    cmd.arg(dst);
    media::run(&mut cmd).with_context(|| format!("normalizing loudness to {}", dst.display()))?;
    Ok(())
}

fn write_silence(path: &Path, seconds: f64) -> Result<()> {
    let samples_f = seconds.max(0.0) * 48_000.0;
    if samples_f > u32::MAX as f64 / 4.0 {
        bail!("silence longer than about 6 hours does not fit in a standard WAV");
    }
    let samples = samples_f.round() as u32;
    let data_len = samples.saturating_mul(4);
    let mut hdr = Vec::with_capacity(44);
    hdr.extend_from_slice(b"RIFF");
    hdr.extend_from_slice(&(36 + data_len).to_le_bytes());
    hdr.extend_from_slice(b"WAVEfmt ");
    hdr.extend_from_slice(&16u32.to_le_bytes());
    hdr.extend_from_slice(&1u16.to_le_bytes());
    hdr.extend_from_slice(&2u16.to_le_bytes());
    hdr.extend_from_slice(&48_000u32.to_le_bytes());
    hdr.extend_from_slice(&(48_000u32 * 4).to_le_bytes());
    hdr.extend_from_slice(&4u16.to_le_bytes());
    hdr.extend_from_slice(&16u16.to_le_bytes());
    hdr.extend_from_slice(b"data");
    hdr.extend_from_slice(&data_len.to_le_bytes());
    let mut f = std::fs::File::create(path).with_context(|| format!("creating {}", path.display()))?;
    use std::io::Write;
    f.write_all(&hdr)?;
    let zeros = [0u8; 8192];
    let mut left = data_len as usize;
    while left > 0 {
        let n = left.min(zeros.len());
        f.write_all(&zeros[..n])?;
        left -= n;
    }
    Ok(())
}

fn tail(s: &str) -> String {
    s.lines().rev().take(8).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n")
}
