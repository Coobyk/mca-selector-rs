//! Port of tile.TileMap + tile.Tile + tile.ImagePool + the region image
//! generation jobs. Behavior is replicated line-for-line where feasible.

use mc_core::cache;
use mc_core::files;
use mc_core::jobs::{self, Job};
use mc_core::mca::McaFile;
use mc_core::point::{Point2f, Point2i};
use mc_core::render;
use mc_core::selection::Selection;
use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Mutex, OnceLock};

pub const TILE_SIZE: usize = 512;
pub const CHUNK_GRID_SCALE: f32 = 1.5;
pub const TILE_VISIBILITY_THRESHOLD: i32 = 2;

// Tile colors (upstream Tile.* + config defaults)
/// max regions given tile objects / requests per update tick
pub const MAX_TILE_REQUESTS: usize = 8192;
/// max concurrent tile/structure jobs in flight
pub const MAX_IN_FLIGHT_LOADS: usize = 2048;

pub const REGION_GRID_COLOR: u32 = 0x8000_0000; // (0,0,0,0.5)
pub const CHUNK_GRID_COLOR: u32 = 0x80A9_A9A9; // (.6627,.6627,.6627,.5)
pub const COORDINATES_COLOR: u32 = 0xFFFF_FFFF;
pub const EMPTY_COLOR: u32 = 0xFF33_3333; // (0.2,0.2,0.2,1)
pub const GRID_LINE_WIDTH: f32 = 1.0;

pub fn get_zoom_level(scale: f32) -> i32 {
    let s = scale as i32;
    if s <= 1 {
        1
    } else {
        i32::MAX - s.leading_zeros() as i32 + 1 // highestOneBit
    }
}

/// Tile = one region. Image = argb pixels of size 512/zoomLevel.
pub struct Tile {
    pub location: Point2i,
    /// argb buffer + its zoom level (image size = 512/zoom)
    pub image: Option<(Vec<u32>, i32)>,
    pub loaded: bool,
    pub overlay: Option<(Vec<u32>, i32)>,
    pub overlay_loaded: bool,
    pub structures: Option<HashMap<i64, Vec<String>>>,
    pub marked_chunks_image: Option<Vec<u32>>, // 32x32 argb
    /// gpu-ready image, built when the argb result lands
    pub render_image: Option<std::sync::Arc<gpui::RenderImage>>,
    /// gpu-ready 32x32 overlay image
    pub overlay_render_image: Option<std::sync::Arc<gpui::RenderImage>>,
}

impl Tile {
    pub fn new(location: Point2i) -> Self {
        Tile {
            location,
            image: None,
            loaded: false,
            overlay: None,
            overlay_loaded: false,
            structures: None,
            marked_chunks_image: None,
            render_image: None,
            overlay_render_image: None,
        }
    }
    pub fn image_zoom_level(&self) -> i32 {
        self.image.as_ref().map(|(_, z)| *z).unwrap_or(0)
    }
    pub fn is_visible(&self, map: &TileMapState, threshold: i32) -> bool {
        let o = map.offset.to_point2i();
        let min = o.sub_v(threshold * TILE_SIZE as i32, threshold * TILE_SIZE as i32)
            .block_to_region()
            .region_to_block();
        let max = Point2i::new(
            (o.x as f32 + map.width * map.scale) as i32,
            (o.z as f32 + map.height * map.scale) as i32,
        )
        .add(threshold * TILE_SIZE as i32, threshold * TILE_SIZE as i32)
        .block_to_region()
        .region_to_block();
        self.location.x * TILE_SIZE as i32 >= min.x
            && self.location.z * TILE_SIZE as i32 >= min.z
            && self.location.x * TILE_SIZE as i32 <= max.x
            && self.location.z * TILE_SIZE as i32 <= max.z
    }
}

/// Async results channel: worker -> UI.
pub enum TileResult {
    Image {
        region: Point2i,
        image: Option<Vec<u32>>,
        zoom: i32,
        structures: Option<HashMap<i64, Vec<String>>>,
        /// write to disk cache as well
        cache_write: bool,
    },
    Structures {
        region: Point2i,
        structures: Option<HashMap<i64, Vec<String>>>,
    },
    /// 32x32 ARGB overlay image (one pixel per chunk)
    Overlay {
        region: Point2i,
        image: Option<Vec<u32>>,
    },
}

pub static RESULT_TX: OnceLock<Sender<TileResult>> = OnceLock::new();

pub fn result_sender() -> Sender<TileResult> {
    RESULT_TX.get_or_init(|| {
        let (tx, _rx) = channel();
        // never used without install
        tx
    })
    .clone()
}

