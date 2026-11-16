//! The main screen on Windows and macOS, through xcap.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result};
use xcap::{Monitor, VideoRecorder};

use super::{Latest, Order, Raw};

pub struct Capture {
    pub latest: Arc<Latest>,
    recorder: VideoRecorder,
    stop: Arc<AtomicBool>,
}

impl Capture {
    pub fn open() -> Result<Self> {
        let monitors = Monitor::all().context("couldn't list the screens")?;
        let monitor = monitors
            .iter()
            .find(|m| m.is_primary().unwrap_or(false))
            .or(monitors.first())
            .context("no screen to share")?;
        let (recorder, frames) = monitor.video_recorder().context("couldn't record the screen")?;
        recorder.start().context("couldn't record the screen")?;
        let latest = Arc::new(Latest::default());
        let stop = Arc::new(AtomicBool::new(false));
        let (l, s) = (latest.clone(), stop.clone());
        std::thread::Builder::new().name("screen-capture".into()).spawn(move || {
            while let Ok(f) = frames.recv() {
                if s.load(Ordering::Relaxed) {
                    break;
                }
                let (width, height) = (f.width as usize, f.height as usize);
                l.put(Raw { width, height, stride: width * 4, order: Order::Rgba, data: f.raw });
            }
            l.end();
        })?;
        Ok(Self { latest, recorder, stop })
    }

    pub fn close(self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.recorder.stop();
    }
}
