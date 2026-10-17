//! Chat conversations per server (server, channel, private by unique id) and
//! their history in ~/.local/share/wisp/chat/<server>/<conversation>.jsonl.

use std::io::Write;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Lines kept in memory (and loaded from history) per conversation.
pub const KEEP: usize = 300;

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ChatKind {
    Server,
    Channel,
    /// By the other side's unique id.
    Private(String),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChatLine {
    /// YYYY-MM-DD
    pub date: String,
    /// HH:MM
    pub time: String,
    pub from: String,
    pub uid: String,
    pub me: bool,
    /// Raw BBCode.
    pub text: String,
    /// Mentions our nickname.
    #[serde(default)]
    pub mention: bool,
    /// Loaded from history (not new in this session).
    #[serde(skip)]
    pub old: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Conversation {
    pub kind: ChatKind,
    /// "Channel", "Server" or the other person's name.
    pub title: String,
    /// The other person's current client id, while they're online.
    pub client: Option<u16>,
    pub lines: Vec<ChatLine>,
    /// Count of lines that arrived in this session; the UI compares it with
    /// what it has shown to get the unread count.
    pub received: u64,
}

pub fn now_line(from: String, uid: String, me: bool, text: String, mention: bool) -> ChatLine {
    let now = chrono::Local::now();
    ChatLine {
        date: now.format("%Y-%m-%d").to_string(),
        time: now.format("%H:%M").to_string(),
        from,
        uid,
        me,
        text,
        mention,
        old: false,
    }
}

fn file_name(kind: &ChatKind) -> String {
    match kind {
        ChatKind::Server => "server.jsonl".into(),
        ChatKind::Channel => "channel.jsonl".into(),
        // uids are base64: keep them file-name safe
        ChatKind::Private(uid) => format!("private-{}.jsonl", uid.replace('/', "_").replace('+', "-")),
    }
}

pub struct History {
    dir: PathBuf,
}

impl History {
    pub fn new(server: &str) -> Self {
        let base = dirs::data_dir().unwrap_or_else(|| PathBuf::from(".")).join("wisp/chat");
        let safe: String =
            server.chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '_' }).collect();
        Self { dir: base.join(safe) }
    }

    pub fn load(&self, kind: &ChatKind) -> Vec<ChatLine> {
        let Ok(text) = std::fs::read_to_string(self.dir.join(file_name(kind))) else { return Vec::new() };
        let lines: Vec<ChatLine> = text
            .lines()
            .filter_map(|l| serde_json::from_str::<ChatLine>(l).ok())
            .map(|mut l| {
                l.old = true;
                l.mention = false;
                l
            })
            .collect();
        lines[lines.len().saturating_sub(KEEP)..].to_vec()
    }

    pub fn append(&self, kind: &ChatKind, line: &ChatLine) {
        let _ = std::fs::create_dir_all(&self.dir);
        let Ok(mut f) =
            std::fs::OpenOptions::new().create(true).append(true).open(self.dir.join(file_name(kind)))
        else {
            return;
        };
        if let Ok(json) = serde_json::to_string(line) {
            let _ = writeln!(f, "{json}");
        }
    }
}
