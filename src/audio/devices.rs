//! Devices straight from cpal (CoreAudio, WASAPI). The PipeWire extras (echo
//! canceller, turning other apps down) have no counterpart here yet.

use cpal::traits::{DeviceTrait, HostTrait};

use super::DeviceInfo;

fn infos(devices: impl Iterator<Item = cpal::Device>) -> Vec<DeviceInfo> {
    devices
        .filter_map(|d| {
            let id = d.id().ok()?.to_string();
            let name = d.description().map(|n| n.name().to_string()).unwrap_or_else(|_| id.clone());
            Some(DeviceInfo { id, name })
        })
        .collect()
}

/// (inputs, outputs); the id is cpal's device id.
pub fn list_devices() -> (Vec<DeviceInfo>, Vec<DeviceInfo>) {
    let host = cpal::default_host();
    let inputs = host.input_devices().map(|d| infos(d)).unwrap_or_default();
    let outputs = host.output_devices().map(|d| infos(d)).unwrap_or_default();
    (inputs, outputs)
}

/// The chosen device, else the system default.
pub(crate) fn find_device(id: Option<&str>, input: bool) -> Option<cpal::Device> {
    let host = cpal::default_host();
    let chosen = id.and_then(|id| {
        let mut all: Box<dyn Iterator<Item = cpal::Device>> =
            if input { Box::new(host.input_devices().ok()?) } else { Box::new(host.output_devices().ok()?) };
        all.find(|d| d.id().is_ok_and(|d| d.to_string() == id))
    });
    chosen.or_else(|| if input { host.default_input_device() } else { host.default_output_device() })
}

/// Devices are opened directly, so a new choice takes effect on the next start.
pub fn route(_input: Option<&str>, _output: Option<&str>) {}

pub fn rnnoise_source(_input: Option<&str>) -> Option<String> {
    None
}

pub const EC_SOURCE: &str = "";
pub const EC_SINK: &str = "";

/// Whether the system echo canceller can be used here.
pub const HAS_ECHO_CANCEL: bool = false;
/// Whether other apps can be turned down here.
pub const HAS_DUCKING: bool = false;

pub fn echo_cancel(_on: bool, _input: Option<&str>, _output: Option<&str>) -> bool {
    false
}

pub type Ducked = std::collections::HashMap<u64, (u32, u32)>;

pub fn duck(_on: bool, _level: f32, _ducked: &mut Ducked) {}
