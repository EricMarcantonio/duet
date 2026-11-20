//! macOS 14.2+ system-audio mirror built on Core Audio *process taps*.
//!
//! Instead of fighting macOS over the default output (AirPods set to "Connect to This
//! Mac: Automatically" keep reclaiming it), Duet lets macOS route audio to whichever
//! pair it wants and copies the system mix to the *other* pairs:
//!
//! 1. a private global tap of every process except Duet itself (so Duet's own playback
//!    is never re-captured — no feedback loop);
//! 2. per extra pair, a private aggregate device = { tap (drift-corrected) → that pair };
//! 3. an IOProc on each aggregate copying the tap's input buffers to the pair's output.
//!
//! The tap captures audio *before* the system volume, so every pair keeps its own volume.
//! Private aggregates never show up in the Sound menu. First use triggers macOS's
//! "System Audio Recording" permission prompt (Info.plist: NSAudioCaptureUsageDescription);
//! if it's denied the tap delivers silence, which [`Mirror::stats`] makes visible.

use super::{AudioError, Result};
use core_foundation::array::CFArray;
use core_foundation::base::{CFType, TCFType};
use core_foundation::dictionary::{CFDictionary, CFDictionaryRef};
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;
use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Bool};
use objc2_foundation::{NSArray, NSNumber, NSString};
use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

#[repr(C)]
struct AudioBuffer {
    channels: u32,
    bytes: u32,
    data: *mut c_void,
}

#[repr(C)]
struct AudioBufferList {
    count: u32,
    buffers: [AudioBuffer; 1], // variable length
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Address {
    selector: u32,
    scope: u32,
    element: u32,
}

type IoProcId = *mut c_void;
type IoProc = unsafe extern "C" fn(
    device: u32,
    now: *const c_void,
    input: *const AudioBufferList,
    input_time: *const c_void,
    output: *mut AudioBufferList,
    output_time: *const c_void,
    client: *mut c_void,
) -> i32;

#[link(name = "CoreAudio", kind = "framework")]
unsafe extern "C" {
    fn AudioHardwareCreateProcessTap(description: *const AnyObject, out_tap: *mut u32) -> i32;
    fn AudioHardwareDestroyProcessTap(tap: u32) -> i32;
    fn AudioHardwareCreateAggregateDevice(description: CFDictionaryRef, out: *mut u32) -> i32;
    fn AudioHardwareDestroyAggregateDevice(id: u32) -> i32;
    fn AudioDeviceCreateIOProcID(
        dev: u32,
        proc_: IoProc,
        client: *mut c_void,
        out: *mut IoProcId,
    ) -> i32;
    fn AudioDeviceDestroyIOProcID(dev: u32, id: IoProcId) -> i32;
    fn AudioDeviceStart(dev: u32, id: IoProcId) -> i32;
    fn AudioDeviceStop(dev: u32, id: IoProcId) -> i32;
    fn AudioObjectGetPropertyData(
        id: u32,
        addr: *const Address,
        qual_size: u32,
        qual: *const c_void,
        io_size: *mut u32,
        out: *mut c_void,
    ) -> i32;
}

const fn fourcc(b: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*b)
}

fn check(call: &'static str, status: i32) -> Result<()> {
    if status == 0 {
        Ok(())
    } else {
        Err(AudioError::OsStatus { call, status })
    }
}

/// Live counters, written by the real-time IOProc (lock-free).
#[derive(Default)]
pub struct Stats {
    /// IOProc invocations.
    pub calls: AtomicU64,
    /// Input buffers seen in the last callback (0 = the tap delivered nothing).
    pub in_buffers: AtomicU32,
    /// Frames copied so far.
    pub frames: AtomicU64,
    /// Peak absolute sample of the last callback, as f32 bits.
    pub peak: AtomicU32,
}

impl Stats {
    pub fn peak(&self) -> f32 {
        f32::from_bits(self.peak.load(Ordering::Relaxed))
    }
}

/// One extra pair being fed from the tap.
struct Route {
    target_uid: String,
    aggregate: u32,
    proc_id: IoProcId,
    stats: Box<Stats>,
}

/// A running mirror; dropping it stops everything and removes the private devices.
pub struct Mirror {
    tap: u32,
    routes: Vec<Route>,
}

// SAFETY: the raw pointers are CoreAudio handles (IOProc ids) and a Box we own; CoreAudio
// calls on them are thread-safe.
unsafe impl Send for Mirror {}

/// True when this macOS has process taps (14.2+).
pub fn supported() -> bool {
    AnyClass::get(c"CATapDescription").is_some()
}

