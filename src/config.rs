//! Settings and bookmarks, stored as JSON in ~/.config/wisp/config.json.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Bookmark {
    pub id: String,
    pub label: String,
    pub address: String,
    pub password: String,
    pub nickname: String,
    /// Channel path to join on connect, e.g. "Lobby/Games".
    pub channel: String,
    pub auto_connect: bool,
    /// Overrides the global voice mode while this server is active.
    pub voice_mode: Option<VoiceMode>,
    /// Identity to connect with (id in identities.json); None: the first one.
    pub identity: Option<String>,
    /// How text-to-speech should say the nickname.
    pub phonetic: String,
    /// No sounds or popups for this server.
    pub quiet: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum VoiceMode {
    #[default]
    PushToTalk,
    VoiceActivation,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// cpal device ids; None is the system default.
    pub input_device: Option<String>,
    pub output_device: Option<String>,
    pub mic_volume: f32,
    pub output_volume: f32,
    pub voice_mode: VoiceMode,
    /// Voice activation opens above this level (dBFS).
    pub vad_threshold_db: f32,
    /// Keep sending this long after push-to-talk is released.
    pub ptt_release_ms: u32,
    /// Per-user volume by unique id (1.0 = 100%).
    pub user_volumes: HashMap<String, f32>,
    pub user_muted: HashSet<String>,
    pub bookmarks: Vec<Bookmark>,
    /// Global hotkeys: action -> Hyprland key spec ("SUPER + M", "F14").
    pub hotkeys: std::collections::BTreeMap<crate::hotkeys::Action, String>,
    /// Closing the window keeps Wisp in the tray while connected.
    pub close_to_tray: bool,
    /// Remembered channel passwords: "<bookmark id>/<channel id>" -> password.
    pub channel_passwords: std::collections::BTreeMap<String, String>,
    /// Collapsed channels in the tree: "<bookmark id>/<channel id>".
    pub collapsed: std::collections::BTreeSet<String>,
    /// Sound / popup per event; missing ones use the defaults.
    pub notify: std::collections::BTreeMap<crate::notify::EventKind, crate::notify::EventSetting>,
    pub sound_volume: f32,
    pub whisper_lists: Vec<WhisperList>,
    /// Main window size (width, height) when it was last closed.
    pub window_size: Option<(f32, f32)>,
    /// People by unique id: friends and blocked.
    pub contacts: std::collections::BTreeMap<String, Contact>,
    /// What opened ts3server:// links before Wisp took them over.
    pub previous_link_handler: Option<String>,
    /// RNNoise on the microphone.
    pub noise_suppression: bool,
    /// PipeWire echo canceller between Wisp and the devices.
    pub echo_cancel: bool,
    /// Even out the loudness of the people we hear.
    pub auto_gain: bool,
    /// Turn other apps down while someone talks, to this fraction.
    pub ducking: bool,
    pub duck_level: f32,
    /// Read event titles aloud (espeak-ng) instead of playing their sound.
    pub speak_events: bool,
}

/// Who a whisper list talks to. Names, paths and unique ids rather than ids,
/// so a list works across reconnects (and on several servers).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum WhisperTarget {
    /// Channel path, e.g. "Lobby/Games".
    Channel { path: String },
    Client { uid: String, name: String },
    ServerGroup { name: String },
    /// Everyone who is channel commander right now.
    Commanders,
    /// The channels directly below yours.
    Subchannels,
    ParentChannel,
}

impl WhisperTarget {
    pub fn label(&self) -> String {
        match self {
            WhisperTarget::Channel { path } => path.rsplit('/').next().unwrap_or(path).to_string(),
            WhisperTarget::Client { name, .. } => name.clone(),
            WhisperTarget::ServerGroup { name } => format!("Group {name}"),
            WhisperTarget::Commanders => "Channel commanders".into(),
            WhisperTarget::Subchannels => "My sub-channels".into(),
            WhisperTarget::ParentChannel => "My parent channel".into(),
        }
    }

