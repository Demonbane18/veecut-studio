//! The kit's sound designs, written for VeeCut. Each sound is built from seeded noise, sines,
//! envelopes and biquad filters, so the same code always produces the same samples. Every sound
//! has a marked hit: its loudest sample sits at the declared `transient`, so placement can line
//! the hit up with a cut to the sample.

use std::f64::consts::PI;
use std::io::Cursor;

use anyhow::{Context, Result};

use super::Sound;

/// Sample rate of every kit file.
pub const RATE: u32 = 48_000;
const SR: f64 = RATE as f64;

/// One synthesized sound, peak-normalized.
pub struct Made {
    pub name: String,
    pub sound: Sound,
    /// Seconds from the start of the sound to its hit.
    pub transient: f64,
    pub samples: Vec<f32>,
}

/// Every kit sound in listing order, each with its sample peak at `peak_db` dBFS.
pub fn all(peak_db: f32) -> Vec<Made> {
    let mut out: Vec<Made> = (0..4).map(click).collect();
    out.extend([
        tick(),
        pane_click(),
        swish(),
        glitch(),
        riser(),
        sub_tone(),
        sub_boom(),
        impact(),
        hit(),
        outro(),
    ]);
    let target = 10f64.powf(peak_db as f64 / 20.0);
    for m in &mut out {
        let peak = m.samples.iter().fold(0.0f32, |p, s| p.max(s.abs())) as f64;
        let scale = target / peak;
        m.samples
            .iter_mut()
            .for_each(|s| *s = (*s as f64 * scale) as f32);
    }
    out
}

/// 16-bit mono WAV bytes at [`RATE`].
pub fn wav(samples: &[f32]) -> Result<Vec<u8>> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut bytes = Cursor::new(Vec::new());
    let mut w = hound::WavWriter::new(&mut bytes, spec).context("starting a kit WAV")?;
    for &s in samples {
        w.write_sample((s.clamp(-1.0, 1.0) * 32767.0).round() as i16)?;
    }
    w.finalize().context("finishing a kit WAV")?;
    Ok(bytes.into_inner())
}

/// Loudness of the loudest 50 ms, K-weighted as in ITU-R BS.1770 (its 48 kHz filter
/// coefficients), on the LUFS scale. Short sounds count whole: the window runs past the end.
pub fn level(samples: &[f32]) -> f32 {
    let mut shelf = Biquad {
        b0: 1.535_124_859_586_97,
        b1: -2.691_696_189_406_38,
        b2: 1.198_392_810_852_85,
        a1: -1.690_659_293_182_41,
        a2: 0.732_480_774_215_85,
        z1: 0.0,
        z2: 0.0,
    };
    let mut rlb = Biquad {
        b0: 1.0,
        b1: -2.0,
        b2: 1.0,
        a1: -1.990_047_454_833_98,
        a2: 0.990_072_250_366_21,
        z1: 0.0,
        z2: 0.0,
    };
    let win = (0.05 * SR) as usize;
    let mut sums = Vec::with_capacity(samples.len() + win + 1);
    sums.push(0.0f64);
    let mut acc = 0.0;
    for &s in samples.iter().chain(std::iter::repeat_n(&0.0, win)) {
        let y = rlb.run(shelf.run(s as f64));
        acc += y * y;
        sums.push(acc);
    }
    let loudest = (0..=samples.len())
        .map(|i| (sums[i + win] - sums[i]) / win as f64)
        .fold(0.0, f64::max);
    if loudest <= 0.0 {
        return -70.0;
    }
    ((-0.691 + 10.0 * loudest.log10()) as f32).max(-70.0)
}

/// splitmix64, so noise is the same on every run and every machine.
struct Rng(u64);

impl Rng {
    /// Uniform in [-1, 1).
    fn next(&mut self) -> f64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        (z >> 11) as f64 / (1u64 << 52) as f64 - 1.0
    }
}

#[derive(Clone, Copy)]
enum Pass {
    Low,
    High,
    Band,
}

/// RBJ cookbook biquad in transposed direct form II.
#[derive(Clone, Copy)]
struct Biquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
    z1: f64,
    z2: f64,
}

impl Biquad {
    fn new(pass: Pass, freq: f64, q: f64) -> Self {
        let mut f = Biquad {
            b0: 0.0,
            b1: 0.0,
            b2: 0.0,
            a1: 0.0,
            a2: 0.0,
            z1: 0.0,
            z2: 0.0,
        };
        f.tune(pass, freq, q);
        f
    }

