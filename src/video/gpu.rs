//! H.264 on the graphics card, through an `ffmpeg` process: raw frames go
//! in on stdin, the stream comes out on stdout. ffmpeg knows every vendor's
//! encoder (NVENC, AMF, Quick Sync, VA-API, Media Foundation,
//! VideoToolbox), so we don't link any of them. Keyframes come at a fixed
//! interval, since a running ffmpeg can't be asked for one.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::OnceLock;
use std::sync::mpsc::{SyncSender, TrySendError, sync_channel};
use std::thread::JoinHandle;

use anyhow::{Context, Result};

use super::{Order, Raw};

/// A working encoder: which one, and the extra arguments it needs.
#[derive(Clone, Debug)]
pub struct Gpu {
    pub ffmpeg: PathBuf,
    pub codec: &'static str,
    /// Before the input (devices).
    pre: Vec<String>,
    /// Filters to get frames onto the card, if it needs that.
    filter: Option<&'static str>,
    /// Low-latency settings for this encoder.
    tuning: &'static [&'static str],
}

impl Gpu {
    /// A name for people: "NVIDIA (NVENC)".
    pub fn label(&self) -> &'static str {
        match self.codec {
            "h264_nvenc" => "NVIDIA (NVENC)",
            "h264_amf" => "AMD (AMF)",
            "h264_qsv" => "Intel (Quick Sync)",
            "h264_vaapi" => "VA-API",
            "h264_mf" => "Media Foundation",
            "h264_videotoolbox" => "VideoToolbox",
            other => other,
        }
    }
}

/// The ffmpeg next to Wisp's config or on the PATH.
fn ffmpeg() -> Option<PathBuf> {
    let exe = if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" };
    let local = crate::config::config_dir().join(exe);
    if local.is_file() {
        return Some(local);
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(exe)).find(|p| p.is_file())
}

/// The first GPU encoder that really works here, found once (each try is a
/// tiny encode). None: no ffmpeg or no usable encoder.
pub fn detect() -> Option<Gpu> {
    static FOUND: OnceLock<Option<Gpu>> = OnceLock::new();
    FOUND
        .get_or_init(|| {
            let ffmpeg = ffmpeg()?;
            let found = candidates(&ffmpeg).into_iter().find(|g| works(g));
            match &found {
                Some(g) => tracing::info!("screen sharing encodes with {}", g.codec),
                None => tracing::info!("no GPU encoder in {}", ffmpeg.display()),
            }
            found
        })
        .clone()
}

fn candidates(ffmpeg: &std::path::Path) -> Vec<Gpu> {
    let gpu = |codec, pre: Vec<String>, filter, tuning| Gpu { ffmpeg: ffmpeg.to_path_buf(), codec, pre, filter, tuning };
    const NVENC: &[&str] = &["-preset", "p4", "-tune", "ll", "-rc", "cbr", "-zerolatency", "1", "-delay", "0"];
    const AMF: &[&str] = &["-usage", "lowlatency", "-rc", "cbr"];
    const QSV: &[&str] = &["-preset", "veryfast", "-look_ahead", "0", "-async_depth", "1"];
    const VAAPI: &[&str] = &["-rc_mode", "CBR", "-async_depth", "1"];
    const MF: &[&str] = &["-rate_control", "cbr", "-scenario", "display_remoting"];
    const VT: &[&str] = &["-realtime", "1", "-prio_speed", "1"];
    let mut out = vec![gpu("h264_nvenc", vec![], None, NVENC)];
    if cfg!(target_os = "linux") {
        // each render node is a card; the first may not encode
        for node in 128..136 {
            let dev = format!("/dev/dri/renderD{node}");
            if std::path::Path::new(&dev).exists() {
                let pre = vec!["-vaapi_device".into(), dev];
                out.push(gpu("h264_vaapi", pre, Some("format=nv12,hwupload"), VAAPI));
            }
        }
        out.push(gpu("h264_qsv", vec![], None, QSV));
    }
    if cfg!(windows) {
        out.push(gpu("h264_amf", vec![], None, AMF));
        out.push(gpu("h264_qsv", vec![], None, QSV));
        out.push(gpu("h264_mf", vec![], None, MF));
    }
    if cfg!(target_os = "macos") {
        out.push(gpu("h264_videotoolbox", vec![], None, VT));
    }
    out
}

