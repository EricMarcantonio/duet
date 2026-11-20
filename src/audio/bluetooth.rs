//! IOBluetooth implementation of [`BluetoothBackend`] (macOS only).
//!
//! Uses raw `objc2` message sends against `IOBluetoothDevice` (no generated bindings
//! needed for the handful of methods we use). All `unsafe` is confined to [`objc`].
//!
//! **Blocking:** [`BluetoothBackend::connect`] calls `-[IOBluetoothDevice openConnection]`,
//! which is synchronous and can block for several seconds (until the baseband connection
//! succeeds or times out). Call it off the UI thread (e.g. `tokio::task::spawn_blocking`
//! or a plain `std::thread`). `disconnect` and `paired_devices` are quick.
//!
//! **Permissions:** IOBluetooth is gated by the Bluetooth TCC permission. Before touching
//! IOBluetooth we query `CBManager.authorization` and return a clean
//! [`AudioError::Bluetooth`] if access is denied/restricted. While authorization is still
//! "not determined", the *first* IOBluetooth call blocks inside
//! `IOBluetoothCoreBluetoothCoordinator init` until TCC answers — for an unbundled CLI
//! binary that can be forever. So the first call runs on a helper thread with a
//! [`FIRST_ACCESS_TIMEOUT`]; on timeout we return [`AudioError::Bluetooth`] and later calls
//! fail fast until that helper thread unblocks (e.g. the user answers the prompt). A bundled `.app` must carry
//! `NSBluetoothAlwaysUsageDescription` in its Info.plist or macOS will kill it on first
//! access.

use super::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// How long the first IOBluetooth call may take before we report a permission problem.
pub const FIRST_ACCESS_TIMEOUT: Duration = Duration::from_secs(5);

/// Set once IOBluetooth has answered at least one call in this process.
static READY: AtomicBool = AtomicBool::new(false);
/// Set while a warm-up thread is (possibly forever) blocked in IOBluetooth.
static WARMING: AtomicBool = AtomicBool::new(false);

/// IOBluetooth-backed Bluetooth backend. Stateless.
#[derive(Debug, Default, Clone, Copy)]
pub struct MacBluetooth;

/// Bluetooth TCC authorization state (`CBManagerAuthorization`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BluetoothAuth {
    NotDetermined,
    Restricted,
    Denied,
    Allowed,
    Unknown(isize),
}

impl BluetoothAuth {
    pub fn from_raw(v: isize) -> Self {
        match v {
            0 => Self::NotDetermined,
            1 => Self::Restricted,
            2 => Self::Denied,
            3 => Self::Allowed,
            other => Self::Unknown(other),
        }
    }
}

/// `kBluetoothDeviceClassMajorAudio`.
pub const DEVICE_CLASS_MAJOR_AUDIO: u32 = 0x04;

/// Is this an audio device? By class-of-device major class, falling back to the name.
pub fn is_audio_device(class_major: u32, name: &str) -> bool {
    class_major == DEVICE_CLASS_MAJOR_AUDIO || name.contains("AirPods") || name.contains("Beats")
}

/// Human description of a non-zero `IOReturn`.
pub fn io_return_message(code: i32) -> String {
    // Common IOReturn values (IOKit/IOReturn.h), as unsigned hex.
    let what = match code as u32 {
        0xE000_02BC => "general error",
        0xE000_02C0 => "no device",
        0xE000_02C2 => "not permitted",
        0xE000_02C5 => "busy",
        0xE000_02D6 => "timeout",
        0xE000_02D8 => "not ready",
        0xE000_02E2 => "not attached",
        _ => "error",
    };
    format!("{what} (IOReturn {:#010x})", code as u32)
}

mod objc {
    use super::{AudioError, BluetoothAuth, Result};
    use objc2::msg_send;
    use objc2::rc::{Retained, autoreleasepool};
    use objc2::runtime::{AnyClass, AnyObject, Bool, Sel};
    use objc2::sel;
    use objc2_foundation::{NSArray, NSString};

    #[link(name = "IOBluetooth", kind = "framework")]
    unsafe extern "C" {}
    #[link(name = "CoreBluetooth", kind = "framework")]
    unsafe extern "C" {}

    /// Raw data read from one `IOBluetoothDevice`.
    pub struct RawDevice {
        pub address: String,
        pub name: String,
        pub connected: bool,
        pub class_major: u32,
    }

