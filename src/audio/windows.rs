//! Windows backends.
//!
//! * Audio endpoints: WASAPI / MMDevice (`IMMDeviceEnumerator`, `IAudioEndpointVolume`).
//! * Default output: the undocumented-but-ubiquitous `IPolicyConfig` COM interface
//!   (what the Sound control panel, EarTrumpet, SoundSwitch, AudioDeviceCmdlets use).
//! * Multi-output: Windows has no Multi-Output device, so Duet provides a **software
//!   mirror** ([`Mirror`]): WASAPI loopback-captures the first member (which is made the
//!   Windows default output) and re-renders those frames to the other members. It is
//!   exposed as a synthetic `Transport::Aggregate` device with id [`MIRROR_DEVICE_ID`].
//! * Bluetooth: WinRT `Windows.Devices.Bluetooth` for the paired list. Windows has no
//!   public API to connect/disconnect an audio device, so `connect`/`disconnect` return
//!   an error and the UI should offer [`open_bluetooth_settings`].
//!
//! ## Device ids
//! `DeviceId`s are assigned by an [`IdRegistry`]: 1, 2, 3, ... in first-seen order of
//! the endpoint id string, kept in the `WindowsAudio` instance behind a `Mutex`. They are
//! stable for the life of the process (an endpoint that disappears and returns gets its
//! old id back) but not across runs — persist `uid`s, not ids.
//!
//! ## UIDs
//! `uid` is the MMDevice endpoint id (`{0.0.0.00000000}.{guid}`), with `|AA-BB-CC-DD-EE-FF`
//! appended for Bluetooth endpoints whose address could be determined, so that
//! [`super::address_from_uid`] / [`super::bluetooth_address_of`] work on Windows. The
//! backend accepts either form wherever a uid is passed in.
//!
//! **Untested on real hardware** (written and compile-checked on macOS). The mirror in
//! particular is isolated in its own thread: any WASAPI failure stops the mirror and is
//! reported through [`WindowsAudio::mirror_error`], never a panic.

use super::windows_util::{self as wu, IdRegistry, MIRROR_DEVICE_ID, MIRROR_UID};
use super::{
    AudioBackend, AudioError, BluetoothBackend, BluetoothDevice, DeviceId, OutputDevice, Result,
    Transport,
};
use std::cell::Cell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::Duration;

use policy_config::IPolicyConfig;
use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::{
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK,
    DEVICE_STATE_ACTIVE, IAudioCaptureClient, IAudioClient, IAudioRenderClient, IMMDevice,
    IMMDeviceEnumerator, MMDeviceEnumerator, WAVEFORMATEX, WAVEFORMATEXTENSIBLE, eCommunications,
    eConsole, eMultimedia, eRender,
};
use windows::Win32::System::Com::StructuredStorage::{
    PROPVARIANT, PropVariantClear, PropVariantToBSTR,
};
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree, STGM_READ,
};
use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;
use windows::core::{GUID, HSTRING, PCWSTR};

// ---------------------------------------------------------------------------
// COM plumbing
// ---------------------------------------------------------------------------

/// Initialize COM (MTA) once per thread. If the thread is already an STA (e.g. the
/// WebView/UI thread), `CoInitializeEx` fails with RPC_E_CHANGED_MODE and COM is still
/// usable, so the result is deliberately ignored. COM is never uninitialized on purpose:
/// the threads that use it live for the whole process.
fn com_init() {
    thread_local!(static DONE: Cell<bool> = const { Cell::new(false) });
    DONE.with(|d| {
        if !d.get() {
            // SAFETY: plain COM initialization for the current thread.
            let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
            d.set(true);
        }
    });
}

