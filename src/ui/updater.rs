//! The update check (on start, then hourly) and the sidebar card that
//! installs a new release and restarts into it.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{Sizable, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{AnyElement, App, Context, Entity, FontWeight, IntoElement, ParentElement, Styled, div, px, rgb};

use super::Model;
use super::main_view::MainView;
use super::widgets::*;
use crate::update::{self, Release};

const CHECK_INTERVAL: Duration = Duration::from_secs(60 * 60);
const PROGRESS_TICK: Duration = Duration::from_millis(150);

pub enum UpdateState {
    Available(Release),
    /// Download progress in percent.
    Installing(Release, Arc<AtomicU8>),
    /// Installed; the new version starts from this executable.
    Ready(Release, PathBuf),
    Failed(Release, String),
}

impl UpdateState {
    fn release(&self) -> &Release {
        match self {
            Self::Available(r) | Self::Installing(r, _) | Self::Ready(r, _) | Self::Failed(r, _) => r,
        }
    }
}

/// Run a future on a short-lived Tokio runtime (reqwest needs one; GPUI's
/// executor isn't).
fn on_tokio<T: Send + 'static>(f: impl Future<Output = T> + Send + 'static) -> futures::channel::oneshot::Receiver<T> {
    let (tx, rx) = futures::channel::oneshot::channel();
    std::thread::spawn(move || {
        if let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() {
            let _ = tx.send(rt.block_on(f));
        }
    });
    rx
}

pub fn start(model: &Entity<Model>, cx: &mut App) {
    let model = model.downgrade();
    cx.spawn(async move |cx| {
        loop {
            if let Ok(Ok(Some(release))) = on_tokio(update::check_for_update()).await {
                let _ = model.update(cx, |m, cx| {
                    m.update = Some(UpdateState::Available(release));
                    cx.notify();
                });
                return;
            }
            cx.background_executor().timer(CHECK_INTERVAL).await;
        }
    })
    .detach();
}

fn install(model: &Entity<Model>, cx: &mut App) {
    let release = match &model.read(cx).update {
        Some(UpdateState::Available(r) | UpdateState::Failed(r, _)) => r.clone(),
        _ => return,
    };
    let progress = Arc::new(AtomicU8::new(0));
    model.update(cx, |m, cx| {
        m.update = Some(UpdateState::Installing(release.clone(), progress.clone()));
        cx.notify();
    });
    let model = model.downgrade();
    cx.spawn(async move |cx| {
        let job = {
            let (release, progress) = (release.clone(), progress.clone());
            on_tokio(async move { update::install(&release, &progress).await })
        };
        let mut job = std::pin::pin!(job);
        let result = loop {
            let tick = std::pin::pin!(cx.background_executor().timer(PROGRESS_TICK));
            match futures::future::select(job.as_mut(), tick).await {
                futures::future::Either::Left((r, _)) => break r.unwrap_or_else(|_| Err("The update stopped".into())),
                futures::future::Either::Right(_) => {
                    let _ = model.update(cx, |_, cx| cx.notify());
                }
            }
        };
        let _ = model.update(cx, |m, cx| {
            m.update = Some(match result {
                Ok(exe) => UpdateState::Ready(release, exe),
                Err(e) => UpdateState::Failed(release, e),
            });
            cx.notify();
        });
    })
    .detach();
}

fn restart(model: &Entity<Model>, exe: PathBuf, cx: &mut App) {
    match update::restart(&exe) {
        Ok(()) => crate::ctl::ui(crate::ctl::UiRequest::Quit),
        Err(e) => model.update(cx, |m, cx| {
            if let Some(state) = m.update.take() {
                m.update = Some(UpdateState::Failed(state.release().clone(), e));
            }
            cx.notify();
        }),
    }
}

impl MainView {
    pub(super) fn update_card(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let state = self.model.read(cx).update.as_ref()?;
        let release = state.release().clone();
        let model = self.model.clone();
        let (text, error) = match state {
            UpdateState::Available(r) if r.installable() => ("Installs in the background, then restarts Wisp.".to_string(), false),
            UpdateState::Available(_) => ("Download it from the release page.".to_string(), false),
            UpdateState::Installing(_, p) => (format!("Downloading... {}%", p.load(Ordering::Relaxed)), false),
            UpdateState::Ready(..) => ("Installed. Restart to use it.".to_string(), false),
            UpdateState::Failed(_, e) => (format!("{}. Try the release page instead.", e.trim_end_matches('.')), true),
        };
        let page = {
            let url = release.url();
            Button::new("update-page").label("Release page").small().ghost().on_click(move |_, _, cx| cx.open_url(&url))
        };
        let later = {
            let model = model.clone();
            Button::new("update-later").label("Later").small().ghost().on_click(move |_, _, cx| {
                model.update(cx, |m, cx| {
                    m.update = None;
                    cx.notify();
                })
            })
        };
        let mut buttons = h_flex().gap_1().justify_end();
        buttons = match state {
            UpdateState::Available(r) if r.installable() => buttons.child(later).child({
                let model = model.clone();
                Button::new("update-install").label("Update").small().primary().on_click(move |_, _, cx| install(&model, cx))
            }),
            UpdateState::Ready(_, exe) => buttons.child(later).child({
                let (model, exe) = (model.clone(), exe.clone());
                Button::new("update-restart")
                    .label("Restart now")
                    .small()
                    .primary()
                    .on_click(move |_, _, cx| restart(&model, exe.clone(), cx))
            }),
            UpdateState::Installing(..) => buttons,
            UpdateState::Available(_) | UpdateState::Failed(..) => buttons.child(later).child(page),
        };
        Some(
            v_flex()
                .mx_2()
                .mb_2()
                .p_3()
                .gap_2()
                .rounded(px(6.))
                .bg(alpha(ACCENT, 0x14))
                .border_1()
                .border_color(alpha(ACCENT, 0x40))
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(mark(16.))
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(format!("Wisp {} is out", release.version)),
                        ),
                )
                .child(label(text).when(error, |d| d.text_color(rgb(RED))))
                .child(buttons)
                .into_any_element(),
        )
    }
}