/// The HAL's AudioObjectID for our own process (to exclude it from the tap).
fn own_process_object() -> Option<u32> {
    let addr = Address {
        selector: fourcc(b"id2p"), // kAudioHardwarePropertyTranslatePIDToProcessObject
        scope: fourcc(b"glob"),
        element: 0,
    };
    let pid: i32 = std::process::id() as i32;
    let mut obj: u32 = 0;
    let mut size = 4u32;
    // SAFETY: valid address, qualifier is a pid_t, out is one AudioObjectID.
    let st = unsafe {
        AudioObjectGetPropertyData(
            1,
            &addr,
            4,
            (&pid as *const i32).cast(),
            &mut size,
            (&mut obj as *mut u32).cast(),
        )
    };
    (st == 0 && obj != 0).then_some(obj)
}

/// Create the private global tap (excluding Duet). Returns (tap id, tap UUID string).
fn create_tap() -> Result<(u32, String)> {
    let cls = AnyClass::get(c"CATapDescription")
        .ok_or_else(|| AudioError::Other("system audio taps need macOS 14.2 or later".into()))?;
    let exclude: Retained<NSArray<NSNumber>> = match own_process_object() {
        Some(me) => NSArray::from_retained_slice(&[NSNumber::new_u32(me)]),
        None => NSArray::new(),
    };
    // SAFETY: documented CATapDescription API (CoreAudio/CATapDescription.h, macOS 14.2):
    // -initStereoGlobalTapButExcludeProcesses:(NSArray<NSNumber*>*), setters take
    // BOOL / NSInteger (CATapMuteBehavior) / NSString*, -UUID returns NSUUID*.
    unsafe {
        let alloc: *mut AnyObject = msg_send![cls, alloc];
        let desc: *mut AnyObject =
            msg_send![alloc, initStereoGlobalTapButExcludeProcesses: &*exclude];
        let desc = Retained::from_raw(desc)
            .ok_or_else(|| AudioError::Other("couldn't describe the system audio tap".into()))?;
        let _: () = msg_send![&*desc, setPrivate: Bool::YES];
        let _: () = msg_send![&*desc, setMuteBehavior: 0isize]; // CATapUnmuted: keep normal playback
        let name = NSString::from_str("Duet");
        let _: () = msg_send![&*desc, setName: &*name];
        let uuid: *mut AnyObject = msg_send![&*desc, UUID];
        let uuid_str: *mut NSString = msg_send![uuid, UUIDString];
        let uuid_str = uuid_str
            .as_ref()
            .map(|s| s.to_string())
            .ok_or_else(|| AudioError::Other("tap has no UUID".into()))?;
        let mut tap = 0u32;
        check(
            "AudioHardwareCreateProcessTap",
            AudioHardwareCreateProcessTap(Retained::as_ptr(&desc), &mut tap),
        )?;
        Ok((tap, uuid_str))
    }
}

/// Copy the tap's (last) f32 input buffer to every output buffer, mapping channels and
/// zero-filling anything the input can't cover. Returns (frames, peak).
///
/// # Safety
/// Pointers come from CoreAudio and describe valid buffers for this callback.
unsafe fn copy_buffers(input: *const AudioBufferList, output: *mut AudioBufferList) -> (u64, f32) {
    unsafe {
        let Some(out) = output.as_mut() else {
            return (0, 0.0);
        };
        let out_bufs = std::slice::from_raw_parts_mut(out.buffers.as_mut_ptr(), out.count as usize);
        let in_bufs: &[AudioBuffer] = match input.as_ref() {
            Some(i) if i.count > 0 => {
                std::slice::from_raw_parts(i.buffers.as_ptr(), i.count as usize)
            }
            _ => &[],
        };
        let mut frames_done = 0u64;
        let mut peak = 0f32;
        for (k, ob) in out_bufs.iter_mut().enumerate() {
            if ob.data.is_null() {
                continue;
            }
            let oc = ob.channels.max(1) as usize;
            let out_s = std::slice::from_raw_parts_mut(ob.data as *mut f32, ob.bytes as usize / 4);
            // An aggregate lists its subdevice inputs first and the tap's stream last, so
            // the tap is always the final input buffer (a target with a microphone
            // would otherwise get its own mic mixed in).
            let _ = k;
            let Some(ib) = in_bufs.last().filter(|b| !b.data.is_null()) else {
                out_s.fill(0.0);
                continue;
            };
            let ic = ib.channels.max(1) as usize;
            let in_s = std::slice::from_raw_parts(ib.data as *const f32, ib.bytes as usize / 4);
            let frames = (out_s.len() / oc).min(in_s.len() / ic);
            for f in 0..frames {
                for c in 0..oc {
                    let v = in_s[f * ic + c.min(ic - 1)];
                    peak = peak.max(v.abs());
                    out_s[f * oc + c] = v;
                }
            }
            out_s[frames * oc..].fill(0.0);
            frames_done = frames_done.max(frames as u64);
        }
        (frames_done, peak)
    }
}

