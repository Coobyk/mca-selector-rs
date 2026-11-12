//! Canvas painting: tiles, selection/paste overlays, grids, coordinates,
//! structure icons (upstream TileMap.draw + helpers).

use crate::config;
use crate::nbt_editor;
use crate::tilemap::*;
use crate::view::MainView;
use gpui::*;
use mc_core::files;
use mc_core::point::{Point2f, Point2i};
use mc_core::selection::ChunkSet;
use std::sync::Arc;

/// color constants and config colors follow upstream's AARRGGBB convention;
/// gpui's rgba() reads RRGGBBAA, which would leave the alpha byte zero.
fn argb(hex: u32) -> Rgba {
    let [a, r, g, b] = hex.to_be_bytes().map(|b| (b as f32) / 255.0);
    Rgba { r, g, b, a }
}

/// cache key for shaped coordinate labels: edge labels hold a single axis
/// value, grid labels an (x, z) block-coord pair. Shaped lines are
/// position-independent, so one entry serves every frame.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum CoordKey {
    Edge(i64),
    /// first line of a two-line "x,\nz" grid label
    Grid(i64, i64),
    /// second line of a two-line "x,\nz" grid label
    GridB(i64, i64),
}

impl MainView {
    pub(crate) fn paint_map(&mut self, bounds: Bounds<Pixels>, window: &mut Window, _cx: &mut App) {
        self.map.canvas_origin = Point2f::new(f32::from(bounds.origin.x), f32::from(bounds.origin.y));
        self.map.width = f32::from(bounds.size.width);
        self.map.height = f32::from(bounds.size.height);
        let now = std::time::Instant::now();
        let dt = now.saturating_duration_since(self.last_frame);
        self.last_frame = now;

        // gpui has no window-blur event — poll is_window_active per frame.
        // Releasing held pan keys while unfocused is the closest equivalent
        // of upstream's releaseAllKeys on focus loss.
        if !window.is_window_active() {
            self.pressed_keys = 0;
            self.pan_accum = std::time::Duration::ZERO;
        }

        // held-key pan runs before the update gate so a pan step and the
        // tile requests for the regions it exposes land in the same frame.
        self.held_key_pan(dt, window);

        // frame-paced tile dispatch: scroll/zoom/view changes arm update_now
        // per input event; running the O(visible-regions) update once here
        // coalesces bursts (scroll wheels can fire many events per frame).
        if self.update_now && self.zoom_settle.is_none() {
            self.map.update();
            self.update_now = false;
        }

        let scale = self.map.scale;
        // read-guard instead of config::world(): world() clones the whole
        // WorldState (dirs, dimensions, cfg) and this ran every frame.
        let show_empty = self.map.show_nonexistent_regions
            && config::WORLD.read().unwrap().as_ref()
                .map(|w| w.cfg.show_nonexistent_regions).unwrap_or(true);
        // upstream draws the empty tile image under every unloaded/missing
        // region — one fullscreen quad does the same job. EMPTY_COLOR is
        // opaque and covers the same bounds, so the black clear is only
        // needed when the empty-tile fill is off.
        if show_empty {
            window.paint_quad(fill(bounds, argb(EMPTY_COLOR)));
        } else {
            window.paint_quad(fill(bounds, rgba(0x000000ff)));
        }

        let (vmin, vmax) = self.map.visible_region_bounds(Point2f::ZERO);

        // -- tile pass ---------------------------------------------------------
        // upstream draws only tiles whose rect intersects the canvas. Iterate
        // the bounded tile map instead of the visible region grid: at max
        // zoom-out the grid is ~32k cells while tile count is capped by
        // MAX_TILE_REQUESTS, and hash lookups dominate the per-frame cost.
        // Collect + sort so paint order is identical to the rx/rz raster
        // scan (x-major, z inner).
        let tile_px = TILE_SIZE as f32 / scale;
        {
            let map = &self.map;
            let buf = &mut self.visible_tiles_buf;
            let paste_cache = &mut self.paste_overlay_cache;

            buf.clear();
            buf.extend(map.tiles.values().filter_map(|t| {
                let l = t.location;
                // same region range the raster loop visited — truncating
                // int math can exclude a sub-pixel sliver at negative offsets,
                // so keep the check for pixel-identical output.
                if t.image.is_some()
                    && l.x >= vmin.x
                    && l.x <= vmax.x
                    && l.z >= vmin.z
                    && l.z <= vmax.z
                {
                    Some((l.x, l.z, l.as_long()))
                } else {
                    None
                }
            }));
            buf.sort_unstable();

            if map.pasted.is_none() {
                *paste_cache = None;
            }
            // config reads held for the whole pass — the per-tile clones of
            // the BTreeMaps were visible in profiles.
            let g_guard = if map.pasted.is_some() || map.show_structure_icons || !map.selection.is_empty() {
                Some(config::GLOBAL.read().unwrap())
            } else {
                None
            };
            let g_cfg = g_guard.as_ref().and_then(|g| g.as_ref());
            let has_overlay = map.overlay.is_some();
            for &(rx, rz, key) in buf.iter() {
                // block-space position -> screen px, same convention as the
                // grids and coordinates below (offset is in world blocks).
                let ox = (rx as f32 * TILE_SIZE as f32 - map.offset.x) / scale;
                let oz = (rz as f32 * TILE_SIZE as f32 - map.offset.z) / scale;
                if ox + tile_px <= 0.0
                    || oz + tile_px <= 0.0
                    || ox >= map.width
                    || oz >= map.height
                    || tile_px < 0.5
                {
                    continue;
                }
                let b = Bounds {
                    origin: bounds.origin + point(px(ox), px(oz)),
                    size: size(px(tile_px), px(tile_px)),
                };
                let Some(tile) = map.tiles.get(&key) else { continue };
                if let Some(ri) = tile.render_image.clone() {
                    let _ = window.paint_image(b, px(0.).into(), ri, 0, false);
                }
                if has_overlay {
                    if let Some(oi) = tile.overlay_render_image.clone() {
                        let _ = window.paint_image(b, px(0.).into(), oi, 0, false);
                    }
                }
            }

            // -- selection + paste-ghost pass -----------------------------------
            // upstream draws both for every VISIBLE region — not just tiles
            // with a finished image. Region state follows TileImage.draw: a
            // region in the tiles map gets its region/chunk overlay, a region
            // outside it fills only when the selection is inverted.
            let sel = &map.selection;
            if !sel.is_empty() || map.pasted.is_some() {
                let region_color = g_cfg.as_ref().map(|g| g.region_selection_color.to_argb()).unwrap_or(0);
                let region_rect = |rx: i32, rz: i32| -> Option<Bounds<Pixels>> {
                    let ox = (rx as f32 * TILE_SIZE as f32 - map.offset.x) / scale;
                    let oz = (rz as f32 * TILE_SIZE as f32 - map.offset.z) / scale;
                    if ox + tile_px <= 0.0
                        || oz + tile_px <= 0.0
                        || ox >= map.width
                        || oz >= map.height
                        || tile_px < 0.5
                    {
                        None
                    } else {
                        Some(Bounds {
                            origin: bounds.origin + point(px(ox), px(oz)),
                            size: size(px(tile_px), px(tile_px)),
                        })
                    }
                };
                if !sel.is_empty() {
                    let draw_sel = |rx: i32, rz: i32, window: &mut Window| {
                        let Some(b) = region_rect(rx, rz) else { return };
                        let key = Point2i::new(rx, rz).as_long();
                        if !map.tiles.contains_key(&key) {
                            if sel.is_inverted() {
                                window.paint_quad(fill(b, argb(region_color)));
                            }
                        } else if sel.is_region_selected(key) {
                            window.paint_quad(fill(b, argb(region_color)));
                        } else if sel.is_any_chunk_in_region_selected(key) {
                            if let Some(ov) =
                                marked_chunks_image(Point2i::new(rx, rz), sel, map.selection_version)
                            {
                                let _ = window.paint_image(b, px(0.).into(), ov, 0, false);
                            }
                        }
                    };
                    if sel.is_inverted() {
                        // inverted selection paints every visible region
                        for rx in vmin.x..=vmax.x {
                            for rz in vmin.z..=vmax.z {
                                draw_sel(rx, rz, window);
                            }
                        }
                    } else {
                        // non-inverted: only regions present in the selection
                        // map can draw — iterate it clipped to the viewport.
                        for (key, _) in sel.regions() {
                            let r = Point2i::from_long(key);
                            if r.x >= vmin.x && r.x <= vmax.x && r.z >= vmin.z && r.z <= vmax.z {
                                draw_sel(r.x, r.z, window);
                            }
                        }
                    }
                }
                if let Some(p) = &map.pasted {
                    // ghost images are keyed by TARGET region — one image per
                    // region the paste touches, painted at its own rect for
                    // every visible region regardless of tile state.
                    let color = g_cfg.as_ref().unwrap().paste_chunks_color.to_argb();
                    let hit = matches!(paste_cache.as_ref(),
                        Some((px_, pz_, c, dir, _))
                            if *px_ == p.offset.x && *pz_ == p.offset.z
                                && *c == color && *dir == p.dir);
                    if !hit {
                        *paste_cache = Some((
                            p.offset.x,
                            p.offset.z,
                            color,
                            p.dir.clone(),
                            build_paste_overlay(p, color),
                        ));
                    }
                    if let Some((_, _, _, _, imgs)) = paste_cache.as_ref() {
                        for (&key, img) in imgs.iter() {
                            let r = Point2i::from_long(key);
                            if r.x < vmin.x || r.x > vmax.x || r.z < vmin.z || r.z > vmax.z {
                                continue;
                            }
                            if let Some(b) = region_rect(r.x, r.z) {
                                let _ = window.paint_image(b, px(0.).into(), img.clone(), 0, false);
                            }
                        }
                    }
                }
            }
        }

        self.paint_grids(bounds, vmin, vmax, window);

        // upstream draws structure icons after the grids, gated by the
        // header-only zoom level; icon size is a constant screen-px size.
        let header_zoom = config::WORLD.read().unwrap().as_ref()
            .map(|w| w.cfg.render_header_only_zoom_level).unwrap_or(16);
        if !self.map.disabled() && self.map.show_structure_icons && scale <= header_zoom as f32 {
            let g_guard = config::GLOBAL.read().unwrap();
            if let Some(g) = g_guard.as_ref() {
                let icon_size_cfg = config::WORLD.read().unwrap().as_ref()
                    .map(|w| w.cfg.structure_icon_size).unwrap_or(32);
                for &(_, _, key) in self.visible_tiles_buf.iter() {
                    if let Some(st) = self.map.tiles.get(&key).and_then(|t| t.structures.as_ref()) {
                        paint_structure_icons(
                            &mut self.icon_cache,
                            &self.map,
                            st,
                            &g.structure_icons,
                            icon_size_cfg,
                            scale,
                            window,
                        );
                    }
                }
            }
        }

        // coordinates (upstream TileMap.draw): chunk coords while zoomed in
        // (scale < 1.2), region coords otherwise. EDGE-style X labels are
        // rotated 90° upstream — gpui can't rotate text, so they stay
        // horizontal here (known divergence).
        if self.map.show_coordinates {
            let ts = window.text_system().clone();
            let style = self.map.coordinate_style;
            let zoom = get_zoom_level(scale);
            let run = [TextRun {
                len: usize::MAX,
                font: Font {
                    family: "Sans".into(),
                    features: FontFeatures::default(),
                    weight: FontWeight::NORMAL,
                    style: FontStyle::Normal,
                    fallbacks: None,
                },
                color: argb(COORDINATES_COLOR).into(),
                background_color: None,
                underline: None,
                strikethrough: None,
            }];
            let coord_cache = &mut self.coord_line_cache;
            // label text repeats across frames; shaping is the expensive
            // part, so cache ShapedLine by coordinate and only paint here.
            // `text` is lazy so a cache hit doesn't even format the label.
            let mut paint = |key: CoordKey, text: &mut dyn FnMut() -> String, sx: f32, sz: f32| {
                paint_coord(coord_cache, &ts, &run, key, text, sx, sz, window, _cx);
            };
            if scale < 1.2 {
                // upstream drawChunkCoordinates: block coords at aligned
                // chunk intersections, thinning out by 2/4/8/16 with zoom.
                let multiplier: i32 = if scale > 0.8 {
                    16
                } else if scale > 0.4 {
                    8
                } else if scale > 0.2 {
                    4
                } else if scale > 0.1 {
                    2
                } else {
                    1
                };
                let cmin = self.map.offset.to_point2i().block_to_chunk();
                let cmax = self
                    .map
                    .offset
                    .add(self.map.width * scale, self.map.height * scale)
                    .to_point2i()
                    .block_to_chunk();
                let x0 = cmin.x - cmin.x.rem_euclid(multiplier);
                let z0 = cmin.z - cmin.z.rem_euclid(multiplier);
                if style == crate::tilemap::CoordinateStyle::Edge {
                    for cx in (x0..=cmax.x + 1).step_by(multiplier as usize) {
                        let bx = cx as i64 * 16;
                        let sx = (cx as f32 * 16.0 - self.map.offset.x) / scale + 2.0;
                        if sx >= 0.0 && sx <= self.map.width {
                            paint(CoordKey::Edge(bx), &mut || abbrev_coord(bx as i32), sx, 2.0);
                        }
                    }
                    for cz in (z0..=cmax.z + 1).step_by(multiplier as usize) {
                        let bz = cz as i64 * 16;
                        let sz = (cz as f32 * 16.0 - self.map.offset.z) / scale + 2.0;
                        if sz >= 0.0 && sz <= self.map.height {
                            paint(CoordKey::Edge(bz), &mut || abbrev_coord(bz as i32), 2.0, sz);
                        }
                    }
                } else {
                    for cx in (x0..=cmax.x).step_by(multiplier as usize) {
                        for cz in (z0..=cmax.z).step_by(multiplier as usize) {
                            let (bx, bz) = (cx as i64 * 16, cz as i64 * 16);
                            let sx = (cx as f32 * 16.0 - self.map.offset.x) / scale + 2.0;
                            let sz = (cz as f32 * 16.0 - self.map.offset.z) / scale + 2.0;
                            if sx > self.map.width || sz > self.map.height {
                                continue;
                            }
                            paint_coord_pair(coord_cache, &ts, &run, bx, bz, sx, sz, window, _cx);
                        }
                    }
                }
            } else {
                // upstream drawRegionCoordinates
                let edge = style == crate::tilemap::CoordinateStyle::Edge
                    || (style == crate::tilemap::CoordinateStyle::Dynamic && scale > 30.0);
                if edge {
                    // edge labels: x along top, z along left
                    let mul = ((zoom / 4).max(1) * 512) as i64;
                    // bx % mul == 0 <=> rx % (mul/512) == 0: step the multiples
                    // directly instead of scanning every visible region.
                    let step = (mul / 512) as i32;
                    let x0 = vmin.x - vmin.x.rem_euclid(step);
                    for rx in (x0..=vmax.x + 1).step_by(step as usize) {
                        let bx = rx as i64 * 512;
                        let sx = (rx as f32 * 512.0 - self.map.offset.x) / scale + 2.0;
                        if sx >= 0.0 && sx <= self.map.width {
                            paint(CoordKey::Edge(bx), &mut || abbrev_coord(bx as i32), sx, 2.0);
                        }
                    }
                    let z0 = vmin.z - vmin.z.rem_euclid(step);
                    for rz in (z0..=vmax.z + 1).step_by(step as usize) {
                        let bz = rz as i64 * 512;
                        let sz = (rz as f32 * 512.0 - self.map.offset.z) / scale + 2.0;
                        if sz >= 0.0 && sz <= self.map.height {
                            paint(CoordKey::Edge(bz), &mut || abbrev_coord(bz as i32), 2.0, sz);
                        }
                    }
                } else {
                    let multiplier = if scale > 7.0 || style == crate::tilemap::CoordinateStyle::Grid {
                        (zoom / 2).max(1)
                    } else {
                        1
                    };
                    // two-line "x,\nz" labels at aligned region intersections
                    let rx0 = vmin.x - vmin.x.rem_euclid(multiplier);
                    let rz0 = vmin.z - vmin.z.rem_euclid(multiplier);
                    for rx in (rx0..=vmax.x).step_by(multiplier as usize) {
                        for rz in (rz0..=vmax.z).step_by(multiplier as usize) {
                            let (bx, bz) = (rx as i64 * 512, rz as i64 * 512);
                            let sx = (rx as f32 * 512.0 - self.map.offset.x) / scale + 2.0;
                            let sz = (rz as f32 * 512.0 - self.map.offset.z) / scale + 2.0;
                            if sx > self.map.width || sz > self.map.height {
                                continue;
                            }
                            paint_coord_pair(coord_cache, &ts, &run, bx, bz, sx, sz, window, _cx);
                        }
                    }
                    // edge-guard: one extra label row above / column left of
                    // the first aligned cell, so labels stay visible while
                    // panning (upstream drawRegionCoordinates tail pass).
                    let rzg = rz0 - multiplier;
                    for rx in ((rx0 - multiplier)..=vmax.x).step_by(multiplier as usize) {
                        let (bx, bz) = (rx as i64 * 512, rzg as i64 * 512);
                        let sx = (rx as f32 * 512.0 - self.map.offset.x) / scale + 2.0;
                        let sz = (rzg as f32 * 512.0 - self.map.offset.z) / scale + 2.0;
                        if sx > self.map.width {
                            continue;
                        }
                        paint_coord_pair(coord_cache, &ts, &run, bx, bz, sx, sz, window, _cx);
                    }
                    let rxg = rx0 - multiplier;
                    for rz in (rz0..=vmax.z).step_by(multiplier as usize) {
                        let (bx, bz) = (rxg as i64 * 512, rz as i64 * 512);
                        let sx = (rxg as f32 * 512.0 - self.map.offset.x) / scale + 2.0;
                        let sz = (rz as f32 * 512.0 - self.map.offset.z) / scale + 2.0;
                        if sz > self.map.height {
                            continue;
                        }
                        paint_coord_pair(coord_cache, &ts, &run, bx, bz, sx, sz, window, _cx);
                    }
                }
            }
        }
    }