    /// New coefficients, same state, so a sweep stays smooth.
    fn tune(&mut self, pass: Pass, freq: f64, q: f64) {
        let w = 2.0 * PI * freq.min(SR * 0.45) / SR;
        let (sin, cos) = w.sin_cos();
        let alpha = sin / (2.0 * q);
        let a0 = 1.0 + alpha;
        let (b0, b1, b2) = match pass {
            Pass::Low => ((1.0 - cos) / 2.0, 1.0 - cos, (1.0 - cos) / 2.0),
            Pass::High => ((1.0 + cos) / 2.0, -(1.0 + cos), (1.0 + cos) / 2.0),
            Pass::Band => (alpha, 0.0, -alpha),
        };
        self.b0 = b0 / a0;
        self.b1 = b1 / a0;
        self.b2 = b2 / a0;
        self.a1 = -2.0 * cos / a0;
        self.a2 = (1.0 - alpha) / a0;
    }

    fn run(&mut self, x: f64) -> f64 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }
}

/// Seeded white noise through one filter.
struct Noise {
    rng: Rng,
    filter: Biquad,
}

impl Noise {
    fn new(seed: u64, pass: Pass, freq: f64, q: f64) -> Self {
        Noise {
            rng: Rng(seed),
            filter: Biquad::new(pass, freq, q),
        }
    }

    fn next(&mut self) -> f64 {
        let x = self.rng.next();
        self.filter.run(x)
    }
}

/// `secs` of `f(t)`, scaled so its loudest sample is 1. Layers are mixed by peak.
fn layer(secs: f64, mut f: impl FnMut(f64) -> f64) -> Vec<f64> {
    let n = (secs * SR).round() as usize;
    let mut v: Vec<f64> = (0..n).map(|i| f(i as f64 / SR)).collect();
    let peak = v.iter().fold(0.0f64, |m, x| m.max(x.abs()));
    if peak > 0.0 {
        v.iter_mut().for_each(|x| *x /= peak);
    }
    v
}

/// Layers summed with their gains.
fn mix(parts: &[(f64, Vec<f64>)]) -> Vec<f64> {
    let n = parts.iter().map(|(_, v)| v.len()).max().unwrap_or(0);
    let mut out = vec![0.0; n];
    for (gain, v) in parts {
        for (o, x) in out.iter_mut().zip(v) {
            *o += gain * x;
        }
    }
    out
}

/// Rises over `attack` to 1 at `at`, then decays with time constant `tau`.
fn strike(t: f64, at: f64, attack: f64, tau: f64) -> f64 {
    if t >= at {
        (-(t - at) / tau).exp()
    } else if t > at - attack {
        let u = (t - (at - attack)) / attack;
        u * u
    } else {
        0.0
    }
}

/// A raised-cosine bump `width` seconds wide centred on `at`: the marked hit.
fn bump(t: f64, at: f64, width: f64) -> f64 {
    let u = (t - at) / (width / 2.0);
    if u.abs() < 1.0 {
        0.5 * (1.0 + (PI * u).cos())
    } else {
        0.0
    }
}

/// A falling-pitch sine with a cosine crest exactly at `at` (so its loudest swing is the hit):
/// `pitch(d)` gives the frequency `d` seconds after the hit; before it the pitch holds.
fn falling(at: f64, mut pitch: impl FnMut(f64) -> f64) -> impl FnMut(f64) -> f64 {
    let mut phase = 0.0;
    move |t| {
        if t < at {
            (2.0 * PI * pitch(0.0) * (t - at)).cos()
        } else {
            let p = phase;
            phase += 2.0 * PI * pitch(t - at) / SR;
            p.cos()
        }
    }
}

/// Fades the last `fade` seconds to silence.
fn made(name: &str, sound: Sound, transient: f64, mut x: Vec<f64>, fade: f64) -> Made {
    let n = x.len();
    let m = ((fade * SR) as usize).min(n);
    for i in 0..m {
        x[n - m + i] *= 0.5 * (1.0 + (PI * (i + 1) as f64 / m as f64).cos());
    }
    Made {
        name: name.into(),
        sound,
        transient,
        samples: x.into_iter().map(|v| v as f32).collect(),
    }
}

