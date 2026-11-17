//! Complaints people made about others.

use super::*;

impl AdminView {
    pub(super) fn load_complaints(&mut self, cx: &mut Context<Self>) {
        ask(&self.model, self.con, Query::Complaints, cx, |this: &mut Self, r, _| {
            this.complaints = Some(r.and_then(|a| match a {
                Answer::Complaints(c) => Ok(c),
                _ => Err("unexpected answer".into()),
            }));
        });
    }

    pub(super) fn complaints_tab(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let mut list = v_flex();
        match &self.complaints {
            None => list = list.child(label("Loading complaints...")),
            Some(Err(e)) => list = list.child(error_box(e)),
            Some(Ok(c)) if c.is_empty() => list = list.child(label("No complaints.")),
            Some(Ok(c)) => {
                for (i, x) in c.iter().enumerate() {
                    let (target, from) = (x.target_db_id, x.from_db_id);
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
                                    .child(div().text_sm().child(format!("{} about {}", x.from, x.target)))
                                    .child(label(format!("{} · {}", x.time, x.message))),
                            )
                            .child(icon_btn(("cdel", i), "x", colors().muted, "Remove").on_click(cx.listener(move |t, _, _, cx| {
                                t.write(AdminWrite::DelComplaint { target, from }, cx);
                                later(cx, |t: &mut Self, cx| t.load_complaints(cx));
                            }))),
                    );
                }
            }
        }
        list.into_any_element()
    }
}