    pub fn icon(&self) -> &'static str {
        match self {
            WhisperTarget::Channel { .. } => "hash",
            WhisperTarget::Client { .. } => "user",
            WhisperTarget::ServerGroup { .. } => "shield",
            WhisperTarget::Commanders => "megaphone",
            WhisperTarget::Subchannels => "corner-down-right",
            WhisperTarget::ParentChannel => "corner-left-up",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WhisperList {
    pub id: String,
    pub name: String,
    /// Hyprland key spec; held to whisper.
    pub hotkey: String,
    pub targets: Vec<WhisperTarget>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            input_device: None,
            output_device: None,
            mic_volume: 1.0,
            output_volume: 1.0,
            voice_mode: VoiceMode::PushToTalk,
            vad_threshold_db: -40.0,
            ptt_release_ms: 0,
            channel_passwords: Default::default(),
            collapsed: Default::default(),
            notify: Default::default(),
            sound_volume: 0.6,
            whisper_lists: Vec::new(),
            previous_link_handler: None,
            contacts: Default::default(),
            window_size: None,
            noise_suppression: true,
            echo_cancel: false,
            auto_gain: false,
            ducking: false,
            duck_level: 0.3,
            speak_events: false,
            user_volumes: HashMap::new(),
            user_muted: HashSet::new(),
            bookmarks: Vec::new(),
            hotkeys: Default::default(),
            close_to_tray: true,
        }
    }
}

pub fn config_dir() -> PathBuf {
    let base = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
    base.join("wisp")
}

pub fn cache_dir() -> PathBuf {
    dirs::cache_dir().unwrap_or_else(|| PathBuf::from(".")).join("wisp")
}

fn path() -> PathBuf {
    config_dir().join("config.json")
}

pub fn channel_key(bookmark: &str, channel: u64) -> String {
    format!("{bookmark}/{channel}")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Contact {
    Friend,
    Blocked,
}

impl Settings {
    pub fn is_blocked(&self, uid: &str) -> bool {
        !uid.is_empty() && self.contacts.get(uid) == Some(&Contact::Blocked)
    }
    pub fn is_friend(&self, uid: &str) -> bool {
        !uid.is_empty() && self.contacts.get(uid) == Some(&Contact::Friend)
    }

    /// A whisper list by id or (case-insensitive) name.
    pub fn whisper_list(&self, q: &str) -> Option<&WhisperList> {
        self.whisper_lists.iter().find(|l| l.id == q).or_else(|| self.whisper_lists.iter().find(|l| l.name.eq_ignore_ascii_case(q)))
    }

    pub fn notify_setting(&self, kind: crate::notify::EventKind) -> crate::notify::EventSetting {
        self.notify.get(&kind).copied().unwrap_or_else(|| kind.default_setting())
    }

    pub fn load() -> Self {
        let s: Settings = std::fs::read_to_string(path())
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        s
    }

    pub fn save(&self) {
        let _ = std::fs::create_dir_all(config_dir());
        if let Ok(text) = serde_json::to_string_pretty(self) {
            let tmp = path().with_extension("json.tmp");
            if std::fs::write(&tmp, text).is_ok() {
                let _ = std::fs::rename(tmp, path());
            }
        }
    }

    /// Find a bookmark by id, label or address (case-insensitive), for the CLI.
    pub fn find_bookmark(&self, query: &str) -> Option<&Bookmark> {
        let q = query.to_lowercase();
        self.bookmarks.iter().find(|b| b.id == query).or_else(|| {
            self.bookmarks
                .iter()
                .find(|b| b.label.to_lowercase() == q || b.address.to_lowercase() == q)
        })
    }

    pub fn user_volume(&self, uid: &str) -> f32 {
        if self.user_muted.contains(uid) || self.is_blocked(uid) {
            0.0
        } else {
            self.user_volumes.get(uid).copied().unwrap_or(1.0)
        }
    }
}

pub fn new_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    format!("{nanos:x}")
}