    fn paint_grids(&mut self, bounds: Bounds<Pixels>, rmin: Point2i, rmax: Point2i, window: &mut Window) {
        let scale = self.map.scale;
        if self.map.show_chunk_grid && scale <= CHUNK_GRID_SCALE {
            for rx in rmin.x..=rmax.x + 1 {
                for i in 0..32 {
                    let sx = px(((rx * 512 + i * 16) as f32 - self.map.offset.x) / scale);
                    if sx >= px(0.) && sx <= bounds.size.width {
                        window.paint_quad(fill(
                            Bounds { origin: point(sx + bounds.origin.x, bounds.origin.y), size: size(px(GRID_LINE_WIDTH), bounds.size.height) },
                            argb(CHUNK_GRID_COLOR),
                        ));
                    }
                }
            }
            for rz in rmin.z..=rmax.z + 1 {
                for i in 0..32 {
                    let sz = px(((rz * 512 + i * 16) as f32 - self.map.offset.z) / scale);
                    if sz >= px(0.) && sz <= bounds.size.height {
                        window.paint_quad(fill(
                            Bounds { origin: point(bounds.origin.x, sz + bounds.origin.y), size: size(bounds.size.width, px(GRID_LINE_WIDTH)) },
                            argb(CHUNK_GRID_COLOR),
                        ));
                    }
                }
            }
        }
        if self.map.show_region_grid {
            for rx in rmin.x..=rmax.x + 1 {
                let sx = px((rx as f32 * 512.0 - self.map.offset.x) / scale);
                if sx >= px(0.) && sx <= bounds.size.width {
                    window.paint_quad(fill(
                        Bounds { origin: point(sx + bounds.origin.x, bounds.origin.y), size: size(px(GRID_LINE_WIDTH), bounds.size.height) },
                        argb(REGION_GRID_COLOR),
                    ));
                }
            }
            for rz in rmin.z..=rmax.z + 1 {
                let sz = px((rz as f32 * 512.0 - self.map.offset.z) / scale);
                if sz >= px(0.) && sz <= bounds.size.height {
                    window.paint_quad(fill(
                        Bounds { origin: point(bounds.origin.x, sz + bounds.origin.y), size: size(bounds.size.width, px(GRID_LINE_WIDTH)) },
                        argb(REGION_GRID_COLOR),
                    ));
                }
            }
        }
    }
}

