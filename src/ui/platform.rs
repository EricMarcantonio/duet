//! Per-OS hooks the UI calls but doesn't implement itself.

/// Where to change AirPods noise control when Duet can't (macOS keeps the
/// Bluetooth control channel for itself, so only Control Center can switch it).
/// `None` where Duet does it in-app (Linux) or the OS has nothing to change
/// (Windows).
pub fn noise_control_hint() -> Option<&'static str> {
    #[cfg(target_os = "macos")]
    {
        Some("Control Center › Sound")
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

/// Open a URL in the user's browser (the update footer's download link).
///
/// `spawn` rather than `status`: the browser may stay in the foreground for as
/// long as the user likes, and this is called from a click handler.
pub fn open_url(url: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(url)
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("couldn't open the link: {e}"))
    }
    #[cfg(target_os = "windows")]
    {
        // The empty string is `start`'s window-title argument: without it a
        // quoted URL would be taken as the title and nothing would open.
        std::process::Command::new("cmd")
            .args(["/C", "start", "", url])
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("couldn't open the link: {e}"))
    }
    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open")
            .arg(url)
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("couldn't open the link: {e}"))
    }
}

/// Open the system Bluetooth settings (offered when a connect fails, e.g. on
/// Windows where connecting must go through Settings).
pub fn open_bluetooth_settings() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg("x-apple.systempreferences:com.apple.BluetoothSettings")
            .spawn()
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
    #[cfg(target_os = "windows")]
    {
        crate::audio::windows::open_bluetooth_settings().map_err(|e| e.to_string())
    }
    #[cfg(target_os = "linux")]
    {
        crate::audio::linux::open_bluetooth_settings().map_err(|e| e.to_string())
    }
}
