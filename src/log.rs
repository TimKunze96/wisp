//! A log file people can send when something goes wrong.

use std::fs::{self, File, OpenOptions};
use std::path::PathBuf;
use std::sync::Mutex;

use tracing::Level;
use tracing_subscriber::filter::Targets;
use tracing_subscriber::prelude::*;

/// Past this the log starts over (the previous one stays as wisp.old.log).
const MAX_SIZE: u64 = 2 * 1024 * 1024;

pub fn path() -> PathBuf {
    crate::config::cache_dir().join("wisp.log")
}

pub fn init() {
    let stderr = tracing_subscriber::fmt::layer().with_writer(std::io::stderr).with_filter(Targets::new().with_default(Level::ERROR));
    let file = open().map(|f| {
        tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .with_writer(Mutex::new(f))
            .with_filter(Targets::new().with_default(Level::WARN).with_target("wisp", Level::INFO))
    });
    tracing_subscriber::registry().with(stderr).with(file).init();

    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        tracing::error!("{info}\n{}", std::backtrace::Backtrace::force_capture());
        default(info);
    }));
    tracing::info!("Wisp {} on {} {}", env!("CARGO_PKG_VERSION"), std::env::consts::OS, std::env::consts::ARCH);
}

fn open() -> Option<File> {
    let path = path();
    fs::create_dir_all(path.parent()?).ok()?;
    if fs::metadata(&path).is_ok_and(|m| m.len() > MAX_SIZE) {
        let _ = fs::rename(&path, path.with_file_name("wisp.old.log"));
    }
    OpenOptions::new().create(true).append(true).open(path).ok()
}
