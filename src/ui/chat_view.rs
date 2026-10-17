//! Right panel when nobody is selected: Activity plus chat tabs (channel,
//! server, one per private conversation), with unread counts.

use gpui_kit::component::input::Input;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{
    AnyElement, Context, FontWeight, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, Window, div, px, rgb,
};

use super::main_view::MainView;
use super::widgets::*;
use crate::net::{ChatKind, Cmd, ConId, Conversation, ServerSnap};

#[derive(Clone, Debug, Default, PartialEq)]
pub enum Tab {
    #[default]
    Activity,
    Chat(ChatKind),
}

/// Unread lines of a conversation and whether one of them mentions us.
pub fn unread(conv: &Conversation, seen: u64) -> (u64, bool) {
    let new = conv.received.saturating_sub(seen) as usize;
    let fresh = conv.lines.iter().rev().take(new).filter(|l| !l.me);
    fresh.fold((0, false), |(n, m), l| (n + 1, m || l.mention))
}

pub(super) fn pill(n: u64, mention: bool) -> impl IntoElement {
    div()
        .min_w(px(16.))
        .h(px(16.))
        .px_1()
        .rounded_full()
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(10.))
        .font_weight(FontWeight::SEMIBOLD)
        .bg(if mention { rgb(ACCENT) } else { rgb(0x2a2d33) })
        .text_color(rgb(if mention { BG } else { TEXT }))
        .child(if n > 99 { "99+".to_string() } else { n.to_string() })
}

