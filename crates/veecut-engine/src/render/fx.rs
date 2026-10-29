//! Transitions, cut effects, masks and the helpers they share.
//!
//! Effect timings come from NextPhase Editor V1's published measurements (at 30 fps: flash 1
//! frame, negative 2 frames out and 2 in, datamosh 4, a 3x3 tile reveal 9, rack focus 9, a whip
//! over the last 4 frames at about x1.15, 1.30, 1.45, 1.83). The code is written fresh.

use std::sync::LazyLock;

/// Frame times drift by a few ulps; the compositor looks this far past them so a frame meant
/// to land on a cut is on it.
pub(crate) const EPS: f64 = 1e-6;

static THREADS: LazyLock<usize> = LazyLock::new(|| {
    std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .clamp(1, 8)
});

/// Runs `f(first_row, rows)` over bands of whole rows (`row_len` elements each) on up to 8
/// threads. Small buffers run on the calling thread.
pub(crate) fn par_rows<T: Send>(buf: &mut [T], row_len: usize, f: impl Fn(usize, &mut [T]) + Sync) {
    if row_len == 0 {
        return;
    }
    let rows = buf.len() / row_len;
    let buf = &mut buf[..rows * row_len];
    let threads = (*THREADS).min(rows / 32).max(1);
    if threads == 1 {
        f(0, buf);
        return;
    }
    let per = rows.div_ceil(threads);
    std::thread::scope(|s| {
        let mut bands = buf.chunks_mut(per * row_len).enumerate();
        let first = bands.next();
        for (i, band) in bands {
            let f = &f;
            s.spawn(move || f(i * per, band));
        }
        if let Some((_, band)) = first {
            f(0, band);
        }
    });
}

/// FNV-1a: a stable seed from an item id.
pub(crate) fn seed(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ b as u64).wrapping_mul(0x0100_0000_01b3)
    })
}

/// SplitMix64: small, deterministic, good enough for picking slices and pane orders.
pub(crate) struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed)
    }

    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// Uniform in `a..b`.
    pub fn range(&mut self, a: f32, b: f32) -> f32 {
        a + (b - a) * ((self.next() >> 40) as f32 / (1u64 << 24) as f32)
    }

    /// Uniform in `0..n`.
    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

/// 0..1 progress through an effect window `[start, start + duration)`; None outside it.
pub(crate) fn progress(t: f64, start: f64, duration: f64) -> Option<f64> {
    (duration > 0.0 && t >= start && t < start + duration).then(|| (t - start) / duration)
}

/// A negative covers half its duration before the cut and half after (2 out, 2 in at 4 frames).
pub(crate) fn negative_on(t: f64, cut: f64, duration: f64) -> bool {
    duration > 0.0 && t >= cut - duration * 0.5 && t < cut + duration * 0.5
}

/// White amount of a main-track flash on `cut`. Up to 3 frames long it is solid white frames
/// from the cut on. Longer flashes keep the fade into white and out centred on the cut, with
/// the frame on the cut solid white.
pub(crate) fn flash_amount(t: f64, cut: f64, duration: f64, frame: f64) -> f32 {
    if duration <= 0.0 {
        return 0.0;
    }
    if duration <= 3.0 * frame + EPS {
        return if progress(t, cut, duration.max(frame)).is_some() {
            1.0
        } else {
            0.0
        };
    }
    let solid = if progress(t, cut, frame).is_some() {
        1.0
    } else {
        0.0
    };
    super::math::peak_at_cut(t, cut, duration).max(solid)
}

/// Zoom of the outgoing picture at `p` (0..1) through a whip: about x1.15, 1.30, 1.45 and 1.83
/// on its last four frames, accelerating.
pub(crate) fn whip_scale(p: f64) -> f32 {
    const KEYS: [(f64, f64); 6] = [
        (-0.25, 1.0),
        (0.0, 1.15),
        (0.25, 1.30),
        (0.5, 1.45),
        (0.75, 1.83),
        (1.0, 2.4),
    ];
    let p = p.clamp(KEYS[0].0, KEYS[KEYS.len() - 1].0);
    let i = KEYS
        .windows(2)
        .position(|w| p <= w[1].0)
        .unwrap_or(KEYS.len() - 2);
    let ((p0, s0), (p1, s1)) = (KEYS[i], KEYS[i + 1]);
    (s0 + (s1 - s0) * (p - p0) / (p1 - p0)) as f32
}

