#!/usr/bin/env bash
set -euo pipefail

root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
resolve() {
    case "$1" in
        /*) printf '%s\n' "$1" ;;
        *) printf '%s\n' "$root/$1" ;;
    esac
}
bin="$(resolve "${1:-target/release/mca-selector}")"
out="$(resolve "${2:-dist/macos}")"
version="${3:-0.1.0}"

if [[ ! -x "$bin" ]]; then
    echo "missing executable: $bin" >&2
    exit 1
fi

mkdir -p "$out"
out="$(cd -- "$out" && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

app="$work/MCA Selector.app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$bin" "$app/Contents/MacOS/mca-selector"
chmod +x "$app/Contents/MacOS/mca-selector"
cp "$root/README.md" "$root/LICENSE" "$app/Contents/Resources/"
printf 'APPL????' > "$app/Contents/PkgInfo"

iconset="$work/mca-selector.iconset"
mkdir -p "$iconset"
while read -r name size; do
    sips -z "$size" "$size" "$root/packaging/mca-selector.png" --out "$iconset/$name" >/dev/null
done <<ICONS
icon_16x16.png 16
icon_16x16@2x.png 32
icon_32x32.png 32
icon_32x32@2x.png 64
icon_128x128.png 128
icon_128x128@2x.png 256
icon_256x256.png 256
icon_256x256@2x.png 512
icon_512x512.png 512
icon_512x512@2x.png 1024
ICONS
iconutil -c icns "$iconset" -o "$app/Contents/Resources/mca-selector.icns"

cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>
    <string>MCA Selector</string>
    <key>CFBundleDisplayName</key>
    <string>MCA Selector</string>
    <key>CFBundleExecutable</key>
    <string>mca-selector</string>
    <key>CFBundleIconFile</key>
    <string>mca-selector.icns</string>
    <key>CFBundleIdentifier</key>
    <string>com.coobyk.mca-selector</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>$version</string>
    <key>CFBundleVersion</key>
    <string>$version</string>
    <key>LSMinimumSystemVersion</key>
    <string>11.0</string>
    <key>LSApplicationCategoryType</key>
    <string>public.app-category.utilities</string>
    <key>NSHighResolutionCapable</key>
    <true/>
</dict>
</plist>
PLIST
plutil -lint "$app/Contents/Info.plist" >/dev/null

codesign --force --deep --sign - "$app"

dmg_root="$work/dmg"
mkdir -p "$dmg_root"
cp -R "$app" "$dmg_root/"
ln -s /Applications "$dmg_root/Applications"
cp "$root/README.md" "$root/LICENSE" "$dmg_root/"

dmg="$out/mca-selector-macos-aarch64.dmg"
rm -f "$dmg"
hdiutil create -volname "MCA Selector" -srcfolder "$dmg_root" -ov -format UDZO "$dmg" >/dev/null
