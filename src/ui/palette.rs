//! Ctrl+K: jump to a server, join a channel or open someone's details by
//! typing part of the name.

use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{
    AnyElement, AppContext, Context, Entity, InteractiveElement, IntoElement, KeyDownEvent, ParentElement,
    StatefulInteractiveElement, Styled, Window, div, px, rgb, rgba,
};

use super::main_view::MainView;
use super::widgets::*;
use crate::net::{Cmd, ConId};

pub struct Palette {
    pub input: Entity<InputState>,
    pub selected: usize,
}

#[derive(Clone)]
enum Pick {
    /// Connect to / switch to a bookmark.
    Server(crate::config::Bookmark, Option<ConId>),
    Channel(ConId, u64),
    Person(ConId, u16),
}

struct Hit {
    icon: &'static str,
    title: String,
    detail: String,
    pick: Pick,
}

impl MainView {
    pub(super) fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Jump to a server, channel or person"));
        cx.subscribe_in(&input, window, |this, _, ev: &InputEvent, window, cx| match ev {
            InputEvent::Change => {
                if let Some(p) = &mut this.palette {
                    p.selected = 0;
                }
                cx.notify();
            }
            InputEvent::PressEnter { .. } => this.palette_pick(None, window, cx),
            _ => {}
        })
        .detach();
        input.update(cx, |i, cx| i.focus(window, cx));
        self.palette = Some(Palette { input, selected: 0 });
        cx.notify();
    }

    fn palette_hits(&self, cx: &Context<Self>) -> Vec<Hit> {
        let Some(p) = &self.palette else { return Vec::new() };
        let q = p.input.read(cx).value().trim().to_lowercase();
        let m = self.model.read(cx);
        let matches = |s: &str| q.is_empty() || s.to_lowercase().contains(&q);
        let mut hits = Vec::new();
        for b in &m.settings.bookmarks {
            let name = if b.label.is_empty() { b.address.clone() } else { b.label.clone() };
            let con = m.snap.servers.iter().find(|s| s.bookmark == b.id && !s.status.is_final()).map(|s| s.con);
            if matches(&name) {
                hits.push(Hit {
                    icon: "server",
                    title: name,
                    detail: if con.is_some() { "Switch to".into() } else { "Connect".into() },
                    pick: Pick::Server(b.clone(), con),
                });
            }
        }
        if !q.is_empty() {
            for s in &m.snap.servers {
                for ch in s.channels.iter().filter(|c| matches(&c.name) && !c.name.starts_with('[')) {
                    hits.push(Hit { icon: "hash", title: ch.name.clone(), detail: "Join".into(), pick: Pick::Channel(s.con, ch.id) });
                }
                for ch in &s.channels {
                    for cl in ch.clients.iter().filter(|c| matches(&c.name)) {
                        hits.push(Hit {
                            icon: "user",
                            title: cl.name.clone(),
                            detail: format!("in {}", ch.name),
                            pick: Pick::Person(s.con, cl.id),
                        });
                    }
                }
            }
        }
        hits.truncate(12);
        hits
    }

    fn palette_pick(&mut self, index: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        let hits = self.palette_hits(cx);
        let i = index.unwrap_or_else(|| self.palette.as_ref().map(|p| p.selected).unwrap_or(0));
        let Some(hit) = hits.into_iter().nth(i) else { return };
        self.palette = None;
        match hit.pick {
            Pick::Server(_, Some(con)) => self.send(cx, Cmd::SetActive(con)),
            Pick::Server(b, None) => self.send(cx, Cmd::Connect(b)),
            Pick::Channel(con, channel) => {
                self.send(cx, Cmd::SetActive(con));
                self.send(cx, Cmd::Join { con, channel, password: None });
            }
            Pick::Person(con, id) => {
                self.send(cx, Cmd::SetActive(con));
                self.model.update(cx, |m, cx| m.select(Some((con, id)), cx));
            }
        }
        let _ = window;
        cx.notify();
    }

    /// Up / Down in the palette.
    pub(super) fn palette_key(&mut self, ev: &KeyDownEvent, cx: &mut Context<Self>) -> bool {
        let n = self.palette_hits(cx).len();
        let Some(p) = &mut self.palette else { return false };
        match ev.keystroke.key.as_str() {
            "down" => p.selected = (p.selected + 1).min(n.saturating_sub(1)),
            "up" => p.selected = p.selected.saturating_sub(1),
            _ => return false,
        }
        cx.notify();
        true
    }

    pub(super) fn palette_overlay(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let p = self.palette.as_ref()?;
        let hits = self.palette_hits(cx);
        let mut list = v_flex().gap_0p5();
        if hits.is_empty() {
            list = list.child(div().px_3().py_2().child(label("Nothing matches.")));
        }
        for (i, h) in hits.iter().enumerate() {
            let on = i == p.selected;
            list = list.child(
                h_flex()
                    .id(("pal", i))
                    .gap_3()
                    .items_center()
                    .h(px(34.))
                    .px_3()
                    .rounded(px(5.))
                    .cursor_pointer()
                    .when(on, |d| d.bg(active_bg()))
                    .hover(|d| d.bg(hover_bg()))
                    .on_click(cx.listener(move |t, _, window, cx| t.palette_pick(Some(i), window, cx)))
                    .child(icon(h.icon, 14., if on { TEXT } else { MUTED }))
                    .child(div().flex_1().text_sm().truncate().child(h.title.clone()))
                    .child(label(h.detail.clone())),
            );
        }
        Some(
            div()
                .absolute()
                .inset_0()
                .bg(rgba(0x00000080))
                .flex()
                .justify_center()
                .pt(px(110.))
                .occlude()
                .on_mouse_down(gpui_kit::MouseButton::Left, cx.listener(|t, _, _, cx| {
                    t.palette = None;
                    cx.notify();
                }))
                .child(
                    v_flex()
                        .id("palette")
                        .w(px(520.))
                        .max_h(px(460.))
                        .gap_2()
                        .p_2()
                        .rounded(px(8.))
                        .bg(rgb(PANEL))
                        .border_1()
                        .border_color(border())
                        .shadow_lg()
                        .on_mouse_down(gpui_kit::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(Input::new(&p.input))
                        .child(list),
                )
                .into_any_element(),
        )
    }
}
