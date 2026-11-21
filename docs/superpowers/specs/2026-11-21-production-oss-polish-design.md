# Duet: production-grade open-source polish

## Intent and why

Duet works, but it reads like a personal project: the menu bar shows a generic
single-headphones glyph rather than the two-headphones mark that *is* the Duet
logo, the README has no badges, no demo and one download link, there is no way to
learn that a newer release exists, and there is no Homebrew path at all. The goal
is to make the repo and the app look and behave like a maintained product without
changing what Duet does.

Five workstreams, independent except that the README needs the demo file and the
badges need the repo to already have CI and releases (it has both).

## Goals

1. The macOS menu bar icon is the Duet mark — two headphones, the back one
   smaller and tinted — not a single pair.
2. The README opens with live status (CI, latest release, downloads, license,
   platforms), a demo animation, and a download section with honest ordering:
   Homebrew cask, direct download, build from source.
3. A Homebrew cask file lives in the repo (`packaging/homebrew/duet.rb`),
   valid, lintable, with a script that regenerates `version`/`sha256` from a
   release tag.
4. The app has a working **Check for updates** control that asks GitHub for the
   latest release, compares it to the running version and either says "up to
   date", offers the download, or — when Duet was installed as a cask — shows
   `brew upgrade --cask duet` with a copy button.
5. The README shows a VHS-graded animation of the real app being used.

## Non-goals

- Notarization, code signing, or silent self-install of updates. The app is
  ad-hoc signed; a silent updater would be broken by Gatekeeper.
- Creating the Homebrew tap repository, or submitting to `homebrew/homebrew-cask`.
  The cask file and its publishing instructions land in this repo; publishing is
  a manual step the owner does later.
- Recording the demo in CI, or recording it on Windows/Linux.
- Rewriting the website (`site/`) beyond nothing — the homepage keeps its current
  install copy. Follow-up, not this work.
- Any change to audio, Bluetooth, or grouping behaviour.

## Anti-goals

- A fake "terminal session" VHS that types a command and pretends to be the app.
- A badge or table row that implies Windows/Linux are verified on hardware; they
  are not, and the README already says so.
- A colored logo in the menu bar. macOS template images must be monochrome
  black+alpha with transparency; the blue tile would look broken in dark mode.
- New runtime crates. The project is deliberately dependency-light.

## Constraints

- Rust edition 2024, stable toolchain; `src/ui/app.rs` and neighbours already use
  Dioxus 0.7 signals.
- No new Cargo dependencies. `serde_json` and `dirs` are already present; HTTPS is
  done with `curl` through `std::process::Command`.
- Shared modules must still compile for Windows and Linux targets; anything
  OS-specific sits behind `cfg`.
- `cargo test --lib --bins --examples`, `cargo clippy --all-targets` and
  `cargo fmt --all` must be clean — that is exactly what CI runs.
- Menu bar icon: 36 px RGBA, black pixels + alpha only (`tray::ICON_SIZE`).
- Version string has one source of truth: `version` in `Cargo.toml`
  (`env!("CARGO_PKG_VERSION")`).
- Committed demo artifacts stay small: GIF ≤ 8 MB, MP4 ≤ 6 MB.
- README copy stays truthful: an unpublished tap must not be presented as a
  working `brew install --cask duet`.

## Approaches considered

**Menu bar icon.**
(a) Ship the 1024 px tile as a menu bar image — rejected: color, wrong size,
macOS draws template images by alpha only and it would render as a blob.
(b) Port the double-glyph geometry from `scripts/make-icon.swift` into the
existing software rasterizer in `src/ui/tray.rs` — **picked**: no new assets, keeps
the auto light/dark template behaviour, and the geometry already exists in the
Swift source (back glyph at ~0.72 scale, offset up-left, knockout gap around the
front glyph).
(c) Generate a PNG at build time with a `build.rs` — rejected: build complexity
for no gain over ~40 lines of SDF code.

