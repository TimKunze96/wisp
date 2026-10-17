//! Icon picker: the server's icons (its /icons folder) as a grid, "no icon",
//! and uploading a new one. TeamSpeak names an icon by the CRC32 of its file.

use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{
    Context, Entity, EventEmitter, InteractiveElement, IntoElement, ParentElement, PathPromptOptions, Render,
    StatefulInteractiveElement, Styled, Window, div, img, px, rgb,
};
use tokio::sync::oneshot;

use super::Model;
use super::widgets::*;
use crate::net::admin::{Answer, Query};
use crate::net::files::FileCmd;
use crate::net::{Cmd, ConId, icon_dir};

/// Picked an icon id (None: no icon).
pub struct IconPicked(pub Option<u32>);

pub struct IconGrid {
    model: Entity<Model>,
    con: ConId,
    current: Option<u32>,
    icons: Option<Result<Vec<u32>, String>>,
    dir: std::path::PathBuf,
    error: Option<String>,
}

impl EventEmitter<IconPicked> for IconGrid {}

impl IconGrid {
    pub fn new(model: Entity<Model>, con: ConId, current: Option<u32>, cx: &mut Context<Self>) -> Self {
        cx.observe(&model, |_, _, cx| cx.notify()).detach();
        let address = model.read(cx).snap.server(con).map(|s| s.bookmark.clone()).unwrap_or_default();
        let address = model
            .read(cx)
            .settings
            .bookmarks
            .iter()
            .find(|b| b.id == address)
            .map(|b| b.address.clone())
            .unwrap_or_default();
        let mut this = Self { model, con, current, icons: None, dir: icon_dir(&address), error: None };
        this.load(cx);
        this
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        let (tx, rx) = oneshot::channel();
        let query = Query::Files { channel: 0, path: "/icons".into(), password: String::new() };
        self.model.read(cx).net.send(Cmd::Query { con: self.con, query, reply: tx });
        cx.spawn(async move |this, cx| {
            let r = rx.await.unwrap_or_else(|_| Err("no answer".into()));
            let _ = this.update(cx, |v, cx| {
                let ids: Result<Vec<u32>, String> = r.and_then(|a| match a {
                    Answer::Files(f) => Ok(f.iter().filter_map(|f| f.name.strip_prefix("icon_")?.parse().ok()).collect()),
                    _ => Err("unexpected answer".into()),
                });
                if let Ok(ids) = &ids {
                    v.model.read(cx).net.send(Cmd::FetchIcons { con: v.con, ids: ids.clone() });
                }
                v.icons = Some(ids);
                cx.notify();
            });
        })
        .detach();
    }

    fn upload(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Upload icon".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else { return };
            let Some(path) = paths.into_iter().next() else { return };
            let _ = this.update(cx, |v, cx| {
                let Ok(bytes) = std::fs::read(&path) else {
                    v.error = Some("Couldn't read that file.".into());
                    cx.notify();
                    return;
                };
                if image::load_from_memory(&bytes).is_err() {
                    v.error = Some("That isn't a picture.".into());
                    cx.notify();
                    return;
                }
                let id = crc32fast::hash(&bytes);
                // show it right away; the server gets it in the background
                let _ = std::fs::create_dir_all(&v.dir);
                let _ = std::fs::write(v.dir.join(id.to_string()), &bytes);
                let cmd = FileCmd::Upload { channel: 0, password: String::new(), path: format!("/icon_{id}"), from: path };
                v.model.read(cx).net.send(Cmd::Files { con: v.con, cmd });
                if let Some(Ok(list)) = &mut v.icons {
                    if !list.contains(&id) {
                        list.push(id);
                    }
                }
                v.current = Some(id);
                v.error = None;
                cx.emit(IconPicked(Some(id)));
                cx.notify();
            });
        })
        .detach();
    }

    fn cell(&self, id: Option<u32>, cx: &mut Context<Self>) -> impl IntoElement {
        let on = self.current == id;
        let path = id.map(|i| self.dir.join(i.to_string())).filter(|p| p.exists());
        div()
            .id(("icon", id.unwrap_or(0) as usize))
            .size(px(34.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(5.))
            .cursor_pointer()
            .border_1()
            .border_color(if on { alpha(ACCENT, 0xaa) } else { border() })
            .when(on, |d| d.bg(alpha(ACCENT, 0x1a)))
            .hover(|d| d.bg(hover_bg()))
            .on_click(cx.listener(move |t, _, _, cx| {
                t.current = id;
                cx.emit(IconPicked(id));
                cx.notify();
            }))
            .child(match (id, path) {
                (None, _) => icon("x", 14., DIM).into_any_element(),
                (Some(_), Some(p)) => img(p).size(px(16.)).into_any_element(),
                (Some(_), None) => div().size(px(6.)).rounded_full().bg(rgb(DIM)).into_any_element(),
            })
    }
}

impl Render for IconGrid {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut grid = h_flex().flex_wrap().gap_1p5().child(self.cell(None, cx));
        match &self.icons {
            None => grid = grid.child(label("Loading icons...")),
            Some(Err(e)) => grid = grid.child(label(e.clone()).text_color(rgb(RED))),
            Some(Ok(ids)) => {
                for id in ids.clone() {
                    grid = grid.child(self.cell(Some(id), cx));
                }
            }
        }
        grid = grid.child(
            div()
                .id("icon-upload")
                .size(px(34.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(5.))
                .cursor_pointer()
                .border_1()
                .border_color(border())
                .hover(|d| d.bg(hover_bg()))
                .tooltip(|w, cx| Tooltip::new("Upload an icon (16×16 works best)").build(w, cx))
                .on_click(cx.listener(|t, _, _, cx| t.upload(cx)))
                .child(icon("upload", 14., MUTED)),
        );
        v_flex().gap_2().child(grid).when_some(self.error.clone(), |d, e| d.child(label(e).text_color(rgb(RED))))
    }
}
