//! A small append-only log so "what just happened?" can be answered after the fact.
//!
//! macOS: `~/Library/Logs/Duet/duet.log`; Windows: `%LOCALAPPDATA%\Duet\duet.log`;
//! Linux: `~/.local/share/Duet/duet.log`. Rotates to `duet.log.1` past 1 MB.
//! Set `DUET_LOG` to a path to override (tests/mock runs use a temp file).

use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_BYTES: u64 = 1 << 20;
static LOCK: Mutex<()> = Mutex::new(());

pub fn path() -> PathBuf {
    if let Some(p) = std::env::var_os("DUET_LOG") {
        return p.into();
    }
    let base = if cfg!(target_os = "macos") {
        dirs::home_dir().map(|h| h.join("Library/Logs"))
    } else {
        dirs::data_local_dir()
    };
    base.unwrap_or_else(std::env::temp_dir)
        .join("Duet")
        .join("duet.log")
}

/// "2026-11-20 18:42:07.123Z" (UTC; no extra dependencies).
pub fn timestamp(t: SystemTime) -> String {
    let d = t.duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = d.as_secs();
    let (days, rem) = (secs / 86_400, secs % 86_400);
    // Civil-from-days (Howard Hinnant's algorithm).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}.{:03}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60,
        d.subsec_millis()
    )
}

/// Append one line. Never fails loudly: logging must not break the app.
pub fn write(message: &str) {
    let _guard = LOCK.lock();
    let path = path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > MAX_BYTES) {
        let _ = std::fs::rename(&path, path.with_extension("log.1"));
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(f, "{} {message}", timestamp(SystemTime::now()));
    }
}

/// `dlog!("switched to {}", name)` — format and append a line to the Duet log.
#[macro_export]
macro_rules! dlog {
    ($($arg:tt)*) => { $crate::log::write(&format!($($arg)*)) };
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn formats_utc() {
        let t = UNIX_EPOCH + Duration::from_millis(1_795_199_193_162);
        assert_eq!(timestamp(t), "2026-11-20 18:26:33.162Z");
        assert_eq!(timestamp(UNIX_EPOCH), "1970-01-01 00:00:00.000Z");
        let leap = UNIX_EPOCH + Duration::from_secs(951_782_400); // 2000-02-29
        assert!(timestamp(leap).starts_with("2000-02-29"));
    }
}
