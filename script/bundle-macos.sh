#!/bin/sh
# Builds Randori.app (release) so it launches like any Mac app - from Finder,
# the Dock or `open` - instead of as a bare binary, which Finder runs inside a
# Terminal window. Uses only cargo and the macOS command-line tools.
#
#   script/bundle-macos.sh            # -> target/release/Randori.app
#   script/bundle-macos.sh --dmg      # also target/release/Randori.dmg
#   open target/release/Randori.app
set -eu

dmg=no
for arg in "$@"; do
  case "$arg" in
    --dmg) dmg=yes ;;
    *) echo "usage: $0 [--dmg]" >&2; exit 2 ;;
  esac
done

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

version=$(sed -n 's/^version = "\(.*\)"/\1/p' crates/randori/Cargo.toml | head -n 1)
cargo build --release -p randori

app="target/release/Randori.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp target/release/randori "$app/Contents/MacOS/randori"
cp crates/randori/assets/icon/Randori.icns "$app/Contents/Resources/Randori.icns"

cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Randori</string>
  <key>CFBundleDisplayName</key><string>Randori</string>
  <key>CFBundleIdentifier</key><string>io.github.jeonsu0609.randori</string>
  <key>CFBundleExecutable</key><string>randori</string>
  <key>CFBundleIconFile</key><string>Randori</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleVersion</key><string>$version</string>
  <key>CFBundleShortVersionString</key><string>$version</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST

# Ad-hoc signature: enough to run locally on Apple silicon. Distribution needs
# a Developer ID signature and notarization instead.
codesign --force --sign - "$app"
echo "built $app ($version)"

if [ "$dmg" = yes ]; then
  # a folder with the app and an Applications link, so the image opens as the
  # usual drag-to-install window
  stage=$(mktemp -d)
  trap 'rm -rf "$stage"' EXIT
  cp -R "$app" "$stage/"
  ln -s /Applications "$stage/Applications"
  image="target/release/Randori.dmg"
  rm -f "$image"
  hdiutil create -quiet -volname "Randori $version" -srcfolder "$stage" -format UDZO "$image"
  echo "built $image"
fi
