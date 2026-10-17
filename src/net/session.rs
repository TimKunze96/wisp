//! One server connection.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::Result;
use futures::prelude::*;
use tokio::io::AsyncReadExt;
use tokio::sync::mpsc;
use tsclientlib::data;
use tsclientlib::messages::s2c::InMessage;
use tsclientlib::prelude::*;
use tsclientlib::{
    ChannelId, ClientId, ClientType, Connection, DisconnectOptions, MessageTarget, StreamItem,
    TextMessageTargetMode,
};
use tsproto_packets::packets::{AudioData as AudioPacket, CodecType, OutAudio};

use super::*;
use crate::ConnectionId;
use crate::config::{self, Bookmark, Settings};
use crate::notify::EventKind;

/// How long after the last voice packet someone still counts as talking.
const TALK_HOLD: Duration = Duration::from_millis(300);
/// How often ping / packet loss are refreshed.
const STATS_EVERY: Duration = Duration::from_secs(3);

pub(crate) fn placeholder(con: ConId, b: &Bookmark) -> ServerSnap {
    ServerSnap {
        con,
        bookmark: b.id.clone(),
        label: if b.label.is_empty() { b.address.clone() } else { b.label.clone() },
        status: Status::Connecting,
        server_name: String::new(),
        welcome: String::new(),
        platform: String::new(),
        version: String::new(),
        users: 0,
        max_users: 0,
        channels: Vec::new(),
        own_channel: None,
        ping_ms: None,
        packet_loss: None,
        details: None,
        groups: Vec::new(),
        channel_groups: Vec::new(),
        chats: Vec::new(),
        host_message: None,
        transfers: Vec::new(),
        banner: None,
        host_button: None,
    }
}

/// Fetch a picture into the cache (once per address); calls `done` with the file.
fn fetch_picture(url: String, done: impl FnOnce(PathBuf) + 'static) {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return;
    }
    let name: String = url.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).take(120).collect();
    let path = config::cache_dir().join("banners").join(name);
    tokio::task::spawn_local(async move {
        if !path.exists() {
            let Ok(resp) = reqwest::get(&url).await else { return };
            let Ok(bytes) = resp.bytes().await else { return };
            if image::load_from_memory(&bytes).is_err() {
                return;
            }
            let _ = std::fs::create_dir_all(path.parent().unwrap_or(&path));
            if std::fs::write(&path, &bytes).is_err() {
                return;
            }
        }
        done(path);
    });
}

struct Session {
    con_id: ConId,
    base: ServerSnap,
    settings: Box<Settings>,
    audio: Option<AudioData>,
    back: mpsc::UnboundedSender<SessionMsg>,
    last_voice: HashMap<u16, Instant>,
    /// Who whispered to us lately.
    last_whisper: HashMap<u16, Instant>,
    /// Who was where, to report joins / leaves / moves.
    seen: Option<HashMap<u16, (String, u64)>>,
    icon_dir: PathBuf,
    icons_requested: HashSet<u32>,
    /// (uid, avatar hash) we already asked for.
    avatars_requested: HashSet<(String, String)>,
    /// Our avatar upload (transfer id) and the hash to announce when it's done.
    avatar_upload: Option<(u64, String)>,
    /// What each running file transfer is for.
    transfers_by_handle: HashMap<u16, files::Purpose>,
    transfers: files::Transfers,
    next_transfer: u64,
    inspect: Option<u16>,
    inspect_vars_sent: Option<u16>,
    last_stats: Option<Instant>,
    volumes_applied: HashMap<u16, f32>,
    last_sent: Option<ServerSnap>,
    /// When we last sent mic audio to this server (for our own talking dot).
    last_own_audio: Option<Instant>,
    /// No join/leave reports until then (initial sync).
    quiet_until: Option<Instant>,
    /// Admin commands waiting for the server's answer, with a description.
    pending: HashMap<tsclientlib::MessageHandle, String>,
    /// Properties a channel create can't carry (talk power), sent as an edit
    /// once the new channel shows up: (parent, name, talk power, deadline).
    after_create: Vec<(u64, String, i32, Instant)>,
    /// Our own channel switches waiting for an answer: (channel, sent a password).
    joins: HashMap<tsclientlib::MessageHandle, (u64, bool)>,
    history: chat::History,
    chats: Vec<Conversation>,
    /// We got kicked or banned (no separate "connection lost" then).
    kicked: bool,
    /// Our channel at the last tree update, to tell joins from our own moves.
    own_channel: Option<u64>,
    away: Option<String>,
    /// Bookmark channel still to join, until the deadline.
    default_channel: Option<(String, Instant)>,
    queries: admin::Pending,
    conn_queries: Vec<(tsclientlib::MessageHandle, admin::Reply)>,
    /// Banner and host button pictures, filled in when downloaded.
    #[allow(clippy::type_complexity)]
    pictures: Option<(
        std::rc::Rc<std::cell::RefCell<Option<(PathBuf, String)>>>,
        std::rc::Rc<std::cell::RefCell<Option<(PathBuf, String, String)>>>,
    )>,
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run(
    con_id: ConId,
    bookmark: Bookmark,
    settings: Box<Settings>,
    audio: Option<AudioData>,
    mut rx: mpsc::UnboundedReceiver<SessionCmd>,
    back: mpsc::UnboundedSender<SessionMsg>,
    input_muted: bool,
    output_muted: bool,
    away: Option<String>,
) {
    let base = placeholder(con_id, &bookmark);
    let icon_dir = super::icon_dir(&bookmark.address);
    let mut s = Session {
        con_id,
        base: base.clone(),
        settings,
        audio,
        back: back.clone(),
        last_voice: HashMap::new(),
        last_whisper: HashMap::new(),
        seen: None,
        icon_dir,
        icons_requested: HashSet::new(),
        avatars_requested: HashSet::new(),
        avatar_upload: None,
        transfers_by_handle: HashMap::new(),
        transfers: Default::default(),
        next_transfer: 1,
        inspect: None,
        inspect_vars_sent: None,
        last_stats: None,
        volumes_applied: HashMap::new(),
        last_sent: None,
        last_own_audio: None,
        quiet_until: None,
        pending: HashMap::new(),
        after_create: Vec::new(),
        joins: HashMap::new(),
        history: chat::History::new(&bookmark.address),
        chats: Vec::new(),
        kicked: false,
        own_channel: None,
        away,
        default_channel: None,
        queries: admin::Pending::default(),
        conn_queries: Vec::new(),
        pictures: None,
    };
    if let Err(e) = s.run(&bookmark, &mut rx, input_muted, output_muted).await {
        let was_up = matches!(s.base.status, Status::Connected | Status::Reconnecting);
        if was_up && !s.kicked {
            let server = if s.base.server_name.is_empty() { s.base.label.clone() } else { s.base.server_name.clone() };
            s.event(EventKind::Disconnected, format!("Disconnected from {server}"), e.to_string());
        }
        let mut snap = base;
        snap.status = if format!("{e:?}").contains("ServerInvalidPassword") {
            Status::NeedsPassword
        } else if s.kicked {
            Status::Failed("Kicked from the server".into())
        } else {
            Status::Failed(connect_error(&e))
        };
        let _ = back.send(SessionMsg::Snap(snap));
    }
    let _ = back.send(SessionMsg::Ended(con_id));
}

pub(crate) fn sanitize(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '_' }).collect()
}