/// paint one coordinate label, reusing the cached ShapedLine when the same
/// coordinate was shaped before. `text` runs only on a cache miss.
#[allow(clippy::too_many_arguments)]
fn paint_coord(
    cache: &mut std::collections::HashMap<CoordKey, ShapedLine>,
    ts: &WindowTextSystem,
    run: &[TextRun],
    key: CoordKey,
    text: &mut dyn FnMut() -> String,
    sx: f32,
    sz: f32,
    window: &mut Window,
    cx: &mut App,
) {
    let line = match cache.get(&key) {
        Some(l) => l,
        None => {
            // bound the cache: a panning session would otherwise accumulate
            // an entry per coordinate ever shown.
            if cache.len() >= 1024 {
                cache.clear();
            }
            let l = ts.shape_line(SharedString::from(text()), px(12.), run, None);
            cache.entry(key).or_insert(l)
        }
    };
    let _ = line.paint(window.element_offset() + point(px(sx), px(sz)), px(14.), window, cx);
}

/// upstream two-line "x,\nz" grid label — two cached ShapedLines stacked.
#[allow(clippy::too_many_arguments)]
fn paint_coord_pair(
    cache: &mut std::collections::HashMap<CoordKey, ShapedLine>,
    ts: &WindowTextSystem,
    run: &[TextRun],
    bx: i64,
    bz: i64,
    sx: f32,
    sz: f32,
    window: &mut Window,
    cx: &mut App,
) {
    paint_coord(cache, ts, run, CoordKey::Grid(bx, bz), &mut || {
        format!("{},", abbrev_coord(bx as i32))
    }, sx, sz, window, cx);
    paint_coord(cache, ts, run, CoordKey::GridB(bx, bz), &mut || {
        abbrev_coord(bz as i32)
    }, sx, sz + 11.0, window, cx);
}