/// Encode a tenth of a second of black with it.
fn works(g: &Gpu) -> bool {
    let mut cmd = Command::new(&g.ffmpeg);
    cmd.args(["-hide_banner", "-loglevel", "error"]).args(&g.pre);
    cmd.args(["-f", "lavfi", "-i", "color=black:s=640x360:r=30:d=0.1"]);
    if let Some(f) = g.filter {
        cmd.args(["-vf", f]);
    }
    cmd.args(["-c:v", g.codec]).args(g.tuning).args(["-f", "null", "-"]);
    quiet(&mut cmd);
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    cmd.status().is_ok_and(|s| s.success())
}

/// No console window flashing up on Windows.
fn quiet(_cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        _cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
}

/// A running encoder for one layer. Frames go in with `push`; packets come
/// out through `on_packet` (keyframe, Annex B access unit).
pub struct GpuEncoder {
    child: Child,
    frames: Option<SyncSender<Vec<u8>>>,
    writer: Option<JoinHandle<()>>,
    reader: Option<JoinHandle<()>>,
    /// The input size and pixel order it was started for.
    pub input: (usize, usize, Order),
}

impl GpuEncoder {
    #[allow(clippy::too_many_arguments)]
    pub fn start(
        gpu: &Gpu,
        input: (usize, usize, Order),
        size: (usize, usize),
        fps: u32,
        bitrate: u32,
        keyframe_every: u32,
        mut on_packet: impl FnMut(bool, Vec<u8>) + Send + 'static,
    ) -> Result<Self> {
        let (iw, ih, order) = input;
        let (w, h) = size;
        let pix = match order {
            Order::Bgra => "bgr0",
            Order::Rgba => "rgb0",
        };
        // fit and center, so the input size can be anything
        let mut filter = format!(
            "scale={w}:{h}:force_original_aspect_ratio=decrease:flags=area,pad={w}:{h}:(ow-iw)/2:(oh-ih)/2"
        );
        if (iw, ih) == (w, h) {
            filter.clear();
        }
        if let Some(f) = gpu.filter {
            filter = if filter.is_empty() { f.to_string() } else { format!("{filter},{f}") };
        }
        let mut cmd = Command::new(&gpu.ffmpeg);
        cmd.args(["-hide_banner", "-loglevel", "error", "-nostdin"]).args(&gpu.pre);
        cmd.args(["-f", "rawvideo", "-pix_fmt", pix, "-s", &format!("{iw}x{ih}"), "-r", &fps.to_string()]);
        cmd.args(["-fflags", "nobuffer", "-i", "pipe:0"]);
        if !filter.is_empty() {
            cmd.args(["-vf", &filter]);
        }
        let rate = bitrate.to_string();
        cmd.args(["-c:v", gpu.codec]).args(gpu.tuning);
        cmd.args(["-b:v", &rate, "-maxrate", &rate, "-bufsize", &(bitrate / 2).to_string()]);
        cmd.args(["-g", &keyframe_every.to_string(), "-bf", "0", "-profile:v", "high"]);
        cmd.args(["-bsf:v", "h264_metadata=aud=insert", "-flush_packets", "1", "-f", "h264", "pipe:1"]);
        quiet(&mut cmd);
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        let mut child = cmd.spawn().with_context(|| format!("starting {}", gpu.ffmpeg.display()))?;
        let stdin = child.stdin.take().context("ffmpeg stdin")?;
        let mut stdout = child.stdout.take().context("ffmpeg stdout")?;

        // a frame or two may wait; more means the card can't keep up, so skip
        let (frames, rx) = sync_channel::<Vec<u8>>(2);
        let writer = std::thread::Builder::new().name("gpu-encode-in".into()).spawn(move || {
            let mut stdin: ChildStdin = stdin;
            while let Ok(frame) = rx.recv() {
                if stdin.write_all(&frame).is_err() {
                    break;
                }
            }
        })?;
        let reader = std::thread::Builder::new().name("gpu-encode-out".into()).spawn(move || {
            let mut split = AccessUnits::default();
            let mut buf = vec![0u8; 1 << 20];
            loop {
                match stdout.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        for unit in split.push(&buf[..n]) {
                            let key = is_keyframe(&unit);
                            on_packet(key, unit);
                        }
                    }
                }
            }
            if let Some(unit) = split.finish() {
                on_packet(is_keyframe(&unit), unit);
            }
        })?;
        Ok(Self { child, frames: Some(frames), writer: Some(writer), reader: Some(reader), input })
    }

    /// Hand a frame over; false if the encoder is gone.
    pub fn push(&mut self, frame: &Raw) -> bool {
        let Some(tx) = &self.frames else { return false };
        let row = frame.width * 4;
        let data = if frame.stride == row {
            frame.data[..row * frame.height].to_vec()
        } else {
            let mut d = Vec::with_capacity(row * frame.height);
            for y in 0..frame.height {
                d.extend_from_slice(&frame.data[y * frame.stride..][..row]);
            }
            d
        };
        match tx.try_send(data) {
            Ok(()) | Err(TrySendError::Full(_)) => true,
            Err(TrySendError::Disconnected(_)) => false,
        }
    }
}

