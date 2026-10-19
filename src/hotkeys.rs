//! Global hotkeys. On Hyprland Wisp writes ~/.config/hypr/wisp.lua with binds
//! that run `wisp ...`, and hyprland.lua loads it with
//! `pcall(require, "hypr.wisp")`. Push-to-talk gets a press and a release
//! bind. Other Linux desktops (KDE Plasma, GNOME) get them through the
//! GlobalShortcuts portal (see `portal`). On Windows and macOS Wisp registers
//! them itself (see `native`).

use std::collections::BTreeMap;
#[cfg(target_os = "linux")]
use std::path::PathBuf;
#[cfg(target_os = "linux")]
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::config::WhisperList;
use crate::ctl::{UiRequest, ui};
use crate::net::{Cmd, Handle};

#[cfg(not(target_os = "linux"))]
mod native;
#[cfg(not(target_os = "linux"))]
pub use native::*;
#[cfg(target_os = "linux")]
mod portal;

#[cfg(target_os = "linux")]
fn hyprland() -> bool {
    std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some()
}

/// Hyprland binds outlive Wisp, so there's nothing to set up there. Elsewhere
/// the portal session lives as long as Wisp does.
#[cfg(target_os = "linux")]
pub fn start(net: Handle, settings: &crate::config::Settings) {
    if !hyprland() {
        portal::start(net, settings);
    }
}

#[derive(Clone)]
enum Binding {
    Action(Action),
    Whisper(String),
}

/// The hotkeys that are set, with what each one does.
fn bindings(hotkeys: &BTreeMap<Action, String>, whispers: &[WhisperList]) -> Vec<(String, Binding)> {
    let mut out: Vec<(String, Binding)> = hotkeys
        .iter()
        .filter(|(_, k)| !k.trim().is_empty())
        .map(|(a, k)| (k.clone(), Binding::Action(*a)))
        .collect();
    out.extend(whispers.iter().filter(|w| !w.hotkey.trim().is_empty()).map(|w| (w.hotkey.clone(), Binding::Whisper(w.id.clone()))));
    out
}

