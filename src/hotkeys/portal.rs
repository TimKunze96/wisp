//! Hotkeys through the XDG GlobalShortcuts portal, for Linux desktops other
//! than Hyprland (KDE Plasma, GNOME). The desktop owns the keys: it may ask
//! before binding them and they can be changed in its shortcut settings.
//! They work while Wisp runs, which a portal session can't outlive.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::OnceLock;
use std::sync::mpsc as std_mpsc;
use std::time::Duration;

use ashpd::desktop::global_shortcuts::{BindShortcutsOptions, GlobalShortcuts, NewShortcut};
use ashpd::desktop::{CreateSessionOptions, Session};
use futures::channel::mpsc;
use futures::StreamExt;

use super::{Action, Binding, bindings, fire, normalize};
use crate::config::{Settings, WhisperList};
use crate::desktop::APP_ID;
use crate::net::Handle;

struct Shortcut {
    id: String,
    description: String,
    trigger: String,
    binding: Binding,
}

type Request = (Vec<Shortcut>, Option<std_mpsc::Sender<Result<(), String>>>);

static REQUESTS: OnceLock<mpsc::UnboundedSender<Request>> = OnceLock::new();

pub fn start(net: Handle, settings: &Settings) {
    let (tx, rx) = mpsc::unbounded();
    if REQUESTS.set(tx).is_err() {
        return;
    }
    std::thread::Builder::new()
        .name("hotkeys".into())
        .spawn(move || {
            if let Err(e) = futures::executor::block_on(run(rx, net)) {
                tracing::warn!("global shortcuts portal: {e}");
            }
        })
        .expect("spawn hotkeys thread");
    send(&settings.hotkeys, &settings.whisper_lists, None);
}

/// Bind the hotkeys anew. Waits a moment for the portal to answer, but not
/// for a confirmation dialog the desktop might show.
pub fn apply(hotkeys: &BTreeMap<Action, String>, whispers: &[WhisperList]) -> Result<(), String> {
    let (tx, rx) = std_mpsc::channel();
    if !send(hotkeys, whispers, Some(tx)) {
        return Err("Global shortcuts aren't available on this desktop".into());
    }
    match rx.recv_timeout(Duration::from_secs(2)) {
        Ok(result) => result,
        Err(std_mpsc::RecvTimeoutError::Timeout) => Ok(()),
        Err(std_mpsc::RecvTimeoutError::Disconnected) => Err("Global shortcuts aren't available on this desktop".into()),
    }
}

fn send(hotkeys: &BTreeMap<Action, String>, whispers: &[WhisperList], reply: Option<std_mpsc::Sender<Result<(), String>>>) -> bool {
    let shortcuts = bindings(hotkeys, whispers)
        .into_iter()
        .filter_map(|(spec, binding)| {
            let (name, description) = match &binding {
                Binding::Action(a) => (action_id(*a).to_string(), a.title().to_string()),
                Binding::Whisper(id) => {
                    let name = whispers.iter().find(|w| &w.id == id).map_or("", |w| w.name.as_str());
                    (format!("whisper-{id}"), format!("Whisper to {name}"))
                }
            };
            // The key is part of the id: desktops remember the trigger per
            // id and only take the preferred one for ids they haven't seen.
            Some(Shortcut { id: format!("{name}:{}", normalize(&spec)), description, trigger: trigger(&spec)?, binding })
        })
        .collect();
    REQUESTS.get().is_some_and(|tx| tx.unbounded_send((shortcuts, reply)).is_ok())
}

async fn run(mut requests: mpsc::UnboundedReceiver<Request>, net: Handle) -> ashpd::Result<()> {
    // Portals need to know which app this is (from the desktop file
    // desktop::install writes). Older portals have no registry.
    if let Ok(id) = APP_ID.parse() {
        let _ = ashpd::register_host_app(id).await;
    }
    let portal = GlobalShortcuts::new().await?;
    let mut activated = portal.receive_activated().await?.fuse();
    let mut deactivated = portal.receive_deactivated().await?.fuse();
    let mut session: Option<Session<GlobalShortcuts>> = None;
    let mut bound: HashMap<String, Binding> = HashMap::new();
    // a held key may repeat; only the first press counts
    let mut held: HashSet<String> = HashSet::new();
    loop {
        futures::select! {
            request = requests.next() => {
                let Some((shortcuts, reply)) = request else { break };
                if let Some(old) = session.take() {
                    let _ = old.close().await;
                }
                bound.clear();
                held.clear();
                let result = if shortcuts.is_empty() {
                    Ok(())
                } else {
                    bind(&portal, &shortcuts).await.map(|s| {
                        session = Some(s);
                        bound = shortcuts.into_iter().map(|s| (s.id, s.binding)).collect();
                    })
                };
                if let Err(e) = &result {
                    tracing::warn!("binding global shortcuts: {e}");
                }
                if let Some(reply) = reply {
                    let _ = reply.send(result.map_err(|e| format!("The desktop refused the hotkeys: {e}")));
                }
            }
            a = activated.next() => {
                let Some(a) = a else { break };
                if let Some(b) = bound.get(a.shortcut_id()) && held.insert(a.shortcut_id().to_string()) {
                    fire(b.clone(), true, &net);
                }
            }
            d = deactivated.next() => {
                let Some(d) = d else { break };
                if let Some(b) = bound.get(d.shortcut_id()) && held.remove(d.shortcut_id()) {
                    fire(b.clone(), false, &net);
                }
            }
        }
    }
    Ok(())
}

async fn bind(portal: &GlobalShortcuts, shortcuts: &[Shortcut]) -> ashpd::Result<Session<GlobalShortcuts>> {
    let session = portal.create_session(CreateSessionOptions::default()).await?;
    let new: Vec<NewShortcut> = shortcuts
        .iter()
        .map(|s| NewShortcut::new(s.id.clone(), format!("Wisp: {}", s.description)).preferred_trigger(s.trigger.as_str()))
        .collect();
    portal.bind_shortcuts(&session, &new, None, BindShortcutsOptions::default()).await?.response()?;
    Ok(session)
}

fn action_id(action: Action) -> &'static str {
    match action {
        Action::PushToTalk => "push-to-talk",
        Action::ToggleTalk => "toggle-talk",
        Action::ToggleMute => "toggle-mute",
        Action::ToggleDeafen => "toggle-deafen",
        Action::ShowWindow => "show-window",
    }
}

/// "CTRL + ALT + F13" (as recorded in the settings) to a trigger in the XDG
/// shortcuts format ("CTRL+ALT+F13"). The keys are already xkb keysym names.
fn trigger(spec: &str) -> Option<String> {
    let mut parts = Vec::new();
    for part in spec.split('+').map(str::trim).filter(|p| !p.is_empty()) {
        parts.push(match part.to_uppercase().as_str() {
            "SUPER" => "LOGO".to_string(),
            "CTRL" | "ALT" | "SHIFT" => part.to_uppercase(),
            _ if part.chars().count() == 1 => part.to_lowercase(),
            _ => part.to_string(),
        });
    }
    (!parts.is_empty()).then(|| parts.join("+"))
}

#[cfg(test)]
mod tests {
    use super::trigger;

    #[test]
    fn triggers() {
        assert_eq!(trigger("CTRL + ALT + F13").as_deref(), Some("CTRL+ALT+F13"));
        assert_eq!(trigger("SUPER + M").as_deref(), Some("LOGO+m"));
        assert_eq!(trigger("SHIFT + grave").as_deref(), Some("SHIFT+grave"));
        assert_eq!(trigger("").as_deref(), None);
    }
}
