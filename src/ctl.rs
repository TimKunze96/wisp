//! Control socket ($XDG_RUNTIME_DIR/wisp-ctl.sock) and the CLI that talks to it.
//! One request line in, one JSON line out.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::net::UnixListener;
use tokio::sync::{mpsc, oneshot};

use crate::config::Settings;
use crate::net::{AdminAction, ChannelProps, Cmd, Snapshot, Zone, channel_drop, runtime_dir};

pub fn socket() -> PathBuf {
    runtime_dir().join("wisp-ctl.sock")
}

/// Requests for the UI thread (show the window, quit).
#[derive(Debug)]
pub enum UiRequest {
    Show,
    Settings(String),
    /// Admin window of the active server, on a tab.
    Admin(String),
    /// A ts3server:// link to connect to.
    Link(String),
    Quit,
}

pub static UI: std::sync::OnceLock<futures::channel::mpsc::UnboundedSender<UiRequest>> =
    std::sync::OnceLock::new();

pub fn ui(req: UiRequest) {
    if let Some(tx) = UI.get() {
        let _ = tx.unbounded_send(req);
    }
}

/// Start the socket server on the current `LocalSet`.
pub fn serve(cmds: mpsc::UnboundedSender<Cmd>) {
    let _ = std::fs::remove_file(socket());
    let Ok(listener) = UnixListener::bind(socket()) else { return };
    tokio::task::spawn_local(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let cmds = cmds.clone();
            tokio::task::spawn_local(async move {
                let (read, mut write) = stream.into_split();
                let mut lines = tokio::io::BufReader::new(read).lines();
                if let Ok(Some(line)) = lines.next_line().await {
                    let reply = handle(&line, &cmds).await;
                    let _ = write.write_all(format!("{reply}\n").as_bytes()).await;
                }
            });
        }
    });
}

async fn status(cmds: &mpsc::UnboundedSender<Cmd>) -> Option<Snapshot> {
    let (tx, rx) = oneshot::channel();
    cmds.send(Cmd::Status(tx)).ok()?;
    rx.await.ok()
}

fn on_off(arg: &str, current: bool) -> bool {
    match arg {
        "on" | "true" | "1" => true,
        "off" | "false" | "0" => false,
        _ => !current,
    }
}