trait Ctx<T> {
    fn ctx(self, call: &'static str) -> Result<T>;
}
impl<T> Ctx<T> for windows::core::Result<T> {
    fn ctx(self, call: &'static str) -> Result<T> {
        self.map_err(|e| AudioError::OsStatus {
            call,
            status: e.code().0,
        })
    }
}

fn enumerator() -> Result<IMMDeviceEnumerator> {
    com_init();
    // SAFETY: standard COM activation of the MMDevice enumerator.
    unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
        .ctx("CoCreateInstance(MMDeviceEnumerator)")
}

/// `IMMDevice::GetId` as a Rust string (frees the COM allocation).
unsafe fn device_id(dev: &IMMDevice) -> windows::core::Result<String> {
    unsafe {
        let p = dev.GetId()?;
        let s = p.to_string().unwrap_or_default();
        CoTaskMemFree(Some(p.0 as *const _));
        Ok(s)
    }
}

const fn pkey(fmtid: u128, pid: u32) -> PROPERTYKEY {
    PROPERTYKEY {
        fmtid: GUID::from_u128(fmtid),
        pid,
    }
}
/// PKEY_Device_FriendlyName ("Alex's AirPods Pro" / "Speakers (Realtek(R) Audio)").
const PKEY_DEVICE_FRIENDLY_NAME: PROPERTYKEY = pkey(0xa45c254e_df1c_4efd_8020_67d146a850e0, 14);
/// PKEY_Device_EnumeratorName ("BTHENUM", "BTHHFENUM", "HDAUDIO", "USB", "ROOT", "SWD", ...).
const PKEY_DEVICE_ENUMERATOR_NAME: PROPERTYKEY = pkey(0xa45c254e_df1c_4efd_8020_67d146a850e0, 24);
/// PKEY_Device_InstanceId.
const PKEY_DEVICE_INSTANCE_ID: PROPERTYKEY = pkey(0x78c34fc8_104a_4aca_9ea4_524d52996e57, 256);
/// Undocumented endpoint property holding the parent devnode's instance path, e.g.
/// `{1}.BTHENUM\{0000110b-...}_VID&0001004c_PID&200e\7&2b3f5d5e&0&022A3B4C5D6E_C00000000`.
/// Only used heuristically (Bluetooth detection + address).
const PKEY_ENDPOINT_DEVNODE_PATH: PROPERTYKEY = pkey(0xb3f8fa53_0004_438e_9003_51a46e139bfc, 2);
/// PKEY_AudioEndpoint_FormFactor (EndpointFormFactor enum; 9 = DigitalAudioDisplayDevice/HDMI).
const PKEY_AUDIOENDPOINT_FORM_FACTOR: PROPERTYKEY = pkey(0x1da5d803_d492_4edd_8c23_e0c0ffee7f0e, 0);

/// Read a property as a string (numbers are formatted). None if absent/empty.
unsafe fn prop_string(store: &IPropertyStore, key: &PROPERTYKEY) -> Option<String> {
    unsafe {
        let mut pv: PROPVARIANT = store.GetValue(key).ok()?;
        let s = PropVariantToBSTR(&pv).ok().map(|b| b.to_string());
        let _ = PropVariantClear(&mut pv);
        s.filter(|s| !s.is_empty())
    }
}

// ---------------------------------------------------------------------------
// IPolicyConfig (undocumented)
// ---------------------------------------------------------------------------

/// CLSID_PolicyConfigClient.
const CLSID_POLICY_CONFIG_CLIENT: GUID = GUID::from_u128(0x870af99c_171d_4f9e_af0d_e63df40c2bc9);

mod policy_config {
    #![allow(non_snake_case)]
    use windows::Win32::Foundation::PROPERTYKEY;
    use windows::Win32::Media::Audio::{ERole, WAVEFORMATEX};
    use windows::core::{HRESULT, IUnknown, IUnknown_Vtbl, PCWSTR, interface};

    /// `IPolicyConfig` (Windows 7 – 11), IID f8679f50-850a-41cf-9c72-430f290290c8.
    ///
    /// Vtable layout (after IUnknown) taken from the widely used reverse-engineered header
    /// "PolicyConfig.h" by EreTIk (2009), as also used by AudioDeviceCmdlets, SoundSwitch and
    /// EarTrumpet (`IPolicyConfig.cs`). Only `SetDefaultEndpoint` (slot 11 after IUnknown) is
    /// called; the other methods are declared solely to keep the slot order right, with
    /// opaque pointer parameters.
    #[interface("f8679f50-850a-41cf-9c72-430f290290c8")]
    pub unsafe trait IPolicyConfig: IUnknown {
        fn GetMixFormat(&self, device: PCWSTR, format: *mut *mut WAVEFORMATEX) -> HRESULT;
        fn GetDeviceFormat(
            &self,
            device: PCWSTR,
            default: i32,
            format: *mut *mut WAVEFORMATEX,
        ) -> HRESULT;
        fn ResetDeviceFormat(&self, device: PCWSTR) -> HRESULT;
        fn SetDeviceFormat(
            &self,
            device: PCWSTR,
            endpoint: *mut WAVEFORMATEX,
            mix: *mut WAVEFORMATEX,
        ) -> HRESULT;
        fn GetProcessingPeriod(
            &self,
            device: PCWSTR,
            default: i32,
            default_period: *mut i64,
            min_period: *mut i64,
        ) -> HRESULT;
        fn SetProcessingPeriod(&self, device: PCWSTR, period: *mut i64) -> HRESULT;
        fn GetShareMode(&self, device: PCWSTR, mode: *mut core::ffi::c_void) -> HRESULT;
        fn SetShareMode(&self, device: PCWSTR, mode: *mut core::ffi::c_void) -> HRESULT;
        fn GetPropertyValue(
            &self,
            device: PCWSTR,
            fx_store: i32,
            key: *const PROPERTYKEY,
            value: *mut core::ffi::c_void,
        ) -> HRESULT;
        fn SetPropertyValue(
            &self,
            device: PCWSTR,
            fx_store: i32,
            key: *const PROPERTYKEY,
            value: *mut core::ffi::c_void,
        ) -> HRESULT;
        fn SetDefaultEndpoint(&self, device: PCWSTR, role: ERole) -> HRESULT;
        fn SetEndpointVisibility(&self, device: PCWSTR, visible: i32) -> HRESULT;
    }

