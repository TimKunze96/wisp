//! Building blocks for settings pages: groups of settings in a card, with
//! each setting's name and what it does on the left and its control on the
//! right.

use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{
    AnyElement, App, Div, FontWeight, IntoElement, ParentElement, RenderOnce, Rgba, SharedString, Styled, Window, div,
    px, rgb,
};

use super::widgets::*;

/// Width of the name column in `field` and `slider` rows.
pub const LABEL_W: f32 = 180.;

/// The background of a card.
pub fn card_bg() -> Rgba {
    alpha(ink(), 0x05)
}

/// A card's look, for cards that aren't a `Group`.
pub fn card() -> Div {
    v_flex().rounded(px(8.)).border_1().border_color(border()).bg(card_bg())
}

/// A titled card of settings, the rows split by hairlines.
#[derive(IntoElement)]
pub struct Group {
    icon: &'static str,
    title: SharedString,
    action: Option<AnyElement>,
    hint: Option<SharedString>,
    rows: Vec<AnyElement>,
}

impl Group {
    pub fn new(icon: &'static str, title: impl Into<SharedString>) -> Self {
        Self { icon, title: title.into(), action: None, hint: None, rows: Vec::new() }
    }

    /// A button or so on the title's line, at the right.
    pub fn action(mut self, el: impl IntoElement) -> Self {
        self.action = Some(el.into_any_element());
        self
    }

    /// A note under the card.
    pub fn hint(mut self, text: impl Into<SharedString>) -> Self {
        self.hint = Some(text.into());
        self
    }

    pub fn child(mut self, el: impl IntoElement) -> Self {
        self.rows.push(el.into_any_element());
        self
    }
}

impl RenderOnce for Group {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let c = colors();
        v_flex()
            .gap_2()
            .child(
                h_flex()
                    .gap_2()
                    .px_1()
                    .min_h(px(28.))
                    .items_center()
                    .child(icon(self.icon, 14., c.muted))
                    .child(div().flex_1().text_sm().font_weight(FontWeight::SEMIBOLD).text_color(rgb(c.soft)).child(self.title))
                    .children(self.action),
            )
            .child(card().overflow_hidden().children(
                self.rows.into_iter().enumerate().map(|(i, r)| div().when(i > 0, |d| d.border_t_1().border_color(border())).child(r)),
            ))
            .when_some(self.hint, |d, h| d.child(label(h).px_1()))
    }
}

/// A setting: its name and what it does on the left, the control at the right.
pub fn item(title: impl Into<SharedString>, hint: Option<SharedString>, control: impl IntoElement) -> Div {
    h_flex()
        .gap_4()
        .px_4()
        .py_3()
        .min_h(px(52.))
        .items_center()
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap(px(2.))
                .child(div().text_sm().child(title.into()))
                .when_some(hint, |d, h| d.child(label(h))),
        )
        .child(div().flex_none().child(control))
}

/// A setting whose control takes the room: name on the left, control filling the rest.
pub fn field(title: impl Into<SharedString>, control: impl IntoElement) -> Div {
    h_flex()
        .gap_4()
        .px_4()
        .py_2p5()
        .min_h(px(48.))
        .items_center()
        .child(div().w(px(LABEL_W)).flex_none().text_sm().child(title.into()))
        .child(div().flex_1().min_w_0().child(control))
}

/// A slider with its name on the left and its value at the right.
pub fn slider(title: impl Into<SharedString>, value: impl Into<SharedString>, control: impl IntoElement) -> Div {
    h_flex()
        .gap_4()
        .px_4()
        .py_3()
        .min_h(px(48.))
        .items_center()
        .child(div().w(px(LABEL_W)).flex_none().text_sm().child(title.into()))
        .child(div().flex_1().min_w_0().child(control))
        .child(div().w(px(64.)).flex_none().text_right().text_sm().text_color(rgb(colors().muted)).child(value.into()))
}

/// Free content inside a card, with the rows' padding.
pub fn block(el: impl IntoElement) -> Div {
    div().px_4().py_3().child(el)
}

/// A colored note: a warning, an error, or that something worked.
pub fn notice(name: &'static str, color: u32, text: impl Into<SharedString>) -> Div {
    h_flex()
        .gap_2()
        .items_center()
        .px_3()
        .py_2()
        .rounded(px(8.))
        .bg(alpha(color, 0x14))
        .border_1()
        .border_color(alpha(color, 0x30))
        .child(icon(name, 14., color))
        .child(label(text).text_color(rgb(color)))
}

/// One page in a window's sidebar; the open one gets an accent mark.
pub fn nav_item(id: impl Into<gpui_kit::ElementId>, name: &'static str, title: impl Into<SharedString>, on: bool) -> gpui_kit::Stateful<Div> {
    use gpui_kit::InteractiveElement;
    let c = colors();
    h_flex()
        .id(id)
        .relative()
        .h(px(34.))
        .mx_2()
        .my(px(1.))
        .gap_3()
        .px_3()
        .rounded(px(7.))
        .cursor_pointer()
        .items_center()
        .when(on, |d| {
            d.bg(active_bg())
                .child(div().absolute().left_0().top(px(9.)).w(px(3.)).h(px(16.)).rounded_full().bg(rgb(c.accent)))
        })
        .hover(|d| d.bg(hover_bg()))
        .child(icon(name, 15., if on { c.accent } else { c.muted }))
        .child(div().text_sm().truncate().text_color(rgb(if on { c.text } else { c.soft })).child(title.into()))
}

/// A small heading between groups of sidebar pages.
pub fn nav_heading(text: &'static str) -> Div {
    div()
        .px_5()
        .pt_4()
        .pb_1()
        .text_size(px(11.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(rgb(colors().dim))
        .child(text.to_uppercase())
}

/// A page's title, with its icon and what it's about.
pub fn page_header(name: &'static str, title: impl Into<SharedString>, about: Option<SharedString>) -> Div {
    let c = colors();
    h_flex()
        .gap_3()
        .items_center()
        .child(
            div()
                .size(px(38.))
                .flex_none()
                .rounded(px(9.))
                .bg(alpha(c.accent, 0x1f))
                .flex()
                .items_center()
                .justify_center()
                .child(icon(name, 18., c.accent)),
        )
        .child(
            v_flex()
                .min_w_0()
                .child(div().text_lg().font_weight(FontWeight::SEMIBOLD).truncate().child(title.into()))
                .when_some(about, |d, a| d.child(label(a))),
        )
}