async fn handle(line: &str, cmds: &mpsc::UnboundedSender<Cmd>) -> Value {
    let line = line.trim();
    let (cmd, arg) = line.split_once(' ').map_or((line, ""), |(a, b)| (a, b.trim()));
    let Some(snap) = status(cmds).await else { return json!({"ok": false, "error": "not running"}) };
    let ok = json!({"ok": true});
    match cmd {
        "status" => json!({"ok": true, "status": snap}),
        "connect" => {
            let settings = Settings::load();
            match settings.find_bookmark(arg).or_else(|| (arg.is_empty()).then(|| settings.bookmarks.first()).flatten()) {
                Some(b) => {
                    let _ = cmds.send(Cmd::Connect(b.clone()));
                    ok
                }
                None => json!({"ok": false, "error": format!("no bookmark matching {arg:?}")}),
            }
        }
        "disconnect" => {
            let targets: Vec<_> = snap
                .servers
                .iter()
                .filter(|s| match arg {
                    "" => Some(s.con) == snap.active,
                    "all" => true,
                    q => s.label.eq_ignore_ascii_case(q) || s.server_name.eq_ignore_ascii_case(q),
                })
                .map(|s| s.con)
                .collect();
            for con in &targets {
                let _ = cmds.send(Cmd::Disconnect(*con));
            }
            json!({"ok": !targets.is_empty()})
        }
        "mute" => {
            let _ = cmds.send(Cmd::SelfMute(on_off(arg, snap.self_muted)));
            ok
        }
        "deafen" => {
            let _ = cmds.send(Cmd::Deafen(on_off(arg, snap.deafened)));
            ok
        }
        "ptt" => {
            let _ = cmds.send(match arg {
                "down" | "on" => Cmd::Ptt(true),
                "up" | "off" => Cmd::Ptt(false),
                _ => Cmd::TogglePtt,
            });
            ok
        }
        "join" => {
            let Some(server) = snap.active_server() else {
                return json!({"ok": false, "error": "not connected"});
            };
            let q = arg.to_lowercase();
            let found = server
                .channels
                .iter()
                .find(|c| c.name.to_lowercase() == q)
                .or_else(|| server.channels.iter().find(|c| c.name.to_lowercase().contains(&q)));
            match found {
                Some(c) => {
                    let _ = cmds.send(Cmd::Join { con: server.con, channel: c.id, password: None });
                    json!({"ok": true, "channel": c.name})
                }
                None => json!({"ok": false, "error": format!("no channel matching {arg:?}")}),
            }
        }
        "channel" => channel(arg, &snap, cmds),
        "file" => file(arg, &snap, cmds),
        "query" => query(arg, &snap, cmds).await,
        "offline" => {
            let Some(server) = snap.active_server() else { return json!({"ok": false, "error": "not connected"}) };
            let words: Vec<String> = serde_json::from_str(arg).unwrap_or_default();
            let (Some(who), true) = (words.first(), words.len() > 1) else {
                return json!({"ok": false, "error": "offline <name or unique id> <text>"});
            };
            let text = &words[1..].join(" ");
            // an online name, else take it as a unique id
            let uid = server
                .channels
                .iter()
                .flat_map(|c| &c.clients)
                .find(|c| c.name.eq_ignore_ascii_case(who))
                .map(|c| c.uid.clone())
                .unwrap_or_else(|| who.to_string());
            let subject: String = crate::net::strip_bbcode(text).chars().take(40).collect();
            let write = crate::net::admin::AdminWrite::SendOffline { uid, subject, message: text.to_string() };
            let _ = cmds.send(Cmd::Write { con: server.con, write });
            ok
        }
        "record" => {
            let _ = cmds.send(Cmd::Record(match arg {
                "on" | "start" => Some(true),
                "off" | "stop" => Some(false),
                _ => None,
            }));
            ok
        }
        "away" => {
            let away = match arg {
                "off" | "back" => None,
                msg => Some(msg.to_string()),
            };
            let _ = cmds.send(Cmd::Away(away));
            ok
        }
        "say" => match snap.active {
            Some(con) if !arg.is_empty() => {
                let _ = cmds.send(Cmd::SendText { con, to: crate::net::ChatKind::Channel, text: arg.to_string() });
                ok
            }
            _ => json!({"ok": false, "error": "not connected or empty message"}),
        },
        "show" => {
            ui(UiRequest::Show);
            ok
        }
        "settings" => {
            ui(UiRequest::Settings(arg.to_string()));
            ok
        }
        "link" => {
            ui(UiRequest::Link(arg.to_string()));
            ok
        }
        "admin" => {
            if snap.active.is_none() {
                return json!({"ok": false, "error": "not connected"});
            }
            ui(UiRequest::Admin(arg.to_string()));
            ok
        }
        "quit" => {
            ui(UiRequest::Quit);
            ok
        }
        _ => json!({"ok": false, "error": format!("unknown command {cmd:?}")}),
    }
}