unsafe extern "C" fn io_proc(
    _device: u32,
    _now: *const c_void,
    input: *const AudioBufferList,
    _input_time: *const c_void,
    output: *mut AudioBufferList,
    _output_time: *const c_void,
    client: *mut c_void,
) -> i32 {
    // SAFETY: CoreAudio passes the buffers for this cycle and our Stats pointer.
    unsafe {
        let (frames, peak) = copy_buffers(input, output);
        if let Some(stats) = (client as *const Stats).as_ref() {
            stats.calls.fetch_add(1, Ordering::Relaxed);
            stats
                .in_buffers
                .store(input.as_ref().map_or(0, |i| i.count), Ordering::Relaxed);
            stats.frames.fetch_add(frames, Ordering::Relaxed);
            stats.peak.store(peak.to_bits(), Ordering::Relaxed);
        }
    }
    0
}

fn start_route(tap_uuid: &str, target_uid: &str) -> Result<Route> {
    let key = CFString::new;
    let sub = CFDictionary::<CFString, CFType>::from_CFType_pairs(&[
        (key("uid"), CFString::new(target_uid).as_CFType()),
        (key("drift"), CFNumber::from(0i32).as_CFType()),
    ]);
    let tap = CFDictionary::<CFString, CFType>::from_CFType_pairs(&[
        (key("uid"), CFString::new(tap_uuid).as_CFType()),
        (key("drift"), CFNumber::from(1i32).as_CFType()),
    ]);
    let uid = format!("local.duet.tap.{tap_uuid}.{}", target_uid.replace(':', "_"));
    let desc = CFDictionary::<CFString, CFType>::from_CFType_pairs(&[
        (key("name"), CFString::new("Duet Mirror").as_CFType()),
        (key("uid"), CFString::new(&uid).as_CFType()),
        (key("private"), CFNumber::from(1i32).as_CFType()),
        (key("master"), CFString::new(target_uid).as_CFType()),
        (
            key("subdevices"),
            CFArray::from_CFTypes(&[sub.as_CFType()]).as_CFType(),
        ),
        (
            key("taps"),
            CFArray::from_CFTypes(&[tap.as_CFType()]).as_CFType(),
        ),
        (key("tapautostart"), CFNumber::from(1i32).as_CFType()),
    ]);
    let mut agg = 0u32;
    // SAFETY: valid CFDictionary; out-pointer for one AudioObjectID.
    check("AudioHardwareCreateAggregateDevice", unsafe {
        AudioHardwareCreateAggregateDevice(desc.as_concrete_TypeRef(), &mut agg)
    })?;
    let stats = Box::<Stats>::default();
    let mut proc_id: IoProcId = std::ptr::null_mut();
    // SAFETY: io_proc matches AudioDeviceIOProc; stats outlives the IOProc (Route owns it
    // and destroys the IOProc before dropping the Box).
    let st = unsafe {
        AudioDeviceCreateIOProcID(
            agg,
            io_proc,
            (&*stats as *const Stats).cast_mut().cast(),
            &mut proc_id,
        )
    };
    if let Err(e) = check("AudioDeviceCreateIOProcID", st) {
        unsafe { AudioHardwareDestroyAggregateDevice(agg) };
        return Err(e);
    }
    if let Err(e) = check("AudioDeviceStart", unsafe {
        AudioDeviceStart(agg, proc_id)
    }) {
        unsafe {
            AudioDeviceDestroyIOProcID(agg, proc_id);
            AudioHardwareDestroyAggregateDevice(agg);
        }
        return Err(e);
    }
    Ok(Route {
        target_uid: target_uid.to_string(),
        aggregate: agg,
        proc_id,
        stats,
    })
}

impl Mirror {
    /// Start copying the system mix to every output in `target_uids`.
    pub fn start(target_uids: &[String]) -> Result<Mirror> {
        let (tap, tap_uuid) = create_tap()?;
        let mut mirror = Mirror {
            tap,
            routes: Vec::new(),
        };
        for uid in target_uids {
            // On error `mirror` drops here and tears down what was started.
            mirror.routes.push(start_route(&tap_uuid, uid)?);
        }
        Ok(mirror)
    }