fn fire(binding: Binding, down: bool, net: &Handle) {
    match binding {
        Binding::Action(Action::PushToTalk) => net.send(Cmd::Ptt(down)),
        Binding::Whisper(id) => net.send(Cmd::Whisper(id, down)),
        _ if !down => {}
        Binding::Action(Action::ToggleTalk) => net.send(Cmd::TogglePtt),
        Binding::Action(Action::ShowWindow) => ui(UiRequest::Show),
        // toggles need the current state, which the control socket has
        Binding::Action(Action::ToggleMute) => {
            std::thread::spawn(|| crate::ctl::request("mute"));
        }
        Binding::Action(Action::ToggleDeafen) => {
            std::thread::spawn(|| crate::ctl::request("deafen"));
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Action {
    PushToTalk,
    ToggleTalk,
    ToggleMute,
    ToggleDeafen,
    ShowWindow,
}

impl Action {
    pub const ALL: [Action; 5] =
        [Action::PushToTalk, Action::ToggleTalk, Action::ToggleMute, Action::ToggleDeafen, Action::ShowWindow];

    pub fn title(self) -> &'static str {
        match self {
            Action::PushToTalk => "Push-to-talk",
            Action::ToggleTalk => "Toggle talking",
            Action::ToggleMute => "Mute microphone",
            Action::ToggleDeafen => "Sound off",
            Action::ShowWindow => "Show Wisp",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            Action::PushToTalk => "Talk while the key is held",
            Action::ToggleTalk => "Press once to start talking, again to stop",
            Action::ToggleMute => "Toggle your microphone",
            Action::ToggleDeafen => "Toggle all sound",
            Action::ShowWindow => "Bring the window up from the tray",
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            Action::PushToTalk => "mic",
            Action::ToggleTalk => "radio",
            Action::ToggleMute => "mic-off",
            Action::ToggleDeafen => "headphone-off",
            Action::ShowWindow => "app-window",
        }
    }

    /// (press command, release command)
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    fn commands(self) -> (&'static str, Option<&'static str>) {
        match self {
            Action::PushToTalk => ("ptt down", Some("ptt up")),
            Action::ToggleTalk => ("ptt", None),
            Action::ToggleMute => ("mute", None),
            Action::ToggleDeafen => ("deafen", None),
            Action::ShowWindow => ("show", None),
        }
    }
}

#[cfg(target_os = "linux")]
fn hypr_dir() -> PathBuf {
    dirs::config_dir().unwrap_or_default().join("hypr")
}

#[cfg(target_os = "linux")]
pub fn file() -> PathBuf {
    hypr_dir().join("wisp.lua")
}

#[cfg(target_os = "linux")]
pub fn about() -> &'static str {
    if hyprland() {
        "Hotkeys work everywhere, also while Wisp is in the background. Wisp writes them to \
         ~/.config/hypr/wisp.lua and reloads Hyprland. Keys Hyprland already uses can't be recorded \
         here (Hyprland takes them first)."
    } else {
        "Hotkeys work everywhere while Wisp is running, also in the background. Wisp registers them \
         with your desktop, which may ask you to confirm them. Keys can also be changed in the \
         desktop's shortcut settings."
    }
}

#[cfg(target_os = "linux")]
/// Whether hyprland.lua loads our file. Only Hyprland needs it.
pub fn included() -> bool {
    !hyprland()
        || std::fs::read_to_string(hypr_dir().join("hyprland.lua")).is_ok_and(|s| s.contains("hypr.wisp"))
}

#[cfg(target_os = "linux")]
pub const INCLUDE_LINE: &str = "pcall(require, \"hypr.wisp\")";

#[cfg(target_os = "linux")]
fn lua_str(s: &str) -> String {
    format!("{s:?}")
}

#[cfg(target_os = "linux")]
/// Set the hotkeys up anew. Returns what went wrong, if anything.
pub fn apply(hotkeys: &BTreeMap<Action, String>, whispers: &[WhisperList]) -> Result<(), String> {
    if hyprland() { apply_hyprland(hotkeys, whispers) } else { portal::apply(hotkeys, whispers) }
}

#[cfg(target_os = "linux")]
/// Write wisp.lua and reload Hyprland. Returns Hyprland's config errors, if any.
fn apply_hyprland(hotkeys: &BTreeMap<Action, String>, whispers: &[WhisperList]) -> Result<(), String> {
    let exe = crate::desktop::exe();
    let mut out = String::from(
        "-- Generated by Wisp (Settings → Hotkeys). Edits here are overwritten;\n\
         -- change the hotkeys in the app instead.\n\n",
    );
    out.push_str(&format!("local wisp = {}\n\n", lua_str(&exe)));
    for (action, keys) in hotkeys {
        if keys.trim().is_empty() {
            continue;
        }
        let (press, release) = action.commands();
        out.push_str(&format!(
            "hl.bind({}, hl.dsp.exec_cmd(wisp .. \" {press}\"), {{ description = {} }})\n",
            lua_str(keys),
            lua_str(&format!("Wisp: {}", action.title())),
        ));
        if let Some(release) = release {
            out.push_str(&format!(
                "hl.bind({}, hl.dsp.exec_cmd(wisp .. \" {release}\"), {{ release = true }})\n",
                lua_str(keys)
            ));
        }
    }
    for w in whispers.iter().filter(|w| !w.hotkey.trim().is_empty()) {
        for (state, extra) in [("down", format!("description = {}", lua_str(&format!("Wisp: whisper to {}", w.name)))), ("up", "release = true".into())] {
            out.push_str(&format!(
                "hl.bind({}, hl.dsp.exec_cmd(wisp .. \" whisper {} {state}\"), {{ {extra} }})\n",
                lua_str(&w.hotkey),
                w.id,
            ));
        }
    }
    std::fs::write(file(), out).map_err(|e| e.to_string())?;
    let _ = Command::new("hyprctl").arg("reload").output();
    let errors = Command::new("hyprctl")
        .arg("configerrors")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    if errors.is_empty() { Ok(()) } else { Err(errors) }
}

#[cfg(target_os = "linux")]
/// Keys other parts of the Hyprland config already use, as normalized specs.
pub fn taken() -> Vec<(String, String)> {
    if !hyprland() {
        return Vec::new();
    }
    let Ok(out) = Command::new("hyprctl").args(["binds", "-j"]).output() else { return Vec::new() };
    let Ok(binds) = serde_json::from_slice::<Vec<serde_json::Value>>(&out.stdout) else { return Vec::new() };
    binds
        .iter()
        .filter(|b| !b["description"].as_str().unwrap_or("").starts_with("Wisp:"))
        .filter(|b| b["release"] != true)
        .filter_map(|b| {
            let key = b["key"].as_str()?;
            let mods = b["modmask"].as_u64().unwrap_or(0);
            let desc = b["description"].as_str().filter(|d| !d.is_empty()).unwrap_or(b["dispatcher"].as_str().unwrap_or(""));
            Some((normalize(&spec_from_mask(mods, key)), desc.to_string()))
        })
        .collect()
}

#[cfg(target_os = "linux")]
fn spec_from_mask(mask: u64, key: &str) -> String {
    let mut parts = Vec::new();
    if mask & 64 != 0 {
        parts.push("SUPER");
    }
    if mask & 4 != 0 {
        parts.push("CTRL");
    }
    if mask & 8 != 0 {
        parts.push("ALT");
    }
    if mask & 1 != 0 {
        parts.push("SHIFT");
    }
    let mut s = parts.join(" + ");
    if !s.is_empty() {
        s.push_str(" + ");
    }
    s.push_str(key);
    s
}

/// Case- and order-insensitive form for comparing specs.
pub fn normalize(spec: &str) -> String {
    let mut parts: Vec<String> = spec.split('+').map(|p| p.trim().to_lowercase()).filter(|p| !p.is_empty()).collect();
    let key = parts.pop().unwrap_or_default();
    parts.sort();
    parts.push(key);
    parts.join("+")
}

/// Turn a GPUI keystroke into a Hyprland key spec. None for bare modifiers.
pub fn spec_from_keystroke(key: &str, ctrl: bool, alt: bool, shift: bool, sup: bool) -> Option<String> {
    let key = match key {
        "shift" | "control" | "alt" | "platform" | "function" | "" => return None,
        "space" => "space".to_string(),
        "enter" => "Return".into(),
        "tab" => "Tab".into(),
        "backspace" => "BackSpace".into(),
        "delete" => "Delete".into(),
        "insert" => "Insert".into(),
        "home" => "Home".into(),
        "end" => "End".into(),
        "pageup" => "Prior".into(),
        "pagedown" => "Next".into(),
        "up" => "Up".into(),
        "down" => "Down".into(),
        "left" => "Left".into(),
        "right" => "Right".into(),
        "pause" => "Pause".into(),
        "`" => "grave".into(),
        "-" => "minus".into(),
        "=" => "equal".into(),
        "[" => "bracketleft".into(),
        "]" => "bracketright".into(),
        "\\" => "backslash".into(),
        ";" => "semicolon".into(),
        "'" => "apostrophe".into(),
        "," => "comma".into(),
        "." => "period".into(),
        "/" => "slash".into(),
        k if k.len() > 1 && k.starts_with('f') && k[1..].chars().all(|c| c.is_ascii_digit()) => k.to_uppercase(),
        k if k.chars().count() == 1 => k.to_uppercase(),
        k => k.to_string(),
    };
    let mut parts = Vec::new();
    if sup {
        parts.push("SUPER");
    }
    if ctrl {
        parts.push("CTRL");
    }
    if alt {
        parts.push("ALT");
    }
    if shift {
        parts.push("SHIFT");
    }
    parts.push(&key);
    Some(parts.join(" + "))
}

#[cfg(target_os = "linux")]
/// Push-to-talk wired up by hand in bindings.lua (the G502 F13 button).
pub fn external_ptt() -> bool {
    hyprland()
        && std::fs::read_to_string(hypr_dir().join("bindings.lua")).is_ok_and(|s| s.contains("wisp ptt"))
}