pub struct TileMapState {
    pub scale: f32, // blocks per screen pixel; higher = more zoomed out
    pub offset: Point2f, // block coords at screen 0,0
    pub width: f32,
    pub height: f32,
    pub tiles: HashMap<i64, Tile>,
    pub tile_priorities: HashMap<i64, i64>,
    pub regions: HashSet<i64>, // region files that exist on disk
    pub selection: Selection,
    pub selected_chunks: i64,
    pub unsaved_selection: bool,
    pub show_chunk_grid: bool,
    pub show_region_grid: bool,
    pub show_coordinates: bool,
    pub show_structure_icons: bool,
    pub show_nonexistent_regions: bool,
    pub coordinate_style: CoordinateStyle,
    pub hovered_block: Option<Point2i>,
    pub disabled: bool,
    /// memory image pool: zoomLevel -> lru region->image
    pub pool: HashMap<i32, HashMap<i64, Vec<u32>>>,
    pub pool_order: HashMap<i32, Vec<i64>>, // insertion order for trim
    pub loading: HashSet<i64>,
    /// region mca cache (visible regions only, maxLoadedFiles)
    pub mca_cache: Vec<Point2i>,
    pub results_rx: Option<Receiver<TileResult>>,
    pub result_tx: Sender<TileResult>,
    /// active overlay parser (overlayPool.parser)
    pub overlay: Option<mc_core::overlay::Overlay>,
    /// configured overlays, sorted by type, terminated by None (overlay list)
    pub overlays: Vec<Option<mc_core::overlay::Overlay>>,
    pub overlay_loading: HashSet<i64>,
}

#[derive(Clone, Copy, PartialEq)]
pub enum CoordinateStyle {
    Dynamic,
    Edge,
    Grid,
}

impl TileMapState {
    pub fn new() -> Self {
        let (tx, rx) = channel();
        let g = crate::config::global();
        TileMapState {
            scale: std::env::var("MCA_SCALE").ok().and_then(|v| v.parse().ok()).unwrap_or(1.0),
            offset: Point2f::new(-400.0, -300.0),
            width: 800.0,
            height: 600.0,
            tiles: HashMap::new(),
            tile_priorities: HashMap::new(),
            regions: HashSet::new(),
            selection: Selection::new(),
            selected_chunks: 0,
            unsaved_selection: false,
            show_chunk_grid: g.show_chunk_grid,
            show_region_grid: g.show_region_grid,
            show_coordinates: g.show_coordinates,
            show_structure_icons: false,
            show_nonexistent_regions: true,
            coordinate_style: CoordinateStyle::Dynamic,
            hovered_block: None,
            disabled: true,
            pool: HashMap::new(),
            pool_order: HashMap::new(),
            loading: HashSet::new(),
            mca_cache: Vec::new(),
            results_rx: Some(rx),
            result_tx: tx,
            overlay: None,
            overlays: vec![None],
            overlay_loading: HashSet::new(),
        }
    }

    pub fn tile_priority(&self, region: Point2i) -> i64 {
        self.tile_priorities
            .get(&region.as_long())
            .copied()
            .unwrap_or(9_999_999)
    }

    /// visible regions rectangle in region coords
    pub fn visible_region_bounds(&self, extra: Point2f) -> (Point2i, Point2i) {
        let min = self.offset.sub(extra).to_point2i().block_to_region();
        let max = self
            .offset
            .sub(extra)
            .add(
                self.width * self.scale,
                self.height * self.scale,
            )
            .to_point2i()
            .block_to_region();
        (min, max)
    }

    /// runOnVisibleRegions — rows, top-left to bottom-right.
    pub fn for_visible_regions(&self, mut f: impl FnMut(Point2i)) {
        let (min, max) = self.visible_region_bounds(Point2f::ZERO);
        for x in min.x..=max.x {
            for z in min.z..=max.z {
                f(Point2i::new(x, z));
            }
        }
    }

    /// spiral iteration center-out with limit (priorities).
    pub fn for_visible_regions_spiral(&self, limit: usize, mut f: impl FnMut(Point2i)) {
        let (min, max) = self.visible_region_bounds(Point2f::ZERO);
        let mid = min
            .region_to_block()
            .add_p(max.region_to_block())
            .div(2)
            .block_to_region()
            .region_to_block()
            .block_to_region();
        let mut dir = 0; // 0 right, 1 down, 2 left, 3 up
        let mut steps = 1;
        let mut x_steps = 0;
        let mut y_steps = 0;
        let mut step = 0;
        let mut x = mid.x;
        let mut y = mid.z;
        let mut count = 0;
        while (x <= max.x || y <= max.z) && (x >= min.x || y >= min.z) {
            for _ in 0..steps * 2 {
                x = mid.x + x_steps;
                y = mid.z + y_steps;
                if x <= max.x && x >= min.x && y <= max.z && y >= min.z {
                    f(Point2i::new(x, y));
                    count += 1;
                    if count == limit {
                        return;
                    }
                }
                match dir {
                    0 => x_steps += 1,
                    1 => y_steps += 1,
                    2 => x_steps -= 1,
                    _ => y_steps -= 1,
                }
                step += 1;
                if step == steps {
                    step = 0;
                    dir = (dir + 1) % 4;
                }
            }
            steps += 1;
        }
    }

    pub fn visible_tiles(&self) -> i32 {
        let (min, max) = self.visible_region_bounds(Point2f::ZERO);
        (max.x - min.x + 1) * (max.z - min.z + 1)
    }

    pub fn go_to(&mut self, x: i32, z: i32) {
        self.offset = Point2f::new(
            x as f32 - self.width * self.scale / 2.0,
            z as f32 - self.height * self.scale / 2.0,
        );
    }

