//! Screens, windows and apps on macOS through ScreenCaptureKit. From macOS 14
//! the system picker asks what to share (like the portal on Linux); before
//! that Wisp lists the screens itself. The stream brings the sound too,
//! without Wisp's own.

use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow, bail};
use apple_cf::cg::Display;
use screencapturekit::content_sharing_picker::{
    SCContentSharingPicker, SCContentSharingPickerConfiguration, SCContentSharingPickerMode, SCPickedSource,
    SCPickerOutcome,
};
use screencapturekit::cv::CVPixelBufferLockFlags;
use screencapturekit::prelude::*;

use super::{Latest, Order, Pick, Raw, ScreenChoice};

/// Interleaved 48 kHz stereo from the stream, while a share wants it.
pub type SoundTap = Box<dyn FnMut(&[f32]) + Send>;
static SOUND: Mutex<Option<SoundTap>> = Mutex::new(None);

pub fn set_sound(tap: Option<SoundTap>) {
    *SOUND.lock().unwrap() = tap;
}

/// The screens, main first; empty when the system picker asks instead.
pub fn screens() -> Vec<ScreenChoice> {
    if SCContentSharingPicker::is_available() {
        return Vec::new();
    }
    let Ok(content) = SCShareableContent::get() else { return Vec::new() };
    let main = Display::main().id();
    let mut displays = content.displays();
    displays.sort_by_key(|d| d.display_id() != main);
    displays
        .iter()
        .enumerate()
        .map(|(i, d)| {
            let id = d.display_id();
            let (w, h) = pixel_size(id).unwrap_or((d.width(), d.height()));
            let main = if id == main { ", main" } else { "" };
            ScreenChoice { id, label: format!("Screen {} ({w} × {h}{main})", i + 1) }
        })
        .collect()
}

pub struct Capture {
    pub latest: Arc<Latest>,
    stream: SCStream,
}

impl Capture {
    pub fn open(pick: Pick) -> Result<Self> {
        let (filter, (width, height), display) = match pick {
            Pick::Default if SCContentSharingPicker::is_available() => ask()?,
            _ => screen(pick)?,
        };
        let latest = Arc::new(Latest::default());
        let refresh = Display::new(display.unwrap_or_else(|| Display::main().id())).mode().map(|m| m.refresh_rate());
        if let Some(hz) = refresh
            && hz >= 1.0
        {
            latest.set_refresh(hz.round() as u32);
        }
        let config = SCStreamConfiguration::new()
            .with_width(width)
            .with_height(height)
            .with_pixel_format(PixelFormat::BGRA)
            .with_shows_cursor(true)
            // as fast as the screen goes; Latest drops what the share doesn't need
            .with_minimum_frame_interval(&CMTime::new(1, 240))
            .with_queue_depth(5)
            .with_captures_audio(true)
            .with_excludes_current_process_audio(true)
            .with_sample_rate(48000)
            .with_channel_count(2);
        let mut stream = SCStream::new(&filter, &config).map_err(|e| anyhow!("{e}"))?;
        let l = latest.clone();
        stream
            .add_output_handler(move |sample: CMSampleBuffer, _| frame(&l, &sample), SCStreamOutputType::Screen)
            .map_err(|e| anyhow!("{e}"))?;
        stream
            .add_output_handler(|sample: CMSampleBuffer, _| sound(&sample), SCStreamOutputType::Audio)
            .map_err(|e| anyhow!("{e}"))?;
        stream.start_capture().map_err(|e| anyhow!("couldn't record the screen: {e}"))?;
        Ok(Self { latest, stream })
    }

    pub fn close(self) {
        let _ = self.stream.stop_capture();
        self.latest.end();
    }
}

