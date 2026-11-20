//! CoreAudio implementation of [`AudioBackend`] (macOS only).
//!
//! Hand-written FFI against the CoreAudio HAL (`AudioObject*` C API). All `unsafe`
//! is confined to the small wrappers in [`hal`]; everything above it is safe Rust.
//!
//! All calls are synchronous but fast (sub-millisecond, except
//! `create_multi_output`, which waits up to ~1s for the new device to publish).

use super::*;
use core_foundation::array::CFArray;
use core_foundation::base::{CFType, TCFType};
use core_foundation::dictionary::CFDictionary;
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;

/// The running system-audio mirror (one per process; CoreAudio is a unit struct).
static MIRROR: std::sync::Mutex<Option<super::tap_mirror::Mirror>> = std::sync::Mutex::new(None);

/// CoreAudio-backed audio backend. Stateless; every call queries the HAL.
#[derive(Debug, Default, Clone, Copy)]
pub struct CoreAudio;

/// Build a CoreAudio four-char code (`'dev#'` etc.).
pub const fn fourcc(s: &[u8; 4]) -> u32 {
    ((s[0] as u32) << 24) | ((s[1] as u32) << 16) | ((s[2] as u32) << 8) | (s[3] as u32)
}

/// Render a four-char code for debugging ("bltn").
pub fn fourcc_to_string(code: u32) -> String {
    code.to_be_bytes()
        .iter()
        .map(|&b| {
            if b.is_ascii_graphic() || b == b' ' {
                b as char
            } else {
                '?'
            }
        })
        .collect()
}

// --- Property selectors / scopes / constants -------------------------------------------------

const SYSTEM_OBJECT: u32 = 1; // kAudioObjectSystemObject
const SCOPE_GLOBAL: u32 = fourcc(b"glob");
const SCOPE_OUTPUT: u32 = fourcc(b"outp");
const SCOPE_INPUT: u32 = fourcc(b"inpt");
const ELEMENT_MAIN: u32 = 0;

const HW_DEVICES: u32 = fourcc(b"dev#");
const HW_DEFAULT_OUTPUT: u32 = fourcc(b"dOut");
const HW_DEFAULT_SYSTEM_OUTPUT: u32 = fourcc(b"sOut");
const HW_DEFAULT_INPUT: u32 = fourcc(b"dIn ");
const OBJ_NAME: u32 = fourcc(b"lnam");
const DEV_UID: u32 = fourcc(b"uid ");
const DEV_TRANSPORT: u32 = fourcc(b"tran");
const DEV_STREAM_CONFIG: u32 = fourcc(b"slay");
const DEV_VOLUME_SCALAR: u32 = fourcc(b"volm");
const AGG_FULL_SUBDEVICES: u32 = fourcc(b"grup");
const AGG_COMPOSITION: u32 = fourcc(b"acom");

// kAudioDeviceTransportType*
const TRANSPORT_BUILTIN: u32 = fourcc(b"bltn");
const TRANSPORT_AGGREGATE: u32 = fourcc(b"grup");
const TRANSPORT_VIRTUAL: u32 = fourcc(b"virt");
const TRANSPORT_BLUETOOTH: u32 = fourcc(b"blue");
const TRANSPORT_BLUETOOTH_LE: u32 = fourcc(b"blea");

/// Map a `kAudioDevicePropertyTransportType` value to [`Transport`].
pub fn transport_from_fourcc(code: u32) -> Transport {
    match code {
        TRANSPORT_BUILTIN => Transport::BuiltIn,
        TRANSPORT_BLUETOOTH | TRANSPORT_BLUETOOTH_LE => Transport::Bluetooth,
        TRANSPORT_VIRTUAL => Transport::Virtual,
        TRANSPORT_AGGREGATE => Transport::Aggregate,
        _ => Transport::Other,
    }
}

