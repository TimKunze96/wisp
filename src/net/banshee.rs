//! A session with a Banshee server (banshee://host:port#fingerprint): channels,
//! channel chat and voice. Answers the same `SessionCmd`s as a TeamSpeak
//! session; what Banshee doesn't have yet is left out.

use std::collections::HashMap;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use banshee_client::{Client, ServerMsg, User, UserId};
use tokio::sync::mpsc;
use tsclientlib::{ChannelType, ClientId, Codec, MaxClients};
use tsproto_packets::packets::{AudioData as AudioPacket, CodecType, Direction, InAudioBuf, OutAudio};

use super::chat::{self, ChatKind, ChatLine, Conversation};
use super::session::{TALK_HOLD, placeholder};
use super::*;
use crate::ConnectionId;
use crate::config::Bookmark;
use crate::notify::EventKind;

pub const SCHEME: &str = "banshee://";

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

struct Session {
    con_id: ConId,
    base: ServerSnap,
    back: mpsc::UnboundedSender<SessionMsg>,
    audio: Option<AudioData>,
    settings: Box<Settings>,
    you: UserId,
    channels: Vec<banshee_client::Channel>,
    users: HashMap<UserId, User>,
    last_voice: HashMap<UserId, Instant>,
    last_own_audio: Option<Instant>,
    seq: u16,
    history: chat::History,
    chat: Conversation,
    input_muted: bool,
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
    let mut client = Client::connect(addr, name, fingerprint.as_deref()).await?;
    let voice = client.voice();

    let history = chat::History::new(&bookmark.address);
    let lines = history.load(&ChatKind::Channel);
    let mut s = Session {
        con_id,
        base: placeholder(con_id, bookmark),
        back,
        audio,
        settings,
        you: client.you,
        channels: client.channels.clone(),
        users: client.users.iter().map(|u| (u.id, u.clone())).collect(),
        last_voice: HashMap::new(),
        last_own_audio: None,
        seq: 0,
        history,
        chat: Conversation { kind: ChatKind::Channel, title: "Channel".into(), client: None, lines, received: 0 },
        input_muted,
    };
    s.base.status = Status::Connected;
    s.base.server_name = host.clone();
    s.base.platform = "Banshee".into();
    s.publish();