impl Session {
    async fn run(
        &mut self,
        b: &Bookmark,
        rx: &mut mpsc::UnboundedReceiver<SessionCmd>,
        input_muted: bool,
        output_muted: bool,
    ) -> Result<()> {
        // same identity on every start, so servers keep our permissions
        let identity = crate::identity::for_bookmark(b.identity.as_deref())?;
        let mut config = Connection::build(b.address.clone())
            .name(if b.nickname.is_empty() { "wisp".to_string() } else { b.nickname.clone() })
            .identity(identity.identity()?)
            .input_muted(input_muted)
            .output_muted(output_muted);
        if !b.password.is_empty() {
            config = config.password(b.password.clone());
        }
        // no default channel in the connect itself: with an existing channel
        // tsclientlib never finishes connecting; join_default() does it after
        let mut con = config.connect()?;

        // Wait for the initial state, still reacting to a disconnect click.
        loop {
            let outcome = {
                let mut first = con
                    .events()
                    .try_filter(|e| future::ready(matches!(e, StreamItem::BookEvents(_))));
                tokio::select! {
                    r = first.next() => match r {
                        Some(r) => { r?; Some(true) }
                        None => anyhow::bail!("Connection closed"),
                    },
                    cmd = rx.recv() => {
                        if matches!(cmd, Some(SessionCmd::Disconnect) | None) { Some(false) } else { None }
                    }
                }
            };
            match outcome {
                Some(true) => break,
                Some(false) => {
                    con.disconnect(DisconnectOptions::new())?;
                    return Ok(());
                }
                None => {}
            }
        }
        con.get_state()?.server.set_subscribed(true).send(&mut con)?;
        if let Some(away) = self.away.clone() {
            con.get_state()?.client_update().set_away(Some(&away)).send(&mut con)?;
        }
        if !b.phonetic.trim().is_empty() {
            con.get_state()?.client_update().set_phonetic_name(b.phonetic.trim()).send(&mut con)?;
        }
        if !b.channel.trim().is_empty() {
            self.default_channel = Some((b.channel.clone(), Instant::now() + Duration::from_secs(5)));
        }
        self.base.status = Status::Connected;
        {
            let server = &con.get_state()?.server;
            let banner = std::rc::Rc::new(std::cell::RefCell::new(None));
            let button = std::rc::Rc::new(std::cell::RefCell::new(None));
            let (b, link) = (banner.clone(), server.hostbanner_url.clone());
            fetch_picture(server.hostbanner_gfx_url.clone(), move |p| *b.borrow_mut() = Some((p, link)));
            let (b, link, tip) = (button.clone(), server.hostbutton_url.clone(), server.hostbutton_tooltip.clone());
            fetch_picture(server.hostbutton_gfx_url.clone(), move |p| *b.borrow_mut() = Some((p, link, tip)));
            self.pictures = Some((banner, button));
        }
        // tsclientlib raises the security level when a server wants more; keep it
        if let Some(raised) = con.get_options().get_identity().map(|i| i.counter()) {
            if raised != identity.counter {
                crate::identity::update_counter(&identity.id, raised);
            }
        }
        {
            use tsclientlib::HostMessageMode as M;
            let server = &con.get_state()?.server;
            let text = strip_bbcode(server.hostmessage.trim());
            match server.hostmessage_mode {
                _ if text.is_empty() => {}
                M::Log => {
                    let _ = self.back.send(SessionMsg::Activity(self.con_id, text, false));
                }
                M::Modal => self.base.host_message = Some((text, 2)),
                M::Modalquit => self.base.host_message = Some((text, 3)),
                M::None => {}
            }
        }
        for (kind, title) in [(ChatKind::Channel, "Channel"), (ChatKind::Server, "Server")] {
            let lines = self.history.load(&kind);
            self.chats.push(Conversation { kind, title: title.into(), client: None, lines, received: 0 });
        }
        // Everyone already here isn't news. Subscribing reveals the other
        // channels' clients over the next moments, so stay quiet for a bit.
        self.quiet_until = Some(Instant::now() + Duration::from_secs(3));
        self.request_icons(&mut con);
        // unread offline messages: say so once
        {
            let (tx, rx) = tokio::sync::oneshot::channel();
            self.queries.send(&mut con, admin::Query::Inbox, tx);
            let (back, con_id) = (self.back.clone(), self.con_id);
            tokio::task::spawn_local(async move {
                if let Ok(Ok(admin::Answer::Inbox(list))) = rx.await {
                    let unread = list.iter().filter(|m| !m.read).count();
                    if unread > 0 {
                        let text = if unread == 1 { "1 unread offline message".to_string() } else { format!("{unread} unread offline messages") };
                        let _ = back.send(SessionMsg::Activity(con_id, format!("{text} (Server admin, Inbox)"), false));
                        let _ = back.send(SessionMsg::Event(con_id, crate::notify::Event {
                            kind: crate::notify::EventKind::PrivateMessage,
                            title: "Offline messages".into(),
                            body: text,
                        }));
                    }
                }
            });
        }

        let mut tick = tokio::time::interval(Duration::from_millis(100));
        loop {
            let mut heard: Vec<(u16, bool)> = Vec::new();
            let mut book_changed = false;
            let mut messages = Vec::new();
            let mut downloads = Vec::new();
            let mut uploads = Vec::new();
            let mut ft_failed = Vec::new();
            let mut reconnecting = None;
            let mut results = Vec::new();
            let mut texts: Vec<(TextMessageTargetMode, u16, String, String, String)> = Vec::new();
            let mut events: Vec<(EventKind, String, String)> = Vec::new();
            let mut kicked = false;
            let own_id = con.get_state()?.own_client;
            let queries = &mut self.queries;
            let blocked: HashSet<String> = self
                .settings
                .contacts
                .iter()
                .filter(|(_, c)| **c == config::Contact::Blocked)
                .map(|(u, _)| u.clone())
                .collect();
            let audio = self.audio.clone();
            let con_key = ConnectionId(self.con_id);
            let stream = con.events().try_for_each(|e| {
                match e {
                    StreamItem::Audio(packet) => {
                        let (from, whisper) = match packet.data().data() {
                            AudioPacket::S2C { from, .. } => (*from, false),
                            AudioPacket::S2CWhisper { from, .. } => (*from, true),
                            _ => return future::ready(Ok(())),
                        };
                        heard.push((from, whisper));
                        if let Some(a) = &audio {
                            let _ = a.ts2a.lock().unwrap().play_packet((con_key, ClientId(from)), packet);
                        }
                    }
                    StreamItem::BookEvents(_) => {
                        book_changed = true;
                        reconnecting = Some(false);
                    }
                    StreamItem::MessageEvent(InMessage::TextMessage(m)) => {
                        for part in m.iter() {
                            let uid = part.invoker_uid.as_ref().map(|u| u.as_ref().to_string()).unwrap_or_default();
                            texts.push((part.target, part.invoker_id.0, part.invoker_name.clone(), uid, part.message.clone()));
                        }
                    }
                    StreamItem::MessageEvent(InMessage::ClientPoke(m)) => {
                        for part in m.iter() {
                            let uid = part.invoker_uid.as_ref().map(|u| u.as_ref().to_string()).unwrap_or_default();
                            if blocked.contains(&uid) {
                                continue;
                            }
                            let text = strip_bbcode(&part.message);
                            messages.push(format!("Poke from {}: {text}", part.invoker_name));
                            events.push((EventKind::Poke, format!("{} poked you", part.invoker_name), text));
                        }
                    }
                    StreamItem::MessageEvent(InMessage::ClientMoved(m)) => {
                        for part in m.iter().filter(|p| p.client_id == own_id && p.invoker_id.is_some_and(|i| i != own_id)) {
                            let by = part.invoker_name.clone().unwrap_or_default();
                            let why = part.reason_message.clone().filter(|r| !r.is_empty()).unwrap_or_default();
                            let title = match part.reason {
                                tsclientlib::Reason::KickChannel => format!("{by} kicked you from the channel"),
                                _ => format!("{by} moved you"),
                            };
                            events.push((EventKind::MovedOrKicked, title, why));
                        }
                    }
                    StreamItem::MessageEvent(InMessage::ClientLeftView(m)) => {
                        for part in m.iter().filter(|p| p.client_id == own_id) {
                            let by = part.invoker_name.clone().unwrap_or_default();
                            let why = part.reason_message.clone().filter(|r| !r.is_empty()).unwrap_or_default();
                            let title = match part.reason {
                                Some(tsclientlib::Reason::KickServer) => format!("{by} kicked you from the server"),
                                Some(tsclientlib::Reason::KickServerBan) => format!("{by} banned you"),
                                _ => continue,
                            };
                            kicked = true;
                            events.push((EventKind::MovedOrKicked, title, why));
                        }
                    }
                    StreamItem::MessageEvent(msg) => {
                        queries.collect(&msg);
                    }
                    StreamItem::FileDownload(handle, result) => downloads.push((handle.0, result)),
                    StreamItem::FileUpload(handle, result) => uploads.push((handle.0, result)),
                    StreamItem::FiletransferFailed(handle, e) => ft_failed.push((handle.0, e.to_string())),
                    StreamItem::DisconnectedTemporarily(_) => reconnecting = Some(true),
                    StreamItem::MessageResult(handle, result) => results.push((handle, result)),
                    _ => {}
                }
                future::ready(Ok(()))
            });

            let mut stop = false;
            tokio::select! {
                r = stream => {
                    self.kicked |= kicked;
                    for (kind, title, body) in events.drain(..) {
                        self.event(kind, title, body);
                    }
                    r?;
                    anyhow::bail!("Disconnected by the server");
                }
                cmd = rx.recv() => match cmd {
                    None | Some(SessionCmd::Disconnect) => stop = true,
                    Some(SessionCmd::Audio(data, whisper)) => {
                        let packet = match &whisper {
                            None => Some(OutAudio::new(&AudioPacket::C2S { id: 0, codec: CodecType::OpusVoice, data: &data })),
                            Some(list) => {
                                let (channels, clients) = whisper_targets(con.get_state()?, list);
                                // nobody to whisper to: say nothing rather than talk to the channel
                                (!channels.is_empty() || !clients.is_empty()).then(|| {
                                    OutAudio::new(&AudioPacket::C2SWhisper {
                                        id: 0,
                                        codec: CodecType::OpusVoice,
                                        channels,
                                        clients,
                                        data: &data,
                                    })
                                })
                            }
                        };
                        if let Some(p) = packet {
                            let _ = con.send_audio(p);
                            self.last_own_audio = Some(Instant::now());
                        }
                    }
                    Some(SessionCmd::Join(id, password)) => {
                        let saved = self.settings.channel_passwords.get(&config::channel_key(&self.base.bookmark, id));
                        let password = password.or_else(|| saved.cloned());
                        let state = con.get_state()?;
                        let mut part = state.clients[&state.own_client].client_move(ChannelId(id));
                        if let Some(pw) = &password {
                            part = part.set_password(pw);
                        }
                        let handle = part.to_packet().send_with_result(&mut con)?;
                        self.joins.insert(handle, (id, password.is_some()));
                    }
                    // the answer to this one lands in our bookkeeping, not in a notify
                    Some(SessionCmd::Query(admin::Query::ServerConnection, reply)) => {
                        let cmd = tsclientlib::messages::c2s::OutServerConnectionInfoRequestMessage::new();
                        match cmd.send_with_result(&mut con) {
                            Ok(h) => self.conn_queries.push((h, reply)),
                            Err(e) => {
                                let _ = reply.send(Err(e.to_string()));
                            }
                        }
                    }
                    Some(SessionCmd::Query(q, reply)) => self.queries.send(&mut con, q, reply),
                    Some(SessionCmd::FetchIcons(ids)) => {
                        let _ = std::fs::create_dir_all(&self.icon_dir);
                        for icon in ids {
                            if self.icon_dir.join(icon.to_string()).exists() || !self.icons_requested.insert(icon) {
                                continue;
                            }
                            if let Ok(h) = con.download_file(ChannelId(0), &format!("/icon_{icon}"), None, None) {
                                let path = self.icon_dir.join(icon.to_string());
                                self.transfers_by_handle.insert(h.0, files::Purpose::Icon(path, None));
                            }
                        }
                    }
                    Some(SessionCmd::Files(cmd)) => {
                        if let Err(e) = self.file_cmd(&mut con, cmd) {
                            messages.push(format!("Couldn't do that: {e}"));
                        }
                    }
                    Some(SessionCmd::ServerInfo(reply)) => {
                        let _ = reply.send(admin::server_info(con.get_state()?));
                    }
                    Some(SessionCmd::Write(w)) => match admin::write(&mut con, &w) {
                        Ok((handle, what)) => {
                            self.pending.insert(handle, what);
                        }
                        Err(e) => messages.push(format!("Couldn't do that: {e}")),
                    },
                    Some(SessionCmd::SetAvatar(avatar)) => match avatar {
                        Some((png, hash)) => {
                            let size = std::fs::metadata(&png).map(|m| m.len()).unwrap_or(0);
                            let id = self.next_transfer;
                            self.next_transfer += 1;
                            self.transfers.borrow_mut().push(files::Transfer {
                                id,
                                name: "Avatar".into(),
                                upload: true,
                                size,
                                done: 0,
                                local: png,
                                state: files::TransferState::Waiting,
                            });
                            let h = con.upload_file(ChannelId(0), "/avatar", None, size, true, false)?;
                            self.transfers_by_handle.insert(h.0, files::Purpose::Transfer(id));
                            self.avatar_upload = Some((id, hash));
                        }
                        None => {
                            con.get_state()?.client_update().set_avatar_hash("").send(&mut con)?;
                            let del = tsclientlib::messages::c2s::OutDeleteFilePart {
                                channel_id: ChannelId(0),
                                channel_password: "".into(),
                                name: "/avatar".into(),
                            };
                            let _ = tsclientlib::messages::c2s::OutDeleteFileMessage::new(&mut std::iter::once(del)).send(&mut con);
                        }
                    },
                    Some(SessionCmd::Recording(on)) => {
                        con.get_state()?.client_update().set_is_recording(on).send(&mut con)?;
                    }
                    Some(SessionCmd::Away(away)) => {
                        self.away = away.clone();
                        con.get_state()?.client_update().set_away(away.as_deref()).send(&mut con)?;
                    }
                    Some(SessionCmd::Me(action)) => {
                        let state = con.get_state()?;
                        let cmd = match &action {
                            SelfAction::Commander(on) => state.client_update().set_is_channel_commander(*on).to_packet(),
                            SelfAction::TalkRequest(msg) => state.client_update().set_talk_power_request(msg.as_deref()).to_packet(),
                            SelfAction::Description(d) => state.clients[&state.own_client].edit().set_description(d).to_packet(),
                        };
                        let what = match action {
                            SelfAction::Commander(true) => "Became channel commander",
                            SelfAction::Commander(false) => "Stopped being channel commander",
                            SelfAction::TalkRequest(Some(_)) => "Requested talk power",
                            SelfAction::TalkRequest(None) => "Withdrew the talk power request",
                            SelfAction::Description(_) => "Updated your description",
                        };
                        let handle = cmd.send_with_result(&mut con)?;
                        self.pending.insert(handle, what.to_string());
                    }
                    Some(SessionCmd::SelfState { input_muted, output_muted }) => {
                        con.get_state()?
                            .client_update()
                            .set_input_muted(input_muted)
                            .set_output_muted(output_muted)
                            .send(&mut con)?;
                    }
                    Some(SessionCmd::Settings(s)) => {
                        self.settings = s;
                        self.volumes_applied.clear();
                    }
                    Some(SessionCmd::Inspect(id)) => {
                        self.inspect = id;
                        self.inspect_vars_sent = None;
                        self.last_stats = None;
                    }
                    Some(SessionCmd::ChannelDescription(id)) => {
                        if let Some(c) = con.get_state()?.channels.get(&ChannelId(id)) {
                            let _ = c.description().to_packet().send(&mut con);
                        }
                    }
                    Some(SessionCmd::Admin(action)) => {
                        if let AdminAction::CreateChannel { parent, props } = &action {
                            if let (Some(name), Some(tp)) = (&props.name, props.talk_power.filter(|t| *t != 0)) {
                                let deadline = Instant::now() + Duration::from_secs(10);
                                self.after_create.push((*parent, name.clone(), tp, deadline));
                            }
                        }
                        match admin(&mut con, &action) {
                            Ok((handle, what)) => { self.pending.insert(handle, what); }
                            Err(e) => messages.push(format!("Couldn't do that: {e}")),
                        }
                    }
                    Some(SessionCmd::SendText(kind, text)) => {
                        let state = con.get_state()?;
                        let target = match &kind {
                            ChatKind::Channel => Some(MessageTarget::Channel),
                            ChatKind::Server => Some(MessageTarget::Server),
                            ChatKind::Private(uid) => state
                                .clients
                                .values()
                                .find(|c| uid_string(c) == *uid)
                                .map(|c| MessageTarget::Client(c.id)),
                        };
                        match target {
                            Some(target) => {
                                state.send_message(target, &text).send(&mut con)?;
                                let state = con.get_state()?;
                                let me = state.clients.get(&state.own_client);
                                let (name, uid) = me.map(|c| (c.name.clone(), uid_string(c))).unwrap_or_default();
                                self.add_line(kind, None, chat::now_line(name, uid, true, text, false));
                            }
                            None => messages.push("Couldn't send: they're offline".into()),
                        }
                    }
                    Some(SessionCmd::OpenChat(client)) => {
                        let state = con.get_state()?;
                        if let Some(c) = state.clients.get(&ClientId(client)) {
                            self.conversation(ChatKind::Private(uid_string(c)), &c.name);
                        }
                    }
                    Some(SessionCmd::CloseChat(kind)) => {
                        // tell them (the official client shows "chat closed"); tsclientlib
                        // has no ready-made message for it
                        if let ChatKind::Private(uid) = &kind {
                            let state = con.get_state()?;
                            if let Some(c) = state.clients.values().find(|c| uid_string(c) == *uid) {
                                use tsproto_packets::packets::{Direction, Flags, OutCommand, PacketType};
                                let mut cmd = OutCommand::new(Direction::C2S, Flags::empty(), PacketType::Command, "clientchatclosed");
                                cmd.write_arg("clid", &c.id.0);
                                cmd.write_arg("cluid", uid);
                                let _ = cmd.send(&mut con);
                            }
                        }
                        self.chats.retain(|c| c.kind != kind || !matches!(kind, ChatKind::Private(_)));
                    }
                },
                _ = tick.tick() => {}
            }
            if stop {
                break;
            }

            self.kicked |= kicked;
            for (kind, title, body) in events {
                self.event(kind, title, body);
            }
            match reconnecting {
                Some(true) => self.base.status = Status::Reconnecting,
                Some(false) => self.base.status = Status::Connected,
                None => {}
            }
            let now = Instant::now();
            for (from, whisper) in heard {
                self.last_voice.insert(from, now);
                if whisper {
                    self.last_whisper.insert(from, now);
                }
            }
            results.retain(|(handle, result)| !self.queries.finish(*handle, result));
            for (handle, result) in &results {
                if let Some(i) = self.conn_queries.iter().position(|(h, _)| h == handle) {
                    let (_, reply) = self.conn_queries.remove(i);
                    let answer = match (result, con.get_state()?.server.connection_data.as_ref()) {
                        (Ok(()), Some(c)) => Ok(admin::Answer::ServerConnection(admin::ServerConnection {
                            uptime_secs: c.connected_time_total.whole_seconds().max(0) as u64,
                            ping_ms: c.ping.as_seconds_f32() * 1000.0,
                            packet_loss: c.packetloss_total * 100.0,
                            sent_per_sec: c.bandwidth_sent_last_second_total,
                            received_per_sec: c.bandwidth_received_last_second_total,
                            sent_per_min: c.bandwidth_sent_last_minute_total,
                            received_per_min: c.bandwidth_received_last_minute_total,
                            sent_total: c.bytes_sent_total,
                            received_total: c.bytes_received_total,
                            files_sent_total: c.filetransfer_bytes_sent_total,
                            files_received_total: c.filetransfer_bytes_received_total,
                        })),
                        (Ok(()), None) => Err("the server sent nothing".to_string()),
                        (Err(e), _) => Err(plain_error(&e.to_string())),
                    };
                    let _ = reply.send(answer);
                }
            }
            for (handle, result) in &results {
                let Some((channel, sent_pw)) = self.joins.remove(handle) else { continue };
                if let Err(e) = result {
                    let name = con.get_state()?.channels.get(&ChannelId(channel)).map(|c| c.name.clone()).unwrap_or_default();
                    if format!("{e:?}").contains("ChannelInvalidPassword") {
                        let _ = self.back.send(SessionMsg::NeedChannelPassword(self.con_id, channel, name, sent_pw));
                    } else {
                        let _ = self.back.send(SessionMsg::Activity(self.con_id, failed(&format!("Joined {name}"), &e.to_string()), false));
                    }
                }
            }
            for (handle, result) in results {
                if let Some(what) = self.pending.remove(&handle) {
                    let line = match result {
                        Ok(()) if what.is_empty() => continue,
                        Ok(()) => what,
                        Err(e) => failed(&what, &e.to_string()),
                    };
                    let _ = self.back.send(SessionMsg::Activity(self.con_id, line, false));
                }
            }
            for m in messages {
                let _ = self.back.send(SessionMsg::Activity(self.con_id, m, true));
            }
            if !texts.is_empty() {
                let state = con.get_state()?;
                let own = state.own_client.0;
                let my_name = state.clients.get(&state.own_client).map(|c| c.name.to_lowercase()).unwrap_or_default();
                for (target, from, name, uid, text) in texts {
                    // our own messages come back from the server; we add them when sending
                    if from == own || self.settings.is_blocked(&uid) {
                        continue;
                    }
                    let kind = match target {
                        TextMessageTargetMode::Server => ChatKind::Server,
                        TextMessageTargetMode::Channel => ChatKind::Channel,
                        _ => ChatKind::Private(uid.clone()),
                    };
                    let mention = !my_name.is_empty() && text.to_lowercase().contains(&my_name);
                    let plain = strip_bbcode(&text);
                    match (&kind, mention) {
                        (ChatKind::Private(_), _) => self.event(EventKind::PrivateMessage, name.clone(), plain),
                        (_, true) => self.event(EventKind::Mention, format!("{name} mentioned you"), plain),
                        _ => self.event(EventKind::ChannelMessage, name.clone(), plain),
                    }
                    let title = name.clone();
                    self.add_line(kind, Some(&title), chat::now_line(name, uid, false, text, mention));
                }
            }
            for (handle, result) in downloads {
                match self.transfers_by_handle.remove(&handle) {
                    Some(files::Purpose::Icon(path, marker)) => {
                        tokio::task::spawn_local(async move {
                            let mut stream = result.stream;
                            let mut buf = Vec::with_capacity(result.size as usize);
                            if stream.read_to_end(&mut buf).await.is_ok() && !buf.is_empty() {
                                let _ = std::fs::write(&path, buf);
                                if let Some((marker, hash)) = marker {
                                    let _ = std::fs::write(marker, hash);
                                }
                            }
                        });
                    }
                    Some(files::Purpose::Transfer(id)) => {
                        let to = self.transfers.borrow().iter().find(|t| t.id == id).map(|t| t.local.clone());
                        if let Some(to) = to {
                            files::run_download(self.transfers.clone(), id, result.size, result.stream, to);
                        }
                    }
                    None => {}
                }
            }
            for (handle, result) in uploads {
                if let Some(files::Purpose::Transfer(id)) = self.transfers_by_handle.remove(&handle) {
                    let from = self.transfers.borrow().iter().find(|t| t.id == id).map(|t| t.local.clone());
                    if let Some(from) = from {
                        files::run_upload(self.transfers.clone(), id, result.seek_position, result.stream, from);
                    }
                }
            }
            for (handle, e) in ft_failed {
                if let Some(files::Purpose::Transfer(id)) = self.transfers_by_handle.remove(&handle) {
                    if let Some(t) = self.transfers.borrow_mut().iter_mut().find(|t| t.id == id) {
                        t.state = files::TransferState::Failed(plain_error(&e));
                    }
                }
            }
            if book_changed {
                self.finish_creates(&mut con);
                self.report_changes(con.get_state()?);
                self.request_icons(&mut con);
                self.request_avatars(&mut con);
            }
            if let Some((path, deadline)) = self.default_channel.clone() {
                if self.join_default(&mut con, &path, now >= deadline)? {
                    self.default_channel = None;
                }
            }
            // our avatar is up: tell everyone its hash
            if let Some((id, hash)) = self.avatar_upload.clone() {
                let state = self.transfers.borrow().iter().find(|t| t.id == id).map(|t| t.state.clone());
                match state {
                    Some(files::TransferState::Done) => {
                        con.get_state()?.client_update().set_avatar_hash(&hash).send(&mut con)?;
                        self.avatar_upload = None;
                    }
                    Some(files::TransferState::Failed(_)) | None => self.avatar_upload = None,
                    _ => {}
                }
            }
            self.refresh_stats(&mut con);
            self.apply_volumes(con.get_state()?);
            self.last_voice.retain(|_, t| now.duration_since(*t) < TALK_HOLD);
            self.last_whisper.retain(|_, t| now.duration_since(*t) < TALK_HOLD);
            let snap = self.snapshot(&con)?;
            if self.last_sent.as_ref() != Some(&snap) {
                self.last_sent = Some(snap.clone());
                let _ = self.back.send(SessionMsg::Snap(snap));
            }
        }

        con.disconnect(DisconnectOptions::new())?;
        let _ = tokio::time::timeout(Duration::from_secs(2), con.events().for_each(|_| future::ready(()))).await;
        Ok(())
    }