/// Blur radius of a rack focus at `p`: about 2% of the frame height, easing out to sharp.
pub(crate) fn focus_radius(p: f64, canvas_h: u32) -> f32 {
    (0.02 * canvas_h as f64 * (1.0 - p.clamp(0.0, 1.0)).powi(3)) as f32
}

/// The order a tile reveal switches the 3x3 panes (row-major indices), shuffled by `seed`.
pub(crate) fn tile_order(seed: u64) -> [usize; 9] {
    let mut order = [0, 1, 2, 3, 4, 5, 6, 7, 8];
    let mut rng = Rng::new(seed);
    for i in (1..9).rev() {
        order.swap(i, rng.below(i + 1));
    }
    order
}

/// Step of a tile reveal at progress `p`: pane `order[step]` is white, the panes before it show
/// the new picture.
pub(crate) fn tile_step(p: f64) -> usize {
    ((p * 9.0).floor().max(0.0) as usize).min(8)
}

/// Rule-of-thirds pane `i` of a `w` x `h` picture, as pixel bounds `(x0, y0, x1, y1)`.
fn pane(i: usize, w: usize, h: usize) -> (usize, usize, usize, usize) {
    let (col, row) = (i % 3, i / 3);
    (
        w * col / 3,
        h * row / 3,
        w * (col + 1) / 3,
        h * (row + 1) / 3,
    )
}

/// One tile-reveal step on a layer: panes not yet switched turn transparent, the pane being
/// switched turns white.
pub(crate) fn tiles(layer: &mut [u8], w: usize, h: usize, order: &[usize; 9], step: usize) {
    for (n, &p) in order.iter().enumerate().skip(step) {
        let (x0, y0, x1, y1) = pane(p, w, h);
        for y in y0..y1 {
            let line = &mut layer[(y * w + x0) * 4..(y * w + x1) * 4];
            for px in line.chunks_exact_mut(4) {
                if n == step {
                    px.copy_from_slice(&[255, 255, 255, 255]);
                } else {
                    px[3] = 0;
                }
            }
        }
    }
}

/// Inverts the colour channels.
pub(crate) fn invert(data: &mut [u8], w: usize) {
    par_rows(data, w * 4, |_, band| {
        for px in band.chunks_exact_mut(4) {
            px[0] = 255 - px[0];
            px[1] = 255 - px[1];
            px[2] = 255 - px[2];
        }
    });
}

/// Datamosh on the composited frame: an RGB split, shifted horizontal slices and blocks smeared
/// down from their top row. The same seed gives the same frame; `strength` is 0..1.
pub(crate) fn glitch(data: &mut [u8], w: usize, h: usize, seed: u64, strength: f32) {
    if w < 2 || h < 2 {
        return;
    }
    let stride = w * 4;
    let split = ((w as f32 * 0.012 * strength).round() as usize).max(1);
    par_rows(data, stride, |_, band| {
        let mut row = vec![0u8; stride];
        for line in band.chunks_exact_mut(stride) {
            row.copy_from_slice(line);
            for x in 0..w {
                line[x * 4] = row[x.saturating_sub(split) * 4];
                line[x * 4 + 2] = row[(x + split).min(w - 1) * 4 + 2];
            }
        }
    });
    let mut rng = Rng::new(seed);
    for _ in 0..5 + rng.below(6) {
        let hh = ((h as f32 * rng.range(0.01, 0.07)) as usize).clamp(1, h);
        let y0 = rng.below(h - hh + 1);
        let dx = (w as f32 * rng.range(0.03, 0.16) * strength) as usize % w;
        let right = rng.next() & 1 == 0;
        for line in data[y0 * stride..(y0 + hh) * stride].chunks_exact_mut(stride) {
            if right {
                line.rotate_right(dx * 4);
            } else {
                line.rotate_left(dx * 4);
            }
        }
    }
    let mut row = vec![0u8; stride];
    for _ in 0..3 + rng.below(5) {
        let bw = ((w as f32 * rng.range(0.08, 0.3)) as usize).clamp(1, w);
        let bh = ((h as f32 * rng.range(0.03, 0.12) * strength.max(0.3)) as usize).clamp(1, h);
        let x0 = rng.below(w - bw + 1);
        let y0 = rng.below(h - bh + 1);
        let top = y0 * stride + x0 * 4;
        row[..bw * 4].copy_from_slice(&data[top..top + bw * 4]);
        for y in y0 + 1..y0 + bh {
            let at = y * stride + x0 * 4;
            data[at..at + bw * 4].copy_from_slice(&row[..bw * 4]);
        }
    }
}

