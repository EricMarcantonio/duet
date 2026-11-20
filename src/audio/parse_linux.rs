//! Pure parsers/formatters for the Linux backend's CLI tools (`pactl`, `bluetoothctl`).
//!
//! Nothing in here touches the OS, so it is compiled (and unit-tested) on every host;
//! `linux.rs` only runs the commands and feeds their stdout through these functions.
//! All text parsing assumes the commands were run with `LC_ALL=C` (pactl localizes
//! its plain-text output).

use super::{BluetoothDevice, OutputDevice, Transport, normalize_address};
use std::collections::BTreeMap;

/// PulseAudio's `PA_INVALID_INDEX`.
const PA_INVALID_INDEX: u32 = u32::MAX;
/// `PA_VOLUME_NORM` (100%).
const PA_VOLUME_NORM: f32 = 65536.0;

// ---------------------------------------------------------------------------
// pactl: sinks
// ---------------------------------------------------------------------------

/// One sink as reported by `pactl list sinks` (JSON or text).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PaSink {
    pub index: u32,
    pub name: String,
    pub description: String,
    pub driver: String,
    pub owner_module: Option<u32>,
    /// Average channel volume, 1.0 = 100% (may exceed 1.0 when over-amplified).
    pub volume: Option<f32>,
    pub properties: BTreeMap<String, String>,
}

impl PaSink {
    fn prop(&self, key: &str) -> Option<&str> {
        self.properties.get(key).map(String::as_str)
    }
}

/// Parse `pactl list sinks` output in either format (auto-detected).
pub fn parse_sinks(out: &str) -> Result<Vec<PaSink>, String> {
    if out.trim_start().starts_with('[') {
        parse_sinks_json(out)
    } else {
        Ok(parse_sinks_text(out))
    }
}

/// Parse `pactl -f json list sinks` (pactl >= 16, PulseAudio or pipewire-pulse).
pub fn parse_sinks_json(out: &str) -> Result<Vec<PaSink>, String> {
    use serde_json::Value;
    let v: Value =
        serde_json::from_str(out.trim()).map_err(|e| format!("invalid pactl JSON: {e}"))?;
    let arr = v
        .as_array()
        .ok_or("pactl JSON: expected an array of sinks")?;
    let as_u32 = |v: Option<&Value>| -> Option<u32> {
        match v? {
            Value::Number(n) => n.as_u64().and_then(|n| u32::try_from(n).ok()),
            Value::String(s) => s.trim().parse().ok(),
            _ => None,
        }
    };
    let as_string = |v: Option<&Value>| -> String {
        match v {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Null) | None => String::new(),
            Some(other) => other.to_string(),
        }
    };
    let mut sinks = Vec::with_capacity(arr.len());
    for s in arr {
        let Some(index) = as_u32(s.get("index")) else {
            continue;
        };
        let volume = s
            .get("volume")
            .and_then(Value::as_object)
            .and_then(|chans| {
                let vals: Vec<f32> = chans
                    .values()
                    .filter_map(|c| {
                        c.get("value")
                            .and_then(Value::as_f64)
                            .map(|v| v as f32 / PA_VOLUME_NORM)
                            .or_else(|| {
                                c.get("value_percent")
                                    .and_then(Value::as_str)
                                    .and_then(|p| {
                                        p.trim().trim_end_matches('%').trim().parse::<f32>().ok()
                                    })
                                    .map(|p| p / 100.0)
                            })
                    })
                    .collect();
                average(&vals)
            });
        let properties = s
            .get("properties")
            .and_then(Value::as_object)
            .map(|m| {
                m.iter()
                    .map(|(k, v)| (k.clone(), as_string(Some(v))))
                    .collect()
            })
            .unwrap_or_default();
        sinks.push(PaSink {
            index,
            name: as_string(s.get("name")),
            description: as_string(s.get("description")),
            driver: as_string(s.get("driver")),
            owner_module: as_u32(s.get("owner_module")).filter(|&m| m != PA_INVALID_INDEX),
            volume,
            properties,
        });
    }
    Ok(sinks)
}

/// Parse plain-text `pactl list sinks` (any pactl version; run with `LC_ALL=C`).
pub fn parse_sinks_text(out: &str) -> Vec<PaSink> {
    let mut sinks = Vec::new();
    let mut cur: Option<PaSink> = None;
    let mut section = String::new();
    for raw in out.lines() {
        let line = raw.trim_end();
        if let Some(rest) = line.strip_prefix("Sink #") {
            if let Some(s) = cur.take() {
                sinks.push(s);
            }
            cur = rest.trim().parse().ok().map(|index| PaSink {
                index,
                ..Default::default()
            });
            section.clear();
            continue;
        }
        let Some(sink) = cur.as_mut() else { continue };
        if let Some(prop_line) = line.strip_prefix("\t\t") {
            if section == "Properties"
                && let Some((k, v)) = prop_line.split_once(" = ")
            {
                sink.properties
                    .insert(k.trim().to_string(), unquote(v.trim()));
            }
            continue;
        }
        let Some(field) = line.strip_prefix('\t') else {
            continue;
        };
        let Some((key, value)) = field.split_once(':') else {
            continue;
        };
        let value = value.trim();
        section = key.to_string();
        match key {
            "Name" => sink.name = value.to_string(),
            "Description" => sink.description = value.to_string(),
            "Driver" => sink.driver = value.to_string(),
            "Owner Module" => {
                sink.owner_module = value.parse().ok().filter(|&m| m != PA_INVALID_INDEX)
            }
            "Volume" => sink.volume = parse_text_volume(value),
            _ => {}
        }
    }
    if let Some(s) = cur {
        sinks.push(s);
    }
    sinks
}

/// "front-left: 34078 /  52% / -17.04 dB,   front-right: 34078 /  52% / -17.04 dB" -> 0.52
fn parse_text_volume(v: &str) -> Option<f32> {
    let vals: Vec<f32> = v
        .split(',')
        .filter_map(|chan| {
            let raw = chan.split('/').next()?;
            raw.rsplit(':')
                .next()?
                .trim()
                .parse::<f32>()
                .ok()
                .map(|n| n / PA_VOLUME_NORM)
        })
        .collect();
    average(&vals)
}

fn average(vals: &[f32]) -> Option<f32> {
    (!vals.is_empty()).then(|| vals.iter().sum::<f32>() / vals.len() as f32)
}

