//! Privilege keys (tokens on Banshee): making, listing and using them.

use super::*;

impl AdminView {
    pub(super) fn load_keys(&mut self, cx: &mut Context<Self>) {
        ask(&self.model, self.con, Query::Tokens, cx, |this: &mut Self, r, _| {
            this.keys = Some(r.and_then(|a| match a {
                Answer::Tokens(t) => Ok(t),
                _ => Err("unexpected answer".into()),
            }));
        });
    }

    pub(super) fn keys_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let _ = window;
        let (sgroups, cgroups, channels) = match self.server(cx) {
            Some(s) => (s.groups.clone(), s.channel_groups.clone(), s.channels.clone()),
            None => Default::default(),
        };
        let group_name = |server: bool, id: u64| {
            let l = if server { &sgroups } else { &cgroups };
            l.iter().find(|g| g.id == id).map(|g| g.badge.name.clone()).unwrap_or_else(|| format!("group {id}"))
        };
        let mut pick = h_flex().gap_2().flex_wrap();
        for g in &sgroups {
            let id = g.id;
            pick = pick.child(
                chip(SharedString::from(format!("ks-{id}")), g.badge.name.clone(), self.key_group == Some((true, id)))
                    .on_click(cx.listener(move |t, _, _, cx| {
                        t.key_group = Some((true, id));
                        cx.notify();
                    })),
            );
        }
        for g in &cgroups {
            let id = g.id;
            pick = pick.child(
                chip(SharedString::from(format!("kc-{id}")), format!("{} (channel)", g.badge.name), self.key_group == Some((false, id)))
                    .on_click(cx.listener(move |t, _, _, cx| {
                        t.key_group = Some((false, id));
                        cx.notify();
                    })),
            );
        }
        let mut chan_pick = h_flex().gap_2().flex_wrap();
        if matches!(self.key_group, Some((false, _))) {
            for c in channels.iter().filter(|c| !c.name.starts_with('[')) {
                let id = c.id;
                chan_pick = chan_pick.child(
                    chip(SharedString::from(format!("kch-{id}")), c.name.clone(), self.key_channel == Some(id)).on_click(
                        cx.listener(move |t, _, _, cx| {
                            t.key_channel = Some(id);
                            cx.notify();
                        }),
                    ),
                );
            }
        }
        let create = v_flex()
            .gap_3()
            .p_4()
            .rounded(px(8.))
            .border_1()
            .border_color(border())
            .bg(form::card_bg())
            .child(h_flex().gap_2().items_center().child(icon("key-round", 14., colors().muted)).child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child("Create a key")))
            .child(label("Whoever uses the key joins this group."))
            .child(pick)
            .when(matches!(self.key_group, Some((false, _))), |d| d.child(label("In channel")).child(chan_pick))
            .child(
                h_flex().gap_2().child(div().flex_1().child(Input::new(&self.key_desc))).child(
                    Button::new("key-add").label("Create").small().primary().on_click(cx.listener(|t, _, _, cx| {
                        let Some((server_group, group)) = t.key_group else {
                            t.error = Some("Pick a group first.".into());
                            cx.notify();
                            return;
                        };
                        let channel = if server_group { 0 } else { t.key_channel.unwrap_or(0) };
                        if !server_group && channel == 0 {
                            t.error = Some("Pick the channel for the channel group.".into());
                            cx.notify();
                            return;
                        }
                        t.error = None;
                        let description = value(&t.key_desc, cx);
                        t.write(AdminWrite::AddToken { server_group, group, channel, description }, cx);
                        later(cx, |t: &mut Self, cx| t.load_keys(cx));
                    })),
                ),
            );
        let use_key = h_flex().gap_2().child(div().flex_1().child(Input::new(&self.key_use))).child(
            Button::new("key-use").label("Use key").small().on_click(cx.listener(|t, _, window, cx| {
                let token = value(&t.key_use, cx);
                if !token.is_empty() {
                    t.write(AdminWrite::UseToken { token }, cx);
                    t.key_use.update(cx, |i, cx| i.set_value("", window, cx));
                }
            })),
        );
        let mut list = v_flex();
        match &self.keys {
            None => list = list.child(label("Loading keys...")),
            Some(Err(e)) => list = list.child(error_box(e)),
            Some(Ok(k)) if k.is_empty() => list = list.child(label("No unused keys.")),
            Some(Ok(k)) => {
                for (i, key) in k.iter().enumerate() {
                    let (token, copy) = (key.token.clone(), key.token.clone());
                    let channel = channels.iter().find(|c| c.id == key.channel).map(|c| format!(" in {}", c.name)).unwrap_or_default();
                    list = list.child(
                        h_flex()
                            .gap_3()
                            .items_center()
                            .py_2()
                            .border_b_1()
                            .border_color(alpha(ink(), 0x08))
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .child(div().text_sm().truncate().child(format!("{}{channel}", group_name(key.server_group, key.group))))
                                    .child(label(format!(
                                        "{} · {}{}",
                                        key.token,
                                        key.created,
                                        if key.description.is_empty() { String::new() } else { format!(" · {}", key.description) }
                                    )).truncate()),
                            )
                            .child(icon_btn(("kcopy", i), "copy", colors().muted, "Copy key").on_click(move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()));
                            }))
                            .child(icon_btn(("kdel", i), "trash", colors().red, "Delete key").on_click(cx.listener(move |t, _, _, cx| {
                                t.write(AdminWrite::DelToken { token: token.clone() }, cx);
                                later(cx, |t: &mut Self, cx| t.load_keys(cx));
                            }))),
                    );
                }
            }
        }
        v_flex().gap_5().child(create).child(use_key).child(list).into_any_element()
    }
}
