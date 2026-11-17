//! Settings → Servers: the saved servers, and the form to add or edit one.

use super::*;

impl SettingsView {
    pub(in crate::ui) fn edit(&mut self, b: Bookmark, is_new: bool, window: &mut Window, cx: &mut Context<Self>) {
        let field = |v: String, ph: &str, masked: bool, window: &mut Window, cx: &mut Context<Self>| {
            let ph = ph.to_string();
            cx.new(|cx| InputState::new(window, cx).placeholder(ph).masked(masked).default_value(v))
        };
        self.form = Some(BookmarkForm {
            id: b.id.clone(),
            is_new,
            label: field(b.label, "Shown in the sidebar (optional)", false, window, cx),
            address: field(b.address, "ts.example.com or ts.example.com:9987", false, window, cx),
            password: field(b.password, "Leave empty if there is none", true, window, cx),
            nickname: field(b.nickname, "Your name on this server, 3 to 30 characters", false, window, cx),
            phonetic: field(b.phonetic, "How text-to-speech says it (optional)", false, window, cx),
            channel: field(b.channel, "e.g. Lobby/Games (optional)", false, window, cx),
            auto_connect: b.auto_connect,
            voice_mode: b.voice_mode,
            identity: b.identity,
            quiet: b.quiet,
            error: None,
        });
        self.identities = crate::identity::load_all();
        cx.notify();
    }

    pub(super) fn save_form(&mut self, cx: &mut Context<Self>) -> Option<Bookmark> {
        let f = self.form.as_ref()?;
        let v = |e: &Entity<InputState>| e.read(cx).value().trim().to_string();
        let b = Bookmark {
            id: f.id.clone(),
            label: v(&f.label),
            address: v(&f.address),
            password: f.password.read(cx).value().to_string(),
            nickname: v(&f.nickname),
            phonetic: v(&f.phonetic),
            channel: v(&f.channel),
            auto_connect: f.auto_connect,
            voice_mode: f.voice_mode,
            identity: f.identity.clone(),
            quiet: f.quiet,
        };
        if b.address.is_empty() {
            return None;
        }
        // TeamSpeak refuses the connect with "invalid parameter size" otherwise
        let len = b.nickname.chars().count();
        if !crate::net::is_banshee(&b.address) && !b.nickname.is_empty() && !(3..=30).contains(&len) {
            if let Some(f) = &mut self.form {
                f.error = Some("TeamSpeak nicknames have to be 3 to 30 characters".into());
            }
            cx.notify();
            return None;
        }
        let saved = b.clone();
        self.model.update(cx, |m, cx| {
            m.update_settings(cx, |s| match s.bookmarks.iter_mut().find(|x| x.id == b.id) {
                Some(x) => *x = b,
                None => s.bookmarks.push(b),
            })
        });
        self.form = None;
        cx.notify();
        Some(saved)
    }

    pub(super) fn servers(&mut self, cx: &mut Context<Self>) -> AnyElement {
        if self.form.is_some() {
            return self.server_form(cx);
        }
        let m = self.model.read(cx);
        let bookmarks = m.settings.bookmarks.clone();
        let servers = m.snap.servers.clone();
        let mut list = Group::new("bookmark", "Saved servers")
            .hint("Servers show up in the sidebar. Click one there to connect.")
            .action(Button::new("add").label("Add server").small().primary().on_click(cx.listener(|t, _, window, cx| {
                let nick = t.model.read(cx).settings.bookmarks.first().map(|b| b.nickname.clone()).unwrap_or_default();
                t.edit(Bookmark { id: new_id(), nickname: nick, ..Default::default() }, true, window, cx)
            })));
        for b in bookmarks.iter() {
            let name = if b.label.is_empty() { b.address.clone() } else { b.label.clone() };
            let server = servers.iter().find(|s| s.bookmark == b.id);
            let online = server.is_some_and(|s| s.status == Status::Connected);
            let sub = format!(
                "{}{}{}",
                b.address,
                if b.nickname.is_empty() { String::new() } else { format!(" · {}", b.nickname) },
                if b.auto_connect { " · connects on start" } else { "" }
            );
            let (edit_b, del_id, conn_b) = (b.clone(), b.id.clone(), b.clone());
            list = list.child(
                h_flex()
                    .id(SharedString::from(format!("srv-{}", b.id)))
                    .gap_3()
                    .px_4()
                    .py_3()
                    .items_center()
                    .hover(|d| d.bg(hover_bg()))
                    .child(
                        div()
                            .size(px(34.))
                            .flex_none()
                            .rounded(px(8.))
                            .bg(rgb(colors().raised))
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_size(px(12.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(colors().muted))
                            .child(initials(&name)),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().text_sm().truncate().child(name))
                            .child(label(sub).truncate()),
                    )
                    .child(if online {
                        h_flex()
                            .gap_1p5()
                            .items_center()
                            .child(div().size(px(7.)).rounded_full().bg(rgb(colors().green)))
                            .child(label("Connected"))
                            .into_any_element()
                    } else {
                        Button::new(SharedString::from(format!("c-{}", b.id)))
                            .label("Connect")
                            .small()
                            .ghost()
                            .on_click(cx.listener(move |t, _, _, cx| t.model.read(cx).net.send(Cmd::Connect(conn_b.clone()))))
                            .into_any_element()
                    })
                    .child(
                        Button::new(SharedString::from(format!("e-{}", b.id)))
                            .label("Edit")
                            .small()
                            .ghost()
                            .on_click(cx.listener(move |t, _, window, cx| t.edit(edit_b.clone(), false, window, cx))),
                    )
                    .child(danger_btn(SharedString::from(format!("d-{}", b.id)), "Remove server").on_click(cx.listener(
                        move |t, _, _, cx| {
                            let id = del_id.clone();
                            t.model.update(cx, |m, cx| m.update_settings(cx, |s| s.bookmarks.retain(|b| b.id != id)));
                        },
                    ))),
            );
        }
        if bookmarks.is_empty() {
            list = list.child(block(label("No servers yet. Add one to get started.")));
        }
        list.into_any_element()
    }

