//! Update checks: the decisions, with no I/O.
//!
//! Nothing in this module touches the network, the disk or a process, so every
//! rule about "is there a newer release?" is unit-testable: version parsing,
//! release-payload parsing, and the mapping from those to a footer state. The
//! one place that shells out (`curl`) is added by a later task; the UI keeps it
//! off the render path.

use std::path::PathBuf;

/// `owner/repo`, as GitHub spells it.
pub const REPO: &str = "EricMarcantonio/duet";
/// GitHub's "latest release" endpoint (unauthenticated, 60 requests an hour).
pub const API_URL: &str = "https://api.github.com/repos/EricMarcantonio/duet/releases/latest";
/// Where "Download" sends people when the release payload has no page URL.
pub const RELEASES_PAGE: &str = "https://github.com/EricMarcantonio/duet/releases/latest";
/// The command a Homebrew install should run instead of downloading a zip.
pub const BREW_UPGRADE: &str = "brew upgrade --cask duet";

/// How this copy of Duet got onto the machine, which decides how it updates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallKind {
    /// Installed by `brew install --cask`: the caskroom has a directory for it.
    Brew,
    /// A zip from the releases page, so the new build is another download.
    Direct,
}

/// What the footer's update cell should say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateState {
    /// A fetch is in flight.
    Checking,
    /// The running version is the latest (or newer, e.g. a local build).
    UpToDate,
    /// A newer release exists; `version` has no leading `v`.
    Available {
        version: String,
        url: String,
        brew: bool,
    },
    /// The check itself failed; the string is for the log and a tooltip.
    Failed(String),
}

/// `"0.3.0"` and `"v0.3.0"` → `(0, 3, 0)`.
///
/// Strictly three numeric parts: `"0.3"`, `""`, `"v0.3.0-beta.1"` and anything
/// with a suffix are rejected, so a prerelease or a malformed tag can never be
/// mistaken for an upgrade (the caller turns `None` into a failure, never into
/// a downgrade prompt).
pub fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let s = s.trim();
    let s = s.strip_prefix('v').unwrap_or(s);
    let mut parts = s.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

/// True only when `latest` parses and is strictly greater than `current`.
///
/// Equal versions are not news, and an unparsable side is never "newer".
pub fn is_newer(latest: &str, current: &str) -> bool {
    match (parse_version(latest), parse_version(current)) {
        (Some(latest), Some(current)) => latest > current,
        _ => false,
    }
}

/// `(tag_name, html_url)` from a GitHub release payload, both required strings.
///
/// `None` for junk, a non-object, or a payload missing either key.
pub fn parse_release(json: &str) -> Option<(String, String)> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    let tag = value.get("tag_name")?.as_str()?;
    let url = value.get("html_url")?.as_str()?;
    Some((tag.to_string(), url.to_string()))
}

/// Does any component of `path` name a `duet` cask? Accepts both the caskroom
/// directory itself and a version directory inside it.
fn is_duet_cask(path: &std::path::Path) -> bool {
    path.components()
        .any(|part| part.as_os_str().eq_ignore_ascii_case("duet"))
}

/// Caskroom directories for Duet that exist on this machine.
///
/// Apple silicon, Intel, and `$HOMEBREW_CASKROOM` (a non-standard prefix) are
/// all probed; every candidate is checked with `is_dir`, so the result is safe
/// to read as "Homebrew has this installed" and never contains a phantom path.
pub fn caskroom_candidates() -> Vec<PathBuf> {
    let mut roots = vec![
        PathBuf::from("/opt/homebrew/Caskroom/duet"),
        PathBuf::from("/usr/local/Caskroom/duet"),
    ];
    if let Some(dir) = std::env::var_os("HOMEBREW_CASKROOM") {
        roots.push(PathBuf::from(dir).join("duet"));
    }
    let mut found: Vec<PathBuf> = Vec::new();
    for candidate in roots {
        if candidate.is_dir() && !found.contains(&candidate) {
            found.push(candidate);
        }
    }
    found
}

/// Any caskroom entry for Duet means Homebrew owns the install.
pub fn install_kind(caskrooms: &[PathBuf]) -> InstallKind {
    if caskrooms.iter().any(|path| is_duet_cask(path)) {
        InstallKind::Brew
    } else {
        InstallKind::Direct
    }
}

/// The whole decision for one release payload, with no I/O.
///
/// A tag that doesn't parse is a failed check (never a downgrade prompt), and
/// only a strictly newer version becomes [`UpdateState::Available`].
pub fn state_for(tag: &str, url: &str, current: &str, kind: InstallKind) -> UpdateState {
    let Some((major, minor, patch)) = parse_version(tag) else {
        return UpdateState::Failed("unexpected version tag".to_string());
    };
    if !is_newer(tag, current) {
        return UpdateState::UpToDate;
    }
    UpdateState::Available {
        version: format!("{major}.{minor}.{patch}"),
        url: url.to_string(),
        brew: matches!(kind, InstallKind::Brew),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            UpdateState::Available {
                version: "0.3.0".into(),
                url: "u3".into(),
                brew: false
            }
        );
        assert_eq!(
            state_for("v0.3.0", "u3", "0.2.0", InstallKind::Brew),
            UpdateState::Available {
                version: "0.3.0".into(),
                url: "u3".into(),
                brew: true
            }
        );
        assert!(matches!(
            state_for("junk", "u", "0.2.0", InstallKind::Direct),
            UpdateState::Failed(_)
        ));
    }

    /// The caskroom probe runs on every real machine, so it must only ever
    /// report directories that are actually there.
    #[test]
    fn caskroom_candidates_only_reports_real_paths() {
        let candidates = caskroom_candidates();
        for path in &candidates {
            assert!(path.exists(), "reported a missing path: {}", path.display());
        }
        assert!(
            candidates.is_empty() || candidates.iter().all(|p| p.ends_with("duet")),
            "every candidate should be a `duet` cask directory: {candidates:?}"
        );
    }
}