    /// updateScale(oldScale, center) — anchor zoom at `center` (screen px).
    /// Returns true if zoom level changed.
    pub fn update_scale(&mut self, old_scale: f32, center: Point2f) -> bool {
        self.scale = if self.scale < crate::config::MAX_SCALE {
            self.scale.max(crate::config::MIN_SCALE)
        } else {
            crate::config::MAX_SCALE
        };
        if old_scale == self.scale {
            return false;
        }
        let diff = self
            .offset
            .add(center.x * old_scale, center.z * old_scale)
            .sub(self.offset.add(center.x * self.scale, center.z * self.scale));
        self.offset = self.offset.add_p(diff);
        let changed = get_zoom_level(old_scale) != get_zoom_level(self.scale);
        if changed {
            // keep the stale tile images: update() replaces them with an
            // instant downscale or a fresh render — clearing them here makes
            // the map flash black on every wheel tick.
            jobs::pools().clear_queues();
            // dropped jobs never send results; clear the in-flight flags so
            // update() can re-request at the new zoom.
            self.loading.clear();
            self.overlay_loading.clear();
        }
        changed
    }

    pub fn unload_tiles(&mut self, overlay: bool, img: bool) {
        for t in self.tiles.values_mut() {
            if img {
                t.image = None;
                t.render_image = None;
                t.structures = None;
            }
            t.marked_chunks_image = None;
            if overlay {
                t.overlay = None;
            }
            t.loaded = false;
        }
    }

    // -- marking (selection) ---------------------------------------------------

    pub fn mark(&mut self, mouse_x: f32, mouse_y: f32, mark: bool, first: Point2f, paint_mode: bool) {
        let mut first = first;
        if paint_mode {
            first = Point2f::new(mouse_x, mouse_y);
        }
        let selected_before = self.selected_chunks;
        if self.scale > CHUNK_GRID_SCALE {
            let mut mouse_region = self.mouse_region(mouse_x, mouse_y);
            let mut first_region = if paint_mode {
                mouse_region
            } else {
                self.mouse_region(first.x, first.z)
            };
            sort_points(&mut first_region, &mut mouse_region);
            for x in first_region.x..=mouse_region.x {
                for z in first_region.z..=mouse_region.z {
                    let region = Point2i::new(x, z);
                    if mark {
                        let diff = self.selection.add_region(region.as_long()) as i64;
                        self.selected_chunks += if self.selection.is_inverted() { -diff } else { diff };
                    } else {
                        let diff = self.selection.remove_region(region.as_long()) as i64;
                        self.selected_chunks += if self.selection.is_inverted() { diff } else { -diff };
                    }
                }
            }
        } else {
            let mut mouse_chunk = self.mouse_chunk(mouse_x, mouse_y);
            let mut first_chunk = if paint_mode {
                mouse_chunk
            } else {
                self.mouse_chunk(first.x, first.z)
            };
            sort_points(&mut first_chunk, &mut mouse_chunk);
            for x in first_chunk.x..=mouse_chunk.x {
                for z in first_chunk.z..=mouse_chunk.z {
                    let chunk = Point2i::new(x, z);
                    let region = chunk.chunk_to_region();
                    if mark {
                        if !self.selection.is_chunk_selected(chunk) {
                            self.selection.add_chunk(chunk);
                            self.selected_chunks += if self.selection.is_inverted() { -1 } else { 1 };
                            self.reset_marked_chunks_image(region);
                        }
                    } else if self.selection.is_chunk_selected(chunk) {
                        self.selection.remove_chunk(chunk);
                        self.selected_chunks += if self.selection.is_inverted() { 1 } else { -1 };
                        self.reset_marked_chunks_image(region);
                    }
                }
            }
        }
        self.unsaved_selection = !self.selection.is_empty()
            || (selected_before == self.selected_chunks && self.unsaved_selection);
    }

    pub fn mouse_block(&self, x: f32, z: f32) -> Point2i {
        Point2i::new(
            (self.offset.x + x * self.scale).floor() as i32,
            (self.offset.z + z * self.scale).floor() as i32,
        )
    }
    pub fn mouse_chunk(&self, x: f32, z: f32) -> Point2i {
        self.mouse_block(x, z).block_to_chunk()
    }
    pub fn mouse_region(&self, x: f32, z: f32) -> Point2i {
        self.mouse_block(x, z).block_to_region()
    }

    fn reset_marked_chunks_image(&mut self, region: Point2i) {
        let t = self
            .tiles
            .entry(region.as_long())
            .or_insert_with(|| Tile::new(region));
        t.marked_chunks_image = None;
    }

    pub fn clear_selection(&mut self) {
        self.selection = Selection::new();
        self.selected_chunks = 0;
        self.unsaved_selection = false;
        for t in self.tiles.values_mut() {
            t.marked_chunks_image = None;
        }
    }

    pub fn invert_selection(&mut self) {
        self.selection.inverted = !self.selection.inverted;
        for t in self.tiles.values_mut() {
            t.marked_chunks_image = None;
        }
    }

