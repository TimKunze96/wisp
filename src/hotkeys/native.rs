//! Hotkeys registered with the system (RegisterHotKey on Windows, Carbon hot
//! keys on macOS) through global-hotkey. They need the main thread, which is
//! where the settings and `start` run.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::str::FromStr;
use std::sync::{Mutex, OnceLock};

use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};

use super::{Action, Binding, fire};
use crate::config::{Settings, WhisperList};
use crate::net::Handle;

thread_local! {
    static MANAGER: RefCell<Option<(GlobalHotKeyManager, Vec<HotKey>)>> = const { RefCell::new(None) };
}

static BINDINGS: Mutex<Option<HashMap<u32, Binding>>> = Mutex::new(None);
static NET: OnceLock<Handle> = OnceLock::new();

/// No other config to load ours.
pub fn included() -> bool {
    true
}

pub const INCLUDE_LINE: &str = "";

pub fn about() -> &'static str {
    "Hotkeys work everywhere, also while Wisp is in the background. A key another app already \
     uses as a hotkey can't be taken."
}

/// The system doesn't say which keys other apps use.
pub fn taken() -> Vec<(String, String)> {
    Vec::new()
}

pub fn external_ptt() -> bool {
    false
}

/// Register the saved hotkeys and start listening.
pub fn start(net: Handle, settings: &Settings) {
    let _ = NET.set(net);
    // a held key repeats; only the first press counts
    let held = Mutex::new(HashSet::new());
    GlobalHotKeyEvent::set_event_handler(Some(move |e: GlobalHotKeyEvent| {
        let down = e.state == HotKeyState::Pressed;
        let first = if down { held.lock().unwrap().insert(e.id) } else { held.lock().unwrap().remove(&e.id) };
        if !first {
            return;
        }
        let binding = BINDINGS.lock().unwrap().as_ref().and_then(|b| b.get(&e.id).cloned());
        let (Some(binding), Some(net)) = (binding, NET.get()) else { return };
        fire(binding, down, net);
    }));
    let _ = apply(&settings.hotkeys, &settings.whisper_lists);
}

/// Register the hotkeys anew. Returns the ones the system refused.
pub fn apply(hotkeys: &BTreeMap<Action, String>, whispers: &[WhisperList]) -> Result<(), String> {
    let wanted = super::bindings(hotkeys, whispers);
    MANAGER.with(|m| {
        let mut m = m.borrow_mut();
        if m.is_none() {
            *m = Some((GlobalHotKeyManager::new().map_err(|e| e.to_string())?, Vec::new()));
        }
        let (manager, registered) = m.as_mut().unwrap();
        let _ = manager.unregister_all(registered);
        registered.clear();
        let mut bindings = HashMap::new();
        let mut failed = Vec::new();
        for (spec, binding) in wanted {
            match hotkey(&spec) {
                Some(key) if manager.register(key).is_ok() => {
                    registered.push(key);
                    bindings.insert(key.id(), binding);
                }
                Some(_) => failed.push(format!("{spec} is already used by another app")),
                None => failed.push(format!("{spec} can't be used as a hotkey")),
            }
        }
        *BINDINGS.lock().unwrap() = Some(bindings);
        if failed.is_empty() { Ok(()) } else { Err(failed.join("\n")) }
    })
}

/// "CTRL + ALT + F13" (as recorded in the settings) to a system hotkey.
fn hotkey(spec: &str) -> Option<HotKey> {
    let mut mods = Modifiers::empty();
    let mut code = None;
    for part in spec.split('+').map(str::trim) {
        match part.to_uppercase().as_str() {
            "SUPER" => mods |= Modifiers::META,
            "CTRL" => mods |= Modifiers::CONTROL,
            "ALT" => mods |= Modifiers::ALT,
            "SHIFT" => mods |= Modifiers::SHIFT,
            _ => code = Some(key_code(part)?),
        }
    }
    Some(HotKey::new((!mods.is_empty()).then_some(mods), code?))
}

fn key_code(key: &str) -> Option<Code> {
    let name = match key {
        "space" => "Space".to_string(),
        "Return" => "Enter".into(),
        "BackSpace" => "Backspace".into(),
        "Prior" => "PageUp".into(),
        "Next" => "PageDown".into(),
        "Up" | "Down" | "Left" | "Right" => format!("Arrow{key}"),
        "grave" => "Backquote".into(),
        "minus" => "Minus".into(),
        "equal" => "Equal".into(),
        "bracketleft" => "BracketLeft".into(),
        "bracketright" => "BracketRight".into(),
        "backslash" => "Backslash".into(),
        "semicolon" => "Semicolon".into(),
        "apostrophe" => "Quote".into(),
        "comma" => "Comma".into(),
        "period" => "Period".into(),
        "slash" => "Slash".into(),
        k if k.len() == 1 && k.chars().all(|c| c.is_ascii_alphabetic()) => format!("Key{}", k.to_uppercase()),
        k if k.len() == 1 && k.chars().all(|c| c.is_ascii_digit()) => format!("Digit{k}"),
        k => k.to_string(),
    };
    Code::from_str(&name).ok()
}
