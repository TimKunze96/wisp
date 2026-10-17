//! StatusNotifierItem tray icon (ksni). The icon is the logo, drawn here so it
//! looks the same under every icon theme: grey idle, blue connected, green
//! talking, red muted.

use ksni::blocking::TrayMethods;
use ksni::menu::{CheckmarkItem, StandardItem, SubMenu};
use ksni::{Icon, MenuItem, ToolTip, Tray};

use crate::ctl::{UiRequest, ui};
use crate::net::{Cmd, Handle, Snapshot};

pub struct TsTray {
    net: Handle,
    snap: Snapshot,
}

pub struct TrayHandle(ksni::blocking::Handle<TsTray>);

impl TrayHandle {
    pub fn update(&self, snap: &Snapshot) {
        let snap = snap.clone();
        self.0.update(move |t| t.snap = snap);
    }
}

pub fn spawn(net: Handle) -> Option<TrayHandle> {
    TsTray { net, snap: Snapshot::default() }.spawn().ok().map(TrayHandle)
}

/// The logo mark as RGBA pixels at `size`.
fn mark(size: u32) -> Vec<[u8; 4]> {
    let tree = resvg::usvg::Tree::from_data(include_bytes!("../assets/wisp-mark.svg"), &Default::default());
    let (Ok(tree), Some(mut pixmap)) = (tree, resvg::tiny_skia::Pixmap::new(size, size)) else { return Vec::new() };
    let scale = size as f32 / tree.size().width().max(tree.size().height());
    resvg::render(&tree, resvg::tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
    pixmap.pixels().iter().map(|p| { let c = p.demultiply(); [c.red(), c.green(), c.blue(), c.alpha()] }).collect()
}

/// The logo as a solid silhouette in `color`, so it reads like the other bar icons.
fn logo(color: (u8, u8, u8)) -> Icon {
    static PIXELS: std::sync::LazyLock<Vec<[u8; 4]>> = std::sync::LazyLock::new(|| mark(SIZE as u32));
    let mut data = Vec::with_capacity(PIXELS.len() * 4);
    for &[_, _, _, a] in PIXELS.iter() {
        // the faint end of the tail would vanish at bar size
        let a = (a as f32 * 2.2).min(255.0) as u8;
        data.extend_from_slice(&[a, color.0, color.1, color.2]);
    }
    Icon { width: SIZE, height: SIZE, data }
}

const SIZE: i32 = 64;

impl TsTray {
    fn state(&self) -> (&'static str, (u8, u8, u8)) {
        let s = &self.snap;
        if s.servers.is_empty() {
            ("Not connected", (0xa9, 0xb1, 0xd6))
        } else if s.self_muted || s.deafened {
            ("Muted", (0xe5, 0x53, 0x4b))
        } else if s.transmitting {
            ("Talking", (0x5e, 0xd2, 0x86))
        } else {
            ("Connected", (0x7a, 0xa2, 0xf7))
        }
    }
}

impl Tray for TsTray {
    fn id(&self) -> String {
        "wisp".into()
    }

    fn title(&self) -> String {
        "Wisp".into()
    }

    fn icon_pixmap(&self) -> Vec<Icon> {
        vec![logo(self.state().1)]
    }

    fn tool_tip(&self) -> ToolTip {
        let (state, _) = self.state();
        let lines: Vec<String> = self
            .snap
            .servers
            .iter()
            .map(|s| {
                let ch = s
                    .own_channel
                    .and_then(|id| s.channels.iter().find(|c| c.id == id))
                    .map(|c| c.name.as_str())
                    .unwrap_or("-");
                let name = if s.server_name.is_empty() { &s.label } else { &s.server_name };
                format!("{name}: {ch}")
            })
            .collect();
        ToolTip { title: format!("Wisp: {state}"), description: lines.join("\n"), ..Default::default() }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        ui(UiRequest::Show);
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let mut items: Vec<MenuItem<Self>> = vec![
            StandardItem {
                label: "Show wisp".into(),
                activate: Box::new(|_| ui(UiRequest::Show)),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            CheckmarkItem {
                label: "Mute microphone".into(),
                checked: self.snap.self_muted,
                activate: Box::new(|t: &mut Self| t.net.send(Cmd::SelfMute(!t.snap.self_muted))),
                ..Default::default()
            }
            .into(),
            CheckmarkItem {
                label: "Sound off".into(),
                checked: self.snap.deafened,
                activate: Box::new(|t: &mut Self| t.net.send(Cmd::Deafen(!t.snap.deafened))),
                ..Default::default()
            }
            .into(),
        ];
        if !self.snap.servers.is_empty() {
            let servers = self
                .snap
                .servers
                .iter()
                .map(|s| {
                    let con = s.con;
                    let name = if s.server_name.is_empty() { s.label.clone() } else { s.server_name.clone() };
                    StandardItem {
                        label: format!("Disconnect {name}"),
                        activate: Box::new(move |t: &mut Self| t.net.send(Cmd::Disconnect(con))),
                        ..Default::default()
                    }
                    .into()
                })
                .collect();
            items.push(SubMenu { label: "Servers".into(), submenu: servers, ..Default::default() }.into());
        }
        items.push(MenuItem::Separator);
        items.push(
            StandardItem { label: "Quit".into(), activate: Box::new(|_| ui(UiRequest::Quit)), ..Default::default() }
                .into(),
        );
        items
    }
}
