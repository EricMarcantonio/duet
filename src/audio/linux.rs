//! Linux backends.
//!
//! * Audio: the `pactl` CLI, which talks to PulseAudio *and* to PipeWire (through
//!   `pipewire-pulse`), so no libpulse linking is needed. A Duet "multi-output" is a
//!   `module-combine-sink` whose members ("slaves") are the headphone sinks; each
//!   headphone keeps its own sink volume.
//! * Bluetooth: the BlueZ `bluetoothctl` CLI.
//! * AirPods / Beats extras (noise control, battery): Apple's AAP protocol over an
//!   L2CAP socket (PSM 0x1001), one background session thread per device. Packet
//!   building/parsing lives in [`super::aap`].
//!
//! All output parsing lives in [`super::parse_linux`] (unit-tested on every host); this
//! file only runs commands.

use super::parse_linux as p;
use super::{
    AudioBackend, AudioError, BluetoothBackend, BluetoothDevice, DeviceId, HeadphoneInfo,
    ListeningMode, OutputDevice, Result, aap, normalize_address,
};
use std::collections::HashMap;
use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

const PACTL_TIMEOUT: Duration = Duration::from_secs(5);
const BT_QUERY_TIMEOUT: Duration = Duration::from_secs(5);
const BT_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

struct Output {
    ok: bool,
    stdout: String,
    stderr: String,
}

/// Run `program args...` with `LC_ALL=C`, killing it after `timeout`.
fn run(program: &str, args: &[&str], timeout: Duration) -> std::result::Result<Output, String> {
    let mut child = Command::new(program)
        .args(args)
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                format!("`{program}` not found; install it (pulseaudio-utils / bluez) to use Duet")
            } else {
                format!("could not run `{program}`: {e}")
            }
        })?;
    // Drain pipes on threads so a chatty child can't block on a full pipe.
    let mut so = child.stdout.take().expect("piped");
    let mut se = child.stderr.take().expect("piped");
    let t_out = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = so.read_to_string(&mut s);
        s
    });
    let t_err = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = se.read_to_string(&mut s);
        s
    });
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st,
            Ok(None) if start.elapsed() > timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("`{program} {}` timed out", args.join(" ")));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(e) => return Err(format!("waiting for `{program}`: {e}")),
        }
    };
    Ok(Output {
        ok: status.success(),
        stdout: t_out.join().unwrap_or_default(),
        stderr: t_err.join().unwrap_or_default(),
    })
}

fn pactl(args: &[&str]) -> Result<String> {
    let out = run("pactl", args, PACTL_TIMEOUT).map_err(AudioError::Other)?;
    if out.ok {
        Ok(out.stdout)
    } else {
        Err(AudioError::Other(format!(
            "pactl {} failed: {}",
            args.join(" "),
            out.stderr.trim()
        )))
    }
}

/// PulseAudio / PipeWire (pipewire-pulse) output backend.
#[derive(Debug, Default)]
pub struct LinuxAudio;

impl LinuxAudio {
    pub fn new() -> Self {
        Self
    }

    fn sinks(&self) -> Result<Vec<p::PaSink>> {
        // JSON needs pactl >= 16; older versions reject `-f` -> fall back to text.
        if let Ok(json) = pactl(&["-f", "json", "list", "sinks"])
            && let Ok(sinks) = p::parse_sinks_json(&json)
        {
            return Ok(sinks);
        }
        Ok(p::parse_sinks_text(&pactl(&["list", "sinks"])?))
    }

    fn modules(&self) -> Vec<p::PaModule> {
        pactl(&["list", "short", "modules"])
            .map(|s| p::parse_modules_short(&s))
            .unwrap_or_default()
    }

    fn default_sink(&self) -> Option<String> {
        // `get-default-sink` needs pactl >= 15.
        pactl(&["get-default-sink"])
            .ok()
            .and_then(|s| p::parse_default_sink(&s))
            .or_else(|| {
                pactl(&["info"])
                    .ok()
                    .and_then(|s| p::parse_info_default_sink(&s))
            })
    }

    fn sink_name(&self, id: DeviceId) -> Result<String> {
        self.sinks()?
            .into_iter()
            .find(|s| s.index == id)
            .map(|s| s.name)
            .ok_or_else(|| AudioError::NotFound(id.to_string()))
    }
}