/// Average of channel volumes, clamped to 0..=1; None for an empty slice.
pub fn average_volume(values: &[f32]) -> Option<f32> {
    if values.is_empty() {
        return None;
    }
    Some((values.iter().sum::<f32>() / values.len() as f32).clamp(0.0, 1.0))
}

/// Generate a unique UID for a new Multi-Output device.
pub fn new_multi_output_uid() -> String {
    static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("local.duet.multi.{millis}.{}.{seq}", std::process::id())
}

// --- Raw FFI ------------------------------------------------------------------------------------

mod hal {
    use super::{AudioError, Result};
    use core_foundation::array::{CFArray, CFArrayRef};
    use core_foundation::base::{CFType, TCFType};
    use core_foundation::dictionary::CFDictionaryRef;
    use core_foundation::string::{CFString, CFStringRef};
    use std::ffi::c_void;

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct Address {
        pub selector: u32,
        pub scope: u32,
        pub element: u32,
    }

    #[repr(C)]
    struct AudioBuffer {
        number_channels: u32,
        data_byte_size: u32,
        data: *mut c_void,
    }

    #[link(name = "CoreAudio", kind = "framework")]
    unsafe extern "C" {
        fn AudioObjectHasProperty(id: u32, addr: *const Address) -> u8;
        fn AudioObjectIsPropertySettable(id: u32, addr: *const Address, out: *mut u8) -> i32;
        fn AudioObjectGetPropertyDataSize(
            id: u32,
            addr: *const Address,
            qual_size: u32,
            qual: *const c_void,
            out_size: *mut u32,
        ) -> i32;
        fn AudioObjectGetPropertyData(
            id: u32,
            addr: *const Address,
            qual_size: u32,
            qual: *const c_void,
            io_size: *mut u32,
            out: *mut c_void,
        ) -> i32;
        fn AudioObjectSetPropertyData(
            id: u32,
            addr: *const Address,
            qual_size: u32,
            qual: *const c_void,
            size: u32,
            data: *const c_void,
        ) -> i32;
        fn AudioHardwareCreateAggregateDevice(desc: CFDictionaryRef, out: *mut u32) -> i32;
        fn AudioHardwareDestroyAggregateDevice(id: u32) -> i32;
    }

    fn check(call: &'static str, status: i32) -> Result<()> {
        if status == 0 {
            Ok(())
        } else {
            Err(AudioError::OsStatus { call, status })
        }
    }

    pub fn has(id: u32, addr: Address) -> bool {
        // SAFETY: addr is a valid pointer for the duration of the call.
        unsafe { AudioObjectHasProperty(id, &addr) != 0 }
    }

    pub fn settable(id: u32, addr: Address) -> bool {
        let mut out = 0u8;
        // SAFETY: valid pointers; out is a Boolean (u8).
        let st = unsafe { AudioObjectIsPropertySettable(id, &addr, &mut out) };
        st == 0 && out != 0
    }

    fn size_of(id: u32, addr: Address) -> Result<u32> {
        let mut size = 0u32;
        // SAFETY: valid pointers, no qualifier.
        let st =
            unsafe { AudioObjectGetPropertyDataSize(id, &addr, 0, std::ptr::null(), &mut size) };
        check("AudioObjectGetPropertyDataSize", st)?;
        Ok(size)
    }

    /// Read a fixed-size POD property (u32, f32, ...).
    pub fn get<T: Copy + Default>(id: u32, addr: Address) -> Result<T> {
        let mut v = T::default();
        let mut size = std::mem::size_of::<T>() as u32;
        // SAFETY: v is a valid writable buffer of `size` bytes; T is POD.
        let st = unsafe {
            AudioObjectGetPropertyData(
                id,
                &addr,
                0,
                std::ptr::null(),
                &mut size,
                (&mut v as *mut T).cast(),
            )
        };
        check("AudioObjectGetPropertyData", st)?;
        Ok(v)
    }

