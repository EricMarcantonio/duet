//! Pure, UI-framework-free logic behind the Duet window (unit-testable).

use crate::audio::{BluetoothDevice, OutputDevice, Transport, address_from_uid, normalize_address};
use std::collections::HashMap;
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// Nicknames
// ---------------------------------------------------------------------------

/// Derive a friendly default nickname from a device name.
///
/// "Alex's AirPod Pro 2's" -> "Alex", "Sam’s AirPods Pro #2 - Find My" -> "Sam",
/// "MacBook Air Speakers" -> "MacBook Air Speakers".
pub fn default_nickname(device_name: &str) -> String {
    let mut name = device_name.trim();
    for suffix in [" - Find My", " – Find My", " — Find My"] {
        if let Some(stripped) = name.strip_suffix(suffix) {
            name = stripped.trim_end();
        }
    }
    for apos in ['\'', '’', 'ʼ'] {
        let pat = format!("{apos}s");
        if let Some(idx) = name.find(&pat) {
            let rest = &name[idx + pat.len()..];
            let owner = name[..idx].trim();
            if !owner.is_empty() && (rest.is_empty() || rest.starts_with(' ')) {
                return owner.to_string();
            }
        }
    }
    name.to_string()
}

// ---------------------------------------------------------------------------
// Volume
// ---------------------------------------------------------------------------

/// Scalar 0.0..=1.0 -> whole percent 0..=100 (rounded, clamped, NaN -> 0).
pub fn to_percent(volume: f32) -> u8 {
    if volume.is_nan() {
        return 0;
    }
    (volume.clamp(0.0, 1.0) * 100.0).round() as u8
}

/// Percent (any number) -> scalar 0.0..=1.0 (clamped, NaN -> 0).
pub fn from_percent(percent: f64) -> f32 {
    if percent.is_nan() {
        return 0.0;
    }
    (percent.clamp(0.0, 100.0) / 100.0) as f32
}

/// Parse a range-input value ("42", "42.5") to a scalar volume.
pub fn parse_slider(value: &str) -> Option<f32> {
    value.trim().parse::<f64>().ok().map(from_percent)
}

// ---------------------------------------------------------------------------
// Device selection / listener matching
// ---------------------------------------------------------------------------

/// The Multi-Output device to feature: the default one if any, else the first.
pub fn primary_multi_output(devices: &[OutputDevice]) -> Option<&OutputDevice> {
    let mut multis = devices.iter().filter(|d| d.is_multi_output());
    let first = multis.clone().next();
    multis.find(|d| d.is_default).or(first)
}

/// One member slot of a Multi-Output device.
#[derive(Debug, Clone, PartialEq)]
pub enum Listener {
    /// The member is present as an output device.
    Connected(OutputDevice),
    /// Not an output right now, but it's a paired Bluetooth device we can connect.
    Disconnected {
        uid: String,
        bluetooth: BluetoothDevice,
    },
    /// Not present and not a known paired device.
    Unknown { uid: String },
}

impl Listener {
    pub fn uid(&self) -> &str {
        match self {
            Listener::Connected(d) => &d.uid,
            Listener::Disconnected { uid, .. } | Listener::Unknown { uid } => uid,
        }
    }

    /// Like [`Listener::device_name`], but falls back to the last name we saw
    /// for this UID before using a raw Bluetooth address.
    pub fn device_name_with(&self, config: &crate::config::Config) -> String {
        match (self, config.known_name(self.uid())) {
            (Listener::Unknown { .. }, Some(n)) => n.to_string(),
            _ => self.device_name(),
        }
    }

    /// The best human name we have for this member.
    pub fn device_name(&self) -> String {
        match self {
            Listener::Connected(d) => d.name.clone(),
            Listener::Disconnected { bluetooth, .. } => bluetooth.name.clone(),
            Listener::Unknown { uid } => match address_from_uid(uid) {
                Some(addr) => format!("Bluetooth device {addr}"),
                None => uid.clone(),
            },
        }
    }
}

/// Match each member UID of `multi` to an output device (by UID) or a paired
/// Bluetooth device (by address).
pub fn listeners(
    multi: &OutputDevice,
    devices: &[OutputDevice],
    paired: &[BluetoothDevice],
) -> Vec<Listener> {
    multi
        .subdevice_uids
        .iter()
        .map(|uid| {
            if let Some(d) = devices.iter().find(|d| &d.uid == uid) {
                return Listener::Connected(d.clone());
            }
            let bt = address_from_uid(uid).and_then(|addr| {
                paired
                    .iter()
                    .find(|b| normalize_address(&b.address) == addr)
            });
            match bt {
                Some(b) => Listener::Disconnected {
                    uid: uid.clone(),
                    bluetooth: b.clone(),
                },
                None => Listener::Unknown { uid: uid.clone() },
            }
        })
        .collect()
}

