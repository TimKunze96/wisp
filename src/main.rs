#![recursion_limit = "256"]
// a GUI app on Windows: no console window (the CLI attaches to its terminal)
#![cfg_attr(windows, windows_subsystem = "windows")]

mod audio;
mod config;
mod ctl;
mod demo;
mod desktop;
mod hotkeys;
mod identity;
mod logo;
mod notify;
mod net;
#[cfg_attr(not(target_os = "linux"), path = "tray_native.rs")]
mod tray;
mod ui;
mod update;

use gpui_kit::App;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Ord, PartialOrd)]
pub struct ConnectionId(u64);

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let hidden = args.iter().any(|a| a == "--hidden");
    let demo = args.iter().any(|a| a == "--demo");
    let mut commands: Vec<String> = args.into_iter().filter(|a| a != "--hidden" && a != "--demo").collect();
    // ts3server:// link (from the browser): hand it to the running app, or
    // start and open it
    let link = commands.first().filter(|a| a.starts_with("ts3server://")).cloned();
    if let Some(l) = &link {
        if ctl::is_running() {
            std::process::exit(ctl::cli(&["link".to_string(), l.clone()]));
        }
        commands.clear();
    }
    if !commands.is_empty() {
        attach_console();
        // `wisp status | head` should end quietly, not panic on the closed pipe
        reset_sigpipe();
        std::process::exit(ctl::cli(&commands));
    }
    // After an update the new version waits for the old one to finish quitting.
    if std::env::var_os(update::RESTARTED_ENV).is_some() {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        while ctl::is_running() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
    }
    update::clean_up();
    if demo {
        demo::isolate();
    }
    // Single instance: a second start just shows the running one.
    if !demo && ctl::is_running() {
        std::process::exit(ctl::cli(&["show".to_string()]));
    }

    tracing_subscriber::fmt().with_max_level(tracing::Level::ERROR).init();
    let (handle, snapshots, settings) = if demo {
        demo::spawn()
    } else {
        let settings = config::Settings::load();
        let (handle, snapshots) = net::spawn(settings.clone());
        (handle, snapshots, settings)
    };
    let (ui_tx, ui_rx) = futures::channel::mpsc::unbounded();
    let _ = ctl::UI.set(ui_tx);

    let app = gpui_kit::application().with_assets(gpui_kit::assets::AllAssets);
    // macOS hands links (and clicks on the Dock icon) to the running app
    app.on_open_urls(|urls| {
        for url in urls.into_iter().filter(|u| u.starts_with("ts3server://")) {
            ctl::ui(ctl::UiRequest::Link(url));
        }
    });
    app.on_reopen(|_| ctl::ui(ctl::UiRequest::Show));
    app.run(move |cx: &mut App| {
        gpui_kit::init(cx);
        // the UI font travels with the app (it's rarely installed outside GNOME)
        let _ = cx.text_system().add_fonts(vec![
            include_bytes!("../assets/fonts/AdwaitaSans-Regular.ttf").as_slice().into(),
            include_bytes!("../assets/fonts/AdwaitaSans-Italic.ttf").as_slice().into(),
        ]);
        ui::apply_theme(cx);
        if !demo {
            desktop::install();
        }
        if let Some(l) = link {
            ctl::ui(ctl::UiRequest::Link(l));
        }
        ui::start(handle, settings, snapshots, ui_rx, !hidden, cx);
    });
}

#[cfg(not(unix))]
fn reset_sigpipe() {}

/// Print into the terminal the CLI was started from.
#[cfg(windows)]
fn attach_console() {
    use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
    unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
}

#[cfg(not(windows))]
fn attach_console() {}

#[cfg(unix)]
fn reset_sigpipe() {
    unsafe extern "C" {
        fn signal(sig: i32, handler: usize) -> usize;
    }
    // SIGPIPE = 13, SIG_DFL = 0
    unsafe { signal(13, 0) };
}