    let mut tick = tokio::time::interval(std::time::Duration::from_millis(100));
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
                None => bail!("Disconnected by the server"),
            },
            v = voice.recv() => match v {
                Ok(v) => s.play(v),
                Err(_) => bail!("Disconnected by the server"),
            },
            _ = tick.tick() => {
                let before = s.last_voice.len();
                s.last_voice.retain(|_, t| t.elapsed() < TALK_HOLD);
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
    fn command(&mut self, client: &Client, cmd: SessionCmd) {
        match cmd {
            SessionCmd::Join(channel, _) => {
                if let Ok(id) = u32::try_from(channel) {
                    client.join(id);
                }
            }
            SessionCmd::Audio(opus, whisper) => {
                // whisper lists are TeamSpeak's; nothing to whisper to here
                if whisper.is_some() || self.input_muted {
                    return;
                }
                self.seq = self.seq.wrapping_add(1);
                if client.send_voice(self.seq, &opus).is_ok() {
                    self.last_own_audio = Some(Instant::now());
                }
            }
            SessionCmd::SelfState { input_muted, .. } => self.input_muted = input_muted,
            SessionCmd::SendText(ChatKind::Channel, text) => client.chat(&text),
            SessionCmd::SendText(..) => self.activity("Only channel chat works on Banshee servers so far"),
            SessionCmd::Settings(settings) => self.settings = settings,
            // the rest is TeamSpeak's (admin, files, groups...)
            _ => {}
        }
    }

    fn server_msg(&mut self, msg: ServerMsg) {
        match msg {
            ServerMsg::UserJoined(u) => {
                if self.users.get(&self.you).is_some_and(|me| me.channel == u.channel) {
                    self.activity(&format!("{} joined", u.name));
                }
                self.users.insert(u.id, u);
            }
            ServerMsg::UserLeft { id } => {
                if let Some(u) = self.users.remove(&id) {
                    self.activity(&format!("{} left", u.name));
                }
                self.last_voice.remove(&id);
            }
            ServerMsg::UserMoved { id, channel } => {
                if let Some(u) = self.users.get_mut(&id) {
                    u.channel = channel;
                }
            }
            ServerMsg::Chat { from, text, .. } => {
                let me = from == self.you;
                let name = self.users.get(&from).map(|u| u.name.clone()).unwrap_or_default();
                let my_name = self.users.get(&self.you).map(|u| u.name.to_lowercase()).unwrap_or_default();
                let mention = !me && !my_name.is_empty() && text.to_lowercase().contains(&my_name);
                if !me {
                    let plain = session::strip_bbcode(&text);
                    match mention {
                        true => self.event(EventKind::Mention, format!("{name} mentioned you"), plain),
                        false => self.event(EventKind::ChannelMessage, name.clone(), plain),
                    }
                }
                self.add_line(chat::now_line(name, from.to_string(), me, text, mention));
            }
            ServerMsg::Error { message } => self.activity(&message),
            ServerMsg::Welcome { .. } => {}
        }
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
        self.last_voice.insert(v.from, Instant::now());
    }

    fn add_line(&mut self, line: ChatLine) {
        self.history.append(&ChatKind::Channel, &line);
        self.chat.lines.push(line);
        self.chat.received += 1;
        let extra = self.chat.lines.len().saturating_sub(chat::KEEP);
        self.chat.lines.drain(..extra);
    }

    fn activity(&self, text: &str) {
        let _ = self.back.send(SessionMsg::Activity(self.con_id, text.to_string(), false));
    }

    fn event(&self, kind: EventKind, title: String, body: String) {
        let _ = self.back.send(SessionMsg::Event(self.con_id, crate::notify::Event { kind, title, body }));
    }

    /// Send the current state to the manager.
    fn publish(&self) {
        let mut snap = self.base.clone();
        let own_channel = self.users.get(&self.you).map(|u| u64::from(u.channel));
        let own_talking = self.last_own_audio.is_some_and(|t| t.elapsed() < TALK_HOLD);
        snap.channels = self
            .channels
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let mut clients: Vec<ClientRow> = self
                    .users
                    .values()
                    .filter(|u| u.channel == c.id)
                    .map(|u| self.client_row(u, own_talking))
                    .collect();
                clients.sort_by_key(|c| c.name.to_lowercase());
                channel_row(c, i == 0, clients)
            })
            .collect();
        snap.own_channel = own_channel;
        snap.users = self.users.len();
        snap.chats = vec![self.chat.clone()];
        let _ = self.back.send(SessionMsg::Snap(snap));
    }

    fn client_row(&self, u: &User, own_talking: bool) -> ClientRow {
        let me = u.id == self.you;
        let uid = u.id.to_string();
        ClientRow {
            id: u16::try_from(u.id).unwrap_or(u16::MAX),
            name: u.name.clone(),
            talking: if me { own_talking } else { self.last_voice.contains_key(&u.id) },
            input_muted: me && self.input_muted,
            output_muted: false,
            away: None,
            me,
            commander: false,
            priority: false,
            recording: false,
            country: String::new(),
            groups: Vec::new(),
            group_ids: Vec::new(),
            channel_group: 0,
            talk_power_granted: false,
            db_id: 0,
            talk_request: None,
            avatar: None,
            friend: false,
            blocked: false,
            whispering: false,
            icon: None,
            volume: 1.0,
            locally_muted: false,
            uid,
        }
    }
}

fn channel_row(c: &banshee_client::Channel, is_default: bool, clients: Vec<ClientRow>) -> ChannelRow {
    ChannelRow {
        id: u64::from(c.id),
        parent: 0,
        name: c.name.clone(),
        phonetic: String::new(),
        topic: String::new(),
        description: Some(String::new()),
        depth: 0,
        has_password: false,
        is_default,
        kind: ChannelType::Permanent,
        codec: Codec::OpusVoice,
        quality: 10,
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
