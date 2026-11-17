//! Shared screens in the main window: a bar of live thumbnails under the
//! channel tree, the stage (one stream large), and the controls shared with
//! the pop-out window.
//!
//! Each stream we show is one `Feed`, however many places show it: the
//! thumbnail, the stage and a pop-out window all draw the same pictures.
//! Thumbnails only need the smallest layer; once a stream is on the stage or
//! in its own window, the feed asks for the quality picked there.

use std::collections::HashMap;
use std::sync::Arc;

use futures::StreamExt;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{
    AnyElement, App, AppContext, Context, Entity, FontWeight, InteractiveElement, IntoElement, ObjectFit,
    ParentElement, RenderImage, SharedString, StatefulInteractiveElement, Styled, StyledImage, div, img, px, rgb,
};

use gpui_kit::component::slider::{Slider, SliderEvent, SliderState};

use super::Model;
use super::colors::colors;
use super::widgets::*;
use crate::net::{ClientRow, Cmd, ConId, Handle, ServerSnap};
use crate::video::{Quality, Screen, StreamInfo};

pub type FeedKey = (ConId, u16);

/// Thumbnails ask for the smallest layer (the server takes the nearest).
const SMALLEST: Quality = Quality::Layer(u8::MAX);

pub struct Feed {
    net: Handle,
    pub con: ConId,
    pub client: u16,
    pub name: String,
    pub info: Option<StreamInfo>,
    screen: Screen,
    image: Option<Arc<RenderImage>>,
    ended: bool,
    /// On the stage or in a window (so the picked quality counts).
    big: bool,
    /// Quality picked for the big view.
    pub choice: Quality,
    sent: Quality,
    /// We could watch it last time we looked (sharing, same channel).
    reachable: bool,
    /// Its sound: volume (1.0 = as sent) and muted.
    volume: f32,
    pub muted: bool,
    pub slider: Entity<SliderState>,
    /// Our own share, previewed from the encoder instead of the server.
    pub own: bool,
}

impl Feed {
    fn new(net: Handle, con: ConId, client: u16, name: String, info: StreamInfo, own: bool, cx: &mut Context<Self>) -> Self {
        cx.on_release(|f: &mut Self, _| {
            if f.own {
                f.screen.end();
            } else {
                f.net.send(Cmd::Watch { con: f.con, client: f.client, screen: None, quality: Quality::Auto });
            }
        })
        .detach();
        let (screen, _) = Screen::new();
        let slider = cx.new(|_| SliderState::new().min(0.).max(150.).step(5.).default_value(100.));
        cx.subscribe(&slider, |f: &mut Self, _, ev: &SliderEvent, cx| {
            let (SliderEvent::Change(v) | SliderEvent::Release(v)) = ev;
            f.volume = v.start() / 100.;
            f.muted = false;
            f.send_volume(cx);
        })
        .detach();
        let mut f = Self {
            net,
            con,
            client,
            name,
            info: Some(info),
            screen,
            image: None,
            ended: true,
            big: false,
            choice: Quality::Auto,
            sent: SMALLEST,
            reachable: true,
            volume: 1.0,
            muted: false,
            slider,
            own,
        };
        f.watch(cx);
        f
    }

    fn quality(&self) -> Quality {
        if self.big { self.choice } else { SMALLEST }
    }

