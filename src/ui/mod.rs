//! UI state shared by every window, and the windows themselves.

mod admin;
mod bbcode;
mod files_view;
mod icons;
mod palette;
mod channel_edit;
mod chat_view;
mod main_view;
mod menus;
mod settings;
mod theme;
mod updater;
mod widgets;

use futures::StreamExt;
use futures::channel::mpsc::UnboundedReceiver;
use gpui_kit::{
    AnyWindowHandle, App, AppContext, Bounds, Context, Entity, WindowBounds, WindowOptions, px, size,
};

use crate::config::Settings;
use crate::ctl::UiRequest;
use crate::net::{Cmd, ConId, Handle, Snapshot};
use crate::tray::TrayHandle;

pub use settings::SettingsTab;
pub use theme::apply as apply_theme;

pub struct Model {
    pub net: Handle,
    pub snap: Snapshot,
    pub settings: Settings,
    /// Client whose details panel is open.
    pub selected: Option<(ConId, u16)>,
    /// Channel whose info panel is open (when no client is selected).
    pub selected_channel: Option<(ConId, u64)>,
    tray: Option<TrayHandle>,
    main_window: Option<AnyWindowHandle>,
    settings_window: Option<AnyWindowHandle>,
    settings_view: Option<gpui_kit::WeakEntity<settings::SettingsView>>,
    admin_window: Option<AnyWindowHandle>,
    admin_view: Option<gpui_kit::WeakEntity<admin::AdminView>>,
    /// Current main window size, saved when it closes.
    pub main_size: Option<(f32, f32)>,
    /// A newer release and how far installing it got.
    pub update: Option<updater::UpdateState>,
}

impl Model {
    pub fn update_settings(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut Settings)) {
        f(&mut self.settings);
        self.settings.save();
        self.net.send(Cmd::Settings(Box::new(self.settings.clone())));
        cx.notify();
    }

    pub fn select(&mut self, target: Option<(ConId, u16)>, cx: &mut Context<Self>) {
        self.selected = target;
        if target.is_some() {
            self.selected_channel = None;
        }
        self.net.send(Cmd::Inspect(target));
        cx.notify();
    }

    pub fn select_channel(&mut self, target: Option<(ConId, u64)>, cx: &mut Context<Self>) {
        if self.selected.is_some() {
            self.select(None, cx);
        }
        self.selected_channel = target;
        if let Some((con, channel)) = target {
            self.net.send(Cmd::ChannelDescription { con, channel });
        }
        cx.notify();
    }
}

