//! What applies to someone in a channel, and which group it comes from.

use super::*;

impl AdminView {
    pub(super) fn overview_tab(&mut self, cx: &mut Context<Self>) -> AnyElement {
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
                    .border_color(alpha(ink(), 0x08))
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
}
