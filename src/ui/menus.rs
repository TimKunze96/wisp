//! Right-click menus for users and channels, and the small prompt dialog
//! some actions need (message text, kick/ban reason, channel name...).

use gpui_kit::component::Icon;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::component::{Sizable, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{
    AnyElement, App, AppContext, ClipboardItem, Context, Entity, FontWeight, InteractiveElement, IntoElement,
    ParentElement, Styled, WeakEntity, Window, div, px, rgb, rgba,
};

use super::Model;
use super::main_view::MainView;
use super::widgets::*;
use crate::config::{WhisperList, WhisperTarget};
use crate::net::admin::PermTarget;
use crate::net::{AdminAction, ChannelProps, SelfAction, ChannelRow, ClientRow, Cmd, ConId, GroupBadge, ServerGroupInfo};

const MENU_WIDTH: f32 = 250.;

fn lucide(name: &str, color: u32) -> Icon {
    // the menu puts icons right against the label; give them some air
    Icon::empty().path(format!("icons/{name}.svg")).text_color(rgb(color)).mr(px(6.))
}

/// A roomier menu row. The icon goes in the menu's own icon column, so every
/// label (submenus included) starts at the same x. Labels stay white; only a
/// red icon marks the destructive ones.
fn row(icon_name: &'static str, label: impl Into<String>, color: u32) -> PopupMenuItem {
    let label = label.into();
    PopupMenuItem::element(move |_, _| {
        div()
            .py(px(5.))
            .text_sm()
            .text_color(rgb(TEXT))
            .child(label.clone())
    })
    .icon(lucide(icon_name, if color == RED { RED } else { MUTED }))
}

/// A group row: the group's own icon (or a placeholder) and its name.
fn group_row(badge_info: GroupBadge, checked: bool) -> PopupMenuItem {
    PopupMenuItem::element(move |_, _| {
        h_flex()
            .py(px(5.))
            .pl(px(6.))
            .gap(px(10.))
            .items_center()
            .child(
                div()
                    .w(px(16.))
                    .flex()
                    .justify_center()
                    .child(badge(&badge_info).map(IntoElement::into_any_element).unwrap_or_else(|| {
                        div().size(px(6.)).rounded_full().bg(rgba(0xffffff33)).into_any_element()
                    })),
            )
            .child(div().text_sm().child(badge_info.name.clone()))
    })
    .checked(checked)
}

#[derive(Clone, Copy, Debug)]
pub enum PromptKind {
    Poke,
    KickChannel,
    KickServer,
    Ban(Option<u64>),
    RenameChannel,
    SetTopic,
    SetPassword,
    DeleteChannel,
    /// Password to join a channel (`Prompt::key` is the bookmark id).
    JoinChannel { wrong: bool },
    /// Server password for a bookmark (`Prompt::key`).
    ServerPassword,
    Away,
    TalkRequest,
    Description,
    /// `Prompt::target` is the client's database id here.
    Complain,
    /// Confirm moving a channel with people in it (`Prompt::key` says how many).
    MoveChannel { parent: u64, after: u64 },
}

pub struct Prompt {
    pub kind: PromptKind,
    pub con: ConId,
    /// Client id or channel id, depending on the kind.
    pub target: u64,
    pub name: String,
    /// Bookmark id for the password prompts.
    pub key: String,
    pub input: Entity<InputState>,
}

impl PromptKind {
    fn title(self, name: &str) -> String {
        match self {
            PromptKind::Poke => format!("Poke {name}"),
            PromptKind::KickChannel => format!("Kick {name} from the channel"),
            PromptKind::KickServer => format!("Kick {name} from the server"),
            PromptKind::Ban(Some(s)) => format!("Ban {name} for {}", crate::net::session_human_secs(s)),
            PromptKind::Ban(None) => format!("Ban {name} permanently"),
            PromptKind::RenameChannel => format!("Rename {name}"),
            PromptKind::SetTopic => format!("Topic of {name}"),
            PromptKind::SetPassword => format!("Password for {name}"),
            PromptKind::DeleteChannel => format!("Delete {name}?"),
            PromptKind::JoinChannel { wrong: false } => format!("{name} has a password"),
            PromptKind::JoinChannel { wrong: true } => format!("Wrong password for {name}"),
            PromptKind::ServerPassword => format!("{name} needs a password"),
            PromptKind::Away => "Set away".into(),
            PromptKind::TalkRequest => "Request talk power".into(),
            PromptKind::Description => "Your description".into(),
            PromptKind::Complain => format!("Complain about {name}"),
            PromptKind::MoveChannel { .. } => format!("Move {name}?"),
        }
    }
    fn placeholder(self) -> &'static str {
        match self {
            PromptKind::Poke => "Message",
            PromptKind::RenameChannel => "Channel name",
            PromptKind::SetTopic => "Topic",
            PromptKind::SetPassword => "Leave empty to remove the password",
            PromptKind::JoinChannel { .. } | PromptKind::ServerPassword => "Password",
            PromptKind::Away | PromptKind::TalkRequest => "Message (optional)",
            PromptKind::Description => "Shown in your details",
            PromptKind::Complain => "What happened",
            _ => "Reason (optional)",
        }
    }
    fn has_input(self) -> bool {
        !matches!(self, PromptKind::DeleteChannel | PromptKind::MoveChannel { .. })
    }
    fn danger(self) -> bool {
        matches!(
            self,
            PromptKind::KickChannel | PromptKind::KickServer | PromptKind::Ban(_) | PromptKind::DeleteChannel
        )
    }
    fn confirm_label(self) -> &'static str {
        match self {
            PromptKind::Poke => "Send",
            PromptKind::JoinChannel { .. } => "Join",
            PromptKind::ServerPassword => "Connect",
            PromptKind::Away => "Set away",
            PromptKind::TalkRequest => "Request",
            PromptKind::DeleteChannel => "Delete",
            PromptKind::MoveChannel { .. } => "Move",
            k if k.danger() => "Confirm",
            _ => "Save",
        }
    }
    fn action(self, target: u64, text: String) -> Option<AdminAction> {
        let client = target as u16;
        Some(match self {
            PromptKind::Poke => AdminAction::Poke { client, message: text },
            PromptKind::KickChannel => AdminAction::KickChannel { client, reason: text },
            PromptKind::KickServer => AdminAction::KickServer { client, reason: text },
            PromptKind::Ban(secs) => AdminAction::Ban { client, secs, reason: text },
            PromptKind::RenameChannel if text.is_empty() => return None,
            PromptKind::RenameChannel => edit(target, ChannelProps { name: Some(text), ..Default::default() }),
            PromptKind::SetTopic => edit(target, ChannelProps { topic: Some(text), ..Default::default() }),
            PromptKind::SetPassword => {
                let password = Some((!text.is_empty()).then_some(text));
                edit(target, ChannelProps { password, ..Default::default() })
            }
            PromptKind::DeleteChannel => AdminAction::DeleteChannel { channel: target },
            PromptKind::JoinChannel { .. }
            | PromptKind::ServerPassword
            | PromptKind::Away
            | PromptKind::TalkRequest
            | PromptKind::Description
            | PromptKind::Complain => return None,
            PromptKind::MoveChannel { parent, after } => AdminAction::MoveChannel { channel: target, parent, after },
        })
    }
}