/// Multiplies a layer's alpha by the mask's brightness (and the mask's own alpha).
pub(crate) fn apply_mask(layer: &mut [u8], mask: &[u8], w: usize) {
    let stride = w * 4;
    par_rows(layer, stride, |y0, band| {
        let mask = &mask[y0 * stride..];
        for (p, m) in band.chunks_exact_mut(4).zip(mask.chunks_exact(4)) {
            let luma = (54 * m[0] as u32 + 183 * m[1] as u32 + 19 * m[2] as u32 + 128) >> 8;
            p[3] = mul255(p[3] as u32, mul255(luma, m[3] as u32) as u32);
        }
    });
}

/// `a * b / 255`, rounded, for a and b in 0..=255.
fn mul255(a: u32, b: u32) -> u8 {
    let v = a * b + 128;
    ((v + (v >> 8)) >> 8) as u8
}

/// Buffers the layer effects reuse from frame to frame.
pub(crate) struct Work {
    small: Vec<u8>,
    tmp: Vec<u8>,
    grid: Vec<f32>,
    grid_t: Vec<f32>,
    edge: Vec<u8>,
}

impl Work {
    pub fn new() -> Self {
        Work {
            small: Vec::new(),
            tmp: Vec::new(),
            grid: Vec::new(),
            grid_t: Vec::new(),
            edge: Vec::new(),
        }
    }

    /// Blurs a straight-alpha RGBA layer with a radius of about `radius` px: three box blurs
    /// (close to a Gaussian) on a picture shrunk by at least 3. Under `FLOOR` px the `FLOOR`
    /// blur is mixed with the sharp picture, so a rack focus eases into sharp without steps.
    /// Layers with `alpha` are premultiplied around the blur so their edges do not darken.
    pub fn blur(&mut self, px: &mut [u8], w: u32, h: u32, radius: f32, alpha: bool) {
        const FLOOR: f32 = 6.0;
        let (w, h) = (w as usize, h as usize);
        if radius < 0.3 || w < 2 || h < 2 {
            return;
        }
        let n = ((radius.max(FLOOR) / 2.0).round() as usize).clamp(1, w.min(h));
        let (sw, sh) = (w / n, h / n);
        if alpha {
            premultiply(px, w);
        }
        let mut small = std::mem::take(&mut self.small);
        small.resize(sw * sh * 4, 0);
        shrink(px, w, n, &mut small, sw, sh);
        self.tmp.resize(small.len(), 0);
        for _ in 0..3 {
            box_rows(&small, &mut self.tmp, sw, 2);
            box_cols(&self.tmp, &mut small, sw, sh, 2);
        }
        // Back up to full size, block centres on block centres, mixed over the sharp picture.
        let back = |i: usize| (i as f32 + 0.5) / n as f32 - 0.5;
        let (cols, rows) = (taps(w, sw, back), taps(h, sh, back));
        let k = ((radius / FLOOR).min(1.0) * 256.0) as u64;
        resample(&small, sw, px, &cols, &rows, |_, _, sharp, blurred| {
            lerp(sharp, blurred, k)
        });
        self.small = small;
        if alpha {
            unpremultiply(px, w);
        }
    }

