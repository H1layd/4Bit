#!/bin/sh
# Builds/4bit.app: the hub (4bit) and the MCP server (4bit-mcp). It finds the engine (templates, crates) by walking up
# from itself, so it stays inside this checkout.
set -e
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT" && cargo build --release -p fourbit-editor
A="$ROOT/Builds/4bit.app/Contents"
rm -rf "$ROOT/Builds/4bit.app"; mkdir -p "$A/MacOS"
cp target/release/4bit target/release/4bit-mcp "$A/MacOS/"
cat > "$A/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict><key>CFBundleExecutable</key><string>4bit</string><key>CFBundleIdentifier</key><string>dev.4bit.hub</string>
<key>CFBundleName</key><string>4bit</string><key>CFBundlePackageType</key><string>APPL</string><key>NSHighResolutionCapable</key><true/></dict></plist>
PLIST
codesign --force --deep --sign - "$ROOT/Builds/4bit.app" >/dev/null 2>&1 || true
echo "-> $ROOT/Builds/4bit.app"
