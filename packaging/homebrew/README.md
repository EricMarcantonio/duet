# Homebrew cask

`duet.rb` is the cask for Duet's macOS release artifact
(`Duet-macOS.zip` on the GitHub release for `v#{version}`). It lives here so the
cask can be reviewed and linted with the source it packages.

The tap is **not published yet**. `brew install --cask duet` does not work today,
and the README says so. Until the tap exists, install the release zip directly
from `https://github.com/EricMarcantonio/duet/releases/latest` and right-click ›
Open on first launch.

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

## Publishing the tap

Publishing is a manual, owner-run step. From the repository root:

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
