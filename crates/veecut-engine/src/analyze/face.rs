//! Where the face is, so crops and zooms keep the speaker in frame. Uses rustface (BSD-2) with
//! the SeetaFace frontal model bundled in `assets/models` (BSD-2, VIPL group, ICT, CAS).

use std::path::Path;

use anyhow::{Context, Result};
use rustface::ImageData;

use crate::media;
use crate::project::Point;

static MODEL: &[u8] = include_bytes!("../../assets/models/seeta_fd_frontal_v1.0.bin");

/// Frames are scaled down to this width before detection.
const DETECT_WIDTH: u32 = 480;

/// Median face center over `samples` frames of `range`, if a face is found. The point is
/// normalized to the source picture; with several faces in a frame, the largest one counts.
pub fn face_focus(path: &Path, range: (f64, f64), samples: usize) -> Result<Option<Point>> {
    let model = rustface::read_model(MODEL)
        .context("the bundled face model is damaged; reinstall VeeCut")?;
    let mut detector = rustface::create_detector_with_model(model);
    detector.set_min_face_size(24);
    detector.set_score_thresh(2.0);
    detector.set_pyramid_scale_factor(0.8);
    detector.set_slide_window_step(4, 4);
    let samples = samples.max(1);
    let (mut xs, mut ys) = (Vec::new(), Vec::new());
    for k in 0..samples {
        let t = range.0 + (range.1 - range.0).max(0.0) * (k as f64 + 0.5) / samples as f64;
        let frame = media::frame_rgba(path, t, DETECT_WIDTH)
            .with_context(|| format!("reading a frame at {t:.2} s for face detection"))?;
        let gray = image::DynamicImage::ImageRgba8(frame).to_luma8();
        let (w, h) = gray.dimensions();
        let faces = detector.detect(&ImageData::new(&gray, w, h));
        if let Some(face) = faces
            .iter()
            .max_by_key(|f| f.bbox().width() * f.bbox().height())
        {
            let b = face.bbox();
            xs.push(((b.x() as f32 + b.width() as f32 / 2.0) / w as f32).clamp(0.0, 1.0));
            ys.push(((b.y() as f32 + b.height() as f32 / 2.0) / h as f32).clamp(0.0, 1.0));
        }
    }
    Ok(median(&mut xs)
        .zip(median(&mut ys))
        .map(|(x, y)| Point::new(x, y)))
}

fn median(v: &mut [f32]) -> Option<f32> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(f32::total_cmp);
    Some(v[v.len() / 2])
}
