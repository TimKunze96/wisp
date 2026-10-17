//! PipeWire extras through pactl: device names, moving our streams, the echo
//! canceller and turning other apps down.

use cpal::traits::HostTrait;

use super::DeviceInfo;

/// Inputs and outputs as PipeWire sees them (via pactl): (sources, sinks).
/// The id is the node name, which is what routing uses.
pub fn list_devices() -> (Vec<DeviceInfo>, Vec<DeviceInfo>) {
    let list = |kind: &str| -> Vec<DeviceInfo> {
        pactl_json(&["list", kind])
            .into_iter()
            .filter_map(|d| {
                let id = d["name"].as_str()?.to_string();
                if id.ends_with(".monitor") || id == EC_SOURCE || id == EC_SINK {
                    return None;
                }
                let name = d["description"].as_str().unwrap_or(&id).to_string();
                Some(DeviceInfo { id, name })
            })
            .collect()
    };
    (list("sources"), list("sinks"))
}

fn pactl_json(args: &[&str]) -> Vec<serde_json::Value> {
    std::process::Command::new("pactl")
        .arg("-f")
        .arg("json")
        .args(args)
        .output()
        .ok()
        .and_then(|o| serde_json::from_slice(&o.stdout).ok())
        .unwrap_or_default()
}

/// Our streams go through the ALSA default device into PipeWire; move them to
/// the chosen source / sink (None = the system default). Runs pactl, so call it
/// off the audio thread.
pub fn route(input: Option<&str>, output: Option<&str>) {
    let ours = |s: &serde_json::Value| s["properties"]["application.name"].as_str() == Some("PipeWire ALSA [wisp]");
    let name_of = |kind: &str| -> std::collections::HashMap<u64, String> {
        pactl_json(&["list", kind])
            .into_iter()
            .filter_map(|d| Some((d["index"].as_u64()?, d["name"].as_str()?.to_string())))
            .collect()
    };
    let default = |what: &str| {
        std::process::Command::new("pactl")
            .arg(what)
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default()
    };
    let jobs = [
        ("source-outputs", "sources", "source", input, "move-source-output", "get-default-source"),
        ("sink-inputs", "sinks", "sink", output, "move-sink-input", "get-default-sink"),
    ];
    for (streams, devices, field, target, mover, def) in jobs {
        let names = name_of(devices);
        let target = target.map(str::to_string).unwrap_or_else(|| default(def));
        if target.is_empty() {
            continue;
        }
        for st in pactl_json(&["list", streams]).iter().filter(|s| ours(s)) {
            let current = st[field].as_u64().and_then(|i| names.get(&i).cloned());
            if current.as_deref() != Some(target.as_str()) {
                if let Some(idx) = st["index"].as_u64() {
                    let _ = std::process::Command::new("pactl").args([mover, &idx.to_string(), &target]).output();
                }
            }
        }
    }
}

pub(crate) fn find_device(_id: Option<&str>, input: bool) -> Option<cpal::Device> {
    let host = cpal::default_host();
    if input { host.default_input_device() } else { host.default_output_device() }
}

/// The PipeWire source we capture from (the chosen one or the default), when
/// it is already an RNNoise filter: no point denoising twice.
pub fn rnnoise_source(input: Option<&str>) -> Option<String> {
    let name = match input {
        Some(i) => i.to_string(),
        None => {
            let out = std::process::Command::new("pactl").arg("get-default-source").output().ok()?;
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        }
    };
    name.to_lowercase().contains("rnnoise").then_some(name)
}

/// Whether the system echo canceller can be used here.
pub const HAS_ECHO_CANCEL: bool = true;
/// Whether other apps can be turned down here.
pub const HAS_DUCKING: bool = true;

pub const EC_SOURCE: &str = "wisp_echo_source";
pub const EC_SINK: &str = "wisp_echo_sink";

