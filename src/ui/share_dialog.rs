//! "Share your screen": what to share and how, picked per share. The
//! choices start from the last share's, fitted to the new source.

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{Disableable, Sizable, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{
    AnyElement, Context, FontWeight, InteractiveElement, IntoElement, ObjectFit, ParentElement, RenderImage,
    StatefulInteractiveElement, Styled, StyledImage, div, img, px, relative, rgb, rgba,
};
use std::sync::Arc;
use std::time::Duration;

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
    /// The latest picture of what's picked.
    preview: Option<Arc<RenderImage>>,
}

const DIALOG_W: f32 = 600.;
/// The preview box, 16:9 inside the dialog's padding.
const PREVIEW_BOX_H: f32 = (DIALOG_W - 40.) * 9. / 16.;
/// The preview's pixels (twice the box, for sharp high-DPI screens).
const PREVIEW_W: usize = 1120;
const PREVIEW_H: usize = 630;
const PREVIEW_EVERY: Duration = Duration::from_millis(250);

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
            preview: None,
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
                        let peek = source.peek();
                        d.source = SourceState::Ready(source);
                        t.run_preview(opened, peek, cx);
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

    /// Keeps the preview of what's picked fresh while the dialog shows it.
    fn run_preview(&mut self, opened: u64, peek: crate::video::SourcePeek, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let mut shown: Option<Arc<RenderImage>> = None;
            loop {
                let p = peek.clone();
                let picture = cx.background_executor().spawn(async move { p.picture(PREVIEW_W, PREVIEW_H) }).await;
                let still = this
                    .update(cx, |t, cx| {
                        let Some(d) = t.share.as_mut().filter(|d| d.opened == opened) else { return false };
                        if !matches!(d.source, SourceState::Ready(_)) {
                            return false;
                        }
                        if let Some(p) = picture
                            && let Some(buf) = image::RgbaImage::from_raw(p.width, p.height, p.bgra)
                        {
                            let image = Arc::new(RenderImage::new(vec![image::Frame::new(buf)]));
                            d.preview = Some(image.clone());
                            // the old picture's texture would stay in the atlas otherwise
                            if let Some(old) = shown.replace(image) {
                                cx.drop_image(old, None);
                            }
                            cx.notify();
                        }
                        true
                    })
                    .unwrap_or(false);
                if !still {
                    break;
                }
                cx.background_executor().timer(PREVIEW_EVERY).await;
            }
            let _ = this.update(cx, |t, cx| {
                if let Some(d) = t.share.as_mut().filter(|d| d.opened == opened) {
                    d.preview = None;
                }
                if let Some(old) = shown {
                    cx.drop_image(old, None);
                }
            });
        })
        .detach();
    }

    pub(super) fn share_overlay(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let d = self.share.as_ref()?;
        let c = colors();
        let ready = match &d.source {
            SourceState::Ready(s) => Some(s.info.clone()),
            _ => None,
        };
        let idle = matches!(d.source, SourceState::Idle);
        let close = |t: &mut MainView, cx: &mut Context<MainView>| {
            t.share = None;
            cx.notify();
        };

        let header = h_flex()
            .gap_3()
            .items_center()
            .px_5()
            .pt_5()
            .pb_4()
            .child(
                div()
                    .size(px(36.))
                    .flex_none()
                    .rounded(px(8.))
                    .bg(alpha(c.accent, 0x22))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(icon("monitor-up", 18., c.accent)),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(if d.live {
                        "Your screen share"
                    } else {
                        "Share your screen"
                    }))
                    .child(label(if idle {
                        "Viewers keep watching while you switch"
                    } else if d.live {
                        "Pick what to show instead"
                    } else {
                        "Check what you share, then pick the quality"
                    })),
            )
            .child(
                div()
                    .id("share-close")
                    .size(px(28.))
                    .flex_none()
                    .rounded(px(6.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .hover(|s| s.bg(hover_bg()))
                    .child(icon("x", 16., c.muted))
                    .on_click(cx.listener(move |t, _, _, cx| close(t, cx))),
            );

        let mut body = v_flex().gap_5().px_5().pb_5();
        if idle {
            let con = d.con;
            body = body.child(
                h_flex()
                    .gap_3()
                    .p_4()
                    .rounded(px(8.))
                    .bg(alpha(ink(), 0x06))
                    .border_1()
                    .border_color(border())
                    .items_center()
                    .child(div().size(px(8.)).flex_none().rounded_full().bg(rgb(c.red)))
                    .child(div().flex_1().text_sm().child("You're live"))
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
            );
        } else {
            // Wisp's own list of screens, where it has one
            if !d.screens.is_empty() {
                let mut chips = h_flex().gap_2().flex_wrap();
                for (i, s) in d.screens.iter().enumerate() {
                    let pick = Pick::Screen(s.id);
                    chips = chips.child(
                        chip(("share-screen", i), s.label.clone(), d.pick == pick)
                            .on_click(cx.listener(move |t, _, _, cx| t.open_source(pick, cx))),
                    );
                }
                body = body.child(chips);
            }
            body = body.child(self.share_preview(d, cx));
        }

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
            body = body.child(
                v_flex()
                    .gap_3()
                    .child(section_title("Quality"))
                    .child(setting("Resolution", "Lower is easier on viewers' connections", sizes.into_any_element()))
                    .child(setting("Frame rate", "Higher is smoother for games and video", rates.into_any_element())),
            );
            if crate::video::SOUND {
                let mut sound = v_flex().gap_3().child(section_title("Sound")).child(setting(
                    "Share sound",
                    if crate::video::SOUND_FOLLOWS_PICK {
                        "A window or app brings only its own sound, a screen brings everything"
                    } else {
                        "What your computer plays, except Wisp itself"
                    },
                    h_flex()
                        .justify_end()
                        .child(Switch::new("share-sound").checked(d.sound).on_click(cx.listener(|t, v: &bool, _, cx| {
                            if let Some(d) = &mut t.share {
                                d.sound = *v;
                            }
                            cx.notify();
                        })))
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
                    sound = sound.child(setting("Sound from", "Just one app, or everything", chips.into_any_element()));
                }
                body = body.child(sound);
            }
        }

        let mut footer = h_flex()
            .gap_3()
            .items_center()
            .px_5()
            .py_3()
            .border_t_1()
            .border_color(border())
            .bg(alpha(ink(), 0x04));
        match ready.as_ref().filter(|_| !d.live) {
            Some(info) => {
                let (note, warn) = d.note(info);
                footer = footer.child(
                    h_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_2()
                        .items_center()
                        .child(icon(if warn { "triangle-alert" } else { "gauge" }, 14., if warn { c.amber } else { c.muted }))
                        .child(div().text_xs().text_color(rgb(if warn { c.amber } else { c.muted })).child(note)),
                );
            }
            None => footer = footer.child(div().flex_1()),
        }
        footer = footer
            .child(Button::new("share-cancel").label("Cancel").small().ghost().on_click(cx.listener(move |t, _, _, cx| close(t, cx))))
            .when(!idle, |f| {
                f.child(
                    Button::new("share-go")
                        .label(if d.live { "Switch" } else { "Go live" })
                        .small()
                        .primary()
                        .disabled(ready.is_none())
                        .on_click(cx.listener(|t, _, _, cx| t.go_live(cx))),
                )
            });

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
                        .w(px(DIALOG_W))
                        .max_h(relative(0.92))
                        .rounded(px(10.))
                        .overflow_hidden()
                        .bg(rgb(c.panel))
                        .border_1()
                        .border_color(border())
                        .shadow_lg()
                        .child(header)
                        .child(div().id("share-body").flex_1().min_h_0().overflow_y_scroll().child(body))
                        .child(footer),
                )
                .into_any_element(),
        )
    }

    /// The picked screen or window, live, with its size on top.
    fn share_preview(&self, d: &ShareDialog, cx: &mut Context<Self>) -> AnyElement {
        let c = colors();
        let centered = |el: AnyElement| v_flex().size_full().gap_2().items_center().justify_center().p_6().child(el);
        let inner = match (&d.source, &d.preview) {
            (SourceState::Ready(_), Some(image)) => {
                img(image.clone()).size_full().object_fit(ObjectFit::Contain).into_any_element()
            }
            (SourceState::Ready(_), None) => centered(label("Waiting for the first picture...").into_any_element()).into_any_element(),
            (SourceState::Failed(e), _) => centered(
                v_flex()
                    .gap_2()
                    .items_center()
                    .child(icon("triangle-alert", 22., c.red))
                    .child(div().text_sm().text_color(rgb(c.red)).text_center().child(e.clone()))
                    .into_any_element(),
            )
            .into_any_element(),
            _ => centered(
                v_flex()
                    .gap_2()
                    .items_center()
                    .child(icon("monitor", 26., c.dim))
                    .child(label(if d.screens.is_empty() {
                        "Pick a screen or window in the desktop's dialog..."
                    } else {
                        "Starting..."
                    }))
                    .into_any_element(),
            )
            .into_any_element(),
        };
        let pill = |text: String| {
            div()
                .px_2()
                .py(px(3.))
                .rounded(px(5.))
                .bg(rgba(0x000000b0))
                .text_xs()
                .text_color(rgb(0xffffff))
                .child(text)
        };
        let again = d.screens.is_empty() && matches!(d.source, SourceState::Ready(_) | SourceState::Failed(_));
        div()
            .relative()
            .w_full()
            .h(px(PREVIEW_BOX_H))
            .rounded(px(8.))
            .overflow_hidden()
            .bg(rgb(0x0b0b0c))
            .border_1()
            .border_color(border())
            .child(inner)
            .when_some(
                match &d.source {
                    SourceState::Ready(s) => Some(match s.info.refresh {
                        Some(hz) => format!("{} × {} · {hz} Hz", s.info.width, s.info.height),
                        None => format!("{} × {}", s.info.width, s.info.height),
                    }),
                    _ => None,
                },
                |b, text| b.child(div().absolute().left_3().bottom_3().child(pill(text))),
            )
            .when(again, |b| {
                b.child(
                    div()
                        .id("share-repick")
                        .absolute()
                        .right_3()
                        .bottom_3()
                        .px_2()
                        .py(px(3.))
                        .rounded(px(5.))
                        .bg(rgba(0x000000b0))
                        .hover(|s| s.bg(rgba(0x000000e0)))
                        .cursor_pointer()
                        .flex()
                        .gap_1()
                        .items_center()
                        .text_xs()
                        .text_color(rgb(0xffffff))
                        .child(icon("refresh-cw", 12., 0xffffff))
                        .child("Pick another")
                        .on_click(cx.listener(|t, _, _, cx| t.open_source(Pick::Default, cx))),
                )
            })
            .into_any_element()
    }
}

