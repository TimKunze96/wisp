//! Recording what we hear (all servers) plus our own mic into an Ogg Opus
//! file. The audio callbacks only push samples into a channel; a thread mixes,
//! encodes and writes.

use std::fs::File;
use std::io::BufWriter;
use std::path::PathBuf;
use std::sync::mpsc;

use audiopus::coder::Encoder;
use ogg::writing::{PacketWriteEndInfo, PacketWriter};

/// 20 ms of stereo audio at 48 kHz.
const FRAME: usize = 960;

pub enum Chunk {
    /// Interleaved stereo from the playback callback.
    Playback(Vec<f32>),
    /// Mono mic frame, only while we send.
    Mic(Vec<f32>),
    Stop,
}

/// Where the audio callbacks send samples while a recording runs.
pub static TAP: std::sync::Mutex<Option<mpsc::Sender<Chunk>>> = std::sync::Mutex::new(None);

/// From an audio callback: never block it, skip the chunk if the lock is busy.
pub fn tap(chunk: impl FnOnce() -> Chunk) {
    if let Ok(guard) = TAP.try_lock() {
        if let Some(tx) = guard.as_ref() {
            let _ = tx.send(chunk());
        }
    }
}

pub struct Recording {
    pub path: PathBuf,
    pub tx: mpsc::Sender<Chunk>,
}

impl Recording {
    pub fn start(path: PathBuf) -> anyhow::Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let file = BufWriter::new(File::create(&path)?);
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new().name("recorder".into()).spawn(move || {
            if let Err(e) = write(file, rx) {
                tracing::error!(%e, "recording failed");
            }
        })?;
        *TAP.lock().unwrap() = Some(tx.clone());
        Ok(Self { path, tx })
    }

    pub fn stop(self) {
        *TAP.lock().unwrap() = None;
        let _ = self.tx.send(Chunk::Stop);
    }
}

fn opus_head() -> Vec<u8> {
    let mut h = b"OpusHead".to_vec();
    h.push(1); // version
    h.push(2); // channels
    h.extend(312u16.to_le_bytes()); // pre-skip (encoder lookahead)
    h.extend(48_000u32.to_le_bytes());
    h.extend(0i16.to_le_bytes()); // gain
    h.push(0); // mapping family
    h
}

fn opus_tags() -> Vec<u8> {
    let vendor = b"wisp";
    let mut t = b"OpusTags".to_vec();
    t.extend((vendor.len() as u32).to_le_bytes());
    t.extend(vendor);
    t.extend(0u32.to_le_bytes());
    t
}

fn write(file: BufWriter<File>, rx: mpsc::Receiver<Chunk>) -> anyhow::Result<()> {
    let encoder = Encoder::new(audiopus::SampleRate::Hz48000, audiopus::Channels::Stereo, audiopus::Application::Audio)?;
    let mut out = PacketWriter::new(file);
    let serial = std::process::id();
    out.write_packet(opus_head(), serial, PacketWriteEndInfo::EndPage, 0)?;
    out.write_packet(opus_tags(), serial, PacketWriteEndInfo::EndPage, 0)?;

    let mut playback: Vec<f32> = Vec::new();
    let mut mic: Vec<f32> = Vec::new();
    let mut granule: u64 = 0;
    let mut packet = vec![0u8; 4000];
    let mut pending: Option<Vec<u8>> = None;
    loop {
        let stop = match rx.recv() {
            Ok(Chunk::Playback(s)) => {
                playback.extend(s);
                false
            }
            Ok(Chunk::Mic(s)) => {
                mic.extend(s);
                false
            }
            Ok(Chunk::Stop) | Err(_) => true,
        };
        // playback runs all the time, so it sets the pace; the mic is mixed in
        while playback.len() >= FRAME * 2 {
            let mut frame: Vec<f32> = playback.drain(..FRAME * 2).collect();
            let take = mic.len().min(FRAME);
            for (i, m) in mic.drain(..take).enumerate() {
                frame[i * 2] += m;
                frame[i * 2 + 1] += m;
            }
            // more than a moment of mic backlog: playback isn't running, drop it
            if mic.len() > FRAME * 10 {
                mic.clear();
            }
            for s in &mut frame {
                *s = s.clamp(-1.0, 1.0);
            }
            let len = encoder.encode_float(&frame, &mut packet)?;
            granule += FRAME as u64;
            // write the previous packet now that we know it isn't the last one
            if let Some(p) = pending.replace(packet[..len].to_vec()) {
                out.write_packet(p, serial, PacketWriteEndInfo::NormalPacket, granule - FRAME as u64)?;
            }
        }
        if stop {
            let last = pending.take().unwrap_or_default();
            out.write_packet(last, serial, PacketWriteEndInfo::EndStream, granule)?;
            return Ok(());
        }
    }
}

/// ~/Music/Wisp/<date time> <server>.opus
pub fn new_path(server: &str) -> PathBuf {
    let music = dirs::audio_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join("Music")))
        .unwrap_or_else(|| PathBuf::from("."));
    let safe: String = server.chars().map(|c| if c.is_alphanumeric() || " .-_".contains(c) { c } else { '_' }).collect();
    let stamp = chrono::Local::now().format("%Y-%m-%d %H-%M");
    music.join("Wisp").join(format!("{stamp} {}.opus", safe.trim()))
}