    /// The server ignores a default channel it can't put us in (password, full,
    /// wrong path). Find it ourselves and join, so a refusal shows up
    /// (password prompt) instead of silently landing in the lobby.
    /// Returns whether it's settled (joined, already there, or given up).
    fn join_default(&mut self, con: &mut Connection, path: &str, last_try: bool) -> Result<bool> {
        let path = path.trim().trim_matches('/');
        if path.is_empty() {
            return Ok(true);
        }
        let state = con.get_state()?;
        let Some(target) = find_channel(state, path) else {
            // the channel list may still be arriving
            if last_try {
                let line = format!("Couldn't find the channel {path:?}");
                let _ = self.back.send(SessionMsg::Activity(self.con_id, line, false));
            }
            return Ok(last_try);
        };
        let me = &state.clients[&state.own_client];
        if me.channel == target {
            return Ok(true);
        }
        let saved = self.settings.channel_passwords.get(&config::channel_key(&self.base.bookmark, target.0)).cloned();
        let mut part = me.client_move(target);
        if let Some(pw) = &saved {
            part = part.set_password(pw);
        }
        let handle = part.to_packet().send_with_result(con)?;
        self.joins.insert(handle, (target.0, saved.is_some()));
        Ok(true)
    }

    fn file_cmd(&mut self, con: &mut Connection, cmd: files::FileCmd) -> Result<()> {
        use files::FileCmd as F;
        let new_transfer = |s: &mut Self, name: &str, upload: bool, size: u64, local: std::path::PathBuf| {
            let id = s.next_transfer;
            s.next_transfer += 1;
            s.transfers.borrow_mut().push(files::Transfer {
                id,
                name: name.to_string(),
                upload,
                size,
                done: 0,
                local,
                state: files::TransferState::Waiting,
            });
            id
        };
        let pw = |p: &str| (!p.is_empty()).then(|| p.to_string());
        match cmd {
            F::Download { channel, password, path, to } => {
                let name = path.rsplit('/').next().unwrap_or(&path).to_string();
                let id = new_transfer(self, &name, false, 0, to);
                let h = con.download_file(ChannelId(channel), &path, pw(&password).as_deref(), None)?;
                self.transfers_by_handle.insert(h.0, files::Purpose::Transfer(id));
            }
            F::Upload { channel, password, path, from } => {
                let size = std::fs::metadata(&from)?.len();
                let name = path.rsplit('/').next().unwrap_or(&path).to_string();
                let id = new_transfer(self, &name, true, size, from);
                let h = con.upload_file(ChannelId(channel), &path, pw(&password).as_deref(), size, true, false)?;
                self.transfers_by_handle.insert(h.0, files::Purpose::Transfer(id));
            }
            F::ClearDone => {
                self.transfers
                    .borrow_mut()
                    .retain(|t| matches!(t.state, files::TransferState::Waiting | files::TransferState::Running));
            }
            other => {
                let state = con.get_state()?;
                let (channel, what, cmd) = match &other {
                    F::Delete { channel, password, path } => {
                        let c = state.channels.get(&ChannelId(*channel)).ok_or_else(|| anyhow::anyhow!("channel is gone"))?;
                        (c.name.clone(), format!("Deleted {path}"), c.delete_file(password, path).to_packet())
                    }
                    F::Mkdir { channel, password, path } => {
                        let c = state.channels.get(&ChannelId(*channel)).ok_or_else(|| anyhow::anyhow!("channel is gone"))?;
                        (c.name.clone(), format!("Created folder {path}"), c.create_directory(password, path).to_packet())
                    }
                    F::Rename { channel, password, from, to } => {
                        let c = state.channels.get(&ChannelId(*channel)).ok_or_else(|| anyhow::anyhow!("channel is gone"))?;
                        (c.name.clone(), format!("Renamed {from} to {to}"), c.rename_file(password, from, to).to_packet())
                    }
                    _ => unreachable!(),
                };
                let handle = cmd.send_with_result(con)?;
                self.pending.insert(handle, format!("{what} in {channel}"));
            }
        }
        Ok(())
    }

