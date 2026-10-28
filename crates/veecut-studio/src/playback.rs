use std::{
    fs::File,
    hash::{Hash, Hasher},
    io::BufReader,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, atomic::{AtomicU64, Ordering}, mpsc},
    thread,
    time::Duration,
};

use anyhow::{Context, Result, ensure};
use eframe::egui;
use rodio::{Player, Source};
use veecut_engine::{project::Project, render};

struct Wav {
    samples: hound::WavIntoSamples<BufReader<File>, i16>,
    duration: Duration,
}

impl Wav {
    fn open(path: &Path, from: f64) -> Result<Self> {
        let mut reader = hound::WavReader::open(path).context("opening the preview audio cache")?;
        let spec = reader.spec();
        ensure!(spec.channels == 2 && spec.sample_rate == 48_000 && spec.bits_per_sample == 16,
            "preview audio must be 48 kHz stereo 16-bit WAV");
        let total = reader.duration();
        let start = ((from.max(0.0) * 48_000.0) as u32).min(total);
        reader.seek(start)?;
        Ok(Self { samples: reader.into_samples(), duration: Duration::from_secs_f64(f64::from(total - start) / 48_000.0) })
    }
}

impl Iterator for Wav {
    type Item = f32;
    fn next(&mut self) -> Option<f32> {
        self.samples.next().and_then(Result::ok).map(|sample| f32::from(sample) / 32768.0)
    }
}

impl Source for Wav {
    fn current_span_len(&self) -> Option<usize> { None }
    fn channels(&self) -> rodio::ChannelCount { 2.try_into().expect("stereo is nonzero") }
    fn sample_rate(&self) -> rodio::SampleRate { 48_000.try_into().expect("sample rate is nonzero") }
    fn total_duration(&self) -> Option<Duration> { Some(self.duration) }
}

struct Request {
    generation: u64,
    project: Project,
    dir: PathBuf,
    at: f64,
    size: [u32; 2],
    play: bool,
    context: egui::Context,
}

pub(crate) struct Frame {
    pub generation: u64,
    pub pixels: tiny_skia::Pixmap,
}

pub(crate) enum Event {
    Ready(u64, Arc<Player>, f64),
    End(u64, f64),
    Error(u64, String),
}

pub(crate) struct Playback {
    tx: mpsc::SyncSender<()>,
    pending: Arc<Mutex<Option<Request>>>,
    pub events: mpsc::Receiver<Event>,
    pub frame: Arc<Mutex<Option<Frame>>>,
    generation: Arc<AtomicU64>,
    pub player: Option<Arc<Player>>,
    pub from: f64,
    pub playing: bool,
}

impl Playback {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::sync_channel::<()>(1);
        let pending: Arc<Mutex<Option<Request>>> = Arc::new(Mutex::new(None));
        let worker_pending = Arc::clone(&pending);
        let (events_tx, events) = mpsc::channel();
        let frame = Arc::new(Mutex::new(None));
        let worker_frame = Arc::clone(&frame);
        let generation = Arc::new(AtomicU64::new(0));
        let worker_generation = Arc::clone(&generation);
        thread::spawn(move || {
            while rx.recv().is_ok() {
                let Some(request) = worker_pending.lock().ok().and_then(|mut pending| pending.take()) else { continue; };
                if worker_generation.load(Ordering::Relaxed) != request.generation { continue; }
                let result = render_request(&request, &worker_generation, &worker_frame, &events_tx);
                if let Err(error) = result {
                    let _ = events_tx.send(Event::Error(request.generation, format!("{error:#}")));
                }
                request.context.request_repaint();
            }
        });
        Self { tx, pending, events, frame, generation, player: None, from: 0.0, playing: false }
    }

    pub fn generation(&self) -> u64 { self.generation.load(Ordering::Relaxed) }

    pub fn stop(&mut self) {
        self.generation.fetch_add(1, Ordering::Relaxed);
        if let Some(player) = self.player.take() { player.stop(); }
        self.playing = false;
    }

    pub fn request(&mut self, project: &Project, dir: &Path, at: f64, size: [u32; 2], play: bool, context: &egui::Context) {
        self.stop();
        self.playing = play;
        let request = Request { generation: self.generation(), project: project.clone(), dir: dir.to_owned(), at, size, play, context: context.clone() };
        if let Ok(mut pending) = self.pending.lock() { *pending = Some(request); }
        let _ = self.tx.try_send(());
    }

    pub fn position(&self) -> Option<f64> {
        self.player.as_ref().map(|player| self.from + player.get_pos().as_secs_f64())
    }
}

impl Drop for Playback {
    fn drop(&mut self) { self.stop(); }
}

fn render_request(request: &Request, generation: &AtomicU64, frame: &Mutex<Option<Frame>>, events: &mpsc::Sender<Event>) -> Result<()> {
    let active = || generation.load(Ordering::Relaxed) == request.generation;
    let publish = |pixels| {
        if active() {
            if let Ok(mut slot) = frame.lock() { *slot = Some(Frame { generation: request.generation, pixels }); }
            request.context.request_repaint();
        }
    };
    let [width, height] = request.size;
    if !request.play {
        let at = request.at.min((request.project.duration() - request.project.canvas.frame()).max(0.0));
        publish(render::render_frame(&request.project, &request.dir, at, width, height)?);
        return Ok(());
    }
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    serde_json::to_vec(&request.project)?.hash(&mut hash);
    let cache = request.dir.join(".cache");
    std::fs::create_dir_all(&cache)?;
    let wav = cache.join(format!("preview-{:016x}.wav", hash.finish()));
    if !wav.is_file() {
        let temporary = cache.join(format!("preview-{}.wav", std::process::id()));
        render::mix_audio(&request.project, &request.dir, &temporary, None, true)?;
        std::fs::rename(&temporary, &wav)?;
    }
    if !active() { return Ok(()); }
    let source = Wav::open(&wav, request.at)?;
    let mut stream = render::FrameStream::new(&request.project, &request.dir, request.at, width, height)?;
    // Decode the first frame before starting the clock, so playback starts with a picture.
    if let Some(first) = stream.next() { publish(first?.1); }
    if !active() { return Ok(()); }
    let device = rodio::DeviceSinkBuilder::open_default_sink().context("opening the audio output; check the system sound device")?;
    let player = Arc::new(Player::connect_new(device.mixer()));
    player.append(source);
    let _ = events.send(Event::Ready(request.generation, Arc::clone(&player), request.at));
    request.context.request_repaint();
    for next in stream {
        if !active() { break; }
        let (at, pixels) = next?;
        while active() && !player.empty() && request.at + player.get_pos().as_secs_f64() < at {
            thread::sleep(Duration::from_millis(3));
        }
        if !active() || player.empty() { break; }
        let clock = request.at + player.get_pos().as_secs_f64();
        // Keep only the newest frame; decoding never builds an unbounded image queue.
        if at + 0.15 >= clock { publish(pixels); }
    }
    while active() && !player.empty() { thread::sleep(Duration::from_millis(10)); }
    player.stop();
    let _ = events.send(Event::End(request.generation, request.project.duration()));
    Ok(())
}
