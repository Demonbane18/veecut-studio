//! Temporal smoothing and luma-guided edge refinement. No model-specific code.

/// Smooth in place; the first frame has no fade-in from black.
pub(super) fn smooth(current: &mut [f32], previous: &mut Vec<f32>) {
    // ponytail: EMA suits modest portrait motion; use flow-aligned history for fast movement.
    if previous.len() == current.len() {
        for (now, old) in current.iter_mut().zip(previous.iter()) {
            *now = 0.8 * *now + 0.2 * *old;
        }
    }
    previous.clear();
    previous.extend_from_slice(current);
}

/// Reuses the full-resolution working arrays across frames.
pub(super) struct Guided {
    width: usize,
    height: usize,
    integral: Vec<f64>,
    mean_i: Vec<f32>,
    mean_p: Vec<f32>,
    a: Vec<f32>,
    b: Vec<f32>,
    work: Vec<f32>,
    mean_a: Vec<f32>,
    mean_b: Vec<f32>,
}

fn mean(input: &[f32], output: &mut [f32], integral: &mut [f64], w: usize, h: usize, r: usize) {
    let stride = w + 1;
    integral.fill(0.0);
    for y in 0..h {
        let mut row = 0.0;
        for x in 0..w {
            row += input[y * w + x] as f64;
            integral[(y + 1) * stride + x + 1] = integral[y * stride + x + 1] + row;
        }
    }
    for y in 0..h {
        let top = y.saturating_sub(r);
        let bottom = (y + r + 1).min(h);
        for x in 0..w {
            let left = x.saturating_sub(r);
            let right = (x + r + 1).min(w);
            let sum = integral[bottom * stride + right]
                - integral[top * stride + right]
                - integral[bottom * stride + left]
                + integral[top * stride + left];
            output[y * w + x] = (sum / ((right - left) * (bottom - top)) as f64) as f32;
        }
    }
}

impl Guided {
    pub(super) fn new(width: usize, height: usize) -> Self {
        let n = width * height;
        Self {
            width,
            height,
            integral: vec![0.0; (width + 1) * (height + 1)],
            mean_i: vec![0.0; n],
            mean_p: vec![0.0; n],
            a: vec![0.0; n],
            b: vec![0.0; n],
            work: vec![0.0; n],
            mean_a: vec![0.0; n],
            mean_b: vec![0.0; n],
        }
    }

    /// Local linear fit q = a*luma+b, with regularization to avoid amplifying texture.
    pub(super) fn refine(&mut self, luma: &[f32], mask: &mut [f32], radius: usize) {
        let (w, h) = (self.width, self.height);
        assert_eq!(luma.len(), w * h);
        assert_eq!(mask.len(), w * h);
        mean(luma, &mut self.mean_i, &mut self.integral, w, h, radius);
        mean(mask, &mut self.mean_p, &mut self.integral, w, h, radius);
        for (v, i) in self.work.iter_mut().zip(luma) {
            *v = i * i;
        }
        mean(&self.work, &mut self.a, &mut self.integral, w, h, radius);
        for ((v, i), p) in self.work.iter_mut().zip(luma).zip(mask.iter()) {
            *v = i * p;
        }
        mean(&self.work, &mut self.b, &mut self.integral, w, h, radius);
        for j in 0..mask.len() {
            let variance = (self.a[j] - self.mean_i[j] * self.mean_i[j]).max(0.0);
            self.a[j] = (self.b[j] - self.mean_i[j] * self.mean_p[j]) / (variance + 0.001);
            self.b[j] = self.mean_p[j] - self.a[j] * self.mean_i[j];
        }
        mean(&self.a, &mut self.mean_a, &mut self.integral, w, h, radius);
        mean(&self.b, &mut self.mean_b, &mut self.integral, w, h, radius);
        for j in 0..mask.len() {
            mask[j] = (self.mean_a[j] * luma[j] + self.mean_b[j]).clamp(0.0, 1.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn temporal_smoothing_and_edge_preservation() {
        let mut old = Vec::new();
        let mut frame = vec![1.0, 0.0];
        smooth(&mut frame, &mut old);
        assert_eq!(frame, [1.0, 0.0]);
        frame.copy_from_slice(&[0.0, 1.0]);
        smooth(&mut frame, &mut old);
        assert_eq!(frame, [0.2, 0.8]);
        let luma: Vec<f32> = (0..256)
            .map(|i| if i % 16 < 8 { 0.0 } else { 1.0 })
            .collect();
        let mut mask = luma.clone();
        let mut filter = Guided::new(16, 16);
        filter.refine(&luma, &mut mask, 3);
        assert!(mask[7] < 0.01 && mask[8] > 0.99);
        mask.fill(0.4);
        filter.refine(&luma, &mut mask, 3);
        assert!(mask.iter().all(|v| (v - 0.4).abs() < 1e-5));
        let mut tiny = [0.7];
        Guided::new(1, 1).refine(&[0.5], &mut tiny, 8);
        assert!((tiny[0] - 0.7).abs() < 1e-5);
    }
}
