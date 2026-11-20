# Duet

**Two pairs of headphones, one computer, separate volumes.**

Duet plays your computer's audio on two pairs of AirPods (or any Bluetooth headphones)
at once, with a separate volume for each pair, plus AirPods battery levels.
It lives in the macOS menu bar (Windows system tray / Linux tray icon on the experimental
builds), works fully offline, and is a single small Rust app.

**Homepage:** <https://ericmarcantonio.github.io/duet/> (Rust + WebAssembly, in [`site/`](site/)).

<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/screenshot-dark.png">
    <img src="docs/screenshot-light.png" alt="The Duet panel with two listeners, each with its own volume slider and battery levels" width="360">
  </picture>
</p>

## Download

- **macOS 11+** (Apple Silicon & Intel): [Duet-macOS.zip](https://github.com/EricMarcantonio/duet/releases/latest/download/Duet-macOS.zip)
- Windows 10/11, experimental: [Duet-Windows.zip](https://github.com/EricMarcantonio/duet/releases/latest/download/Duet-Windows.zip)
- Linux, experimental: [Duet-Linux.tar.gz](https://github.com/EricMarcantonio/duet/releases/latest/download/Duet-Linux.tar.gz)

The macOS app isn't notarized, so the first launch needs right-click › Open (or System
Settings › Privacy & Security › Open Anyway). All builds: [Releases](https://github.com/EricMarcantonio/duet/releases).

## Build from source (macOS)

```sh
./scripts/bundle-macos.sh --install   # universal (Apple Silicon + Intel), copies to ~/Applications
./scripts/bundle-macos.sh --zip       # dist/Duet-macOS.zip to send to someone
open ~/Applications/Duet.app
```

Sharing: the zip is ad-hoc signed, so on another Mac the first launch needs right-click › Open
(or System Settings › Privacy & Security › Open Anyway). With an Apple Developer ID, set
`SIGN_IDENTITY` and `NOTARY_PROFILE` and the script signs and notarizes it so it opens normally.

The first time it runs, allow Bluetooth access when asked. If you miss the prompt,
go to System Settings › Privacy & Security › Bluetooth. To start Duet at login, add it
in System Settings › General › Login Items.

## Use

- **Click the menu bar headphones icon** to show or hide the panel (Quit is at the bottom of the panel; on Linux the tray menu has Open Duet / Quit).
  Drag its header to move it; close it with × or Esc. Opening Duet.app again also shows it.
- **Choose headphones…** lists every paired Bluetooth audio device. Tick any two (or more):
  Duet connects them and they become listeners. Until you choose, Duet uses whatever
  Bluetooth headphones are connected.
- **Listen together** (or **Connect & listen together**) connects any chosen headphones that
  aren't connected, then plays through one output called **"Duet"** (the only thing Duet adds to
  the system Sound menu). Changing your headphones updates that same output; Duet never
  touches Multi-Output devices you made yourself in Audio MIDI Setup. While you're listening
  together, if macOS switches the output to a pair that just connected (it does that for
  AirPods), Duet switches it back so nobody drops out. Pick another output in Duet (or the
  Sound menu) and it stops.
- **Each listener card**: its own volume slider (the Mac's volume keys don't work on
  Multi-Output devices), Mute and presets, and battery (left/right/case). Click a name to rename it.
- **Noise control** is switchable inside Duet only where the OS allows it (Linux, over Apple's
  AAP protocol). On macOS, Apple keeps that Bluetooth channel to itself — the card says
  "Noise control: Control Center › Sound" and you switch Off / Transparency / Adaptive /
  Noise Cancellation there. Windows exposes no API for it at all.
- **Other outputs** lets you go back to just the speakers or one pair.

While listening together Duet also moves the default **microphone** off the AirPods to the
built-in mic: an AirPods mic forces call-quality mode, which keeps knocking that pair out of the
Duet output and makes macOS flip the output between Duet and one pair.

**Log:** `~/Library/Logs/Duet/duet.log` (Windows `%LOCALAPPDATA%\Duet\duet.log`, Linux
`~/.local/share/Duet/duet.log`) records output switches, devices coming and going, connects,
and every time Duet re-selects its output.

Settings (nicknames, chosen headphones) live in `~/Library/Application Support/Duet/config.json`
(`%APPDATA%\Duet` on Windows, `~/.config/Duet` on Linux).

### What it uses from each OS

Duet is one Rust binary; the window is drawn by the OS's own web view (WebKit on macOS,
WebView2 on Windows, WebKitGTK on Linux), and everything else goes through system APIs:

| | Audio | Bluetooth | Noise control & battery |
|---|---|---|---|
| macOS | CoreAudio (Multi-Output device) | IOBluetooth | battery readable; noise control only in Control Center (macOS keeps that channel) |
| Windows | WASAPI + Duet's own audio mirror | WinRT Bluetooth (list only) | none (Windows exposes no API for it) |
| Linux | PulseAudio/PipeWire `module-combine-sink` | `bluetoothctl` | Apple's AAP protocol over L2CAP (as in LibrePods) — experimental |

## Platforms

| | Tray | Two pairs at once | Connect headphones |
|---|---|---|---|
| macOS | Menu bar (no Dock icon) | CoreAudio Multi-Output device | IOBluetooth |
| Windows 10/11 | Notification area | Duet mirrors audio in software (WASAPI loopback); the second pair runs ~20–60 ms behind | Opens Bluetooth settings (no public API) |
| Linux | StatusNotifierItem / AppIndicator | PulseAudio / PipeWire `module-combine-sink` | `bluetoothctl` |

Windows and Linux backends compile and have parser/unit tests, but **have not been run on real
hardware yet**. Linux needs `pactl` (with `pipewire-pulse` on PipeWire systems), `bluetoothctl`,
and the WebKitGTK/GTK + libayatana-appindicator dev packages to build. Build those with
`dx bundle --release --platform desktop` on the target OS.

## Develop

```sh
cargo test                       # unit + live CoreAudio tests (read-only except a speaker volume round-trip it restores)
cargo test -- --ignored          # also create/destroy a temp multi-output and switch outputs (restored)
DUET_MOCK=1 cargo run            # fake devices; variants: full, new, nomulti, flaky
swift scripts/make-icon.swift    # regenerate assets/icon/* (app icon, .icns, .ico)
DUET_WINDOWED=1 cargo run        # normal window instead of the menu bar popover
cargo run --example probe        # print output devices + paired Bluetooth devices
```

Code layout: `src/audio/` (shared model + per-OS backends + mocks), `src/ui/` (Dioxus
components, tray, pure logic in `logic.rs`), `src/config.rs`, `assets/main.css`.
The homepage is a separate Dioxus web (WebAssembly) crate in [`site/`](site/README.md).

Releases: pushing a `v*` tag runs `.github/workflows/release.yml`, which builds the macOS
universal app, Windows and Linux, and attaches `Duet-macOS.zip`, `Duet-Windows.zip` and
`Duet-Linux.tar.gz` to the GitHub release.

## License

[MIT](LICENSE)
