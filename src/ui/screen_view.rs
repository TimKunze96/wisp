//! A stream in its own window, with its controls along the bottom.
//! Double-click the picture for fullscreen. Keys: F fullscreen, Escape out
//! of it, M mute, P keep on top.

use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::{
    Context, Entity, FocusHandle, FontWeight, InteractiveElement, IntoElement, KeyDownEvent, ParentElement, Render,
    StatefulInteractiveElement, Styled, Window, div, rgb,
};

use super::Model;
use super::colors::colors;
use super::streams::{Feed, quality_picker, sound_controls, status_line, tool_button};
use super::widgets::*;

pub struct ScreenView {
    model: Entity<Model>,
    feed: Entity<Feed>,
    /// Kept above other windows.
    pinned: bool,
    focus: FocusHandle,
}

impl ScreenView {
    pub fn new(model: Entity<Model>, feed: Entity<Feed>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.observe_in(&feed, window, |_, feed, window, cx| {
            window.set_window_title(&format!("{}'s screen", feed.read(cx).name));
            cx.notify();
        })
        .detach();
        window.set_window_title(&format!("{}'s screen", feed.read(cx).name));
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        Self { model, feed, pinned: false, focus }
    }

    fn toggle_pin(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if super::pin::AVAILABLE {
            self.pinned = !self.pinned;
            super::pin::set(window, self.pinned);
            cx.notify();
        }
    }

    fn on_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let k = &ev.keystroke;
        if k.modifiers.modified() {
            return;
        }
        match k.key.as_str() {
            "escape" if window.is_fullscreen() => window.toggle_fullscreen(),
            "f" | "f11" => window.toggle_fullscreen(),
            "m" => self.feed.update(cx, |f, cx| f.toggle_mute(cx)),
            "p" => self.toggle_pin(window, cx),
            _ => return,
        }
        cx.stop_propagation();
    }
}

impl ScreenView {
    fn content(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let f = self.feed.read(cx);
        let key = (f.con, f.client);
        let model = self.model.clone();
        let fullscreen = window.is_fullscreen();
        v_flex()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|t, ev: &KeyDownEvent, window, cx| t.on_key(ev, window, cx)))
            .size_full()
            .bg(rgb(0x000000))
            .text_color(rgb(colors().text))
            .font_family(FONT)
            .child(
                div()
                    .id("screen")
                    .flex_1()
                    .min_h_0()
                    .on_click(|e, window, _| {
                        if e.click_count() == 2 {
                            window.toggle_fullscreen();
                        }
                    })
                    .child(f.picture()),
            )
            .when(!fullscreen, |d| {
                d.child(
                    h_flex()
                        .flex_none()
                        .px_3()
                        .py_2()
                        .gap_3()
                        .items_center()
                        .bg(rgb(colors().bg))
                        .border_t_1()
                        .border_color(border())
                        .child(icon("monitor-up", 15., colors().accent))
                        .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child(f.name.clone()))
                        .child(div().text_xs().text_color(rgb(colors().muted)).child(status_line(f)))
                        .child(div().flex_1())
                        .children(sound_controls(&self.feed, cx))
                        .children(quality_picker(&self.feed, cx))
                        .when(super::pin::AVAILABLE, |d| {
                            let pinned = self.pinned;
                            d.child(
                                tool_button(
                                    "pin",
                                    if pinned { "pin-off" } else { "pin" },
                                    if pinned { "Stop keeping on top (P)" } else { "Keep on top of other windows (P)" },
                                )
                                .when(pinned, |b| b.bg(active_bg()))
                                .on_click(cx.listener(|t, _, window, cx| t.toggle_pin(window, cx))),
                            )
                        })
                        .child(tool_button("fullscreen", "maximize", "Fullscreen (F or double-click, Escape to leave)").on_click(
                            |_, window, _| window.toggle_fullscreen(),
                        ))
                        .child(tool_button("pop-in", "panel-bottom", "Back into the main window").on_click(
                            move |_, window, cx| {
                                model.update(cx, |m, cx| m.stage(Some(key), cx));
                                window.remove_window();
                                super::open_main(&model, cx);
                            },
                        )),
                )
            })
    }
}

use gpui_kit::prelude::FluentBuilder;

impl Render for ScreenView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let title = format!("{} · screen", self.feed.read(cx).name);
        let content = self.content(window, cx).into_any_element();
        super::frame::framed(title, content, window, cx)
    }
}