    /// Write a fixed-size POD property.
    pub fn set<T: Copy>(id: u32, addr: Address, v: T) -> Result<()> {
        // SAFETY: v is valid for size_of::<T>() bytes for the duration of the call.
        let st = unsafe {
            AudioObjectSetPropertyData(
                id,
                &addr,
                0,
                std::ptr::null(),
                std::mem::size_of::<T>() as u32,
                (&v as *const T).cast(),
            )
        };
        check("AudioObjectSetPropertyData", st)
    }

    /// Read a variable-length array of u32 (e.g. AudioObjectIDs).
    pub fn get_u32_array(id: u32, addr: Address) -> Result<Vec<u32>> {
        let size = size_of(id, addr)?;
        let mut v = vec![0u32; size as usize / 4];
        let mut io = (v.len() * 4) as u32;
        // SAFETY: v has room for `io` bytes.
        let st = unsafe {
            AudioObjectGetPropertyData(
                id,
                &addr,
                0,
                std::ptr::null(),
                &mut io,
                v.as_mut_ptr().cast(),
            )
        };
        check("AudioObjectGetPropertyData", st)?;
        v.truncate(io as usize / 4);
        Ok(v)
    }

    /// Read a CFString property (returned +1 retained by the HAL).
    pub fn get_string(id: u32, addr: Address) -> Result<String> {
        let mut s: CFStringRef = std::ptr::null();
        let mut size = std::mem::size_of::<CFStringRef>() as u32;
        // SAFETY: s is a valid out-pointer for one CFStringRef.
        let st = unsafe {
            AudioObjectGetPropertyData(
                id,
                &addr,
                0,
                std::ptr::null(),
                &mut size,
                (&mut s as *mut CFStringRef).cast(),
            )
        };
        check("AudioObjectGetPropertyData", st)?;
        if s.is_null() {
            return Ok(String::new());
        }
        // SAFETY: HAL returns an owned (+1) CFString.
        Ok(unsafe { CFString::wrap_under_create_rule(s) }.to_string())
    }

    /// Read a CFArray-of-CFString property (returned +1 retained).
    pub fn get_string_array(id: u32, addr: Address) -> Result<Vec<String>> {
        let mut a: CFArrayRef = std::ptr::null();
        let mut size = std::mem::size_of::<CFArrayRef>() as u32;
        // SAFETY: a is a valid out-pointer for one CFArrayRef.
        let st = unsafe {
            AudioObjectGetPropertyData(
                id,
                &addr,
                0,
                std::ptr::null(),
                &mut size,
                (&mut a as *mut CFArrayRef).cast(),
            )
        };
        check("AudioObjectGetPropertyData", st)?;
        if a.is_null() {
            return Ok(Vec::new());
        }
        // SAFETY: HAL returns an owned (+1) CFArray.
        let arr: CFArray<CFType> = unsafe { CFArray::wrap_under_create_rule(a) };
        Ok(arr
            .iter()
            .filter_map(|item| item.downcast::<CFString>().map(|s| s.to_string()))
            .collect())
    }

    /// Is this aggregate a Multi-Output ("stacked") device, per its composition dictionary?
    pub fn is_stacked(id: u32, addr: Address) -> bool {
        use core_foundation::boolean::CFBoolean;
        use core_foundation::dictionary::{CFDictionary, CFDictionaryRef};
        use core_foundation::number::CFNumber;
        let mut d: CFDictionaryRef = std::ptr::null();
        let mut size = std::mem::size_of::<CFDictionaryRef>() as u32;
        // SAFETY: d is a valid out-pointer for one CFDictionaryRef.
        let st = unsafe {
            AudioObjectGetPropertyData(
                id,
                &addr,
                0,
                std::ptr::null(),
                &mut size,
                (&mut d as *mut CFDictionaryRef).cast(),
            )
        };
        if st != 0 || d.is_null() {
            return false;
        }
        // SAFETY: HAL returns an owned (+1) CFDictionary.
        let dict: CFDictionary<CFString, CFType> =
            unsafe { CFDictionary::wrap_under_create_rule(d) };
        // "stacked" may be a CFBoolean or a CFNumber depending on who created the device.
        dict.find(CFString::from_static_string("stacked"))
            .is_some_and(|v| {
                v.downcast::<CFBoolean>()
                    .map(bool::from)
                    .or_else(|| {
                        v.downcast::<CFNumber>()
                            .and_then(|n| n.to_i64())
                            .map(|n| n != 0)
                    })
                    .unwrap_or(false)
            })
    }