    /// Radial zoom blur toward the centre of `small` (`sw` x `sh`), spread over `amount` of the
    /// distance to it (eight taps from three doubling passes), scaled up into `out` (`w` x `h`).
    pub fn zoom_blur(
        &mut self,
        small: &mut Vec<u8>,
        sw: u32,
        sh: u32,
        out: &mut [u8],
        w: u32,
        h: u32,
        amount: f32,
    ) {
        let (sw, sh) = (sw as usize, sh as usize);
        if amount > 0.002 && sw >= 2 && sh >= 2 {
            self.tmp.resize(small.len(), 0);
            for j in 1..=3 {
                zoom_pass(small, &mut self.tmp, sw, sh, 1.0 - amount / (1 << j) as f32);
                std::mem::swap(small, &mut self.tmp);
            }
        }
        scale_into(small, sw, sh, out, w as usize, h as usize);
    }

    /// A white sticker edge `radius` px wide outside the layer's alpha: the alpha is dilated
    /// with a round brush (an exact distance transform at half size for large layers) and the
    /// layer turns white where the dilated alpha exceeds its own.
    pub fn outline(&mut self, px: &mut [u8], w: u32, h: u32, radius: f32) {
        let (w, h) = (w as usize, h as usize);
        if radius < 0.5 || w < 2 || h < 2 {
            return;
        }
        let half = w * h > 600_000;
        let s = if half { 2 } else { 1 };
        let (gw, gh) = (w.div_ceil(s), h.div_ceil(s));
        let r = radius / s as f32;
        const FAR: f32 = 1e7;
        self.grid.clear();
        self.grid.resize(gw * gh, FAR);
        for gy in 0..gh {
            for gx in 0..gw {
                let (mut sum, mut n) = (0u32, 0u32);
                for y in gy * s..((gy + 1) * s).min(h) {
                    for x in gx * s..((gx + 1) * s).min(w) {
                        sum += px[(y * w + x) * 4 + 3] as u32;
                        n += 1;
                    }
                }
                if sum >= 128 * n {
                    self.grid[gy * gw + gx] = 0.0;
                }
            }
        }
        // Squared distances along rows, then along columns of the transposed grid.
        par_rows(&mut self.grid, gw, |_, band| edt_rows(band, gw));
        self.grid_t.resize(gw * gh, 0.0);
        for y in 0..gh {
            for x in 0..gw {
                self.grid_t[x * gh + y] = self.grid[y * gw + x];
            }
        }
        par_rows(&mut self.grid_t, gh, |_, band| edt_rows(band, gh));
        // Edge coverage on the (transposed) grid: 1 inside the brush, a 1 px ramp at its rim.
        self.edge.resize(gw * gh, 0);
        for (e, d2) in self.edge.iter_mut().zip(&self.grid_t) {
            *e = ((r + 0.5 - d2.sqrt()).clamp(0.0, 1.0) * 255.0).round() as u8;
        }
        let edge = &self.edge;
        let inv = 1.0 / s as f32;
        par_rows(px, w * 4, |y0, band| {
            for (k, line) in band.chunks_exact_mut(w * 4).enumerate() {
                let gy = (((y0 + k) as f32 + 0.5) * inv - 0.5).clamp(0.0, (gh - 1) as f32);
                let (y_lo, fy) = (gy as usize, gy.fract());
                let y_hi = (y_lo + 1).min(gh - 1);
                for (x, p) in line.chunks_exact_mut(4).enumerate() {
                    if p[3] == 255 {
                        continue;
                    }
                    let gx = ((x as f32 + 0.5) * inv - 0.5).clamp(0.0, (gw - 1) as f32);
                    let (x_lo, fx) = (gx as usize, gx.fract());
                    let x_hi = (x_lo + 1).min(gw - 1);
                    let at = |gx: usize, gy: usize| edge[gx * gh + gy] as f32;
                    let top = at(x_lo, y_lo) + (at(x_hi, y_lo) - at(x_lo, y_lo)) * fx;
                    let bot = at(x_lo, y_hi) + (at(x_hi, y_hi) - at(x_lo, y_hi)) * fx;
                    let d = top + (bot - top) * fy;
                    let a = p[3] as f32;
                    if d <= a + 0.5 {
                        continue;
                    }
                    // The clip over a white sticker of alpha `d`.
                    for c in &mut p[..3] {
                        *c = ((*c as f32 * a + 255.0 * (d - a)) / d).round() as u8;
                    }
                    p[3] = d.round() as u8;
                }
            }
        });
    }
}

