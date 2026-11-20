//! Shared audio/Bluetooth model. Both the macOS backends and the UI code against
//! these types and traits; `mock` provides an in-memory implementation for tests
//! and UI development.

/// Apple Accessory Protocol (AirPods noise control / battery) packets; pure, tested everywhere.
pub mod aap;
pub mod mock;

#[cfg(target_os = "macos")]
pub mod bluetooth;
#[cfg(target_os = "macos")]
pub mod coreaudio;
#[cfg(target_os = "macos")]
pub mod tap_mirror;

/// Linux backends (PulseAudio / PipeWire via `pactl`, BlueZ via `bluetoothctl`).
#[cfg(target_os = "linux")]
pub mod linux;
/// Pure parsers for `pactl` / `bluetoothctl` output (no OS calls; tested on every host).
#[cfg(any(target_os = "linux", test))]
pub mod parse_linux;
/// Windows backends (WASAPI / IPolicyConfig / WinRT Bluetooth) + software mirror.
#[cfg(target_os = "windows")]
pub mod windows;
/// Pure helpers for the Windows backend: sample conversion, resampling, device-path parsing.
#[cfg(any(target_os = "windows", test))]
pub mod windows_util;

use serde::{Deserialize, Serialize};

/// CoreAudio `AudioObjectID`.
pub type DeviceId = u32;

#[derive(Debug, thiserror::Error, Clone, PartialEq)]
pub enum AudioError {
    #[error("device {0} not found")]
    NotFound(String),
    #[error("CoreAudio call `{call}` failed with OSStatus {status}")]
    OsStatus { call: &'static str, status: i32 },
    #[error("Bluetooth: {0}")]
    Bluetooth(String),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, AudioError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Transport {
    BuiltIn,
    Bluetooth,
    Virtual,
    /// Aggregate / Multi-Output device (CoreAudio "aggregate" transport).
    Aggregate,
    Other,
}

/// An audio device that has output channels.
#[derive(Debug, Clone, PartialEq)]
pub struct OutputDevice {
    pub id: DeviceId,
    /// Persistent CoreAudio UID (e.g. "02-2A-3B-4C-5D-6E:output" or "~:AMS2_StackedOutput:0").
    pub uid: String,
    pub name: String,
    pub transport: Transport,
    /// Current output volume 0.0..=1.0 (average of channels), None if not settable/readable.
    pub volume: Option<f32>,
    /// True if this is the current system default output.
    pub is_default: bool,
    /// For Multi-Output / aggregate devices: UIDs of member devices (empty otherwise).
    pub subdevice_uids: Vec<String>,
}

impl OutputDevice {
    pub fn is_multi_output(&self) -> bool {
        self.transport == Transport::Aggregate && !self.subdevice_uids.is_empty()
    }
}

/// Everything the UI needs to read/write about audio outputs.
pub trait AudioBackend: Send + Sync + 'static {
    /// All devices with at least one output channel.
    fn output_devices(&self) -> Result<Vec<OutputDevice>>;
    /// Make `id` the default output (and system/alert output).
    fn set_default_output(&self, id: DeviceId) -> Result<()>;
    /// Set output volume (0.0..=1.0, clamped) on every settable channel of `id`.
    fn set_volume(&self, id: DeviceId, volume: f32) -> Result<()>;
    /// Create a Multi-Output (stacked aggregate) device containing `member_uids`.
    /// The first member is the clock source; drift correction is enabled on the rest.
    fn create_multi_output(&self, name: &str, member_uids: &[String]) -> Result<DeviceId>;
    /// If the default *input* is a Bluetooth device, switch it to the built-in
    /// microphone and return the old input's name. Using AirPods as a microphone puts
    /// them in low-quality call mode, which keeps knocking them out of a Multi-Output
    /// device (and makes the system flip the output between them). Default: no-op.
    fn avoid_bluetooth_input(&self) -> Result<Option<String>> {
        Ok(None)
    }
    /// The OS can copy system audio to extra outputs while the system output stays on
    /// one pair (macOS 14.2+ process taps). When true, "listen together" uses
    /// [`AudioBackend::start_mirror`] instead of switching to a Multi-Output device.
    fn mirror_supported(&self) -> bool {
        false
    }
    /// Copy the system mix to exactly `target_uids` (replacing any running mirror).
    fn start_mirror(&self, _target_uids: &[String]) -> Result<()> {
        Err(AudioError::Other(
            "mirroring isn't available on this system".into(),
        ))
    }
    /// Stop mirroring (no-op if not running).
    fn stop_mirror(&self) {}
    /// Outputs currently being fed by the mirror (None = not running).
    fn mirror_targets(&self) -> Option<Vec<String>> {
        None
    }
    /// Remove a multi-output that Duet itself created (see [`is_duet_group_uid`]).
    /// Backends that can't remove groups keep the default no-op.
    fn remove_multi_output(&self, _id: DeviceId) -> Result<()> {
        Ok(())
    }
}

/// True for multi-output UIDs Duet creates on each platform (never the user's own
/// Audio MIDI Setup devices), so Duet only ever cleans up after itself.
pub fn is_duet_group_uid(uid: &str) -> bool {
    uid.starts_with("local.duet.multi.") || uid.starts_with("duet_") || uid == "duet:mirror"
}

/// A paired Bluetooth device.
#[derive(Debug, Clone, PartialEq)]
pub struct BluetoothDevice {
    /// "AA-BB-CC-DD-EE-FF" form (matches the prefix of CoreAudio Bluetooth UIDs).
    pub address: String,
    pub name: String,
    pub connected: bool,
    /// Headphones / audio sink (by class-of-device major class = audio).
    pub is_audio: bool,
}

pub trait BluetoothBackend: Send + Sync + 'static {
    fn paired_devices(&self) -> Result<Vec<BluetoothDevice>>;
    fn connect(&self, address: &str) -> Result<()>;
    fn disconnect(&self, address: &str) -> Result<()>;
    /// Extra controls/status for a *connected* device (AirPods-style noise control,
    /// battery). `Ok(None)` when the device or platform offers nothing extra.
    /// Must be quick (well under a second); called on every poll for each connected device.
    fn headphone_info(&self, _address: &str) -> Result<Option<HeadphoneInfo>> {
        Ok(None)
    }
    /// Switch noise control. Only called with a mode listed in `supported_modes`.
    fn set_listening_mode(&self, _address: &str, _mode: ListeningMode) -> Result<()> {
        Err(AudioError::Other(
            "noise control isn't available on this platform".into(),
        ))
    }
}