    /// Total channel count from an AudioBufferList-valued property (stream configuration).
    pub fn channel_count(id: u32, addr: Address) -> Result<u32> {
        let size = size_of(id, addr)?;
        if size < 4 {
            return Ok(0);
        }
        // u64 backing for 8-byte alignment of the AudioBuffer pointers.
        let mut buf = vec![0u64; (size as usize).div_ceil(8)];
        let mut io = size;
        // SAFETY: buf has at least `size` writable bytes.
        let st = unsafe {
            AudioObjectGetPropertyData(
                id,
                &addr,
                0,
                std::ptr::null(),
                &mut io,
                buf.as_mut_ptr().cast(),
            )
        };
        check("AudioObjectGetPropertyData", st)?;
        let base = buf.as_ptr() as *const u8;
        // SAFETY: AudioBufferList = { u32 count; [pad]; AudioBuffer[count] }; we bound-check each
        // buffer against the returned byte size before reading it.
        unsafe {
            let n = *(base as *const u32) as usize;
            let off = std::mem::align_of::<AudioBuffer>().max(4); // mBuffers offset (8 on 64-bit)
            let stride = std::mem::size_of::<AudioBuffer>();
            let mut total = 0u32;
            for i in 0..n {
                let start = off + i * stride;
                if start + stride > io as usize {
                    break;
                }
                total += (*(base.add(start) as *const AudioBuffer)).number_channels;
            }
            Ok(total)
        }
    }

    pub fn create_aggregate(
        desc: &core_foundation::dictionary::CFDictionary<CFString, CFType>,
    ) -> Result<u32> {
        let mut id = 0u32;
        // SAFETY: desc is a valid CFDictionary for the duration of the call.
        let st = unsafe { AudioHardwareCreateAggregateDevice(desc.as_concrete_TypeRef(), &mut id) };
        check("AudioHardwareCreateAggregateDevice", st)?;
        Ok(id)
    }

    pub fn destroy_aggregate(id: u32) -> Result<()> {
        // SAFETY: plain value call.
        check("AudioHardwareDestroyAggregateDevice", unsafe {
            AudioHardwareDestroyAggregateDevice(id)
        })
    }
}

use hal::Address;

const fn addr(selector: u32, scope: u32, element: u32) -> Address {
    Address {
        selector,
        scope,
        element,
    }
}

/// Volume elements that are readable for `id` on the output scope:
/// the main element if present, else channels 1..=N.
fn volume_elements(id: DeviceId) -> Vec<u32> {
    if hal::has(id, addr(DEV_VOLUME_SCALAR, SCOPE_OUTPUT, ELEMENT_MAIN)) {
        return vec![ELEMENT_MAIN];
    }
    let n =
        hal::channel_count(id, addr(DEV_STREAM_CONFIG, SCOPE_OUTPUT, ELEMENT_MAIN)).unwrap_or(0);
    (1..=n)
        .filter(|&ch| hal::has(id, addr(DEV_VOLUME_SCALAR, SCOPE_OUTPUT, ch)))
        .collect()
}

fn read_volume(id: DeviceId) -> Option<f32> {
    let vals: Vec<f32> = volume_elements(id)
        .into_iter()
        .filter_map(|el| hal::get::<f32>(id, addr(DEV_VOLUME_SCALAR, SCOPE_OUTPUT, el)).ok())
        .collect();
    average_volume(&vals)
}

