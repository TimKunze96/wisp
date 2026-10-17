//! Sounds and desktop notifications for things that happen on a server.
//! Sounds are short tones generated once into ~/.cache/wisp/sounds and played
//! with pw-play on the chosen output; popups go through notify-send.

use std::path::PathBuf;
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EventKind {
    Poke,
    PrivateMessage,
    Mention,
    ChannelMessage,
    UserJoined,
    UserLeft,
    MovedOrKicked,
    Disconnected,
    MicToggled,
    FriendOnline,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct EventSetting {
    pub sound: bool,
    pub popup: bool,
}

impl EventKind {
    pub const ALL: [EventKind; 10] = [
        EventKind::Poke,
        EventKind::PrivateMessage,
        EventKind::Mention,
        EventKind::ChannelMessage,
        EventKind::UserJoined,
        EventKind::UserLeft,
        EventKind::MovedOrKicked,
        EventKind::Disconnected,
        EventKind::FriendOnline,
        EventKind::MicToggled,
    ];

    pub fn title(self) -> &'static str {
        match self {
            EventKind::Poke => "Poke",
            EventKind::PrivateMessage => "Private message",
            EventKind::Mention => "Someone mentions you",
            EventKind::ChannelMessage => "Channel message",
            EventKind::UserJoined => "Someone joins your channel",
            EventKind::UserLeft => "Someone leaves your channel",
            EventKind::MovedOrKicked => "You get moved, kicked or banned",
            EventKind::Disconnected => "Connection lost",
            EventKind::MicToggled => "Mic muted / unmuted",
            EventKind::FriendOnline => "A friend comes online",
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            EventKind::Poke => "hand",
            EventKind::PrivateMessage => "message-square",
            EventKind::Mention => "at-sign",
            EventKind::ChannelMessage => "messages-square",
            EventKind::UserJoined => "user-plus",
            EventKind::UserLeft => "user-minus",
            EventKind::MovedOrKicked => "log-out",
            EventKind::Disconnected => "unplug",
            EventKind::MicToggled => "mic",
            EventKind::FriendOnline => "star",
        }
    }

    /// Popups make no sense for some events (your own mic, people coming and going).
    pub fn can_popup(self) -> bool {
        !matches!(self, EventKind::MicToggled | EventKind::UserJoined | EventKind::UserLeft)
    }

    pub fn default_setting(self) -> EventSetting {
        let (sound, popup) = match self {
            EventKind::Poke | EventKind::PrivateMessage | EventKind::Mention => (true, true),
            EventKind::MovedOrKicked | EventKind::Disconnected => (true, true),
            EventKind::ChannelMessage => (true, false),
            EventKind::FriendOnline => (true, true),
            EventKind::UserJoined | EventKind::UserLeft | EventKind::MicToggled => (true, false),
        };
        EventSetting { sound, popup }
    }

    fn sound(self) -> Sound {
        match self {
            EventKind::Poke => Sound::Poke,
            EventKind::PrivateMessage | EventKind::Mention => Sound::Message,
            EventKind::ChannelMessage => Sound::Soft,
            EventKind::UserJoined | EventKind::FriendOnline => Sound::Join,
            EventKind::UserLeft => Sound::Leave,
            EventKind::MovedOrKicked | EventKind::Disconnected => Sound::Alert,
            EventKind::MicToggled => Sound::Soft,
        }
    }
}

/// Something worth a sound or popup.
#[derive(Clone, Debug)]
pub struct Event {
    pub kind: EventKind,
    pub title: String,
    pub body: String,
}

#[derive(Clone, Copy)]
enum Sound {
    Join,
    Leave,
    Poke,
    Message,
    Soft,
    Alert,
    Muted,
    Unmuted,
}

impl Sound {
    fn name(self) -> &'static str {
        match self {
            Sound::Join => "join",
            Sound::Leave => "leave",
            Sound::Poke => "poke",
            Sound::Message => "message",
            Sound::Soft => "soft",
            Sound::Alert => "alert",
            Sound::Muted => "muted",
            Sound::Unmuted => "unmuted",
        }
    }

    /// (frequency Hz, start ms, length ms) notes.
    fn notes(self) -> &'static [(f32, u32, u32)] {
        match self {
            Sound::Join => &[(660., 0, 110), (880., 90, 160)],
            Sound::Leave => &[(880., 0, 110), (587., 90, 180)],
            Sound::Poke => &[(988., 0, 70), (988., 110, 70), (1319., 220, 140)],
            Sound::Message => &[(1047., 0, 90), (1568., 70, 200)],
            Sound::Soft => &[(784., 0, 140)],
            Sound::Alert => &[(523., 0, 160), (392., 150, 260)],
            Sound::Muted => &[(440., 0, 90), (330., 70, 120)],
            Sound::Unmuted => &[(330., 0, 90), (440., 70, 120)],
        }
    }
}

const RATE: u32 = 48_000;

