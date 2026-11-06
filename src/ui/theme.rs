//! The component theme (inputs, buttons, sliders, menus), built from the
//! current colors.

use std::rc::Rc;

use std::time::Duration;

use gpui_kit::App;
use gpui_kit::component::{Theme, ThemeConfig, ThemeMode};

use super::colors::mix;
use super::widgets::*;

fn hex(c: u32) -> String {
    format!("#{c:06x}")
}

fn hexa(c: u32, a: u8) -> String {
    format!("#{c:06x}{a:02x}")
}

/// Push the current colors into the component theme and redraw.
pub fn apply(cx: &mut App) {
    let c = colors();
    let ink = ink();
    let colors = serde_json::json!({
        "background": hex(c.bg),
        "foreground": hex(c.text),
        "border": hexa(ink, 0x1f),
        "input.border": hexa(ink, 0x26),
        "ring": hexa(c.accent, 0x99),
        "caret": hex(c.text),
        "accent.background": hexa(ink, 0x14),
        "accent.foreground": hex(c.text),
        "primary.background": hex(c.text),
        "primary.hover.background": hex(mix(c.text, ink, 0.5)),
        "primary.active.background": hex(mix(c.text, c.bg, 0.12)),
        "primary.foreground": hex(c.bg),
        "secondary.background": hex(c.raised),
        "secondary.hover.background": hexa(ink, 0x1a),
        "secondary.active.background": hexa(ink, 0x24),
        "secondary.foreground": hex(c.text),
        "muted.background": hex(c.panel),
        "muted.foreground": hex(c.muted),
        "popover.background": hex(c.raised),
        "popover.foreground": hex(c.text),
        "list.background": hex(c.panel),
        "list.active.background": hexa(ink, 0x14),
        "list.active.border": hexa(ink, 0x00),
        "selection.background": hexa(c.accent, 0x40),
        "danger.background": hexa(c.red, 0x26),
        "danger.hover.background": hexa(c.red, 0x3d),
        "danger.active.background": hexa(c.red, 0x52),
        "danger.foreground": hex(c.red),
        "success.background": hexa(c.green, 0x26),
        "success.foreground": hex(c.green),
        "warning.background": hexa(c.amber, 0x26),
        "warning.foreground": hex(c.amber),
        "info.background": hexa(ink, 0x14),
        "info.foreground": hex(c.text),
        "switch.background": hex(mix(c.bg, c.text, 0.14)),
        "slider.background": hex(c.text),
        "slider.thumb.background": hex(c.text),
        "scrollbar.background": "#00000000",
        "scrollbar.thumb.background": hexa(ink, 0x1f),
        "scrollbar.thumb.hover.background": hexa(ink, 0x33),
        "overlay": "#000000a6",
        "window.border": hexa(ink, 0x1f),
        "base.red": hex(c.red),
        "base.green": hex(c.green),
        "base.yellow": hex(c.amber),
        "base.blue": hex(c.accent),
        "base.cyan": hex(c.accent),
        "base.magenta": hex(c.accent),
    });
    let mode = if c.light { ThemeMode::Light } else { ThemeMode::Dark };
    let config = serde_json::json!({
        "is_default": true,
        "name": "Wisp",
        "mode": if c.light { "light" } else { "dark" },
        "font.size": 14.0,
        "font.family": FONT,
        "radius": 5,
        "radius.lg": 6,
        "shadow": false,
        "colors": colors,
    });
    Theme::change(mode, None, cx);
    match serde_json::from_value::<ThemeConfig>(config) {
        Ok(config) => Theme::global_mut(cx).apply_config(&Rc::new(config)),
        Err(e) => tracing::warn!("theme config rejected: {e}"),
    }
    cx.refresh_windows();
}

/// Reload when theme.json changes (or appears, or goes away).
pub fn watch(cx: &mut App) {
    cx.spawn(async move |cx| {
        let mut last = super::colors::stamp();
        loop {
            cx.background_executor().timer(Duration::from_secs(1)).await;
            let now = super::colors::stamp();
            if now == last {
                continue;
            }
            last = now;
            if let Some(c) = super::colors::load()
                && super::colors::set(c)
            {
                cx.update(apply);
            }
        }
    })
    .detach();
}