fn device_exists(id: DeviceId) -> Result<bool> {
    Ok(
        hal::get_u32_array(SYSTEM_OBJECT, addr(HW_DEVICES, SCOPE_GLOBAL, ELEMENT_MAIN))?
            .contains(&id),
    )
}

impl CoreAudio {
    pub fn new() -> Self {
        CoreAudio
    }

    /// Current default output device id.
    pub fn default_output(&self) -> Result<DeviceId> {
        hal::get::<u32>(
            SYSTEM_OBJECT,
            addr(HW_DEFAULT_OUTPUT, SCOPE_GLOBAL, ELEMENT_MAIN),
        )
    }

    /// Current default *system* (alert / sound-effects) output device id.
    pub fn default_system_output(&self) -> Result<DeviceId> {
        hal::get::<u32>(
            SYSTEM_OBJECT,
            addr(HW_DEFAULT_SYSTEM_OUTPUT, SCOPE_GLOBAL, ELEMENT_MAIN),
        )
    }

    /// Set only the alert/system output (used to restore state in tests).
    pub fn set_default_system_output(&self, id: DeviceId) -> Result<()> {
        hal::set(
            SYSTEM_OBJECT,
            addr(HW_DEFAULT_SYSTEM_OUTPUT, SCOPE_GLOBAL, ELEMENT_MAIN),
            id,
        )
    }

    /// Read the output volume of a single device (None if it has no volume control).
    pub fn volume(&self, id: DeviceId) -> Option<f32> {
        read_volume(id)
    }

    /// Destroy an aggregate device previously created by [`AudioBackend::create_multi_output`].
    /// Refuses to touch devices whose UID wasn't generated by Duet (protects user-made
    /// Audio MIDI Setup devices such as "~:AMS2_StackedOutput:0").
    pub fn destroy_multi_output(&self, id: DeviceId) -> Result<()> {
        let uid = hal::get_string(id, addr(DEV_UID, SCOPE_GLOBAL, ELEMENT_MAIN))?;
        if !uid.starts_with("local.duet.multi.") {
            return Err(AudioError::Other(format!(
                "refusing to destroy non-Duet device {uid}"
            )));
        }
        hal::destroy_aggregate(id)
    }

    fn describe(&self, id: DeviceId, default_id: Option<DeviceId>) -> Result<Option<OutputDevice>> {
        let transport = hal::get::<u32>(id, addr(DEV_TRANSPORT, SCOPE_GLOBAL, ELEMENT_MAIN))
            .map(transport_from_fourcc)
            .unwrap_or(Transport::Other);
        let channels = hal::channel_count(id, addr(DEV_STREAM_CONFIG, SCOPE_OUTPUT, ELEMENT_MAIN))
            .unwrap_or(0);
        // Multi-Output devices whose members are all disconnected report 0 output channels;
        // keep them so the UI can reuse them once the headphones reconnect.
        let stacked = transport == Transport::Aggregate
            && hal::is_stacked(id, addr(AGG_COMPOSITION, SCOPE_GLOBAL, ELEMENT_MAIN));
        if channels == 0 && !stacked {
            return Ok(None);
        }
        let uid = hal::get_string(id, addr(DEV_UID, SCOPE_GLOBAL, ELEMENT_MAIN))?;
        let name = hal::get_string(id, addr(OBJ_NAME, SCOPE_GLOBAL, ELEMENT_MAIN))?;
        let subdevice_uids = if transport == Transport::Aggregate {
            hal::get_string_array(id, addr(AGG_FULL_SUBDEVICES, SCOPE_GLOBAL, ELEMENT_MAIN))
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        Ok(Some(OutputDevice {
            id,
            uid,
            name,
            transport,
            volume: read_volume(id),
            is_default: Some(id) == default_id,
            subdevice_uids,
        }))
    }
}

impl AudioBackend for CoreAudio {
    fn output_devices(&self) -> Result<Vec<OutputDevice>> {
        let ids = hal::get_u32_array(SYSTEM_OBJECT, addr(HW_DEVICES, SCOPE_GLOBAL, ELEMENT_MAIN))?;
        let default_id = self.default_output().ok();
        let mut out = Vec::new();
        for id in ids {
            // A device can vanish mid-enumeration (e.g. AirPods disconnecting); skip it.
            if let Ok(Some(d)) = self.describe(id, default_id) {
                out.push(d);
            }
        }
        Ok(out)
    }