    /// The conversation of this kind, created (with its history) if needed.
    fn conversation(&mut self, kind: ChatKind, title: &str) -> &mut Conversation {
        if let Some(i) = self.chats.iter().position(|c| c.kind == kind) {
            return &mut self.chats[i];
        }
        let lines = self.history.load(&kind);
        self.chats.push(Conversation { kind, title: title.to_string(), client: None, lines, received: 0 });
        self.chats.last_mut().unwrap()
    }

    fn add_line(&mut self, kind: ChatKind, title: Option<&str>, line: ChatLine) {
        self.history.append(&kind, &line);
        let conv = self.conversation(kind, title.unwrap_or_default());
        if let (Some(t), ChatKind::Private(_)) = (title, &conv.kind) {
            conv.title = t.to_string();
        }
        conv.lines.push(line);
        conv.received += 1;
        let extra = conv.lines.len().saturating_sub(chat::KEEP);
        conv.lines.drain(..extra);
    }

    fn finish_creates(&mut self, con: &mut Connection) {
        let now = Instant::now();
        self.after_create.retain(|(.., deadline)| *deadline > now);
        if self.after_create.is_empty() {
            return;
        }
        let Ok(state) = con.get_state() else { return };
        let mut cmds = Vec::new();
        self.after_create.retain(|(parent, name, tp, _)| {
            match state.channels.values().find(|c| c.parent.0 == *parent && c.name == *name) {
                Some(c) => {
                    cmds.push(c.edit().set_needed_talk_power(*tp).to_packet());
                    false
                }
                None => true,
            }
        });
        for c in cmds {
            let _ = c.send(con);
        }
    }

