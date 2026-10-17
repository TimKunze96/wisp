//! Networking thread: a manager that owns every server session, the audio
//! streams and the control socket. Everything runs on one current-thread tokio
//! runtime inside a `LocalSet` (tsclientlib and the audio code need that).

pub mod admin;
pub mod chat;
pub mod files;
mod session;

pub use chat::{ChatKind, ChatLine, Conversation};

pub use session::human_secs as session_human_secs;
pub use session::strip_bbcode;

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::time::Duration;

use futures::channel::mpsc as fmpsc;
use serde::Serialize;
#[cfg(unix)]
use tokio::net::UnixDatagram;
use tokio::sync::{mpsc, oneshot};
use tokio::task::LocalSet;

use crate::audio::{self, AudioData, Gate};
use crate::config::{Bookmark, Settings, VoiceMode, WhisperList};

pub use tsclientlib::{ChannelType, Codec, MaxClients};

pub type ConId = u64;

/// Quick and admin actions on another client.
#[derive(Clone, Debug)]
pub enum AdminAction {
    Poke { client: u16, message: String },
    KickChannel { client: u16, reason: String },
    KickServer { client: u16, reason: String },
    Ban { client: u16, secs: Option<u64>, reason: String },
    /// Move someone into a channel (your own, usually).
    Move { client: u16, channel: u64 },
    ServerGroup { client: u16, group: u64, add: bool },
    /// Set someone's channel group in the channel they're in.
    ChannelGroup { client: u16, group: u64 },
    TalkPower { client: u16, grant: bool },
    /// `props.name` is required.
    CreateChannel { parent: u64, props: ChannelProps },
    /// Only the fields that are set get sent.
    EditChannel { channel: u64, props: ChannelProps },
    /// Put a channel under `parent`, right below `after` (0: first).
    MoveChannel { channel: u64, parent: u64, after: u64 },
    DeleteChannel { channel: u64 },
    Subscribe { channel: u64, on: bool },
}

#[derive(Clone, Debug)]
pub enum SelfAction {
    Commander(bool),
    /// Some(message) asks for talk power, None withdraws the request.
    TalkRequest(Option<String>),
    Description(String),
}

/// Channel properties for create / edit; None means "leave as is".
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ChannelProps {
    pub name: Option<String>,
    pub phonetic: Option<String>,
    pub topic: Option<String>,
    pub description: Option<String>,
    /// Some(None) removes the password.
    pub password: Option<Option<String>>,
    pub kind: Option<ChannelType>,
    pub is_default: Option<bool>,
    pub codec: Option<Codec>,
    pub quality: Option<u8>,
    pub max_clients: Option<MaxClients>,
    pub max_family: Option<MaxClients>,
    pub talk_power: Option<i32>,
    pub delete_delay: Option<u64>,
    pub latency: Option<i32>,
    pub unencrypted: Option<bool>,
}

/// Where a channel goes relative to another one when moving it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Zone {
    Above,
    Into,
    Below,
}

/// Whether `id` sits somewhere below `ancestor` (or is it).
pub fn within(channels: &[ChannelRow], ancestor: u64, mut id: u64) -> bool {
    while id != 0 {
        if id == ancestor {
            return true;
        }
        id = channels.iter().find(|c| c.id == id).map(|c| c.parent).unwrap_or(0);
    }
    false
}

/// (parent, after) for putting channel `moving` at `zone` of `target`
/// (target 0 with `Zone::Into` means the top level), or None when that
/// changes nothing or isn't possible.
pub fn channel_drop(channels: &[ChannelRow], moving: u64, target: u64, zone: Zone) -> Option<(u64, u64)> {
    let m = channels.iter().find(|c| c.id == moving)?;
    let (t_id, t_parent) = match channels.iter().find(|c| c.id == target) {
        Some(t) => (t.id, t.parent),
        None if target == 0 && zone == Zone::Into => (0, 0),
        None => return None,
    };
    if t_id != 0 && within(channels, moving, t_id) {
        return None;
    }
    let siblings = |parent: u64| channels.iter().filter(move |c| c.parent == parent && c.id != moving);
    let (parent, after) = match zone {
        Zone::Into => (t_id, siblings(t_id).last().map(|c| c.id).unwrap_or(0)),
        Zone::Below => (t_parent, t_id),
        Zone::Above => (t_parent, siblings(t_parent).take_while(|c| c.id != t_id).last().map(|c| c.id).unwrap_or(0)),
    };
    let now_after =
        channels.iter().filter(|c| c.parent == m.parent).take_while(|c| c.id != moving).last().map(|c| c.id).unwrap_or(0);
    if parent == m.parent && after == now_after {
        return None;
    }
    Some((parent, after))
}

