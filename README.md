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

## Status

Working: world opening (classic and `dimensions/` layouts), map viewing,
selection, export. On the roadmap: import chunks into other worlds, NBT
inspection, go-to coordinates, biome-aware grass tints, and an editable
path field in dialogs.

## License

MIT, same as the original mca-selector.
