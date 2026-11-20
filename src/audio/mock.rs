//! In-memory backends for tests and UI development (`DUET_MOCK=1`).

use super::*;
use std::sync::Mutex;

pub struct MockAudio {
    pub devices: Mutex<Vec<OutputDevice>>,
}

impl MockAudio {
    /// A typical setup: two AirPods, speakers, a virtual device, and a user-made "Living Room Group".
    pub fn sample() -> Self {
        let d = |id, uid: &str, name: &str, t, v: Option<f32>, def, subs: &[&str]| OutputDevice {
            id,
            uid: uid.into(),
            name: name.into(),
            transport: t,
            volume: v,
            is_default: def,
            subdevice_uids: subs.iter().map(|s| s.to_string()).collect(),
        };
        Self {
            devices: Mutex::new(vec![
                d(
                    10,
                    "02-2A-3B-4C-5D-6E:output",
                    "Alex's AirPod Pro 2's",
                    Transport::Bluetooth,
                    Some(0.52),
                    false,
                    &[],
                ),
                d(
                    11,
                    "02-1B-2C-3D-4E-5F:output",
                    "Sam’s AirPods Pro #2 - Find My",
                    Transport::Bluetooth,
                    Some(0.60),
                    false,
                    &[],
                ),
                d(
                    20,
                    "BuiltInSpeakerDevice",
                    "MacBook Air Speakers",
                    Transport::BuiltIn,
                    Some(0.75),
                    false,
                    &[],
                ),
                d(
                    30,
                    "LoopbackDevice",
                    "Loopback Mixer",
                    Transport::Virtual,
                    Some(1.0),
                    false,
                    &[],
                ),
                d(
                    40,
                    "~:AMS2_StackedOutput:0",
                    "Living Room Group",
                    Transport::Aggregate,
                    None,
                    true,
                    &["02-2A-3B-4C-5D-6E:output", "02-1B-2C-3D-4E-5F:output"],
                ),
            ]),
        }
    }
}

impl AudioBackend for MockAudio {
    fn output_devices(&self) -> Result<Vec<OutputDevice>> {
        Ok(self.devices.lock().unwrap().clone())
    }
    fn set_default_output(&self, id: DeviceId) -> Result<()> {
        let mut ds = self.devices.lock().unwrap();
        if !ds.iter().any(|d| d.id == id) {
            return Err(AudioError::NotFound(id.to_string()));
        }
        for d in ds.iter_mut() {
            d.is_default = d.id == id;
        }
        Ok(())
    }
    fn set_volume(&self, id: DeviceId, volume: f32) -> Result<()> {
        let mut ds = self.devices.lock().unwrap();
        let d = ds
            .iter_mut()
            .find(|d| d.id == id)
            .ok_or(AudioError::NotFound(id.to_string()))?;
        if d.volume.is_none() {
            return Err(AudioError::Other(format!(
                "{} has no volume control",
                d.name
            )));
        }
        d.volume = Some(volume.clamp(0.0, 1.0));
        Ok(())
    }
    fn create_multi_output(&self, name: &str, member_uids: &[String]) -> Result<DeviceId> {
        let mut ds = self.devices.lock().unwrap();
        let id = ds.iter().map(|d| d.id).max().unwrap_or(0) + 1;
        ds.push(OutputDevice {
            id,
            uid: format!("local.duet.multi.{id}"),
            name: name.into(),
            transport: Transport::Aggregate,
            volume: None,
            is_default: false,
            subdevice_uids: member_uids.to_vec(),
        });
        Ok(id)
    }
    fn remove_multi_output(&self, id: DeviceId) -> Result<()> {
        let mut ds = self.devices.lock().unwrap();
        let before = ds.len();
        ds.retain(|d| !(d.id == id && is_duet_group_uid(&d.uid)));
        if ds.len() == before {
            return Err(AudioError::NotFound(id.to_string()));
        }
        Ok(())
    }
}

pub struct MockBluetooth {
    pub devices: Mutex<Vec<BluetoothDevice>>,
    /// Noise-control mode per address (every mock headphone supports all four).
    pub modes: Mutex<std::collections::HashMap<String, ListeningMode>>,
}

impl MockBluetooth {
    pub fn sample() -> Self {
        let b = |a: &str, n: &str, c, audio| BluetoothDevice {
            address: a.into(),
            name: n.into(),
            connected: c,
            is_audio: audio,
        };
        Self {
            devices: Mutex::new(vec![
                b("02-2A-3B-4C-5D-6E", "Alex's AirPod Pro 2's", true, true),
                b(
                    "02-1B-2C-3D-4E-5F",
                    "Sam’s AirPods Pro #2 - Find My",
                    false,
                    true,
                ),
                b("02-3C-4D-5E-6F-70", "Alex’s AirPods Pro", false, true),
                b("11-22-33-44-55-66", "MX Master 3", false, false),
            ]),
            modes: Mutex::new(Default::default()),
        }
    }
    fn set(&self, address: &str, connected: bool) -> Result<()> {
        let addr = normalize_address(address);
        let mut ds = self.devices.lock().unwrap();
        let d = ds
            .iter_mut()
            .find(|d| d.address == addr)
            .ok_or(AudioError::NotFound(addr))?;
        d.connected = connected;
        Ok(())
    }
}

impl BluetoothBackend for MockBluetooth {
    fn paired_devices(&self) -> Result<Vec<BluetoothDevice>> {
        Ok(self.devices.lock().unwrap().clone())
    }
    fn connect(&self, address: &str) -> Result<()> {
        self.set(address, true)
    }
    fn disconnect(&self, address: &str) -> Result<()> {
        self.set(address, false)
    }
    fn headphone_info(&self, address: &str) -> Result<Option<HeadphoneInfo>> {
        let addr = normalize_address(address);
        let mode = self
            .modes
            .lock()
            .unwrap()
            .get(&addr)
            .copied()
            .unwrap_or(ListeningMode::NoiseCancellation);
        // Stable fake battery levels derived from the address.
        let seed = addr
            .bytes()
            .fold(7u32, |a, b| a.wrapping_mul(31).wrapping_add(b as u32));
        let level = |k: u32| Some((35 + (seed / k) % 65) as u8);
        Ok(Some(HeadphoneInfo {
            listening_mode: Some(mode),
            supported_modes: ListeningMode::ALL.to_vec(),
            battery: Battery {
                left: level(3),
                right: level(5),
                case: level(11),
                single: None,
            },
        }))
    }
    fn set_listening_mode(&self, address: &str, mode: ListeningMode) -> Result<()> {
        self.modes
            .lock()
            .unwrap()
            .insert(normalize_address(address), mode);
        Ok(())
    }
}