fn pactl(args: &[&str]) -> String {
    std::process::Command::new("pactl")
        .args(args)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

/// Load PipeWire's echo canceller around the chosen (or default) devices, or
/// unload it. Reloads when the devices changed. Returns whether it's running.
pub fn echo_cancel(on: bool, input: Option<&str>, output: Option<&str>) -> bool {
    // routing runs on threads; two at once would load two cancellers
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // the JSON listing has no module index; the short one does
    let ours: Vec<(u64, String)> = pactl(&["list", "modules", "short"])
        .lines()
        .filter_map(|l| {
            let mut cols = l.splitn(3, '\t');
            let idx = cols.next()?.trim().parse().ok()?;
            let name = cols.next()?;
            let args = cols.next().unwrap_or("").to_string();
            (name == "module-echo-cancel" && args.contains(EC_SOURCE)).then_some((idx, args))
        })
        .collect();
    if !on {
        for (idx, _) in ours {
            pactl(&["unload-module", &idx.to_string()]);
        }
        return false;
    }
    let source = input.map(str::to_string).unwrap_or_else(|| pactl(&["get-default-source"]));
    let sink = output.map(str::to_string).unwrap_or_else(|| pactl(&["get-default-sink"]));
    // the canceller can't sit on itself (it may have become the default)
    if source.is_empty() || sink.is_empty() || source == EC_SOURCE || sink == EC_SINK {
        return !ours.is_empty();
    }
    let wanted = format!("source_master={source} sink_master={sink}");
    if ours.iter().any(|(_, args)| args.contains(&wanted)) {
        return true;
    }
    for (idx, _) in ours {
        pactl(&["unload-module", &idx.to_string()]);
    }
    let args = format!(
        "aec_method=webrtc source_name={EC_SOURCE} sink_name={EC_SINK} {wanted} \
         source_properties=device.description=\"Wisp echo cancel\" sink_properties=device.description=\"Wisp echo cancel\""
    );
    let mut cmd = vec!["load-module", "module-echo-cancel"];
    let parts: Vec<String> = shell_split(&args);
    cmd.extend(parts.iter().map(String::as_str));
    !pactl(&cmd).is_empty()
}

/// Split on spaces, keeping "quoted parts" together (quotes stay, pactl wants them).
fn shell_split(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for c in s.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                cur.push(c);
            }
            ' ' if !quoted => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            _ => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Other apps' playback we turned down: stream index -> (their volume before,
/// the volume we set), both in percent.
pub type Ducked = std::collections::HashMap<u64, (u32, u32)>;

/// Turn other apps down to `level` (0..1) of their volume, or restore them.
/// A stream whose volume changed since we set it is left alone on restore.
pub fn duck(on: bool, level: f32, ducked: &mut Ducked) {
    let percent = |s: &serde_json::Value| -> Option<u32> {
        s["volume"].as_object()?.values().filter_map(|c| c["value_percent"].as_str()?.trim_end_matches('%').parse().ok()).max()
    };
    let streams = pactl_json(&["list", "sink-inputs"]);
    if on {
        for s in &streams {
            let app = s["properties"]["application.name"].as_str().unwrap_or("");
            let Some(idx) = s["index"].as_u64() else { continue };
            // not us, not our notification sounds
            if app == "PipeWire ALSA [wisp]" || app == "pw-play" || ducked.contains_key(&idx) {
                continue;
            }
            let Some(before) = percent(s) else { continue };
            let after = (before as f32 * level).round() as u32;
            pactl(&["set-sink-input-volume", &idx.to_string(), &format!("{after}%")]);
            ducked.insert(idx, (before, after));
        }
    } else {
        for (idx, (before, after)) in ducked.drain() {
            let now = streams.iter().find(|s| s["index"].as_u64() == Some(idx)).and_then(percent);
            if now == Some(after) {
                pactl(&["set-sink-input-volume", &idx.to_string(), &format!("{before}%")]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    /// Needs PipeWire; run with `cargo test -- --ignored duck`.
    #[test]
    #[ignore]
    fn duck_and_restore() {
        let dir = std::env::temp_dir().join("wisp-duck-test");
        std::fs::create_dir_all(&dir).unwrap();
        let wav = dir.join("silence.wav");
        // 8 s of silence, 48 kHz mono 16-bit
        let data = vec![0u8; 48_000 * 2 * 8];
        let mut f = Vec::new();
        f.extend(b"RIFF");
        f.extend((36 + data.len() as u32).to_le_bytes());
        f.extend(b"WAVEfmt ");
        f.extend(16u32.to_le_bytes());
        f.extend(1u16.to_le_bytes());
        f.extend(1u16.to_le_bytes());
        f.extend(48_000u32.to_le_bytes());
        f.extend(96_000u32.to_le_bytes());
        f.extend(2u16.to_le_bytes());
        f.extend(16u16.to_le_bytes());
        f.extend(b"data");
        f.extend((data.len() as u32).to_le_bytes());
        f.extend(data);
        std::fs::write(&wav, f).unwrap();
        let mut child = std::process::Command::new("pw-play")
            .args(["--properties", "{ application.name = wisp-duck-test }"])
            .arg(&wav)
            .spawn()
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(800));
        let volume = || {
            super::pactl_json(&["list", "sink-inputs"])
                .into_iter()
                .find(|s| s["properties"]["application.name"].as_str() == Some("wisp-duck-test"))
                .and_then(|s| {
                    let ch = s["volume"].as_object()?.values().next()?.clone();
                    ch["value_percent"].as_str().map(str::to_string)
                })
        };
        assert_eq!(volume().as_deref(), Some("100%"));
        let mut ducked = super::Ducked::new();
        super::duck(true, 0.3, &mut ducked);
        assert_eq!(volume().as_deref(), Some("30%"));
        super::duck(false, 0.3, &mut ducked);
        assert_eq!(volume().as_deref(), Some("100%"));
        let _ = child.kill();
    }
}