fn edit(channel: u64, props: ChannelProps) -> AdminAction {
    AdminAction::EditChannel { channel, props }
}

impl MainView {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn open_prompt(
        &mut self,
        kind: PromptKind,
        con: ConId,
        target: u64,
        name: String,
        initial: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(kind.placeholder())
                .masked(matches!(
                    kind,
                    PromptKind::SetPassword | PromptKind::JoinChannel { .. } | PromptKind::ServerPassword
                ))
                .default_value(initial)
        });
        cx.subscribe_in(&input, window, |this, _, ev: &InputEvent, _, cx| {
            if let InputEvent::PressEnter { .. } = ev {
                this.submit_prompt(cx);
            }
        })
        .detach();
        if kind.has_input() {
            input.update(cx, |i, cx| i.focus(window, cx));
        }
        self.prompt = Some(Prompt { kind, con, target, name, key: String::new(), input });
        cx.notify();
    }

    pub(super) fn submit_prompt(&mut self, cx: &mut Context<Self>) {
        let Some(p) = self.prompt.take() else { return };
        let text = p.input.read(cx).value().trim().to_string();
        match p.kind {
            PromptKind::JoinChannel { .. } if !text.is_empty() => {
                let key = crate::config::channel_key(&p.key, p.target);
                let pw = text.clone();
                self.model.update(cx, |m, cx| m.update_settings(cx, |s| {
                    s.channel_passwords.insert(key, pw);
                }));
                let join = Cmd::Join { con: p.con, channel: p.target, password: Some(text.clone()) };
                self.model.read(cx).net.send(join);
            }
            PromptKind::ServerPassword => {
                let id = p.key.clone();
                let bookmark = self.model.update(cx, |m, cx| {
                    m.update_settings(cx, |s| {
                        if let Some(b) = s.bookmarks.iter_mut().find(|b| b.id == id) {
                            b.password = text.clone();
                        }
                    });
                    m.settings.bookmarks.iter().find(|b| b.id == id).cloned()
                });
                if let Some(b) = bookmark {
                    self.model.read(cx).net.send(Cmd::Connect(b));
                }
            }
            PromptKind::Away => self.model.read(cx).net.send(Cmd::Away(Some(text.clone()))),
            PromptKind::TalkRequest => {
                let action = SelfAction::TalkRequest(Some(text.clone()));
                self.model.read(cx).net.send(Cmd::Me { con: p.con, action });
            }
            PromptKind::Description => {
                let action = SelfAction::Description(text.clone());
                self.model.read(cx).net.send(Cmd::Me { con: p.con, action });
            }
            PromptKind::Complain if !text.is_empty() => {
                let write = crate::net::admin::AdminWrite::AddComplaint { db_id: p.target, message: text.clone() };
                self.model.read(cx).net.send(Cmd::Write { con: p.con, write });
            }
            _ => {}
        }
        if let Some(action) = p.kind.action(p.target, text) {
            self.model.read(cx).net.send(Cmd::Admin { con: p.con, action });
        }
        cx.notify();
    }

    pub(super) fn prompt_overlay(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let p = self.prompt.as_ref()?;
        let kind = p.kind;
        let ok = Button::new("prompt-ok").label(kind.confirm_label()).small();
        let ok = if kind.danger() { ok.danger() } else { ok.primary() };
        Some(
            div()
                .absolute()
                .inset_0()
                .bg(rgba(0x00000099))
                .flex()
                .items_center()
                .justify_center()
                .occlude()
                .child(
                    v_flex()
                        .w(px(400.))
                        .gap_4()
                        .p_5()
                        .rounded(px(8.))
                        .bg(rgb(PANEL))
                        .border_1()
                        .border_color(border())
                        .shadow_lg()
                        .child(div().font_weight(FontWeight::SEMIBOLD).child(kind.title(&p.name)))
                        .when(kind.has_input(), |d| d.child(Input::new(&p.input)))
                        .when(matches!(kind, PromptKind::MoveChannel { .. }), |d| {
                            d.child(div().text_sm().text_color(rgb(MUTED)).child(format!(
                                "{} in it or below will move along.",
                                if p.key == "1" { "1 person is".to_string() } else { format!("{} people are", p.key) }
                            )))
                        })
                        .when(matches!(kind, PromptKind::DeleteChannel), |d| {
                            d.child(
                                div()
                                    .text_sm()
                                    .text_color(rgb(MUTED))
                                    .child("Everyone in it gets moved out. This can't be undone."),
                            )
                        })
                        .child(
                            h_flex()
                                .gap_2()
                                .justify_end()
                                .child(Button::new("prompt-cancel").label("Cancel").small().ghost().on_click(
                                    cx.listener(|this, _, _, cx| {
                                        this.prompt = None;
                                        cx.notify();
                                    }),
                                ))
                                .child(ok.on_click(cx.listener(|this, _, _, cx| this.submit_prompt(cx)))),
                        ),
                )
                .into_any_element(),
        )
    }
}

