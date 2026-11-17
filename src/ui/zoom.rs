//! Zooming into a stream: the mouse wheel (or a pinch, or Ctrl+scroll)
//! zooms where the pointer is, dragging moves around, and a small map in
//! the corner shows the whole picture with the part in view marked. Click
//! or drag on the map to jump there. The map and the zoom buttons only show
//! while zooming or moving around, and while the pointer is near them.

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui_kit::component::h_flex;
use gpui_kit::{
    AnyElement, App, Bounds, Context, Task, CursorStyle, Entity, InteractiveElement, IntoElement, MouseButton, MouseDownEvent,
    MouseMoveEvent, ObjectFit, ParentElement, PinchEvent, Pixels, Point, ScrollDelta, ScrollWheelEvent,
    StatefulInteractiveElement, Styled, StyledImage, canvas, div, img, px, rgb, rgba,
};

use super::streams::Feed;
use super::widgets::*;

pub const MAX: f32 = 8.;
/// One step of the keys and buttons.
pub const STEP: f32 = 1.5;
/// The map's longer side, and its distance from the corner.
const MAP: f32 = 200.;
const MARGIN: f32 = 12.;
/// How long the map stays after the last zoom or move.
const SHOW_FOR: Duration = Duration::from_secs(2);
/// The zoom buttons' height and gap above the map.
const BAR: f32 = 38.;

#[derive(Default)]
pub struct Zoom {
    /// 1 shows the whole picture.
    level: f32,
    /// The point of the picture in the middle of the view (0..1 each way).
    center: (f32, f32),
    /// Where the view was last drawn, in window coordinates.
    bounds: Rc<Cell<Bounds<Pixels>>>,
    drag: Option<Drag>,
    /// The map shows until then.
    shown_until: Option<Instant>,
    /// The pointer is over the map's corner.
    near: bool,
    /// Redraws once the map should go.
    hide: Option<Task<()>>,
}

#[derive(Clone, Copy)]
enum Drag {
    /// Moving the picture: where the pointer and the center were.
    Pan(Point<Pixels>, (f32, f32)),
    /// Moving the marked part on the map.
    Map,
}

/// Where the picture goes inside the view: left, top, width, height.
#[derive(Clone, Copy)]
struct Place {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

impl Zoom {
    pub fn new() -> Self {
        Self { level: 1., center: (0.5, 0.5), ..Default::default() }
    }

    pub fn zoomed(&self) -> bool {
        self.level > 1.001
    }

    pub fn reset(&mut self) {
        self.level = 1.;
        self.center = (0.5, 0.5);
        self.drag = None;
        self.near = false;
    }

    /// Whether the map and the buttons show.
    fn visible(&self) -> bool {
        self.zoomed() && (self.drag.is_some() || self.near || self.shown_until.is_some_and(|t| Instant::now() < t))
    }

    /// Whether `at` (window coordinates) is on or near the map and the buttons.
    fn in_corner(&self, at: Point<Pixels>, picture: (f32, f32)) -> bool {
        let b = self.bounds.get();
        let (mw, mh) = Self::map_size(picture);
        let reach = 24.;
        let left = f32::from(b.origin.x + b.size.width) - MARGIN - mw - reach;
        let top = f32::from(b.origin.y + b.size.height) - MARGIN - mh - BAR - reach;
        f32::from(at.x) >= left && f32::from(at.y) >= top
    }

    fn view(&self) -> (f32, f32) {
        let b = self.bounds.get();
        (f32::from(b.size.width), f32::from(b.size.height))
    }

    fn place(&self, picture: (f32, f32)) -> Place {
        let (vw, vh) = self.view();
        let fit = (vw / picture.0).min(vh / picture.1);
        let (w, h) = (picture.0 * fit * self.level, picture.1 * fit * self.level);
        let cx = clamp_center(self.center.0, w, vw);
        let cy = clamp_center(self.center.1, h, vh);
        Place { x: vw / 2. - cx * w, y: vh / 2. - cy * h, w, h }
    }

    /// Zoom by `factor`, keeping the picture's point under `at` (in the view) where it is.
    fn zoom_at(&mut self, factor: f32, at: (f32, f32), picture: (f32, f32)) {
        let before = self.place(picture);
        let u = ((at.0 - before.x) / before.w, (at.1 - before.y) / before.h);
        self.level = (self.level * factor).clamp(1., MAX);
        if !self.zoomed() {
            self.reset();
            return;
        }
        let (vw, vh) = self.view();
        self.center = (0.5, 0.5);
        let p = self.place(picture);
        let (left, top) = (at.0 - u.0 * p.w, at.1 - u.1 * p.h);
        self.center = (
            clamp_center((vw / 2. - left) / p.w, p.w, vw),
            clamp_center((vh / 2. - top) / p.h, p.h, vh),
        );
    }

