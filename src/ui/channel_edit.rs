//! Create / edit channel dialog. Shown as an overlay in the main window; only
//! the fields that changed are sent when editing (the server rejects setting
//! a channel's name to the name it already has).

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputState, Textarea, TextareaState};
use gpui_kit::component::slider::{Slider, SliderEvent, SliderState};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{Sizable, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{
    AppContext, Context, DismissEvent, Entity, EventEmitter, FontWeight, InteractiveElement, IntoElement,
    ParentElement, Render, SharedString, StatefulInteractiveElement, Styled, Window, div, px, rgb,
};

use super::Model;
use super::widgets::*;
use crate::net::{AdminAction, ChannelProps, ChannelRow, ChannelType, Cmd, Codec, ConId, MaxClients};

const LABEL_W: f32 = 140.;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Family {
    Inherited,
    Unlimited,
    Limited,
}

pub struct ChannelEditor {
    model: Entity<Model>,
    con: ConId,
    /// The channel being edited; None when creating one.
    original: Option<ChannelRow>,
    parent: u64,
    parent_name: String,
    name: Entity<InputState>,
    phonetic: Entity<InputState>,
    topic: Entity<InputState>,
    description: Entity<TextareaState>,
    description_loaded: bool,
    password: Entity<InputState>,
    remove_password: bool,
    kind: ChannelType,
    is_default: bool,
    codec: Codec,
    quality: Entity<SliderState>,
    quality_value: u8,
    latency: Entity<SliderState>,
    latency_value: i32,
    unencrypted: bool,
    unlimited: bool,
    max_clients: Entity<InputState>,
    family: Family,
    max_family: Entity<InputState>,
    talk_power: Entity<InputState>,
    delete_delay: Entity<InputState>,
    error: Option<String>,
    /// The icon to set (editing only), and the picker while it's open.
    icon: Option<u32>,
    icons: Option<Entity<super::icons::IconGrid>>,
}

impl EventEmitter<DismissEvent> for ChannelEditor {}

fn input(window: &mut Window, cx: &mut Context<ChannelEditor>, value: String, placeholder: &str) -> Entity<InputState> {
    let placeholder = placeholder.to_string();
    cx.new(|cx| InputState::new(window, cx).placeholder(placeholder).default_value(value))
}

impl ChannelEditor {
    /// `original` None creates a channel under `parent`.
    pub fn new(
        model: Entity<Model>,
        con: ConId,
        original: Option<ChannelRow>,
        parent: u64,
        parent_name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let o = original.as_ref();
        let text = |f: fn(&ChannelRow) -> String| o.map(f).unwrap_or_default();
        let name = input(window, cx, text(|c| c.name.clone()), "Channel name");
        let phonetic = input(window, cx, text(|c| c.phonetic.clone()), "How text-to-speech says the name");
        let topic = input(window, cx, text(|c| c.topic.clone()), "Shown next to the name");
        let has_desc = o.and_then(|c| c.description.clone());
        let description_loaded = o.is_none() || has_desc.is_some();
        let desc_value = has_desc.unwrap_or_default();
        let description = cx.new(|cx| {
            TextareaState::new(window, cx)
                .rows(4)
                .placeholder(if description_loaded { "" } else { "Loading..." })
                .default_value(desc_value)
        });
        let has_pw = o.is_some_and(|c| c.has_password);
        let password = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder(if has_pw { "Leave empty to keep the current one" } else { "No password" })
        });
        let quality_value = o.map(|c| c.quality).unwrap_or(10);
        let quality =
            cx.new(|_| SliderState::new().min(0.).max(10.).step(1.).default_value(f32::from(quality_value)));
        cx.subscribe(&quality, |this, _, ev: &SliderEvent, cx| {
            let (SliderEvent::Change(v) | SliderEvent::Release(v)) = ev;
            this.quality_value = v.start().round() as u8;
            cx.notify();
        })
        .detach();
        let unencrypted = o.is_some_and(|c| c.unencrypted);
        let latency_value = o.map(|c| c.latency).unwrap_or(1).clamp(1, 10);
        let latency =
            cx.new(|_| SliderState::new().min(1.).max(10.).step(1.).default_value(latency_value as f32));
        cx.subscribe(&latency, |this, _, ev: &SliderEvent, cx| {
            let (SliderEvent::Change(v) | SliderEvent::Release(v)) = ev;
            this.latency_value = v.start().round() as i32;
            cx.notify();
        })
        .detach();
        let limit = o.map(|c| c.limit).unwrap_or(MaxClients::Unlimited);
        let family_limit = o.map(|c| c.family_limit).unwrap_or(MaxClients::Inherited);
        let num = |m: MaxClients| match m {
            MaxClients::Limited(n) => n.to_string(),
            _ => String::new(),
        };
        let max_clients = input(window, cx, num(limit), "e.g. 10");
        let max_family = input(window, cx, num(family_limit), "e.g. 20");
        let talk_power = input(window, cx, o.map(|c| c.talk_power).unwrap_or(0).to_string(), "0");
        let delete_delay = input(window, cx, o.map(|c| c.delete_delay).unwrap_or(0).to_string(), "0");
        name.update(cx, |i, cx| i.focus(window, cx));

        // The description arrives after the dialog opens.
        if !description_loaded {
            let id = o.map(|c| c.id).unwrap_or(0);
            cx.observe_in(&model, window, move |this: &mut Self, model, window, cx| {
                if this.description_loaded {
                    return;
                }
                let desc = model
                    .read(cx)
                    .snap
                    .server(this.con)
                    .and_then(|s| s.channels.iter().find(|c| c.id == id))
                    .and_then(|c| c.description.clone());
                if let Some(d) = desc {
                    this.description_loaded = true;
                    if let Some(orig) = &mut this.original {
                        orig.description = Some(d.clone());
                    }
                    this.description.update(cx, |t, cx| {
                        t.set_placeholder("", window, cx);
                        t.set_value(d, window, cx);
                    });
                    cx.notify();
                }
            })
            .detach();
        }

        Self {
            con,
            parent,
            parent_name,
            kind: o.map(|c| c.kind).unwrap_or(ChannelType::Permanent),
            is_default: o.is_some_and(|c| c.is_default),
            codec: o.map(|c| c.codec).unwrap_or(Codec::OpusVoice),
            original,
            model,
            name,
            phonetic,
            topic,
            description,
            description_loaded,
            password,
            remove_password: false,
            quality,
            quality_value,
            latency,
            latency_value,
            unencrypted,
            unlimited: !matches!(limit, MaxClients::Limited(_)),
            max_clients,
            family: match family_limit {
                MaxClients::Inherited => Family::Inherited,
                MaxClients::Unlimited => Family::Unlimited,
                MaxClients::Limited(_) => Family::Limited,
            },
            max_family,
            talk_power,
            delete_delay,
            error: None,
            icon: None,
            icons: None,
        }
        .with_icon()
    }

    fn with_icon(mut self) -> Self {
        self.icon = self.original.as_ref().map(|o| o.icon_id).filter(|i| *i != 0);
        self
    }

    fn toggle_icons(&mut self, cx: &mut Context<Self>) {
        if self.icons.take().is_some() {
            cx.notify();
            return;
        }
        let (model, con, current) = (self.model.clone(), self.con, self.icon);
        let grid = cx.new(|cx| super::icons::IconGrid::new(model, con, current, cx));
        cx.subscribe(&grid, |this, _, ev: &super::icons::IconPicked, cx| {
            this.icon = ev.0;
            cx.notify();
        })
        .detach();
        self.icons = Some(grid);
        cx.notify();
    }

    fn value(e: &Entity<InputState>, cx: &App) -> String {
        e.read(cx).value().trim().to_string()
    }

    /// Everything the form says, as full properties.
    fn read_form(&self, cx: &App) -> Result<ChannelProps, String> {
        let name = Self::value(&self.name, cx);
        if name.is_empty() {
            return Err("The channel needs a name.".into());
        }
        let number = |e: &Entity<InputState>, what: &str| -> Result<u16, String> {
            Self::value(e, cx).parse::<u16>().map_err(|_| format!("{what} must be a number."))
        };
        let max_clients = if self.unlimited {
            MaxClients::Unlimited
        } else {
            MaxClients::Limited(number(&self.max_clients, "Max. clients")?)
        };
        let max_family = match self.family {
            Family::Inherited => MaxClients::Inherited,
            Family::Unlimited => MaxClients::Unlimited,
            Family::Limited => MaxClients::Limited(number(&self.max_family, "Max. family clients")?),
        };
        let talk_power = Self::value(&self.talk_power, cx)
            .parse::<i32>()
            .map_err(|_| "Needed talk power must be a number.".to_string())?;
        let delete_delay = Self::value(&self.delete_delay, cx)
            .parse::<u64>()
            .map_err(|_| "Delete delay must be a number of seconds.".to_string())?;
        let pw = self.password.read(cx).value().to_string();
        Ok(ChannelProps {
            name: Some(name),
            phonetic: Some(Self::value(&self.phonetic, cx)),
            topic: Some(Self::value(&self.topic, cx)),
            description: Some(self.description.read(cx).value().trim_end().to_string()),
            password: if self.remove_password {
                Some(None)
            } else if pw.is_empty() {
                None
            } else {
                Some(Some(pw))
            },
            kind: Some(self.kind),
            is_default: Some(self.is_default && self.kind == ChannelType::Permanent),
            codec: Some(self.codec),
            quality: Some(self.quality_value),
            max_clients: Some(max_clients),
            max_family: Some(max_family),
            talk_power: Some(talk_power),
            delete_delay: Some(delete_delay),
            latency: Some(self.latency_value),
            unencrypted: Some(self.unencrypted),
        })
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        let mut p = match self.read_form(cx) {
            Ok(p) => p,
            Err(e) => {
                self.error = Some(e);
                cx.notify();
                return;
            }
        };
        let action = match &self.original {
            None => AdminAction::CreateChannel { parent: self.parent, props: p },
            Some(o) => {
                // keep only what changed
                fn keep<T: PartialEq>(field: &mut Option<T>, old: T) {
                    if field.as_ref() == Some(&old) {
                        *field = None;
                    }
                }
                keep(&mut p.name, o.name.clone());
                keep(&mut p.phonetic, o.phonetic.clone());
                keep(&mut p.topic, o.topic.clone());
                match &o.description {
                    Some(d) => keep(&mut p.description, d.clone()),
                    // never loaded: don't wipe it
                    None => p.description = None,
                }
                keep(&mut p.kind, o.kind);
                keep(&mut p.is_default, o.is_default);
                keep(&mut p.codec, o.codec);
                keep(&mut p.quality, o.quality);
                keep(&mut p.max_clients, o.limit);
                keep(&mut p.max_family, o.family_limit);
                keep(&mut p.talk_power, o.talk_power);
                keep(&mut p.delete_delay, o.delete_delay);
                keep(&mut p.latency, o.latency);
                keep(&mut p.unencrypted, o.unencrypted);
                if p.password == Some(None) && !o.has_password {
                    p.password = None;
                }
                let old_icon = Some(o.icon_id).filter(|i| *i != 0);
                if self.icon != old_icon {
                    let target = crate::net::admin::PermTarget::Channel(o.id);
                    if let Some(write) = crate::net::admin::icon_write(target, self.icon) {
                        self.model.read(cx).net.send(Cmd::Write { con: self.con, write });
                    }
                }
                if p.is_empty() {
                    cx.emit(DismissEvent);
                    return;
                }
                AdminAction::EditChannel { channel: o.id, props: p }
            }
        };
        self.model.read(cx).net.send(Cmd::Admin { con: self.con, action });
        cx.emit(DismissEvent);
    }

    fn field(&self, text: &'static str, el: impl IntoElement) -> impl IntoElement {
        h_flex()
            .gap_4()
            .items_center()
            .min_h(px(32.))
            .child(div().w(px(LABEL_W)).flex_none().text_sm().text_color(rgb(MUTED)).child(text))
            .child(div().flex_1().min_w_0().child(el))
    }

    fn kind_chips(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut row = h_flex().gap_2();
        for (i, (k, text)) in [
            (ChannelType::Permanent, "Permanent"),
            (ChannelType::SemiPermanent, "Semi-permanent"),
            (ChannelType::Temporary, "Temporary"),
        ]
        .into_iter()
        .enumerate()
        {
            row = row.child(chip(("kind", i), text, self.kind == k).on_click(cx.listener(move |t, _, _, cx| {
                t.kind = k;
                cx.notify();
            })));
        }
        row
    }

    fn codec_chips(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut row = h_flex().gap_2();
        for (i, (c, text)) in [(Codec::OpusVoice, "Opus Voice"), (Codec::OpusMusic, "Opus Music")].into_iter().enumerate() {
            row = row.child(chip(("codec", i), text, self.codec == c).on_click(cx.listener(move |t, _, _, cx| {
                t.codec = c;
                cx.notify();
            })));
        }
        if !matches!(self.codec, Codec::OpusVoice | Codec::OpusMusic) {
            row = row.child(label(format!("{:?}", self.codec)));
        }
        row
    }

    fn family_chips(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut row = h_flex().gap_2();
        for (i, (f, text)) in
            [(Family::Inherited, "Inherited"), (Family::Unlimited, "Unlimited"), (Family::Limited, "Limited")]
                .into_iter()
                .enumerate()
        {
            row = row.child(chip(("family", i), text, self.family == f).on_click(cx.listener(move |t, _, _, cx| {
                t.family = f;
                cx.notify();
            })));
        }
        row
    }
}

