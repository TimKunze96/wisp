//! Settings → Identities: making, importing, exporting and raising identities.

use super::*;

impl SettingsView {
    pub(super) fn import_identity(&mut self, text: String, cx: &mut Context<Self>) {
        match crate::identity::import(&text, "Imported") {
            Ok(i) => self.identity_note = Some((format!("Imported {} (level {})", i.name, i.level()), false)),
            Err(e) => self.identity_note = Some((e.to_string(), true)),
        }
        self.identities = crate::identity::load_all();
        cx.notify();
    }

    pub(super) fn start_raise(&mut self, ident: crate::identity::StoredIdentity, target: u8, cx: &mut Context<Self>) {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, AtomicU64};
        let tried = Arc::new(AtomicU64::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        self.raising = Some(Raising { id: ident.id.clone(), target, tried: tried.clone(), cancel: cancel.clone() });
        let (tx, rx) = tokio::sync::oneshot::channel();
        std::thread::Builder::new()
            .name("identity-level".into())
            .spawn(move || {
                let _ = tx.send(crate::identity::raise(&ident, target, &tried, &cancel).map(|c| (ident.id, c)));
            })
            .ok();
        cx.spawn(async move |this, cx| {
            let result = rx.await.ok().flatten();
            let _ = this.update(cx, |t, cx| {
                if let Some((id, counter)) = result {
                    crate::identity::update_counter(&id, counter);
                    t.identity_note = Some((format!("Reached level {target}"), false));
                }
                t.raising = None;
                t.identities = crate::identity::load_all();
                cx.notify();
            });
        })
        .detach();
        // redraw now and then for the progress
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(std::time::Duration::from_millis(500)).await;
                let running = this.update(cx, |t, cx| {
                    cx.notify();
                    t.raising.is_some()
                });
                if !matches!(running, Ok(true)) {
                    break;
                }
            }
        })
        .detach();
        cx.notify();
    }

    pub(super) fn identities_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        use std::sync::atomic::Ordering;
        let list = self.identities.clone();
        for i in &list {
            if self.identity_names.contains_key(&i.id) {
                continue;
            }
            let id = i.id.clone();
            let input = cx.new(|cx| InputState::new(window, cx).placeholder("Name").default_value(i.name.clone()));
            cx.subscribe(&input, move |t, input, ev: &gpui_kit::component::input::InputEvent, cx| {
                use gpui_kit::component::input::InputEvent;
                if matches!(ev, InputEvent::Blur | InputEvent::PressEnter { .. }) {
                    let name = input.read(cx).value().trim().to_string();
                    if !name.is_empty() {
                        crate::identity::rename(&id, &name);
                        t.identities = crate::identity::load_all();
                        cx.notify();
                    }
                }
            })
            .detach();
            self.identity_names.insert(i.id.clone(), input);
        }
        let paste = self
            .identity_paste
            .get_or_insert_with(|| cx.new(|cx| InputState::new(window, cx).placeholder("Paste an identity (\"123V...\")")))
            .clone();
        let used = self.model.read(cx).settings.bookmarks.clone();
        let mut col = v_flex().gap_3();
        for (n, ident) in list.iter().enumerate() {
            let level = ident.level();
            let uses: Vec<String> = used
                .iter()
                .filter(|b| b.identity.as_deref() == Some(ident.id.as_str()) || (b.identity.is_none() && n == 0))
                .map(|b| if b.label.is_empty() { b.address.clone() } else { b.label.clone() })
                .collect();
            let raising = self.raising.as_ref().filter(|r| r.id == ident.id);
            let (uid, id_export, id_del, id_raise) = (ident.uid.clone(), ident.clone(), ident.id.clone(), ident.clone());
            let confirm = self.confirm_delete_identity.as_deref() == Some(ident.id.as_str());
            let mut raise = h_flex().gap_2().items_center();
            match raising {
                Some(r) => {
                    let tried = r.tried.load(Ordering::Relaxed);
                    let cancel = r.cancel.clone();
                    raise = raise
                        .child(label(format!("Raising to {}: {:.1} M tries", r.target, tried as f64 / 1e6)))
                        .child(Button::new(("id-cancel", n)).label("Stop").small().ghost().on_click(move |_, _, _| {
                            cancel.store(true, Ordering::Relaxed);
                        }));
                }
                None => {
                    // each level takes about twice as long as the one before
                    for (k, target) in [level + 1, level.max(20), level.max(25)].into_iter().filter(|t| *t > level).enumerate() {
                        let ident = id_raise.clone();
                        raise = raise.child(
                            chip(SharedString::from(format!("raise-{n}-{k}")), format!("Raise to {target}"), false)
                                .on_click(cx.listener(move |t, _, _, cx| t.start_raise(ident.clone(), target, cx))),
                        );
                    }
                }
            }
            col = col.child(
                card()
                    .gap_3()
                    .p_4()
                    .child(
                        h_flex()
                            .gap_3()
                            .items_center()
                            .child(icon("fingerprint-pattern", 16., colors().muted))
                            .child(div().flex_1().child(Input::new(&self.identity_names[&ident.id])))
                            .child(label(format!("Level {level}")))
                            .child(
                                Button::new(("id-export", n)).label("Export").small().ghost().on_click(cx.listener(
                                    move |_, _, _, cx| {
                                        let ident = id_export.clone();
                                        let dir = dirs::home_dir().unwrap_or_default();
                                        let rx = cx.prompt_for_new_path(&dir, Some(&format!("{}.ini", ident.name)));
                                        cx.spawn(async move |this, cx| {
                                            let Ok(Ok(Some(path))) = rx.await else { return };
                                            let result = ident.to_ini().and_then(|ini| Ok(std::fs::write(&path, ini)?));
                                            let _ = this.update(cx, |t, cx| {
                                                t.identity_note = Some(match result {
                                                    Ok(()) => (format!("Saved to {}", path.display()), false),
                                                    Err(e) => (e.to_string(), true),
                                                });
                                                cx.notify();
                                            });
                                        })
                                        .detach();
                                    },
                                )),
                            )
                            .when(list.len() > 1, |d| {
                                d.child(
                                    div()
                                        .id(("id-del", n))
                                        .size(px(28.))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .rounded(px(5.))
                                        .cursor_pointer()
                                        .when(confirm, |d| d.bg(alpha(colors().red, 0x26)))
                                        .hover(|d| d.bg(alpha(colors().red, 0x1a)))
                                        .tooltip(move |w, cx| {
                                            Tooltip::new(if confirm { "Click again: the key is gone for good" } else { "Delete identity" }).build(w, cx)
                                        })
                                        .child(icon("trash", 14., colors().red))
                                        .on_click(cx.listener(move |t, _, _, cx| {
                                            if t.confirm_delete_identity.as_deref() == Some(id_del.as_str()) {
                                                crate::identity::delete(&id_del);
                                                t.confirm_delete_identity = None;
                                                t.identities = crate::identity::load_all();
                                            } else {
                                                t.confirm_delete_identity = Some(id_del.clone());
                                            }
                                            cx.notify();
                                        })),
                                )
                            }),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(label(uid.clone()).truncate())
                            .child(
                                div()
                                    .id(("id-copy", n))
                                    .cursor_pointer()
                                    .tooltip(|w, cx| Tooltip::new("Copy unique id").build(w, cx))
                                    .child(icon("copy", 12., colors().dim))
                                    .on_click(move |_, _, cx| cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(uid.clone()))),
                            ),
                    )
                    .child(label(if uses.is_empty() { "Not used by a server yet".to_string() } else { format!("Used by {}", uses.join(", ")) }))
                    .child(raise),
            );
        }
        v_flex()
            .gap_4()
            .child(label(
                "Your identity is who you are to a server: groups and permissions stick to it. Each server can \
                 use its own (Settings → Servers → Edit). Servers can require a minimum security level; \
                 raising it takes a while (each level about twice as long as the last).",
            ))
            .when_some(self.identity_note.clone(), |d, (note, bad)| {
                d.child(if bad { notice("triangle-alert", colors().red, note) } else { notice("check", colors().green, note) })
            })
            .child(
                h_flex()
                    .gap_2()
                    .child(Button::new("id-new").label("New identity").small().primary().on_click(cx.listener(|t, _, _, cx| {
                        let n = t.identities.len() + 1;
                        crate::identity::create(&format!("Identity {n}"));
                        t.identities = crate::identity::load_all();
                        cx.notify();
                    })))
                    .child(Button::new("id-import").label("Import file...").small().ghost().on_click(cx.listener(|_, _, _, cx| {
                        let rx = cx.prompt_for_paths(gpui_kit::PathPromptOptions {
                            files: true,
                            directories: false,
                            multiple: false,
                            prompt: Some("Import identity".into()),
                        });
                        cx.spawn(async move |this, cx| {
                            let Ok(Ok(Some(paths))) = rx.await else { return };
                            let Some(text) = paths.first().and_then(|p| std::fs::read_to_string(p).ok()) else { return };
                            let _ = this.update(cx, |t, cx| t.import_identity(text, cx));
                        })
                        .detach();
                    }))),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(div().flex_1().child(Input::new(&paste)))
                    .child(Button::new("id-paste").label("Import").small().ghost().on_click(cx.listener(move |t, _, window, cx| {
                        let text = paste.read(cx).value().to_string();
                        if !text.trim().is_empty() {
                            t.import_identity(text, cx);
                            paste.update(cx, |i, cx| i.set_value("", window, cx));
                        }
                    }))),
            )
            .child(col)
            .into_any_element()
    }
}
