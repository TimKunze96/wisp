//! Main window: servers on the left, channel tree in the middle, user
//! details or the activity feed on the right. Every column shares the same
//! header and footer height so the rows line up.

use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::ContextMenuExt;
use gpui_kit::component::slider::{Slider, SliderEvent, SliderState};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{
    AnyElement, AppContext, ClickEvent, Context, DismissEvent, DragMoveEvent, Entity, FontWeight, InteractiveElement, KeyDownEvent,
    IntoElement, ParentElement, Render, SharedString, StatefulInteractiveElement, Styled, Window, div, px, rgb, rgba,
};

use std::collections::HashMap;

use gpui_kit::{Anchor, ScrollHandle, StyledImage};

use super::channel_edit::ChannelEditor;
use super::chat_view::Tab;
use gpui_kit::component::Icon;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::menu::DropdownMenu;
use gpui_kit::component::Sizable;

use super::menus::{Prompt, PromptKind, UserMenuData, channel_menu, self_menu, user_menu};
use super::widgets::*;
use super::{Model, SettingsTab, open_settings};
use crate::config::{Bookmark, VoiceMode};
use crate::net::{
    AdminAction, ChannelRow, ChannelType, ChatKind, ClientDetails, Cmd, Codec, ConId, MaxClients, ServerSnap, Status, Zone,
    channel_drop,
};

const SIDEBAR_W: f32 = 240.;
const RIGHT_W: f32 = 320.;

pub struct MainView {
    pub(super) model: Entity<Model>,
    pub(super) prompt: Option<Prompt>,
    pub(super) chat: Entity<InputState>,
    /// Volume slider of the selected user, with their uid.
    volume: Option<(String, Entity<SliderState>)>,
    editor: Option<Entity<ChannelEditor>>,
    /// Where a dragged channel or user would land.
    drop_hint: Option<(u64, Zone)>,
    /// Last password request we showed a prompt for.
    asked: u64,
    pub(super) tab: Tab,
    /// Lines of each conversation we've shown: (server, conversation) -> received.
    pub(super) seen: HashMap<(ConId, ChatKind), u64>,
    pub(super) chat_scroll: ScrollHandle,
    pub(super) shown_len: usize,
    chat_hint: String,
    /// Tree filter, shown while `filtering`.
    filter: Entity<InputState>,
    filtering: bool,
    /// Connections whose host message popup was closed.
    host_seen: std::collections::HashSet<ConId>,
    pub(super) palette: Option<super::palette::Palette>,
    focus: gpui_kit::FocusHandle,
    cursor: Option<Cursor>,
}

/// TeamSpeak spacer channels: `[spacer]`, `[cspacer]`, `[rspacer]`, `[*spacer]`,
/// each optionally followed by an id (`[cspacer12]Title`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Spacer {
    Left,
    Center,
    Right,
    Repeat,
}

impl Spacer {
    fn parse(name: &str) -> Option<(Spacer, String)> {
        let rest = name.strip_prefix('[')?;
        let (tag, text) = rest.split_once(']')?;
        let (kind, id) = if let Some(id) = tag.strip_prefix("cspacer") {
            (Spacer::Center, id)
        } else if let Some(id) = tag.strip_prefix("rspacer") {
            (Spacer::Right, id)
        } else if let Some(id) = tag.strip_prefix("*spacer") {
            (Spacer::Repeat, id)
        } else if let Some(id) = tag.strip_prefix("lspacer").or_else(|| tag.strip_prefix("spacer")) {
            (Spacer::Left, id)
        } else {
            return None;
        };
        // `[*cspacer]` style combos aren't a thing; anything else after the
        // keyword must be the id
        if id.contains(['[', ']']) {
            return None;
        }
        Some((kind, text.to_string()))
    }
}

/// A row of the tree.
#[derive(Clone, Copy)]
enum TreeItem {
    Channel { index: usize, has_children: bool, folded: bool },
    Client { channel: usize, client: usize },
}

/// The row the arrow keys are on.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Cursor {
    Channel(u64),
    Client(u16),
}

#[derive(Clone)]
struct DragChannel {
    con: ConId,
    id: u64,
    name: String,
}

#[derive(Clone)]
struct DragClient {
    con: ConId,
    id: u16,
    name: String,
    me: bool,
}

/// What follows the pointer while dragging.
struct DragGhost {
    icon: &'static str,
    text: String,
}

impl Render for DragGhost {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .gap_2()
            .items_center()
            .px_3()
            .h(px(28.))
            .rounded(px(5.))
            .bg(rgb(RAISED))
            .border_1()
            .border_color(rgba(0xffffff26))
            .shadow_lg()
            .font_family(FONT)
            .text_sm()
            .text_color(rgb(TEXT))
            .child(icon(self.icon, 14., MUTED))
            .child(self.text.clone())
    }
}

