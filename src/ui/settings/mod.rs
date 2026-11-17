//! Settings window: audio, servers, hotkeys and general options. Changes apply
//! right away (except the server form, which has Save).

mod servers;
mod audio;
mod keys;
mod identities;
mod whisper;
mod notifications;
mod general;

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

