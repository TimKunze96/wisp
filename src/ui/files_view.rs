//! A channel's files: browse folders, download, upload (picker or drop from
//! the file manager), new folder, rename, delete, and the transfer list.

use std::time::Duration;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Sizable, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{
    AnyElement, AppContext, Context, Entity, ExternalPaths, InteractiveElement, IntoElement,
    ParentElement, PathPromptOptions, Render, SharedString, StatefulInteractiveElement, Styled, Window, div, px, rgb,
    rgba,
};
use tokio::sync::oneshot;

use super::Model;
use super::widgets::*;
use crate::net::admin::{Answer, Query};
use crate::net::files::{FileCmd, FileEntry, TransferState, download_path};
use crate::net::{Cmd, ConId};

pub struct FilesView {
    model: Entity<Model>,
    con: ConId,
    channel: u64,
    /// Current folder, "/" for the top.
    path: String,
    entries: Option<Result<Vec<FileEntry>, String>>,
    /// The entry being renamed, or a new folder ("" name).
    naming: Option<String>,
    name_input: Entity<InputState>,
    /// Delete asks once: the name that was clicked first.
    confirm_delete: Option<String>,
}

fn size_text(n: u64) -> String {
    match n {
        n if n >= 1 << 30 => format!("{:.1} GB", n as f64 / (1u64 << 30) as f64),
        n if n >= 1 << 20 => format!("{:.1} MB", n as f64 / (1u64 << 20) as f64),
        n if n >= 1 << 10 => format!("{:.0} KB", n as f64 / 1024.0),
        n => format!("{n} B"),
    }
}

fn join(dir: &str, name: &str) -> String {
    format!("{}/{name}", dir.trim_end_matches('/'))
}

impl FilesView {
    pub fn new(model: Entity<Model>, con: ConId, channel: u64, window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.observe(&model, |_, _, cx| cx.notify()).detach();
        let name_input = cx.new(|cx| InputState::new(window, cx).placeholder("Name"));
        cx.subscribe_in(&name_input, window, |this: &mut Self, _, ev: &InputEvent, _, cx| match ev {
            InputEvent::PressEnter { .. } => this.finish_naming(cx),
            InputEvent::Blur => {
                this.naming = None;
                cx.notify();
            }
            _ => {}
        })
        .detach();
        let mut this =
            Self { model, con, channel, path: "/".into(), entries: None, naming: None, name_input, confirm_delete: None };
        this.load(cx);
        this
    }

    fn password(&self, cx: &Context<Self>) -> String {
        let m = self.model.read(cx);
        let bookmark = m.snap.server(self.con).map(|s| s.bookmark.clone()).unwrap_or_default();
        m.settings.channel_passwords.get(&crate::config::channel_key(&bookmark, self.channel)).cloned().unwrap_or_default()
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        let (tx, rx) = oneshot::channel();
        let query = Query::Files { channel: self.channel, path: self.path.clone(), password: self.password(cx) };
        self.model.read(cx).net.send(Cmd::Query { con: self.con, query, reply: tx });
        cx.spawn(async move |this, cx| {
            let r = rx.await.unwrap_or_else(|_| Err("no answer".into()));
            let _ = this.update(cx, |v, cx| {
                v.entries = Some(r.and_then(|a| match a {
                    Answer::Files(mut f) => {
                        // folders first, then by name
                        f.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
                        Ok(f)
                    }
                    _ => Err("unexpected answer".into()),
                }));
                cx.notify();
            });
        })
        .detach();
    }