/// Square-ish icon button used in headers and footers.
fn icon_button(id: impl Into<SharedString>, name: &'static str, color: u32, on: bool) -> gpui_kit::Stateful<gpui_kit::Div> {
    div()
        .id(id.into())
        .size(px(30.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(5.))
        .cursor_pointer()
        .when(on, |d| d.bg(alpha(color, 0x1f)))
        .hover(|d| d.bg(hover_bg()))
        .child(icon(name, 16., color))
}

impl MainView {
    pub fn new(model: Entity<Model>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.observe_window_bounds(window, |this, window, cx| {
            let v = window.viewport_size();
            let size = (f32::from(v.width), f32::from(v.height));
            // before the compositor configures the window it reports the whole output
            if size.0 >= 400. && size.1 >= 300. && size.0 <= 3000. && size.1 <= 2000. {
                this.model.update(cx, |m, _| m.main_size = Some(size));
            }
        })
        .detach();
        cx.observe_window_activation(window, |this, window, cx| {
            this.send(cx, Cmd::Focused(window.is_window_active()));
        })
        .detach();
        cx.observe(&model, |this, _, cx| {
            this.chat_seen(cx);
            cx.notify()
        })
        .detach();
        let chat = cx.new(|cx| InputState::new(window, cx).placeholder("Message your channel"));
        cx.subscribe_in(&chat, window, |this, input, ev: &InputEvent, window, cx| {
            if let InputEvent::PressEnter { .. } = ev {
                let text = input.read(cx).value().trim().to_string();
                if this.send_chat(text, cx) {
                    input.update(cx, |i, cx| i.set_value("", window, cx));
                }
            }
        })
        .detach();
        // keys (Ctrl+K, Esc) need focus somewhere in the window from the start
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter channels and people"));
        cx.subscribe(&filter, |_, _, _: &InputEvent, cx| cx.notify()).detach();
        // The server wants a channel password: ask (once per request).
        cx.observe_in(&model, window, |this, model, window, cx| {
            let Some(req) = model.read(cx).snap.password_request.clone() else { return };
            if this.asked == req.serial {
                return;
            }
            this.asked = req.serial;
            if req.wrong {
                let key = crate::config::channel_key(&req.bookmark, req.channel);
                model.update(cx, |m, cx| {
                    if m.settings.channel_passwords.contains_key(&key) {
                        m.update_settings(cx, |s| {
                            s.channel_passwords.remove(&key);
                        });
                    }
                });
            }
            let kind = PromptKind::JoinChannel { wrong: req.wrong };
            this.open_prompt(kind, req.con, req.channel, req.name.clone(), String::new(), window, cx);
            if let Some(p) = &mut this.prompt {
                p.key = req.bookmark.clone();
            }
        })
        .detach();
        Self {
            model,
            prompt: None,
            chat,
            volume: None,
            editor: None,
            drop_hint: None,
            asked: 0,
            tab: Tab::default(),
            seen: Default::default(),
            chat_scroll: ScrollHandle::new(),
            shown_len: 0,
            chat_hint: "Message your channel".into(),
            filter,
            filtering: false,
            host_seen: Default::default(),
            palette: None,
            focus,
            cursor: None,
        }
    }

    /// Esc closes whatever is open; Ctrl+K opens the quick switcher.
    fn on_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let k = &ev.keystroke;
        if k.modifiers.control && k.key == "k" {
            self.open_palette(window, cx);
            cx.stop_propagation();
            return;
        }
        if self.palette.is_some() && self.palette_key(ev, cx) {
            cx.stop_propagation();
            return;
        }
        if self.focus.is_focused(window)
            && !k.modifiers.modified()
            && matches!(k.key.as_str(), "up" | "down" | "left" | "right" | "enter")
            && self.tree_key(&k.key, window, cx)
        {
            cx.stop_propagation();
            return;
        }
        if k.key == "escape" {
            if self.palette.take().is_some() {
            } else if self.prompt.take().is_some() {
            } else if self.editor.take().is_some() {
            } else if self.filtering {
                self.filtering = false;
                self.filter.update(cx, |i, cx| i.set_value("", window, cx));
            } else {
                let m = self.model.read(cx);
                let (user, channel) = (m.selected.is_some(), m.selected_channel.is_some());
                if user || channel {
                    self.model.update(cx, |m, cx| {
                        m.select(None, cx);
                        m.select_channel(None, cx);
                    });
                } else {
                    return;
                }
            }
            window.focus(&self.focus, cx);
            cx.stop_propagation();
            cx.notify();
        }
    }

    /// The server's host message popup, once per connection.
    fn host_message(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let s = self.model.read(cx).snap.active_server()?;
        let (text, mode) = s.host_message.clone()?;
        let con = s.con;
        if self.host_seen.contains(&con) {
            return None;
        }
        let title = if s.server_name.is_empty() { s.label.clone() } else { s.server_name.clone() };
        let rich = super::bbcode::parse(&text);
        Some(
            div()
                .absolute()
                .inset_0()
                .bg(rgba(0x00000099))
                .flex()
                .items_center()
                .justify_center()
                .occlude()
                .child(
                    v_flex()
                        .w(px(460.))
                        .gap_4()
                        .p_5()
                        .rounded(px(8.))
                        .bg(rgb(PANEL))
                        .border_1()
                        .border_color(border())
                        .child(div().font_weight(FontWeight::SEMIBOLD).child(title))
                        .child(div().text_sm().text_color(rgb(0xc4c8ce)).child(rich.element("host-msg")))
                        .when(mode == 3, |d| d.child(label("The server disconnects you after this message.")))
                        .child(h_flex().justify_end().child(
                            Button::new("host-ok").label("OK").small().primary().on_click(cx.listener(move |this, _, _, cx| {
                                this.host_seen.insert(con);
                                if mode == 3 {
                                    this.send(cx, Cmd::Disconnect(con));
                                }
                                cx.notify();
                            })),
                        )),
                )
                .into_any_element(),
        )
    }

    pub(super) fn open_channel_editor(
        &mut self,
        con: ConId,
        original: Option<ChannelRow>,
        parent: u64,
        parent_name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(o) = original.as_ref().filter(|o| o.description.is_none()) {
            self.send(cx, Cmd::ChannelDescription { con, channel: o.id });
        }
        let model = self.model.clone();
        let editor = cx.new(|cx| ChannelEditor::new(model, con, original, parent, parent_name, window, cx));
        cx.subscribe(&editor, |this, _, _: &DismissEvent, cx| {
            this.editor = None;
            cx.notify();
        })
        .detach();
        self.editor = Some(editor);
        cx.notify();
    }

    fn set_hint(&mut self, hint: Option<(u64, Zone)>, cx: &mut Context<Self>) {
        if self.drop_hint != hint {
            self.drop_hint = hint;
            cx.notify();
        }
    }

    pub(super) fn send(&self, cx: &Context<Self>, cmd: Cmd) {
        self.model.read(cx).net.send(cmd);
    }

    // ---- sidebar -------------------------------------------------------

    fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let m = self.model.read(cx);
        let bookmarks = m.settings.bookmarks.clone();
        let servers = m.snap.servers.clone();
        let active = m.snap.active;
        let mut list = v_flex().gap_0p5();
        for b in &bookmarks {
            let server = servers.iter().find(|s| s.bookmark == b.id);
            list = list.child(self.bookmark_row(b, server, active, cx));
        }
        if bookmarks.is_empty() {
            list = list.child(label("No servers yet."));
        }
        let model = self.model.clone();
        v_flex()
            .w(px(SIDEBAR_W))
            .flex_none()
            .h_full()
            .bg(rgb(SIDEBAR))
            .border_r_1()
            .border_color(border())
            .child(
                header_row().child(logo()).child(div().flex_1()).child({
                    let model = self.model.clone();
                    icon_button("settings", "settings", MUTED, false)
                        .size(px(28.))
                        .tooltip(|w, cx| Tooltip::new("Settings").build(w, cx))
                        .on_click(move |_, _, cx| open_settings(&model, SettingsTab::Audio, cx))
                }),
            )
            .child(
                v_flex()
                    .id("servers")
                    .flex_1()
                    .overflow_y_scroll()
                    .px_2()
                    .py_3()
                    .gap_2()
                    .child(
                        h_flex()
                            .px_2()
                            .justify_between()
                            .items_center()
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(rgb(DIM))
                                    .child("SERVERS"),
                            )
                            .child(
                                icon_button("add-server", "plus", MUTED, false)
                                    .size(px(22.))
                                    .tooltip(|w, cx| Tooltip::new("Add server").build(w, cx))
                                    .on_click(move |_, _, cx| open_settings(&model, SettingsTab::Servers, cx)),
                            ),
                    )
                    .child(list),
            )
            .child(self.self_panel(cx))
    }

    fn bookmark_row(
        &self,
        b: &Bookmark,
        server: Option<&ServerSnap>,
        active: Option<ConId>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let (dot, sub) = match server.map(|s| &s.status) {
            None => (DIM, "Offline".to_string()),
            Some(Status::Connecting) => (AMBER, "Connecting...".into()),
            Some(Status::Reconnecting) => (AMBER, "Reconnecting...".into()),
            Some(Status::Connected) => (GREEN, format!("{} online", server.map(|s| s.users).unwrap_or(0))),
            Some(Status::NeedsPassword) => (AMBER, "Password needed".into()),
            Some(Status::Failed(e)) => (RED, e.clone()),
        };
        let is_active = server.is_some_and(|s| Some(s.con) == active);
        let (unread, mention) = server.map(|s| self.server_unread(s)).unwrap_or((0, false));
        let connected = server.is_some_and(|s| !s.status.is_final());
        let needs_password = server.is_some_and(|s| s.status == Status::NeedsPassword);
        let con = server.map(|s| s.con);
        let bookmark = b.clone();
        let name = if b.label.is_empty() { b.address.clone() } else { b.label.clone() };
        h_flex()
            .id(SharedString::from(format!("bm-{}", b.id)))
            .group("bm")
            .gap_3()
            .px_2()
            .py_1p5()
            .rounded(px(6.))
            .cursor_pointer()
            .when(is_active, |d| d.bg(active_bg()))
            .hover(|d| d.bg(hover_bg()))
            .on_click(cx.listener(move |this, _, window, cx| match con {
                Some(con) if connected => this.send(cx, Cmd::SetActive(con)),
                Some(con) if needs_password => {
                    let name = if bookmark.label.is_empty() { bookmark.address.clone() } else { bookmark.label.clone() };
                    this.open_prompt(PromptKind::ServerPassword, con, 0, name, String::new(), window, cx);
                    if let Some(p) = &mut this.prompt {
                        p.key = bookmark.id.clone();
                    }
                }
                _ => this.send(cx, Cmd::Connect(bookmark.clone())),
            }))
            .child(
                div()
                    .size(px(32.))
                    .flex_none()
                    .rounded(px(6.))
                    .bg(rgb(RAISED))
                    .flex()
                    .items_center()
                    .justify_center()
                    .relative()
                    .text_size(px(12.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(if is_active { TEXT } else { MUTED }))
                    .child(initials(&name))
                    .child(
                        div()
                            .absolute()
                            .bottom(px(-2.))
                            .right(px(-2.))
                            .size(px(10.))
                            .rounded_full()
                            .border_2()
                            .border_color(rgb(SIDEBAR))
                            .bg(rgb(dot)),
                    ),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .text_sm()
                            .truncate()
                            .text_color(rgb(if is_active { TEXT } else { 0xc4c8ce }))
                            .child(name),
                    )
                    .child(label(sub).truncate().when(dot == RED, |d| d.text_color(rgb(RED)))),
            )
            .when(unread > 0, |d| d.child(super::chat_view::pill(unread, mention)))
            .when_some(con.filter(|_| connected), |d, con| {
                d.child(
                    icon_button(format!("dc-{con}"), "unplug", MUTED, false)
                        .size(px(24.))
                        .invisible()
                        .group_hover("bm", |s| s.visible())
                        .tooltip(|w, cx| Tooltip::new("Disconnect").build(w, cx))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.send(cx, Cmd::Disconnect(con));
                        })),
                )
            })
    }

    fn self_panel(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let m = self.model.read(cx);
        let me_row = m.snap.active_server().and_then(|s| s.channels.iter().flat_map(|c| &c.clients).find(|c| c.me).cloned());
        let me = me_row.as_ref().map(|c| (c.name.clone(), c.uid.clone()));
        let my_avatar = me_row.as_ref().and_then(|c| c.avatar.clone());
        let away = m.snap.away.clone();
        let con = m.snap.active;
        let (name, _) = me.unwrap_or_else(|| {
            let nick = m.settings.bookmarks.first().map(|b| b.nickname.clone()).unwrap_or_else(|| "Wisp".into());
            (nick.clone(), nick)
        });
        let (muted, deaf, talking) = (m.snap.self_muted, m.snap.deafened, m.snap.transmitting);
        footer_row()
            .bg(rgb(SIDEBAR))
            .child(avatar(&name, my_avatar.as_deref(), 30., talking))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(div().text_sm().truncate().child(name))
                    .child(
                        label(match (&away, muted, talking) {
                            (_, true, _) => "Mic muted".to_string(),
                            (_, _, true) => "Talking".into(),
                            (Some(a), ..) if !a.is_empty() => format!("Away: {a}"),
                            (Some(_), ..) => "Away".into(),
                            _ => "Online".into(),
                        })
                        .truncate()
                        .when(talking, |d| d.text_color(rgb(GREEN)))
                        .when(muted, |d| d.text_color(rgb(RED))),
                    ),
            )
            .child({
                let (model, view) = (self.model.clone(), cx.entity().downgrade());
                Button::new("me-menu")
                    .ghost()
                    .xsmall()
                    .icon(Icon::empty().path("icons/chevron-up.svg"))
                    .dropdown_menu_with_anchor(Anchor::BottomLeft, move |menu, _, _| {
                        self_menu(menu, con, me_row.clone(), away.clone(), &model, &view)
                    })
            })
            .child(
                icon_button("self-mute", if muted { "mic-off" } else { "mic" }, if muted { RED } else { MUTED }, muted)
                    .tooltip(|w, cx| Tooltip::new("Mute microphone").build(w, cx))
                    .on_click(cx.listener(move |this, _, _, cx| this.send(cx, Cmd::SelfMute(!muted)))),
            )
            .child(
                icon_button("self-deaf", if deaf { "headphone-off" } else { "headphones" }, if deaf { RED } else { MUTED }, deaf)
                    .tooltip(|w, cx| Tooltip::new("Sound off").build(w, cx))
                    .on_click(cx.listener(move |this, _, _, cx| this.send(cx, Cmd::Deafen(!deaf)))),
            )
    }

    // ---- centre ----------------------------------------------------------

    fn center(&self, cx: &mut Context<Self>) -> AnyElement {
        let m = self.model.read(cx);
        let Some(server) = m.snap.active_server().cloned() else {
            return v_flex()
                .flex_1()
                .child(header_row())
                .child(
                    v_flex()
                        .flex_1()
                        .items_center()
                        .justify_center()
                        .gap_3()
                        .child(icon("wind", 32., DIM))
                        .child(label("Pick a server on the left to connect.")),
                )
                .child(footer_row())
                .into_any_element();
        };
        v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(self.server_header(&server, cx))
            .when(self.filtering, |d| {
                d.child(
                    h_flex()
                        .px_3()
                        .pt_3()
                        .child(div().flex_1().child(Input::new(&self.filter).cleanable(true))),
                )
            })
            .child(self.tree(&server, cx))
            .child(self.talk_bar(cx))
            .into_any_element()
    }

    fn server_header(&self, s: &ServerSnap, cx: &mut Context<Self>) -> impl IntoElement {
        let con = s.con;
        let name = if s.server_name.is_empty() { s.label.clone() } else { s.server_name.clone() };
        let details = format!("{} · {} {}", s.label, s.platform, s.version.split(' ').next().unwrap_or(""));
        let dot = || div().size(px(3.)).rounded_full().bg(rgb(DIM));
        header_row()
            .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).truncate().child(name))
            .child(div().flex_1())
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(readout(format!("{}/{} online", s.users, s.max_users), MUTED))
                    .when_some(s.ping_ms, |d, p| d.child(dot()).child(readout(format!("{p:.0} ms"), ping_color(p))))
                    .when_some(s.packet_loss.filter(|l| *l >= 0.05), |d, l| {
                        d.child(dot()).child(readout(format!("{l:.1}% loss"), if l < 5. { AMBER } else { RED }))
                    }),
            )
            .when_some(s.host_button.clone(), |d, (path, link, tip)| {
                d.child(
                    div()
                        .id("host-button")
                        .size(px(26.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(5.))
                        .cursor_pointer()
                        .hover(|d| d.bg(hover_bg()))
                        .when(!tip.is_empty(), |d| d.tooltip(move |w, cx| Tooltip::new(tip.clone()).build(w, cx)))
                        .when(!link.is_empty(), |d| d.on_click(move |_, _, cx| cx.open_url(&link)))
                        .child(gpui_kit::img(path).size(px(18.))),
                )
            })
            .child(
                div()
                    .id("server-info")
                    .tooltip(move |w, cx| Tooltip::new(details.clone()).build(w, cx))
                    .child(icon("info", 14., DIM)),
            )
            .child(
                icon_button("filter", "search", if self.filtering { TEXT } else { MUTED }, self.filtering)
                    .size(px(26.))
                    .tooltip(|w, cx| Tooltip::new("Filter").build(w, cx))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.filtering = !this.filtering;
                        if this.filtering {
                            this.filter.update(cx, |i, cx| i.focus(window, cx));
                        } else {
                            this.filter.update(cx, |i, cx| i.set_value("", window, cx));
                        }
                        cx.notify();
                    })),
            )
            .child({
                let model = self.model.clone();
                icon_button("admin", "shield", MUTED, false)
                    .size(px(26.))
                    .tooltip(|w, cx| Tooltip::new("Server admin").build(w, cx))
                    .on_click(move |_, _, cx| super::open_admin(&model, con, super::AdminTab::ServerGroups, cx))
            })
            .child(
                icon_button("new-channel", "folder-plus", MUTED, false)
                    .size(px(26.))
                    .tooltip(|w, cx| Tooltip::new("New channel").build(w, cx))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_channel_editor(con, None, 0, String::new(), window, cx)
                    })),
            )
    }

    fn tree(&self, s: &ServerSnap, cx: &mut Context<Self>) -> impl IntoElement {
        let selected = self.model.read(cx).selected;
        let mut col = v_flex().px_3().py_3().gap_px();
        if let Some((path, link)) = s.banner.clone() {
            col = col.child(
                div()
                    .id("banner")
                    .mb_3()
                    .rounded(px(6.))
                    .overflow_hidden()
                    .when(!link.is_empty(), |d| d.cursor_pointer().on_click(move |_, _, cx| cx.open_url(&link)))
                    .child(gpui_kit::img(path).w_full().max_h(px(120.)).object_fit(gpui_kit::ObjectFit::Contain)),
            );
        }
        if !s.welcome.trim().is_empty() {
            col = col.child(
                div()
                    .mb_3()
                    .px_3()
                    .py_2()
                    .rounded(px(6.))
                    .bg(rgb(PANEL))
                    .border_1()
                    .border_color(border())
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child(s.welcome.trim().to_string()),
            );
        }
        let items = self.visible_rows(s, cx);
        if self.filtering && items.is_empty() && !self.filter.read(cx).value().trim().is_empty() {
            col = col.child(div().px_2().py_2().child(label("Nothing matches.")));
        }
        for item in items {
            match item {
                TreeItem::Channel { index, has_children, folded } => {
                    let ch = &s.channels[index];
                    col = col.child(self.channel_row(s, ch, s.own_channel == Some(ch.id), has_children, folded, cx));
                }
                TreeItem::Client { channel, client } => {
                    let ch = &s.channels[channel];
                    let cl = &ch.clients[client];
                    let is_sel = selected == Some((s.con, cl.id));
                    let data = UserMenuData {
                        con: s.con,
                        row: cl.clone(),
                        own_channel: s.own_channel,
                        their_channel: ch.id,
                        groups: s.groups.clone(),
                        channel_groups: s.channel_groups.clone(),
                    };
                    col = col.child(self.client_row(data, ch.id, ch.depth, is_sel, cx));
                }
            }
        }
        div().id("tree").flex_1().overflow_y_scroll().child(col)
    }

    /// The rows the tree shows, in order (collapsed channels and the filter
    /// taken into account).
    fn visible_rows(&self, s: &ServerSnap, cx: &gpui_kit::App) -> Vec<TreeItem> {
        let collapsed = &self.model.read(cx).settings.collapsed;
        let is_collapsed = |id: u64| collapsed.contains(&crate::config::channel_key(&s.bookmark, id));
        let q = if self.filtering { self.filter.read(cx).value().trim().to_lowercase() } else { String::new() };
        let hit = |text: &str| text.to_lowercase().contains(&q);
        let mut out = Vec::new();
        let mut hide_below: Option<usize> = None;
        for (i, ch) in s.channels.iter().enumerate() {
            // with a filter: channels that match, hold a match, or have one below
            if !q.is_empty() {
                let d = ch.depth;
                let shown = s.channels[i..]
                    .iter()
                    .enumerate()
                    .take_while(|(k, c)| *k == 0 || c.depth > d)
                    .any(|(_, c)| hit(&c.name) || c.clients.iter().any(|cl| hit(&cl.name)));
                if !shown {
                    continue;
                }
            }
            match hide_below {
                Some(d) if ch.depth > d => continue,
                _ => hide_below = None,
            }
            let has_children = !ch.clients.is_empty() || s.channels.get(i + 1).is_some_and(|n| n.depth > ch.depth);
            let folded = q.is_empty() && is_collapsed(ch.id);
            out.push(TreeItem::Channel { index: i, has_children, folded });
            if folded {
                hide_below = Some(ch.depth);
                continue;
            }
            let channel_hit = !q.is_empty() && hit(&ch.name);
            for (k, cl) in ch.clients.iter().enumerate() {
                if q.is_empty() || channel_hit || hit(&cl.name) {
                    out.push(TreeItem::Client { channel: i, client: k });
                }
            }
        }
        out
    }

    /// Arrow keys in the tree. Returns whether the key was used.
    fn tree_key(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(s) = self.model.read(cx).snap.active_server().cloned() else { return false };
        let items = self.visible_rows(&s, cx);
        let id_of = |item: &TreeItem| match *item {
            TreeItem::Channel { index, .. } => Cursor::Channel(s.channels[index].id),
            TreeItem::Client { channel, client } => Cursor::Client(s.channels[channel].clients[client].id),
        };
        let pos = self.cursor.and_then(|c| items.iter().position(|i| id_of(i) == c));
        match key {
            "down" | "up" => {
                let next = match (pos, key) {
                    (None, _) => 0,
                    (Some(p), "down") => (p + 1).min(items.len().saturating_sub(1)),
                    (Some(p), _) => p.saturating_sub(1),
                };
                self.cursor = items.get(next).map(id_of);
            }
            "enter" => match self.cursor {
                Some(Cursor::Channel(id)) => self.send(cx, Cmd::Join { con: s.con, channel: id, password: None }),
                Some(Cursor::Client(id)) => self.model.update(cx, |m, cx| m.select(Some((s.con, id)), cx)),
                None => return false,
            },
            "left" | "right" => {
                let Some(Cursor::Channel(id)) = self.cursor else { return false };
                let collapse = key == "left";
                let folded = crate::config::channel_key(&s.bookmark, id);
                self.model.update(cx, |m, cx| {
                    m.update_settings(cx, |st| {
                        if collapse {
                            st.collapsed.insert(folded);
                        } else {
                            st.collapsed.remove(&folded);
                        }
                    })
                });
            }
            _ => return false,
        }
        let _ = window;
        cx.notify();
        true
    }

    fn channel_row(
        &self,
        s: &ServerSnap,
        ch: &ChannelRow,
        here: bool,
        has_children: bool,
        folded: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let con = s.con;
        let id = ch.id;
        let key = crate::config::channel_key(&s.bookmark, id);
        let chosen = self.model.read(cx).selected_channel == Some((con, id));
        let cursored = self.cursor == Some(Cursor::Channel(id));
        let hint = self.drop_hint.filter(|(c, _)| *c == id).map(|(_, z)| z);
        let line = |top: bool| {
            div()
                .absolute()
                .left(px(8. + 16. * ch.depth as f32))
                .right_0()
                .h(px(2.))
                .rounded_full()
                .bg(rgb(ACCENT))
                .when(top, |d| d.top(px(-1.)))
                .when(!top, |d| d.bottom(px(-1.)))
        };
        let drag = DragChannel { con, id, name: ch.name.clone() };
        let (drop_channels, move_channels) = (s.channels.clone(), s.channels.clone());
        let count = ch.clients.len();
        let spacer = (ch.depth == 0).then(|| Spacer::parse(&ch.name)).flatten();
        let name = match &spacer {
            Some((_, text)) => text.clone(),
            None => ch.name.clone(),
        };
        let chevron = div()
            .id(("fold", id as usize))
            .w(px(14.))
            .flex_none()
            .flex()
            .justify_center()
            .when(has_children, |d| {
                d.child(icon(if folded { "chevron-right" } else { "chevron-down" }, 12., DIM))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        let key = key.clone();
                        this.model.update(cx, |m, cx| {
                            m.update_settings(cx, |s| {
                                if !s.collapsed.remove(&key) {
                                    s.collapsed.insert(key);
                                }
                            })
                        });
                    }))
            });
        h_flex()
            .id(("channel", id as usize))
            .relative()
            .h(px(28.))
            .gap_2()
            .items_center()
            .pl(px(2. + 16. * ch.depth as f32))
            .pr_2()
            .mt_0p5()
            .rounded(px(5.))
            .border_1()
            .border_color(rgba(0x00000000))
            .cursor_pointer()
            .when(here || chosen, |d| d.bg(active_bg()))
            .when(chosen, |d| d.border_color(rgba(0xffffff1f)))
            .when(cursored, |d| d.border_color(alpha(ACCENT, 0x99)))
            .hover(|d| d.bg(hover_bg()))
            .when(hint == Some(Zone::Into), |d| d.bg(alpha(ACCENT, 0x1a)).border_color(alpha(ACCENT, 0x80)))
            .when(hint == Some(Zone::Above), |d| d.child(line(true)))
            .when(hint == Some(Zone::Below), |d| d.child(line(false)))
            .tooltip(|w, cx| Tooltip::new("Double-click to join").build(w, cx))
            .on_drag(drag, |d, _, _, cx| cx.new(|_| DragGhost { icon: "hash", text: d.name.clone() }))
            .on_drag_move(cx.listener(move |this, e: &DragMoveEvent<DragChannel>, _, cx| {
                if !e.bounds.contains(&e.event.position) {
                    return;
                }
                let d = e.drag(cx);
                let y = (e.event.position.y - e.bounds.origin.y) / e.bounds.size.height;
                let zone = if y < 0.3 { Zone::Above } else if y > 0.7 { Zone::Below } else { Zone::Into };
                let ok = d.con == con && channel_drop(&move_channels, d.id, id, zone).is_some();
                this.set_hint(ok.then_some((id, zone)), cx);
            }))
            .on_drag_move(cx.listener(move |this, e: &DragMoveEvent<DragClient>, _, cx| {
                if e.bounds.contains(&e.event.position) {
                    let ok = e.drag(cx).con == con;
                    this.set_hint(ok.then_some((id, Zone::Into)), cx);
                }
            }))
            .on_drop(cx.listener(move |this, d: &DragChannel, window, cx| {
                let zone = this.drop_hint.filter(|(c, _)| *c == id).map(|(_, z)| z);
                this.set_hint(None, cx);
                if let (Some(zone), true) = (zone, d.con == con) {
                    if let Some((parent, after)) = channel_drop(&drop_channels, d.id, id, zone) {
                        // people inside move along: ask first
                        let people: usize = drop_channels
                            .iter()
                            .filter(|c| crate::net::within(&drop_channels, d.id, c.id))
                            .map(|c| c.clients.len())
                            .sum();
                        if people > 0 {
                            let kind = PromptKind::MoveChannel { parent, after };
                            this.open_prompt(kind, con, d.id, d.name.clone(), String::new(), window, cx);
                            if let Some(p) = &mut this.prompt {
                                p.key = people.to_string();
                            }
                        } else {
                            let action = AdminAction::MoveChannel { channel: d.id, parent, after };
                            this.send(cx, Cmd::Admin { con, action });
                        }
                    }
                }
            }))
            .on_drop(cx.listener(move |this, d: &DragClient, _, cx| {
                this.set_hint(None, cx);
                this.drop_client(d, con, id, cx);
            }))
            .on_click(cx.listener(move |this, e: &ClickEvent, _, cx| {
                if e.click_count() >= 2 {
                    this.send(cx, Cmd::Join { con, channel: id, password: None });
                } else {
                    this.model.update(cx, |m, cx| {
                        let target = if m.selected_channel == Some((con, id)) { None } else { Some((con, id)) };
                        m.select_channel(target, cx);
                    });
                }
            }))
            .child(chevron)
            .when(spacer.is_none(), |d| {
                d.child(icon(
                    if ch.has_password { "lock" } else if here { "volume-2" } else { "hash" },
                    14.,
                    if here { ACCENT } else { DIM },
                ))
            })
            .child(match spacer.map(|(kind, _)| kind) {
                None => div()
                    .flex_1()
                    .truncate()
                    .text_sm()
                    .text_color(rgb(if here { TEXT } else { 0xb3b8c0 }))
                    .when(here, |d| d.font_weight(FontWeight::MEDIUM))
                    .child(name),
                Some(kind) => {
                    let text = div().text_sm().text_color(rgb(DIM)).whitespace_nowrap();
                    let row = div().flex_1().min_w_0().overflow_hidden().flex();
                    match kind {
                        Spacer::Left => row.child(text.child(name)),
                        Spacer::Center => row.justify_center().child(text.child(name)),
                        Spacer::Right => row.justify_end().child(text.child(name)),
                        // the pattern repeated across the row
                        Spacer::Repeat => row.child(text.child(name.repeat(400 / name.chars().count().max(1)))),
                    }
                }
            })
            .when_some(ch.icon.as_ref().and_then(badge), |d, b| d.child(b))
            .when(ch.is_default, |d| d.child(icon("house", 12., DIM)))
            .when(count > 0, |d| {
                d.child(label(match ch.max_clients {
                    Some(max) => format!("{count}/{max}"),
                    None => count.to_string(),
                }))
            })
            .context_menu({
                let (ch, model, view) = (ch.clone(), self.model.clone(), cx.entity().downgrade());
                // names from the top level down, for whisper lists
                let mut names = vec![ch.name.clone()];
                let mut parent = ch.parent;
                while let Some(p) = s.channels.iter().find(|c| c.id == parent) {
                    names.push(p.name.clone());
                    parent = p.parent;
                }
                names.reverse();
                let path = names.join("/");
                move |menu, window, cx| channel_menu(menu, con, &ch, path.clone(), here, &model, &view, window, cx)
            })
    }

    /// A user dropped on a channel: join it ourselves, or move them there.
    fn drop_client(&mut self, d: &DragClient, con: ConId, channel: u64, cx: &mut Context<Self>) {
        if d.con != con {
            return;
        }
        if d.me {
            self.send(cx, Cmd::Join { con, channel, password: None });
        } else {
            self.send(cx, Cmd::Admin { con, action: AdminAction::Move { client: d.id, channel } });
        }
    }

    fn client_row(
        &self,
        data: UserMenuData,
        channel: u64,
        depth: usize,
        selected: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let con = data.con;
        let cl = data.row.clone();
        let id = cl.id;
        let drag = DragClient { con, id, name: cl.name.clone(), me: cl.me };
        let cursored = self.cursor == Some(Cursor::Client(id));
        let model = self.model.clone();
        let menu_model = self.model.clone();
        let view = cx.entity().downgrade();
        let mut badges = h_flex().gap_1().items_center();
        for b in &cl.groups {
            if let Some(el) = badge(b) {
                let name = b.name.clone();
                badges = badges.child(
                    div()
                        .id(SharedString::from(format!("b-{con}-{id}-{name}")))
                        .tooltip(move |w, cx| Tooltip::new(name.clone()).build(w, cx))
                        .child(el),
                );
            }
        }
        let status = |name: &'static str, color: u32| icon(name, 13., color);
        h_flex()
            .id(SharedString::from(format!("cl-{con}-{id}")))
            .h(px(28.))
            .gap_2()
            .items_center()
            .pl(px(28. + 16. * depth as f32))
            .pr_2()
            .rounded(px(5.))
            .cursor_pointer()
            .when(selected, |d| d.bg(active_bg()))
            .border_1()
            .border_color(if cursored { alpha(ACCENT, 0x99) } else { rgba(0x00000000) })
            .hover(|d| d.bg(hover_bg()))
            .on_drag(drag, |d, _, _, cx| cx.new(|_| DragGhost { icon: "user", text: d.name.clone() }))
            // dropping on someone means their channel
            .on_drag_move(cx.listener(move |this, e: &DragMoveEvent<DragClient>, _, cx| {
                if e.bounds.contains(&e.event.position) {
                    let ok = e.drag(cx).con == con;
                    this.set_hint(ok.then_some((channel, Zone::Into)), cx);
                }
            }))
            .on_drop(cx.listener(move |this, d: &DragClient, _, cx| {
                this.set_hint(None, cx);
                this.drop_client(d, con, channel, cx);
            }))
            .on_click(move |_, _, cx| {
                model.update(cx, |m, cx| {
                    let target = if m.selected == Some((con, id)) { None } else { Some((con, id)) };
                    m.select(target, cx);
                })
            })
            .child(avatar(&cl.name, cl.avatar.as_deref(), 20., cl.talking))
            .child(
                div()
                    .truncate()
                    .text_sm()
                    .text_color(rgb(if cl.whispering {
                        ACCENT
                    } else if cl.talking {
                        GREEN
                    } else if cl.away.is_some() {
                        DIM
                    } else {
                        TEXT
                    }))
                    .when(cl.me, |d| d.font_weight(FontWeight::SEMIBOLD))
                    .child(cl.name.clone()),
            )
            .child(badges)
            .when_some(cl.icon.as_ref().and_then(badge), |d, b| d.child(b))
            .when(cl.friend, |d| d.child(status("star", AMBER)))
            .when(cl.blocked, |d| d.child(status("ban", DIM)))
            .child(div().flex_1())
            .when(cl.whispering, |d| {
                d.child(
                    div()
                        .id(SharedString::from(format!("wh-{con}-{id}")))
                        .tooltip(|w, cx| Tooltip::new("Whispering to you").build(w, cx))
                        .child(status("radio", ACCENT)),
                )
            })
            .when(cl.commander, |d| d.child(status("megaphone", MUTED)))
            .when(cl.priority, |d| d.child(status("zap", MUTED)))
            .when(cl.recording, |d| d.child(status("circle-dot", RED)))
            .when(cl.away.is_some(), |d| d.child(status("moon", DIM)))
            .when_some(cl.talk_request.clone(), |d, msg| {
                let tip = if msg.is_empty() { "Asks for talk power".to_string() } else { format!("Asks for talk power: {msg}") };
                d.child(
                    div()
                        .id(SharedString::from(format!("tr-{con}-{id}")))
                        .tooltip(move |w, cx| Tooltip::new(tip.clone()).build(w, cx))
                        .child(status("hand", AMBER)),
                )
            })
            .when(cl.locally_muted, |d| d.child(status("volume-x", RED)))
            .when(!cl.locally_muted && (cl.volume - 1.0).abs() > 0.01, |d| {
                d.child(label(format!("{:.0}%", cl.volume * 100.)))
            })
            .when(cl.output_muted, |d| d.child(status("headphone-off", RED)))
            .when(cl.input_muted && !cl.output_muted, |d| d.child(status("mic-off", RED)))
            .context_menu(move |menu, window, cx| user_menu(menu, &data, &menu_model, &view, window, cx))
    }

    fn talk_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let snap = &self.model.read(cx).snap;
        let mode = snap.voice_mode.unwrap_or_default();
        let (talking, muted, ptt) = (snap.transmitting, snap.self_muted, snap.ptt);
        let level = ((snap.mic_level_db + 60.) / 60.).clamp(0., 1.);
        let threshold = ((self.model.read(cx).settings.vad_threshold_db + 60.) / 60.).clamp(0., 1.);
        let whispering = snap.whispering.clone();
        let status = match (mode, muted, talking, &whispering) {
            (_, true, ..) => "Microphone muted".to_string(),
            (_, _, true, Some(w)) => format!("Whispering to {w}"),
            (_, _, true, None) => "Talking".into(),
            (VoiceMode::PushToTalk, ..) => "Push-to-talk".into(),
            (VoiceMode::VoiceActivation, ..) => "Voice activation".into(),
        };
        let talk_color = if whispering.is_some() { ACCENT } else { GREEN };
        footer_row()
            .px_4()
            .child(icon(
                if muted { "mic-off" } else { "mic" },
                16.,
                if muted { RED } else if talking { talk_color } else { MUTED },
            ))
            .child(
                v_flex()
                    .flex_1()
                    .gap_1p5()
                    .child(label(status).when(talking, |d| d.text_color(rgb(talk_color))).when(muted, |d| d.text_color(rgb(RED))))
                    .child(
                        div()
                            .h(px(3.))
                            .w_full()
                            .rounded_full()
                            .overflow_hidden()
                            .relative()
                            .child(segments(level, if talking { talk_color } else { DIM }))
                            .when(mode == VoiceMode::VoiceActivation, |d| {
                                d.child(
                                    div()
                                        .absolute()
                                        .top_0()
                                        .left(gpui_kit::relative(threshold))
                                        .w(px(2.))
                                        .h_full()
                                        .bg(rgb(MUTED)),
                                )
                            }),
                    ),
            )
            .child({
                let rec = snap.recording.clone();
                let on = rec.is_some();
                icon_button("record", "circle-dot", if on { RED } else { MUTED }, on)
                    .tooltip(move |w, cx| {
                        let text = match &rec {
                            Some(p) => format!("Recording to {}", p.display()),
                            None => "Record what you hear and say".into(),
                        };
                        Tooltip::new(text).build(w, cx)
                    })
                    .on_click(cx.listener(move |this, _, _, cx| this.send(cx, Cmd::Record(Some(!on)))))
            })
            .when(mode == VoiceMode::PushToTalk, |d| {
                d.child(
                    div()
                        .id("ptt")
                        .h(px(30.))
                        .px_4()
                        .flex()
                        .items_center()
                        .rounded(px(5.))
                        .cursor_pointer()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .border_1()
                        .border_color(if talking { alpha(GREEN, 0x80) } else { border() })
                        .bg(if talking { alpha(GREEN, 0x1a) } else { rgb(RAISED) })
                        .text_color(rgb(if talking { GREEN } else { TEXT }))
                        .hover(|d| d.bg(rgba(0xffffff14)))
                        .on_click(cx.listener(move |this, _, _, cx| this.send(cx, Cmd::Ptt(!ptt))))
                        .child(if talking { "Talking" } else { "Talk" }),
                )
            })
    }

    // ---- right panel ---------------------------------------------------

    fn right(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let m = self.model.read(cx);
        let details = m.selected.and_then(|(con, id)| {
            let s = m.snap.server(con)?;
            let row = s.channels.iter().flat_map(|c| &c.clients).find(|c| c.id == id)?.clone();
            Some((con, row, s.details.clone().filter(|d| d.id == id)))
        });
        let channel = m.selected_channel.and_then(|(con, id)| {
            let s = m.snap.server(con)?;
            Some((s.clone(), s.channels.iter().find(|c| c.id == id)?.clone()))
        });
        let panel = v_flex().w(px(RIGHT_W)).flex_none().h_full().bg(rgb(PANEL)).border_l_1().border_color(border());
        match (details, channel) {
            (Some((con, row, d)), _) => panel.child(self.user_panel(con, row, d, window, cx)).into_any_element(),
            (None, Some((s, ch))) => panel.child(self.channel_panel(&s, ch, cx)).into_any_element(),
            (None, None) => panel.child(self.chat_panel(cx)).into_any_element(),
        }
    }

    fn ensure_slider(&mut self, uid: &str, volume: f32, cx: &mut Context<Self>) -> Entity<SliderState> {
        if let Some((u, s)) = &self.volume {
            if u == uid {
                return s.clone();
            }
        }
        let slider = cx.new(|_| SliderState::new().min(0.).max(200.).step(5.).default_value(volume * 100.));
        let key = uid.to_string();
        cx.subscribe(&slider, move |this, _, ev: &SliderEvent, cx| {
            let (SliderEvent::Change(v) | SliderEvent::Release(v)) = ev;
            let v = v.start() / 100.;
            let key = key.clone();
            this.model.update(cx, |m, cx| {
                m.update_settings(cx, |s| {
                    if (v - 1.0).abs() < 0.01 {
                        s.user_volumes.remove(&key);
                    } else {
                        s.user_volumes.insert(key, v);
                    }
                })
            });
        })
        .detach();
        self.volume = Some((uid.to_string(), slider.clone()));
        slider
    }

    fn user_panel(
        &mut self,
        con: ConId,
        row: crate::net::ClientRow,
        d: Option<ClientDetails>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let slider = self.ensure_slider(&row.uid, row.volume, cx);
        let model = self.model.clone();
        let uid = row.uid.clone();
        let muted = row.locally_muted;
        let stat = |name: &'static str, value: String, color: u32| {
            h_flex()
                .justify_between()
                .py_1p5()
                .child(label(name))
                .child(div().text_xs().text_color(rgb(color)).child(value))
        };
        let mut stats = v_flex();
        if let Some(d) = &d {
            if let Some(p) = d.ping_ms {
                stats = stats.child(stat("Ping", format!("{p:.0} ms"), if ping_color(p) == MUTED { TEXT } else { ping_color(p) }));
            }
            if let Some(l) = d.packet_loss {
                stats = stats.child(stat("Packet loss", format!("{l:.1}%"), if l < 1. { TEXT } else { AMBER }));
            }
            if let Some(c) = d.connected_secs {
                stats = stats.child(stat("Connected", fmt_duration(c), TEXT));
            }
            stats = stats.child(stat("Idle", fmt_duration(d.idle_secs), TEXT));
            if !d.version.is_empty() {
                stats = stats.child(stat("Client", d.version.split(' ').next().unwrap_or("").to_string(), TEXT));
            }
            if !d.platform.is_empty() {
                stats = stats.child(stat("Platform", d.platform.clone(), TEXT));
            }
        } else {
            stats = stats.child(label("Loading details..."));
        }
        if !row.country.is_empty() {
            stats = stats.child(stat("Country", row.country.clone(), TEXT));
        }
        let mut groups = v_flex().gap_2();
        for g in &row.groups {
            groups = groups.child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .when_some(badge(g), |d, b| d.child(b))
                    .child(div().text_sm().child(g.name.clone())),
            );
        }
        let description = d.as_ref().map(|d| d.description.clone()).filter(|x| !x.is_empty());
        v_flex()
            .size_full()
            .child(
                header_row()
                    .child(div().flex_1().text_sm().font_weight(FontWeight::SEMIBOLD).child("Details"))
                    .child(
                        icon_button("close-user", "x", MUTED, false)
                            .on_click(cx.listener(|this, _, _, cx| this.model.update(cx, |m, cx| m.select(None, cx)))),
                    ),
            )
            .child(
                v_flex()
                    .id("user-panel")
                    .flex_1()
                    .overflow_y_scroll()
                    .p_4()
                    .gap_5()
                    .child(
                        h_flex()
                            .gap_3()
                            .items_center()
                            .child(avatar(&row.name, row.avatar.as_deref(), 44., row.talking))
                            .child(
                                v_flex()
                                    .min_w_0()
                                    .child(div().font_weight(FontWeight::SEMIBOLD).truncate().child(row.name.clone()))
                                    .child(
                                        label(match (&row.away, row.talking) {
                                            (_, true) => "Talking".to_string(),
                                            (Some(a), _) if !a.is_empty() => format!("Away: {a}"),
                                            (Some(_), _) => "Away".into(),
                                            _ => "Online".into(),
                                        })
                                        .when(row.talking, |d| d.text_color(rgb(GREEN))),
                                    ),
                            ),
                    )
                    .when_some(description, |el, desc| el.child(div().text_sm().text_color(rgb(MUTED)).child(desc)))
                    .when(!row.me, |el| {
                        el.child(
                            v_flex()
                                .gap_3()
                                .child(section_title("Volume"))
                                .child(
                                    h_flex()
                                        .gap_3()
                                        .items_center()
                                        .child(div().flex_1().child(Slider::new(&slider)))
                                        .child(
                                            div()
                                                .w(px(40.))
                                                .text_xs()
                                                .text_color(rgb(MUTED))
                                                .text_right()
                                                .child(format!("{:.0}%", row.volume * 100.)),
                                        ),
                                )
                                .child(
                                    div()
                                        .id("local-mute")
                                        .h(px(30.))
                                        .rounded(px(5.))
                                        .cursor_pointer()
                                        .flex()
                                        .gap_2()
                                        .items_center()
                                        .justify_center()
                                        .text_sm()
                                        .border_1()
                                        .border_color(if muted { alpha(RED, 0x80) } else { border() })
                                        .bg(if muted { alpha(RED, 0x14) } else { rgb(RAISED) })
                                        .text_color(rgb(if muted { RED } else { TEXT }))
                                        .hover(|d| d.bg(rgba(0xffffff14)))
                                        .on_click(move |_, _, cx| {
                                            let uid = uid.clone();
                                            model.update(cx, |m, cx| {
                                                m.update_settings(cx, |s| {
                                                    if !s.user_muted.remove(&uid) {
                                                        s.user_muted.insert(uid);
                                                    }
                                                })
                                            });
                                        })
                                        .child(icon(if muted { "volume-x" } else { "volume-2" }, 14., if muted { RED } else { MUTED }))
                                        .child(if muted { "Unmute for me" } else { "Mute for me" }),
                                ),
                        )
                    })
                    .when(!row.groups.is_empty(), |el| el.child(v_flex().gap_3().child(section_title("Roles")).child(groups)))
                    .child(v_flex().gap_2().child(section_title("Connection")).child(stats))
                    .child(label(format!("Server {con} · client {}", row.id)).text_color(rgb(DIM))),
            )
    }

    fn channel_panel(&self, s: &ServerSnap, ch: ChannelRow, cx: &mut Context<Self>) -> impl IntoElement {
        let con = s.con;
        let id = ch.id;
        let here = s.own_channel == Some(id);
        let stat = |name: &'static str, value: String| {
            h_flex()
                .justify_between()
                .py_1p5()
                .child(label(name))
                .child(div().text_xs().text_color(rgb(TEXT)).child(value))
        };
        let limit = |m: MaxClients, inherited: &str| match m {
            MaxClients::Unlimited => "Unlimited".to_string(),
            MaxClients::Inherited => inherited.to_string(),
            MaxClients::Limited(n) => n.to_string(),
        };
        let kind = match ch.kind {
            ChannelType::Permanent => "Permanent",
            ChannelType::SemiPermanent => "Semi-permanent",
            ChannelType::Temporary => "Temporary",
        };
        let codec = match ch.codec {
            Codec::OpusVoice => "Opus Voice".to_string(),
            Codec::OpusMusic => "Opus Music".to_string(),
            c => format!("{c:?}"),
        };
        let mut info = v_flex()
            .child(stat("Type", if ch.is_default { format!("{kind}, default") } else { kind.to_string() }))
            .child(stat("Codec", format!("{codec}, quality {}", ch.quality)))
            .child(stat("Clients", format!("{} / {}", ch.clients.len(), limit(ch.limit, "Inherited"))))
            .child(stat("Family limit", limit(ch.family_limit, "Inherited")));
        if ch.talk_power > 0 {
            info = info.child(stat("Talk power needed", ch.talk_power.to_string()));
        }
        if ch.has_password {
            info = info.child(stat("Password", "Yes".into()));
        }
        if ch.kind == ChannelType::Temporary && ch.delete_delay > 0 {
            info = info.child(stat("Delete delay", fmt_duration(ch.delete_delay)));
        }
        let description = match &ch.description {
            None => Some(label("Loading description...").into_any_element()),
            Some(d) => {
                let rich = super::bbcode::parse(d.trim());
                (!rich.is_empty()).then(|| {
                    div().text_sm().text_color(rgb(0xc4c8ce)).child(rich.element(("ch-desc", id as usize))).into_any_element()
                })
            }
        };
        let mut people = v_flex().gap_1();
        for cl in &ch.clients {
            let cid = cl.id;
            people = people.child(
                h_flex()
                    .id(SharedString::from(format!("chp-{cid}")))
                    .gap_2()
                    .items_center()
                    .px_1()
                    .py_1()
                    .rounded(px(5.))
                    .cursor_pointer()
                    .hover(|d| d.bg(hover_bg()))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.model.update(cx, |m, cx| m.select(Some((con, cid)), cx))
                    }))
                    .child(avatar(&cl.name, cl.avatar.as_deref(), 20., cl.talking))
                    .child(div().text_sm().truncate().child(cl.name.clone())),
            );
        }
        let original = ch.clone();
        let button = |id: &'static str, text: &'static str, icon_name: &'static str| {
            div()
                .id(id)
                .flex_1()
                .h(px(30.))
                .rounded(px(5.))
                .cursor_pointer()
                .flex()
                .gap_2()
                .items_center()
                .justify_center()
                .text_sm()
                .border_1()
                .border_color(border())
                .bg(rgb(RAISED))
                .hover(|d| d.bg(rgba(0xffffff14)))
                .child(icon(icon_name, 14., MUTED))
                .child(text)
        };
        v_flex()
            .size_full()
            .child(
                header_row()
                    .child(div().flex_1().text_sm().font_weight(FontWeight::SEMIBOLD).child("Channel"))
                    .child(icon_button("close-channel", "x", MUTED, false).on_click(
                        cx.listener(|this, _, _, cx| this.model.update(cx, |m, cx| m.select_channel(None, cx))),
                    )),
            )
            .child(
                v_flex()
                    .id("channel-panel")
                    .flex_1()
                    .overflow_y_scroll()
                    .p_4()
                    .gap_5()
                    .child(
                        v_flex()
                            .gap_1()
                            .child(div().font_weight(FontWeight::SEMIBOLD).child(ch.name.clone()))
                            .when(!ch.topic.is_empty(), |d| d.child(label(ch.topic.clone()))),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .when(!here, |d| {
                                d.child(button("ch-join", "Join", "log-in").on_click(cx.listener(move |this, _, _, cx| {
                                    this.send(cx, Cmd::Join { con, channel: id, password: None })
                                })))
                            })
                            .child(button("ch-files", "Files", "folder").on_click({
                                let (model, name) = (self.model.clone(), ch.name.clone());
                                move |_, _, cx| super::open_admin(&model, con, super::AdminTab::Files(id, name.clone()), cx)
                            }))
                            .child(button("ch-edit", "Edit", "settings-2").on_click(cx.listener(
                                move |this, _, window, cx| {
                                    this.open_channel_editor(con, Some(original.clone()), id, String::new(), window, cx)
                                },
                            ))),
                    )
                    .when_some(description, |d, desc| d.child(v_flex().gap_2().child(section_title("Description")).child(desc)))
                    .child(v_flex().gap_2().child(section_title("Details")).child(info))
                    .when(!ch.clients.is_empty(), |d| {
                        d.child(v_flex().gap_2().child(section_title("In this channel")).child(people))
                    }),
            )
    }
}

