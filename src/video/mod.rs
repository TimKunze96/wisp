//! Screen sharing: capture the screen, encode it as H.264 for the server,
//! and decode the screens we watch into pictures for the viewer window.

mod convert;
pub mod gpu;
#[cfg(target_os = "linux")]
#[path = "sound_linux.rs"]
mod sound;
#[cfg(windows)]
#[path = "sound_windows.rs"]
mod sound;
#[cfg(target_os = "macos")]
mod sound {
    pub struct Sound;
    impl Sound {
        pub fn start(_packets: impl FnMut(Vec<u8>) + Send + 'static) -> anyhow::Result<Self> {
            anyhow::bail!("sharing sound isn't supported here yet")
        }
    }
}
#[cfg(target_os = "linux")]
#[path = "capture_linux.rs"]
mod capture;
#[cfg(not(target_os = "linux"))]
#[path = "capture_xcap.rs"]
mod capture;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use futures::channel::mpsc as fmpsc;
use openh264::OpenH264API;
use openh264::decoder::Decoder;
use openh264::encoder::{BitRate, Encoder, EncoderConfig, FrameRate, FrameType, IntraFramePeriod, UsageType};
use openh264::formats::YUVBuffer;

pub use banshee_client::{AUDIO, Layer, Packet, Quality, StreamInfo};
use banshee_client::MAX_LAYERS;

/// Frame rate of the smaller layers (and of everything on the CPU at 30).
const FPS: u32 = 30;
/// A keyframe at least this often on the CPU layers, so a picture that
/// broke heals by itself.
const KEYFRAME_FRAMES: u32 = FPS * 10;
/// The GPU layer can't be asked for keyframes, so it sends one this often.
const GPU_KEYFRAME_SECS: u32 = 2;

/// What the sharer picked.
#[derive(Clone, Copy, Debug)]
pub struct ShareOptions {
    /// Biggest picture height (the width follows the screen).
    pub max_height: u32,
    pub fps: u32,
    /// Encode the best layer on the graphics card when there's a way.
    pub gpu: bool,
}

impl ShareOptions {
    fn max_size(&self) -> (usize, usize) {
        let h = self.max_height as usize;
        (h * 16 / 9, h)
    }
}

/// Pixel order of a captured frame (the fourth byte is ignored).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Order {
    Bgra,
    Rgba,
}

/// One captured frame.
pub struct Raw {
    pub width: usize,
    pub height: usize,
    /// Bytes per row.
    pub stride: usize,
    pub order: Order,
    pub data: Vec<u8>,
}

/// The newest frame from a capture thread; older ones are dropped.
#[derive(Default)]
pub struct Latest {
    frame: Mutex<(Option<Raw>, bool)>,
    ready: Condvar,
}

impl Latest {
    pub fn put(&self, frame: Raw) {
        self.frame.lock().unwrap().0 = Some(frame);
        self.ready.notify_one();
    }

    /// The capture ended (the desktop stopped it, or it failed).
    pub fn end(&self) {
        self.frame.lock().unwrap().1 = true;
        self.ready.notify_one();
    }

    /// A new frame, None after `timeout`, or Err once the capture ended.
    fn take(&self, timeout: Duration) -> Result<Option<Raw>, ()> {
        let guard = self.frame.lock().unwrap();
        let (mut guard, _) =
            self.ready.wait_timeout_while(guard, timeout, |(f, ended)| f.is_none() && !*ended).unwrap();
        match guard.0.take() {
            Some(f) => Ok(Some(f)),
            None if guard.1 => Err(()),
            None => Ok(None),
        }
    }
}

/// Samples per channel in a 20 ms sound packet.
pub const SOUND_FRAME: usize = 960;

/// Stereo Opus for the sound of a shared screen.
pub struct SoundEncoder(audiopus::coder::Encoder, Vec<u8>);

