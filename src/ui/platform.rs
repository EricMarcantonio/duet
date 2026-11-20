//! Per-OS hooks the UI calls but doesn't implement itself.

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