    fn reload_soon(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(500)).await;
            let _ = this.update(cx, |v, cx| v.load(cx));
        })
        .detach();
    }

    fn send(&mut self, cmd: FileCmd, cx: &mut Context<Self>) {
        self.model.read(cx).net.send(Cmd::Files { con: self.con, cmd });
    }

    fn open(&mut self, dir: String, cx: &mut Context<Self>) {
        self.path = dir;
        self.entries = None;
        self.confirm_delete = None;
        self.load(cx);
        cx.notify();
    }

    fn upload(&mut self, paths: Vec<std::path::PathBuf>, cx: &mut Context<Self>) {
        let password = self.password(cx);
        for from in paths.into_iter().filter(|p| p.is_file()) {
            let Some(name) = from.file_name().map(|n| n.to_string_lossy().to_string()) else { continue };
            let path = join(&self.path, &name);
            self.send(FileCmd::Upload { channel: self.channel, password: password.clone(), path, from }, cx);
        }
        // reload when the uploads are through
        cx.spawn(async move |this, cx| {
            for _ in 0..60 {
                cx.background_executor().timer(Duration::from_millis(500)).await;
                let busy = this
                    .update(cx, |v, cx| {
                        let snap = &v.model.read(cx).snap;
                        snap.server(v.con).is_some_and(|s| {
                            s.transfers.iter().any(|t| t.upload && matches!(t.state, TransferState::Waiting | TransferState::Running))
                        })
                    })
                    .unwrap_or(false);
                if !busy {
                    break;
                }
            }
            let _ = this.update(cx, |v, cx| v.load(cx));
        })
        .detach();
    }

    fn pick_files(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Upload".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await {
                let _ = this.update(cx, |v, cx| v.upload(paths, cx));
            }
        })
        .detach();
    }

    fn start_naming(&mut self, current: String, window: &mut Window, cx: &mut Context<Self>) {
        self.naming = Some(current.clone());
        self.name_input.update(cx, |i, cx| {
            i.set_value(current, window, cx);
            i.focus(window, cx);
        });
        cx.notify();
    }

    fn finish_naming(&mut self, cx: &mut Context<Self>) {
        let Some(old) = self.naming.take() else { return };
        let new = self.name_input.read(cx).value().trim().replace('/', "_");
        if new.is_empty() || new == old {
            cx.notify();
            return;
        }
        let password = self.password(cx);
        let cmd = if old.is_empty() {
            FileCmd::Mkdir { channel: self.channel, password, path: join(&self.path, &new) }
        } else {
            FileCmd::Rename { channel: self.channel, password, from: join(&self.path, &old), to: join(&self.path, &new) }
        };
        self.send(cmd, cx);
        self.reload_soon(cx);
        cx.notify();
    }

    fn row(&self, i: usize, f: &FileEntry, cx: &mut Context<Self>) -> AnyElement {
        let name = f.name.clone();
        let path = join(&self.path, &f.name);
        let renaming = self.naming.as_deref() == Some(f.name.as_str());
        let confirm = self.confirm_delete.as_deref() == Some(f.name.as_str());
        let btn = |id: (&'static str, usize), icon_name: &'static str, color: u32, tip: &'static str| {
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
                .child(icon(icon_name, 14., color))
        };
        let (open_path, dl_path, del_path, rn_name) = (path.clone(), path.clone(), path.clone(), name.clone());
        let is_dir = f.is_dir;
        h_flex()
            .id(("file", i))
            .group("file-row")
            .gap_3()
            .items_center()
            .h(px(38.))
            .px_2()
            .rounded(px(5.))
            .hover(|d| d.bg(hover_bg()))
            .when(is_dir, |d| {
                d.cursor_pointer().on_click(cx.listener(move |t, _, _, cx| t.open(open_path.clone(), cx)))
            })
            .child(icon(if is_dir { "folder" } else { "file" }, 16., if is_dir { ACCENT } else { MUTED }))
            .child(if renaming {
                div().flex_1().child(Input::new(&self.name_input).small()).into_any_element()
            } else {
                div().flex_1().min_w_0().text_sm().truncate().child(name.clone()).into_any_element()
            })
            .child(div().w(px(80.)).text_xs().text_color(rgb(MUTED)).text_right().child(if is_dir { String::new() } else { size_text(f.size) }))
            .child(div().w(px(120.)).text_xs().text_color(rgb(DIM)).text_right().child(f.date.clone()))
            .child(
                h_flex()
                    .gap_0p5()
                    .w(px(96.))
                    .justify_end()
                    .when(!is_dir, |d| {
                        d.child(btn(("fdl", i), "download", MUTED, "Download").on_click(cx.listener(move |t, _, _, cx| {
                            cx.stop_propagation();
                            let to = download_path(dl_path.rsplit('/').next().unwrap_or(&dl_path));
                            let password = t.password(cx);
                            t.send(FileCmd::Download { channel: t.channel, password, path: dl_path.clone(), to }, cx);
                        })))
                    })
                    .child(btn(("frn", i), "pencil", MUTED, "Rename").on_click(cx.listener(move |t, _, window, cx| {
                        cx.stop_propagation();
                        t.start_naming(rn_name.clone(), window, cx);
                    })))
                    .child(
                        btn(("fdel", i), "trash", RED, if confirm { "Click again to delete" } else { "Delete" })
                            .when(confirm, |d| d.bg(alpha(RED, 0x26)))
                            .on_click(cx.listener(move |t, _, _, cx| {
                                cx.stop_propagation();
                                let name = del_path.rsplit('/').next().unwrap_or("").to_string();
                                if t.confirm_delete.as_deref() == Some(name.as_str()) {
                                    t.confirm_delete = None;
                                    let password = t.password(cx);
                                    t.send(FileCmd::Delete { channel: t.channel, password, path: del_path.clone() }, cx);
                                    t.reload_soon(cx);
                                } else {
                                    t.confirm_delete = Some(name);
                                }
                                cx.notify();
                            })),
                    ),
            )
            .into_any_element()
    }

    fn transfers(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let list = self.model.read(cx).snap.server(self.con)?.transfers.clone();
        if list.is_empty() {
            return None;
        }
        let mut col = v_flex().gap_2();
        for t in list.iter().rev() {
            let frac = if t.size > 0 { (t.done as f32 / t.size as f32).clamp(0., 1.) } else { 0. };
            let (status, color) = match &t.state {
                TransferState::Waiting => ("Waiting".to_string(), MUTED),
                TransferState::Running => (format!("{} of {}", size_text(t.done), size_text(t.size)), MUTED),
                TransferState::Done if t.upload => ("Uploaded".into(), GREEN),
                TransferState::Done => (format!("Saved to {}", t.local.display()), GREEN),
                TransferState::Failed(e) => (format!("Failed: {e}"), RED),
            };
            let local = t.local.clone();
            col = col.child(
                v_flex()
                    .gap_1()
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(icon(if t.upload { "upload" } else { "download" }, 13., MUTED))
                            .child(div().flex_1().text_sm().truncate().child(t.name.clone()))
                            .child(label(status).text_color(rgb(color)).truncate().max_w(px(380.)))
                            .when(t.state == TransferState::Done && !t.upload, |d| {
                                d.child(
                                    div()
                                        .id(SharedString::from(format!("show-{}", t.id)))
                                        .cursor_pointer()
                                        .tooltip(|w, cx| Tooltip::new("Show in folder").build(w, cx))
                                        .child(icon("folder-open", 13., MUTED))
                                        .on_click(move |_, _, cx| {
                                            if let Some(dir) = local.parent() {
                                                cx.open_with_system(dir);
                                            }
                                        }),
                                )
                            }),
                    )
                    .when(matches!(t.state, TransferState::Running | TransferState::Waiting), |d| {
                        d.child(div().h(px(3.)).w_full().rounded_full().overflow_hidden().child(segments(frac, ACCENT)))
                    }),
            );
        }
        Some(
            v_flex()
                .gap_3()
                .child(
                    h_flex().items_center().child(div().flex_1().child(section_title("Transfers"))).child(
                        Button::new("tr-clear").label("Clear finished").small().ghost().on_click(cx.listener(|t, _, _, cx| {
                            t.send(FileCmd::ClearDone, cx);
                        })),
                    ),
                )
                .child(col)
                .into_any_element(),
        )
    }
}

