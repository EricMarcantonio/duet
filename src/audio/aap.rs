//! Apple Accessory Protocol (AAP): the proprietary protocol AirPods / Beats speak to
//! their host over a Bluetooth Classic L2CAP channel on PSM `0x1001`. Used by the Linux
//! backend for noise control and battery; everything here is pure (no OS calls) so it is
//! unit-tested on every host.
//!
//! Sources (public reverse-engineering, no Apple documentation exists):
//! * LibrePods (formerly "AirPods Like Normal"), kavishdevar/librepods on GitHub —
//!   `AAP Definitions.md` and the Linux client's `airpods_packets.h`.
//! * The earlier "AirPods on Linux" / `tyalie/AAP-Protocol-Defintion` notes, which
//!   first documented the handshake, noise-control (`0x09`/`0x0D`) and battery
//!   (`0x04`) messages.
//!
//! Wire format: every message after the handshake starts with `04 00 04 00`, then a
//! little-endian 16-bit opcode, then the payload. Packets are framed by the
//! `SOCK_SEQPACKET` socket, so one `recv` = one message.

use super::{Battery, HeadphoneInfo, ListeningMode, battery_percent, supported_modes};

/// L2CAP PSM of the AAP channel.
pub const PSM: u16 = 0x1001;

/// Apple's Bluetooth SIG company id (`v004C` in a BlueZ modalias).
pub const APPLE_VENDOR_ID: u16 = 0x004C;

