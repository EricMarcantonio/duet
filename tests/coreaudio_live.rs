//! Live tests against the real CoreAudio HAL and IOBluetooth (macOS only).
//!
//! Default tests are read-only except a volume round-trip on the built-in speakers,
//! which restores the original value. Anything that changes routing or creates devices
//! is `#[ignore]`d: run with `cargo test --test coreaudio_live -- --ignored --test-threads=1`.
#![cfg(target_os = "macos")]

use duet::audio::bluetooth::MacBluetooth;
use duet::audio::coreaudio::CoreAudio;
use duet::audio::*;
use std::sync::Mutex;

/// Serializes tests that mutate (or depend on stable) system audio state.
static LOCK: Mutex<()> = Mutex::new(());

fn lock() -> std::sync::MutexGuard<'static, ()> {
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn devices() -> Vec<OutputDevice> {
    CoreAudio.output_devices().expect("output_devices")
}

fn builtin() -> OutputDevice {
    devices()
        .into_iter()
        .find(|d| d.transport == Transport::BuiltIn)
        .expect("no built-in output device")
}

#[test]
fn lists_output_devices() {
    let _g = lock();
    let ds = devices();
    assert!(!ds.is_empty());
    assert!(
        ds.iter().any(|d| d.transport == Transport::BuiltIn),
        "no BuiltIn device: {ds:#?}"
    );
    for d in &ds {
        assert!(!d.uid.is_empty(), "empty uid: {d:?}");
        assert!(!d.name.is_empty(), "empty name: {d:?}");
        if let Some(v) = d.volume {
            assert!((0.0..=1.0).contains(&v), "volume out of range: {d:?}");
        }
        if d.transport == Transport::Aggregate {
            assert!(
                !d.subdevice_uids.is_empty(),
                "aggregate without subdevices: {d:?}"
            );
        } else {
            assert!(d.subdevice_uids.is_empty());
        }
    }
    assert_eq!(
        ds.iter().filter(|d| d.is_default).count(),
        1,
        "exactly one default expected: {ds:#?}"
    );
    assert_eq!(
        ds.iter().find(|d| d.is_default).unwrap().id,
        CoreAudio.default_output().unwrap()
    );
}

#[test]
fn bluetooth_outputs_have_addresses() {
    let _g = lock();
    for d in devices()
        .iter()
        .filter(|d| d.transport == Transport::Bluetooth)
    {
        assert!(
            address_from_uid(&d.uid).is_some(),
            "bluetooth uid without address: {}",
            d.uid
        );
    }
}

#[test]
fn builtin_volume_roundtrip() {
    let _g = lock();
    let dev = builtin();
    let original = dev.volume.expect("built-in speakers should have a volume");
    let target = if original > 0.5 {
        original - 0.2
    } else {
        original + 0.2
    };
    let result = std::panic::catch_unwind(|| {
        CoreAudio.set_volume(dev.id, target).expect("set_volume");
        let got = CoreAudio.volume(dev.id).expect("volume readback");
        assert!((got - target).abs() < 0.02, "set {target}, read back {got}");
    });
    CoreAudio
        .set_volume(dev.id, original)
        .expect("restore volume");
    let restored = CoreAudio.volume(dev.id).unwrap();
    assert!(
        (restored - original).abs() < 0.02,
        "restore failed: {restored} vs {original}"
    );
    result.unwrap();
}

#[test]
fn unknown_device_errors() {
    assert!(matches!(
        CoreAudio.set_volume(0xDEAD_BEEF, 0.5),
        Err(AudioError::NotFound(_))
    ));
    assert!(matches!(
        CoreAudio.set_default_output(0xDEAD_BEEF),
        Err(AudioError::NotFound(_))
    ));
}