use gpui_kit::App;

impl Render for ChannelEditor {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let title: SharedString = match (&self.original, self.parent_name.is_empty()) {
            (Some(o), _) => format!("Edit {}", o.name).into(),
            (None, true) => "New channel".into(),
            (None, false) => format!("New channel in {}", self.parent_name).into(),
        };
        let has_pw = self.original.as_ref().is_some_and(|o| o.has_password);
        let creating = self.original.is_none();

        let general = v_flex()
            .gap_2()
            .child(section_title("General"))
            .child(self.field("Name", Input::new(&self.name)))
            .child(self.field("Phonetic name", Input::new(&self.phonetic)))
            .child(self.field("Topic", Input::new(&self.topic)))
            .when(!creating, |d| {
                let path = self.icon.and_then(|id| {
                    let s = self.model.read(cx).snap.server(self.con)?;
                    let b = self.model.read(cx).settings.bookmarks.iter().find(|b| b.id == s.bookmark)?;
                    Some(crate::net::icon_dir(&b.address).join(id.to_string())).filter(|p| p.exists())
                });
                d.child(self.field(
                    "Icon",
                    h_flex()
                        .gap_3()
                        .items_center()
                        .child(match path {
                            Some(p) => gpui_kit::img(p).size(px(16.)).into_any_element(),
                            None => label(if self.icon.is_some() { "Custom" } else { "None" }).into_any_element(),
                        })
                        .child(Button::new("ce-icon").label(if self.icons.is_some() { "Done" } else { "Change" }).small().ghost().on_click(
                            cx.listener(|t, _, _, cx| t.toggle_icons(cx)),
                        )),
                ))
                .when_some(self.icons.clone(), |d, g| d.child(div().pl(px(LABEL_W + 16.)).child(g)))
            })
            .child(
                h_flex()
                    .gap_4()
                    .items_start()
                    .child(div().w(px(LABEL_W)).flex_none().pt_1p5().text_sm().text_color(rgb(MUTED)).child("Description"))
                    .child(div().flex_1().min_w_0().child(Textarea::new(&self.description))),
            )
            .child(self.field("Password", Input::new(&self.password).mask_toggle()))
            .when(has_pw, |d| {
                d.child(self.field(
                    "Remove password",
                    Switch::new("rm-pw").checked(self.remove_password).on_click(cx.listener(|t, v: &bool, _, cx| {
                        t.remove_password = *v;
                        cx.notify();
                    })),
                ))
            });