/// The system picker (macOS 14+): a screen, a window or an app.
fn ask() -> Result<(SCContentFilter, (u32, u32), Option<u32>)> {
    let mut config = SCContentSharingPickerConfiguration::new().map_err(|e| anyhow!("{e}"))?;
    config.set_allowed_picker_modes(&[
        SCContentSharingPickerMode::SingleDisplay,
        SCContentSharingPickerMode::SingleWindow,
        SCContentSharingPickerMode::SingleApplication,
    ]);
    // sharing Wisp itself only shows the share inside the share
    let _ = config.set_excluded_bundle_ids(&["at.timkunze.wisp"]);
    let (tx, rx) = std::sync::mpsc::channel();
    SCContentSharingPicker::show(&config, move |outcome| {
        let _ = tx.send(outcome);
    });
    match rx.recv().context("the picker went away")? {
        SCPickerOutcome::Picked(r) => {
            let display = match r.source() {
                SCPickedSource::Display(id) => Some(id),
                _ => None,
            };
            let (w, h) = r.pixel_size();
            tracing::info!(
                "picked {:?}: {w}x{h} at {:?}, {} displays, {} windows, {} apps",
                r.source(),
                r.rect(),
                r.displays().len(),
                r.windows().len(),
                r.applications().len()
            );
            if w == 0 || h == 0 {
                bail!("nothing was picked to share");
            }
            Ok((r.filter(), (w & !1, h & !1), display))
        }
        SCPickerOutcome::Cancelled => bail!("Screen sharing cancelled"),
        SCPickerOutcome::Error(e) => bail!("{e}"),
    }
}

/// A whole screen, from Wisp's own list (before macOS 14).
fn screen(pick: Pick) -> Result<(SCContentFilter, (u32, u32), Option<u32>)> {
    let content = SCShareableContent::get().map_err(|e| anyhow!("couldn't list the screens: {e}"))?;
    let displays = content.displays();
    let main = Display::main().id();
    let display = match pick {
        Pick::Screen(id) => displays.iter().find(|d| d.display_id() == id),
        Pick::Default => None,
    }
    .or_else(|| displays.iter().find(|d| d.display_id() == main))
    .or(displays.first())
    .context("no screen to share")?;
    let filter =
        SCContentFilter::create().with_display(display).with_excluding_windows(&[]).build().map_err(|e| anyhow!("{e}"))?;
    let id = display.display_id();
    let (w, h) = pixel_size(id).unwrap_or((display.width(), display.height()));
    Ok((filter, (w & !1, h & !1), Some(id)))
}

/// A screen's size in pixels (its width() and height() are in points).
fn pixel_size(id: u32) -> Option<(u32, u32)> {
    let mode = Display::new(id).mode()?;
    Some((mode.pixel_width() as u32, mode.pixel_height() as u32))
}

fn frame(latest: &Latest, sample: &CMSampleBuffer) {
    // frames without a picture just say nothing changed
    let Some(buffer) = sample.pixel_buffer() else { return };
    if !latest.due() {
        return;
    }
    let Ok(guard) = buffer.lock(CVPixelBufferLockFlags::READ_ONLY) else { return };
    let (width, height, stride) = (guard.width(), guard.height(), guard.bytes_per_row());
    let Some(bytes) = (unsafe { guard.as_slice() }) else { return };
    let need = stride * height.saturating_sub(1) + width * 4;
    if width == 0 || bytes.len() < need {
        return;
    }
    latest.put(Raw { width, height, stride, order: Order::Bgra, data: bytes[..need].to_vec() });
}

fn sound(sample: &CMSampleBuffer) {
    let mut tap = SOUND.lock().unwrap();
    let Some(tap) = tap.as_mut() else { return };
    let Ok(list) = sample.audio_buffer_list() else { return };
    let floats = |b: &[u8]| b.chunks_exact(4).map(|c| f32::from_ne_bytes(c.try_into().unwrap())).collect::<Vec<f32>>();
    let buffers: Vec<_> = (0..list.num_buffers()).filter_map(|i| list.get(i)).collect();
    // usually one buffer per channel; sometimes one interleaved buffer
    let stereo: Vec<f32> = match buffers.as_slice() {
        [left, right, ..] => {
            let (l, r) = (floats(left.data()), floats(right.data()));
            l.iter().zip(&r).flat_map(|(a, b)| [*a, *b]).collect()
        }
        [one] if one.number_channels >= 2 => {
            let n = one.number_channels as usize;
            floats(one.data()).chunks_exact(n).flat_map(|c| [c[0], c[1]]).collect()
        }
        [mono] => floats(mono.data()).into_iter().flat_map(|s| [s, s]).collect(),
        [] => return,
    };
    tap(&stereo);
}
