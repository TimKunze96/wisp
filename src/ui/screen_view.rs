//! A stream in its own window, with its controls along the bottom.
//! Double-click the picture for fullscreen.

use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::{
    Context, Entity, FontWeight, InteractiveElement, IntoElement, ParentElement, Render, StatefulInteractiveElement,
    Styled, Window, div, rgb,
};

use super::Model;
use super::colors::colors;
use super::streams::{Feed, quality_picker, status_line, tool_button};
use super::widgets::*;

pub struct ScreenView {
    model: Entity<Model>,
    feed: Entity<Feed>,
}

impl ScreenView {
    pub fn new(model: Entity<Model>, feed: Entity<Feed>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.observe_in(&feed, window, |_, feed, window, cx| {
            window.set_window_title(&format!("{}'s screen", feed.read(cx).name));
            cx.notify();
        })
        .detach();
        window.set_window_title(&format!("{}'s screen", feed.read(cx).name));
        Self { model, feed }
    }
}

impl Render for ScreenView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let f = self.feed.read(cx);
        let key = (f.con, f.client);
        let model = self.model.clone();
        let fullscreen = window.is_fullscreen();
        v_flex()
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
                        .child(quality_picker(&self.feed, cx))
                        .child(tool_button("fullscreen", "maximize", "Fullscreen (or double-click)").on_click(
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
