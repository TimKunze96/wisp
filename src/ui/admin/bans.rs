//! Bans: the list, and adding one by address, name or key.

use super::*;

impl AdminView {
    pub(super) fn load_bans(&mut self, cx: &mut Context<Self>) {
        ask(&self.model, self.con, Query::Bans, cx, |this: &mut Self, r, _| {
            this.bans = Some(r.and_then(|a| match a {
                Answer::Bans(b) => Ok(b),
                _ => Err("unexpected answer".into()),
            }));
        });
    }

    pub(super) fn bans_tab(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let mut durations = h_flex().gap_2().flex_wrap();
        for (i, (text, secs)) in
            [("1 hour", Some(3600)), ("1 day", Some(86_400)), ("1 week", Some(604_800)), ("30 days", Some(2_592_000)), ("Permanent", None)]
                .into_iter()
                .enumerate()
        {
            durations = durations.child(chip(("bd", i), text, self.ban_secs == secs).on_click(cx.listener(move |t, _, _, cx| {
                t.ban_secs = secs;
                cx.notify();
            })));
        }
        let form = v_flex()
            .gap_2()
            .p_4()
            .rounded(px(8.))
            .border_1()
            .border_color(border())
            .bg(form::card_bg())
            .child(h_flex().gap_2().items_center().child(icon("ban", 14., colors().muted)).child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child("Add a ban")))
            .child(h_flex().gap_2().child(div().flex_1().child(Input::new(&self.ban_ip))).child(div().flex_1().child(Input::new(&self.ban_name))))
            .child(Input::new(&self.ban_uid))
            .child(Input::new(&self.ban_reason))
            .child(
                h_flex().gap_3().items_center().child(durations).child(div().flex_1()).child(
                    Button::new("ban-add").label("Ban").small().danger().on_click(cx.listener(|t, _, window, cx| {
                        let (ip, name, uid, reason) =
                            (value(&t.ban_ip, cx), value(&t.ban_name, cx), value(&t.ban_uid, cx), value(&t.ban_reason, cx));
                        if ip.is_empty() && name.is_empty() && uid.is_empty() {
                            t.error = Some("Give an IP, a name pattern or a unique id.".into());
                            cx.notify();
                            return;
                        }
                        t.error = None;
                        t.write(AdminWrite::AddBan { ip, name, uid, secs: t.ban_secs, reason }, cx);
                        for i in [&t.ban_ip, &t.ban_name, &t.ban_uid, &t.ban_reason] {
                            i.update(cx, |i, cx| i.set_value("", window, cx));
                        }
                        later(cx, |t: &mut Self, cx| t.load_bans(cx));
                    })),
                ),
            );
        let mut list = v_flex();
        match &self.bans {
            None => list = list.child(label("Loading bans...")),
            Some(Err(e)) => list = list.child(error_box(e)),
            Some(Ok(b)) if b.is_empty() => list = list.child(label("Nobody is banned.")),
            Some(Ok(b)) => {
                for ban in b {
                    let id = ban.id;
                    let who = [&ban.last_nickname, &ban.name, &ban.ip, &ban.uid].into_iter().find(|s| !s.is_empty()).cloned().unwrap_or_default();
                    let mut what = Vec::new();
                    if !ban.ip.is_empty() {
                        what.push(format!("IP {}", ban.ip));
                    }
                    if !ban.name.is_empty() {
                        what.push(format!("name {}", ban.name));
                    }
                    if !ban.uid.is_empty() {
                        what.push("unique id".to_string());
                    }
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
                                    .child(div().text_sm().truncate().child(who))
                                    .child(label(format!(
                                        "{} · by {} · {}{}",
                                        what.join(", "),
                                        ban.by,
                                        ban.created,
                                        if ban.reason.is_empty() { String::new() } else { format!(" · {}", ban.reason) }
                                    )).truncate()),
                            )
                            .child(label(human(ban.secs)))
                            .child(icon_btn(("unban", id as usize), "x", colors().muted, "Remove ban").on_click(cx.listener(
                                move |t, _, _, cx| {
                                    t.write(AdminWrite::DelBan { id }, cx);
                                    later(cx, |t: &mut Self, cx| t.load_bans(cx));
                                },
                            ))),
                    );
                }
            }
        }
        v_flex().gap_5().child(form).child(list).into_any_element()
    }
}