    fn class(name: &std::ffi::CStr) -> Result<&'static AnyClass> {
        AnyClass::get(name)
            .ok_or_else(|| AudioError::Bluetooth(format!("Objective-C class {name:?} unavailable")))
    }

    pub fn authorization() -> BluetoothAuth {
        let Ok(cls) = class(c"CBManager") else {
            return BluetoothAuth::Unknown(-1);
        };
        // SAFETY: +[CBManager authorization] (macOS 10.15+) returns an NSInteger enum.
        let raw: isize = unsafe { msg_send![cls, authorization] };
        BluetoothAuth::from_raw(raw)
    }

    fn string(s: Option<Retained<NSString>>) -> String {
        s.map(|s| s.to_string()).unwrap_or_default()
    }

    fn read(dev: &AnyObject) -> RawDevice {
        // SAFETY: `dev` is an IOBluetoothDevice; these are documented getters with these
        // return types (NSString*, NSString*, BOOL, BluetoothDeviceClassMajor = UInt32).
        unsafe {
            RawDevice {
                address: string(msg_send![dev, addressString]),
                name: string(msg_send![dev, name]),
                connected: {
                    let b: Bool = msg_send![dev, isConnected];
                    b.as_bool()
                },
                class_major: msg_send![dev, deviceClassMajor],
            }
        }
    }

    fn paired() -> Result<Option<Retained<NSArray<AnyObject>>>> {
        let cls = class(c"IOBluetoothDevice")?;
        // SAFETY: +[IOBluetoothDevice pairedDevices] returns NSArray<IOBluetoothDevice*>* (nullable).
        Ok(unsafe { msg_send![cls, pairedDevices] })
    }

    pub fn paired_devices() -> Result<Vec<RawDevice>> {
        autoreleasepool(|_| {
            let Some(arr) = paired()? else {
                return Ok(Vec::new());
            };
            Ok(arr.iter().map(|d| read(&d)).collect())
        })
    }

    /// Run `f` on the paired device whose normalized address equals `address`.
    pub fn with_device<T>(address: &str, f: impl FnOnce(&AnyObject) -> T) -> Result<Option<T>> {
        autoreleasepool(|_| {
            let Some(arr) = paired()? else {
                return Ok(None);
            };
            for d in arr.iter() {
                // SAFETY: documented getter returning NSString*.
                let a = string(unsafe { msg_send![&*d, addressString] });
                if super::normalize_address(&a) == address {
                    return Ok(Some(f(&d)));
                }
            }
            Ok(None)
        })
    }

    pub fn open_connection(dev: &AnyObject) -> i32 {
        // SAFETY: -[IOBluetoothDevice openConnection] returns IOReturn (int). Blocks.
        unsafe { msg_send![dev, openConnection] }
    }

    pub fn close_connection(dev: &AnyObject) -> i32 {
        // SAFETY: -[IOBluetoothDevice closeConnection] returns IOReturn (int).
        unsafe { msg_send![dev, closeConnection] }
    }

    /// AirPods/Beats extras via IOBluetooth's private (undocumented, but stable since
    /// macOS 10.15 and what Control Center uses) properties. Every selector is checked
    /// with -respondsToSelector: first, so a macOS that drops one just yields "unknown".
    /// Type encodings verified on macOS 27: listeningMode `C`, isANCSupported `B`,
    /// batteryPercent* `C`, productID `S`, setListeningMode: `v@:C`.
    #[derive(Debug, Default)]
    pub struct RawExtras {
        pub mode: u8,
        pub anc: bool,
        pub transparency: bool,
        pub product_id: u16,
        pub multi_battery: bool,
        pub left: u8,
        pub right: u8,
        pub case: u8,
        pub single: u8,
    }

    fn responds(dev: &AnyObject, sel: Sel) -> bool {
        // SAFETY: -respondsToSelector: is NSObject API returning BOOL.
        let b: Bool = unsafe { msg_send![dev, respondsToSelector: sel] };
        b.as_bool()
    }

    pub fn extras(dev: &AnyObject) -> RawExtras {
        let mut x = RawExtras::default();
        // SAFETY (all below): each getter exists (checked) and has the encoding noted above.
        unsafe {
            if responds(dev, sel!(listeningMode)) {
                x.mode = msg_send![dev, listeningMode];
            }
            if responds(dev, sel!(isANCSupported)) {
                x.anc = msg_send![dev, isANCSupported];
            }
            if responds(dev, sel!(isTransparencySupported)) {
                x.transparency = msg_send![dev, isTransparencySupported];
            }
            if responds(dev, sel!(productID)) {
                x.product_id = msg_send![dev, productID];
            }
            if responds(dev, sel!(isMultiBatteryDevice)) {
                x.multi_battery = msg_send![dev, isMultiBatteryDevice];
            }
            if responds(dev, sel!(batteryPercentLeft)) {
                x.left = msg_send![dev, batteryPercentLeft];
            }
            if responds(dev, sel!(batteryPercentRight)) {
                x.right = msg_send![dev, batteryPercentRight];
            }
            if responds(dev, sel!(batteryPercentCase)) {
                x.case = msg_send![dev, batteryPercentCase];
            }
            if responds(dev, sel!(batteryPercentSingle)) {
                x.single = msg_send![dev, batteryPercentSingle];
            }
        }
        x
    }

    /// Returns false if this macOS has no setter.
    pub fn set_listening_mode(dev: &AnyObject, mode: u8) -> bool {
        if !responds(dev, sel!(setListeningMode:)) {
            return false;
        }
        // SAFETY: -[IOBluetoothDevice setListeningMode:(uint8_t)] returns void.
        unsafe {
            let _: () = msg_send![dev, setListeningMode: mode];
        }
        true
    }
}

