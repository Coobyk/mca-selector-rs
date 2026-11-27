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
out="$(resolve "${2:-dist/linux}")"

if [[ ! -x "$bin" ]]; then
    echo "missing executable: $bin" >&2
    exit 1
fi
if ! command -v zip >/dev/null 2>&1; then
    echo "zip is required" >&2
    exit 1
fi

mkdir -p "$out"
out="$(cd -- "$out" && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

package_dir="$work/zip/mca-selector-linux-x86_64"
mkdir -p "$package_dir"
cp "$bin" "$package_dir/mca-selector"
cp "$root/README.md" "$root/LICENSE" "$package_dir/"
cp "$root/packaging/mca-selector.desktop" "$package_dir/"
cp "$root/packaging/mca-selector-512.png" "$package_dir/mca-selector.png"
cp "$root/packaging/linux-install.sh" "$package_dir/install.sh"
chmod +x "$package_dir/mca-selector" "$package_dir/install.sh"
rm -f "$out/mca-selector-linux-x86_64.zip"
(cd "$work/zip" && zip -qr "$out/mca-selector-linux-x86_64.zip" mca-selector-linux-x86_64)

tools="$work/tools"
mkdir -p "$tools"
download_tool() {
    local url="$1" sha256="$2" dest="$3"
    curl -fL --retry 3 --silent --show-error -o "$dest" "$url"
    printf '%s  %s\n' "$sha256" "$dest" | sha256sum -c - >/dev/null
    chmod +x "$dest"
}

download_tool \
    https://github.com/AppImage/appimagetool/releases/download/1.9.1/appimagetool-x86_64.AppImage \
    ed4ce84f0d9caff66f50bcca6ff6f35aae54ce8135408b3fa33abfc3cb384eb0 \
    "$tools/appimagetool"
download_tool \
    https://github.com/linuxdeploy/linuxdeploy/releases/download/1-alpha-20251107-1/linuxdeploy-x86_64.AppImage \
    c20cd71e3a4e3b80c3483cef793cda3f4e990aca14014d23c544ca3ce1270b4d \
    "$tools/linuxdeploy"

extract_tool() {
    (cd "$work" && "$1" --appimage-extract >/dev/null)
    mv "$work/squashfs-root" "$2"
    chmod +x "$2/AppRun"
}
extract_tool "$tools/linuxdeploy" "$tools/linuxdeploy-root"
extract_tool "$tools/appimagetool" "$tools/appimagetool-root"

appimage_icon="$work/mca-selector.png"
cp "$root/packaging/mca-selector-512.png" "$appimage_icon"

"$tools/linuxdeploy-root/AppRun" \
    --appdir "$work/AppDir" \
    --executable "$bin" \
    --desktop-file "$root/packaging/mca-selector.desktop" \
    --icon-file "$appimage_icon"
ARCH=x86_64 "$tools/appimagetool-root/AppRun" "$work/AppDir" "$out/mca-selector-linux-x86_64.AppImage"