/// First packet the host must send; AirPods ignore everything until they get it.
pub const HANDSHAKE: [u8; 16] = [
    0x00, 0x00, 0x04, 0x00, 0x01, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

/// LibrePods' "set specific features" packet (opcode `0x4D`). Unlocks features newer
/// firmware gates behind it, notably Adaptive / conversational awareness.
pub const SET_SPECIFIC_FEATURES: [u8; 14] = [
    0x04, 0x00, 0x04, 0x00, 0x4D, 0x00, 0xFF, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

/// "Request notifications" (opcode `0x0F`): subscribe to every notification class
/// (battery, noise control, ear detection, ...). Some LibrePods versions send
/// `FF FF FE FF`; all-ones is the original documented form and is accepted too.
pub const REQUEST_NOTIFICATIONS: [u8; 10] =
    [0x04, 0x00, 0x04, 0x00, 0x0F, 0x00, 0xFF, 0xFF, 0xFF, 0xFF];

/// The packets to send right after connecting, in order.
pub const INIT_SEQUENCE: [&[u8]; 3] = [&HANDSHAKE, &SET_SPECIFIC_FEATURES, &REQUEST_NOTIFICATIONS];

const HEADER: [u8; 4] = [0x04, 0x00, 0x04, 0x00];
const OP_BATTERY: u16 = 0x0004;
const OP_CONTROL: u16 = 0x0009;
const OP_METADATA: u16 = 0x001D;
/// Control-command identifier for "listening mode" inside an `0x09` message.
const CONTROL_LISTENING_MODE: u8 = 0x0D;

/// Packet that switches noise control: `04 00 04 00 09 00 0D <mode> 00 00 00`.
pub fn listening_mode_packet(mode: ListeningMode) -> [u8; 11] {
    [
        0x04,
        0x00,
        0x04,
        0x00,
        0x09,
        0x00,
        CONTROL_LISTENING_MODE,
        mode.raw(),
        0x00,
        0x00,
        0x00,
    ]
}

/// Battery component ids in a `0x04` notification.
const COMPONENT_SINGLE: u8 = 0x01;
const COMPONENT_RIGHT: u8 = 0x02;
const COMPONENT_LEFT: u8 = 0x04;
const COMPONENT_CASE: u8 = 0x08;
/// Component status byte: 0x01 charging, 0x02 discharging, 0x04 disconnected.
const STATUS_DISCONNECTED: u8 = 0x04;

/// A decoded AAP message the backend cares about.
#[derive(Debug, Clone, PartialEq)]
pub enum Notification {
    /// Current noise-control mode (sent on connect and whenever it changes).
    ListeningMode(ListeningMode),
    /// Full battery snapshot (components not listed / disconnected are `None`).
    Battery(Battery),
    /// Device metadata (`0x1D`): printable strings it carries (name, model number, ...).
    Metadata(Vec<String>),
}

/// Decode one received packet. `None` for anything unrecognized, truncated or malformed.
pub fn parse_notification(pkt: &[u8]) -> Option<Notification> {
    let rest = pkt.strip_prefix(&HEADER)?;
    if rest.len() < 2 {
        return None;
    }
    let opcode = u16::from_le_bytes([rest[0], rest[1]]);
    let payload = &rest[2..];
    match opcode {
        OP_CONTROL => match payload {
            [CONTROL_LISTENING_MODE, mode, ..] => {
                ListeningMode::from_raw(*mode).map(Notification::ListeningMode)
            }
            _ => None,
        },
        OP_BATTERY => parse_battery(payload).map(Notification::Battery),
        OP_METADATA => Some(Notification::Metadata(printable_strings(payload))),
        _ => None,
    }
}

/// `<count> ( <component> 01 <level> <status> 01 ){count}`.
fn parse_battery(payload: &[u8]) -> Option<Battery> {
    let (&count, entries) = payload.split_first()?;
    let count = usize::from(count);
    if count == 0 || count > 4 || entries.len() < count * 5 {
        return None;
    }
    let mut b = Battery::default();
    for e in entries.as_chunks::<5>().0.iter().take(count) {
        let level = if e[3] == STATUS_DISCONNECTED {
            None
        } else {
            battery_percent(e[2])
        };
        match e[0] {
            COMPONENT_SINGLE => b.single = level,
            COMPONENT_RIGHT => b.right = level,
            COMPONENT_LEFT => b.left = level,
            COMPONENT_CASE => b.case = level,
            _ => {}
        }
    }
    Some(b)
}

/// NUL-separated printable ASCII runs of at least 2 chars.
fn printable_strings(payload: &[u8]) -> Vec<String> {
    payload
        .split(|b| !(0x20..0x7F).contains(b))
        .filter(|s| s.len() >= 2)
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect()
}

/// Latest state learned from a device's notifications.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AapState {
    pub listening_mode: Option<ListeningMode>,
    pub battery: Battery,
    /// Bluetooth PnP product id (from the modalias), if known; decides Adaptive support.
    pub product_id: Option<u16>,
    /// Strings from the metadata packet (model number such as "A2698" among them).
    pub metadata: Vec<String>,
}

impl AapState {
    pub fn apply(&mut self, n: Notification) {
        match n {
            Notification::ListeningMode(m) => self.listening_mode = Some(m),
            Notification::Battery(b) => self.battery = b,
            Notification::Metadata(s) => self.metadata = s,
        }
    }

    /// What `headphone_info` reports; `None` until the device has told us anything.
    pub fn info(&self) -> Option<HeadphoneInfo> {
        let modes = match self.listening_mode {
            // Devices without ANC (AirPods 2/3) never report a listening mode.
            None => Vec::new(),
            Some(m) => {
                let adaptive = m == ListeningMode::Adaptive
                    || self.product_id.is_some_and(super::supports_adaptive);
                supported_modes(true, true, adaptive)
            }
        };
        let info = HeadphoneInfo {
            listening_mode: self.listening_mode,
            supported_modes: modes,
            battery: self.battery,
        };
        (!info.is_empty()).then_some(info)
    }
}

/// Parse a BlueZ modalias such as `bluetooth:v004Cp2014d6105` (or `usb:v05ACp...`)
/// into `(vendor, product)`.
pub fn parse_modalias(s: &str) -> Option<(u16, u16)> {
    let (_, ids) = s.trim().split_once(':')?;
    let ids = ids.strip_prefix('v')?;
    let (vendor, rest) = ids.split_at_checked(4)?;
    let product = rest.strip_prefix('p')?.get(..4)?;
    Some((
        u16::from_str_radix(vendor, 16).ok()?,
        u16::from_str_radix(product, 16).ok()?,
    ))
}

/// Worth trying AAP on: Apple vendor id in the modalias, or an AirPods/Beats name.
pub fn looks_apple(name: &str, modalias: Option<&str>) -> bool {
    if modalias
        .and_then(parse_modalias)
        .is_some_and(|(v, _)| v == APPLE_VENDOR_ID)
    {
        return true;
    }
    let n = name.to_ascii_lowercase();
    n.contains("airpods") || n.contains("beats")
}

/// `l2_bdaddr` bytes for "AA-BB-CC-DD-EE-FF" / "aa:bb:...": BlueZ stores addresses
/// little-endian, i.e. reversed relative to the printed form.
pub fn bdaddr_bytes(addr: &str) -> Option<[u8; 6]> {
    let parts: Vec<&str> = addr.trim().split([':', '-']).collect();
    if parts.len() != 6 {
        return None;
    }
    let mut out = [0u8; 6];
    for (i, p) in parts.iter().enumerate() {
        if p.len() != 2 {
            return None;
        }
        out[5 - i] = u8::from_str_radix(p, 16).ok()?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ListeningMode::*;

    #[test]
    fn init_packets() {
        assert_eq!(HANDSHAKE, [0, 0, 4, 0, 1, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(&SET_SPECIFIC_FEATURES[..6], &[4, 0, 4, 0, 0x4D, 0]);
        assert_eq!(
            REQUEST_NOTIFICATIONS,
            [4, 0, 4, 0, 0x0F, 0, 0xFF, 0xFF, 0xFF, 0xFF]
        );
        assert_eq!(INIT_SEQUENCE[0], &HANDSHAKE[..]);
        assert_eq!(INIT_SEQUENCE.len(), 3);
        assert_eq!(PSM, 0x1001);
    }

    #[test]
    fn mode_packets() {
        let expect = |m, raw: u8| {
            assert_eq!(
                listening_mode_packet(m),
                [
                    0x04, 0x00, 0x04, 0x00, 0x09, 0x00, 0x0D, raw, 0x00, 0x00, 0x00
                ]
            );
        };
        expect(Off, 0x01);
        expect(NoiseCancellation, 0x02);
        expect(Transparency, 0x03);
        expect(Adaptive, 0x04);
        // Round trip: the status notification has the same shape as the command.
        for m in ListeningMode::ALL {
            assert_eq!(
                parse_notification(&listening_mode_packet(m)),
                Some(Notification::ListeningMode(m))
            );
        }
    }

    #[test]
    fn mode_notifications() {
        assert_eq!(
            parse_notification(&[
                0x04, 0x00, 0x04, 0x00, 0x09, 0x00, 0x0D, 0x02, 0x00, 0x00, 0x00
            ]),
            Some(Notification::ListeningMode(NoiseCancellation))
        );
        // Unknown mode value / other control id (0x28 = conversational awareness).
        assert_eq!(
            parse_notification(&[
                0x04, 0x00, 0x04, 0x00, 0x09, 0x00, 0x0D, 0x07, 0x00, 0x00, 0x00
            ]),
            None
        );
        assert_eq!(
            parse_notification(&[
                0x04, 0x00, 0x04, 0x00, 0x09, 0x00, 0x28, 0x01, 0x00, 0x00, 0x00
            ]),
            None
        );
        // Truncated right after the control id.
        assert_eq!(
            parse_notification(&[0x04, 0x00, 0x04, 0x00, 0x09, 0x00, 0x0D]),
            None
        );
    }

    #[test]
    fn battery_airpods_pro() {
        // Right 100% discharging, left 99% charging, case 17% discharging.
        let pkt = [
            0x04, 0x00, 0x04, 0x00, 0x04, 0x00, 0x03, //
            0x02, 0x01, 0x64, 0x02, 0x01, //
            0x04, 0x01, 0x63, 0x01, 0x01, //
            0x08, 0x01, 0x11, 0x02, 0x01,
        ];
        assert_eq!(
            parse_notification(&pkt),
            Some(Notification::Battery(Battery {
                left: Some(99),
                right: Some(100),
                case: Some(17),
                single: None,
            }))
        );
    }

    #[test]
    fn battery_case_disconnected() {
        // Both buds out of the case: case reported with status 0x04 (level stale).
        let pkt = [
            0x04, 0x00, 0x04, 0x00, 0x04, 0x00, 0x03, //
            0x02, 0x01, 0x50, 0x02, 0x01, //
            0x04, 0x01, 0x4B, 0x02, 0x01, //
            0x08, 0x01, 0x2A, 0x04, 0x01,
        ];
        let Some(Notification::Battery(b)) = parse_notification(&pkt) else {
            panic!("not battery")
        };
        assert_eq!((b.left, b.right, b.case), (Some(75), Some(80), None));
        // Only the left bud in use: two components.
        let pkt = [
            0x04, 0x00, 0x04, 0x00, 0x04, 0x00, 0x02, //
            0x04, 0x01, 0x3C, 0x02, 0x01, //
            0x08, 0x01, 0x00, 0x04, 0x01,
        ];
        assert_eq!(
            parse_notification(&pkt),
            Some(Notification::Battery(Battery {
                left: Some(60),
                ..Default::default()
            }))
        );
    }

    #[test]
    fn battery_single() {
        // AirPods Max / Beats over-ear: one component of type 0x01.
        let pkt = [
            0x04, 0x00, 0x04, 0x00, 0x04, 0x00, 0x01, 0x01, 0x01, 0x2D, 0x01, 0x01,
        ];
        assert_eq!(
            parse_notification(&pkt),
            Some(Notification::Battery(Battery {
                single: Some(45),
                ..Default::default()
            }))
        );
    }

    #[test]
    fn garbage_packets() {
        let bad: [&[u8]; 9] = [
            &[],
            &[0x04],
            &[0x04, 0x00, 0x04, 0x00],
            &[0x04, 0x00, 0x04, 0x00, 0x04],
            // Battery count says 3 but only one entry follows.
            &[
                0x04, 0x00, 0x04, 0x00, 0x04, 0x00, 0x03, 0x02, 0x01, 0x64, 0x02, 0x01,
            ],
            // Battery count 0.
            &[0x04, 0x00, 0x04, 0x00, 0x04, 0x00, 0x00],
            // Wrong header.
            &[
                0x05, 0x00, 0x04, 0x00, 0x09, 0x00, 0x0D, 0x02, 0x00, 0x00, 0x00,
            ],
            // Handshake echo / ear detection (0x06): ignored.
            &HANDSHAKE,
            &[0x04, 0x00, 0x04, 0x00, 0x06, 0x00, 0x00, 0x00],
        ];
        for p in bad {
            assert_eq!(parse_notification(p), None, "{p:02X?}");
        }
    }

    #[test]
    fn metadata() {
        let mut pkt = vec![0x04, 0x00, 0x04, 0x00, 0x1D, 0x00, 0x02, 0xED, 0x00];
        pkt.extend_from_slice(b"Alex's AirPods Pro\0A2698\0Apple Inc.\0");
        assert_eq!(
            parse_notification(&pkt),
            Some(Notification::Metadata(vec![
                "Alex's AirPods Pro".into(),
                "A2698".into(),
                "Apple Inc.".into()
            ]))
        );
    }

    #[test]
    fn state_to_info() {
        let mut s = AapState::default();
        assert_eq!(s.info(), None);
        // AirPods 3: battery only, no noise control.
        s.apply(Notification::Battery(Battery {
            left: Some(50),
            right: Some(60),
            ..Default::default()
        }));
        let i = s.info().unwrap();
        assert!(i.supported_modes.is_empty());
        assert_eq!(i.battery.right, Some(60));
        // AirPods Pro (1st gen, 0x200E): no Adaptive.
        s.product_id = Some(0x200E);
        s.apply(Notification::ListeningMode(Transparency));
        let i = s.info().unwrap();
        assert_eq!(i.listening_mode, Some(Transparency));
        assert_eq!(i.supported_modes, [Off, Transparency, NoiseCancellation]);
        // AirPods Pro 2: Adaptive.
        s.product_id = Some(0x2014);
        assert_eq!(
            s.info().unwrap().supported_modes,
            [Off, Transparency, Adaptive, NoiseCancellation]
        );
        // Unknown product but the device itself reports Adaptive.
        s.product_id = None;
        s.apply(Notification::ListeningMode(Adaptive));
        assert!(s.info().unwrap().supported_modes.contains(&Adaptive));
    }

    #[test]
    fn modalias_and_apple_detection() {
        assert_eq!(
            parse_modalias("bluetooth:v004Cp2014d6105"),
            Some((0x004C, 0x2014))
        );
        assert_eq!(
            parse_modalias("usb:v05ACp8290d0100"),
            Some((0x05AC, 0x8290))
        );
        assert_eq!(
            parse_modalias("bluetooth:v046DpB023d0007"),
            Some((0x046D, 0xB023))
        );
        assert_eq!(parse_modalias("garbage"), None);
        assert_eq!(parse_modalias("bluetooth:v00"), None);
        assert_eq!(parse_modalias("bluetooth:v004Cx2014"), None);
        assert!(looks_apple("Headphones", Some("bluetooth:v004Cp2014d6105")));
        assert!(looks_apple("Alex's AirPods Pro", None));
        assert!(looks_apple(
            "Beats Studio Pro",
            Some("bluetooth:v004Cp2024")
        ));
        assert!(!looks_apple(
            "MX Master 3",
            Some("bluetooth:v046DpB023d0007")
        ));
        assert!(!looks_apple("WH-1000XM5", None));
    }

    #[test]
    fn bdaddr() {
        assert_eq!(
            bdaddr_bytes("02-2A-3B-4C-5D-6E"),
            Some([0x6E, 0x5D, 0x4C, 0x3B, 0x2A, 0x02])
        );
        assert_eq!(
            bdaddr_bytes("02:2a:3b:4c:5d:6e"),
            Some([0x6E, 0x5D, 0x4C, 0x3B, 0x2A, 0x02])
        );
        assert_eq!(bdaddr_bytes("02-2A-3B-4C-5D"), None);
        assert_eq!(bdaddr_bytes("02-2A-3B-4C-5D-ZZ"), None);
        assert_eq!(bdaddr_bytes("02-2A-3B-4C-5D-6EA"), None);
    }
}
