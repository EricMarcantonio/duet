# Duet production-OS polish Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make Duet read and behave like a maintained product: two-headphone logo in the tray, live README status with a VHS-graded demo, a real Homebrew cask, and a working "Check for updates" control, released as v0.3.0.

**Architecture:** One shared SVG-free glyph rasterizer change (`src/ui/tray.rs`), one new dependency-free pure module for update decisions (`src/update.rs`) plus thin I/O wrappers in the UI, packaging assets under `packaging/homebrew/`, a repeatable capture script under `scripts/`, and a README rebuilt around badges, demo and install paths. No backend, audio or Bluetooth behaviour changes.

**Tech Stack:** Rust 2024 + Dioxus 0.7 desktop (Dioxus signals), `serde_json` (already a dependency), `curl` via `std::process::Command`, ffmpeg + cliclick for the demo, Ruby cask + `brew style`.

**Spec:** `docs/superpowers/specs/2026-11-21-production-oss-polish-design.md`

## Global Constraints

- No new Cargo dependencies; `serde_json`, `dirs` and `tokio` are the only ones allowed.
- `cargo test --lib --bins --examples`, `cargo clippy --all-targets`, `cargo fmt --all` clean — this is exactly CI.
- Shared modules compile for Windows and Linux; OS-specific code behind `cfg`.
- Tray icon: 36 px RGBA, black `[0,0,0,alpha]` only (`tray::ICON_SIZE`).
- Version single source: `Cargo.toml` `version`, read as `env!("CARGO_PKG_VERSION")`.
- Demo artifacts: `docs/demo.gif` ≤ 8 MB, `docs/demo.mp4` ≤ 6 MB, 640 px wide GIF.
- Cask pins the artifact that exists: after Task 7 the sha256 must match the `v0.3.0` release zip exactly.
- README must not present an unpublished tap as working, and must not claim Windows/Linux are hardware-verified.
- Release tag `v0.3.0` is pushed only after the PR merges (Task 7), never from a task branch.

## Review Focus

- **No network / curl missing / GitHub rate limit** → the footer shows `Couldn't check`; the UI never blocks, never panics, never opens a dialog. Fetch must run off the UI thread with a ≤10 s timeout.
- **Non-plain version strings** (`v0.3.0`, `0.3`, `0.3.0-beta.1`, empty tag) → never report a downgrade or a prerelease as an available update, never panic.
- **Menu bar at 18 pt** → the two glyphs must not merge into one blob; corners transparent, seam knocked out, ink coverage in the 8–45 % band.
- **Cask sha256 drift** → if the release zip is rebuilt/clobbered, the cask must be regenerated, not silently stale; `scripts/update-cask.sh` is the only writer.
- **Demo script without ffmpeg/TCC/window focus** → fail with an actionable message and refuse to write a partial GIF.

---

### Task 1: Two-headphone tray mark

**Files:**
- Modify: `src/ui/tray.rs` (glyph rasterizer + tests)

**Interfaces:**
- Produces: `pub fn headphones_rgba(size: u32) -> Vec<u8>` — unchanged name/signature, now draws the Duet mark (front glyph + smaller, tinted-back glyph with a knocked-out seam). `pub const ICON_SIZE: u32 = 36` unchanged.

- [ ] **Step 1: Replace the glyph test with the new expectations**

