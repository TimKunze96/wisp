//! Our own title bar on every window: under the traffic lights on macOS,
//! with minimize, maximize and close on Windows, and on Linux desktops that
//! leave decorations to the app (GNOME, KDE). Tiling compositors get none.

use gpui_kit::component::{TitleBar, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{
    AnyElement, App, Bounds, Decorations, IntoElement, ParentElement, Pixels, SharedString, Styled, Window,
    WindowBounds, WindowOptions, div, rgb,
};

use super::widgets::*;

/// Options for a Wisp window of `bounds` that draws its own title bar.
pub fn window_options(bounds: Bounds<Pixels>) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        app_id: Some("wisp".into()),
        #[cfg(target_os = "linux")]
        window_decorations: Some(gpui_kit::WindowDecorations::Client),
        ..TitleBar::window_options()
    }
}

/// Whether this window gets our title bar right now.
fn has_title_bar(window: &Window) -> bool {
    if window.is_fullscreen() {
        return false;
    }
    if cfg!(target_os = "linux") {
        // client-side decorations on a desktop that moves and resizes
        // windows; a tiling compositor offers neither minimize nor maximize
        let controls = window.window_controls();
        return matches!(window.window_decorations(), Decorations::Client { .. })
            && (controls.minimize || controls.maximize);
    }
    true
}

/// `content` under a title bar saying `title`, in the window's frame.
pub fn framed(title: impl Into<SharedString>, content: impl IntoElement, window: &mut Window, _cx: &mut App) -> AnyElement {
    let bar = has_title_bar(window);
    // no window_border here: gpui-kit's Root already draws the Linux border
    // and shadow, and a second one shows up as a frame around the window
    v_flex()
        .size_full()
        .font_family(FONT)
        .text_color(rgb(colors().text))
        .bg(rgb(colors().bg))
        .when(bar, |d| {
            d.child(
                TitleBar::new().bg(rgb(colors().sidebar)).border_color(border()).child(
                    h_flex()
                        .h_full()
                        .items_center()
                        .text_xs()
                        .text_color(rgb(colors().muted))
                        .child(title.into()),
                ),
            )
        })
        .child(div().flex_1().min_h_0().w_full().flex().child(content))
        .into_any_element()
}
