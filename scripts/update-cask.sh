#!/usr/bin/env bash
# Regenerate packaging/homebrew/duet.rb from a released GitHub tag.
#
#   scripts/update-cask.sh v0.3.0        (bare "0.3.0" is accepted too)
#
# Downloads Duet-macOS.zip for that tag, computes its sha256, rewrites the cask's
# `version` and `sha256`, then runs `brew style --cask` on it when Homebrew is
# installed. This script is the only writer of those two fields.
set -euo pipefail

cd "$(dirname "$0")/.."

readonly CASK="packaging/homebrew/duet.rb"
readonly REPO="EricMarcantonio/duet"
readonly ARTIFACT="Duet-macOS.zip"

die() { printf 'update-cask: %s\n' "$*" >&2; exit 1; }

tag="${1:-}"
[ -n "$tag" ] || die "usage: scripts/update-cask.sh vX.Y.Z"
version="${tag#v}"
[ "$tag" = "$version" ] && tag="v${version}"
printf '%s' "$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$' \
  || die "not a release version: $version (expected vX.Y.Z)"

[ -f "$CASK" ] || die "missing $CASK"
command -v shasum >/dev/null 2>&1 || die "shasum not found"

url="https://github.com/${REPO}/releases/download/${tag}/${ARTIFACT}"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

printf 'Downloading %s\n' "$url"
curl -fsSL --retry 3 --max-time 300 -o "$tmp/$ARTIFACT" "$url" \
  || die "download failed: $url (has the release finished uploading?)"
[ -s "$tmp/$ARTIFACT" ] || die "empty download: $url"

sha="$(shasum -a 256 "$tmp/$ARTIFACT" | cut -d' ' -f1)"
printf '%s' "$sha" | grep -Eq '^[0-9a-f]{64}$' || die "bad sha256: $sha"
printf 'sha256 %s\n' "$sha"

# Anchored rewrites; the temporary file keeps the cask from being truncated mid-write.
awk -v version="$version" -v sha="$sha" '
  /^  version "[^"]*"$/ { print "  version \"" version "\""; next }
  /^  sha256 "[^"]*"$/  { print "  sha256 \"" sha "\""; next }
  { print }
' "$CASK" > "$tmp/duet.rb"

changed="$(grep -c -e "^  version \"${version}\"$" -e "^  sha256 \"${sha}\"$" "$tmp/duet.rb" || true)"
[ "$changed" -eq 2 ] || die "could not rewrite version/sha256 in $CASK"

mv "$tmp/duet.rb" "$CASK"
printf 'Updated %s: version %s, sha256 %s\n' "$CASK" "$version" "$sha"

if command -v brew >/dev/null 2>&1; then
  # Homebrew 7 refuses to load a cask from a path, so the lint can only be a
  # best effort here; the message below says how to lint it by name.
  abs_cask="$PWD/$CASK"
  if style_out="$(brew style --cask "$abs_cask" 2>&1)"; then
    printf '%s\n' "$style_out"
    printf 'brew style --cask %s: clean\n' "$CASK"
  else
    printf '%s\n' "$style_out"
    if printf '%s' "$style_out" | grep -q 'Loading casks from paths'; then
      printf 'Skipping brew style: this Homebrew will not load a cask from a path.\n'
      printf 'Lint by name instead: copy %s into a tap as Casks/d/duet.rb, then run\n' "$CASK"
      printf '  brew style --cask <user>/<tap>/duet\n'
    else
      die "brew style reported offenses in $CASK"
    fi
  fi
else
  printf 'Skipping brew style: Homebrew not installed.\n'
fi

printf 'Next: review the diff, then commit %s\n' "$CASK"