impl SoundEncoder {
    pub fn new() -> Result<Self> {
        use audiopus::{Application, Bitrate, Channels, SampleRate};
        let mut e = audiopus::coder::Encoder::new(SampleRate::Hz48000, Channels::Stereo, Application::Audio)?;
        e.set_bitrate(Bitrate::BitsPerSecond(128_000))?;
        Ok(Self(e, vec![0; 4000]))
    }

    /// One 20 ms frame, interleaved stereo.
    pub fn encode(&mut self, frame: &[f32]) -> Option<Vec<u8>> {
        let n = self.0.encode_float(frame, &mut self.1).ok()?;
        Some(self.1[..n].to_vec())
    }
}

/// From the share thread to the session.
pub enum ShareMsg {
    /// The first frame arrived: the layers we'll send.
    Started(StreamInfo),
    Packet(Packet),
    /// Why sharing stopped.
    Failed(String),
}

type Keyframes = [AtomicBool; MAX_LAYERS];

/// Our screen being shared; dropping it stops.
pub struct Sharing {
    stop: Arc<AtomicBool>,
    keyframes: Arc<Keyframes>,
}

impl Sharing {
    /// Ask the user what to share, then send packets to `out` until stopped.
    pub fn start(options: ShareOptions, out: tokio::sync::mpsc::Sender<ShareMsg>) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let keyframes: Arc<Keyframes> = Arc::new(Default::default());
        let (s, k) = (stop.clone(), keyframes.clone());
        std::thread::Builder::new()
            .name("screen-share".into())
            .spawn(move || {
                if let Err(e) = share(options, &s, &k, &out) {
                    let _ = out.blocking_send(ShareMsg::Failed(format!("{e:#}")));
                }
            })
            .expect("spawn screen share thread");
        Self { stop, keyframes }
    }

    /// Someone needs a full picture on this layer.
    pub fn keyframe(&self, layer: u8) {
        if let Some(k) = self.keyframes.get(layer as usize) {
            k.store(true, Ordering::Relaxed);
        }
    }
}

