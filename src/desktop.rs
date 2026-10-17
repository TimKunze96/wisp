//! Desktop integration: launcher entry and icon, ts3server:// links,
//! autostart in the tray.

use std::path::PathBuf;
use std::process::Command;

const ICON: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="256" height="256" viewBox="0 0 24 24">
  <rect x="0.5" y="0.5" width="23" height="23" rx="5.5" fill="#17191d"/>
  <g transform="translate(3 3) scale(0.75)" fill="none" stroke="#e3e5e8" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
    <path d="M12.8 19.6A2 2 0 1 0 14 16H2"/>
    <path d="M17.5 8a2.5 2.5 0 1 1 2 4H2"/>
    <path d="M9.8 4.4A2 2 0 1 1 11 8H2"/>
  </g>
</svg>
"##;

fn data_dir() -> PathBuf {
    dirs::data_dir().unwrap_or_else(|| PathBuf::from("."))
}

fn exe() -> String {
    // the ~/.local/bin link survives rebuilds; the build path doesn't
    let link = dirs::home_dir().map(|h| h.join(".local/bin/wisp"));
    match link.filter(|l| l.exists()) {
        Some(l) => l.display().to_string(),
        None => std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_else(|_| "wisp".into()),
    }
}

fn entry(hidden: bool) -> String {
    format!(
        "[Desktop Entry]\nType=Application\nName=Wisp\nGenericName=Voice chat\nComment=Voice chat for TeamSpeak 3 servers\n\
         Exec={} {}\nIcon=wisp\nTerminal=false\nCategories=Network;Chat;AudioVideo;\n\
         MimeType=x-scheme-handler/ts3server;\nStartupWMClass=wisp\n",
        exe(),
        if hidden { "--hidden" } else { "%u" }
    )
}

/// Write a file when its content differs; returns whether it changed.
fn write_if_changed(path: &PathBuf, content: &str) -> bool {
    if std::fs::read_to_string(path).is_ok_and(|c| c == content) {
        return false;
    }
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    std::fs::write(path, content).is_ok()
}

/// Launcher entry and icon (on every start; cheap when nothing changed).
pub fn install() {
    let apps = data_dir().join("applications");
    let changed = write_if_changed(&apps.join("wisp.desktop"), &entry(false));
    write_if_changed(&data_dir().join("icons/hicolor/scalable/apps/wisp.svg"), ICON);
    if changed {
        let _ = Command::new("update-desktop-database").arg(&apps).output();
    }
}

fn autostart_file() -> PathBuf {
    dirs::config_dir().unwrap_or_default().join("autostart/wisp.desktop")
}

pub fn autostart() -> bool {
    autostart_file().exists()
}

pub fn set_autostart(on: bool) {
    if on {
        write_if_changed(&autostart_file(), &entry(true));
    } else {
        let _ = std::fs::remove_file(autostart_file());
    }
}

pub fn link_handler() -> String {
    Command::new("xdg-mime")
        .args(["query", "default", "x-scheme-handler/ts3server"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

/// Make Wisp (or `other`, e.g. the previous handler) open ts3server:// links.
pub fn set_link_handler(desktop_file: &str) {
    let _ = Command::new("xdg-mime").args(["default", desktop_file, "x-scheme-handler/ts3server"]).output();
}

/// What a ts3server:// link asks for.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Link {
    pub address: String,
    pub nickname: String,
    pub password: String,
    pub channel: String,
    pub channel_password: String,
}

fn decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                match u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""), 16) {
                    Ok(b) => {
                        out.push(b);
                        i += 3;
                        continue;
                    }
                    Err(_) => out.push(b'%'),
                }
            }
            b'+' => out.push(b' '),
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// ts3server://host[:port]?port=9987&nickname=..&password=..&channel=a/b&channelpassword=..
pub fn parse_link(url: &str) -> Option<Link> {
    let rest = url.strip_prefix("ts3server://")?;
    let (host, query) = rest.split_once('?').unwrap_or((rest, ""));
    let host = host.trim_end_matches('/');
    if host.is_empty() {
        return None;
    }
    let mut link = Link { address: decode(host), ..Default::default() };
    let mut port = None;
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        let v = decode(v);
        match k {
            "port" => port = Some(v),
            "nickname" => link.nickname = v,
            "password" => link.password = v,
            "channel" => link.channel = v,
            "channelpassword" => link.channel_password = v,
            _ => {}
        }
    }
    if let Some(p) = port.filter(|p| !p.is_empty() && p != "9987") {
        if !link.address.contains(':') {
            link.address = format!("{}:{p}", link.address);
        }
    }
    Some(link)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links() {
        let l = parse_link("ts3server://ts.example.com?port=9988&nickname=Tim%20K&channel=Lobby/Sub%201&password=a+b").unwrap();
        assert_eq!(l.address, "ts.example.com:9988");
        assert_eq!(l.nickname, "Tim K");
        assert_eq!(l.channel, "Lobby/Sub 1");
        assert_eq!(l.password, "a b");
        assert_eq!(parse_link("ts3server://ts.example.com/").unwrap().address, "ts.example.com");
        assert!(parse_link("https://x").is_none());
    }
}
