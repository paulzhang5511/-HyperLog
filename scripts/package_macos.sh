#!/usr/bin/env bash
# 生成可分发 macOS .app 包（M10，提前交付 G11 打包项）。
# 用法：bash scripts/package_macos.sh
# 产物：dist/HyperLog.app（可拖入 /Applications）+ dist/HyperLog-macos.zip
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

APP_NAME="HyperLog"
BIN_NAME="hyper-log"
VERSION="$(grep -m1 '^version' Cargo.toml | sed -E 's/.*"([0-9.]+)".*/\1/')"
DEST="dist/${APP_NAME}.app"
MACOS_DIR="${DEST}/Contents/MacOS"

echo "==> 构建 release 二进制"
cargo build --release

echo "==> 组装 ${DEST}"
rm -rf "${DEST}"
mkdir -p "${MACOS_DIR}"

cp "target/release/${BIN_NAME}" "${MACOS_DIR}/${BIN_NAME}"
chmod +x "${MACOS_DIR}/${BIN_NAME}"

cat > "${DEST}/Contents/Info.plist" <<PLIST_EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>${APP_NAME}</string>
    <key>CFBundleDisplayName</key><string>${APP_NAME}</string>
    <key>CFBundleExecutable</key><string>${BIN_NAME}</string>
    <key>CFBundleIdentifier</key><string>com.hyperlog.app</string>
    <key>CFBundleVersion</key><string>${VERSION}</string>
    <key>CFBundleShortVersionString</key><string>${VERSION}</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
    <key>LSMinimumSystemVersion</key><string>11.0</string>
    <key>NSHighResolutionCapable</key><true/>
    <key>NSPrincipalClass</key><string>NSApplication</string>
</dict>
</plist>
PLIST_EOF

# 尽力移除 quarantine 属性并做 ad-hoc 签名，便于本地双击启动
xattr -dr com.apple.quarantine "${DEST}" 2>/dev/null || true
codesign --force --sign - "${DEST}" 2>/dev/null || true

echo "==> 打包 zip"
( cd dist && rm -f "${APP_NAME}-macos.zip" && zip -r "${APP_NAME}-macos.zip" "${APP_NAME}.app" )

echo "==> 完成"
echo "    app : ${DEST}"
echo "    zip : dist/${APP_NAME}-macos.zip"
