//! The server page: status, settings and temporary passwords.

use super::*;

impl AdminView {
    pub(super) fn load_server(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (tx, rx) = oneshot::channel();
        self.model.read(cx).net.send(Cmd::ServerInfo { con: self.con, reply: tx });
        let window_handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            let Ok(info) = rx.await else { return };
            let _ = window_handle.update(cx, |_, window, cx| {
                let _ = this.update(cx, |t, cx| t.fill_server(info, window, cx));
            });
        })
        .detach();
        ask(&self.model, self.con, Query::ServerConnection, cx, |this: &mut Self, r, _| {
            this.connection = Some(r.and_then(|a| match a {
                Answer::ServerConnection(c) => Ok(c),
                _ => Err("unexpected answer".into()),
            }));
        });
        ask(&self.model, self.con, Query::TempPasswords, cx, |this: &mut Self, r, _| {
            this.temp = Some(r.and_then(|a| match a {
                Answer::TempPasswords(t) => Ok(t),
                _ => Err("unexpected answer".into()),
            }));
        });
    }

    pub(super) fn fill_server(&mut self, info: ServerInfo, window: &mut Window, cx: &mut Context<Self>) {
        let vals: [(&str, String); 12] = [
            ("name", info.name.clone()),
            ("phonetic_name", info.phonetic_name.clone()),
            ("welcome_message", info.welcome_message.clone()),
            ("max_clients", info.max_clients.to_string()),
            ("reserved_slots", String::new()),
            ("password", String::new()),
            ("hostmessage", info.hostmessage.clone()),
            ("hostbanner_url", info.hostbanner_url.clone()),
            ("hostbanner_gfx_url", info.hostbanner_gfx_url.clone()),
            ("hostbutton_tooltip", info.hostbutton_tooltip.clone()),
            ("hostbutton_url", info.hostbutton_url.clone()),
            ("temp_delete_delay", info.temp_delete_delay.to_string()),
        ];
        for (k, v) in vals {
            if let Some(i) = self.srv.get(k) {
                i.update(cx, |i, cx| i.set_value(v, window, cx));
            }
        }
        self.host_mode = info.hostmessage_mode;
        self.info = Some(info);
        cx.notify();
    }

    pub(super) fn save_server(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(info) = self.info.clone() else { return };
        let v = |k: &str| self.srv.get(k).map(|i| value(i, cx)).unwrap_or_default();
        let changed = |new: String, old: &str| (new != old).then_some(new);
        let num = |k: &str| v(k).parse::<u64>().ok();
        let p = ServerProps {
            name: changed(v("name"), &info.name),
            phonetic_name: changed(v("phonetic_name"), &info.phonetic_name),
            welcome_message: changed(v("welcome_message"), &info.welcome_message),
            max_clients: num("max_clients").map(|n| n as u16).filter(|n| *n != info.max_clients),
            reserved_slots: num("reserved_slots").map(|n| n as u16),
            password: match v("password").as_str() {
                "" => None,
                "-" => Some(String::new()),
                pw => Some(pw.to_string()),
            },
            hostmessage: changed(v("hostmessage"), &info.hostmessage),
            hostmessage_mode: (self.host_mode != info.hostmessage_mode).then_some(self.host_mode),
            hostbanner_url: changed(v("hostbanner_url"), &info.hostbanner_url),
            hostbanner_gfx_url: changed(v("hostbanner_gfx_url"), &info.hostbanner_gfx_url),
            hostbutton_tooltip: changed(v("hostbutton_tooltip"), &info.hostbutton_tooltip),
            hostbutton_url: changed(v("hostbutton_url"), &info.hostbutton_url),
            temp_delete_delay: num("temp_delete_delay").filter(|d| *d != info.temp_delete_delay),
            ..Default::default()
        };
        if p == ServerProps::default() {
            self.error = Some("Nothing changed.".into());
            cx.notify();
            return;
        }
        self.error = None;
        self.write(AdminWrite::EditServer(Box::new(p)), cx);
        let handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(600)).await;
            let _ = handle.update(cx, |_, window, cx| {
                let _ = this.update(cx, |t, cx| t.load_server(window, cx));
            });
        })
        .detach();
    }

    pub(super) fn server_tab(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let Some(info) = self.info.clone() else { return label("Loading...").into_any_element() };
        let mut fields = form::Group::new("settings-2", "Settings").action(
            Button::new("srv-save").label("Save").small().primary().on_click(cx.listener(|t, _, window, cx| t.save_server(window, cx))),
        );
        for (key, text, _) in SERVER_FIELDS {
            fields = fields.child(form::field(text, Input::new(&self.srv[key])));
            if key == "hostmessage" {
                let mut modes = h_flex().gap_2().flex_wrap();
                for (i, m) in ["Off", "In the log", "Popup", "Popup, then disconnect"].into_iter().enumerate() {
                    let i = i as u8;
                    modes = modes.child(chip(("hm", i as usize), m, self.host_mode == i).on_click(cx.listener(move |t, _, _, cx| {
                        t.host_mode = i;
                        cx.notify();
                    })));
                }
                fields = fields.child(form::field("Host message shows", modes));
            }
        }
        let mut temp = v_flex();
        match &self.temp {
            None => temp = temp.child(form::block(label("Loading..."))),
            Some(Err(e)) => temp = temp.child(form::block(error_box(e))),
            Some(Ok(t)) if t.is_empty() => temp = temp.child(form::block(label("No temporary passwords."))),
            Some(Ok(t)) => {
                for (i, p) in t.iter().enumerate() {
                    let pw = p.password.clone();
                    temp = temp.child(
                        h_flex()
                            .gap_3()
                            .items_center()
                            .px_4()
                            .py_2()
                            .child(div().flex_1().text_sm().child(format!("{} · {}", p.password, p.description)))
                            .child(label(format!("until {} · {}", p.ends, p.by)))
                            .child(icon_btn(("tpd", i), "x", colors().muted, "Remove").on_click(cx.listener(move |t, _, window, cx| {
                                t.write(AdminWrite::DelTempPassword { password: pw.clone() }, cx);
                                let handle = window.window_handle();
                                cx.spawn(async move |this, cx| {
                                    cx.background_executor().timer(Duration::from_millis(400)).await;
                                    let _ = handle.update(cx, |_, window, cx| {
                                        let _ = this.update(cx, |t, cx| t.load_server(window, cx));
                                    });
                                })
                                .detach();
                            }))),
                    );
                }
            }
        }
        let stat = |name: &'static str, value: String| {
            h_flex().justify_between().px_4().py_2().child(label(name)).child(div().text_sm().child(value))
        };
        let bytes = |n: u64| match n {
            n if n >= 1 << 30 => format!("{:.1} GB", n as f64 / (1u64 << 30) as f64),
            n if n >= 1 << 20 => format!("{:.1} MB", n as f64 / (1u64 << 20) as f64),
            n => format!("{:.0} KB", n as f64 / 1024.0),
        };
        let connection = match &self.connection {
            None => form::block(label("Loading...")).into_any_element(),
            Some(Err(e)) => form::block(error_box(e)).into_any_element(),
            Some(Ok(c)) => v_flex()
                .py_1()
                .child(stat("Uptime", fmt_duration(c.uptime_secs)))
                .child(stat("Ping (us to the server)", format!("{:.0} ms", c.ping_ms)))
                .child(stat("Packet loss", format!("{:.1}%", c.packet_loss)))
                .child(stat("Bandwidth now (out / in)", format!("{} / {} per second", bytes(c.sent_per_sec), bytes(c.received_per_sec))))
                .child(stat("Last minute (out / in)", format!("{} / {} per second", bytes(c.sent_per_min), bytes(c.received_per_min))))
                .child(stat("Since start (out / in)", format!("{} / {}", bytes(c.sent_total), bytes(c.received_total))))
                .child(stat("File transfers (out / in)", format!("{} / {}", bytes(c.files_sent_total), bytes(c.files_received_total))))
                .into_any_element(),
        };
        let rights = self.server(cx).map(|s| s.rights.clone()).unwrap_or_default();
        v_flex()
            .gap_6()
            .max_w(px(760.))
            .when(rights.connection_info, |d| d.child(
                form::Group::new("activity", "Status")
                    .hint(format!("{} {} · created {}", info.platform, info.version, info.created))
                    .child(connection),
            ))
            .when(rights.edit_server, |d| d.child(fields))
            .when(rights.temp_passwords, |d| d.child(
                form::Group::new("key-round", "Temporary passwords")
                    .hint("Extra server passwords that expire after a day; handy for guests.")
                    .child(
                        h_flex()
                            .gap_2()
                            .px_4()
                            .py_3()
                            .child(div().flex_1().child(Input::new(&self.temp_pw)))
                            .child(div().flex_1().child(Input::new(&self.temp_desc)))
                            .child(Button::new("tp-add").label("Add").small().on_click(cx.listener(|t, _, window, cx| {
                                let (password, description) = (value(&t.temp_pw, cx), value(&t.temp_desc, cx));
                                if password.is_empty() {
                                    return;
                                }
                                t.write(AdminWrite::AddTempPassword { password, description, secs: 86_400 }, cx);
                                t.temp_pw.update(cx, |i, cx| i.set_value("", window, cx));
                                t.temp_desc.update(cx, |i, cx| i.set_value("", window, cx));
                                let handle = window.window_handle();
                                cx.spawn(async move |this, cx| {
                                    cx.background_executor().timer(Duration::from_millis(400)).await;
                                    let _ = handle.update(cx, |_, window, cx| {
                                        let _ = this.update(cx, |t, cx| t.load_server(window, cx));
                                    });
                                })
                                .detach();
                            }))),
                    )
                    .child(temp),
            ))
            .into_any_element()
    }
}