pub fn start(
    net: Handle,
    settings: Settings,
    mut snapshots: UnboundedReceiver<Snapshot>,
    mut requests: UnboundedReceiver<UiRequest>,
    show: bool,
    cx: &mut App,
) {
    // GPUI quits when the last window closes on Linux by default; we decide
    // ourselves (tray while connected, see on_window_closed below).
    cx.set_quit_mode(gpui_kit::QuitMode::Explicit);
    let tray = crate::tray::spawn(net.clone());
    crate::hotkeys::start(net.clone(), &settings);
    let model = cx.new(|_| Model {
        net,
        snap: Snapshot::default(),
        settings,
        selected: None,
        selected_channel: None,
        tray,
        main_window: None,
        settings_window: None,
        settings_view: None,
        admin_window: None,
        admin_view: None,
        main_size: None,
        update: None,
    });
    updater::start(&model, cx);

    let m = model.clone();
    cx.spawn(async move |cx| {
        while let Some(snap) = snapshots.next().await {
            let _ = cx.update(|cx| {
                m.update(cx, |m, cx| {
                    if let Some(t) = &m.tray {
                        t.update(&snap);
                    }
                    m.snap = snap;
                    cx.notify();
                })
            });
        }
    })
    .detach();

    let m = model.clone();
    cx.spawn(async move |cx| {
        while let Some(req) = requests.next().await {
            let _ = cx.update(|cx| match req {
                UiRequest::Show => open_main(&m, cx),
                UiRequest::Settings(tab) => {
                    let tab = match tab.as_str() {
                        "servers" => SettingsTab::Servers,
                        "hotkeys" => SettingsTab::Hotkeys,
                        "general" => SettingsTab::General,
                        "whisper" => SettingsTab::Whisper,
                        "identities" => SettingsTab::Identities,
                        "notifications" => SettingsTab::Notifications,
                        _ => SettingsTab::Audio,
                    };
                    open_settings(&m, tab, cx);
                }
                UiRequest::Admin(tab) => {
                    let files = tab.strip_prefix("files ").and_then(|name| {
                        let s = m.read(cx).snap.active_server()?;
                        let c = s.channels.iter().find(|c| c.name.eq_ignore_ascii_case(name.trim()))?;
                        Some(AdminTab::Files(c.id, c.name.clone()))
                    });
                    let tab = match tab.as_str() {
                        _ if files.is_some() => files.unwrap(),
                        "channel-groups" => AdminTab::ChannelGroups,
                        "bans" => AdminTab::Bans,
                        "keys" => AdminTab::Keys,
                        "complaints" => AdminTab::Complaints,
                        "server" => AdminTab::Server,
                        "log" => AdminTab::Log,
                        "inbox" => AdminTab::Inbox(None),
                        _ => AdminTab::ServerGroups,
                    };
                    if let Some(con) = m.read(cx).snap.active {
                        open_admin(&m, con, tab, cx);
                    }
                }
                UiRequest::Link(url) => open_link(&m, &url, cx),
                UiRequest::Quit => quit(&m, cx),
            });
        }
    })
    .detach();

    // Closing the main window: stay in the tray while connected (if enabled),
    // otherwise quit.
    let m = model.clone();
    cx.on_window_closed(move |cx, id| {
        let (is_main, keep) = m.update(cx, |m, cx| {
            let is_main = m.main_window.is_some_and(|w| w.window_id() == id);
            if is_main {
                m.main_window = None;
                remember_size(m, cx);
            }
            if m.settings_window.is_some_and(|w| w.window_id() == id) {
                m.settings_window = None;
            }
            if m.admin_window.is_some_and(|w| w.window_id() == id) {
                m.admin_window = None;
            }
            (is_main, m.settings.close_to_tray && m.tray.is_some() && !m.snap.servers.is_empty())
        });
        if is_main && !keep {
            quit(&m, cx);
        }
    })
    .detach();

    if show {
        open_main(&model, cx);
    }
}

/// Connect to a ts3server:// link: its bookmark if we have one, else a new one.
fn open_link(model: &Entity<Model>, url: &str, cx: &mut App) {
    let Some(link) = crate::desktop::parse_link(url) else { return };
    let host = |a: &str| a.trim_end_matches(":9987").to_lowercase();
    let bookmark = model.update(cx, |m, cx| {
        let existing = m.settings.bookmarks.iter().find(|b| host(&b.address) == host(&link.address)).cloned();
        let mut b = match existing {
            Some(b) => b,
            None => {
                let nickname = if link.nickname.is_empty() {
                    m.settings.bookmarks.first().map(|b| b.nickname.clone()).unwrap_or_default()
                } else {
                    link.nickname.clone()
                };
                let b = crate::config::Bookmark {
                    id: crate::config::new_id(),
                    label: link.address.clone(),
                    address: link.address.clone(),
                    password: link.password.clone(),
                    nickname,
                    ..Default::default()
                };
                let saved = b.clone();
                m.update_settings(cx, |s| s.bookmarks.push(saved));
                b
            }
        };
        // this time: the link's channel and password
        if !link.channel.is_empty() {
            b.channel = link.channel.clone();
        }
        if !link.password.is_empty() {
            b.password = link.password.clone();
        }
        b
    });
    model.read(cx).net.send(Cmd::Connect(bookmark));
    open_main(model, cx);
}

