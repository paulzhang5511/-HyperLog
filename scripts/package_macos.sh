#!/usr/bin/env bash
# 生成可分发 macOS .app 包（M10，提前交付 G11 打包项）。
# 用法：bash scripts/package_macos.sh [x86_64|aarch64]
#   参数为 Rust 目标架构（缺省用当前机器原生架构）。
# 产物：dist/HyperLog.app（可拖入 /Applications）+ dist/HyperLog-macos-<arch>.zip
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

APP_NAME="HyperLog"
BIN_NAME="hyper-log"
VERSION="$(grep -m1 '^version' Cargo.toml | sed -E 's/.*"([0-9.]+)".*/\1/')"
DEST="dist/${APP_NAME}.app"
MACOS_DIR="${DEST}/Contents/MacOS"

# 目标架构：显式传入 > 环境变量 > 本机原生（uname -m 归一化）。
# 支持两种写法：短名（aarch64 / x86_64）或完整 rust target（aarch64-apple-darwin 等）。
ARCH="${1:-${TARGET_ARCH:-}}"
if [ -z "$ARCH" ]; then
    case "$(uname -m)" in
        arm64|aarch64) ARCH="aarch64" ;;
        x86_64) ARCH="x86_64" ;;
        *) echo "无法识别本机架构: $(uname -m)" >&2; exit 1 ;;
    esac
fi

RUST_TARGET=""
case "$ARCH" in
    aarch64|arm64|aarch64-apple-darwin) RUST_TARGET="aarch64-apple-darwin"; ZIP_ARCH="arm" ;;
    x86_64|intel|x86_64-apple-darwin)   RUST_TARGET="x86_64-apple-darwin";  ZIP_ARCH="intel" ;;
    *) echo "不支持的架构: $ARCH（仅支持 aarch64 / x86_64）" >&2; exit 1 ;;
esac

echo "==> 构建 release 二进制（target: ${RUST_TARGET}）"
if [ -n "$RUST_TARGET" ] && [ "$RUST_TARGET" != "$(rustc -vV | sed -n 's/^host: //p')" ]; then
    # 交叉编译：先确保目标已安装。
    rustup target add "$RUST_TARGET"
    cargo build --release --target "$RUST_TARGET"
    BIN_SRC="target/${RUST_TARGET}/release/${BIN_NAME}"
else
    cargo build --release
    BIN_SRC="target/release/${BIN_NAME}"
fi

echo "==> 组装 ${DEST}"
rm -rf "${DEST}"
mkdir -p "${MACOS_DIR}"

cp "$BIN_SRC" "${MACOS_DIR}/${BIN_NAME}"
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

ZIP_NAME="${APP_NAME}-macos-${ZIP_ARCH}.zip"
echo "==> 打包 ${ZIP_NAME}"
( cd dist && rm -f "${ZIP_NAME}" && zip -r "${ZIP_NAME}" "${APP_NAME}.app" )

echo "==> 完成"
echo "    app : ${DEST}"
echo "    zip : dist/${ZIP_NAME}"
