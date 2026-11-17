//! The sound of everything that plays, except Wisp itself (so viewers
//! don't hear the voice chat twice), or of one app: a PipeWire capture node
//! that we link to the apps' outputs ourselves, as Opus packets on the
//! audio track.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::io::Cursor;
use std::rc::Rc;
use std::thread::JoinHandle;

use anyhow::{Context, Result, anyhow};
use pipewire as pw;
use pw::spa::param::audio::{AudioFormat, AudioInfoRaw};
use pw::spa::pod::{self, Pod, serialize::PodSerializer};
use pw::spa::utils::{Direction, SpaTypes};
use pw::stream::StreamFlags;
use pw::types::ObjectType;

use super::SoundEncoder;

pub struct Sound {
    quit: pw::channel::Sender<()>,
    thread: Option<JoinHandle<()>>,
}

impl Sound {
    /// Start capturing (everything, or only the app named `app`); `packets`
    /// gets each 20 ms Opus packet.
    pub fn start(packets: impl FnMut(Vec<u8>) + Send + 'static, app: Option<String>) -> Result<Self> {
        let (quit, quit_rx) = pw::channel::channel();
        let (ready_tx, ready) = std::sync::mpsc::channel();
        let thread = std::thread::Builder::new().name("screen-sound".into()).spawn(move || {
            if let Err(e) = run(packets, app, quit_rx, &ready_tx) {
                let _ = ready_tx.send(Err(e));
            }
        })?;
        match ready.recv_timeout(std::time::Duration::from_secs(3)) {
            Ok(Ok(())) => Ok(Self { quit, thread: Some(thread) }),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(anyhow!("PipeWire didn't answer")),
        }
    }
}