        let kind = v_flex()
            .gap_2()
            .child(section_title("Type"))
            .child(self.field("Lifetime", self.kind_chips(cx)))
            .when(self.kind == ChannelType::Temporary, |d| {
                d.child(self.field(
                    "Delete delay",
                    h_flex().gap_2().items_center().child(div().w(px(90.)).child(Input::new(&self.delete_delay))).child(label("seconds after the last one leaves")),
                ))
            })
            .when(self.kind == ChannelType::Permanent, |d| {
                d.child(self.field(
                    "Default channel",
                    Switch::new("default").checked(self.is_default).on_click(cx.listener(|t, v: &bool, _, cx| {
                        t.is_default = *v;
                        cx.notify();
                    })),
                ))
            });

        let audio = v_flex()
            .gap_2()
            .child(section_title("Audio"))
            .child(self.field("Codec", self.codec_chips(cx)))
            .child(self.field(
                "Quality",
                h_flex()
                    .gap_3()
                    .items_center()
                    .child(div().flex_1().child(Slider::new(&self.quality)))
                    .child(div().w(px(24.)).text_sm().text_right().child(self.quality_value.to_string())),
            ))
            .child(self.field(
                "Latency",
                h_flex()
                    .gap_3()
                    .items_center()
                    .child(div().flex_1().child(Slider::new(&self.latency)))
                    .child(div().w(px(60.)).text_sm().text_right().child(format!("{} ms", self.latency_value * 20))),
            ))
            .child(self.field(
                "Unencrypted voice",
                Switch::new("unenc").checked(self.unencrypted).on_click(cx.listener(|t, v: &bool, _, cx| {
                    t.unencrypted = *v;
                    cx.notify();
                })),
            ));