/// `query <what> [name]`: a server list as JSON, for scripts (and testing).
async fn query(arg: &str, snap: &Snapshot, cmds: &mpsc::UnboundedSender<Cmd>) -> Value {
    use crate::net::admin::{PermTarget, Query};
    let Some(server) = snap.active_server() else { return json!({"ok": false, "error": "not connected"}) };
    let (what, rest) = arg.split_once(' ').map_or((arg, ""), |(a, b)| (a, b.trim()));
    let group = |q: &str| server.groups.iter().find(|g| g.badge.name.eq_ignore_ascii_case(q)).map(|g| g.id);
    let q = match what {
        "permissions" => Query::PermissionList,
        "bans" => Query::Bans,
        "keys" => Query::Tokens,
        "complaints" => Query::Complaints,
        "log" => Query::Log { lines: rest.parse().unwrap_or(50) },
        "passwords" => Query::TempPasswords,
        "inbox" => Query::Inbox,
        "connection" => Query::ServerConnection,
        "overview" => {
            let who = server.channels.iter().flat_map(|c| c.clients.iter().map(move |cl| (c.id, cl))).find(|(_, cl)| cl.name.eq_ignore_ascii_case(rest));
            match who {
                Some((channel, cl)) => Query::Overview { channel, db_id: cl.db_id },
                None => return json!({"ok": false, "error": format!("nobody called {rest:?} is online")}),
            }
        }
        "icons" => Query::Files { channel: 0, path: "/icons".into(), password: String::new() },
        "group-perms" | "members" => match group(rest) {
            Some(g) if what == "members" => Query::ServerGroupMembers(g),
            Some(g) => Query::Perms(PermTarget::ServerGroup(g)),
            None => return json!({"ok": false, "error": format!("no server group {rest:?}")}),
        },
        "files" => match server.channels.iter().find(|c| c.name.eq_ignore_ascii_case(rest)) {
            Some(c) => Query::Files { channel: c.id, path: "/".into(), password: String::new() },
            None => return json!({"ok": false, "error": format!("no channel {rest:?}")}),
        },
        "channel-perms" => match server.channels.iter().find(|c| c.name.eq_ignore_ascii_case(rest)) {
            Some(c) => Query::Perms(PermTarget::Channel(c.id)),
            None => return json!({"ok": false, "error": format!("no channel {rest:?}")}),
        },
        _ => {
            return json!({"ok": false, "error": "query permissions|bans|keys|complaints|log [n]|passwords|group-perms <group>|members <group>|channel-perms <channel>"});
        }
    };
    let (tx, rx) = oneshot::channel();
    let _ = cmds.send(Cmd::Query { con: server.con, query: q, reply: tx });
    match tokio::time::timeout(std::time::Duration::from_secs(10), rx).await {
        Ok(Ok(Ok(answer))) => json!({"ok": true, "answer": answer}),
        Ok(Ok(Err(e))) => json!({"ok": false, "error": e}),
        _ => json!({"ok": false, "error": "no answer"}),
    }
}

/// `file <json array of words>`: upload / download / rm / mkdir in a channel.
fn file(arg: &str, snap: &Snapshot, cmds: &mpsc::UnboundedSender<Cmd>) -> Value {
    use crate::net::files::{FileCmd, download_path};
    let words: Vec<String> = serde_json::from_str(arg).unwrap_or_default();
    let w = |i: usize| words.get(i).map(String::as_str).unwrap_or("");
    let Some(server) = snap.active_server() else { return json!({"ok": false, "error": "not connected"}) };
    let Some(ch) = server.channels.iter().find(|c| c.name.eq_ignore_ascii_case(w(1))) else {
        return json!({"ok": false, "error": format!("no channel {:?}", w(1))});
    };
    let password = Settings::load().channel_passwords.get(&crate::config::channel_key(&server.bookmark, ch.id)).cloned().unwrap_or_default();
    let remote = |p: &str| if p.starts_with('/') { p.to_string() } else { format!("/{p}") };
    let cmd = match (w(0), w(2)) {
        ("upload", local) if !local.is_empty() => {
            let from = std::path::PathBuf::from(local);
            let Some(name) = from.file_name().map(|n| n.to_string_lossy().to_string()) else {
                return json!({"ok": false, "error": "not a file"});
            };
            let dir = if w(3).is_empty() { "/".to_string() } else { remote(w(3)) };
            let from = std::fs::canonicalize(&from).unwrap_or(from);
            FileCmd::Upload { channel: ch.id, password, path: format!("{}/{name}", dir.trim_end_matches('/')), from }
        }
        ("download", path) if !path.is_empty() => {
            let to = download_path(path.rsplit('/').next().unwrap_or(path));
            FileCmd::Download { channel: ch.id, password, path: remote(path), to }
        }
        ("rm", path) if !path.is_empty() => FileCmd::Delete { channel: ch.id, password, path: remote(path) },
        ("mkdir", path) if !path.is_empty() => FileCmd::Mkdir { channel: ch.id, password, path: remote(path) },
        _ => return json!({"ok": false, "error": "file upload <channel> <local> [dir] | download <channel> <path> | rm <channel> <path> | mkdir <channel> <path>"}),
    };
    let _ = cmds.send(Cmd::Files { con: server.con, cmd });
    json!({"ok": true})
}