impl Drop for Sound {
    fn drop(&mut self) {
        let _ = self.quit.send(());
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// What the registry told us, to work out the links.
#[derive(Default)]
struct Graph {
    /// App playback nodes (not ours).
    apps: HashSet<u32>,
    /// Our own PipeWire clients (the process id is on them, not on nodes).
    own_clients: HashSet<u32>,
    /// Output ports of nodes: (port, node, channel).
    outputs: Vec<(u32, u32, String)>,
    /// Our capture node's input ports by channel.
    inputs: HashMap<String, u32>,
    linked: HashSet<(u32, u32)>,
    links: Vec<pw::link::Link>,
}

fn run(
    mut packets: impl FnMut(Vec<u8>) + 'static,
    app: Option<String>,
    quit: pw::channel::Receiver<()>,
    ready: &std::sync::mpsc::Sender<Result<()>>,
) -> Result<()> {
    pw::init();
    let main_loop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&main_loop, None)?;
    let core = context.connect_rc(None).context("connecting to PipeWire")?;
    let registry = core.get_registry_rc()?;
    let pid = std::process::id().to_string();

    let stream = pw::stream::StreamRc::new(
        core.clone(),
        "Wisp screen sound",
        pw::properties::properties! {
            *pw::keys::MEDIA_TYPE => "Audio",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Music",
            *pw::keys::NODE_NAME => "wisp-screen-sound",
        },
    )?;
    let mut encoder = SoundEncoder::new()?;
    let mut pending: Vec<f32> = Vec::with_capacity(super::SOUND_FRAME * 4);
    let _listener = stream
        .add_local_listener()
        .process(move |stream, _: &mut ()| {
            let Some(mut buffer) = stream.dequeue_buffer() else { return };
            let Some(data) = buffer.datas_mut().first_mut() else { return };
            let size = data.chunk().size() as usize;
            let offset = data.chunk().offset() as usize;
            let Some(bytes) = data.data() else { return };
            let Some(bytes) = bytes.get(offset..offset + size) else { return };
            pending.extend(bytes.chunks_exact(4).map(|b| f32::from_le_bytes(b.try_into().unwrap())));
            while pending.len() >= super::SOUND_FRAME * 2 {
                let frame: Vec<f32> = pending.drain(..super::SOUND_FRAME * 2).collect();
                if let Some(p) = encoder.encode(&frame) {
                    packets(p);
                }
            }
        })
        .register()?;

    let mut info = AudioInfoRaw::new();
    info.set_format(AudioFormat::F32LE);
    info.set_rate(48000);
    info.set_channels(2);
    let mut position = [0u32; pw::spa::sys::SPA_AUDIO_MAX_CHANNELS as usize];
    position[0] = pw::spa::sys::SPA_AUDIO_CHANNEL_FL;
    position[1] = pw::spa::sys::SPA_AUDIO_CHANNEL_FR;
    info.set_position(position);
    let format = pod::Object {
        type_: SpaTypes::ObjectParamFormat.as_raw(),
        id: pw::spa::param::ParamType::EnumFormat.as_raw(),
        properties: info.into(),
    };
    let bytes = PodSerializer::serialize(Cursor::new(Vec::new()), &pod::Value::Object(format))
        .map_err(|e| anyhow!("{e:?}"))?
        .0
        .into_inner();
    let mut params = [Pod::from_bytes(&bytes).context("format pod")?];
    // no autoconnect: we pick what it hears
    stream.connect(Direction::Input, None, StreamFlags::MAP_BUFFERS, &mut params)?;

    let graph = Rc::new(RefCell::new(Graph::default()));
    let own_node = Rc::new(RefCell::new(None::<u32>));
    let (g, c, own, s) = (graph.clone(), core.clone(), own_node.clone(), stream.clone());
    let _registry_listener = registry
        .add_listener_local()
        .global(move |obj| {
            let Some(props) = obj.props else { return };
            let mut graph = g.borrow_mut();
            match obj.type_ {
                ObjectType::Client => {
                    // the registry only shows the socket's pid for clients
                    let client_pid = props.get("pipewire.sec.pid").or(props.get("application.process.id"));
                    if client_pid == Some(pid.as_str()) {
                        graph.own_clients.insert(obj.id);
                    }
                    return;
                }
                ObjectType::Node => {
                    let client = props.get("client.id").and_then(|c| c.parse::<u32>().ok());
                    let ours = props.get("application.process.id") == Some(pid.as_str())
                        || client.is_some_and(|c| graph.own_clients.contains(&c));
                    let wanted = app.as_deref().is_none_or(|a| props.get("application.name") == Some(a));
                    if props.get("media.class") == Some("Stream/Output/Audio") && !ours && wanted {
                        graph.apps.insert(obj.id);
                    }
                }
                ObjectType::Port => {
                    let (Some(node), Some(dir)) =
                        (props.get("node.id").and_then(|n| n.parse::<u32>().ok()), props.get("port.direction"))
                    else {
                        return;
                    };
                    let channel = props.get("audio.channel").unwrap_or("MONO").to_string();
                    if own.borrow().is_none() && s.node_id() != u32::MAX {
                        *own.borrow_mut() = Some(s.node_id());
                    }
                    if dir == "in" && Some(node) == *own.borrow() {
                        graph.inputs.insert(channel, obj.id);
                    } else if dir == "out" {
                        graph.outputs.push((obj.id, node, channel));
                    }
                }
                _ => return,
            }
            link_all(&mut graph, &c, *own.borrow());
        })
        .global_remove({
            let g = graph.clone();
            move |id| {
                let mut graph = g.borrow_mut();
                graph.apps.remove(&id);
                graph.outputs.retain(|(port, node, _)| *port != id && *node != id);
                graph.linked.retain(|(a, b)| *a != id && *b != id);
            }
        })
        .register();

    let ml = main_loop.clone();
    let _quit = quit.attach(main_loop.loop_(), move |()| ml.quit());
    let _ = ready.send(Ok(()));
    main_loop.run();
    graph.borrow_mut().links.clear();
    let _ = stream.disconnect();
    Ok(())
}

/// The apps playing sound right now (by name, not Wisp): one look at the
/// PipeWire registry.
pub fn apps() -> Vec<super::SoundApp> {
    let (tx, rx) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new().name("sound-apps".into()).spawn(move || {
        let _ = tx.send(scan_apps().unwrap_or_default());
    });
    if spawned.is_err() {
        return Vec::new();
    }
    rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap_or_default()
}

fn scan_apps() -> Result<Vec<super::SoundApp>> {
    pw::init();
    let main_loop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&main_loop, None)?;
    let core = context.connect_rc(None).context("connecting to PipeWire")?;
    let registry = core.get_registry_rc()?;
    let pid = std::process::id().to_string();
    let found: Rc<RefCell<Vec<super::SoundApp>>> = Rc::default();
    let f = found.clone();
    let _registry_listener = registry
        .add_listener_local()
        .global(move |obj| {
            let Some(props) = obj.props else { return };
            if obj.type_ != ObjectType::Node || props.get("media.class") != Some("Stream/Output/Audio") {
                return;
            }
            if props.get("application.process.id") == Some(pid.as_str()) {
                return;
            }
            // apps going through the ALSA bridge only name themselves in the node
            let binary = props
                .get("application.process.binary")
                .or_else(|| props.get("node.name").and_then(|n| n.strip_prefix("alsa_playback.")))
                .unwrap_or_default();
            // other Wisp windows (the test clients, a second instance)
            if binary == "wisp" {
                return;
            }
            let Some(key) = props.get("application.name").filter(|n| !n.is_empty()) else { return };
            // apps going through the ALSA bridge are all "PipeWire ALSA [binary]"
            let name = if key.starts_with("PipeWire ALSA") && !binary.is_empty() { binary } else { key };
            let mut found = f.borrow_mut();
            if !found.iter().any(|a| a.key == key) {
                found.push(super::SoundApp { key: key.to_string(), name: name.to_string() });
            }
        })
        .register();
    // everything the registry has is in once the server answers a sync
    let pending = core.sync(0)?;
    let ml = main_loop.clone();
    let _core_listener = core
        .add_listener_local()
        .done(move |id, seq| {
            if id == pw::core::PW_ID_CORE && seq == pending {
                ml.quit();
            }
        })
        .register();
    main_loop.run();
    let mut apps = found.take();
    apps.sort_by_key(|a| a.name.to_lowercase());
    Ok(apps)
}

/// Link every app output port to our input of the same channel (mono to both).
fn link_all(graph: &mut Graph, core: &pw::core::CoreRc, own: Option<u32>) {
    let Some(own) = own else { return };
    let mut wanted = Vec::new();
    for (port, node, channel) in &graph.outputs {
        if !graph.apps.contains(node) {
            continue;
        }
        let targets: Vec<u32> = match channel.as_str() {
            "FL" | "FR" => graph.inputs.get(channel).copied().into_iter().collect(),
            "MONO" => graph.inputs.values().copied().collect(),
            _ => Vec::new(),
        };
        for input in targets {
            if !graph.linked.contains(&(*port, input)) {
                wanted.push((*port, *node, input));
            }
        }
    }
    for (port, node, input) in wanted {
        let props = pw::properties::properties! {
            "link.output.port" => port.to_string(),
            "link.input.port" => input.to_string(),
            "link.output.node" => node.to_string(),
            "link.input.node" => own.to_string(),
        };
        match core.create_object::<pw::link::Link>("link-factory", &props) {
            Ok(link) => {
                graph.linked.insert((port, input));
                graph.links.push(link);
            }
            Err(e) => tracing::debug!("linking app sound: {e}"),
        }
    }
}