    pub(super) fn server_form(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let f = self.form.as_ref().unwrap();
        let (auto, is_new, mode) = (f.auto_connect, f.is_new, f.voice_mode);
        let chosen = f.identity.clone().or_else(|| self.identities.first().map(|i| i.id.clone()));
        let mut idents = h_flex().gap_2().flex_wrap();
        for (i, ident) in self.identities.iter().enumerate() {
            let id = ident.id.clone();
            idents = idents.child(chip(("bm-id", i), ident.name.clone(), chosen.as_deref() == Some(id.as_str())).on_click(
                cx.listener(move |t, _, _, cx| {
                    if let Some(f) = &mut t.form {
                        f.identity = Some(id.clone());
                    }
                    cx.notify();
                }),
            ));
        }
        let mut modes = h_flex().gap_2();
        for (i, (m, text)) in [
            (None, "Default"),
            (Some(VoiceMode::PushToTalk), "Push-to-talk"),
            (Some(VoiceMode::VoiceActivation), "Voice activation"),
        ]
        .into_iter()
        .enumerate()
        {
            modes = modes.child(chip(("bm-mode", i), text, mode == m).on_click(cx.listener(move |t, _, _, cx| {
                if let Some(f) = &mut t.form {
                    f.voice_mode = m;
                }
                cx.notify();
            })));
        }
        v_flex()
            .gap_6()
            .child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .child(
                        div()
                            .id("back")
                            .size(px(30.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(6.))
                            .cursor_pointer()
                            .hover(|d| d.bg(hover_bg()))
                            .tooltip(|w, cx| Tooltip::new("All servers").build(w, cx))
                            .on_click(cx.listener(|t, _, _, cx| {
                                t.form = None;
                                cx.notify();
                            }))
                            .child(icon("arrow-left", 16., colors().muted)),
                    )
                    .child(div().text_lg().font_weight(FontWeight::SEMIBOLD).child(if is_new { "Add server" } else { "Edit server" })),
            )
            .child(
                Group::new("server", "Server")
                    .child(field("Address", Input::new(&f.address)))
                    .child(field("Name", Input::new(&f.label)))
                    .child(field("Server password", Input::new(&f.password)))
                    .child(field("Default channel", Input::new(&f.channel))),
            )
            .child(
                Group::new("user-round", "You")
                    .child(field("Nickname", Input::new(&f.nickname)))
                    .when_some(f.error.clone(), |d, e| d.child(notice("triangle-alert", colors().red, e).mx_4().my_2()))
                    .child(field("Phonetic nickname", Input::new(&f.phonetic)))
                    .when(self.identities.len() > 1, |d| d.child(field("Identity", idents))),
            )
            .child(
                Group::new("sliders-horizontal", "Behavior")
                    .child(field("Voice mode", modes))
                    .child(item(
                        "Notifications",
                        Some("Sounds and popups for this server".into()),
                        Switch::new("bm-quiet").checked(!f.quiet).on_click(cx.listener(|t, v: &bool, _, cx| {
                            if let Some(f) = &mut t.form {
                                f.quiet = !*v;
                            }
                            cx.notify();
                        })),
                    ))
                    .child(item(
                        "Connect on start",
                        Some("When Wisp starts, it joins this server on its own".into()),
                        Switch::new("auto").checked(auto).on_click(cx.listener(|t, v: &bool, _, cx| {
                            if let Some(f) = &mut t.form {
                                f.auto_connect = *v;
                            }
                            cx.notify();
                        })),
                    )),
            )
            .child(
                h_flex()
                    .gap_2()
                    .justify_end()
                    .child(Button::new("cancel").label("Cancel").small().ghost().on_click(cx.listener(|t, _, _, cx| {
                        t.form = None;
                        cx.notify();
                    })))
                    .child(Button::new("save-connect").label("Save and connect").small().on_click(cx.listener(
                        |t, _, _, cx| {
                            if let Some(b) = t.save_form(cx) {
                                t.model.read(cx).net.send(Cmd::Connect(b));
                            }
                        },
                    )))
                    .child(Button::new("save").label("Save").small().primary().on_click(cx.listener(|t, _, _, cx| {
                        t.save_form(cx);
                    }))),
            )
            .into_any_element()
    }
}