impl MainView {
    /// The conversation the current tab shows, on the active server.
    fn current_conversation<'a>(&self, server: Option<&'a ServerSnap>) -> Option<&'a Conversation> {
        let Tab::Chat(kind) = &self.tab else { return None };
        server?.chats.iter().find(|c| c.kind == *kind)
    }

    /// Total unread for a server (sidebar badge).
    pub(super) fn server_unread(&self, s: &ServerSnap) -> (u64, bool) {
        s.chats.iter().fold((0, false), |(n, m), c| {
            let (cn, cm) = unread(c, self.seen.get(&(s.con, c.kind.clone())).copied().unwrap_or(0));
            (n + cn, m || cm)
        })
    }

    /// Mark the visible conversation read and keep it scrolled to the end.
    pub(super) fn chat_seen(&mut self, cx: &mut Context<Self>) {
        let m = self.model.read(cx);
        if m.selected.is_some() || m.selected_channel.is_some() {
            return;
        }
        let server = m.snap.active_server();
        let Some(conv) = self.current_conversation(server) else { return };
        let key = (server.map(|s| s.con).unwrap_or(0), conv.kind.clone());
        let received = conv.received;
        let len = conv.lines.len();
        if self.seen.get(&key) != Some(&received) {
            self.seen.insert(key, received);
            self.chat_scroll.scroll_to_bottom();
        } else if self.shown_len != len {
            self.chat_scroll.scroll_to_bottom();
        }
        self.shown_len = len;
    }

    pub(super) fn set_tab(&mut self, tab: Tab, cx: &mut Context<Self>) {
        self.tab = tab;
        self.shown_len = usize::MAX;
        self.chat_scroll.scroll_to_bottom();
        self.chat_seen(cx);
        cx.notify();
    }

    /// "Send message" on a user: open (or switch to) the private tab.
    pub(super) fn open_private(&mut self, con: ConId, client: u16, uid: String, window: &mut Window, cx: &mut Context<Self>) {
        self.send(cx, Cmd::OpenChat { con, client });
        self.send(cx, Cmd::SetActive(con));
        self.model.update(cx, |m, cx| {
            m.select(None, cx);
            m.select_channel(None, cx);
        });
        self.set_tab(Tab::Chat(ChatKind::Private(uid)), cx);
        self.chat.update(cx, |i, cx| i.focus(window, cx));
    }

    pub(super) fn chat_placeholder(&self, cx: &Context<Self>) -> String {
        let server = self.model.read(cx).snap.active_server();
        match (&self.tab, self.current_conversation(server)) {
            (Tab::Chat(ChatKind::Server), _) => "Message the server".into(),
            (Tab::Chat(ChatKind::Private(_)), Some(c)) if c.client.is_none() => format!("{} is offline", c.title),
            (Tab::Chat(ChatKind::Private(_)), Some(c)) => format!("Message {}", c.title),
            _ => "Message your channel".into(),
        }
    }

    /// Enter in the chat input.
    pub(super) fn send_chat(&mut self, text: String, cx: &mut Context<Self>) -> bool {
        let Some(con) = self.model.read(cx).snap.active else { return false };
        let to = match &self.tab {
            Tab::Activity => ChatKind::Channel,
            Tab::Chat(k) => k.clone(),
        };
        if text.is_empty() {
            return false;
        }
        self.send(cx, Cmd::SendText { con, to, text });
        true
    }

    fn tab_button(
        &self,
        id: impl Into<SharedString>,
        title: String,
        on: bool,
        badge: (u64, bool),
        closable: Option<(ConId, ChatKind)>,
        tab: Tab,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        h_flex()
            .id(id.into())
            .flex_none()
            .h(px(28.))
            .px_2p5()
            .gap_1p5()
            .items_center()
            .rounded(px(5.))
            .cursor_pointer()
            .text_sm()
            .text_color(rgb(if on { TEXT } else { MUTED }))
            .when(on, |d| d.bg(active_bg()))
            .hover(|d| d.bg(hover_bg()))
            .on_click(cx.listener(move |this, _, _, cx| this.set_tab(tab.clone(), cx)))
            .child(div().max_w(px(110.)).truncate().child(title))
            .when(badge.0 > 0, |d| d.child(pill(badge.0, badge.1)))
            .when_some(closable, |d, (con, kind)| {
                d.child(
                    div()
                        .id("close-tab")
                        .rounded(px(3.))
                        .hover(|d| d.bg(hover_bg()))
                        .child(icon("x", 12., DIM))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.send(cx, Cmd::CloseChat { con, kind: kind.clone() });
                            this.set_tab(Tab::Activity, cx);
                        })),
                )
            })
    }

    fn tab_strip(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let server = self.model.read(cx).snap.active_server().cloned();
        let mut strip = h_flex()
            .id("chat-tabs")
            .flex_1()
            .min_w_0()
            .gap_1()
            .overflow_x_scroll()
            .child(self.tab_button("tab-activity", "Activity".into(), self.tab == Tab::Activity, (0, false), None, Tab::Activity, cx));
        if let Some(s) = &server {
            for (i, c) in s.chats.iter().enumerate() {
                let tab = Tab::Chat(c.kind.clone());
                let on = self.tab == tab;
                let seen = self.seen.get(&(s.con, c.kind.clone())).copied().unwrap_or(0);
                let badge = if on { (0, false) } else { unread(c, seen) };
                let closable = matches!(c.kind, ChatKind::Private(_)).then(|| (s.con, c.kind.clone()));
                strip = strip.child(self.tab_button(format!("tab-{i}"), c.title.clone(), on, badge, closable, tab, cx));
            }
        }
        strip
    }

    fn chat_lines(&self, conv: &Conversation) -> AnyElement {
        let mut col = v_flex().gap_2().p_4();
        if conv.lines.is_empty() {
            col = col.child(label(match conv.kind {
                ChatKind::Channel => "Nothing said in your channel yet.",
                ChatKind::Server => "Nothing said on the server yet.",
                ChatKind::Private(_) => "No messages yet.",
            }));
        }
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        let mut last_date = String::new();
        let mut last_from: Option<(String, String)> = None;
        for (i, l) in conv.lines.iter().enumerate() {
            if l.date != last_date {
                last_date = l.date.clone();
                last_from = None;
                let day = if l.date == today { "Today".to_string() } else { l.date.clone() };
                col = col.child(
                    h_flex()
                        .gap_3()
                        .items_center()
                        .py_1()
                        .child(div().flex_1().h(px(1.)).bg(border()))
                        .child(div().text_size(px(11.)).text_color(rgb(DIM)).child(day))
                        .child(div().flex_1().h(px(1.)).bg(border())),
                );
            }
            // consecutive lines from the same person share one name row
            let same = last_from.as_ref() == Some(&(l.uid.clone(), l.time.clone()));
            last_from = Some((l.uid.clone(), l.time.clone()));
            let rich = super::bbcode::parse(&l.text);
            let body = div()
                .text_sm()
                .text_color(rgb(if l.old { 0xb3b8c0 } else { TEXT }))
                .child(rich.element(SharedString::from(format!("line-{i}"))));
            col = col.child(
                v_flex()
                    .gap_0p5()
                    .when(l.mention, |d| d.pl_2().border_l_2().border_color(rgb(ACCENT)))
                    .when(!same, |d| {
                        d.child(
                            h_flex()
                                .gap_2()
                                .items_baseline()
                                .child(
                                    div()
                                        .text_xs()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(rgb(if l.me { MUTED } else { TEXT }))
                                        .child(l.from.clone()),
                                )
                                .child(div().text_size(px(11.)).text_color(rgb(DIM)).child(l.time.clone())),
                        )
                    })
                    .child(body),
            );
        }
        col.into_any_element()
    }

    fn activity_feed(&self, cx: &Context<Self>) -> AnyElement {
        let snap = &self.model.read(cx).snap;
        let mut feed = v_flex().gap_2p5().p_4();
        if snap.activity.is_empty() {
            feed = feed.child(label("Joins, leaves, moves and pokes show up here."));
        }
        let multi = snap.servers.len() > 1;
        for a in &snap.activity {
            feed = feed.child(
                h_flex()
                    .gap_3()
                    .items_start()
                    .child(div().w(px(34.)).flex_none().text_xs().text_color(rgb(DIM)).child(a.time.clone()))
                    .child(
                        div()
                            .flex_1()
                            .text_xs()
                            .text_color(rgb(if a.chat { TEXT } else { MUTED }))
                            .child(if multi { format!("{} · {}", a.server, a.text) } else { a.text.clone() }),
                    ),
            );
        }
        feed.into_any_element()
    }

    pub(super) fn chat_panel(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let server = self.model.read(cx).snap.active_server().cloned();
        // a closed private tab or a server switch: fall back to Activity
        if let Tab::Chat(_) = &self.tab {
            if self.current_conversation(server.as_ref()).is_none() {
                self.tab = Tab::Activity;
            }
        }
        let body = match self.current_conversation(server.as_ref()) {
            Some(conv) => self.chat_lines(conv),
            None => self.activity_feed(cx),
        };
        let offline = matches!(self.current_conversation(server.as_ref()), Some(c) if matches!(c.kind, ChatKind::Private(_)) && c.client.is_none());
        v_flex()
            .size_full()
            .child(header_row().px_2().child(self.tab_strip(cx)))
            .child(div().id("feed").flex_1().overflow_y_scroll().track_scroll(&self.chat_scroll).child(body))
            .child(
                footer_row()
                    .child(div().flex_1().child(Input::new(&self.chat).disabled(offline)))
                    .when(offline, |d| {
                        d.child(
                            div()
                                .id("offline")
                                .tooltip(move |w, cx| Tooltip::new("They're offline").build(w, cx))
                                .child(icon("user-x", 14., DIM)),
                        )
                    }),
            )
    }
}