/// `channel <json array of words>` from the CLI.
fn channel(arg: &str, snap: &Snapshot, cmds: &mpsc::UnboundedSender<Cmd>) -> Value {
    let words: Vec<String> = serde_json::from_str(arg).unwrap_or_default();
    let Some(server) = snap.active_server() else { return json!({"ok": false, "error": "not connected"}) };
    let find = |q: &str| {
        let q = q.to_lowercase();
        server
            .channels
            .iter()
            .find(|c| c.name.to_lowercase() == q)
            .or_else(|| server.channels.iter().find(|c| c.name.to_lowercase().contains(&q)))
    };
    let err = |e: String| json!({"ok": false, "error": e});
    let w = |i: usize| words.get(i).map(String::as_str).unwrap_or("");
    let action = match w(0) {
        "create" if !w(1).is_empty() => {
            let parent = match w(2) {
                "" => 0,
                p => match find(p) {
                    Some(c) => c.id,
                    None => return err(format!("no channel matching {p:?}")),
                },
            };
            AdminAction::CreateChannel { parent, props: ChannelProps { name: Some(w(1).into()), ..Default::default() } }
        }
        "rename" | "topic" | "delete" | "move" => {
            let Some(c) = find(w(1)) else { return err(format!("no channel matching {:?}", w(1))) };
            match w(0) {
                "rename" if !w(2).is_empty() => AdminAction::EditChannel {
                    channel: c.id,
                    props: ChannelProps { name: Some(words[2..].join(" ")), ..Default::default() },
                },
                "topic" => AdminAction::EditChannel {
                    channel: c.id,
                    props: ChannelProps { topic: Some(words.get(2..).unwrap_or_default().join(" ")), ..Default::default() },
                },
                "delete" => AdminAction::DeleteChannel { channel: c.id },
                "move" => {
                    let zone = match w(2) {
                        "into" => Zone::Into,
                        "above" => Zone::Above,
                        "below" => Zone::Below,
                        _ => return err("move <channel> into|above|below <target>".into()),
                    };
                    let target = match w(3) {
                        "root" | "top" if zone == Zone::Into => 0,
                        t => match find(t) {
                            Some(t) => t.id,
                            None => return err(format!("no channel matching {t:?}")),
                        },
                    };
                    match channel_drop(&server.channels, c.id, target, zone) {
                        Some((parent, after)) => AdminAction::MoveChannel { channel: c.id, parent, after },
                        None => return err("that wouldn't change anything".into()),
                    }
                }
                _ => return err("missing argument".into()),
            }
        }
        _ => return err("channel create|rename|topic|move|delete ...".into()),
    };
    let _ = cmds.send(Cmd::Admin { con: server.con, action });
    json!({"ok": true})
}