**Update check.**
(a) Sparkle (the macOS updater standard) — rejected: Objective-C framework, an
appcast feed and signing we do not have.
(b) Tauri-updater-style signed artifact feed — rejected: same signing problem.
(c) Poll `api.github.com/repos/.../releases/latest`, parse the tag with
`serde_json`, compare semver, then hand off to the browser or to
`brew upgrade --cask duet` — **picked**: no dependency, no signing, works from
day one, and never touches the installation itself.

**Demo animation.**
(a) Real terminal VHS taping a fake app — rejected (anti-goal).
(b) Drive the mock build with `cliclick`, record with `screencapture -v`/
`ffmpeg`, then grade with ffmpeg (chroma shift, tape noise, scanlines, vignette,
`PLAY ▶` + timecode) and emit MP4 + GIF — **picked**: shows the real UI, and the
grading is a script so it can be re-run after a UI change.
(c) Clean capture, no grading — the owner asked for the VHS look.

**Cask distribution.**
(a) Submit to `homebrew/homebrew-cask` — rejected for now: notability criteria.
(b) Publish an own tap repository — deferred by the owner.
(c) Keep a lint-clean cask file in `packaging/homebrew/` with a
`version`/`sha256` regeneration script and publishing instructions —
**picked**.

## Design

### 1. Tray mark (`src/ui/tray.rs`)

Replace the single-glyph SDF with a two-glyph composition in the existing 24×24
design space:

- Front glyph: today's headphone silhouette (band + two ear cups), centred.
- Back glyph: the same silhouette at 0.72 scale, offset `(-4.2, -3.4)`.
- Knockout: a 1.1-unit transparent ring around the front glyph's outline, so the
  two read as two objects rather than a blob (the Swift icon knocks out the same
  gap).
- Rasterization keeps the current 4×4 supersampling and black+alpha output.

`headphones_rgba` keeps its name and signature (other call sites and tests use
it); a `pub fn mark_rgba(size: u32) -> Vec<u8>` is added only if the tray builder
needs a distinct name — the plan pins the final names.

Tests: corners transparent; ink coverage between 8 % and 45 %; ink present in the
back-glyph region and in the front-glyph region (two clusters, not one blob); a
transparent gap exists on the seam between them.

### 2. README

Structure, top to bottom:

