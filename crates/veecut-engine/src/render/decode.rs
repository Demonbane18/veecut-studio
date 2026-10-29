//! One ffmpeg process per active video clip, plus images decoded once.

use std::collections::HashMap;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::thread::JoinHandle;

use anyhow::{Context, Result, anyhow, bail};
use fast_image_resize::images::{Image, ImageRef};
use fast_image_resize::{FilterType, PixelType, ResizeAlg, ResizeOptions, Resizer};

use crate::media::{self, ffmpeg};
use crate::project::{AssetKind, Clip, Fit, Point, Rect};

use super::math::{self, CropRect};

#[derive(Clone)]
pub(crate) struct FrameReq {
    pub id: String,
    pub path: PathBuf,
    pub image: bool,
    /// Picture size the framing is computed for (the clip asset's size).
    pub src_w: u32,
    pub src_h: u32,
    pub src_t: f64,
    pub src_out: f64,
    pub speed: f64,
    pub fps: f64,
    pub fit: Fit,
    pub focus: Point,
    pub dec_w: u32,
    pub dec_h: u32,
    pub pad: [u8; 3],
    /// The file is another size than `src_w` x `src_h` (a mask): stretch it to that size first
    /// so it frames exactly like the clip.
    pub stretch: bool,
    /// No frame (a mask past its end) is a transparent picture, not an error.
    pub optional: bool,
}

pub(crate) fn video_filter(speed: f64, fps: f64, framing: &str) -> String {
    // STARTPTS so a mid-clip seek still starts at time 0. setpts stays before fps.
    format!("setpts=(PTS-STARTPTS)/{speed:.6},fps={fps:.6}:round=near,{framing}")
}

/// `clear_pad`: letterbox bars are transparent (sources with an alpha plane).
fn framing_filter(req: &FrameReq, crop: Option<(u32, u32, u32, u32)>, clear_pad: bool) -> String {
    let stretch = if req.stretch {
        format!(
            "scale={}:{}:flags=bilinear,",
            req.src_w.max(2),
            req.src_h.max(2)
        )
    } else {
        String::new()
    };
    match req.fit {
        Fit::Cover => {
            let (x, y, w, h) = crop.unwrap_or((0, 0, req.src_w.max(2), req.src_h.max(2)));
            format!(
                "{stretch}crop={w}:{h}:{x}:{y},scale={}:{}:flags=bicubic,setsar=1",
                req.dec_w, req.dec_h
            )
        }
        Fit::Contain => format!(
            "{stretch}scale={}:{}:force_original_aspect_ratio=decrease:flags=bicubic,pad={}:{}:(ow-iw)/2:(oh-ih)/2:color=0x{:02X}{:02X}{:02X}{},setsar=1",
            req.dec_w,
            req.dec_h,
            req.dec_w,
            req.dec_h,
            req.pad[0],
            req.pad[1],
            req.pad[2],
            if clear_pad { "@0" } else { "" }
        ),
    }
}

/// How a source-normalized point lands in the decoded (already framed) picture.
#[derive(Clone, Copy)]
enum FocusMap {
    Cover {
        crop: CropRect,
        src_w: f64,
        src_h: f64,
    },
    Contain {
        ox: f64,
        oy: f64,
        scale: f64,
        src_w: f64,
        src_h: f64,
    },
}

impl FocusMap {
    fn pixel(self, focus: Point, dec_w: f64, dec_h: f64) -> (f64, f64) {
        match self {
            FocusMap::Cover { crop, src_w, src_h } => {
                let sx = focus.x as f64 * src_w;
                let sy = focus.y as f64 * src_h;
                (
                    (sx - crop.x) / crop.w.max(1.0) * dec_w,
                    (sy - crop.y) / crop.h.max(1.0) * dec_h,
                )
            }
            FocusMap::Contain {
                ox,
                oy,
                scale,
                src_w,
                src_h,
            } => (
                ox + focus.x as f64 * src_w * scale,
                oy + focus.y as f64 * src_h * scale,
            ),
        }
    }
}