    /// Zoom by `factor` around the middle of the view.
    pub fn step(&mut self, factor: f32, picture: Option<(f32, f32)>) {
        let Some(picture) = picture else { return };
        let (vw, vh) = self.view();
        self.zoom_at(factor, (vw / 2., vh / 2.), picture);
    }

    fn pan(&mut self, by: (f32, f32), picture: (f32, f32)) {
        let p = self.place(picture);
        let (vw, vh) = self.view();
        self.center = (
            clamp_center(self.center.0 - by.0 / p.w, p.w, vw),
            clamp_center(self.center.1 - by.1 / p.h, p.h, vh),
        );
    }

    /// The map's size for a picture of this shape.
    fn map_size(picture: (f32, f32)) -> (f32, f32) {
        if picture.0 >= picture.1 {
            (MAP, MAP * picture.1 / picture.0)
        } else {
            (MAP * picture.0 / picture.1, MAP)
        }
    }

    /// Center on the picture's point under `at` (window coordinates) on the map.
    fn jump(&mut self, at: Point<Pixels>, picture: (f32, f32)) {
        let b = self.bounds.get();
        let (mw, mh) = Self::map_size(picture);
        let left = f32::from(b.origin.x + b.size.width) - MARGIN - mw;
        let top = f32::from(b.origin.y + b.size.height) - MARGIN - mh;
        let p = self.place(picture);
        let (vw, vh) = self.view();
        self.center = (
            clamp_center((f32::from(at.x) - left) / mw, p.w, vw),
            clamp_center((f32::from(at.y) - top) / mh, p.h, vh),
        );
    }

