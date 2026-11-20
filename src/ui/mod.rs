//! Duet's Dioxus UI. Pure logic lives in [`logic`] (unit-tested); [`app`] holds
//! the components; [`tray`] the menu bar / system tray integration.

pub mod app;
pub mod demo;
pub mod icons;
pub mod logic;
pub mod platform;
pub mod tray;

use crate::audio::{AudioBackend, BluetoothBackend};
use std::sync::Arc;

/// Backends shared with every component through context.
#[derive(Clone)]
pub struct Backends {
    pub audio: Arc<dyn AudioBackend>,
    pub bluetooth: Arc<dyn BluetoothBackend>,
}

/// How the window is presented.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowMode {
    /// Frameless, always-on-top panel toggled from the tray icon; hides on blur.
    Popover,
    /// Ordinary window (Linux, or `DUET_WINDOWED=1`). Closing hides it.
    Windowed,
}

pub const CSS: &str = include_str!("../../assets/main.css");