struct VideoDecoder {
    child: Child,
    stdout: BufReader<ChildStdout>,
    frame_bytes: usize,
    stderr_thread: Option<JoinHandle<String>>,
}

impl Drop for VideoDecoder {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl VideoDecoder {
    fn spawn(
        path: &Path,
        src_t: f64,
        src_len: f64,
        filter: &str,
        w: u32,
        h: u32,
        vp9_alpha: bool,
    ) -> Result<Self> {
        let mut cmd = Command::new(ffmpeg()?);
        // No -noautorotate: phone rotation metadata is applied. +genpts keeps VFR timestamps usable.
        cmd.args([
            "-hide_banner",
            "-nostdin",
            "-loglevel",
            "error",
            "-fflags",
            "+genpts",
        ]);
        cmd.arg("-ss").arg(format!("{src_t:.6}"));
        cmd.arg("-t").arg(format!("{src_len:.6}"));
        if vp9_alpha {
            // ffmpeg's own VP9 decoder drops the alpha plane of transparent WebM; libvpx keeps it.
            cmd.args(["-c:v", "libvpx-vp9"]);
        }
        cmd.arg("-i").arg(path);
        cmd.args([
            "-vf", filter, "-an", "-f", "rawvideo", "-pix_fmt", "rgba", "-",
        ]);
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd
            .spawn()
            .with_context(|| format!("starting ffmpeg to decode {}", path.display()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("ffmpeg stdout was not piped"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow!("ffmpeg stderr was not piped"))?;
        let stderr_thread = std::thread::spawn(move || {
            let mut buf = Vec::new();
            let mut tmp = [0u8; 1024];
            let mut stderr = stderr;
            loop {
                match stderr.read(&mut tmp) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        buf.extend_from_slice(&tmp[..n]);
                        if buf.len() > 4096 {
                            buf.drain(0..buf.len() - 2048);
                        }
                    }
                }
            }
            String::from_utf8_lossy(&buf).trim().to_string()
        });
        Ok(VideoDecoder {
            child,
            stdout: BufReader::new(stdout),
            frame_bytes: w as usize * h as usize * 4,
            stderr_thread: Some(stderr_thread),
        })
    }

    /// Reads the next frame into `buf`. False at the end of the stream.
    fn read_into(&mut self, buf: &mut Vec<u8>) -> Result<bool> {
        buf.resize(self.frame_bytes, 0);
        match self.stdout.read_exact(buf) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => Ok(false),
            Err(e) => {
                let detail = self.stderr_tail();
                bail!(
                    "reading a frame from ffmpeg failed: {e}{}",
                    fmt_detail(&detail)
                )
            }
        }
    }

    fn stderr_tail(&mut self) -> String {
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.stderr_thread
            .take()
            .and_then(|h| h.join().ok())
            .unwrap_or_default()
    }
}

fn fmt_detail(detail: &str) -> String {
    if detail.is_empty() {
        String::new()
    } else {
        format!("\n{detail}")
    }
}

struct Slot {
    key: String,
    image: bool,
    dec: Option<VideoDecoder>,
    pixels: Vec<u8>,
    spare: Vec<u8>,
    w: u32,
    h: u32,
    map: FocusMap,
    origin: f64,
    step: f64,
    next_i: u64,
    has_frame: bool,
    /// Some pixel of the source is not opaque (checked on the first frame).
    alpha: bool,
    alpha_checked: bool,
    generation: u64,
}

/// Keeps decoders alive across frames. Scrub keeps the last few; playback drops clips that ended.
pub(crate) struct DecoderPool {
    slots: Vec<Slot>,
    drop_unused: bool,
    generation: u64,
    resizer: Resizer,
    probes: HashMap<PathBuf, (u32, u32)>,
    alpha_info: HashMap<PathBuf, AlphaInfo>,
    resize_opts: ResizeOptions,
}

impl DecoderPool {
    pub fn new(drop_unused: bool) -> Self {
        DecoderPool {
            slots: Vec::new(),
            drop_unused,
            generation: 0,
            resizer: Resizer::new(),
            probes: HashMap::new(),
            alpha_info: HashMap::new(),
            resize_opts: ResizeOptions::new()
                .resize_alg(ResizeAlg::Convolution(FilterType::Bilinear))
                .use_alpha(false),
        }
    }