/// upstream TileMap.drawStructureIcons / TileImage.drawStructures: first
/// enabled structure icon per chunk, centered on the chunk. Icons are
/// pre-rendered at structureIconSize pixels upstream — a constant screen
/// size, not scaled by the view. Free function so the tile loop can hold an
/// immutable borrow of `map` while icons resolve through the mutable cache.
fn paint_structure_icons(
    icon_cache: &mut std::collections::HashMap<String, Arc<RenderImage>>,
    map: &TileMapState,
    structures: &std::collections::HashMap<i64, Vec<String>>,
    structure_icons: &std::collections::BTreeMap<String, bool>,
    icon_size_cfg: i32,
    scale: f32,
    window: &mut Window,
) {
    let registry = mc_core::structures::structures();
    let chunk_size = 16.0 / scale;
    let icon_size = icon_size_cfg as f32;
    let mut canon = String::with_capacity(32);
    for (chunk_l, names) in structures {
        let chunk = Point2i::from_long(*chunk_l);
        let off = chunk
            .chunk_to_block()
            .to_point2f()
            .sub(map.offset)
            .mul(1.0 / scale);
        let icon_l = off.x + (chunk_size - icon_size) / 2.0;
        let icon_t = off.z + (chunk_size - icon_size) / 2.0;
        // cull chunks whose icon rect is fully off-screen — paint_image
        // would clip it away anyway, so skip the per-name config lookups.
        if icon_l + icon_size <= 0.0
            || icon_t + icon_size <= 0.0
            || icon_l >= map.width
            || icon_t >= map.height
        {
            continue;
        }
        for name in names {
            // SelectStructures submenu persists per-type toggles
            if structure_icons.get(name).copied().unwrap_or(true) == false {
                continue;
            }
            let Some(sd) = registry.icons.get(name) else { continue };
            if sd.max_scale < scale {
                continue;
            }
            canon.clear();
            canon.push_str("minecraft:");
            canon.push_str(&sd.name);
            if !structure_icons.get(canon.as_str()).copied().unwrap_or(true) {
                continue;
            }
            if let Some(icon) = load_icon(icon_cache, &sd.icon) {
                let b = Bounds {
                    origin: point(
                        px(icon_l + map.canvas_origin.x),
                        px(icon_t + map.canvas_origin.z),
                    ),
                    size: size(px(icon_size), px(icon_size)),
                };
                if b.size.width >= px(0.5) {
                    let _ = window.paint_image(b, px(0.).into(), icon, 0, false);
                }
            }
            break;
        }
    }
}