```rust
#[test]
fn glyph_is_two_headphones_with_a_gap_between_them() {
    let px = headphones_rgba(ICON_SIZE);
    assert_eq!(px.len(), (ICON_SIZE * ICON_SIZE * 4) as usize);
    let alpha = |x: u32, y: u32| px[((y * ICON_SIZE + x) * 4 + 3) as usize];
    // Template image: black pixels only, transparent elsewhere.
    assert!(px.chunks(4).all(|p| p[0] == 0 && p[1] == 0 && p[2] == 0));
    assert_eq!(alpha(0, 0), 0);
    assert_eq!(alpha(ICON_SIZE - 1, ICON_SIZE - 1), 0);
    // Ink coverage stays in a band that reads at 18 pt.
    let ink = px.chunks(4).filter(|p| p[3] > 32).count();
    let pct = ink * 100 / (ICON_SIZE * ICON_SIZE) as usize;
    assert!((8..=45).contains(&pct), "ink coverage {pct}% out of band");
    // Two clusters: ink in the upper-left back glyph and in the lower-right front glyph.
    assert!(alpha(9, 8) > 0, "back glyph band missing");
    assert!(alpha(26, 26) > 0, "front glyph ear cup missing");
}

#[test]
fn the_seam_between_the_glyphs_is_knocked_out() {
    let px = headphones_rgba(ICON_SIZE);
    let alpha = |x: u32, y: u32| px[((y * ICON_SIZE + x) * 4 + 3) as usize];
    // The gap the icon has between the back and front silhouettes.
    let gap = (12..=16).any(|x| (12..=16).any(|y| alpha(x, y) == 0));
    assert!(gap, "back and front glyphs merged into one blob");
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib ui::tray`
Expected: FAIL — `back glyph band missing` (today's single glyph has no ink at `9,8`).

- [ ] **Step 3: Draw the mark**

Keep the 4×4 supersampled SDF rasterizer. Draw the existing headphone silhouette twice: front at scale 1.0 centred as today, back at scale 0.72 offset `(-4.2, -3.4)` (design units), and clear a 1.1-unit ring around the front glyph's outline so the seam opens up. Reference geometry: `scripts/make-icon.swift` `headphones(center:scale:bold:)` plus the knockout ring at lines 112–130.

- [ ] **Step 4: Verify**

Run: `cargo test --lib ui::tray && cargo clippy --all-targets && cargo fmt --all --check`
Expected: 2 passed, no warnings.

- [ ] **Step 5: Look at it**

Run: `DUET_MOCK=1 DUET_WINDOWED=1 cargo run` and screenshot the menu bar with `screencapture -x /tmp/tray.png`, crop the tray area. Expected: two distinguishable headphone shapes, no blob. Re-tune the back-glyph offset/scale until two shapes read at 18 pt.

- [ ] **Step 6: Commit**

```bash
git add src/ui/tray.rs
git commit -m "Tray: use the Duet two-headphone mark instead of a single pair"
```

---

### Task 2: Update-check decisions (pure module)

**Files:**
- Create: `src/update.rs`
- Modify: `src/lib.rs` (`pub mod update;`)

**Interfaces:**
- Consumes: nothing.
- Produces (later tasks rely on these exact names):

```rust
pub const REPO: &str = "EricMarcantonio/duet";
pub const API_URL: &str = "https://api.github.com/repos/EricMarcantonio/duet/releases/latest";
pub const RELEASES_PAGE: &str = "https://github.com/EricMarcantonio/duet/releases/latest";

pub enum InstallKind { Brew, Direct }
pub enum UpdateState {
    Checking,
    UpToDate,
    Available { version: String, url: String, brew: bool },
    Failed(String),
}

pub fn parse_version(s: &str) -> Option<(u64, u64, u64)>;
pub fn is_newer(latest: &str, current: &str) -> bool;
pub fn parse_release(json: &str) -> Option<(String, String)>;
pub fn caskroom_candidates() -> Vec<std::path::PathBuf>;
pub fn install_kind(caskrooms: &[std::path::PathBuf]) -> InstallKind;
pub fn state_for(tag: &str, url: &str, current: &str, kind: InstallKind) -> UpdateState;
pub const BREW_UPGRADE: &str = "brew upgrade --cask duet";
```

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn parses_plain_and_v_prefixed_versions() {
    assert_eq!(parse_version("0.3.0"), Some((0, 3, 0)));
    assert_eq!(parse_version("v0.3.0"), Some((0, 3, 0)));
    assert_eq!(parse_version("v1.10.2"), Some((1, 10, 2)));
    assert_eq!(parse_version("0.3"), None);
    assert_eq!(parse_version("garbage"), None);
}

#[test]
fn only_a_strictly_greater_version_is_newer() {
    assert!(is_newer("v0.3.0", "0.2.0"));
    assert!(!is_newer("v0.2.0", "0.2.0"));
    assert!(!is_newer("v0.1.9", "0.2.0"));
    assert!(!is_newer("nonsense", "0.2.0"));
}

#[test]
fn reads_tag_and_page_from_a_release_payload() {
    let json = r#"{"tag_name":"v0.3.0","html_url":"https://github.com/EricMarcantonio/duet/releases/tag/v0.3.0"}"#;
    assert_eq!(
        parse_release(json),
        Some((
            "v0.3.0".to_string(),
            "https://github.com/EricMarcantonio/duet/releases/tag/v0.3.0".to_string()
        ))
    );
    assert_eq!(parse_release("{}"), None);
    assert_eq!(parse_release("not json"), None);
}

#[test]
fn detects_a_homebrew_install_from_the_caskroom() {
    use std::path::PathBuf;
    let tapped = vec![PathBuf::from("/opt/homebrew/Caskroom/duet/0.2.0")];
    assert_eq!(install_kind(&tapped), InstallKind::Brew);
    let empty: Vec<PathBuf> = Vec::new();
    assert_eq!(install_kind(&empty), InstallKind::Direct);
}

#[test]
fn state_for_covers_every_outcome() {
    assert_eq!(
        state_for("v0.2.0", "u", "0.2.0", InstallKind::Direct),
        UpdateState::UpToDate
    );
    assert_eq!(
        state_for("v0.3.0", "u3", "0.2.0", InstallKind::Direct),
        UpdateState::Available { version: "0.3.0".into(), url: "u3".into(), brew: false }
    );
    assert_eq!(
        state_for("v0.3.0", "u3", "0.2.0", InstallKind::Brew),
        UpdateState::Available { version: "0.3.0".into(), url: "u3".into(), brew: true }
    );
    assert!(matches!(
        state_for("junk", "u", "0.2.0", InstallKind::Direct),
        UpdateState::Failed(_)
    ));
}
```

`UpdateState` needs `PartialEq` + `Debug` for these assertions.

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --lib update::`
Expected: FAIL — `cannot find type 'UpdateState'` / module `update` missing.

- [ ] **Step 3: Implement the module**

`parse_version` splits on `.` into exactly three numeric parts, tolerating one leading `v`. `parse_release` uses `serde_json::Value` and requires both keys as strings. `state_for` returns `Failed("unexpected version tag")` when the tag doesn't parse, `UpToDate` when `!is_newer`, otherwise `Available` with the version without its `v` and `brew: matches!(kind, InstallKind::Brew)`. `caskroom_candidates` returns `/opt/homebrew/Caskroom/duet` and `/usr/local/Caskroom/duet` (plus `$HOMEBREW_CASKROOM/duet` when the env var is set) as existing paths only.

- [ ] **Step 4: Verify**

Run: `cargo test --lib update:: && cargo clippy --all-targets && cargo fmt --all --check`
Expected: 5 passed.

- [ ] **Step 5: Commit**

```bash
git add src/update.rs src/lib.rs
git commit -m "update: decide up-to-date vs available without any I/O"
```

---

### Task 3: "Check for updates" in the footer

**Files:**
- Modify: `src/update.rs` (add `fetch_latest` + tests for its command construction)
- Modify: `src/ui/platform.rs` (add `open_url`)
- Modify: `src/ui/app.rs` (`AppState.update`, `check_for_updates`, footer markup)
- Modify: `assets/main.css` (footer button styles if the existing `.link.quiet` is not enough)

**Interfaces:**
- Consumes: Task 2's `UpdateState`, `state_for`, `parse_release`, `install_kind`, `caskroom_candidates`.
- Produces: `pub fn fetch_latest() -> Result<String, String>`; `pub fn outcome(fetched: Result<String, String>, current: &str, kind: InstallKind) -> UpdateState`; `pub fn open_url(url: &str) -> Result<(), String>`; `pub fn check_for_updates(mut state: AppState)`; `AppState.update: Signal<Option<UpdateState>>`.

- [ ] **Step 1: Add the fetch wrapper and the outcome mapping, with failing tests**

```rust
// src/update.rs
#[test]
fn a_missing_curl_fails_with_a_message_naming_it() {
    let empty = std::env::temp_dir().join("duet-no-curl");
    std::fs::create_dir_all(&empty).unwrap();
    let err = fetch_latest_in(&empty.to_string_lossy(), None).unwrap_err();
    assert!(err.contains("curl"), "unhelpful error: {err}");
}

#[test]
fn a_failed_fetch_becomes_the_failed_state() {
    let s = outcome(Err("curl failed".into()), "0.2.0", InstallKind::Direct);
    assert_eq!(s, UpdateState::Failed("curl failed".into()));
}

#[test]
fn a_successful_fetch_with_a_junk_payload_is_a_failure_too() {
    let s = outcome(Ok("not json".into()), "0.2.0", InstallKind::Direct);
    assert!(matches!(s, UpdateState::Failed(_)));
}
```

Implement `fetch_latest()` as `fetch_latest_in(PATH, Some(Duration::from_secs(10)))`, which runs `curl -sSL --max-time 10 -H User-Agent:duet <API_URL>` through `std::process::Command` and returns stdout, mapping a spawn failure to an error string that names `curl`. `outcome` is the pure mapping: `Err` → `Failed`, `Ok(json)` → `parse_release` → `state_for`, unparsable payload → `Failed("couldn't read the latest release")`.

- [ ] **Step 2: Run them and confirm they fail for the right reason**

Run: `cargo test --lib update::`
Expected: FAIL to compile until `fetch_latest_in` and `outcome` exist; then PASS with the 8 update-module tests.

- [ ] **Step 3: Wire the state and the footer**

Add `pub update: Signal<Option<UpdateState>>` to `AppState` (init `None`). Add:

```rust
pub fn check_for_updates(mut state: AppState) {
    if matches!(state.update.peek().as_ref(), Some(UpdateState::Checking)) {
        return;
    }
    state.update.set(Some(UpdateState::Checking));
    spawn_forever(async move {
        let current = env!("CARGO_PKG_VERSION");
        let kind = crate::update::install_kind(&crate::update::caskroom_candidates());
        let next = {
        let current = env!("CARGO_PKG_VERSION");
        let kind = crate::update::install_kind(&crate::update::caskroom_candidates());
        crate::update::outcome(blocking(crate::update::fetch_latest).await.unwrap_or_else(Err), current, kind)
    };
        crate::dlog!("update check: {next:?}");
        state.update.set(Some(next));
    });
}
```

Footer markup (`src/ui/app.rs`, the block at the `footer { class: "footer", … }`):

```rust
footer { class: "footer",
    span { class: "version", "Duet {env!(\"CARGO_PKG_VERSION\")}" }
    UpdateCheck {}
    button { class: "link quiet", onclick: move |_| quit_app(), "Quit Duet" }
}
```

`UpdateCheck` renders, from `state.update`: nothing when `None` (just a `Check for updates` link button), `Checking…` as text, `Up to date`, `Failed` as `Couldn't check` (title attribute carries the detail), and `Available { version, url, brew }` as `{version} available` followed by either a `Download` button calling `platform::open_url(&url)` or, when `brew`, a `Copy` button that puts `BREW_UPGRADE` on the clipboard and is labelled `brew upgrade --cask duet`. Clipboard: reuse whatever the app already uses if anything; otherwise `arboard` is **not** allowed — write the command into the log and show it as selectable text instead.

`open_url` in `platform.rs`: `open` (macOS), `cmd /C start "" <url>` (Windows), `xdg-open` (Linux), each `.spawn()` mapped to `Result<(), String>`.

- [ ] **Step 4: Verify by hand against the real API**

Run: `DUET_MOCK=1 DUET_WINDOWED=1 cargo run`, click `Check for updates`.
Expected: footer reads `Up to date` (latest release is `v0.2.0`, app is `0.2.0`), and `~/Library/Logs/Duet/duet.log` gains `update check: UpToDate`. Then run with `DUET_MOCK=1` and a temporary local JSON fixture path that returns `v9.9.9` to see the `Download` state (scripted, not committed).

- [ ] **Step 5: Verify CI gates**

Run: `cargo test --lib --bins --examples && cargo clippy --all-targets && cargo fmt --all --check`
Expected: pass.

- [ ] **Step 6: Commit**

```bash
git add src/update.rs src/ui/platform.rs src/ui/app.rs assets/main.css
git commit -m "Footer: check for updates against GitHub Releases, brew-aware"
```

---

### Task 4: Homebrew cask in-repo

**Files:**
- Create: `packaging/homebrew/duet.rb`
- Create: `packaging/homebrew/README.md`
- Create: `scripts/update-cask.sh`

**Interfaces:**
- Produces: `scripts/update-cask.sh <tag>` — rewrites `version` and `sha256` in `packaging/homebrew/duet.rb` from the release zip, then lints.

- [ ] **Step 1: Write the cask**

```ruby
cask "duet" do
  version "0.3.0"
  sha256 "0000000000000000000000000000000000000000000000000000000000000000"

  url "https://github.com/EricMarcantonio/duet/releases/download/v#{version}/Duet-macOS.zip",
      verified: "github.com/EricMarcantonio/duet/"
  name "Duet"
  desc "Play audio on two pairs of Bluetooth headphones, each with its own volume"
  homepage "https://github.com/EricMarcantonio/duet"

  livecheck do
    url :url
    strategy :github_latest
  end

  app "Duet.app"

  caveats <<~EOS
    Duet is not notarized: the first launch needs right-click › Open, or
    System Settings › Privacy & Security › Open Anyway.
    Allow Bluetooth access when macOS asks, or Duet can't see your headphones.
  EOS
end
```

The sha256 is a placeholder until Task 7 replaces it with the real `v0.3.0` artifact digest. Do **not** commit a digest for an artifact that does not exist yet.

- [ ] **Step 2: Write the tap-publishing README**

`packaging/homebrew/README.md` documents, in order: `gh repo create EricMarcantonio/homebrew-duet --public`, copying `duet.rb` to `Casks/d/duet.rb` in that repo, `brew install --cask EricMarcantonio/duet/duet`, and the note that `homebrew/homebrew-cask` requires notability so a core submission is a later step.

- [ ] **Step 3: Write `scripts/update-cask.sh`**

```sh
#!/usr/bin/env bash
# usage: scripts/update-cask.sh v0.3.0
set -euo pipefail
tag="${1:?usage: scripts/update-cask.sh vX.Y.Z}"
version="${tag#v}"
url="https://github.com/EricMarcantonio/duet/releases/download/${tag}/Duet-macOS.zip"
tmp="$(mktemp -d)"
# download, sha256, rewrite version+sha256 in packaging/homebrew/duet.rb, then:
#   brew style --cask packaging/homebrew/duet.rb   (skip with a message if brew is absent)
```

- [ ] **Step 4: Verify against the artifact that exists**

Run: `scripts/update-cask.sh v0.2.0`, then confirm `packaging/homebrew/duet.rb` carries `0.2.0` and the digest printed by `shasum -a 256` of the downloaded zip, and `brew style --cask packaging/homebrew/duet.rb` is clean. Revert the file to the `0.3.0` placeholder afterwards (`git checkout packaging/homebrew/duet.rb` and re-apply the placeholder text) so Task 7 pins the release artifact.

- [ ] **Step 5: Commit**

```bash
git add packaging/homebrew scripts/update-cask.sh
git commit -m "packaging: add a Homebrew cask and its version/sha256 updater"
```

---

### Task 5: VHS-graded demo capture

**Files:**
- Create: `scripts/demo-vhs.sh`
- Create: `docs/demo.gif`, `docs/demo.mp4` (generated, committed)

**Interfaces:**
- Produces: the two `docs/demo.*` files referenced by Task 6's README.

- [ ] **Step 1: Install the tools and write the script skeleton with its guards**

`brew install ffmpeg` (one time). The script's first block checks `command -v ffmpeg`, `command -v cliclick`, and `cargo build` success, printing `brew install ffmpeg` / `brew install cliclick` and exiting 1 instead of recording garbage.

- [ ] **Step 2: Add the performance**

Launch `DUET_MOCK=1 DUET_WINDOWED=1 ./target/debug/duet` in the background, wait for the window (poll `screencapture` + `cliclick` focus), move it to a fixed rect with `osascript` (`System Events` `position`/`size`), then drive the sequence with `cliclick` and fixed sleeps: open the picker, tick one paired pair, tick a second, press `Listen together`, drag a volume slider, untick a pair, close the picker. Total ~35 s.

- [ ] **Step 3: Record and grade**

`ffmpeg -f avfoundation -capture_cursor 1 -framerate 30 -i "1:none"` cropped to the window rect (fallback `screencapture -v`), then one filter chain for the VHS look — chroma shift + rgbashift for tape bleed, `noise=alls=12:allf=t+u` for grain, `drawgrid` scanlines, `vignette`, mild desaturation, and `drawtext` for a `PLAY ▶` badge plus a running timecode. Encode `docs/demo.mp4` (h264, `-pix_fmt yuv420p -movflags +faststart`) and `docs/demo.gif` (palettegen/paletteuse, 13 fps, 640 px wide).

- [ ] **Step 4: Verify the artifacts and the size limits**

Run: `scripts/demo-vhs.sh` then `ls -l docs/demo.gif docs/demo.mp4` and inspect `docs/demo.gif`.
Expected: GIF ≤ 8 MB, MP4 ≤ 6 MB, the animation shows the real panel, the VHS grain is visible but the text stays readable, and no cursor sits over a label for the whole take. Re-run with adjusted timings if not.

- [ ] **Step 5: Commit**

```bash
git add scripts/demo-vhs.sh docs/demo.gif docs/demo.mp4
git commit -m "docs: VHS-graded demo capture script and the take it produces"
```

---

### Task 6: README, badges, version 0.3.0

**Files:**
- Modify: `README.md` (badges, demo, Install section, Status block, Develop)
- Modify: `Cargo.toml` (`version = "0.3.0"`)
- Modify: `Cargo.lock` (the `duet` package version)

**Interfaces:**
- Consumes: `docs/demo.gif` + `docs/demo.mp4` (Task 5), `packaging/homebrew/` (Task 4), the update-check footer label text (Task 3).

- [ ] **Step 1: Bump the version and confirm nothing else drifts**

`Cargo.toml` → `version = "0.3.0"`; run `cargo check` so `Cargo.lock` updates. `env!("CARGO_PKG_VERSION")` then reads `0.3.0` in the footer and in the update comparison.

- [ ] **Step 2: Add the badge row and the demo**

Badges (dynamic, shields.io): CI `github/actions/workflow/status/EricMarcantonio/duet/ci.yml?branch=main`, Release `github/v/release/EricMarcantonio/duet`, Downloads `github/downloads/EricMarcantonio/duet/total?label=downloads`, `License MIT`, and two static ones: `macOS 11+` and `platforms macOS · Windows · Linux`. Then a centered `docs/demo.gif` linking to `docs/demo.mp4`, under a `## Demo` heading.

- [ ] **Step 3: Rebuild the Install section**

Order: **Homebrew (cask)** showing `brew install --cask EricMarcantonio/duet/duet` with one line stating the tap is not published yet and pointing at `packaging/homebrew/README.md`; then **macOS** direct zip with the right-click › Open caveat; then **Windows / Linux**, experimental; then **Build from source**. Add a `## Status` block: macOS tested on hardware, Windows and Linux compile and are unit-tested only.

- [ ] **Step 4: Verify the README renders and says only true things**

Open the file and check: every badge URL 200s (`curl -sI` each one), the GIF path resolves, the cask command matches `packaging/homebrew/README.md`, and no sentence claims a cask/tap that does not exist.

- [ ] **Step 5: Full gate**

Run: `cargo test --lib --bins --examples && cargo clippy --all-targets && cargo fmt --all --check`
Expected: pass (the version bump touches nothing else).

- [ ] **Step 6: Commit**

```bash
git add README.md Cargo.toml Cargo.lock
git commit -m "README: status badges, demo, install paths; 0.3.0"
```

---

### Task 7: Release v0.3.0 and pin the cask

**Files:**
- Modify: `packaging/homebrew/duet.rb` (real sha256)

**Interfaces:**
- Consumes: everything merged on `main`.

- [ ] **Step 1: Merge the branch, then push the tag**

After the PR is merged and `main` is green: `git tag v0.3.0 && git push origin v0.3.0`. This fires `release.yml`, which creates the release and builds/uploads all three artifacts.

- [ ] **Step 2: Wait for the release artifacts**

`gh run watch` on the release workflow, then `gh release view v0.3.0 --json assets` — `Duet-macOS.zip`, `Duet-Windows.zip`, `Duet-Linux.tar.gz` all present and non-zero.

- [ ] **Step 3: Pin the cask to the real artifact**

Run: `scripts/update-cask.sh v0.3.0`.
Expected: `packaging/homebrew/duet.rb` now has `version "0.3.0"` and the sha256 of the uploaded `Duet-macOS.zip`; `brew style --cask` clean.

- [ ] **Step 4: Commit the digest and confirm the live states**

Commit `packaging/homebrew/duet.rb` (`packaging: pin the cask to the v0.3.0 artifact`) and push. Then confirm: the Release badge reads `v0.3.0`, the CI badge is green, and `DUET_MOCK=1` + `Check for updates` says `Up to date` at 0.3.0.