    pub fn probe_size(&mut self, path: &Path) -> Result<(u32, u32)> {
        if let Some(size) = self.probes.get(path) {
            return Ok(*size);
        }
        let info = media::probe(path)?;
        if info.width == 0 || info.height == 0 {
            bail!("{} has no picture size", path.display());
        }
        self.probes
            .insert(path.to_path_buf(), (info.width, info.height));
        Ok((info.width, info.height))
    }

    /// Whether a file has an alpha plane and needs the libvpx decoder to keep it. Probed only
    /// where it matters: WebM/Matroska (VP9 alpha) and letterboxed fits (clear bars).
    fn alpha_info(&mut self, path: &Path, fit: Fit) -> AlphaInfo {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase);
        if fit != Fit::Contain && !matches!(ext.as_deref(), Some("webm" | "mkv")) {
            return AlphaInfo::default();
        }
        *self
            .alpha_info
            .entry(path.to_path_buf())
            .or_insert_with(|| probe_alpha(path))
    }

    /// Writes the zoomed picture into `dest` (RGBA, `dest_w` x `dest_h`). Returns whether the
    /// picture may be transparent somewhere (decoded alpha, or no frame for an optional one).
    pub fn fill(
        &mut self,
        req: &FrameReq,
        scale: f32,
        focus: Point,
        dest_w: u32,
        dest_h: u32,
        dest: &mut Vec<u8>,
    ) -> Result<bool> {
        let key = slot_key(req);
        let idx = if let Some(i) = self.slots.iter().position(|s| s.key == key) {
            self.ensure_frame(i, req)?;
            i
        } else {
            self.open_slot(req)?;
            self.slots.len() - 1
        };
        self.slots[idx].generation = self.generation;
        let (fx, fy) = self.slots[idx]
            .map
            .pixel(focus, req.dec_w as f64, req.dec_h as f64);
        let (x, y, w, h) =
            math::zoom_window(req.dec_w as f64, req.dec_h as f64, scale as f64, fx, fy);
        let slot = &self.slots[idx];
        let alpha = slot.alpha || !slot.has_frame;
        self.blit_zoom(idx, x, y, w, h, dest_w, dest_h, dest)?;
        Ok(alpha)
    }

    pub fn end_frame(&mut self) {
        let g = self.generation;
        if self.drop_unused {
            self.slots.retain(|s| s.image || s.generation == g);
        } else if self.slots.len() > 6 {
            self.slots.sort_by_key(|s| std::cmp::Reverse(s.generation));
            self.slots.truncate(6);
        }
        self.generation = self.generation.wrapping_add(1);
    }

    fn spawn(&mut self, req: &FrameReq) -> Result<(FocusMap, VideoDecoder)> {
        let info = self.alpha_info(&req.path, req.fit);
        let (map, filter) = video_framing(req, info.alpha);
        let src_len = (req.src_out - req.src_t)
            .max(req.speed / req.fps.max(1.0))
            .max(0.04);
        let dec = VideoDecoder::spawn(
            &req.path,
            req.src_t.max(0.0),
            src_len,
            &filter,
            req.dec_w,
            req.dec_h,
            info.libvpx,
        )?;
        Ok((map, dec))
    }

    fn open_slot(&mut self, req: &FrameReq) -> Result<()> {
        let (pixels, map, dec) = if req.image {
            let (pixels, map) = frame_image(req)?;
            (pixels, map, None)
        } else {
            let (map, dec) = self.spawn(req)?;
            (Vec::new(), map, Some(dec))
        };
        let step = req.speed / req.fps.max(1.0);
        let mut slot = Slot {
            key: slot_key(req),
            image: req.image,
            dec,
            pixels,
            spare: Vec::new(),
            w: req.dec_w,
            h: req.dec_h,
            map,
            origin: req.src_t,
            step,
            next_i: 0,
            has_frame: req.image,
            alpha: false,
            alpha_checked: false,
            generation: self.generation,
        };
        if req.image {
            check_alpha(&mut slot);
        } else {
            pull(&mut slot, req)?;
        }
        self.slots.push(slot);
        Ok(())
    }

    fn ensure_frame(&mut self, idx: usize, req: &FrameReq) -> Result<()> {
        if self.slots[idx].image {
            return Ok(());
        }
        let step = self.slots[idx].step.max(1e-9);
        let want = ((req.src_t - self.slots[idx].origin) / step).round() as i64;
        let next = self.slots[idx].next_i as i64;
        let frame_bytes = self.slots[idx].w as usize * self.slots[idx].h as usize * 4;
        let max_skip: i64 = if frame_bytes > 4_000_000 { 3 } else { 12 };
        if self.slots[idx].has_frame && want == next - 1 {
            return Ok(());
        }
        if want >= next && want - next <= max_skip {
            while (self.slots[idx].next_i as i64) <= want {
                if !pull(&mut self.slots[idx], req)? {
                    break;
                }
            }
            return Ok(());
        }
        // Jump. Restart at the requested source time.
        let (map, dec) = self.spawn(req)?;
        let slot = &mut self.slots[idx];
        slot.dec = Some(dec);
        slot.map = map;
        slot.origin = req.src_t;
        slot.step = req.speed / req.fps.max(1.0);
        slot.next_i = 0;
        slot.has_frame = false;
        pull(slot, req)?;
        Ok(())
    }

    fn blit_zoom(
        &mut self,
        idx: usize,
        x: f64,
        y: f64,
        w: f64,
        h: f64,
        dest_w: u32,
        dest_h: u32,
        dest: &mut Vec<u8>,
    ) -> Result<()> {
        let need = dest_w as usize * dest_h as usize * 4;
        dest.resize(need, 0);
        let DecoderPool {
            slots,
            resizer,
            resize_opts,
            ..
        } = self;
        let slot = &slots[idx];
        if !slot.has_frame || slot.pixels.len() < slot.w as usize * slot.h as usize * 4 {
            dest.fill(0);
            return Ok(());
        }
        let full = (x - 0.0).abs() < 0.5
            && (y - 0.0).abs() < 0.5
            && (w - slot.w as f64).abs() < 0.5
            && (h - slot.h as f64).abs() < 0.5
            && dest_w == slot.w
            && dest_h == slot.h;
        if full {
            dest.copy_from_slice(&slot.pixels[..need]);
            return Ok(());
        }
        let src = ImageRef::new(slot.w, slot.h, &slot.pixels, PixelType::U8x4)
            .map_err(|e| anyhow!("decoded frame is the wrong size: {e}"))?;
        let mut dst = Image::from_slice_u8(dest_w, dest_h, dest, PixelType::U8x4)
            .map_err(|e| anyhow!("clip buffer is the wrong size: {e}"))?;
        let opts = resize_opts.use_alpha(slot.alpha).crop(x, y, w, h);
        resizer
            .resize(&src, &mut dst, &opts)
            .map_err(|e| anyhow!("scaling a clip frame failed: {e}"))?;
        Ok(())
    }
}

