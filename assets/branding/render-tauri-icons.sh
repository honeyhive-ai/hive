#!/bin/zsh
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/../.." && pwd)"
BRAND_DIR="$ROOT_DIR/assets/branding"
ICON_DIR="$ROOT_DIR/app/icons"
TMP_DIR="${TMPDIR:-/tmp}/hive-brand-iconset"
ICONSET_DIR="$TMP_DIR/hive.iconset"

rm -rf "$TMP_DIR"
mkdir -p "$ICON_DIR" "$ICONSET_DIR"

node "$BRAND_DIR/generate-brand-assets.mjs"
# Render the 1024 master directly to RGBA (transparent corners). Quick Look
# flattens SVG transparency onto white, which left white tips outside the
# squircle — so we rasterize in-process instead.
node "$BRAND_DIR/generate-brand-assets.mjs" --png "$BRAND_DIR/hive-app-icon-1024.png" 1024

if [[ ! -f "$BRAND_DIR/hive-app-icon-1024.png" ]]; then
  printf '%s\n' "Failed to render hive-app-icon-1024.png"
  exit 1
fi

cp "$BRAND_DIR/hive-app-icon-1024.png" "$ICON_DIR/icon.png"
# macOS menu-bar template: tray-icon fits it to 18pt, so @1x=18px, @2x=36px
# (downscaled from the frame-filling 128px master for crispness).
sips -z 36 36 "$BRAND_DIR/hive-tray-template-128.png" --out "$ICON_DIR/trayTemplate@2x.png" >/dev/null
sips -z 18 18 "$BRAND_DIR/hive-tray-template-128.png" --out "$ICON_DIR/trayTemplate.png" >/dev/null

sips -z 32 32 "$BRAND_DIR/hive-app-icon-1024.png" --out "$ICON_DIR/32x32.png" >/dev/null
sips -z 64 64 "$BRAND_DIR/hive-app-icon-1024.png" --out "$ICON_DIR/64x64.png" >/dev/null
sips -z 128 128 "$BRAND_DIR/hive-app-icon-1024.png" --out "$ICON_DIR/128x128.png" >/dev/null
sips -z 256 256 "$BRAND_DIR/hive-app-icon-1024.png" --out "$ICON_DIR/128x128@2x.png" >/dev/null

sips -z 30 30 "$BRAND_DIR/hive-app-icon-1024.png" --out "$ICON_DIR/Square30x30Logo.png" >/dev/null
sips -z 44 44 "$BRAND_DIR/hive-app-icon-1024.png" --out "$ICON_DIR/Square44x44Logo.png" >/dev/null
sips -z 71 71 "$BRAND_DIR/hive-app-icon-1024.png" --out "$ICON_DIR/Square71x71Logo.png" >/dev/null
sips -z 89 89 "$BRAND_DIR/hive-app-icon-1024.png" --out "$ICON_DIR/Square89x89Logo.png" >/dev/null
sips -z 107 107 "$BRAND_DIR/hive-app-icon-1024.png" --out "$ICON_DIR/Square107x107Logo.png" >/dev/null
sips -z 142 142 "$BRAND_DIR/hive-app-icon-1024.png" --out "$ICON_DIR/Square142x142Logo.png" >/dev/null
sips -z 150 150 "$BRAND_DIR/hive-app-icon-1024.png" --out "$ICON_DIR/Square150x150Logo.png" >/dev/null
sips -z 284 284 "$BRAND_DIR/hive-app-icon-1024.png" --out "$ICON_DIR/Square284x284Logo.png" >/dev/null
sips -z 310 310 "$BRAND_DIR/hive-app-icon-1024.png" --out "$ICON_DIR/Square310x310Logo.png" >/dev/null
sips -z 50 50 "$BRAND_DIR/hive-app-icon-1024.png" --out "$ICON_DIR/StoreLogo.png" >/dev/null

sips -z 16 16 "$BRAND_DIR/hive-app-icon-1024.png" --out "$ICONSET_DIR/icon_16x16.png" >/dev/null
sips -z 32 32 "$BRAND_DIR/hive-app-icon-1024.png" --out "$ICONSET_DIR/icon_16x16@2x.png" >/dev/null
sips -z 32 32 "$BRAND_DIR/hive-app-icon-1024.png" --out "$ICONSET_DIR/icon_32x32.png" >/dev/null
sips -z 64 64 "$BRAND_DIR/hive-app-icon-1024.png" --out "$ICONSET_DIR/icon_32x32@2x.png" >/dev/null
sips -z 128 128 "$BRAND_DIR/hive-app-icon-1024.png" --out "$ICONSET_DIR/icon_128x128.png" >/dev/null
sips -z 256 256 "$BRAND_DIR/hive-app-icon-1024.png" --out "$ICONSET_DIR/icon_128x128@2x.png" >/dev/null
sips -z 256 256 "$BRAND_DIR/hive-app-icon-1024.png" --out "$ICONSET_DIR/icon_256x256.png" >/dev/null
sips -z 512 512 "$BRAND_DIR/hive-app-icon-1024.png" --out "$ICONSET_DIR/icon_256x256@2x.png" >/dev/null
sips -z 512 512 "$BRAND_DIR/hive-app-icon-1024.png" --out "$ICONSET_DIR/icon_512x512.png" >/dev/null
cp "$BRAND_DIR/hive-app-icon-1024.png" "$ICONSET_DIR/icon_512x512@2x.png"

if iconutil -c icns "$ICONSET_DIR" -o "$ICON_DIR/icon.icns" 2>/dev/null; then
  printf '%s\n' "Refreshed app/icons/icon.icns"
else
  printf '%s\n' "Warning: iconutil could not build icon.icns from the generated iconset; existing icon.icns was left unchanged."
fi

printf '%s\n' "Rendered SVG/PNG brand assets and refreshed core Tauri PNG/ICNS icons, including tray template assets."
