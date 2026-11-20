//! Persistent user settings (`~/Library/Application Support/Duet/config.json`).
//!
//! Loading never fails: a missing or corrupt file yields the default config.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// CoreAudio device UID -> user-chosen nickname.
    pub nicknames: BTreeMap<String, String>,
    /// Device UID -> last name the system reported, so a listener still shows
    /// a real name while its AirPods are disconnected (and Bluetooth is unreadable).
    pub known_names: BTreeMap<String, String>,
    /// Bluetooth addresses of the headphones the user chose for Duet, in order.
    /// Empty = automatic (every connected Bluetooth output).
    pub listeners: Vec<String>,
}

/// Exact UID match, else any entry for the same Bluetooth address (a listener
/// may be known by its output UID on one poll and its bare address on another).
fn lookup<'a>(map: &'a BTreeMap<String, String>, uid: &str) -> Option<&'a str> {
    if let Some(v) = map.get(uid) {
        return Some(v);
    }
    let addr = crate::audio::address_from_uid(uid)?;
    map.iter()
        .find(|(k, _)| crate::audio::address_from_uid(k).as_deref() == Some(addr.as_str()))
        .map(|(_, v)| v.as_str())
}

impl Config {
    /// `~/Library/Application Support/Duet/config.json` (falls back to `$HOME/...`).
    pub fn default_path() -> PathBuf {
        let base = dirs::config_dir()
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(|h| PathBuf::from(h).join("Library/Application Support"))
            })
            .unwrap_or_else(std::env::temp_dir);
        base.join("Duet").join("config.json")
    }

    /// Load from `path`; missing/unreadable/corrupt files give `Config::default()`.
    pub fn load_from(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn load() -> Self {
        Self::load_from(&Self::default_path())
    }

    /// Write atomically (temp file + rename), creating parent directories.
    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let json = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, path)
    }

    pub fn save(&self) -> std::io::Result<()> {
        self.save_to(&Self::default_path())
    }

    pub fn nickname(&self, uid: &str) -> Option<&str> {
        lookup(&self.nicknames, uid).filter(|s| !s.trim().is_empty())
    }

    pub fn known_name(&self, uid: &str) -> Option<&str> {
        lookup(&self.known_names, uid)
    }

    /// Record the current name of every device; returns true if anything changed.
    pub fn remember_names<'a>(
        &mut self,
        devices: impl IntoIterator<Item = (&'a str, &'a str)>,
    ) -> bool {
        let mut changed = false;
        for (uid, name) in devices {
            if name.trim().is_empty() || self.known_name(uid) == Some(name) {
                continue;
            }
            self.known_names.insert(uid.to_string(), name.to_string());
            changed = true;
        }
        changed
    }

    /// Set (or, when blank, clear) the nickname for `uid`.
    pub fn set_nickname(&mut self, uid: &str, nickname: &str) {
        let n = nickname.trim();
        if n.is_empty() {
            self.nicknames.remove(uid);
        } else {
            self.nicknames.insert(uid.to_string(), n.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_by_address() {
        let mut c = Config::default();
        c.set_nickname("02-2A-3B-4C-5D-6E:output", "Alex");
        assert_eq!(c.nickname("02-2A-3B-4C-5D-6E"), Some("Alex"));
        assert_eq!(c.nickname("bluez_output.02_2A_3B_4C_5D_6E.1"), Some("Alex"));
        assert_eq!(c.nickname("02-1B-2C-3D-4E-5F"), None);
        let old: Config = serde_json::from_str("{}").unwrap();
        assert!(old.listeners.is_empty());
    }

    #[test]
    fn remembers_device_names() {
        let mut c = Config::default();
        assert!(c.remember_names([("u1", "Alex's AirPods"), ("u2", "")]));
        assert!(!c.remember_names([("u1", "Alex's AirPods")]));
        assert_eq!(c.known_name("u1"), Some("Alex's AirPods"));
        assert_eq!(c.known_name("u2"), None);
        // Old config files without the field still load.
        let old: Config = serde_json::from_str(r#"{"nicknames":{"u1":"Alex"}}"#).unwrap();
        assert_eq!(old.nickname("u1"), Some("Alex"));
        assert!(old.known_names.is_empty());
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let d =
            std::env::temp_dir().join(format!("duet-test-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn round_trip() {
        let dir = temp_dir("rt");
        let path = dir.join("nested").join("config.json");
        let mut c = Config::default();
        c.set_nickname("02-1B-2C-3D-4E-5F:output", "  Sam ");
        c.set_nickname("02-2A-3B-4C-5D-6E:output", "Alex");
        c.save_to(&path).unwrap();
        let loaded = Config::load_from(&path);
        assert_eq!(loaded, c);
        assert_eq!(
            loaded.nickname("02-1B-2C-3D-4E-5F:output"),
            Some("Sam")
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn missing_and_corrupt_files_give_default() {
        let dir = temp_dir("bad");
        assert_eq!(Config::load_from(&dir.join("nope.json")), Config::default());
        let p = dir.join("config.json");
        std::fs::write(&p, "{ not json").unwrap();
        assert_eq!(Config::load_from(&p), Config::default());
        std::fs::write(&p, r#"{"unknown": 1}"#).unwrap();
        assert_eq!(Config::load_from(&p), Config::default());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn blank_nickname_clears() {
        let mut c = Config::default();
        c.set_nickname("a", "X");
        c.set_nickname("a", "   ");
        assert_eq!(c.nickname("a"), None);
        assert!(c.nicknames.is_empty());
    }
}
