//! Duet's homepage: one static page, written in Dioxus and compiled to WebAssembly.
//!
//! Build: `dx bundle --platform web --release` (see README.md in this folder).

use dioxus::prelude::*;

const REPO: &str = "https://github.com/EricMarcantonio/duet";
const DL_MAC: &str =
    "https://github.com/EricMarcantonio/duet/releases/latest/download/Duet-macOS.zip";
const DL_WIN: &str =
    "https://github.com/EricMarcantonio/duet/releases/latest/download/Duet-Windows.zip";
const DL_LINUX: &str =
    "https://github.com/EricMarcantonio/duet/releases/latest/download/Duet-Linux.tar.gz";
const RELEASES: &str = "https://github.com/EricMarcantonio/duet/releases/latest";

const STYLE: Asset = asset!("/assets/style.css");
const ICON: Asset = asset!("/assets/icon-256.png");
const FAVICON: Asset = asset!("/assets/favicon.png");
const TOUCH_ICON: Asset = asset!("/assets/apple-touch-icon.png");
const SHOT_LIGHT: Asset = asset!("/assets/screenshot-light.png");
const SHOT_DARK: Asset = asset!("/assets/screenshot-dark.png");

fn main() {
    dioxus::launch(App);
}

#[derive(Clone, Copy, PartialEq)]
enum Os {
    Mac,
    Windows,
    Linux,
    Other,
}

fn detect_os(ua: &str) -> Os {
    let ua = ua.to_ascii_lowercase();
    if ua.contains("windows") {
        Os::Windows
    } else if ua.contains("android") || ua.contains("iphone") || ua.contains("ipad") {
        Os::Other
    } else if ua.contains("mac os") || ua.contains("macintosh") {
        Os::Mac
    } else if ua.contains("linux") || ua.contains("x11") {
        Os::Linux
    } else {
        Os::Other
    }
}

