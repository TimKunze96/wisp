//! Server admin window: groups (with permissions and members), bans,
//! privilege keys, complaints, server settings, the server log, and a
//! permission editor for channels and clients.

use std::collections::HashMap;
use std::time::Duration;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Sizable, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{
    AnyElement, App, AppContext, ClipboardItem, Context, Entity, FontWeight, InteractiveElement, IntoElement,
    ParentElement, Render, SharedString, StatefulInteractiveElement, Styled, Window, div, px, rgb, rgba,
};
use tokio::sync::oneshot;

use super::Model;
use super::widgets::*;
use crate::net::admin::{
    AdminWrite, Answer, Ban, Complaint, GroupKind, Member, OfflineMessage, PermTarget, PermValue, Query, ServerInfo,
    ServerProps, TempPassword, Token, permissions,
};
use crate::net::{Cmd, ConId, ServerSnap};

#[derive(Clone, Debug, PartialEq)]
pub enum AdminTab {
    ServerGroups,
    ChannelGroups,
    Bans,
    Keys,
    Complaints,
    Server,
    Log,
    /// Permissions of a channel or client, opened from a context menu.
    Perms(PermTarget, String),
    /// A channel's files (channel id, name).
    Files(u64, String),
    /// Offline messages; with someone to write to (uid, name).
    Inbox(Option<(String, String)>),
    /// What applies to someone in a channel, and why (channel, db id, name).
    Overview(u64, u64, String),
}

impl AdminTab {
    fn title(&self) -> String {
        match self {
            AdminTab::ServerGroups => "Server groups".into(),
            AdminTab::ChannelGroups => "Channel groups".into(),
            AdminTab::Bans => "Bans".into(),
            AdminTab::Keys => "Privilege keys".into(),
            AdminTab::Complaints => "Complaints".into(),
            AdminTab::Server => "Server".into(),
            AdminTab::Log => "Log".into(),
            AdminTab::Perms(_, title) => title.clone(),
            AdminTab::Files(_, name) => format!("Files in {name}"),
            AdminTab::Inbox(_) => "Inbox".into(),
            AdminTab::Overview(.., name) => format!("{name}: effective"),
        }
    }
    fn icon(&self) -> &'static str {
        match self {
            AdminTab::ServerGroups => "shield",
            AdminTab::ChannelGroups => "users",
            AdminTab::Bans => "ban",
            AdminTab::Keys => "key-round",
            AdminTab::Complaints => "flag",
            AdminTab::Server => "server",
            AdminTab::Log => "scroll-text",
            AdminTab::Perms(..) => "lock-keyhole",
            AdminTab::Files(..) => "folder",
            AdminTab::Inbox(_) => "inbox",
            AdminTab::Overview(..) => "list-checks",
        }
    }
}

/// Ask the server something; `f` gets the answer on the UI thread.
fn ask<V: 'static>(
    model: &Entity<Model>,
    con: ConId,
    query: Query,
    cx: &mut Context<V>,
    f: impl FnOnce(&mut V, Result<Answer, String>, &mut Context<V>) + 'static,
) {
    let (tx, rx) = oneshot::channel();
    model.read(cx).net.send(Cmd::Query { con, query, reply: tx });
    cx.spawn(async move |this, cx| {
        let r = rx.await.unwrap_or_else(|_| Err("no answer".into()));
        let _ = this.update(cx, |v, cx| {
            f(v, r, cx);
            cx.notify();
        });
    })
    .detach();
}

/// Run `f` a moment from now (after a write, to reload what changed).
fn later<V: 'static>(cx: &mut Context<V>, f: impl FnOnce(&mut V, &mut Context<V>) + 'static) {
    cx.spawn(async move |this, cx| {
        cx.background_executor().timer(Duration::from_millis(400)).await;
        let _ = this.update(cx, |v, cx| f(v, cx));
    })
    .detach();
}

fn input(window: &mut Window, cx: &mut App, placeholder: &str) -> Entity<InputState> {
    let p = placeholder.to_string();
    cx.new(|cx| InputState::new(window, cx).placeholder(p))
}

fn value(e: &Entity<InputState>, cx: &App) -> String {
    e.read(cx).value().trim().to_string()
}

fn error_box(e: &str) -> AnyElement {
    div().px_3().py_2().rounded(px(6.)).bg(alpha(RED, 0x14)).child(label(e.to_string()).text_color(rgb(RED))).into_any_element()
}

fn icon_btn(id: impl Into<gpui_kit::ElementId>, name: &'static str, color: u32, tip: &'static str) -> gpui_kit::Stateful<gpui_kit::Div> {
    div()
        .id(id)
        .size(px(28.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(5.))
        .cursor_pointer()
        .hover(|d| d.bg(hover_bg()))
        .tooltip(move |w, cx| Tooltip::new(tip).build(w, cx))
        .child(icon(name, 14., color))
}

fn human(secs: Option<u64>) -> String {
    match secs {
        None => "Permanent".into(),
        Some(s) if s % 86_400 == 0 => format!("{} d", s / 86_400),
        Some(s) if s % 3600 == 0 => format!("{} h", s / 3600),
        Some(s) => format!("{} min", s.div_ceil(60)),
    }
}

// ---- permission editor ------------------------------------------------------

/// Category of a permission, from its name (b_virtualserver_... -> Virtual server).
fn category(name: &str) -> &'static str {
    match name.split('_').nth(1).unwrap_or("") {
        "serverinstance" => "Instance",
        "virtualserver" => "Virtual server",
        "channel" => "Channel",
        "group" => "Groups",
        "client" => "Client",
        "ft" => "File transfer",
        _ => "Other",
    }
}

pub struct PermEditor {
    model: Entity<Model>,
    con: ConId,
    target: PermTarget,
    values: Option<Result<HashMap<u32, PermValue>, String>>,
    search: Entity<InputState>,
    only_set: bool,
    /// The row being edited: (permission id incl. 0x8000 for the grant).
    editing: Option<u32>,
    edit: Entity<InputState>,
}

impl PermEditor {
    pub fn new(model: Entity<Model>, con: ConId, target: PermTarget, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = input(window, cx, "Search permissions");
        cx.subscribe(&search, |_, _, _: &InputEvent, cx| cx.notify()).detach();
        let edit = input(window, cx, "Value");
        cx.subscribe_in(&edit, window, |this: &mut Self, _, ev: &InputEvent, _, cx| match ev {
            InputEvent::PressEnter { .. } => this.commit(cx),
            InputEvent::Blur => {
                this.editing = None;
                cx.notify();
            }
            _ => {}
        })
        .detach();
        let mut this = Self { model, con, target, values: None, search, only_set: true, editing: None, edit };
        this.load(cx);
        this
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        ask(&self.model, self.con, Query::Perms(self.target), cx, |this: &mut Self, r, _| {
            this.values = Some(r.and_then(|a| match a {
                Answer::Perms(list) => Ok(list.into_iter().map(|p| (p.id, p)).collect()),
                _ => Err("unexpected answer".into()),
            }));
        });
    }

    fn write(&mut self, w: AdminWrite, cx: &mut Context<Self>) {
        self.model.read(cx).net.send(Cmd::Write { con: self.con, write: w });
        later(cx, |this: &mut Self, cx| this.load(cx));
    }

    fn set(&mut self, id: u32, value: i32, cx: &mut Context<Self>) {
        let (negated, skip) = self
            .values
            .as_ref()
            .and_then(|v| v.as_ref().ok())
            .and_then(|v| v.get(&id))
            .map(|p| (p.negated, p.skip))
            .unwrap_or((false, false));
        // show it right away; the reload confirms (or reverts) it
        if let Some(Ok(v)) = &mut self.values {
            v.insert(id, PermValue { id, value, negated, skip });
        }
        self.write(AdminWrite::SetPerm { target: self.target, perm: id, value, negated, skip }, cx);
    }

    fn start_edit(&mut self, id: u32, current: Option<i32>, window: &mut Window, cx: &mut Context<Self>) {
        self.editing = Some(id);
        let v = current.map(|v| v.to_string()).unwrap_or_default();
        self.edit.update(cx, |i, cx| {
            i.set_value(v, window, cx);
            i.focus(window, cx);
        });
        cx.notify();
    }

