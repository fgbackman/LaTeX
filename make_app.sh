#!/bin/sh
# Build Typeset.app (release, Apple Silicon or Intel — whatever this Mac is).
#   ./make_app.sh            -> ./Typeset.app
#   ./make_app.sh install    -> also copies it to /Applications
set -eu
cd "$(dirname "$0")"

command -v cargo >/dev/null || { echo "Rust isn't installed: curl https://sh.rustup.rs -sSf | sh"; exit 1; }
command -v tectonic >/dev/null || [ -x /opt/homebrew/bin/tectonic ] || echo "Note: Tectonic not found. Install it with: brew install tectonic"

# --locked: build exactly the dependency versions in Cargo.lock (no surprise updates).
cargo build --release --locked

APP=Typeset.app
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp target/release/typeset "$APP/Contents/MacOS/Typeset"

# App icon: build a native .icns from icon/icon_1024.png with Apple's tools
# (every size Finder/Dock wants); fall back to the prebuilt one.
ICONSET=$(mktemp -d)/Typeset.iconset
mkdir -p "$ICONSET"
if command -v iconutil >/dev/null && [ -f icon/icon_1024.png ]; then
  for s in 16 32 128 256 512; do
    sips -z $s $s icon/icon_1024.png --out "$ICONSET/icon_${s}x${s}.png" >/dev/null
    d=$((s * 2))
    sips -z $d $d icon/icon_1024.png --out "$ICONSET/icon_${s}x${s}@2x.png" >/dev/null
  done
  iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/Typeset.icns" \
    || cp icon/Typeset.icns "$APP/Contents/Resources/Typeset.icns"
else
  cp icon/Typeset.icns "$APP/Contents/Resources/Typeset.icns"
fi

# Version from Cargo.toml; build number = when it was built (shown in About Typeset).
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
BUILD=$(date +%Y%m%d.%H%M)

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Typeset</string>
  <key>CFBundleDisplayName</key><string>Typeset</string>
  <key>CFBundleIdentifier</key><string>app.typeset.editor</string>
  <key>CFBundleExecutable</key><string>Typeset</string>
  <key>CFBundleIconFile</key><string>Typeset</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleVersion</key><string>$BUILD</string>
  <key>LSMinimumSystemVersion</key><string>12.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSSupportsAutomaticTermination</key><false/>
  <key>NSHumanReadableCopyright</key><string>Typeset — live LaTeX in Rust</string>
</dict>
</plist>
PLIST

# Ad-hoc signature so Gatekeeper/hardened runtime treat it as a real app.
codesign --force --sign - "$APP" >/dev/null 2>&1 || true

echo "Built $(pwd)/$APP — version $VERSION (build $BUILD)"
if [ "${1:-}" = "install" ]; then
  rm -rf "/Applications/$APP"
  cp -R "$APP" /Applications/
  # Nudge Finder/Dock to pick up the new icon.
  touch "/Applications/$APP"
  /System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister -f "/Applications/$APP" >/dev/null 2>&1 || true
  echo "Installed to /Applications/$APP"
fi