    fn event(&self, kind: EventKind, title: String, body: String) {
        let _ = self.back.send(SessionMsg::Event(self.con_id, crate::notify::Event { kind, title, body }));
    }

    fn report_changes(&mut self, state: &data::Connection) {
        let own = state.clients.get(&state.own_client).map(|c| c.channel.0);
        let we_moved = own != self.own_channel;
        self.own_channel = own;
        let now: HashMap<u16, (String, u64)> = state
            .clients
            .values()
            .filter(|c| c.client_type == ClientType::Normal)
            .map(|c| (c.id.0, (c.name.clone(), c.channel.0)))
            .collect();
        let channel = |id: u64| state.channels.get(&ChannelId(id)).map(|c| c.name.clone()).unwrap_or_default();
        let quiet = self.quiet_until.is_some_and(|t| Instant::now() < t);
        if let (Some(prev), false) = (&self.seen, quiet) {
            let mut lines = Vec::new();
            let mut joined = Vec::new();
            let mut left = Vec::new();
            let mut friends = Vec::new();
            for (id, (name, ch)) in &now {
                let old = prev.get(id).map(|(_, c)| *c);
                match old {
                    None => {
                        lines.push(format!("{name} connected ({})", channel(*ch)));
                        let uid = state.clients.get(&ClientId(*id)).map(uid_string).unwrap_or_default();
                        if self.settings.is_friend(&uid) {
                            friends.push(name.clone());
                        }
                    }
                    Some(old) if old != *ch => lines.push(format!("{name} moved to {}", channel(*ch))),
                    _ => {}
                }
                if *id == state.own_client.0 || we_moved || old == Some(*ch) {
                    continue;
                }
                if Some(*ch) == own {
                    joined.push(name.clone());
                } else if old.is_some() && old == own {
                    left.push(name.clone());
                }
            }
            for (id, (name, old)) in prev {
                if !now.contains_key(id) {
                    lines.push(format!("{name} disconnected"));
                    if !we_moved && Some(*old) == own {
                        left.push(name.clone());
                    }
                }
            }
            let here = own.map(channel).unwrap_or_default();
            let server = if state.server.name.is_empty() { self.base.label.clone() } else { state.server.name.clone() };
            for name in friends {
                self.event(EventKind::FriendOnline, format!("{name} is online"), server.clone());
            }
            for name in joined {
                self.event(EventKind::UserJoined, format!("{name} joined {here}"), String::new());
            }
            for name in left {
                self.event(EventKind::UserLeft, format!("{name} left {here}"), String::new());
            }
            for l in lines {
                let _ = self.back.send(SessionMsg::Activity(self.con_id, l, false));
            }
        }
        self.seen = Some(now);
    }

    /// Download custom group icons we don't have yet (ids < 1000 are built in).
    fn request_icons(&mut self, con: &mut Connection) {
        let Ok(state) = con.get_state() else { return };
        let wanted: Vec<u32> = state
            .server_groups
            .values()
            .map(|g| g.icon.0)
            .chain(state.channel_groups.values().map(|g| g.icon.0))
            .chain(state.channels.values().filter_map(|c| c.icon.map(|i| i.0)))
            .chain(state.clients.values().map(|c| c.icon.0))
            .filter(|&i| i >= 1000 && !self.icons_requested.contains(&i))
            .collect();
        let _ = std::fs::create_dir_all(&self.icon_dir);
        for icon in wanted {
            self.icons_requested.insert(icon);
            if self.icon_dir.join(icon.to_string()).exists() {
                continue;
            }
            if let Ok(h) = con.download_file(ChannelId(0), &format!("/icon_{icon}"), None, None) {
                self.transfers_by_handle.insert(h.0, files::Purpose::Icon(self.icon_dir.join(icon.to_string()), None));
            }
        }
    }

    fn avatar_paths(&self, c: &data::Client) -> Option<(PathBuf, PathBuf)> {
        let uid = c.uid.as_ref()?;
        let name = uid.as_avatar();
        let dir = self.icon_dir.parent()?.parent()?.join("avatars").join(self.icon_dir.file_name()?);
        Some((dir.join(&name), dir.join(format!("{name}.hash"))))
    }

    /// The cached picture, when it's the one their hash says.
    fn avatar_of(&self, c: &data::Client) -> Option<PathBuf> {
        if c.avatar_hash.is_empty() {
            return None;
        }
        let (file, marker) = self.avatar_paths(c)?;
        (std::fs::read_to_string(marker).ok()? == c.avatar_hash && file.exists()).then_some(file)
    }