/// Output devices not in `exclude` (the featured group and its listeners),
/// sorted default-first then by name.
pub fn other_outputs<'a>(devices: &'a [OutputDevice], exclude: &[String]) -> Vec<&'a OutputDevice> {
    let mut v: Vec<&OutputDevice> = devices
        .iter()
        .filter(|d| !exclude.contains(&d.uid))
        .collect();
    v.sort_by(|a, b| {
        b.is_default
            .cmp(&a.is_default)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    v
}

// ---------------------------------------------------------------------------
// "Listen together": the chosen headphones and the single "Duet" output
// ---------------------------------------------------------------------------

/// The one Multi-Output device Duet keeps (shown under this name in the system
/// Sound menu). Its members change with the user's choice; it's never duplicated.
pub const DUET_GROUP_NAME: &str = "Duet";

/// What the main card shows.
#[derive(Debug, Clone, PartialEq)]
pub enum Together {
    /// The Duet output exists and holds exactly the connected chosen headphones
    /// (playing when it's the default output).
    Group {
        group: OutputDevice,
        members: Vec<Listener>,
    },
    /// Chosen headphones without a matching Duet output yet. "Listen together"
    /// (re)builds it once at least two of them are connected.
    Proposed { members: Vec<Listener> },
    /// Nothing chosen and fewer than two headphones connected.
    Empty,
}

impl Together {
    pub fn listeners(&self) -> Vec<Listener> {
        match self {
            Together::Group { members, .. } | Together::Proposed { members } => members.clone(),
            Together::Empty => Vec::new(),
        }
    }

    pub fn group(&self) -> Option<&OutputDevice> {
        match self {
            Together::Group { group, .. } => Some(group),
            _ => None,
        }
    }

    pub fn is_active(&self) -> bool {
        self.group().is_some_and(|g| g.is_default)
    }

    /// Connected listeners' output UIDs (the members a Duet output should have).
    pub fn live_uids(&self) -> Vec<String> {
        self.listeners()
            .iter()
            .filter_map(|l| match l {
                Listener::Connected(d) => Some(d.uid.clone()),
                _ => None,
            })
            .collect()
    }

    /// At least two chosen headphones are connected.
    pub fn is_ready(&self) -> bool {
        self.live_uids().len() >= 2
    }

    /// Bluetooth addresses of chosen headphones that are paired but not connected.
    pub fn to_connect(&self) -> Vec<String> {
        self.listeners()
            .iter()
            .filter_map(|l| match l {
                Listener::Disconnected { bluetooth, .. } => {
                    Some(normalize_address(&bluetooth.address))
                }
                _ => None,
            })
            .collect()
    }

    /// Enough headphones are chosen to start (connecting the missing ones first).
    pub fn can_start(&self) -> bool {
        self.live_uids().len() + self.to_connect().len() >= 2
    }

    /// UIDs shown in the main card, so "Other outputs" can leave them out.
    pub fn shown_uids(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .listeners()
            .iter()
            .map(|l| l.uid().to_string())
            .collect();
        if let Some(g) = self.group() {
            v.push(g.uid.clone());
        }
        v
    }
}

/// Connected Bluetooth outputs (any brand of headphones/speakers), by name.
pub fn connected_headphones(devices: &[OutputDevice]) -> Vec<&OutputDevice> {
    let mut v: Vec<&OutputDevice> = devices
        .iter()
        .filter(|d| d.transport == Transport::Bluetooth)
        .collect();
    v.sort_by_key(|d| d.name.to_lowercase());
    v
}

/// The Duet-made Multi-Output device, if any (the newest one if several linger).
pub fn duet_group(devices: &[OutputDevice]) -> Option<&OutputDevice> {
    devices
        .iter()
        .filter(|d| d.is_multi_output() && crate::audio::is_duet_group_uid(&d.uid))
        .max_by_key(|d| d.id)
}

/// The listeners Duet is about: the user's picks (Bluetooth addresses, in the
/// order chosen) or, before they've picked any, every connected Bluetooth output.
pub fn chosen_listeners(
    devices: &[OutputDevice],
    paired: &[BluetoothDevice],
    picks: &[String],
) -> Vec<Listener> {
    if picks.is_empty() {
        return connected_headphones(devices)
            .into_iter()
            .cloned()
            .map(Listener::Connected)
            .collect();
    }
    picks
        .iter()
        .map(|addr| {
            let addr = normalize_address(addr);
            if let Some(d) = devices
                .iter()
                .find(|d| crate::audio::bluetooth_address_of(d).as_deref() == Some(addr.as_str()))
            {
                return Listener::Connected(d.clone());
            }
            match paired
                .iter()
                .find(|b| normalize_address(&b.address) == addr)
            {
                Some(b) => Listener::Disconnected {
                    uid: addr,
                    bluetooth: b.clone(),
                },
                None => Listener::Unknown { uid: addr },
            }
        })
        .collect()
}

/// Decide what the main card shows for the current devices and picks.
pub fn plan_together(
    devices: &[OutputDevice],
    paired: &[BluetoothDevice],
    picks: &[String],
) -> Together {
    let members = chosen_listeners(devices, paired, picks);
    if members.is_empty() || (picks.is_empty() && members.len() < 2) {
        return Together::Empty;
    }
    let live: std::collections::BTreeSet<&str> = members
        .iter()
        .filter_map(|l| match l {
            Listener::Connected(d) => Some(d.uid.as_str()),
            _ => None,
        })
        .collect();
    match duet_group(devices) {
        Some(g)
            if live.len() >= 2
                && g.subdevice_uids
                    .iter()
                    .map(String::as_str)
                    .collect::<std::collections::BTreeSet<_>>()
                    == live =>
        {
            Together::Group {
                group: g.clone(),
                members,
            }
        }
        _ => Together::Proposed { members },
    }
}

/// What to do on each audio poll while the user wants to listen together.
#[derive(Debug, Clone, PartialEq)]
pub enum Keep {
    /// Everything's as it should be (or nothing can be done yet).
    Fine,
    /// The Duet output must be (re)built for these member UIDs and selected.
    Rebuild(Vec<String>),
    /// The Duet output is right but isn't selected: select it again.
    Reselect(crate::audio::DeviceId),
}

/// Decide how to keep "listen together" in force. macOS routes audio to a pair of
/// AirPods the moment it (re)connects, and falls back to the speakers while one drops
/// out, either of which would silently leave a listener without sound. Duet re-asserts
/// itself when the output moved to the Duet group or one of the listeners, or when the
/// chosen headphones have just all come back (`became_ready`); an output the user picked
/// elsewhere (e.g. speakers) is otherwise left alone.
pub fn keep_together(devices: &[OutputDevice], plan: &Together, became_ready: bool) -> Keep {
    let Some(default) = devices.iter().find(|d| d.is_default) else {
        return Keep::Fine;
    };
    let is_duet = default.is_multi_output() && crate::audio::is_duet_group_uid(&default.uid);
    let is_listener = plan.listeners().iter().any(|l| l.uid() == default.uid);
    let ours = is_duet || is_listener || became_ready;
    match plan {
        Together::Group { group, .. } if group.is_default => Keep::Fine,
        Together::Group { group, .. } if ours => Keep::Reselect(group.id),
        Together::Proposed { .. } if ours && plan.is_ready() => Keep::Rebuild(plan.live_uids()),
        _ => Keep::Fine,
    }
}

/// Stops Duet and macOS fighting over the output forever: allow a few re-asserts per
/// window, then give up (and tell the user) rather than flip-flopping.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct FlipGuard {
    recent: Vec<Instant>,
}