    fn watch(&mut self, cx: &mut Context<Self>) {
        let (screen, mut changed) = if self.own { crate::video::preview() } else { Screen::new() };
        self.screen = screen.clone();
        self.ended = false;
        self.sent = self.quality();
        if !self.own {
            self.net.send(Cmd::Watch { con: self.con, client: self.client, screen: Some(screen), quality: self.sent });
        }
        cx.spawn(async move |this, cx| {
            while changed.next().await.is_some() {
                if this.update(cx, |f, cx| f.refresh(cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
        cx.notify();
    }

    /// The decoder has a new picture (or the stream ended).
    fn refresh(&mut self, cx: &mut Context<Self>) {
        if let Some(p) = self.screen.take()
            && let Some(buf) = image::RgbaImage::from_raw(p.width, p.height, p.bgra)
        {
            let picture = Arc::new(RenderImage::new(vec![image::Frame::new(buf)]));
            // the old picture's texture would stay in the atlas otherwise
            if let Some(old) = self.image.replace(picture) {
                cx.drop_image(old, None);
            }
        }
        self.ended = self.screen.ended();
        if self.ended
            && let Some(old) = self.image.take()
        {
            cx.drop_image(old, None);
        }
        cx.notify();
    }

    /// What the server says about the stream now; picks it up again when
    /// it comes back (they share again, or we're back in their channel).
    fn update(&mut self, info: Option<StreamInfo>, reachable: bool, name: &str, cx: &mut Context<Self>) {
        if reachable && !self.reachable && self.ended {
            self.info = info.clone();
            self.watch(cx);
        }
        self.reachable = reachable;
        if info.is_some() {
            self.info = info;
        }
        self.name = name.to_string();
        cx.notify();
    }

    pub fn toggle_mute(&mut self, cx: &mut Context<Self>) {
        self.muted = !self.muted;
        self.send_volume(cx);
    }

    fn send_volume(&mut self, cx: &mut Context<Self>) {
        let volume = if self.muted { 0.0 } else { self.volume };
        self.net.send(Cmd::StreamVolume { con: self.con, client: self.client, volume });
        cx.notify();
    }

    fn set_big(&mut self, big: bool, cx: &mut Context<Self>) {
        self.big = big;
        self.apply(cx);
    }

    pub fn set_choice(&mut self, choice: Quality, cx: &mut Context<Self>) {
        self.choice = choice;
        self.apply(cx);
    }

    fn apply(&mut self, cx: &mut Context<Self>) {
        let q = self.quality();
        if q != self.sent && !self.ended && !self.own {
            self.sent = q;
            self.net.send(Cmd::WatchQuality { con: self.con, client: self.client, quality: q });
        }
        cx.notify();
    }

    /// "1080p" for the layer we get.
    pub fn showing(&self) -> Option<String> {
        if self.own {
            let top = self.info.as_ref()?.layers.first()?;
            return Some(format!("Sending {}p{}", top.height, top.fps));
        }
        let layer = self.info.as_ref()?.layers.get(self.screen.layer()? as usize)?;
        Some(format!("{}p", layer.height))
    }

    pub fn viewers(&self) -> u32 {
        self.info.as_ref().map_or(0, |i| i.viewers.len() as u32)
    }

    /// The picture, or a line saying why there's none, filling its box.
    pub fn picture(&self) -> AnyElement {
        match (&self.image, self.ended) {
            (Some(image), false) => img(image.clone()).size_full().object_fit(ObjectFit::Contain).into_any_element(),
            (_, ended) => {
                let text = match ended {
                    true if self.own => "You stopped sharing".to_string(),
                    false if self.own => "Starting the preview...".to_string(),
                    true if self.info.is_some() && self.reachable => "The stream ended".to_string(),
                    true => format!("{} stopped sharing", self.name),
                    false => format!("Waiting for {}'s screen...", self.name),
                };
                div()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_sm()
                    .text_color(rgb(colors().muted))
                    .child(text)
                    .into_any_element()
            }
        }
    }
}

/// Streams in our channel on a server: (client id, name, info, is us).
pub fn streams_here(s: &ServerSnap) -> Vec<(u16, String, StreamInfo, bool)> {
    let Some(own) = s.own_channel else { return Vec::new() };
    s.channels
        .iter()
        .filter(|c| c.id == own)
        .flat_map(|c| &c.clients)
        .filter_map(|c| Some((c.id, c.name.clone(), c.stream.clone()?, c.me)))
        .collect()
}

fn find(s: &ServerSnap, client: u16) -> Option<(&ClientRow, u64)> {
    s.channels.iter().find_map(|ch| ch.clients.iter().find(|c| c.id == client).map(|c| (c, ch.id)))
}

impl Model {
    /// Start and stop feeds to match what's shown: thumbnails of the
    /// streams in our channel while the main window is open, plus whatever
    /// is on the stage or in its own window.
    pub fn sync_feeds(&mut self, cx: &mut Context<Self>) {
        let mut wanted: HashMap<FeedKey, (String, StreamInfo, bool)> = HashMap::new();
        if self.main_window.is_some()
            && let Some(s) = self.snap.active_server()
        {
            for (id, name, info, me) in streams_here(s) {
                wanted.insert((s.con, id), (name, info, me));
            }
        }
        let big: Vec<FeedKey> = self.screen_windows.keys().copied().chain(self.staged).collect();
        // feeds for big views stay (showing "stopped sharing") until closed
        self.feeds.retain(|k, _| wanted.contains_key(k) || big.contains(k));
        for (key, (name, info, own)) in &wanted {
            if !self.feeds.contains_key(key) {
                let net = self.net.clone();
                let (name, info, own) = (name.clone(), info.clone(), *own);
                let feed = cx.new(|cx| Feed::new(net, key.0, key.1, name, info, own, cx));
                self.feeds.insert(*key, feed);
            }
        }
        let snap = &self.snap;
        for (key, feed) in &self.feeds {
            let server = snap.server(key.0);
            let row = server.and_then(|s| find(s, key.1));
            let info = row.and_then(|(c, _)| c.stream.clone());
            let same_channel = row.is_some_and(|(_, ch)| server.and_then(|s| s.own_channel) == Some(ch));
            let reachable = info.is_some() && same_channel;
            let name = row.map(|(c, _)| c.name.clone());
            let is_big = big.contains(key);
            feed.update(cx, |f, cx| {
                let name = name.unwrap_or_else(|| f.name.clone());
                f.update(info, reachable, &name, cx);
                if f.big != is_big {
                    f.set_big(is_big, cx);
                }
            });
        }
        if self.staged.is_some_and(|k| !self.feeds.contains_key(&k)) {
            self.staged = None;
        }
    }

    /// The feed for a stream we're about to show big, made if needed.
    pub fn open_feed(&mut self, key: FeedKey, name: &str, cx: &mut Context<Self>) -> Option<Entity<Feed>> {
        if let Some(f) = self.feeds.get(&key) {
            return Some(f.clone());
        }
        let row = self.snap.server(key.0).and_then(|s| find(s, key.1)).map(|(c, _)| c.clone())?;
        let (info, own) = (row.stream.clone()?, row.me);
        let net = self.net.clone();
        let name = name.to_string();
        let feed = cx.new(|cx| Feed::new(net, key.0, key.1, name, info, own, cx));
        self.feeds.insert(key, feed.clone());
        Some(feed)
    }

    /// Mute every stream we watch, or unmute them all if they're all muted.
    pub fn toggle_stream_sound(&mut self, cx: &mut Context<Self>) {
        let feeds: Vec<_> = self.feeds.values().filter(|f| !f.read(cx).own).cloned().collect();
        let mute = feeds.iter().any(|f| !f.read(cx).muted);
        for f in feeds {
            f.update(cx, |f, cx| {
                if f.muted != mute {
                    f.toggle_mute(cx);
                }
            });
        }
    }

    /// Show this stream on the stage (None: back to the channel tree).
    pub fn stage(&mut self, key: Option<FeedKey>, cx: &mut Context<Self>) {
        if let Some(k) = key {
            let name = self.feeds.get(&k).map(|f| f.read(cx).name.clone()).unwrap_or_default();
            self.open_feed(k, &name, cx);
        }
        self.staged = key;
        self.sync_feeds(cx);
        cx.notify();
    }
}

/// Quality picks for a stream: Auto, then each layer.
pub fn quality_picker(feed: &Entity<Feed>, cx: &App) -> Option<impl IntoElement> {
    let f = feed.read(cx);
    if f.own {
        return None;
    }
    let mut choices = vec![(Quality::Auto, "Auto".to_string())];
    if let Some(info) = &f.info {
        for (i, l) in info.layers.iter().enumerate() {
            choices.push((Quality::Layer(i as u8), format!("{}p", l.height)));
        }
    }
    let current = f.choice;
    Some(h_flex().gap_1().children(choices.into_iter().enumerate().map(|(i, (q, text))| {
        let feed = feed.clone();
        chip(SharedString::from(format!("q-{i}")), text, q == current)
            .on_click(move |_, _, cx| feed.update(cx, |f, cx| f.set_choice(q, cx)))
    })))
}

/// Mute button and volume slider, for streams with sound.
pub fn sound_controls(feed: &Entity<Feed>, cx: &App) -> Option<impl IntoElement> {
    let f = feed.read(cx);
    // our own sound isn't played back to us
    if f.own || !f.info.as_ref().is_some_and(|i| i.audio) {
        return None;
    }
    let muted = f.muted;
    let feed2 = feed.clone();
    Some(
        h_flex()
            .gap_2()
            .items_center()
            .child(
                tool_button("mute", if muted { "volume-x" } else { "volume-2" }, if muted { "Unmute" } else { "Mute" })
                    .on_click(move |_, _, cx| feed2.update(cx, |f, cx| f.toggle_mute(cx))),
            )
            .child(div().w(px(110.)).child(Slider::new(&f.slider))),
    )
}

/// Who watches a stream, by name.
pub fn watcher_names(model: &Entity<Model>, f: &Feed, cx: &App) -> String {
    let Some(info) = &f.info else { return String::new() };
    let snap = &model.read(cx).snap;
    let Some(s) = snap.server(f.con) else { return String::new() };
    let names: Vec<String> = info
        .viewers
        .iter()
        .filter_map(|id| u16::try_from(*id).ok())
        .map(|id| find(s, id).map_or_else(|| "someone who left".to_string(), |(c, _)| c.name.clone()))
        .collect();
    names.join(", ")
}

/// "1080p · 3 watching", with who watches when you hover it.
pub fn status(model: &Entity<Model>, feed: &Entity<Feed>, cx: &App) -> impl IntoElement {
    let f = feed.read(cx);
    let names = watcher_names(model, f, cx);
    div()
        .id("stream-status")
        .text_xs()
        .text_color(rgb(colors().muted))
        .when(!names.is_empty(), |d| d.tooltip(move |w, cx| Tooltip::new(names.clone()).build(w, cx)))
        .child(status_line(f))
}

/// "1080p · 3 watching"
pub fn status_line(f: &Feed) -> String {
    let viewers = match f.viewers() {
        1 => "1 watching".to_string(),
        n => format!("{n} watching"),
    };
    match f.showing() {
        Some(q) => format!("{q} · {viewers}"),
        None => viewers,
    }
}

/// The stage: one stream large, with its controls.
pub fn stage(model: &Entity<Model>, feed: &Entity<Feed>, cx: &App) -> impl IntoElement {
    let f = feed.read(cx);
    let key = (f.con, f.client);
    let (m1, m2) = (model.clone(), model.clone());
    v_flex()
        .flex_1()
        .min_h_0()
        .p_3()
        .gap_2()
        .child(
            h_flex()
                .gap_3()
                .items_center()
                .child(icon("monitor-up", 15., colors().accent))
                .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child(f.name.clone()))
                .child(status(model, feed, cx))
                .child(div().flex_1())
                .children(sound_controls(feed, cx))
                .children(quality_picker(feed, cx))
                .child(
                    tool_button("pop-out", "external-link", "Open in its own window").on_click(move |_, _, cx| {
                        let (con, client, name) = key_name(&m1, key, cx);
                        m1.update(cx, |m, cx| m.stage(None, cx));
                        super::open_screen(&m1, con, client, name, cx);
                    }),
                )
                .child(
                    tool_button("unstage", "x", "Back to the channel").on_click(move |_, _, cx| {
                        m2.update(cx, |m, cx| m.stage(None, cx));
                    }),
                ),
        )
        .child(div().flex_1().min_h_0().rounded(px(6.)).overflow_hidden().bg(rgb(0x000000)).child(f.picture()))
}

fn key_name(model: &Entity<Model>, key: FeedKey, cx: &App) -> (ConId, u16, String) {
    let name = model.read(cx).feeds.get(&key).map(|f| f.read(cx).name.clone()).unwrap_or_default();
    (key.0, key.1, name)
}

pub fn tool_button(id: &'static str, name: &'static str, tip: &'static str) -> gpui_kit::Stateful<gpui_kit::Div> {
    div()
        .id(id)
        .size(px(28.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(5.))
        .cursor_pointer()
        .hover(|d| d.bg(hover_bg()))
        .tooltip(move |w, cx| Tooltip::new(tip).build(w, cx))
        .child(icon(name, 15., colors().muted))
}

const TILE_W: f32 = 176.;
const TILE_H: f32 = 99.;

/// The bar under the tree: a live thumbnail for each stream in our channel
/// (and, with `everyone`, the others in it too). Clicking one puts it on
/// the stage.
pub fn bar(model: &Entity<Model>, s: &ServerSnap, everyone: bool, on_toggle: impl Fn(&mut App) + 'static, cx: &App) -> Option<AnyElement> {
    let m = model.read(cx);
    let streams = streams_here(s);
    if streams.is_empty() {
        return None;
    }
    let mut tiles: Vec<AnyElement> = Vec::new();
    for (id, name, info, me) in &streams {
        let key = (s.con, *id);
        let staged = m.staged == Some(key);
        let model = model.clone();
        let body = match m.feeds.get(&key) {
            Some(feed) => feed.read(cx).picture(),
            _ => div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_xs()
                .text_color(rgb(colors().muted))
                .child(if *me { "You're sharing" } else { "" })
                .into_any_element(),
        };
        let viewers = info.viewers.len();
        let names: Vec<String> = info
            .viewers
            .iter()
            .filter_map(|id| u16::try_from(*id).ok())
            .filter_map(|id| find(s, id).map(|(c, _)| c.name.clone()))
            .collect();
        let names = names.join(", ");
        tiles.push(
            v_flex()
                .id(SharedString::from(format!("tile-{}-{id}", s.con)))
                .w(px(TILE_W))
                .flex_none()
                .gap_1()
                .cursor_pointer()
                .on_click(move |_, _, cx| {
                    model.update(cx, |m, cx| m.stage(if staged { None } else { Some(key) }, cx));
                })
                .child(
                    div()
                        .relative()
                        .w(px(TILE_W))
                        .h(px(TILE_H))
                        .rounded(px(6.))
                        .overflow_hidden()
                        .bg(rgb(0x000000))
                        .border_2()
                        .border_color(if staged { gpui_kit::Rgba::from(rgb(colors().accent)) } else { alpha(0, 0) })
                        .child(body)
                        .child(
                            div()
                                .absolute()
                                .top(px(5.))
                                .left(px(5.))
                                .px_1()
                                .rounded(px(3.))
                                .bg(rgb(colors().red))
                                .text_color(rgb(0xffffff))
                                .text_xs()
                                .font_weight(FontWeight::BOLD)
                                .child("LIVE"),
                        )
                        .when(viewers > 0, |d| {
                            d.child(
                                h_flex()
                                    .id(SharedString::from(format!("viewers-{}-{id}", s.con)))
                                    .tooltip(move |w, cx| Tooltip::new(names.clone()).build(w, cx))
                                    .absolute()
                                    .top(px(5.))
                                    .right(px(5.))
                                    .px_1()
                                    .gap_1()
                                    .items_center()
                                    .rounded(px(3.))
                                    .bg(gpui_kit::rgba(0x000000aa))
                                    .text_color(rgb(0xffffff))
                                    .text_xs()
                                    .child(icon("eye", 11., 0xffffff))
                                    .child(viewers.to_string()),
                            )
                        }),
                )
                .child(div().text_xs().truncate().child(if *me { format!("{name} (you)") } else { name.clone() }))
                .into_any_element(),
        );
    }
    if everyone && let Some(own) = s.own_channel {
        let others = s.channels.iter().filter(|c| c.id == own).flat_map(|c| &c.clients).filter(|c| c.stream.is_none());
        for c in others {
            tiles.push(
                v_flex()
                    .w(px(TILE_W * 0.6))
                    .flex_none()
                    .gap_1()
                    .child(
                        div()
                            .w(px(TILE_W * 0.6))
                            .h(px(TILE_H))
                            .rounded(px(6.))
                            .bg(rgb(colors().raised))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(avatar(&c.name, c.avatar.as_deref(), 40., c.talking)),
                    )
                    .child(div().text_xs().truncate().child(c.name.clone()))
                    .into_any_element(),
            );
        }
    }
    Some(
        v_flex()
            .flex_none()
            .border_t_1()
            .border_color(border())
            .px_3()
            .py_2()
            .gap_2()
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .child(section_title("Streams"))
                    .child(div().flex_1())
                    .child(
                        chip("bar-everyone", "Everyone", everyone).on_click(move |_, _, cx| on_toggle(cx)),
                    ),
            )
            .child(
                h_flex()
                    .id("stream-tiles")
                    .gap_3()
                    .overflow_x_scroll()
                    .children(tiles),
            )
            .into_any_element(),
    )
}
