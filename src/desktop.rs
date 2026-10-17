//! Desktop integration: launcher entry and icon, ts3server:// links,
//! autostart in the tray.

use std::path::PathBuf;
#[cfg(target_os = "linux")]
use std::process::Command;

#[cfg(target_os = "linux")]
const ICON: &str = include_str!("../assets/wisp.svg");

#[cfg(target_os = "linux")]
fn data_dir() -> PathBuf {
    dirs::data_dir().unwrap_or_else(|| PathBuf::from("."))
}

/// How to start Wisp from outside (launcher entries, hotkeys, autostart).
pub fn exe() -> String {
    // inside an AppImage the binary sits in a temporary mount; the image is what to start
    if let Some(image) = std::env::var_os("APPIMAGE") {
        return PathBuf::from(image).display().to_string();
    }
    // the ~/.local/bin link survives rebuilds; the build path doesn't
    let link = dirs::home_dir().map(|h| h.join(".local/bin/wisp")).filter(|_| cfg!(target_os = "linux"));
    match link.filter(|l| l.exists()) {
        Some(l) => l.display().to_string(),
        None => std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_else(|_| "wisp".into()),
    }
}

#[cfg(target_os = "linux")]
fn entry(hidden: bool) -> String {
    format!(
        "[Desktop Entry]\nType=Application\nName=Wisp\nGenericName=Voice chat\nComment=Voice chat for TeamSpeak 3 servers\n\
         Exec={} {}\nIcon=wisp\nTerminal=false\nCategories=Network;Chat;AudioVideo;\n\
         MimeType=x-scheme-handler/ts3server;\nStartupWMClass=wisp\n",
        exe(),
        if hidden { "--hidden" } else { "%u" }
    )
}

#[cfg(unix)]
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
#[cfg(target_os = "linux")]
pub fn install() {
    let apps = data_dir().join("applications");
    let changed = write_if_changed(&apps.join("wisp.desktop"), &entry(false));
    write_if_changed(&data_dir().join("icons/hicolor/scalable/apps/wisp.svg"), ICON);
    if changed {
        let _ = Command::new("update-desktop-database").arg(&apps).output();
    }
}

/// The installer (Windows) or the app bundle (macOS) takes care of this.
#[cfg(not(target_os = "linux"))]
pub fn install() {}

#[cfg(target_os = "linux")]
fn autostart_file() -> PathBuf {
    dirs::config_dir().unwrap_or_default().join("autostart/wisp.desktop")
}

#[cfg(target_os = "macos")]
fn autostart_file() -> PathBuf {
    dirs::home_dir().unwrap_or_default().join("Library/LaunchAgents/at.timkunze.wisp.plist")
}

#[cfg(target_os = "macos")]
fn launch_agent() -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\"><dict>\n\
         <key>Label</key><string>at.timkunze.wisp</string>\n\
         <key>ProgramArguments</key><array><string>{}</string><string>--hidden</string></array>\n\
         <key>RunAtLoad</key><true/>\n\
         </dict></plist>\n",
        exe().replace('&', "&amp;").replace('<', "&lt;")
    )
}

#[cfg(unix)]
pub fn autostart() -> bool {
    autostart_file().exists()
}

#[cfg(unix)]
pub fn set_autostart(on: bool) {
    if on {
        #[cfg(target_os = "linux")]
        write_if_changed(&autostart_file(), &entry(true));
        #[cfg(target_os = "macos")]
        write_if_changed(&autostart_file(), &launch_agent());
    } else {
        let _ = std::fs::remove_file(autostart_file());
    }
}

#[cfg(windows)]
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

#[cfg(windows)]
pub fn autostart() -> bool {
    let hkcu = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER);
    hkcu.open_subkey(RUN_KEY).and_then(|k| k.get_value::<String, _>("Wisp")).is_ok()
}

#[cfg(windows)]
pub fn set_autostart(on: bool) {
    let hkcu = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER);
    let Ok((key, _)) = hkcu.create_subkey(RUN_KEY) else { return };
    if on {
        let _ = key.set_value("Wisp", &format!("\"{}\" --hidden", exe()));
    } else {
        let _ = key.delete_value("Wisp");
    }
}

/// Whether Wisp can make itself the ts3server:// handler. On macOS the app
/// bundle declares the scheme and the system picks.
pub const CAN_SET_LINKS: bool = !cfg!(target_os = "macos");

#[cfg(target_os = "linux")]
fn link_handler() -> String {
    Command::new("xdg-mime")
        .args(["query", "default", "x-scheme-handler/ts3server"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

#[cfg(target_os = "linux")]
fn set_link_handler(desktop_file: &str) {
    let _ = Command::new("xdg-mime").args(["default", desktop_file, "x-scheme-handler/ts3server"]).output();
}

#[cfg(target_os = "linux")]
pub fn handles_links() -> bool {
    link_handler() == "wisp.desktop"
}

/// Make Wisp open ts3server:// links. Returns what opened them before, to
/// give back later.
#[cfg(target_os = "linux")]
pub fn take_links() -> Option<String> {
    let before = link_handler();
    set_link_handler("wisp.desktop");
    (before != "wisp.desktop" && !before.is_empty()).then_some(before)
}

#[cfg(target_os = "linux")]
pub fn give_back_links(previous: Option<&str>) {
    set_link_handler(previous.unwrap_or("teamspeak.desktop"));
}

#[cfg(windows)]
const LINK_KEY: &str = r"Software\Classes\ts3server";

#[cfg(windows)]
fn link_command() -> Option<String> {
    let hkcu = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER);
    hkcu.open_subkey(format!(r"{LINK_KEY}\shell\open\command")).and_then(|k| k.get_value::<String, _>("")).ok()
}

#[cfg(windows)]
fn set_link_command(command: &str) {
    let hkcu = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER);
    if let Ok((key, _)) = hkcu.create_subkey(LINK_KEY) {
        let _ = key.set_value("", &"URL:TeamSpeak 3 server");
        let _ = key.set_value("URL Protocol", &"");
    }
    if let Ok((key, _)) = hkcu.create_subkey(format!(r"{LINK_KEY}\shell\open\command")) {
        let _ = key.set_value("", &command);
    }
}

#[cfg(windows)]
pub fn handles_links() -> bool {
    link_command().is_some_and(|c| c.contains(&exe()))
}

#[cfg(windows)]
pub fn take_links() -> Option<String> {
    let before = link_command().filter(|c| !c.contains(&exe()));
    set_link_command(&format!("\"{}\" \"%1\"", exe()));
    before
}

#[cfg(windows)]
pub fn give_back_links(previous: Option<&str>) {
    match previous {
        Some(command) => set_link_command(command),
        None => {
            let hkcu = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER);
            let _ = hkcu.delete_subkey_all(LINK_KEY);
        }
    }
}

#[cfg(target_os = "macos")]
pub fn handles_links() -> bool {
    true
}

#[cfg(target_os = "macos")]
pub fn take_links() -> Option<String> {
    None
}

#[cfg(target_os = "macos")]
pub fn give_back_links(_previous: Option<&str>) {}

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
