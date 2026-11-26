#!/usr/bin/env bash
set -euo pipefail

root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
app="$root/dist/app"
mkdir -p "$app"
wasm-pack build --release --target web --out-dir "$app/wasm" --out-name mca_selector_web \
    --no-typescript --no-pack --no-gitignore --no-opt "$root/crates/mca-selector-web" --locked
cp "$root/web/"{index.html,style.css,app.js,world-worker.js,zip.js} "$app/"