impl Render for FilesView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // breadcrumbs: / > folder > sub
        let mut crumbs = h_flex().gap_1().items_center();
        let mut acc = String::new();
        let parts: Vec<String> = std::iter::once(String::new())
            .chain(self.path.split('/').filter(|p| !p.is_empty()).map(str::to_string))
            .collect();
        for (i, part) in parts.iter().enumerate() {
            if i > 0 {
                acc = format!("{acc}/{part}");
                crumbs = crumbs.child(icon("chevron-right", 12., DIM));
            }
            let target = if acc.is_empty() { "/".to_string() } else { acc.clone() };
            let last = i == parts.len() - 1;
            crumbs = crumbs.child(
                div()
                    .id(("crumb", i))
                    .px_1p5()
                    .py_0p5()
                    .rounded(px(4.))
                    .text_sm()
                    .text_color(rgb(if last { TEXT } else { MUTED }))
                    .when(!last, |d| d.cursor_pointer().hover(|d| d.bg(hover_bg())))
                    .on_click(cx.listener(move |t, _, _, cx| t.open(target.clone(), cx)))
                    .child(if i == 0 { "Files".to_string() } else { part.clone() }),
            );
        }
        let mut list = v_flex();
        if self.naming.as_deref() == Some("") {
            list = list.child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .h(px(38.))
                    .px_2()
                    .child(icon("folder-plus", 16., ACCENT))
                    .child(div().flex_1().child(Input::new(&self.name_input).small())),
            );
        }
        match &self.entries {
            None => list = list.child(label("Loading...")),
            Some(Err(e)) => list = list.child(label(e.clone()).text_color(rgb(RED))),
            Some(Ok(e)) if e.is_empty() => {
                list = list.child(div().py_6().flex().justify_center().child(label("No files here. Drop files on this window to upload them.")))
            }
            Some(Ok(e)) => {
                for (i, f) in e.clone().iter().enumerate() {
                    list = list.child(self.row(i, f, cx));
                }
            }
        }
        v_flex()
            .id("files-drop")
            .gap_4()
            .rounded(px(6.))
            .drag_over::<ExternalPaths>(|s, _, _, _| s.bg(rgba(0x8fb3d914)))
            .on_drop(cx.listener(|t, paths: &ExternalPaths, _, cx| t.upload(paths.paths().to_vec(), cx)))
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(div().flex_1().child(crumbs))
                    .child(Button::new("f-mkdir").label("New folder").small().ghost().on_click(cx.listener(|t, _, window, cx| {
                        t.start_naming(String::new(), window, cx)
                    })))
                    .child(Button::new("f-reload").label("Reload").small().ghost().on_click(cx.listener(|t, _, _, cx| t.load(cx))))
                    .child(Button::new("f-upload").label("Upload").small().primary().on_click(cx.listener(|t, _, _, cx| t.pick_files(cx)))),
            )
            .child(
                h_flex()
                    .gap_3()
                    .px_2()
                    .child(div().w(px(16.)))
                    .child(div().flex_1().child(label("Name")))
                    .child(div().w(px(80.)).text_right().child(label("Size")))
                    .child(div().w(px(120.)).text_right().child(label("Changed")))
                    .child(div().w(px(96.))),
            )
            .child(list)
            .when_some(self.transfers(cx), |d, t| d.child(div().pt_4().child(t)))
    }
}