    pub fn invert_regions_with_selection(&mut self) {
        self.selection.invert_all();
        self.selected_chunks = self.selection.count() as i64;
        self.unsaved_selection = true;
        for t in self.tiles.values_mut() {
            t.marked_chunks_image = None;
        }
    }

    pub fn clear(&mut self) {
        self.tiles.clear();
        self.pool.clear();
        self.pool_order.clear();
        self.loading.clear();
        self.mca_cache.clear();
    }

    /// region file path for a tile
    pub fn mca_path(&self, r: Point2i) -> Option<std::path::PathBuf> {
        let w = crate::config::world()?;
        Some(w.region_dir().join(files::mca_file_name(r)))
    }

    // -- the periodic update (updateService) ------------------------------------

    /// Called ~every 100ms or when the view changes. Mirrors
    /// TileMap.initUpdateService: unload invisible tiles, drop stale jobs,
    /// assign priorities center-out, request images.
    pub fn update(&mut self) {
        let Some(ws) = crate::config::world() else { return };
        // unload invisible tiles — pixels stay in `pool`, so offscreen tiles
        // are dropped entirely (bounds live GPU textures to the viewport).
        let mut to_remove = Vec::new();
        for (k, t) in &self.tiles {
            if !t.is_visible(self, TILE_VISIBILITY_THRESHOLD) {
                to_remove.push(*k);
            }
        }
        for k in to_remove {
            self.tiles.remove(&k);
        }

        // priorities + image requests, spiral order. cap the request count —
        // at max zoom-out a huge screen can see >100k regions; loading all of
        // them exhausts file descriptors, the job queue and the GPU atlas.
        let zoom_level = get_zoom_level(self.scale);
        let mut new_prios = HashMap::new();
        let mut prio = 1i64;
        let mut requests: Vec<Point2i> = Vec::new();
        self.for_visible_regions_spiral(MAX_TILE_REQUESTS, |region| {
            new_prios.insert(region.as_long(), prio);
            prio += 1;
            requests.push(region);
        });
        self.tile_priorities = new_prios;

        let header_zoom = ws.cfg.render_header_only_zoom_level;
        for region in requests {
            if !self.tiles.contains_key(&region.as_long()) {
                self.tiles.insert(region.as_long(), Tile::new(region));
            }
            let need_overlay = self.overlay.is_some()
                && !self.tiles.get(&region.as_long()).map(|t| t.overlay_loaded).unwrap_or(false);
            let tile = self.tiles.get_mut(&region.as_long()).unwrap();
            if tile.image.is_some() && tile.loaded {
                let img_zoom = tile.image_zoom_level();
                if img_zoom != zoom_level {
                    if zoom_level >= header_zoom && img_zoom < header_zoom {
                        self.request_image(region, zoom_level);
                    } else if img_zoom < zoom_level {
                        // image larger than needed: scale down immediately.
                        // use the actual pixel count as the source size — LOD
                        // tiles don't follow the 512/zoom rule.
                        if let Some((img, iz)) = tile.image.take() {
                            let from = (img.len() as f64).sqrt() as usize;
                            let to = TILE_SIZE / zoom_level as usize;
                            let scaled = if from > to && to > 0 {
                                render::scale_down(&img, from, to)
                            } else {
                                img
                            };
                            let mut rgba = Vec::with_capacity(to * to * 4);
                            for &p in scaled.iter() {
                                rgba.push((p >> 16) as u8);
                                rgba.push((p >> 8) as u8);
                                rgba.push(p as u8);
                                rgba.push((p >> 24) as u8);
                            }
                            if let Some(buf) = image::RgbaImage::from_raw(to as u32, to as u32, rgba) {
                                tile.render_image = Some(std::sync::Arc::new(gpui::RenderImage::new(smallvec::SmallVec::from_elem(image::Frame::new(buf), 1))));
                            }
                            tile.image = Some((scaled, zoom_level));
                        }
                    } else {
                        self.request_image(region, zoom_level);
                    }
                }
            } else if !tile.loaded {
                self.request_image(region, zoom_level);
            }
            if need_overlay {
                self.request_overlay(region);
            }
        }
    }

    /// OverlayPool.requestImage: parse overlay data for a region and send back
    /// the 32x32 color-graded image.
    fn request_overlay(&mut self, region: Point2i) {
        let Some(ov) = self.overlay.clone() else { return };
        if !ov.is_valid() || !ov.active {
            return;
        }
        if !self.regions.contains(&region.as_long()) {
            if let Some(t) = self.tiles.get_mut(&region.as_long()) {
                t.overlay_loaded = true;
            }
            return;
        }
        if !self.overlay_loading.insert(region.as_long()) {
            return;
        }
        let Some(path) = self.mca_path(region) else { return };
        let tx = self.result_tx.clone();
        let priority = self.tile_priority(region);
        jobs::pools().add_process(Job::new(
            jobs::PRIORITY_MEDIUM,
            Some(Box::new(move || priority)),
            move || {
                let image = McaFile::new(path)
                    .and_then(|mut m| m.load(true).map(|_| m))
                    .ok()
                    .map(|m| ov.image(&mc_core::overlay::region_overlay_data(&m, &ov)));
                let _ = tx.send(TileResult::Overlay { region, image });
            },
        ));
    }

