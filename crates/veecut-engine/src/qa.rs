//! Checks a rendered file: size, length, loudness, black frames; `check_words` checks its words.

mod words;

pub use words::{WordIssue, WordsReport, check_words};

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::media::{self, ffmpeg};

/// One stretch of near-black picture.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BlackSegment {
    pub start: f64,
    pub end: f64,
}

/// What `check` found. Problems are plain sentences; a readable file is still `Ok`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct QaReport {
    pub duration: f64,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub has_audio: bool,
    pub integrated_lufs: Option<f32>,
    pub true_peak_db: Option<f32>,
    pub black_segments: Vec<BlackSegment>,
    pub problems: Vec<String>,
}

/// Probes `path`, then measures loudness (`ebur128=peak=true`) and black stretches (`blackdetect`).
pub fn check(path: &Path) -> Result<QaReport> {
    if !path.is_file() {
        bail!("{} does not exist", path.display());
    }
    let info = media::probe(path).with_context(|| format!("checking {}", path.display()))?;
    let mut report = QaReport {
        duration: info.duration,
        width: info.width,
        height: info.height,
        fps: info.fps,
        has_audio: info.has_audio,
        integrated_lufs: None,
        true_peak_db: None,
        black_segments: Vec::new(),
        problems: Vec::new(),
    };
    if !info.has_video {
        report.problems.push("the file has no picture".into());
    }
    if !info.has_audio {
        report.problems.push("the file has no audio".into());
    }
    if info.duration <= 0.0 {
        report.problems.push("the file has no duration".into());
    }
    if info.width % 2 == 1 || info.height % 2 == 1 {
        report.problems.push(format!(
            "size is {}x{}, and H.264 needs an even width and height",
            info.width, info.height
        ));
    }
    match analyze(path, info.has_audio) {
        Ok((lufs, peak, black)) => {
            report.integrated_lufs = lufs;
            report.true_peak_db = peak;
            report.black_segments = black;
        }
        Err(e) => report
            .problems
            .push(format!("could not measure loudness or black frames: {e:#}")),
    }
    if let Some(tp) = report.true_peak_db
        && tp > -1.0
    {
        report
            .problems
            .push(format!("true peak is {tp:.1} dB, above -1 dBTP"));
    }
    if let Some(lufs) = report.integrated_lufs
        && (lufs - -14.0).abs() > 1.0
    {
        report.problems.push(format!(
            "integrated loudness is {lufs:.1} LUFS, more than 1 LU from -14"
        ));
    }
    for seg in &report.black_segments {
        let d = seg.end - seg.start;
        if d >= 0.4 {
            report.problems.push(format!(
                "black from {:.2}s to {:.2}s ({:.2}s)",
                seg.start, seg.end, d
            ));
        }
    }
    Ok(report)
}

fn analyze(path: &Path, has_audio: bool) -> Result<(Option<f32>, Option<f32>, Vec<BlackSegment>)> {
    let mut cmd = Command::new(ffmpeg()?);
    cmd.args(["-hide_banner", "-nostdin", "-i"]).arg(path);
    cmd.args(["-vf", "blackdetect=d=0.1:pix_th=0.10"]);
    if has_audio {
        cmd.args(["-af", "ebur128=peak=true"]);
    }
    cmd.args(["-f", "null", "-"]);
    let out = cmd
        .output()
        .with_context(|| format!("analyzing {}", path.display()))?;
    let err = String::from_utf8_lossy(&out.stderr);
    if !out.status.success() {
        bail!("{}", tail(&err));
    }
    let lufs = section_value(&err, "Integrated loudness:", "I:");
    let peak = section_value(&err, "True peak:", "Peak:");
    Ok((lufs, peak, black_segments(&err)))
}

pub(crate) fn section_value(text: &str, section: &str, prefix: &str) -> Option<f32> {
    let rest = text.split(section).nth(1)?;
    let end = rest.find("\n\n").unwrap_or(rest.len().min(400));
    for line in rest[..end].lines() {
        let t = line.trim();
        let Some(v) = t.strip_prefix(prefix) else {
            continue;
        };
        return first_f32(v);
    }
    None
}

fn black_segments(text: &str) -> Vec<BlackSegment> {
    let mut out = Vec::new();
    for line in text.lines() {
        if !line.contains("black_start:") {
            continue;
        }
        let Some(start) = field(line, "black_start:") else {
            continue;
        };
        let Some(end) = field(line, "black_end:") else {
            continue;
        };
        out.push(BlackSegment { start, end });
    }
    out
}

fn field(line: &str, key: &str) -> Option<f64> {
    let v = line.split(key).nth(1)?;
    first_f32(v).map(|n| n as f64)
}

fn first_f32(s: &str) -> Option<f32> {
    let num: String = s
        .trim()
        .chars()
        .skip_while(|c| c.is_whitespace())
        .take_while(|c| c.is_ascii_digit() || *c == '-' || *c == '+' || *c == '.')
        .collect();
    num.parse().ok()
}

pub(crate) fn tail(s: &str) -> String {
    let lines: Vec<&str> = s.lines().rev().take(6).collect();
    lines.into_iter().rev().collect::<Vec<_>>().join("\n")
}