    /// Download avatars we don't have (or that changed).
    fn request_avatars(&mut self, con: &mut Connection) {
        let Ok(state) = con.get_state() else { return };
        let mut wanted = Vec::new();
        for c in state.clients.values().filter(|c| !c.avatar_hash.is_empty() && c.client_type == ClientType::Normal) {
            let Some((file, marker)) = self.avatar_paths(c) else { continue };
            let key = (uid_string(c), c.avatar_hash.clone());
            if self.avatars_requested.contains(&key) || self.avatar_of(c).is_some() {
                continue;
            }
            self.avatars_requested.insert(key);
            let remote = format!("/avatar_{}", c.uid.as_ref().map(|u| u.as_avatar()).unwrap_or_default());
            wanted.push((remote, file, marker, c.avatar_hash.clone()));
        }
        for (remote, file, marker, hash) in wanted {
            if let Some(dir) = file.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            if let Ok(h) = con.download_file(ChannelId(0), &remote, None, None) {
                self.transfers_by_handle.insert(h.0, files::Purpose::Icon(file, Some((marker, hash))));
            }
        }
    }

    fn refresh_stats(&mut self, con: &mut Connection) {
        if self.last_stats.is_some_and(|t| t.elapsed() < STATS_EVERY) {
            return;
        }
        self.last_stats = Some(Instant::now());
        let Ok(state) = con.get_state() else { return };
        let mut cmds = Vec::new();
        if let Some(me) = state.clients.get(&state.own_client) {
            cmds.push(me.connection_info().to_packet());
        }
        if let Some(c) = self.inspect.and_then(|id| state.clients.get(&ClientId(id))) {
            cmds.push(c.connection_info().to_packet());
            if self.inspect_vars_sent != self.inspect {
                cmds.push(c.variables().to_packet());
                self.inspect_vars_sent = self.inspect;
            }
        }
        for c in cmds {
            let _ = c.send(con);
        }
    }

    fn apply_volumes(&mut self, state: &data::Connection) {
        let Some(a) = &self.audio else { return };
        for c in state.clients.values() {
            let uid = uid_string(c);
            let v = self.settings.user_volume(&uid);
            if self.volumes_applied.get(&c.id.0) != Some(&v) {
                a.ts2a.lock().unwrap().set_volume((ConnectionId(self.con_id), c.id), v);
                self.volumes_applied.insert(c.id.0, v);
            }
        }
    }

    fn badges(&self, state: &data::Connection, c: &data::Client) -> Vec<GroupBadge> {
        let mut groups: Vec<_> = c.server_groups.iter().filter_map(|g| state.server_groups.get(g)).collect();
        groups.sort_by_key(|g| g.sort_id);
        let mut out: Vec<GroupBadge> = groups.iter().map(|g| self.badge(&g.name, g.icon.0)).collect();
        if let Some(cg) = state.channel_groups.get(&c.channel_group) {
            if cg.icon.0 != 0 {
                out.push(self.badge(&cg.name, cg.icon.0));
            }
        }
        out
    }

    fn badge(&self, name: &str, icon: u32) -> GroupBadge {
        let path = self.icon_dir.join(icon.to_string());
        GroupBadge {
            name: name.to_string(),
            icon: (icon >= 1000 && path.exists()).then_some(path),
            builtin: (icon != 0 && icon < 1000).then_some(icon),
        }
    }

    fn snapshot(&self, con: &Connection) -> Result<ServerSnap> {
        let state = con.get_state()?;
        let transmitting = self.last_own_audio.is_some_and(|t| t.elapsed() < TALK_HOLD);
        let mut channels = Vec::new();
        self.push_children(state, ChannelId(0), 0, transmitting, &mut channels);

        let me = state.clients.get(&state.own_client);
        let own_cd = me.and_then(|c| c.connection_data.as_ref());
        let ping_ms = con
            .get_network_stats()
            .ok()
            .map(|s| s.rtt.as_secs_f32() * 1000.0)
            .filter(|p| *p > 0.0);
        let packet_loss = own_cd.map(|d| {
            d.client_to_server_packetloss_total.max(d.server_to_client_packetloss_total.unwrap_or(0.0)) * 100.0
        });

        let details = self.inspect.and_then(|id| state.clients.get(&ClientId(id))).map(|c| {
            let cd = c.connection_data.as_ref();
            let od = c.optional_data.as_ref();
            ClientDetails {
                id: c.id.0,
                name: c.name.clone(),
                uid: uid_string(c),
                description: c.description.clone(),
                country: c.country_code.clone(),
                version: od.map(|o| o.version.clone()).unwrap_or_default(),
                platform: od.map(|o| o.platform.clone()).unwrap_or_default(),
                groups: self.badges(state, c),
                ping_ms: cd.and_then(|d| d.ping).map(|p| p.as_seconds_f32() * 1000.0),
                packet_loss: cd.map(|d| {
                    d.client_to_server_packetloss_total.max(d.server_to_client_packetloss_total.unwrap_or(0.0))
                        * 100.0
                }),
                connected_secs: cd.and_then(|d| d.connected_time).map(|d| d.as_seconds_f32() as u64),
                idle_secs: cd.map(|d| d.idle_time.as_seconds_f32() as u64).unwrap_or(0),
                away: c.away_message.clone(),
            }
        });

        Ok(ServerSnap {
            status: self.base.status.clone(),
            server_name: state.server.name.clone(),
            welcome: strip_bbcode(&state.server.welcome_message),
            platform: state.server.platform.clone(),
            version: state.server.version.clone(),
            users: state.clients.values().filter(|c| c.client_type == ClientType::Normal).count(),
            max_users: state.server.max_clients,
            own_channel: me.map(|c| c.channel.0),
            channels,
            ping_ms,
            packet_loss,
            details,
            groups: {
                let mut g: Vec<_> = state
                    .server_groups
                    .values()
                    .filter(|g| g.group_type == tsclientlib::GroupType::Regular)
                    .collect();
                g.sort_by_key(|g| (g.sort_id, g.id.0));
                g.iter().map(|g| ServerGroupInfo { id: g.id.0, badge: self.badge(&g.name, g.icon.0), icon_id: g.icon.0 }).collect()
            },
            channel_groups: {
                let mut g: Vec<_> = state
                    .channel_groups
                    .values()
                    .filter(|g| g.group_type == tsclientlib::GroupType::Regular)
                    .collect();
                g.sort_by_key(|g| (g.sort_id, g.id.0));
                g.iter().map(|g| ServerGroupInfo { id: g.id.0, badge: self.badge(&g.name, g.icon.0), icon_id: g.icon.0 }).collect()
            },
            chats: self
                .chats
                .iter()
                .map(|c| {
                    let mut c = c.clone();
                    if let ChatKind::Private(uid) = &c.kind {
                        let online = state.clients.values().find(|cl| uid_string(cl) == *uid);
                        c.client = online.map(|cl| cl.id.0);
                        if let Some(cl) = online {
                            c.title = cl.name.clone();
                        }
                    }
                    c
                })
                .collect(),
            transfers: self.transfers.borrow().clone(),
            banner: self.pictures.as_ref().and_then(|(b, _)| b.borrow().clone()),
            host_button: self.pictures.as_ref().and_then(|(_, b)| b.borrow().clone()),
            ..self.base.clone()
        })
    }