#[derive(Clone, Copy, Default)]
struct AlphaInfo {
    alpha: bool,
    libvpx: bool,
}

/// Asks ffprobe for the first video stream's codec, pixel format and VP9 alpha tag.
fn probe_alpha(path: &Path) -> AlphaInfo {
    let Ok(probe) = media::ffprobe() else {
        return AlphaInfo::default();
    };
    let mut cmd = Command::new(probe);
    cmd.args([
        "-v",
        "error",
        "-select_streams",
        "v:0",
        "-show_entries",
        "stream=codec_name,pix_fmt:stream_tags=alpha_mode",
        "-of",
        "csv=p=0",
    ])
    .arg(path);
    let Ok(out) = media::run(&mut cmd) else {
        return AlphaInfo::default();
    };
    let out = String::from_utf8_lossy(&out);
    let mut fields = out.trim().split(',');
    let (codec, fmt, tag) = (fields.next(), fields.next().unwrap_or(""), fields.next());
    let libvpx = codec == Some("vp9") && tag == Some("1");
    let alpha_fmt = ["yuva", "gbrap", "ya", "pal8"]
        .iter()
        .any(|p| fmt.starts_with(p))
        || ["rgba", "bgra", "argb", "abgr"]
            .iter()
            .any(|p| fmt.contains(p));
    AlphaInfo {
        alpha: libvpx || alpha_fmt,
        libvpx,
    }
}

