//! Channel files: transfers (download / upload with progress) and the
//! commands around them (list, delete, new folder, rename).

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use serde::Serialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct FileEntry {
    pub name: String,
    pub size: u64,
    pub date: String,
    pub is_dir: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Transfer {
    pub id: u64,
    /// Shown name (the file's name).
    pub name: String,
    pub upload: bool,
    pub size: u64,
    pub done: u64,
    /// Local file (download target or upload source).
    pub local: PathBuf,
    pub state: TransferState,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub enum TransferState {
    Waiting,
    Running,
    Done,
    Failed(String),
}

/// File actions from the UI.
#[derive(Clone, Debug)]
pub enum FileCmd {
    Download { channel: u64, password: String, path: String, to: PathBuf },
    Upload { channel: u64, password: String, path: String, from: PathBuf },
    Delete { channel: u64, password: String, path: String },
    Mkdir { channel: u64, password: String, path: String },
    Rename { channel: u64, password: String, from: String, to: String },
    /// Forget finished transfers.
    ClearDone,
}

/// What a file transfer handle is for.
pub enum Purpose {
    /// A group / channel icon or an avatar for the cache; with a marker file
    /// and the hash it was downloaded for (avatars).
    Icon(PathBuf, Option<(PathBuf, String)>),
    /// A user's transfer, by our transfer id.
    Transfer(u64),
}

pub type Transfers = Rc<RefCell<Vec<Transfer>>>;

fn update(list: &Transfers, id: u64, f: impl FnOnce(&mut Transfer)) {
    if let Some(t) = list.borrow_mut().iter_mut().find(|t| t.id == id) {
        f(t);
    }
}

/// Copy a download stream to its file, reporting progress.
pub fn run_download(list: Transfers, id: u64, size: u64, mut stream: tokio::net::TcpStream, to: PathBuf) {
    update(&list, id, |t| {
        t.state = TransferState::Running;
        t.size = size;
    });
    tokio::task::spawn_local(async move {
        let result = async {
            if let Some(dir) = to.parent() {
                tokio::fs::create_dir_all(dir).await?;
            }
            let mut file = tokio::fs::File::create(&to).await?;
            let mut buf = vec![0u8; 64 * 1024];
            let mut done = 0u64;
            loop {
                let n = stream.read(&mut buf).await?;
                if n == 0 {
                    break;
                }
                file.write_all(&buf[..n]).await?;
                done += n as u64;
                update(&list, id, |t| t.done = done);
                if done >= size {
                    break;
                }
            }
            file.flush().await?;
            anyhow::Ok(())
        }
        .await;
        update(&list, id, |t| {
            t.state = match result {
                Ok(()) => TransferState::Done,
                Err(e) => TransferState::Failed(e.to_string()),
            }
        });
    });
}

/// Copy a file into an upload stream, reporting progress.
pub fn run_upload(list: Transfers, id: u64, seek: u64, mut stream: tokio::net::TcpStream, from: PathBuf) {
    update(&list, id, |t| t.state = TransferState::Running);
    tokio::task::spawn_local(async move {
        let result = async {
            use tokio::io::AsyncSeekExt;
            let mut file = tokio::fs::File::open(&from).await?;
            if seek > 0 {
                file.seek(std::io::SeekFrom::Start(seek)).await?;
            }
            let mut buf = vec![0u8; 64 * 1024];
            let mut done = seek;
            loop {
                let n = file.read(&mut buf).await?;
                if n == 0 {
                    break;
                }
                stream.write_all(&buf[..n]).await?;
                done += n as u64;
                update(&list, id, |t| t.done = done);
            }
            stream.shutdown().await?;
            anyhow::Ok(())
        }
        .await;
        update(&list, id, |t| {
            t.state = match result {
                Ok(()) => TransferState::Done,
                Err(e) => TransferState::Failed(e.to_string()),
            }
        });
    });
}

/// Downloads go to ~/Downloads (or the home folder), without overwriting.
pub fn download_path(name: &str) -> PathBuf {
    let dir = dirs::download_dir().or_else(dirs::home_dir).unwrap_or_else(|| PathBuf::from("."));
    let mut path = dir.join(name);
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
        _ => (name.to_string(), String::new()),
    };
    let mut n = 1;
    while path.exists() {
        path = dir.join(format!("{stem} ({n}){ext}"));
        n += 1;
    }
    path
}

/// Square-crop and scale a picture for use as avatar (300 px PNG in the cache).
/// Returns the file and its md5 (hex), which TeamSpeak uses as avatar hash.
pub fn prepare_avatar(from: &std::path::Path) -> anyhow::Result<(PathBuf, String)> {
    use md5::{Digest, Md5};
    let img = image::open(from)?;
    let side = img.width().min(img.height());
    let x = (img.width() - side) / 2;
    let y = (img.height() - side) / 2;
    let square = img.crop_imm(x, y, side, side).resize_exact(300.min(side), 300.min(side), image::imageops::FilterType::Lanczos3);
    let mut png = Vec::new();
    square.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)?;
    let path = crate::config::cache_dir().join("my-avatar.png");
    std::fs::create_dir_all(crate::config::cache_dir())?;
    std::fs::write(&path, &png)?;
    let hash: String = Md5::digest(&png).iter().map(|b| format!("{b:02x}")).collect();
    Ok((path, hash))
}

#[cfg(test)]
mod tests {
    #[test]
    fn avatar_is_square_png() {
        let dir = std::env::temp_dir().join("wisp-avatar-test");
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("wide.png");
        image::RgbImage::from_pixel(800, 400, image::Rgb([200, 40, 40])).save(&src).unwrap();
        let (out, hash) = super::prepare_avatar(&src).unwrap();
        let img = image::open(&out).unwrap();
        assert_eq!((img.width(), img.height()), (300, 300));
        assert_eq!(hash.len(), 32);
    }
}
