//! The permission editor: every permission of a group, channel or person, by category.

use super::*;

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
            .text_color(rgb(if current.is_some() { colors().text } else { colors().dim }))
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
                    .border_color(alpha(ink(), 0x08))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().text_sm().truncate().text_color(rgb(if set.is_some() { colors().text } else { colors().muted })).child(p.name.clone()))
                            .when(!p.description.is_empty(), |d| d.child(label(p.description.clone()).truncate())),
                    )
                    .child(div().w(px(90.)).flex().justify_end().child(self.value_cell(id, set, boolean, cx)))
                    .when(grants, |d| d.child(div().w(px(90.)).child(self.value_cell(id | 0x8000, grant, false, cx))))
                    .child(div().w(px(28.)).when(set.is_some() || grant.is_some(), |d| {
                        d.child(icon_btn(("pdel", id as usize), "x", colors().muted, "Remove").on_click(cx.listener(move |t, _, _, cx| {
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