    /// Channels in server order: each channel's `order` is the id of the one above.
    fn push_children(
        &self,
        state: &data::Connection,
        parent: ChannelId,
        depth: usize,
        transmitting: bool,
        out: &mut Vec<ChannelRow>,
    ) {
        for ch in children(state, parent) {
            let mut clients: Vec<_> = state
                .clients
                .values()
                .filter(|cl| cl.channel == ch.id && cl.client_type == ClientType::Normal)
                .map(|cl| {
                    let uid = uid_string(cl);
                    let me = cl.id == state.own_client;
                    ClientRow {
                        id: cl.id.0,
                        name: cl.name.clone(),
                        talking: if me { transmitting } else { self.last_voice.contains_key(&cl.id.0) },
                        input_muted: cl.input_muted || !cl.input_hardware_enabled,
                        output_muted: cl.output_muted || !cl.output_hardware_enabled,
                        away: cl.away_message.clone(),
                        me,
                        commander: cl.is_channel_commander,
                        priority: cl.is_priority_speaker,
                        recording: cl.is_recording,
                        country: cl.country_code.clone(),
                        groups: self.badges(state, cl),
                        group_ids: cl.server_groups.iter().map(|g| g.0).collect(),
                        channel_group: cl.channel_group.0,
                        talk_power_granted: cl.talk_power_granted,
                        db_id: cl.database_id.0,
                        talk_request: cl.talk_power_request.as_ref().map(|r| r.message.clone()),
                        avatar: self.avatar_of(cl),
                        friend: self.settings.is_friend(&uid),
                        blocked: self.settings.is_blocked(&uid),
                        whispering: self.last_whisper.contains_key(&cl.id.0),
                        icon: (cl.icon.0 != 0).then(|| self.badge("", cl.icon.0)),
                        volume: self.settings.user_volumes.get(&uid).copied().unwrap_or(1.0),
                        locally_muted: self.settings.user_muted.contains(&uid),
                        uid,
                    }
                })
                .collect();
            // Talk power first, then by name, like the official client.
            clients.sort_by(|a, b| {
                let tp = |r: &ClientRow| state.clients.get(&ClientId(r.id)).map(|c| -c.talk_power).unwrap_or(0);
                tp(a).cmp(&tp(b)).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            });
            out.push(ChannelRow {
                id: ch.id.0,
                parent: ch.parent.0,
                name: ch.name.clone(),
                phonetic: ch.phonetic_name.clone().unwrap_or_default(),
                topic: ch.topic.clone().unwrap_or_default(),
                description: ch.optional_data.as_ref().map(|o| o.description.clone()),
                depth,
                has_password: ch.has_password.unwrap_or(false),
                is_default: ch.is_default.unwrap_or(false),
                kind: ch.channel_type,
                codec: ch.codec,
                quality: ch.codec_quality.unwrap_or(6),
                talk_power: ch.needed_talk_power.unwrap_or(0),
                delete_delay: ch.delete_delay.map(|d| d.whole_seconds().max(0) as u64).unwrap_or(0),
                latency: ch.codec_latency_factor.unwrap_or(1),
                unencrypted: ch.is_unencrypted.unwrap_or(false),
                subscribed: ch.subscribed,
                max_clients: match ch.max_clients {
                    Some(MaxClients::Limited(n)) => Some(u32::from(n)),
                    _ => None,
                },
                icon: ch.icon.filter(|i| i.0 != 0).map(|i| self.badge(&ch.name, i.0)),
                icon_id: ch.icon.map(|i| i.0).unwrap_or(0),
                limit: ch.max_clients.unwrap_or(MaxClients::Unlimited),
                family_limit: ch.max_family_clients.unwrap_or(MaxClients::Inherited),
                clients,
            });
            self.push_children(state, ch.id, depth + 1, transmitting, out);
        }
    }
}

/// A channel's sub-channels in server order: each `order` is the id of the
/// channel above it.
fn children(state: &data::Connection, parent: ChannelId) -> Vec<&data::Channel> {
    let mut siblings: Vec<_> = state.channels.values().filter(|c| c.parent == parent).collect();
    let mut ordered = Vec::with_capacity(siblings.len());
    let mut prev = ChannelId(0);
    while let Some(i) = siblings.iter().position(|c| c.order == prev) {
        let c = siblings.remove(i);
        prev = c.id;
        ordered.push(c);
    }
    siblings.sort_by_key(|c| c.id.0);
    ordered.extend(siblings);
    ordered
}

/// A channel by path ("Lobby/Games", any case) or, for a single name, the
/// first channel anywhere with that name.
fn find_channel(state: &data::Connection, path: &str) -> Option<ChannelId> {
    let path = path.trim().trim_matches('/');
    let mut parent = ChannelId(0);
    let mut found = None;
    for part in path.split('/').map(str::trim) {
        match children(state, parent).into_iter().find(|c| c.name.eq_ignore_ascii_case(part)) {
            Some(c) => {
                parent = c.id;
                found = Some(c.id);
            }
            None => {
                found = None;
                break;
            }
        }
    }
    if found.is_none() && !path.contains('/') {
        found = state.channels.values().find(|c| c.name.eq_ignore_ascii_case(path)).map(|c| c.id);
    }
    found
}

/// Channel and client ids a whisper list reaches on this server right now.
fn whisper_targets(state: &data::Connection, list: &crate::config::WhisperList) -> (Vec<u64>, Vec<u16>) {
    use crate::config::WhisperTarget as T;
    let own = state.own_client;
    let own_channel = state.clients.get(&own).map(|c| c.channel).unwrap_or(ChannelId(0));
    let mut channels = Vec::new();
    let mut clients = Vec::new();
    for t in &list.targets {
        match t {
            T::Channel { path } => channels.extend(find_channel(state, path).map(|c| c.0)),
            T::Client { uid, .. } => {
                clients.extend(state.clients.values().filter(|c| uid_string(c) == *uid).map(|c| c.id.0))
            }
            T::ServerGroup { name } => {
                let groups: Vec<_> =
                    state.server_groups.values().filter(|g| g.name.eq_ignore_ascii_case(name)).map(|g| g.id).collect();
                clients.extend(
                    state.clients.values().filter(|c| c.server_groups.iter().any(|g| groups.contains(g))).map(|c| c.id.0),
                );
            }
            T::Commanders => {
                clients.extend(state.clients.values().filter(|c| c.is_channel_commander).map(|c| c.id.0))
            }
            T::Subchannels => channels.extend(children(state, own_channel).iter().map(|c| c.id.0)),
            T::ParentChannel => {
                let parent = state.channels.get(&own_channel).map(|c| c.parent.0).unwrap_or(0);
                if parent != 0 {
                    channels.push(parent);
                }
            }
        }
    }
    clients.retain(|c| *c != own.0);
    channels.sort_unstable();
    channels.dedup();
    clients.sort_unstable();
    clients.dedup();
    (channels, clients)
}