impl Drop for Sharing {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// The sound of the computer, if we can get it.
fn sound(out: &tokio::sync::mpsc::Sender<ShareMsg>) -> Option<sound::Sound> {
    let out = out.clone();
    let packets = move |data| {
        let _ = out.try_send(ShareMsg::Packet(Packet { track: AUDIO, keyframe: false, data }));
    };
    match sound::Sound::start(packets) {
        Ok(s) => Some(s),
        Err(e) => {
            tracing::warn!("screen share without sound: {e:#}");
            None
        }
    }
}

fn share(
    options: ShareOptions,
    stop: &AtomicBool,
    keyframes: &Keyframes,
    out: &tokio::sync::mpsc::Sender<ShareMsg>,
) -> Result<()> {
    // WISP_TEST_SCREEN=WxH shares a moving test picture instead of the screen
    if let Some((w, h)) = std::env::var("WISP_TEST_SCREEN").ok().and_then(|v| {
        let (w, h) = v.split_once('x')?;
        Some((w.parse().ok()?, h.parse().ok()?))
    }) {
        let latest = Arc::new(Latest::default());
        let (l, stopped) = (latest.clone(), Arc::new(AtomicBool::new(false)));
        let s = stopped.clone();
        let fps = options.fps;
        std::thread::spawn(move || test_pattern(w, h, fps, &l, &s));
        let sound = sound(out);
        let result = encode_loop(options, &latest, stop, keyframes, out, sound.is_some());
        stopped.store(true, Ordering::Relaxed);
        return result;
    }
    let capture = capture::Capture::open(options.fps)?;
    let sound = sound(out);
    let result = encode_loop(options, &capture.latest, stop, keyframes, out, sound.is_some());
    drop(sound);
    capture.close();
    result
}

/// Bars that move, `fps` frames a second.
fn test_pattern(width: usize, height: usize, fps: u32, latest: &Latest, stop: &AtomicBool) {
    let mut t = 0usize;
    // two pictures drawn once, then shifted: drawing each frame costs too much at 4K
    let draw = |shift: usize| {
        let mut data = vec![0u8; width * height * 4];
        for (i, px) in data.chunks_exact_mut(4).enumerate() {
            let (x, y) = (i % width, i / width);
            let band = ((x + shift) / 64) % 4;
            let c = [[255, 80, 80], [80, 255, 80], [80, 80, 255], [240, 240, 240]][band];
            let shade = (y * 255 / height) as u8;
            px.copy_from_slice(&[c[2].min(shade.saturating_add(60)), c[1], c[0], 255]);
        }
        data
    };
    let base = draw(0);
    while !stop.load(Ordering::Relaxed) {
        // moving bars: the pattern repeats every 256 pixels
        let shift = (t * 8) % 256;
        let mut data = vec![0u8; width * height * 4];
        for y in 0..height {
            let row = &base[y * width * 4..(y + 1) * width * 4];
            let dst = &mut data[y * width * 4..(y + 1) * width * 4];
            let cut = (width - (shift % width)) * 4;
            dst[..cut].copy_from_slice(&row[(shift % width) * 4..]);
            dst[cut..].copy_from_slice(&row[..(shift % width) * 4]);
        }
        latest.put(Raw { width, height, stride: width * 4, order: Order::Bgra, data });
        t += 1;
        std::thread::sleep(Duration::from_secs(1) / fps);
    }
    latest.end();
}

/// A layer encoded on the CPU: its size and encoder.
struct CpuLayer {
    width: usize,
    height: usize,
    encoder: Encoder,
    since_key: u32,
}

/// The best layer on the graphics card.
struct GpuLayer {
    gpu: gpu::Gpu,
    width: usize,
    height: usize,
    encoder: Option<gpu::GpuEncoder>,
}

fn encode_loop(
    options: ShareOptions,
    latest: &Latest,
    stop: &AtomicBool,
    keyframes: &Keyframes,
    out: &tokio::sync::mpsc::Sender<ShareMsg>,
    audio: bool,
) -> Result<()> {
    let fps = options.fps.clamp(1, 120);
    let gpu = if options.gpu { gpu::detect() } else { None };
    let mut top: Option<GpuLayer> = None;
    let mut layers: Vec<CpuLayer> = Vec::new();
    let mut started = false;
    let mut last: Option<Raw> = None;
    let frame_time = Duration::from_secs(1) / fps;
    let mut next = Instant::now();
    let mut frame_no: u64 = 0;
    while !stop.load(Ordering::Relaxed) && !out.is_closed() {
        let fresh = match latest.take(Duration::from_millis(200)) {
            Ok(f) => f,
            Err(()) => break,
        };
        let want: Vec<bool> = keyframes.iter().map(|k| k.swap(false, Ordering::Relaxed)).collect();
        // a still screen sends nothing, unless someone needs a keyframe
        let is_fresh = fresh.is_some();
        let frame = match fresh {
            Some(f) => last.insert(f),
            None if !want.contains(&true) => continue,
            None => match last.as_mut() {
                Some(f) => f,
                None => {
                    for (k, w) in keyframes.iter().zip(&want) {
                        k.fetch_or(*w, Ordering::Relaxed);
                    }
                    continue;
                }
            },
        };
        // the layers are fixed by the first frame; later sizes are letterboxed
        if !started {
            started = true;
            let sizes = ladder(frame.width, frame.height, options.max_size());
            let mut info = Vec::new();
            let mut cpu_sizes = &sizes[..];
            if let Some(g) = &gpu {
                let (width, height) = sizes[0];
                top = Some(GpuLayer { gpu: g.clone(), width, height, encoder: None });
                info.push(Layer { width: width as u16, height: height as u16, fps: fps as u8, bitrate: gpu_bitrate(width, height, fps) });
                cpu_sizes = &sizes[1..];
            }
            let cpu_fps = if gpu.is_some() { fps.min(FPS) } else { fps };
            for &(width, height) in cpu_sizes {
                let encoder = new_encoder(width, height, cpu_fps)?;
                layers.push(CpuLayer { width, height, encoder, since_key: 0 });
                info.push(Layer {
                    width: width as u16,
                    height: height as u16,
                    fps: cpu_fps as u8,
                    bitrate: bitrate(width, height, cpu_fps),
                });
            }
            let info = StreamInfo { layers: info, audio, viewers: 0 };
            if out.blocking_send(ShareMsg::Started(info)).is_err() {
                break;
            }
        }
        // the card takes every new frame; it keeps its own keyframe rhythm
        if let Some(t) = &mut top
            && is_fresh
        {
            let input = (frame.width, frame.height, frame.order);
            if t.encoder.as_ref().is_none_or(|e| e.input != input) {
                let out = out.clone();
                let packets = move |keyframe, data| {
                    let _ = out.blocking_send(ShareMsg::Packet(Packet { track: 0, keyframe, data }));
                };
                let rate = gpu_bitrate(t.width, t.height, fps);
                // drop the old one first: two encoders on one card can be one too many
                t.encoder = None;
                let enc = gpu::GpuEncoder::start(&t.gpu, input, (t.width, t.height), fps, rate, fps * GPU_KEYFRAME_SECS, packets)
                    .with_context(|| format!("starting the {} encoder", t.gpu.label()))?;
                t.encoder = Some(enc);
            }
            if !t.encoder.as_mut().unwrap().push(frame) {
                anyhow::bail!("the {} encoder stopped", t.gpu.label());
            }
        }
        // the CPU layers run at 30 at most
        let cpu_every = if top.is_some() { (fps / FPS).max(1) as u64 } else { 1 };
        let base = if top.is_some() { 1 } else { 0 };
        let cpu_turn = frame_no % cpu_every == 0 || want[base..].contains(&true);
        frame_no += 1;
        if cpu_turn {
            let mut picture: Option<convert::Rgb> = None;
            for (i, l) in layers.iter_mut().enumerate() {
                let track = base + i;
                // each layer from the one above (cheaper than from the screen)
                let rgb = match picture.take() {
                    None => convert::Rgb::from_raw(frame, l.width, l.height),
                    Some(above) => above.scaled(l.width, l.height),
                };
                if want[track] || l.since_key >= KEYFRAME_FRAMES {
                    l.encoder.force_intra_frame();
                }
                let yuv = YUVBuffer::from_vec(rgb.to_i420(), l.width, l.height);
                picture = Some(rgb);
                let bits = l.encoder.encode(&yuv).context("encoding the screen")?;
                let keyframe = matches!(bits.frame_type(), FrameType::IDR | FrameType::I);
                let data = bits.to_vec();
                l.since_key = if keyframe { 0 } else { l.since_key + 1 };
                if data.is_empty() {
                    continue;
                }
                let packet = Packet { track: track as u8, keyframe, data };
                if out.blocking_send(ShareMsg::Packet(packet)).is_err() {
                    return Ok(());
                }
            }
        }
        // keep to the frame rate
        next += frame_time;
        let now = Instant::now();
        if next > now {
            std::thread::sleep(next - now);
        } else {
            next = now;
        }
    }
    Ok(())
}

/// The layers for a screen: the screen itself (scaled down to fit `max`),
/// then 720p and 360p versions when they're clearly smaller.
fn ladder(width: usize, height: usize, max: (usize, usize)) -> Vec<(usize, usize)> {
    let even = |v: f64| ((v.round() as usize) & !1).max(16);
    let scale = (max.0 as f64 / width as f64).min(max.1 as f64 / height as f64).min(1.0);
    let (tw, th) = (width as f64 * scale, height as f64 * scale);
    let mut out = vec![(even(tw), even(th))];
    for h in [720.0, 360.0] {
        let (_, last_h) = *out.last().unwrap();
        if last_h as f64 / h >= 1.4 {
            out.push((even(tw * h / th), even(h)));
        }
    }
    out
}

/// What a CPU layer aims for: about 4 Mbit/s at 1080p30.
fn bitrate(width: usize, height: usize, fps: u32) -> u32 {
    ((width * height) as f64 * fps as f64 * 0.07).clamp(300_000.0, 8_000_000.0) as u32
}

/// The card does better per bit, and big pictures need less per pixel:
/// about 5 Mbit/s at 1080p30, 9 at 1080p60, 30 at 4K60.
fn gpu_bitrate(width: usize, height: usize, fps: u32) -> u32 {
    let pixels = (width * height) as f64 * fps as f64;
    (pixels.powf(0.85) * 1.2).clamp(1_000_000.0, 40_000_000.0) as u32
}

fn new_encoder(width: usize, height: usize, fps: u32) -> Result<Encoder> {
    let config = EncoderConfig::new()
        .bitrate(BitRate::from_bps(bitrate(width, height, fps)))
        .max_frame_rate(FrameRate::from_hz(fps as f32))
        .usage_type(UsageType::ScreenContentRealTime)
        .intra_frame_period(IntraFramePeriod::from_num_frames(KEYFRAME_FRAMES))
        .adaptive_quantization(false)
        .background_detection(false)
        .skip_frames(true);
    Encoder::with_api_config(OpenH264API::from_source(), config).context("starting the video encoder")
}

/// A decoded picture: BGRA, rows without padding.
pub struct Picture {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

/// Someone's screen as we watch it: the decoder thread puts pictures in,
/// the viewer window takes them out.
#[derive(Clone)]
pub struct Screen(Arc<ScreenInner>);

struct ScreenInner {
    picture: Mutex<Option<Picture>>,
    /// They stopped sharing (or we can't watch any more).
    ended: AtomicBool,
    /// The decoder lost the picture; ask the sharer for a keyframe.
    want_key: AtomicBool,
    /// The layer we get (u8::MAX before the first picture).
    layer: std::sync::atomic::AtomicU8,
    wake: Mutex<fmpsc::Sender<()>>,
}

impl std::fmt::Debug for Screen {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Screen")
    }
}

impl Screen {
    /// A screen and the signal that it changed.
    pub fn new() -> (Self, fmpsc::Receiver<()>) {
        let (tx, rx) = fmpsc::channel(1);
        let inner = ScreenInner {
            picture: Mutex::new(None),
            ended: AtomicBool::new(false),
            want_key: AtomicBool::new(false),
            layer: std::sync::atomic::AtomicU8::new(u8::MAX),
            wake: Mutex::new(tx),
        };
        (Self(Arc::new(inner)), rx)
    }