    pub fn targets(&self) -> Vec<String> {
        self.routes.iter().map(|r| r.target_uid.clone()).collect()
    }

    /// (target uid, frames copied, last peak) per route.
    pub fn stats(&self) -> Vec<(String, u64, f32)> {
        self.routes
            .iter()
            .map(|r| {
                (
                    r.target_uid.clone(),
                    r.stats.frames.load(Ordering::Relaxed),
                    r.stats.peak(),
                )
            })
            .collect()
    }

    /// (callbacks, input buffers in the last callback) per route — for diagnostics.
    pub fn debug_counts(&self) -> Vec<(u64, u32)> {
        self.routes
            .iter()
            .map(|r| {
                (
                    r.stats.calls.load(Ordering::Relaxed),
                    r.stats.in_buffers.load(Ordering::Relaxed),
                )
            })
            .collect()
    }
}

impl Drop for Mirror {
    fn drop(&mut self) {
        for r in self.routes.drain(..) {
            // SAFETY: handles we created; stop before destroying, IOProc before its Stats.
            unsafe {
                AudioDeviceStop(r.aggregate, r.proc_id);
                AudioDeviceDestroyIOProcID(r.aggregate, r.proc_id);
                AudioHardwareDestroyAggregateDevice(r.aggregate);
            }
            drop(r.stats);
        }
        // SAFETY: tap we created.
        unsafe { AudioHardwareDestroyProcessTap(self.tap) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(bufs: &mut [(u32, Vec<f32>)]) -> (Vec<u8>, *mut AudioBufferList) {
        // Build a variable-length AudioBufferList in a byte buffer.
        let n = bufs.len();
        let size = 8 + n * std::mem::size_of::<AudioBuffer>();
        let mut mem = vec![0u8; size + 16];
        let p = mem.as_mut_ptr().align_offset(8);
        let abl = unsafe { mem.as_mut_ptr().add(p) as *mut AudioBufferList };
        unsafe {
            (*abl).count = n as u32;
            let b = (*abl).buffers.as_mut_ptr();
            for (i, (ch, data)) in bufs.iter_mut().enumerate() {
                *b.add(i) = AudioBuffer {
                    channels: *ch,
                    bytes: (data.len() * 4) as u32,
                    data: data.as_mut_ptr().cast(),
                };
            }
        }
        (mem, abl)
    }

    #[test]
    fn copies_and_maps_channels() {
        // Stereo interleaved in -> stereo interleaved out.
        let mut inb = vec![(2u32, vec![0.1, -0.2, 0.3, -0.4])];
        let mut outb = vec![(2u32, vec![9.0; 6])];
        let (_m1, i) = list(&mut inb);
        let (_m2, o) = list(&mut outb);
        let (frames, peak) = unsafe { copy_buffers(i, o) };
        assert_eq!(frames, 2);
        assert!((peak - 0.4).abs() < 1e-6);
        assert_eq!(
            outb[0].1,
            [0.1, -0.2, 0.3, -0.4, 0.0, 0.0],
            "extra frames zeroed"
        );

        // Stereo in -> mono out takes the first channel.
        let mut inb = vec![(2u32, vec![0.5, 0.25, -0.5, -0.25])];
        let mut outb = vec![(1u32, vec![0.0; 2])];
        let (_m1, i) = list(&mut inb);
        let (_m2, o) = list(&mut outb);
        unsafe { copy_buffers(i, o) };
        assert_eq!(outb[0].1, [0.5, -0.5]);

        // Target has its own input (mic) first; the tap is last and is what's copied.
        let mut inb = vec![(1u32, vec![0.9, 0.9]), (2u32, vec![0.1, 0.2, 0.3, 0.4])];
        let mut outb = vec![(2u32, vec![0.0; 4])];
        let (_m1, i) = list(&mut inb);
        let (_m2, o) = list(&mut outb);
        unsafe { copy_buffers(i, o) };
        assert_eq!(outb[0].1, [0.1, 0.2, 0.3, 0.4]);

        // No input (permission denied / starting up) -> silence, no crash.
        let mut outb = vec![(2u32, vec![1.0; 4])];
        let (_m2, o) = list(&mut outb);
        let (frames, _) = unsafe { copy_buffers(std::ptr::null(), o) };
        assert_eq!(frames, 0);
        assert_eq!(outb[0].1, [0.0; 4]);
    }
}
