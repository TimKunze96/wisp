//! Capture and playback. Both streams stay open the whole time (pausing fails
//! on PipeWire's ALSA plugin); what gets sent is decided by [`Gate`].

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use tokio::task::LocalSet;

use audio_to_ts::AudioToTs;
use ts_to_audio::TsToAudio;

pub mod audio_to_ts;
#[cfg(not(target_os = "linux"))]
mod devices;
#[cfg(target_os = "linux")]
mod pipewire;
pub mod record;
pub mod ts_to_audio;

#[cfg(not(target_os = "linux"))]
pub use devices::*;
#[cfg(target_os = "linux")]
pub use pipewire::*;

/// 48 kHz, 20 ms frames, mono.
const USUAL_FRAME_SIZE: usize = 48000 / 50;

/// The maximum size of an opus frame is 1275 as from RFC6716.
const MAX_OPUS_FRAME_SIZE: usize = 1275;

/// How long voice activation stays open after the level drops (in frames).
const VAD_HOLD_FRAMES: u32 = 20;

/// Shared between the audio callbacks and the rest of the app.
#[derive(Default)]
pub struct Gate {
    /// Push-to-talk held (or talk toggled on).
    pub ptt: AtomicBool,
    pub vad_enabled: AtomicBool,
    /// f32 bits, dBFS.
    pub vad_threshold: AtomicU32,
    /// f32 bits, latest mic level in dBFS (for the meter).
    pub level: AtomicU32,
    /// Whether mic audio is currently being sent.
    pub open: AtomicBool,
    /// Output volume, f32 bits.
    pub output_volume: AtomicU32,
    /// Sending allowed at all (not muted, connected).
    pub allowed: AtomicBool,
    /// Keep sending this many 20 ms frames after push-to-talk is released.
    pub ptt_release_frames: AtomicU32,
    /// Run the mic through RNNoise.
    pub denoise: AtomicBool,
    /// Even out how loud the people we hear are.
    pub agc: AtomicBool,
}

impl Gate {
    pub fn level_db(&self) -> f32 {
        f32::from_bits(self.level.load(Ordering::Relaxed))
    }
    pub fn is_open(&self) -> bool {
        self.open.load(Ordering::Relaxed)
    }
    pub fn set_f32(cell: &AtomicU32, v: f32) {
        cell.store(v.to_bits(), Ordering::Relaxed);
    }
}

#[derive(Clone)]
pub struct AudioData {
    pub a2ts: Arc<Mutex<AudioToTs>>,
    pub ts2a: Arc<Mutex<TsToAudio>>,
    pub gate: Arc<Gate>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
}

pub fn start(input: Option<&str>, output: Option<&str>, local_set: &LocalSet) -> Result<AudioData> {
    let gate = Arc::new(Gate::default());
    Gate::set_f32(&gate.output_volume, 1.0);
    Gate::set_f32(&gate.vad_threshold, -40.0);
    Gate::set_f32(&gate.level, -100.0);
    let output_device = find_device(output, false).ok_or_else(|| anyhow::anyhow!("no output device"))?;
    let input_device = find_device(input, true).ok_or_else(|| anyhow::anyhow!("no input device"))?;
    let ts2a = TsToAudio::new(output_device, gate.clone(), local_set)?;
    let a2ts = AudioToTs::new(input_device, gate.clone(), local_set)?;
    Ok(AudioData { a2ts, ts2a, gate })
}
