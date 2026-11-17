//! The sound that comes with the ScreenCaptureKit stream (everything but
//! Wisp itself), as Opus packets on the audio track.

use anyhow::Result;

use super::capture::set_sound;
use super::{SOUND_FRAME, SoundEncoder};

/// While it lives, the capture's sound goes out.
pub struct Sound;

impl Sound {
    /// `packets` gets each 20 ms Opus packet.
    pub fn start(mut packets: impl FnMut(Vec<u8>) + Send + 'static) -> Result<Self> {
        let mut encoder = SoundEncoder::new()?;
        let mut pending: Vec<f32> = Vec::new();
        set_sound(Some(Box::new(move |stereo: &[f32]| {
            pending.extend_from_slice(stereo);
            let frame = SOUND_FRAME * 2;
            let mut used = 0;
            while pending.len() - used >= frame {
                if let Some(p) = encoder.encode(&pending[used..used + frame]) {
                    packets(p);
                }
                used += frame;
            }
            pending.drain(..used);
        })));
        Ok(Self)
    }
}

impl Drop for Sound {
    fn drop(&mut self) {
        set_sound(None);
    }
}
