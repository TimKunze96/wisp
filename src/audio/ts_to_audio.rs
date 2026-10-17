use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use anyhow::{Result, format_err};
use cpal::traits::{DeviceTrait, StreamTrait};
use cpal::{Device, Stream, StreamConfig};
use futures::prelude::*;
use tokio::task::LocalSet;
use tokio::time::{self, Duration};
use tokio_stream::wrappers::IntervalStream;
use tracing::{debug, error};
use tsclientlib::ClientId;
use tsproto_packets::packets::InAudioBuf;

use super::*;
use crate::ConnectionId;

pub type Id = (ConnectionId, ClientId);
type AudioHandler = tsclientlib::audio::AudioHandler<Id>;

pub struct TsToAudio {
    device: Device,
    stream: Option<Stream>,
    data: Arc<Mutex<AudioHandler>>,
    gate: Arc<Gate>,
    /// Per talker volume (0.0 mutes); shared with the playback callback,
    /// which multiplies in the automatic gain.
    volumes: Arc<Mutex<HashMap<Id, f32>>>,
}

/// Level the automatic gain aims for (RMS, about -20 dBFS).
const AGC_TARGET: f32 = 0.1;
/// Below this a frame counts as silence and doesn't move the gain.
const AGC_FLOOR: f32 = 0.004;

struct Callback {
    data: Arc<Mutex<AudioHandler>>,
    gate: Arc<Gate>,
    volumes: Arc<Mutex<HashMap<Id, f32>>>,
    /// Per talker (smoothed level, current gain).
    agc: HashMap<Id, (f32, f32)>,
}

impl TsToAudio {
    pub fn new(device: Device, gate: Arc<Gate>, local_set: &LocalSet) -> Result<Arc<Mutex<Self>>> {
        let res = Arc::new(Mutex::new(Self {
            device,
            stream: None,
            data: Arc::new(Mutex::new(AudioHandler::new())),
            gate,
            volumes: Arc::default(),
        }));
        Self::start(res.clone(), local_set);
        Ok(res)
    }

    fn open_playback(&self) -> Result<Stream> {
        let config = StreamConfig {
            channels: 2,
            sample_rate: 48000,
            buffer_size: cpal::BufferSize::Fixed(USUAL_FRAME_SIZE as u32),
        };
        let mut callback = Callback {
            data: self.data.clone(),
            gate: self.gate.clone(),
            volumes: self.volumes.clone(),
            agc: HashMap::new(),
        };
        let stream = self
            .device
            .build_output_stream(
                config,
                move |audio_data, _| callback.callback(audio_data),
                |error| error!(%error, "Error during audio playback"),
                Some(Duration::from_secs(5)),
            )
            .map_err(|e| format_err!("cpal error: {}", e))?;
        stream.play().map_err(|e| format_err!("cpal error: {}", e))?;
        Ok(stream)
    }

    pub fn set_volume(&mut self, id: Id, volume: f32) {
        let mut data = self.data.lock().unwrap();
        self.volumes.lock().unwrap().insert(id, volume);
        if let Some(q) = data.get_mut_queues().get_mut(&id) {
            q.volume = volume;
        }
    }

    /// Drop everything belonging to a connection (on disconnect).
    pub fn forget(&mut self, con: ConnectionId) {
        let mut data = self.data.lock().unwrap();
        self.volumes.lock().unwrap().retain(|(c, _), _| *c != con);
        data.get_mut_queues().retain(|(c, _), _| *c != con);
    }

    fn start(t2a: Arc<Mutex<Self>>, local_set: &LocalSet) {
        local_set.spawn_local(
            IntervalStream::new(time::interval(Duration::from_secs(1))).for_each(move |_| {
                let mut t2a = t2a.lock().unwrap();
                if t2a.stream.is_none() {
                    match t2a.open_playback() {
                        Ok(s) => {
                            t2a.stream = Some(s);
                            debug!("Opened playback device");
                        }
                        Err(error) => error!(%error, "Failed to open playback device"),
                    }
                }
                future::ready(())
            }),
        );
    }

    pub fn play_packet(&mut self, id: Id, packet: InAudioBuf) -> Result<()> {
        let mut data = self.data.lock().unwrap();
        let volume = self.volumes.lock().unwrap().get(&id).copied().unwrap_or(1.0);
        let new = !data.get_queues().contains_key(&id);
        data.handle_packet(id, packet)?;
        // a new queue starts at the user volume; the callback adds the gain
        if new {
            if let Some(q) = data.get_mut_queues().get_mut(&id) {
                q.volume = volume;
            }
        }
        Ok(())
    }
}

impl Callback {
    fn callback(&mut self, buffer: &mut [f32]) {
        buffer.fill(0.0);
        let agc_on = self.gate.agc.load(Ordering::Relaxed);
        let mut data = self.data.lock().unwrap();
        let mut levels: Vec<(Id, f32)> = Vec::new();
        data.fill_buffer_with_proc(buffer, |id, samples| {
            if agc_on && !samples.is_empty() {
                let rms = (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt();
                levels.push((*id, rms));
            }
        });
        // steer each talker's gain toward the target, slowly, for the next frame
        for (id, rms) in levels {
            let (level, gain) = self.agc.entry(id).or_insert((AGC_TARGET, 1.0));
            if rms > AGC_FLOOR {
                *level = *level * 0.9 + rms * 0.1;
                let want = (AGC_TARGET / *level).clamp(0.5, 4.0);
                *gain += (want - *gain) * 0.05;
            }
        }
        let volumes = self.volumes.lock().unwrap();
        for (id, q) in data.get_mut_queues().iter_mut() {
            let user = volumes.get(id).copied().unwrap_or(1.0);
            let gain = if agc_on { self.agc.get(id).map(|(_, g)| *g).unwrap_or(1.0) } else { 1.0 };
            q.volume = user * gain;
        }
        drop(volumes);
        let talking: Vec<Id> = data.get_queues().keys().copied().collect();
        drop(data);
        self.agc.retain(|id, _| talking.contains(id));

        let volume = f32::from_bits(self.gate.output_volume.load(Ordering::Relaxed));
        for s in buffer.iter_mut() {
            *s *= volume;
            // soft limiter: louder talkers plus gain mustn't clip harshly
            if s.abs() > 0.9 {
                *s = s.signum() * (0.9 + 0.1 * ((s.abs() - 0.9) * 10.0).tanh());
            }
        }
        super::record::tap(|| super::record::Chunk::Playback(buffer.to_vec()));
    }
}