1. Title + one-line pitch (keeps today's line).
2. Badge row (shields.io, all dynamic where possible):
   `CI` (workflow status, `branch=main`), `Release`, `Downloads`,
   `License: MIT`, `Platforms: macOS · Windows · Linux`, `macOS 11+`.
3. Demo: centered `docs/demo.gif` linked to `docs/demo.mp4`.
4. `## Install` — four blocks in this order, each honest about its state:
   - **Homebrew (cask file in this repo)** — shows the exact command that will
     work once the owner publishes the tap, plus one line saying the tap is not
     published yet and pointing at `packaging/homebrew/README.md`.
   - **macOS** — the existing `releases/latest/download/Duet-macOS.zip` link plus
     the right-click › Open caveat.
   - **Windows / Linux** — existing links, marked experimental.
   - **From source** — existing `scripts/bundle-macos.sh` block.
5. Existing sections (Use, What it uses from each OS, Platforms, Develop,
   License) mostly unchanged; `Develop` gains the demo script and the cask script.
6. A short `## Status` block making the verification state explicit: macOS tested
   on hardware, Windows/Linux compile + unit-tested only.

Badge truth rule: no badge claims a platform is verified; the Status block says
what is.

### 3. Cask (`packaging/homebrew/`)

- `duet.rb`: `cask "duet"`, `version "0.2.0"`, sha256 pinned, `url` on the
  `releases/download/v#{version}/Duet-macOS.zip` pattern, `name "Duet"`,
  `desc`, `homepage`, `app "Duet.app"`, `uninstall quit:`, `zap trash:` for
  `~/Library/Application Support/Duet`, `~/Library/Logs/Duet`,
  `~/Library/Preferences/dev.duet.plist`-style leftovers that actually exist,
  `livecheck` block, and `caveats` covering Gatekeeper and the Bluetooth prompt.
- `scripts/update-cask.sh <tag>`: downloads the release zip, computes
  `sha256`, rewrites `version` and `sha256` in the cask, then runs
  `brew style --cask` when brew is present (skips with a message otherwise).
- `packaging/homebrew/README.md`: the three commands that publish the tap
  (`gh repo create`, copy the cask, `brew install --cask
  EricMarcantonio/duet/duet`) and the notability note about
  `homebrew/homebrew-cask`.

### 4. Check for updates (`src/update.rs` + footer)

Pure module, no I/O in the parts that are tested:

```rust
pub const REPO: &str = "EricMarcantonio/duet";
pub const API_URL: &str = "https://api.github.com/repos/EricMarcantonio/duet/releases/latest";

pub enum InstallKind { Brew, Direct }
pub enum UpdateState {
    Checking,
    UpToDate,
    Available { version: String, url: String, brew: bool },
    Failed(String),
}

pub fn parse_version(s: &str) -> Option<(u64, u64, u64)>;   // accepts "v0.3.0", "0.3.0"
pub fn is_newer(latest: &str, current: &str) -> bool;
pub fn parse_release(json: &str) -> Option<(String, String)>; // (tag_name, html_url)
pub fn install_kind(caskrooms: &[PathBuf]) -> InstallKind;    // /opt/homebrew/Caskroom/duet, /usr/local/Caskroom/duet
pub fn state_for(latest: &str, url: &str, current: &str, kind: InstallKind) -> UpdateState;
```

I/O wrapper (not unit-tested, thin):

```rust
fn fetch_latest() -> Result<String, String>; // curl -sSL --max-time 10 -H "User-Agent: duet" <API_URL>
```

UI: the footer's left cell becomes `Duet {version}` plus a `Check for updates`
link button. States render as: `Checking…`; `Up to date`; `0.3.0 available` +
`Download` (opens `html_url`) or, for `InstallKind::Brew`, `0.3.0 available` +
`brew upgrade --cask duet` text and a `Copy` button; `Couldn't check`. Nothing
polls automatically; the button is the only trigger. Failures are silent in the
sense that they land in this footer cell and the log, never in a toast.

Opening a URL: reuse whatever `src/ui/platform.rs` already uses for "Open
Bluetooth Settings"; if it has no such helper, add one behind `cfg` (`open` on
macOS, `cmd /C start` on Windows, `xdg-open` on Linux) — the plan pins which.

### 5. Demo capture (`scripts/demo-vhs.sh`)

Stages, each resumable, artifacts in `docs/`:

1. Preflight: `ffmpeg` present (tell the user `brew install ffmpeg` and exit 1 if
   not), `cliclick` present, `cargo` builds.
2. `DUET_MOCK=1 DUET_WINDOWED=1 cargo run` in the background; wait for the window;
   position it with `osascript` at a fixed screen rect so framing is repeatable.
3. Scripted performance with `cliclick`: open the picker, tick a paired pair,
   watch "Connecting…", tick a second pair, press `Listen together`, drag one
   volume slider, untick a pair (shows the fix from the previous change), quit.
4. Record with `ffmpeg -f avfoundation -capture_cursor 1` cropped to the window
   rect (fallback: `screencapture -v`), 30 fps.
5. Grade: `rgbashift` + `chromashift` (chroma bleed), `noise` (tape grain),
   scanline overlay (`geq`-free: `drawgrid` at 2 px), `vignette`, slight
   desaturation/contrast, `drawtext` `PLAY ▶` badge and a running timecode.
6. Encode `docs/demo.mp4` (h264, yuv420p, `+faststart`) and `docs/demo.gif`
   (palettegen/paletteuse, 12–15 fps, 640 px wide).

The script prints the two output sizes and refuses to write a GIF over 8 MB.

## What is left to the builder

- Exact SDF constants for the back glyph and knockout ring (must look right at
  18 pt; a screenshot check is part of the task).
- Exact ffmpeg filter values and the on-screen performance timings.
- Badge colours/labels and README wording beyond the constraints above.
- Whether the cask's `zap` list needs a preferences path (verify against a real
  install; if the key does not exist, leave it out).
