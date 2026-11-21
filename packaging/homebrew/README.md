# Homebrew cask

`duet.rb` is the cask for Duet's macOS release artifact
(`Duet-macOS.zip` on the GitHub release for `v#{version}`). It lives here so the
cask can be reviewed and linted with the source it packages.

The tap is published: <https://github.com/EricMarcantonio/homebrew-duet>, with
this file mirrored at `Casks/d/duet.rb`. Homebrew 7 refuses casks from a
third-party tap until it is trusted, so the install is two commands:

```sh
brew trust ericmarcantonio/duet
brew install --cask EricMarcantonio/duet/duet
```

## Regenerating version and sha256

The release sha256 is generated, never typed. Run this after a release's
artifacts have finished uploading:

```sh
scripts/update-cask.sh v0.3.0
```

It downloads `Duet-macOS.zip` for that tag, sets `version` and `sha256` in
`packaging/homebrew/duet.rb`, and runs `brew style --cask` on the result when
Homebrew is installed. The file currently carries `version "0.3.0"` with an
all-zero placeholder digest because that artifact has not been released yet; a
real digest is committed only once the artifact exists.

## Publishing the tap (already done)

Kept for reference — the tap exists, this is how it was created and how it is
refreshed. From the repository root:

```sh
# 1. Create the tap repository
gh repo create EricMarcantonio/homebrew-duet --public \
  --description "Homebrew tap for Duet"

# 2. Copy the cask into the tap, under Casks/d/
git clone https://github.com/EricMarcantonio/homebrew-duet.git /tmp/homebrew-duet
mkdir -p /tmp/homebrew-duet/Casks/d
cp packaging/homebrew/duet.rb /tmp/homebrew-duet/Casks/d/duet.rb
cd /tmp/homebrew-duet && git add Casks/d/duet.rb \
  && git commit -m "duet 0.3.0" && git push

# 3. Install from the tap
brew install --cask EricMarcantonio/duet/duet
```

Keep `Casks/d/duet.rb` byte-identical to `packaging/homebrew/duet.rb`, and
re-run `scripts/update-cask.sh` on every release so a rebuilt or clobbered
release zip cannot leave the tap with a stale digest.

## What core submission actually requires

Checked against <https://docs.brew.sh/Package-Acceptance-Policy> and
<https://docs.brew.sh/Acceptable-Casks>. Four separate bars, and Duet currently
clears none of the first three:

1. **Gatekeeper.** A cask for macOS must "pass Homebrew's Gatekeeper checks and
   must not require System Integrity Protection or Gatekeeper to be disabled or
   bypassed". Duet is ad-hoc signed and un-notarized, so its own caveats tell
   people to right-click › Open. **A Developer ID certificate, `codesign` with a
   hardened runtime, `notarytool` submission and a stapled ticket are the
   prerequisite** — `scripts/bundle-macos.sh` already honours `SIGN_IDENTITY`
   and `NOTARY_PROFILE` for exactly this.
2. **Notability, self-submission tier.** The owner submitting their own repo
   needs **225 stars, or 90 forks, or 90 watchers** (the 75/30/30 tier is for
   third-party submissions). Duet has 0/0/0. The repository must also be at
   least 30 days old.
3. **A free token.** Homebrew's core tap already ships `duet` (Duet Display), and
   "the existing or more widely recognised application normally keeps the
   unprefixed token", so a submission needs a distinct name, e.g.
   `duet-audio` — with `name "Duet Audio"` or similar so search stays honest.
4. **Mechanics.** Working `livecheck` (this file has one: `strategy
   :github_latest`), a versioned upstream URL, `zap` stanzas, `brew audit --cask
   --new <token>` clean, `brew style` clean, then a PR to `homebrew/homebrew-cask`
   with its template. The tap stays the distribution channel until all of this
   lands.

## Notability: homebrew/homebrew-cask

Submitting to the core tap (`homebrew/homebrew-cask`) is a later step. Core
requires the cask to meet Homebrew's notability criteria — a sufficient number of
GitHub stars, forks and watchers, or a notable upstream project — plus a stable,
notarized or otherwise Gatekeeper-clean artifact. Duet is ad-hoc signed today
and does not yet meet the notability bar, so the own tap is the supported path.
Revisit core submission once the project has traction and a signed artifact.

Note also that `homebrew/homebrew-cask` already ships a cask named `duet` (Duet
Display, the iPad-as-second-screen tool), so `brew install --cask duet` installs
that app and not this one. Always install this project through the tap-qualified
name `EricMarcantonio/duet/duet`; a core submission would need a different cask
token.