    /// TileMap.setOverlays
    pub fn set_overlays(&mut self, list: Vec<mc_core::overlay::Overlay>) {
        self.overlays = {
            let mut v: Vec<Option<mc_core::overlay::Overlay>> =
                list.into_iter().map(Some).collect();
            v.sort_by_key(|o| o.as_ref().map(|o| o.typ));
            v.push(None);
            v
        };
        self.set_overlay(None);
        jobs::pools().cancel_parser_queue();
    }

    /// TileMap.setOverlay
    pub fn set_overlay(&mut self, ov: Option<mc_core::overlay::Overlay>) {
        self.overlay = ov;
        self.clear_overlay();
    }

    /// TileMap.nextOverlay — cycle within the same overlay type.
    pub fn next_overlay(&mut self) {
        let Some(cur) = &self.overlay else { return };
        if self.disabled { return; }
        let cur_typ = cur.typ;
        let mut index = self
            .overlays
            .iter()
            .position(|o| o.as_ref() == Some(cur))
            .unwrap_or(self.overlays.len() - 1);
        loop {
            index += 1;
            if index == self.overlays.len() {
                index = 0;
            }
            let p = &self.overlays[index];
            if let Some(o) = p {
                if o.active && o.is_valid() && o.typ == cur_typ {
                    let o = o.clone();
                    self.set_overlay(Some(o));
                    break;
                }
            }
            if index == self.overlays.len() - 1 {
                break; // wrapped past the None terminator without match
            }
        }
        jobs::pools().cancel_parser_queue();
    }

    /// TileMap.nextOverlayType — cycle to next overlay of a different type
    /// (None = overlay off).
    pub fn next_overlay_type(&mut self) {
        if self.disabled { return; }
        let cur_typ = self.overlay.as_ref().map(|o| o.typ);
        let mut index = match &self.overlay {
            Some(cur) => self
                .overlays
                .iter()
                .position(|o| o.as_ref() == Some(cur))
                .unwrap_or(self.overlays.len() - 1),
            None => self.overlays.len() - 1,
        };
        loop {
            index += 1;
            if index == self.overlays.len() {
                index = 0;
            }
            match &self.overlays[index] {
                None => {
                    self.set_overlay(None);
                    break;
                }
                Some(o) => {
                    if o.active && o.is_valid() && Some(o.typ) != cur_typ {
                        let o = o.clone();
                        self.set_overlay(Some(o));
                        break;
                    }
                }
            }
            if index == self.overlays.len() - 1 && self.overlays.len() <= 1 {
                break;
            }
            // guard against infinite loop when nothing valid exists
            if self.overlays[index].is_none() && cur_typ.is_none() {
                break;
            }
        }
        jobs::pools().cancel_parser_queue();
    }

    /// TileMap.clearOverlay
    pub fn clear_overlay(&mut self) {
        for t in self.tiles.values_mut() {
            t.overlay = None;
            t.overlay_loaded = false;
            t.overlay_render_image = None;
        }
        self.overlay_loading.clear();
    }

