//! `wisp --demo`: made-up servers, channels and people for screenshots. Nothing
//! connects anywhere and settings go to a throwaway folder.

use futures::channel::mpsc as fmpsc;
use tokio::sync::mpsc;

use crate::config::{Bookmark, Settings};
use crate::net::chat::{ChatKind, ChatLine, Conversation};
use crate::net::{
    Activity, ChannelRow, ChannelType, ClientDetails, ClientRow, Codec, GroupBadge, Handle, MaxClients, ServerSnap, Snapshot, Status,
};

/// Point the config, data and cache folders at a temporary one, before
/// anything reads them.
pub fn isolate() {
    let dir = std::env::temp_dir().join("wisp-demo");
    let _ = std::fs::remove_dir_all(&dir);
    for (var, sub) in [("XDG_CONFIG_HOME", "config"), ("XDG_DATA_HOME", "data"), ("XDG_CACHE_HOME", "cache")] {
        let path = dir.join(sub);
        let _ = std::fs::create_dir_all(&path);
        // single-threaded at this point
        unsafe { std::env::set_var(var, path) };
    }
}

/// A network handle that drops every command, and one snapshot of fake servers.
pub fn spawn() -> (Handle, fmpsc::UnboundedReceiver<Snapshot>, Settings) {
    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel();
    std::thread::spawn(move || while cmd_rx.blocking_recv().is_some() {});
    let (snap_tx, snap_rx) = fmpsc::unbounded();
    let _ = snap_tx.unbounded_send(snapshot());
    std::mem::forget(snap_tx);
    (Handle { cmds: cmd_tx }, snap_rx, settings())
}

fn bookmark(id: &str, label: &str, address: &str) -> Bookmark {
    Bookmark {
        id: id.into(),
        label: label.into(),
        address: address.into(),
        nickname: "Alex".into(),
        ..Default::default()
    }
}

fn settings() -> Settings {
    Settings {
        bookmarks: vec![
            bookmark("b1", "Northwind Gaming", "ts.northwind.example"),
            bookmark("b2", "Pixel Forge", "voice.pixelforge.example"),
            bookmark("b3", "Late Night Raids", "raids.example:9988"),
        ],
        ..Settings::load()
    }
}

fn badge(name: &str, builtin: u32) -> GroupBadge {
    GroupBadge { name: name.into(), icon: None, builtin: Some(builtin) }
}

#[derive(Default, Clone, Copy)]
struct Who {
    talking: bool,
    muted: bool,
    deaf: bool,
    away: bool,
    commander: bool,
    admin: bool,
    moderator: bool,
    me: bool,
    recording: bool,
}

fn person(id: u16, name: &str, who: Who) -> ClientRow {
    let mut groups = Vec::new();
    if who.admin {
        groups.push(badge("Server Admin", 300));
    }
    if who.moderator {
        groups.push(badge("Moderator", 600));
    }
    ClientRow {
        id,
        uid: format!("demo{id}="),
        db_id: id as u64,
        name: name.into(),
        talking: who.talking,
        input_muted: who.muted,
        output_muted: who.deaf,
        away: who.away.then(|| "brb, food".to_string()),
        me: who.me,
        commander: who.commander,
        priority: false,
        recording: who.recording,
        country: String::new(),
        groups,
        group_ids: Vec::new(),
        channel_group: 0,
        talk_power_granted: false,
        friend: false,
        blocked: false,
        avatar: None,
        icon: None,
        whispering: false,
        talk_request: None,
        volume: 1.0,
        locally_muted: false,
    }
}

fn channel(id: u64, parent: u64, depth: usize, name: &str, clients: Vec<ClientRow>) -> ChannelRow {
    ChannelRow {
        id,
        parent,
        name: name.into(),
        phonetic: String::new(),
        topic: String::new(),
        description: None,
        depth,
        has_password: false,
        is_default: false,
        kind: ChannelType::Permanent,
        codec: Codec::OpusVoice,
        quality: 6,
        talk_power: 0,
        delete_delay: 0,
        latency: 1,
        unencrypted: false,
        subscribed: true,
        max_clients: None,
        icon: None,
        icon_id: 0,
        limit: MaxClients::Unlimited,
        family_limit: MaxClients::Inherited,
        clients,
    }
}

fn line(time: &str, from: &str, me: bool, text: &str) -> ChatLine {
    ChatLine {
        date: chrono::Local::now().format("%Y-%m-%d").to_string(),
        time: time.into(),
        from: from.into(),
        uid: format!("demo-{from}"),
        me,
        text: text.into(),
        mention: false,
        old: false,
    }
}

fn server(con: u64, bookmark: &str, name: &str, channels: Vec<ChannelRow>, own: Option<u64>, chats: Vec<Conversation>) -> ServerSnap {
    let users = channels.iter().map(|c| c.clients.len()).sum();
    ServerSnap {
        con,
        bookmark: bookmark.into(),
        label: name.into(),
        status: Status::Connected,
        server_name: name.into(),
        welcome: String::new(),
        platform: "Linux".into(),
        version: "3.13.7".into(),
        users,
        max_users: 128,
        channels,
        own_channel: own,
        ping_ms: Some(18.0),
        packet_loss: Some(0.0),
        details: None,
        groups: Vec::new(),
        channel_groups: Vec::new(),
        chats,
        host_message: None,
        transfers: Vec::new(),
        banner: None,
        host_button: None,
    }
}