/// About 70 ms: a snap of bright noise, a short woody resonance and a little low thump. The four
/// variants move the bands so a run of clicks never sounds like a metronome.
fn click(variant: usize) -> Made {
    const T: f64 = 0.004;
    const LEN: f64 = 0.07;
    let (band, wood, low) = [
        (2600.0, 1400.0, 170.0),
        (3300.0, 1750.0, 150.0),
        (2200.0, 1150.0, 190.0),
        (3900.0, 2050.0, 140.0),
    ][variant];
    let seed = 0xC11C_0000 + variant as u64 * 16;
    let mut snap = Noise::new(seed, Pass::Band, band, 1.1);
    let mut body = Noise::new(seed + 1, Pass::Band, wood, 6.0);
    let mut room = Noise::new(seed + 2, Pass::Low, 3000.0, 0.7);
    let x = mix(&[
        (1.0, layer(LEN, |t| bump(t, T, 0.0003))),
        (
            0.5,
            layer(LEN, |t| snap.next() * strike(t, T, 0.0006, 0.0028)),
        ),
        (
            0.4,
            layer(LEN, |t| body.next() * strike(t, T, 0.0004, 0.008)),
        ),
        (
            0.25,
            layer(LEN, |t| {
                (2.0 * PI * low * (t - T)).cos() * strike(t, T, 0.002, 0.012)
            }),
        ),
        (
            0.05,
            layer(LEN, |t| room.next() * strike(t, T, 0.001, 0.016)),
        ),
    ]);
    made(&format!("click-{}", variant + 1), Sound::Click, T, x, 0.01)
}

/// About 25 ms: a dry, bright tick.
fn tick() -> Made {
    const T: f64 = 0.002;
    const LEN: f64 = 0.025;
    let mut air = Noise::new(0x71C4, Pass::High, 4500.0, 0.7);
    let mut ping = Noise::new(0x71C5, Pass::Band, 6200.0, 5.0);
    let x = mix(&[
        (1.0, layer(LEN, |t| bump(t, T, 0.0002))),
        (
            0.45,
            layer(LEN, |t| air.next() * strike(t, T, 0.0003, 0.002)),
        ),
        (
            0.3,
            layer(LEN, |t| ping.next() * strike(t, T, 0.0003, 0.004)),
        ),
    ]);
    made("tick", Sound::Tick, T, x, 0.006)
}

/// 16 ms, shorter than a frame at 60 fps: one per tile pane makes a soft ratchet.
fn pane_click() -> Made {
    const T: f64 = 0.0015;
    const LEN: f64 = 0.016;
    let mut grit = Noise::new(0x9A4E, Pass::Band, 1900.0, 1.4);
    let x = mix(&[
        (1.0, layer(LEN, |t| bump(t, T, 0.0003))),
        (
            0.5,
            layer(LEN, |t| grit.next() * strike(t, T, 0.0004, 0.0018)),
        ),
        (
            0.3,
            layer(LEN, |t| {
                (2.0 * PI * 900.0 * (t - T)).cos() * strike(t, T, 0.0004, 0.003)
            }),
        ),
    ]);
    made("pane-click", Sound::PaneClick, T, x, 0.004)
}

/// 0.38 s: noise swept up from 350 Hz to 4.2 kHz, swelling into a small snap at 0.35 s (the cut).
fn swish() -> Made {
    const T: f64 = 0.35;
    const LEN: f64 = 0.38;
    let swell = |t: f64| {
        if t <= T {
            (t / T).powf(2.5)
        } else {
            (-(t - T) / 0.012).exp()
        }
    };
    let mut sweep = Noise::new(0x5715, Pass::Band, 350.0, 0.8);
    let body = layer(LEN, |t| {
        let u = (t / T).min(1.0);
        sweep
            .filter
            .tune(Pass::Band, 350.0 * 12.0f64.powf(u.powf(1.5)), 0.8);
        sweep.next() * swell(t)
    });
    let mut hiss = Noise::new(0x5716, Pass::High, 3000.0, 0.7);
    let air = layer(LEN, |t| hiss.next() * swell(t).powi(2));
    let x = mix(&[
        (1.0, layer(LEN, |t| bump(t, T, 0.0004))),
        (0.42, body),
        (0.15, air),
    ]);
    made("swish", Sound::Swish, T, x, 0.01)
}

