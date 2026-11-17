//! Server and channel groups (roles on Banshee): the list, their members and icons.

use super::*;

impl AdminView {
    pub(super) fn kind(&self) -> GroupKind {
        if self.tab == AdminTab::ChannelGroups { GroupKind::Channel } else { GroupKind::Server }
    }

    pub(super) fn groups(&self, cx: &App) -> Vec<(u64, crate::net::GroupBadge)> {
        let Some(s) = self.server(cx) else { return Vec::new() };
        let list = if self.kind() == GroupKind::Channel { &s.channel_groups } else { &s.groups };
        list.iter().map(|g| (g.id, g.badge.clone())).collect()
    }

    pub(super) fn group_target(&self, id: u64) -> PermTarget {
        if self.kind() == GroupKind::Channel { PermTarget::ChannelGroup(id) } else { PermTarget::ServerGroup(id) }
    }

    pub(super) fn toggle_group_icons(&mut self, cx: &mut Context<Self>) {
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
        let grid = cx.new(|cx| crate::ui::icons::IconGrid::new(model, con, current, cx));
        cx.subscribe(&grid, move |this, _, ev: &crate::ui::icons::IconPicked, cx| {
            if let Some(w) = crate::net::admin::icon_write(this.group_target(group), ev.0) {
                this.write(w, cx);
            }
        })
        .detach();
        self.group_icons = Some(grid);
        cx.notify();
    }

    pub(super) fn pick_group(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
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

    pub(super) fn load_members(&mut self, cx: &mut Context<Self>) {
        let Some(g) = self.group else { return };
        ask(&self.model, self.con, Query::ServerGroupMembers(g), cx, |this: &mut Self, r, _| {
            this.members = Some(r.and_then(|a| match a {
                Answer::Members(m) => Ok(m),
                _ => Err("unexpected answer".into()),
            }));
        });
    }

    pub(super) fn add_member(&mut self, cx: &mut Context<Self>) {
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

    pub(super) fn groups_tab(&mut self, cx: &mut Context<Self>) -> AnyElement {
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
                    .child(div().text_sm().truncate().text_color(rgb(if on { colors().text } else { colors().soft })).child(b.name.clone())),
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
            .child(icon_btn("grp-del", "trash", colors().red, "Delete group").on_click(cx.listener(move |t, _, _, cx| {
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
                                .border_color(alpha(ink(), 0x08))
                                .child(avatar(&mem.name, None, 22., false))
                                .child(div().flex_1().text_sm().truncate().child(mem.name.clone()))
                                .child(label(mem.uid.clone()).truncate().max_w(px(220.)))
                                .child(icon_btn(("mrm", i), "user-minus", colors().red, "Remove from group").on_click(cx.listener(
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
}