fn snapshot() -> Snapshot {
    let w = Who::default();
    let mut id = 0u16;
    let mut p = |name: &str, who: Who| {
        id += 1;
        person(id, name, who)
    };
    let mut welcome = channel(1, 0, 0, "Welcome", vec![p("Kestrel", Who { admin: true, ..w })]);
    welcome.is_default = true;
    welcome.topic = "Read the rules in the description".into();
    let mut focus = channel(14, 12, 1, "Focus Room", vec![p("Juniper", Who { deaf: true, ..w })]);
    focus.has_password = true;
    let mut music = channel(10, 9, 1, "Music Bar", vec![p("Fennel", w), p("Quill", Who { muted: true, ..w })]);
    music.codec = Codec::OpusMusic;
    music.topic = "Lo-fi all night".into();
    let channels = vec![
        welcome,
        channel(2, 0, 0, "Lobby", vec![p("Marlow", w), p("Saffron", Who { away: true, ..w }), p("Tibbs", w)]),
        channel(3, 0, 0, "Gaming", vec![]),
        channel(
            4,
            3,
            1,
            "Valorant",
            vec![
                p("Alex", Who { me: true, talking: true, ..w }),
                p("Rook", Who { commander: true, moderator: true, ..w }),
                p("Nyx", Who { talking: true, ..w }),
                p("Bramble", w),
                p("Okapi", Who { muted: true, ..w }),
            ],
        ),
        channel(5, 3, 1, "Counter-Strike 2", vec![p("Vesper", w), p("Holloway", w), p("Wren", w)]),
        channel(6, 3, 1, "Deep Rock Galactic", vec![p("Pippin", w), p("Gristle", Who { recording: true, ..w }), p("Ember", w), p("Moss", w)]),
        channel(7, 3, 1, "Minecraft", vec![p("Clover", Who { away: true, ..w })]),
        channel(8, 3, 1, "Looking for group", vec![]),
        channel(9, 0, 0, "Chill", vec![]),
        music,
        channel(11, 9, 1, "Movie Night", vec![p("Hazel", w), p("Ludo", Who { deaf: true, ..w }), p("Sorrel", w)]),
        channel(12, 0, 0, "Work", vec![]),
        channel(13, 12, 1, "Standup", vec![p("Odette", Who { moderator: true, ..w }), p("Basil", w)]),
        focus,
        channel(15, 0, 0, "Away from keyboard", vec![p("Dormouse", Who { away: true, deaf: true, ..w }), p("Lark", Who { away: true, ..w })]),
    ];
    let chat = Conversation {
        kind: ChatKind::Channel,
        title: "Channel".into(),
        client: None,
        lines: vec![
            line("21:02", "Rook", false, "ranked in 5? need one more"),
            line("21:03", "Nyx", false, "I'm in, warming up in the range"),
            line("21:03", "Alex", true, "same, give me two minutes"),
            line("21:05", "Bramble", false, "[b]Ascent[/b] again please, no more Breeze"),
            line("21:06", "Rook", false, "queueing now"),
        ],
        received: 0,
    };
    let server_chat = Conversation {
        kind: ChatKind::Server,
        title: "Server".into(),
        client: None,
        lines: vec![line("20:40", "Kestrel", false, "Movie night starts at 22:00 in Chill/Movie Night")],
        received: 0,
    };
    let mut northwind = server(1, "b1", "Northwind Gaming", channels, Some(4), vec![chat, server_chat]);
    // what the side panel shows for Nyx
    northwind.details = Some(ClientDetails {
        id: 10,
        name: "Nyx".into(),
        uid: "demo10=".into(),
        description: "Duelist main, ask me about lineups".into(),
        version: "3.6.2 [Build: 1695203293]".into(),
        platform: "Windows".into(),
        ping_ms: Some(24.0),
        packet_loss: Some(0.0),
        connected_secs: Some(2 * 3600 + 17 * 60),
        idle_secs: 0,
        ..Default::default()
    });

    let mut id = 100u16;
    let mut q = |name: &str, who: Who| {
        id += 1;
        person(id, name, who)
    };
    let forge = server(
        2,
        "b2",
        "Pixel Forge",
        vec![
            channel(101, 0, 0, "Lobby", vec![q("Alex", Who { me: true, ..w }), q("Indigo", w)]),
            channel(102, 0, 0, "Pairing", vec![q("Thistle", w), q("Cobalt", w)]),
            channel(103, 0, 0, "Playtesting", vec![q("Marigold", w), q("Pike", Who { muted: true, ..w }), q("Rowan", w)]),
            channel(104, 0, 0, "Quiet", vec![]),
        ],
        Some(101),
        Vec::new(),
    );

    Snapshot {
        servers: vec![northwind, forge],
        active: Some(1),
        transmitting: true,
        mic_level_db: -18.0,
        activity: [
            ("20:58", "Pixel Forge", "Cobalt joined Pairing"),
            ("21:01", "Northwind Gaming", "Bramble joined Valorant"),
            ("21:02", "Northwind Gaming", "Okapi joined Valorant"),
            ("21:04", "Northwind Gaming", "Clover went away"),
            ("21:05", "Northwind Gaming", "Ludo moved to Movie Night"),
            ("21:06", "Northwind Gaming", "Holloway joined the server"),
            ("21:07", "Northwind Gaming", "Rook poked you"),
        ]
        .into_iter()
        .map(|(time, server, text)| Activity { time: time.into(), server: server.into(), text: text.into(), chat: false })
        .collect(),
        ..Default::default()
    }
}
