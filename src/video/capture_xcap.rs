//! Screens on Windows and macOS, through xcap.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result};
use xcap::{Monitor, VideoRecorder};

use super::{Latest, Order, Pick, Raw, ScreenChoice};

/// Every screen, the main one first.
pub fn screens() -> Vec<ScreenChoice> {
    let mut monitors = Monitor::all().unwrap_or_default();
    monitors.sort_by_key(|m| !m.is_primary().unwrap_or(false));
    monitors
        .iter()
        .enumerate()
        .filter_map(|(i, m)| {
            let id = m.id().ok()?;
            let name = m.friendly_name().or_else(|_| m.name()).unwrap_or_default();
            let name = if name.trim().is_empty() { format!("Screen {}", i + 1) } else { name };
            let primary = if m.is_primary().unwrap_or(false) { " (main)" } else { "" };
            Some(ScreenChoice { id, label: format!("{name}{primary}") })
        })
        .collect()
}

pub struct Capture {
    pub latest: Arc<Latest>,
    recorder: VideoRecorder,
    stop: Arc<AtomicBool>,
}

impl Capture {
    pub fn open(pick: Pick) -> Result<Self> {
        let monitors = Monitor::all().context("couldn't list the screens")?;
        let monitor = match pick {
            Pick::Screen(id) => monitors.iter().find(|m| m.id().ok() == Some(id)),
            Pick::Default => None,
        }
        .or_else(|| monitors.iter().find(|m| m.is_primary().unwrap_or(false)))
        .or(monitors.first())
        .context("no screen to share")?;
        let latest = Arc::new(Latest::default());
        if let Ok(hz) = monitor.frequency()
            && hz >= 1.0
        {
            latest.set_refresh(hz.round() as u32);
        }
        let (recorder, frames) = monitor.video_recorder().context("couldn't record the screen")?;
        recorder.start().context("couldn't record the screen")?;
        let stop = Arc::new(AtomicBool::new(false));
        let (l, s) = (latest.clone(), stop.clone());
        std::thread::Builder::new().name("screen-capture".into()).spawn(move || {
            while let Ok(f) = frames.recv() {
                if s.load(Ordering::Relaxed) {
                    break;
                }
                if !l.due() {
                    continue;
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