/// 0.35 s: a buzzy source held to 6 kHz and crushed to 4 bits, chopped into stutters that repeat
/// shrinking slices of it.
fn glitch() -> Made {
    const T: f64 = 0.003;
    const LEN: f64 = 0.35;
    let mut rng = Rng(0x611C);
    let mut held = 0.0;
    let src: Vec<f64> = (0..(0.12 * SR) as usize)
        .map(|i| {
            if i % 8 == 0 {
                let t = i as f64 / SR;
                // The square is positive at the hit, so the first slice and the bump add up.
                let square = if (2.0 * PI * 155.0 * (t - T + 1e-4)).sin() >= 0.0 {
                    0.6
                } else {
                    -0.6
                };
                held = ((square + 0.4 * rng.next()) * 8.0).round() / 8.0;
            }
            held
        })
        .collect();
    // (onset, source offset, slice length, repeats, gain)
    const STUTTERS: [(f64, f64, f64, usize, f64); 6] = [
        (0.000, 0.000, 0.040, 1, 1.0),
        (0.052, 0.012, 0.012, 3, 0.85),
        (0.100, 0.030, 0.007, 5, 0.7),
        (0.150, 0.055, 0.028, 1, 0.9),
        (0.192, 0.020, 0.005, 7, 0.6),
        (0.240, 0.070, 0.045, 2, 0.5),
    ];
    let n = (LEN * SR).round() as usize;
    let mut chops = vec![0.0; n];
    for (k, &(on, off, slice, reps, gain)) in STUTTERS.iter().enumerate() {
        let g = (slice * SR) as usize;
        let o = (off * SR) as usize;
        for r in 0..reps {
            let s = ((on + r as f64 * slice) * SR) as usize;
            for j in 0..g {
                let Some(out) = chops.get_mut(s + j) else {
                    break;
                };
                let t = (s + j) as f64 / SR;
                let shape = if k == 0 {
                    strike(t, T, 0.002, 0.03)
                } else {
                    // 0.5 ms edges: the stutter ticks a little but never pops.
                    let edge = (j.min(g - 1 - j) as f64 / 24.0).min(1.0);
                    let tail = if k == STUTTERS.len() - 1 {
                        1.0 - (r * g + j) as f64 / (reps * g) as f64
                    } else {
                        1.0
                    };
                    edge * tail
                };
                *out += src[o + j] * gain * shape;
            }
        }
    }
    let peak = chops.iter().fold(0.0f64, |m, x| m.max(x.abs()));
    chops.iter_mut().for_each(|x| *x /= peak);
    let x = mix(&[(1.0, layer(LEN, |t| bump(t, T, 0.0003))), (0.7, chops)]);
    made("glitch", Sound::Glitch, T, x, 0.01)
}

/// 1.5 s: noise swept from 300 Hz to 5 kHz over a low 70 to 140 Hz undertone, swelling
/// exponentially into a snap at 1.48 s.
fn riser() -> Made {
    const T: f64 = 1.48;
    const LEN: f64 = 1.5;
    let swell = |t: f64| {
        if t <= T {
            ((4.0 * t / T).exp() - 1.0) / (4.0f64.exp() - 1.0)
        } else {
            (-(t - T) / 0.005).exp()
        }
    };
    let mut sweep = Noise::new(0x8153, Pass::Band, 300.0, 0.9);
    let body = layer(LEN, |t| {
        let u = (t / T).min(1.0);
        sweep
            .filter
            .tune(Pass::Band, 300.0 * (5000.0f64 / 300.0).powf(u), 0.9);
        sweep.next() * swell(t)
    });
    let mut hiss = Noise::new(0x8154, Pass::High, 6000.0, 0.7);
    let air = layer(LEN, |t| hiss.next() * swell(t).powi(2));
    let mut phase = 0.0;
    let tone = layer(LEN, |t| {
        phase += 2.0 * PI * 70.0 * 2f64.powf((t / T).min(1.0)) / SR;
        phase.sin() * swell(t)
    });
    let x = mix(&[
        (1.0, layer(LEN, |t| bump(t, T, 0.0005))),
        (0.45, body),
        (0.15, air),
        (0.12, tone),
    ]);
    made("riser", Sound::Riser, T, x, 0.008)
}

/// 2 s: a 45 Hz pad with its octave, both at a crest on the hit, breathing slowly as it fades.
fn sub_tone() -> Made {
    const T: f64 = 0.04;
    const LEN: f64 = 2.0;
    let x = layer(LEN, |t| {
        let env = if t < T {
            let u = t / T;
            u * u * (3.0 - 2.0 * u)
        } else {
            (-(t - T) / 2.5).exp() * (0.92 + 0.08 * (PI * (t - T)).cos())
        };
        let w = 2.0 * PI * (t - T);
        env * ((45.0 * w).cos() + 0.25 * (90.0 * w).cos())
    });
    made("sub-tone", Sound::SubTone, T, x, 0.6)
}