    /// ImagePool.requestImage.
    pub fn request_image(&mut self, region: Point2i, zoom_level: i32) {
        let Some(ws) = crate::config::world() else { return };
        // no mca file -> nothing to load
        if !self.regions.contains(&region.as_long()) {
            if let Some(t) = self.tiles.get_mut(&region.as_long()) {
                t.loaded = true;
            }
            return;
        }
        if self.loading.contains(&region.as_long()) {
            return;
        }
        if self.loading.len() >= MAX_IN_FLIGHT_LOADS {
            return;
        }

        // memory pool exact zoom
        if let Some(img) = self.pool.get(&zoom_level).and_then(|m| m.get(&region.as_long())) {
            let img = img.clone();
            let t = self.tiles.get_mut(&region.as_long()).unwrap();
            t.image = Some((img.clone(), zoom_level));
            t.render_image = argb_to_render_image(&img, TILE_SIZE / zoom_level as usize);
            t.loaded = true;
            self.fetch_or_parse_structures(region);
            return;
        }
        // memory pool other zooms
        for zl in (1..=crate::config::MAX_ZOOM_LEVEL).step_by(1) {
            if (zl & (zl - 1)) != 0 || zl == zoom_level {
                continue;
            }
            if let Some(img) = self.pool.get(&zl).and_then(|m| m.get(&region.as_long())) {
                let img = img.clone();
                if zl < zoom_level {
                    // larger than needed -> scale down
                    let from = TILE_SIZE / zl as usize;
                    let to = TILE_SIZE / zoom_level as usize;
                    let scaled = render::scale_down(&img, from, to);
                    self.push_pool(zoom_level, region, scaled.clone());
                    let t = self.tiles.get_mut(&region.as_long()).unwrap();
                    t.render_image = argb_to_render_image(&scaled, TILE_SIZE / zoom_level as usize);
                    t.image = Some((scaled, zoom_level));
                    t.loaded = true;
                    self.fetch_or_parse_structures(region);
                    return;
                } else {
                    // lower res: use anyway, keep looking on disk
                    let t = self.tiles.get_mut(&region.as_long()).unwrap();
                    t.render_image = argb_to_render_image(&img, TILE_SIZE / zoom_level as usize);
                    t.image = Some((img, zoom_level));
                    t.loaded = true;
                    self.fetch_or_parse_structures(region);
                    break;
                }
            }
        }

        // disk cache: exact zoom png (zoom<32), lod db image (zoom>=32)
        if zoom_level >= 32 {
            if let Some((img, _)) = cache::get_lod_image(&ws.uuid, region) {
                self.push_pool(zoom_level, region, img.clone());
                if let Some(t) = self.tiles.get_mut(&region.as_long()) {
                    t.render_image = argb_to_render_image(&img, TILE_SIZE / zoom_level as usize);
                    t.image = Some((img, zoom_level));
                }
                if self.is_image_outdated(region) {
                    self.discard_cached_image(region);
                    if let Some(t) = self.tiles.get_mut(&region.as_long()) {
                        t.loaded = false;
                    }
                }
                return;
            }
        } else {
            let png = files::png_cache_path(&ws.cache_dir, zoom_level, region);
            if png.exists() {
                if self.load_cached_image(region, png, zoom_level, zoom_level) {
                    return;
                }
            }
        }
        // disk cache: any zoom
        for zl in [1, 2, 4, 8, 16, 32] {
            if zl == zoom_level {
                continue;
            }
            let png = files::png_cache_path(&ws.cache_dir, zl, region);
            if png.exists() {
                let (load_zl, target) = if zl < zoom_level {
                    (zl, zoom_level)
                } else {
                    (zl, zl)
                };
                if self.load_cached_image(region, png, load_zl, target) {
                    return;
                }
            }
        }

        let header_only = zoom_level >= ws.cfg.render_header_only_zoom_level;
        if header_only {
            // try cached chunk set
            if let Some(cs) = cache::get_chunks(&ws.uuid, region) {
                let img = render::lod_image(&cs, ws.cfg.zoom_level_lod_color);
                let t = self.tiles.get_mut(&region.as_long()).unwrap();
                t.render_image = argb_to_render_image(&img, 32);
                t.image = Some((img, zoom_level));
                t.loaded = true;
                return;
            }
            if self.loading.contains(&region.as_long()) {
                return;
            }
            self.loading.insert(region.as_long());
            let tx = self.result_tx.clone();
            let path = match self.mca_path(region) {
                Some(p) => p,
                None => return,
            };
            let uuid = ws.uuid.clone();
            let lod_color = ws.cfg.zoom_level_lod_color;
            let priority = self.tile_priority(region);
            jobs::pools().add_process(Job::new(
                jobs::PRIORITY_LOW,
                Some(Box::new(move || priority)),
                move || {
                    let mut m = match McaFile::new(path) {
                        Ok(m) => m,
                        Err(_) => {
                            let _ = tx.send(TileResult::Image {
                                region,
                                image: None,
                                zoom: zoom_level,
                                structures: None,
                                cache_write: false,
                            });
                            return;
                        }
                    };
                    let res = m.load_offsets().map(|_| m.offsets_chunk_set()).ok();
                    match res {
                        Some(cs) => {
                            cache::set_chunks(&uuid, region, &cs);
                            let img = render::lod_image(&cs, lod_color);
                            let _ = tx.send(TileResult::Image {
                                region,
                                image: Some(img),
                                zoom: zoom_level,
                                structures: None,
                                cache_write: false,
                            });
                        }
                        None => {
                            let _ = tx.send(TileResult::Image {
                                region,
                                image: None,
                                zoom: zoom_level,
                                structures: None,
                                cache_write: false,
                            });
                        }
                    }
                },
            ));
            return;
        }

        // generate from mca
        if self.loading.contains(&region.as_long()) {
            return;
        }
        self.loading.insert(region.as_long());
        let tx = self.result_tx.clone();
        let path = match self.mca_path(region) {
            Some(p) => p,
            None => return,
        };
        let cfg = mc_core::render::RenderSettings {
            render_height: ws.cfg.render_height,
            layer_only: ws.cfg.render_layer_only,
            caves: ws.cfg.render_caves,
            shade: ws.cfg.shade,
            shade_water: ws.cfg.shade_water,
            shade_altitude: ws.cfg.shade_altitude,
        };
        let uuid = ws.uuid.clone();
        let priority = self.tile_priority(region);
        jobs::pools().add_process(Job::new(
            jobs::PRIORITY_LOW,
            Some(Box::new(move || priority)),
            move || {
                let mut m = match McaFile::new(path) {
                    Ok(m) => m,
                    Err(_) => {
                        let _ = tx.send(TileResult::Image {
                            region,
                            image: None,
                            zoom: zoom_level,
                            structures: None,
                            cache_write: false,
                        });
                        return;
                    }
                };
                if m.load(true).is_err() {
                    let _ = tx.send(TileResult::Image {
                        region,
                        image: None,
                        zoom: zoom_level,
                        structures: None,
                        cache_write: false,
                    });
                    return;
                }
                let corrupted = CORRUPTED.with(|c| *c.borrow());
                let img = render::generate_image(&m, zoom_level, &cfg, &corrupted);
                let structures = m.structures();
                let _ = tx.send(TileResult::Image {
                    region,
                    image: img,
                    zoom: zoom_level,
                    structures: Some(structures),
                    cache_write: true,
                });
            },
        ));
    }

