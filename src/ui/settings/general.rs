//! Settings → General: window, startup, troubleshooting and the command line.

use super::*;

impl SettingsView {
    pub(super) fn general(&self, cx: &mut Context<Self>) -> AnyElement {
        let tray = self.model.read(cx).settings.close_to_tray;
        let theme_file = crate::ui::colors::path();
        let theme_file = match dirs::home_dir().and_then(|h| theme_file.strip_prefix(h).ok().map(|p| p.to_path_buf())) {
            Some(rel) => format!("~/{}", rel.display()),
            None => theme_file.display().to_string(),
        };
        v_flex()
            .gap_6()
            .child(Group::new("palette", "Appearance").child(item(
                "Theme",
                Some(format!("Colors come from {theme_file} when it exists and change as soon as the file does.").into()),
                div(),
            )))
            .child(
                Group::new("app-window", "Window")
                    .child(item(
                        "Keep running in the tray",
                        Some("Closing the window while connected keeps Wisp in the tray".into()),
                        Switch::new("tray").checked(tray).on_click(cx.listener(|t, v: &bool, _, cx| {
                            let v = *v;
                            t.model.update(cx, |m, cx| m.update_settings(cx, |s| s.close_to_tray = v));
                        })),
                    ))
                    .child(item(
                        "Start at login",
                        Some("Starts in the tray and connects to your auto-connect servers".into()),
                        Switch::new("autostart").checked(crate::desktop::autostart()).on_click(cx.listener(
                            |_, v: &bool, _, cx| {
                                crate::desktop::set_autostart(*v);
                                cx.notify();
                            },
                        )),
                    ))
                    .when(crate::desktop::CAN_SET_LINKS, |d| {
                        d.child(item(
                            "Open ts3server:// links",
                            Some("Join links from websites and chats open in Wisp".into()),
                            Switch::new("links").checked(crate::desktop::handles_links()).on_click(cx.listener(
                                |t, v: &bool, _, cx| {
                                    if *v {
                                        let before = crate::desktop::take_links();
                                        if before.is_some() {
                                            t.model.update(cx, |m, cx| m.update_settings(cx, |s| s.previous_link_handler = before));
                                        }
                                    } else {
                                        let back = t.model.read(cx).settings.previous_link_handler.clone();
                                        crate::desktop::give_back_links(back.as_deref());
                                    }
                                    cx.notify();
                                },
                            )),
                        ))
                    }),
            )
            .child(Group::new("life-buoy", "Troubleshooting").child(item(
                "Log file",
                Some("Send wisp.log along when something doesn't work".into()),
                Button::new("show-log").label("Show log").small().on_click(|_, _, cx| {
                    if let Some(dir) = crate::log::path().parent() {
                        cx.open_with_system(dir);
                    }
                }),
            )))
            .child(
                Group::new("terminal", "Command line").child(block(
                    v_flex().gap_1p5().children(
                        [
                            ("wisp status [--json | --bar]", "connection status"),
                            ("wisp connect <server>", "connect to a saved server"),
                            ("wisp disconnect [server | all]", "disconnect"),
                            ("wisp mute | deafen [on | off]", "microphone / sound"),
                            ("wisp ptt down | up", "push-to-talk"),
                            ("wisp share [on | off | toggle]", "screen sharing"),
                            ("wisp join <channel>", "join a channel"),
                            ("wisp say <text>", "message your channel"),
                            ("wisp channel create | rename | topic | move | delete", "channel admin"),
                            ("wisp show | quit", "window / exit"),
                        ]
                        .into_iter()
                        .map(|(c, d)| {
                            h_flex()
                                .gap_4()
                                .child(
                                    div()
                                        .w(px(330.))
                                        .flex_none()
                                        .font_family("monospace")
                                        .text_xs()
                                        .text_color(rgb(colors().soft))
                                        .child(c),
                                )
                                .child(label(d))
                        }),
                    ),
                )),
            )
            .into_any_element()
    }
}