/// 2 s: a sine whose pitch knocks down from 125 Hz, then sinks from 55 Hz to 40 Hz, with a
/// soft low thud on the hit.
fn sub_boom() -> Made {
    const T: f64 = 0.005;
    const LEN: f64 = 2.0;
    let mut wave = falling(T, |d| {
        40.0 + 15.0 * (-d / 0.35).exp() + 70.0 * (-d / 0.012).exp()
    });
    let boom = layer(LEN, |t| wave(t) * strike(t, T, 0.003, 0.7));
    let mut thud = Noise::new(0x50B0, Pass::Low, 700.0, 0.7);
    let x = mix(&[
        (1.0, boom),
        (0.3, layer(LEN, |t| bump(t, T, 0.001))),
        (
            0.15,
            layer(LEN, |t| thud.next() * strike(t, T, 0.001, 0.008)),
        ),
    ]);
    made("sub-boom", Sound::SubBoom, T, x, 0.4)
}

/// 0.9 s: a low thump falling from 92 Hz to 52 Hz, a mid crunch, a bright crack and a short room.
fn impact() -> Made {
    const T: f64 = 0.006;
    const LEN: f64 = 0.9;
    let mut wave = falling(T, |d| 52.0 + 40.0 * (-d / 0.03).exp());
    let thump = layer(LEN, |t| wave(t) * strike(t, T, 0.002, 0.16));
    let mut crunch = Noise::new(0x1A9C, Pass::Band, 900.0, 0.9);
    let mut crack = Noise::new(0x1A9D, Pass::High, 3500.0, 0.7);
    let mut room = Noise::new(0x1A9E, Pass::Low, 2500.0, 0.7);
    let x = mix(&[
        (1.0, thump),
        (1.0, layer(LEN, |t| bump(t, T, 0.0004))),
        (
            0.45,
            layer(LEN, |t| crunch.next() * strike(t, T, 0.001, 0.035)),
        ),
        (
            0.3,
            layer(LEN, |t| crack.next() * strike(t, T, 0.0005, 0.004)),
        ),
        (
            0.08,
            layer(LEN, |t| room.next() * strike(t, T, 0.004, 0.22)),
        ),
    ]);
    made("impact", Sound::Impact, T, x, 0.15)
}

/// 1.6 s: a heavier hit, a boom falling from 96 Hz to 46 Hz under a dark body and a long, low
/// air tail.
fn hit() -> Made {
    const T: f64 = 0.008;
    const LEN: f64 = 1.6;
    let mut wave = falling(T, |d| 46.0 + 50.0 * (-d / 0.04).exp());
    let boom = layer(LEN, |t| wave(t) * strike(t, T, 0.003, 0.45));
    let mut body = Noise::new(0x4177, Pass::Band, 380.0, 1.0);
    let mut crack = Noise::new(0x4178, Pass::High, 2500.0, 0.7);
    let mut air = Noise::new(0x4179, Pass::Low, 1400.0, 0.7);
    let x = mix(&[
        (1.0, boom),
        (1.0, layer(LEN, |t| bump(t, T, 0.0005))),
        (0.5, layer(LEN, |t| body.next() * strike(t, T, 0.002, 0.07))),
        (
            0.3,
            layer(LEN, |t| crack.next() * strike(t, T, 0.0006, 0.006)),
        ),
        (0.1, layer(LEN, |t| air.next() * strike(t, T, 0.006, 0.5))),
    ]);
    made("hit", Sound::Hit, T, x, 0.3)
}

/// 2.5 s: a soft felt hit on a low fifth (58 and 87 Hz) that breathes out slowly.
fn outro() -> Made {
    const T: f64 = 0.01;
    const LEN: f64 = 2.5;
    let low = layer(LEN, |t| {
        let w = 2.0 * PI * (t - T);
        strike(t, T, 0.008, 0.9) * ((58.0 * w).cos() + 0.5 * (87.0 * w).cos())
    });
    let mut felt = Noise::new(0x0B70, Pass::Low, 700.0, 0.7);
    let mut breath = Noise::new(0x0B71, Pass::Band, 650.0, 0.7);
    let x = mix(&[
        (1.0, low),
        (0.8, layer(LEN, |t| bump(t, T, 0.0008))),
        (0.3, layer(LEN, |t| felt.next() * strike(t, T, 0.002, 0.03))),
        (
            0.18,
            layer(LEN, |t| breath.next() * strike(t, T, 0.008, 0.7)),
        ),
    ]);
    made("outro", Sound::Outro, T, x, 0.9)
}