/// Turn raw IOBluetooth values into [`HeadphoneInfo`] (pure; unit-tested).
pub fn headphone_info_from_raw(x: &objc::RawExtras) -> Option<HeadphoneInfo> {
    let mode = ListeningMode::from_raw(x.mode);
    let modes = if x.anc || x.transparency || mode.is_some() {
        supported_modes(
            x.anc || mode == Some(ListeningMode::NoiseCancellation),
            x.transparency || mode == Some(ListeningMode::Transparency),
            supports_adaptive(x.product_id) || mode == Some(ListeningMode::Adaptive),
        )
    } else {
        Vec::new()
    };
    let battery = if x.multi_battery || x.left > 0 || x.right > 0 {
        Battery {
            left: battery_percent(x.left),
            right: battery_percent(x.right),
            case: battery_percent(x.case),
            single: None,
        }
    } else {
        Battery {
            single: battery_percent(x.single),
            ..Battery::default()
        }
    };
    let info = HeadphoneInfo {
        listening_mode: mode.filter(|m| modes.contains(m)),
        supported_modes: modes,
        battery,
    };
    (!info.is_empty()).then_some(info)
}

impl MacBluetooth {
    pub fn new() -> Self {
        MacBluetooth
    }

    /// Current Bluetooth TCC authorization for this process.
    pub fn authorization(&self) -> BluetoothAuth {
        objc::authorization()
    }

    fn ensure_access(&self) -> Result<()> {
        match self.authorization() {
            BluetoothAuth::Denied => Err(AudioError::Bluetooth(
                "access denied — enable this app under System Settings › Privacy & Security › Bluetooth".into(),
            )),
            BluetoothAuth::Restricted => Err(AudioError::Bluetooth("access restricted by system policy".into())),
            // NotDetermined: the first IOBluetooth call triggers the system prompt.
            _ => self.warm_up(),
        }
    }

    /// Make sure IOBluetooth has initialized without hanging the caller forever.
    fn warm_up(&self) -> Result<()> {
        if READY.load(Ordering::Acquire) {
            return Ok(());
        }
        let pending = || {
            AudioError::Bluetooth(
                "Bluetooth did not respond — macOS is probably waiting for Bluetooth permission \
                 (System Settings › Privacy & Security › Bluetooth)"
                    .into(),
            )
        };
        if WARMING.swap(true, Ordering::AcqRel) {
            return Err(pending());
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("duet-bluetooth-warmup".into())
            .spawn(move || {
                let r = objc::paired_devices().map(|_| ());
                if r.is_ok() {
                    READY.store(true, Ordering::Release);
                }
                WARMING.store(false, Ordering::Release);
                let _ = tx.send(r);
            });
        if let Err(e) = spawned {
            WARMING.store(false, Ordering::Release);
            return Err(AudioError::Bluetooth(format!("cannot spawn thread: {e}")));
        }
        match rx.recv_timeout(FIRST_ACCESS_TIMEOUT) {
            Ok(r) => r,
            Err(_) => Err(pending()),
        }
    }

    fn act(
        &self,
        address: &str,
        verb: &str,
        f: fn(&objc2::runtime::AnyObject) -> i32,
    ) -> Result<()> {
        self.ensure_access()?;
        let addr = normalize_address(address);
        match objc::with_device(&addr, f)? {
            None => Err(AudioError::NotFound(addr)),
            Some(0) => Ok(()),
            Some(code) => Err(AudioError::Bluetooth(format!(
                "{verb} {addr} failed: {}",
                io_return_message(code)
            ))),
        }
    }
}

impl BluetoothBackend for MacBluetooth {
    fn paired_devices(&self) -> Result<Vec<BluetoothDevice>> {
        self.ensure_access()?;
        Ok(objc::paired_devices()?
            .into_iter()
            .filter(|d| !d.address.is_empty())
            .map(|d| BluetoothDevice {
                address: normalize_address(&d.address),
                is_audio: is_audio_device(d.class_major, &d.name),
                name: d.name,
                connected: d.connected,
            })
            .collect())
    }

