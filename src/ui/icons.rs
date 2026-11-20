//! Inline SVG glyphs (no external resources). Stroke-based, `currentColor`.

macro_rules! svg {
    ($body:expr) => {
        concat!(
            r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">"#,
            $body,
            "</svg>"
        )
    };
}

pub const AIRPODS: &str = svg!(
    r#"<circle cx="7" cy="7.5" r="3.6" fill="currentColor" fill-opacity=".14"/><path d="M8.9 10.6V19" stroke-width="2.6"/><circle cx="17" cy="7.5" r="3.6" fill="currentColor" fill-opacity=".14"/><path d="M15.1 10.6V19" stroke-width="2.6"/>"#
);
pub const HEADPHONES: &str = svg!(
    r#"<path d="M4 15v-3a8 8 0 0 1 16 0v3"/><rect x="3" y="14" width="4.5" height="6.5" rx="1.6" fill="currentColor" fill-opacity=".14"/><rect x="16.5" y="14" width="4.5" height="6.5" rx="1.6" fill="currentColor" fill-opacity=".14"/>"#
);
pub const LAPTOP: &str = svg!(
    r#"<rect x="4" y="5" width="16" height="11" rx="1.6" fill="currentColor" fill-opacity=".1"/><path d="M2 19.5h20"/>"#
);
pub const WAVEFORM: &str = svg!(r#"<path d="M4 10v4M8 7v10M12 4v16M16 7v10M20 10v4"/>"#);
pub const STACK: &str = svg!(
    r#"<rect x="3" y="4" width="13" height="9" rx="2"/><rect x="8" y="11" width="13" height="9" rx="2" fill="currentColor" fill-opacity=".14"/>"#
);
pub const TOGETHER: &str = svg!(
    r#"<circle cx="9" cy="8" r="3.2"/><path d="M3 19.5a6 6 0 0 1 12 0"/><circle cx="16.8" cy="9" r="2.6"/><path d="M15.6 14.1a5 5 0 0 1 5.9 5.4"/>"#
);
pub const BLUETOOTH: &str = svg!(r#"<path d="M7 7.5l10 9-5 4.5V3l5 4.5-10 9"/>"#);
pub const SPEAKER_LOW: &str = svg!(
    r#"<path d="M11 5 6.5 9H3.5v6h3l4.5 4V5z" fill="currentColor" fill-opacity=".14"/><path d="M15 9.5a3.5 3.5 0 0 1 0 5"/>"#
);
pub const SPEAKER_HIGH: &str = svg!(
    r#"<path d="M11 5 6.5 9H3.5v6h3l4.5 4V5z" fill="currentColor" fill-opacity=".14"/><path d="M15 9.5a3.5 3.5 0 0 1 0 5M18 7a7 7 0 0 1 0 10"/>"#
);
pub const SPEAKER_MUTE: &str = svg!(
    r#"<path d="M11 5 6.5 9H3.5v6h3l4.5 4V5z" fill="currentColor" fill-opacity=".14"/><path d="M16 9.5l5 5M21 9.5l-5 5"/>"#
);
pub const CHEVRON: &str = svg!(r#"<path d="M9 6l6 6-6 6"/>"#);
pub const CHECK: &str = svg!(r#"<path d="M5 12.5l4.5 4.5L19 7.5"/>"#);
pub const CLOSE: &str = svg!(r#"<path d="M7 7l10 10M17 7 7 17"/>"#);
pub const WARNING: &str =
    svg!(r#"<path d="M12 4 2.8 19.5h18.4L12 4z"/><path d="M12 10v4.2M12 17v.2"/>"#);
/// Noise control: off (plain ear).
pub const NC_OFF: &str = svg!(
    r#"<path d="M8 9.5a4 4 0 0 1 8 0c0 2.6-2.4 3.4-2.9 5.4-.4 1.7-1.3 3.1-3.1 3.1"/><path d="M10.5 10a1.6 1.6 0 0 1 3 .4"/>"#
);
/// Transparency: sound passing through (open circle with rays).
pub const NC_TRANSPARENCY: &str = svg!(
    r#"<circle cx="12" cy="12" r="3.4" fill="currentColor" fill-opacity=".14"/><path d="M12 3.5v2.2M12 18.3v2.2M3.5 12h2.2M18.3 12h2.2M6 6l1.5 1.5M16.5 16.5 18 18M6 18l1.5-1.5M16.5 7.5 18 6"/>"#
);
/// Adaptive: sparkle.
pub const NC_ADAPTIVE: &str = svg!(
    r#"<path d="M12 3.8l1.9 5.1 5.1 1.9-5.1 1.9-1.9 5.1-1.9-5.1-5.1-1.9 5.1-1.9z" fill="currentColor" fill-opacity=".14"/><path d="M18.5 15.5l.7 1.8 1.8.7-1.8.7-.7 1.8-.7-1.8-1.8-.7 1.8-.7z"/>"#
);
/// Noise cancellation: concentric sealed rings.
pub const NC_ANC: &str = svg!(
    r#"<circle cx="12" cy="12" r="8" /><circle cx="12" cy="12" r="4.4" fill="currentColor" fill-opacity=".22"/>"#
);

pub fn for_mode(m: crate::audio::ListeningMode) -> &'static str {
    use crate::audio::ListeningMode::*;
    match m {
        Off => NC_OFF,
        Transparency => NC_TRANSPARENCY,
        Adaptive => NC_ADAPTIVE,
        NoiseCancellation => NC_ANC,
    }
}

pub const PLUS: &str = svg!(r#"<path d="M12 5v14M5 12h14"/>"#);

/// Glyph for an output device.
pub fn for_device(transport: crate::audio::Transport, name: &str) -> &'static str {
    use crate::audio::Transport::*;
    match transport {
        Bluetooth if name.to_lowercase().contains("airpod") => AIRPODS,
        Bluetooth => HEADPHONES,
        BuiltIn => LAPTOP,
        Virtual => WAVEFORM,
        Aggregate => STACK,
        Other => SPEAKER_HIGH,
    }
}
