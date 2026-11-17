//! Offline messages: reading, writing and deleting them.

use super::*;

impl AdminView {
    pub(super) fn load_inbox(&mut self, cx: &mut Context<Self>) {
        ask(&self.model, self.con, Query::Inbox, cx, |this: &mut Self, r, _| {
            this.inbox = Some(r.and_then(|a| match a {
                Answer::Inbox(mut l) => {
                    l.sort_by(|a, b| b.time.cmp(&a.time));
                    Ok(l)
                }
                _ => Err("unexpected answer".into()),
            }));
        });
    }

    pub(super) fn open_message(&mut self, id: u32, cx: &mut Context<Self>) {
        ask(&self.model, self.con, Query::InboxMessage(id), cx, move |this: &mut Self, r, cx| match r {
            Ok(Answer::Inbox(mut l)) if !l.is_empty() => {
                this.open_message = Some(l.remove(0));
                this.write(AdminWrite::MarkOffline { id, read: true }, cx);
                if let Some(Ok(list)) = &mut this.inbox {
                    if let Some(m) = list.iter_mut().find(|m| m.id == id) {
                        m.read = true;
                    }
                }
            }
            Ok(_) => this.error = Some("That message is gone.".into()),
            Err(e) => this.error = Some(e),
        });
    }

    /// A unique id as a name, when they're online.
    pub(super) fn who(&self, uid: &str, cx: &App) -> String {
        self.server(cx)
            .and_then(|s| s.channels.iter().flat_map(|c| &c.clients).find(|c| c.uid == uid))
            .map(|c| c.name.clone())
            .unwrap_or_else(|| uid.chars().take(12).collect::<String>() + "…")
    }

    pub(super) fn inbox_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let _ = window;
        let compose = v_flex()
            .gap_2()
            .p_4()
            .rounded(px(8.))
            .border_1()
            .border_color(border())
            .bg(form::card_bg())
            .child(h_flex().gap_2().items_center().child(icon("mail", 14., colors().muted)).child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child("New offline message")))
            .child(label("They get it the next time they connect, even if they're offline now."))
            .child(Input::new(&self.compose_to))
            .child(Input::new(&self.compose_subject))
            .child(gpui_kit::component::input::Textarea::new(&self.compose_text))
            .child(
                h_flex()
                    .gap_2()
                    .justify_end()
                    .child(Button::new("cmp-cancel").label("Cancel").small().ghost().on_click(cx.listener(|t, _, _, cx| {
                        t.composing = false;
                        cx.notify();
                    })))
                    .child(Button::new("cmp-send").label("Send").small().primary().on_click(cx.listener(|t, _, window, cx| {
                        let (uid, subject) = (value(&t.compose_to, cx), value(&t.compose_subject, cx));
                        let message = t.compose_text.read(cx).value().trim().to_string();
                        if uid.is_empty() || message.is_empty() {
                            t.error = Some("Needs a unique id and a message.".into());
                            cx.notify();
                            return;
                        }
                        t.error = None;
                        let subject =
                            if subject.is_empty() { crate::net::strip_bbcode(&message).chars().take(40).collect() } else { subject };
                        t.write(AdminWrite::SendOffline { uid, subject, message }, cx);
                        t.compose_text.update(cx, |i, cx| i.set_value("", window, cx));
                        t.composing = false;
                        cx.notify();
                    }))),
            );
        let mut list = v_flex();
        match &self.inbox {
            None => list = list.child(label("Loading...")),
            Some(Err(e)) => list = list.child(error_box(e)),
            Some(Ok(l)) if l.is_empty() => list = list.child(label("No offline messages.")),
            Some(Ok(l)) => {
                for (i, m) in l.iter().enumerate() {
                    let (id, from, from_uid) = (m.id, self.who(&m.from_uid, cx), m.from_uid.clone());
                    let open = self.open_message.as_ref().filter(|o| o.id == m.id).cloned();
                    list = list.child(
                        v_flex()
                            .border_b_1()
                            .border_color(alpha(ink(), 0x08))
                            .child(
                                h_flex()
                                    .id(("msg", i))
                                    .gap_3()
                                    .items_center()
                                    .py_2()
                                    .cursor_pointer()
                                    .hover(|d| d.bg(hover_bg()))
                                    .on_click(cx.listener(move |t, _, _, cx| {
                                        if t.open_message.as_ref().is_some_and(|o| o.id == id) {
                                            t.open_message = None;
                                            cx.notify();
                                        } else {
                                            t.open_message(id, cx);
                                        }
                                    }))
                                    .child(div().size(px(6.)).rounded_full().when(!m.read, |d| d.bg(rgb(colors().accent))))
                                    .child(
                                        v_flex()
                                            .flex_1()
                                            .min_w_0()
                                            .child(
                                                div()
                                                    .text_sm()
                                                    .truncate()
                                                    .when(!m.read, |d| d.font_weight(FontWeight::SEMIBOLD))
                                                    .child(if m.subject.is_empty() { "(no subject)".to_string() } else { m.subject.clone() }),
                                            )
                                            .child(label(format!("{from} · {}", m.time))),
                                    )
                                    .child(icon_btn(("mreply", i), "reply", colors().muted, "Reply").on_click(cx.listener(
                                        move |t, _, window, cx| {
                                            cx.stop_propagation();
                                            t.compose_to.update(cx, |i, cx| i.set_value(from_uid.clone(), window, cx));
                                            t.composing = true;
                                            cx.notify();
                                        },
                                    )))
                                    .child(icon_btn(("mdel", i), "trash", colors().red, "Delete").on_click(cx.listener(move |t, _, _, cx| {
                                        cx.stop_propagation();
                                        t.write(AdminWrite::DelOffline { id }, cx);
                                        if let Some(Ok(l)) = &mut t.inbox {
                                            l.retain(|m| m.id != id);
                                        }
                                        cx.notify();
                                    }))),
                            )
                            .when_some(open, |d, o| {
                                d.child(
                                    div()
                                        .pl_5()
                                        .pb_3()
                                        .text_sm()
                                        .text_color(rgb(colors().soft))
                                        .child(crate::ui::bbcode::parse(&o.message).element(("msg-body", i))),
                                )
                            }),
                    );
                }
            }
        }
        v_flex()
            .gap_4()
            .max_w(px(760.))
            .child(
                h_flex()
                    .gap_2()
                    .justify_end()
                    .child(Button::new("inbox-reload").label("Reload").small().ghost().on_click(cx.listener(|t, _, _, cx| t.load_inbox(cx))))
                    .when(!self.composing, |d| {
                        d.child(Button::new("inbox-new").label("New message").small().primary().on_click(cx.listener(|t, _, _, cx| {
                            t.composing = true;
                            cx.notify();
                        })))
                    }),
            )
            .when(self.composing, |d| d.child(compose))
            .child(list)
            .into_any_element()
    }
}