fn unquote(v: &str) -> String {
    let v = v.trim();
    if v.len() >= 2
        && ((v.starts_with('"') && v.ends_with('"')) || (v.starts_with('\'') && v.ends_with('\'')))
    {
        v[1..v.len() - 1].to_string()
    } else {
        v.to_string()
    }
}

/// `pactl get-default-sink` output -> sink name.
pub fn parse_default_sink(out: &str) -> Option<String> {
    let s = out.lines().map(str::trim).find(|l| !l.is_empty())?;
    Some(s.to_string())
}

/// `pactl info` output ("Default Sink: name") -> sink name (fallback for pactl < 15).
pub fn parse_info_default_sink(out: &str) -> Option<String> {
    out.lines()
        .find_map(|l| l.trim().strip_prefix("Default Sink:"))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

// ---------------------------------------------------------------------------
// pactl: modules
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct PaModule {
    pub index: u32,
    pub name: String,
    pub argument: String,
}

/// Parse `pactl list short modules` ("<index>\t<name>\t<argument>\t<usage>" per line;
/// trailing columns vary across PulseAudio / pipewire-pulse versions).
pub fn parse_modules_short(out: &str) -> Vec<PaModule> {
    out.lines()
        .filter_map(|l| {
            let mut cols = l.split('\t');
            let index = cols.next()?.trim().parse().ok()?;
            let name = cols.next()?.trim().to_string();
            let argument = cols.next().unwrap_or("").trim().to_string();
            Some(PaModule {
                index,
                name,
                argument,
            })
        })
        .collect()
}

/// Parse a module argument string the way PulseAudio's `pa_modargs` does:
/// whitespace-separated `key=value`, values optionally quoted with `'` or `"`,
/// backslash escapes inside values.
pub fn parse_module_args(arg: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut chars = arg.chars().peekable();
    loop {
        while chars.next_if(|c| c.is_whitespace()).is_some() {}
        if chars.peek().is_none() {
            break;
        }
        let mut key = String::new();
        while let Some(c) = chars.next_if(|c| *c != '=' && !c.is_whitespace()) {
            key.push(c);
        }
        let mut value = String::new();
        if chars.next_if_eq(&'=').is_some() {
            let quote = chars.next_if(|c| *c == '"' || *c == '\'');
            while let Some(c) = chars.next() {
                match (c, quote) {
                    ('\\', _) => {
                        if let Some(n) = chars.next() {
                            value.push(n);
                        }
                    }
                    (c, Some(q)) if c == q => break,
                    (c, None) if c.is_whitespace() => break,
                    (c, _) => value.push(c),
                }
            }
        }
        if !key.is_empty() {
            out.insert(key, value);
        }
    }
    out
}

/// A `module-combine-sink` instance: the sink it creates and its member ("slave") sinks.
#[derive(Debug, Clone, PartialEq)]
pub struct CombineSink {
    pub module_index: u32,
    pub sink_name: String,
    pub members: Vec<String>,
}

/// All `module-combine-sink` modules in a module list.
pub fn combine_sinks(modules: &[PaModule]) -> Vec<CombineSink> {
    modules
        .iter()
        .filter(|m| m.name == "module-combine-sink")
        .map(|m| {
            let args = parse_module_args(&m.argument);
            let members = args
                .get("sink_names")
                .or_else(|| args.get("slaves"))
                .map(|s| {
                    s.split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(String::from)
                        .collect()
                })
                .unwrap_or_default();
            CombineSink {
                module_index: m.index,
                sink_name: args
                    .get("sink_name")
                    .cloned()
                    .unwrap_or_else(|| "combined".into()),
                members,
            }
        })
        .collect()
}

/// Arguments (after `pactl load-module module-combine-sink`) to create a combined sink.
/// Index of the module-combine-sink that created sink `sink_name`, if any.
pub fn combine_module_for(modules: &[PaModule], sink_name: &str) -> Option<u32> {
    modules
        .iter()
        .filter(|m| m.name == "module-combine-sink")
        .find(|m| {
            parse_module_args(&m.argument)
                .get("sink_name")
                .map(String::as_str)
                == Some(sink_name)
        })
        .map(|m| m.index)
}

#[cfg(test)]
mod combine_module_tests {
    use super::*;

    #[test]
    fn finds_owning_module() {
        let mods = parse_modules_short(
            "7\tmodule-null-sink\tsink_name=x\t\n\
             23\tmodule-combine-sink\tsink_name=duet_1 slaves=a,b\t\n\
             24\tmodule-combine-sink\tsink_name=other slaves=a,b\t\n",
        );
        assert_eq!(combine_module_for(&mods, "duet_1"), Some(23));
        assert_eq!(combine_module_for(&mods, "x"), None);
        assert_eq!(combine_module_for(&mods, "nope"), None);
    }
}

/// `slaves=` is accepted by both PulseAudio (deprecated alias of `sink_names` since 15)
/// and pipewire-pulse.
pub fn combine_sink_args(sink_name: &str, description: &str, members: &[String]) -> Vec<String> {
    // The description lands inside two levels of quoting (modargs, then proplist), so
    // drop the characters that could break out of it.
    let desc: String = description
        .chars()
        .filter(|c| !matches!(c, '"' | '\'' | '\\'))
        .collect();
    vec![
        format!("sink_name={sink_name}"),
        format!("sink_properties=\"device.description='{desc}'\""),
        format!("slaves={}", members.join(",")),
    ]
}

/// First unused "duet_<n>" sink name.
pub fn next_duet_sink_name(existing: &[PaSink]) -> String {
    (1..)
        .map(|n| format!("duet_{n}"))
        .find(|cand| !existing.iter().any(|s| &s.name == cand))
        .expect("infinite iterator")
}

/// `pactl set-sink-volume` argument for a 0.0..=1.0 volume, e.g. "52%".
pub fn volume_arg(volume: f32) -> String {
    let v = if volume.is_nan() {
        0.0
    } else {
        volume.clamp(0.0, 1.0)
    };
    format!("{}%", (v * 100.0).round() as u32)
}

// ---------------------------------------------------------------------------
// Sink -> OutputDevice
// ---------------------------------------------------------------------------

/// Classify a sink. `is_combine` = the sink was created by `module-combine-sink`.
pub fn classify_sink(sink: &PaSink, is_combine: bool) -> Transport {
    let p = |k| sink.prop(k).unwrap_or("");
    if is_combine || sink.driver.contains("combine") || p("factory.name").contains("combine") {
        return Transport::Aggregate;
    }
    if p("device.bus") == "bluetooth"
        || p("device.api") == "bluez5"
        || sink
            .properties
            .keys()
            .any(|k| k.starts_with("api.bluez5.") || k.starts_with("bluez."))
        || sink.name.starts_with("bluez_")
    {
        return Transport::Bluetooth;
    }
    let is_alsa = p("device.api") == "alsa"
        || sink.properties.contains_key("alsa.card")
        || sink.properties.contains_key("api.alsa.path")
        || sink.name.starts_with("alsa_output.");
    if !is_alsa {
        // module-null-sink, support.null-audio-sink, loopbacks, filter chains, RTP, ...
        return Transport::Virtual;
    }
    let hdmi = p("device.profile.name").contains("hdmi") || sink.name.contains("hdmi");
    let internal = p("device.form_factor") == "internal"
        || matches!(p("device.bus"), "pci" | "isa" | "platform")
        || sink.name.starts_with("alsa_output.pci-")
        || sink.name.starts_with("alsa_output.platform-");
    if internal && !hdmi {
        Transport::BuiltIn
    } else {
        Transport::Other
    }
}

/// Build the backend's device list from parsed `pactl` state.
pub fn sinks_to_devices(
    sinks: &[PaSink],
    modules: &[PaModule],
    default_sink: Option<&str>,
) -> Vec<OutputDevice> {
    let combines = combine_sinks(modules);
    sinks
        .iter()
        .map(|s| {
            let combine = combines
                .iter()
                .find(|c| c.sink_name == s.name || s.owner_module == Some(c.module_index));
            OutputDevice {
                id: s.index,
                uid: s.name.clone(),
                name: if s.description.is_empty() {
                    s.name.clone()
                } else {
                    s.description.clone()
                },
                transport: classify_sink(s, combine.is_some()),
                volume: s.volume.map(|v| v.clamp(0.0, 1.0)),
                is_default: default_sink == Some(s.name.as_str()),
                subdevice_uids: combine.map(|c| c.members.clone()).unwrap_or_default(),
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// bluetoothctl
// ---------------------------------------------------------------------------

/// Remove ANSI escape sequences (bluetoothctl colors its prompt/events) and `\r`.
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\x1b' => {
                if chars.next_if_eq(&'[').is_some() {
                    // CSI: parameters/intermediates until a final byte in 0x40..=0x7e
                    for n in chars.by_ref() {
                        if ('\x40'..='\x7e').contains(&n) {
                            break;
                        }
                    }
                } else {
                    chars.next();
                }
            }
            '\r' | '\x01' | '\x02' => {}
            c => out.push(c),
        }
    }
    out
}

fn is_colon_mac(s: &str) -> bool {
    let parts: Vec<&str> = s.split(':').collect();
    parts.len() == 6
        && parts
            .iter()
            .all(|p| p.len() == 2 && p.chars().all(|c| c.is_ascii_hexdigit()))
}

/// "AA-BB-CC-DD-EE-FF" / "aa:bb:..." -> "AA:BB:CC:DD:EE:FF" (bluetoothctl's form).
pub fn colon_address(addr: &str) -> String {
    normalize_address(addr).replace('-', ":")
}

/// Parse `bluetoothctl devices [Paired]` / `bluetoothctl paired-devices`:
/// lines "Device AA:BB:CC:DD:EE:FF Name". Returns (normalized address, name).
pub fn parse_bt_device_list(out: &str) -> Vec<(String, String)> {
    let clean = strip_ansi(out);
    let mut seen = Vec::<(String, String)>::new();
    for line in clean.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("Device ") else {
            continue;
        };
        let (addr, name) = rest.split_once(' ').unwrap_or((rest, ""));
        if !is_colon_mac(addr) {
            continue;
        }
        let addr = normalize_address(addr);
        if !seen.iter().any(|(a, _)| *a == addr) {
            let name = if name.trim().is_empty() {
                addr.clone()
            } else {
                name.trim().to_string()
            };
            seen.push((addr, name));
        }
    }
    seen
}