        let limits = v_flex()
            .gap_2()
            .child(section_title("Limits"))
            .child(self.field(
                "Max. clients",
                h_flex()
                    .gap_3()
                    .items_center()
                    .child(Switch::new("unlimited").checked(self.unlimited).on_click(cx.listener(|t, v: &bool, _, cx| {
                        t.unlimited = *v;
                        cx.notify();
                    })))
                    .child(label("Unlimited"))
                    .when(!self.unlimited, |d| d.child(div().w(px(90.)).child(Input::new(&self.max_clients)))),
            ))
            .child(self.field(
                "Family clients",
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(self.family_chips(cx))
                    .when(self.family == Family::Limited, |d| d.child(div().w(px(80.)).child(Input::new(&self.max_family)))),
            ))
            .child(self.field("Needed talk power", div().w(px(90.)).child(Input::new(&self.talk_power))));

        v_flex()
            .w(px(580.))
            .max_h(gpui_kit::relative(0.9))
            .rounded(px(8.))
            .bg(rgb(PANEL))
            .border_1()
            .border_color(border())
            .shadow_lg()
            .overflow_hidden()
            .child(header_row().child(div().flex_1().font_weight(FontWeight::SEMIBOLD).truncate().child(title)))
            .child(
                v_flex()
                    .id("channel-edit-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_5()
                    .py_4()
                    .gap_6()
                    .child(general)
                    .child(kind)
                    .child(audio)
                    .child(limits),
            )
            .child(
                footer_row()
                    .px_5()
                    .when_some(self.error.clone(), |d, e| d.child(div().text_xs().text_color(rgb(RED)).child(e)))
                    .child(div().flex_1())
                    .child(Button::new("ce-cancel").label("Cancel").small().ghost().on_click(cx.listener(|_, _, _, cx| {
                        cx.emit(DismissEvent);
                    })))
                    .child(
                        Button::new("ce-save")
                            .label(if creating { "Create" } else { "Save" })
                            .small()
                            .primary()
                            .on_click(cx.listener(|t, _, _, cx| t.submit(cx))),
                    ),
            )
    }
}