    /// The macro-generated methods are private to this module; this is the one we use.
    ///
    /// # Safety
    /// `device` must be a valid NUL-terminated wide string for the duration of the call.
    pub unsafe fn set_default_endpoint(pc: &IPolicyConfig, device: PCWSTR, role: ERole) -> HRESULT {
        unsafe { pc.SetDefaultEndpoint(device, role) }
    }
}

/// Make `endpoint_id` the default render device for all three roles.
fn set_default_endpoint(endpoint_id: &str) -> Result<()> {
    com_init();
    let wide = HSTRING::from(endpoint_id);
    // SAFETY: COM activation + calls with a valid NUL-terminated wide string that outlives them.
    unsafe {
        let pc: IPolicyConfig = CoCreateInstance(&CLSID_POLICY_CONFIG_CLIENT, None, CLSCTX_ALL)
            .ctx("CoCreateInstance(PolicyConfigClient)")?;
        for role in [eConsole, eMultimedia, eCommunications] {
            policy_config::set_default_endpoint(&pc, PCWSTR(wide.as_ptr()), role)
                .ok()
                .ctx("IPolicyConfig::SetDefaultEndpoint")?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Endpoints
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Endpoint {
    endpoint_id: String,
    uid: String,
    name: String,
    transport: Transport,
    volume: Option<f32>,
}

fn classify(
    enumerator_name: Option<&str>,
    devnode: Option<&str>,
    instance: Option<&str>,
    form_factor: Option<u32>,
) -> Transport {
    let all = [enumerator_name, devnode, instance];
    if all.iter().flatten().any(|s| wu::is_bluetooth_enumerator(s)) {
        return Transport::Bluetooth;
    }
    let en = enumerator_name.unwrap_or("").to_ascii_uppercase();
    let path = devnode.unwrap_or("").to_ascii_uppercase();
    let has = |s: &str| en == s || path.contains(&format!("{s}\\"));
    if has("ROOT") || has("SWD") || has("SW") {
        // Virtual cables (VB-Audio, Voicemeeter), Steam streaming speakers, ...
        return Transport::Virtual;
    }
    if (has("HDAUDIO") || has("INTELAUDIO") || has("ACPI")) && form_factor != Some(9) {
        return Transport::BuiltIn;
    }
    Transport::Other
}

unsafe fn read_endpoint(dev: &IMMDevice) -> windows::core::Result<Endpoint> {
    unsafe {
        let endpoint_id = device_id(dev)?;
        let store = dev.OpenPropertyStore(STGM_READ)?;
        let name =
            prop_string(&store, &PKEY_DEVICE_FRIENDLY_NAME).unwrap_or_else(|| endpoint_id.clone());
        let en = prop_string(&store, &PKEY_DEVICE_ENUMERATOR_NAME);
        let devnode = prop_string(&store, &PKEY_ENDPOINT_DEVNODE_PATH);
        let inst = prop_string(&store, &PKEY_DEVICE_INSTANCE_ID);
        let ff = prop_string(&store, &PKEY_AUDIOENDPOINT_FORM_FACTOR)
            .and_then(|s| s.trim().parse().ok());
        let transport = classify(en.as_deref(), devnode.as_deref(), inst.as_deref(), ff);
        let bt_addr = if transport == Transport::Bluetooth {
            devnode
                .as_deref()
                .and_then(wu::bt_address_from_instance_id)
                .or_else(|| inst.as_deref().and_then(wu::bt_address_from_instance_id))
        } else {
            None
        };
        let volume = dev
            .Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None)
            .and_then(|v| v.GetMasterVolumeLevelScalar())
            .ok();
        Ok(Endpoint {
            uid: wu::make_endpoint_uid(&endpoint_id, bt_addr.as_deref()),
            endpoint_id,
            name,
            transport,
            volume,
        })
    }
}

/// Active render endpoints and the current default (console role) endpoint id.
fn list_endpoints() -> Result<(Vec<Endpoint>, Option<String>)> {
    let en = enumerator()?;
    // SAFETY: COM calls on valid interfaces; out-strings freed by `device_id`.
    unsafe {
        let coll = en
            .EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)
            .ctx("EnumAudioEndpoints")?;
        let n = coll.GetCount().ctx("IMMDeviceCollection::GetCount")?;
        let mut out = Vec::with_capacity(n as usize);
        for i in 0..n {
            let dev = coll.Item(i).ctx("IMMDeviceCollection::Item")?;
            // One unreadable endpoint shouldn't hide the rest.
            if let Ok(ep) = read_endpoint(&dev) {
                out.push(ep);
            }
        }
        let default = en
            .GetDefaultAudioEndpoint(eRender, eConsole)
            .ok()
            .and_then(|d| device_id(&d).ok());
        Ok((out, default))
    }
}

fn get_device(endpoint_id: &str) -> Result<IMMDevice> {
    let en = enumerator()?;
    // SAFETY: valid enumerator, HSTRING outlives the call.
    unsafe { en.GetDevice(&HSTRING::from(endpoint_id)) }
        .map_err(|_| AudioError::NotFound(endpoint_id.to_string()))
}

// ---------------------------------------------------------------------------
// WindowsAudio
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct MirrorConfig {
    name: String,
    /// Endpoint ids (no `|MAC` suffix); [0] is the source / Windows default.
    members: Vec<String>,
}

#[derive(Default)]
struct State {
    ids: IdRegistry,
    mirror_config: Option<MirrorConfig>,
    mirror: Option<Mirror>,
}

/// WASAPI output backend. See the module docs for id/uid semantics and the mirror.
#[derive(Default)]
pub struct WindowsAudio {
    state: Mutex<State>,
}

impl WindowsAudio {
    pub fn new() -> Self {
        Self::default()
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn endpoint_for(&self, id: DeviceId) -> Result<String> {
        self.state()
            .ids
            .endpoint_of(id)
            .map(str::to_string)
            .ok_or_else(|| AudioError::NotFound(id.to_string()))
    }

    /// Error that stopped the running mirror, if any (e.g. a member disconnected).
    pub fn mirror_error(&self) -> Option<String> {
        self.state().mirror.as_ref().and_then(Mirror::error)
    }

    /// True while the software mirror is running.
    pub fn mirror_running(&self) -> bool {
        self.state().mirror.as_ref().is_some_and(Mirror::is_running)
    }

    /// Stop the software mirror (the Windows default output is left as is).
    pub fn stop_mirror(&self) {
        let m = self.state().mirror.take();
        drop(m); // joins the thread outside the lock
    }

    /// Forget the mirror configuration (removes the synthetic device) and stop it.
    pub fn remove_mirror(&self) {
        let m = {
            let mut st = self.state();
            st.mirror_config = None;
            st.mirror.take()
        };
        drop(m);
    }
}

impl AudioBackend for WindowsAudio {
    fn output_devices(&self) -> Result<Vec<OutputDevice>> {
        let (eps, default) = list_endpoints()?;
        let default = default.map(|d| d.to_ascii_lowercase());
        let mut st = self.state();
        let mirror_on = st.mirror.as_ref().is_some_and(Mirror::is_running);
        let mirror_src = st
            .mirror_config
            .as_ref()
            .and_then(|c| c.members.first())
            .map(|s| s.to_ascii_lowercase());
        let mirror_default = mirror_on && mirror_src.is_some() && mirror_src == default;
        let mut devices: Vec<OutputDevice> = eps
            .iter()
            .map(|e| OutputDevice {
                id: st.ids.id_for(&e.endpoint_id),
                uid: e.uid.clone(),
                name: e.name.clone(),
                transport: e.transport,
                volume: e.volume,
                is_default: !mirror_default
                    && default.as_deref() == Some(&*e.endpoint_id.to_ascii_lowercase()),
                subdevice_uids: vec![],
            })
            .collect();
        if let Some(cfg) = &st.mirror_config {
            let subdevice_uids = cfg
                .members
                .iter()
                .map(|m| {
                    eps.iter()
                        .find(|e| e.endpoint_id.eq_ignore_ascii_case(m))
                        .map_or_else(|| m.clone(), |e| e.uid.clone())
                })
                .collect();
            devices.push(OutputDevice {
                id: MIRROR_DEVICE_ID,
                uid: MIRROR_UID.into(),
                name: cfg.name.clone(),
                transport: Transport::Aggregate,
                volume: None,
                is_default: mirror_default,
                subdevice_uids,
            });
        }
        Ok(devices)
    }

    fn set_default_output(&self, id: DeviceId) -> Result<()> {
        if id == MIRROR_DEVICE_ID {
            let cfg = self
                .state()
                .mirror_config
                .clone()
                .ok_or_else(|| AudioError::NotFound(id.to_string()))?;
            let (source, targets) = cfg
                .members
                .split_first()
                .ok_or_else(|| AudioError::Other("mirror has no members".into()))?;
            self.stop_mirror();
            set_default_endpoint(source)?;
            let m = Mirror::start(source, targets)?;
            self.state().mirror = Some(m);
            return Ok(());
        }
        let ep = self.endpoint_for(id)?;
        self.stop_mirror();
        set_default_endpoint(&ep)
    }

    fn set_volume(&self, id: DeviceId, volume: f32) -> Result<()> {
        if id == MIRROR_DEVICE_ID {
            return Err(AudioError::Other(
                "the Duet mirror has no volume of its own; set each member's volume".into(),
            ));
        }
        let ep = self.endpoint_for(id)?;
        let v = if volume.is_nan() {
            0.0
        } else {
            volume.clamp(0.0, 1.0)
        };
        let dev = get_device(&ep)?;
        // SAFETY: COM calls on a valid device.
        unsafe {
            let vol: IAudioEndpointVolume = dev
                .Activate(CLSCTX_ALL, None)
                .ctx("Activate(IAudioEndpointVolume)")?;
            vol.SetMasterVolumeLevelScalar(v, std::ptr::null())
                .ctx("SetMasterVolumeLevelScalar")
        }
    }

    /// Windows has no Multi-Output device. Instead this (re)configures Duet's single
    /// software mirror and returns its synthetic id [`MIRROR_DEVICE_ID`]; making that the
    /// default output (`set_default_output`) starts mirroring. Needs >= 2 members.
    fn create_multi_output(&self, name: &str, member_uids: &[String]) -> Result<DeviceId> {
        if member_uids.len() < 2 {
            return Err(AudioError::Other(
                "Windows has no built-in Multi-Output device; Duet's software mirror needs at least two outputs".into(),
            ));
        }
        let (eps, _) = list_endpoints()?;
        let mut members = Vec::with_capacity(member_uids.len());
        for uid in member_uids {
            let ep = wu::endpoint_id_from_uid(uid);
            let found = eps
                .iter()
                .find(|e| e.endpoint_id.eq_ignore_ascii_case(ep))
                .ok_or_else(|| AudioError::NotFound(uid.clone()))?;
            members.push(found.endpoint_id.clone());
        }
        let old = {
            let mut st = self.state();
            st.mirror_config = Some(MirrorConfig {
                name: name.to_string(),
                members,
            });
            st.mirror.take()
        };
        drop(old);
        Ok(MIRROR_DEVICE_ID)
    }
}

// ---------------------------------------------------------------------------
// Software mirror (UNTESTED on hardware)
// ---------------------------------------------------------------------------

/// Loopback-capture buffer requested from WASAPI (100-ns units): 200 ms.
const CAPTURE_BUFFER_HNS: i64 = 2_000_000;
/// Render buffer per target: 100 ms.
const RENDER_BUFFER_HNS: i64 = 1_000_000;
/// Max audio (queued + in the device buffer) per target before old frames are dropped.
/// Bounds the extra delay of mirrored members vs. the source when clocks drift.
const MAX_TARGET_LATENCY_HNS: i64 = 800_000;
/// Poll period of the mirror thread.
const POLL: Duration = Duration::from_millis(5);

/// Software "multi-output": WASAPI loopback capture of `source` rendered to `targets`.
///
/// Runs on its own thread with its own COM objects. Frames are decoded to f32, channel
/// mapped and (if rates differ) linearly resampled per target. Mirrored targets lag the
/// source by roughly the loopback period + render buffer (~20–60 ms, plus whatever the
/// Bluetooth stacks add); targets whose clocks drift are kept in bounds by dropping
/// frames. Any WASAPI error (e.g. a headphone disconnecting) ends the thread; read it
/// with [`Mirror::error`].
pub struct Mirror {
    stop: Arc<AtomicBool>,
    error: Arc<Mutex<Option<String>>>,
    thread: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for Mirror {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Mirror")
            .field("running", &self.is_running())
            .field("error", &self.error())
            .finish()
    }
}

impl Mirror {
    /// Start mirroring `source` (endpoint id or Duet uid) to `targets`. Returns once all
    /// streams are open and started, or with the setup error.
    pub fn start(source: &str, targets: &[String]) -> Result<Mirror> {
        let source = wu::endpoint_id_from_uid(source).to_string();
        let targets: Vec<String> = targets
            .iter()
            .map(|t| wu::endpoint_id_from_uid(t).to_string())
            .collect();
        let stop = Arc::new(AtomicBool::new(false));
        let error = Arc::new(Mutex::new(None));
        let (tx, rx) = mpsc::channel();
        let (s2, e2) = (stop.clone(), error.clone());
        let thread = std::thread::Builder::new()
            .name("duet-mirror".into())
            .spawn(move || mirror_thread(&source, &targets, &s2, &e2, tx))
            .map_err(|e| AudioError::Other(format!("could not start mirror thread: {e}")))?;
        let mirror = Mirror {
            stop,
            error,
            thread: Some(thread),
        };
        match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(())) => Ok(mirror),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(AudioError::Other("mirror did not start within 5 s".into())),
        }
    }

    pub fn is_running(&self) -> bool {
        self.thread.as_ref().is_some_and(|t| !t.is_finished())
    }

    /// The error that stopped the mirror, if it stopped on its own.
    pub fn error(&self) -> Option<String> {
        self.error.lock().ok().and_then(|e| e.clone())
    }

    /// Stop and join the mirror thread.
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for Mirror {
    fn drop(&mut self) {
        self.shutdown();
    }
}

struct Source {
    client: IAudioClient,
    capture: IAudioCaptureClient,
    fmt: wu::StreamFormat,
}

struct Target {
    client: IAudioClient,
    render: IAudioRenderClient,
    fmt: wu::StreamFormat,
    buffer_frames: u32,
    max_frames: usize,
    conv: wu::Converter,
}

/// Read a WASAPI mix format and free it.
unsafe fn take_mix_format(p: *mut WAVEFORMATEX) -> Result<wu::StreamFormat> {
    unsafe {
        let wf: WAVEFORMATEX = std::ptr::read_unaligned(p);
        let sub = (wf.wFormatTag == wu::WAVE_FORMAT_EXTENSIBLE && wf.cbSize >= 22).then(|| {
            let ext: WAVEFORMATEXTENSIBLE = std::ptr::read_unaligned(p.cast());
            ext.SubFormat.data1
        });
        let fmt = wu::stream_format(
            wf.wFormatTag,
            wf.nChannels,
            wf.nSamplesPerSec,
            wf.wBitsPerSample,
            sub,
        );
        fmt.ok_or_else(|| {
            AudioError::Other(format!(
                "unsupported mix format (tag {:#x}, {} bit, {} ch)",
                { wf.wFormatTag },
                { wf.wBitsPerSample },
                { wf.nChannels }
            ))
        })
    }
}

/// Open a shared-mode client on `endpoint_id` in its own mix format.
unsafe fn open_client(
    endpoint_id: &str,
    flags: u32,
    buffer_hns: i64,
) -> Result<(IAudioClient, wu::StreamFormat)> {
    unsafe {
        let dev = get_device(endpoint_id)?;
        let client: IAudioClient = dev
            .Activate(CLSCTX_ALL, None)
            .ctx("Activate(IAudioClient)")?;
        let p = client.GetMixFormat().ctx("IAudioClient::GetMixFormat")?;
        let init = client.Initialize(AUDCLNT_SHAREMODE_SHARED, flags, buffer_hns, 0, p, None);
        let fmt = take_mix_format(p);
        CoTaskMemFree(Some(p as *const _));
        init.ctx("IAudioClient::Initialize")?;
        Ok((client, fmt?))
    }
}

unsafe fn setup(source: &str, targets: &[String]) -> Result<(Source, Vec<Target>)> {
    unsafe {
        let (client, fmt) = open_client(source, AUDCLNT_STREAMFLAGS_LOOPBACK, CAPTURE_BUFFER_HNS)?;
        let capture: IAudioCaptureClient =
            client.GetService().ctx("GetService(IAudioCaptureClient)")?;
        let src = Source {
            client,
            capture,
            fmt,
        };
        let mut outs = Vec::with_capacity(targets.len());
        for t in targets {
            let (client, tfmt) = open_client(t, 0, RENDER_BUFFER_HNS)?;
            let render: IAudioRenderClient =
                client.GetService().ctx("GetService(IAudioRenderClient)")?;
            let buffer_frames = client.GetBufferSize().ctx("IAudioClient::GetBufferSize")?;
            outs.push(Target {
                client,
                render,
                fmt: tfmt,
                buffer_frames,
                max_frames: wu::hns_to_frames(MAX_TARGET_LATENCY_HNS, tfmt.rate),
                conv: wu::Converter::new((fmt.rate, fmt.channels), (tfmt.rate, tfmt.channels)),
            });
        }
        for t in &outs {
            t.client.Start().ctx("IAudioClient::Start (render)")?;
        }
        src.client.Start().ctx("IAudioClient::Start (loopback)")?;
        Ok((src, outs))
    }
}

/// Move everything captured so far to the targets.
unsafe fn pump(
    src: &Source,
    targets: &mut [Target],
    samples: &mut Vec<f32>,
    out: &mut Vec<f32>,
    bytes: &mut Vec<u8>,
) -> Result<()> {
    unsafe {
        let block = src.fmt.block_align();
        loop {
            let packet = src.capture.GetNextPacketSize().ctx("GetNextPacketSize")?;
            if packet == 0 {
                break;
            }
            let mut data: *mut u8 = std::ptr::null_mut();
            let (mut frames, mut flags) = (0u32, 0u32);
            src.capture
                .GetBuffer(&mut data, &mut frames, &mut flags, None, None)
                .ctx("IAudioCaptureClient::GetBuffer")?;
            samples.clear();
            if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 || data.is_null() {
                samples.resize(frames as usize * src.fmt.channels as usize, 0.0);
            } else {
                let raw = std::slice::from_raw_parts(data, frames as usize * block);
                wu::decode_to_f32(raw, src.fmt.sample, samples);
            }
            src.capture
                .ReleaseBuffer(frames)
                .ctx("IAudioCaptureClient::ReleaseBuffer")?;
            for t in targets.iter_mut() {
                t.conv.push(samples);
            }
        }
        for t in targets.iter_mut() {
            let padding = t.client.GetCurrentPadding().ctx("GetCurrentPadding")?;
            t.conv
                .trim_to(t.max_frames.saturating_sub(padding as usize));
            let free = t.buffer_frames.saturating_sub(padding) as usize;
            let n = t.conv.pop(free, out);
            if n == 0 {
                continue;
            }
            let len = n * t.fmt.block_align();
            bytes.resize(len, 0);
            wu::encode_from_f32(out, t.fmt.sample, &mut bytes[..len]);
            let dst = t
                .render
                .GetBuffer(n as u32)
                .ctx("IAudioRenderClient::GetBuffer")?;
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), dst, len);
            t.render
                .ReleaseBuffer(n as u32, 0)
                .ctx("IAudioRenderClient::ReleaseBuffer")?;
        }
        Ok(())
    }
}