/// Everything the user menu needs, cloned so the menu closure can be 'static.
#[derive(Clone)]
pub struct UserMenuData {
    pub con: ConId,
    pub row: ClientRow,
    pub own_channel: Option<u64>,
    pub their_channel: u64,
    pub groups: Vec<ServerGroupInfo>,
    pub channel_groups: Vec<ServerGroupInfo>,
}

type PromptFn = std::rc::Rc<dyn Fn(PromptKind, String, String, &mut Window, &mut App)>;

fn prompt_fn(view: &WeakEntity<MainView>, con: ConId, target: u64) -> PromptFn {
    let view = view.clone();
    std::rc::Rc::new(move |kind, name, initial, window: &mut Window, cx: &mut App| {
        let _ = view.update(cx, |v, cx| v.open_prompt(kind, con, target, name, initial, window, cx));
    })
}

pub fn user_menu(
    menu: PopupMenu,
    data: &UserMenuData,
    model: &Entity<Model>,
    view: &WeakEntity<MainView>,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let d = data.clone();
    let (con, id, name, uid) = (d.con, d.row.id, d.row.name.clone(), d.row.uid.clone());
    let send = {
        let model = model.clone();
        std::rc::Rc::new(move |action: AdminAction, cx: &mut App| model.read(cx).net.send(Cmd::Admin { con, action }))
    };
    let prompt = prompt_fn(view, con, u64::from(id));
    let ask = |kind: PromptKind| {
        let (p, name) = (prompt.clone(), name.clone());
        move |_: &gpui_kit::ClickEvent, w: &mut Window, cx: &mut App| p(kind, name.clone(), String::new(), w, cx)
    };

    let m = model.clone();
    let mut menu = menu.min_w(px(MENU_WIDTH)).label(name.clone()).item(row("user", "Show details", MUTED).on_click(
        move |_, _, cx| {
            m.update(cx, |m, cx| m.select(Some((con, id)), cx));
        },
    ));

    if !d.row.me {
        menu = menu
            .item(row("message-square", "Send message", MUTED).on_click({
                let (view, uid) = (view.clone(), uid.clone());
                move |_, w, cx| {
                    let uid = uid.clone();
                    let _ = view.update(cx, |v, cx| v.open_private(con, id, uid, w, cx));
                }
            }))
            .item(row("hand", "Poke...", MUTED).on_click(ask(PromptKind::Poke)));

        let (m, u, muted) = (model.clone(), uid.clone(), d.row.locally_muted);
        menu = menu.item(
            row(if muted { "volume-2" } else { "volume-x" }, if muted { "Unmute for me" } else { "Mute for me" }, MUTED)
                .on_click(move |_, _, cx| {
                    let u = u.clone();
                    m.update(cx, |m, cx| {
                        m.update_settings(cx, |s| {
                            if !s.user_muted.remove(&u) {
                                s.user_muted.insert(u);
                            }
                        })
                    });
                }),
        );
        let contact = |c: Option<crate::config::Contact>| {
            let (m, u) = (model.clone(), uid.clone());
            move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut App| {
                let u = u.clone();
                m.update(cx, |m, cx| {
                    m.update_settings(cx, |s| match c {
                        Some(c) => {
                            s.contacts.insert(u, c);
                        }
                        None => {
                            s.contacts.remove(&u);
                        }
                    })
                });
            }
        };
        use crate::config::Contact;
        menu = if d.row.friend {
            menu.item(row("star-off", "Remove friend", MUTED).on_click(contact(None)))
        } else {
            menu.item(row("star", "Add as friend", MUTED).on_click(contact(Some(Contact::Friend))))
        };
        menu = if d.row.blocked {
            menu.item(row("circle-check", "Unblock", MUTED).on_click(contact(None)))
        } else {
            menu.item(row("ban", "Block", MUTED).on_click(contact(Some(Contact::Blocked))))
        };
        if (d.row.volume - 1.0).abs() > 0.01 {
            let (m, u) = (model.clone(), uid.clone());
            menu = menu.item(row("rotate-ccw", "Reset volume", MUTED).on_click(move |_, _, cx| {
                let u = u.clone();
                m.update(cx, |m, cx| {
                    m.update_settings(cx, |s| {
                        s.user_volumes.remove(&u);
                    })
                });
            }));
        }

        let target = WhisperTarget::Client { uid: uid.clone(), name: name.clone() };
        menu = whisper_submenu(menu, target, model, window, cx);
        menu = menu.separator().label("Moderation");
        if let Some(own) = d.own_channel.filter(|c| *c != d.their_channel) {
            let s = send.clone();
            menu = menu.item(
                row("arrow-down-to-line", "Move to my channel", MUTED)
                    .on_click(move |_, _, cx| s(AdminAction::Move { client: id, channel: own }, cx)),
            );
        }
        let grant = !d.row.talk_power_granted;
        let s = send.clone();
        menu = menu.item(
            row("mic-vocal", if grant { "Grant talk power" } else { "Revoke talk power" }, MUTED)
                .on_click(move |_, _, cx| s(AdminAction::TalkPower { client: id, grant }, cx)),
        );
    }

    if !d.groups.is_empty() {
        let (groups, member, s) = (d.groups.clone(), d.row.group_ids.clone(), send.clone());
        menu = menu.submenu_with_icon(Some(lucide("shield", MUTED)), "Server groups", window, cx, move |mut sub, _, _| {
            sub = sub.min_w(px(MENU_WIDTH));
            for g in &groups {
                let (gid, s, is_member) = (g.id, s.clone(), member.contains(&g.id));
                sub = sub.item(
                    group_row(g.badge.clone(), is_member).on_click(move |_, _, cx| {
                        s(AdminAction::ServerGroup { client: id, group: gid, add: !is_member }, cx)
                    }),
                );
            }
            sub
        });
    }
    if !d.channel_groups.is_empty() {
        let (groups, current, s) = (d.channel_groups.clone(), d.row.channel_group, send.clone());
        menu = menu.submenu_with_icon(Some(lucide("users", MUTED)), "Channel group", window, cx, move |mut sub, _, _| {
            sub = sub.min_w(px(MENU_WIDTH));
            for g in &groups {
                let (gid, s) = (g.id, s.clone());
                sub = sub.item(
                    group_row(g.badge.clone(), g.id == current)
                        .on_click(move |_, _, cx| s(AdminAction::ChannelGroup { client: id, group: gid }, cx)),
                );
            }
            sub
        });
    }

    let (db_id, their) = (d.row.db_id, d.their_channel);
    let (m1, m2, n1, n2) = (model.clone(), model.clone(), name.clone(), name.clone());
    menu = menu
        .item(row("lock-keyhole", "Permissions...", MUTED).on_click(move |_, _, cx| {
            super::open_admin(&m1, con, super::AdminTab::Perms(PermTarget::Client(db_id), n1.clone()), cx)
        }))
        .item(row("list-checks", "Effective permissions...", MUTED).on_click({
            let (m, n) = (model.clone(), name.clone());
            move |_, _, cx| super::open_admin(&m, con, super::AdminTab::Overview(their, db_id, n.clone()), cx)
        }))
        .item(row("lock-keyhole", "Permissions in this channel...", MUTED).on_click(move |_, _, cx| {
            let tab = super::AdminTab::Perms(PermTarget::ChannelClient { channel: their, client: db_id }, format!("{n2} in channel"));
            super::open_admin(&m2, con, tab, cx)
        }));

    if !d.row.me {
        menu = menu
            .item(row("mail", "Send offline message...", MUTED).on_click({
                let (m, uid, name) = (model.clone(), uid.clone(), name.clone());
                move |_, _, cx| super::open_admin(&m, con, super::AdminTab::Inbox(Some((uid.clone(), name.clone()))), cx)
            }))
            .item(row("flag", "Complain...", MUTED).on_click({
                // complaints go by database id, not client id
                let (p, name) = (prompt_fn(view, con, d.row.db_id), name.clone());
                move |_, w, cx| p(PromptKind::Complain, name.clone(), String::new(), w, cx)
            }))
            .separator()
            .item(row("log-out", "Kick from channel...", RED).on_click(ask(PromptKind::KickChannel)))
            .item(row("door-open", "Kick from server...", RED).on_click(ask(PromptKind::KickServer)));
        let (p, name) = (prompt.clone(), name.clone());
        menu = menu.submenu_with_icon(Some(lucide("ban", RED)), "Ban", window, cx, move |mut sub, _, _| {
            sub = sub.min_w(px(200.));
            for (label, secs) in [
                ("10 minutes", Some(600)),
                ("1 hour", Some(3600)),
                ("1 day", Some(86_400)),
                ("1 week", Some(604_800)),
                ("Permanently", None),
            ] {
                let (p, name) = (p.clone(), name.clone());
                sub = sub.item(
                    row("ban", label, RED).on_click(move |_, w, cx| p(PromptKind::Ban(secs), name.clone(), String::new(), w, cx)),
                );
            }
            sub
        });
    }

    menu.separator().item(row("copy", "Copy unique ID", MUTED).on_click(move |_, _, cx| {
        cx.write_to_clipboard(ClipboardItem::new_string(uid.clone()));
    }))
}

