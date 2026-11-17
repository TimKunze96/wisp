//! Server admin window: groups (with permissions and members), bans,
//! privilege keys, complaints, server settings, the server log, and a
//! permission editor for channels and clients.

mod groups;
mod perms;
mod bans;
mod keys;
mod complaints;
mod server;
mod overview;
mod inbox;
mod log;

use perms::PermEditor;
use std::collections::HashMap;
use std::time::Duration;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{Sizable, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{
    AnyElement, App, AppContext, ClipboardItem, Context, Entity, FontWeight, InteractiveElement, IntoElement,
    ParentElement, Render, SharedString, StatefulInteractiveElement, Styled, Window, div, px, rgb,
};
use tokio::sync::oneshot;

use super::Model;
use super::form;
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
    /// Whether these rights open the page. Files and the inbox are for
    /// everyone; the server checks what's inside.
    pub fn allowed(&self, r: &crate::net::Rights) -> bool {
        match self {
            AdminTab::ServerGroups => r.server_groups,
            AdminTab::ChannelGroups => r.channel_groups,
            AdminTab::Bans => r.bans,
            AdminTab::Keys => r.keys,
            AdminTab::Complaints => r.complaints,
            AdminTab::Server => r.edit_server || r.temp_passwords || r.connection_info,
            AdminTab::Log => r.log,
            AdminTab::Perms(..) | AdminTab::Overview(..) => r.permissions,
            AdminTab::Files(..) | AdminTab::Inbox(_) => true,
        }
    }

    /// The first admin page these rights open, if any.
    pub fn first(r: &crate::net::Rights) -> Option<AdminTab> {
        [
            AdminTab::Server,
            AdminTab::ServerGroups,
            AdminTab::ChannelGroups,
            AdminTab::Keys,
            AdminTab::Bans,
            AdminTab::Complaints,
            AdminTab::Log,
        ]
        .into_iter()
        .find(|t| t.allowed(r))
    }

    /// What the page is about, under its title.
    fn about(&self) -> Option<&'static str> {
        Some(match self {
            AdminTab::ServerGroups => "Roles across the whole server, and what each may do",
            AdminTab::ChannelGroups => "Roles inside a channel, like channel admin",
            AdminTab::Bans => "Who can't come in, and until when",
            AdminTab::Keys => "One-time tokens that hand out a role",
            AdminTab::Complaints => "What people reported about others",
            AdminTab::Server => "Name, welcome message, limits and passwords",
            AdminTab::Log => "What happened on the server lately",
            AdminTab::Inbox(_) => "Messages people left for you",
            AdminTab::Perms(..) => "Every permission and its value here",
            AdminTab::Files(..) => "Upload, download and tidy up",
            AdminTab::Overview(..) => "What applies to them here, and which role it comes from",
        })
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
    form::notice("triangle-alert", colors().red, e.to_string()).into_any_element()
}

fn human(secs: Option<u64>) -> String {
    match secs {
        None => "Permanent".into(),
        Some(s) if s % 86_400 == 0 => format!("{} d", s / 86_400),
        Some(s) if s % 3600 == 0 => format!("{} h", s / 3600),
        Some(s) => format!("{} min", s.div_ceil(60)),
    }
}

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

    fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let server = self.server(cx).map(|s| if s.server_name.is_empty() { s.label.clone() } else { s.server_name.clone() });
        let mut col = v_flex()
            .w(px(210.))
            .flex_none()
            .h_full()
            .bg(rgb(colors().sidebar))
            .border_r_1()
            .border_color(border())
            .child(header_row().child(
                h_flex()
                    .gap_2()
                    .min_w_0()
                    .items_center()
                    .child(icon("server", 14., colors().muted))
                    .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).truncate().child(server.unwrap_or_else(|| "Server".into()))),
            ));
        let rights = self.server(cx).map(|s| s.rights.clone()).unwrap_or_default();
        let mut sections: Vec<(&'static str, Vec<AdminTab>)> = vec![
            ("Roles", vec![AdminTab::ServerGroups, AdminTab::ChannelGroups, AdminTab::Keys]),
            ("Moderation", vec![AdminTab::Bans, AdminTab::Complaints, AdminTab::Log]),
            ("Server", vec![AdminTab::Server, AdminTab::Inbox(None)]),
        ];
        if let AdminTab::Perms(..) | AdminTab::Files(..) | AdminTab::Overview(..) = &self.tab {
            sections.push(("Open", vec![self.tab.clone()]));
        }
        let mut i: usize = 0;
        for (heading, tabs) in sections {
            let tabs: Vec<_> = tabs.into_iter().filter(|t| t.allowed(&rights)).collect();
            if tabs.is_empty() {
                continue;
            }
            col = col.child(form::nav_heading(heading));
            for tab in tabs {
                let on = self.tab == tab || (matches!(self.tab, AdminTab::Inbox(_)) && matches!(tab, AdminTab::Inbox(_)));
                let (title, ic) = (tab.title(), tab.icon());
                col = col.child(
                    form::nav_item(("atab", i), ic, title, on)
                        .on_click(cx.listener(move |t, _, window, cx| t.switch(tab.clone(), window, cx))),
                );
                i += 1;
            }
        }
        col
    }
}

impl AdminView {
    fn content(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rights = self.server(cx).map(|s| s.rights.clone());
        let body = if rights.is_none() {
            label("Not connected to this server anymore.").into_any_element()
        } else if !rights.as_ref().is_some_and(|r| self.tab.allowed(r)) {
            form::notice("lock-keyhole", colors().muted, "You can't manage this on this server.").into_any_element()
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
            .bg(rgb(colors().bg))
            .text_color(rgb(colors().text))
            .font_family(FONT)
            .child(self.sidebar(cx))
            .child(
                v_flex()
                    .flex_1()
                    .h_full()
                    .min_w_0()
                    .child(
                        div()
                            .id("admin-body")
                            .flex_1()
                            .overflow_y_scroll()
                            .px_8()
                            .pt_6()
                            .pb_10()
                            .child(
                                v_flex()
                                    .gap_5()
                                    .child(form::page_header(self.tab.icon(), self.tab.title(), self.tab.about().map(Into::into)))
                                    .when_some(self.error.clone(), |d, e| d.child(error_box(&e)))
                                    .child(body),
                            ),
                    ),
            )
    }
}

impl Render for AdminView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let title = match self.server(cx) {
            Some(s) if !s.server_name.is_empty() => format!("Server admin · {}", s.server_name),
            _ => "Server admin".to_string(),
        };
        let content = self.content(window, cx).into_any_element();
        super::frame::framed(title, content, window, cx)
    }
}
