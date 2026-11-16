//! The screen through the XDG ScreenCast portal (the desktop asks what to
//! share) and PipeWire.

use std::io::Cursor;
use std::os::fd::OwnedFd;
use std::sync::Arc;
use std::thread::JoinHandle;

use anyhow::{Context, Result, anyhow};
use ashpd::desktop::screencast::{CursorMode, Screencast, SelectSourcesOptions, SourceType};
use ashpd::desktop::{PersistMode, Session};
use pipewire as pw;
use pw::spa::param::ParamType;
use pw::spa::param::format::{FormatProperties, MediaSubtype, MediaType};
use pw::spa::param::video::{VideoFormat, VideoInfoRaw};
use pw::spa::pod::{self, Pod, serialize::PodSerializer};
use pw::spa::utils::{Direction, Fraction, Rectangle, SpaTypes};
use pw::stream::{StreamFlags, StreamState};

use super::{FPS, Latest, Order, Raw};

pub struct Capture {
    pub latest: Arc<Latest>,
    quit: pw::channel::Sender<()>,
    thread: Option<JoinHandle<()>>,
    portal: (Screencast, Session<Screencast>),
}

impl Capture {
    /// Ask the desktop what to share and start receiving it.
    pub fn open() -> Result<Self> {
        let (portal, node, fd) = futures::executor::block_on(ask())?;
        let latest = Arc::new(Latest::default());
        let (quit, quit_rx) = pw::channel::channel();
        let l = latest.clone();
        let thread = std::thread::Builder::new()
            .name("screen-capture".into())
            .spawn(move || {
                if let Err(e) = receive(node, fd, &l, quit_rx) {
                    tracing::warn!("screen capture: {e:#}");
                }
                l.end();
            })?;
        Ok(Self { latest, quit, thread: Some(thread), portal })
    }

    pub fn close(mut self) {
        let _ = self.quit.send(());
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        let _ = futures::executor::block_on(self.portal.1.close());
    }
}

async fn ask() -> Result<((Screencast, Session<Screencast>), u32, OwnedFd)> {
    let proxy = Screencast::new().await.context("screen sharing isn't available on this desktop")?;
    let session = proxy.create_session(Default::default()).await?;
    let cursor = match proxy.available_cursor_modes().await {
        Ok(modes) if modes.contains(CursorMode::Embedded) => CursorMode::Embedded,
        _ => CursorMode::Hidden,
    };
    proxy
        .select_sources(
            &session,
            SelectSourcesOptions::default()
                .set_cursor_mode(cursor)
                .set_sources(SourceType::Monitor | SourceType::Window)
                .set_multiple(false)
                .set_persist_mode(PersistMode::DoNot),
        )
        .await?;
    let streams = proxy
        .start(&session, None, Default::default())
        .await?
        .response()
        .map_err(|e| match e {
            ashpd::Error::Response(ashpd::desktop::ResponseError::Cancelled) => anyhow!("Screen sharing cancelled"),
            e => anyhow!(e),
        })?;
    let node = streams.streams().first().context("nothing was picked to share")?.pipe_wire_node_id();
    let fd = proxy.open_pipe_wire_remote(&session, Default::default()).await?;
    Ok(((proxy, session), node, fd))
}

fn receive(node: u32, fd: OwnedFd, latest: &Arc<Latest>, quit: pw::channel::Receiver<()>) -> Result<()> {
    pw::init();
    let main_loop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&main_loop, None)?;
    let core = context.connect_fd_rc(fd, None)?;
    let stream = pw::stream::StreamRc::new(
        core,
        "Wisp screen share",
        pw::properties::properties! {
            *pw::keys::MEDIA_TYPE => "Video",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Screen",
        },
    )?;

    let l = latest.clone();
    let ml = main_loop.clone();
    let ml2 = main_loop.clone();
    let _listener = stream
        .add_local_listener_with_user_data(VideoInfoRaw::default())
        .state_changed(move |_, _, _, new| {
            if matches!(new, StreamState::Error(_) | StreamState::Unconnected) {
                ml.quit();
            }
        })
        .param_changed(|_, format, id, param| {
            let Some(param) = param else { return };
            if id != ParamType::Format.as_raw() {
                return;
            }
            if let Ok((MediaType::Video, MediaSubtype::Raw)) = pw::spa::param::format_utils::parse_format(param) {
                let _ = format.parse(param);
            }
        })
        .process(move |stream, format| {
            let Some(mut buffer) = stream.dequeue_buffer() else { return };
            let order = match format.format() {
                VideoFormat::BGRx | VideoFormat::BGRA => Order::Bgra,
                VideoFormat::RGBx | VideoFormat::RGBA => Order::Rgba,
                _ => return,
            };
            let (width, height) = (format.size().width as usize, format.size().height as usize);
            let Some(data) = buffer.datas_mut().first_mut() else { return };
            let chunk = data.chunk();
            let (offset, size) = (chunk.offset() as usize, chunk.size() as usize);
            let stride = match chunk.stride() {
                s if s > 0 => s as usize,
                _ => width * 4,
            };
            let Some(bytes) = data.data() else { return };
            let need = stride * height.saturating_sub(1) + width * 4;
            if width == 0 || size < need || bytes.len() < offset + need {
                return;
            }
            l.put(Raw { width, height, stride, order, data: bytes[offset..offset + need].to_vec() });
        })
        .register()?;

    let format = pod::object!(
        SpaTypes::ObjectParamFormat,
        ParamType::EnumFormat,
        pod::property!(FormatProperties::MediaType, Id, MediaType::Video),
        pod::property!(FormatProperties::MediaSubtype, Id, MediaSubtype::Raw),
        pod::property!(
            FormatProperties::VideoFormat,
            Choice,
            Enum,
            Id,
            VideoFormat::BGRx,
            VideoFormat::BGRx,
            VideoFormat::BGRA,
            VideoFormat::RGBx,
            VideoFormat::RGBA,
        ),
        pod::property!(
            FormatProperties::VideoSize,
            Choice,
            Range,
            Rectangle,
            Rectangle { width: 1920, height: 1080 },
            Rectangle { width: 1, height: 1 },
            Rectangle { width: 8192, height: 8192 }
        ),
        pod::property!(
            FormatProperties::VideoFramerate,
            Choice,
            Range,
            Fraction,
            Fraction { num: FPS, denom: 1 },
            Fraction { num: 0, denom: 1 },
            Fraction { num: 1000, denom: 1 }
        ),
        pod::property!(
            FormatProperties::VideoMaxFramerate,
            Choice,
            Range,
            Fraction,
            Fraction { num: FPS, denom: 1 },
            Fraction { num: 0, denom: 1 },
            Fraction { num: FPS, denom: 1 }
        ),
    );
    let bytes = PodSerializer::serialize(Cursor::new(Vec::new()), &pod::Value::Object(format))
        .map_err(|e| anyhow!("{e:?}"))?
        .0
        .into_inner();
    let mut params = [Pod::from_bytes(&bytes).context("format pod")?];
    stream.connect(Direction::Input, Some(node), StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS, &mut params)?;

    let _quit = quit.attach(main_loop.loop_(), move |()| ml2.quit());
    main_loop.run();
    let _ = stream.disconnect();
    Ok(())
}
