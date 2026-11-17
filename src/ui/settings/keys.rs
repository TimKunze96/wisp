//! Settings → Hotkeys: recording global keys and handing them to the desktop.

use super::*;

impl SettingsView {
    pub(super) fn set_hotkey(&mut self, rec: Rec, keys: Option<String>, cx: &mut Context<Self>) {
        self.model.update(cx, |m, cx| {
            m.update_settings(cx, |s| match (rec, keys) {
                (Rec::Action(a), Some(k)) => {
                    s.hotkeys.insert(a, k);
                }
                (Rec::Action(a), None) => {
                    s.hotkeys.remove(&a);
                }
                (Rec::Whisper(id), k) => {
                    if let Some(l) = s.whisper_lists.iter_mut().find(|l| l.id == id) {
                        l.hotkey = k.unwrap_or_default();
                    }
                }
            })
        });
        self.apply_hotkeys(cx);
    }

    /// Rewrite wisp.lua from the settings and reload Hyprland.
    pub(super) fn apply_hotkeys(&mut self, cx: &mut Context<Self>) {
        let s = &self.model.read(cx).settings;
        self.hotkey_error = hotkeys::apply(&s.hotkeys, &s.whisper_lists).err();
        self.taken = hotkeys::taken();
        cx.notify();
    }

    pub(super) fn on_key(&mut self, ev: &KeyDownEvent, cx: &mut Context<Self>) {
        let Some(action) = self.recording.clone() else { return };
        let k = &ev.keystroke;
        if k.key == "escape" && !k.modifiers.modified() {
            self.recording = None;
            cx.notify();
            return;
        }
        let m = k.modifiers;
        if let Some(spec) = hotkeys::spec_from_keystroke(&k.key, m.control, m.alt, m.shift, m.platform) {
            self.recording = None;
            self.set_hotkey(action, Some(spec), cx);
        }
    }

    pub(super) fn hotkeys_tab(&self, cx: &mut Context<Self>) -> AnyElement {
        let set = self.model.read(cx).settings.hotkeys.clone();
        let external = hotkeys::external_ptt();
        let mut list = Group::new("keyboard", "Actions");
        for (i, action) in Action::ALL.into_iter().enumerate() {
            let keys = set.get(&action).cloned().filter(|k| !k.is_empty());
            let recording = self.recording == Some(Rec::Action(action));
            let conflict = keys
                .as_ref()
                .and_then(|k| self.taken.iter().find(|(t, _)| *t == hotkeys::normalize(k)))
                .map(|(_, d)| d.clone());
            let chip = key_chip(
                match &keys {
                    None if action == Action::PushToTalk && external => "F13 (bindings.lua)".into(),
                    None => "Not set".into(),
                    Some(k) => k.clone(),
                },
                keys.is_some(),
                recording,
            )
            .min_w(px(140.));
            list = list.child(
                v_flex()
                    .px_4()
                    .py_3()
                    .gap_1p5()
                    .child(
                        h_flex()
                            .gap_3()
                            .items_center()
                            .child(icon(action.icon(), 16., colors().muted))
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .child(div().text_sm().child(action.title()))
                                    .child(label(action.hint())),
                            )
                            .child(chip)
                            .child(
                                Button::new(SharedString::from(format!("rec-{i}")))
                                    .label(if recording { "Cancel" } else { "Record" })
                                    .small()
                                    .ghost()
                                    .on_click(cx.listener(move |t, _, window, cx| {
                                        let rec = Some(Rec::Action(action));
                                        t.recording = if t.recording == rec { None } else { rec };
                                        window.focus(&t.focus, cx);
                                        cx.notify();
                                    })),
                            )
                            .child(
                                icon_btn(SharedString::from(format!("clr-{i}")), "x", colors().muted, "Clear")
                                    .when(keys.is_none(), |d| d.invisible())
                                    .on_click(cx.listener(move |t, _, _, cx| t.set_hotkey(Rec::Action(action), None, cx))),
                            ),
                    )
                    .when_some(conflict, |d, c| {
                        d.child(
                            h_flex()
                                .pl(px(28.))
                                .gap_1p5()
                                .items_center()
                                .child(icon("triangle-alert", 12., colors().amber))
                                .child(label(format!("Also bound in Hyprland: {c}")).text_color(rgb(colors().amber))),
                        )
                    }),
            );
        }
        let included = hotkeys::included();
        v_flex()
            .gap_4()
            .when(!included, |d| {
                d.child(notice(
                    "triangle-alert",
                    colors().amber,
                    format!("Add {} to ~/.config/hypr/hyprland.lua", hotkeys::INCLUDE_LINE),
                ))
            })
            .when_some(self.hotkey_error.clone(), |d, e| d.child(notice("triangle-alert", colors().red, e)))
            .child(list.hint(hotkeys::about()))
            .into_any_element()
    }
}

/// The keys of a hotkey, or "Press a key..." while recording one.
pub(super) fn key_chip(text: String, set: bool, recording: bool) -> gpui_kit::Div {
    div()
        .h(px(30.))
        .px_3()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(5.))
        .border_1()
        .border_color(if recording { alpha(ink(), 0x55) } else { border() })
        .bg(if recording { alpha(ink(), 0x14) } else { rgb(colors().raised) })
        .text_sm()
        .text_color(rgb(if set || recording { colors().text } else { colors().dim }))
        .child(if recording { "Press a key...".to_string() } else { text })
}