impl Render for MainView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let audio_error = self.model.read(cx).snap.audio_error.clone();
        let hint = self.chat_placeholder(cx);
        if hint != self.chat_hint {
            self.chat_hint = hint.clone();
            self.chat.update(cx, |i, cx| i.set_placeholder(hint, window, cx));
        }
        if self.drop_hint.is_some() && !cx.has_active_drag() {
            self.drop_hint = None;
        }
        let overlay = self.prompt_overlay(cx).or_else(|| self.host_message(cx)).or_else(|| self.palette_overlay(cx));
        let editor = self.editor.clone().map(|e| {
            div()
                .absolute()
                .inset_0()
                .bg(rgba(0x00000099))
                .flex()
                .items_center()
                .justify_center()
                .occlude()
                .child(e)
        });
        h_flex()
            .relative()
            .size_full()
            .bg(rgb(BG))
            .text_color(rgb(TEXT))
            .font_family(FONT)
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| this.on_key(ev, window, cx)))
            .child(self.sidebar(cx))
            .child(
                v_flex()
                    .flex_1()
                    .h_full()
                    .min_w_0()
                    .when_some(audio_error, |d, e| {
                        d.child(div().px_4().py_2().bg(alpha(RED, 0x1a)).text_xs().text_color(rgb(RED)).child(e))
                    })
                    .child(self.center(cx)),
            )
            .child(self.right(window, cx))
            .when_some(overlay, |d, o| d.child(o))
            .when_some(editor, |d, e| d.child(e))
    }
}