    fn set_default_output(&self, id: DeviceId) -> Result<()> {
        if !device_exists(id)? {
            return Err(AudioError::NotFound(id.to_string()));
        }
        hal::set(
            SYSTEM_OBJECT,
            addr(HW_DEFAULT_OUTPUT, SCOPE_GLOBAL, ELEMENT_MAIN),
            id,
        )?;
        // Aggregates can't always be the alert device; don't fail the main switch over it.
        let _ = hal::set(
            SYSTEM_OBJECT,
            addr(HW_DEFAULT_SYSTEM_OUTPUT, SCOPE_GLOBAL, ELEMENT_MAIN),
            id,
        );
        Ok(())
    }

    fn set_volume(&self, id: DeviceId, volume: f32) -> Result<()> {
        if !device_exists(id)? {
            return Err(AudioError::NotFound(id.to_string()));
        }
        let v = if volume.is_nan() {
            0.0
        } else {
            volume.clamp(0.0, 1.0)
        };
        let elements: Vec<u32> = volume_elements(id)
            .into_iter()
            .filter(|&el| hal::settable(id, addr(DEV_VOLUME_SCALAR, SCOPE_OUTPUT, el)))
            .collect();
        if elements.is_empty() {
            return Err(AudioError::Other(format!(
                "device {id} has no settable volume control"
            )));
        }
        for el in elements {
            hal::set(id, addr(DEV_VOLUME_SCALAR, SCOPE_OUTPUT, el), v)?;
        }
        Ok(())
    }

