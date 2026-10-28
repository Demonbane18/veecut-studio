//! Beat tracking for cutting to music: spectral-flux onsets, tempo by autocorrelation, a beat
//! grid by dynamic programming (Ellis 2007) and downbeats every four beats.

use realfft::RealFftPlanner;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Beats of a music track, in seconds.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Beats {
    /// Beats per minute, 60-200; 0 when no steady beat was found.
    pub tempo: f64,
    pub beats: Vec<f64>,
    /// Every fourth beat, on the strongest phase: where bars start.
    pub downbeats: Vec<f64>,
    /// Every note or hit, beat or not.
    pub onsets: Vec<f64>,
}

const MIN_BPM: f64 = 60.0;
const MAX_BPM: f64 = 200.0;
/// How strongly the beat grid sticks to the tempo (librosa's default tightness).
const TIGHTNESS: f64 = 100.0;

/// Finds the tempo, beats, downbeats and onsets of mono audio. Needs at least 2 seconds.
pub fn beats(pcm: &[f32], sample_rate: u32) -> Beats {
    let sr = sample_rate as f64;
    if sample_rate == 0 || (pcm.len() as f64) < sr * 2.0 {
        return Beats::default();
    }
    let win = ((sr * 0.03) as usize).next_power_of_two();
    let hop = ((sr * 0.005).round() as usize).max(1);
    let fps = sr / hop as f64;
    let env = onset_envelope(pcm, win, hop);
    // A hit enters the window between two frames; the flux jumps on the first frame that holds it.
    let time = |f: usize| (f * hop + win) as f64 / sr - hop as f64 / (2.0 * sr);
    let onsets = peaks(&env, fps).into_iter().map(time).collect();

    let Some(period) = tempo_period(&env, fps) else {
        return Beats {
            onsets,
            ..Beats::default()
        };
    };
    let grid = beat_grid(&env, period);
    if grid.len() < 2 {
        return Beats {
            onsets,
            ..Beats::default()
        };
    }
    let beats: Vec<f64> = grid.iter().map(|&f| time(f)).collect();
    // Steady-tempo estimate: slope of beat time against beat number.
    let n = beats.len() as f64;
    let mean_k = (n - 1.0) / 2.0;
    let mean_t = beats.iter().sum::<f64>() / n;
    let (mut num, mut den) = (0.0, 0.0);
    for (k, t) in beats.iter().enumerate() {
        num += (k as f64 - mean_k) * (t - mean_t);
        den += (k as f64 - mean_k).powi(2);
    }
    let tempo = 60.0 * den / num;
    let phase = (0..4)
        .max_by(|&a, &b| {
            let strength = |p: usize| grid.iter().skip(p).step_by(4).map(|&f| env[f]).sum::<f64>();
            strength(a).total_cmp(&strength(b))
        })
        .unwrap_or(0);
    let downbeats = beats.iter().copied().skip(phase).step_by(4).collect();
    Beats {
        tempo,
        beats,
        downbeats,
        onsets,
    }
}

/// Spectral flux on a log-magnitude spectrum, minus its local mean, never negative.
fn onset_envelope(pcm: &[f32], win: usize, hop: usize) -> Vec<f64> {
    let fft = RealFftPlanner::<f32>::new().plan_fft_forward(win);
    let hann: Vec<f32> = (0..win)
        .map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / win as f32).cos())
        .collect();
    let mut input = fft.make_input_vec();
    let mut spectrum = fft.make_output_vec();
    let mut prev = vec![0f32; spectrum.len()];
    let frames = (pcm.len().saturating_sub(win)) / hop + 1;
    let mut flux = Vec::with_capacity(frames);
    for f in 0..frames {
        let s = f * hop;
        for (i, x) in input.iter_mut().enumerate() {
            *x = pcm[s + i] * hann[i];
        }
        if fft.process(&mut input, &mut spectrum).is_err() {
            flux.push(0.0);
            continue;
        }
        let mut sum = 0.0f64;
        for (bin, c) in spectrum.iter().enumerate() {
            let m = (1.0 + 100.0 * c.norm()).ln();
            sum += (m - prev[bin]).max(0.0) as f64;
            prev[bin] = m;
        }
        flux.push(sum);
    }
    if let Some(first) = flux.first_mut() {
        *first = 0.0; // the first frame has nothing to compare against
    }
    // Subtract a 0.2 s moving average (20 frames each side at the 5 ms hop) so steady sounds do
    // not count as onsets.
    let half = 20;
    let mut prefix = vec![0.0; flux.len() + 1];
    for (i, v) in flux.iter().enumerate() {
        prefix[i + 1] = prefix[i] + v;
    }
    (0..flux.len())
        .map(|i| {
            let (a, b) = (i.saturating_sub(half), (i + half + 1).min(flux.len()));
            (flux[i] - (prefix[b] - prefix[a]) / (b - a) as f64).max(0.0)
        })
        .collect()
}

