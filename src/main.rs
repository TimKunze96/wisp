#![recursion_limit = "256"]

mod audio;
mod config;
mod ctl;
mod desktop;
mod hotkeys;
mod identity;
mod notify;
mod net;
mod tray;
mod ui;

use gpui_kit::App;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Ord, PartialOrd)]
pub struct ConnectionId(u64);

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let hidden = args.iter().any(|a| a == "--hidden");
    let mut commands: Vec<String> = args.into_iter().filter(|a| a != "--hidden").collect();
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
        // `wisp status | head` should end quietly, not panic on the closed pipe
        reset_sigpipe();
        std::process::exit(ctl::cli(&commands));
    }
    // Single instance: a second start just shows the running one.
    if ctl::is_running() {
        std::process::exit(ctl::cli(&["show".to_string()]));
    }

    tracing_subscriber::fmt().with_max_level(tracing::Level::ERROR).init();
    let settings = config::Settings::load();
    let (handle, snapshots) = net::spawn(settings.clone());
    let (ui_tx, ui_rx) = futures::channel::mpsc::unbounded();
    let _ = ctl::UI.set(ui_tx);

    gpui_kit::application().with_assets(gpui_kit::assets::AllAssets).run(move |cx: &mut App| {
        gpui_kit::init(cx);
        ui::apply_theme(cx);
        desktop::install();
        if let Some(l) = link {
            ctl::ui(ctl::UiRequest::Link(l));
        }
        ui::start(handle, settings, snapshots, ui_rx, !hidden, cx);
    });
}

fn reset_sigpipe() {
    unsafe extern "C" {
        fn signal(sig: i32, handler: usize) -> usize;
    }
    // SIGPIPE = 13, SIG_DFL = 0
    unsafe { signal(13, 0) };
}
