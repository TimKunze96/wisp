//! Settings window: audio, servers, hotkeys and general options. Changes apply
//! right away (except the server form, which has Save).

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::slider::{Slider, SliderEvent, SliderState};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Disableable, Sizable, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{
    AnyElement, AppContext, Context, Entity, FocusHandle, FontWeight, InteractiveElement, IntoElement, KeyDownEvent,
    ParentElement, Render, SharedString, StatefulInteractiveElement, Styled, Window, div, px, rgb, rgba,
};

use super::Model;
use super::form::{self, Group, block, card, field, item, notice};
use super::widgets::*;
use crate::audio::{DeviceInfo, list_devices};
use crate::config::{Bookmark, Settings, VoiceMode, WhisperList, WhisperTarget, new_id};
use crate::hotkeys::{self, Action};
use crate::net::{Cmd, Status};
use crate::notify::EventKind;

/// What a key press is being recorded for.
#[derive(Clone, PartialEq, Eq)]
enum Rec {
    Action(Action),
    /// A whisper list, by id.
    Whisper(String),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SettingsTab {
    Audio,
    Servers,
    Hotkeys,
    Whisper,
    Notifications,
    Identities,
    General,
}

impl SettingsTab {
    fn title(self) -> &'static str {
        match self {
            SettingsTab::Audio => "Audio",
            SettingsTab::Servers => "Servers",
            SettingsTab::Hotkeys => "Hotkeys",
            SettingsTab::Whisper => "Whisper",
            SettingsTab::Identities => "Identities",
            SettingsTab::Notifications => "Notifications",
            SettingsTab::General => "General",
        }
    }
    /// What the page is about, under its title.
    fn about(self) -> &'static str {
        match self {
            SettingsTab::Audio => "Your microphone, your speakers and how you talk",
            SettingsTab::Servers => "The servers you go to and how you join them",
            SettingsTab::Hotkeys => "Keys that work anywhere, even with Wisp in the background",
            SettingsTab::Whisper => "Talk to some people or channels without leaving yours",
            SettingsTab::Identities => "Who servers know you as",
            SettingsTab::Notifications => "Sounds and popups for what happens on a server",
            SettingsTab::General => "Window, startup and help when something breaks",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            SettingsTab::Audio => "audio-lines",
            SettingsTab::Servers => "server",
            SettingsTab::Hotkeys => "keyboard",
            SettingsTab::Whisper => "radio",
            SettingsTab::Identities => "fingerprint-pattern",
            SettingsTab::Notifications => "bell",
            SettingsTab::General => "sliders-horizontal",
        }
    }
}

struct BookmarkForm {
    id: String,
    is_new: bool,
    label: Entity<InputState>,
    address: Entity<InputState>,
    password: Entity<InputState>,
    nickname: Entity<InputState>,
    phonetic: Entity<InputState>,
    channel: Entity<InputState>,
    auto_connect: bool,
    voice_mode: Option<VoiceMode>,
    identity: Option<String>,
    quiet: bool,
    error: Option<String>,
}

