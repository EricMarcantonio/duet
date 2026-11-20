//! Menu bar / system tray icon: template glyph, native menu, popover placement.

use super::logic::{Anchor, ScreenRect, TRAY_OPEN, TRAY_QUIT, popover_position};

/// Size (px) of the generated tray glyph. macOS draws it at 18pt, so 36px is @2x.
pub const ICON_SIZE: u32 = 36;

/// Monochrome headphones glyph as RGBA (black + alpha), suitable for a macOS
/// template image. Rendered from a tiny signed-distance description with 4x4
/// supersampling so edges are anti-aliased.
pub fn headphones_rgba(size: u32) -> Vec<u8> {
    let s = size as f64 / 24.0; // design space is 24x24
    let inside = |x: f64, y: f64| -> bool {
        let (x, y) = (x / s, y / s);
        // Headband: upper arc of a ring.
        let (cx, cy, r, half) = (12.0, 13.0, 8.4, 1.15);
        let d = ((x - cx).powi(2) + (y - cy).powi(2)).sqrt();
        let band = (d - r).abs() <= half && y <= cy + 2.5;
        // Ear cups: rounded rectangles.
        let cup = |ccx: f64| {
            let (hx, hy, rr) = (2.5, 4.2, 1.7);
            let qx = ((x - ccx).abs() - (hx - rr)).max(0.0);
            let qy = ((y - 17.2).abs() - (hy - rr)).max(0.0);
            (qx * qx + qy * qy).sqrt() <= rr
        };
        band || cup(4.6) || cup(19.4)
    };
    let mut out = Vec::with_capacity((size * size * 4) as usize);
    const SS: u32 = 4;
    for py in 0..size {
        for px in 0..size {
            let mut hits = 0;
            for sy in 0..SS {
                for sx in 0..SS {
                    let x = px as f64 + (sx as f64 + 0.5) / SS as f64;
                    let y = py as f64 + (sy as f64 + 0.5) / SS as f64;
                    hits += inside(x, y) as u32;
                }
            }
            let a = (hits * 255 / (SS * SS)) as u8;
            out.extend_from_slice(&[0, 0, 0, a]);
        }
    }
    out
}

/// Which side of the tray icon the popover opens on, per platform.
pub fn platform_anchor() -> Anchor {
    if cfg!(target_os = "windows") {
        Anchor::Above
    } else {
        Anchor::Below
    }
}

/// Whether this platform delivers tray click events (Linux AppIndicator does not).
pub fn supports_tray_clicks() -> bool {
    cfg!(any(target_os = "macos", target_os = "windows"))
}

#[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
pub use native::*;

#[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
mod native {
    use super::*;
    use dioxus::desktop::tao::dpi::{PhysicalPosition, PhysicalSize};
    use dioxus::desktop::tao::window::Window;
    use dioxus::desktop::trayicon::menu::{Menu, MenuItem, PredefinedMenuItem};
    use dioxus::desktop::trayicon::{Icon, Rect, TrayIcon, TrayIconBuilder};

    /// The tray menu: just "Open Duet" and "Quit Duet" — everything else lives in the window.
    pub fn build_menu() -> Menu {
        let menu = Menu::new();
        let _ = menu.append(&MenuItem::with_id(TRAY_OPEN, "Open Duet", true, None));
        let _ = menu.append(&PredefinedMenuItem::separator());
        let _ = menu.append(&MenuItem::with_id(TRAY_QUIT, "Quit Duet", true, None));
        menu
    }

    /// Create the tray icon. Left click is handled by us (popover) on macOS and
    /// Windows; on Linux the menu is the only entry point.
    pub fn create_tray() -> Result<TrayIcon, String> {
        let icon = Icon::from_rgba(headphones_rgba(ICON_SIZE), ICON_SIZE, ICON_SIZE)
            .map_err(|e| e.to_string())?;
        let mut builder = TrayIconBuilder::new()
            .with_id("duet")
            .with_icon(icon)
            .with_icon_as_template(true)
            .with_tooltip("Duet")
            .with_menu_on_left_click(!supports_tray_clicks());
        // Where clicks reach the app (macOS, Windows) the icon just toggles the panel:
        // no menu. Linux tray icons can't report clicks, so they get Open/Quit.
        if !supports_tray_clicks() {
            builder = builder.with_menu(Box::new(build_menu()));
        }
        builder.build().map_err(|e| e.to_string())
    }

    pub fn to_screen_rect(r: &Rect) -> ScreenRect {
        ScreenRect {
            x: r.position.x,
            y: r.position.y,
            w: r.size.width as f64,
            h: r.size.height as f64,
        }
    }

    /// Show `window` as a popover anchored to the tray icon `icon` (physical
    /// screen coords). Without an icon rect, place it at the top-right corner.
    pub fn show_popover(window: &Window, icon: Option<ScreenRect>) {
        place_popover(window, icon);
        bring_to_front(window);
    }

    /// Show, focus and raise `window` on the Space/desktop the user is looking at.
    pub fn bring_to_front(window: &Window) {
        #[cfg(target_os = "macos")]
        {
            follow_active_space(window);
            // Visible but on another Space: hide first so showing moves it here.
            if window.is_visible() && !on_active_space(window) {
                window.set_visible(false);
            }
        }
        window.set_minimized(false);
        window.set_visible(true);
        window.set_focus();
        #[cfg(target_os = "macos")]
        {
            activate_app();
            order_front_regardless(window);
        }
    }

