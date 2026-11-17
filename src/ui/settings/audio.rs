//! Settings → Audio: devices, volumes, voice processing and how you talk.

use super::*;

impl SettingsView {
    pub(super) fn device_list(
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

    pub(super) fn audio(&self, cx: &mut Context<Self>) -> AnyElement {
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
}