#[component]
fn App() -> Element {
    // Which download to point out first. Everything still renders without it.
    let os = use_resource(|| async {
        document::eval("return navigator.userAgent")
            .join::<String>()
            .await
            .map(|ua| detect_os(&ua))
            .unwrap_or(Os::Other)
    });
    let os = os().unwrap_or(Os::Mac);

    rsx! {
        document::Title { "Duet: two pairs of headphones, one computer" }
        document::Meta {
            name: "description",
            content: "Duet plays your computer's audio on two pairs of Bluetooth headphones at once, each with its own volume. Free and open source for macOS.",
        }
        document::Meta { name: "color-scheme", content: "light dark" }
        document::Link { rel: "icon", r#type: "image/png", href: FAVICON }
        document::Link { rel: "apple-touch-icon", href: TOUCH_ICON }
        document::Stylesheet { href: STYLE }

        Nav {}
        main {
            Hero { os }
            Features {}
            HowItWorks {}
            Install {}
        }
        Footer {}
    }
}

#[component]
fn Nav() -> Element {
    rsx! {
        header { class: "nav",
            div { class: "wrap nav-inner",
                a { class: "brand", href: "#top",
                    img {
                        src: ICON,
                        alt: "",
                        width: "28",
                        height: "28",
                    }
                    span { "Duet" }
                }
                nav { class: "nav-links",
                    a { href: "#features", "Features" }
                    a { href: "#how", "How it works" }
                    a { href: "#install", "Install" }
                    a { class: "nav-gh", href: REPO,
                        GitHubIcon {}
                        span { "GitHub" }
                    }
                }
            }
        }
    }
}

#[component]
fn Hero(os: Os) -> Element {
    let other_note = match os {
        Os::Windows => Some(
            "You're on Windows: the Windows build is experimental and hasn't been tested on real hardware yet.",
        ),
        Os::Linux => Some(
            "You're on Linux: the Linux build is experimental and hasn't been tested on real hardware yet.",
        ),
        _ => None,
    };
    rsx! {
        section { class: "hero", id: "top",
            div { class: "wrap hero-grid",
                div { class: "hero-copy",
                    img {
                        class: "app-icon",
                        src: ICON,
                        alt: "Duet app icon",
                        width: "112",
                        height: "112",
                    }
                    h1 { "Duet" }
                    p { class: "tagline",
                        "Two pairs of headphones. One computer. "
                        span { class: "accent", "Separate volumes." }
                    }
                    p { class: "lede",
                        "Duet plays your Mac's audio on two pairs of AirPods (or any Bluetooth headphones) at the same time, "
                        "so you can watch a movie on a plane or a show in bed without sharing an earbud."
                    }
                    div { class: "cta",
                        a { class: "btn btn-primary", href: DL_MAC,
                            AppleIcon {}
                            span { "Download for macOS" }
                        }
                        a { class: "btn btn-ghost", href: REPO,
                            GitHubIcon {}
                            span { "View on GitHub" }
                        }
                    }
                    p { class: "fine", "Free and open source · macOS 11+ · Apple Silicon & Intel" }
                    p { class: "fine alt-dl",
                        "Also for "
                        a { href: DL_WIN, "Windows" }
                        " and "
                        a { href: DL_LINUX, "Linux" }
                        span { class: "pill", "experimental" }
                    }
                    if let Some(note) = other_note {
                        p { class: "os-note", "{note}" }
                    }
                }
                div { class: "hero-shot",
                    picture {
                        source {
                            "srcset": SHOT_DARK,
                            "media": "(prefers-color-scheme: dark)",
                        }
                        img {
                            src: SHOT_LIGHT,
                            alt: "The Duet panel: two listeners, Alex and Sam, each with a volume slider, battery levels and noise control, listening together.",
                            width: "400",
                            height: "760",
                        }
                    }
                }
            }
        }
    }
}

struct Feature {
    icon: fn() -> Element,
    title: &'static str,
    body: &'static str,
}

#[component]
fn Features() -> Element {
    let features = [
        Feature {
            icon: || rsx! {
                PeopleIcon {}
            },
            title: "Listen together",
            body: "One click sends everything you hear to both pairs at once, through a single output called “Duet”.",
        },
        Feature {
            icon: || rsx! {
                SliderIcon {}
            },
            title: "A volume for each pair",
            body: "Each listener gets their own slider, mute and presets, so nobody has to settle for the other's volume.",
        },
        Feature {
            icon: || rsx! {
                WaveIcon {}
            },
            title: "Noise control & battery",
            body: "Switch AirPods between Off, Transparency, Adaptive and Noise Cancellation, and see left, right and case battery.",
        },
        Feature {
            icon: || rsx! {
                MenuBarIcon {}
            },
            title: "Lives in the menu bar",
            body: "No Dock icon, no window to manage. Click the headphones icon to show or hide the panel.",
        },
        Feature {
            icon: || rsx! {
                OfflineIcon {}
            },
            title: "Works offline",
            body: "No account, no cloud, no tracking. Duet talks only to your Mac's audio and Bluetooth.",
        },
        Feature {
            icon: || rsx! {
                CodeIcon {}
            },
            title: "Open source",
            body: "One small Rust app under the MIT license. Read the code, file an issue, or send a fix.",
        },
    ];
    rsx! {
        section { class: "section", id: "features",
            div { class: "wrap",
                h2 { "Everything you need, nothing you don't" }
                div { class: "grid features",
                    for f in features {
                        article { class: "card feature", key: "{f.title}",
                            div { class: "feature-icon", {(f.icon)()} }
                            h3 { "{f.title}" }
                            p { "{f.body}" }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn HowItWorks() -> Element {
    rsx! {
        section { class: "section", id: "how",
            div { class: "wrap",
                h2 { "How it works" }
                p { class: "section-lede",
                    "Duet doesn't install drivers or extensions. It uses what each system already has:"
                }
                div { class: "grid how",
                    article { class: "card how-card",
                        h3 { "macOS" }
                        p {
                            "Duet builds one CoreAudio "
                            strong { "Multi-Output device" }
                            " named “Duet” from the headphones you choose, then keeps it selected even when AirPods reconnect and try to take over."
                        }
                    }
                    article { class: "card how-card",
                        h3 {
                            "Windows "
                            span { class: "pill", "experimental" }
                        }
                        p {
                            "Windows has no multi-output device, so Duet runs its own "
                            strong { "audio mirror" }
                            ": it captures the first pair with WASAPI loopback and plays it on the second (about 20–60 ms behind)."
                        }
                    }
                    article { class: "card how-card",
                        h3 {
                            "Linux "
                            span { class: "pill", "experimental" }
                        }
                        p {
                            "Duet loads a "
                            strong { "PipeWire / PulseAudio" }
                            " combine sink over your Bluetooth outputs and connects headphones with "
                            code { "bluetoothctl" }
                            "."
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn Install() -> Element {
    rsx! {
        section { class: "section", id: "install",
            div { class: "wrap install-grid",
                div { class: "card install-card",
                    h2 { "Opening Duet the first time" }
                    p {
                        "Duet isn't notarized by Apple yet, so macOS asks before the first launch. "
                        "Unzip it, move "
                        strong { "Duet.app" }
                        " to Applications, then:"
                    }
                    ol { class: "steps",
                        li {
                            strong { "Right-click" }
                            " (or Control-click) Duet.app and choose "
                            strong { "Open" }
                            ", then "
                            strong { "Open" }
                            " again, "
                            em { "or" }
                        }
                        li {
                            "try to open it once, then go to "
                            strong { "System Settings › Privacy & Security" }
                            " and click "
                            strong { "Open Anyway" }
                            "."
                        }
                    }
                    p { class: "muted",
                        "Allow Bluetooth access when asked so Duet can list and connect your headphones. "
                        "To start Duet at login, add it under System Settings › General › Login Items."
                    }
                }
                div { class: "card install-card",
                    h2 { "Requirements" }
                    ul { class: "reqs",
                        li {
                            CheckIcon {}
                            span { "macOS 11 Big Sur or later" }
                        }
                        li {
                            CheckIcon {}
                            span { "Apple Silicon or Intel (one universal app)" }
                        }
                        li {
                            CheckIcon {}
                            span { "Two pairs of Bluetooth headphones, paired with this Mac" }
                        }
                        li {
                            CheckIcon {}
                            span { "Noise control and battery: AirPods and Beats" }
                        }
                    }
                    div { class: "cta",
                        a { class: "btn btn-primary", href: DL_MAC,
                            AppleIcon {}
                            span { "Download for macOS" }
                        }
                    }
                    p { class: "fine",
                        a { href: RELEASES, "All downloads and release notes" }
                    }
                }
            }
        }
    }
}

#[component]
fn Footer() -> Element {
    rsx! {
        footer { class: "footer",
            div { class: "wrap footer-inner",
                span { "Duet is free software under the MIT license." }
                span { class: "footer-links",
                    a { href: REPO, "Source" }
                    a { href: "{REPO}/issues", "Report an issue" }
                    a { href: RELEASES, "Releases" }
                }
            }
            div { class: "wrap footer-small",
                "AirPods and macOS are trademarks of Apple Inc. Duet is not affiliated with Apple."
            }
        }
    }
}

// --- Icons (inline SVG, no external requests) ---------------------------------

#[component]
fn AppleIcon() -> Element {
    rsx! {
        svg {
            class: "i",
            view_box: "0 0 24 24",
            "aria-hidden": "true",
            fill: "currentColor",
            path { d: "M16.37 12.6c-.02-2.3 1.88-3.4 1.96-3.46-1.07-1.56-2.73-1.78-3.32-1.8-1.41-.14-2.76.83-3.47.83-.72 0-1.82-.81-3-.79-1.54.02-2.96.9-3.76 2.27-1.6 2.78-.41 6.9 1.15 9.16.76 1.1 1.67 2.34 2.86 2.3 1.15-.05 1.58-.74 2.97-.74 1.38 0 1.77.74 2.98.72 1.23-.02 2.01-1.12 2.76-2.23.87-1.28 1.23-2.52 1.25-2.58-.03-.01-2.4-.92-2.42-3.66zM14.1 5.86c.63-.77 1.06-1.83.94-2.89-.91.04-2.01.61-2.66 1.37-.58.67-1.09 1.75-.96 2.79 1.02.08 2.05-.52 2.68-1.27z" }
        }
    }
}

#[component]
fn GitHubIcon() -> Element {
    rsx! {
        svg {
            class: "i",
            view_box: "0 0 24 24",
            "aria-hidden": "true",
            fill: "currentColor",
            path { d: "M12 2C6.48 2 2 6.58 2 12.25c0 4.53 2.87 8.37 6.84 9.73.5.1.68-.22.68-.49 0-.24-.01-.88-.01-1.73-2.78.62-3.37-1.37-3.37-1.37-.46-1.18-1.11-1.5-1.11-1.5-.91-.64.07-.62.07-.62 1 .07 1.53 1.06 1.53 1.06.89 1.57 2.34 1.12 2.91.85.09-.66.35-1.12.63-1.37-2.22-.26-4.56-1.14-4.56-5.07 0-1.12.39-2.04 1.03-2.76-.1-.26-.45-1.3.1-2.71 0 0 .84-.28 2.75 1.05A9.4 9.4 0 0 1 12 6.96c.85 0 1.7.12 2.5.34 1.91-1.33 2.75-1.05 2.75-1.05.55 1.41.2 2.45.1 2.71.64.72 1.03 1.64 1.03 2.76 0 3.94-2.34 4.81-4.57 5.06.36.32.68.94.68 1.9 0 1.37-.01 2.47-.01 2.81 0 .27.18.6.69.49A10.2 10.2 0 0 0 22 12.25C22 6.58 17.52 2 12 2z" }
        }
    }
}

/// Stroke icon wrapper shared by the feature icons.
#[component]
fn Stroke(children: Element) -> Element {
    rsx! {
        svg {
            class: "i",
            view_box: "0 0 24 24",
            "aria-hidden": "true",
            fill: "none",
            stroke: "currentColor",
            stroke_width: "1.8",
            stroke_linecap: "round",
            stroke_linejoin: "round",
            {children}
        }
    }
}

#[component]
fn PeopleIcon() -> Element {
    rsx! {
        Stroke {
            circle { cx: "9", cy: "8", r: "3.2" }
            path { d: "M3 19c.6-3.2 3-5 6-5s5.4 1.8 6 5" }
            circle { cx: "17", cy: "9", r: "2.6" }
            path { d: "M16.5 14.1c2.4.2 4 1.7 4.5 4.4" }
        }
    }
}

#[component]
fn SliderIcon() -> Element {
    rsx! {
        Stroke {
            path { d: "M4 7h9M17 7h3M4 17h3M11 17h9" }
            circle { cx: "15", cy: "7", r: "2" }
            circle { cx: "9", cy: "17", r: "2" }
        }
    }
}

#[component]
fn WaveIcon() -> Element {
    rsx! {
        Stroke {
            circle { cx: "12", cy: "12", r: "3" }
            path { d: "M7.5 7.5a6.4 6.4 0 0 0 0 9M16.5 7.5a6.4 6.4 0 0 1 0 9" }
            path { d: "M4.6 4.6a10.5 10.5 0 0 0 0 14.8M19.4 4.6a10.5 10.5 0 0 1 0 14.8" }
        }
    }
}

#[component]
fn MenuBarIcon() -> Element {
    rsx! {
        Stroke {
            rect {
                x: "3",
                y: "4",
                width: "18",
                height: "16",
                rx: "3",
            }
            path { d: "M3 8.5h18" }
            path { d: "M14 13h4M14 16h2.5" }
        }
    }
}

#[component]
fn OfflineIcon() -> Element {
    rsx! {
        Stroke {
            path { d: "M12 3l7 3v5.5c0 4.4-3 8-7 9.5-4-1.5-7-5.1-7-9.5V6z" }
            path { d: "M9 12l2 2 4-4" }
        }
    }
}

#[component]
fn CodeIcon() -> Element {
    rsx! {
        Stroke {
            path { d: "M8.5 7L3.5 12l5 5M15.5 7l5 5-5 5M13.5 4.5l-3 15" }
        }
    }
}

#[component]
fn CheckIcon() -> Element {
    rsx! {
        Stroke {
            path { d: "M5 12.5l4.5 4.5L19 7.5" }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_platforms() {
        let mac = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15";
        let win = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36";
        let linux = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36";
        let ios = "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X)";
        let android = "Mozilla/5.0 (Linux; Android 14; Pixel 8)";
        assert!(detect_os(mac) == Os::Mac);
        assert!(detect_os(win) == Os::Windows);
        assert!(detect_os(linux) == Os::Linux);
        assert!(detect_os(ios) == Os::Other);
        assert!(detect_os(android) == Os::Other);
    }
}