    /// LoadCachedImageJob — read png on a pool thread, scaled to target zoom.
    fn load_cached_image(&mut self, region: Point2i, path: std::path::PathBuf, load_zoom: i32, target_zoom: i32) -> bool {
        if self.loading.contains(&region.as_long()) {
            return false;
        }
        self.loading.insert(region.as_long());
        let tx = self.result_tx.clone();
        jobs::pools().add_process(Job::new(jobs::PRIORITY_MEDIUM, None, move || {
            let img = image::open(&path).ok().map(|im| {
                let rgba = im.to_rgba8();
                let (w, _h) = rgba.dimensions();
                let mut px: Vec<u32> = rgba
                    .chunks_exact(4)
                    .map(|c| {
                        ((c[3] as u32) << 24) | ((c[0] as u32) << 16) | ((c[1] as u32) << 8) | c[2] as u32
                    })
                    .collect();
                if load_zoom != target_zoom {
                    px = render::scale_down(&px, w as usize, TILE_SIZE / target_zoom as usize);
                }
                px
            });
            let _ = tx.send(TileResult::Image {
                region,
                image: img,
                zoom: target_zoom,
                structures: None,
                cache_write: false,
            });
        }));
        true
    }

    fn fetch_or_parse_structures(&mut self, region: Point2i) {
        let Some(ws) = crate::config::world() else { return };
        if let Some(s) = cache::get_structures(&ws.uuid, region) {
            if let Some(t) = self.tiles.get_mut(&region.as_long()) {
                t.structures = Some(s);
            }
            return;
        }
        // structure parsing piggybacks on the normal image job in our impl
    }

    fn is_image_outdated(&self, region: Point2i) -> bool {
        let Some(ws) = crate::config::world() else { return false };
        let t = cache::get_file_time(&ws.uuid, region);
        if t == -1 {
            return false;
        }
        let mt = self
            .mca_path(region)
            .and_then(|p| std::fs::metadata(p).ok())
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        t != mt
    }

    fn record_file_time(&self, region: Point2i) {
        let Some(ws) = crate::config::world() else { return };
        let mt = self
            .mca_path(region)
            .and_then(|p| std::fs::metadata(p).ok())
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        cache::set_file_time(&ws.uuid, region, mt);
    }

    fn push_pool(&mut self, zoom: i32, region: Point2i, img: Vec<u32>) {
        self.pool
            .entry(zoom)
            .or_default()
            .insert(region.as_long(), img);
        let order = self.pool_order.entry(zoom).or_default();
        order.retain(|r| *r != region.as_long());
        order.push(region.as_long());
        self.trim_pool(zoom);
    }

    /// ImagePool.trim — keep pool size <= visibleTiles * IMAGE_POOL_SIZE.
    fn trim_pool(&mut self, zoom: i32) {
        let limit = (self.visible_tiles() as f32 * crate::config::IMAGE_POOL_SIZE) as usize;
        let entry = match self.pool.get(&zoom) {
            Some(e) => e.len(),
            None => return,
        };
        if entry <= limit {
            return;
        }
        let scale = self.scale * 2.0;
        let (min, max) = {
            let old = self.scale;
            let _ = old;
            // visible regions at scale*2
            let min = self.offset.to_point2i().block_to_region();
            let max = self
                .offset
                .add(self.width * scale, self.height * scale)
                .to_point2i()
                .block_to_region();
            (min, max)
        };
        let mut visible = HashSet::new();
        for x in min.x..=max.x {
            for z in min.z..=max.z {
                visible.insert(Point2i::new(x, z).as_long());
            }
        }
        if let Some(order) = self.pool_order.get_mut(&zoom) {
            order.retain(|r| visible.contains(r) || {
                self.pool.get_mut(&zoom).unwrap().remove(r);
                false
            });
        }
    }

    pub fn discard_cached_image(&mut self, region: Point2i) {
        if let Some(ws) = crate::config::world() {
            for m in self.pool.values_mut() {
                m.remove(&region.as_long());
            }
            for o in self.pool_order.values_mut() {
                o.retain(|r| *r != region.as_long());
            }
            cache::delete_data(&ws.uuid, region);
            for zl in [1, 2, 4, 8, 16, 32] {
                let _ = std::fs::remove_file(files::png_cache_path(&ws.cache_dir, zl, region));
            }
        }
    }

    pub fn mark_all_tiles_obsolete(&mut self) {
        for t in self.tiles.values_mut() {
            t.loaded = false;
        }
        self.pool.clear();
        self.pool_order.clear();
    }

    /// Drain async tile results (call every UI tick).
    /// Returns true if anything changed.
    /// take a completed filter result
    pub fn take_filter_result(&mut self) -> Option<Selection> {
        FILTER_RESULT.with(|r| r.borrow_mut().take())
    }