    fn commit(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.editing.take() else { return };
        match value(&self.edit, cx).parse::<i32>() {
            Ok(v) => self.set(id, v, cx),
            Err(_) => cx.notify(),
        }
    }

    fn value_cell(&self, id: u32, current: Option<i32>, boolean: bool, cx: &mut Context<Self>) -> AnyElement {
        if boolean {
            return Switch::new(("pb", id as usize))
                .checked(current == Some(1))
                .on_click(cx.listener(move |t, v: &bool, _, cx| t.set(id, i32::from(*v), cx)))
                .into_any_element();
        }
        if self.editing == Some(id) {
            return div().w(px(90.)).child(Input::new(&self.edit).small()).into_any_element();
        }
        div()
            .id(("pv", id as usize))
            .w(px(90.))
            .h(px(26.))
            .px_2()
            .flex()
            .items_center()
            .rounded(px(5.))
            .cursor_pointer()
            .text_sm()
            .text_color(rgb(if current.is_some() { TEXT } else { DIM }))
            .hover(|d| d.bg(hover_bg()))
            .child(current.map(|v| v.to_string()).unwrap_or_else(|| "–".into()))
            .on_click(cx.listener(move |t, _, window, cx| t.start_edit(id, current, window, cx)))
            .into_any_element()
    }
}

impl Render for PermEditor {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let values = match &self.values {
            None => return v_flex().child(label("Loading permissions...")).into_any_element(),
            Some(Err(e)) => return v_flex().child(error_box(e)).into_any_element(),
            Some(Ok(v)) => v.clone(),
        };
        let q = value(&self.search, cx).to_lowercase();
        let grants = matches!(self.target, PermTarget::ServerGroup(_) | PermTarget::ChannelGroup(_) | PermTarget::Client(_));
        let mut rows = v_flex();
        let mut last_cat = "";
        let mut shown = 0;
        for p in permissions().iter().filter(|p| p.id < 0x8000) {
            let set = values.get(&p.id).map(|v| v.value);
            let grant = values.get(&(p.id | 0x8000)).map(|v| v.value);
            if self.only_set && set.is_none() && grant.is_none() {
                continue;
            }
            if !q.is_empty() && !p.name.contains(&q) && !p.description.to_lowercase().contains(&q) {
                continue;
            }
            shown += 1;
            let cat = category(&p.name);
            if cat != last_cat {
                last_cat = cat;
                rows = rows.child(div().pt_4().pb_1().child(section_title(cat)));
            }
            let id = p.id;
            let boolean = p.name.starts_with("b_");
            rows = rows.child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .min_h(px(40.))
                    .py_1()
                    .border_b_1()
                    .border_color(rgba(0xffffff08))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().text_sm().truncate().text_color(rgb(if set.is_some() { TEXT } else { MUTED })).child(p.name.clone()))
                            .when(!p.description.is_empty(), |d| d.child(label(p.description.clone()).truncate())),
                    )
                    .child(div().w(px(90.)).flex().justify_end().child(self.value_cell(id, set, boolean, cx)))
                    .when(grants, |d| d.child(div().w(px(90.)).child(self.value_cell(id | 0x8000, grant, false, cx))))
                    .child(div().w(px(28.)).when(set.is_some() || grant.is_some(), |d| {
                        d.child(icon_btn(("pdel", id as usize), "x", MUTED, "Remove").on_click(cx.listener(move |t, _, _, cx| {
                            let target = t.target;
                            if let Some(Ok(v)) = &mut t.values {
                                v.remove(&id);
                                v.remove(&(id | 0x8000));
                            }
                            if set.is_some() {
                                t.write(AdminWrite::DelPerm { target, perm: id }, cx);
                            }
                            if grant.is_some() {
                                t.write(AdminWrite::DelPerm { target, perm: id | 0x8000 }, cx);
                            }
                        })))
                    })),
            );
        }
        if shown == 0 {
            rows = rows.child(div().pt_4().child(label(if self.only_set && q.is_empty() {
                "Nothing set here. Turn off \"Only set\" to add permissions."
            } else {
                "No permission matches."
            })));
        }
        v_flex()
            .gap_2()
            .child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .child(div().flex_1().child(Input::new(&self.search).cleanable(true)))
                    .child(Switch::new("only-set").checked(self.only_set).on_click(cx.listener(|t, v: &bool, _, cx| {
                        t.only_set = *v;
                        cx.notify();
                    })))
                    .child(label("Only set")),
            )
            .child(
                h_flex()
                    .gap_3()
                    .pt_2()
                    .child(div().flex_1().child(label("Permission")))
                    .child(div().w(px(90.)).flex().justify_end().child(label("Value")))
                    .when(grants, |d| d.child(div().w(px(90.)).child(label("Grant"))))
                    .child(div().w(px(28.))),
            )
            .child(rows)
            .into_any_element()
    }
}

// ---- the window -------------------------------------------------------------

pub struct AdminView {
    model: Entity<Model>,
    pub con: ConId,
    tab: AdminTab,
    error: Option<String>,
    // groups
    group: Option<u64>,
    show_members: bool,
    perms: Option<Entity<PermEditor>>,
    group_icons: Option<Entity<super::icons::IconGrid>>,
    files: Option<Entity<super::files_view::FilesView>>,
    members: Option<Result<Vec<Member>, String>>,
    group_name: Entity<InputState>,
    new_group: Entity<InputState>,
    member_uid: Entity<InputState>,
    // bans
    bans: Option<Result<Vec<Ban>, String>>,
    ban_ip: Entity<InputState>,
    ban_name: Entity<InputState>,
    ban_uid: Entity<InputState>,
    ban_reason: Entity<InputState>,
    ban_secs: Option<u64>,
    // keys
    keys: Option<Result<Vec<Token>, String>>,
    key_desc: Entity<InputState>,
    key_use: Entity<InputState>,
    /// (server group?, group id, channel for channel groups)
    key_group: Option<(bool, u64)>,
    key_channel: Option<u64>,
    // complaints
    complaints: Option<Result<Vec<Complaint>, String>>,
    // server
    info: Option<ServerInfo>,
    srv: HashMap<&'static str, Entity<InputState>>,
    host_mode: u8,
    temp: Option<Result<Vec<TempPassword>, String>>,
    temp_pw: Entity<InputState>,
    temp_desc: Entity<InputState>,
    // log
    log: Option<Result<Vec<String>, String>>,
    // overview
    overview: Option<Result<Vec<crate::net::admin::OverviewRow>, String>>,
    overview_search: Entity<InputState>,
    // connection
    connection: Option<Result<crate::net::admin::ServerConnection, String>>,
    // inbox
    inbox: Option<Result<Vec<OfflineMessage>, String>>,
    /// Opened message (with its text).
    open_message: Option<OfflineMessage>,
    compose_to: Entity<InputState>,
    compose_subject: Entity<InputState>,
    compose_text: Entity<gpui_kit::component::input::TextareaState>,
    composing: bool,
}

const SERVER_FIELDS: [(&str, &str, &str); 12] = [
    ("name", "Name", ""),
    ("phonetic_name", "Phonetic name", ""),
    ("welcome_message", "Welcome message", ""),
    ("max_clients", "Max. clients", "e.g. 32"),
    ("reserved_slots", "Reserved slots", "0"),
    ("password", "New password", "Leave empty to keep, \"-\" removes it"),
    ("hostmessage", "Host message", ""),
    ("hostbanner_url", "Banner link", "https://"),
    ("hostbanner_gfx_url", "Banner image", "https://"),
    ("hostbutton_tooltip", "Host button tooltip", ""),
    ("hostbutton_url", "Host button link", "https://"),
    ("temp_delete_delay", "Temp channel delete delay", "seconds"),
];

