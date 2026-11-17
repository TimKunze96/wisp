//! A session with a Banshee server (banshee://host:port#fingerprint). Answers
//! the same `SessionCmd`s as a TeamSpeak session: roles stand in for server
//! groups, and what Banshee doesn't have (files, channel groups, permission
//! numbers) is left out.

use std::cell::Cell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use banshee_client::{
    Account, Channel, ChannelEdit, ChatTarget, Client, ClientMsg, Files, Identity, Key, Leave, NewChannel,
    OfflineMessage, Options, Request, Role, ServerEdit, ServerInfo as Info, ServerMsg, User, UserId, Video, Watching,
    Whisper, key_hex,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;
use tsclientlib::{ChannelType, ClientId, Codec, MaxClients};
use tsproto_packets::packets::{AudioData as AudioPacket, CodecType, Direction, InAudioBuf, OutAudio};

use super::chat::{self, ChatKind, ChatLine, Conversation};
use super::files::{FileCmd, Transfer, TransferState, Transfers};
use super::session::{TALK_HOLD, placeholder};
use super::*;
use crate::ConnectionId;
use crate::config::{self, Bookmark, WhisperTarget};
use crate::notify::EventKind;
use crate::video::{Screen, ShareMsg, Sharing};

pub const SCHEME: &str = "banshee://";

/// Voice right after switching whisper targets is dropped for this long, so
/// none of it reaches the old listeners before the server knows.
const WHISPER_SETTLE: Duration = Duration::from_millis(100);

pub fn is_banshee(address: &str) -> bool {
    address.starts_with(SCHEME)
}

/// "banshee://host:port#fingerprint" into ("host:port", fingerprint).
fn parse(address: &str) -> Result<(String, Option<String>)> {
    let rest = address.strip_prefix(SCHEME).context("not a banshee:// address")?;
    let (host, fp) = match rest.split_once('#') {
        Some((h, f)) => (h, Some(f.trim().to_lowercase()).filter(|f| !f.is_empty())),
        None => (rest, None),
    };
    let host = host.trim_end_matches('/');
    let host = if host.contains(':') { host.to_string() } else { format!("{host}:7700") };
    Ok((host, fp))
}

/// Our key for every Banshee server, made on first use.
fn identity() -> Result<Identity> {
    Identity::load_or_create(&config::config_dir().join("banshee-identity.der"))
}

/// TeamSpeak-style database ids for the admin window, from the key.
fn db_id(key: &Key) -> u64 {
    u64::from_le_bytes(key[..8].try_into().expect("8 bytes"))
}

fn date(secs: u64) -> String {
    chrono::DateTime::from_timestamp(secs as i64, 0)
        .map(|d| d.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_default()
}

/// A list request waiting for the server's answer.
enum Pending {
    Bans(admin::Reply),
    Tokens(admin::Reply),
    Members(u64, admin::Reply),
    DbId(String, admin::Reply),
    Files(admin::Reply),
    /// The whole inbox, or one message.
    Inbox(Option<u32>, admin::Reply),
    Log(admin::Reply),
}

impl Pending {
    fn fail(self, message: String) {
        let (Pending::Bans(r)
        | Pending::Tokens(r)
        | Pending::Members(_, r)
        | Pending::DbId(_, r)
        | Pending::Files(r)
        | Pending::Inbox(_, r)
        | Pending::Log(r)) = self;
        let _ = r.send(Err(message));
    }
}

struct Session {
    con_id: ConId,
    base: ServerSnap,
    back: mpsc::UnboundedSender<SessionMsg>,
    audio: Option<AudioData>,
    settings: Box<Settings>,
    you: UserId,
    info: Info,
    channels: Vec<Channel>,
    roles: Vec<Role>,
    users: HashMap<UserId, User>,
    /// Who we last heard, and whether they whispered.
    last_voice: HashMap<UserId, (Instant, bool)>,
    last_own_audio: Option<Instant>,
    seq: u16,
    whisper: Option<Whisper>,
    whisper_since: Instant,
    history: chat::History,
    chats: Vec<Conversation>,
    pending: VecDeque<Pending>,
    /// Everyone the server told us about in its account list, for the admin window.
    accounts: Vec<Account>,
    input_muted: bool,
    /// Why the server dropped us, once it did.
    removed: Option<String>,
    files: Files,
    transfers: Transfers,
    next_transfer: u64,
    /// Where avatars are cached, and which (key, hash) we already fetch.
    avatar_dir: PathBuf,
    avatars_requested: HashSet<(Key, String)>,
    /// A background task (transfer, avatar) changed something to show.
    dirty: Rc<Cell<bool>>,
    video: Video,
    /// Our screen, while we share it.
    sharing: Option<Sharing>,
    /// Screens we watch (or asked to).
    watching: HashMap<UserId, Screen>,
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
) {
    let base = placeholder(con_id, &bookmark);
    if let Err(e) = connect_and_run(con_id, &bookmark, settings, audio, &mut rx, back.clone(), input_muted).await {
        tracing::warn!("{}: {e:?}", bookmark.address);
        let mut snap = base;
        snap.status = Status::Failed(format!("{e:#}"));
        let _ = back.send(SessionMsg::Snap(snap));
    }
    let _ = back.send(SessionMsg::Ended(con_id));
}

async fn connect_and_run(
    con_id: ConId,
    bookmark: &Bookmark,
    settings: Box<Settings>,
    audio: Option<AudioData>,
    rx: &mut mpsc::UnboundedReceiver<SessionCmd>,
    back: mpsc::UnboundedSender<SessionMsg>,
    input_muted: bool,
) -> Result<()> {
    let (host, fingerprint) = parse(&bookmark.address)?;
    let addr = tokio::net::lookup_host(&host).await?.next().with_context(|| format!("no address for {host}"))?;
    let name = if bookmark.nickname.trim().is_empty() { "Wisp user" } else { bookmark.nickname.trim() };
    let identity = identity()?;
    let password = Some(bookmark.password.as_str()).filter(|p| !p.is_empty());
    let opts = Options { name, identity: &identity, fingerprint: fingerprint.as_deref(), password };
    let mut client = Client::connect(addr, opts).await?;
    let voice = client.voice();

    let history = chat::History::new(&bookmark.address);
    let chats = [(ChatKind::Channel, "Channel"), (ChatKind::Server, "Server")]
        .into_iter()
        .map(|(kind, title)| {
            let lines = history.load(&kind);
            Conversation { kind, title: title.into(), client: None, lines, received: 0 }
        })
        .collect();
    let mut s = Session {
        con_id,
        base: placeholder(con_id, bookmark),
        back,
        audio,
        settings,
        you: client.you,
        info: client.server.clone(),
        channels: client.channels.clone(),
        roles: client.roles.clone(),
        users: client.users.iter().map(|u| (u.id, u.clone())).collect(),
        last_voice: HashMap::new(),
        last_own_audio: None,
        seq: 0,
        whisper: None,
        whisper_since: Instant::now(),
        history,
        chats,
        pending: VecDeque::new(),
        accounts: Vec::new(),
        input_muted,
        removed: None,
        files: client.files(),
        transfers: Default::default(),
        next_transfer: 1,
        avatar_dir: crate::config::cache_dir().join("avatars").join(session::sanitize(&bookmark.address)),
        avatars_requested: HashSet::new(),
        dirty: Default::default(),
        video: client.video(),
        sharing: None,
        watching: HashMap::new(),
    };
    s.base.status = Status::Connected;
    s.base.platform = "Banshee".into();
    s.base.screens = true;
    if input_muted {
        client.send(ClientMsg::SetMuted { input: true, output: false });
    }
    s.join_path(&client, &bookmark.channel);
    s.publish();

    // screens arrive on their own task: reading a stream's header can't be cut short
    let (screens_tx, mut screens) = mpsc::unbounded_channel();
    let video = s.video.clone();
    tokio::task::spawn_local(async move {
        while let Ok(w) = video.next().await {
            if screens_tx.send(w).is_err() {
                break;
            }
        }
    });
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    loop {
        tokio::select! {
            cmd = rx.recv() => match cmd {
                None | Some(SessionCmd::Disconnect) => {
                    client.close().await;
                    return Ok(());
                }
                Some(cmd) => s.command(&client, cmd),
            },
            event = client.next_event() => match event {
                Some(e) => s.server_msg(e),
                None => match s.removed.take() {
                    Some(why) => bail!(why),
                    None => bail!("Disconnected by the server"),
                },
            },
            Some(w) = screens.recv() => s.watch_stream(w),
            v = voice.recv() => match v {
                Ok(v) => s.play(v),
                Err(_) => bail!(s.removed.take().unwrap_or_else(|| "Disconnected by the server".into())),
            },
            _ = tick.tick() => {
                s.watching.retain(|_, screen| !screen.ended());
                for (id, screen) in &s.watching {
                    if screen.wants_keyframe() {
                        client.send(ClientMsg::WantKeyframe { user: *id });
                    }
                }
                let running = s.transfers.borrow().iter().any(|t| t.state == TransferState::Running);
                if s.dirty.replace(false) || running {
                    s.publish();
                }
                let before = s.last_voice.len();
                s.last_voice.retain(|_, (t, _)| t.elapsed() < TALK_HOLD);
                let own_talking = s.last_own_audio.is_some_and(|t| t.elapsed() < TALK_HOLD);
                if s.last_voice.len() != before || (!own_talking && s.last_own_audio.take().is_some()) {
                    s.publish();
                }
                continue;
            }
        }
        s.publish();
    }
}

impl Session {
    fn me(&self) -> Option<&User> {
        self.users.get(&self.you)
    }

    fn name(&self, id: UserId) -> String {
        self.users.get(&id).map(|u| u.name.clone()).unwrap_or_else(|| "Someone".into())
    }

    fn user_by_uid(&self, uid: &str) -> Option<&User> {
        self.users.values().find(|u| key_hex(&u.key) == uid)
    }

    fn user_by_client(&self, client: u16) -> Option<&User> {
        self.users.get(&UserId::from(client))
    }

    /// The key behind one of our made-up database ids.
    fn key_for(&self, d: u64) -> Option<Key> {
        let known = self.accounts.iter().map(|a| a.key).chain(self.users.values().map(|u| u.key));
        known.into_iter().find(|k| db_id(k) == d)
    }

    fn role(&self, id: u32) -> Option<&Role> {
        self.roles.iter().find(|r| r.id == id)
    }

    /// "Lobby/Games" to a channel id.
    fn channel_by_path(&self, path: &str) -> Option<u32> {
        let mut parent = None;
        let mut found = None;
        for part in path.trim().trim_matches('/').split('/').filter(|p| !p.is_empty()) {
            let c = self.channels.iter().find(|c| c.parent == parent && c.name.eq_ignore_ascii_case(part.trim()))?;
            parent = Some(c.id);
            found = Some(c.id);
        }
        found
    }

    fn join_path(&self, client: &Client, path: &str) {
        if path.trim().trim_matches('/').is_empty() {
            return;
        }
        match self.channel_by_path(path) {
            Some(id) if self.me().is_some_and(|me| me.channel != id) => self.join(client, id, None),
            Some(_) => {}
            None => self.activity(&format!("Couldn't find the channel {path:?}")),
        }
    }

    fn join(&self, client: &Client, channel: u32, password: Option<String>) {
        let saved = self.settings.channel_passwords.get(&config::channel_key(&self.base.bookmark, channel.into()));
        let password = password.or_else(|| saved.cloned());
        client.send(ClientMsg::Join { channel, password });
    }

    fn command(&mut self, client: &Client, cmd: SessionCmd) {
        match cmd {
            SessionCmd::Join(channel, password) => {
                if let Ok(id) = u32::try_from(channel) {
                    self.join(client, id, password);
                }
            }
            SessionCmd::Audio(opus, whisper) => self.send_audio(client, &opus, whisper.as_deref()),
            SessionCmd::SelfState { input_muted, output_muted } => {
                self.input_muted = input_muted;
                client.send(ClientMsg::SetMuted { input: input_muted, output: output_muted });
            }
            SessionCmd::Away(away) => client.send(ClientMsg::SetAway(away)),
            SessionCmd::Me(SelfAction::TalkRequest(request)) => client.send(ClientMsg::RequestTalk(request)),
            SessionCmd::Me(SelfAction::Description(d)) => client.send(ClientMsg::SetDescription(d)),
            SessionCmd::Me(SelfAction::Commander(_)) => {}
            SessionCmd::SendText(kind, text) => {
                let to = match &kind {
                    ChatKind::Channel => ChatTarget::Channel,
                    ChatKind::Server => ChatTarget::Server,
                    ChatKind::Private(uid) => match self.user_by_uid(uid) {
                        Some(u) => ChatTarget::User(u.id),
                        None => return self.activity("Couldn't send: they're offline"),
                    },
                };
                // our own line comes back from the server
                client.send(ClientMsg::Chat { to, text });
            }
            SessionCmd::OpenChat(c) => {
                if let Some(u) = self.user_by_client(c) {
                    let (uid, name) = (key_hex(&u.key), u.name.clone());
                    self.conversation(ChatKind::Private(uid), &name);
                }
            }
            SessionCmd::CloseChat(kind) => {
                if matches!(kind, ChatKind::Private(_)) {
                    self.chats.retain(|c| c.kind != kind);
                }
            }
            SessionCmd::Admin(action) => self.admin(client, action),
            SessionCmd::Write(write) => self.write(client, write),
            SessionCmd::Query(query, reply) => self.query(client, query, reply),
            SessionCmd::ServerInfo(reply) => {
                let _ = reply.send(admin::ServerInfo {
                    name: self.info.name.clone(),
                    welcome_message: self.info.welcome.clone(),
                    max_clients: self.info.max_users.map(|m| m.min(u16::MAX as u32) as u16).unwrap_or(0),
                    default_server_group: self.info.default_role.into(),
                    platform: "Banshee".into(),
                    ..Default::default()
                });
            }
            SessionCmd::Settings(settings) => self.settings = settings,
            SessionCmd::ChannelDescription(_) => {}
            SessionCmd::Files(cmd) => self.file_cmd(client, cmd),
            SessionCmd::SetAvatar(Some((path, _))) => self.set_avatar(path),
            SessionCmd::SetAvatar(None) => client.send(ClientMsg::ClearAvatar),
            SessionCmd::Share(Some(start)) => self.share(start),
            SessionCmd::Share(None) => self.sharing = None,
            SessionCmd::ShareSwitch(source) => {
                if let Some(s) = &self.sharing {
                    s.switch(source);
                }
            }
            SessionCmd::Watch(id, Some(screen), quality) => {
                let id = UserId::from(id);
                client.send(ClientMsg::Watch { user: id, quality });
                if let Some(old) = self.watching.insert(id, screen) {
                    old.end();
                }
            }
            SessionCmd::StreamVolume(id, volume) => {
                if let Some(audio) = &self.audio {
                    audio.ts2a.lock().unwrap().set_volume(stream_sound_id(self.con_id, id.into()), volume);
                }
            }
            SessionCmd::WatchQuality(id, quality) => {
                client.send(ClientMsg::SetQuality { user: UserId::from(id), quality });
            }
            SessionCmd::Watch(id, None, _) => {
                let id = UserId::from(id);
                if let Some(screen) = self.watching.remove(&id) {
                    screen.end();
                    client.send(ClientMsg::Unwatch { user: id });
                }
            }
            // avatars, recording notices, icons, details: TeamSpeak's
            _ => {}
        }
    }

    fn send_audio(&mut self, client: &Client, opus: &[u8], list: Option<&config::WhisperList>) {
        if self.input_muted {
            return;
        }
        let whisper = list.map(|l| self.whisper_targets(l));
        if whisper != self.whisper {
            client.send(ClientMsg::SetWhisper(whisper.clone()));
            self.whisper = whisper;
            self.whisper_since = Instant::now();
        }
        if self.whisper_since.elapsed() < WHISPER_SETTLE {
            return;
        }
        self.seq = self.seq.wrapping_add(1);
        if client.send_voice(self.seq, opus).is_ok() {
            self.last_own_audio = Some(Instant::now());
        }
    }

    fn whisper_targets(&self, list: &config::WhisperList) -> Whisper {
        let mut w = Whisper::default();
        let own = self.me().map(|u| u.channel);
        for t in &list.targets {
            match t {
                WhisperTarget::Channel { path } => w.channels.extend(self.channel_by_path(path)),
                WhisperTarget::Client { uid, .. } => w.users.extend(self.user_by_uid(uid).map(|u| u.id)),
                WhisperTarget::ServerGroup { name } => w.users.extend(
                    self.users
                        .values()
                        .filter(|u| self.role(u.role).is_some_and(|r| r.name.eq_ignore_ascii_case(name)))
                        .map(|u| u.id),
                ),
                WhisperTarget::Subchannels => {
                    w.channels.extend(self.channels.iter().filter(|c| own.is_some() && c.parent == own).map(|c| c.id))
                }
                WhisperTarget::ParentChannel => w
                    .channels
                    .extend(own.and_then(|o| self.channels.iter().find(|c| c.id == o)).and_then(|c| c.parent)),
                WhisperTarget::Commanders => {}
            }
        }
        w.channels.sort();
        w.channels.dedup();
        w.users.sort();
        w.users.dedup();
        w
    }

    fn admin(&mut self, client: &Client, action: AdminAction) {
        let user = |c: u16| UserId::from(c);
        let msg = match action {
            AdminAction::Poke { client: c, message } => ClientMsg::Poke { user: user(c), text: message },
            AdminAction::KickChannel { client: c, reason } => ClientMsg::Kick { user: user(c), reason, server: false },
            AdminAction::KickServer { client: c, reason } => ClientMsg::Kick { user: user(c), reason, server: true },
            AdminAction::Ban { client: c, secs, reason } => ClientMsg::Ban { user: user(c), reason, secs },
            AdminAction::Move { client: c, channel } => {
                let Ok(channel) = u32::try_from(channel) else { return };
                ClientMsg::MoveUser { user: user(c), channel }
            }
            AdminAction::ServerGroup { client: c, group, add } => {
                let Some(u) = self.user_by_client(c) else { return };
                let Ok(group) = u32::try_from(group) else { return };
                // one role each: taking one away means back to the default
                let role = if add { group } else { self.info.default_role };
                ClientMsg::SetRole { key: u.key, role }
            }
            AdminAction::TalkPower { client: c, grant } => ClientMsg::GrantTalk { user: user(c), granted: grant },
            AdminAction::CreateChannel { parent, props } => ClientMsg::CreateChannel(NewChannel {
                parent: u32::try_from(parent).ok().filter(|p| *p != 0),
                name: props.name.unwrap_or_default(),
                topic: props.topic.unwrap_or_default(),
                description: props.description.unwrap_or_default(),
                password: props.password.flatten(),
                max_users: props.max_clients.and_then(limit),
                talk_power: props.talk_power.unwrap_or(0).max(0) as u32,
                permanent: !matches!(props.kind, Some(ChannelType::Temporary)),
            }),
            AdminAction::EditChannel { channel, props } => {
                let Ok(id) = u32::try_from(channel) else { return };
                let edit = ChannelEdit {
                    name: props.name,
                    topic: props.topic,
                    description: props.description,
                    password: props.password,
                    max_users: props.max_clients.map(limit),
                    talk_power: props.talk_power.map(|t| t.max(0) as u32),
                    permanent: props.kind.map(|k| !matches!(k, ChannelType::Temporary)),
                    default: props.is_default.filter(|d| *d),
                };
                ClientMsg::EditChannel { id, edit }
            }
            AdminAction::MoveChannel { channel, parent, after } => {
                let Ok(id) = u32::try_from(channel) else { return };
                let parent = u32::try_from(parent).ok().filter(|p| *p != 0);
                let after = u32::try_from(after).ok().filter(|a| *a != 0);
                ClientMsg::MoveChannel { id, parent, after }
            }
            AdminAction::DeleteChannel { channel } => {
                let Ok(id) = u32::try_from(channel) else { return };
                ClientMsg::DeleteChannel { id }
            }
            // everything is always visible on Banshee
            AdminAction::ChannelGroup { .. } | AdminAction::Subscribe { .. } | AdminAction::SubscribeAll { .. } => return,
        };
        client.send(msg);
    }

    fn write(&mut self, client: &Client, write: admin::AdminWrite) {
        use admin::AdminWrite as W;
        let msg = match write {
            W::UseToken { token } => ClientMsg::Redeem { token },
            W::AddToken { group, description, .. } => {
                let Ok(role) = u32::try_from(group) else { return };
                ClientMsg::CreateToken { role, note: description }
            }
            W::DelToken { token } => ClientMsg::DeleteToken { token },
            W::AddBan { ip, uid, secs, reason, .. } => ClientMsg::AddBan {
                key: banshee_client::parse_key(&uid),
                ip: Some(ip).filter(|i| !i.trim().is_empty()),
                reason,
                secs,
            },
            W::DelBan { id } => ClientMsg::Unban { id },
            W::AddGroup { kind: admin::GroupKind::Server, name } => {
                ClientMsg::SaveRole(Role { id: 0, name, rank: 1, perms: Vec::new(), talk_power: 0 })
            }
            W::CopyGroup { kind: admin::GroupKind::Server, from, name } => {
                let Some(r) = self.roles.iter().find(|r| u64::from(r.id) == from) else { return };
                ClientMsg::SaveRole(Role { id: 0, name, ..r.clone() })
            }
            W::RenameGroup { kind: admin::GroupKind::Server, id, name } => {
                let Some(r) = self.roles.iter().find(|r| u64::from(r.id) == id) else { return };
                ClientMsg::SaveRole(Role { name, ..r.clone() })
            }
            W::DelGroup { kind: admin::GroupKind::Server, id } => {
                let Ok(id) = u32::try_from(id) else { return };
                ClientMsg::DeleteRole { id }
            }
            W::AddMember { group, db_id: d } => {
                let (Some(key), Ok(role)) = (self.key_for(d), u32::try_from(group)) else { return };
                ClientMsg::SetRole { key, role }
            }
            // one role each: out of one means back to the default
            W::RemoveMember { db_id: d, .. } => {
                let Some(key) = self.key_for(d) else { return };
                ClientMsg::SetRole { key, role: self.info.default_role }
            }
            W::SendOffline { uid, subject, message } => {
                let Some(to) = banshee_client::parse_key(&uid) else {
                    return self.activity("That isn't a Banshee key");
                };
                ClientMsg::SendOffline { to, subject, text: message }
            }
            W::DelOffline { id } => ClientMsg::DeleteOffline { id },
            W::MarkOffline { id, read } => ClientMsg::MarkRead { id, read },
            W::EditServer(p) => ClientMsg::EditServer(ServerEdit {
                name: p.name,
                welcome: p.welcome_message,
                max_users: p.max_clients.map(|m| Some(u32::from(m)).filter(|m| *m > 0)),
                password: p.password.map(|p| Some(p).filter(|p| !p.is_empty())),
            }),
            _ => return self.activity("Banshee servers don't have that"),
        };
        client.send(msg);
    }

    fn query(&mut self, client: &Client, query: admin::Query, reply: admin::Reply) {
        use admin::Query as Q;
        match query {
            Q::Bans => {
                client.send(ClientMsg::ListBans);
                self.pending.push_back(Pending::Bans(reply));
            }
            Q::Tokens => {
                client.send(ClientMsg::ListTokens);
                self.pending.push_back(Pending::Tokens(reply));
            }
            Q::ServerGroupMembers(group) => {
                client.send(ClientMsg::ListAccounts);
                self.pending.push_back(Pending::Members(group, reply));
            }
            Q::DbIdFromUid(uid) => {
                client.send(ClientMsg::ListAccounts);
                self.pending.push_back(Pending::DbId(uid, reply));
            }
            Q::Files { channel, path, password } => {
                let Ok(channel) = u32::try_from(channel) else { return };
                let password = Some(password).filter(|p| !p.is_empty());
                client.send(ClientMsg::ListFiles { channel, password, path });
                self.pending.push_back(Pending::Files(reply));
            }
            Q::Inbox => {
                client.send(ClientMsg::ListInbox);
                self.pending.push_back(Pending::Inbox(None, reply));
            }
            Q::InboxMessage(id) => {
                // reading one marks it read, like TeamSpeak does
                client.send(ClientMsg::MarkRead { id, read: true });
                self.pending.push_back(Pending::Inbox(Some(id), reply));
            }
            Q::Log { lines } => {
                client.send(ClientMsg::ReadLog { lines });
                self.pending.push_back(Pending::Log(reply));
            }
            _ => {
                let _ = reply.send(Err("Banshee servers don't have that".into()));
            }
        }
    }

    fn file_cmd(&mut self, client: &Client, cmd: FileCmd) {
        let pw = |p: String| Some(p).filter(|p| !p.is_empty());
        let id = |c: u64| u32::try_from(c).unwrap_or(u32::MAX);
        match cmd {
            FileCmd::Download { channel, password, path, to } => {
                let request = Request::Download { channel: id(channel), password: pw(password), path: path.clone() };
                self.download(request, &path, to);
            }
            FileCmd::Upload { channel, password, path, from } => self.upload(id(channel), pw(password), path, from),
            FileCmd::Delete { channel, password, path } => {
                client.send(ClientMsg::DeleteFile { channel: id(channel), password: pw(password), path })
            }
            FileCmd::Mkdir { channel, password, path } => {
                client.send(ClientMsg::MakeDir { channel: id(channel), password: pw(password), path })
            }
            FileCmd::Rename { channel, password, from, to } => {
                client.send(ClientMsg::RenameFile { channel: id(channel), password: pw(password), from, to })
            }
            FileCmd::ClearDone => {
                self.transfers.borrow_mut().retain(|t| matches!(t.state, TransferState::Waiting | TransferState::Running))
            }
        }
    }

    fn new_transfer(&mut self, name: &str, upload: bool, local: PathBuf) -> u64 {
        let id = self.next_transfer;
        self.next_transfer += 1;
        let name = name.rsplit('/').next().unwrap_or(name).to_string();
        self.transfers.borrow_mut().push(Transfer {
            id,
            name,
            upload,
            size: 0,
            done: 0,
            local,
            state: TransferState::Waiting,
        });
        id
    }

    fn download(&mut self, request: Request, path: &str, to: PathBuf) {
        let id = self.new_transfer(path, false, to.clone());
        let (files, list, dirty) = (self.files.clone(), self.transfers.clone(), self.dirty.clone());
        tokio::task::spawn_local(async move {
            let result = async {
                let (size, recv) = files.download(request).await?;
                files::update(&list, id, |t| {
                    t.state = TransferState::Running;
                    t.size = size;
                });
                if let Some(dir) = to.parent() {
                    tokio::fs::create_dir_all(dir).await?;
                }
                let mut file = tokio::fs::File::create(&to).await?;
                let mut recv = recv.take(size);
                let mut buf = vec![0u8; 64 * 1024];
                let mut done = 0u64;
                loop {
                    let n = recv.read(&mut buf).await?;
                    if n == 0 {
                        break;
                    }
                    file.write_all(&buf[..n]).await?;
                    done += n as u64;
                    files::update(&list, id, |t| t.done = done);
                }
                file.flush().await?;
                if done < size {
                    bail!("the download stopped early");
                }
                anyhow::Ok(())
            }
            .await;
            finish(&list, id, result);
            dirty.set(true);
        });
    }

    fn upload(&mut self, channel: u32, password: Option<String>, path: String, from: PathBuf) {
        let id = self.new_transfer(&path, true, from.clone());
        let (files, list, dirty) = (self.files.clone(), self.transfers.clone(), self.dirty.clone());
        tokio::task::spawn_local(async move {
            let result = async {
                let mut file = tokio::fs::File::open(&from).await?;
                let size = file.metadata().await?.len();
                files::update(&list, id, |t| t.size = size);
                let mut up = files.upload(Request::Upload { channel, password, path, size }).await?;
                files::update(&list, id, |t| t.state = TransferState::Running);
                let mut buf = vec![0u8; 64 * 1024];
                let mut done = 0u64;
                loop {
                    let n = file.read(&mut buf).await?;
                    if n == 0 {
                        break;
                    }
                    up.send.write_all(&buf[..n]).await?;
                    done += n as u64;
                    files::update(&list, id, |t| t.done = done);
                }
                up.finish().await
            }
            .await;
            finish(&list, id, result);
            dirty.set(true);
        });
    }

    fn set_avatar(&mut self, path: PathBuf) {
        let (files, back, con) = (self.files.clone(), self.back.clone(), self.con_id);
        tokio::task::spawn_local(async move {
            let result = async { files.set_avatar(&tokio::fs::read(&path).await?).await }.await;
            if let Err(e) = result {
                let _ = back.send(SessionMsg::Activity(con, format!("Couldn't set your picture: {e:#}"), false));
            }
        });
    }

    /// The cached picture of someone, fetching it the first time.
    fn avatar(&self, u: &User) -> Option<PathBuf> {
        let hash = u.avatar.as_ref()?;
        let path = self.avatar_dir.join(format!("{}-{hash}.png", key_hex(&u.key)));
        if path.exists() {
            return Some(path);
        }
        None
    }

    /// Start fetching pictures we don't have yet.
    fn fetch_avatars(&mut self) {
        let wanted: Vec<(Key, String)> = self
            .users
            .values()
            .filter_map(|u| Some((u.key, u.avatar.clone()?)))
            .filter(|(k, h)| !self.avatar_dir.join(format!("{}-{h}.png", key_hex(k))).exists())
            .filter(|w| !self.avatars_requested.contains(w))
            .collect();
        for (key, hash) in wanted {
            self.avatars_requested.insert((key, hash.clone()));
            let (files, dir, dirty) = (self.files.clone(), self.avatar_dir.clone(), self.dirty.clone());
            tokio::task::spawn_local(async move {
                let Ok(data) = files.avatar(key).await else { return };
                let path = dir.join(format!("{}-{hash}.png", key_hex(&key)));
                if tokio::fs::create_dir_all(&dir).await.is_ok() && tokio::fs::write(&path, data).await.is_ok() {
                    dirty.set(true);
                }
            });
        }
    }

    fn answer(&mut self, msg: &ServerMsg) -> bool {
        let fits = matches!(
            (self.pending.front(), msg),
            (Some(Pending::Bans(_)), ServerMsg::Bans(_))
                | (Some(Pending::Files(_)), ServerMsg::Files { .. })
                | (Some(Pending::Inbox(..)), ServerMsg::Inbox(_))
                | (Some(Pending::Log(_)), ServerMsg::Log(_))
                | (Some(Pending::Tokens(_)), ServerMsg::Tokens(_))
                | (Some(Pending::Members(..) | Pending::DbId(..)), ServerMsg::Accounts(_))
        );
        if !fits {
            return false;
        }
        let answer = match (self.pending.pop_front().expect("checked"), msg) {
            (Pending::Bans(r), ServerMsg::Bans(bans)) => (r, admin::Answer::Bans(bans.iter().map(ban_row).collect())),
            (Pending::Tokens(r), ServerMsg::Tokens(tokens)) => {
                let rows = tokens
                    .iter()
                    .map(|t| admin::Token {
                        token: t.token.clone(),
                        server_group: true,
                        group: t.role.into(),
                        channel: 0,
                        created: date(t.created),
                        description: t.note.clone(),
                    })
                    .collect();
                (r, admin::Answer::Tokens(rows))
            }
            (Pending::Members(group, r), ServerMsg::Accounts(accounts)) => {
                self.accounts = accounts.clone();
                let rows = accounts
                    .iter()
                    .filter(|a| u64::from(a.role) == group)
                    .map(|a| admin::Member { db_id: db_id(&a.key), name: a.name.clone(), uid: key_hex(&a.key) })
                    .collect();
                (r, admin::Answer::Members(rows))
            }
            (Pending::DbId(uid, r), ServerMsg::Accounts(accounts)) => {
                self.accounts = accounts.clone();
                match accounts.iter().find(|a| key_hex(&a.key) == uid) {
                    Some(a) => (r, admin::Answer::DbId(db_id(&a.key))),
                    None => {
                        let _ = r.send(Err("Nobody with that key has been here".into()));
                        return true;
                    }
                }
            }
            (Pending::Files(r), ServerMsg::Files { entries, .. }) => {
                let rows = entries
                    .iter()
                    .map(|e| files::FileEntry {
                        name: e.name.clone(),
                        size: e.size,
                        date: date(e.modified),
                        is_dir: e.is_dir,
                    })
                    .collect();
                (r, admin::Answer::Files(rows))
            }
            (Pending::Inbox(one, r), ServerMsg::Inbox(messages)) => {
                let rows = messages.iter().filter(|m| one.is_none_or(|id| id == m.id)).map(offline_row).collect();
                (r, admin::Answer::Inbox(rows))
            }
            (Pending::Log(r), ServerMsg::Log(lines)) => {
                let rows = lines.iter().map(|l| format!("{} | {}", date(l.time), l.text)).collect();
                (r, admin::Answer::Log(rows))
            }
            _ => unreachable!("checked above"),
        };
        let _ = answer.0.send(Ok(answer.1));
        true
    }

    fn server_msg(&mut self, msg: ServerMsg) {
        if self.answer(&msg) {
            return;
        }
        match msg {
            ServerMsg::UserJoined(u) => {
                if self.me().is_some_and(|me| me.channel == u.channel) {
                    self.activity(&format!("{} joined", u.name));
                    self.event(EventKind::UserJoined, format!("{} joined your channel", u.name), String::new());
                }
                self.users.insert(u.id, u);
            }
            ServerMsg::UserChanged(u) => self.user_changed(u),
            ServerMsg::UserLeft { id, reason } => self.user_left(id, reason),
            ServerMsg::Channels(channels) => self.channels = channels,
            ServerMsg::Roles(roles) => self.roles = roles,
            ServerMsg::Server(info) => self.info = info,
            ServerMsg::Chat { from, to, text } => self.chat(from, to, text),
            ServerMsg::Poke { from, text } => {
                let name = self.name(from);
                self.activity(&format!("{name} pokes you: {text}"));
                self.event(EventKind::Poke, format!("{name} pokes you"), text);
            }
            ServerMsg::NeedPassword { channel, wrong } => {
                let name = self.channels.iter().find(|c| c.id == channel).map(|c| c.name.clone()).unwrap_or_default();
                let _ = self.back.send(SessionMsg::NeedChannelPassword(self.con_id, channel.into(), name, wrong));
            }
            ServerMsg::SendKeyframe { layer } => {
                if let Some(s) = &self.sharing {
                    s.keyframe(layer);
                }
            }
            ServerMsg::TokenCreated(t) => self.activity(&format!("New token: {}", t.token)),
            ServerMsg::Notice { text } => {
                self.activity(&text);
                if text.contains("kicked you") {
                    self.event(EventKind::MovedOrKicked, text, String::new());
                }
            }
            ServerMsg::Error { message } => match self.pending.pop_front() {
                // a list we asked for was refused
                Some(p) => p.fail(message),
                None => self.activity(&capitalized(&message)),
            },
            // answers nobody waits for (after a change)
            ServerMsg::Bans(_)
            | ServerMsg::Tokens(_)
            | ServerMsg::Accounts(_)
            | ServerMsg::Files { .. }
            | ServerMsg::Inbox(_)
            | ServerMsg::Log(_)
            | ServerMsg::Welcome { .. } => {}
        }
    }

    fn user_changed(&mut self, u: User) {
        let my_channel = self.me().map(|m| m.channel);
        let old = self.users.insert(u.id, u.clone());
        let Some(old) = old else { return };
        if u.id != self.you && old.stream.is_none() && u.stream.is_some() && Some(u.channel) == my_channel {
            self.activity(&format!("{} started sharing their screen", u.name));
            if let Ok(client) = u16::try_from(u.id) {
                let title = format!("{} is sharing their screen", u.name);
                let watch = Some((self.con_id, client, u.name.clone()));
                let event = crate::notify::Event { kind: EventKind::ScreenShare, title, body: String::new(), watch };
                let _ = self.back.send(SessionMsg::Event(self.con_id, event));
            }
        }
        if u.id == self.you || old.channel == u.channel {
            return;
        }
        if Some(u.channel) == my_channel {
            self.activity(&format!("{} joined", u.name));
            self.event(EventKind::UserJoined, format!("{} joined your channel", u.name), String::new());
        } else if Some(old.channel) == my_channel {
            self.activity(&format!("{} left", u.name));
            self.event(EventKind::UserLeft, format!("{} left your channel", u.name), String::new());
        }
    }

    fn user_left(&mut self, id: UserId, reason: Leave) {
        self.last_voice.remove(&id);
        if id == self.you {
            self.removed = Some(match reason {
                Leave::Kicked { by, reason } => format!("Kicked from the server by {}{}", self.name(by), suffix(&reason)),
                Leave::Banned { by, reason } => format!("Kicked and banned by {}{}", self.name(by), suffix(&reason)),
                // "Kicked" so it doesn't reconnect and push the other one out in turn
                Leave::Replaced => "Kicked: connected again from somewhere else".into(),
                Leave::Quit | Leave::Lost => "Disconnected by the server".into(),
            });
            if let Some(why) = &self.removed {
                self.event(EventKind::MovedOrKicked, why.clone(), String::new());
            }
            return;
        }
        let Some(u) = self.users.remove(&id) else { return };
        let line = match reason {
            Leave::Kicked { by, reason } => format!("{} was kicked by {}{}", u.name, self.name(by), suffix(&reason)),
            Leave::Banned { by, reason } => format!("{} was banned by {}{}", u.name, self.name(by), suffix(&reason)),
            Leave::Lost => format!("{} lost the connection", u.name),
            Leave::Quit | Leave::Replaced => format!("{} left", u.name),
        };
        self.activity(&line);
        if self.me().is_some_and(|me| me.channel == u.channel) {
            self.event(EventKind::UserLeft, format!("{} left your channel", u.name), String::new());
        }
    }

    fn chat(&mut self, from: UserId, to: ChatTarget, text: String) {
        let me = from == self.you;
        let name = self.name(from);
        let uid = self.users.get(&from).map(|u| key_hex(&u.key)).unwrap_or_default();
        let my_name = self.me().map(|u| u.name.to_lowercase()).unwrap_or_default();
        let mention = !me && !my_name.is_empty() && text.to_lowercase().contains(&my_name);
        let (kind, title) = match to {
            ChatTarget::Channel => (ChatKind::Channel, None),
            ChatTarget::Server => (ChatKind::Server, None),
            ChatTarget::User(other) => {
                // the conversation is with whoever isn't us
                let peer = if me { other } else { from };
                let Some(p) = self.users.get(&peer) else { return };
                (ChatKind::Private(key_hex(&p.key)), Some(p.name.clone()))
            }
        };
        if !me {
            let plain = session::strip_bbcode(&text);
            match (&kind, mention) {
                (ChatKind::Private(_), _) => self.event(EventKind::PrivateMessage, name.clone(), plain),
                (_, true) => self.event(EventKind::Mention, format!("{name} mentioned you"), plain),
                (ChatKind::Channel, false) => self.event(EventKind::ChannelMessage, name.clone(), plain),
                (ChatKind::Server, false) => {}
            }
        }
        self.add_line(kind, title.as_deref(), chat::now_line(name, uid, me, text, mention));
    }

    /// The conversation of this kind, created (with its history) if needed.
    fn conversation(&mut self, kind: ChatKind, title: &str) -> &mut Conversation {
        if let Some(i) = self.chats.iter().position(|c| c.kind == kind) {
            return &mut self.chats[i];
        }
        let lines = self.history.load(&kind);
        self.chats.push(Conversation { kind, title: title.to_string(), client: None, lines, received: 0 });
        self.chats.last_mut().expect("just pushed")
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

    /// Hand a voice frame to playback, as if it were a TeamSpeak packet, so
    /// it gets the same jitter buffer, decoder and mixing.
    fn play(&mut self, v: banshee_client::Voice) {
        let Some(audio) = &self.audio else { return };
        // our client ids are u16; Banshee's are u32 but start at 1 and count up
        let Ok(from) = u16::try_from(v.from) else { return };
        let packet = OutAudio::new(&AudioPacket::S2C { id: v.seq, codec: CodecType::OpusVoice, from, data: &v.opus });
        if let Ok(input) = InAudioBuf::try_new(Direction::S2C, packet.into_vec()) {
            let _ = audio.ts2a.lock().unwrap().play_packet((ConnectionId(self.con_id), ClientId(from)), input);
        }
        self.last_voice.insert(v.from, (Instant::now(), v.whisper));
    }

    fn activity(&self, text: &str) {
        let _ = self.back.send(SessionMsg::Activity(self.con_id, text.to_string(), false));
    }

    fn event(&self, kind: EventKind, title: String, body: String) {
        let _ = self.back.send(SessionMsg::Event(self.con_id, crate::notify::Event { kind, title, body, watch: None }));
    }

    /// Send the current state to the manager.
    /// Capture and encode on a thread; the stream to the server opens with
    /// the first frame (so a cancelled picker shows nothing to others).
    fn share(&mut self, start: crate::video::ShareStart) {
        if self.sharing.is_some() && self.me().is_some_and(|me| me.stream.is_some()) {
            return;
        }
        // video frames and 20 ms sound packets
        let (tx, mut rx) = mpsc::channel(32);
        self.sharing = Some(Sharing::start(start.source, start.options, start.sound_app, tx));
        let (video, back, con) = (self.video.clone(), self.back.clone(), self.con_id);
        tokio::task::spawn_local(async move {
            let mut share = None;
            while let Some(msg) = rx.recv().await {
                match msg {
                    ShareMsg::Started(info) => match video.share(&info).await {
                        Ok(s) => share = Some(s),
                        Err(e) => {
                            let _ = back.send(SessionMsg::Activity(con, format!("Couldn't share: {e:#}"), false));
                            break;
                        }
                    },
                    ShareMsg::Packet(p) => {
                        if let Some(s) = share.as_mut()
                            && s.send(&p).await.is_err()
                        {
                            break;
                        }
                    }
                    ShareMsg::Failed(e) => {
                        let _ = back.send(SessionMsg::Activity(con, e, false));
                        break;
                    }
                }
            }
            if let Some(s) = share {
                s.stop();
            }
        });
    }

    /// A screen we asked for arrived: decode it onto its viewer.
    fn watch_stream(&mut self, mut w: Watching) {
        let Some(screen) = self.watching.get(&w.from).cloned() else { return };
        let audio = self.audio.clone();
        let id = stream_sound_id(self.con_id, w.from);
        tokio::task::spawn_local(async move {
            let (tx, rx) = std::sync::mpsc::channel();
            crate::video::decode(rx, screen.clone());
            let mut seq = 0u16;
            while let Ok(Some(packet)) = w.packet().await {
                if screen.ended() {
                    break;
                }
                if packet.is_audio() {
                    // the voice mixer plays it, as stereo "music"
                    let Some(audio) = &audio else { continue };
                    let out = OutAudio::new(&AudioPacket::S2C {
                        id: seq,
                        codec: CodecType::OpusMusic,
                        from: id.1.0,
                        data: &packet.data,
                    });
                    seq = seq.wrapping_add(1);
                    if let Ok(input) = InAudioBuf::try_new(Direction::S2C, out.into_vec()) {
                        let _ = audio.ts2a.lock().unwrap().play_packet(id, input);
                    }
                } else if tx.send(packet).is_err() {
                    break;
                }
            }
            screen.end();
        });
    }

    fn publish(&mut self) {
        self.fetch_avatars();
        let mut snap = self.base.clone();
        snap.server_name = self.info.name.clone();
        snap.welcome = self.info.welcome.clone();
        snap.max_users = self.info.max_users.map(|m| m.min(u16::MAX as u32) as u16).unwrap_or(0);
        let own_talking = self.last_own_audio.is_some_and(|t| t.elapsed() < TALK_HOLD);
        snap.channels = self
            .channels
            .iter()
            .map(|c| {
                let mut clients: Vec<ClientRow> = self
                    .users
                    .values()
                    .filter(|u| u.channel == c.id)
                    .map(|u| self.client_row(u, own_talking))
                    .collect();
                clients.sort_by_key(|c| c.name.to_lowercase());
                channel_row(c, self.depth(c), clients)
            })
            .collect();
        snap.own_channel = self.me().map(|u| u64::from(u.channel));
        snap.users = self.users.len();
        snap.groups = self
            .roles
            .iter()
            .map(|r| ServerGroupInfo { id: r.id.into(), badge: badge(&r.name), icon_id: 0 })
            .collect();
        snap.chats = self
            .chats
            .iter()
            .map(|c| {
                let mut c = c.clone();
                if let ChatKind::Private(uid) = &c.kind {
                    let online = self.user_by_uid(uid);
                    c.client = online.and_then(|u| u16::try_from(u.id).ok());
                    if let Some(u) = online {
                        c.title = u.name.clone();
                    }
                }
                c
            })
            .collect();
        snap.transfers = self.transfers.borrow().clone();
        let _ = self.back.send(SessionMsg::Snap(snap));
    }

    fn depth(&self, c: &Channel) -> usize {
        let mut depth = 0;
        let mut parent = c.parent;
        while let Some(p) = parent {
            depth += 1;
            parent = self.channels.iter().find(|c| c.id == p).and_then(|c| c.parent);
        }
        depth
    }

    fn client_row(&self, u: &User, own_talking: bool) -> ClientRow {
        let me = u.id == self.you;
        let (heard, whispering) = match self.last_voice.get(&u.id) {
            Some((_, w)) => (true, *w),
            None => (false, false),
        };
        let role = self.role(u.role);
        ClientRow {
            id: u16::try_from(u.id).unwrap_or(u16::MAX),
            name: u.name.clone(),
            talking: if me { own_talking } else { heard },
            input_muted: u.input_muted || u.server_muted || (me && self.input_muted),
            output_muted: u.output_muted,
            away: u.away.clone(),
            me,
            commander: false,
            priority: false,
            recording: false,
            country: String::new(),
            groups: role.map(|r| badge(&r.name)).into_iter().collect(),
            group_ids: vec![u.role.into()],
            channel_group: 0,
            talk_power_granted: u.talk_granted,
            db_id: db_id(&u.key),
            talk_request: u.talk_request.clone(),
            avatar: self.avatar(u),
            friend: false,
            blocked: false,
            whispering,
            icon: None,
            volume: 1.0,
            locally_muted: false,
            stream: u.stream.clone(),
            uid: key_hex(&u.key),
        }
    }
}

/// Where a shared screen's sound goes in the voice mixer: next to the
/// sharer's voice, with the top bit set.
fn stream_sound_id(con: ConId, from: UserId) -> crate::audio::ts_to_audio::Id {
    (ConnectionId(con), ClientId(0x8000 | (from as u16 & 0x7fff)))
}

/// Server messages start lowercase; our activity lines don't.
fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map(|c| c.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

fn suffix(reason: &str) -> String {
    if reason.is_empty() { String::new() } else { format!(": {reason}") }
}

fn badge(name: &str) -> GroupBadge {
    GroupBadge { name: name.to_string(), icon: None, builtin: None }
}

/// A TeamSpeak client limit as Banshee's (None: no limit).
fn limit(m: MaxClients) -> Option<u32> {
    match m {
        MaxClients::Limited(n) => Some(u32::from(n)),
        _ => None,
    }
}

fn finish(list: &Transfers, id: u64, result: Result<()>) {
    files::update(list, id, |t| {
        t.state = match result {
            Ok(()) => TransferState::Done,
            Err(e) => TransferState::Failed(format!("{e:#}")),
        }
    });
}

fn offline_row(m: &OfflineMessage) -> admin::OfflineMessage {
    admin::OfflineMessage {
        id: m.id,
        from_uid: key_hex(&m.from),
        subject: m.subject.clone(),
        message: m.text.clone(),
        time: date(m.sent),
        read: m.read,
    }
}

fn ban_row(b: &banshee_client::Ban) -> admin::Ban {
    admin::Ban {
        id: b.id,
        ip: b.ip.clone().unwrap_or_default(),
        name: String::new(),
        uid: b.key.map(|k| key_hex(&k)).unwrap_or_default(),
        last_nickname: b.name.clone(),
        created: date(b.created),
        secs: b.until.map(|u| u.saturating_sub(b.created)),
        by: b.by.clone(),
        reason: b.reason.clone(),
        enforcements: 0,
    }
}

fn channel_row(c: &Channel, depth: usize, clients: Vec<ClientRow>) -> ChannelRow {
    ChannelRow {
        id: u64::from(c.id),
        parent: c.parent.map(u64::from).unwrap_or(0),
        name: c.name.clone(),
        phonetic: String::new(),
        topic: c.topic.clone(),
        description: Some(c.description.clone()),
        depth,
        has_password: c.has_password,
        is_default: c.default,
        kind: if c.permanent { ChannelType::Permanent } else { ChannelType::Temporary },
        codec: Codec::OpusVoice,
        quality: 10,
        talk_power: c.talk_power.min(i32::MAX as u32) as i32,
        delete_delay: 0,
        latency: 1,
        unencrypted: false,
        subscribed: true,
        max_clients: c.max_users,
        icon: None,
        icon_id: 0,
        limit: match c.max_users {
            Some(n) => MaxClients::Limited(n.min(u16::MAX as u32) as u16),
            None => MaxClients::Unlimited,
        },
        family_limit: MaxClients::Inherited,
        clients,
    }
}

#[cfg(test)]
mod tests {
    use super::parse;

    #[test]
    fn addresses() {
        assert_eq!(parse("banshee://example.com").unwrap(), ("example.com:7700".into(), None));
        assert_eq!(parse("banshee://1.2.3.4:9000#ABcd").unwrap(), ("1.2.3.4:9000".into(), Some("abcd".into())));
        assert!(parse("ts.example.com").is_err());
    }
}
