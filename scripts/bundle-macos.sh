#!/usr/bin/env bash
# Builds dist/ClipboardX.app (menu-bar app, no Dock icon) and, with --dmg,
# dist/ClipboardX-<version>.dmg. Signs ad hoc unless SIGN_IDENTITY is set.
#
#   scripts/bundle-macos.sh [--dmg] [--target aarch64-apple-darwin|x86_64-apple-darwin]
set -euo pipefail

cd "$(dirname "$0")/.."

make_dmg=false
target=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --dmg) make_dmg=true; shift ;;
    --target) target="$2"; shift 2 ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
done

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
identity="${SIGN_IDENTITY:--}"

if [[ -n "$target" ]]; then
  cargo build --release --target "$target"
  binary="target/$target/release/clipboardx"
else
  cargo build --release
  binary="target/release/clipboardx"
fi

app="dist/ClipboardX.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$binary" "$app/Contents/MacOS/ClipboardX"
cp assets/icon.icns "$app/Contents/Resources/icon.icns"

cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>ClipboardX</string>
  <key>CFBundleDisplayName</key><string>ClipboardX</string>
  <key>CFBundleIdentifier</key><string>com.clipboardx.app</string>
  <key>CFBundleExecutable</key><string>ClipboardX</string>
  <key>CFBundleIconFile</key><string>icon</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>${version}</string>
  <key>CFBundleVersion</key><string>${version}</string>
  <key>LSMinimumSystemVersion</key><string>14.0</string>
  <key>LSUIElement</key><true/>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST

codesign --force --deep --sign "$identity" "$app"
echo "built $app ($version)"

if $make_dmg; then
  dmg="dist/ClipboardX-${version}.dmg"
  staging=$(mktemp -d)
  cp -R "$app" "$staging/"
  ln -s /Applications "$staging/Applications"
  rm -f "$dmg"
  hdiutil create -volname ClipboardX -srcfolder "$staging" -ov -format UDZO "$dmg" >/dev/null
  rm -rf "$staging"
  echo "built $dmg"
fi