impl AdminView {
    pub fn new(model: Entity<Model>, con: ConId, tab: AdminTab, window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.observe(&model, |_, _, cx| cx.notify()).detach();
        let mut srv = HashMap::new();
        for (key, _, ph) in SERVER_FIELDS {
            srv.insert(key, input(window, cx, ph));
        }
        let mut this = Self {
            group_name: input(window, cx, "Group name"),
            new_group: input(window, cx, "New group name"),
            member_uid: input(window, cx, "Unique id to add"),
            ban_ip: input(window, cx, "IP (optional)"),
            ban_name: input(window, cx, "Name pattern, regex (optional)"),
            ban_uid: input(window, cx, "Unique id (optional)"),
            ban_reason: input(window, cx, "Reason (optional)"),
            key_desc: input(window, cx, "Description (optional)"),
            key_use: input(window, cx, "Privilege key"),
            temp_pw: input(window, cx, "Password"),
            temp_desc: input(window, cx, "What it's for"),
            compose_to: input(window, cx, "Unique id"),
            overview_search: input(window, cx, "Search permissions"),
            compose_subject: input(window, cx, "Subject"),
            compose_text: cx.new(|cx| gpui_kit::component::input::TextareaState::new(window, cx).rows(5).placeholder("Message")),
            model,
            con,
            tab: AdminTab::ServerGroups,
            error: None,
            group: None,
            show_members: false,
            perms: None,
            group_icons: None,
            files: None,
            members: None,
            bans: None,
            ban_secs: Some(86_400),
            keys: None,
            key_group: None,
            key_channel: None,
            complaints: None,
            info: None,
            srv,
            host_mode: 0,
            temp: None,
            log: None,
            inbox: None,
            overview: None,
            connection: None,
            open_message: None,
            composing: false,
        };
        cx.subscribe(&this.overview_search, |_, _, _: &InputEvent, cx| cx.notify()).detach();
        this.switch(tab, window, cx);
        this
    }