/// Send an admin action; returns the handle of the server's answer and a
/// line for the activity feed.
fn admin(con: &mut Connection, action: &AdminAction) -> Result<(tsclientlib::MessageHandle, String)> {
    use tsclientlib::messages::c2s;
    let state = con.get_state()?;
    let client = |id: u16| {
        state.clients.get(&ClientId(id)).ok_or_else(|| anyhow::anyhow!("that user left"))
    };
    let name = |id: u16| state.clients.get(&ClientId(id)).map(|c| c.name.clone()).unwrap_or_default();
    let (cmd, what) = match action {
        AdminAction::Poke { client, message } => {
            (state.send_message(MessageTarget::Poke(ClientId(*client)), message), format!("Poked {}", name(*client)))
        }
        AdminAction::KickChannel { client: id, reason } => {
            let mut k = client(*id)?.kick(tsclientlib::Reason::KickChannel);
            if !reason.is_empty() {
                k = k.set_reason_message(reason);
            }
            (k.to_packet(), format!("Kicked {} from the channel", name(*id)))
        }
        AdminAction::KickServer { client: id, reason } => {
            let mut k = client(*id)?.kick(tsclientlib::Reason::KickServer);
            if !reason.is_empty() {
                k = k.set_reason_message(reason);
            }
            (k.to_packet(), format!("Kicked {} from the server", name(*id)))
        }
        AdminAction::Ban { client: id, secs, reason } => {
            client(*id)?;
            let part = c2s::OutBanClientPart {
                client_id: ClientId(*id),
                time: secs.map(|s| time::Duration::seconds(s as i64)),
                ban_reason: (!reason.is_empty()).then(|| reason.as_str().into()),
            };
            let what = match secs {
                Some(s) => format!("Banned {} for {}", name(*id), crate::net::session::human_secs(*s)),
                None => format!("Banned {} permanently", name(*id)),
            };
            (c2s::OutBanClientMessage::new(&mut std::iter::once(part)), what)
        }
        AdminAction::Move { client: id, channel } => {
            let ch = state.channels.get(&ChannelId(*channel)).map(|c| c.name.clone()).unwrap_or_default();
            (client(*id)?.client_move(ChannelId(*channel)).to_packet(), format!("Moved {} to {ch}", name(*id)))
        }
        AdminAction::ServerGroup { client: id, group, add } => {
            let c = client(*id)?;
            let g = tsclientlib::ServerGroupId(*group);
            let gname = state.server_groups.get(&g).map(|g| g.name.clone()).unwrap_or_default();
            if *add {
                (c.add_server_group(g), format!("Added {} to {gname}", name(*id)))
            } else {
                (c.remove_server_group(g), format!("Removed {} from {gname}", name(*id)))
            }
        }
        AdminAction::ChannelGroup { client: id, group } => {
            let c = client(*id)?;
            let g = tsclientlib::ChannelGroupId(*group);
            let gname = state.channel_groups.get(&g).map(|g| g.name.clone()).unwrap_or_default();
            let part = c2s::OutSetClientChannelGroupPart {
                channel_group: g,
                channel_id: c.channel,
                client_db_id: c.database_id,
            };
            (
                c2s::OutSetClientChannelGroupMessage::new(&mut std::iter::once(part)),
                format!("Set {} to {gname}", name(*id)),
            )
        }
        AdminAction::CreateChannel { parent, props: p } => {
            let ch = p.name.as_deref().unwrap_or_default();
            let mut o = tsclientlib::data::ChannelOptions::new(ch);
            if *parent != 0 {
                o = o.parent_id(ChannelId(*parent));
            }
            // at the bottom, like the official client
            if let Some(last) = children(state, ChannelId(*parent)).last() {
                o = o.order(last.id);
            }
            if let Some(v) = p.phonetic.as_deref().filter(|v| !v.is_empty()) {
                o = o.phonetic_name(v);
            }
            if let Some(v) = p.topic.as_deref().filter(|v| !v.is_empty()) {
                o = o.topic(v);
            }
            if let Some(v) = p.description.as_deref().filter(|v| !v.is_empty()) {
                o = o.description(v);
            }
            if let Some(Some(pw)) = &p.password {
                o = o.password(pw);
            }
            if let Some(k) = p.kind {
                o = o.channel_type(k);
            }
            if p.is_default == Some(true) {
                o = o.default();
            }
            if let Some(c) = p.codec {
                o = o.codec(c);
            }
            if let Some(q) = p.quality {
                o = o.codec_quality(q);
            }
            if let Some(m) = p.max_clients {
                o = o.max_clients(m);
            }
            if let Some(m) = p.max_family {
                o = o.max_family_clients(m);
            }
            if let Some(u) = p.unencrypted {
                o = o.is_unencrypted(u);
            }
            if let Some(d) = p.delete_delay.filter(|d| *d > 0) {
                o = o.delete_delay(time::Duration::seconds(d as i64));
            }
            (state.server.add_channel(o), format!("Created channel {ch}"))
        }
        AdminAction::EditChannel { channel, props: p } => {
            let c = state.channels.get(&ChannelId(*channel)).ok_or_else(|| anyhow::anyhow!("channel is gone"))?;
            let mut e = c.edit();
            if let Some(v) = &p.name {
                e = e.set_name(v);
            }
            if let Some(v) = &p.phonetic {
                e = e.set_phonetic_name(v);
            }
            if let Some(v) = &p.topic {
                e = e.set_topic(v);
            }
            if let Some(v) = &p.description {
                e = e.set_description(v);
            }
            if let Some(pw) = &p.password {
                e = e.set_password(pw.as_deref());
            }
            if let Some(k) = p.kind {
                e = e.set_channel_type(k);
            }
            if let Some(d) = p.is_default {
                e = e.set_is_default(d);
            }
            if let Some(v) = p.codec {
                e = e.set_codec(v);
            }
            if let Some(v) = p.quality {
                e = e.set_codec_quality(v);
            }
            if let Some(v) = p.max_clients {
                e = e.set_max_clients(v);
            }
            if let Some(v) = p.max_family {
                e = e.set_max_family_clients(v);
            }
            if let Some(v) = p.talk_power {
                e = e.set_needed_talk_power(v);
            }
            if let Some(v) = p.delete_delay {
                e = e.set_delete_delay(time::Duration::seconds(v as i64));
            }
            if let Some(v) = p.latency {
                e = e.set_codec_latency_factor(v);
            }
            if let Some(v) = p.unencrypted {
                e = e.set_is_unencrypted(v);
            }
            let only = |f: &dyn Fn(&mut ChannelProps)| {
                let mut q = p.clone();
                f(&mut q);
                q.is_empty()
            };
            let what = if only(&|q| q.name = None) && p.name.is_some() {
                format!("Renamed {} to {}", c.name, p.name.as_deref().unwrap_or_default())
            } else if only(&|q| q.password = None) && p.password.is_some() {
                match &p.password {
                    Some(Some(_)) => format!("Set a password on {}", c.name),
                    _ => format!("Removed the password from {}", c.name),
                }
            } else {
                format!("Updated {}", c.name)
            };
            (e.to_packet(), what)
        }
        AdminAction::Subscribe { channel, on } => {
            let c = state.channels.get(&ChannelId(*channel)).ok_or_else(|| anyhow::anyhow!("channel is gone"))?;
            let what = if *on { format!("Subscribed to {}", c.name) } else { format!("Unsubscribed from {}", c.name) };
            (c.set_subscribed(*on), what)
        }
        AdminAction::MoveChannel { channel, parent, after } => {
            let c = state.channels.get(&ChannelId(*channel)).ok_or_else(|| anyhow::anyhow!("channel is gone"))?;
            // channelmove only changes the parent; reordering is an edit
            let cmd = if c.parent.0 == *parent {
                c.edit().set_order(ChannelId(*after)).to_packet()
            } else {
                c.channel_move(ChannelId(*parent), ChannelId(*after)).to_packet()
            };
            (cmd, format!("Moved {}", c.name))
        }
        AdminAction::DeleteChannel { channel } => {
            let n = state.channels.get(&ChannelId(*channel)).map(|c| c.name.clone()).unwrap_or_default();
            let part = c2s::OutChannelDeletePart { channel_id: ChannelId(*channel), force: true };
            (c2s::OutChannelDeleteMessage::new(&mut std::iter::once(part)), format!("Deleted channel {n}"))
        }
        AdminAction::TalkPower { client: id, grant } => (
            client(*id)?.edit().set_talk_power_granted(*grant).to_packet(),
            format!("{} talk power for {}", if *grant { "Granted" } else { "Revoked" }, name(*id)),
        ),
    };
    Ok((cmd.send_with_result(con)?, what))
}

/// Connection errors in plain words (the raw ones are nested debug output).
fn connect_error(e: &anyhow::Error) -> String {
    let raw = format!("{e:?} {e}");
    let known = [
        ("Timeout", "Server not reachable"),
        ("ResolveAddress", "Couldn't find that address"),
        ("ClientTooManyClonesConnected", "Too many connections from this identity"),
        ("ServerMaxclientsReached", "The server is full"),
        ("BanIp", "You are banned from this server"),
        ("Banned", "You are banned from this server"),
        ("ConnectionRefused", "The server refused the connection"),
        ("IdentityLevel", "The server needs a higher identity security level (Settings, Identities)"),
        ("Disconnected by the server", "Disconnected by the server"),
    ];
    known
        .iter()
        .find(|(k, _)| raw.contains(k))
        .map(|(_, v)| v.to_string())
        .unwrap_or_else(|| e.to_string())
}

/// "Moved X" + error -> "Couldn't move X: channel already in".
fn failed(what: &str, error: &str) -> String {
    const VERBS: &[(&str, &str)] = &[
        ("Poked", "poke"),
        ("Joined", "join"),
        ("Message to", "message"),
        ("Kicked", "kick"),
        ("Banned", "ban"),
        ("Moved", "move"),
        ("Added", "add"),
        ("Removed", "remove"),
        ("Set", "set"),
        ("Created", "create"),
        ("Renamed", "rename"),
        ("Updated", "update"),
        ("Deleted", "delete"),
        ("Granted", "grant"),
        ("Sent", "send"),
        ("Created folder", "create folder"),
        ("Subscribed to", "subscribe to"),
        ("Unsubscribed from", "unsubscribe from"),
        ("Changed", "change"),
        ("Copied", "copy"),
        ("Filed", "file"),
        ("Used", "use"),
        ("Became", "become"),
        ("Stopped", "stop"),
        ("Requested", "request"),
        ("Withdrew", "withdraw"),
        ("Revoked", "revoke"),
    ];
    let what = VERBS
        .iter()
        .find_map(|(past, verb)| what.strip_prefix(past).map(|rest| format!("{verb}{rest}")))
        .unwrap_or_else(|| what.to_lowercase());
    format!("Couldn't {what}: {}", plain_error(error))
}

/// TsError's names are CamelCase: ChannelAlreadyIn -> channel already in.
pub(crate) fn plain_error(error: &str) -> String {
    let mut err = String::new();
    for (i, ch) in error.chars().enumerate() {
        if ch.is_uppercase() && i > 0 {
            err.push(' ');
        }
        err.extend(ch.to_lowercase());
    }
    err
}

pub fn human_secs(s: u64) -> String {
    match s {
        s if s % 86_400 == 0 => format!("{} day(s)", s / 86_400),
        s if s % 3600 == 0 => format!("{} hour(s)", s / 3600),
        s => format!("{} minute(s)", s / 60),
    }
}

fn uid_string(c: &data::Client) -> String {
    c.uid.as_ref().map(|u| u.as_ref().to_string()).unwrap_or_default()
}

/// TeamSpeak messages use BBCode; show plain text.
pub fn strip_bbcode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for ch in s.chars() {
        match ch {
            '[' => in_tag = true,
            ']' if in_tag => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    /// Needs the network; run with `cargo test -- --ignored picture`.
    #[test]
    #[ignore]
    fn picture_download() {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let local = tokio::task::LocalSet::new();
        let got = std::rc::Rc::new(std::cell::RefCell::new(None));
        let g = got.clone();
        local.block_on(&rt, async move {
            super::fetch_picture("https://www.google.com/favicon.ico".into(), move |p| *g.borrow_mut() = Some(p));
            tokio::time::sleep(std::time::Duration::from_secs(8)).await;
        });
        let path = got.borrow().clone().expect("no picture");
        assert!(std::fs::metadata(&path).unwrap().len() > 0);
        let _ = std::fs::remove_file(path);
    }
}