const USAGE: &str = "\
usage: wisp [command]
  (no command)              start the app (or show it if it's running)
  status [--json|--bar]     connection status; --bar prints {text, tooltip, class}
  connect [bookmark]        connect to a bookmark (label, address or id)
  disconnect [name|all]     disconnect the active server, one by name, or all
  mute [on|off]             toggle or set mic mute
  away [message] | away off set or clear away on every server
  record [on|off]           record what you hear and say to ~/Music/Wisp
  offline <name|uid> <text> leave an offline message
  query <list> [arg]        server lists as JSON: permissions, bans, keys,
                            complaints, log [n], passwords, inbox, icons, connection, overview <name>, members <group>,
                            group-perms <group>, channel-perms <channel>, files <channel>
  deafen [on|off]           toggle or set sound off
  ptt [down|up]             push-to-talk (toggle without argument)
  whisper <list> down|up    whisper to a whisper list while held
  join <channel>            join a channel on the active server
  say <text>                send a message to your channel
  channel create <name> [parent]
  channel rename <channel> <new name>
  channel topic <channel> [topic]
  channel move <channel> into|above|below <target|root>
  channel delete <channel>  channel admin on the active server
  file upload <channel> <local file> [folder]
  file download <channel> <path>   (to ~/Downloads)
  file rm | mkdir <channel> <path>
  settings [tab]            open settings (audio, servers, hotkeys, whisper, notifications, general)
  admin [tab]               server admin window (groups, channel-groups, bans, keys,
                            complaints, server, log, files <channel>)
  show | quit               show the window / quit the app";

fn request(line: &str) -> Option<Value> {
    let mut stream = UnixStream::connect(socket()).ok()?;
    stream.write_all(format!("{line}\n").as_bytes()).ok()?;
    let mut reply = String::new();
    BufReader::new(stream).read_line(&mut reply).ok()?;
    serde_json::from_str(&reply).ok()
}

pub fn is_running() -> bool {
    UnixStream::connect(socket()).is_ok()
}

/// Run a CLI command. Returns the process exit code.
pub fn cli(args: &[String]) -> i32 {
    let cmd = args[0].as_str();
    if matches!(cmd, "-h" | "--help" | "help") {
        println!("{USAGE}");
        return 0;
    }
    // push-to-talk goes over the datagram socket: fast and fire-and-forget
    if cmd == "ptt" && matches!(args.get(1).map(String::as_str), Some("down" | "up")) {
        crate::net::send_ptt(&args[1]);
        return 0;
    }
    if cmd == "whisper" {
        let (list, state) = match args.len() {
            3.. if matches!(args[args.len() - 1].as_str(), "down" | "up") => {
                (args[1..args.len() - 1].join(" "), args[args.len() - 1].clone())
            }
            _ => {
                eprintln!("usage: wisp whisper <list> down|up");
                return 2;
            }
        };
        if Settings::load().whisper_list(&list).is_none() {
            eprintln!("no whisper list {list:?}");
            return 1;
        }
        crate::net::send_ptt(&format!("whisper {list} {state}"));
        return 0;
    }
    let flag = args.get(1).map(String::as_str);
    let line = match cmd {
        "status" => "status".to_string(),
        // names can have spaces; keep the words apart
        "channel" | "file" | "offline" => format!("{cmd} {}", json!(args[1..])),
        _ => args.join(" "),
    };
    let Some(reply) = request(&line) else {
        if cmd == "status" && flag == Some("--bar") {
            println!("{}", json!({"text": "", "tooltip": "Wisp is not running", "class": "off"}));
            return 0;
        }
        eprintln!("Wisp is not running");
        return 1;
    };
    if cmd == "status" {
        let status = &reply["status"];
        match flag {
            Some("--json") => println!("{status}"),
            Some("--bar") => println!("{}", bar_line(status)),
            _ => print_status(status),
        }
        return 0;
    }
    if cmd == "query" && reply["ok"].as_bool() == Some(true) {
        println!("{}", reply["answer"]);
        return 0;
    }
    if reply["ok"].as_bool() == Some(true) {
        0
    } else {
        eprintln!("{}", reply["error"].as_str().unwrap_or("failed"));
        1
    }
}

fn own_channel(server: &Value) -> Option<String> {
    let own = server["own_channel"].as_u64()?;
    server["channels"].as_array()?.iter().find(|c| c["id"].as_u64() == Some(own))?["name"]
        .as_str()
        .map(str::to_string)
}

fn print_status(s: &Value) {
    let servers = s["servers"].as_array().cloned().unwrap_or_default();
    if servers.is_empty() {
        println!("not connected");
    }
    for server in &servers {
        let active = s["active"] == server["con"];
        let status = match &server["status"] {
            Value::String(x) => x.clone(),
            other => format!("{other}"),
        };
        let ping = server["ping_ms"].as_f64().map(|p| format!(" {p:.0} ms")).unwrap_or_default();
        println!(
            "{} {} [{}]{} in {}",
            if active { "*" } else { " " },
            server["server_name"].as_str().filter(|n| !n.is_empty()).or(server["label"].as_str()).unwrap_or("?"),
            status,
            ping,
            own_channel(server).unwrap_or_else(|| "-".into()),
        );
    }
    println!(
        "mic: {}{}{}",
        if s["self_muted"] == true { "muted" } else if s["transmitting"] == true { "talking" } else { "idle" },
        if s["deafened"] == true { ", sound off" } else { "" },
        if s["ptt"] == true { ", ptt held" } else { "" },
    );
}

/// One line for a status bar widget: {text, tooltip, class}.
fn bar_line(s: &Value) -> Value {
    let servers = s["servers"].as_array().cloned().unwrap_or_default();
    let active = servers.iter().find(|x| x["con"] == s["active"]);
    let class = if servers.is_empty() {
        "idle"
    } else if s["self_muted"] == true || s["deafened"] == true {
        "muted"
    } else if s["transmitting"] == true {
        "talking"
    } else {
        "connected"
    };
    let text = active.and_then(own_channel).unwrap_or_default();
    let tooltip = servers
        .iter()
        .map(|x| {
            format!(
                "{}: {}",
                x["server_name"].as_str().filter(|n| !n.is_empty()).or(x["label"].as_str()).unwrap_or("?"),
                own_channel(x).unwrap_or_else(|| "-".into())
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    json!({"text": text, "tooltip": tooltip, "class": class})
}