    fn server<'a>(&self, cx: &'a App) -> Option<&'a ServerSnap> {
        self.model.read(cx).snap.server(self.con)
    }

    fn write(&mut self, w: AdminWrite, cx: &mut Context<Self>) {
        self.model.read(cx).net.send(Cmd::Write { con: self.con, write: w });
    }

    pub fn switch(&mut self, tab: AdminTab, window: &mut Window, cx: &mut Context<Self>) {
        self.tab = tab.clone();
        self.error = None;
        self.perms = None;
        match tab {
            AdminTab::ServerGroups | AdminTab::ChannelGroups => {
                self.group = None;
                let first = self.groups(cx).first().map(|(id, _)| *id);
                if let Some(id) = first {
                    self.pick_group(id, window, cx);
                }
            }
            AdminTab::Bans => self.load_bans(cx),
            AdminTab::Keys => self.load_keys(cx),
            AdminTab::Complaints => self.load_complaints(cx),
            AdminTab::Server => self.load_server(window, cx),
            AdminTab::Log => self.load_log(cx),
            AdminTab::Perms(target, _) => {
                let (model, con) = (self.model.clone(), self.con);
                self.perms = Some(cx.new(|cx| PermEditor::new(model, con, target, window, cx)));
            }
            AdminTab::Overview(channel, db_id, _) => {
                self.overview = None;
                ask(&self.model, self.con, Query::Overview { channel, db_id }, cx, |this: &mut Self, r, _| {
                    this.overview = Some(r.and_then(|a| match a {
                        Answer::Overview(rows) => Ok(rows),
                        _ => Err("unexpected answer".into()),
                    }));
                });
            }
            AdminTab::Inbox(to) => {
                self.open_message = None;
                self.composing = to.is_some();
                if let Some((uid, name)) = to {
                    self.compose_to.update(cx, |i, cx| i.set_value(uid, window, cx));
                    self.compose_subject.update(cx, |i, cx| i.set_value(format!("Hi {name}"), window, cx));
                }
                self.load_inbox(cx);
            }
            AdminTab::Files(channel, _) => {
                let (model, con) = (self.model.clone(), self.con);
                self.files = Some(cx.new(|cx| super::files_view::FilesView::new(model, con, channel, window, cx)));
            }
        }
        cx.notify();
    }

    // ---- groups

    fn kind(&self) -> GroupKind {
        if self.tab == AdminTab::ChannelGroups { GroupKind::Channel } else { GroupKind::Server }
    }

    fn groups(&self, cx: &App) -> Vec<(u64, crate::net::GroupBadge)> {
        let Some(s) = self.server(cx) else { return Vec::new() };
        let list = if self.kind() == GroupKind::Channel { &s.channel_groups } else { &s.groups };
        list.iter().map(|g| (g.id, g.badge.clone())).collect()
    }

    fn group_target(&self, id: u64) -> PermTarget {
        if self.kind() == GroupKind::Channel { PermTarget::ChannelGroup(id) } else { PermTarget::ServerGroup(id) }
    }

    fn toggle_group_icons(&mut self, cx: &mut Context<Self>) {
        if self.group_icons.take().is_some() {
            cx.notify();
            return;
        }
        let Some(group) = self.group else { return };
        let current = self.server(cx).and_then(|s| {
            let list = if self.kind() == GroupKind::Channel { &s.channel_groups } else { &s.groups };
            list.iter().find(|g| g.id == group).map(|g| g.icon_id).filter(|i| *i != 0)
        });
        let (model, con) = (self.model.clone(), self.con);
        let grid = cx.new(|cx| super::icons::IconGrid::new(model, con, current, cx));
        cx.subscribe(&grid, move |this, _, ev: &super::icons::IconPicked, cx| {
            if let Some(w) = crate::net::admin::icon_write(this.group_target(group), ev.0) {
                this.write(w, cx);
            }
        })
        .detach();
        self.group_icons = Some(grid);
        cx.notify();
    }

    fn pick_group(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        self.group = Some(id);
        self.group_icons = None;
        self.members = None;
        let name = self.groups(cx).into_iter().find(|(g, _)| *g == id).map(|(_, b)| b.name).unwrap_or_default();
        self.group_name.update(cx, |i, cx| i.set_value(name, window, cx));
        let target = if self.kind() == GroupKind::Channel { PermTarget::ChannelGroup(id) } else { PermTarget::ServerGroup(id) };
        let (model, con) = (self.model.clone(), self.con);
        self.perms = Some(cx.new(|cx| PermEditor::new(model, con, target, window, cx)));
        if self.show_members && self.kind() == GroupKind::Server {
            self.load_members(cx);
        }
        cx.notify();
    }

    fn load_members(&mut self, cx: &mut Context<Self>) {
        let Some(g) = self.group else { return };
        ask(&self.model, self.con, Query::ServerGroupMembers(g), cx, |this: &mut Self, r, _| {
            this.members = Some(r.and_then(|a| match a {
                Answer::Members(m) => Ok(m),
                _ => Err("unexpected answer".into()),
            }));
        });
    }

    fn add_member(&mut self, cx: &mut Context<Self>) {
        let (Some(group), uid) = (self.group, value(&self.member_uid, cx)) else { return };
        if uid.is_empty() {
            return;
        }
        ask(&self.model, self.con, Query::DbIdFromUid(uid), cx, move |this: &mut Self, r, cx| match r {
            Ok(Answer::DbId(db_id)) if db_id != 0 => {
                this.error = None;
                this.write(AdminWrite::AddMember { group, db_id }, cx);
                later(cx, |this: &mut Self, cx| this.load_members(cx));
            }
            Ok(_) => this.error = Some("Nobody with that unique id has been on this server.".into()),
            Err(e) => this.error = Some(e),
        });
    }

    fn groups_tab(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let kind = self.kind();
        let groups = self.groups(cx);
        let mut list = v_flex().gap_0p5();
        for (id, b) in &groups {
            let (id, on) = (*id, self.group == Some(*id));
            list = list.child(
                h_flex()
                    .id(("grp", id as usize))
                    .gap_2()
                    .items_center()
                    .h(px(30.))
                    .px_2()
                    .rounded(px(5.))
                    .cursor_pointer()
                    .when(on, |d| d.bg(active_bg()))
                    .hover(|d| d.bg(hover_bg()))
                    .on_click(cx.listener(move |t, _, window, cx| t.pick_group(id, window, cx)))
                    .child(div().w(px(16.)).flex().justify_center().children(badge(b)))
                    .child(div().text_sm().truncate().text_color(rgb(if on { TEXT } else { 0xb3b8c0 })).child(b.name.clone())),
            );
        }
        let left = v_flex()
            .w(px(220.))
            .flex_none()
            .gap_3()
            .child(list)
            .child(
                v_flex()
                    .gap_2()
                    .pt_3()
                    .border_t_1()
                    .border_color(border())
                    .child(Input::new(&self.new_group).small())
                    .child(Button::new("grp-new").label("Create group").small().on_click(cx.listener(move |t, _, _, cx| {
                        let name = value(&t.new_group, cx);
                        if !name.is_empty() {
                            t.write(AdminWrite::AddGroup { kind, name }, cx);
                        }
                    }))),
            );
        let Some(group) = self.group else {
            return h_flex().gap_6().items_start().child(left).child(label("No groups.")).into_any_element();
        };
        let tabs = h_flex()
            .gap_2()
            .child(chip("g-perms", "Permissions", !self.show_members).on_click(cx.listener(|t, _, _, cx| {
                t.show_members = false;
                cx.notify();
            })))
            .when(kind == GroupKind::Server, |d| {
                d.child(chip("g-members", "Members", self.show_members).on_click(cx.listener(|t, _, _, cx| {
                    t.show_members = true;
                    t.load_members(cx);
                    cx.notify();
                })))
            });
        let head = h_flex()
            .gap_2()
            .items_center()
            .child(div().flex_1().child(Input::new(&self.group_name)))
            .child(Button::new("grp-rename").label("Rename").small().ghost().on_click(cx.listener(move |t, _, _, cx| {
                let name = value(&t.group_name, cx);
                if !name.is_empty() {
                    t.write(AdminWrite::RenameGroup { kind, id: group, name }, cx);
                }
            })))
            .child(Button::new("grp-icon").label(if self.group_icons.is_some() { "Done" } else { "Icon" }).small().ghost().on_click(
                cx.listener(|t, _, _, cx| t.toggle_group_icons(cx)),
            ))
            .child(Button::new("grp-copy").label("Copy").small().ghost().on_click(cx.listener(move |t, _, _, cx| {
                let name = format!("{} (copy)", value(&t.group_name, cx));
                t.write(AdminWrite::CopyGroup { kind, from: group, name }, cx);
            })))
            .child(icon_btn("grp-del", "trash", RED, "Delete group").on_click(cx.listener(move |t, _, _, cx| {
                t.write(AdminWrite::DelGroup { kind, id: group }, cx);
                t.group = None;
                t.perms = None;
                cx.notify();
            })));
        let body: AnyElement = if self.show_members {
            let mut col = v_flex().gap_1();
            match &self.members {
                None => col = col.child(label("Loading members...")),
                Some(Err(e)) => col = col.child(error_box(e)),
                Some(Ok(m)) if m.is_empty() => col = col.child(label("Nobody is in this group.")),
                Some(Ok(m)) => {
                    for (i, mem) in m.iter().enumerate() {
                        let db_id = mem.db_id;
                        col = col.child(
                            h_flex()
                                .gap_3()
                                .items_center()
                                .h(px(34.))
                                .border_b_1()
                                .border_color(rgba(0xffffff08))
                                .child(avatar(&mem.name, None, 22., false))
                                .child(div().flex_1().text_sm().truncate().child(mem.name.clone()))
                                .child(label(mem.uid.clone()).truncate().max_w(px(220.)))
                                .child(icon_btn(("mrm", i), "user-minus", RED, "Remove from group").on_click(cx.listener(
                                    move |t, _, _, cx| {
                                        t.write(AdminWrite::RemoveMember { group, db_id }, cx);
                                        later(cx, |t: &mut Self, cx| t.load_members(cx));
                                    },
                                ))),
                        );
                    }
                }
            }
            v_flex()
                .gap_3()
                .child(
                    h_flex()
                        .gap_2()
                        .child(div().flex_1().child(Input::new(&self.member_uid)))
                        .child(Button::new("m-add").label("Add").small().on_click(cx.listener(|t, _, _, cx| t.add_member(cx)))),
                )
                .child(col)
                .into_any_element()
        } else {
            match &self.perms {
                Some(p) => p.clone().into_any_element(),
                None => label("Loading...").into_any_element(),
            }
        };
        h_flex()
            .gap_6()
            .items_start()
            .child(left)
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_4()
                    .child(head)
                    .when_some(self.group_icons.clone(), |d, g| d.child(g))
                    .child(tabs)
                    .child(body),
            )
            .into_any_element()
    }

    // ---- bans

    fn load_bans(&mut self, cx: &mut Context<Self>) {
        ask(&self.model, self.con, Query::Bans, cx, |this: &mut Self, r, _| {
            this.bans = Some(r.and_then(|a| match a {
                Answer::Bans(b) => Ok(b),
                _ => Err("unexpected answer".into()),
            }));
        });
    }

    fn bans_tab(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let mut durations = h_flex().gap_2().flex_wrap();
        for (i, (text, secs)) in
            [("1 hour", Some(3600)), ("1 day", Some(86_400)), ("1 week", Some(604_800)), ("30 days", Some(2_592_000)), ("Permanent", None)]
                .into_iter()
                .enumerate()
        {
            durations = durations.child(chip(("bd", i), text, self.ban_secs == secs).on_click(cx.listener(move |t, _, _, cx| {
                t.ban_secs = secs;
                cx.notify();
            })));
        }
        let form = v_flex()
            .gap_2()
            .p_4()
            .rounded(px(6.))
            .border_1()
            .border_color(border())
            .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child("Add a ban"))
            .child(h_flex().gap_2().child(div().flex_1().child(Input::new(&self.ban_ip))).child(div().flex_1().child(Input::new(&self.ban_name))))
            .child(Input::new(&self.ban_uid))
            .child(Input::new(&self.ban_reason))
            .child(
                h_flex().gap_3().items_center().child(durations).child(div().flex_1()).child(
                    Button::new("ban-add").label("Ban").small().danger().on_click(cx.listener(|t, _, window, cx| {
                        let (ip, name, uid, reason) =
                            (value(&t.ban_ip, cx), value(&t.ban_name, cx), value(&t.ban_uid, cx), value(&t.ban_reason, cx));
                        if ip.is_empty() && name.is_empty() && uid.is_empty() {
                            t.error = Some("Give an IP, a name pattern or a unique id.".into());
                            cx.notify();
                            return;
                        }
                        t.error = None;
                        t.write(AdminWrite::AddBan { ip, name, uid, secs: t.ban_secs, reason }, cx);
                        for i in [&t.ban_ip, &t.ban_name, &t.ban_uid, &t.ban_reason] {
                            i.update(cx, |i, cx| i.set_value("", window, cx));
                        }
                        later(cx, |t: &mut Self, cx| t.load_bans(cx));
                    })),
                ),
            );
        let mut list = v_flex();
        match &self.bans {
            None => list = list.child(label("Loading bans...")),
            Some(Err(e)) => list = list.child(error_box(e)),
            Some(Ok(b)) if b.is_empty() => list = list.child(label("Nobody is banned.")),
            Some(Ok(b)) => {
                for ban in b {
                    let id = ban.id;
                    let who = [&ban.last_nickname, &ban.name, &ban.ip, &ban.uid].into_iter().find(|s| !s.is_empty()).cloned().unwrap_or_default();
                    let mut what = Vec::new();
                    if !ban.ip.is_empty() {
                        what.push(format!("IP {}", ban.ip));
                    }
                    if !ban.name.is_empty() {
                        what.push(format!("name {}", ban.name));
                    }
                    if !ban.uid.is_empty() {
                        what.push("unique id".to_string());
                    }
                    list = list.child(
                        h_flex()
                            .gap_3()
                            .items_center()
                            .py_2()
                            .border_b_1()
                            .border_color(rgba(0xffffff08))
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .child(div().text_sm().truncate().child(who))
                                    .child(label(format!(
                                        "{} · by {} · {}{}",
                                        what.join(", "),
                                        ban.by,
                                        ban.created,
                                        if ban.reason.is_empty() { String::new() } else { format!(" · {}", ban.reason) }
                                    )).truncate()),
                            )
                            .child(label(human(ban.secs)))
                            .child(icon_btn(("unban", id as usize), "x", MUTED, "Remove ban").on_click(cx.listener(
                                move |t, _, _, cx| {
                                    t.write(AdminWrite::DelBan { id }, cx);
                                    later(cx, |t: &mut Self, cx| t.load_bans(cx));
                                },
                            ))),
                    );
                }
            }
        }
        v_flex().gap_5().child(form).child(list).into_any_element()
    }

    // ---- keys

    fn load_keys(&mut self, cx: &mut Context<Self>) {
        ask(&self.model, self.con, Query::Tokens, cx, |this: &mut Self, r, _| {
            this.keys = Some(r.and_then(|a| match a {
                Answer::Tokens(t) => Ok(t),
                _ => Err("unexpected answer".into()),
            }));
        });
    }

    fn keys_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let _ = window;
        let (sgroups, cgroups, channels) = match self.server(cx) {
            Some(s) => (s.groups.clone(), s.channel_groups.clone(), s.channels.clone()),
            None => Default::default(),
        };
        let group_name = |server: bool, id: u64| {
            let l = if server { &sgroups } else { &cgroups };
            l.iter().find(|g| g.id == id).map(|g| g.badge.name.clone()).unwrap_or_else(|| format!("group {id}"))
        };
        let mut pick = h_flex().gap_2().flex_wrap();
        for g in &sgroups {
            let id = g.id;
            pick = pick.child(
                chip(SharedString::from(format!("ks-{id}")), g.badge.name.clone(), self.key_group == Some((true, id)))
                    .on_click(cx.listener(move |t, _, _, cx| {
                        t.key_group = Some((true, id));
                        cx.notify();
                    })),
            );
        }
        for g in &cgroups {
            let id = g.id;
            pick = pick.child(
                chip(SharedString::from(format!("kc-{id}")), format!("{} (channel)", g.badge.name), self.key_group == Some((false, id)))
                    .on_click(cx.listener(move |t, _, _, cx| {
                        t.key_group = Some((false, id));
                        cx.notify();
                    })),
            );
        }
        let mut chan_pick = h_flex().gap_2().flex_wrap();
        if matches!(self.key_group, Some((false, _))) {
            for c in channels.iter().filter(|c| !c.name.starts_with('[')) {
                let id = c.id;
                chan_pick = chan_pick.child(
                    chip(SharedString::from(format!("kch-{id}")), c.name.clone(), self.key_channel == Some(id)).on_click(
                        cx.listener(move |t, _, _, cx| {
                            t.key_channel = Some(id);
                            cx.notify();
                        }),
                    ),
                );
            }
        }
        let create = v_flex()
            .gap_3()
            .p_4()
            .rounded(px(6.))
            .border_1()
            .border_color(border())
            .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child("Create a key"))
            .child(label("Whoever uses the key joins this group."))
            .child(pick)
            .when(matches!(self.key_group, Some((false, _))), |d| d.child(label("In channel")).child(chan_pick))
            .child(
                h_flex().gap_2().child(div().flex_1().child(Input::new(&self.key_desc))).child(
                    Button::new("key-add").label("Create").small().primary().on_click(cx.listener(|t, _, _, cx| {
                        let Some((server_group, group)) = t.key_group else {
                            t.error = Some("Pick a group first.".into());
                            cx.notify();
                            return;
                        };
                        let channel = if server_group { 0 } else { t.key_channel.unwrap_or(0) };
                        if !server_group && channel == 0 {
                            t.error = Some("Pick the channel for the channel group.".into());
                            cx.notify();
                            return;
                        }
                        t.error = None;
                        let description = value(&t.key_desc, cx);
                        t.write(AdminWrite::AddToken { server_group, group, channel, description }, cx);
                        later(cx, |t: &mut Self, cx| t.load_keys(cx));
                    })),
                ),
            );
        let use_key = h_flex().gap_2().child(div().flex_1().child(Input::new(&self.key_use))).child(
            Button::new("key-use").label("Use key").small().on_click(cx.listener(|t, _, window, cx| {
                let token = value(&t.key_use, cx);
                if !token.is_empty() {
                    t.write(AdminWrite::UseToken { token }, cx);
                    t.key_use.update(cx, |i, cx| i.set_value("", window, cx));
                }
            })),
        );
        let mut list = v_flex();
        match &self.keys {
            None => list = list.child(label("Loading keys...")),
            Some(Err(e)) => list = list.child(error_box(e)),
            Some(Ok(k)) if k.is_empty() => list = list.child(label("No unused keys.")),
            Some(Ok(k)) => {
                for (i, key) in k.iter().enumerate() {
                    let (token, copy) = (key.token.clone(), key.token.clone());
                    let channel = channels.iter().find(|c| c.id == key.channel).map(|c| format!(" in {}", c.name)).unwrap_or_default();
                    list = list.child(
                        h_flex()
                            .gap_3()
                            .items_center()
                            .py_2()
                            .border_b_1()
                            .border_color(rgba(0xffffff08))
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .child(div().text_sm().truncate().child(format!("{}{channel}", group_name(key.server_group, key.group))))
                                    .child(label(format!(
                                        "{} · {}{}",
                                        key.token,
                                        key.created,
                                        if key.description.is_empty() { String::new() } else { format!(" · {}", key.description) }
                                    )).truncate()),
                            )
                            .child(icon_btn(("kcopy", i), "copy", MUTED, "Copy key").on_click(move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()));
                            }))
                            .child(icon_btn(("kdel", i), "trash", RED, "Delete key").on_click(cx.listener(move |t, _, _, cx| {
                                t.write(AdminWrite::DelToken { token: token.clone() }, cx);
                                later(cx, |t: &mut Self, cx| t.load_keys(cx));
                            }))),
                    );
                }
            }
        }
        v_flex().gap_5().child(create).child(use_key).child(list).into_any_element()
    }

    // ---- complaints

    fn load_complaints(&mut self, cx: &mut Context<Self>) {
        ask(&self.model, self.con, Query::Complaints, cx, |this: &mut Self, r, _| {
            this.complaints = Some(r.and_then(|a| match a {
                Answer::Complaints(c) => Ok(c),
                _ => Err("unexpected answer".into()),
            }));
        });
    }

    fn complaints_tab(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let mut list = v_flex();
        match &self.complaints {
            None => list = list.child(label("Loading complaints...")),
            Some(Err(e)) => list = list.child(error_box(e)),
            Some(Ok(c)) if c.is_empty() => list = list.child(label("No complaints.")),
            Some(Ok(c)) => {
                for (i, x) in c.iter().enumerate() {
                    let (target, from) = (x.target_db_id, x.from_db_id);
                    list = list.child(
                        h_flex()
                            .gap_3()
                            .items_center()
                            .py_2()
                            .border_b_1()
                            .border_color(rgba(0xffffff08))
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .child(div().text_sm().child(format!("{} about {}", x.from, x.target)))
                                    .child(label(format!("{} · {}", x.time, x.message))),
                            )
                            .child(icon_btn(("cdel", i), "x", MUTED, "Remove").on_click(cx.listener(move |t, _, _, cx| {
                                t.write(AdminWrite::DelComplaint { target, from }, cx);
                                later(cx, |t: &mut Self, cx| t.load_complaints(cx));
                            }))),
                    );
                }
            }
        }
        list.into_any_element()
    }

    // ---- server

    fn load_server(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (tx, rx) = oneshot::channel();
        self.model.read(cx).net.send(Cmd::ServerInfo { con: self.con, reply: tx });
        let window_handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            let Ok(info) = rx.await else { return };
            let _ = window_handle.update(cx, |_, window, cx| {
                let _ = this.update(cx, |t, cx| t.fill_server(info, window, cx));
            });
        })
        .detach();
        ask(&self.model, self.con, Query::ServerConnection, cx, |this: &mut Self, r, _| {
            this.connection = Some(r.and_then(|a| match a {
                Answer::ServerConnection(c) => Ok(c),
                _ => Err("unexpected answer".into()),
            }));
        });
        ask(&self.model, self.con, Query::TempPasswords, cx, |this: &mut Self, r, _| {
            this.temp = Some(r.and_then(|a| match a {
                Answer::TempPasswords(t) => Ok(t),
                _ => Err("unexpected answer".into()),
            }));
        });
    }

    fn fill_server(&mut self, info: ServerInfo, window: &mut Window, cx: &mut Context<Self>) {
        let vals: [(&str, String); 12] = [
            ("name", info.name.clone()),
            ("phonetic_name", info.phonetic_name.clone()),
            ("welcome_message", info.welcome_message.clone()),
            ("max_clients", info.max_clients.to_string()),
            ("reserved_slots", String::new()),
            ("password", String::new()),
            ("hostmessage", info.hostmessage.clone()),
            ("hostbanner_url", info.hostbanner_url.clone()),
            ("hostbanner_gfx_url", info.hostbanner_gfx_url.clone()),
            ("hostbutton_tooltip", info.hostbutton_tooltip.clone()),
            ("hostbutton_url", info.hostbutton_url.clone()),
            ("temp_delete_delay", info.temp_delete_delay.to_string()),
        ];
        for (k, v) in vals {
            if let Some(i) = self.srv.get(k) {
                i.update(cx, |i, cx| i.set_value(v, window, cx));
            }
        }
        self.host_mode = info.hostmessage_mode;
        self.info = Some(info);
        cx.notify();
    }

    fn save_server(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(info) = self.info.clone() else { return };
        let v = |k: &str| self.srv.get(k).map(|i| value(i, cx)).unwrap_or_default();
        let changed = |new: String, old: &str| (new != old).then_some(new);
        let num = |k: &str| v(k).parse::<u64>().ok();
        let p = ServerProps {
            name: changed(v("name"), &info.name),
            phonetic_name: changed(v("phonetic_name"), &info.phonetic_name),
            welcome_message: changed(v("welcome_message"), &info.welcome_message),
            max_clients: num("max_clients").map(|n| n as u16).filter(|n| *n != info.max_clients),
            reserved_slots: num("reserved_slots").map(|n| n as u16),
            password: match v("password").as_str() {
                "" => None,
                "-" => Some(String::new()),
                pw => Some(pw.to_string()),
            },
            hostmessage: changed(v("hostmessage"), &info.hostmessage),
            hostmessage_mode: (self.host_mode != info.hostmessage_mode).then_some(self.host_mode),
            hostbanner_url: changed(v("hostbanner_url"), &info.hostbanner_url),
            hostbanner_gfx_url: changed(v("hostbanner_gfx_url"), &info.hostbanner_gfx_url),
            hostbutton_tooltip: changed(v("hostbutton_tooltip"), &info.hostbutton_tooltip),
            hostbutton_url: changed(v("hostbutton_url"), &info.hostbutton_url),
            temp_delete_delay: num("temp_delete_delay").filter(|d| *d != info.temp_delete_delay),
            ..Default::default()
        };
        if p == ServerProps::default() {
            self.error = Some("Nothing changed.".into());
            cx.notify();
            return;
        }
        self.error = None;
        self.write(AdminWrite::EditServer(Box::new(p)), cx);
        let handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(600)).await;
            let _ = handle.update(cx, |_, window, cx| {
                let _ = this.update(cx, |t, cx| t.load_server(window, cx));
            });
        })
        .detach();
    }

    fn server_tab(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let Some(info) = self.info.clone() else { return label("Loading...").into_any_element() };
        let row = |text: &'static str, el: AnyElement| {
            h_flex()
                .gap_4()
                .items_center()
                .min_h(px(32.))
                .child(div().w(px(190.)).flex_none().text_sm().text_color(rgb(MUTED)).child(text))
                .child(div().flex_1().min_w_0().child(el))
        };
        let mut form = v_flex().gap_2();
        for (key, text, _) in SERVER_FIELDS {
            form = form.child(row(text, Input::new(&self.srv[key]).into_any_element()));
            if key == "hostmessage" {
                let mut modes = h_flex().gap_2();
                for (i, m) in ["Off", "In the log", "Popup", "Popup, then disconnect"].into_iter().enumerate() {
                    let i = i as u8;
                    modes = modes.child(chip(("hm", i as usize), m, self.host_mode == i).on_click(cx.listener(move |t, _, _, cx| {
                        t.host_mode = i;
                        cx.notify();
                    })));
                }
                form = form.child(row("Host message shows", modes.into_any_element()));
            }
        }
        let mut temp = v_flex();
        match &self.temp {
            None => temp = temp.child(label("Loading...")),
            Some(Err(e)) => temp = temp.child(error_box(e)),
            Some(Ok(t)) if t.is_empty() => temp = temp.child(label("No temporary passwords.")),
            Some(Ok(t)) => {
                for (i, p) in t.iter().enumerate() {
                    let pw = p.password.clone();
                    temp = temp.child(
                        h_flex()
                            .gap_3()
                            .items_center()
                            .py_1p5()
                            .child(div().flex_1().text_sm().child(format!("{} · {}", p.password, p.description)))
                            .child(label(format!("until {} · {}", p.ends, p.by)))
                            .child(icon_btn(("tpd", i), "x", MUTED, "Remove").on_click(cx.listener(move |t, _, window, cx| {
                                t.write(AdminWrite::DelTempPassword { password: pw.clone() }, cx);
                                let handle = window.window_handle();
                                cx.spawn(async move |this, cx| {
                                    cx.background_executor().timer(Duration::from_millis(400)).await;
                                    let _ = handle.update(cx, |_, window, cx| {
                                        let _ = this.update(cx, |t, cx| t.load_server(window, cx));
                                    });
                                })
                                .detach();
                            }))),
                    );
                }
            }
        }
        let stat = |name: &'static str, value: String| {
            h_flex()
                .justify_between()
                .py_1()
                .border_b_1()
                .border_color(rgba(0xffffff08))
                .child(label(name))
                .child(div().text_sm().child(value))
        };
        let bytes = |n: u64| match n {
            n if n >= 1 << 30 => format!("{:.1} GB", n as f64 / (1u64 << 30) as f64),
            n if n >= 1 << 20 => format!("{:.1} MB", n as f64 / (1u64 << 20) as f64),
            n => format!("{:.0} KB", n as f64 / 1024.0),
        };
        let connection = match &self.connection {
            None => label("Loading...").into_any_element(),
            Some(Err(e)) => error_box(e),
            Some(Ok(c)) => v_flex()
                .child(stat("Uptime", fmt_duration(c.uptime_secs)))
                .child(stat("Ping (us to the server)", format!("{:.0} ms", c.ping_ms)))
                .child(stat("Packet loss", format!("{:.1}%", c.packet_loss)))
                .child(stat("Bandwidth now (out / in)", format!("{} / {} per second", bytes(c.sent_per_sec), bytes(c.received_per_sec))))
                .child(stat("Last minute (out / in)", format!("{} / {} per second", bytes(c.sent_per_min), bytes(c.received_per_min))))
                .child(stat("Since start (out / in)", format!("{} / {}", bytes(c.sent_total), bytes(c.received_total))))
                .child(stat("File transfers (out / in)", format!("{} / {}", bytes(c.files_sent_total), bytes(c.files_received_total))))
                .into_any_element(),
        };
        v_flex()
            .gap_6()
            .max_w(px(760.))
            .child(label(format!("{} {} · created {}", info.platform, info.version, info.created)))
            .child(v_flex().gap_2().child(section_title("Connection")).child(connection))
            .child(form)
            .child(
                h_flex().justify_end().child(
                    Button::new("srv-save").label("Save").small().primary().on_click(cx.listener(|t, _, window, cx| t.save_server(window, cx))),
                ),
            )
            .child(
                v_flex()
                    .gap_3()
                    .child(section_title("Temporary passwords"))
                    .child(label("Extra server passwords that expire after a day; handy for guests."))
                    .child(
                        h_flex()
                            .gap_2()
                            .child(div().flex_1().child(Input::new(&self.temp_pw)))
                            .child(div().flex_1().child(Input::new(&self.temp_desc)))
                            .child(Button::new("tp-add").label("Add").small().on_click(cx.listener(|t, _, window, cx| {
                                let (password, description) = (value(&t.temp_pw, cx), value(&t.temp_desc, cx));
                                if password.is_empty() {
                                    return;
                                }
                                t.write(AdminWrite::AddTempPassword { password, description, secs: 86_400 }, cx);
                                t.temp_pw.update(cx, |i, cx| i.set_value("", window, cx));
                                t.temp_desc.update(cx, |i, cx| i.set_value("", window, cx));
                                let handle = window.window_handle();
                                cx.spawn(async move |this, cx| {
                                    cx.background_executor().timer(Duration::from_millis(400)).await;
                                    let _ = handle.update(cx, |_, window, cx| {
                                        let _ = this.update(cx, |t, cx| t.load_server(window, cx));
                                    });
                                })
                                .detach();
                            }))),
                    )
                    .child(temp),
            )
            .into_any_element()
    }

    // ---- overview

    fn overview_tab(&mut self, cx: &mut Context<Self>) -> AnyElement {
        use crate::net::admin::OverviewRow;
        let rows = match &self.overview {
            None => return label("Loading...").into_any_element(),
            Some(Err(e)) => return error_box(e),
            Some(Ok(r)) => r.clone(),
        };
        let s = self.server(cx).cloned();
        let source_name = |r: &OverviewRow| -> String {
            let Some(s) = &s else { return r.source.to_string() };
            let group = |l: &[crate::net::ServerGroupInfo], id: u64| l.iter().find(|g| g.id == id).map(|g| g.badge.name.clone());
            match r.source {
                "server group" => group(&s.groups, r.id1).unwrap_or_else(|| format!("server group {}", r.id1)),
                "channel group" => group(&s.channel_groups, r.id2).map(|g| format!("{g} (channel)")).unwrap_or_else(|| format!("channel group {}", r.id2)),
                "channel" => s.channels.iter().find(|c| c.id == r.id1).map(|c| format!("channel {}", c.name)).unwrap_or_else(|| "channel".into()),
                "client" => "their own".into(),
                _ => "their own in this channel".into(),
            }
        };
        // TeamSpeak's order: server groups (highest wins, negated lowest),
        // then their own, then channel group and channel client unless skipped
        let effective = |list: &[&OverviewRow]| -> Option<i32> {
            let groups: Vec<&&OverviewRow> = list.iter().filter(|r| r.source == "server group").collect();
            let mut value = if groups.iter().any(|r| r.negated) {
                groups.iter().filter(|r| r.negated).map(|r| r.value).min()
            } else {
                groups.iter().map(|r| r.value).max()
            };
            let mut skip = groups.iter().any(|r| r.skip);
            if let Some(c) = list.iter().find(|r| r.source == "client") {
                value = Some(c.value);
                skip |= c.skip;
            }
            if !skip {
                for src in ["channel group", "channel client"] {
                    if let Some(c) = list.iter().find(|r| r.source == src) {
                        value = Some(c.value);
                    }
                }
            }
            value
        };
        let q = value(&self.overview_search, cx).to_lowercase();
        let names: std::collections::HashMap<u32, &crate::net::admin::PermInfo> = permissions().iter().map(|p| (p.id, p)).collect();
        let mut perms: Vec<u32> = rows.iter().map(|r| r.perm).filter(|p| *p < 0x8000).collect();
        perms.sort_unstable();
        perms.dedup();
        let mut list = v_flex();
        for perm in perms {
            let Some(info) = names.get(&perm) else { continue };
            if !q.is_empty() && !info.name.contains(&q) && !info.description.to_lowercase().contains(&q) {
                continue;
            }
            let here: Vec<&OverviewRow> = rows.iter().filter(|r| r.perm == perm && r.source != "channel").collect();
            let channel_rows: Vec<&OverviewRow> = rows.iter().filter(|r| r.perm == perm && r.source == "channel").collect();
            let eff = effective(&here);
            let sources: Vec<String> = here
                .iter()
                .chain(channel_rows.iter())
                .map(|r| {
                    let mut t = format!("{} {}", source_name(r), r.value);
                    if r.negated {
                        t.push_str(" negated");
                    }
                    if r.skip {
                        t.push_str(" skip");
                    }
                    t
                })
                .collect();
            list = list.child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .min_h(px(40.))
                    .py_1()
                    .border_b_1()
                    .border_color(rgba(0xffffff08))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().text_sm().truncate().child(info.name.clone()))
                            .child(label(sources.join(" · ")).truncate()),
                    )
                    .child(div().w(px(80.)).text_sm().text_right().child(eff.map(|v| v.to_string()).unwrap_or_else(|| "–".into()))),
            );
        }
        v_flex()
            .gap_3()
            .child(label("Every permission that applies to them here, where it comes from, and the value that wins."))
            .child(Input::new(&self.overview_search).cleanable(true))
            .child(h_flex().gap_3().child(div().flex_1().child(label("Permission"))).child(div().w(px(80.)).text_right().child(label("Effective"))))
            .child(list)
            .into_any_element()
    }

    // ---- inbox

    fn load_inbox(&mut self, cx: &mut Context<Self>) {
        ask(&self.model, self.con, Query::Inbox, cx, |this: &mut Self, r, _| {
            this.inbox = Some(r.and_then(|a| match a {
                Answer::Inbox(mut l) => {
                    l.sort_by(|a, b| b.time.cmp(&a.time));
                    Ok(l)
                }
                _ => Err("unexpected answer".into()),
            }));
        });
    }

    fn open_message(&mut self, id: u32, cx: &mut Context<Self>) {
        ask(&self.model, self.con, Query::InboxMessage(id), cx, move |this: &mut Self, r, cx| match r {
            Ok(Answer::Inbox(mut l)) if !l.is_empty() => {
                this.open_message = Some(l.remove(0));
                this.write(AdminWrite::MarkOffline { id, read: true }, cx);
                if let Some(Ok(list)) = &mut this.inbox {
                    if let Some(m) = list.iter_mut().find(|m| m.id == id) {
                        m.read = true;
                    }
                }
            }
            Ok(_) => this.error = Some("That message is gone.".into()),
            Err(e) => this.error = Some(e),
        });
    }

    /// A unique id as a name, when they're online.
    fn who(&self, uid: &str, cx: &App) -> String {
        self.server(cx)
            .and_then(|s| s.channels.iter().flat_map(|c| &c.clients).find(|c| c.uid == uid))
            .map(|c| c.name.clone())
            .unwrap_or_else(|| uid.chars().take(12).collect::<String>() + "…")
    }

    fn inbox_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let _ = window;
        let compose = v_flex()
            .gap_2()
            .p_4()
            .rounded(px(6.))
            .border_1()
            .border_color(border())
            .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child("New offline message"))
            .child(label("They get it the next time they connect, even if they're offline now."))
            .child(Input::new(&self.compose_to))
            .child(Input::new(&self.compose_subject))
            .child(gpui_kit::component::input::Textarea::new(&self.compose_text))
            .child(
                h_flex()
                    .gap_2()
                    .justify_end()
                    .child(Button::new("cmp-cancel").label("Cancel").small().ghost().on_click(cx.listener(|t, _, _, cx| {
                        t.composing = false;
                        cx.notify();
                    })))
                    .child(Button::new("cmp-send").label("Send").small().primary().on_click(cx.listener(|t, _, window, cx| {
                        let (uid, subject) = (value(&t.compose_to, cx), value(&t.compose_subject, cx));
                        let message = t.compose_text.read(cx).value().trim().to_string();
                        if uid.is_empty() || message.is_empty() {
                            t.error = Some("Needs a unique id and a message.".into());
                            cx.notify();
                            return;
                        }
                        t.error = None;
                        let subject =
                            if subject.is_empty() { crate::net::strip_bbcode(&message).chars().take(40).collect() } else { subject };
                        t.write(AdminWrite::SendOffline { uid, subject, message }, cx);
                        t.compose_text.update(cx, |i, cx| i.set_value("", window, cx));
                        t.composing = false;
                        cx.notify();
                    }))),
            );
        let mut list = v_flex();
        match &self.inbox {
            None => list = list.child(label("Loading...")),
            Some(Err(e)) => list = list.child(error_box(e)),
            Some(Ok(l)) if l.is_empty() => list = list.child(label("No offline messages.")),
            Some(Ok(l)) => {
                for (i, m) in l.iter().enumerate() {
                    let (id, from, from_uid) = (m.id, self.who(&m.from_uid, cx), m.from_uid.clone());
                    let open = self.open_message.as_ref().filter(|o| o.id == m.id).cloned();
                    list = list.child(
                        v_flex()
                            .border_b_1()
                            .border_color(rgba(0xffffff08))
                            .child(
                                h_flex()
                                    .id(("msg", i))
                                    .gap_3()
                                    .items_center()
                                    .py_2()
                                    .cursor_pointer()
                                    .hover(|d| d.bg(hover_bg()))
                                    .on_click(cx.listener(move |t, _, _, cx| {
                                        if t.open_message.as_ref().is_some_and(|o| o.id == id) {
                                            t.open_message = None;
                                            cx.notify();
                                        } else {
                                            t.open_message(id, cx);
                                        }
                                    }))
                                    .child(div().size(px(6.)).rounded_full().when(!m.read, |d| d.bg(rgb(ACCENT))))
                                    .child(
                                        v_flex()
                                            .flex_1()
                                            .min_w_0()
                                            .child(
                                                div()
                                                    .text_sm()
                                                    .truncate()
                                                    .when(!m.read, |d| d.font_weight(FontWeight::SEMIBOLD))
                                                    .child(if m.subject.is_empty() { "(no subject)".to_string() } else { m.subject.clone() }),
                                            )
                                            .child(label(format!("{from} · {}", m.time))),
                                    )
                                    .child(icon_btn(("mreply", i), "reply", MUTED, "Reply").on_click(cx.listener(
                                        move |t, _, window, cx| {
                                            cx.stop_propagation();
                                            t.compose_to.update(cx, |i, cx| i.set_value(from_uid.clone(), window, cx));
                                            t.composing = true;
                                            cx.notify();
                                        },
                                    )))
                                    .child(icon_btn(("mdel", i), "trash", RED, "Delete").on_click(cx.listener(move |t, _, _, cx| {
                                        cx.stop_propagation();
                                        t.write(AdminWrite::DelOffline { id }, cx);
                                        if let Some(Ok(l)) = &mut t.inbox {
                                            l.retain(|m| m.id != id);
                                        }
                                        cx.notify();
                                    }))),
                            )
                            .when_some(open, |d, o| {
                                d.child(
                                    div()
                                        .pl_5()
                                        .pb_3()
                                        .text_sm()
                                        .text_color(rgb(0xc4c8ce))
                                        .child(super::bbcode::parse(&o.message).element(("msg-body", i))),
                                )
                            }),
                    );
                }
            }
        }
        v_flex()
            .gap_4()
            .max_w(px(760.))
            .child(
                h_flex()
                    .gap_2()
                    .justify_end()
                    .child(Button::new("inbox-reload").label("Reload").small().ghost().on_click(cx.listener(|t, _, _, cx| t.load_inbox(cx))))
                    .when(!self.composing, |d| {
                        d.child(Button::new("inbox-new").label("New message").small().primary().on_click(cx.listener(|t, _, _, cx| {
                            t.composing = true;
                            cx.notify();
                        })))
                    }),
            )
            .when(self.composing, |d| d.child(compose))
            .child(list)
            .into_any_element()
    }

    // ---- log

    fn load_log(&mut self, cx: &mut Context<Self>) {
        ask(&self.model, self.con, Query::Log { lines: 100 }, cx, |this: &mut Self, r, _| {
            this.log = Some(r.and_then(|a| match a {
                Answer::Log(l) => Ok(l),
                _ => Err("unexpected answer".into()),
            }));
        });
    }

    fn log_tab(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let mut col = v_flex().gap_1();
        match &self.log {
            None => col = col.child(label("Loading the log...")),
            Some(Err(e)) => col = col.child(error_box(e)),
            Some(Ok(lines)) => {
                for l in lines {
                    // 2026-10-17 00:42:30.751959|INFO    |PktHandler    |1  |Dropping client ...
                    let parts: Vec<&str> = l.splitn(5, '|').collect();
                    let (time, level, what, text) = match parts.as_slice() {
                        [t, lv, w, _, x] => (t.get(..16).unwrap_or(t), lv.trim(), w.trim(), *x),
                        _ => ("", "", "", l.as_str()),
                    };
                    let color = match level {
                        "ERROR" | "CRITICAL" => RED,
                        "WARNING" => AMBER,
                        _ => MUTED,
                    };
                    col = col.child(
                        h_flex()
                            .gap_3()
                            .items_start()
                            .child(div().w(px(120.)).flex_none().text_xs().text_color(rgb(DIM)).child(time.to_string()))
                            .child(div().w(px(100.)).flex_none().text_xs().text_color(rgb(color)).child(what.to_string()))
                            .child(div().flex_1().text_xs().text_color(rgb(0xc4c8ce)).child(text.to_string())),
                    );
                }
            }
        }
        v_flex()
            .gap_3()
            .child(h_flex().justify_end().child(
                Button::new("log-reload").label("Reload").small().ghost().on_click(cx.listener(|t, _, _, cx| t.load_log(cx))),
            ))
            .child(col)
            .into_any_element()
    }

    fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let server = self.server(cx).map(|s| if s.server_name.is_empty() { s.label.clone() } else { s.server_name.clone() });
        let mut col = v_flex()
            .w(px(210.))
            .flex_none()
            .h_full()
            .bg(rgb(SIDEBAR))
            .border_r_1()
            .border_color(border())
            .child(header_row().child(div().text_sm().font_weight(FontWeight::SEMIBOLD).truncate().child(server.unwrap_or_else(|| "Server".into()))))
            .child(div().h(px(8.)));
        let mut tabs = vec![
            AdminTab::ServerGroups,
            AdminTab::ChannelGroups,
            AdminTab::Bans,
            AdminTab::Keys,
            AdminTab::Complaints,
            AdminTab::Server,
            AdminTab::Log,
            AdminTab::Inbox(None),
        ];
        if let AdminTab::Perms(..) | AdminTab::Files(..) | AdminTab::Overview(..) = &self.tab {
            tabs.push(self.tab.clone());
        }
        for (i, tab) in tabs.into_iter().enumerate() {
            let on = self.tab == tab || (matches!(self.tab, AdminTab::Inbox(_)) && matches!(tab, AdminTab::Inbox(_)));
            let (title, ic) = (tab.title(), tab.icon());
            col = col.child(
                h_flex()
                    .id(("atab", i))
                    .h(px(32.))
                    .mx_2()
                    .gap_3()
                    .px_3()
                    .rounded(px(6.))
                    .cursor_pointer()
                    .items_center()
                    .when(on, |d| d.bg(active_bg()))
                    .hover(|d| d.bg(hover_bg()))
                    .on_click(cx.listener(move |t, _, window, cx| t.switch(tab.clone(), window, cx)))
                    .child(icon(ic, 15., if on { TEXT } else { MUTED }))
                    .child(div().text_sm().truncate().text_color(rgb(if on { TEXT } else { 0xb3b8c0 })).child(title)),
            );
        }
        col
    }
}