    fn create_multi_output(&self, name: &str, member_uids: &[String]) -> Result<DeviceId> {
        let Some(main_uid) = member_uids.first() else {
            return Err(AudioError::Other(
                "a Multi-Output device needs at least one member".into(),
            ));
        };
        let key = CFString::new;
        let subdevices: Vec<CFType> = member_uids
            .iter()
            .enumerate()
            .map(|(i, uid)| {
                let d = CFDictionary::<CFString, CFType>::from_CFType_pairs(&[
                    (key("uid"), CFString::new(uid).as_CFType()),
                    (key("drift"), CFNumber::from(i32::from(i > 0)).as_CFType()),
                ]);
                d.as_CFType()
            })
            .collect();
        let uid = new_multi_output_uid();
        let desc = CFDictionary::<CFString, CFType>::from_CFType_pairs(&[
            (key("name"), CFString::new(name).as_CFType()),
            (key("uid"), CFString::new(&uid).as_CFType()),
            (key("stacked"), CFNumber::from(1i32).as_CFType()),
            (key("private"), CFNumber::from(0i32).as_CFType()),
            (key("master"), CFString::new(main_uid).as_CFType()),
            (
                key("subdevices"),
                CFArray::from_CFTypes(&subdevices).as_CFType(),
            ),
        ]);
        let id = hal::create_aggregate(&desc)?;
        // The HAL publishes the new device asynchronously; wait briefly until it is listed.
        for _ in 0..50 {
            if device_exists(id).unwrap_or(false) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        Ok(id)
    }
    fn remove_multi_output(&self, id: DeviceId) -> Result<()> {
        self.destroy_multi_output(id)
    }

    fn avoid_bluetooth_input(&self) -> Result<Option<String>> {
        let current = hal::get::<u32>(
            SYSTEM_OBJECT,
            addr(HW_DEFAULT_INPUT, SCOPE_GLOBAL, ELEMENT_MAIN),
        )?;
        let transport = |id: u32| {
            hal::get::<u32>(id, addr(DEV_TRANSPORT, SCOPE_GLOBAL, ELEMENT_MAIN))
                .map(transport_from_fourcc)
        };
        if transport(current).ok() != Some(Transport::Bluetooth) {
            return Ok(None);
        }
        let ids = hal::get_u32_array(SYSTEM_OBJECT, addr(HW_DEVICES, SCOPE_GLOBAL, ELEMENT_MAIN))?;
        let builtin_mic = ids.into_iter().find(|&id| {
            transport(id).ok() == Some(Transport::BuiltIn)
                && hal::channel_count(id, addr(DEV_STREAM_CONFIG, SCOPE_INPUT, ELEMENT_MAIN))
                    .unwrap_or(0)
                    > 0
        });
        let Some(mic) = builtin_mic else {
            return Ok(None);
        };
        let old = hal::get_string(current, addr(OBJ_NAME, SCOPE_GLOBAL, ELEMENT_MAIN))
            .unwrap_or_default();
        hal::set(
            SYSTEM_OBJECT,
            addr(HW_DEFAULT_INPUT, SCOPE_GLOBAL, ELEMENT_MAIN),
            mic,
        )?;
        Ok(Some(old))
    }

    fn mirror_supported(&self) -> bool {
        super::tap_mirror::supported()
    }

    fn start_mirror(&self, target_uids: &[String]) -> Result<()> {
        let mut slot = MIRROR.lock().unwrap_or_else(|e| e.into_inner());
        if slot.as_ref().is_some_and(|m| m.targets() == target_uids) {
            return Ok(());
        }
        *slot = None; // stop the old one first (releases its devices)
        if target_uids.is_empty() {
            return Ok(());
        }
        *slot = Some(super::tap_mirror::Mirror::start(target_uids)?);
        Ok(())
    }

    fn stop_mirror(&self) {
        *MIRROR.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    fn mirror_targets(&self) -> Option<Vec<String>> {
        MIRROR
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|m| m.targets())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fourcc_roundtrip() {
        assert_eq!(fourcc(b"bltn"), 0x626C_746E);
        assert_eq!(fourcc_to_string(fourcc(b"grup")), "grup");
        assert_eq!(fourcc_to_string(fourcc(b"uid ")), "uid ");
    }

    #[test]
    fn transport_mapping() {
        assert_eq!(transport_from_fourcc(fourcc(b"bltn")), Transport::BuiltIn);
        assert_eq!(transport_from_fourcc(fourcc(b"blue")), Transport::Bluetooth);
        assert_eq!(transport_from_fourcc(fourcc(b"blea")), Transport::Bluetooth);
        assert_eq!(transport_from_fourcc(fourcc(b"virt")), Transport::Virtual);
        assert_eq!(transport_from_fourcc(fourcc(b"grup")), Transport::Aggregate);
        assert_eq!(transport_from_fourcc(fourcc(b"usb ")), Transport::Other);
        assert_eq!(transport_from_fourcc(0), Transport::Other);
    }

    #[test]
    fn volume_average() {
        assert_eq!(average_volume(&[]), None);
        assert_eq!(average_volume(&[0.5]), Some(0.5));
        assert_eq!(average_volume(&[0.25, 0.75]), Some(0.5));
        assert_eq!(average_volume(&[1.5, 1.5]), Some(1.0));
    }

    #[test]
    fn multi_uid_unique() {
        let a = new_multi_output_uid();
        let b = new_multi_output_uid();
        assert!(a.starts_with("local.duet.multi."));
        assert_ne!(a, b);
    }

    #[test]
    fn backend_is_send_sync() {
        fn check<T: Send + Sync + 'static>() {}
        check::<CoreAudio>();
    }
}
