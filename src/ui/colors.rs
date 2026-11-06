//! Colors at runtime. Wisp's own palette, or ~/.config/wisp/theme.json when it
//! exists; the file is watched, so whatever writes it (a desktop theme hook, a
//! script, an editor) recolors the app live. See the README for the keys.

use std::path::PathBuf;
use std::sync::RwLock;
use std::time::SystemTime;

use serde::Deserialize;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Colors {
    pub light: bool,
    pub bg: u32,
    pub sidebar: u32,
    pub panel: u32,
    pub raised: u32,
    pub text: u32,
    /// Between text and muted: inactive names, descriptions.
    pub soft: u32,
    pub muted: u32,
    pub dim: u32,
    pub accent: u32,
    pub green: u32,
    pub red: u32,
    pub amber: u32,
}

pub const WISP: Colors = Colors {
    light: false,
    bg: 0x0c0d0f,
    sidebar: 0x101114,
    panel: 0x101114,
    raised: 0x17191d,
    text: 0xe3e5e8,
    soft: 0xb3b8c0,
    muted: 0x8b9098,
    dim: 0x5b6069,
    accent: 0x8fb3d9,
    green: 0x5fc98a,
    red: 0xe06c75,
    amber: 0xd8b26a,
};

static CURRENT: RwLock<Colors> = RwLock::new(WISP);

pub fn colors() -> Colors {
    *CURRENT.read().unwrap_or_else(|e| e.into_inner())
}

/// Swap the palette; false when nothing changed.
pub fn set(c: Colors) -> bool {
    let mut cur = CURRENT.write().unwrap_or_else(|e| e.into_inner());
    let changed = *cur != c;
    *cur = c;
    changed
}

/// White on dark themes, black on light ones; for hairlines and hover tints.
pub fn ink() -> u32 {
    if colors().light { 0x000000 } else { 0xffffff }
}

/// Blend `a` toward `b` by `t` (0..1).
pub fn mix(a: u32, b: u32, t: f32) -> u32 {
    let ch = |c: u32, s: u32| ((c >> s) & 0xff) as f32;
    [16, 8, 0].into_iter().fold(0, |acc, s| {
        let v = ch(a, s) + (ch(b, s) - ch(a, s)) * t;
        acc | ((v.round().clamp(0., 255.) as u32) << s)
    })
}

/// theme.json. Every key is optional; missing shades are mixed from
/// background and text, missing signal colors are Wisp's.
#[derive(Default, Deserialize)]
#[serde(default)]
struct ThemeFile {
    /// "dark" or "light"; guessed from the background when missing.
    mode: Option<String>,
    background: Option<String>,
    /// Sidebars and side panels.
    panel: Option<String>,
    /// Popovers, buttons, inputs.
    raised: Option<String>,
    text: Option<String>,
    soft: Option<String>,
    muted: Option<String>,
    dim: Option<String>,
    accent: Option<String>,
    /// Someone talking.
    talking: Option<String>,
    /// Muted, errors, destructive actions.
    danger: Option<String>,
    warning: Option<String>,
}

fn parse_hex(s: &Option<String>) -> Option<u32> {
    let s = s.as_deref()?.trim().trim_start_matches('#');
    // #rrggbbaa: drop the alpha
    u32::from_str_radix(s.get(..6)?, 16).ok()
}

fn luminance(c: u32) -> f32 {
    let ch = |s: u32| ((c >> s) & 0xff) as f32 / 255.;
    0.2126 * ch(16) + 0.7152 * ch(8) + 0.0722 * ch(0)
}

fn parse(json: &str) -> anyhow::Result<Colors> {
    let f: ThemeFile = serde_json::from_str(json)?;
    let bg = parse_hex(&f.background).unwrap_or(WISP.bg);
    let text = parse_hex(&f.text).unwrap_or(WISP.text);
    let light = match f.mode.as_deref() {
        Some("light") => true,
        Some("dark") => false,
        _ => luminance(bg) > 0.5,
    };
    let panel = parse_hex(&f.panel).unwrap_or_else(|| mix(bg, text, 0.03));
    Ok(Colors {
        light,
        bg,
        sidebar: panel,
        panel,
        raised: parse_hex(&f.raised).unwrap_or_else(|| mix(bg, text, 0.06)),
        text,
        soft: parse_hex(&f.soft).unwrap_or_else(|| mix(text, bg, 0.22)),
        muted: parse_hex(&f.muted).unwrap_or_else(|| mix(text, bg, 0.42)),
        dim: parse_hex(&f.dim).unwrap_or_else(|| mix(text, bg, 0.62)),
        accent: parse_hex(&f.accent).unwrap_or(WISP.accent),
        green: parse_hex(&f.talking).unwrap_or(WISP.green),
        red: parse_hex(&f.danger).unwrap_or(WISP.red),
        amber: parse_hex(&f.warning).unwrap_or(WISP.amber),
    })
}

/// ~/.config/wisp/theme.json (a symlink is fine, it's followed).
pub fn path() -> PathBuf {
    crate::config::config_dir().join("theme.json")
}

/// The colors to use now: theme.json, or Wisp's own without one. None when
/// the file is there but broken (half written, a typo): keep what we have.
pub fn load() -> Option<Colors> {
    let Ok(json) = std::fs::read_to_string(path()) else { return Some(WISP) };
    parse(&json).inspect_err(|e| tracing::warn!("{}: {e}", path().display())).ok()
}

/// When theme.json last changed; None without one.
pub fn stamp() -> Option<SystemTime> {
    std::fs::metadata(path()).and_then(|m| m.modified()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mix_blends_channels() {
        assert_eq!(mix(0x000000, 0xffffff, 0.5), 0x808080);
        assert_eq!(mix(0x102030, 0x102030, 0.7), 0x102030);
    }

    #[test]
    fn reads_a_partial_theme() {
        let c = parse(r##"{"background": "#eff1f5", "text": "#4c4f69", "danger": "#d20f39ff"}"##).unwrap();
        assert!(c.light);
        assert_eq!(c.bg, 0xeff1f5);
        assert_eq!(c.red, 0xd20f39);
        assert_eq!(c.green, WISP.green);
        assert_eq!(c.muted, mix(0x4c4f69, 0xeff1f5, 0.42));
    }

    #[test]
    fn empty_theme_is_wisp_shades() {
        let c = parse("{}").unwrap();
        assert_eq!((c.bg, c.text, c.accent), (WISP.bg, WISP.text, WISP.accent));
        assert!(!c.light);
    }
}
