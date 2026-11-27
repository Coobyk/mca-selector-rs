#!/usr/bin/env bash
set -euo pipefail

cd -- "$(dirname -- "${BASH_SOURCE[0]}")"
prefix="${PREFIX:-$HOME/.local}"

install -Dm755 mca-selector "$prefix/bin/mca-selector"
install -Dm644 mca-selector.desktop "$prefix/share/applications/mca-selector.desktop"
install -Dm644 mca-selector.png "$prefix/share/icons/hicolor/1024x1024/apps/mca-selector.png"

echo "Installed MCA Selector to $prefix"