/// Soft sine notes with a quick attack and exponential decay, as 16-bit WAV.
fn render(sound: Sound) -> Vec<u8> {
    let notes = sound.notes();
    let len_ms = notes.iter().map(|(_, s, l)| s + l).max().unwrap_or(0) + 20;
    let n = (RATE * len_ms / 1000) as usize;
    let mut samples = vec![0f32; n];
    for &(freq, start, len) in notes {
        let s0 = (RATE * start / 1000) as usize;
        let count = (RATE * len / 1000) as usize;
        for i in 0..count {
            let t = i as f32 / RATE as f32;
            let attack = (i as f32 / (RATE as f32 * 0.005)).min(1.0);
            let decay = (-t * 6000.0 / len as f32).exp();
            // a little second harmonic keeps it from sounding like a test tone
            let v = (t * freq * std::f32::consts::TAU).sin() + 0.15 * (t * freq * 2.0 * std::f32::consts::TAU).sin();
            if let Some(s) = samples.get_mut(s0 + i) {
                *s += v * attack * decay * 0.22;
            }
        }
    }
    let data: Vec<u8> = samples
        .iter()
        .flat_map(|s| ((s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16).to_le_bytes())
        .collect();
    let mut wav = Vec::with_capacity(44 + data.len());
    wav.extend(b"RIFF");
    wav.extend((36 + data.len() as u32).to_le_bytes());
    wav.extend(b"WAVEfmt ");
    wav.extend(16u32.to_le_bytes());
    wav.extend(1u16.to_le_bytes()); // PCM
    wav.extend(1u16.to_le_bytes()); // mono
    wav.extend(RATE.to_le_bytes());
    wav.extend((RATE * 2).to_le_bytes());
    wav.extend(2u16.to_le_bytes());
    wav.extend(16u16.to_le_bytes());
    wav.extend(b"data");
    wav.extend((data.len() as u32).to_le_bytes());
    wav.extend(data);
    wav
}

fn sound_file(sound: Sound) -> Option<PathBuf> {
    // bump the version when the sounds change so old files get replaced
    let dir = crate::config::cache_dir().join("sounds/v1");
    let path = dir.join(format!("{}.wav", sound.name()));
    if !path.exists() {
        std::fs::create_dir_all(&dir).ok()?;
        std::fs::write(&path, render(sound)).ok()?;
    }
    Some(path)
}

fn play(sound: Sound, volume: f32, output: Option<&str>) {
    let Some(path) = sound_file(sound) else { return };
    let mut cmd = Command::new("pw-play");
    cmd.arg("--volume").arg(format!("{:.2}", volume.clamp(0.0, 1.5)));
    if let Some(target) = output {
        cmd.arg("--target").arg(target);
    }
    cmd.arg(path);
    run(cmd);
}

/// Start a helper process and wait for it on a thread (no zombies).
fn run(mut cmd: Command) {
    if let Ok(mut child) = cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() {
        std::thread::spawn(move || child.wait());
    }
}

/// Play an event's sound regardless of its setting (the settings preview).
pub fn preview(kind: EventKind, settings: &crate::config::Settings) {
    let sound = if kind == EventKind::MicToggled { Sound::Muted } else { kind.sound() };
    play(sound, settings.sound_volume, settings.output_device.as_deref());
}

/// Say `text` with espeak-ng, played like the sounds (volume, output).
fn speak(text: &str, volume: f32, output: Option<&str>) {
    let Ok(mut speech) = Command::new("espeak-ng")
        .args(["-v", "en-us", "-s", "170", "--stdout", text])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return;
    };
    let Some(wav) = speech.stdout.take() else { return };
    let mut play = Command::new("pw-play");
    play.arg("--volume").arg(format!("{:.2}", volume.clamp(0.0, 1.5)));
    if let Some(target) = output {
        play.arg("--target").arg(target);
    }
    play.arg("-").stdin(wav);
    if let Ok(mut child) = play.stdout(Stdio::null()).stderr(Stdio::null()).spawn() {
        std::thread::spawn(move || {
            let _ = child.wait();
            let _ = speech.wait();
        });
    }
}

pub fn can_speak() -> bool {
    Command::new("espeak-ng").arg("--version").stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok()
}

/// Mic mute toggles have their own two sounds.
pub fn mic(muted: bool, settings: &crate::config::Settings) {
    if settings.notify_setting(EventKind::MicToggled).sound {
        play(if muted { Sound::Muted } else { Sound::Unmuted }, settings.sound_volume, settings.output_device.as_deref());
    }
}

/// Play / show an event according to the settings. `focused`: the Wisp
/// window has focus, so chat popups would only be noise.
pub fn fire(event: &Event, settings: &crate::config::Settings, focused: bool) {
    let s = settings.notify_setting(event.kind);
    // the spoken line replaces the sound, so the two don't overlap
    let spoken = settings.speak_events && event.kind != EventKind::ChannelMessage;
    if spoken && (s.sound || s.popup) {
        speak(&event.title, settings.sound_volume, settings.output_device.as_deref());
    } else if s.sound {
        play(event.kind.sound(), settings.sound_volume, settings.output_device.as_deref());
    }
    let chat = matches!(event.kind, EventKind::PrivateMessage | EventKind::Mention | EventKind::ChannelMessage);
    if s.popup && event.kind.can_popup() && !(chat && focused) {
        let mut cmd = Command::new("notify-send");
        cmd.args(["--app-name", "Wisp", "--icon", "audio-headset"]);
        if matches!(event.kind, EventKind::Poke | EventKind::MovedOrKicked) {
            cmd.args(["--urgency", "critical"]);
        }
        cmd.arg(&event.title).arg(&event.body);
        run(cmd);
    }
}