/// AirPods / Beats noise-control mode. Raw values are Apple's (shared by the macOS
/// IOBluetooth `listeningMode` property and the AAP protocol).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ListeningMode {
    Off,
    NoiseCancellation,
    Transparency,
    Adaptive,
}

impl ListeningMode {
    /// Display order, matching Control Center.
    pub const ALL: [ListeningMode; 4] = [
        ListeningMode::Off,
        ListeningMode::Transparency,
        ListeningMode::Adaptive,
        ListeningMode::NoiseCancellation,
    ];

    pub fn from_raw(v: u8) -> Option<Self> {
        match v {
            1 => Some(Self::Off),
            2 => Some(Self::NoiseCancellation),
            3 => Some(Self::Transparency),
            4 => Some(Self::Adaptive),
            _ => None,
        }
    }

    pub fn raw(self) -> u8 {
        match self {
            Self::Off => 1,
            Self::NoiseCancellation => 2,
            Self::Transparency => 3,
            Self::Adaptive => 4,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::NoiseCancellation => "Noise Cancellation",
            Self::Transparency => "Transparency",
            Self::Adaptive => "Adaptive",
        }
    }
}

/// Battery levels in percent; `None` when the device doesn't report that part.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Battery {
    pub left: Option<u8>,
    pub right: Option<u8>,
    pub case: Option<u8>,
    /// Single-battery devices (over-ear headphones, speakers).
    pub single: Option<u8>,
}

impl Battery {
    pub fn is_empty(&self) -> bool {
        self.left.is_none() && self.right.is_none() && self.case.is_none() && self.single.is_none()
    }
}

/// Extras for a connected headphone.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HeadphoneInfo {
    pub listening_mode: Option<ListeningMode>,
    /// Modes the device can switch to, in [`ListeningMode::ALL`] order. Empty = no noise control.
    pub supported_modes: Vec<ListeningMode>,
    pub battery: Battery,
}

impl HeadphoneInfo {
    pub fn is_empty(&self) -> bool {
        self.supported_modes.is_empty() && self.battery.is_empty()
    }
}

/// Apple product IDs (Bluetooth PnP) of AirPods/Beats models with Adaptive Audio.
/// AirPods Pro 2 (Lightning 0x2014, USB-C 0x2024), AirPods Pro 3 (0x2027),
/// AirPods 4 with ANC (0x201B).
pub fn supports_adaptive(product_id: u16) -> bool {
    matches!(product_id, 0x2014 | 0x2024 | 0x2027 | 0x201B)
}

/// Build the supported-mode list from capability flags, in display order.
pub fn supported_modes(anc: bool, transparency: bool, adaptive: bool) -> Vec<ListeningMode> {
    if !anc && !transparency {
        return Vec::new();
    }
    ListeningMode::ALL
        .into_iter()
        .filter(|m| match m {
            ListeningMode::Off => true,
            ListeningMode::NoiseCancellation => anc,
            ListeningMode::Transparency => transparency,
            ListeningMode::Adaptive => adaptive && anc && transparency,
        })
        .collect()
}

/// Battery percent from Apple's encoding: 1..=100 valid, 0/>100 = not reported.
pub fn battery_percent(raw: u8) -> Option<u8> {
    (1..=100).contains(&raw).then_some(raw)
}

/// Normalize a Bluetooth address to "AA-BB-CC-DD-EE-FF" (uppercase, dashes).
pub fn normalize_address(addr: &str) -> String {
    addr.trim().replace([':', '-'], "-").to_uppercase()
}

