//! StatusNotifierItem tray icon (ksni). The icon is drawn here, so it looks the
//! same under every icon theme: grey idle, blue connected, green talking, red muted.

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

fn circle(rgb: (u8, u8, u8), ring: bool) -> Icon {
    let size = 32i32;
    let mut data = Vec::with_capacity((size * size * 4) as usize);
    let c = (size as f32 - 1.0) / 2.0;
    for y in 0..size {
        for x in 0..size {
            let d = ((x as f32 - c).powi(2) + (y as f32 - c).powi(2)).sqrt();
            let inside = if ring { d <= 15.0 && (d >= 11.0 || d <= 7.0) } else { d <= 13.0 };
            let a = if inside { 255 } else { 0 };
            data.extend_from_slice(&[a, rgb.0, rgb.1, rgb.2]); // ARGB32, network order
        }
    }
    Icon { width: size, height: size, data }
}

impl TsTray {
    fn state(&self) -> (&'static str, (u8, u8, u8), bool) {
        let s = &self.snap;
        if s.servers.is_empty() {
            ("Not connected", (0x8a, 0x8f, 0x98), false)
        } else if s.self_muted || s.deafened {
            ("Muted", (0xe5, 0x53, 0x4b), false)
        } else if s.transmitting {
            ("Talking", (0x3e, 0xc2, 0x6b), true)
        } else {
            ("Connected", (0x4f, 0x8c, 0xff), false)
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
        let (_, rgb, ring) = self.state();
        vec![circle(rgb, ring)]
    }

    fn tool_tip(&self) -> ToolTip {
        let (state, _, _) = self.state();
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
