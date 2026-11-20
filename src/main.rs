// Release builds on Windows: no console window.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::sync::Arc;

use dioxus::desktop::{Config as DesktopConfig, LogicalSize, WindowBuilder, WindowCloseBehaviour};
use duet::config::Config;
use duet::ui::app::{App, AppOptions};
use duet::ui::{Backends, CSS, WindowMode, demo};

/// Native backends for the current OS.
fn real_backends() -> Backends {
    #[cfg(target_os = "macos")]
    {
        use duet::audio::{bluetooth::MacBluetooth, coreaudio::CoreAudio};
        Backends {
            audio: Arc::new(CoreAudio),
            bluetooth: Arc::new(MacBluetooth),
        }
    }
    #[cfg(target_os = "windows")]
    {
        use duet::audio::windows::{WindowsAudio, WindowsBluetooth};
        Backends {
            audio: Arc::new(WindowsAudio::new()),
            bluetooth: Arc::new(WindowsBluetooth::new()),
        }
    }
    #[cfg(target_os = "linux")]
    {
        use duet::audio::linux::{LinuxAudio, LinuxBluetooth};
        Backends {
            audio: Arc::new(LinuxAudio::new()),
            bluetooth: Arc::new(LinuxBluetooth::new()),
        }
    }
}

fn env_flag(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .filter(|v| !v.is_empty() && v != "0")
}

fn main() {
    let mock = env_flag("DUET_MOCK");
    let backends = match &mock {
        Some(variant) => demo::mock_backends(variant),
        None => real_backends(),
    };

    // Linux AppIndicator delivers no click events, so a popover can't be toggled there.
    let mode = if cfg!(target_os = "linux") || env_flag("DUET_WINDOWED").is_some() {
        WindowMode::Windowed
    } else {
        WindowMode::Popover
    };

    let config_path = match (std::env::var_os("DUET_CONFIG"), &mock) {
        (Some(p), _) => p.into(),
        // Keep mock sessions from touching the real nickname file.
        (None, Some(_)) => std::env::temp_dir().join("duet-mock-config.json"),
        (None, None) => Config::default_path(),
    };

    let mut window = WindowBuilder::new()
        .with_title("Duet")
        .with_inner_size(LogicalSize::new(400.0, 600.0))
        .with_min_inner_size(LogicalSize::new(360.0, 440.0));

    window = match mode {
        WindowMode::Popover => window
            .with_decorations(false)
            .with_transparent(true)
            .with_resizable(false)
            .with_visible(false),
        WindowMode::Windowed => window.with_resizable(true),
    };

    #[cfg(target_os = "macos")]
    {
        use dioxus::desktop::tao::platform::macos::WindowBuilderExtMacOS;
        window = match mode {
            WindowMode::Popover => window.with_has_shadow(true),
            WindowMode::Windowed => window
                .with_titlebar_transparent(true)
                .with_fullsize_content_view(true)
                .with_title_hidden(true),
        };
    }
    #[cfg(target_os = "windows")]
    {
        use dioxus::desktop::tao::platform::windows::WindowBuilderExtWindows;
        if mode == WindowMode::Popover {
            window = window.with_skip_taskbar(true);
        }
    }

    let mut cfg = DesktopConfig::new()
        .with_window(window)
        .with_custom_head(format!("<style>{CSS}</style>"))
        .with_close_behaviour(WindowCloseBehaviour::WindowHides)
        .with_tray_icon_show_window_on_click(false)
        .with_disable_context_menu(!cfg!(debug_assertions));
    if mode == WindowMode::Popover {
        cfg = cfg.with_menu(None).with_background_color((0, 0, 0, 0));
    }

    dioxus::LaunchBuilder::desktop()
        .with_cfg(cfg)
        .with_context(backends)
        .with_context(AppOptions { mode, config_path })
        .launch(App);
}
