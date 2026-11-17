//! "Share your screen": what to share and how, picked per share. The
//! choices start from the last share's, fitted to the new source.

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{Disableable, Sizable, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{
    AnyElement, Context, FontWeight, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement, Styled, div,
    px, rgb, rgba,
};

use super::main_view::MainView;
use super::widgets::*;
use crate::net::{Cmd, ConId};
use crate::video::{Pick, ScreenChoice, ShareOptions, ShareStart, Source};

pub struct ShareDialog {
    con: ConId,
    /// Where Wisp lists the screens itself (empty: the desktop asks).
    screens: Vec<ScreenChoice>,
    pick: Pick,
    source: SourceState,
    /// Picture height (0: the source's own), frame rate, sound.
    height: u32,
    fps: u32,
    sound: bool,
    /// The graphics card encoder, once looked for.
    gpu: Option<Option<&'static str>>,
    /// Counts the sources opened, so a late one from before is dropped.
    opened: u64,
    /// Opened while sharing: switch the source, or stop.
    live: bool,
    /// Apps playing sound right now (None while looking), and the one
    /// picked (None: everything).
    sound_apps: Option<Vec<crate::video::SoundApp>>,
    sound_app: Option<String>,
}

enum SourceState {
    /// Live, before picking something new.
    Idle,
    Opening,
    Ready(Source),
    Failed(String),
}

impl MainView {
    pub(super) fn open_share(&mut self, con: ConId, live: bool, cx: &mut Context<Self>) {
        let s = &self.model.read(cx).settings;
        let screens = crate::video::screens();
        let pick = screens.first().map_or(Pick::Default, |s| Pick::Screen(s.id));
        self.share = Some(ShareDialog {
            con,
            screens,
            pick,
            source: if live { SourceState::Idle } else { SourceState::Opening },
            height: s.screen_height,
            fps: s.screen_fps,
            sound: s.screen_sound,
            gpu: None,
            opened: 0,
            live,
            sound_apps: None,
            sound_app: None,
        });
        if live {
            return;
        }
        if !crate::video::SOUND_FOLLOWS_PICK {
            cx.spawn(async move |this, cx| {
                let apps = cx.background_executor().spawn(async { crate::video::sound_apps() }).await;
                let _ = this.update(cx, |t, cx| {
                    if let Some(d) = &mut t.share {
                        d.sound_apps = Some(apps);
                        cx.notify();
                    }
                });
            })
            .detach();
        }
        // trying the encoders takes a moment the first time
        cx.spawn(async move |this, cx| {
            let found = cx.background_executor().spawn(async { crate::video::gpu::detect().map(|g| g.label()) }).await;
            let _ = this.update(cx, |t, cx| {
                if let Some(d) = &mut t.share {
                    d.gpu = Some(found);
                    cx.notify();
                }
            });
        })
        .detach();
        self.open_source(pick, cx);
    }

    /// Start capturing `pick`; on Linux the desktop asks what.
    fn open_source(&mut self, pick: Pick, cx: &mut Context<Self>) {
        let Some(d) = &mut self.share else { return };
        d.opened += 1;
        d.pick = pick;
        // dropping the old source stops it
        d.source = SourceState::Opening;
        let opened = d.opened;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx.background_executor().spawn(async move { crate::video::open(pick) }).await;
            let _ = this.update(cx, |t, cx| {
                let Some(d) = &mut t.share else { return };
                if d.opened != opened {
                    return;
                }
                match result {
                    Ok(source) => {
                        d.fit(&source.info);
                        d.source = SourceState::Ready(source);
                    }
                    // cancelled in the desktop's picker: no share then
                    Err(e) if format!("{e:#}").contains("cancelled") => t.share = None,
                    Err(e) => d.source = SourceState::Failed(format!("{e:#}")),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn go_live(&mut self, cx: &mut Context<Self>) {
        let Some(mut d) = self.share.take() else { return };
        let SourceState::Ready(source) = std::mem::replace(&mut d.source, SourceState::Opening) else {
            self.share = Some(d);
            return;
        };
        if d.live {
            self.send(cx, Cmd::ShareSwitch { con: d.con, source });
            cx.notify();
            return;
        }
        let (height, fps, sound) = (d.height, d.fps, d.sound);
        self.model.update(cx, |m, cx| {
            m.update_settings(cx, |s| {
                s.screen_height = height;
                s.screen_fps = fps;
                s.screen_sound = sound;
            })
        });
        let options = ShareOptions { height, fps, sound: sound && crate::video::SOUND };
        let start = ShareStart { source: Some(source), options, sound_app: d.sound_app.clone() };
        self.send(cx, Cmd::Share { con: d.con, start: Some(start) });
        cx.notify();
    }

    pub(super) fn share_overlay(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let d = self.share.as_ref()?;
        let row = |name: &'static str, el: AnyElement| {
            h_flex()
                .gap_4()
                .items_start()
                .child(div().w(px(96.)).flex_none().pt(px(6.)).text_sm().text_color(rgb(colors().muted)).child(name))
                .child(div().flex_1().min_w_0().child(el))
        };
        let status = match &d.source {
            SourceState::Idle => label(""),
            SourceState::Opening if d.screens.is_empty() => label("Pick a screen or window in the desktop's dialog..."),
            SourceState::Opening => label("Starting..."),
            SourceState::Ready(s) => label(match s.info.refresh {
                Some(hz) => format!("{} × {} at {hz} Hz", s.info.width, s.info.height),
                None => format!("{} × {}", s.info.width, s.info.height),
            }),
            SourceState::Failed(e) => div().text_sm().text_color(rgb(colors().red)).child(e.clone()),
        };
        // the source: Wisp's own list of screens, or the desktop's picker again
        let mut source = v_flex().gap_2();
        if d.screens.is_empty() {
            let again = matches!(d.source, SourceState::Ready(_) | SourceState::Failed(_));
            source = source.child(h_flex().gap_3().items_center().child(status).when(again, |r| {
                r.child(Button::new("share-repick").label("Pick another").small().ghost().on_click(
                    cx.listener(|t, _, _, cx| t.open_source(Pick::Default, cx)),
                ))
            }));
        } else {
            let mut chips = h_flex().gap_2().flex_wrap();
            for (i, s) in d.screens.iter().enumerate() {
                let pick = Pick::Screen(s.id);
                chips = chips.child(
                    chip(("share-screen", i), s.label.clone(), d.pick == pick)
                        .on_click(cx.listener(move |t, _, _, cx| t.open_source(pick, cx))),
                );
            }
            source = source.child(chips).child(status);
        }
        let ready = match &d.source {
            SourceState::Ready(s) => Some(s.info.clone()),
            _ => None,
        };
        let idle = matches!(d.source, SourceState::Idle);
        let mut body = if idle {
            let con = d.con;
            v_flex()
                .gap_3()
                .child(label("You're sharing. Switch to another screen or window, and viewers keep watching."))
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("share-change")
                                .label("Change what you share")
                                .small()
                                .primary()
                                .on_click(cx.listener(|t, _, _, cx| {
                                    let pick = t.share.as_ref().map_or(Pick::Default, |d| d.pick);
                                    t.open_source(pick, cx)
                                })),
                        )
                        .child(Button::new("share-stop").label("Stop sharing").small().danger().on_click(cx.listener(
                            move |t, _, _, cx| {
                                t.share = None;
                                t.send(cx, Cmd::Share { con, start: None });
                                cx.notify();
                            },
                        ))),
                )
        } else {
            v_flex().gap_4().child(row("Share", source.into_any_element()))
        };
        if let Some(info) = ready.as_ref().filter(|_| !d.live) {
            let mut sizes = h_flex().gap_2().flex_wrap();
            for (i, size) in crate::video::sizes(info.width, info.height).into_iter().enumerate() {
                let text = match size.choice {
                    0 => format!("Native {} × {}", size.width, size.height),
                    h if size.width.abs_diff(h * 16 / 9) <= 2 => format!("{h}p"),
                    _ => format!("{} × {}", size.width, size.height),
                };
                let choice = size.choice;
                sizes = sizes.child(chip(("share-size", i), text, d.height == choice).on_click(cx.listener(
                    move |t, _, _, cx| {
                        if let Some(d) = &mut t.share {
                            d.height = choice;
                        }
                        cx.notify();
                    },
                )));
            }
            let mut rates = h_flex().gap_2().flex_wrap();
            for f in crate::video::rates(info.refresh) {
                rates = rates.child(chip(("share-fps", f as usize), format!("{f} fps"), d.fps == f).on_click(
                    cx.listener(move |t, _, _, cx| {
                        if let Some(d) = &mut t.share {
                            d.fps = f;
                        }
                        cx.notify();
                    }),
                ));
            }
            body = body.child(row("Resolution", sizes.into_any_element())).child(row("Frame rate", rates.into_any_element()));
            if crate::video::SOUND {
                body = body.child(row(
                    "Sound",
                    h_flex()
                        .gap_3()
                        .pt(px(4.))
                        .items_center()
                        .child(Switch::new("share-sound").checked(d.sound).on_click(cx.listener(|t, v: &bool, _, cx| {
                            if let Some(d) = &mut t.share {
                                d.sound = *v;
                            }
                            cx.notify();
                        })))
                        .child(label(if crate::video::SOUND_FOLLOWS_PICK {
                            "A window or app brings only its own sound, a screen brings everything"
                        } else {
                            "What your computer plays, except Wisp itself"
                        }))
                        .into_any_element(),
                ));
                // one app instead of everything
                let apps = d.sound_apps.clone().unwrap_or_default();
                if d.sound && !apps.is_empty() {
                    let mut chips = h_flex().gap_2().flex_wrap().child(
                        chip("sound-all", "Everything", d.sound_app.is_none()).on_click(cx.listener(|t, _, _, cx| {
                            if let Some(d) = &mut t.share {
                                d.sound_app = None;
                            }
                            cx.notify();
                        })),
                    );
                    for (i, app) in apps.into_iter().enumerate() {
                        let on = d.sound_app.as_deref() == Some(app.key.as_str());
                        chips = chips.child(chip(("sound-app", i), app.name.clone(), on).on_click(cx.listener(
                            move |t, _, _, cx| {
                                if let Some(d) = &mut t.share {
                                    d.sound_app = Some(app.key.clone());
                                }
                                cx.notify();
                            },
                        )));
                    }
                    body = body.child(row("Sound from", chips.into_any_element()));
                }
            }
            body = body.child(div().text_sm().text_color(rgb(colors().muted)).child(d.note(info)));
        }
        Some(
            div()
                .absolute()
                .inset_0()
                .bg(rgba(0x00000099))
                .flex()
                .items_center()
                .justify_center()
                .occlude()
                .child(
                    v_flex()
                        .w(px(560.))
                        .gap_5()
                        .p_5()
                        .rounded(px(8.))
                        .bg(rgb(colors().panel))
                        .border_1()
                        .border_color(border())
                        .shadow_lg()
                        .child(div().font_weight(FontWeight::SEMIBOLD).child(if d.live {
                            "Your screen share"
                        } else {
                            "Share your screen"
                        }))
                        .child(body)
                        .child(
                            h_flex()
                                .gap_2()
                                .justify_end()
                                .child(Button::new("share-cancel").label("Cancel").small().ghost().on_click(cx.listener(
                                    |t, _, _, cx| {
                                        t.share = None;
                                        cx.notify();
                                    },
                                )))
                                .when(!idle, |d2| {
                                    d2.child(
                                        Button::new("share-go")
                                            .label(if d.live { "Switch" } else { "Go live" })
                                            .small()
                                            .primary()
                                            .disabled(ready.is_none())
                                            .on_click(cx.listener(|t, _, _, cx| t.go_live(cx))),
                                    )
                                }),
                        ),
                )
                .into_any_element(),
        )
    }
}

impl ShareDialog {
    /// Start from the last share's choices, moved to the nearest this source offers.
    fn fit(&mut self, info: &crate::video::SourceInfo) {
        let sizes = crate::video::sizes(info.width, info.height);
        if !sizes.iter().any(|s| s.choice == self.height) {
            let want = if self.height == 0 { info.height } else { self.height };
            self.height = sizes.iter().min_by_key(|s| s.height.abs_diff(want)).map_or(0, |s| s.choice);
        }
        let rates = crate::video::rates(info.refresh);
        if !rates.contains(&self.fps) {
            self.fps = rates.iter().copied().min_by_key(|f| f.abs_diff(self.fps)).unwrap_or(30);
        }
    }

    /// Upload and encoder, and a warning when the CPU would have a hard time.
    fn note(&self, info: &crate::video::SourceInfo) -> String {
        let options = ShareOptions { height: self.height, fps: self.fps, sound: self.sound && crate::video::SOUND };
        let gpu = matches!(self.gpu, Some(Some(_)));
        let bits = crate::video::estimate(info.width, info.height, options, gpu);
        let upload = format!("About {:.0} Mbit/s upload", (bits as f64 / 1_000_000.0).max(1.0));
        let (w, h) = match self.height {
            0 => (info.width, info.height),
            h => (info.width * h / info.height.max(1), h),
        };
        match self.gpu {
            None => format!("{upload}. Looking for a graphics card encoder..."),
            Some(Some(name)) => format!("{upload}, encoded with {name}."),
            Some(None) if (w * h) as u64 * self.fps as u64 > 1920 * 1080 * 60 => {
                format!("{upload}. No graphics card encoder found, and this is a lot for the CPU: it may stutter.")
            }
            Some(None) => format!("{upload}, encoded on the CPU."),
        }
    }
}