    fn wake(&self) {
        let _ = self.0.wake.lock().unwrap().try_send(());
    }

    fn show(&self, picture: Picture) {
        *self.0.picture.lock().unwrap() = Some(picture);
        self.wake();
    }

    /// The newest picture, if there's one we haven't taken.
    pub fn take(&self) -> Option<Picture> {
        self.0.picture.lock().unwrap().take()
    }

    pub fn end(&self) {
        self.0.ended.store(true, Ordering::Relaxed);
        self.wake();
    }

    pub fn ended(&self) -> bool {
        self.0.ended.load(Ordering::Relaxed)
    }

    /// The layer of the stream we get, once a picture came.
    pub fn layer(&self) -> Option<u8> {
        Some(self.0.layer.load(Ordering::Relaxed)).filter(|l| *l != u8::MAX)
    }

    /// Whether the picture broke since the last call.
    pub fn wants_keyframe(&self) -> bool {
        self.0.want_key.swap(false, Ordering::Relaxed)
    }
}

/// Decode frames from `frames` onto `screen` until the sender goes away.
/// Decode video packets from `packets` onto `screen` until the sender goes away.
pub fn decode(packets: std::sync::mpsc::Receiver<Packet>, screen: Screen) {
    std::thread::Builder::new()
        .name("screen-decode".into())
        .spawn(move || {
            let mut decoder = match Decoder::new() {
                Ok(d) => d,
                Err(e) => {
                    tracing::warn!("video decoder: {e}");
                    return;
                }
            };
            let video = || loop {
                match packets.recv() {
                    Ok(p) if p.is_audio() => continue,
                    other => return other.ok(),
                }
            };
            while let Some(mut packet) = video() {
                // if we fell behind, all go through the decoder but only the last is shown
                while let Ok(newer) = packets.try_recv() {
                    if !newer.is_audio() {
                        let _ = decoder.decode(&packet.data);
                        packet = newer;
                    }
                }
                screen.0.layer.store(packet.track, Ordering::Relaxed);
                match decoder.decode(&packet.data) {
                    Ok(Some(yuv)) => {
                        use openh264::formats::YUVSource;
                        let (w, h) = yuv.dimensions();
                        let mut bgra = vec![0; w * h * 4];
                        yuv.write_rgba8(&mut bgra);
                        for px in bgra.chunks_exact_mut(4) {
                            px.swap(0, 2);
                        }
                        screen.show(Picture { width: w as u32, height: h as u32, bgra });
                    }
                    Ok(None) => {}
                    Err(e) => {
                        tracing::debug!("video decode: {e}");
                        screen.0.want_key.store(true, Ordering::Relaxed);
                    }
                }
            }
        })
        .expect("spawn screen decode thread");
}

#[cfg(test)]
mod tests {
    use super::*;