/// A running security level search.
struct Raising {
    id: String,
    target: u8,
    tried: std::sync::Arc<std::sync::atomic::AtomicU64>,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

pub struct SettingsView {
    model: Entity<Model>,
    tab: SettingsTab,
    inputs: Vec<DeviceInfo>,
    outputs: Vec<DeviceInfo>,
    mic: Entity<SliderState>,
    out: Entity<SliderState>,
    sounds: Entity<SliderState>,
    duck: Entity<SliderState>,
    can_speak: bool,
    /// The input is already an RNNoise filter (name), checked when the tab opens.
    rnnoise_source: Option<String>,
    vad: Entity<SliderState>,
    release: Entity<SliderState>,
    /// The server being edited; None shows the list.
    form: Option<BookmarkForm>,
    focus: FocusHandle,
    /// Hotkey waiting for a key press.
    recording: Option<Rec>,
    /// Name fields of the whisper lists, by list id.
    whisper_names: std::collections::HashMap<String, Entity<InputState>>,
    identities: Vec<crate::identity::StoredIdentity>,
    identity_names: std::collections::HashMap<String, Entity<InputState>>,
    identity_paste: Option<Entity<InputState>>,
    raising: Option<Raising>,
    identity_note: Option<(String, bool)>,
    confirm_delete_identity: Option<String>,
    /// Binds Hyprland already has (normalized spec, description).
    taken: Vec<(String, String)>,
    hotkey_error: Option<String>,
}

impl SettingsView {
    pub fn new(model: Entity<Model>, tab: SettingsTab, window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.observe(&model, |_, _, cx| cx.notify()).detach();
        let s = model.read(cx).settings.clone();
        let (inputs, outputs) = list_devices();
        let slider = |min: f32, max: f32, step: f32, v: f32, cx: &mut Context<Self>| {
            cx.new(|_| SliderState::new().min(min).max(max).step(step).default_value(v))
        };
        let mic = slider(0., 200., 5., s.mic_volume * 100., cx);
        let out = slider(0., 200., 5., s.output_volume * 100., cx);
        let vad = slider(-60., 0., 1., s.vad_threshold_db, cx);
        let release = slider(0., 1000., 20., s.ptt_release_ms as f32, cx);
        let sounds = slider(0., 100., 5., s.sound_volume * 100., cx);
        Self::on_slide(&sounds, cx, |s, v| s.sound_volume = v / 100.);
        let duck = slider(0., 90., 5., s.duck_level * 100., cx);
        Self::on_slide(&duck, cx, |s, v| s.duck_level = v / 100.);
        Self::on_slide(&mic, cx, |s, v| s.mic_volume = v / 100.);
        Self::on_slide(&out, cx, |s, v| s.output_volume = v / 100.);
        Self::on_slide(&vad, cx, |s, v| s.vad_threshold_db = v);
        Self::on_slide(&release, cx, |s, v| s.ptt_release_ms = v.round() as u32);
        let focus = cx.focus_handle();
        let mut this = Self {
            model,
            tab,
            inputs,
            outputs,
            mic,
            out,
            sounds,
            duck,
            can_speak: crate::notify::can_speak(),
            rnnoise_source: None,
            vad,
            release,
            form: None,
            focus,
            recording: None,
            whisper_names: Default::default(),
            identities: crate::identity::load_all(),
            identity_names: Default::default(),
            identity_paste: None,
            raising: None,
            identity_note: None,
            confirm_delete_identity: None,
            taken: Vec::new(),
            hotkey_error: None,
        };
        this.switch(tab, window, cx);
        this
    }

    fn on_slide(slider: &Entity<SliderState>, cx: &mut Context<Self>, apply: fn(&mut Settings, f32)) {
        cx.subscribe(slider, move |this, _, ev: &SliderEvent, cx| {
            let (SliderEvent::Change(v) | SliderEvent::Release(v)) = ev;
            let v = v.start();
            this.model.update(cx, |m, cx| m.update_settings(cx, |s| apply(s, v)));
        })
        .detach();
    }

    pub fn switch(&mut self, tab: SettingsTab, _window: &mut Window, cx: &mut Context<Self>) {
        self.tab = tab;
        self.form = None;
        self.recording = None;
        if tab == SettingsTab::Identities {
            self.identities = crate::identity::load_all();
        }
        if tab == SettingsTab::Audio {
            let input = self.model.read(cx).settings.input_device.clone();
            self.rnnoise_source = crate::audio::rnnoise_source(input.as_deref());
        }
        if matches!(tab, SettingsTab::Hotkeys | SettingsTab::Whisper) {
            self.taken = hotkeys::taken();
        }
        cx.notify();
    }

    // ---- servers -------------------------------------------------------

