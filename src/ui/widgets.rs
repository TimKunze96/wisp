//! Palette and building blocks. Neutral greys, hard corners, one accent used
//! only for "active"; green only for talking, red only for muted / danger.

use gpui_kit::{Div, FontWeight, IntoElement, ParentElement, Rgba, SharedString, Styled, div, img, px, rgb, rgba, svg};

use crate::net::GroupBadge;

pub const BG: u32 = 0x0c0d0f;
pub const SIDEBAR: u32 = 0x101114;
pub const PANEL: u32 = 0x101114;
pub const RAISED: u32 = 0x17191d;
pub const TEXT: u32 = 0xe3e5e8;
pub const MUTED: u32 = 0x8b9098;
pub const DIM: u32 = 0x5b6069;
pub const ACCENT: u32 = 0x8fb3d9;
pub const GREEN: u32 = 0x5fc98a;
pub const RED: u32 = 0xe06c75;
pub const AMBER: u32 = 0xd8b26a;

pub const FONT: &str = "Adwaita Sans";

/// Every header row (sidebar, centre, right panel) has this height.
pub const HEADER_H: f32 = 52.;
/// Every footer row has this height.
pub const FOOTER_H: f32 = 56.;

pub fn border() -> Rgba {
    rgba(0xffffff12)
}

pub fn alpha(c: u32, a: u8) -> Rgba {
    rgba((c << 8) | u32::from(a))
}

pub fn hover_bg() -> Rgba {
    rgba(0xffffff0a)
}

pub fn active_bg() -> Rgba {
    rgba(0xffffff10)
}

/// A Lucide icon from the bundled set (`assets/icons/<name>.svg`).
pub fn icon(name: &str, size: f32, color: u32) -> impl IntoElement {
    svg()
        .path(SharedString::from(format!("icons/{name}.svg")))
        .size(px(size))
        .flex_none()
        .text_color(rgb(color))
}

/// Small muted caption.
pub fn label(text: impl Into<SharedString>) -> Div {
    div().text_xs().text_color(rgb(MUTED)).child(text.into())
}

/// Section header: small uppercase title and a hairline.
pub fn section_title(text: &str) -> Div {
    div()
        .flex()
        .items_center()
        .gap_3()
        .child(
            div()
                .text_size(px(11.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(DIM))
                .child(text.to_uppercase()),
        )
        .child(div().flex_1().h(px(1.)).bg(border()))
}

/// A fixed-height header row; use it for every column so they line up.
pub fn header_row() -> Div {
    div()
        .h(px(HEADER_H))
        .flex_none()
        .flex()
        .items_center()
        .gap_3()
        .px_4()
        .border_b_1()
        .border_color(border())
}

/// A fixed-height footer row; same idea as [`header_row`].
pub fn footer_row() -> Div {
    div()
        .h(px(FOOTER_H))
        .flex_none()
        .flex()
        .items_center()
        .gap_3()
        .px_3()
        .border_t_1()
        .border_color(border())
}

pub fn initials(name: &str) -> String {
    let mut words = name.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty());
    let first = words.next().and_then(|w| w.chars().next());
    let second = words.next().and_then(|w| w.chars().next());
    match (first, second) {
        (Some(a), Some(b)) => format!("{a}{b}").to_uppercase(),
        (Some(a), None) => a.to_uppercase().to_string(),
        _ => "?".into(),
    }
}

/// Round avatar: their picture if they have one, else initials; a green
/// outline while talking.
pub fn avatar(name: &str, image: Option<&std::path::Path>, size: f32, talking: bool) -> Div {
    let base = div()
        .flex_none()
        .size(px(size))
        .rounded_full()
        .overflow_hidden()
        .flex()
        .items_center()
        .justify_center()
        .bg(rgb(RAISED))
        .border_1()
        .border_color(if talking { rgb(GREEN) } else { border() });
    match image {
        Some(path) => base.child(img(path.to_path_buf()).size_full().rounded_full()),
        None => base
            .text_size(px(size * 0.38))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(rgb(if talking { GREEN } else { MUTED }))
            .child(initials(name)),
    }
}

/// A group (role) badge: the server's own icon if we have it, a Lucide
/// stand-in for TeamSpeak's built-in icons, nothing otherwise.
pub fn badge(b: &GroupBadge) -> Option<Div> {
    let inner = if let Some(path) = &b.icon {
        div().child(img(path.clone()).size(px(16.)))
    } else if let Some(id) = b.builtin {
        let name = match id {
            500 => "crown",
            600 => "star",
            300 => "shield-check",
            100 | 200 => "shield",
            _ => "badge",
        };
        div().child(icon(name, 14., MUTED))
    } else {
        return None;
    };
    Some(inner.flex_none().flex().items_center())
}

/// Quiet stat text, e.g. "15 ms". Coloured only when it signals a problem.
pub fn readout(text: impl Into<SharedString>, color: u32) -> Div {
    div().text_xs().text_color(rgb(if color == GREEN || color == ACCENT { MUTED } else { color })).child(text.into())
}

/// Thin level meter (0..1).
pub fn segments(level: f32, color: u32) -> Div {
    div()
        .size_full()
        .bg(rgb(RAISED))
        .child(div().h_full().w(gpui_kit::relative(level)).bg(rgb(color)))
}

/// The Wisp logo without its tile.
pub fn mark(size: f32) -> impl IntoElement {
    static MARK: std::sync::LazyLock<std::sync::Arc<gpui_kit::Image>> = std::sync::LazyLock::new(|| {
        std::sync::Arc::new(gpui_kit::Image::from_bytes(
            gpui_kit::ImageFormat::Svg,
            include_bytes!("../../assets/wisp-mark.svg").to_vec(),
        ))
    });
    img(MARK.clone()).size(px(size)).flex_none()
}

/// The Wisp wordmark.
pub fn logo() -> Div {
    div()
        .flex()
        .items_center()
        .gap_2()
        .child(mark(18.))
        .child(div().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).child("Wisp"))
}

pub fn ping_color(ms: f32) -> u32 {
    if ms < 80. {
        MUTED
    } else if ms < 150. {
        AMBER
    } else {
        RED
    }
}

pub fn fmt_duration(secs: u64) -> String {
    let (h, m) = (secs / 3600, (secs % 3600) / 60);
    if h >= 24 {
        format!("{}d {}h", h / 24, h % 24)
    } else if h > 0 {
        format!("{h}h {m}m")
    } else if m > 0 {
        format!("{m}m")
    } else {
        format!("{secs}s")
    }
}

/// One option of a small segmented choice (mode, channel type, codec...).
pub fn chip(id: impl Into<gpui_kit::ElementId>, text: impl Into<SharedString>, on: bool) -> gpui_kit::Stateful<Div> {
    use gpui_kit::InteractiveElement;
    div()
        .id(id)
        .h(px(30.))
        .px_3()
        .flex()
        .items_center()
        .rounded(px(5.))
        .cursor_pointer()
        .text_sm()
        .border_1()
        .border_color(if on { rgba(0xffffff33) } else { border() })
        .bg(if on { rgba(0xffffff14) } else { rgba(0x00000000) })
        .text_color(rgb(if on { TEXT } else { MUTED }))
        .hover(|d| d.bg(hover_bg()))
        .child(text.into())
}