/// Parsed `bluetoothctl info <MAC>`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct BtInfo {
    pub address: String,
    pub name: Option<String>,
    pub alias: Option<String>,
    pub icon: Option<String>,
    pub class: Option<u32>,
    pub paired: Option<bool>,
    pub connected: bool,
    pub uuids: Vec<String>,
    /// e.g. "bluetooth:v004Cp2014d6105" (vendor 004C = Apple, product 2014).
    pub modalias: Option<String>,
}

impl BtInfo {
    /// Headphones / headset / speaker: icon "audio-*", CoD major class 0x04 (Audio/Video),
    /// or an audio-sink / headset profile UUID.
    pub fn is_audio(&self) -> bool {
        const AUDIO_UUIDS: [&str; 4] = [
            "0000110b-0000-1000-8000-00805f9b34fb", // A2DP Audio Sink
            "0000111e-0000-1000-8000-00805f9b34fb", // Handsfree
            "00001108-0000-1000-8000-00805f9b34fb", // Headset
            "0000184e-0000-1000-8000-00805f9b34fb", // LE Audio Stream Control
        ];
        self.icon
            .as_deref()
            .is_some_and(|i| i.starts_with("audio-"))
            || self.class.is_some_and(|c| (c >> 8) & 0x1f == 0x04)
            || self
                .uuids
                .iter()
                .any(|u| AUDIO_UUIDS.iter().any(|a| u.eq_ignore_ascii_case(a)))
    }

    pub fn to_device(&self, fallback_name: &str) -> BluetoothDevice {
        let name = self
            .alias
            .clone()
            .or_else(|| self.name.clone())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| fallback_name.to_string());
        BluetoothDevice {
            address: self.address.clone(),
            name,
            connected: self.connected,
            is_audio: self.is_audio(),
        }
    }
}

