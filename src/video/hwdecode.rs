//! H.264 decoded on the graphics card by ffmpeg (`-hwaccel auto` picks
//! NVDEC, VA-API, VideoToolbox or D3D11): Annex B access units go in on
//! stdin, BGRA frames of a size we know come out on stdout. Used for the
//! big layers, where decoding on the CPU costs the most.

use std::io::{Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::OnceLock;
use std::thread::JoinHandle;

use super::{Picture, Screen};

/// From this height up, the graphics card decodes.
pub const MIN_HEIGHT: u16 = 1080;

pub struct HwDecoder {
    child: Child,
    stdin: Option<ChildStdin>,
    reader: Option<JoinHandle<()>>,
    pub size: (u16, u16),
}

/// Whether ffmpeg can decode H.264 here at all (found once).
pub fn available() -> bool {
    static OK: OnceLock<bool> = OnceLock::new();
    *OK.get_or_init(|| {
        // WISP_NO_HWDECODE=1 decodes everything on the CPU
        if std::env::var_os("WISP_NO_HWDECODE").is_some() {
            return false;
        }
        let Some(ffmpeg) = super::gpu::ffmpeg() else { return false };
        let mut cmd = Command::new(ffmpeg);
        cmd.args(["-hide_banner", "-loglevel", "error", "-decoders"]);
        super::gpu::quiet(&mut cmd);
        cmd.output().is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains(" h264 "))
    })
}

impl HwDecoder {
    /// Start one for a layer of `size`; its pictures go to `screen`.
    pub fn start(size: (u16, u16), screen: Screen) -> Option<Self> {
        let ffmpeg = super::gpu::ffmpeg()?;
        let (w, h) = (size.0 as usize, size.1 as usize);
        let mut cmd = Command::new(ffmpeg);
        cmd.args(["-hide_banner", "-loglevel", "error", "-hwaccel", "auto"]);
        // no buffering or probing: a frame out for every frame in
        cmd.args(["-fflags", "nobuffer", "-flags", "low_delay", "-probesize", "32", "-analyzeduration", "0"]);
        cmd.args(["-f", "h264", "-i", "-"]);
        // the size we split the output by, whatever the stream says
        cmd.args(["-vf", &format!("scale={w}:{h}"), "-pix_fmt", "bgra", "-f", "rawvideo", "-"]);
        super::gpu::quiet(&mut cmd);
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("hardware decoder: {e}");
                return None;
            }
        };
        let stdin = child.stdin.take();
        let mut stdout = child.stdout.take()?;
        let reader = std::thread::Builder::new()
            .name("screen-hwdecode".into())
            .spawn(move || {
                let mut frame = vec![0u8; w * h * 4];
                while stdout.read_exact(&mut frame).is_ok() {
                    screen.show(Picture { width: w as u32, height: h as u32, bgra: frame.clone() });
                }
            })
            .ok()?;
        Some(Self { child, stdin, reader: Some(reader), size })
    }

    /// Feed one access unit; false once ffmpeg is gone.
    pub fn push(&mut self, data: &[u8]) -> bool {
        self.stdin.as_mut().is_some_and(|s| s.write_all(data).and_then(|_| s.flush()).is_ok())
    }
}

impl Drop for HwDecoder {
    fn drop(&mut self) {
        // closing stdin ends ffmpeg; kill in case it hangs
        self.stdin = None;
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(r) = self.reader.take() {
            let _ = r.join();
        }
    }
}