/// Frames that are the highest within 50 ms and stand clearly above the envelope's average.
fn peaks(env: &[f64], fps: f64) -> Vec<usize> {
    let n = env.len().max(1) as f64;
    let mean = env.iter().sum::<f64>() / n;
    let sd = (env.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / n).sqrt();
    let threshold = mean + 0.5 * sd;
    let reach = ((0.05 * fps) as usize).max(1);
    let mut out: Vec<usize> = Vec::new();
    for i in 0..env.len() {
        let near = &env[i.saturating_sub(reach)..(i + reach + 1).min(env.len())];
        if env[i] > threshold
            && near.iter().all(|&v| v <= env[i])
            && out.last().is_none_or(|&p| i - p > reach)
        {
            out.push(i);
        }
    }
    out
}

/// Beat period in frames: the autocorrelation peak in 60-200 BPM, weighted toward 120 BPM so
/// half and double tempo lose, refined between frames.
fn tempo_period(env: &[f64], fps: f64) -> Option<f64> {
    let lo = (60.0 * fps / MAX_BPM).floor() as usize;
    let hi = (60.0 * fps / MIN_BPM).ceil() as usize;
    if env.len() <= hi + 2 {
        return None;
    }
    let ac = |lag: usize| {
        env.iter().zip(&env[lag..]).map(|(a, b)| a * b).sum::<f64>() / (env.len() - lag) as f64
    };
    let raw: Vec<f64> = (lo - 1..=hi + 1).map(ac).collect();
    let weight = |lag: usize| {
        let bpm = 60.0 * fps / lag as f64;
        (-0.5 * (bpm / 120.0).log2().powi(2)).exp()
    };
    let best = (1..raw.len() - 1)
        .max_by(|&a, &b| (raw[a] * weight(a + lo - 1)).total_cmp(&(raw[b] * weight(b + lo - 1))))?;
    if raw[best] <= 0.0 {
        return None;
    }
    let (a, b, c) = (raw[best - 1], raw[best], raw[best + 1]);
    let curve = a - 2.0 * b + c;
    let shift = if curve < 0.0 {
        (0.5 * (a - c) / curve).clamp(-0.5, 0.5)
    } else {
        0.0
    };
    Some((best + lo - 1) as f64 + shift)
}

/// Dynamic-programming beat tracker: each beat scores its onset strength plus the best earlier
/// beat about one period back, penalizing tempo drift.
fn beat_grid(env: &[f64], period: f64) -> Vec<usize> {
    let mut score = vec![0.0; env.len()];
    let mut back: Vec<Option<usize>> = vec![None; env.len()];
    let (near, far) = (
        (period / 2.0).round() as usize,
        (period * 2.0).round() as usize,
    );
    for i in 0..env.len() {
        let mut best: Option<(f64, usize)> = None;
        for j in i.saturating_sub(far)..i.saturating_sub(near.max(1)) {
            let penalty = TIGHTNESS * ((i - j) as f64 / period).ln().powi(2);
            let s = score[j] - penalty;
            if best.is_none_or(|(b, _)| s > b) {
                best = Some((s, j));
            }
        }
        score[i] = env[i] + best.map_or(0.0, |(s, _)| s.max(0.0));
        back[i] = best.filter(|(s, _)| *s > 0.0).map(|(_, j)| j);
    }
    // End on the best-scoring frame in the last period, then walk back.
    let tail = env.len().saturating_sub(period.round() as usize);
    let Some(mut i) = (tail..env.len()).max_by(|&a, &b| score[a].total_cmp(&score[b])) else {
        return Vec::new();
    };
    let mut grid = vec![i];
    while let Some(j) = back[i] {
        grid.push(j);
        i = j;
    }
    grid.reverse();
    // Drop beats at the edges that sit in near silence.
    let rms = (env.iter().map(|v| v * v).sum::<f64>() / env.len().max(1) as f64).sqrt();
    let strong = |f: &usize| env[*f] >= 0.5 * rms;
    let first = grid.iter().position(strong).unwrap_or(grid.len());
    let last = grid.iter().rposition(strong).map_or(0, |p| p + 1);
    if first < last {
        grid[first..last].to_vec()
    } else {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn click_track_at_120_bpm() {
        let sr = 22_050u32;
        let mut pcm = vec![0f32; sr as usize * 16];
        let clicks: Vec<f64> = (0..31).map(|k| 0.25 + k as f64 * 0.5).collect();
        let mut seed = 1u32;
        for &t in &clicks {
            let at = (t * sr as f64) as usize;
            for i in 0..(sr as usize / 200) {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let noise = (seed >> 9) as f32 / (1u32 << 23) as f32 * 2.0 - 1.0;
                pcm[at + i] = 0.8 * noise * (-(i as f32) / 40.0).exp();
            }
        }
        let b = beats(&pcm, sr);
        assert!((b.tempo - 120.0).abs() < 1.0, "tempo {}", b.tempo);
        assert!(b.beats.len() >= 28, "{} beats", b.beats.len());
        for t in &b.beats {
            let off = clicks
                .iter()
                .map(|c| (c - t).abs())
                .fold(f64::INFINITY, f64::min);
            assert!(off < 0.02, "beat {t:.3} is {off:.3} s from a click");
        }
        assert!(
            b.downbeats.len() >= b.beats.len() / 4
                && b.downbeats.iter().all(|d| b.beats.contains(d))
        );
        assert!(b.onsets.len() >= 28);
    }
}