impl ChannelProps {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

#[derive(Debug)]
pub enum Cmd {
    Admin { con: ConId, action: AdminAction },
    Connect(Bookmark),
    Disconnect(ConId),
    SetActive(ConId),
    /// Join a channel; without a password a remembered one is used.
    Join { con: ConId, channel: u64, password: Option<String> },
    /// Talk toggle from the UI, or push-to-talk from the socket/CLI.
    Ptt(bool),
    /// Whisper key for a list (id or name): down starts, up stops.
    Whisper(String, bool),
    TogglePtt,
    SelfMute(bool),
    Deafen(bool),
    Settings(Box<Settings>),
    /// Show details (ping, version...) for this client; None closes them.
    Inspect(Option<(ConId, u16)>),
    SendText { con: ConId, to: ChatKind, text: String },
    /// Open a private conversation with this client.
    OpenChat { con: ConId, client: u16 },
    CloseChat { con: ConId, kind: ChatKind },
    /// Ask the server for a channel's description (it isn't sent up front).
    ChannelDescription { con: ConId, channel: u64 },
    Status(oneshot::Sender<Snapshot>),
    /// Away on every server (Some(message), possibly empty) or back (None).
    Away(Option<String>),
    /// Something about ourselves on one server.
    Me { con: ConId, action: SelfAction },
    /// Ask a server for a list (permissions, bans...).
    Query { con: ConId, query: admin::Query, reply: admin::Reply },
    /// An admin change from the admin window.
    Write { con: ConId, write: admin::AdminWrite },
    ServerInfo { con: ConId, reply: oneshot::Sender<admin::ServerInfo> },
    Files { con: ConId, cmd: files::FileCmd },
    FetchIcons { con: ConId, ids: Vec<u32> },
    /// Our avatar on every connected server (prepared PNG and its md5), or none.
    SetAvatar(Option<(PathBuf, String)>),
    /// Start (true) / stop (false) / toggle (None) recording.
    Record(Option<bool>),
    /// Whether the main window has focus (no chat popups then).
    Focused(bool),
    /// Disconnect everything, then answer.
    Shutdown(oneshot::Sender<()>),
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub enum Status {
    #[default]
    Connecting,
    Connected,
    Reconnecting,
    /// The server wants a (different) password.
    NeedsPassword,
    Failed(String),
}

/// The server turned down a join because of the channel password.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PasswordRequest {
    pub con: ConId,
    pub bookmark: String,
    pub channel: u64,
    pub name: String,
    /// A saved or typed password was sent and was wrong.
    pub wrong: bool,
    /// Changes with every request, so the UI asks again after another failure.
    pub serial: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct GroupBadge {
    pub name: String,
    /// Cached icon file, when the server has a custom icon for the group.
    pub icon: Option<PathBuf>,
    /// Built-in TeamSpeak icon id (< 1000) without a file.
    pub builtin: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ClientRow {
    pub id: u16,
    pub uid: String,
    /// Database id (for permissions and group membership).
    pub db_id: u64,
    pub name: String,
    pub talking: bool,
    pub input_muted: bool,
    pub output_muted: bool,
    pub away: Option<String>,
    pub me: bool,
    pub commander: bool,
    pub priority: bool,
    pub recording: bool,
    pub country: String,
    pub groups: Vec<GroupBadge>,
    pub group_ids: Vec<u64>,
    pub channel_group: u64,
    pub talk_power_granted: bool,
    pub friend: bool,
    pub blocked: bool,
    /// Their avatar picture (cached file), if they have one.
    pub avatar: Option<PathBuf>,
    /// The client's own icon (set by admins), if any.
    pub icon: Option<GroupBadge>,
    /// Whispering to us right now.
    pub whispering: bool,
    /// Asking for talk power, with their message.
    pub talk_request: Option<String>,
    pub volume: f32,
    pub locally_muted: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ChannelRow {
    pub id: u64,
    pub parent: u64,
    pub name: String,
    pub phonetic: String,
    pub topic: String,
    /// None until requested with `Cmd::ChannelDescription`.
    pub description: Option<String>,
    pub depth: usize,
    pub has_password: bool,
    pub is_default: bool,
    pub kind: ChannelType,
    pub codec: Codec,
    pub quality: u8,
    pub talk_power: i32,
    pub delete_delay: u64,
    pub latency: i32,
    pub unencrypted: bool,
    /// We get its clients and voice (subscribed).
    pub subscribed: bool,
    pub max_clients: Option<u32>,
    /// The channel's own icon, if it has one.
    pub icon: Option<GroupBadge>,
    pub icon_id: u32,
    pub limit: MaxClients,
    pub family_limit: MaxClients,
    pub clients: Vec<ClientRow>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ClientDetails {
    pub id: u16,
    pub name: String,
    pub uid: String,
    pub description: String,
    pub country: String,
    pub version: String,
    pub platform: String,
    pub groups: Vec<GroupBadge>,
    pub ping_ms: Option<f32>,
    pub packet_loss: Option<f32>,
    pub connected_secs: Option<u64>,
    pub idle_secs: u64,
    pub away: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ServerSnap {
    pub con: ConId,
    pub bookmark: String,
    pub label: String,
    pub status: Status,
    pub server_name: String,
    pub welcome: String,
    pub platform: String,
    pub version: String,
    pub users: usize,
    pub max_users: u16,
    pub channels: Vec<ChannelRow>,
    pub own_channel: Option<u64>,
    pub ping_ms: Option<f32>,
    pub packet_loss: Option<f32>,
    pub details: Option<ClientDetails>,
    /// Regular server groups, for the "Server groups" menu.
    pub groups: Vec<ServerGroupInfo>,
    /// Regular channel groups, for the "Channel group" menu.
    pub channel_groups: Vec<ServerGroupInfo>,
    /// Channel, server, then private conversations.
    pub chats: Vec<Conversation>,
    /// The server's host message to show as a popup on connect, with the
    /// mode (2: popup, 3: popup and then disconnect).
    pub host_message: Option<(String, u8)>,
    /// File downloads and uploads of this session.
    pub transfers: Vec<files::Transfer>,
    /// The server's banner (cached picture, link) and host button
    /// (cached picture, link, tooltip).
    pub banner: Option<(PathBuf, String)>,
    pub host_button: Option<(PathBuf, String, String)>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ServerGroupInfo {
    pub id: u64,
    pub badge: GroupBadge,
    pub icon_id: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Activity {
    pub time: String,
    pub server: String,
    pub text: String,
    pub chat: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Snapshot {
    pub servers: Vec<ServerSnap>,
    pub active: Option<ConId>,
    /// Mic audio is being sent right now.
    pub transmitting: bool,
    pub ptt: bool,
    pub self_muted: bool,
    pub deafened: bool,
    pub voice_mode: Option<VoiceMode>,
    pub mic_level_db: f32,
    pub activity: Vec<Activity>,
    pub audio_error: Option<String>,
    pub password_request: Option<PasswordRequest>,
    /// Our away message on every server (empty: away without a message).
    pub away: Option<String>,
    /// Name of the whisper list the mic goes to (while talking).
    pub whispering: Option<String>,
    /// File being recorded to.
    pub recording: Option<PathBuf>,
}

impl Status {
    /// The connection is over (failed or waiting for a password).
    pub fn is_final(&self) -> bool {
        matches!(self, Status::Failed(_) | Status::NeedsPassword)
    }
}

impl Snapshot {
    pub fn server(&self, con: ConId) -> Option<&ServerSnap> {
        self.servers.iter().find(|s| s.con == con)
    }
    pub fn active_server(&self) -> Option<&ServerSnap> {
        self.active.and_then(|c| self.server(c))
    }
}

/// From a session to the manager.
pub(crate) enum SessionMsg {
    Snap(ServerSnap),
    Activity(ConId, String, bool),
    Event(ConId, crate::notify::Event),
    /// (con, channel id, channel name, a password was sent)
    NeedChannelPassword(ConId, u64, String, bool),
    Ended(ConId),
}

/// From the manager to one session.
#[derive(Debug)]
pub(crate) enum SessionCmd {
    Disconnect,
    Join(u64, Option<String>),
    /// One Opus frame from the mic, and the whisper list it goes to (if any).
    Audio(Vec<u8>, Option<std::sync::Arc<WhisperList>>),
    SelfState { input_muted: bool, output_muted: bool },
    Away(Option<String>),
    Me(SelfAction),
    Recording(bool),
    /// Upload this (prepared) picture as our avatar with its md5; None removes it.
    SetAvatar(Option<(PathBuf, String)>),
    Query(admin::Query, admin::Reply),
    Write(admin::AdminWrite),
    ServerInfo(oneshot::Sender<admin::ServerInfo>),
    Files(files::FileCmd),
    /// Download these icons into the cache (if missing).
    FetchIcons(Vec<u32>),
    Settings(Box<Settings>),
    Inspect(Option<u16>),
    SendText(ChatKind, String),
    OpenChat(u16),
    CloseChat(ChatKind),
    Admin(AdminAction),
    ChannelDescription(u64),
}

#[derive(Clone)]
pub struct Handle {
    pub cmds: mpsc::UnboundedSender<Cmd>,
}

impl Handle {
    pub fn send(&self, cmd: Cmd) {
        let _ = self.cmds.send(cmd);
    }
}

/// Where a server's icons are cached (by icon id).
pub fn icon_dir(address: &str) -> PathBuf {
    crate::config::cache_dir().join("icons").join(session::sanitize(address))
}

pub fn ptt_socket() -> PathBuf {
    runtime_dir().join("wisp.sock")
}

pub fn runtime_dir() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir)
}

/// Send "down" / "up" to a running client; does nothing if none runs.
#[cfg(unix)]
pub fn send_ptt(state: &str) {
    if let Ok(sock) = std::os::unix::net::UnixDatagram::unbound() {
        let _ = sock.send_to(state.as_bytes(), ptt_socket());
    }
}

/// No datagram sockets here; the control pipe takes it.
#[cfg(not(unix))]
pub fn send_ptt(state: &str) {
    let line = match state {
        "down" | "up" => format!("ptt {state}"),
        other => other.to_string(),
    };
    let _ = crate::ctl::request(&line);
}

pub fn spawn(settings: Settings) -> (Handle, fmpsc::UnboundedReceiver<Snapshot>) {
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let (snap_tx, snap_rx) = fmpsc::unbounded();
    let handle = Handle { cmds: cmd_tx.clone() };
    std::thread::Builder::new()
        .name("ts-net".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
            let local = LocalSet::new();
            local.block_on(&rt, async {
                crate::ctl::serve(cmd_tx.clone());
                Manager::new(settings, snap_tx, &local).run(cmd_rx).await;
            });
        })
        .unwrap();
    (handle, snap_rx)
}

struct Manager {
    settings: Settings,
    audio: Result<AudioData, String>,
    sessions: HashMap<ConId, mpsc::UnboundedSender<SessionCmd>>,
    servers: Vec<ServerSnap>,
    active: Option<ConId>,
    next_id: ConId,
    ptt: bool,
    self_muted: bool,
    deafened: bool,
    activity: VecDeque<Activity>,
    out: fmpsc::UnboundedSender<Snapshot>,
    last: Snapshot,
    password_request: Option<PasswordRequest>,
    serial: u64,
    focused: bool,
    away: Option<String>,
    /// Mic audio goes to this whisper list instead of the channel.
    whisper: Option<std::sync::Arc<WhisperList>>,
    /// Apps we turned down, and since when nobody talks (restore after a moment).
    ducked: std::sync::Arc<std::sync::Mutex<crate::audio::Ducked>>,
    ducking: bool,
    quiet_since: Option<std::time::Instant>,
    recording: Option<crate::audio::record::Recording>,
    /// Bookmarks waiting to reconnect, and how often they failed in a row.
    retry: HashMap<String, std::time::Instant>,
    attempts: HashMap<String, u32>,
    from_sessions: (mpsc::UnboundedSender<SessionMsg>, mpsc::UnboundedReceiver<SessionMsg>),
}

const ACTIVITY_LEN: usize = 200;

impl Manager {
    fn new(settings: Settings, out: fmpsc::UnboundedSender<Snapshot>, local: &LocalSet) -> Self {
        let audio = audio::start(settings.input_device.as_deref(), settings.output_device.as_deref(), local)
            .map_err(|e| e.to_string());
        let mut m = Self {
            settings,
            audio,
            sessions: HashMap::new(),
            servers: Vec::new(),
            active: None,
            next_id: 1,
            ptt: false,
            self_muted: false,
            deafened: false,
            activity: VecDeque::new(),
            out,
            last: Snapshot::default(),
            password_request: None,
            serial: 0,
            focused: false,
            away: None,
            whisper: None,
            ducked: Default::default(),
            ducking: false,
            quiet_since: None,
            recording: None,
            retry: HashMap::new(),
            attempts: HashMap::new(),
            from_sessions: mpsc::unbounded_channel(),
        };
        m.apply_audio_settings();
        if m.settings.echo_cancel {
            m.route_audio();
        }
        m
    }

    /// Move our PipeWire streams to the chosen devices (pactl, on a thread).
    /// Move our PipeWire streams to the chosen devices, through the echo
    /// canceller when it's on (pactl, on a thread).
    fn route_audio(&self) {
        let (i, o) = (self.settings.input_device.clone(), self.settings.output_device.clone());
        let ec = self.settings.echo_cancel;
        std::thread::spawn(move || {
            use crate::audio::{EC_SINK, EC_SOURCE, echo_cancel, route};
            if echo_cancel(ec, i.as_deref(), o.as_deref()) {
                route(Some(EC_SOURCE), Some(EC_SINK));
            } else {
                route(i.as_deref(), o.as_deref());
            }
        });
    }

    fn apply_audio_settings(&mut self) {
        let Ok(audio) = &self.audio else { return };
        let s = &self.settings;
        audio.a2ts.lock().unwrap().set_volume(s.mic_volume);
        Gate::set_f32(&audio.gate.output_volume, s.output_volume);
        Gate::set_f32(&audio.gate.vad_threshold, s.vad_threshold_db);
        audio.gate.ptt_release_frames.store(s.ptt_release_ms / 20, std::sync::atomic::Ordering::Relaxed);
        let already = s.noise_suppression && audio::rnnoise_source(s.input_device.as_deref()).is_some();
        audio.gate.denoise.store(s.noise_suppression && !already, std::sync::atomic::Ordering::Relaxed);
        audio.gate.agc.store(s.auto_gain, std::sync::atomic::Ordering::Relaxed);
        self.update_gate();
    }

    /// The active server's own voice mode, else the global one.
    fn voice_mode(&self) -> VoiceMode {
        let bookmark = self.active.and_then(|c| self.servers.iter().find(|s| s.con == c)).map(|s| s.bookmark.as_str());
        bookmark
            .and_then(|id| self.settings.bookmarks.iter().find(|b| b.id == id))
            .and_then(|b| b.voice_mode)
            .unwrap_or(self.settings.voice_mode)
    }

    fn update_gate(&self) {
        let Ok(audio) = &self.audio else { return };
        use std::sync::atomic::Ordering::Relaxed;
        let can_talk = !self.self_muted && self.active.is_some();
        audio.gate.allowed.store(can_talk, Relaxed);
        audio.gate.ptt.store(self.ptt, Relaxed);
        audio.gate.vad_enabled.store(self.voice_mode() == VoiceMode::VoiceActivation, Relaxed);
    }

    async fn run(mut self, mut cmds: mpsc::UnboundedReceiver<Cmd>) {
        let (mic_tx, mut mic_rx) = mpsc::channel(16);
        if let Ok(audio) = &self.audio {
            audio.a2ts.lock().unwrap().set_listener(mic_tx);
        }
        let _ = std::fs::remove_file(ptt_socket());
        #[cfg(unix)]
        let ptt_sock = UnixDatagram::bind(ptt_socket()).ok();
        #[cfg(not(unix))]
        let ptt_sock: Option<()> = None;
        let mut tick = tokio::time::interval(Duration::from_millis(100));
        let mut reroute = tokio::time::interval(Duration::from_secs(10));

        let autoconnect: Vec<_> =
            self.settings.bookmarks.iter().filter(|b| b.auto_connect).cloned().collect();
        for b in autoconnect {
            self.connect(b);
        }

        let mut shutdown = None;
        loop {
            tokio::select! {
                cmd = cmds.recv() => match cmd {
                    Some(Cmd::Shutdown(reply)) => { shutdown = Some(reply); break; }
                    Some(cmd) => self.handle(cmd),
                    None => break,
                },
                msg = self.from_sessions.1.recv() => {
                    if let Some(msg) = msg { self.on_session(msg); }
                }
                packet = mic_rx.recv() => {
                    if let (Some(packet), Some(con)) = (packet, self.active) {
                        if let Some(s) = self.sessions.get(&con) {
                            let _ = s.send(SessionCmd::Audio(packet, self.whisper.clone()));
                        }
                    }
                }
                ptt = recv_ptt(ptt_sock.as_ref()) => {
                    if let Some(cmd) = ptt { self.handle(cmd); }
                }
                _ = tick.tick() => self.run_retries(),
                _ = reroute.tick() => {
                    let s = &self.settings;
                    if s.input_device.is_some() || s.output_device.is_some() || s.echo_cancel {
                        self.route_audio();
                    }
                }
            }
            self.update_ducking();
            self.publish();
        }
        for s in self.sessions.values() {
            let _ = s.send(SessionCmd::Disconnect);
        }
        if self.settings.echo_cancel {
            std::thread::spawn(|| crate::audio::echo_cancel(false, None, None));
        }
        if let Some(r) = self.recording.take() {
            r.stop();
        }
        if self.ducking {
            let ducked = self.ducked.clone();
            std::thread::spawn(move || crate::audio::duck(false, 1.0, &mut ducked.lock().unwrap()));
        }
        // give the sessions a moment to say goodbye, so no ghosts stay behind
        let deadline = tokio::time::Instant::now() + Duration::from_millis(1500);
        while !self.sessions.is_empty() {
            match tokio::time::timeout_at(deadline, self.from_sessions.1.recv()).await {
                Ok(Some(SessionMsg::Ended(con))) => {
                    self.sessions.remove(&con);
                }
                Ok(Some(_)) => {}
                _ => break,
            }
        }
        let _ = std::fs::remove_file(ptt_socket());
        let _ = std::fs::remove_file(crate::ctl::socket());
        if let Some(reply) = shutdown {
            let _ = reply.send(());
        }
    }

    fn connect(&mut self, bookmark: Bookmark) {
        self.retry.remove(&bookmark.id);
        // One connection per bookmark: reconnecting replaces the old one.
        if let Some(old) = self.servers.iter().find(|s| s.bookmark == bookmark.id).map(|s| s.con) {
            if let Some(s) = self.sessions.remove(&old) {
                let _ = s.send(SessionCmd::Disconnect);
            }
            self.servers.retain(|s| s.con != old);
        }
        let con = self.next_id;
        self.next_id += 1;
        let (tx, rx) = mpsc::unbounded_channel();
        self.sessions.insert(con, tx);
        self.servers.push(session::placeholder(con, &bookmark));
        self.active = Some(con);
        self.update_gate();
        let audio = self.audio.as_ref().ok().cloned();
        let settings = Box::new(self.settings.clone());
        let back = self.from_sessions.0.clone();
        let (muted, deaf) = (self.self_muted, self.deafened);
        let away = self.away.clone();
        tokio::task::spawn_local(session::run(con, bookmark, settings, audio, rx, back, muted, deaf, away));
    }

    fn handle(&mut self, cmd: Cmd) {
        let to = |m: &Self, con: ConId, c: SessionCmd| {
            if let Some(s) = m.sessions.get(&con) {
                let _ = s.send(c);
            }
        };
        match cmd {
            Cmd::Connect(b) => self.connect(b),
            Cmd::Disconnect(con) => {
                if let Some(s) = self.servers.iter().find(|s| s.con == con) {
                    self.retry.remove(&s.bookmark);
                }
                to(self, con, SessionCmd::Disconnect);
                self.sessions.remove(&con);
                self.servers.retain(|s| s.con != con);
                if let Ok(a) = &self.audio {
                    a.ts2a.lock().unwrap().forget(crate::ConnectionId(con));
                }
                if self.active == Some(con) {
                    self.active = self.servers.first().map(|s| s.con);
                }
                self.update_gate();
            }
            Cmd::SetActive(con) => {
                if self.sessions.contains_key(&con) {
                    self.active = Some(con);
                    self.update_gate();
                }
            }
            Cmd::Join { con, channel, password } => {
                self.password_request = None;
                to(self, con, SessionCmd::Join(channel, password))
            }
            Cmd::Ptt(down) => {
                // the whisper target stays through the release delay; a normal
                // press is what switches back to talking
                if down {
                    self.whisper = None;
                }
                self.ptt = down;
                self.update_gate();
            }
            Cmd::TogglePtt => {
                self.whisper = None;
                self.ptt = !self.ptt;
                self.update_gate();
            }
            Cmd::Whisper(list, down) => {
                if down {
                    match self.settings.whisper_list(&list) {
                        Some(l) => self.whisper = Some(std::sync::Arc::new(l.clone())),
                        None => return,
                    }
                }
                self.ptt = down;
                self.update_gate();
            }
            Cmd::SelfMute(m) => {
                if m != self.self_muted {
                    crate::notify::mic(m, &self.settings);
                }
                self.self_muted = m;
                self.broadcast_self_state();
                self.update_gate();
            }
            Cmd::Deafen(d) => {
                self.deafened = d;
                if let Ok(a) = &self.audio {
                    let v = if d { 0.0 } else { self.settings.output_volume };
                    Gate::set_f32(&a.gate.output_volume, v);
                }
                self.broadcast_self_state();
            }
            Cmd::Settings(s) => {
                let devices_changed = s.input_device != self.settings.input_device
                    || s.output_device != self.settings.output_device
                    || s.echo_cancel != self.settings.echo_cancel;
                self.settings = *s;
                if devices_changed {
                    self.route_audio();
                }
                self.apply_audio_settings();
                if self.deafened {
                    self.handle(Cmd::Deafen(true));
                }
                for s in self.sessions.values() {
                    let _ = s.send(SessionCmd::Settings(Box::new(self.settings.clone())));
                }
            }
            Cmd::Inspect(target) => {
                for (con, s) in &self.sessions {
                    let which = target.filter(|(c, _)| c == con).map(|(_, id)| id);
                    let _ = s.send(SessionCmd::Inspect(which));
                }
            }
            Cmd::SendText { con, to: kind, text } => to(self, con, SessionCmd::SendText(kind, text)),
            Cmd::OpenChat { con, client } => to(self, con, SessionCmd::OpenChat(client)),
            Cmd::CloseChat { con, kind } => to(self, con, SessionCmd::CloseChat(kind)),
            Cmd::ChannelDescription { con, channel } => to(self, con, SessionCmd::ChannelDescription(channel)),
            Cmd::Admin { con, action } => to(self, con, SessionCmd::Admin(action)),
            Cmd::Focused(f) => self.focused = f,
            Cmd::Away(away) => {
                self.away = away.clone();
                for s in self.sessions.values() {
                    let _ = s.send(SessionCmd::Away(away.clone()));
                }
            }
            Cmd::Me { con, action } => to(self, con, SessionCmd::Me(action)),
            Cmd::Query { con, query, reply } => match self.sessions.get(&con) {
                Some(s) => {
                    let _ = s.send(SessionCmd::Query(query, reply));
                }
                None => {
                    let _ = reply.send(Err("not connected".into()));
                }
            },
            Cmd::Write { con, write } => to(self, con, SessionCmd::Write(write)),
            Cmd::ServerInfo { con, reply } => to(self, con, SessionCmd::ServerInfo(reply)),
            Cmd::Files { con, cmd } => to(self, con, SessionCmd::Files(cmd)),
            Cmd::FetchIcons { con, ids } => to(self, con, SessionCmd::FetchIcons(ids)),
            Cmd::SetAvatar(a) => {
                for s in self.sessions.values() {
                    let _ = s.send(SessionCmd::SetAvatar(a.clone()));
                }
            }
            Cmd::Record(on) => {
                let on = on.unwrap_or(self.recording.is_none());
                if on == self.recording.is_some() {
                    return;
                }
                if on {
                    let server = self
                        .active
                        .and_then(|c| self.servers.iter().find(|s| s.con == c))
                        .map(|s| if s.server_name.is_empty() { s.label.clone() } else { s.server_name.clone() })
                        .unwrap_or_else(|| "Wisp".into());
                    match crate::audio::record::Recording::start(crate::audio::record::new_path(&server)) {
                        Ok(r) => {
                            self.note(
                                "Recording. Others see the recording marker; make sure everyone is fine with being recorded."
                                    .into(),
                            );
                            self.recording = Some(r);
                        }
                        Err(e) => self.note(format!("Couldn't start recording: {e}")),
                    }
                } else if let Some(r) = self.recording.take() {
                    self.note(format!("Saved recording to {}", r.path.display()));
                    r.stop();
                }
                for s in self.sessions.values() {
                    let _ = s.send(SessionCmd::Recording(on && self.recording.is_some()));
                }
            }
            Cmd::Status(reply) => {
                let _ = reply.send(self.snapshot());
            }
            Cmd::Shutdown(_) => {}
        }
    }

    fn broadcast_self_state(&self) {
        for s in self.sessions.values() {
            let _ = s.send(SessionCmd::SelfState {
                input_muted: self.self_muted,
                output_muted: self.deafened,
            });
        }
    }

    fn on_session(&mut self, msg: SessionMsg) {
        match msg {
            SessionMsg::Snap(snap) => {
                if !self.sessions.contains_key(&snap.con) {
                    return;
                }
                if snap.status == Status::Connected {
                    self.attempts.remove(&snap.bookmark);
                }
                match self.servers.iter_mut().find(|s| s.con == snap.con) {
                    Some(s) => *s = snap,
                    None => self.servers.push(snap),
                }
            }
            SessionMsg::Activity(con, text, chat) => {
                let server = self
                    .servers
                    .iter()
                    .find(|s| s.con == con)
                    .map(|s| s.label.clone())
                    .unwrap_or_default();
                self.activity.push_back(Activity { time: now_hhmm(), server, text, chat });
                while self.activity.len() > ACTIVITY_LEN {
                    self.activity.pop_front();
                }
            }
            SessionMsg::Event(con, e) => {
                // servers can be set to quiet (no sounds or popups)
                let quiet = self
                    .servers
                    .iter()
                    .find(|s| s.con == con)
                    .and_then(|s| self.settings.bookmarks.iter().find(|b| b.id == s.bookmark))
                    .is_some_and(|b| b.quiet);
                if !quiet {
                    crate::notify::fire(&e, &self.settings, self.focused);
                }
            }
            SessionMsg::NeedChannelPassword(con, channel, name, wrong) => {
                let bookmark = self.servers.iter().find(|s| s.con == con).map(|s| s.bookmark.clone()).unwrap_or_default();
                self.serial += 1;
                self.password_request =
                    Some(PasswordRequest { con, bookmark, channel, name, wrong, serial: self.serial });
            }
            SessionMsg::Ended(con) => {
                // Keep a failed server in the list so the error stays visible.
                self.sessions.remove(&con);
                if self.servers.iter().all(|s| s.con != con || s.status.is_final()) {
                    if let Some(s) = self.servers.iter_mut().find(|s| s.con == con) {
                        if !s.status.is_final() {
                            s.status = Status::Failed("Disconnected".into());
                        }
                    }
                }
                if self.active == Some(con) {
                    self.active = self.servers.iter().find(|s| self.sessions.contains_key(&s.con)).map(|s| s.con);
                }
                self.update_gate();
                self.schedule_retry(con);
            }
        }
    }

    /// An auto-connect server failed (not kicked, no password problem): try
    /// again after 5 s, 15 s, 30 s, then every minute.
    fn schedule_retry(&mut self, con: ConId) {
        let Some(s) = self.servers.iter_mut().find(|s| s.con == con) else { return };
        let Status::Failed(e) = &s.status else { return };
        let auto = self.settings.bookmarks.iter().any(|b| b.id == s.bookmark && b.auto_connect);
        if !auto || e.contains("Kicked") || e.contains("retrying") {
            return;
        }
        let attempt = self.attempts.get(&s.bookmark).copied().unwrap_or(0) + 1;
        self.attempts.insert(s.bookmark.clone(), attempt);
        let secs = match attempt {
            1 => 5,
            2 => 15,
            3 => 30,
            _ => 60,
        };
        s.status = Status::Failed(format!("{e}, retrying in {secs} s"));
        self.retry.insert(s.bookmark.clone(), std::time::Instant::now() + Duration::from_secs(secs));
    }

    fn run_retries(&mut self) {
        let now = std::time::Instant::now();
        let due: Vec<String> = self.retry.iter().filter(|(_, t)| **t <= now).map(|(b, _)| b.clone()).collect();
        for id in due {
            self.retry.remove(&id);
            if let Some(b) = self.settings.bookmarks.iter().find(|b| b.id == id).cloned() {
                self.connect(b);
            }
        }
    }

    fn snapshot(&self) -> Snapshot {
        let (transmitting, level) = match &self.audio {
            Ok(a) => (a.gate.is_open() && self.active.is_some(), a.gate.level_db()),
            Err(_) => (false, -100.0),
        };
        Snapshot {
            servers: self.servers.clone(),
            active: self.active,
            transmitting,
            ptt: self.ptt,
            self_muted: self.self_muted,
            deafened: self.deafened,
            voice_mode: Some(self.voice_mode()),
            // coarse steps, so an idle mic doesn't redraw the window constantly
            mic_level_db: (level / 2.0).round() * 2.0,
            activity: self.activity.iter().cloned().collect(),
            audio_error: self.audio.as_ref().err().cloned(),
            password_request: self.password_request.clone(),
            away: self.away.clone(),
            whispering: self.whisper.as_ref().filter(|_| transmitting).map(|w| w.name.clone()),
            recording: self.recording.as_ref().map(|r| r.path.clone()),
        }
    }

    /// A line in the activity feed that isn't about one server.
    fn note(&mut self, text: String) {
        self.activity.push_back(Activity { time: now_hhmm(), server: String::new(), text, chat: false });
        while self.activity.len() > ACTIVITY_LEN {
            self.activity.pop_front();
        }
    }

    /// Someone (not us) talks on any server: duck other apps; restore them
    /// once it's been quiet for a moment.
    fn update_ducking(&mut self) {
        let talking = self.servers.iter().flat_map(|s| &s.channels).flat_map(|c| &c.clients).any(|c| c.talking && !c.me);
        let want = if !self.settings.ducking {
            false
        } else if talking {
            self.quiet_since = None;
            true
        } else {
            let since = *self.quiet_since.get_or_insert_with(std::time::Instant::now);
            self.ducking && since.elapsed() < Duration::from_millis(800)
        };
        if want != self.ducking {
            self.ducking = want;
            let (ducked, level) = (self.ducked.clone(), self.settings.duck_level);
            std::thread::spawn(move || crate::audio::duck(want, level, &mut ducked.lock().unwrap()));
        }
    }

    fn publish(&mut self) {
        let snap = self.snapshot();
        if snap != self.last {
            self.last = snap.clone();
            let _ = self.out.unbounded_send(snap);
        }
    }
}

#[cfg(not(unix))]
async fn recv_ptt(_: Option<&()>) -> Option<Cmd> {
    futures::future::pending().await
}

#[cfg(unix)]
async fn recv_ptt(sock: Option<&UnixDatagram>) -> Option<Cmd> {
    let Some(sock) = sock else { return futures::future::pending().await };
    let mut buf = [0u8; 256];
    let n = sock.recv(&mut buf).await.ok()?;
    ptt_message(std::str::from_utf8(&buf[..n]).unwrap_or("").trim())
}

/// "down" / "up" for push-to-talk, "whisper <list> down|up" for a whisper key.
pub fn ptt_message(msg: &str) -> Option<Cmd> {
    match msg {
        "down" => Some(Cmd::Ptt(true)),
        "up" => Some(Cmd::Ptt(false)),
        _ => {
            let rest = msg.strip_prefix("whisper ")?;
            let (list, state) = rest.rsplit_once(' ')?;
            Some(Cmd::Whisper(list.to_string(), state == "down"))
        }
    }
}

fn now_hhmm() -> String {
    chrono::Local::now().format("%H:%M").to_string()
}