#[allow(clippy::too_many_arguments)]
pub fn channel_menu(
    menu: PopupMenu,
    con: ConId,
    ch: &ChannelRow,
    path: String,
    here: bool,
    model: &Entity<Model>,
    view: &WeakEntity<MainView>,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let (id, name, topic) = (ch.id, ch.name.clone(), ch.topic.clone());
    let prompt = prompt_fn(view, con, id);
    let ask = |kind: PromptKind, initial: String| {
        let (p, name) = (prompt.clone(), name.clone());
        move |_: &gpui_kit::ClickEvent, w: &mut Window, cx: &mut App| p(kind, name.clone(), initial.clone(), w, cx)
    };
    let editor = |original: Option<ChannelRow>| {
        let (view, parent_name) = (view.clone(), name.clone());
        move |_: &gpui_kit::ClickEvent, w: &mut Window, cx: &mut App| {
            let (original, parent_name) = (original.clone(), parent_name.clone());
            let _ = view.update(cx, |v, cx| v.open_channel_editor(con, original, id, parent_name, w, cx));
        }
    };
    let m = model.clone();
    let mut menu = menu.min_w(px(MENU_WIDTH)).label(name.clone());
    if !here {
        menu = menu.item(row("log-in", "Join channel", MUTED).on_click(move |_, _, cx| {
            m.read(cx).net.send(Cmd::Join { con, channel: id, password: None });
        }));
    }
    let n = name.clone();
    let mut menu = menu.item(row("copy", "Copy name", MUTED).on_click(move |_, _, cx| {
        cx.write_to_clipboard(ClipboardItem::new_string(n.clone()));
    }));
    let (m, on) = (model.clone(), !ch.subscribed);
    menu = menu.item(row(if on { "eye" } else { "eye-off" }, if on { "Subscribe" } else { "Unsubscribe" }, MUTED).on_click(
        move |_, _, cx| m.read(cx).net.send(Cmd::Admin { con, action: AdminAction::Subscribe { channel: id, on } }),
    ));
    whisper_submenu(menu, WhisperTarget::Channel { path }, model, window, cx)
    .separator()
    .label("Channel settings")
    .item(row("settings-2", "Edit channel...", MUTED).on_click(editor(Some(ch.clone()))))
    .item(row("folder", "Files...", MUTED).on_click({
        let (m, n) = (model.clone(), ch.name.clone());
        move |_, _, cx| super::open_admin(&m, con, super::AdminTab::Files(id, n.clone()), cx)
    }))
    .item(row("lock-keyhole", "Permissions...", MUTED).on_click({
        let (m, title) = (model.clone(), format!("Channel {}", ch.name));
        move |_, _, cx| super::open_admin(&m, con, super::AdminTab::Perms(PermTarget::Channel(id), title.clone()), cx)
    }))
    .item(row("folder-plus", "Create sub-channel...", MUTED).on_click(editor(None)))
    .item(row("pencil", "Rename...", MUTED).on_click(ask(PromptKind::RenameChannel, name.clone())))
    .item(row("text-quote", "Set topic...", MUTED).on_click(ask(PromptKind::SetTopic, topic)))
    .item(
        row("lock", if ch.has_password { "Change password..." } else { "Set password..." }, MUTED)
            .on_click(ask(PromptKind::SetPassword, String::new())),
    )
    .separator()
    .item(row("trash", "Delete channel...", RED).on_click(ask(PromptKind::DeleteChannel, String::new())))
}

