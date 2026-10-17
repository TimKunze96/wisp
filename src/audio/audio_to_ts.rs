use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Result, format_err};
use audiopus::coder::Encoder;
use nnnoiseless::DenoiseState;
use cpal::traits::{DeviceTrait, StreamTrait};
use cpal::{Device, Stream, StreamConfig};
use futures::prelude::*;
use tokio::sync::mpsc;
use tokio::task::LocalSet;
use tokio::time::{self, Duration};
use tokio_stream::wrappers::IntervalStream;
use tracing::{debug, error, warn};

use super::*;

pub struct AudioToTs {
    device: Device,
    listener: Arc<Mutex<Option<mpsc::Sender<Vec<u8>>>>>,
    stream: Option<Stream>,
    /// f32 bits
    volume: Arc<AtomicU32>,
    gate: Arc<Gate>,
}

struct Callback {
    listener: Arc<Mutex<Option<mpsc::Sender<Vec<u8>>>>>,
    encoder: Encoder,
    volume: Arc<AtomicU32>,
    gate: Arc<Gate>,
    vad_hold: u32,
    ptt_hold: u32,
    was_open: bool,
    tmp_buffer: [f32; USUAL_FRAME_SIZE * 2],
    opus_output: [u8; MAX_OPUS_FRAME_SIZE],
    denoise: Box<DenoiseState<'static>>,
    denoise_in: [f32; DenoiseState::FRAME_SIZE],
    denoise_out: [f32; DenoiseState::FRAME_SIZE],
}

impl AudioToTs {
    pub fn new(device: Device, gate: Arc<Gate>, local_set: &LocalSet) -> Result<Arc<Mutex<Self>>> {
        let res = Arc::new(Mutex::new(Self {
            device,
            listener: Arc::default(),
            stream: None,
            volume: Arc::new(AtomicU32::new(1.0_f32.to_bits())),
            gate,
        }));
        Self::start(res.clone(), local_set);
        Ok(res)
    }

    fn open_capture(&self) -> Result<Stream> {
        let config = StreamConfig {
            channels: 1,
            sample_rate: 48000,
            buffer_size: cpal::BufferSize::Fixed(USUAL_FRAME_SIZE as u32),
        };
        let encoder = Encoder::new(
            audiopus::SampleRate::Hz48000,
            audiopus::Channels::Mono,
            audiopus::Application::Voip,
        )
        .expect("Could not create encoder");
        let mut callback = Callback {
            listener: self.listener.clone(),
            encoder,
            volume: self.volume.clone(),
            gate: self.gate.clone(),
            vad_hold: 0,
            ptt_hold: 0,
            was_open: false,
            tmp_buffer: [0.0; _],
            opus_output: [0; _],
            denoise: DenoiseState::new(),
            denoise_in: [0.0; _],
            denoise_out: [0.0; _],
        };
        let stream = self
            .device
            .build_input_stream(
                config,
                move |audio_data, _| callback.callback(audio_data),
                |error| error!(%error, "Error during audio capture"),
                Some(Duration::from_secs(5)),
            )
            .map_err(|e| format_err!("cpal error: {}", e))?;
        stream.play().map_err(|e| format_err!("cpal error: {}", e))?;
        Ok(stream)
    }

    pub fn set_listener(&self, sender: mpsc::Sender<Vec<u8>>) {
        *self.listener.lock().unwrap() = Some(sender);
    }

    pub fn set_volume(&mut self, volume: f32) {
        self.volume.store(volume.to_bits(), Ordering::Relaxed);
    }

    fn start(a2t: Arc<Mutex<Self>>, local_set: &LocalSet) {
        // Opens the stream and reopens it when the device goes away.
        local_set.spawn_local(
            IntervalStream::new(time::interval(Duration::from_secs(1))).for_each(move |_| {
                let mut a2t = a2t.lock().unwrap();
                if a2t.stream.is_none() {
                    match a2t.open_capture() {
                        Ok(s) => {
                            a2t.stream = Some(s);
                            debug!("Opened capture device");
                        }
                        Err(error) => error!(%error, "Failed to open capture device"),
                    }
                }
                future::ready(())
            }),
        );
    }
}

impl Callback {
    fn callback(&mut self, data: &[f32]) {
        let volume = f32::from_bits(self.volume.load(Ordering::Relaxed));
        let len = data.len().min(self.tmp_buffer.len());
        for (dst, s) in self.tmp_buffer[..len].iter_mut().zip(data) {
            *dst = *s * volume;
        }
        // RNNoise works on 10 ms frames in 16-bit range; a leftover partial
        // frame (unusual buffer sizes) goes out as it is
        if self.gate.denoise.load(Ordering::Relaxed) {
            const N: usize = DenoiseState::FRAME_SIZE;
            for chunk in self.tmp_buffer[..len].chunks_exact_mut(N) {
                for (d, s) in self.denoise_in.iter_mut().zip(chunk.iter()) {
                    *d = *s * 32768.0;
                }
                self.denoise.process_frame(&mut self.denoise_out, &self.denoise_in);
                for (d, s) in chunk.iter_mut().zip(self.denoise_out.iter()) {
                    *d = *s / 32768.0;
                }
            }
        }
        let sum: f32 = self.tmp_buffer[..len].iter().map(|s| s * s).sum();
        let rms = (sum / len.max(1) as f32).sqrt();
        let db = if rms > 0.0 { 20.0 * rms.log10() } else { -100.0 };
        Gate::set_f32(&self.gate.level, db);

        let ptt = self.gate.ptt.load(Ordering::Relaxed);
        let vad = self.gate.vad_enabled.load(Ordering::Relaxed);
        if vad && db > f32::from_bits(self.gate.vad_threshold.load(Ordering::Relaxed)) {
            self.vad_hold = VAD_HOLD_FRAMES;
        } else if self.vad_hold > 0 {
            self.vad_hold -= 1;
        }
        if ptt {
            self.ptt_hold = self.gate.ptt_release_frames.load(Ordering::Relaxed);
        } else if self.ptt_hold > 0 {
            self.ptt_hold -= 1;
        }
        let allowed = self.gate.allowed.load(Ordering::Relaxed);
        let open = allowed && (ptt || self.ptt_hold > 0 || (vad && self.vad_hold > 0));
        if !allowed {
            self.ptt_hold = 0;
        }
        self.gate.open.store(open, Ordering::Relaxed);
        if !open {
            self.was_open = false;
            return;
        }
        self.was_open = true;
        super::record::tap(|| super::record::Chunk::Mic(self.tmp_buffer[..len].to_vec()));

        let len = match self.encoder.encode_float(&self.tmp_buffer[..len], &mut self.opus_output[..]) {
            Err(error) => {
                error!(%error, "Failed to encode opus");
                return;
            }
            Ok(len) => len,
        };
        // the session wraps it (normal talk or whisper, depending on the target)
        let packet = self.opus_output[..len].to_vec();
        if let Some(listener) = &*self.listener.lock().unwrap() {
            if let Err(mpsc::error::TrySendError::Closed(_)) = listener.try_send(packet) {
                warn!("Mic packet receiver closed");
            }
        }
    }
}