fn load_icon(
    icon_cache: &mut std::collections::HashMap<String, Arc<RenderImage>>,
    icon: &str,
) -> Option<Arc<RenderImage>> {
    if let Some(i) = icon_cache.get(icon) {
        return Some(i.clone());
    }
    let bytes = match icon {
            "abandoned_camp" => include_bytes!("../resources/img/structure/abandoned_camp.png").as_slice(),
            "blaze" => include_bytes!("../resources/img/structure/blaze.png").as_slice(),
            "boat" => include_bytes!("../resources/img/structure/boat.png").as_slice(),
            "brush" => include_bytes!("../resources/img/structure/brush.png").as_slice(),
            "chest" => include_bytes!("../resources/img/structure/chest.png").as_slice(),
            "chest_minecart" => include_bytes!("../resources/img/structure/chest_minecart.png").as_slice(),
            "desert_village" => include_bytes!("../resources/img/structure/desert_village.png").as_slice(),
            "drowned" => include_bytes!("../resources/img/structure/drowned.png").as_slice(),
            "ender_pearl" => include_bytes!("../resources/img/structure/ender_pearl.png").as_slice(),
            "jungle_temple" => include_bytes!("../resources/img/structure/jungle_temple.png").as_slice(),
            "more" => include_bytes!("../resources/img/structure/more.png").as_slice(),
            "nether_portal" => include_bytes!("../resources/img/structure/nether_portal.png").as_slice(),
            "ocean_monument" => include_bytes!("../resources/img/structure/ocean_monument.png").as_slice(),
            "piglin_brute" => include_bytes!("../resources/img/structure/piglin_brute.png").as_slice(),
            "pillager" => include_bytes!("../resources/img/structure/pillager.png").as_slice(),
            "plains_village" => include_bytes!("../resources/img/structure/plains_village.png").as_slice(),
            "pyramid" => include_bytes!("../resources/img/structure/pyramid.png").as_slice(),
            "red_x" => include_bytes!("../resources/img/structure/red_x.png").as_slice(),
            "savanna_village" => include_bytes!("../resources/img/structure/savanna_village.png").as_slice(),
            "shulker" => include_bytes!("../resources/img/structure/shulker.png").as_slice(),
            "skull" => include_bytes!("../resources/img/structure/skull.png").as_slice(),
            "snowball" => include_bytes!("../resources/img/structure/snowball.png").as_slice(),
            "snowy_village" => include_bytes!("../resources/img/structure/snowy_village.png").as_slice(),
            "soul_lantern" => include_bytes!("../resources/img/structure/soul_lantern.png").as_slice(),
            "swamp_hut" => include_bytes!("../resources/img/structure/swamp_hut.png").as_slice(),
            "taiga_village" => include_bytes!("../resources/img/structure/taiga_village.png").as_slice(),
            "trial_chambers" => include_bytes!("../resources/img/structure/trial_chambers.png").as_slice(),
            "woodland_mansion" => include_bytes!("../resources/img/structure/woodland_mansion.png").as_slice(),
            _ => return None,
        };
    let mut img = image::load_from_memory(bytes).ok()?.to_rgba8();
    for c in img.chunks_exact_mut(4) {
        c.swap(0, 2); // blade atlas is Bgra8Unorm
    }
    let r = Arc::new(RenderImage::new(smallvec::SmallVec::from_elem(image::Frame::new(img), 1)));
    icon_cache.insert(icon.to_string(), r.clone());
    Some(r)
}

