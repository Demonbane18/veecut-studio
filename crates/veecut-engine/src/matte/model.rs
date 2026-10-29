//! MODNet weights are Apache-2.0, including models explicitly covered by the upstream README:
//! https://github.com/ZHKKKe/MODNet#license
//! ONNX export card: https://huggingface.co/Xenova/modnet (Apache-2.0).
//! Weights stay outside the application and are downloaded only on first use.

use anyhow::{Context, Result, ensure};
use fast_image_resize::images::{Image, ImageRef};
use fast_image_resize::{FilterType, PixelType, ResizeAlg, ResizeOptions, Resizer};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::Arc;
use tract_onnx::prelude::*;

pub(super) const SHA: &str = "07c308cf0fc7e6e8b2065a12ed7fc07e1de8febb7dc7839d7b7f15dd66584df9";
const SIZE: u64 = 25_888_640;
const URL: &str = "https://huggingface.co/Xenova/modnet/resolve/fa2fa546052fba4c08921230a26cc69a333fca12/onnx/model.onnx";

fn path() -> PathBuf {
    crate::config::home().join("models/modnet-fp32-07c308cf.onnx")
}
pub(super) fn ready() -> bool {
    valid(&path()).unwrap_or(false)
}

pub(super) fn status() -> String {
    match fs::metadata(path()) {
        Ok(m) if m.len() == SIZE => format!("MODNet: downloaded ({} bytes, Apache-2.0)", m.len()),
        Ok(m) => format!(
            "MODNet: incomplete ({} of {SIZE} bytes); run cutout to download",
            m.len()
        ),
        Err(_) => format!("MODNet: not downloaded ({SIZE} bytes, downloaded on first cutout)"),
    }
}

fn valid(path: &std::path::Path) -> Result<bool> {
    if fs::metadata(path)?.len() != SIZE {
        return Ok(false);
    }
    let mut file = File::open(path)?;
    let mut hash = Sha256::new();
    let mut buf = [0; 65536];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
    }
    Ok(format!("{:x}", hash.finalize()) == SHA)
}

fn download(progress: &mut dyn FnMut(&str)) -> Result<PathBuf> {
    let path = path();
    if path.is_file() && valid(&path)? {
        return Ok(path);
    }
    let dir = path.parent().context("model directory is missing")?;
    fs::create_dir_all(dir)?;
    let mut part = tempfile::Builder::new()
        .prefix("modnet-")
        .suffix(".part")
        .tempfile_in(dir)?;
    progress("Downloading MODNet (25.9 MB, Apache-2.0)");
    let mut response = ureq::get(URL)
        .call()
        .context("downloading MODNet; check your internet connection")?;
    let mut reader = response.body_mut().as_reader();
    let mut count = 0u64;
    let mut hash = Sha256::new();
    let mut buf = [0; 65536];
    let mut next = 0;
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        count += n as u64;
        ensure!(
            count <= SIZE,
            "MODNet download exceeds pinned size; refusing it"
        );
        hash.update(&buf[..n]);
        part.write_all(&buf[..n])?;
        if count >= next {
            progress(&format!("MODNet download: {count}/{SIZE} bytes"));
            next = count + 1_048_576;
        }
    }
    ensure!(
        count == SIZE && format!("{:x}", hash.finalize()) == SHA,
        "MODNet download failed size/SHA-256 verification; retry cutout"
    );
    part.as_file().sync_all()?;
    part.persist(&path)
        .context("installing verified MODNet weights")?;
    Ok(path)
}

pub(super) struct Model {
    plan: Arc<TypedRunnableModel>,
    input: Arc<Tensor>,
    small: Image<'static>,
    resizer: Resizer,
    pub width: u32,
    pub height: u32,
}

impl Model {
    pub(super) fn load(width: u32, height: u32, progress: &mut dyn FnMut(&str)) -> Result<Self> {
        let path = download(progress)?;
        // Keep aspect ratio, with a 512px short edge and multiples of 32 as MODNet expects.
        let scale = 512.0 / width.min(height) as f64;
        let w = ((width as f64 * scale / 32.0).round() as u32).max(1) * 32;
        let h = ((height as f64 * scale / 32.0).round() as u32).max(1) * 32;
        ensure!(
            w <= 2048 && h <= 2048,
            "cutout needs a source aspect ratio no wider than 4:1"
        );
        progress("Loading MODNet in Rust (tract)");
        let plan = tract_onnx::onnx()
            .model_for_path(path)?
            .with_input_fact(0, f32::fact([1, 3, h as usize, w as usize]).into())?
            .into_optimized()?
            .into_runnable()?;
        Ok(Self {
            plan,
            input: Arc::new(Tensor::zero::<f32>(&[1, 3, h as usize, w as usize])?),
            small: Image::new(w, h, PixelType::U8x3),
            resizer: Resizer::new(),
            width: w,
            height: h,
        })
    }

    pub(super) fn infer(&mut self, rgb: &image::RgbImage, mask: &mut Vec<f32>) -> Result<()> {
        let source = ImageRef::new(rgb.width(), rgb.height(), rgb.as_raw(), PixelType::U8x3)?;
        let options = ResizeOptions::new().resize_alg(ResizeAlg::Convolution(FilterType::Bilinear));
        self.resizer.resize(&source, &mut self.small, &options)?;
        let n = (self.width * self.height) as usize;
        let input = Arc::get_mut(&mut self.input).context("MODNet retained its input tensor")?;
        let mut view = input.to_plain_array_view_mut::<f32>()?;
        let values = view
            .as_slice_mut()
            .context("MODNet input tensor is not contiguous")?;
        for (i, pixel) in self.small.buffer().chunks_exact(3).enumerate() {
            for c in 0..3 {
                values[c * n + i] = pixel[c] as f32 / 127.5 - 1.0;
            }
        }
        let output = self.plan.run(tvec!(self.input.clone().into_tvalue()))?;
        let view = output[0].to_plain_array_view::<f32>()?;
        let values = view
            .as_slice()
            .context("MODNet output tensor is not contiguous")?;
        ensure!(
            values.len() == n && values.iter().all(|x| x.is_finite()),
            "MODNet returned an invalid mask"
        );
        mask.clear();
        mask.extend(values.iter().map(|x| x.clamp(0.0, 1.0)));
        Ok(())
    }
}
