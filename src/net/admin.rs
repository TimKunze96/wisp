//! Server administration: list queries (permissions, group members, bans,
//! privilege keys, complaints, log) and the write commands behind the admin
//! window.
//!
//! A query sends a request and collects the `notify...` parts the server sends
//! back; the command's result (which arrives after them) ends it. An empty
//! list comes back as the error "database empty result", which is just `[]`.

use std::net::IpAddr;

use serde::Serialize;
use tokio::sync::oneshot;
use tsclientlib::messages::c2s;
use tsclientlib::messages::s2c::InMessage;
use tsclientlib::prelude::*;
use tsclientlib::{ChannelGroupId, ChannelId, ClientDbId, Connection, MessageHandle, Permission, ServerGroupId};

/// Whose permissions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub enum PermTarget {
    ServerGroup(u64),
    ChannelGroup(u64),
    Channel(u64),
    /// By database id.
    Client(u64),
    ChannelClient { channel: u64, client: u64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum GroupKind {
    Server,
    Channel,
}

#[derive(Clone, Debug)]
pub enum Query {
    /// Every permission the server knows (id, name, description).
    PermissionList,
    Perms(PermTarget),
    ServerGroupMembers(u64),
    Bans,
    Tokens,
    Complaints,
    Log { lines: u32 },
    TempPasswords,
    /// Database id of a unique id.
    DbIdFromUid(String),
    /// A channel's files in a folder ("/" for the top).
    Files { channel: u64, path: String, password: String },
    /// Every permission that applies to a client in a channel, by source.
    Overview { channel: u64, db_id: u64 },
    /// Traffic and uptime of the server.
    ServerConnection,
    /// Offline messages to us (without their text).
    Inbox,
    /// One offline message with its text.
    InboxMessage(u32),
}

/// One permission from one source (permoverview).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct OverviewRow {
    /// "server group", "client", "channel", "channel group", "channel client"
    pub source: &'static str,
    pub id1: u64,
    pub id2: u64,
    pub perm: u32,
    pub value: i32,
    pub negated: bool,
    pub skip: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ServerConnection {
    pub uptime_secs: u64,
    pub ping_ms: f32,
    pub packet_loss: f32,
    pub sent_per_sec: u64,
    pub received_per_sec: u64,
    pub sent_per_min: u64,
    pub received_per_min: u64,
    pub sent_total: u64,
    pub received_total: u64,
    pub files_sent_total: u64,
    pub files_received_total: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct OfflineMessage {
    pub id: u32,
    pub from_uid: String,
    pub subject: String,
    pub message: String,
    pub time: String,
    pub read: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PermInfo {
    pub id: u32,
    pub name: String,
    pub description: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PermValue {
    pub id: u32,
    pub value: i32,
    pub negated: bool,
    pub skip: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Member {
    pub db_id: u64,
    pub name: String,
    pub uid: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Ban {
    pub id: u32,
    pub ip: String,
    pub name: String,
    pub uid: String,
    pub last_nickname: String,
    pub created: String,
    /// None: permanent.
    pub secs: Option<u64>,
    pub by: String,
    pub reason: String,
    pub enforcements: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Token {
    pub token: String,
    pub server_group: bool,
    pub group: u64,
    pub channel: u64,
    pub created: String,
    pub description: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Complaint {
    pub target_db_id: u64,
    pub target: String,
    pub from_db_id: u64,
    pub from: String,
    pub message: String,
    pub time: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct TempPassword {
    pub password: String,
    pub description: String,
    pub by: String,
    pub ends: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub enum Answer {
    PermissionList(Vec<PermInfo>),
    Perms(Vec<PermValue>),
    Members(Vec<Member>),
    Bans(Vec<Ban>),
    Tokens(Vec<Token>),
    Complaints(Vec<Complaint>),
    Log(Vec<String>),
    TempPasswords(Vec<TempPassword>),
    DbId(u64),
    Files(Vec<super::files::FileEntry>),
    Inbox(Vec<OfflineMessage>),
    Overview(Vec<OverviewRow>),
    ServerConnection(ServerConnection),
}

pub type Reply = oneshot::Sender<Result<Answer, String>>;

/// Admin writes that need more than the bookkeeping helpers offer.
#[derive(Clone, Debug)]
pub enum AdminWrite {
    SetPerm { target: PermTarget, perm: u32, value: i32, negated: bool, skip: bool },
    DelPerm { target: PermTarget, perm: u32 },
    AddGroup { kind: GroupKind, name: String },
    CopyGroup { kind: GroupKind, from: u64, name: String },
    RenameGroup { kind: GroupKind, id: u64, name: String },
    DelGroup { kind: GroupKind, id: u64 },
    AddMember { group: u64, db_id: u64 },
    RemoveMember { group: u64, db_id: u64 },
    AddBan { ip: String, name: String, uid: String, secs: Option<u64>, reason: String },
    DelBan { id: u32 },
    AddToken { server_group: bool, group: u64, channel: u64, description: String },
    DelToken { token: String },
    UseToken { token: String },
    AddComplaint { db_id: u64, message: String },
    DelComplaint { target: u64, from: u64 },
    EditServer(Box<ServerProps>),
    AddTempPassword { password: String, description: String, secs: u64 },
    DelTempPassword { password: String },
    SendOffline { uid: String, subject: String, message: String },
    DelOffline { id: u32 },
    MarkOffline { id: u32, read: bool },
}

/// Server settings; None leaves a field as it is.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ServerProps {
    pub name: Option<String>,
    pub phonetic_name: Option<String>,
    pub welcome_message: Option<String>,
    pub max_clients: Option<u16>,
    pub reserved_slots: Option<u16>,
    /// Some("") removes the password.
    pub password: Option<String>,
    pub hostmessage: Option<String>,
    /// 0 none, 1 log, 2 modal, 3 modal and quit
    pub hostmessage_mode: Option<u8>,
    pub hostbanner_url: Option<String>,
    pub hostbanner_gfx_url: Option<String>,
    pub hostbutton_tooltip: Option<String>,
    pub hostbutton_url: Option<String>,
    pub hostbutton_gfx_url: Option<String>,
    pub default_server_group: Option<u64>,
    pub default_channel_group: Option<u64>,
    pub default_channel_admin_group: Option<u64>,
    pub needed_security_level: Option<u8>,
    pub temp_delete_delay: Option<u64>,
}

/// What the admin window shows on the Server tab (from our bookkeeping).
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ServerInfo {
    pub name: String,
    pub phonetic_name: String,
    pub welcome_message: String,
    pub max_clients: u16,
    pub hostmessage: String,
    pub hostmessage_mode: u8,
    pub hostbanner_url: String,
    pub hostbanner_gfx_url: String,
    pub hostbutton_tooltip: String,
    pub hostbutton_url: String,
    pub hostbutton_gfx_url: String,
    pub default_server_group: u64,
    pub default_channel_group: u64,
    pub temp_delete_delay: u64,
    pub created: String,
    pub platform: String,
    pub version: String,
}

pub fn server_info(state: &tsclientlib::data::Connection) -> ServerInfo {
    use tsclientlib::HostMessageMode as M;
    let s = &state.server;
    ServerInfo {
        name: s.name.clone(),
        phonetic_name: s.phonetic_name.clone(),
        welcome_message: s.welcome_message.clone(),
        max_clients: s.max_clients,
        hostmessage: s.hostmessage.clone(),
        hostmessage_mode: match s.hostmessage_mode {
            M::None => 0,
            M::Log => 1,
            M::Modal => 2,
            M::Modalquit => 3,
        },
        hostbanner_url: s.hostbanner_url.clone(),
        hostbanner_gfx_url: s.hostbanner_gfx_url.clone(),
        hostbutton_tooltip: s.hostbutton_tooltip.clone(),
        hostbutton_url: s.hostbutton_url.clone(),
        hostbutton_gfx_url: s.hostbutton_gfx_url.clone(),
        default_server_group: s.default_server_group.0,
        default_channel_group: s.default_channel_group.0,
        temp_delete_delay: s.temp_channel_default_delete_delay.whole_seconds().max(0) as u64,
        created: date(s.created),
        platform: s.platform.clone(),
        version: s.version.clone(),
    }
}

fn date(t: time::OffsetDateTime) -> String {
    use chrono::TimeZone;
    chrono::Local
        .timestamp_opt(t.unix_timestamp(), 0)
        .single()
        .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_default()
}

/// A base64 unique id as tsclientlib wants it (raw bytes).
fn uid_from(s: &str) -> Option<tsclientlib::UidBuf> {
    use base64::prelude::*;
    BASE64_STANDARD.decode(s.trim()).ok().map(tsclientlib::UidBuf)
}

/// The kind of answer a query collects into.
fn empty(q: &Query) -> Answer {
    match q {
        Query::PermissionList => Answer::PermissionList(Vec::new()),
        Query::Perms(_) => Answer::Perms(Vec::new()),
        Query::ServerGroupMembers(_) => Answer::Members(Vec::new()),
        Query::Bans => Answer::Bans(Vec::new()),
        Query::Tokens => Answer::Tokens(Vec::new()),
        Query::Complaints => Answer::Complaints(Vec::new()),
        Query::Log { .. } => Answer::Log(Vec::new()),
        Query::TempPasswords => Answer::TempPasswords(Vec::new()),
        Query::DbIdFromUid(_) => Answer::DbId(0),
        Query::Files { .. } => Answer::Files(Vec::new()),
        Query::Inbox | Query::InboxMessage(_) => Answer::Inbox(Vec::new()),
        Query::Overview { .. } => Answer::Overview(Vec::new()),
        Query::ServerConnection => Answer::ServerConnection(ServerConnection::default()),
    }
}

/// Queries waiting for their answer, in the order they were sent.
#[derive(Default)]
pub struct Pending {
    queries: Vec<(MessageHandle, Answer, Reply)>,
}

fn one<T>(part: T) -> std::iter::Once<T> {
    std::iter::once(part)
}

/// Every permission of a TeamSpeak 3.13 server. Over the client protocol the
/// server only sends group boundaries, not names, so (like the official
/// client) we carry the ids and names.
/// Ids from 0x8000 are the "needed modify power" grant of the permission
/// `id & 0x7fff`.
pub fn permissions() -> &'static [PermInfo] {
    static TABLE: std::sync::OnceLock<Vec<PermInfo>> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| {
        include_str!("permissions.tsv")
            .lines()
            .filter(|l| !l.starts_with('#'))
            .filter_map(|l| {
                let mut f = l.splitn(2, '\t');
                let id = f.next()?.parse().ok()?;
                let name = f.next()?.to_string();
                Some(PermInfo { id, description: describe(&name), name })
            })
            .collect()
    })
}

/// A plain description from the name: b_channel_create_child -> "Channel create child".
fn describe(name: &str) -> String {
    let (kind, rest) = name.split_at(name.find('_').map(|i| i + 1).unwrap_or(0));
    let words = rest.replace('_', " ");
    let mut text = match kind {
        "b_" => format!("Allowed: {words}"),
        "i_" => format!("Value: {words}"),
        _ => words,
    };
    if let Some(first) = text.get(0..1) {
        text.replace_range(0..1, &first.to_uppercase());
    }
    text
}

/// A permission's id by name (from the bundled table).
pub fn perm_id(name: &str) -> Option<u32> {
    permissions().iter().find(|p| p.name == name).map(|p| p.id)
}

/// Set (Some) or remove (None) an icon through the i_icon_id permission.
pub fn icon_write(target: PermTarget, icon: Option<u32>) -> Option<AdminWrite> {
    let perm = perm_id("i_icon_id")?;
    Some(match icon {
        // icon ids are CRC32s; the permission value is the same 32 bits, signed
        Some(id) => AdminWrite::SetPerm { target, perm, value: id as i32, negated: false, skip: false },
        None => AdminWrite::DelPerm { target, perm },
    })
}

impl Pending {
    pub fn send(&mut self, con: &mut Connection, q: Query, reply: Reply) {
        let cmd = match &q {
            Query::PermissionList => {
                let _ = reply.send(Ok(Answer::PermissionList(permissions().to_vec())));
                return;
            }
            Query::Perms(t) => match *t {
                PermTarget::ServerGroup(g) => c2s::OutServerGroupPermListRequestMessage::new(&mut one(
                    c2s::OutServerGroupPermListRequestPart { server_group_id: ServerGroupId(g) },
                )),
                PermTarget::ChannelGroup(g) => c2s::OutChannelGroupPermListRequestMessage::new(&mut one(
                    c2s::OutChannelGroupPermListRequestPart { channel_group: ChannelGroupId(g) },
                )),
                PermTarget::Channel(c) => c2s::OutChannelPermListRequestMessage::new(&mut one(
                    c2s::OutChannelPermListRequestPart { channel_id: ChannelId(c) },
                )),
                PermTarget::Client(db) => c2s::OutClientPermListRequestMessage::new(&mut one(
                    c2s::OutClientPermListRequestPart { client_db_id: ClientDbId(db) },
                )),
                PermTarget::ChannelClient { channel, client } => {
                    c2s::OutChannelClientPermListRequestMessage::new(&mut one(c2s::OutChannelClientPermListRequestPart {
                        channel_id: ChannelId(channel),
                        client_db_id: ClientDbId(client),
                    }))
                }
            },
            Query::ServerGroupMembers(g) => c2s::OutServerGroupClientListRequestMessage::new(&mut one(
                c2s::OutServerGroupClientListRequestPart { server_group_id: ServerGroupId(*g) },
            )),
            Query::Bans => c2s::OutBanListRequestMessage::new(),
            Query::Tokens => c2s::OutPrivilegeKeyListRequestMessage::new(),
            Query::Complaints => {
                c2s::OutComplainListRequestMessage::new(&mut one(c2s::OutComplainListRequestPart { target_client_db_id: None }))
            }
            Query::Log { lines } => c2s::OutLogViewMessage::new(&mut one(c2s::OutLogViewPart {
                // the server refuses more than 100 ("parameter invalid size")
                lines: Some((*lines).clamp(1, 100)),
                reverse: Some(true),
                instance_log: None,
                offset: None,
            })),
            Query::TempPasswords => c2s::OutServerTempPasswordListRequestMessage::new(),
            Query::Inbox => c2s::OutOfflineMessageListRequestMessage::new(),
            Query::Overview { channel, db_id } => c2s::OutPermOverviewRequestMessage::new(&mut one(c2s::OutPermOverviewRequestPart {
                channel_id: ChannelId(*channel),
                client_db_id: ClientDbId(*db_id),
                permission_id: Some(Permission(0)),
                permission_name_id: None,
            })),
            // handled in the session (the answer goes into the bookkeeping)
            Query::ServerConnection => c2s::OutServerConnectionInfoRequestMessage::new(),
            Query::InboxMessage(id) => {
                c2s::OutOfflineMessageGetMessage::new(&mut one(c2s::OutOfflineMessageGetPart { message_id: *id }))
            }
            Query::Files { channel, path, password } => {
                let state = match con.get_state() {
                    Ok(s) => s,
                    Err(e) => {
                        let _ = reply.send(Err(e.to_string()));
                        return;
                    }
                };
                match state.channels.get(&ChannelId(*channel)) {
                    Some(c) => c.file_list(password, path).to_packet(),
                    // channel 0: the server's own files (icons, avatars)
                    None if *channel == 0 => c2s::OutFileListRequestMessage::new(&mut one(c2s::OutFileListRequestPart {
                        channel_id: ChannelId(0),
                        channel_password: "".into(),
                        path: path.as_str().into(),
                    })),
                    None => {
                        let _ = reply.send(Err("channel is gone".into()));
                        return;
                    }
                }
            }
            Query::DbIdFromUid(uid) => {
                let Some(uid) = uid_from(uid) else {
                    let _ = reply.send(Err("not a unique id".into()));
                    return;
                };
                c2s::OutClientDbIdFromUidRequestMessage::new(&mut one(c2s::OutClientDbIdFromUidRequestPart {
                    client_uid: std::borrow::Cow::Borrowed(uid.as_ref()),
                }))
            }
        };
        match cmd.send_with_result(con) {
            Ok(handle) => self.queries.push((handle, empty(&q), reply)),
            Err(e) => {
                let _ = reply.send(Err(e.to_string()));
            }
        }
    }

    /// A `notify...` part for one of our queries? Returns whether it was used.
    pub fn collect(&mut self, msg: &InMessage) -> bool {
        // the oldest query of the matching kind gets it
        macro_rules! into {
            ($variant:ident, $items:expr) => {{
                if let Some((_, Answer::$variant(list), _)) =
                    self.queries.iter_mut().find(|(_, a, _)| matches!(a, Answer::$variant(_)))
                {
                    list.extend($items);
                    return true;
                }
                false
            }};
        }
        let perm = |id: Option<Permission>, value: i32, negated: bool, skip: bool| {
            id.map(|p| PermValue { id: p.0, value, negated, skip })
        };
        match msg {
            InMessage::PermList(m) => into!(
                PermissionList,
                m.iter().filter_map(|p| Some(PermInfo {
                    id: p.permission_id?.0,
                    name: p.permission_name.clone()?,
                    description: p.permission_description.clone().unwrap_or_default(),
                }))
            ),
            InMessage::ServerGroupPermList(m) => into!(
                Perms,
                m.iter().filter_map(|p| perm(p.permission_id, p.permission_value, p.permission_negated, p.permission_skip))
            ),
            InMessage::ChannelGroupPermList(m) => into!(
                Perms,
                m.iter().filter_map(|p| perm(p.permission_id, p.permission_value, p.permission_negated, p.permission_skip))
            ),
            InMessage::ChannelPermList(m) => into!(
                Perms,
                m.iter().map(|p| PermValue {
                    id: p.permission_id.0,
                    value: p.permission_value,
                    negated: p.permission_negated,
                    skip: p.permission_skip,
                })
            ),
            InMessage::ClientPermList(m) => into!(
                Perms,
                m.iter().filter_map(|p| perm(p.permission_id, p.permission_value, p.permission_negated, p.permission_skip))
            ),
            InMessage::ChannelClientPermList(m) => into!(
                Perms,
                m.iter().filter_map(|p| perm(p.permission_id, p.permission_value, p.permission_negated, p.permission_skip))
            ),
            InMessage::ServerGroupClientList(m) => into!(
                Members,
                m.iter().map(|p| Member {
                    db_id: p.client_db_id.0,
                    name: p.name.clone().unwrap_or_default(),
                    uid: p.uid.as_ref().map(|u| u.as_ref().to_string()).unwrap_or_default(),
                })
            ),
            InMessage::BanList(m) => into!(
                Bans,
                m.iter().map(|b| Ban {
                    id: b.ban_id,
                    ip: if b.ip.is_unspecified() { String::new() } else { b.ip.to_string() },
                    name: b.name.clone(),
                    uid: b.uid.as_ref().to_string(),
                    last_nickname: b.last_nickname.clone(),
                    created: date(b.created),
                    secs: (b.duration.whole_seconds() > 0).then(|| b.duration.whole_seconds() as u64),
                    by: b.invoker_name.clone(),
                    reason: b.reason.clone(),
                    enforcements: b.enforcements,
                })
            ),
            InMessage::TokenList(m) => into!(
                Tokens,
                m.iter().map(|t| Token {
                    token: t.token.clone(),
                    server_group: t.token_type == tsclientlib::TokenType::ServerGroup,
                    group: t.token_id1,
                    channel: t.token_id2.0,
                    created: date(t.token_create_time),
                    description: t.token_description.clone(),
                })
            ),
            InMessage::ComplainList(m) => into!(
                Complaints,
                m.iter().map(|c| Complaint {
                    target_db_id: c.target_client_db_id.0,
                    target: c.target_name.clone(),
                    from_db_id: c.from_client_db_id.0,
                    from: c.from_name.clone(),
                    message: c.message.clone(),
                    time: date(c.timestamp),
                })
            ),
            InMessage::ServerLog(m) => into!(Log, m.iter().flat_map(|l| l.log.lines().map(str::to_string).collect::<Vec<_>>())),
            InMessage::ServerTempPasswordList(m) => into!(
                TempPasswords,
                m.iter().map(|p| TempPassword {
                    password: p.password_clear.clone(),
                    description: p.description.clone(),
                    by: p.nickname.clone(),
                    ends: date(p.end),
                })
            ),
            InMessage::FileList(m) => into!(
                Files,
                m.iter().map(|f| super::files::FileEntry {
                    name: f.name.clone(),
                    size: f.size,
                    date: date(f.date_time),
                    is_dir: !f.is_file,
                })
            ),
            InMessage::PermOverview(m) => into!(
                Overview,
                m.iter().map(|p| {
                    use tsclientlib::PermissionType as T;
                    OverviewRow {
                        source: match p.permission_type {
                            T::ServerGroup => "server group",
                            T::GlobalClient => "client",
                            T::Channel => "channel",
                            T::ChannelGroup => "channel group",
                            T::ChannelClient => "channel client",
                        },
                        id1: p.id1,
                        id2: p.id2,
                        perm: p.permission_id.0,
                        value: p.permission_value,
                        negated: p.permission_negated,
                        skip: p.permission_skip,
                    }
                })
            ),
            InMessage::OfflineMessageList(m) => into!(
                Inbox,
                m.iter().map(|o| OfflineMessage {
                    id: o.message_id,
                    from_uid: o.client_uid.as_ref().to_string(),
                    subject: o.subject.clone(),
                    message: String::new(),
                    time: date(o.timestamp),
                    read: o.is_read,
                })
            ),
            InMessage::OfflineMessage(m) => into!(
                Inbox,
                m.iter().map(|o| OfflineMessage {
                    id: o.message_id,
                    from_uid: o.client_uid.as_ref().to_string(),
                    subject: o.subject.clone(),
                    message: o.message.clone(),
                    time: date(o.timestamp),
                    read: true,
                })
            ),
            InMessage::ClientDbIdFromUid(m) => {
                if let Some((_, a @ Answer::DbId(_), _)) =
                    self.queries.iter_mut().find(|(_, a, _)| matches!(a, Answer::DbId(_)))
                {
                    if let Some(p) = m.iter().next() {
                        *a = Answer::DbId(p.client_db_id.0);
                    }
                    return true;
                }
                false
            }
            _ => false,
        }
    }

    /// The result of some command: ends the query it belongs to, if any.
    pub fn finish(&mut self, handle: MessageHandle, result: &Result<(), tsclientlib::CommandError>) -> bool {
        let Some(i) = self.queries.iter().position(|(h, ..)| *h == handle) else { return false };
        let (_, answer, reply) = self.queries.remove(i);
        let out = match result {
            Ok(()) => Ok(answer),
            // nothing to list
            Err(e) if format!("{e:?}").contains("DatabaseEmptyResult") => Ok(answer),
            Err(e) => Err(super::session::plain_error(&e.to_string())),
        };
        let _ = reply.send(out);
        true
    }
}

pub fn write(con: &mut Connection, w: &AdminWrite) -> anyhow::Result<(MessageHandle, String)> {
    use std::borrow::Cow;
    let state = con.get_state()?;
    let sgroup = |id: u64| state.server_groups.get(&ServerGroupId(id)).map(|g| g.name.clone()).unwrap_or_default();
    let cgroup = |id: u64| state.channel_groups.get(&ChannelGroupId(id)).map(|g| g.name.clone()).unwrap_or_default();
    let (cmd, what) = match w {
        AdminWrite::SetPerm { target, perm, value, negated, skip } => {
            let p = Some(Permission(*perm));
            let cmd = match *target {
                PermTarget::ServerGroup(g) => c2s::OutServerGroupAddPermMessage::new(&mut one(c2s::OutServerGroupAddPermPart {
                    server_group_id: ServerGroupId(g),
                    permission_id: p,
                    permission_name_id: None,
                    permission_value: *value,
                    permission_negated: *negated,
                    permission_skip: *skip,
                })),
                PermTarget::ChannelGroup(g) => c2s::OutChannelGroupAddPermMessage::new(&mut one(c2s::OutChannelGroupAddPermPart {
                    channel_group: ChannelGroupId(g),
                    permission_id: p,
                    permission_name_id: None,
                    permission_value: *value,
                })),
                PermTarget::Channel(c) => c2s::OutChannelAddPermMessage::new(&mut one(c2s::OutChannelAddPermPart {
                    channel_id: ChannelId(c),
                    permission_id: p,
                    permission_name_id: None,
                    permission_value: *value,
                })),
                PermTarget::Client(db) => c2s::OutClientAddPermMessage::new(&mut one(c2s::OutClientAddPermPart {
                    client_db_id: ClientDbId(db),
                    permission_id: p,
                    permission_name_id: None,
                    permission_value: *value,
                    permission_skip: *skip,
                })),
                PermTarget::ChannelClient { channel, client } => {
                    c2s::OutChannelClientAddPermMessage::new(&mut one(c2s::OutChannelClientAddPermPart {
                        channel_id: ChannelId(channel),
                        client_db_id: ClientDbId(client),
                        permission_id: p,
                        permission_name_id: None,
                        permission_value: *value,
                    }))
                }
            };
            (cmd, "Changed a permission".to_string())
        }
        AdminWrite::DelPerm { target, perm } => {
            let p = Some(Permission(*perm));
            let cmd = match *target {
                PermTarget::ServerGroup(g) => c2s::OutServerGroupDelPermMessage::new(&mut one(c2s::OutServerGroupDelPermPart {
                    server_group_id: ServerGroupId(g),
                    permission_id: p,
                    permission_name_id: None,
                })),
                PermTarget::ChannelGroup(g) => c2s::OutChannelGroupDelPermMessage::new(&mut one(c2s::OutChannelGroupDelPermPart {
                    channel_group: ChannelGroupId(g),
                    permission_id: p,
                    permission_name_id: None,
                })),
                PermTarget::Channel(c) => c2s::OutChannelDelPermMessage::new(&mut one(c2s::OutChannelDelPermPart {
                    channel_id: ChannelId(c),
                    permission_id: p,
                    permission_name_id: None,
                })),
                PermTarget::Client(db) => c2s::OutClientDelPermMessage::new(&mut one(c2s::OutClientDelPermPart {
                    client_db_id: ClientDbId(db),
                    permission_id: p,
                    permission_name_id: None,
                })),
                PermTarget::ChannelClient { channel, client } => {
                    c2s::OutChannelClientDelPermMessage::new(&mut one(c2s::OutChannelClientDelPermPart {
                        channel_id: ChannelId(channel),
                        client_db_id: ClientDbId(client),
                        permission_id: p,
                        permission_name_id: None,
                    }))
                }
            };
            (cmd, "Removed a permission".to_string())
        }
        AdminWrite::AddGroup { kind: GroupKind::Server, name } => (
            c2s::OutServerGroupAddMessage::new(&mut one(c2s::OutServerGroupAddPart { name: Cow::Borrowed(name), group_type: None })),
            format!("Created server group {name}"),
        ),
        AdminWrite::AddGroup { kind: GroupKind::Channel, name } => (
            c2s::OutChannelGroupAddMessage::new(&mut one(c2s::OutChannelGroupAddPart { name: Cow::Borrowed(name), group_type: None })),
            format!("Created channel group {name}"),
        ),
        AdminWrite::CopyGroup { kind: GroupKind::Server, from, name } => (
            c2s::OutServerGroupCopyMessage::new(&mut one(c2s::OutServerGroupCopyPart {
                source_server_group_id: ServerGroupId(*from),
                target_server_group_id: ServerGroupId(0),
                name: Cow::Borrowed(name),
                group_type: tsclientlib::GroupType::Regular,
            })),
            format!("Copied {} to {name}", sgroup(*from)),
        ),
        AdminWrite::CopyGroup { kind: GroupKind::Channel, from, name } => (
            c2s::OutChannelGroupCopyMessage::new(&mut one(c2s::OutChannelGroupCopyPart {
                source_channel_group_id: ChannelGroupId(*from),
                target_channel_group_id: ChannelGroupId(0),
                name: Cow::Borrowed(name),
                group_type: tsclientlib::GroupType::Regular,
            })),
            format!("Copied {} to {name}", cgroup(*from)),
        ),
        AdminWrite::RenameGroup { kind: GroupKind::Server, id, name } => (
            c2s::OutServerGroupRenameMessage::new(&mut one(c2s::OutServerGroupRenamePart {
                server_group_id: ServerGroupId(*id),
                name: Cow::Borrowed(name),
            })),
            format!("Renamed {} to {name}", sgroup(*id)),
        ),
        AdminWrite::RenameGroup { kind: GroupKind::Channel, id, name } => (
            c2s::OutChannelGroupRenameMessage::new(&mut one(c2s::OutChannelGroupRenamePart {
                channel_group: ChannelGroupId(*id),
                name: Cow::Borrowed(name),
            })),
            format!("Renamed {} to {name}", cgroup(*id)),
        ),
        AdminWrite::DelGroup { kind: GroupKind::Server, id } => (
            c2s::OutServerGroupDelMessage::new(&mut one(c2s::OutServerGroupDelPart { server_group_id: ServerGroupId(*id), force: true })),
            format!("Deleted server group {}", sgroup(*id)),
        ),
        AdminWrite::DelGroup { kind: GroupKind::Channel, id } => (
            c2s::OutChannelGroupDelMessage::new(&mut one(c2s::OutChannelGroupDelPart { channel_group: ChannelGroupId(*id), force: true })),
            format!("Deleted channel group {}", cgroup(*id)),
        ),
        AdminWrite::AddMember { group, db_id } => (
            c2s::OutServerGroupAddClientMessage::new(&mut one(c2s::OutServerGroupAddClientPart {
                server_group_id: ServerGroupId(*group),
                client_db_id: ClientDbId(*db_id),
            })),
            format!("Added someone to {}", sgroup(*group)),
        ),
        AdminWrite::RemoveMember { group, db_id } => (
            c2s::OutServerGroupDelClientMessage::new(&mut one(c2s::OutServerGroupDelClientPart {
                server_group_id: ServerGroupId(*group),
                client_db_id: ClientDbId(*db_id),
            })),
            format!("Removed someone from {}", sgroup(*group)),
        ),
        AdminWrite::AddBan { ip, name, uid, secs, reason } => {
            let ip: Option<IpAddr> = ip.parse().ok();
            let uid = (!uid.is_empty()).then(|| uid_from(uid)).flatten();
            let what = format!("Added a ban for {}", [name.as_str(), &ip.map(|i| i.to_string()).unwrap_or_default()].iter().find(|s| !s.is_empty()).copied().unwrap_or("a unique id"));
            (
                c2s::OutBanAddMessage::new(&mut one(c2s::OutBanAddPart {
                    ip,
                    name: (!name.is_empty()).then(|| Cow::Borrowed(name.as_str())),
                    uid: uid.as_ref().map(|u| Cow::Borrowed(u.as_ref())),
                    time: secs.map(|s| time::Duration::seconds(s as i64)),
                    ban_reason: (!reason.is_empty()).then(|| Cow::Borrowed(reason.as_str())),
                })),
                what,
            )
        }
        AdminWrite::DelBan { id } => (
            c2s::OutBanDelMessage::new(&mut one(c2s::OutBanDelPart { ban_id: *id })),
            "Removed a ban".to_string(),
        ),
        AdminWrite::AddToken { server_group, group, channel, description } => (
            c2s::OutPrivilegeKeyAddRequestMessage::new(&mut one(c2s::OutPrivilegeKeyAddRequestPart {
                token_type: if *server_group { tsclientlib::TokenType::ServerGroup } else { tsclientlib::TokenType::ChannelGroup },
                token_id1: *group,
                token_id2: ChannelId(*channel),
                token_description: (!description.is_empty()).then(|| Cow::Borrowed(description.as_str())),
                token_custom_set: None,
            })),
            format!("Created a privilege key for {}", if *server_group { sgroup(*group) } else { cgroup(*group) }),
        ),
        AdminWrite::DelToken { token } => (
            c2s::OutPrivilegeKeyDeleteMessage::new(&mut one(c2s::OutPrivilegeKeyDeletePart { token: Cow::Borrowed(token) })),
            "Deleted a privilege key".to_string(),
        ),
        AdminWrite::UseToken { token } => (
            c2s::OutPrivilegeKeyUseMessage::new(&mut one(c2s::OutPrivilegeKeyUsePart { token: Cow::Borrowed(token) })),
            "Used a privilege key".to_string(),
        ),
        AdminWrite::AddComplaint { db_id, message } => (
            c2s::OutComplainAddMessage::new(&mut one(c2s::OutComplainAddPart {
                target_client_db_id: ClientDbId(*db_id),
                message: Cow::Borrowed(message),
            })),
            "Filed a complaint".to_string(),
        ),
        AdminWrite::DelComplaint { target, from } => (
            c2s::OutComplainDelMessage::new(&mut one(c2s::OutComplainDelPart {
                target_client_db_id: ClientDbId(*target),
                from_client_db_id: ClientDbId(*from),
            })),
            "Removed a complaint".to_string(),
        ),
        AdminWrite::EditServer(p) => {
            use tsclientlib::HostMessageMode as M;
            fn s(v: &Option<String>) -> Option<Cow<'_, str>> {
                v.as_deref().map(Cow::Borrowed)
            }
            (
                c2s::OutServerEditMessage::new(&mut one(c2s::OutServerEditPart {
                    server_id: None,
                    name: s(&p.name),
                    welcome_message: s(&p.welcome_message),
                    max_clients: p.max_clients,
                    password: s(&p.password),
                    hostmessage: s(&p.hostmessage),
                    hostmessage_mode: p.hostmessage_mode.map(|m| match m {
                        1 => M::Log,
                        2 => M::Modal,
                        3 => M::Modalquit,
                        _ => M::None,
                    }),
                    hostbanner_url: s(&p.hostbanner_url),
                    hostbanner_gfx_url: s(&p.hostbanner_gfx_url),
                    hostbanner_gfx_interval: None,
                    hostbutton_tooltip: s(&p.hostbutton_tooltip),
                    hostbutton_url: s(&p.hostbutton_url),
                    hostbutton_gfx_url: s(&p.hostbutton_gfx_url),
                    icon: None,
                    reserved_slots: p.reserved_slots,
                    hostbanner_mode: None,
                    nickname: None,
                    max_download_bandwidth_total: None,
                    max_upload_bandwidth_total: None,
                    download_quota: None,
                    upload_quota: None,
                    antiflood_points_tick_reduce: None,
                    antiflood_points_to_command_block: None,
                    antiflood_points_to_ip_block: None,
                    codec_encryption_mode: None,
                    needed_identity_security_level: p.needed_security_level,
                    default_server_group: p.default_server_group.map(ServerGroupId),
                    default_channel_group: p.default_channel_group.map(ChannelGroupId),
                    default_channel_admin_group: p.default_channel_admin_group.map(ChannelGroupId),
                    complain_autoban_count: None,
                    complain_autoban_time: None,
                    complain_remove_time: None,
                    min_clients_in_channel_before_forced_silence: None,
                    priority_speaker_dimm_modificator: None,
                    phonetic_name: s(&p.phonetic_name),
                    temp_channel_default_delete_delay: p.temp_delete_delay.map(|d| time::Duration::seconds(d as i64)),
                    weblist_enabled: None,
                    log_client: None,
                    log_query: None,
                    log_channel: None,
                    log_permissions: None,
                    log_server: None,
                    log_filetransfer: None,
                })),
                "Updated the server settings".to_string(),
            )
        }
        AdminWrite::AddTempPassword { password, description, secs } => (
            c2s::OutServerTempPasswordAddMessage::new(&mut one(c2s::OutServerTempPasswordAddPart {
                password: Cow::Borrowed(password),
                description: Cow::Borrowed(description),
                duration: time::Duration::seconds(*secs as i64),
                target_channel_id: ChannelId(0),
                target_channel_password: Cow::Borrowed(""),
            })),
            "Added a temporary server password".to_string(),
        ),
        AdminWrite::SendOffline { uid, subject, message } => {
            let uid = uid_from(uid).ok_or_else(|| anyhow::anyhow!("not a unique id"))?;
            (
                c2s::OutOfflineMessageAddMessage::new(&mut one(c2s::OutOfflineMessageAddPart {
                    client_uid: Cow::Borrowed(uid.as_ref()),
                    subject: Cow::Borrowed(subject),
                    message: Cow::Borrowed(message),
                })),
                "Sent an offline message".to_string(),
            )
        }
        AdminWrite::DelOffline { id } => (
            c2s::OutOfflineMessageDelMessage::new(&mut one(c2s::OutOfflineMessageDelPart { message_id: *id })),
            "Deleted an offline message".to_string(),
        ),
        AdminWrite::MarkOffline { id, read } => (
            c2s::OutOfflineMessageUpdateFlagMessage::new(&mut one(c2s::OutOfflineMessageUpdateFlagPart {
                message_id: *id,
                is_read: *read,
            })),
            String::new(),
        ),
        AdminWrite::DelTempPassword { password } => (
            c2s::OutServerTempPasswordDelMessage::new(&mut one(c2s::OutServerTempPasswordDelPart { password: Cow::Borrowed(password) })),
            "Removed a temporary server password".to_string(),
        ),
    };
    Ok((cmd.send_with_result(con)?, what))
}