/// pasted-chunks ghost images keyed by target region long: every chunk of
/// the copied selection offset by p.offset, bucketed into per-region
/// ChunkSets and rendered to 32x32 images.
fn build_paste_overlay(
    p: &Pasted,
    color: u32,
) -> std::collections::HashMap<i64, Arc<RenderImage>> {
    let mut sets: std::collections::HashMap<i64, ChunkSet> = std::collections::HashMap::new();
    {
        let mut put = |pos: Point2i| {
            let region = pos.chunk_to_region();
            let first = region.region_to_chunk();
            sets.entry(region.as_long())
                .or_default()
                .set(((pos.z - first.z) * 32 + (pos.x - first.x)) as usize);
        };
        if let Some(sel) = &p.selection {
            for (region_l, _) in sel.regions() {
                let base = Point2i::from_long(region_l).region_to_chunk();
                match sel.selected_chunks(Point2i::from_long(region_l)) {
                    Some(Some(c)) => {
                        for i in c.iter() {
                            put(base.add_p(Point2i::from_chunk_index(i as i32)).add_p(p.offset));
                        }
                    }
                    Some(None) | None => {
                        for i in 0..1024i32 {
                            put(base.add_p(Point2i::from_chunk_index(i)).add_p(p.offset));
                        }
                    }
                }
            }
        } else {
            // no selection data: all chunks in all copied regions are pasted.
            // build from the region file names present in the clipboard dir.
            if let Ok(rd) = std::fs::read_dir(&p.dir) {
                for e in rd.flatten() {
                    let name = e.file_name().to_string_lossy().to_string();
                    if let Some(r) = files::parse_mca_file_name(&name) {
                        let base = r.region_to_chunk();
                        for i in 0..1024i32 {
                            put(base.add_p(Point2i::from_chunk_index(i)).add_p(p.offset));
                        }
                    }
                }
            }
        }
    }
    let mut out = std::collections::HashMap::with_capacity(sets.len());
    let mut pxa = [0u32; 32 * 32];
    for (k, cs) in sets {
        pxa.iter_mut().for_each(|v| *v = 0);
        for i in cs.iter() {
            pxa[i] = color;
        }
        if let Some(img) = paste_argb_image(&pxa) {
            out.insert(k, img);
        }
    }
    out
}

