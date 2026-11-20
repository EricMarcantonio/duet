//! Mock wiring for `DUET_MOCK=…` so the UI can be exercised without hardware.
//!
//! * `DUET_MOCK=1`       – a sample setup with Sam's AirPods paired but not
//!   connected; "Connect" takes ~2s and then the AirPods appear as an output.
//! * `DUET_MOCK=full`    – both AirPods connected (raw `MockAudio::sample()`).
//! * `DUET_MOCK=nomulti` – no Multi-Output device yet (shows the create flow).
//! * `DUET_MOCK=flaky`   – like `1`, but every Bluetooth connect fails.
//! * `DUET_MOCK=hint`    – both connected, battery readable, noise control not
//!   switchable (current macOS), so the cards point at Control Center.

use super::Backends;
use crate::audio::mock::{MockAudio, MockBluetooth};
use crate::audio::{
    AudioError, BluetoothBackend, BluetoothDevice, HeadphoneInfo, ListeningMode, OutputDevice,
    Result, address_from_uid, normalize_address,
};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Bluetooth mock that, on connect, (re)adds the matching output device to the audio mock.
struct LinkedBluetooth {
    bt: MockBluetooth,
    /// Every connect fails (exercises the error toast).
    flaky: bool,
    audio: Arc<MockAudio>,
    parked: Mutex<Vec<OutputDevice>>,
}

impl BluetoothBackend for LinkedBluetooth {
    fn paired_devices(&self) -> Result<Vec<BluetoothDevice>> {
        self.bt.paired_devices()
    }
    fn connect(&self, address: &str) -> Result<()> {
        std::thread::sleep(Duration::from_millis(2000));
        let addr = normalize_address(address);
        if self.flaky || addr == "02-3C-4D-5E-6F-70" {
            return Err(AudioError::Bluetooth(
                "the device didn’t respond — is it in range and out of its case?".into(),
            ));
        }
        self.bt.connect(&addr)?;
        let mut parked = self.parked.lock().unwrap();
        if let Some(i) = parked
            .iter()
            .position(|d| address_from_uid(&d.uid).as_deref() == Some(addr.as_str()))
        {
            self.audio.devices.lock().unwrap().push(parked.remove(i));
        }
        Ok(())
    }
    fn headphone_info(&self, address: &str) -> Result<Option<HeadphoneInfo>> {
        self.bt.headphone_info(address)
    }
    fn set_listening_mode(&self, address: &str, mode: ListeningMode) -> Result<()> {
        self.bt.set_listening_mode(address, mode)
    }
    fn disconnect(&self, address: &str) -> Result<()> {
        let addr = normalize_address(address);
        self.bt.disconnect(&addr)?;
        let mut devs = self.audio.devices.lock().unwrap();
        if let Some(i) = devs
            .iter()
            .position(|d| address_from_uid(&d.uid).as_deref() == Some(addr.as_str()))
        {
            self.parked.lock().unwrap().push(devs.remove(i));
        }
        Ok(())
    }
}

pub fn mock_backends(variant: &str) -> Backends {
    let audio = Arc::new(MockAudio::sample());
    let mut parked = Vec::new();
    {
        let mut devs = audio.devices.lock().unwrap();
        match variant {
            "full" | "hint" => {}
            // Two headphones Duet has never seen: proposes a new group.
            "new" => {
                for d in devs.iter_mut().filter(|d| d.id == 10 || d.id == 11) {
                    d.uid = d.uid.replace("02-2A", "AA-01").replace("02-1B", "AA-02");
                }
                if let Some(d) = devs.iter_mut().find(|d| d.id == 10) {
                    d.name = "Alex’s WH-1000XM5".into();
                }
                if let Some(d) = devs.iter_mut().find(|d| d.id == 11) {
                    d.name = "Jordan’s AirPods".into();
                }
            }
            "nomulti" => {
                devs.retain(|d| !d.is_multi_output());
                if let Some(d) = devs.iter_mut().find(|d| d.id == 20) {
                    d.is_default = true;
                }
            }
            _ => {
                if let Some(i) = devs.iter().position(|d| d.id == 11) {
                    parked.push(devs.remove(i));
                }
            }
        }
    }
    // `hint` mirrors current macOS: both pairs connected, battery readable,
    // noise control only switchable in Control Center.
    let mut bt = MockBluetooth::sample();
    bt.read_only = variant == "hint";
    if matches!(variant, "full" | "new" | "hint") {
        bt.connect("02-1B-2C-3D-4E-5F").ok();
    }
    Backends {
        audio: audio.clone(),
        bluetooth: Arc::new(LinkedBluetooth {
            bt,
            flaky: variant == "flaky",
            audio,
            parked: Mutex::new(parked),
        }),
    }
}