impl Render for AdminView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let connected = self.server(cx).is_some();
        let body = if !connected {
            label("Not connected to this server anymore.").into_any_element()
        } else {
            match self.tab.clone() {
                AdminTab::ServerGroups | AdminTab::ChannelGroups => self.groups_tab(cx),
                AdminTab::Bans => self.bans_tab(cx),
                AdminTab::Keys => self.keys_tab(window, cx),
                AdminTab::Complaints => self.complaints_tab(cx),
                AdminTab::Server => self.server_tab(cx),
                AdminTab::Log => self.log_tab(cx),
                AdminTab::Perms(..) => match &self.perms {
                    Some(p) => p.clone().into_any_element(),
                    None => label("Loading...").into_any_element(),
                },
                AdminTab::Inbox(_) => self.inbox_tab(window, cx),
                AdminTab::Overview(..) => self.overview_tab(cx),
                AdminTab::Files(..) => match &self.files {
                    Some(f) => f.clone().into_any_element(),
                    None => label("Loading...").into_any_element(),
                },
            }
        };
        h_flex()
            .size_full()
            .bg(rgb(BG))
            .text_color(rgb(TEXT))
            .font_family(FONT)
            .child(self.sidebar(cx))
            .child(
                v_flex()
                    .flex_1()
                    .h_full()
                    .min_w_0()
                    .child(header_row().child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child(self.tab.title())))
                    .child(
                        div()
                            .id("admin-body")
                            .flex_1()
                            .overflow_y_scroll()
                            .px_6()
                            .py_5()
                            .child(
                                v_flex()
                                    .gap_4()
                                    .when_some(self.error.clone(), |d, e| d.child(error_box(&e)))
                                    .child(body),
                            ),
                    ),
            )
    }
}
