//! The sound of everything that plays except Wisp itself (so viewers don't
//! hear the voice chat twice), or of one app, through WASAPI process
//! loopback (Windows 10 2004 and newer), as Opus packets on the audio track.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

use anyhow::{Result, anyhow};
use wasapi::{AudioClient, DeviceEnumerator, Direction, SampleType, SessionState, StreamMode, WaveFormat, initialize_mta};

use super::{SOUND_FRAME, SoundEncoder};

pub struct Sound {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Sound {
    /// Start capturing (everything, or only the process `app`, a pid);
    /// `packets` gets each 20 ms Opus packet.
    pub fn start(packets: impl FnMut(Vec<u8>) + Send + 'static, app: Option<String>) -> Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready) = std::sync::mpsc::channel();
        let s = stop.clone();
        let thread = std::thread::Builder::new().name("screen-sound".into()).spawn(move || {
            if let Err(e) = run(packets, app, &s, &ready_tx) {
                let _ = ready_tx.send(Err(e));
            }
        })?;
        match ready.recv_timeout(std::time::Duration::from_secs(3)) {
            Ok(Ok(())) => Ok(Self { stop, thread: Some(thread) }),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(anyhow!("Windows didn't start the sound capture")),
        }
    }
}

impl Drop for Sound {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn run(
    mut packets: impl FnMut(Vec<u8>),
    app: Option<String>,
    stop: &AtomicBool,
    ready: &std::sync::mpsc::Sender<Result<()>>,
) -> Result<()> {
    let wasapi = |e: wasapi::WasapiError| anyhow!("{e}");
    initialize_mta().ok().map_err(|e| anyhow!("{e}"))?;
    let format = WaveFormat::new(32, 32, SampleType::Float, 48000, 2, None);
    // one app (and its children), or everything but our own process
    let (pid, include) = match app.and_then(|a| a.parse::<u32>().ok()) {
        Some(pid) => (pid, true),
        None => (std::process::id(), false),
    };
    let mut client = AudioClient::new_application_loopback_client(pid, include).map_err(wasapi)?;
    let mode = StreamMode::EventsShared { autoconvert: true, buffer_duration_hns: 0 };
    client.initialize_client(&format, Direction::Capture, mode, None).map_err(wasapi)?;
    let event = client.set_get_eventhandle().map_err(wasapi)?;
    let capture = client.get_audiocaptureclient().map_err(wasapi)?;
    client.start_stream().map_err(wasapi)?;
    let _ = ready.send(Ok(()));

    let mut encoder = SoundEncoder::new()?;
    let mut bytes: VecDeque<u8> = VecDeque::new();
    let frame_bytes = SOUND_FRAME * 2 * 4;
    while !stop.load(Ordering::Relaxed) {
        if event.wait_for_event(100).is_err() {
            continue;
        }
        if capture.read_from_device_to_deque(&mut bytes).is_err() {
            break;
        }
        while bytes.len() >= frame_bytes {
            let raw: Vec<u8> = bytes.drain(..frame_bytes).collect();
            let frame: Vec<f32> = raw.chunks_exact(4).map(|b| f32::from_le_bytes(b.try_into().unwrap())).collect();
            if let Some(p) = encoder.encode(&frame) {
                packets(p);
            }
        }
    }
    let _ = client.stop_stream();
    Ok(())
}

/// The apps playing sound right now (not Wisp), from the default output's
/// audio sessions.
pub fn apps() -> Vec<super::SoundApp> {
    // COM wants a thread of its own here
    std::thread::spawn(|| list_apps().unwrap_or_default()).join().unwrap_or_default()
}

fn list_apps() -> Result<Vec<super::SoundApp>> {
    let wasapi = |e: wasapi::WasapiError| anyhow!("{e}");
    initialize_mta().ok().map_err(|e| anyhow!("{e}"))?;
    let device = DeviceEnumerator::new().map_err(wasapi)?.get_default_device(Direction::Render).map_err(wasapi)?;
    let sessions = device.get_iaudiosessionmanager().map_err(wasapi)?.get_audiosessionenumerator().map_err(wasapi)?;
    let own = std::process::id();
    let mut out: Vec<super::SoundApp> = Vec::new();
    for i in 0..sessions.get_count().map_err(wasapi)? {
        let Ok(session) = sessions.get_session(i) else { continue };
        if !matches!(session.get_state(), Ok(SessionState::Active)) {
            continue;
        }
        let Ok(pid) = session.get_process_id() else { continue };
        // 0 is the system sounds
        if pid == 0 || pid == own || out.iter().any(|a| a.key == pid.to_string()) {
            continue;
        }
        let name = process_name(pid)
            .or_else(|| session.get_display_name().ok().filter(|n| !n.is_empty()))
            .unwrap_or_else(|| format!("Process {pid}"));
        out.push(super::SoundApp { key: pid.to_string(), name });
    }
    out.sort_by_key(|a| a.name.to_lowercase());
    Ok(out)
}

/// "firefox" for a process id.
fn process_name(pid: u32) -> Option<String> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
    };
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return None;
        }
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len);
        CloseHandle(handle);
        if ok == 0 {
            return None;
        }
        let path = String::from_utf16_lossy(&buf[..len as usize]);
        std::path::Path::new(&path).file_stem().map(|s| s.to_string_lossy().into_owned())
    }
}