/// static nbt type icons (img/nbt/*.png), lazily decoded once.
pub(crate) fn nbt_icon(type_id: u8) -> Option<Arc<RenderImage>> {
    static CACHE: std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<u8, Arc<RenderImage>>>> =
        std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    if let Some(i) = CACHE.lock().unwrap().get(&type_id) {
        return Some(i.clone());
    }
    let name = nbt_editor::icon_name(type_id);
    let bytes: &[u8] = match name {
        "byte" => include_bytes!("../resources/img/nbt/byte.png"),
        "short" => include_bytes!("../resources/img/nbt/short.png"),
        "int" => include_bytes!("../resources/img/nbt/int.png"),
        "long" => include_bytes!("../resources/img/nbt/long.png"),
        "float" => include_bytes!("../resources/img/nbt/float.png"),
        "double" => include_bytes!("../resources/img/nbt/double.png"),
        "byte_array" => include_bytes!("../resources/img/nbt/byte_array.png"),
        "int_array" => include_bytes!("../resources/img/nbt/int_array.png"),
        "long_array" => include_bytes!("../resources/img/nbt/long_array.png"),
        "string" => include_bytes!("../resources/img/nbt/string.png"),
        "list" => include_bytes!("../resources/img/nbt/list.png"),
        "compound" => include_bytes!("../resources/img/nbt/compound.png"),
        _ => return None,
    };
    let mut img = image::load_from_memory(bytes).ok()?.to_rgba8();
    for c in img.chunks_exact_mut(4) {
        c.swap(0, 2); // blade atlas is Bgra8Unorm
    }
    let r = Arc::new(RenderImage::new(smallvec::SmallVec::from_elem(image::Frame::new(img), 1)));
    CACHE.lock().unwrap().insert(type_id, r.clone());
    Some(r)
}