fn check_alpha(slot: &mut Slot) {
    if !slot.alpha_checked && slot.has_frame {
        slot.alpha = slot.pixels.iter().skip(3).step_by(4).any(|&a| a != 255);
        slot.alpha_checked = true;
    }
}

fn pull(slot: &mut Slot, req: &FrameReq) -> Result<bool> {
    let Some(dec) = slot.dec.as_mut() else {
        return Ok(slot.has_frame);
    };
    if dec.read_into(&mut slot.spare)? {
        std::mem::swap(&mut slot.pixels, &mut slot.spare);
        slot.has_frame = true;
        slot.next_i += 1;
        check_alpha(slot);
        return Ok(true);
    }
    if !slot.has_frame && !req.optional {
        let detail = dec.stderr_tail();
        bail!(
            "ffmpeg returned no frame from {} at {:.3}s{}",
            req.path.display(),
            req.src_t,
            fmt_detail(&detail)
        );
    }
    // Hold the last frame. The clip ran out a frame early (VFR or a short file). An optional
    // picture with no frame at all stays transparent.
    slot.next_i += 1;
    Ok(false)
}

/// The map from source points to the decoded picture, and the source window ffmpeg crops for
/// `Fit::Cover` (None for `Fit::Contain`). Shared by decoding and by caption placement, so the
/// face a caption avoids is where the picture really draws it.
fn video_map(
    fit: Fit,
    src_w: u32,
    src_h: u32,
    dec_w: u32,
    dec_h: u32,
    focus: Point,
) -> (FocusMap, Option<(u32, u32, u32, u32)>) {
    match fit {
        Fit::Cover => {
            let crop = math::cover_crop(
                src_w as f64,
                src_h as f64,
                dec_w as f64,
                dec_h as f64,
                focus.x as f64,
                focus.y as f64,
            );
            let px = math::crop_px(crop, src_w, src_h);
            let map = FocusMap::Cover {
                crop: CropRect {
                    x: px.0 as f64,
                    y: px.1 as f64,
                    w: px.2 as f64,
                    h: px.3 as f64,
                },
                src_w: src_w as f64,
                src_h: src_h as f64,
            };
            (map, Some(px))
        }
        Fit::Contain => {
            let scale =
                (dec_w as f64 / src_w.max(1) as f64).min(dec_h as f64 / src_h.max(1) as f64);
            let iw = src_w as f64 * scale;
            let ih = src_h as f64 * scale;
            let map = FocusMap::Contain {
                ox: (dec_w as f64 - iw) * 0.5,
                oy: (dec_h as f64 - ih) * 0.5,
                scale,
                src_w: src_w as f64,
                src_h: src_h as f64,
            };
            (map, None)
        }
    }
}

fn video_framing(req: &FrameReq, clear_pad: bool) -> (FocusMap, String) {
    let (map, crop) = video_map(
        req.fit, req.src_w, req.src_h, req.dec_w, req.dec_h, req.focus,
    );
    (
        map,
        video_filter(req.speed, req.fps, &framing_filter(req, crop, clear_pad)),
    )
}