impl Drop for GpuEncoder {
    fn drop(&mut self) {
        // closing stdin lets ffmpeg finish; then make sure it's gone
        self.frames.take();
        if let Some(w) = self.writer.take() {
            let _ = w.join();
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            if let Ok(Some(_)) = self.child.try_wait() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(r) = self.reader.take() {
            let _ = r.join();
        }
    }
}

/// Splits an Annex B byte stream into access units at access unit
/// delimiters (NAL type 9), which ffmpeg puts in front of every frame.
#[derive(Default)]
struct AccessUnits {
    buf: Vec<u8>,
    /// Where we've looked for start codes up to.
    scanned: usize,
}

impl AccessUnits {
    fn push(&mut self, data: &[u8]) -> Vec<Vec<u8>> {
        self.buf.extend_from_slice(data);
        let mut out = Vec::new();
        let mut i = self.scanned.max(1);
        while i + 4 <= self.buf.len() {
            let b = &self.buf;
            if b[i] == 0 && b[i + 1] == 0 && b[i + 2] == 1 && b[i + 3] & 0x1f == 9 {
                // the delimiter's start code may have a fourth zero in front
                let start = if b[i - 1] == 0 { i - 1 } else { i };
                if start > 0 {
                    out.push(self.buf.drain(..start).collect());
                    i = i - start + 3;
                    continue;
                }
            }
            i += 1;
        }
        self.scanned = self.buf.len().saturating_sub(3);
        out
    }

    fn finish(&mut self) -> Option<Vec<u8>> {
        (!self.buf.is_empty()).then(|| std::mem::take(&mut self.buf))
    }
}

/// Whether an access unit holds an IDR picture (NAL type 5).
fn is_keyframe(unit: &[u8]) -> bool {
    unit.windows(4).any(|w| w[0] == 0 && w[1] == 0 && w[2] == 1 && w[3] & 0x1f == 5)
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_at_delimiters() {
        let aud = [0, 0, 0, 1, 9, 0xf0];
        let idr = [0, 0, 1, 0x65, 1, 2];
        let p = [0, 0, 1, 0x41, 3];
        let stream: Vec<u8> = [&aud[..], &idr, &aud, &p, &aud, &p].concat();
        let mut s = AccessUnits::default();
        // in awkward pieces
        let mut units = Vec::new();
        for chunk in stream.chunks(3) {
            units.extend(s.push(chunk));
        }
        units.extend(s.finish());
        assert_eq!(units.len(), 3);
        assert_eq!(units[0], [&aud[..], &idr].concat());
        assert!(is_keyframe(&units[0]) && !is_keyframe(&units[1]));
        assert_eq!(units[2], [&aud[..], &p].concat());
    }
}
