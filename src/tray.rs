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

/// The logo as a solid silhouette in `color`, ARGB32 in network order.
fn logo(color: (u8, u8, u8)) -> Icon {
    let data = crate::logo::silhouette(SIZE as u32, color).chunks(4).flat_map(|p| [p[3], p[0], p[1], p[2]]).collect();
    Icon { width: SIZE, height: SIZE, data }
}

const SIZE: i32 = 64;

impl Tray for TsTray {
    fn id(&self) -> String {
        "wisp".into()
    }

    fn title(&self) -> String {
        "Wisp".into()
    }

    fn icon_pixmap(&self) -> Vec<Icon> {
        vec![logo(crate::logo::state(&self.snap).1)]
    }

    fn tool_tip(&self) -> ToolTip {
        let (state, _) = crate::logo::state(&self.snap);
        ToolTip { title: format!("Wisp: {state}"), description: crate::logo::where_am_i(&self.snap), ..Default::default() }
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