#[test]
fn paired_devices_ok_or_clean_error() {
    match MacBluetooth.paired_devices() {
        Ok(ds) => {
            for d in &ds {
                assert_eq!(d.address, normalize_address(&d.address));
                assert_eq!(d.address.len(), 17, "bad address {:?}", d.address);
            }
        }
        Err(AudioError::Bluetooth(msg)) => eprintln!("bluetooth unavailable (ok): {msg}"),
        Err(e) => panic!("unexpected error kind: {e:?}"),
    }
}

#[test]
fn connect_unknown_address_is_not_found() {
    // Address that is certainly not paired: never touches a real device.
    match MacBluetooth.connect("00:00:00:00:00:01") {
        Err(AudioError::NotFound(_)) | Err(AudioError::Bluetooth(_)) => {}
        other => panic!("unexpected: {other:?}"),
    }
}

/// Creates a Multi-Output device with only the built-in speakers, verifies it, destroys it.
#[test]
#[ignore = "creates (and destroys) a system audio device"]
fn create_and_destroy_multi_output() {
    let _g = lock();
    let speakers = builtin();
    let id = CoreAudio
        .create_multi_output(
            "Duet Test Multi-Output",
            std::slice::from_ref(&speakers.uid),
        )
        .expect("create_multi_output");
    let result = std::panic::catch_unwind(|| {
        let d = devices()
            .into_iter()
            .find(|d| d.id == id)
            .expect("new device not listed");
        assert_eq!(d.name, "Duet Test Multi-Output");
        assert_eq!(d.transport, Transport::Aggregate);
        assert!(d.uid.starts_with("local.duet.multi."));
        assert_eq!(d.subdevice_uids, vec![speakers.uid.clone()]);
        assert!(d.is_multi_output());
    });
    CoreAudio.destroy_multi_output(id).expect("destroy");
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert!(
        !devices().iter().any(|d| d.id == id),
        "device still present after destroy"
    );
    result.unwrap();
}

/// Safe: the UID guard rejects non-Duet devices before any HAL call.
#[test]
fn destroy_refuses_foreign_devices() {
    let speakers = builtin();
    assert!(CoreAudio.destroy_multi_output(speakers.id).is_err());
}

/// Switches default output to the built-in speakers and back. Audible blip if
/// something is playing; run deliberately.
#[test]
#[ignore = "changes the default output device (restored afterwards)"]
fn switch_default_output_and_restore() {
    let _g = lock();
    let orig = CoreAudio.default_output().unwrap();
    let orig_sys = CoreAudio.default_system_output().unwrap();
    let target = devices()
        .into_iter()
        .find(|d| d.id != orig)
        .expect("need a second output device");
    let result = std::panic::catch_unwind(|| {
        CoreAudio
            .set_default_output(target.id)
            .expect("set_default_output");
        assert_eq!(CoreAudio.default_output().unwrap(), target.id);
        assert!(
            devices()
                .iter()
                .find(|d| d.id == target.id)
                .unwrap()
                .is_default
        );
    });
    CoreAudio.set_default_output(orig).expect("restore default");
    let _ = CoreAudio.set_default_system_output(orig_sys);
    assert_eq!(CoreAudio.default_output().unwrap(), orig);
    result.unwrap();
}

/// Disconnects then reconnects a paired audio device given by DUET_TEST_BT_ADDR.
#[test]
#[ignore = "disconnects/reconnects real Bluetooth headphones; set DUET_TEST_BT_ADDR"]
fn bluetooth_disconnect_reconnect() {
    let Ok(addr) = std::env::var("DUET_TEST_BT_ADDR") else {
        eprintln!("DUET_TEST_BT_ADDR not set; skipping");
        return;
    };
    MacBluetooth.disconnect(&addr).expect("disconnect");
    std::thread::sleep(std::time::Duration::from_secs(2));
    MacBluetooth.connect(&addr).expect("connect");
    let d = MacBluetooth
        .paired_devices()
        .unwrap()
        .into_iter()
        .find(|d| d.address == normalize_address(&addr));
    assert!(d.unwrap().connected);
}