fn remember_size(m: &mut Model, cx: &mut Context<Model>) {
    if let Some(size) = m.main_size.filter(|s| Some(*s) != m.settings.window_size) {
        m.update_settings(cx, |s| s.window_size = Some(size));
    }
}

/// Disconnect cleanly (so the servers don't keep a ghost of us), then quit.
pub fn quit(model: &Entity<Model>, cx: &mut App) {
    model.update(cx, remember_size);
    let (tx, rx) = tokio::sync::oneshot::channel();
    model.read(cx).net.send(Cmd::Shutdown(tx));
    cx.spawn(async move |cx| {
        let timer = cx.background_executor().timer(std::time::Duration::from_secs(2));
        futures::future::select(rx, timer).await;
        let _ = cx.update(|cx| cx.quit());
    })
    .detach();
}

pub fn open_main(model: &Entity<Model>, cx: &mut App) {
    if let Some(w) = model.read(cx).main_window {
        if w.update(cx, |_, window, _| window.activate_window()).is_ok() {
            return;
        }
    }
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            {
                let (w, h) = model.read(cx).settings.window_size.unwrap_or((1100., 760.));
                size(px(w.max(640.)), px(h.max(420.)))
            },
            cx,
        ))),
        app_id: Some("wisp".into()),
        ..Default::default()
    };
    let m = model.clone();
    if let Ok((handle, _)) =
        gpui_kit::open_window(options, cx, |window, cx| cx.new(|cx| main_view::MainView::new(m, window, cx)))
    {
        model.update(cx, |m, _| m.main_window = Some(handle));
    }
}

pub use admin::AdminTab;

/// The admin window for one server (reused, switched to `tab`).
pub fn open_admin(model: &Entity<Model>, con: ConId, tab: AdminTab, cx: &mut App) {
    let (window, view) = {
        let m = model.read(cx);
        (m.admin_window, m.admin_view.clone())
    };
    if let (Some(w), Some(view)) = (window, view) {
        let same_server = view.upgrade().is_some_and(|v| v.read(cx).con == con);
        if same_server {
            let shown = w.update(cx, |_, window, cx| {
                window.activate_window();
                let _ = view.update(cx, |v, cx| v.switch(tab.clone(), window, cx));
            });
            if shown.is_ok() {
                return;
            }
        } else {
            let _ = w.update(cx, |_, window, _| window.remove_window());
        }
    }
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size(px(1000.), px(720.)), cx))),
        app_id: Some("wisp".into()),
        ..Default::default()
    };
    let m = model.clone();
    if let Ok((handle, view)) =
        gpui_kit::open_window(options, cx, |window, cx| cx.new(|cx| admin::AdminView::new(m, con, tab, window, cx)))
    {
        model.update(cx, |m, _| {
            m.admin_window = Some(handle);
            m.admin_view = Some(view.downgrade());
        });
    }
}

pub fn open_settings(model: &Entity<Model>, tab: SettingsTab, cx: &mut App) {
    let (window, view) = {
        let m = model.read(cx);
        (m.settings_window, m.settings_view.clone())
    };
    if let (Some(w), Some(view)) = (window, view) {
        let shown = w.update(cx, |_, window, cx| {
            window.activate_window();
            let _ = view.update(cx, |v, cx| v.switch(tab, window, cx));
        });
        if shown.is_ok() {
            return;
        }
    }
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size(px(900.), px(640.)), cx))),
        app_id: Some("wisp".into()),
        ..Default::default()
    };
    let m = model.clone();
    if let Ok((handle, view)) = gpui_kit::open_window(options, cx, |window, cx| {
        cx.new(|cx| settings::SettingsView::new(m, tab, window, cx))
    }) {
        model.update(cx, |m, _| {
            m.settings_window = Some(handle);
            m.settings_view = Some(view.downgrade());
        });
    }
}
