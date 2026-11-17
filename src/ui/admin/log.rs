//! The server log.

use super::*;

impl AdminView {
    pub(super) fn load_log(&mut self, cx: &mut Context<Self>) {
        ask(&self.model, self.con, Query::Log { lines: 100 }, cx, |this: &mut Self, r, _| {
            this.log = Some(r.and_then(|a| match a {
                Answer::Log(l) => Ok(l),
                _ => Err("unexpected answer".into()),
            }));
        });
    }

    pub(super) fn log_tab(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let mut col = v_flex().gap_1();
        match &self.log {
            None => col = col.child(label("Loading the log...")),
            Some(Err(e)) => col = col.child(error_box(e)),
            Some(Ok(lines)) => {
                for l in lines {
                    // 2026-10-17 00:42:30.751959|INFO    |PktHandler    |1  |Dropping client ...
                    let parts: Vec<&str> = l.splitn(5, '|').collect();
                    let (time, level, what, text) = match parts.as_slice() {
                        [t, lv, w, _, x] => (t.get(..16).unwrap_or(t), lv.trim(), w.trim(), *x),
                        _ => ("", "", "", l.as_str()),
                    };
                    let color = match level {
                        "ERROR" | "CRITICAL" => colors().red,
                        "WARNING" => colors().amber,
                        _ => colors().muted,
                    };
                    col = col.child(
                        h_flex()
                            .gap_3()
                            .items_start()
                            .child(div().w(px(120.)).flex_none().text_xs().text_color(rgb(colors().dim)).child(time.to_string()))
                            .child(div().w(px(100.)).flex_none().text_xs().text_color(rgb(color)).child(what.to_string()))
                            .child(div().flex_1().text_xs().text_color(rgb(colors().soft)).child(text.to_string())),
                    );
                }
            }
        }
        v_flex()
            .gap_3()
            .child(h_flex().justify_end().child(
                Button::new("log-reload").label("Reload").small().ghost().on_click(cx.listener(|t, _, _, cx| t.load_log(cx))),
            ))
            .child(col)
            .into_any_element()
    }
}