    const HD: (usize, usize) = (1920, 1080);

    #[test]
    fn layers() {
        assert_eq!(ladder(3840, 2160, (3840, 2160)), [(3840, 2160), (1280, 720), (640, 360)]);
        assert_eq!(ladder(1920, 1080, HD), [(1920, 1080), (1280, 720), (640, 360)]);
        assert_eq!(ladder(3840, 2160, HD), [(1920, 1080), (1280, 720), (640, 360)]);
        assert_eq!(ladder(1001, 701, HD), [(1000, 700), (514, 360)]);
        assert_eq!(ladder(3440, 1440, HD), [(1920, 804), (860, 360)]);
        assert_eq!(ladder(800, 450, HD), [(800, 450)]);
    }

    /// A frame survives encoding and decoding (roughly: it's lossy).
    #[test]
    fn round_trip() {
        let (w, h) = (64, 48);
        let mut data = vec![0u8; w * h * 4];
        for (i, px) in data.chunks_exact_mut(4).enumerate() {
            let x = i % w;
            px.copy_from_slice(if x < w / 2 { &[255, 0, 0, 255] } else { &[0, 0, 255, 255] });
        }
        let raw = Raw { width: w, height: h, stride: w * 4, order: Order::Bgra, data };
        let mut enc = new_encoder(w, h, FPS).unwrap();
        let yuv = YUVBuffer::from_vec(convert::Rgb::from_raw(&raw, w, h).to_i420(), w, h);
        let bits = enc.encode(&yuv).unwrap();
        assert!(matches!(bits.frame_type(), FrameType::IDR | FrameType::I));
        let data = bits.to_vec();

        let (screen, _wake) = Screen::new();
        let (tx, rx) = std::sync::mpsc::channel();
        decode(rx, screen.clone());
        tx.send(Packet { track: 0, keyframe: true, data }).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let pic = loop {
            if let Some(p) = screen.take() {
                break p;
            }
            assert!(Instant::now() < deadline, "no picture");
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!((pic.width, pic.height), (w as u32, h as u32));
        // BGRA: left blue, right red
        let left = &pic.bgra[(h / 2 * w + 4) * 4..][..3];
        let right = &pic.bgra[(h / 2 * w + w - 4) * 4..][..3];
        assert!(left[0] > 200 && left[2] < 60, "{left:?}");
        assert!(right[2] > 200 && right[0] < 60, "{right:?}");
    }
}
