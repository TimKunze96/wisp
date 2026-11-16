//! The sound of everything that plays except Wisp itself (so viewers don't
//! hear the voice chat twice), through WASAPI process loopback in exclude
//! mode (Windows 10 2004 and newer), as Opus packets on the audio track.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

use anyhow::{Result, anyhow};
use wasapi::{AudioClient, Direction, SampleType, StreamMode, WaveFormat, initialize_mta};

use super::{SOUND_FRAME, SoundEncoder};

pub struct Sound {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Sound {
    /// Start capturing; `packets` gets each 20 ms Opus packet.
    pub fn start(packets: impl FnMut(Vec<u8>) + Send + 'static) -> Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready) = std::sync::mpsc::channel();
        let s = stop.clone();
        let thread = std::thread::Builder::new().name("screen-sound".into()).spawn(move || {
            if let Err(e) = run(packets, &s, &ready_tx) {
                let _ = ready_tx.send(Err(e));
            }
        })?;
        match ready.recv_timeout(std::time::Duration::from_secs(3)) {
            Ok(Ok(())) => Ok(Self { stop, thread: Some(thread) }),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(anyhow!("Windows didn't start the sound capture")),
        }
    }
}

impl Drop for Sound {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn run(
    mut packets: impl FnMut(Vec<u8>),
    stop: &AtomicBool,
    ready: &std::sync::mpsc::Sender<Result<()>>,
) -> Result<()> {
    let wasapi = |e: wasapi::WasapiError| anyhow!("{e}");
    initialize_mta().ok().map_err(|e| anyhow!("{e}"))?;
    let format = WaveFormat::new(32, 32, SampleType::Float, 48000, 2, None);
    // everything but our own process (and its children)
    let mut client = AudioClient::new_application_loopback_client(std::process::id(), false).map_err(wasapi)?;
    let mode = StreamMode::EventsShared { autoconvert: true, buffer_duration_hns: 0 };
    client.initialize_client(&format, Direction::Capture, mode, None).map_err(wasapi)?;
    let event = client.set_get_eventhandle().map_err(wasapi)?;
    let capture = client.get_audiocaptureclient().map_err(wasapi)?;
    client.start_stream().map_err(wasapi)?;
    let _ = ready.send(Ok(()));

    let mut encoder = SoundEncoder::new()?;
    let mut bytes: VecDeque<u8> = VecDeque::new();
    let frame_bytes = SOUND_FRAME * 2 * 4;
    while !stop.load(Ordering::Relaxed) {
        if event.wait_for_event(100).is_err() {
            continue;
        }
        if capture.read_from_device_to_deque(&mut bytes).is_err() {
            break;
        }
        while bytes.len() >= frame_bytes {
            let raw: Vec<u8> = bytes.drain(..frame_bytes).collect();
            let frame: Vec<f32> = raw.chunks_exact(4).map(|b| f32::from_le_bytes(b.try_into().unwrap())).collect();
            if let Some(p) = encoder.encode(&frame) {
                packets(p);
            }
        }
    }
    let _ = client.stop_stream();
    Ok(())
}