    fn in_view(&self, at: Point<Pixels>) -> (f32, f32) {
        let b = self.bounds.get();
        (f32::from(at.x - b.origin.x), f32::from(at.y - b.origin.y))
    }
}

/// Keep the view on the picture; a picture smaller than the view sits in the middle.
fn clamp_center(c: f32, size: f32, view: f32) -> f32 {
    if size <= view {
        0.5
    } else {
        let half = view / 2. / size;
        c.clamp(half, 1. - half)
    }
}

/// Wheel lines and trackpad pixels, as a zoom factor.
fn wheel_factor(delta: &ScrollDelta) -> f32 {
    let lines = match delta {
        ScrollDelta::Lines(p) => p.y,
        ScrollDelta::Pixels(p) => f32::from(p.y) / 50.,
    };
    1.2f32.powf(lines.clamp(-5., 5.))
}

/// Something moved: redraw, with the map up for a while.
pub fn changed(f: &mut Feed, cx: &mut Context<Feed>) {
    f.zoom.shown_until = Some(Instant::now() + SHOW_FOR);
    f.zoom.hide = Some(cx.spawn(async move |this, cx| {
        cx.background_executor().timer(SHOW_FOR + Duration::from_millis(50)).await;
        let _ = this.update(cx, |_, cx| cx.notify());
    }));
    cx.notify();
}

/// The stream's picture, zoomable. Shows the plain picture (or why there's
/// none) when there's nothing to zoom.
pub fn view(feed: &Entity<Feed>, cx: &App) -> AnyElement {
    let f = feed.read(cx);
    let (Some(image), Some(picture)) = (f.image(), f.picture_size()) else {
        return f.picture();
    };
    let z = &f.zoom;
    let bounds = z.bounds.clone();
    let known = bounds.get().size.width > px(0.);
    let zoomed = z.zoomed() && known;
    let place = z.place(picture);
    let dragging = z.drag.is_some();

    let track = canvas(
        move |b, window, _| {
            if bounds.get() != b {
                bounds.set(b);
                window.refresh();
            }
        },
        |_, _, _, _| {},
    )
    .absolute()
    .size_full();

    let picture_el = if zoomed {
        img(image.clone())
            .absolute()
            .left(px(place.x))
            .top(px(place.y))
            .w(px(place.w))
            .h(px(place.h))
            .object_fit(ObjectFit::Fill)
            .into_any_element()
    } else {
        img(image.clone()).size_full().object_fit(ObjectFit::Contain).into_any_element()
    };

    let (f1, f2, f3, f4, f5, f6) = (feed.clone(), feed.clone(), feed.clone(), feed.clone(), feed.clone(), feed.clone());
    let mut el = div()
        .id("stream-zoom")
        .relative()
        .size_full()
        .overflow_hidden()
        .child(track)
        .child(picture_el)
        .on_scroll_wheel(move |ev: &ScrollWheelEvent, _, cx| {
            f1.update(cx, |f, cx| {
                let Some(picture) = f.picture_size() else { return };
                let at = f.zoom.in_view(ev.position);
                let wheel = matches!(ev.delta, ScrollDelta::Lines(_));
                if wheel || ev.modifiers.control || ev.modifiers.platform {
                    f.zoom.zoom_at(wheel_factor(&ev.delta), at, picture);
                } else if f.zoom.zoomed()
                    && let ScrollDelta::Pixels(d) = ev.delta
                {
                    // a trackpad's two-finger scroll moves around
                    f.zoom.pan((f32::from(d.x), f32::from(d.y)), picture);
                } else {
                    return;
                }
                changed(f, cx);
            });
            cx.stop_propagation();
        })
        .on_pinch(move |ev: &PinchEvent, _, cx| {
            f2.update(cx, |f, cx| {
                let Some(picture) = f.picture_size() else { return };
                let at = f.zoom.in_view(ev.position);
                f.zoom.zoom_at(1. + ev.delta, at, picture);
                changed(f, cx);
            });
        })
        .on_mouse_down(MouseButton::Left, move |ev: &MouseDownEvent, _, cx| {
            f3.update(cx, |f, _| {
                if f.zoom.zoomed() {
                    f.zoom.drag = Some(Drag::Pan(ev.position, f.zoom.center));
                }
            });
        })
        .on_mouse_move(move |ev: &MouseMoveEvent, _, cx| {
            f4.update(cx, |f, cx| {
                let near = f.zoom.zoomed() && f.picture_size().is_some_and(|p| f.zoom.in_corner(ev.position, p));
                if near != f.zoom.near {
                    f.zoom.near = near;
                    // leaving the corner starts the countdown
                    if near { cx.notify() } else { changed(f, cx) }
                }
                let Some(drag) = f.zoom.drag else { return };
                if ev.pressed_button != Some(MouseButton::Left) {
                    f.zoom.drag = None;
                    cx.notify();
                    return;
                }
                let Some(picture) = f.picture_size() else { return };
                match drag {
                    Drag::Pan(from, center) => {
                        f.zoom.center = center;
                        let by = ev.position - from;
                        f.zoom.pan((f32::from(by.x), f32::from(by.y)), picture);
                    }
                    Drag::Map => f.zoom.jump(ev.position, picture),
                }
                changed(f, cx);
            });
        })
        .on_mouse_up(MouseButton::Left, move |_, _, cx| {
            f5.update(cx, |f, cx| {
                if f.zoom.drag.take().is_some() {
                    changed(f, cx);
                }
            });
        })
        .on_hover(move |hovered: &bool, _, cx| {
            if !hovered {
                f6.update(cx, |f, cx| {
                    if std::mem::take(&mut f.zoom.near) {
                        changed(f, cx);
                    }
                });
            }
        });
    if zoomed {
        el = el.cursor(if dragging { CursorStyle::ClosedHand } else { CursorStyle::OpenHand });
        if z.visible() {
            el = el.child(map(feed, image, picture, place, z)).child(controls(feed, z.level, picture));
        }
    }
    el.into_any_element()
}

/// The whole picture, small, with the part in view outlined.
fn map(
    feed: &Entity<Feed>,
    image: std::sync::Arc<gpui_kit::RenderImage>,
    picture: (f32, f32),
    place: Place,
    z: &Zoom,
) -> impl IntoElement {
    let (mw, mh) = Zoom::map_size(picture);
    let (vw, vh) = z.view();
    // the view, in the picture's 0..1 coordinates
    let x0 = (-place.x / place.w).clamp(0., 1.);
    let x1 = ((vw - place.x) / place.w).clamp(0., 1.);
    let y0 = (-place.y / place.h).clamp(0., 1.);
    let y1 = ((vh - place.y) / place.h).clamp(0., 1.);
    let feed = feed.clone();
    div()
        .id("zoom-map")
        .absolute()
        .right(px(MARGIN))
        .bottom(px(MARGIN))
        .w(px(mw))
        .h(px(mh))
        .rounded(px(6.))
        .overflow_hidden()
        .bg(rgb(0x000000))
        .shadow_lg()
        .cursor(CursorStyle::PointingHand)
        .child(img(image).size_full().object_fit(ObjectFit::Fill).opacity(0.75))
        .child(
            div()
                .absolute()
                .left(px(x0 * mw))
                .top(px(y0 * mh))
                .w(px((x1 - x0) * mw))
                .h(px((y1 - y0) * mh))
                .border_2()
                .border_color(rgb(colors().accent))
                .bg(alpha(colors().accent, 0x22))
                .rounded(px(2.)),
        )
        .child(div().absolute().inset_0().rounded(px(6.)).border_1().border_color(rgba(0xffffff33)))
        .on_mouse_down(MouseButton::Left, move |ev: &MouseDownEvent, _, cx| {
            feed.update(cx, |f, cx| {
                f.zoom.drag = Some(Drag::Map);
                f.zoom.jump(ev.position, picture);
                changed(f, cx);
            });
            cx.stop_propagation();
        })
}

/// "− 250% + ⟲" above the map.
fn controls(feed: &Entity<Feed>, level: f32, picture: (f32, f32)) -> impl IntoElement {
    let (_, mh) = Zoom::map_size(picture);
    let button = |id: &'static str, name: &'static str| {
        div()
            .id(id)
            .size(px(24.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(4.))
            .cursor_pointer()
            .hover(|d| d.bg(rgba(0xffffff22)))
            .child(icon(name, 14., 0xffffff))
    };
    let (f1, f2, f3) = (feed.clone(), feed.clone(), feed.clone());
    h_flex()
        .absolute()
        .right(px(MARGIN))
        .bottom(px(MARGIN + mh + 8.))
        .h(px(BAR - 8.))
        .gap_1()
        .p(px(3.))
        .items_center()
        .rounded(px(6.))
        .bg(rgba(0x000000b8))
        .text_xs()
        .text_color(rgb(0xffffff))
        .child(button("zoom-out", "minus").on_click(move |_, _, cx| {
            f1.update(cx, |f, cx| {
                f.zoom.step(1. / STEP, f.picture_size());
                changed(f, cx);
            })
        }))
        .child(div().w(px(42.)).text_center().child(format!("{:.0}%", level * 100.)))
        .child(button("zoom-in", "plus").on_click(move |_, _, cx| {
            f2.update(cx, |f, cx| {
                f.zoom.step(STEP, f.picture_size());
                changed(f, cx);
            })
        }))
        .child(div().w(px(1.)).h(px(14.)).mx_1().bg(rgba(0xffffff33)))
        .child(button("zoom-fit", "scan").on_click(move |_, _, cx| {
            f3.update(cx, |f, cx| {
                f.zoom.reset();
                cx.notify();
            })
        }))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
}

/// For the toolbars: zoom in, or back to the whole picture when zoomed.
pub fn toggle(feed: &Entity<Feed>, cx: &mut App) {
    feed.update(cx, |f, cx| {
        if f.zoom.zoomed() {
            f.zoom.reset();
        } else {
            f.zoom.step(2., f.picture_size());
        }
        changed(f, cx);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::{point, size};

    fn zoom(w: f32, h: f32) -> Zoom {
        let z = Zoom::new();
        z.bounds.set(Bounds { origin: point(px(0.), px(0.)), size: size(px(w), px(h)) });
        z
    }

    #[test]
    fn fits_when_not_zoomed() {
        let z = zoom(1000., 1000.);
        let p = z.place((1920., 1080.));
        assert!((p.w - 1000.).abs() < 0.01 && (p.h - 562.5).abs() < 0.01);
        assert!((p.y - 218.75).abs() < 0.01 && p.x.abs() < 0.01);
    }

    #[test]
    fn keeps_the_point_under_the_pointer() {
        let mut z = zoom(1600., 900.);
        let picture = (3840., 2160.);
        let at = (400., 300.);
        let before = z.place(picture);
        let u = ((at.0 - before.x) / before.w, (at.1 - before.y) / before.h);
        z.zoom_at(2., at, picture);
        let after = z.place(picture);
        assert!((after.x + u.0 * after.w - at.0).abs() < 0.5);
        assert!((after.y + u.1 * after.h - at.1).abs() < 0.5);
    }

    #[test]
    fn never_leaves_the_picture() {
        let mut z = zoom(1600., 900.);
        let picture = (1920., 1080.);
        z.step(4., Some(picture));
        z.pan((100000., 100000.), picture);
        let p = z.place(picture);
        assert!(p.x <= 0.01 && p.y <= 0.01);
        z.pan((-100000., -100000.), picture);
        let p = z.place(picture);
        assert!(p.x + p.w >= 1599.99 && p.y + p.h >= 899.99);
    }

    #[test]
    fn zooming_out_all_the_way_resets() {
        let mut z = zoom(1600., 900.);
        z.step(3., Some((1920., 1080.)));
        z.zoom_at(0.01, (10., 10.), (1920., 1080.));
        assert!(!z.zoomed() && z.center == (0.5, 0.5));
    }
}