impl AudioBackend for LinuxAudio {
    fn output_devices(&self) -> Result<Vec<OutputDevice>> {
        let sinks = self.sinks()?;
        let modules = self.modules();
        let default = self.default_sink();
        Ok(p::sinks_to_devices(&sinks, &modules, default.as_deref()))
    }

    fn set_default_output(&self, id: DeviceId) -> Result<()> {
        let name = self.sink_name(id)?;
        pactl(&["set-default-sink", &name]).map(drop)
    }

    fn set_volume(&self, id: DeviceId, volume: f32) -> Result<()> {
        let name = self.sink_name(id)?;
        pactl(&["set-sink-volume", &name, &p::volume_arg(volume)]).map(drop)
    }

    fn create_multi_output(&self, name: &str, member_uids: &[String]) -> Result<DeviceId> {
        if member_uids.is_empty() {
            return Err(AudioError::Other(
                "a multi-output needs at least one member".into(),
            ));
        }
        let sinks = self.sinks()?;
        for uid in member_uids {
            if !sinks.iter().any(|s| &s.name == uid) {
                return Err(AudioError::NotFound(uid.clone()));
            }
        }
        let sink_name = p::next_duet_sink_name(&sinks);
        let args = p::combine_sink_args(&sink_name, name, member_uids);
        let mut argv = vec!["load-module", "module-combine-sink"];
        argv.extend(args.iter().map(String::as_str));
        pactl(&argv)?;
        // pipewire-pulse creates the node asynchronously; poll briefly for it.
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(s) = self.sinks()?.into_iter().find(|s| s.name == sink_name) {
                return Ok(s.index);
            }
            if Instant::now() > deadline {
                return Err(AudioError::Other(format!(
                    "module-combine-sink loaded but sink `{sink_name}` did not appear (is pipewire-pulse/PulseAudio running?)"
                )));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// Unload the module-combine-sink behind a Duet-made sink (`duet_N`).
    fn remove_multi_output(&self, id: DeviceId) -> Result<()> {
        let name = self.sink_name(id)?;
        if !crate::audio::is_duet_group_uid(&name) {
            return Err(AudioError::Other(format!(
                "refusing to remove non-Duet sink {name}"
            )));
        }
        let module = p::combine_module_for(&self.modules(), &name)
            .ok_or_else(|| AudioError::NotFound(format!("module for {name}")))?;
        pactl(&["unload-module", &module.to_string()]).map(drop)
    }
}

/// BlueZ backend via `bluetoothctl`, plus AAP sessions for AirPods / Beats.
#[derive(Debug, Default)]
pub struct LinuxBluetooth {
    aap: Mutex<AapSessions>,
}

impl LinuxBluetooth {
    pub fn new() -> Self {
        Self::default()
    }

    fn btctl(args: &[&str], timeout: Duration) -> Result<Output> {
        run("bluetoothctl", args, timeout).map_err(AudioError::Bluetooth)
    }

    fn sessions(&self) -> MutexGuard<'_, AapSessions> {
        self.aap.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl BluetoothBackend for LinuxBluetooth {
    fn paired_devices(&self) -> Result<Vec<BluetoothDevice>> {
        // BlueZ >= 5.66: `devices Paired`; older: `paired-devices` (removed in 5.66).
        let mut list = Self::btctl(&["devices", "Paired"], BT_QUERY_TIMEOUT)
            .map(|o| p::parse_bt_device_list(&o.stdout))?;
        if list.is_empty()
            && let Ok(o) = Self::btctl(&["paired-devices"], BT_QUERY_TIMEOUT)
        {
            list = p::parse_bt_device_list(&o.stdout);
        }
        let mut out = Vec::with_capacity(list.len());
        for (addr, name) in list {
            let info = Self::btctl(&["info", &p::colon_address(&addr)], BT_QUERY_TIMEOUT)
                .ok()
                .and_then(|o| p::parse_bt_info(&o.stdout));
            match info {
                // Pre-5.66 `devices Paired` ignores the filter: drop unpaired devices here.
                Some(i) if i.paired == Some(false) => {}
                Some(i) => out.push(i.to_device(&name)),
                None => out.push(BluetoothDevice {
                    address: addr,
                    name,
                    connected: false,
                    is_audio: false,
                }),
            }
        }
        Ok(out)
    }

    fn connect(&self, address: &str) -> Result<()> {
        let o = Self::btctl(&["connect", &p::colon_address(address)], BT_CONNECT_TIMEOUT)?;
        p::bt_command_result(&format!("{}{}", o.stdout, o.stderr), o.ok)
            .map_err(AudioError::Bluetooth)
    }

    fn disconnect(&self, address: &str) -> Result<()> {
        // Stop the AAP session first (its Drop signals the thread).
        self.sessions().sessions.remove(&normalize_address(address));
        let o = Self::btctl(
            &["disconnect", &p::colon_address(address)],
            BT_CONNECT_TIMEOUT,
        )?;
        p::bt_command_result(&format!("{}{}", o.stdout, o.stderr), o.ok)
            .map_err(AudioError::Bluetooth)
    }

    /// Never blocks on Bluetooth: returns the session's cached state, starting a session
    /// in the background on first call. `Ok(None)` while connecting, for non-Apple
    /// devices, and (for [`AAP_FAILURE_TTL`]) after a failed connection.
    fn headphone_info(&self, address: &str) -> Result<Option<HeadphoneInfo>> {
        let addr = normalize_address(address);
        let mut g = self.sessions();
        g.failures.retain(|_, t| t.elapsed() < AAP_FAILURE_TTL);
        if let Some(session) = g.sessions.get(&addr) {
            let (phase, info) = {
                let sh = session.shared();
                (sh.phase, sh.state.info())
            };
            return Ok(match phase {
                Phase::Connected => info,
                Phase::Starting | Phase::Reconnecting => None,
                Phase::Failed => {
                    g.sessions.remove(&addr);
                    g.failures.insert(addr, Instant::now());
                    None
                }
                Phase::Gone => {
                    g.sessions.remove(&addr);
                    None
                }
            });
        }
        if g.failures.contains_key(&addr) {
            return Ok(None);
        }
        match Session::spawn(&addr) {
            Ok(s) => {
                g.sessions.insert(addr, s);
            }
            Err(_) => {
                g.failures.insert(addr, Instant::now());
            }
        }
        Ok(None)
    }

    fn set_listening_mode(&self, address: &str, mode: ListeningMode) -> Result<()> {
        let addr = normalize_address(address);
        let shared = self
            .sessions()
            .sessions
            .get(&addr)
            .map(|s| Arc::clone(&s.shared))
            .ok_or_else(|| AudioError::Bluetooth("noise control isn't connected".into()))?;
        let fd = lock(&shared)
            .socket
            .clone()
            .ok_or_else(|| AudioError::Bluetooth("noise control is reconnecting".into()))?;
        send_packet(&fd, &aap::listening_mode_packet(mode))
            .map_err(|e| AudioError::Bluetooth(format!("setting noise control: {e}")))?;
        // Optimistic; the device confirms with a 0x09/0x0D notification.
        lock(&shared).state.listening_mode = Some(mode);
        Ok(())
    }
}

// ---------------------------------------------------------------------------------
// AAP sessions (AirPods noise control + battery over L2CAP PSM 0x1001)
// ---------------------------------------------------------------------------------

/// How long a failed AAP attempt (no permission, BlueZ refused, not an Apple device)
/// suppresses retries for that address.
const AAP_FAILURE_TTL: Duration = Duration::from_secs(60);
const AAP_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// recv/send timeout once connected; also bounds how long a stopped session lingers.
const AAP_IO_TIMEOUT: Duration = Duration::from_secs(1);
const AAP_MAX_BACKOFF: Duration = Duration::from_secs(30);

// <bluetooth/bluetooth.h>, <bluetooth/l2cap.h> (not exported by `libc`).
const BTPROTO_L2CAP: libc::c_int = 0;
const BDADDR_BREDR: u8 = 0;

/// `struct sockaddr_l2` from <bluetooth/l2cap.h>.
#[repr(C)]
struct SockaddrL2 {
    l2_family: libc::sa_family_t,
    /// Little-endian ("htobs").
    l2_psm: u16,
    /// Little-endian, i.e. reversed from the printed address.
    l2_bdaddr: [u8; 6],
    l2_cid: u16,
    l2_bdaddr_type: u8,
}

#[derive(Debug, Default)]
struct AapSessions {
    sessions: HashMap<String, Session>,
    failures: HashMap<String, Instant>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Probing `bluetoothctl info` / opening the first connection.
    Starting,
    Connected,
    /// The socket died while the device is still connected; retrying with backoff.
    Reconnecting,
    /// Never connected (not Apple, no permission, refused): cache and drop.
    Failed,
    /// The device disconnected: drop without caching.
    Gone,
}

#[derive(Debug)]
struct Shared {
    phase: Phase,
    state: aap::AapState,
    /// Present while connected; `Arc` so a writer can't race the reader closing it.
    socket: Option<Arc<OwnedFd>>,
}

fn lock(m: &Mutex<Shared>) -> MutexGuard<'_, Shared> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Debug)]
struct Session {
    shared: Arc<Mutex<Shared>>,
    stop: Arc<AtomicBool>,
}

impl Drop for Session {
    fn drop(&mut self) {
        // The thread notices within AAP_IO_TIMEOUT and closes the socket.
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Session {
    fn spawn(addr: &str) -> std::io::Result<Self> {
        let shared = Arc::new(Mutex::new(Shared {
            phase: Phase::Starting,
            state: aap::AapState::default(),
            socket: None,
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let (a, sh, st) = (addr.to_string(), Arc::clone(&shared), Arc::clone(&stop));
        std::thread::Builder::new()
            .name(format!("duet-aap-{addr}"))
            .spawn(move || {
                let phase = run_session(&a, &sh, &st);
                let mut g = lock(&sh);
                g.phase = phase;
                g.socket = None;
            })?;
        Ok(Self { shared, stop })
    }

    fn shared(&self) -> MutexGuard<'_, Shared> {
        lock(&self.shared)
    }
}

/// Session thread body; returns the terminal phase (`Failed` or `Gone`).
fn run_session(addr: &str, shared: &Mutex<Shared>, stop: &AtomicBool) -> Phase {
    let colon = p::colon_address(addr);
    let probe = || {
        LinuxBluetooth::btctl(&["info", &colon], BT_QUERY_TIMEOUT)
            .ok()
            .and_then(|o| p::parse_bt_info(&o.stdout))
    };
    let Some(info) = probe() else {
        return Phase::Failed;
    };
    if !info.connected {
        return Phase::Gone;
    }
    let name = info.alias.as_deref().or(info.name.as_deref()).unwrap_or("");
    if !aap::looks_apple(name, info.modalias.as_deref()) {
        return Phase::Failed;
    }
    let Some(bdaddr) = aap::bdaddr_bytes(addr) else {
        return Phase::Failed;
    };
    lock(shared).state.product_id = info
        .modalias
        .as_deref()
        .and_then(aap::parse_modalias)
        .filter(|(vendor, _)| *vendor == aap::APPLE_VENDOR_ID)
        .map(|(_, product)| product);

    let mut ever_connected = false;
    let mut backoff = Duration::from_secs(1);
    loop {
        if stop.load(Ordering::Relaxed) {
            return Phase::Gone;
        }
        match open_aap_socket(bdaddr) {
            Ok(fd) => {
                ever_connected = true;
                backoff = Duration::from_secs(1);
                let fd = Arc::new(fd);
                {
                    let mut g = lock(shared);
                    g.socket = Some(Arc::clone(&fd));
                    g.phase = Phase::Connected;
                }
                read_loop(&fd, shared, stop);
                let mut g = lock(shared);
                g.socket = None;
                g.phase = Phase::Reconnecting;
            }
            Err(e) if !ever_connected => {
                eprintln!("duet: AirPods controls unavailable for {addr}: {e}");
                return Phase::Failed;
            }
            Err(_) => {}
        }
        if stop.load(Ordering::Relaxed) {
            return Phase::Gone;
        }
        // Socket closed: reconnect only if the device itself is still connected.
        if !probe().is_some_and(|i| i.connected) {
            return Phase::Gone;
        }
        let until = Instant::now() + backoff;
        while Instant::now() < until {
            if stop.load(Ordering::Relaxed) {
                return Phase::Gone;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        backoff = (backoff * 2).min(AAP_MAX_BACKOFF);
    }
}

/// Feed notifications into `shared` until the socket errors/closes or `stop` is set.
fn read_loop(fd: &OwnedFd, shared: &Mutex<Shared>, stop: &AtomicBool) {
    let mut buf = [0u8; 1024];
    while !stop.load(Ordering::Relaxed) {
        // SAFETY: `buf` is valid for writes of `buf.len()` bytes; `fd` is an open socket.
        let n = unsafe { libc::recv(fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len(), 0) };
        if n > 0 {
            let len = usize::try_from(n).unwrap_or(0).min(buf.len());
            if let Some(note) = aap::parse_notification(&buf[..len]) {
                lock(shared).state.apply(note);
            }
            continue;
        }
        if n == 0 {
            return; // orderly shutdown
        }
        match std::io::Error::last_os_error().kind() {
            std::io::ErrorKind::WouldBlock
            | std::io::ErrorKind::TimedOut
            | std::io::ErrorKind::Interrupted => {}
            _ => return,
        }
    }
}

fn set_timeout(fd: &OwnedFd, opt: libc::c_int, d: Duration) -> std::io::Result<()> {
    let tv = libc::timeval {
        tv_sec: d.as_secs() as libc::time_t,
        tv_usec: d.subsec_micros() as libc::suseconds_t,
    };
    // SAFETY: `tv` is a valid `timeval` and the length matches its size.
    let rc = unsafe {
        libc::setsockopt(
            fd.as_raw_fd(),
            libc::SOL_SOCKET,
            opt,
            (&raw const tv).cast(),
            std::mem::size_of::<libc::timeval>() as libc::socklen_t,
        )
    };
    if rc < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Open the AAP L2CAP channel and send the init sequence.
fn open_aap_socket(bdaddr: [u8; 6]) -> std::io::Result<OwnedFd> {
    // SAFETY: plain syscall; the result is checked before use.
    let raw = unsafe {
        libc::socket(
            libc::AF_BLUETOOTH,
            libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC,
            BTPROTO_L2CAP,
        )
    };
    if raw < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `raw` is a freshly created, owned file descriptor.
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    // L2CAP `connect` waits up to SO_SNDTIMEO for the channel to open.
    set_timeout(&fd, libc::SO_SNDTIMEO, AAP_CONNECT_TIMEOUT)?;
    set_timeout(&fd, libc::SO_RCVTIMEO, AAP_IO_TIMEOUT)?;
    let sa = SockaddrL2 {
        l2_family: libc::AF_BLUETOOTH as libc::sa_family_t,
        l2_psm: aap::PSM.to_le(),
        l2_bdaddr: bdaddr,
        l2_cid: 0,
        l2_bdaddr_type: BDADDR_BREDR,
    };
    // SAFETY: `sa` is a valid sockaddr_l2 and the length matches its size.
    let rc = unsafe {
        libc::connect(
            fd.as_raw_fd(),
            (&raw const sa).cast(),
            std::mem::size_of::<SockaddrL2>() as libc::socklen_t,
        )
    };
    if rc < 0 {
        return Err(std::io::Error::last_os_error());
    }
    set_timeout(&fd, libc::SO_SNDTIMEO, AAP_IO_TIMEOUT)?;
    for pkt in aap::INIT_SEQUENCE {
        send_packet(&fd, pkt)?;
    }
    Ok(fd)
}

fn send_packet(fd: &OwnedFd, pkt: &[u8]) -> std::io::Result<()> {
    // SAFETY: `pkt` is valid for reads of `pkt.len()` bytes; `fd` is an open socket.
    let n = unsafe {
        libc::send(
            fd.as_raw_fd(),
            pkt.as_ptr().cast(),
            pkt.len(),
            libc::MSG_NOSIGNAL,
        )
    };
    if n < 0 {
        Err(std::io::Error::last_os_error())
    } else if usize::try_from(n).ok() != Some(pkt.len()) {
        Err(std::io::Error::new(
            std::io::ErrorKind::WriteZero,
            "short write on AAP socket",
        ))
    } else {
        Ok(())
    }
}

/// Open the desktop's Bluetooth settings. There is no cross-desktop standard, so this
/// tries GNOME, KDE Plasma, then Blueman, and returns an error if none is installed.
pub fn open_bluetooth_settings() -> Result<()> {
    const CANDIDATES: [(&str, &[&str]); 4] = [
        ("gnome-control-center", &["bluetooth"]),
        ("systemsettings", &["kcm_bluetooth"]),
        ("kcmshell6", &["kcm_bluetooth"]),
        ("blueman-manager", &[]),
    ];
    for (prog, args) in CANDIDATES {
        let spawned = Command::new(prog)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        if let Ok(mut child) = spawned {
            // Reap it when it exits so it doesn't linger as a zombie.
            std::thread::spawn(move || child.wait());
            return Ok(());
        }
    }
    Err(AudioError::Bluetooth(
        "no Bluetooth settings app found (tried gnome-control-center, systemsettings, kcmshell6, blueman-manager)".into(),
    ))
}
