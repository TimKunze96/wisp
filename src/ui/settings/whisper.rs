//! Settings → Whisper: lists of people and channels to whisper to, each with a key.

use super::*;
use super::keys::key_chip;

impl SettingsView {
    pub(super) fn whisper_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let lists = self.model.read(cx).settings.whisper_lists.clone();
        // a name field per list, kept across renders
        for l in &lists {
            if self.whisper_names.contains_key(&l.id) {
                continue;
            }
            let id = l.id.clone();
            let input = cx.new(|cx| InputState::new(window, cx).placeholder("List name").default_value(l.name.clone()));
            cx.subscribe(&input, move |t, input, ev: &gpui_kit::component::input::InputEvent, cx| {
                use gpui_kit::component::input::InputEvent;
                match ev {
                    InputEvent::Change => {
                        let name = input.read(cx).value().trim().to_string();
                        let id = id.clone();
                        t.model.update(cx, |m, cx| {
                            m.update_settings(cx, |s| {
                                if let Some(l) = s.whisper_lists.iter_mut().find(|l| l.id == id) {
                                    l.name = name;
                                }
                            })
                        });
                    }
                    // the bind's description carries the name
                    InputEvent::Blur | InputEvent::PressEnter { .. } => t.apply_hotkeys(cx),
                    _ => {}
                }
            })
            .detach();
            self.whisper_names.insert(l.id.clone(), input);
        }

        let mut col = v_flex().gap_4();
        if lists.is_empty() {
            col = col.child(label("No whisper lists yet."));
        }
        for (i, l) in lists.iter().enumerate() {
            let id = l.id.clone();
            let recording = self.recording == Some(Rec::Whisper(id.clone()));
            let conflict = (!l.hotkey.is_empty())
                .then(|| self.taken.iter().find(|(t, _)| *t == hotkeys::normalize(&l.hotkey)))
                .flatten()
                .map(|(_, d)| d.clone());
            let mut targets = h_flex().flex_wrap().gap_2();
            if l.targets.is_empty() {
                targets = targets.child(label("No targets. Right-click people or channels in the main window to add them.").w_full());
            }
            for (j, t) in l.targets.iter().enumerate() {
                let (id, t2) = (id.clone(), t.clone());
                targets = targets.child(
                    h_flex()
                        .gap_1p5()
                        .items_center()
                        .h(px(28.))
                        .pl_2p5()
                        .pr_1()
                        .rounded(px(5.))
                        .border_1()
                        .border_color(border())
                        .bg(rgb(colors().raised))
                        .child(icon(t.icon(), 13., colors().muted))
                        .child(div().text_sm().child(t.label()))
                        .child(
                            div()
                                .id(SharedString::from(format!("wt-{i}-{j}")))
                                .rounded(px(3.))
                                .p_0p5()
                                .cursor_pointer()
                                .hover(|d| d.bg(hover_bg()))
                                .child(icon("x", 12., colors().dim))
                                .on_click(cx.listener(move |t, _, _, cx| {
                                    let (id, target) = (id.clone(), t2.clone());
                                    t.model.update(cx, |m, cx| {
                                        m.update_settings(cx, |s| {
                                            if let Some(l) = s.whisper_lists.iter_mut().find(|l| l.id == id) {
                                                l.targets.retain(|x| *x != target);
                                            }
                                        })
                                    });
                                })),
                        ),
                );
            }
            // the targets that aren't a person or channel
            let mut extras = h_flex().gap_2();
            for (k, extra) in [WhisperTarget::Commanders, WhisperTarget::Subchannels, WhisperTarget::ParentChannel]
                .into_iter()
                .enumerate()
            {
                if l.targets.contains(&extra) {
                    continue;
                }
                let id = id.clone();
                extras = extras.child(
                    chip(SharedString::from(format!("wx-{i}-{k}")), format!("+ {}", extra.label()), false).on_click(
                        cx.listener(move |t, _, _, cx| {
                            let (id, extra) = (id.clone(), extra.clone());
                            t.model.update(cx, |m, cx| {
                                m.update_settings(cx, |s| {
                                    if let Some(l) = s.whisper_lists.iter_mut().find(|l| l.id == id) {
                                        l.targets.push(extra);
                                    }
                                })
                            });
                        }),
                    ),
                );
            }
            let key_chip = key_chip(
                if l.hotkey.is_empty() { "No key".into() } else { l.hotkey.clone() },
                !l.hotkey.is_empty(),
                recording,
            )
            .min_w(px(120.));
            let (rec_id, clear_id, del_id) = (id.clone(), id.clone(), id.clone());
            col = col.child(
                card()
                    .gap_3()
                    .p_4()
                    .child(
                        h_flex()
                            .gap_3()
                            .items_center()
                            .child(icon("radio", 16., colors().muted))
                            .child(div().flex_1().child(Input::new(&self.whisper_names[&l.id])))
                            .child(key_chip)
                            .child(
                                Button::new(SharedString::from(format!("wrec-{i}")))
                                    .label(if recording { "Cancel" } else { "Record" })
                                    .small()
                                    .ghost()
                                    .on_click(cx.listener(move |t, _, window, cx| {
                                        let rec = Some(Rec::Whisper(rec_id.clone()));
                                        t.recording = if t.recording == rec { None } else { rec };
                                        window.focus(&t.focus, cx);
                                        cx.notify();
                                    })),
                            )
                            .when(!l.hotkey.is_empty(), |d| {
                                d.child(icon_btn(SharedString::from(format!("wclr-{i}")), "x", colors().muted, "Clear key").on_click(
                                    cx.listener(move |t, _, _, cx| t.set_hotkey(Rec::Whisper(clear_id.clone()), None, cx)),
                                ))
                            })
                            .child(danger_btn(SharedString::from(format!("wdel-{i}")), "Delete list").on_click(cx.listener(
                                move |t, _, _, cx| {
                                    let id = del_id.clone();
                                    t.whisper_names.remove(&id);
                                    t.model.update(cx, |m, cx| m.update_settings(cx, |s| s.whisper_lists.retain(|l| l.id != id)));
                                    t.apply_hotkeys(cx);
                                },
                            ))),
                    )
                    .when_some(conflict, |d, c| {
                        d.child(
                            h_flex()
                                .gap_1p5()
                                .items_center()
                                .child(icon("triangle-alert", 12., colors().amber))
                                .child(label(format!("Also bound in Hyprland: {c}")).text_color(rgb(colors().amber))),
                        )
                    })
                    .child(targets)
                    .child(extras),
            );
        }
        v_flex()
            .gap_4()
            .child(
                h_flex()
                    .gap_4()
                    .items_start()
                    .child(label(
                        "Hold a list's key to talk only to its targets instead of your channel. Add people and \
                         channels by right-clicking them in the main window. Groups and commanders are looked up \
                         when you press the key.",
                    ).flex_1().min_w_0())
                    .child(Button::new("new-whisper").label("New list").small().primary().on_click(cx.listener(
                        |t, _, _, cx| {
                            t.model.update(cx, |m, cx| {
                                m.update_settings(cx, |s| {
                                    let n = s.whisper_lists.len() + 1;
                                    s.whisper_lists.push(WhisperList {
                                        id: new_id(),
                                        name: format!("Whisper {n}"),
                                        ..Default::default()
                                    });
                                })
                            });
                        },
                    ))),
            )
            .when_some(self.hotkey_error.clone(), |d, e| d.child(notice("triangle-alert", colors().red, e)))
            .child(col)
            .into_any_element()
    }
}
