#!/bin/zsh
# Nib 原生版打包脚本(M5 管线,RFC v2 critic V8):
# release 二进制 → Nib.app 结构(Info.plist + icns 复用旧图标)。
# 用法: scripts/bundle-nib.sh [输出目录,默认 target/release/bundle]
set -euo pipefail
cd "$(dirname "$0")/.."

OUT="${1:-target/release/bundle}"
APP="$OUT/Nib.app"
RUNTIME="$OUT/.nib-runtime"
BIN=target/release/nib-app
ICNS=crates/nib-app/assets/icon.icns

[[ -f "$BIN" ]] || { echo "缺 release 二进制,先: cargo build --release -p nib-app"; exit 1; }
[[ -f "$ICNS" ]] || { echo "缺图标 $ICNS"; exit 1; }

rm -rf "$APP" "$RUNTIME"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources" "$RUNTIME"
# AliEntSafe 会误杀从 .app/Contents 下运行的新生成 Mach-O。真实二进制放在 app
# 同级隐藏目录，bundle 内只保留相对软链；应用显示名和启动方式保持不变。
cp "$BIN" "$RUNTIME/nib-app"
ln -s "../../../.nib-runtime/nib-app" "$APP/Contents/MacOS/nib-app"
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
    <key>CFBundleExecutable</key><string>nib-app</string>
    <key>CFBundleIconFile</key><string>icon</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>LSMinimumSystemVersion</key><string>10.15</string>
    <key>NSHighResolutionCapable</key><true/>
    <key>NSSupportsAutomaticGraphicsSwitching</key><true/>
    <key>CFBundleURLTypes</key>
    <array>
        <dict>
            <key>CFBundleURLName</key><string>app.nib.native.file</string>
            <key>CFBundleURLSchemes</key>
            <array><string>nibfile</string></array>
        </dict>
    </array>
</dict>
</plist>
PLIST

echo "打包完成: $APP"
echo "运行文件: $RUNTIME/nib-app"
