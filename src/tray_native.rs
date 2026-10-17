//! Tray icon on Windows (notification area) and macOS (menu bar), through
//! tray-icon. Lives on the main thread; menu clicks come back through muda's
//! event handler.

use std::sync::{Arc, Mutex};

use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

use crate::ctl::{UiRequest, ui};
use crate::net::{Cmd, ConId, Handle, Snapshot};

const SIZE: u32 = 64;

/// What the menu and the icon were last built from, to skip needless rebuilds.
#[derive(Clone, Default, PartialEq)]
struct Shown {
    state: &'static str,
    muted: bool,
    deafened: bool,
    servers: Vec<(ConId, String)>,
}

pub struct TrayHandle {
    tray: TrayIcon,
    shown: Mutex<Shown>,
    /// Read by the menu handler for the toggles.
    current: Arc<Mutex<Shown>>,
}

fn icon(color: (u8, u8, u8)) -> Option<Icon> {
    Icon::from_rgba(crate::logo::silhouette(SIZE, color), SIZE, SIZE).ok()
}

fn menu(shown: &Shown) -> Menu {
    let menu = Menu::new();
    let _ = menu.append(&MenuItem::with_id("show", "Show Wisp", true, None));
    let _ = menu.append(&PredefinedMenuItem::separator());
    let _ = menu.append(&CheckMenuItem::with_id("mute", "Mute microphone", true, shown.muted, None));
    let _ = menu.append(&CheckMenuItem::with_id("deafen", "Sound off", true, shown.deafened, None));
    if !shown.servers.is_empty() {
        let servers = Submenu::with_id("servers", "Servers", true);
        for (con, name) in &shown.servers {
            let _ = servers.append(&MenuItem::with_id(format!("disconnect:{con}"), format!("Disconnect {name}"), true, None));
        }
        let _ = menu.append(&servers);
    }
    let _ = menu.append(&PredefinedMenuItem::separator());
    let _ = menu.append(&MenuItem::with_id("quit", "Quit", true, None));
    menu
}

impl TrayHandle {
    pub fn update(&self, snap: &Snapshot) {
        let (state, color) = crate::logo::state(snap);
        let now = Shown {
            state,
            muted: snap.self_muted,
            deafened: snap.deafened,
            servers: snap
                .servers
                .iter()
                .map(|s| (s.con, if s.server_name.is_empty() { s.label.clone() } else { s.server_name.clone() }))
                .collect(),
        };
        let tooltip = match crate::logo::where_am_i(snap) {
            w if w.is_empty() => format!("Wisp: {state}"),
            w => format!("Wisp: {state}\n{w}"),
        };
        let _ = self.tray.set_tooltip(Some(tooltip));
        let mut shown = self.shown.lock().unwrap();
        if *shown == now {
            return;
        }
        if shown.state != now.state {
            // the macOS menu bar wants a template (it picks the color) unless
            // the state needs a color
            #[cfg(target_os = "macos")]
            if matches!(state, "Not connected" | "Connected") {
                let _ = self.tray.set_icon_templated(icon((0, 0, 0)));
            } else {
                let _ = self.tray.set_icon(icon(color));
            }
            #[cfg(not(target_os = "macos"))]
            let _ = self.tray.set_icon(icon(color));
        }
        self.tray.set_menu(Some(Box::new(menu(&now))));
        *self.current.lock().unwrap() = now.clone();
        *shown = now;
    }
}

pub fn spawn(net: Handle) -> Option<TrayHandle> {
    let current = Arc::new(Mutex::new(Shown::default()));
    let state = current.clone();
    MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
        let shown = state.lock().unwrap().clone();
        match e.id.0.as_str() {
            "show" => ui(UiRequest::Show),
            "mute" => net.send(Cmd::SelfMute(!shown.muted)),
            "deafen" => net.send(Cmd::Deafen(!shown.deafened)),
            "quit" => ui(UiRequest::Quit),
            id => {
                if let Some(con) = id.strip_prefix("disconnect:").and_then(|c| c.parse().ok()) {
                    net.send(Cmd::Disconnect(con));
                }
            }
        }
    }));
    TrayIconEvent::set_event_handler(Some(|e: TrayIconEvent| {
        if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = e {
            ui(UiRequest::Show);
        }
    }));
    let shown = Shown::default();
    let builder = TrayIconBuilder::new()
        .with_id("wisp")
        .with_tooltip("Wisp")
        .with_menu(Box::new(menu(&shown)))
        .with_menu_on_left_click(false);
    #[cfg(target_os = "macos")]
    let builder = builder.with_icon_templated(icon((0, 0, 0))?);
    #[cfg(not(target_os = "macos"))]
    let builder = builder.with_icon(icon(crate::logo::state(&Snapshot::default()).1)?);
    let tray = builder.build().ok()?;
    Some(TrayHandle { tray, shown: Mutex::new(Shown { state: "Not connected", ..shown }), current })
}