/// Parse `bluetoothctl info <MAC>`. None if the device is unknown ("Device ... not available").
pub fn parse_bt_info(out: &str) -> Option<BtInfo> {
    let clean = strip_ansi(out);
    let mut info: Option<BtInfo> = None;
    for line in clean.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("Device ") {
            let addr = rest.split_whitespace().next().unwrap_or("");
            if is_colon_mac(addr) && !rest.contains("not available") {
                if info.is_some() {
                    break; // a second device block: stop at the first
                }
                info = Some(BtInfo {
                    address: normalize_address(addr),
                    ..Default::default()
                });
            }
            continue;
        }
        let Some(i) = info.as_mut() else { continue };
        let Some((key, value)) = t.split_once(':') else {
            continue;
        };
        let value = value.trim();
        let yes = value.eq_ignore_ascii_case("yes");
        match key {
            "Name" => i.name = Some(value.to_string()),
            "Alias" => i.alias = Some(value.to_string()),
            "Icon" => i.icon = Some(value.to_string()),
            "Class" => {
                let hex = value.split_whitespace().next().unwrap_or("");
                i.class =
                    u32::from_str_radix(hex.trim_start_matches("0x").trim_start_matches("0X"), 16)
                        .ok();
            }
            "Paired" => i.paired = Some(yes),
            "Connected" => i.connected = yes,
            "Modalias" => i.modalias = Some(value.to_string()),
            "UUID" => {
                if let (Some(open), Some(close)) = (value.rfind('('), value.rfind(')'))
                    && open < close
                {
                    i.uuids.push(value[open + 1..close].to_ascii_lowercase());
                }
            }
            _ => {}
        }
    }
    info
}