    /// Make Duet the active app. macOS 14+ ignores the old
    /// `activateIgnoringOtherApps:` that tao uses, so call `-[NSApplication activate]`
    /// when it exists (it's allowed here: the user just clicked our icon or menu).
    #[cfg(target_os = "macos")]
    fn activate_app() {
        use objc2::msg_send;
        use objc2::runtime::{AnyClass, AnyObject, Bool};
        use objc2::sel;
        let Some(cls) = AnyClass::get(c"NSApplication") else {
            return;
        };
        // SAFETY: +sharedApplication returns the app object; -activate (macOS 14+) and
        // -activateIgnoringOtherApps:(BOOL) return void; checked with respondsToSelector.
        unsafe {
            let app: *mut AnyObject = msg_send![cls, sharedApplication];
            let Some(app) = app.as_ref() else { return };
            let has_new: Bool = msg_send![app, respondsToSelector: sel!(activate)];
            if has_new.as_bool() {
                let _: () = msg_send![app, activate];
            } else {
                let _: () = msg_send![app, activateIgnoringOtherApps: Bool::YES];
            }
        }
    }

    /// True if the window is on the Space the user is looking at.
    #[cfg(target_os = "macos")]
    pub fn on_active_space(window: &Window) -> bool {
        use dioxus::desktop::tao::platform::macos::WindowExtMacOS;
        use objc2::msg_send;
        use objc2::runtime::{AnyObject, Bool};
        let ns_window = window.ns_window() as *mut AnyObject;
        if ns_window.is_null() {
            return true;
        }
        // SAFETY: live NSWindow on the main thread; -isOnActiveSpace returns BOOL.
        let b: Bool = unsafe { msg_send![&*ns_window, isOnActiveSpace] };
        b.as_bool()
    }

    /// A menu bar app's panel must appear on the current Space (and over full-screen
    /// apps) instead of silently showing on whichever Space it was last on.
    #[cfg(target_os = "macos")]
    fn follow_active_space(window: &Window) {
        use dioxus::desktop::tao::platform::macos::WindowExtMacOS;
        use objc2::msg_send;
        use objc2::runtime::AnyObject;
        // NSWindowCollectionBehavior: MoveToActiveSpace = 1 << 1, FullScreenAuxiliary = 1 << 8.
        const MOVE_TO_ACTIVE_SPACE: usize = 1 << 1;
        const FULL_SCREEN_AUXILIARY: usize = 1 << 8;
        let ns_window = window.ns_window() as *mut AnyObject;
        if ns_window.is_null() {
            return;
        }
        // SAFETY: tao hands out its live NSWindow; we're on the main thread (UI event
        // handlers), and collectionBehavior is an NSUInteger property.
        unsafe {
            let w = &*ns_window;
            let current: usize = msg_send![w, collectionBehavior];
            let wanted = current | MOVE_TO_ACTIVE_SPACE | FULL_SCREEN_AUXILIARY;
            if wanted != current {
                let _: () = msg_send![w, setCollectionBehavior: wanted];
            }
        }
    }

    #[cfg(target_os = "macos")]
    fn order_front_regardless(window: &Window) {
        use dioxus::desktop::tao::platform::macos::WindowExtMacOS;
        use objc2::msg_send;
        use objc2::runtime::AnyObject;
        let ns_window = window.ns_window() as *mut AnyObject;
        if !ns_window.is_null() {
            // SAFETY: as above; -orderFrontRegardless takes no arguments.
            unsafe {
                let _: () = msg_send![&*ns_window, orderFrontRegardless];
            }
        }
    }

    /// Move `window` next to the tray icon without showing it.
    pub fn place_popover(window: &Window, icon: Option<ScreenRect>) {
        let size: PhysicalSize<u32> = window.outer_size();
        let (w, h) = (size.width as f64, size.height as f64);
        let point = icon.map(|r| (r.x + r.w / 2.0, r.y + r.h / 2.0));
        let monitor = point
            .and_then(|(px, py)| {
                window.available_monitors().find(|m| {
                    let (p, s) = (m.position(), m.size());
                    px >= p.x as f64
                        && px < (p.x + s.width as i32) as f64
                        && py >= p.y as f64
                        && py < (p.y + s.height as i32) as f64
                })
            })
            .or_else(|| window.primary_monitor())
            .or_else(|| window.current_monitor());
        if let Some(m) = monitor {
            let (p, s) = (m.position(), m.size());
            let screen = ScreenRect {
                x: p.x as f64,
                y: p.y as f64,
                w: s.width as f64,
                h: s.height as f64,
            };
            let margin = 6.0 * m.scale_factor();
            let icon = icon.unwrap_or(ScreenRect {
                x: screen.x + screen.w - w / 2.0 - margin,
                y: screen.y,
                w: 0.0,
                h: 24.0 * m.scale_factor(),
            });
            let (x, y) = popover_position(icon, w, h, screen, platform_anchor(), margin);
            window.set_outer_position(PhysicalPosition::new(x.round() as i32, y.round() as i32));
        }
    }

    /// Show a normal (non-popover) window and bring it forward.
    pub fn show_window(window: &Window) {
        bring_to_front(window);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glyph_has_ink_and_transparent_corners() {
        let px = headphones_rgba(ICON_SIZE);
        assert_eq!(px.len(), (ICON_SIZE * ICON_SIZE * 4) as usize);
        let alpha = |x: u32, y: u32| px[((y * ICON_SIZE + x) * 4 + 3) as usize];
        assert_eq!(alpha(0, 0), 0);
        assert_eq!(alpha(ICON_SIZE / 2, ICON_SIZE / 2), 0, "centre is empty");
        assert_eq!(alpha(7, 26), 255, "left ear cup is solid");
        assert_eq!(alpha(ICON_SIZE - 8, 26), 255, "right ear cup is solid");
        assert!(
            px.chunks(4).all(|p| p[0] == 0 && p[1] == 0 && p[2] == 0),
            "template = black only"
        );
    }
}
