#!/bin/sh
# Wrap the binary in a minimal .app so macOS gives it Dock presence, focus and
# cmd-tab. A bare Mach-O binary can open its window behind every other app.
set -e
cd "$(dirname "$0")"
~/.cargo/bin/cargo build
APP="target/DeltaMock.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS"
cp target/debug/delta-mock "$APP/Contents/MacOS/DeltaMock"
cat > "$APP/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>DeltaMock</string>
  <key>CFBundleDisplayName</key><string>Delta Mock</string>
  <key>CFBundleIdentifier</key><string>dev.local.delta-mock</string>
  <key>CFBundleExecutable</key><string>DeltaMock</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>0.1.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
</dict>
</plist>
PLIST
echo "built $APP"
# `--no-open` lets demo.sh launch it with its own environment instead.
[ "$1" = "--no-open" ] || open "$APP"