/// The Bluetooth address portion of an output device UID, if any. Understands:
/// - macOS CoreAudio: "02-2A-3B-4C-5D-6E:output" -> Some("02-2A-3B-4C-5D-6E")
/// - Linux PulseAudio/PipeWire sink names: "bluez_output.02_2A_3B_4C_5D_6E.1",
///   "bluez_sink.02_2A_3B_4C_5D_6E.a2dp_sink" -> Some("02-2A-3B-4C-5D-6E")
/// - Windows (Duet's own UID form): "{0.0.0.00000000}.{guid}|02-2A-3B-4C-5D-6E"
pub fn address_from_uid(uid: &str) -> Option<String> {
    fn dashed_mac(s: &str) -> Option<String> {
        let parts: Vec<&str> = s.split(['-', '_', ':']).collect();
        (parts.len() == 6
            && parts
                .iter()
                .all(|p| p.len() == 2 && p.chars().all(|c| c.is_ascii_hexdigit())))
        .then(|| parts.join("-").to_uppercase())
    }
    // Linux: bluez_output.<MAC>.<n> / bluez_sink.<MAC>.<profile>
    for prefix in [
        "bluez_output.",
        "bluez_sink.",
        "bluez_input.",
        "bluez_source.",
    ] {
        if let Some(rest) = uid.strip_prefix(prefix) {
            return dashed_mac(rest.split('.').next()?);
        }
    }
    // Windows: "<endpoint id>|<MAC>"
    if let Some((_, mac)) = uid.rsplit_once('|') {
        return dashed_mac(mac);
    }
    // macOS: "<MAC>:output" / "<MAC>:input"
    let head = uid.split(':').next()?;
    let parts: Vec<&str> = head.split('-').collect();
    (parts.len() == 6
        && parts
            .iter()
            .all(|p| p.len() == 2 && p.chars().all(|c| c.is_ascii_hexdigit())))
    .then(|| head.to_uppercase())
}

/// The Bluetooth address ("AA-BB-CC-DD-EE-FF") of an output device on any platform,
/// derived from its UID (see [`address_from_uid`]).
pub fn bluetooth_address_of(device: &OutputDevice) -> Option<String> {
    address_from_uid(&device.uid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listening_modes() {
        for m in ListeningMode::ALL {
            assert_eq!(ListeningMode::from_raw(m.raw()), Some(m));
        }
        assert_eq!(ListeningMode::from_raw(0), None);
        use ListeningMode::*;
        assert_eq!(
            supported_modes(true, true, true),
            [Off, Transparency, Adaptive, NoiseCancellation]
        );
        assert_eq!(
            supported_modes(true, true, false),
            [Off, Transparency, NoiseCancellation]
        );
        assert_eq!(supported_modes(false, false, true), []);
        assert!(supports_adaptive(0x2014) && !supports_adaptive(0x200E));
        assert_eq!(battery_percent(0), None);
        assert_eq!(battery_percent(57), Some(57));
        assert_eq!(battery_percent(255), None);
    }

    #[test]
    fn address_parsing() {
        assert_eq!(
            address_from_uid("02-2A-3B-4C-5D-6E:output").as_deref(),
            Some("02-2A-3B-4C-5D-6E")
        );
        assert_eq!(address_from_uid("~:AMS2_StackedOutput:0"), None);
        assert_eq!(address_from_uid("BuiltInSpeakerDevice"), None);
        // Linux sink names
        assert_eq!(
            address_from_uid("bluez_output.02_2A_3B_4C_5D_6E.1").as_deref(),
            Some("02-2A-3B-4C-5D-6E")
        );
        assert_eq!(
            address_from_uid("bluez_sink.02_2a_3b_4c_5d_6e.a2dp_sink").as_deref(),
            Some("02-2A-3B-4C-5D-6E")
        );
        assert_eq!(
            address_from_uid("alsa_output.pci-0000_00_1f.3.analog-stereo"),
            None
        );
        assert_eq!(address_from_uid("duet_1"), None);
        // Windows endpoint ids
        assert_eq!(
            address_from_uid(
                "{0.0.0.00000000}.{1b9e2b6c-5b0a-4d3e-9a2c-1f3c9e0d7a11}|02-1B-2C-3D-4E-5F"
            )
            .as_deref(),
            Some("02-1B-2C-3D-4E-5F")
        );
        assert_eq!(
            address_from_uid("{0.0.0.00000000}.{1b9e2b6c-5b0a-4d3e-9a2c-1f3c9e0d7a11}"),
            None
        );
        let d = OutputDevice {
            id: 1,
            uid: "bluez_output.02_1B_2C_3D_4E_5F.1".into(),
            name: "x".into(),
            transport: Transport::Bluetooth,
            volume: None,
            is_default: false,
            subdevice_uids: vec![],
        };
        assert_eq!(
            bluetooth_address_of(&d).as_deref(),
            Some("02-1B-2C-3D-4E-5F")
        );
        assert_eq!(normalize_address("02:3c:4d:5e:6f:70"), "02-3C-4D-5E-6F-70");
    }
}