/// Felzenszwalb's 1-D squared distance transform over each row of `band`, in place.
fn edt_rows(band: &mut [f32], n: usize) {
    let mut f = vec![0f32; n];
    let mut v = vec![0usize; n];
    let mut z = vec![0f32; n + 1];
    for row in band.chunks_exact_mut(n) {
        f.copy_from_slice(row);
        let mut k = 0usize;
        v[0] = 0;
        z[0] = f32::NEG_INFINITY;
        z[1] = f32::INFINITY;
        let cross = |f: &[f32], q: usize, p: usize| {
            ((f[q] + (q * q) as f32) - (f[p] + (p * p) as f32)) / (2 * (q - p)) as f32
        };
        for q in 1..n {
            let mut s = cross(&f, q, v[k]);
            while s <= z[k] {
                k -= 1;
                s = cross(&f, q, v[k]);
            }
            k += 1;
            v[k] = q;
            z[k] = s;
            z[k + 1] = f32::INFINITY;
        }
        k = 0;
        for (q, out) in row.iter_mut().enumerate() {
            while z[k + 1] < q as f32 {
                k += 1;
            }
            let d = q as f32 - v[k] as f32;
            *out = d * d + f[v[k]];
        }
    }
}

fn premultiply(px: &mut [u8], w: usize) {
    par_rows(px, w * 4, |_, band| {
        for p in band.chunks_exact_mut(4) {
            let a = p[3] as u32;
            for c in &mut p[..3] {
                *c = mul255(*c as u32, a);
            }
        }
    });
}

fn unpremultiply(px: &mut [u8], w: usize) {
    par_rows(px, w * 4, |_, band| {
        for p in band.chunks_exact_mut(4) {
            let a = p[3] as u32;
            if a == 0 || a == 255 {
                continue;
            }
            for c in &mut p[..3] {
                *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
            }
        }
    });
}

/// Fixed-point reciprocal of a box width: `(sum * inv + 2^15) >> 16` divides by it, rounded.
fn box_inv(r: usize) -> u32 {
    let div = (2 * r + 1) as u32;
    ((1 << 16) + div / 2) / div
}

/// Horizontal box blur of radius `r` from `src` into `dst` (edge pixels repeat).
fn box_rows(src: &[u8], dst: &mut [u8], w: usize, r: usize) {
    let inv = box_inv(r);
    par_rows(dst, w * 4, |y0, band| {
        for (k, out) in band.chunks_exact_mut(w * 4).enumerate() {
            let row = &src[(y0 + k) * w * 4..(y0 + k + 1) * w * 4];
            let px = |x: usize| {
                let i = x.min(w - 1) * 4;
                [
                    row[i] as u32,
                    row[i + 1] as u32,
                    row[i + 2] as u32,
                    row[i + 3] as u32,
                ]
            };
            let first = px(0);
            let mut sum = first.map(|v| v * (r as u32 + 1));
            for x in 1..=r {
                let p = px(x);
                for c in 0..4 {
                    sum[c] += p[c];
                }
            }
            for (x, o) in out.chunks_exact_mut(4).enumerate() {
                let (add, sub) = (px(x + r + 1), px(x.saturating_sub(r)));
                for c in 0..4 {
                    o[c] = ((sum[c] * inv + (1 << 15)) >> 16) as u8;
                    sum[c] = sum[c] + add[c] - sub[c];
                }
            }
        }
    });
}