impl FlipGuard {
    pub const WINDOW: Duration = Duration::from_secs(30);
    /// One automatic retry; if macOS takes the output again, ask the user instead.
    pub const MAX: usize = 1;

    /// Record a re-assert at `now`; false means "too many, stop".
    pub fn allow(&mut self, now: Instant) -> bool {
        self.recent
            .retain(|t| now.duration_since(*t) < Self::WINDOW);
        self.recent.push(now);
        self.recent.len() <= Self::MAX
    }

    pub fn reset(&mut self) {
        self.recent.clear();
    }
}

/// Bluetooth audio devices offered in "Choose headphones": every paired audio
/// device plus connected Bluetooth outputs the pairing list doesn't cover, by name.
/// Returns (address, name, connected).
pub fn headphone_choices(
    devices: &[OutputDevice],
    paired: &[BluetoothDevice],
) -> Vec<(String, String, bool)> {
    let mut v: Vec<(String, String, bool)> = paired
        .iter()
        .filter(|b| b.is_audio)
        .map(|b| (normalize_address(&b.address), b.name.clone(), b.connected))
        .collect();
    for d in connected_headphones(devices) {
        if let Some(a) = crate::audio::bluetooth_address_of(d)
            && !v.iter().any(|(x, _, _)| *x == a)
        {
            v.push((a, d.name.clone(), true));
        }
    }
    v.sort_by(|a, b| {
        b.2.cmp(&a.2)
            .then_with(|| a.1.to_lowercase().cmp(&b.1.to_lowercase()))
    });
    v
}

/// Toggle `address` in the picks. When the user hasn't picked yet, the current
/// (automatic) listeners become the starting selection.
pub fn toggle_pick(current_listeners: &[Listener], picks: &[String], address: &str) -> Vec<String> {
    let address = normalize_address(address);
    let mut v: Vec<String> = if picks.is_empty() {
        current_listeners
            .iter()
            .filter_map(|l| address_from_uid(l.uid()))
            .collect()
    } else {
        picks.iter().map(|p| normalize_address(p)).collect()
    };
    if let Some(i) = v.iter().position(|a| *a == address) {
        v.remove(i);
    } else {
        v.push(address);
    }
    v
}

// ---------------------------------------------------------------------------
// Drag suppression
// ---------------------------------------------------------------------------

/// Tracks local slider interaction so polling doesn't yank a slider the user is
/// moving. Keys are device UIDs; time is injected for testability.
#[derive(Debug, Clone, Default)]
pub struct InteractionGuard {
    entries: HashMap<String, Interaction>,
}

#[derive(Debug, Clone, Copy)]
struct Interaction {
    last_input: Instant,
    pressed: bool,
}

impl InteractionGuard {
    /// Ignore remote values for this long after the last local input.
    pub const QUIET: Duration = Duration::from_millis(1000);
    /// A press with no input for this long is considered stale (lost pointerup).
    pub const STALE_PRESS: Duration = Duration::from_secs(8);

    pub fn press(&mut self, uid: &str, now: Instant) {
        self.entries.insert(
            uid.to_string(),
            Interaction {
                last_input: now,
                pressed: true,
            },
        );
    }

    pub fn input(&mut self, uid: &str, now: Instant) {
        let e = self.entries.entry(uid.to_string()).or_insert(Interaction {
            last_input: now,
            pressed: false,
        });
        e.last_input = now;
    }

    pub fn release(&mut self, uid: &str, now: Instant) {
        if let Some(e) = self.entries.get_mut(uid) {
            e.pressed = false;
            e.last_input = now;
        }
    }

    /// True while remote volume updates for `uid` should be ignored.
    pub fn is_suppressed(&self, uid: &str, now: Instant) -> bool {
        self.entries.get(uid).is_some_and(|e| {
            let since = now.saturating_duration_since(e.last_input);
            (e.pressed && since < Self::STALE_PRESS) || since < Self::QUIET
        })
    }

    /// Drop expired entries.
    pub fn prune(&mut self, now: Instant) {
        self.entries.retain(|_, e| {
            let since = now.saturating_duration_since(e.last_input);
            (e.pressed && since < Self::STALE_PRESS) || since < Self::QUIET
        });
    }
}

