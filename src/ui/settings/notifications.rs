//! Settings → Notifications: sounds and popups per event.

use super::*;

impl SettingsView {
    pub(super) fn notifications(&self, cx: &mut Context<Self>) -> AnyElement {
        let s = self.model.read(cx).settings.clone();
        let mut rows = v_flex().gap_1();
        rows = rows.child(
            h_flex()
                .gap_4()
                .pb_1()
                .pt_1()
                .child(div().flex_1())
                .child(div().w(px(60.)).text_xs().text_color(rgb(colors().dim)).child("Sound"))
                .child(div().w(px(60.)).text_xs().text_color(rgb(colors().dim)).child("Popup"))
                .child(div().w(px(30.))),
        );
        for (i, kind) in EventKind::ALL.into_iter().enumerate() {
            let setting = s.notify_setting(kind);
            let toggle = move |sound: bool| {
                move |t: &mut Self, v: &bool, _: &mut Window, cx: &mut Context<Self>| {
                    let v = *v;
                    t.model.update(cx, |m, cx| {
                        m.update_settings(cx, |s| {
                            let mut e = s.notify_setting(kind);
                            if sound { e.sound = v } else { e.popup = v }
                            s.notify.insert(kind, e);
                        })
                    });
                }
            };
            rows = rows.child(
                h_flex()
                    .gap_4()
                    .items_center()
                    .min_h(px(34.))
                    .child(
                        h_flex()
                            .flex_1()
                            .gap_3()
                            .items_center()
                            .child(icon(kind.icon(), 15., colors().muted))
                            .child(div().text_sm().child(kind.title())),
                    )
                    .child(
                        div()
                            .w(px(60.))
                            .child(Switch::new(("snd", i)).checked(setting.sound).on_click(cx.listener(toggle(true)))),
                    )
                    .child(div().w(px(60.)).when(kind.can_popup(), |d| {
                        d.child(Switch::new(("pop", i)).checked(setting.popup).on_click(cx.listener(toggle(false))))
                    }))
                    .child(
                        div()
                            .id(("play", i))
                            .w(px(30.))
                            .h(px(26.))
                            .rounded(px(5.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .hover(|d| d.bg(hover_bg()))
                            .tooltip(|w, cx| Tooltip::new("Play").build(w, cx))
                            .child(icon("play", 13., colors().muted))
                            .on_click(cx.listener(move |t, _, _, cx| {
                                crate::notify::preview(kind, &t.model.read(cx).settings);
                            })),
                    ),
            );
        }
        v_flex()
            .gap_6()
            .child(
                Group::new("volume-2", "Sounds")
                    .child(form::slider("Volume", format!("{:.0}%", s.sound_volume * 100.), Slider::new(&self.sounds)))
                    .child(item(
                        "Read events aloud",
                        Some(if self.can_speak {
                            "Says \"Alex poked you\" instead of playing the sound (espeak-ng)".into()
                        } else {
                            "Needs espeak-ng".into()
                        }),
                        Switch::new("speak").checked(s.speak_events).disabled(!self.can_speak).on_click(cx.listener(
                            |t, v: &bool, _, cx| {
                                let v = *v;
                                t.model.update(cx, |m, cx| m.update_settings(cx, |s| s.speak_events = v));
                            },
                        )),
                    )),
            )
            .child(
                Group::new("bell-ring", "Events")
                    .child(block(rows))
                    .hint("Chat popups are skipped while the Wisp window has focus."),
            )
            .into_any_element()
    }
}
