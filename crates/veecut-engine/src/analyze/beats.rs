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
/// Offbeat and double-tempo peaks outside this band lose to a relative inside it.
const PREFERRED_LO: f64 = 80.0;
const PREFERRED_HI: f64 = 160.0;
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

/// Beat period in frames. Autocorrelation peaks in 60-200 BPM are scored with the
/// strength at 1×, 2× and 1.5× that period. A candidate in 80-160 BPM wins when it
/// is at least half as strong as a faster or slower relative, so offbeats do not
/// lock the grid onto a double, dotted or subharmonic tempo.
fn tempo_period(env: &[f64], fps: f64) -> Option<f64> {
    let lo = (60.0 * fps / MAX_BPM).floor() as usize;
    let hi = (60.0 * fps / MIN_BPM).ceil() as usize;
    if env.len() <= hi + 2 || lo < 1 {
        return None;
    }
    let ac = |lag: usize| -> f64 {
        if lag == 0 || lag >= env.len() {
            return 0.0;
        }
        env.iter().zip(&env[lag..]).map(|(a, b)| a * b).sum::<f64>() / (env.len() - lag) as f64
    };
    let raw: Vec<f64> = (lo - 1..=hi + 1).map(ac).collect();
    let at = |lag: usize| -> f64 {
        if (lo - 1..=hi + 1).contains(&lag) {
            raw[lag - (lo - 1)]
        } else {
            ac(lag)
        }
    };
    let prior = |bpm: f64| (-2.0 * (bpm / 120.0).log2().powi(2)).exp();
    let mut best: Option<(f64, usize)> = None;
    for lag in lo..=hi {
        let idx = lag - (lo - 1);
        if raw[idx] < raw[idx - 1] || raw[idx] < raw[idx + 1] {
            continue;
        }
        let bpm = 60.0 * fps / lag as f64;
        let family = raw[idx] + 0.5 * at(lag * 2) + 0.35 * at(lag * 3 / 2);
        let range = if (PREFERRED_LO..=PREFERRED_HI).contains(&bpm) {
            1.0
        } else {
            0.35
        };
        let score = family * prior(bpm) * range;
        if best.is_none_or(|(b, _)| score > b) {
            best = Some((score, lag));
        }
    }
    let mut lag = best?.1;
    // Prefer the 2× / 1.5× relative inside 80-160 when it is nearly as strong and closer to 120.
    let bpm = 60.0 * fps / lag as f64;
    let mut rel_best = lag;
    let mut rel_score = at(lag) * prior(bpm);
    for factor in [0.5_f64, 2.0 / 3.0, 1.5, 2.0] {
        let rel = (lag as f64 * factor).round() as usize;
        if rel < lo || rel > hi {
            continue;
        }
        let rel_bpm = 60.0 * fps / rel as f64;
        if !(PREFERRED_LO..=PREFERRED_HI).contains(&rel_bpm) || at(rel) < at(lag) * 0.5 {
            continue;
        }
        let s = at(rel) * prior(rel_bpm);
        if s > rel_score && (rel_bpm - 120.0).abs() + 1e-6 < (bpm - 120.0).abs() {
            rel_best = rel;
            rel_score = s;
        }
    }
    lag = rel_best;
    let idx = lag - (lo - 1);
    if raw.get(idx).copied().unwrap_or(0.0) <= 0.0 && at(lag) <= 0.0 {
        return None;
    }
    let (a, b, c) = (
        raw.get(idx.wrapping_sub(1)).copied().unwrap_or(0.0),
        at(lag),
        raw.get(idx + 1).copied().unwrap_or(0.0),
    );
    let curve = a - 2.0 * b + c;
    let shift = if curve < 0.0 {
        (0.5 * (a - c) / curve).clamp(-0.5, 0.5)
    } else {
        0.0
    };
    Some(lag as f64 + shift)
}

/// Dynamic-programming beat tracker: each beat scores its onset strength plus the best earlier
/// beat about one period back, penalizing tempo drift.
fn beat_grid(env: &[f64], period: f64) -> Vec<usize> {
    let mut score = vec![0.0; env.len()];
    let mut back: Vec<Option<usize>> = vec![None; env.len()];
    // ponytail: jumps under 0.75 period lock onto offbeats. Widen toward 0.5 only if a
    // track's tempo really drifts that fast inside one beat.
    let (near, far) = (
        (period * 0.75).round() as usize,
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

    #[test]
    fn offbeat_clicks_stay_at_120_and_128() {
        let sr = 22_050u32;
        for bpm in [120.0, 128.0] {
            let beat = 60.0 / bpm;
            let mut pcm = vec![0f32; sr as usize * 16];
            let mut downs = Vec::new();
            let mut seed = 1u32;
            let mut t = 0.25;
            while t < 15.5 {
                downs.push(t);
                // Early offbeat sits inside the old half-period window and used to double the grid.
                for hit in [t, t + beat * 0.45] {
                    let at = (hit * sr as f64) as usize;
                    for i in 0..(sr as usize / 200) {
                        if at + i >= pcm.len() {
                            break;
                        }
                        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                        let noise = (seed >> 9) as f32 / (1u32 << 23) as f32 * 2.0 - 1.0;
                        pcm[at + i] += 0.8 * noise * (-(i as f32) / 40.0).exp();
                    }
                }
                t += beat;
            }
            let b = beats(&pcm, sr);
            assert!(
                (b.tempo - bpm).abs() < 4.0,
                "{bpm} BPM with offbeats detected as {}",
                b.tempo
            );
            for wrong in [bpm * 1.5, bpm * 2.0, bpm / 1.5, bpm / 2.0] {
                assert!(
                    (b.tempo - wrong).abs() > 8.0,
                    "locked to {wrong:.0}, got {}",
                    b.tempo
                );
            }
            let on_beat = b
                .beats
                .iter()
                .filter(|t| downs.iter().any(|c| (c - *t).abs() < 0.04))
                .count();
            assert!(
                on_beat * 2 >= b.beats.len(),
                "grid followed offbeats at {bpm}: {:?}",
                b.beats
            );
        }
    }
}
