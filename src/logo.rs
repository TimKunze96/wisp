//! The logo for tray icons, drawn from assets/wisp-mark.svg.

use crate::net::Snapshot;

/// The logo mark as RGBA pixels at `size`.
fn mark(size: u32) -> Vec<[u8; 4]> {
    let tree = resvg::usvg::Tree::from_data(include_bytes!("../assets/wisp-mark.svg"), &Default::default());
    let (Ok(tree), Some(mut pixmap)) = (tree, resvg::tiny_skia::Pixmap::new(size, size)) else { return Vec::new() };
    let scale = size as f32 / tree.size().width().max(tree.size().height());
    resvg::render(&tree, resvg::tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
    pixmap
        .pixels()
        .iter()
        .map(|p| {
            let c = p.demultiply();
            [c.red(), c.green(), c.blue(), c.alpha()]
        })
        .collect()
}

/// The logo as a solid silhouette in `color`, RGBA, so it reads like the
/// other icons next to it.
pub fn silhouette(size: u32, color: (u8, u8, u8)) -> Vec<u8> {
    mark(size)
        .into_iter()
        .flat_map(|[_, _, _, a]| {
            // the faint end of the tail would vanish at bar size
            let a = (a as f32 * 2.2).min(255.0) as u8;
            [color.0, color.1, color.2, a]
        })
        .collect()
}

/// What the tray shows: grey idle, blue connected, green talking, red muted.
pub fn state(s: &Snapshot) -> (&'static str, (u8, u8, u8)) {
    if s.servers.is_empty() {
        ("Not connected", (0xa9, 0xb1, 0xd6))
    } else if s.self_muted || s.deafened {
        ("Muted", (0xe5, 0x53, 0x4b))
    } else if s.transmitting {
        ("Talking", (0x5e, 0xd2, 0x86))
    } else {
        ("Connected", (0x7a, 0xa2, 0xf7))
    }
}

/// "Server: channel" per connection, one per line.
pub fn where_am_i(snap: &Snapshot) -> String {
    snap.servers
        .iter()
        .map(|s| {
            let ch = s.own_channel.and_then(|id| s.channels.iter().find(|c| c.id == id)).map(|c| c.name.as_str()).unwrap_or("-");
            let name = if s.server_name.is_empty() { &s.label } else { &s.server_name };
            format!("{name}: {ch}")
        })
        .collect::<Vec<_>>()
        .join("\n")
}