    /// Blocking (seconds). Call off the UI thread.
    fn connect(&self, address: &str) -> Result<()> {
        self.act(address, "connect", objc::open_connection)
    }

    fn disconnect(&self, address: &str) -> Result<()> {
        self.act(address, "disconnect", objc::close_connection)
    }

    fn headphone_info(&self, address: &str) -> Result<Option<HeadphoneInfo>> {
        self.ensure_access()?;
        let addr = normalize_address(address);
        match objc::with_device(&addr, objc::extras)? {
            None => Err(AudioError::NotFound(addr)),
            Some(x) => Ok(headphone_info_from_raw(&x)),
        }
    }

    fn set_listening_mode(&self, address: &str, mode: ListeningMode) -> Result<()> {
        self.ensure_access()?;
        let addr = normalize_address(address);
        match objc::with_device(&addr, |d| objc::set_listening_mode(d, mode.raw()))? {
            None => Err(AudioError::NotFound(addr)),
            Some(true) => Ok(()),
            Some(false) => Err(AudioError::Bluetooth(
                "this version of macOS doesn't expose noise control".into(),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extras_mapping() {
        use objc::RawExtras;
        // AirPods Pro 2 in Transparency, buds 80/75, case 40.
        let pro2 = RawExtras {
            mode: 3,
            anc: true,
            transparency: true,
            product_id: 0x2014,
            multi_battery: true,
            left: 80,
            right: 75,
            case: 40,
            single: 0,
        };
        let i = headphone_info_from_raw(&pro2).unwrap();
        assert_eq!(i.listening_mode, Some(ListeningMode::Transparency));
        assert_eq!(i.supported_modes.len(), 4);
        assert_eq!(
            i.battery,
            Battery {
                left: Some(80),
                right: Some(75),
                case: Some(40),
                single: None
            }
        );
        // AirPods Pro (1st gen): no Adaptive, case not reported.
        let pro1 = RawExtras {
            product_id: 0x200E,
            case: 0,
            ..pro2
        };
        let i = headphone_info_from_raw(&pro1).unwrap();
        assert!(!i.supported_modes.contains(&ListeningMode::Adaptive));
        assert_eq!(i.battery.case, None);
        // Plain Bluetooth headphones with only a single battery level.
        let other = RawExtras {
            single: 64,
            ..RawExtras::default()
        };
        let i = headphone_info_from_raw(&other).unwrap();
        assert!(i.supported_modes.is_empty());
        assert_eq!(i.battery.single, Some(64));
        // Nothing reported at all.
        assert_eq!(headphone_info_from_raw(&RawExtras::default()), None);
    }

    #[test]
    fn audio_classification() {
        assert!(is_audio_device(0x04, "Headphones"));
        assert!(is_audio_device(0x00, "Alex’s AirPods Pro"));
        assert!(is_audio_device(0x1F, "Beats Studio Buds"));
        assert!(!is_audio_device(0x05, "MX Master 3"));
    }

    #[test]
    fn auth_mapping() {
        assert_eq!(BluetoothAuth::from_raw(0), BluetoothAuth::NotDetermined);
        assert_eq!(BluetoothAuth::from_raw(2), BluetoothAuth::Denied);
        assert_eq!(BluetoothAuth::from_raw(3), BluetoothAuth::Allowed);
        assert_eq!(BluetoothAuth::from_raw(9), BluetoothAuth::Unknown(9));
    }

    #[test]
    fn io_return_text() {
        assert!(io_return_message(0xE000_02D6_u32 as i32).contains("timeout"));
        assert!(io_return_message(0x1234).contains("0x00001234"));
    }

    #[test]
    fn backend_is_send_sync() {
        fn check<T: Send + Sync + 'static>() {}
        check::<MacBluetooth>();
    }
}
