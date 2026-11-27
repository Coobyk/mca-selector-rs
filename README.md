# mca-selector-rs

A Minecraft region/chunk map editor — a Rust + GPUI rewrite of
[MCA Selector](https://github.com/Querz/mca-selector), with Xaero's World Map
style chunk rendering.

## Features

- Open a Minecraft save folder (modern Anvil, 1.18+, DataVersions up to the
  current snapshot palette formats) and view every dimension as a rendered
  map: top-block colors, water depth blending and north-west height shading.
- Smooth pan (middle/right drag) and zoom (scroll wheel at the cursor), from
  full-world region view down to per-block chunk tiles.
- Select chunks by dragging a rectangle — `Ctrl` adds to the selection,
  `Alt` removes from it.
- Export the selection into a standalone world folder (`region/*.mca` +
  `level.dat`), which can be opened in the app or pasted into a real save.
- Dark chrome after the original: menu bar, toolbar, status bar with block /
  chunk / region coordinates, selection count and zoom level.

## Downloads

Every push to `main` updates the rolling
[nightly release](https://github.com/Coobyk/mca-selector-rs/releases/tag/nightly):

- **Linux:** use `mca-selector-linux-x86_64.AppImage` (`chmod +x` may be
  needed after downloading), or download
  `mca-selector-linux-x86_64.zip` and run its `install.sh` for user-local
  desktop integration (`PREFIX=/custom/path ./install.sh` overrides the
  default `~/.local` prefix). The raw binary remains available for the AUR
  package.
- **Windows:** use `mca-selector-windows-x86_64.exe` directly as a portable
  executable, or download the portable ZIP if you want the README and license
  bundled with it.
- **macOS (Apple Silicon):** open `mca-selector-macos-aarch64.dmg` and drag
  `MCA Selector.app` into `Applications`. The app is ad-hoc signed but not
  notarized, so the first launch may require right-clicking the app and
  choosing **Open**.

## Building

Rust 1.75+ and the usual Linux GUI dependencies are required:

```sh
sudo apt install libxcb1-dev libxkbcommon-dev libxkbcommon-x11-dev \
    libwayland-dev libfontconfig-dev libfreetype-dev \
    mesa-vulkan-drivers   # software rendering works (lavapipe)
cargo build --release
```

A patched `xattr` is vendored under `patches/` (see `[patch.crates-io]` in
the root `Cargo.toml`) because gpui 0.2 pulls an `xattr` version that no
longer compiles against current `libc` on Linux.

## Running

```sh
./target/release/mca-selector <path-to-world-folder>
```

The world is only ever read; exports go to the folder shown in the export
dialog (`MCA_EXPORT_DIR` sets its default).

## Layout

- `crates/mc-core` — Anvil region reading/writing, chunk NBT model, block
  colors and tile rendering. No GUI dependencies; fully unit tested.
- `crates/mca-selector` — the GPUI application: map canvas, selection,
  chrome, background tile loading.
- `crates/mca-selector-web` — WebAssembly bindings for `mc-core`, used by
  the browser editor in `web/`.

## Browser build

With stable Rust and [wasm-pack](https://drager.github.io/wasm-pack/) 0.15.1:

```sh
rustup target add wasm32-unknown-unknown
bash web/build.sh
python3 -m http.server 8000 --directory dist
```

Open [localhost:8000/app/](http://localhost:8000/app/), then choose the root
of a Java Edition save with `level.dat` and Anvil region files. Choose a
dimension and select populated chunks or regions. Export downloads a new
world ZIP containing the selection; **Delete in copy** downloads the world
without those chunks. Unzip the download before opening it. Files are read
in a Web Worker, never uploaded or modified in place. ZIP downloads are
limited to 4 GiB and 65,535 files.

The build output is `dist/app/`. To publish, add that directory to the
existing Pages upload bundle without replacing its root landing page;
serve `/app/` with a trailing slash for relative asset URLs. The GitHub
workflow builds an artifact only; it does not deploy.

## Status

Working: world opening (classic and `dimensions/` layouts), map viewing,
selection, export. On the roadmap: import chunks into other worlds, NBT
inspection, go-to coordinates, biome-aware grass tints, and an editable
path field in dialogs.

## License

MIT, same as the original mca-selector.