/// Vertical box blur of radius `r` from `src` into `dst`, sliding a row of sums down each band.
fn box_cols(src: &[u8], dst: &mut [u8], w: usize, h: usize, r: usize) {
    let inv = box_inv(r);
    let stride = w * 4;
    let row = |y: isize| {
        let y = y.clamp(0, h as isize - 1) as usize;
        &src[y * stride..(y + 1) * stride]
    };
    par_rows(dst, stride, |y0, band| {
        let (y0, r) = (y0 as isize, r as isize);
        let mut sum = vec![0u32; stride];
        for y in y0 - r..=y0 + r {
            for (s, &v) in sum.iter_mut().zip(row(y)) {
                *s += v as u32;
            }
        }
        for (k, out) in band.chunks_exact_mut(stride).enumerate() {
            let y = y0 + k as isize;
            for (o, s) in out.iter_mut().zip(&sum) {
                *o = ((s * inv + (1 << 15)) >> 16) as u8;
            }
            for ((s, &a), &b) in sum.iter_mut().zip(row(y + r + 1)).zip(row(y - r)) {
                *s = *s + a as u32 - b as u32;
            }
        }
    });
}

// Resampling works on whole pixels: the four bytes spread into the four 16-bit lanes of a u64,
// so one multiply blends all channels at once.
const LANES: u64 = 0x00FF_00FF_00FF_00FF;

#[inline]
fn load(buf: &[u8], i: usize) -> u64 {
    let p = u32::from_le_bytes([buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]) as u64;
    (p & 0xFF) | ((p & 0xFF00) << 8) | ((p & 0xFF_0000) << 16) | ((p & 0xFF00_0000) << 24)
}

#[inline]
fn store(buf: &mut [u8], i: usize, v: u64) {
    let p = (v & 0xFF) | ((v >> 8) & 0xFF00) | ((v >> 16) & 0xFF_0000) | ((v >> 24) & 0xFF00_0000);
    buf[i..i + 4].copy_from_slice(&(p as u32).to_le_bytes());
}

/// `a + (b - a) * f / 256` per channel, rounded, `f` in 0..=256.
#[inline]
fn lerp(a: u64, b: u64, f: u64) -> u64 {
    ((a * (256 - f) + b * f + 0x0080_0080_0080_0080) >> 8) & LANES
}

/// Bilinear taps (byte offset low, byte offset high, weight of high in 0..256) for each of
/// `n_out` positions, where `at(i)` is the source coordinate of output pixel `i`.
fn taps(n_out: usize, n_src: usize, at: impl Fn(usize) -> f32) -> Vec<(usize, usize, u64)> {
    (0..n_out)
        .map(|i| {
            let s = at(i).clamp(0.0, (n_src - 1) as f32);
            let lo = s as usize;
            (
                lo * 4,
                (lo + 1).min(n_src - 1) * 4,
                ((s - lo as f32) * 256.0) as u64,
            )
        })
        .collect()
}

/// Samples `src` (`sw` wide) at the given column and row taps into each output pixel; the
/// stored value is `finish(x, y, old, sample)` with the pixel's old value.
fn resample(
    src: &[u8],
    sw: usize,
    dst: &mut [u8],
    cols: &[(usize, usize, u64)],
    rows: &[(usize, usize, u64)],
    finish: impl Fn(usize, usize, u64, u64) -> u64 + Sync,
) {
    let (src_stride, stride) = (sw * 4, cols.len() * 4);
    par_rows(dst, stride, |y0, band| {
        for (k, out) in band.chunks_exact_mut(stride).enumerate() {
            let (yl, yh, fy) = rows[y0 + k];
            let top = &src[yl / 4 * src_stride..][..src_stride];
            let bot = &src[yh / 4 * src_stride..][..src_stride];
            for (x, &(xl, xh, fx)) in cols.iter().enumerate() {
                let t = lerp(load(top, xl), load(top, xh), fx);
                let b = lerp(load(bot, xl), load(bot, xh), fx);
                let v = finish(x, y0 + k, load(out, x * 4), lerp(t, b, fy));
                store(out, x * 4, v);
            }
        }
    });
}

