#!/bin/zsh
# Nib 原生版打包脚本(M5 管线,RFC v2 critic V8):
# release 二进制 → Nib.app 结构(Info.plist + icns 复用旧图标)→ ad-hoc 签名。
# 用法: scripts/bundle-nib.sh [输出目录,默认 target/release/bundle]
set -euo pipefail
cd "$(dirname "$0")/.."

OUT="${1:-target/release/bundle}"
APP="$OUT/Nib.app"
BIN=target/release/nib-app
ICNS=crates/nib-app/assets/icon.icns

[[ -f "$BIN" ]] || { echo "缺 release 二进制,先: cargo build --release -p nib-app"; exit 1; }
[[ -f "$ICNS" ]] || { echo "缺图标 $ICNS"; exit 1; }

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/Nib"
cp "$ICNS" "$APP/Contents/Resources/icon.icns"

cat > "$APP/Contents/Info.plist" << 'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>Nib</string>
    <key>CFBundleDisplayName</key><string>Nib</string>
    <key>CFBundleIdentifier</key><string>app.nib.native</string>
    <key>CFBundleVersion</key><string>0.2.0</string>
    <key>CFBundleShortVersionString</key><string>0.2.0</string>
    <key>CFBundleExecutable</key><string>Nib</string>
    <key>CFBundleIconFile</key><string>icon</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>LSMinimumSystemVersion</key><string>10.15</string>
    <key>NSHighResolutionCapable</key><true/>
    <key>NSSupportsAutomaticGraphicsSwitching</key><true/>
</dict>
</plist>
PLIST

codesign --force --deep --sign - "$APP"
echo "打包完成: $APP"
codesign -dv "$APP" 2>&1 | head -2
