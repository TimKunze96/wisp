//! "?" or F1: every key Wisp listens to, and the global hotkeys that are set.

use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::{
    AnyElement, Context, FontWeight, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement, Styled, div,
    px, rgb, rgba,
};

use super::main_view::MainView;
use super::widgets::*;
use crate::hotkeys::Action;

const IN_APP: &[(&str, &[(&str, &str)])] = &[
    (
        "Main window",
        &[
            ("Ctrl+K", "Command palette: jump to people, channels, servers"),
            ("↑ ↓", "Move through the channel list"),
            ("← →", "Fold or unfold a channel"),
            ("Enter", "Join the channel"),
            ("Escape", "Close a dialog, leave the stage, clear the selection"),
            ("? or F1", "This list"),
        ],
    ),
    (
        "Stream windows",
        &[
            ("F or F11", "Fullscreen (double-click works too)"),
            ("Escape", "Leave fullscreen"),
            ("M", "Mute the stream's sound"),
            ("+ − 0", "Zoom in, out, back to all of it (wheel or pinch too, drag to move)"),
            ("P", "Keep the window on top (macOS, Windows)"),
        ],
    ),
];

impl MainView {
    pub(super) fn shortcuts_overlay(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.shortcuts {
            return None;
        }
        let row = |key: String, what: String| {
            h_flex()
                .gap_4()
                .child(
                    div()
                        .w(px(120.))
                        .flex_none()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(colors().text))
                        .child(key),
                )
                .child(label(what))
        };
        let mut body = v_flex().gap_4();
        for (title, keys) in IN_APP {
            body = body.child(
                v_flex()
                    .gap_1()
                    .child(section_title(title))
                    .children(keys.iter().map(|(k, w)| row(k.to_string(), w.to_string()))),
            );
        }
        let hotkeys = &self.model.read(cx).settings.hotkeys;
        let set: Vec<_> = Action::ALL
            .iter()
            .filter_map(|a| hotkeys.get(a).filter(|k| !k.trim().is_empty()).map(|k| (k.clone(), a.title().to_string())))
            .collect();
        let global = if set.is_empty() {
            v_flex().child(label("None set yet (Settings, Hotkeys)."))
        } else {
            v_flex().gap_1().children(set.into_iter().map(|(k, w)| row(k, w)))
        };
        body = body.child(v_flex().gap_1().child(section_title("Global hotkeys")).child(global));
        Some(
            div()
                .id("shortcuts")
                .absolute()
                .inset_0()
                .bg(rgba(0x00000099))
                .flex()
                .items_center()
                .justify_center()
                .occlude()
                .on_click(cx.listener(|t, _, _, cx| {
                    t.shortcuts = false;
                    cx.notify();
                }))
                .child(
                    v_flex()
                        .w(px(560.))
                        .gap_5()
                        .p_5()
                        .rounded(px(8.))
                        .bg(rgb(colors().panel))
                        .border_1()
                        .border_color(border())
                        .shadow_lg()
                        .child(div().font_weight(FontWeight::SEMIBOLD).child("Keyboard shortcuts"))
                        .child(body),
                )
                .into_any_element(),
        )
    }
}