/// Bilinear resize of `src` (`sw` x `sh`) into `dst` (`dw` x `dh`), pixel centres aligned.
fn scale_into(src: &[u8], sw: usize, sh: usize, dst: &mut [u8], dw: usize, dh: usize) {
    let (kx, ky) = (sw as f32 / dw as f32, sh as f32 / dh as f32);
    let cols = taps(dw, sw, |i| (i as f32 + 0.5) * kx - 0.5);
    let rows = taps(dh, sh, |i| (i as f32 + 0.5) * ky - 0.5);
    resample(src, sw, dst, &cols, &rows, |_, _, _, v| v);
}

/// Averages `n` x `n` blocks of `src` (`w` wide) into `dst` (`sw` x `sh`, at most `w / n` by
/// `h / n`). A leftover strip at the right and bottom edges is left out.
fn shrink(src: &[u8], w: usize, n: usize, dst: &mut [u8], sw: usize, sh: usize) {
    let stride = w * 4;
    let area = (n * n) as u32;
    par_rows(&mut dst[..sw * sh * 4], sw * 4, |y0, band| {
        let mut acc = vec![0u32; sw * 4];
        for (k, out) in band.chunks_exact_mut(sw * 4).enumerate() {
            acc.fill(0);
            for row in src[(y0 + k) * n * stride..][..n * stride].chunks_exact(stride) {
                for (a, block) in acc.chunks_exact_mut(4).zip(row.chunks_exact(n * 4)) {
                    for p in block.chunks_exact(4) {
                        for c in 0..4 {
                            a[c] += p[c] as u32;
                        }
                    }
                }
            }
            for (o, a) in out.iter_mut().zip(&acc) {
                *o = ((a + area / 2) / area) as u8;
            }
        }
    });
}