/// Where `r`, a rectangle of the source picture (fractions of the source), lands on the canvas
/// (fractions of the canvas) when `clip` is `local` seconds into its item: the same crop, focus
/// and zoom window `DecoderPool::fill` draws with. The result may reach past the canvas edges.
pub fn canvas_rect(
    clip: &Clip,
    src: (u32, u32),
    canvas: (u32, u32),
    local: f64,
    duration: f64,
    r: Rect,
) -> Rect {
    let (scale, view) = math::view_at(clip, local, duration);
    let (dec_w, dec_h) = math::decode_size(clip, canvas.0, canvas.1);
    let (map, _) = video_map(
        clip.fit,
        src.0,
        src.1,
        dec_w,
        dec_h,
        clip.focus.unwrap_or(Point::CENTER),
    );
    let (dw, dh) = (dec_w as f64, dec_h as f64);
    let (fx, fy) = map.pixel(view, dw, dh);
    let (x, y, w, h) = math::zoom_window(dw, dh, scale as f64, fx, fy);
    let corner = |px: f32, py: f32| {
        let (cx, cy) = map.pixel(Point::new(px, py), dw, dh);
        (((cx - x) / w) as f32, ((cy - y) / h) as f32)
    };
    let (x0, y0) = corner(r.x, r.y);
    let (x1, y1) = corner(r.x + r.w, r.y + r.h);
    Rect {
        x: x0,
        y: y0,
        w: x1 - x0,
        h: y1 - y0,
    }
}

fn frame_image(req: &FrameReq) -> Result<(Vec<u8>, FocusMap)> {
    let img = image::ImageReader::open(&req.path)
        .with_context(|| format!("opening image {}", req.path.display()))?
        .with_guessed_format()
        .with_context(|| format!("{} is not a readable image", req.path.display()))?
        .decode()
        .with_context(|| format!("decoding image {}", req.path.display()))?
        .to_rgba8();
    let (sw, sh) = (img.width().max(1), img.height().max(1));
    // Framing geometry: a stretched mask frames as if it were the clip's size.
    let (gw, gh) = if req.stretch {
        (req.src_w.max(1), req.src_h.max(1))
    } else {
        (sw, sh)
    };
    let (kx, ky) = (sw as f64 / gw as f64, sh as f64 / gh as f64);
    let raw = img.into_raw();
    let src = ImageRef::new(sw, sh, &raw, PixelType::U8x4).map_err(|e| {
        anyhow!(
            "image {} could not be read as RGBA: {e}",
            req.path.display()
        )
    })?;
    let mut resizer = Resizer::new();
    let opts = ResizeOptions::new()
        .resize_alg(ResizeAlg::Convolution(FilterType::Bilinear))
        .use_alpha(true);
    match req.fit {
        Fit::Cover => {
            let crop = math::cover_crop(
                gw as f64,
                gh as f64,
                req.dec_w as f64,
                req.dec_h as f64,
                req.focus.x as f64,
                req.focus.y as f64,
            );
            let mut dst = Image::new(req.dec_w, req.dec_h, PixelType::U8x4);
            let px_crop = opts.crop(crop.x * kx, crop.y * ky, crop.w * kx, crop.h * ky);
            resizer
                .resize(&src, &mut dst, &px_crop)
                .map_err(|e| anyhow!("scaling image {} failed: {e}", req.path.display()))?;
            let map = FocusMap::Cover {
                crop,
                src_w: gw as f64,
                src_h: gh as f64,
            };
            Ok((dst.into_vec(), map))
        }
        Fit::Contain => {
            let scale = (req.dec_w as f64 / gw as f64).min(req.dec_h as f64 / gh as f64);
            let iw = ((gw as f64 * scale).round() as u32).clamp(1, req.dec_w);
            let ih = ((gh as f64 * scale).round() as u32).clamp(1, req.dec_h);
            let mut fitted = Image::new(iw, ih, PixelType::U8x4);
            resizer
                .resize(&src, &mut fitted, &opts)
                .map_err(|e| anyhow!("scaling image {} failed: {e}", req.path.display()))?;
            // Pictures with transparency get clear bars, like videos with an alpha plane.
            let clear = raw.iter().skip(3).step_by(4).any(|&a| a != 255);
            let mut framed = vec![0u8; req.dec_w as usize * req.dec_h as usize * 4];
            fill_rgba(&mut framed, req.pad, if clear { 0 } else { 255 });
            let ox = (req.dec_w - iw) / 2;
            let oy = (req.dec_h - ih) / 2;
            blit_opaque(&mut framed, req.dec_w, fitted.buffer(), iw, ih, ox, oy);
            let map = FocusMap::Contain {
                ox: ox as f64,
                oy: oy as f64,
                scale,
                src_w: gw as f64,
                src_h: gh as f64,
            };
            Ok((framed, map))
        }
    }
}