/// One setting: its name and what it does on the left, the control on the right.
fn setting(name: &'static str, hint: &'static str, control: AnyElement) -> impl IntoElement {
    h_flex()
        .gap_4()
        .items_start()
        .child(
            v_flex()
                .w(px(170.))
                .flex_none()
                .pt(px(4.))
                .gap(px(2.))
                .child(div().text_sm().text_color(rgb(colors().text)).child(name))
                .child(label(hint)),
        )
        .child(div().flex_1().min_w_0().child(control))
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
    fn note(&self, info: &crate::video::SourceInfo) -> (String, bool) {
        let options = ShareOptions { height: self.height, fps: self.fps, sound: self.sound && crate::video::SOUND };
        let gpu = matches!(self.gpu, Some(Some(_)));
        let bits = crate::video::estimate(info.width, info.height, options, gpu);
        let upload = format!("About {:.0} Mbit/s up", (bits as f64 / 1_000_000.0).max(1.0));
        let (w, h) = match self.height {
            0 => (info.width, info.height),
            h => (info.width * h / info.height.max(1), h),
        };
        match self.gpu {
            None => (format!("{upload}. Looking for a graphics card encoder..."), false),
            Some(Some(name)) => (format!("{upload}, encoded with {name}"), false),
            Some(None) if (w * h) as u64 * self.fps as u64 > 1920 * 1080 * 60 => {
                (format!("{upload}. No graphics card encoder, so this may stutter"), true)
            }
            Some(None) => (format!("{upload}, encoded on the CPU"), false),
        }
    }
}