    pub(super) fn edit(&mut self, b: Bookmark, is_new: bool, window: &mut Window, cx: &mut Context<Self>) {
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

    fn save_form(&mut self, cx: &mut Context<Self>) -> Option<Bookmark> {
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

    fn servers(&mut self, cx: &mut Context<Self>) -> AnyElement {
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

    fn server_form(&mut self, cx: &mut Context<Self>) -> AnyElement {
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

    // ---- audio ---------------------------------------------------------

    fn device_list(
        &self,
        id: &'static str,
        devices: &[DeviceInfo],
        current: Option<String>,
        input: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut list = v_flex().gap_px();
        let mut rows: Vec<(Option<String>, String)> = vec![(None, "System default".into())];
        rows.extend(devices.iter().map(|d| (Some(d.id.clone()), d.name.clone())));
        for (i, (dev, name)) in rows.into_iter().enumerate() {
            let on = dev == current;
            list = list.child(
                h_flex()
                    .id((id, i))
                    .h(px(30.))
                    .gap_2()
                    .px_2()
                    .rounded(px(5.))
                    .cursor_pointer()
                    .items_center()
                    .when(on, |d| d.bg(active_bg()))
                    .hover(|d| d.bg(hover_bg()))
                    .on_click(cx.listener(move |t, _, _, cx| {
                        let dev = dev.clone();
                        t.model.update(cx, |m, cx| {
                            m.update_settings(cx, |s| {
                                if input {
                                    s.input_device = dev;
                                } else {
                                    s.output_device = dev;
                                }
                            })
                        });
                        if input {
                            let input = t.model.read(cx).settings.input_device.clone();
                            t.rnnoise_source = crate::audio::rnnoise_source(input.as_deref());
                        }
                    }))
                    .child(div().w(px(14.)).when(on, |d| d.child(icon("check", 14., colors().text))))
                    .child(div().text_sm().truncate().text_color(rgb(if on { colors().text } else { colors().soft })).child(name)),
            );
        }
        div().id(id).max_h(px(168.)).overflow_y_scroll().p_1p5().child(list).into_any_element()
    }

    fn audio(&self, cx: &mut Context<Self>) -> AnyElement {
        let m = self.model.read(cx);
        let s = m.settings.clone();
        let level = ((m.snap.mic_level_db + 60.) / 60.).clamp(0., 1.);
        let threshold = ((s.vad_threshold_db + 60.) / 60.).clamp(0., 1.);
        let open = m.snap.transmitting;
        let inputs = self.device_list("in", &self.inputs, s.input_device.clone(), true, cx);
        let outputs = self.device_list("out", &self.outputs, s.output_device.clone(), false, cx);
        let mode_btn = |id: &'static str, text: &'static str, mode: VoiceMode, cx: &mut Context<Self>| {
            let on = s.voice_mode == mode;
            div()
                .id(id)
                .h(px(30.))
                .px_3()
                .flex()
                .items_center()
                .rounded(px(5.))
                .cursor_pointer()
                .text_sm()
                .border_1()
                .border_color(if on { alpha(ink(), 0x33) } else { border() })
                .bg(if on { alpha(ink(), 0x14) } else { rgba(0x00000000) })
                .text_color(rgb(if on { colors().text } else { colors().muted }))
                .hover(|d| d.bg(hover_bg()))
                .child(text)
                .on_click(cx.listener(move |t, _, _, cx| {
                    t.model.update(cx, |m, cx| m.update_settings(cx, |s| s.voice_mode = mode));
                }))
        };
        let meter = div()
            .h(px(4.))
            .w_full()
            .rounded_full()
            .overflow_hidden()
            .relative()
            .child(segments(level, if open { colors().green } else { colors().muted }))
            .when(s.voice_mode == VoiceMode::VoiceActivation, |d| {
                d.child(div().absolute().top_0().left(gpui_kit::relative(threshold)).w(px(2.)).h_full().bg(rgb(colors().amber)))
            });
        let switch = |id: &'static str, on: bool, set: fn(&mut Settings, bool), cx: &mut Context<Self>| {
            Switch::new(id).checked(on).on_click(cx.listener(move |t, v: &bool, _, cx| {
                let v = *v;
                t.model.update(cx, |m, cx| m.update_settings(cx, |s| set(s, v)));
            }))
        };
        v_flex()
            .gap_6()
            .when(cfg!(not(target_os = "linux")), |d| {
                d.child(notice("info", colors().muted, "A different device takes effect the next time Wisp starts."))
            })
            .child(
                Group::new("mic", "Microphone")
                    .child(inputs)
                    .child(form::slider("Volume", format!("{:.0}%", s.mic_volume * 100.), Slider::new(&self.mic)))
                    .child(field("Level", meter))
                    .when(crate::audio::HAS_ECHO_CANCEL, |d| {
                        d.child(item(
                            "Echo cancellation",
                            Some("For speakers: keeps what Wisp plays out of your mic (WebRTC)".into()),
                            switch("echo", s.echo_cancel, |s, v| s.echo_cancel = v, cx),
                        ))
                    })
                    .child(item(
                        "Noise suppression",
                        Some(match &self.rnnoise_source {
                            Some(src) => format!("Your microphone already goes through RNNoise ({src}), so Wisp leaves it alone").into(),
                            None => "Filters out fans, keyboards and background hum (RNNoise)".into(),
                        }),
                        switch("denoise", s.noise_suppression, |s, v| s.noise_suppression = v, cx),
                    )),
            )
            .child(
                Group::new("volume-2", "Speakers")
                    .child(outputs)
                    .child(form::slider("Volume", format!("{:.0}%", s.output_volume * 100.), Slider::new(&self.out)))
                    .child(item(
                        "Even out voices",
                        Some("Turns quiet people up and loud ones down, on top of their own volume".into()),
                        switch("agc", s.auto_gain, |s, v| s.auto_gain = v, cx),
                    ))
                    .when(crate::audio::HAS_DUCKING, |d| {
                        d.child(item(
                            "Lower other apps",
                            Some("While someone talks, music and videos play quieter".into()),
                            switch("duck", s.ducking, |s, v| s.ducking = v, cx),
                        ))
                    })
                    .when(crate::audio::HAS_DUCKING && s.ducking, |d| {
                        d.child(form::slider("Lower them to", format!("{:.0}%", s.duck_level * 100.), Slider::new(&self.duck)))
                    }),
            )
            .child(
                Group::new("audio-lines", "Voice")
                    .child(field(
                        "Mode",
                        h_flex()
                            .gap_2()
                            .child(mode_btn("m-ptt", "Push-to-talk", VoiceMode::PushToTalk, cx))
                            .child(mode_btn("m-vad", "Voice activation", VoiceMode::VoiceActivation, cx)),
                    ))
                    .when(s.voice_mode == VoiceMode::VoiceActivation, |d| {
                        d.child(form::slider("Threshold", format!("{:.0} dB", s.vad_threshold_db), Slider::new(&self.vad)))
                    })
                    .when(s.voice_mode == VoiceMode::PushToTalk, |d| {
                        d.child(form::slider("Release delay", format!("{} ms", s.ptt_release_ms), Slider::new(&self.release)))
                    }),
            )
            .into_any_element()
    }

    // ---- hotkeys -------------------------------------------------------

    fn set_hotkey(&mut self, rec: Rec, keys: Option<String>, cx: &mut Context<Self>) {
        self.model.update(cx, |m, cx| {
            m.update_settings(cx, |s| match (rec, keys) {
                (Rec::Action(a), Some(k)) => {
                    s.hotkeys.insert(a, k);
                }
                (Rec::Action(a), None) => {
                    s.hotkeys.remove(&a);
                }
                (Rec::Whisper(id), k) => {
                    if let Some(l) = s.whisper_lists.iter_mut().find(|l| l.id == id) {
                        l.hotkey = k.unwrap_or_default();
                    }
                }
            })
        });
        self.apply_hotkeys(cx);
    }

    /// Rewrite wisp.lua from the settings and reload Hyprland.
    fn apply_hotkeys(&mut self, cx: &mut Context<Self>) {
        let s = &self.model.read(cx).settings;
        self.hotkey_error = hotkeys::apply(&s.hotkeys, &s.whisper_lists).err();
        self.taken = hotkeys::taken();
        cx.notify();
    }

    fn on_key(&mut self, ev: &KeyDownEvent, cx: &mut Context<Self>) {
        let Some(action) = self.recording.clone() else { return };
        let k = &ev.keystroke;
        if k.key == "escape" && !k.modifiers.modified() {
            self.recording = None;
            cx.notify();
            return;
        }
        let m = k.modifiers;
        if let Some(spec) = hotkeys::spec_from_keystroke(&k.key, m.control, m.alt, m.shift, m.platform) {
            self.recording = None;
            self.set_hotkey(action, Some(spec), cx);
        }
    }

    fn hotkeys_tab(&self, cx: &mut Context<Self>) -> AnyElement {
        let set = self.model.read(cx).settings.hotkeys.clone();
        let external = hotkeys::external_ptt();
        let mut list = Group::new("keyboard", "Actions");
        for (i, action) in Action::ALL.into_iter().enumerate() {
            let keys = set.get(&action).cloned().filter(|k| !k.is_empty());
            let recording = self.recording == Some(Rec::Action(action));
            let conflict = keys
                .as_ref()
                .and_then(|k| self.taken.iter().find(|(t, _)| *t == hotkeys::normalize(k)))
                .map(|(_, d)| d.clone());
            let chip = key_chip(
                match &keys {
                    None if action == Action::PushToTalk && external => "F13 (bindings.lua)".into(),
                    None => "Not set".into(),
                    Some(k) => k.clone(),
                },
                keys.is_some(),
                recording,
            )
            .min_w(px(140.));
            list = list.child(
                v_flex()
                    .px_4()
                    .py_3()
                    .gap_1p5()
                    .child(
                        h_flex()
                            .gap_3()
                            .items_center()
                            .child(icon(action.icon(), 16., colors().muted))
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .child(div().text_sm().child(action.title()))
                                    .child(label(action.hint())),
                            )
                            .child(chip)
                            .child(
                                Button::new(SharedString::from(format!("rec-{i}")))
                                    .label(if recording { "Cancel" } else { "Record" })
                                    .small()
                                    .ghost()
                                    .on_click(cx.listener(move |t, _, window, cx| {
                                        let rec = Some(Rec::Action(action));
                                        t.recording = if t.recording == rec { None } else { rec };
                                        window.focus(&t.focus, cx);
                                        cx.notify();
                                    })),
                            )
                            .child(
                                icon_btn(SharedString::from(format!("clr-{i}")), "x", colors().muted, "Clear")
                                    .when(keys.is_none(), |d| d.invisible())
                                    .on_click(cx.listener(move |t, _, _, cx| t.set_hotkey(Rec::Action(action), None, cx))),
                            ),
                    )
                    .when_some(conflict, |d, c| {
                        d.child(
                            h_flex()
                                .pl(px(28.))
                                .gap_1p5()
                                .items_center()
                                .child(icon("triangle-alert", 12., colors().amber))
                                .child(label(format!("Also bound in Hyprland: {c}")).text_color(rgb(colors().amber))),
                        )
                    }),
            );
        }
        let included = hotkeys::included();
        v_flex()
            .gap_4()
            .when(!included, |d| {
                d.child(notice(
                    "triangle-alert",
                    colors().amber,
                    format!("Add {} to ~/.config/hypr/hyprland.lua", hotkeys::INCLUDE_LINE),
                ))
            })
            .when_some(self.hotkey_error.clone(), |d, e| d.child(notice("triangle-alert", colors().red, e)))
            .child(list.hint(hotkeys::about()))
            .into_any_element()
    }

    // ---- general -------------------------------------------------------

    // ---- identities ----------------------------------------------------

    fn import_identity(&mut self, text: String, cx: &mut Context<Self>) {
        match crate::identity::import(&text, "Imported") {
            Ok(i) => self.identity_note = Some((format!("Imported {} (level {})", i.name, i.level()), false)),
            Err(e) => self.identity_note = Some((e.to_string(), true)),
        }
        self.identities = crate::identity::load_all();
        cx.notify();
    }

    fn start_raise(&mut self, ident: crate::identity::StoredIdentity, target: u8, cx: &mut Context<Self>) {
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

    fn identities_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
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

    // ---- whisper -------------------------------------------------------

    fn whisper_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let lists = self.model.read(cx).settings.whisper_lists.clone();
        // a name field per list, kept across renders
        for l in &lists {
            if self.whisper_names.contains_key(&l.id) {
                continue;
            }
            let id = l.id.clone();
            let input = cx.new(|cx| InputState::new(window, cx).placeholder("List name").default_value(l.name.clone()));
            cx.subscribe(&input, move |t, input, ev: &gpui_kit::component::input::InputEvent, cx| {
                use gpui_kit::component::input::InputEvent;
                match ev {
                    InputEvent::Change => {
                        let name = input.read(cx).value().trim().to_string();
                        let id = id.clone();
                        t.model.update(cx, |m, cx| {
                            m.update_settings(cx, |s| {
                                if let Some(l) = s.whisper_lists.iter_mut().find(|l| l.id == id) {
                                    l.name = name;
                                }
                            })
                        });
                    }
                    // the bind's description carries the name
                    InputEvent::Blur | InputEvent::PressEnter { .. } => t.apply_hotkeys(cx),
                    _ => {}
                }
            })
            .detach();
            self.whisper_names.insert(l.id.clone(), input);
        }

        let mut col = v_flex().gap_4();
        if lists.is_empty() {
            col = col.child(label("No whisper lists yet."));
        }
        for (i, l) in lists.iter().enumerate() {
            let id = l.id.clone();
            let recording = self.recording == Some(Rec::Whisper(id.clone()));
            let conflict = (!l.hotkey.is_empty())
                .then(|| self.taken.iter().find(|(t, _)| *t == hotkeys::normalize(&l.hotkey)))
                .flatten()
                .map(|(_, d)| d.clone());
            let mut targets = h_flex().flex_wrap().gap_2();
            if l.targets.is_empty() {
                targets = targets.child(label("No targets. Right-click people or channels in the main window to add them.").w_full());
            }
            for (j, t) in l.targets.iter().enumerate() {
                let (id, t2) = (id.clone(), t.clone());
                targets = targets.child(
                    h_flex()
                        .gap_1p5()
                        .items_center()
                        .h(px(28.))
                        .pl_2p5()
                        .pr_1()
                        .rounded(px(5.))
                        .border_1()
                        .border_color(border())
                        .bg(rgb(colors().raised))
                        .child(icon(t.icon(), 13., colors().muted))
                        .child(div().text_sm().child(t.label()))
                        .child(
                            div()
                                .id(SharedString::from(format!("wt-{i}-{j}")))
                                .rounded(px(3.))
                                .p_0p5()
                                .cursor_pointer()
                                .hover(|d| d.bg(hover_bg()))
                                .child(icon("x", 12., colors().dim))
                                .on_click(cx.listener(move |t, _, _, cx| {
                                    let (id, target) = (id.clone(), t2.clone());
                                    t.model.update(cx, |m, cx| {
                                        m.update_settings(cx, |s| {
                                            if let Some(l) = s.whisper_lists.iter_mut().find(|l| l.id == id) {
                                                l.targets.retain(|x| *x != target);
                                            }
                                        })
                                    });
                                })),
                        ),
                );
            }
            // the targets that aren't a person or channel
            let mut extras = h_flex().gap_2();
            for (k, extra) in [WhisperTarget::Commanders, WhisperTarget::Subchannels, WhisperTarget::ParentChannel]
                .into_iter()
                .enumerate()
            {
                if l.targets.contains(&extra) {
                    continue;
                }
                let id = id.clone();
                extras = extras.child(
                    chip(SharedString::from(format!("wx-{i}-{k}")), format!("+ {}", extra.label()), false).on_click(
                        cx.listener(move |t, _, _, cx| {
                            let (id, extra) = (id.clone(), extra.clone());
                            t.model.update(cx, |m, cx| {
                                m.update_settings(cx, |s| {
                                    if let Some(l) = s.whisper_lists.iter_mut().find(|l| l.id == id) {
                                        l.targets.push(extra);
                                    }
                                })
                            });
                        }),
                    ),
                );
            }
            let key_chip = key_chip(
                if l.hotkey.is_empty() { "No key".into() } else { l.hotkey.clone() },
                !l.hotkey.is_empty(),
                recording,
            )
            .min_w(px(120.));
            let (rec_id, clear_id, del_id) = (id.clone(), id.clone(), id.clone());
            col = col.child(
                card()
                    .gap_3()
                    .p_4()
                    .child(
                        h_flex()
                            .gap_3()
                            .items_center()
                            .child(icon("radio", 16., colors().muted))
                            .child(div().flex_1().child(Input::new(&self.whisper_names[&l.id])))
                            .child(key_chip)
                            .child(
                                Button::new(SharedString::from(format!("wrec-{i}")))
                                    .label(if recording { "Cancel" } else { "Record" })
                                    .small()
                                    .ghost()
                                    .on_click(cx.listener(move |t, _, window, cx| {
                                        let rec = Some(Rec::Whisper(rec_id.clone()));
                                        t.recording = if t.recording == rec { None } else { rec };
                                        window.focus(&t.focus, cx);
                                        cx.notify();
                                    })),
                            )
                            .when(!l.hotkey.is_empty(), |d| {
                                d.child(icon_btn(SharedString::from(format!("wclr-{i}")), "x", colors().muted, "Clear key").on_click(
                                    cx.listener(move |t, _, _, cx| t.set_hotkey(Rec::Whisper(clear_id.clone()), None, cx)),
                                ))
                            })
                            .child(danger_btn(SharedString::from(format!("wdel-{i}")), "Delete list").on_click(cx.listener(
                                move |t, _, _, cx| {
                                    let id = del_id.clone();
                                    t.whisper_names.remove(&id);
                                    t.model.update(cx, |m, cx| m.update_settings(cx, |s| s.whisper_lists.retain(|l| l.id != id)));
                                    t.apply_hotkeys(cx);
                                },
                            ))),
                    )
                    .when_some(conflict, |d, c| {
                        d.child(
                            h_flex()
                                .gap_1p5()
                                .items_center()
                                .child(icon("triangle-alert", 12., colors().amber))
                                .child(label(format!("Also bound in Hyprland: {c}")).text_color(rgb(colors().amber))),
                        )
                    })
                    .child(targets)
                    .child(extras),
            );
        }
        v_flex()
            .gap_4()
            .child(
                h_flex()
                    .gap_4()
                    .items_start()
                    .child(label(
                        "Hold a list's key to talk only to its targets instead of your channel. Add people and \
                         channels by right-clicking them in the main window. Groups and commanders are looked up \
                         when you press the key.",
                    ).flex_1().min_w_0())
                    .child(Button::new("new-whisper").label("New list").small().primary().on_click(cx.listener(
                        |t, _, _, cx| {
                            t.model.update(cx, |m, cx| {
                                m.update_settings(cx, |s| {
                                    let n = s.whisper_lists.len() + 1;
                                    s.whisper_lists.push(WhisperList {
                                        id: new_id(),
                                        name: format!("Whisper {n}"),
                                        ..Default::default()
                                    });
                                })
                            });
                        },
                    ))),
            )
            .when_some(self.hotkey_error.clone(), |d, e| d.child(notice("triangle-alert", colors().red, e)))
            .child(col)
            .into_any_element()
    }

    fn notifications(&self, cx: &mut Context<Self>) -> AnyElement {
        let s = self.model.read(cx).settings.clone();
        let mut rows = v_flex().gap_1();
        rows = rows.child(
            h_flex()
                .gap_4()
                .pb_1()
                .pt_1()
                .child(div().flex_1())
                .child(div().w(px(60.)).text_xs().text_color(rgb(colors().dim)).child("Sound"))
                .child(div().w(px(60.)).text_xs().text_color(rgb(colors().dim)).child("Popup"))
                .child(div().w(px(30.))),
        );
        for (i, kind) in EventKind::ALL.into_iter().enumerate() {
            let setting = s.notify_setting(kind);
            let toggle = move |sound: bool| {
                move |t: &mut Self, v: &bool, _: &mut Window, cx: &mut Context<Self>| {
                    let v = *v;
                    t.model.update(cx, |m, cx| {
                        m.update_settings(cx, |s| {
                            let mut e = s.notify_setting(kind);
                            if sound { e.sound = v } else { e.popup = v }
                            s.notify.insert(kind, e);
                        })
                    });
                }
            };
            rows = rows.child(
                h_flex()
                    .gap_4()
                    .items_center()
                    .min_h(px(34.))
                    .child(
                        h_flex()
                            .flex_1()
                            .gap_3()
                            .items_center()
                            .child(icon(kind.icon(), 15., colors().muted))
                            .child(div().text_sm().child(kind.title())),
                    )
                    .child(
                        div()
                            .w(px(60.))
                            .child(Switch::new(("snd", i)).checked(setting.sound).on_click(cx.listener(toggle(true)))),
                    )
                    .child(div().w(px(60.)).when(kind.can_popup(), |d| {
                        d.child(Switch::new(("pop", i)).checked(setting.popup).on_click(cx.listener(toggle(false))))
                    }))
                    .child(
                        div()
                            .id(("play", i))
                            .w(px(30.))
                            .h(px(26.))
                            .rounded(px(5.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .hover(|d| d.bg(hover_bg()))
                            .tooltip(|w, cx| Tooltip::new("Play").build(w, cx))
                            .child(icon("play", 13., colors().muted))
                            .on_click(cx.listener(move |t, _, _, cx| {
                                crate::notify::preview(kind, &t.model.read(cx).settings);
                            })),
                    ),
            );
        }
        v_flex()
            .gap_6()
            .child(
                Group::new("volume-2", "Sounds")
                    .child(form::slider("Volume", format!("{:.0}%", s.sound_volume * 100.), Slider::new(&self.sounds)))
                    .child(item(
                        "Read events aloud",
                        Some(if self.can_speak {
                            "Says \"Alex poked you\" instead of playing the sound (espeak-ng)".into()
                        } else {
                            "Needs espeak-ng".into()
                        }),
                        Switch::new("speak").checked(s.speak_events).disabled(!self.can_speak).on_click(cx.listener(
                            |t, v: &bool, _, cx| {
                                let v = *v;
                                t.model.update(cx, |m, cx| m.update_settings(cx, |s| s.speak_events = v));
                            },
                        )),
                    )),
            )
            .child(
                Group::new("bell-ring", "Events")
                    .child(block(rows))
                    .hint("Chat popups are skipped while the Wisp window has focus."),
            )
            .into_any_element()
    }

    fn general(&self, cx: &mut Context<Self>) -> AnyElement {
        let tray = self.model.read(cx).settings.close_to_tray;
        let theme_file = super::colors::path();
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

    fn tabs(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let c = colors();
        let mut col = v_flex()
            .w(px(210.))
            .flex_none()
            .h_full()
            .bg(rgb(c.sidebar))
            .border_r_1()
            .border_color(border())
            .child(header_row().child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child("Settings")))
            .child(div().h(px(8.)));
        for tab in [
            SettingsTab::Audio,
            SettingsTab::Servers,
            SettingsTab::Hotkeys,
            SettingsTab::Whisper,
            SettingsTab::Notifications,
            SettingsTab::Identities,
            SettingsTab::General,
        ] {
            col = col.child(
                form::nav_item(tab.title(), tab.icon(), tab.title(), self.tab == tab)
                    .on_click(cx.listener(move |t, _, window, cx| t.switch(tab, window, cx))),
            );
        }
        col.child(div().flex_1()).child(
            h_flex()
                .px_5()
                .py_4()
                .gap_2()
                .items_center()
                .child(mark(14.))
                .child(label(format!("Wisp {}", env!("CARGO_PKG_VERSION")))),
        )
    }
}

impl SettingsView {
    fn content(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match self.tab {
            SettingsTab::Audio => self.audio(cx),
            SettingsTab::Servers => self.servers(cx),
            SettingsTab::Hotkeys => self.hotkeys_tab(cx),
            SettingsTab::Notifications => self.notifications(cx),
            SettingsTab::Whisper => self.whisper_tab(window, cx),
            SettingsTab::Identities => self.identities_tab(window, cx),
            SettingsTab::General => self.general(cx),
        };
        h_flex()
            .size_full()
            .bg(rgb(colors().bg))
            .text_color(rgb(colors().text))
            .font_family(FONT)
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|t, ev: &KeyDownEvent, _, cx| t.on_key(ev, cx)))
            .child(self.tabs(cx))
            .child(
                v_flex()
                    .flex_1()
                    .h_full()
                    .min_w_0()
                    .child(
                        div().id("settings-body").flex_1().overflow_y_scroll().child(
                            v_flex()
                                .max_w(px(760.))
                                .px_8()
                                .pt_6()
                                .pb_10()
                                .gap_6()
                                .child(form::page_header(self.tab.icon(), self.tab.title(), Some(self.tab.about().into())))
                                .child(body),
                        ),
                    ),
            )
    }
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let title = "Settings";
        let content = self.content(window, cx).into_any_element();
        super::frame::framed(title, content, window, cx)
    }
}

/// The keys of a hotkey, or "Press a key..." while recording one.
fn key_chip(text: String, set: bool, recording: bool) -> gpui_kit::Div {
    div()
        .h(px(30.))
        .px_3()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(5.))
        .border_1()
        .border_color(if recording { alpha(ink(), 0x55) } else { border() })
        .bg(if recording { alpha(ink(), 0x14) } else { rgb(colors().raised) })
        .text_sm()
        .text_color(rgb(if set || recording { colors().text } else { colors().dim }))
        .child(if recording { "Press a key...".to_string() } else { text })
}