fn fill_rgba(buf: &mut [u8], rgb: [u8; 3], alpha: u8) {
    for px in buf.chunks_exact_mut(4) {
        px.copy_from_slice(&[rgb[0], rgb[1], rgb[2], alpha]);
    }
}

fn blit_opaque(dst: &mut [u8], dst_w: u32, src: &[u8], sw: u32, sh: u32, x: u32, y: u32) {
    let row = sw as usize * 4;
    for row_i in 0..sh {
        let dy = y + row_i;
        let src_off = row_i as usize * row;
        let dst_off = (dy as usize * dst_w as usize + x as usize) * 4;
        let n = row
            .min(src.len().saturating_sub(src_off))
            .min(dst.len().saturating_sub(dst_off));
        dst[dst_off..dst_off + n].copy_from_slice(&src[src_off..src_off + n]);
    }
}

fn slot_key(req: &FrameReq) -> String {
    // Video clips of the same file still need their own decoder (crossfade). Images share one.
    let id = if req.image { "" } else { req.id.as_str() };
    format!(
        "{id}|{}|{}|{}x{}|{:.5}|{:.5}|{}x{}|{}|{:.4},{:.4}|{:02X}{:02X}{:02X}",
        req.path.display(),
        req.image as u8,
        req.src_w,
        req.src_h,
        req.speed,
        req.fps,
        req.dec_w,
        req.dec_h,
        match req.fit {
            Fit::Cover => 0,
            Fit::Contain => 1,
        },
        req.focus.x,
        req.focus.y,
        req.pad[0],
        req.pad[1],
        req.pad[2]
    )
}

pub(crate) fn is_image(kind: AssetKind) -> bool {
    kind == AssetKind::Image
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speed_filter_puts_setpts_before_fps() {
        let f = video_filter(1.5, 30.0, "crop=8:8:0:0");
        let s = f.find("setpts=").expect("setpts");
        let p = f.find("fps=").expect("fps");
        assert!(s < p, "{f}");
        assert!(f.contains("PTS-STARTPTS)/1.500000"), "{f}");
        assert!(!f.contains("noautorotate"));
    }

    #[test]
    fn canvas_rect_follows_the_crop_and_the_punch_in() {
        // A 16:9 source in a 9:16 canvas keeps the full height and 608 of 1920 px of width.
        let mut clip = Clip::new("a", 0.0);
        clip.focus = Some(Point::new(0.5, 0.45));
        let (src, canvas) = ((1920, 1080), (1080, 1920));
        let face = Rect {
            x: 0.45,
            y: 0.3,
            w: 0.1,
            h: 0.2,
        };
        let plain = canvas_rect(&clip, src, canvas, 1.0, 4.0, face);
        assert!((plain.y - 0.3).abs() < 1e-3 && (plain.h - 0.2).abs() < 1e-3);
        assert!((plain.w - 0.1 * 1920.0 / 608.0).abs() < 2e-3);
        assert!((plain.x + plain.w / 2.0 - 0.5).abs() < 2e-3);

        // A 1.15x punch-in aimed at y 0.45 maps source y to 1.15 * y - 0.0175 and grows the box.
        clip.zooms.push(crate::project::Zoom {
            at: 0.0,
            duration: 4.0,
            scale: 1.15,
            focus: None,
            ease: 0.0,
        });
        let zoomed = canvas_rect(&clip, src, canvas, 1.0, 4.0, face);
        assert!(
            (zoomed.y - (1.15 * 0.3 - 0.0175)).abs() < 2e-3,
            "{zoomed:?}"
        );
        assert!((zoomed.h - 0.2 * 1.15).abs() < 2e-3, "{zoomed:?}");
        // Outside the zoom the picture is back at 1x.
        clip.zooms[0].at = 3.0;
        let before = canvas_rect(&clip, src, canvas, 1.0, 4.0, face);
        assert!((before.y - 0.3).abs() < 1e-3);
    }
}
