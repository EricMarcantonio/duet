#!/bin/sh
# Build Duet.app for macOS.
#
#   ./scripts/bundle-macos.sh                 build (Apple Silicon + Intel universal binary)
#   ./scripts/bundle-macos.sh --install       ...and copy to ~/Applications
#   ./scripts/bundle-macos.sh --zip           ...and write dist/Duet-macOS.zip to share
#
# Signing: ad-hoc by default (fine on this Mac; others must right-click > Open the
# first time). For frictionless sharing set SIGN_IDENTITY="Developer ID Application: …"
# and NOTARY_PROFILE=<notarytool keychain profile> to sign + notarize.
set -eu
cd "$(dirname "$0")/.."
dx bundle --release --platform desktop --package-types macos
APP="target/dx/duet/bundle/macos/macos/Duet.app"
PL="$APP/Contents/Info.plist"
BIN="$APP/Contents/MacOS/duet"

# Universal binary: add an Intel slice when the target is installed.
if rustup target list --installed | grep -q x86_64-apple-darwin; then
  cargo build --release --target x86_64-apple-darwin --bin duet
  cargo build --release --target aarch64-apple-darwin --bin duet
  lipo -create -output "$BIN" \
    target/aarch64-apple-darwin/release/duet target/x86_64-apple-darwin/release/duet
fi

pb() { /usr/libexec/PlistBuddy -c "$1" "$PL" 2>/dev/null || /usr/libexec/PlistBuddy -c "$2" "$PL"; }
if [ ! -f "$APP/Contents/Resources/Duet.icns" ]; then
  mkdir -p "$APP/Contents/Resources"
  cp assets/icon/Duet.icns "$APP/Contents/Resources/Duet.icns"
  pb "Set :CFBundleIconFile Duet.icns" "Add :CFBundleIconFile string Duet.icns"
fi
# Menu bar app: no Dock icon, no flash at launch.
pb "Set :LSUIElement true" "Add :LSUIElement bool true"
pb "Set :NSBluetoothAlwaysUsageDescription 'Duet lists and connects your paired Bluetooth headphones.'" \
   "Add :NSBluetoothAlwaysUsageDescription string 'Duet lists and connects your paired Bluetooth headphones.'"
pb "Set :LSMinimumSystemVersion 11.0" "Add :LSMinimumSystemVersion string 11.0"

if [ -n "${SIGN_IDENTITY:-}" ]; then
  codesign --force --deep --options runtime --timestamp --sign "$SIGN_IDENTITY" "$APP"
else
  codesign --force --deep --sign - "$APP"
fi
echo "Built $APP ($(lipo -archs "$BIN"))"

for arg in "$@"; do
  case "$arg" in
    --install)
      mkdir -p "$HOME/Applications"
      pkill -x duet 2>/dev/null && sleep 1 || true
      rm -rf "$HOME/Applications/Duet.app"
      cp -R "$APP" "$HOME/Applications/"
      echo "Installed to $HOME/Applications/Duet.app"
      ;;
    --zip)
      mkdir -p dist
      rm -f dist/Duet-macOS.zip
      ditto -c -k --keepParent "$APP" dist/Duet-macOS.zip
      if [ -n "${SIGN_IDENTITY:-}" ] && [ -n "${NOTARY_PROFILE:-}" ]; then
        xcrun notarytool submit dist/Duet-macOS.zip --keychain-profile "$NOTARY_PROFILE" --wait
        xcrun stapler staple "$APP"
        rm -f dist/Duet-macOS.zip
        ditto -c -k --keepParent "$APP" dist/Duet-macOS.zip
      fi
      echo "Wrote dist/Duet-macOS.zip"
      ;;
  esac
done
