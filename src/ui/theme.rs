//! The component theme (inputs, buttons, sliders, menus): neutral palette, small radius.

use std::rc::Rc;

use gpui_kit::App;
use gpui_kit::component::{Theme, ThemeConfig, ThemeMode};

use super::widgets::*;

fn hex(c: u32) -> String {
    format!("#{c:06x}")
}

fn hexa(c: u32, a: u8) -> String {
    format!("#{c:06x}{a:02x}")
}

pub fn apply(cx: &mut App) {
    let colors = serde_json::json!({
        "background": hex(BG),
        "foreground": hex(TEXT),
        "border": "#ffffff1f",
        "input.border": "#ffffff26",
        "ring": hexa(ACCENT, 0x99),
        "caret": hex(TEXT),
        "accent.background": "#ffffff14",
        "accent.foreground": hex(TEXT),
        "primary.background": hex(0xe3e5e8),
        "primary.hover.background": hex(0xffffff),
        "primary.active.background": hex(0xc9ccd1),
        "primary.foreground": hex(0x0c0d0f),
        "secondary.background": hex(RAISED),
        "secondary.hover.background": "#ffffff1a",
        "secondary.active.background": "#ffffff24",
        "secondary.foreground": hex(TEXT),
        "muted.background": hex(PANEL),
        "muted.foreground": hex(MUTED),
        "popover.background": hex(0x17191d),
        "popover.foreground": hex(TEXT),
        "list.background": hex(PANEL),
        "list.active.background": "#ffffff14",
        "list.active.border": "#ffffff00",
        "selection.background": hexa(ACCENT, 0x40),
        "danger.background": hexa(RED, 0x26),
        "danger.hover.background": hexa(RED, 0x3d),
        "danger.active.background": hexa(RED, 0x52),
        "danger.foreground": hex(RED),
        "success.background": hexa(GREEN, 0x26),
        "success.foreground": hex(GREEN),
        "warning.background": hexa(AMBER, 0x26),
        "warning.foreground": hex(AMBER),
        "info.background": "#ffffff14",
        "info.foreground": hex(TEXT),
        "switch.background": hex(0x2a2d33),
        "slider.background": hex(TEXT),
        "slider.thumb.background": hex(TEXT),
        "scrollbar.background": "#00000000",
        "scrollbar.thumb.background": "#ffffff1f",
        "scrollbar.thumb.hover.background": "#ffffff33",
        "overlay": "#000000a6",
        "window.border": "#ffffff1f",
        "base.red": hex(RED),
        "base.green": hex(GREEN),
        "base.yellow": hex(AMBER),
        "base.blue": hex(ACCENT),
        "base.cyan": hex(ACCENT),
        "base.magenta": hex(ACCENT),
    });
    let config = serde_json::json!({
        "is_default": true,
        "name": "Wisp",
        "mode": "dark",
        "font.size": 14.0,
        "font.family": FONT,
        "radius": 5,
        "radius.lg": 6,
        "shadow": false,
        "colors": colors,
    });
    Theme::change(ThemeMode::Dark, None, cx);
    match serde_json::from_value::<ThemeConfig>(config) {
        Ok(config) => {
            Theme::global_mut(cx).apply_config(&Rc::new(config));
            cx.refresh_windows();
        }
        Err(e) => tracing::warn!("theme config rejected: {e}"),
    }
}