/// `dst` = average of `src` and `src` scaled by `m` about its centre.
fn zoom_pass(src: &[u8], dst: &mut [u8], w: usize, h: usize, m: f32) {
    let about = |n: usize| {
        let c = n as f32 * 0.5;
        move |i: usize| c + (i as f32 + 0.5 - c) * m - 0.5
    };
    let cols = taps(w, w, about(w));
    let rows = taps(h, h, about(h));
    let stride = w * 4;
    resample(src, w, dst, &cols, &rows, |x, y, _, s| {
        ((load(src, y * stride + x * 4) + s + 0x0001_0001_0001_0001) >> 1) & LANES
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const FPS: f64 = 30.0;

    fn frame_t(i: i64, cut: f64) -> f64 {
        cut + i as f64 / FPS + EPS
    }

    #[test]
    fn tiles_reveal_every_pane_once_in_nine_frames() {
        let order = tile_order(seed("item-7"));
        assert_eq!(order, tile_order(seed("item-7")), "same id, same order");
        assert_ne!(
            order,
            tile_order(seed("item-8")),
            "another id shuffles differently"
        );
        let cut = 2.0;
        let duration = 9.0 / FPS;
        let mut white = Vec::new();
        let mut revealed_before = 0;
        for i in 0..9 {
            let p = progress(frame_t(i, cut), cut, duration).expect("inside the reveal");
            let step = tile_step(p);
            assert_eq!(step, i as usize, "one pane per frame");
            assert!(step >= revealed_before);
            revealed_before = step;
            white.push(order[step]);
        }
        white.sort();
        assert_eq!(
            white,
            (0..9).collect::<Vec<_>>(),
            "each pane white exactly once"
        );
        assert!(
            progress(frame_t(9, cut), cut, duration).is_none(),
            "done after 9 frames"
        );

        // A 6x6 layer after step 4: the first four panes show the picture, the fifth is white,
        // the rest are transparent.
        let mut layer = vec![10u8; 6 * 6 * 4];
        tiles(&mut layer, 6, 6, &order, 4);
        for (n, &p) in order.iter().enumerate() {
            let (x0, y0, _, _) = pane(p, 6, 6);
            let px = &layer[(y0 * 6 + x0) * 4..(y0 * 6 + x0) * 4 + 4];
            match n {
                0..4 => assert_eq!(px, [10, 10, 10, 10]),
                4 => assert_eq!(px, [255, 255, 255, 255]),
                _ => assert_eq!(px[3], 0),
            }
        }
    }

    #[test]
    fn negative_covers_two_frames_each_side_of_the_cut() {
        let duration = 4.0 / FPS;
        for cut in [1.0, 1.0 + 0.4 / FPS] {
            let on: Vec<i64> = (-5..5)
                .filter(|&i| {
                    // Frames on the 30 fps grid nearest the cut.
                    let t = (cut * FPS).ceil() / FPS + i as f64 / FPS + EPS;
                    negative_on(t, cut, duration)
                })
                .collect();
            assert_eq!(on.len(), 4, "cut {cut}: {on:?}");
            if cut == 1.0 {
                assert_eq!(on, vec![-2, -1, 0, 1]);
            }
        }
    }

    #[test]
    fn short_flash_is_solid_white_frames_from_the_cut() {
        let frame = 1.0 / FPS;
        let one: Vec<f32> = (-2..3)
            .map(|i| flash_amount(frame_t(i, 1.0), 1.0, frame, frame))
            .collect();
        assert_eq!(one, vec![0.0, 0.0, 1.0, 0.0, 0.0]);
        let long = flash_amount(frame_t(0, 1.0), 1.0, 0.3, frame);
        assert_eq!(long, 1.0);
        assert!(flash_amount(frame_t(-3, 1.0), 1.0, 0.3, frame) > 0.0);
    }

    #[test]
    fn whip_scales_follow_the_measured_steps() {
        let d = 4.0 / FPS;
        let got: Vec<f32> = (0..4)
            .map(|i| whip_scale(progress(1.0 - d + i as f64 / FPS + EPS, 1.0 - d, d).unwrap()))
            .collect();
        for (g, want) in got.iter().zip([1.15, 1.30, 1.45, 1.83]) {
            assert!((g - want).abs() < 0.01, "{got:?}");
        }
    }

    #[test]
    fn outline_only_outside_the_mask() {
        let (w, h) = (64usize, 64usize);
        let mut layer = vec![0u8; w * h * 4];
        for y in 22..42 {
            for x in 22..42 {
                layer[(y * w + x) * 4..(y * w + x) * 4 + 4].copy_from_slice(&[200, 30, 30, 255]);
            }
        }
        let before = layer.clone();
        Work::new().outline(&mut layer, w as u32, h as u32, 4.0);
        let px = |l: &[u8], x: usize, y: usize| {
            <[u8; 4]>::try_from(&l[(y * w + x) * 4..(y * w + x) * 4 + 4]).unwrap()
        };
        for y in 0..h {
            for x in 0..w {
                let inside = (22..42).contains(&x) && (22..42).contains(&y);
                let dx = (22i32 - x as i32).max(x as i32 - 41).max(0);
                let dy = (22i32 - y as i32).max(y as i32 - 41).max(0);
                let dist = ((dx * dx + dy * dy) as f32).sqrt();
                let p = px(&layer, x, y);
                if inside {
                    assert_eq!(p, px(&before, x, y), "inside changed at {x},{y}");
                } else if dist <= 3.0 {
                    assert_eq!(p, [255, 255, 255, 255], "edge missing at {x},{y}");
                } else if dist >= 5.5 {
                    assert_eq!(p[3], 0, "edge too wide at {x},{y}");
                }
            }
        }
    }

    #[test]
    fn glitch_is_deterministic() {
        let base: Vec<u8> = (0..64 * 48 * 4).map(|i| (i * 7 % 251) as u8).collect();
        let mut a = base.clone();
        let mut b = base.clone();
        glitch(&mut a, 64, 48, 42, 1.0);
        glitch(&mut b, 64, 48, 42, 1.0);
        assert_eq!(a, b);
        assert_ne!(a, base);
    }
}