/// The menu on your own name in the sidebar footer.
pub fn self_menu(
    menu: PopupMenu,
    con: Option<ConId>,
    me: Option<ClientRow>,
    away: Option<String>,
    model: &Entity<Model>,
    view: &WeakEntity<MainView>,
) -> PopupMenu {
    let send = {
        let model = model.clone();
        move |cmd: Cmd, cx: &mut App| model.read(cx).net.send(cmd)
    };
    let ask = |kind: PromptKind, initial: String| {
        let view = view.clone();
        move |_: &gpui_kit::ClickEvent, w: &mut Window, cx: &mut App| {
            let initial = initial.clone();
            let _ = view.update(cx, |v, cx| v.open_prompt(kind, con.unwrap_or(0), 0, String::new(), initial, w, cx));
        }
    };
    let mut menu = menu.min_w(px(MENU_WIDTH));
    menu = match &away {
        Some(_) => {
            let s = send.clone();
            menu.item(row("sun", "Back", MUTED).on_click(move |_, _, cx| s(Cmd::Away(None), cx)))
                .item(row("moon", "Change away message...", MUTED).on_click(ask(PromptKind::Away, away.clone().unwrap_or_default())))
        }
        None => menu.item(row("moon", "Set away...", MUTED).on_click(ask(PromptKind::Away, String::new()))),
    };
    let (Some(con), Some(me)) = (con, me) else { return menu };
    let s = send.clone();
    let commander = me.commander;
    menu = menu.separator().item(
        row("megaphone", if commander { "Stop being channel commander" } else { "Channel commander" }, MUTED)
            .on_click(move |_, _, cx| s(Cmd::Me { con, action: SelfAction::Commander(!commander) }, cx)),
    );
    menu = match me.talk_request {
        Some(_) => {
            let s = send.clone();
            menu.item(
                row("hand", "Withdraw talk power request", MUTED)
                    .on_click(move |_, _, cx| s(Cmd::Me { con, action: SelfAction::TalkRequest(None) }, cx)),
            )
        }
        None => menu.item(row("hand", "Request talk power...", MUTED).on_click(ask(PromptKind::TalkRequest, String::new()))),
    };
    let has_avatar = me.avatar.is_some();
    let model = model.clone();
    menu.item(row("pencil", "Edit description...", MUTED).on_click(ask(PromptKind::Description, String::new())))
        .separator()
        .item(row("image", "Set avatar...", MUTED).on_click({
            let model = model.clone();
            move |_, _, cx| {
                let rx = cx.prompt_for_paths(gpui_kit::PathPromptOptions {
                    files: true,
                    directories: false,
                    multiple: false,
                    prompt: Some("Use as avatar".into()),
                });
                let model = model.clone();
                cx.spawn(async move |cx| {
                    let Ok(Ok(Some(paths))) = rx.await else { return };
                    let Some(path) = paths.into_iter().next() else { return };
                    let prepared = cx
                        .background_executor()
                        .spawn(async move { crate::net::files::prepare_avatar(&path) })
                        .await;
                    let _ = cx.update(|cx| match prepared {
                        Ok(a) => model.read(cx).net.send(Cmd::SetAvatar(Some(a))),
                        Err(e) => tracing::error!(%e, "couldn't read that picture"),
                    });
                })
                .detach();
            }
        }))
        .when(has_avatar, |menu| {
            menu.item(row("image-off", "Remove avatar", MUTED).on_click(move |_, _, cx| {
                model.read(cx).net.send(Cmd::SetAvatar(None))
            }))
        })
}