/// Interpret the output of `bluetoothctl connect|disconnect <MAC>`. Older BlueZ always
/// exits 0, so the text decides; newer versions also return non-zero on failure.
pub fn bt_command_result(out: &str, exit_ok: bool) -> Result<(), String> {
    let clean = strip_ansi(out);
    let lower = clean.to_ascii_lowercase();
    let failure = clean.lines().map(str::trim).find(|l| {
        let ll = l.to_ascii_lowercase();
        ll.starts_with("failed")
            || ll.contains("not available")
            || ll.contains("org.bluez.error")
            || ll.starts_with("no default controller")
    });
    if let Some(f) = failure {
        return Err(f.to_string());
    }
    if lower.contains("successful") || exit_ok {
        Ok(())
    } else {
        let last = clean
            .lines()
            .map(str::trim)
            .rfind(|l| !l.is_empty())
            .unwrap_or("bluetoothctl failed");
        Err(last.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // `pactl -f json list sinks` on Fedora 40 / PipeWire 1.0.x (pactl 16), trimmed.
    const PW_SINKS_JSON: &str = r#"[{"index":47,"state":"SUSPENDED","name":"alsa_output.pci-0000_00_1f.3.analog-stereo","description":"Built-in Audio Analog Stereo","driver":"PipeWire","sample_specification":"s32le 2ch 48000Hz","channel_map":"front-left,front-right","owner_module":4294967295,"mute":false,"volume":{"front-left":{"value":26214,"value_percent":"40%","db":"-23.88 dB"},"front-right":{"value":26214,"value_percent":"40%","db":"-23.88 dB"}},"balance":0.000000,"base_volume":{"value":65536,"value_percent":"100%","db":"0.00 dB"},"monitor_source":"alsa_output.pci-0000_00_1f.3.analog-stereo.monitor","latency":{"actual":0.000000,"configured":0.000000},"flags":["HARDWARE","HW_MUTE_CTRL","HW_VOLUME_CTRL","DECIBEL_VOLUME","LATENCY"],"properties":{"alsa.card":"0","alsa.card_name":"HDA Intel PCH","api.alsa.path":"front:0","api.alsa.pcm.card":"0","device.api":"alsa","device.bus":"pci","device.class":"sound","device.form_factor":"internal","device.profile.name":"analog-stereo","device.description":"Built-in Audio","media.class":"Audio/Sink","node.name":"alsa_output.pci-0000_00_1f.3.analog-stereo","object.serial":"48"},"ports":[{"name":"analog-output-speaker","description":"Speakers","type":"Speaker","priority":10000,"availability_group":"Legacy 1","availability":"available"}],"active_port":"analog-output-speaker","formats":["pcm"]},{"index":61,"state":"RUNNING","name":"bluez_output.02_2A_3B_4C_5D_6E.1","description":"Alex's AirPods Pro","driver":"PipeWire","sample_specification":"s16le 2ch 48000Hz","channel_map":"front-left,front-right","owner_module":4294967295,"mute":false,"volume":{"front-left":{"value":34078,"value_percent":"52%","db":"-17.04 dB"},"front-right":{"value":34078,"value_percent":"52%","db":"-17.04 dB"}},"balance":0.000000,"base_volume":{"value":65536,"value_percent":"100%","db":"0.00 dB"},"monitor_source":"bluez_output.02_2A_3B_4C_5D_6E.1.monitor","latency":{"actual":0.000000,"configured":0.000000},"flags":["HARDWARE","HW_VOLUME_CTRL","LATENCY"],"properties":{"api.bluez5.address":"02:2A:3B:4C:5D:6E","api.bluez5.codec":"aac","api.bluez5.profile":"a2dp-sink","bluez5.loopback":"false","device.api":"bluez5","device.bus":"bluetooth","device.description":"Alex's AirPods Pro","device.form_factor":"headphone","device.icon_name":"audio-headphones-bluetooth","media.class":"Audio/Sink","node.name":"bluez_output.02_2A_3B_4C_5D_6E.1"},"ports":[{"name":"headphone-output","description":"Headphone","type":"Headphones","priority":0,"availability_group":null,"availability":"available"}],"active_port":"headphone-output","formats":["pcm"]},{"index":64,"state":"IDLE","name":"bluez_output.02_1B_2C_3D_4E_5F.1","description":"Sam’s AirPods Pro #2","driver":"PipeWire","sample_specification":"s16le 2ch 48000Hz","channel_map":"front-left,front-right","owner_module":4294967295,"mute":false,"volume":{"front-left":{"value":39321,"value_percent":"60%","db":"-13.31 dB"},"front-right":{"value":39321,"value_percent":"60%","db":"-13.31 dB"}},"balance":0.000000,"base_volume":{"value":65536,"value_percent":"100%","db":"0.00 dB"},"monitor_source":"bluez_output.02_1B_2C_3D_4E_5F.1.monitor","latency":{"actual":0.000000,"configured":0.000000},"flags":["HARDWARE","HW_VOLUME_CTRL","LATENCY"],"properties":{"api.bluez5.address":"02:1B:2C:3D:4E:5F","api.bluez5.profile":"a2dp-sink","device.api":"bluez5","media.class":"Audio/Sink","node.name":"bluez_output.02_1B_2C_3D_4E_5F.1"},"ports":[],"active_port":null,"formats":["pcm"]},{"index":70,"state":"RUNNING","name":"duet_1","description":"Duet","driver":"PipeWire","sample_specification":"float32le 2ch 48000Hz","channel_map":"front-left,front-right","owner_module":536870914,"mute":false,"volume":{"front-left":{"value":65536,"value_percent":"100%","db":"0.00 dB"},"front-right":{"value":65536,"value_percent":"100%","db":"0.00 dB"}},"balance":0.000000,"base_volume":{"value":65536,"value_percent":"100%","db":"0.00 dB"},"monitor_source":"duet_1.monitor","latency":{"actual":0.000000,"configured":0.000000},"flags":["DECIBEL_VOLUME","LATENCY"],"properties":{"node.name":"duet_1","device.description":"Duet","media.class":"Audio/Sink","node.virtual":"true","pulse.module.id":"2"},"ports":[],"active_port":null,"formats":["pcm"]},{"index":75,"state":"SUSPENDED","name":"Virtual_Sink","description":"Virtual Sink","driver":"PipeWire","sample_specification":"float32le 2ch 48000Hz","channel_map":"front-left,front-right","owner_module":536870915,"mute":false,"volume":{"front-left":{"value":65536,"value_percent":"100%","db":"0.00 dB"},"front-right":{"value":65536,"value_percent":"100%","db":"0.00 dB"}},"balance":0.000000,"base_volume":{"value":65536,"value_percent":"100%","db":"0.00 dB"},"monitor_source":"Virtual_Sink.monitor","latency":{"actual":0.000000,"configured":0.000000},"flags":["DECIBEL_VOLUME","LATENCY"],"properties":{"factory.name":"support.null-audio-sink","media.class":"Audio/Sink","node.name":"Virtual_Sink","node.virtual":"true"},"ports":[],"active_port":null,"formats":["pcm"]}]"#;

    // `pactl list short modules` on pipewire-pulse 1.0.
    const PW_MODULES_SHORT: &str = "536870912\tlibpipewire-module-protocol-pulse\t\t\n\
536870913\tmodule-always-sink\t\t\n\
536870914\tmodule-combine-sink\tsink_name=duet_1 sink_properties=\"device.description='Duet'\" slaves=bluez_output.02_2A_3B_4C_5D_6E.1,bluez_output.02_1B_2C_3D_4E_5F.1\t\n\
536870915\tmodule-null-sink\tsink_name=Virtual_Sink\t\n";

    // `LC_ALL=C pactl list sinks` on Ubuntu 22.04 / PulseAudio 15.99.1 (pactl 15, no JSON).
    const PA_SINKS_TEXT: &str = "Sink #0
\tState: SUSPENDED
\tName: alsa_output.pci-0000_00_1f.3.analog-stereo
\tDescription: Built-in Audio Analog Stereo
\tDriver: module-alsa-card.c
\tSample Specification: s16le 2ch 44100Hz
\tChannel Map: front-left,front-right
\tOwner Module: 7
\tMute: no
\tVolume: front-left: 49152 /  75% / -7.50 dB,   front-right: 49152 /  75% / -7.50 dB
\t        balance 0.00
\tBase Volume: 65536 / 100% / 0.00 dB
\tMonitor Source: alsa_output.pci-0000_00_1f.3.analog-stereo.monitor
\tLatency: 0 usec, configured 0 usec
\tFlags: HARDWARE HW_MUTE_CTRL HW_VOLUME_CTRL DECIBEL_VOLUME LATENCY
\tProperties:
\t\talsa.resolution_bits = \"16\"
\t\tdevice.api = \"alsa\"
\t\tdevice.class = \"sound\"
\t\talsa.class = \"generic\"
\t\talsa.card = \"0\"
\t\tdevice.bus = \"pci\"
\t\tdevice.form_factor = \"internal\"
\t\tdevice.profile.name = \"analog-stereo\"
\t\tdevice.description = \"Built-in Audio Analog Stereo\"
\t\tdevice.icon_name = \"audio-card-pci\"
\tPorts:
\t\tanalog-output-speaker: Speakers (type: Speaker, priority: 10000, availability unknown)
\t\tanalog-output-headphones: Headphones (type: Headphones, priority: 9900, not available)
\tActive Port: analog-output-speaker
\tFormats:
\t\tpcm

Sink #3
\tState: RUNNING
\tName: bluez_sink.02_2A_3B_4C_5D_6E.a2dp_sink
\tDescription: Alex's AirPods Pro
\tDriver: module-bluez5-device.c
\tSample Specification: s16le 2ch 44100Hz
\tChannel Map: front-left,front-right
\tOwner Module: 27
\tMute: no
\tVolume: front-left: 34078 /  52% / -17.04 dB,   front-right: 34078 /  52% / -17.04 dB
\t        balance 0.00
\tBase Volume: 65536 / 100% / 0.00 dB
\tMonitor Source: bluez_sink.02_2A_3B_4C_5D_6E.a2dp_sink.monitor
\tLatency: 41290 usec, configured 40000 usec
\tFlags: HARDWARE DECIBEL_VOLUME LATENCY
\tProperties:
\t\tbluetooth.protocol = \"a2dp_sink\"
\t\tdevice.description = \"Alex's AirPods Pro\"
\t\tdevice.string = \"02:2A:3B:4C:5D:6E\"
\t\tdevice.api = \"bluez\"
\t\tdevice.class = \"sound\"
\t\tdevice.bus = \"bluetooth\"
\t\tdevice.form_factor = \"headphone\"
\t\tbluez.path = \"/org/bluez/hci0/dev_02_2A_3B_4C_5D_6E\"
\t\tbluez.class = \"0x240418\"
\t\tbluez.alias = \"Alex's AirPods Pro\"
\t\tdevice.icon_name = \"audio-headphones-bluetooth\"
\tPorts:
\t\theadphone-output: Headphone (type: Headphones, priority: 0, available)
\tActive Port: headphone-output
\tFormats:
\t\tpcm

Sink #4
\tState: SUSPENDED
\tName: bluez_sink.02_1B_2C_3D_4E_5F.a2dp_sink
\tDescription: Sam's AirPods Pro #2
\tDriver: module-bluez5-device.c
\tSample Specification: s16le 2ch 44100Hz
\tChannel Map: front-left,front-right
\tOwner Module: 29
\tMute: no
\tVolume: front-left: 39322 /  60% / -13.31 dB,   front-right: 39322 /  60% / -13.31 dB
\t        balance 0.00
\tBase Volume: 65536 / 100% / 0.00 dB
\tMonitor Source: bluez_sink.02_1B_2C_3D_4E_5F.a2dp_sink.monitor
\tLatency: 0 usec, configured 0 usec
\tFlags: HARDWARE DECIBEL_VOLUME LATENCY
\tProperties:
\t\tdevice.bus = \"bluetooth\"
\t\tdevice.description = \"Sam's AirPods Pro #2\"
\tFormats:
\t\tpcm

Sink #5
\tState: RUNNING
\tName: duet_1
\tDescription: Duet
\tDriver: module-combine-sink.c
\tSample Specification: s16le 2ch 44100Hz
\tChannel Map: front-left,front-right
\tOwner Module: 31
\tMute: no
\tVolume: front-left: 65536 / 100% / 0.00 dB,   front-right: 65536 / 100% / 0.00 dB
\t        balance 0.00
\tBase Volume: 65536 / 100% / 0.00 dB
\tMonitor Source: duet_1.monitor
\tLatency: 0 usec, configured 0 usec
\tFlags: DECIBEL_VOLUME LATENCY
\tProperties:
\t\tdevice.class = \"filter\"
\t\tdevice.description = \"Duet\"
\t\tdevice.icon_name = \"audio-card\"
\tFormats:
\t\tpcm

Sink #6
\tState: SUSPENDED
\tName: null
\tDescription: Null Output
\tDriver: module-null-sink.c
\tSample Specification: s16le 2ch 44100Hz
\tChannel Map: front-left,front-right
\tOwner Module: 33
\tMute: no
\tVolume: mono: 65536 / 100% / 0.00 dB
\t        balance 0.00
\tBase Volume: 65536 / 100% / 0.00 dB
\tMonitor Source: null.monitor
\tLatency: 0 usec, configured 0 usec
\tFlags: DECIBEL_VOLUME LATENCY
\tProperties:
\t\tdevice.description = \"Null Output\"
\t\tdevice.class = \"abstract\"
\t\tdevice.icon_name = \"audio-card\"
\tFormats:
\t\tpcm
";

    // `pactl list short modules` on PulseAudio 15.99.
    const PA_MODULES_SHORT: &str = "0\tmodule-device-restore\t\t\n\
7\tmodule-alsa-card\tdevice_id=\"0\" name=\"pci-0000_00_1f.3\" card_name=\"alsa_card.pci-0000_00_1f.3\" namereg_fail=false tsched=yes fixed_latency_range=no ignore_dB=no deferred_volume=yes use_ucm=yes avoid_resampling=no card_properties=\"module-udev-detect.discovered=1\"\t\n\
27\tmodule-bluez5-device\tpath=/org/bluez/hci0/dev_02_2A_3B_4C_5D_6E autodetect_mtu=0\t\n\
29\tmodule-bluez5-device\tpath=/org/bluez/hci0/dev_02_1B_2C_3D_4E_5F autodetect_mtu=0\t\n\
31\tmodule-combine-sink\tsink_name=duet_1 sink_properties=\"device.description='Duet'\" sink_names=bluez_sink.02_2A_3B_4C_5D_6E.a2dp_sink,bluez_sink.02_1B_2C_3D_4E_5F.a2dp_sink\t\n\
33\tmodule-null-sink\t\t\n";

    const PACTL_INFO: &str = "Server String: /run/user/1000/pulse/native
Library Protocol Version: 35
Server Protocol Version: 35
Is Local: yes
Client Index: 42
Tile Size: 65472
User Name: sam
Host Name: thinkpad
Server Name: pulseaudio
Server Version: 15.99.1
Default Sample Specification: s16le 2ch 44100Hz
Default Channel Map: front-left,front-right
Default Sink: bluez_sink.02_2A_3B_4C_5D_6E.a2dp_sink
Default Source: alsa_input.pci-0000_00_1f.3.analog-stereo
Cookie: 4a1d:9b2e
";

    #[test]
    fn json_sinks() {
        let sinks = parse_sinks(PW_SINKS_JSON).unwrap();
        assert_eq!(sinks.len(), 5);
        assert_eq!(sinks[0].index, 47);
        assert_eq!(sinks[0].owner_module, None);
        assert!((sinks[0].volume.unwrap() - 0.40).abs() < 0.001);
        assert_eq!(sinks[1].name, "bluez_output.02_2A_3B_4C_5D_6E.1");
        assert_eq!(sinks[1].description, "Alex's AirPods Pro");
        assert!((sinks[1].volume.unwrap() - 0.52).abs() < 0.001);
        assert_eq!(sinks[3].owner_module, Some(536870914));
        assert_eq!(
            sinks[1].properties["api.bluez5.address"],
            "02:2A:3B:4C:5D:6E"
        );
    }

    #[test]
    fn json_garbage_is_error() {
        assert!(parse_sinks_json("[{").is_err());
        assert!(parse_sinks_json("{}").is_err());
        assert_eq!(parse_sinks_json("[]").unwrap(), vec![]);
    }

    #[test]
    fn json_devices() {
        let sinks = parse_sinks_json(PW_SINKS_JSON).unwrap();
        let mods = parse_modules_short(PW_MODULES_SHORT);
        let ds = sinks_to_devices(&sinks, &mods, Some("duet_1"));
        let t: Vec<_> = ds.iter().map(|d| d.transport).collect();
        assert_eq!(
            t,
            [
                Transport::BuiltIn,
                Transport::Bluetooth,
                Transport::Bluetooth,
                Transport::Aggregate,
                Transport::Virtual
            ]
        );
        let duet = &ds[3];
        assert!(duet.is_default && duet.is_multi_output());
        assert_eq!(duet.uid, "duet_1");
        assert_eq!(duet.id, 70);
        assert_eq!(
            duet.subdevice_uids,
            [
                "bluez_output.02_2A_3B_4C_5D_6E.1",
                "bluez_output.02_1B_2C_3D_4E_5F.1"
            ]
        );
        assert_eq!(ds.iter().filter(|d| d.is_default).count(), 1);
        assert_eq!(
            super::super::bluetooth_address_of(&ds[1]).as_deref(),
            Some("02-2A-3B-4C-5D-6E")
        );
        assert_eq!(ds[2].name, "Sam’s AirPods Pro #2");
    }

    #[test]
    fn text_sinks() {
        let sinks = parse_sinks(PA_SINKS_TEXT).unwrap();
        assert_eq!(sinks.len(), 5);
        let names: Vec<_> = sinks.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "alsa_output.pci-0000_00_1f.3.analog-stereo",
                "bluez_sink.02_2A_3B_4C_5D_6E.a2dp_sink",
                "bluez_sink.02_1B_2C_3D_4E_5F.a2dp_sink",
                "duet_1",
                "null"
            ]
        );
        assert_eq!(sinks[1].index, 3);
        assert_eq!(sinks[1].owner_module, Some(27));
        assert!((sinks[0].volume.unwrap() - 0.75).abs() < 0.001);
        assert!((sinks[1].volume.unwrap() - 0.52).abs() < 0.001);
        assert!((sinks[4].volume.unwrap() - 1.0).abs() < 0.001);
        assert_eq!(sinks[1].properties["device.bus"], "bluetooth");
        assert_eq!(sinks[1].properties["bluez.alias"], "Alex's AirPods Pro");
        // Ports section (also double-tab indented) must not leak into properties.
        assert!(
            !sinks[0]
                .properties
                .contains_key("analog-output-speaker: Speakers (type")
        );
        assert_eq!(sinks[0].properties.len(), 10);
    }

    #[test]
    fn text_devices() {
        let sinks = parse_sinks_text(PA_SINKS_TEXT);
        let mods = parse_modules_short(PA_MODULES_SHORT);
        let default = parse_info_default_sink(PACTL_INFO);
        assert_eq!(
            default.as_deref(),
            Some("bluez_sink.02_2A_3B_4C_5D_6E.a2dp_sink")
        );
        let ds = sinks_to_devices(&sinks, &mods, default.as_deref());
        let t: Vec<_> = ds.iter().map(|d| d.transport).collect();
        assert_eq!(
            t,
            [
                Transport::BuiltIn,
                Transport::Bluetooth,
                Transport::Bluetooth,
                Transport::Aggregate,
                Transport::Virtual
            ]
        );
        assert!(ds[1].is_default);
        assert_eq!(ds[3].subdevice_uids.len(), 2);
        assert_eq!(
            ds[3].subdevice_uids[1],
            "bluez_sink.02_1B_2C_3D_4E_5F.a2dp_sink"
        );
        assert_eq!(
            super::super::bluetooth_address_of(&ds[2]).as_deref(),
            Some("02-1B-2C-3D-4E-5F")
        );
    }

    #[test]
    fn default_sink() {
        assert_eq!(
            parse_default_sink("bluez_output.02_2A_3B_4C_5D_6E.1\n").as_deref(),
            Some("bluez_output.02_2A_3B_4C_5D_6E.1")
        );
        assert_eq!(parse_default_sink("\n"), None);
        assert_eq!(parse_info_default_sink("Server Name: x\n"), None);
    }

    #[test]
    fn module_args() {
        let a = parse_module_args(
            r#"sink_name=duet_1 sink_properties="device.description='Duet Mirror'" slaves=a,b  rate=48000 flag"#,
        );
        assert_eq!(a["sink_name"], "duet_1");
        assert_eq!(a["sink_properties"], "device.description='Duet Mirror'");
        assert_eq!(a["slaves"], "a,b");
        assert_eq!(a["rate"], "48000");
        assert_eq!(a["flag"], "");
        let b = parse_module_args(r#"x='a b\'c' y=\"z"#);
        assert_eq!(b["x"], "a b'c");
        assert_eq!(b["y"], "\"z");
        assert!(parse_module_args("   ").is_empty());
    }

    #[test]
    fn combine_args_roundtrip() {
        let members = vec![
            "bluez_output.02_2A_3B_4C_5D_6E.1".to_string(),
            "bluez_output.02_1B_2C_3D_4E_5F.1".to_string(),
        ];
        let args = combine_sink_args("duet_2", "Living \"Room's\"", &members);
        assert_eq!(
            args[1],
            "sink_properties=\"device.description='Living Rooms'\""
        );
        let parsed = parse_module_args(&args.join(" "));
        assert_eq!(parsed["sink_name"], "duet_2");
        assert_eq!(parsed["slaves"], members.join(","));
        let m = PaModule {
            index: 9,
            name: "module-combine-sink".into(),
            argument: args.join(" "),
        };
        let c = combine_sinks(&[m]);
        assert_eq!(c[0].sink_name, "duet_2");
        assert_eq!(c[0].members, members);
    }

    #[test]
    fn combine_defaults() {
        let m = PaModule {
            index: 1,
            name: "module-combine-sink".into(),
            argument: String::new(),
        };
        assert_eq!(
            combine_sinks(&[m]),
            vec![CombineSink {
                module_index: 1,
                sink_name: "combined".into(),
                members: vec![]
            }]
        );
    }

    #[test]
    fn duet_names_and_volume() {
        let sinks = parse_sinks_text(PA_SINKS_TEXT);
        assert_eq!(next_duet_sink_name(&sinks), "duet_2");
        assert_eq!(next_duet_sink_name(&[]), "duet_1");
        assert_eq!(volume_arg(0.52), "52%");
        assert_eq!(volume_arg(1.7), "100%");
        assert_eq!(volume_arg(-1.0), "0%");
        assert_eq!(volume_arg(f32::NAN), "0%");
    }

    #[test]
    fn classify_usb_and_hdmi() {
        let mut s = PaSink {
            name: "alsa_output.usb-Focusrite_Scarlett_2i2-00.analog-stereo".into(),
            ..Default::default()
        };
        s.properties.insert("device.api".into(), "alsa".into());
        s.properties.insert("device.bus".into(), "usb".into());
        assert_eq!(classify_sink(&s, false), Transport::Other);
        let mut h = PaSink {
            name: "alsa_output.pci-0000_01_00.1.hdmi-stereo".into(),
            ..Default::default()
        };
        h.properties
            .insert("device.profile.name".into(), "hdmi-stereo".into());
        assert_eq!(classify_sink(&h, false), Transport::Other);
        let rtp = PaSink {
            name: "rtp_sink".into(),
            ..Default::default()
        };
        assert_eq!(classify_sink(&rtp, false), Transport::Virtual);
    }

    // `bluetoothctl devices Paired` (BlueZ 5.72); first lines are typical stray output.
    const BT_DEVICES: &str = "\x1b[0;94m[bluetooth]\x1b[0m# \rAgent registered\n\
Device 02:2A:3B:4C:5D:6E Alex's AirPods Pro\n\
Device 02:1B:2C:3D:4E:5F Sam’s AirPods Pro #2 - Find My\n\
Device C8:3F:26:11:22:33 MX Master 3\n\
[\x1b[0;93mCHG\x1b[0m] Device 02:2A:3B:4C:5D:6E Connected: yes\n\
Device 02:2A:3B:4C:5D:6E Alex's AirPods Pro\n";

    // `bluetoothctl paired-devices` (BlueZ 5.64).
    const BT_PAIRED_OLD: &str =
        "Device 02:3C:4D:5E:6F:70 Alex’s AirPods Pro\nDevice 11:22:33:44:55:66 11-22-33-44-55-66\n";

    // `bluetoothctl info 02:2A:3B:4C:5D:6E` (BlueZ 5.72).
    const BT_INFO_AIRPODS: &str = "Device 02:2A:3B:4C:5D:6E (public)
\tName: Alex's AirPods Pro
\tAlias: Alex's AirPods Pro
\tClass: 0x00240418 (2360344)
\tIcon: audio-headphones
\tPaired: yes
\tBonded: yes
\tTrusted: yes
\tBlocked: no
\tConnected: yes
\tLegacyPairing: no
\tUUID: Vendor specific           (74ec2172-0bad-4d01-8f77-997b2be0722a)
\tUUID: Serial Port               (00001101-0000-1000-8000-00805f9b34fb)
\tUUID: Audio Sink                (0000110b-0000-1000-8000-00805f9b34fb)
\tUUID: A/V Remote Control Target (0000110c-0000-1000-8000-00805f9b34fb)
\tUUID: Handsfree                 (0000111e-0000-1000-8000-00805f9b34fb)
\tModalias: bluetooth:v004Cp2014d6105
\tBattery Percentage: 0x5a (90)
";

    // `bluetoothctl info` for a mouse, BlueZ 5.64 (no Bonded line), not connected.
    const BT_INFO_MOUSE: &str = "Device C8:3F:26:11:22:33 (random)
\tName: MX Master 3
\tAlias: MX Master 3
\tAppearance: 0x03c2
\tIcon: input-mouse
\tPaired: yes
\tTrusted: yes
\tBlocked: no
\tConnected: no
\tLegacyPairing: no
\tUUID: Generic Access Profile    (00001800-0000-1000-8000-00805f9b34fb)
\tUUID: Human Interface Device    (00001812-0000-1000-8000-00805f9b34fb)
";

    // Headset that reports only a class (no icon) and a renamed alias.
    const BT_INFO_CLASS_ONLY: &str = "Device 02:1B:2C:3D:4E:5F (public)
\tName: AirPods Pro
\tAlias: Sam’s AirPods Pro #2 - Find My
\tClass: 0x240404
\tPaired: yes
\tConnected: no
";

    #[test]
    fn bt_device_lists() {
        let d = parse_bt_device_list(BT_DEVICES);
        assert_eq!(
            d,
            vec![
                (
                    "02-2A-3B-4C-5D-6E".to_string(),
                    "Alex's AirPods Pro".to_string()
                ),
                (
                    "02-1B-2C-3D-4E-5F".to_string(),
                    "Sam’s AirPods Pro #2 - Find My".to_string()
                ),
                ("C8-3F-26-11-22-33".to_string(), "MX Master 3".to_string()),
            ]
        );
        let old = parse_bt_device_list(BT_PAIRED_OLD);
        assert_eq!(old.len(), 2);
        assert_eq!(old[0].0, "02-3C-4D-5E-6F-70");
        assert!(parse_bt_device_list("Invalid command in menu main: paired-devices\n").is_empty());
        assert!(parse_bt_device_list("No default controller available\n").is_empty());
    }

    #[test]
    fn bt_info() {
        let a = parse_bt_info(BT_INFO_AIRPODS).unwrap();
        assert_eq!(a.address, "02-2A-3B-4C-5D-6E");
        assert_eq!(a.class, Some(0x240418));
        assert_eq!(a.paired, Some(true));
        assert!(a.connected && a.is_audio());
        assert_eq!(a.uuids.len(), 5);
        assert_eq!(a.modalias.as_deref(), Some("bluetooth:v004Cp2014d6105"));
        let dev = a.to_device("fallback");
        assert_eq!(
            dev,
            BluetoothDevice {
                address: "02-2A-3B-4C-5D-6E".into(),
                name: "Alex's AirPods Pro".into(),
                connected: true,
                is_audio: true
            }
        );

        let m = parse_bt_info(BT_INFO_MOUSE).unwrap();
        assert!(!m.connected && !m.is_audio());

        let c = parse_bt_info(BT_INFO_CLASS_ONLY).unwrap();
        assert!(c.is_audio());
        assert_eq!(c.to_device("x").name, "Sam’s AirPods Pro #2 - Find My");

        assert_eq!(
            parse_bt_info("Device 02:2A:3B:4C:5D:6E not available\n"),
            None
        );
        assert_eq!(parse_bt_info(""), None);
    }

    #[test]
    fn bt_results() {
        let ok = "Attempting to connect to 02:2A:3B:4C:5D:6E\n[CHG] Device 02:2A:3B:4C:5D:6E Connected: yes\nConnection successful\n";
        assert_eq!(bt_command_result(ok, true), Ok(()));
        let fail = "Attempting to connect to 02:2A:3B:4C:5D:6E\nFailed to connect: org.bluez.Error.Failed br-connection-profile-unavailable\n";
        assert_eq!(
            bt_command_result(fail, true),
            Err(
                "Failed to connect: org.bluez.Error.Failed br-connection-profile-unavailable"
                    .into()
            )
        );
        assert!(bt_command_result("Device 02:2A:3B:4C:5D:6E not available\n", true).is_err());
        let disc = "Attempting to disconnect from 02:1B:2C:3D:4E:5F\n[CHG] Device 02:1B:2C:3D:4E:5F ServicesResolved: no\nSuccessful disconnected\n";
        assert_eq!(bt_command_result(disc, true), Ok(()));
        assert!(bt_command_result("weird\n", false).is_err());
        assert_eq!(bt_command_result("", true), Ok(()));
    }

    #[test]
    fn ansi_and_addresses() {
        assert_eq!(
            strip_ansi("\x1b[0;94m[bluetooth]\x1b[0m# hi\r"),
            "[bluetooth]# hi"
        );
        assert_eq!(colon_address("02-2a-3b-4c-5d-6e"), "02:2A:3B:4C:5D:6E");
        assert_eq!(colon_address("02:2A:3B:4C:5D:6E"), "02:2A:3B:4C:5D:6E");
    }
}