/// selection overlay cache: keyed by (region, selection_version)
static OVERLAY_CACHE: std::sync::LazyLock<std::sync::Mutex<(u64, std::collections::HashMap<i64, Arc<RenderImage>>)>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new((0, std::collections::HashMap::new())));

fn paste_argb_image(px: &[u32; 1024]) -> Option<Arc<RenderImage>> {
    crate::tilemap::argb_to_render_image(px, 32)
}

/// 32x32 chunk-selection overlay for a region with partially selected
/// chunks (whole-region selection paints a fill quad instead — see the
/// selection pass in paint_map). Mirrors upstream markedChunksImage.
fn marked_chunks_image(region: Point2i, sel: &mc_core::selection::Selection, version: u64) -> Option<Arc<RenderImage>> {
    let key = region.as_long();
    if !sel.is_any_chunk_in_region_selected(key) || sel.is_region_selected(key) {
        return None;
    }
    let mut cache = OVERLAY_CACHE.lock().unwrap();
    if cache.0 != version {
        cache.1.clear();
        cache.0 = version;
    }
    if let Some(i) = cache.1.get(&key) {
        return Some(i.clone());
    }
    let g = config::global();
    let mut px = [0u32; 32 * 32];
    if let Some(Some(cs)) = sel.selected_chunks(region) {
        let color = g.chunk_selection_color.to_argb();
        for i in cs.iter() {
            px[i] = color;
        }
    }
    let r = crate::tilemap::argb_to_render_image(&px, 32)?;
    cache.1.insert(key, r.clone());
    Some(r)
}

/// upstream TextHelper.abbreviateCoordinate: K/M/B suffix over 100k.
fn abbrev_coord(n: i32) -> String {
    let abs = (n as i64).abs();
    if abs < 100_000 {
        return n.to_string();
    }
    let (value, suffix) = if abs >= 1_000_000_000 {
        (n as f64 / 1_000_000_000.0, "B")
    } else if abs >= 1_000_000 {
        (n as f64 / 1_000_000.0, "M")
    } else {
        (n as f64 / 1_000.0, "K")
    };
    format!("{:.2}{}", value, suffix)
}