    pub fn poll_results(&mut self) -> bool {
        let mut changed = false;
        let mut pending = Vec::new();
        if let Some(rx) = &self.results_rx {
            while let Ok(res) = rx.try_recv() {
                pending.push(res);
            }
        }
        for res in pending {
            changed = true;
            match res {
                TileResult::Image {
                    region,
                    image,
                    zoom,
                    structures,
                    cache_write,
                } => {
                    self.loading.remove(&region.as_long());
                    // renders that landed at a stale zoom are still valid
                    // data — keep them in the pool but don't clobber the
                    // tile's current-zoom image.
                    if image.is_some() && zoom != get_zoom_level(self.scale) {
                        if let Some(img) = &image {
                            self.push_pool(zoom, region, img.clone());
                        }
                        continue;
                    }
                    if let Some(img) = &image {
                        self.push_pool(zoom, region, img.clone());
                    }
                    if let Some(t) = self.tiles.get_mut(&region.as_long()) {
                        if let Some(img) = image {
                            let size = TILE_SIZE / zoom as usize;
                            let mut rgba = Vec::with_capacity(size * size * 4);
                            for &p in img.iter() {
                                rgba.push((p >> 16) as u8);
                                rgba.push((p >> 8) as u8);
                                rgba.push(p as u8);
                                rgba.push((p >> 24) as u8);
                            }
                            if let Some(buf) = image::RgbaImage::from_raw(size as u32, size as u32, rgba) {
                                let frame = image::Frame::new(buf);
                                t.render_image = Some(std::sync::Arc::new(gpui::RenderImage::new(smallvec::SmallVec::from_elem(frame, 1))));
                            }
                            t.image = Some((img, zoom));
                        }
                        if let Some(s) = structures {
                            t.structures = Some(s);
                        }
                        t.loaded = true;
                    }
                    if cache_write {
                        self.write_image_cache(region, zoom);
                    }
                    self.record_file_time(region);
                }
                TileResult::Structures { region, structures } => {
                    if let Some(t) = self.tiles.get_mut(&region.as_long()) {
                        t.structures = structures;
                    }
                }
                TileResult::Overlay { region, image } => {
                    self.overlay_loading.remove(&region.as_long());
                    if let Some(t) = self.tiles.get_mut(&region.as_long()) {
                        if let Some(img) = image {
                            if self.overlay.is_some() {
                                t.overlay_render_image = argb_to_render_image(&img, 32);
                                t.overlay = Some((img, 32));
                            }
                        }
                        t.overlay_loaded = true;
                    }
                }
            }
        }
        changed
    }

    /// write the tile's current image to the disk caches (RegionImageGeneratorSaveJob).
    fn write_image_cache(&self, region: Point2i, zoom: i32) {
        let Some(ws) = crate::config::world() else { return };
        let Some(t) = self.tiles.get(&region.as_long()) else { return };
        let Some((img, _)) = &t.image else { return };
        if zoom >= 32 {
            cache::set_lod_image(&ws.uuid, region, img, TILE_SIZE / zoom as usize);
        } else {
            let size = TILE_SIZE / zoom as usize;
            let path = files::png_cache_path(&ws.cache_dir, zoom, region);
            if let Some(p) = path.parent() {
                let _ = std::fs::create_dir_all(p);
            }
            let mut rgba = Vec::with_capacity(size * size * 4);
            for &p in img.iter() {
                rgba.push((p >> 16) as u8);
                rgba.push((p >> 8) as u8);
                rgba.push(p as u8);
                rgba.push((p >> 24) as u8);
            }
            let _ = image::save_buffer(
                &path,
                &rgba,
                size as u32,
                size as u32,
                image::ColorType::Rgba8,
            );
        }
        if let Some(s) = &t.structures {
            cache::set_structures(&ws.uuid, region, s);
        }
    }
}

fn sort_points(a: &mut Point2i, b: &mut Point2i) {
    let aa = *a;
    a.x = a.x.min(b.x);
    a.z = a.z.min(b.z);
    b.x = aa.x.max(b.x);
    b.z = aa.z.max(b.z);
}

thread_local! {
    /// 16x16 corrupted-chunk overlay pixels
    pub static CORRUPTED: std::cell::RefCell<[u32; 256]> = std::cell::RefCell::new([0; 256]);
}

pub fn set_corrupted_overlay(argb: [u32; 256]) {
    CORRUPTED.with(|c| *c.borrow_mut() = argb);
}


/// Build a gpu-side image from an argb buffer of size×size pixels.
pub fn argb_to_render_image(argb: &[u32], size: usize) -> Option<std::sync::Arc<gpui::RenderImage>> {
    let mut rgba = Vec::with_capacity(size * size * 4);
    for &p in argb.iter() {
        rgba.push((p >> 16) as u8);
        rgba.push((p >> 8) as u8);
        rgba.push(p as u8);
        rgba.push((p >> 24) as u8);
    }
    let buf = image::RgbaImage::from_raw(size as u32, size as u32, rgba)?;
    Some(std::sync::Arc::new(gpui::RenderImage::new(
        smallvec::SmallVec::from_elem(image::Frame::new(buf), 1),
    )))
}

thread_local! {
    /// completed filter query result, consumed by the view
    pub static FILTER_RESULT: std::cell::RefCell<Option<Selection>> = const { std::cell::RefCell::new(None) };
}