/// Merge freshly polled device volumes into the displayed volumes, keeping the
/// local value for any device the user is interacting with.
pub fn merge_volumes(
    displayed: &HashMap<String, f32>,
    devices: &[OutputDevice],
    guard: &InteractionGuard,
    now: Instant,
) -> HashMap<String, f32> {
    devices
        .iter()
        .filter_map(|d| {
            let remote = d.volume?;
            let v = match displayed.get(&d.uid) {
                Some(&local) if guard.is_suppressed(&d.uid, now) => local,
                _ => remote,
            };
            Some((d.uid.clone(), v))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Write coalescing
// ---------------------------------------------------------------------------

/// Latest-value-wins mailbox for volume writes: at most one writer runs per
/// key; inputs arriving while it runs replace each other, and the writer picks
/// up the newest when it finishes.
#[derive(Debug, Clone, Default)]
pub struct Coalescer<T> {
    pending: HashMap<String, T>,
    busy: std::collections::HashSet<String>,
}

impl<T> Coalescer<T> {
    /// Queue `value`. Returns true if the caller should start a writer for `key`.
    pub fn push(&mut self, key: &str, value: T) -> bool {
        self.pending.insert(key.to_string(), value);
        self.busy.insert(key.to_string())
    }

    /// Called by the writer: next value to write, or None (writer exits, key idle).
    pub fn next(&mut self, key: &str) -> Option<T> {
        let v = self.pending.remove(key);
        if v.is_none() {
            self.busy.remove(key);
        }
        v
    }
}

/// An existing Multi-Output whose members are exactly `uids` (order-insensitive).
pub fn find_group_with_members<'a>(
    devices: &'a [OutputDevice],
    uids: &[String],
) -> Option<&'a OutputDevice> {
    let want: std::collections::BTreeSet<&str> = uids.iter().map(String::as_str).collect();
    devices.iter().filter(|d| d.is_multi_output()).find(|d| {
        d.subdevice_uids
            .iter()
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>()
            == want
    })
}

/// Does a Bluetooth backend error mean "the user hasn't granted Bluetooth access"?
pub fn is_bluetooth_permission_error(message: &str) -> bool {
    let m = message.to_lowercase();
    [
        "permission",
        "not authorized",
        "unauthorized",
        "privacy",
        "denied",
        "not allowed",
        "tcc",
    ]
    .iter()
    .any(|k| m.contains(k))
}

/// Nickname for a device: the saved one, else derived from its name.
pub fn display_name(config: &crate::config::Config, uid: &str, device_name: &str) -> String {
    match config.nickname(uid) {
        Some(n) => n.to_string(),
        None => {
            let d = default_nickname(device_name);
            if d.is_empty() {
                device_name.to_string()
            } else {
                d
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tray popover
// ---------------------------------------------------------------------------

/// A rectangle in physical screen pixels (top-left origin).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScreenRect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// Where the popover goes relative to the tray icon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Anchor {
    /// Menu bar at the top (macOS, top panels on Linux).
    Below,
    /// Taskbar at the bottom (Windows default).
    Above,
}

/// Top-left position for a `win_w` x `win_h` popover anchored to the tray `icon`,
/// centred horizontally on it and kept inside `screen` (with `margin`). Flips to
/// the other side if it would not fit.
pub fn popover_position(
    icon: ScreenRect,
    win_w: f64,
    win_h: f64,
    screen: ScreenRect,
    anchor: Anchor,
    margin: f64,
) -> (f64, f64) {
    let min_x = screen.x + margin;
    let max_x = (screen.x + screen.w - win_w - margin).max(min_x);
    let x = (icon.x + icon.w / 2.0 - win_w / 2.0).clamp(min_x, max_x);

    let below = icon.y + icon.h + margin;
    let above = icon.y - win_h - margin;
    let fits_below = below + win_h <= screen.y + screen.h;
    let fits_above = above >= screen.y;
    let y = match anchor {
        Anchor::Below if fits_below || !fits_above => below,
        Anchor::Below => above,
        Anchor::Above if fits_above || !fits_below => above,
        Anchor::Above => below,
    };
    let min_y = screen.y;
    let max_y = (screen.y + screen.h - win_h).max(min_y);
    (x, y.clamp(min_y, max_y))
}

// ---------------------------------------------------------------------------
// Headphone extras (noise control, battery)
// ---------------------------------------------------------------------------

/// Short battery parts for display, e.g. [("L", 80), ("R", 75), ("Case", 40)].
pub fn battery_parts(b: &crate::audio::Battery) -> Vec<(&'static str, u8)> {
    if let Some(v) = b.single {
        return vec![("", v)];
    }
    [("L", b.left), ("R", b.right), ("Case", b.case)]
        .into_iter()
        .filter_map(|(k, v)| v.map(|v| (k, v)))
        .collect()
}

/// Where to send the user for noise control when Duet can't offer the buttons.
///
/// Only AirPods/Beats are mentioned (they're the ones that answer to the OS),
/// and only when the device reports battery but no switchable modes — i.e. the
/// platform reads the pair but can't change it.
pub fn noise_hint(
    device_name: &str,
    info: &crate::audio::HeadphoneInfo,
    hint: Option<&'static str>,
) -> Option<&'static str> {
    let hint = hint?;
    let apple = crate::audio::aap::looks_apple(device_name, None);
    (apple && info.supported_modes.is_empty()).then_some(hint)
}

/// A mode the user just picked wins over polled state until the device reports
/// it (or a few seconds pass), so the switcher doesn't flicker back.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct PendingModes {
    picks: HashMap<String, (crate::audio::ListeningMode, Instant)>,
}

impl PendingModes {
    pub const HOLD: Duration = Duration::from_secs(4);

    pub fn pick(&mut self, address: &str, mode: crate::audio::ListeningMode, now: Instant) {
        self.picks.insert(address.to_string(), (mode, now));
    }

    /// The mode to show, given what the device last reported.
    pub fn shown(
        &self,
        address: &str,
        reported: Option<crate::audio::ListeningMode>,
        now: Instant,
    ) -> Option<crate::audio::ListeningMode> {
        match self.picks.get(address) {
            Some((m, at)) if now.duration_since(*at) < Self::HOLD && reported != Some(*m) => {
                Some(*m)
            }
            _ => reported,
        }
    }

    pub fn prune(&mut self, now: Instant) {
        self.picks
            .retain(|_, (_, at)| now.duration_since(*at) < Self::HOLD);
    }
}

// ---------------------------------------------------------------------------
// Tray menu (kept minimal: open the app, or quit)
// ---------------------------------------------------------------------------

pub const TRAY_OPEN: &str = "duet.open";
pub const TRAY_QUIT: &str = "duet.quit";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrayAction {
    Open,
    Quit,
}

pub fn parse_tray_id(id: &str) -> Option<TrayAction> {
    match id {
        TRAY_OPEN => Some(TrayAction::Open),
        TRAY_QUIT => Some(TrayAction::Quit),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::mock::{MockAudio, MockBluetooth};
    use crate::audio::{AudioBackend, BluetoothBackend};

    #[test]
    fn nickname_derivation() {
        assert_eq!(default_nickname("Alex's AirPod Pro 2's"), "Alex");
        assert_eq!(default_nickname("Sam’s AirPods Pro #2 - Find My"), "Sam");
        assert_eq!(default_nickname("Alex’s AirPods Pro"), "Alex");
        assert_eq!(
            default_nickname("MacBook Air Speakers"),
            "MacBook Air Speakers"
        );
        assert_eq!(
            default_nickname("Studio Display - Find My"),
            "Studio Display"
        );
        assert_eq!(default_nickname("Chris's"), "Chris");
        assert_eq!(default_nickname("'s thing"), "'s thing");
        assert_eq!(default_nickname("Bose Sounds Good"), "Bose Sounds Good");
        assert_eq!(default_nickname("  "), "");
    }

    #[test]
    fn percent_conversion() {
        assert_eq!(to_percent(0.52), 52);
        assert_eq!(to_percent(0.555), 56);
        assert_eq!(to_percent(-1.0), 0);
        assert_eq!(to_percent(1.7), 100);
        assert_eq!(to_percent(f32::NAN), 0);
        assert_eq!(from_percent(50.0), 0.5);
        assert_eq!(from_percent(150.0), 1.0);
        assert_eq!(from_percent(-3.0), 0.0);
        assert_eq!(from_percent(f64::NAN), 0.0);
        assert_eq!(parse_slider(" 75 "), Some(0.75));
        assert_eq!(parse_slider("x"), None);
        for p in 0..=100u8 {
            assert_eq!(to_percent(from_percent(p as f64)), p);
        }
    }

    fn sample() -> (Vec<OutputDevice>, Vec<BluetoothDevice>) {
        (
            MockAudio::sample().output_devices().unwrap(),
            MockBluetooth::sample().paired_devices().unwrap(),
        )
    }

    #[test]
    fn listener_matching_connected() {
        let (devices, bt) = sample();
        let multi = primary_multi_output(&devices).unwrap();
        assert_eq!(multi.name, "Living Room Group");
        let ls = listeners(multi, &devices, &bt);
        assert_eq!(ls.len(), 2);
        assert!(matches!(&ls[0], Listener::Connected(d) if d.id == 10));
        assert!(matches!(&ls[1], Listener::Connected(d) if d.id == 11));
    }

    #[test]
    fn listener_matching_disconnected_and_unknown() {
        let (mut devices, bt) = sample();
        devices.retain(|d| d.id != 11);
        let multi = devices.iter().find(|d| d.id == 40).unwrap().clone();
        let mut multi2 = multi.clone();
        multi2
            .subdevice_uids
            .push("AA-AA-AA-AA-AA-AA:output".into());
        multi2.subdevice_uids.push("SomeOtherUID".into());
        let ls = listeners(&multi2, &devices, &bt);
        assert!(matches!(&ls[0], Listener::Connected(d) if d.id == 10));
        match &ls[1] {
            Listener::Disconnected { uid, bluetooth } => {
                assert_eq!(uid, "02-1B-2C-3D-4E-5F:output");
                assert_eq!(bluetooth.name, "Sam’s AirPods Pro #2 - Find My");
            }
            other => panic!("expected disconnected, got {other:?}"),
        }
        assert_eq!(
            ls[2],
            Listener::Unknown {
                uid: "AA-AA-AA-AA-AA-AA:output".into()
            }
        );
        assert_eq!(ls[2].device_name(), "Bluetooth device AA-AA-AA-AA-AA-AA");
        let mut cfg = crate::config::Config::default();
        cfg.remember_names([(ls[2].uid(), "Sam’s AirPods Pro #2 - Find My")]);
        assert_eq!(
            ls[2].device_name_with(&cfg),
            "Sam’s AirPods Pro #2 - Find My"
        );
        assert_eq!(
            display_name(&cfg, ls[2].uid(), &ls[2].device_name_with(&cfg)),
            "Sam"
        );
        assert_eq!(
            ls[3],
            Listener::Unknown {
                uid: "SomeOtherUID".into()
            }
        );
    }

    #[test]
    fn bluetooth_address_case_insensitive() {
        let (mut devices, mut bt) = sample();
        devices.retain(|d| d.id != 11);
        bt[1].address = "02:1b:2c:3d:4e:5f".into();
        let multi = devices.iter().find(|d| d.id == 40).unwrap();
        assert!(matches!(
            listeners(multi, &devices, &bt)[1],
            Listener::Disconnected { .. }
        ));
    }

    #[test]
    fn primary_prefers_default() {
        let (mut devices, _) = sample();
        let mut other = devices[4].clone();
        other.id = 41;
        other.uid = "~:AMS2_StackedOutput:1".into();
        other.is_default = false;
        devices.insert(0, other);
        assert_eq!(primary_multi_output(&devices).unwrap().id, 40);
        for d in &mut devices {
            d.is_default = false;
        }
        assert_eq!(primary_multi_output(&devices).unwrap().id, 41);
        assert!(primary_multi_output(&devices[1..4]).is_none());
    }

    fn bt(id: u32, mac: &str, name: &str) -> OutputDevice {
        OutputDevice {
            id,
            uid: format!("{mac}:output"),
            name: name.into(),
            transport: Transport::Bluetooth,
            volume: Some(0.5),
            is_default: false,
            subdevice_uids: vec![],
        }
    }

    fn duet_group_dev(id: u32, members: &[&str], default: bool) -> OutputDevice {
        OutputDevice {
            id,
            uid: format!("local.duet.multi.{id}"),
            name: DUET_GROUP_NAME.into(),
            transport: Transport::Aggregate,
            volume: None,
            is_default: default,
            subdevice_uids: members.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn plan_auto_uses_connected_headphones() {
        let (devices, paired) = sample();
        // "Living Room Group" is the user's own group: Duet doesn't adopt it.
        let plan = plan_together(&devices, &paired, &[]);
        let Together::Proposed { members } = &plan else {
            panic!("{plan:?}")
        };
        assert_eq!(members.len(), 2);
        assert!(plan.is_ready() && !plan.is_active());
    }

    #[test]
    fn plan_matches_the_duet_group() {
        let (mut devices, paired) = sample();
        for d in &mut devices {
            d.is_default = false;
        }
        devices.push(duet_group_dev(
            70,
            &["02-1B-2C-3D-4E-5F:output", "02-2A-3B-4C-5D-6E:output"],
            true,
        ));
        let plan = plan_together(&devices, &paired, &[]);
        assert_eq!(plan.group().unwrap().id, 70);
        assert!(plan.is_active());
        // Different headphones chosen -> needs rebuilding.
        let plan = plan_together(
            &devices,
            &paired,
            &["02-2A-3B-4C-5D-6E".into(), "02-3C-4D-5E-6F-70".into()],
        );
        assert!(matches!(plan, Together::Proposed { .. }));
        assert!(!plan.is_ready(), "the second pair isn't connected");
        assert!(matches!(plan.listeners()[1], Listener::Disconnected { .. }));
    }

    #[test]
    fn plan_with_any_two_headphones_and_empty() {
        let (mut devices, paired) = sample();
        devices.retain(|d| d.transport != Transport::Bluetooth);
        assert_eq!(plan_together(&devices, &paired, &[]), Together::Empty);
        devices.push(bt(50, "AA-AA-AA-AA-AA-01", "Jane's Sony WH-1000XM5"));
        assert_eq!(
            plan_together(&devices, &paired, &[]),
            Together::Empty,
            "one isn't enough"
        );
        devices.push(bt(51, "AA-AA-AA-AA-AA-02", "Bose QC Ultra"));
        let plan = plan_together(&devices, &paired, &[]);
        let names: Vec<_> = plan.listeners().iter().map(|l| l.device_name()).collect();
        assert_eq!(names, ["Bose QC Ultra", "Jane's Sony WH-1000XM5"]);
        assert!(plan.is_ready());
    }

    #[test]
    fn keeping_together() {
        let (mut devices, paired) = sample();
        for d in &mut devices {
            d.is_default = false;
        }
        let both = ["02-1B-2C-3D-4E-5F:output", "02-2A-3B-4C-5D-6E:output"];
        devices.push(duet_group_dev(70, &both, true));
        let plan = plan_together(&devices, &paired, &[]);
        assert_eq!(keep_together(&devices, &plan, false), Keep::Fine);
        // macOS switched to Alex's AirPods when they reconnected.
        devices.iter_mut().for_each(|d| d.is_default = d.id == 10);
        let plan = plan_together(&devices, &paired, &[]);
        assert_eq!(keep_together(&devices, &plan, false), Keep::Reselect(70));
        // Speakers: left alone (user's choice, or a fallback while a pair dropped)...
        devices.iter_mut().for_each(|d| d.is_default = d.id == 20);
        let plan = plan_together(&devices, &paired, &[]);
        assert_eq!(keep_together(&devices, &plan, false), Keep::Fine);
        // ...unless the chosen headphones just all came back.
        assert_eq!(keep_together(&devices, &plan, true), Keep::Reselect(70));
        // A different pair joined while Duet was playing: rebuild.
        devices.iter_mut().for_each(|d| d.is_default = d.id == 70);
        devices.push(bt(52, "02-3C-4D-5E-6F-70", "Alex’s AirPods Pro"));
        let picks = [
            "02-2A-3B-4C-5D-6E".to_string(),
            "02-3C-4D-5E-6F-70".to_string(),
        ];
        let plan = plan_together(&devices, &paired, &picks);
        assert_eq!(
            keep_together(&devices, &plan, false),
            Keep::Rebuild(vec![
                "02-2A-3B-4C-5D-6E:output".into(),
                "02-3C-4D-5E-6F-70:output".into()
            ])
        );
        // One chosen pair not connected yet: can start (Duet connects it), not ready.
        devices.retain(|d| d.id != 52);
        let plan = plan_together(&devices, &paired, &picks);
        assert!(plan.can_start() && !plan.is_ready());
        assert_eq!(plan.to_connect(), ["02-3C-4D-5E-6F-70"]);
    }

    #[test]
    fn flip_guard_stops_fights() {
        let t0 = Instant::now();
        let mut g = FlipGuard::default();
        for i in 0..FlipGuard::MAX {
            assert!(g.allow(t0 + Duration::from_secs(i as u64)));
        }
        assert!(!g.allow(t0 + Duration::from_secs(5)), "5th within 30s");
        assert!(g.allow(t0 + Duration::from_secs(60)), "old ones expire");
        g.reset();
        assert_eq!(g, FlipGuard::default());
    }

    #[test]
    fn picks_and_choices() {
        let (devices, paired) = sample();
        let auto = chosen_listeners(&devices, &paired, &[]);
        // With only one pair connected, ticking a second keeps the first.
        let mut one = devices.clone();
        one.retain(|d| d.id != 11);
        let auto_one = chosen_listeners(&one, &paired, &[]);
        assert_eq!(
            toggle_pick(&auto_one, &[], "02-1B-2C-3D-4E-5F"),
            ["02-2A-3B-4C-5D-6E", "02-1B-2C-3D-4E-5F"]
        );
        // First toggle starts from the automatic listeners.
        let picks = toggle_pick(&auto, &[], "02:3c:4d:5e:6f:70");
        assert_eq!(
            picks,
            [
                "02-2A-3B-4C-5D-6E",
                "02-1B-2C-3D-4E-5F",
                "02-3C-4D-5E-6F-70"
            ]
        );
        let picks = toggle_pick(&auto, &picks, "02-1B-2C-3D-4E-5F");
        assert_eq!(picks, ["02-2A-3B-4C-5D-6E", "02-3C-4D-5E-6F-70"]);
        let choices: Vec<_> = headphone_choices(&devices, &paired)
            .into_iter()
            .map(|(_, n, c)| (n, c))
            .collect();
        assert_eq!(
            choices,
            [
                ("Alex's AirPod Pro 2's".to_string(), true),
                ("Alex’s AirPods Pro".to_string(), false),
                ("Sam’s AirPods Pro #2 - Find My".to_string(), false),
            ]
        );
        assert!(crate::audio::is_duet_group_uid("local.duet.multi.3"));
        assert!(!crate::audio::is_duet_group_uid("~:AMS2_StackedOutput:0"));
        assert_eq!(parse_tray_id(TRAY_OPEN), Some(TrayAction::Open));
        assert_eq!(parse_tray_id("duet.together"), None);
    }

    #[test]
    fn battery_and_pending_modes() {
        use crate::audio::{Battery, ListeningMode::*};
        let b = Battery {
            left: Some(80),
            right: Some(75),
            case: None,
            single: None,
        };
        assert_eq!(battery_parts(&b), [("L", 80), ("R", 75)]);
        let s = Battery {
            single: Some(64),
            ..Battery::default()
        };
        assert_eq!(battery_parts(&s), [("", 64)]);
        assert!(battery_parts(&Battery::default()).is_empty());

        let t0 = Instant::now();
        let mut p = PendingModes::default();
        assert_eq!(p.shown("A", Some(Off), t0), Some(Off));
        p.pick("A", Transparency, t0);
        assert_eq!(
            p.shown("A", Some(Off), t0 + Duration::from_secs(1)),
            Some(Transparency)
        );
        assert_eq!(p.shown("A", Some(Transparency), t0), Some(Transparency));
        assert_eq!(
            p.shown("A", Some(Off), t0 + Duration::from_secs(5)),
            Some(Off)
        );
        assert_eq!(p.shown("B", None, t0), None);
        p.prune(t0 + Duration::from_secs(5));
        assert_eq!(p, PendingModes::default());
    }

    #[test]
    fn other_outputs_excludes_listeners() {
        let (devices, _) = sample();
        // The listeners are in the main card; the user's own "Living Room Group" (in use) is an
        // ordinary output here, listed first.
        let shown = plan_together(&devices, &[], &[]).shown_uids();
        let names: Vec<_> = other_outputs(&devices, &shown)
            .iter()
            .map(|d| d.name.as_str())
            .collect();
        assert_eq!(
            names,
            [
                "Living Room Group",
                "Loopback Mixer",
                "MacBook Air Speakers"
            ]
        );
        assert_eq!(other_outputs(&devices, &[]).len(), 5);
    }

    #[test]
    fn drag_suppression() {
        let t0 = Instant::now();
        let ms = |n| t0 + Duration::from_millis(n);
        let mut g = InteractionGuard::default();
        assert!(!g.is_suppressed("a", t0));
        g.press("a", t0);
        g.input("a", ms(100));
        assert!(
            g.is_suppressed("a", ms(3000)),
            "held without input stays suppressed"
        );
        assert!(!g.is_suppressed("b", ms(3000)));
        g.release("a", ms(3100));
        assert!(g.is_suppressed("a", ms(3900)));
        assert!(!g.is_suppressed("a", ms(4200)));
        // Keyboard / click input without press: only the quiet window applies.
        g.input("b", ms(5000));
        assert!(g.is_suppressed("b", ms(5500)));
        assert!(!g.is_suppressed("b", ms(6100)));
        // A lost pointerup eventually expires.
        g.press("c", t0);
        assert!(!g.is_suppressed("c", t0 + InteractionGuard::STALE_PRESS));
        g.prune(ms(20_000));
        assert!(g.entries.is_empty());
    }

    #[test]
    fn merge_keeps_local_while_dragging() {
        let (devices, _) = sample();
        let t0 = Instant::now();
        let mut g = InteractionGuard::default();
        let displayed: HashMap<String, f32> = [
            ("02-2A-3B-4C-5D-6E:output".to_string(), 0.9),
            ("02-1B-2C-3D-4E-5F:output".to_string(), 0.1),
        ]
        .into();
        g.input("02-2A-3B-4C-5D-6E:output", t0);
        let merged = merge_volumes(&displayed, &devices, &g, t0 + Duration::from_millis(200));
        assert_eq!(merged["02-2A-3B-4C-5D-6E:output"], 0.9);
        assert_eq!(merged["02-1B-2C-3D-4E-5F:output"], 0.60);
        assert!(
            !merged.contains_key("~:AMS2_StackedOutput:0"),
            "no volume -> omitted"
        );
        let later = merge_volumes(&displayed, &devices, &g, t0 + Duration::from_secs(2));
        assert_eq!(later["02-2A-3B-4C-5D-6E:output"], 0.52);
    }

    #[test]
    fn coalescer_latest_wins() {
        let mut c = Coalescer::default();
        assert!(c.push("a", 1));
        assert!(!c.push("a", 2));
        assert!(!c.push("a", 3));
        assert!(c.push("b", 9));
        assert_eq!(c.next("a"), Some(3));
        assert!(!c.push("a", 4));
        assert_eq!(c.next("a"), Some(4));
        assert_eq!(c.next("a"), None);
        assert!(c.push("a", 5), "idle again -> new writer");
    }

    #[test]
    fn popover_below_menu_bar() {
        let screen = ScreenRect {
            x: 0.0,
            y: 0.0,
            w: 3024.0,
            h: 1964.0,
        };
        let icon = ScreenRect {
            x: 2400.0,
            y: 0.0,
            w: 44.0,
            h: 48.0,
        };
        let (x, y) = popover_position(icon, 840.0, 1280.0, screen, Anchor::Below, 8.0);
        assert_eq!((x, y), (2422.0 - 420.0, 56.0));
        // Near the right edge: clamped inside the screen.
        let icon = ScreenRect { x: 2990.0, ..icon };
        let (x, _) = popover_position(icon, 840.0, 1280.0, screen, Anchor::Below, 8.0);
        assert_eq!(x, 3024.0 - 840.0 - 8.0);
    }

    #[test]
    fn popover_above_taskbar_and_flip() {
        let screen = ScreenRect {
            x: 0.0,
            y: 0.0,
            w: 1920.0,
            h: 1080.0,
        };
        let icon = ScreenRect {
            x: 1700.0,
            y: 1040.0,
            w: 24.0,
            h: 40.0,
        };
        let (_, y) = popover_position(icon, 420.0, 640.0, screen, Anchor::Above, 8.0);
        assert_eq!(y, 1040.0 - 640.0 - 8.0);
        // Taskbar actually at the top: "Above" doesn't fit, so it flips below.
        let icon = ScreenRect { y: 0.0, ..icon };
        let (_, y) = popover_position(icon, 420.0, 640.0, screen, Anchor::Above, 8.0);
        assert_eq!(y, 48.0);
        // Second monitor to the left (negative x).
        let screen2 = ScreenRect {
            x: -1920.0,
            y: 0.0,
            w: 1920.0,
            h: 1080.0,
        };
        let icon = ScreenRect {
            x: -10.0,
            y: 0.0,
            w: 24.0,
            h: 24.0,
        };
        let (x, _) = popover_position(icon, 420.0, 640.0, screen2, Anchor::Below, 8.0);
        assert_eq!(x, -420.0 - 8.0);
    }

    #[test]
    fn display_name_prefers_saved() {
        let mut cfg = crate::config::Config::default();
        assert_eq!(
            display_name(&cfg, "u", "Sam’s AirPods Pro #2 - Find My"),
            "Sam"
        );
        cfg.set_nickname("u", "Jordan");
        assert_eq!(
            display_name(&cfg, "u", "Sam’s AirPods Pro #2 - Find My"),
            "Jordan"
        );
        assert_eq!(display_name(&cfg, "v", "   "), "   ");
    }

    #[test]
    fn noise_hint_only_for_apple_pairs_duet_cant_switch() {
        use crate::audio::{Battery, HeadphoneInfo, ListeningMode};
        let battery = Battery {
            left: Some(80),
            right: Some(75),
            case: Some(40),
            single: None,
        };
        let read_only = HeadphoneInfo {
            listening_mode: None,
            supported_modes: vec![],
            battery,
        };
        let switchable = HeadphoneInfo {
            listening_mode: Some(ListeningMode::Transparency),
            supported_modes: ListeningMode::ALL.to_vec(),
            battery,
        };
        let hint = Some("Control Center \u{203a} Sound");
        // AirPods that Duet can only read: point at the OS.
        assert_eq!(noise_hint("Alex's AirPod Pro 2's", &read_only, hint), hint);
        assert_eq!(
            noise_hint("Sam\u{2019}s Beats Studio", &read_only, hint),
            hint
        );
        // Nothing to point at when Duet can switch them itself, when the pair
        // isn't an Apple one, or when this platform has no such setting.
        assert_eq!(noise_hint("Alex's AirPod Pro 2's", &switchable, hint), None);
        assert_eq!(noise_hint("Sony WH-1000XM5", &read_only, hint), None);
        assert_eq!(noise_hint("Alex's AirPod Pro 2's", &read_only, None), None);
    }

    #[test]
    fn group_reuse_and_permission_errors() {
        let (devices, _) = sample();
        let want = vec![
            "02-1B-2C-3D-4E-5F:output".to_string(),
            "02-2A-3B-4C-5D-6E:output".to_string(),
        ];
        assert_eq!(
            find_group_with_members(&devices, &want).map(|d| d.id),
            Some(40)
        );
        assert!(find_group_with_members(&devices, &want[..1]).is_none());
        assert!(is_bluetooth_permission_error(
            "Bluetooth: access not authorized (TCC)"
        ));
        assert!(is_bluetooth_permission_error("Permission denied"));
        assert!(!is_bluetooth_permission_error("device 40 not found"));
    }
}