fn mirror_thread(
    source: &str,
    targets: &[String],
    stop: &AtomicBool,
    error: &Mutex<Option<String>>,
    ready: mpsc::Sender<Result<()>>,
) {
    com_init();
    // SAFETY: all COM objects are created and used only on this thread.
    let (src, mut outs) = match unsafe { setup(source, targets) } {
        Ok(v) => {
            let _ = ready.send(Ok(()));
            v
        }
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    let (mut samples, mut out, mut bytes) = (Vec::new(), Vec::new(), Vec::new());
    while !stop.load(Ordering::SeqCst) {
        // SAFETY: see above.
        if let Err(e) = unsafe { pump(&src, &mut outs, &mut samples, &mut out, &mut bytes) } {
            if let Ok(mut slot) = error.lock() {
                *slot = Some(format!("mirror stopped: {e}"));
            }
            break;
        }
        std::thread::sleep(POLL);
    }
    // SAFETY: stopping streams we started; errors are irrelevant at this point.
    unsafe {
        let _ = src.client.Stop();
        for t in &outs {
            let _ = t.client.Stop();
        }
    }
}

// ---------------------------------------------------------------------------
// Bluetooth (WinRT)
// ---------------------------------------------------------------------------

/// Paired Bluetooth devices via WinRT. Connect/disconnect of audio devices has no public
/// Windows API, so those return an error pointing to Settings (see [`open_bluetooth_settings`]).
#[derive(Debug, Default)]
pub struct WindowsBluetooth;

impl WindowsBluetooth {
    pub fn new() -> Self {
        Self
    }
}

const CONNECT_UNSUPPORTED: &str = "Windows doesn't let apps connect or disconnect Bluetooth audio. Use Settings > Bluetooth & devices";

impl BluetoothBackend for WindowsBluetooth {
    fn paired_devices(&self) -> Result<Vec<BluetoothDevice>> {
        use windows::Devices::Bluetooth::{
            BluetoothConnectionStatus, BluetoothDevice as WinBtDevice, BluetoothMajorClass,
        };
        use windows::Devices::Enumeration::DeviceInformation;
        com_init();
        let bt = |e: windows::core::Error| AudioError::Bluetooth(e.message());
        let selector = WinBtDevice::GetDeviceSelectorFromPairingState(true).map_err(bt)?;
        let infos = DeviceInformation::FindAllAsyncAqsFilter(&selector)
            .and_then(|op| op.join())
            .map_err(bt)?;
        let mut out = Vec::new();
        for i in 0..infos.Size().map_err(bt)? {
            let Ok(info) = infos.GetAt(i) else { continue };
            let Ok(id) = info.Id() else { continue };
            // FromIdAsync can fail for stale pairings; skip those rather than failing the list.
            let Ok(dev) = WinBtDevice::FromIdAsync(&id).and_then(|op| op.join()) else {
                continue;
            };
            let Ok(addr) = dev.BluetoothAddress() else {
                continue;
            };
            let name = dev
                .Name()
                .or_else(|_| info.Name())
                .map(|n| n.to_string())
                .unwrap_or_default();
            let address = wu::format_bt_address(addr);
            out.push(BluetoothDevice {
                name: if name.is_empty() {
                    address.clone()
                } else {
                    name
                },
                address,
                connected: dev
                    .ConnectionStatus()
                    .is_ok_and(|s| s == BluetoothConnectionStatus::Connected),
                is_audio: dev
                    .ClassOfDevice()
                    .and_then(|c| c.MajorClass())
                    .is_ok_and(|m| m == BluetoothMajorClass::AudioVideo),
            });
        }
        Ok(out)
    }

    fn connect(&self, _address: &str) -> Result<()> {
        Err(AudioError::Bluetooth(format!(
            "{CONNECT_UNSUPPORTED} to connect."
        )))
    }

    fn disconnect(&self, _address: &str) -> Result<()> {
        Err(AudioError::Bluetooth(format!(
            "{CONNECT_UNSUPPORTED} to disconnect."
        )))
    }
}

/// Open Settings > Bluetooth & devices (`ms-settings:bluetooth`).
pub fn open_bluetooth_settings() -> Result<()> {
    // explorer.exe resolves the URI without flashing a console window. Its exit code is
    // meaningless (it's 1 on success), so only a spawn failure is an error.
    std::process::Command::new("explorer.exe")
        .arg("ms-settings:bluetooth")
        .spawn()
        .map(drop)
        .map_err(|e| AudioError::Other(format!("could not open Bluetooth settings: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classification() {
        assert_eq!(
            classify(Some("BTHENUM"), None, None, None),
            Transport::Bluetooth
        );
        assert_eq!(
            classify(None, Some(r"{1}.BTHHFENUM\x\8&1&0&97"), None, None),
            Transport::Bluetooth
        );
        assert_eq!(
            classify(Some("HDAUDIO"), None, None, Some(1)),
            Transport::BuiltIn
        );
        assert_eq!(
            classify(Some("HDAUDIO"), None, None, Some(9)),
            Transport::Other
        );
        assert_eq!(classify(Some("USB"), None, None, None), Transport::Other);
        assert_eq!(classify(Some("ROOT"), None, None, None), Transport::Virtual);
    }

    #[test]
    fn send_sync() {
        fn ok<T: Send + Sync + 'static>() {}
        ok::<WindowsAudio>();
        ok::<WindowsBluetooth>();
        ok::<Mirror>();
    }
}