/// "Whisper lists" submenu: toggle `target` in each list, or start a new one.
fn whisper_submenu(
    menu: PopupMenu,
    target: WhisperTarget,
    model: &Entity<Model>,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let lists = model.read(cx).settings.whisper_lists.clone();
    let model = model.clone();
    menu.submenu_with_icon(Some(lucide("radio", MUTED)), "Whisper lists", window, cx, move |mut sub, _, _| {
        sub = sub.min_w(px(MENU_WIDTH));
        for l in &lists {
            let has = l.targets.contains(&target);
            let (m, id, t) = (model.clone(), l.id.clone(), target.clone());
            let name = if l.name.is_empty() { "Unnamed list".to_string() } else { l.name.clone() };
            sub = sub.item(
                PopupMenuItem::element(move |_, _| div().py(px(5.)).text_sm().child(name.clone())).checked(has).on_click(
                    move |_, _, cx| {
                        let (id, t) = (id.clone(), t.clone());
                        m.update(cx, |m, cx| {
                            m.update_settings(cx, |s| {
                                if let Some(l) = s.whisper_lists.iter_mut().find(|l| l.id == id) {
                                    if has {
                                        l.targets.retain(|x| *x != t);
                                    } else {
                                        l.targets.push(t);
                                    }
                                }
                            })
                        });
                    },
                ),
            );
        }
        if !lists.is_empty() {
            sub = sub.separator();
        }
        let (m, t) = (model.clone(), target.clone());
        sub.item(row("plus", "New list with this", MUTED).on_click(move |_, _, cx| {
            let t = t.clone();
            m.update(cx, |m, cx| {
                m.update_settings(cx, |s| {
                    s.whisper_lists.push(WhisperList {
                        id: crate::config::new_id(),
                        name: t.label(),
                        hotkey: String::new(),
                        targets: vec![t],
                    });
                })
            });
            super::open_settings(&m, super::SettingsTab::Whisper, cx);
        }))
    })
}
