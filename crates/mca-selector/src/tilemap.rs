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
use std::sync::Arc;

pub const TILE_SIZE: usize = 512;
pub const CHUNK_GRID_SCALE: f32 = 1.5;
pub const TILE_VISIBILITY_THRESHOLD: i32 = 2;

// Tile colors (upstream Tile.* + config defaults)
/// max regions given tile objects / requests per update tick
pub const MAX_TILE_REQUESTS: usize = 8192;
/// max queued+running image jobs. Queued jobs are small (~200B of args —
/// fds are only touched by the ~8 pool threads actually running jobs), so
/// the cap only needs to stop `loading` growing without bound when results
/// stall. Aligned with MAX_TILE_REQUESTS: a fully-visible screen at far
/// zoom must be enqueueable in one pass instead of trickling in over
/// multiple update ticks behind a tighter cap.
pub const MAX_IN_FLIGHT_LOADS: usize = MAX_TILE_REQUESTS;

pub const REGION_GRID_COLOR: u32 = 0xB300_0000; // black 70%
pub const CHUNK_GRID_COLOR: u32 = 0x7300_0000; // black 45%
pub const COORDINATES_COLOR: u32 = 0xFFFF_FFFF;
pub const EMPTY_COLOR: u32 = 0xFF0A_0A0A; // near-black
pub const GRID_LINE_WIDTH: f32 = 1.0;
/// power-of-two tile zoom levels (upstream zoomLevel: 1,2,4,...,32)
pub const ZOOM_LEVELS: [i32; 6] = [1, 2, 4, 8, 16, 32];
/// pending_scratch keeps its capacity across ticks; drop the buffer only
/// after a spike far past a normal drain — a tick can legitimately see
/// ~MAX_TILE_REQUESTS results, so that count is no spike bound.
const MAX_PENDING_RESULTS: usize = 4 * MAX_TILE_REQUESTS;

pub fn get_zoom_level(scale: f32) -> i32 {
    let s = scale as i32;
    if s <= 1 {
        1
    } else {
        // Integer.highestOneBit
        1 << (31 - s.leading_zeros())
    }
}

/// Tile = one region. Image = argb pixels of size 512/zoomLevel.
pub struct Tile {
    pub location: Point2i,
    /// argb buffer + its zoom level (image size = 512/zoom); shared with the
    /// memory pool via Arc so handoffs are refcount bumps, not buffer copies
    pub image: Option<(Arc<Vec<u32>>, i32)>,
    pub loaded: bool,
    pub overlay_data: Option<std::sync::Arc<[i32; 1024]>>,
    pub overlay_loaded: bool,
    pub structures: Option<HashMap<i64, Vec<String>>>,
    /// structure disk-cache lookup was already attempted for this image
    pub structures_checked: bool,
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
            overlay_data: None,
            overlay_loaded: false,
            structures: None,
            structures_checked: false,
            render_image: None,
            overlay_render_image: None,
        }
    }
    pub fn image_zoom_level(&self) -> i32 {
        self.image.as_ref().map(|(_, z)| *z).unwrap_or(0)
    }
}

/// Async results channel: worker -> UI.
/// bump when the renderer's pixel output changes — forces every cached
/// PNG/LOD image to be treated as stale and re-rendered once.
const RENDER_CACHE_VERSION: i64 = 3;

/// fingerprint of everything that changes a tile's pixels. keyed caches
/// (disk PNGs, meta) are invalidated by comparing this.
fn render_hash(cfg: &crate::config::WorldConfig, dim: mc_core::render::DimKind) -> i64 {
    ((cfg.render_height.clamp(-64, 2047) + 64) as i64)
        | (dim as i64) << 15
        | (cfg.render_layer_only as i64) << 10
        | (cfg.render_caves as i64) << 11
        | (cfg.shade as i64) << 12
        | (cfg.shade_water as i64) << 13
        | (cfg.shade_altitude as i64) << 14
        | RENDER_CACHE_VERSION << 20
}

/// a tile image shipped from a worker: raw argb pixels (kept for pool
/// scale_down + disk-cache writes) plus the gpu image built on the worker —
/// the ~1MB u32->BGRA repack + RgbaImage/RenderImage construction no longer
/// happens on the UI thread. `render` is only None when RgbaImage
/// construction failed (non-square input — can't happen for tile sizes).
pub struct TileImage {
    argb: Vec<u32>,
    render: Option<std::sync::Arc<gpui::RenderImage>>,
}

/// memory-pool entry: same payload, Arc-shared so pool hits and tile
/// adoptions are refcount bumps. The shared RenderImage also shares its
/// ImageId, so re-adopted tiles hit the same GPU atlas slot.
pub struct PooledImage {
    argb: Arc<Vec<u32>>,
    render: std::sync::Arc<gpui::RenderImage>,
}

pub enum TileResult {
    Image {
        region: Point2i,
        /// worker sends Some(TileImage) with a prebuilt gpu image; `None` is
        /// only sent on a worker-side render failure (bad mca file).
        image: Option<TileImage>,
        zoom: i32,
        structures: Option<HashMap<i64, Vec<String>>>,
    },
    /// a batch job rewrote this region on disk: drop the tile's pixels +
    /// in-flight flags so update() re-renders it.
    Invalidate { region: Point2i },
    /// batch-job completion message for the Info dialog (filter counts,
    /// sum-selection totals, change-fields/import results). Single
    /// back-channel for every "job finished" UI message.
    JobInfo(String),
    /// completed filter-select job: replace the map selection.
    FilterResult { selection: Selection },
    /// worker-side structures-cache read for a tile that adopted a pooled
    /// image (was a UI-side file read per tile per invalidation).
    Structures {
        region: Point2i,
        structures: Option<HashMap<i64, Vec<String>>>,
    },
    /// far-zoom chunk-presence placeholder (sent before the real render)
    Presence {
        region: Point2i,
        image: Vec<u32>,
    },
    /// 32x32 ARGB overlay image (one pixel per chunk)
    Overlay {
        region: Point2i,
        image: Option<Vec<u32>>,
        data: Option<std::sync::Arc<[i32; 1024]>>,
    },
}


/// upstream pastedChunks state: chunks copied to clipboard dir + drag offset.
pub struct Pasted {
    pub dir: std::path::PathBuf,        // clipboard region dir
    pub selection: Option<Selection>, // copied selection (from selection.csv)
    pub offset: Point2i,              // pastedChunksOffset, in chunks
    pub first_offset: Point2i,
    pub first_mouse: Option<Point2f>,
}

pub struct TileMapState {
    pub scale: f32, // blocks per screen pixel; higher = more zoomed out
    pub offset: Point2f, // block coords at screen 0,0
    /// canvas bounds origin in window coords — gpui mouse events are
    /// window-space, so handlers subtract this to get canvas coords.
    pub canvas_origin: Point2f,
    pub width: f32,
    pub height: f32,
    pub tiles: HashMap<i64, Tile>,
    pub tile_priorities: HashMap<i64, i64>,
    pub regions: HashSet<i64>, // region files that exist on disk
    pub selection: Selection,
    pub selected_chunks: i64,
    pub unsaved_selection: bool,
    /// bumped by every selection mutation; overlay caches key on it.
    pub selection_version: u64,
    pub show_chunk_grid: bool,
    pub show_region_grid: bool,
    pub show_coordinates: bool,
    pub show_structure_icons: bool,
    pub show_nonexistent_regions: bool,
    pub coordinate_style: CoordinateStyle,
    pub hovered_block: Option<Point2i>,
    pub pasted: Option<Pasted>,
    /// memory image pool: zoomLevel -> region->image (pixels + gpu image,
    /// both Arc-shared with tiles so adoptions are refcount bumps)
    pub pool: HashMap<i32, HashMap<i64, PooledImage>>,
    pub pool_order: HashMap<i32, Vec<i64>>, // keys currently in the pool, for trim
    /// region -> render-settings hash at enqueue; results that land with a
    /// stale hash are discarded so obsolete renders never resurface.
    pub loading: HashMap<i64, i64>,
    /// visible bounds `tile_priorities` was last built for — rebuilding the
    /// map is skipped while the viewport stays put
    prio_bounds: Option<(Point2i, Point2i)>,
    /// amortizes the O(visible regions) visible-set build in `trim_pool`
    pool_trim_defer: u32,
    /// reusable drain buffer for poll_results — was a fresh Vec alloc on
    /// every UI tick, spiking to thousands of entries during pan/zoom.
    pending_scratch: Vec<TileResult>,
    pub results_rx: Receiver<TileResult>,
    pub result_tx: Sender<TileResult>,
    /// pending Info-dialog payload from the last finished batch job
    /// (TileResult::JobInfo), consumed by the view's poll tick.
    pub job_info: Option<String>,
    /// pending selection from a finished filter job
    /// (TileResult::FilterResult), consumed by the view's poll tick.
    pub filter_result: Option<Selection>,
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
            canvas_origin: Point2f::new(0.0, 0.0),
            width: 800.0,
            height: 600.0,
            tiles: HashMap::new(),
            tile_priorities: HashMap::new(),
            regions: HashSet::new(),
            selection: Selection::new(),
            selected_chunks: 0,
            unsaved_selection: false,
            selection_version: 0,
            show_chunk_grid: g.show_chunk_grid,
            show_region_grid: g.show_region_grid,
            show_coordinates: g.show_coordinates,
            show_structure_icons: g.show_structure_icons,
            show_nonexistent_regions: true,
            coordinate_style: match g.coordinate_style.to_ascii_uppercase().as_str() {
                "EDGE" => CoordinateStyle::Edge,
                "GRID" => CoordinateStyle::Grid,
                _ => CoordinateStyle::Dynamic,
            },
            hovered_block: None,
            pasted: None,
            pool: HashMap::new(),
            pool_order: HashMap::new(),
            loading: HashMap::new(),
            prio_bounds: None,
            pool_trim_defer: 0,
            pending_scratch: Vec::new(),
            results_rx: rx,
            result_tx: tx,
            job_info: None,
            filter_result: None,
            overlay: None,
            overlays: vec![None],
            overlay_loading: HashSet::new(),
        }
    }

    /// input/overlay gating: "disabled" == no world open (upstream disables
    /// the whole tile map until a world is loaded).
    pub fn disabled(&self) -> bool {
        crate::config::WORLD.read().unwrap().is_none()
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


    /// spiral over an explicit region rectangle, center-out, capped at `limit`.
    fn for_regions_spiral(min: Point2i, max: Point2i, limit: usize, mut f: impl FnMut(Point2i)) {
        let mid = min.add_p(max).div(2);
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
        // stale tile images stay painted at whatever resolution they have
        // (the GPU stretches them) until a fresh render lands — clearing
        // them here would flash the map black on every wheel tick. Queued
        // jobs are kept too: a render at an intermediate zoom still lands
        // in the image pool and is reusable if the zoom settles back
        // there — cancelling on every crossing just threw that work away.
        changed
    }

    /// pools().clear_queues() plus the bookkeeping that assumes a result
    /// will arrive: dropped jobs never send one, so `loading`/
    /// `overlay_loading` would stay set forever (update() thinks the tile
    /// is still loading) and `structures_checked` would leak (it is set at
    /// enqueue — after a queue clear the icon fetch never re-runs).
    pub fn cancel_jobs(&mut self) {
        jobs::pools().clear_queues();
        self.loading.clear();
        self.overlay_loading.clear();
        for t in self.tiles.values_mut() {
            t.structures_checked = false;
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
                    if mark {
                        if !self.selection.is_chunk_selected(chunk) {
                            self.selection.add_chunk(chunk);
                            self.selected_chunks += if self.selection.is_inverted() { -1 } else { 1 };
                        }
                    } else if self.selection.is_chunk_selected(chunk) {
                        self.selection.remove_chunk(chunk);
                        self.selected_chunks += if self.selection.is_inverted() { 1 } else { -1 };
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

    /// window-space event coords -> canvas coords. gpui delivers mouse
    /// positions relative to the window; upstream's canvas filled it so
    /// all position math below is canvas-relative.
    pub fn canvas_pos(&self, x: f32, z: f32) -> Point2f {
        Point2f::new(x - self.canvas_origin.x, z - self.canvas_origin.z)
    }

    /// TileMap.setPastedChunks — enter paste mode, offset centered on screen.
    pub fn set_pasted(&mut self, dir: std::path::PathBuf, selection: Option<Selection>, window_size: Point2f) {
        let (min, max) = selection
            .as_ref()
            .and_then(|s| s.bounds())
            .unwrap_or((Point2i::new(0, 0), Point2i::new(0, 0)));
        let offset_in_chunks = self.offset.to_point2i().block_to_chunk();
        let pasted_mid = Point2i::new((max.x - min.x) / 2, (max.z - min.z) / 2);
        let origin = offset_in_chunks.sub(min).sub(pasted_mid);
        let screen = Point2i::new(
            (window_size.x * self.scale / 16.0) as i32 / 2,
            (window_size.z * self.scale / 16.0) as i32 / 2,
        );
        let offset = origin.add_p(screen);
        self.pasted = Some(Pasted {
            dir,
            selection,
            offset,
            first_offset: offset,
            first_mouse: None,
        });
    }

    pub fn cancel_paste(&mut self) {
        self.pasted = None;
    }

    /// TileMap.onMouseDragged while pasting: offset tracks the grab delta.
    pub fn drag_paste_offset(&mut self, mouse: Point2f) {
        if let Some(p) = &mut self.pasted {
            if let Some(first) = p.first_mouse {
                let diff = mouse.sub(first).mul(self.scale).to_point2i().div(16);
                p.offset = p.first_offset.add_p(diff);
            }
        }
    }

    pub fn clear_selection(&mut self) {
        self.selection = Selection::new();
        self.selected_chunks = 0;
        self.unsaved_selection = false;
        self.selection_version += 1;
    }

    pub fn invert_selection(&mut self) {
        self.selection.inverted = !self.selection.inverted;
        self.selection_version += 1;
    }

    pub fn invert_regions_with_selection(&mut self) {
        self.selection.invert_all();
        self.selection_version += 1;
        self.selected_chunks = self.selection.count() as i64;
        self.unsaved_selection = true;
    }

    /// full reset for world-open: tiles, pools, priorities, selection, paste
    /// state and in-flight bookkeeping. The configured overlay list is global
    /// (upstream OverlayConfig) and survives; its per-tile data dies with the
    /// tiles anyway.
    pub fn clear(&mut self) {
        self.loading.clear();
        self.overlay_loading.clear();
        self.tiles.clear();
        self.pool.clear();
        self.pool_order.clear();
        self.tile_priorities.clear();
        self.prio_bounds = None;
        self.regions.clear();
        self.pasted = None;
        self.hovered_block = None;
        self.clear_selection();
    }

    /// region file path for a tile
    fn mca_path(&self, ws: &crate::config::WorldState, r: Point2i) -> std::path::PathBuf {
        ws.region_dir().join(files::mca_file_name(r))
    }

    // -- the periodic update (updateService) ------------------------------------

    /// Called ~every 100ms or when the view changes. Mirrors
    /// evict tiles outside the viewport margin — O(total tiles), so it runs
    /// on the slow poll rather than per scroll event.
    pub fn evict_invisible(&mut self) {
        // visibility bounds in region coords, computed once (per-tile
        // is_visible() used to redo the whole float pipeline each call).
        let t = TILE_VISIBILITY_THRESHOLD * TILE_SIZE as i32;
        let o = self.offset.to_point2i();
        let min = o.sub_v(t, t).block_to_region();
        let max = Point2i::new(
            (o.x as f32 + self.width * self.scale) as i32,
            (o.z as f32 + self.height * self.scale) as i32,
        )
        .add(t, t)
        .block_to_region();
        self.tiles.retain(|k, _| {
            let r = Point2i::from_long(*k);
            r.x >= min.x && r.z >= min.z && r.x <= max.x && r.z <= max.z
        });
    }

    /// TileMap.initUpdateService: assign priorities center-out, request images.
    /// cheap enough to run per input event (O(visible regions)).
    pub fn update(&mut self) {
        let Some(ws) = crate::config::world() else { return };

        // priorities + image requests, spiral order. cap the request count —
        // at max zoom-out a huge screen can see >100k regions; loading all of
        // them exhausts file descriptors, the job queue and the GPU atlas.
        let zoom_level = get_zoom_level(self.scale);
        let bounds = self.visible_region_bounds(Point2f::ZERO);
        if self.prio_bounds != Some(bounds) {
            // viewport moved: rebuild priorities in spiral order (one pass).
            let visible = (bounds.1.x as i64 - bounds.0.x as i64 + 1)
                .saturating_mul(bounds.1.z as i64 - bounds.0.z as i64 + 1)
                .clamp(0, MAX_TILE_REQUESTS as i64) as usize;
            let mut new_prios = HashMap::with_capacity(visible);
            let mut prio = 1i64;
            Self::for_regions_spiral(bounds.0, bounds.1, MAX_TILE_REQUESTS, |region| {
                new_prios.insert(region.as_long(), prio);
                prio += 1;
            });
            self.tile_priorities = new_prios;
            self.prio_bounds = Some(bounds);
        }
        // single fused spiral pass — the old two-pass loop materialized a
        // `requests` Vec (up to 8k entries = alloc + copy) between the
        // priority assignment and the request loop even when priorities were
        // already current. Steady-state ticks are now alloc-free here.
        // Render-job specs are collected and dispatched by one worker job so
        // the Job/path/closure construction per region leaves the UI thread.
        let mut renders: Vec<(Point2i, i32, i64)> = Vec::new();
        Self::for_regions_spiral(bounds.0, bounds.1, MAX_TILE_REQUESTS, |region| {
            let key = region.as_long();
            // get_mut before insert: visible tiles almost always already
            // exist, so the common case is one hash lookup with no entry()
            // churn (new regions insert once, then hit the Some arm).
            let (request, need_overlay) = match self.tiles.get_mut(&key) {
                Some(tile) => {
                    let need_overlay = self.overlay.is_some() && !tile.overlay_loaded;
                    // Any existing image is paintable at ANY zoom: paint_map
                    // draws render_image into a fixed-size screen rect and the
                    // GPU does the stretching. A finer-than-needed image
                    // (img_zoom < zoom_level) just paints smaller — keeping it
                    // is free, while the old scale_down + texture rebuild cost
                    // a ~1MB CPU rescale and an atlas upload per tile per zoom
                    // boundary. Only an image too coarse for the current zoom
                    // (img_zoom > zoom_level) still needs a finer render; it
                    // stays on screen until that lands.
                    let request = !tile.loaded
                        || (tile.image.is_some() && tile.image_zoom_level() > zoom_level);
                    (request, need_overlay)
                }
                None => {
                    self.tiles.insert(key, Tile::new(region));
                    (true, self.overlay.is_some())
                }
            };
            if request {
                self.request_image(&ws, region, zoom_level, &mut renders);
            } else {
                // ready tile: kick off the one-shot worker-side structures
                // read unless it already ran for this image.
                self.request_structures(&ws, region);
            }
            if need_overlay {
                self.request_overlay(&ws, region);
            }
        });
        if !renders.is_empty() {
            self.dispatch_renders(&ws, renders);
        }
    }

    /// One dispatcher job that builds + enqueues the collected render jobs
    /// on a pool worker: the per-region Job::new, path joins and ws field
    /// clones happen off the UI thread. `loading` entries were inserted at
    /// collect time so a follow-up update() can't double-request.
    fn dispatch_renders(&mut self, ws: &crate::config::WorldState, specs: Vec<(Point2i, i32, i64)>) {
        let tx = self.result_tx.clone();
        let uuid = ws.uuid.clone();
        let cache_dir = ws.cache_dir.clone();
        let region_dir = ws.region_dir().to_path_buf();
        let header_zoom = ws.cfg.render_header_only_zoom_level;
        let lod_color = ws.cfg.zoom_level_lod_color;
        let cfg = mc_core::render::RenderSettings {
            render_height: ws.cfg.render_height,
            layer_only: ws.cfg.render_layer_only,
            caves: ws.cfg.render_caves,
            shade: ws.cfg.shade,
            shade_water: ws.cfg.shade_water,
            shade_altitude: ws.cfg.shade_altitude,
            dim: ws.dim_kind(),
        };
        let hash = render_hash(&ws.cfg, ws.dim_kind());
        let corrupted = CORRUPTED.with(|c| *c.borrow());
        jobs::pools().add_process(Job::new(
            jobs::PRIORITY_MEDIUM,
            Option::<Box<dyn Fn() -> i64 + Send>>::None,
            move || {
                for (region, zoom_level, priority) in specs {
                    let tx = tx.clone();
                    let uuid = uuid.clone();
                    let cache_dir = cache_dir.clone();
                    let region_dir = region_dir.clone();
                    jobs::pools().add_process(Job::new(
                        jobs::PRIORITY_LOW,
                        Some(Box::new(move || priority)),
                        move || {
                            worker_probe_and_render(ProbeArgs {
                                tx,
                                uuid,
                                cache_dir,
                                region_dir,
                                region,
                                zoom_level,
                                cfg,
                                header_zoom,
                                lod_color,
                                hash,
                                corrupted,
                            });
                        },
                    ));
                }
            },
        ));
    }

    /// OverlayPool.requestImage: parse overlay data for a region and send back
    /// the 32x32 color-graded image.
    fn request_overlay(&mut self, ws: &crate::config::WorldState, region: Point2i) {
        {
            // cheap checks first: this runs per visible region per update, so
            // cloning the Overlay before the dedupe was an alloc storm.
            let Some(ov) = self.overlay.as_ref() else { return };
            if !ov.is_valid() || !ov.active {
                return;
            }
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
        let Some(ov) = self.overlay.clone() else { return };
        let path = self.mca_path(ws, region);
        let tx = self.result_tx.clone();
        let priority = self.tile_priority(region);
        let name = files::mca_file_name(region);
        let poi_path = ws.dirs.poi.clone().map(|d| d.join(&name));
        let ent_path = ws.dirs.entities.clone().map(|d| d.join(&name));
        jobs::pools().add_process(Job::new(
            jobs::PRIORITY_MEDIUM,
            Some(Box::new(move || priority)),
            move || {
                let data = McaFile::new(path)
                    .and_then(|mut m| m.load(true).map(|_| m))
                    .ok()
                    .map(|m| {
                        // Path overlays can read from the poi/entities files
                        let load = |p: Option<std::path::PathBuf>| -> Option<McaFile> {
                            p.and_then(|p| {
                                McaFile::new(p).ok().and_then(|mut f| f.load(true).ok().map(|_| f))
                            })
                        };
                        let poi = if ov.path_root == "poi" { load(poi_path) } else { None };
                        let ent = if ov.path_root == "entities" { load(ent_path) } else { None };
                        std::sync::Arc::new(mc_core::overlay::region_overlay_data(
                            &m, &ov, poi.as_ref(), ent.as_ref(),
                        ))
                    });
                let image = data.as_deref().map(|d| ov.image(d));
                let _ = tx.send(TileResult::Overlay { region, image, data });
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
    }

    /// TileMap.setOverlay
    pub fn set_overlay(&mut self, ov: Option<mc_core::overlay::Overlay>) {
        self.overlay = ov;
        self.clear_overlay();
    }

    /// TileMap.nextOverlay — cycle within the same overlay type.
    pub fn next_overlay(&mut self) {
        let Some(cur) = &self.overlay else { return };
        if self.disabled() { return; }
        let cur_typ = cur.typ;
        let mut index = self
            .overlays
            .iter()
            .position(|o| o.as_ref() == Some(cur))
            .unwrap_or(self.overlays.len() - 1);
        // upstream wraps around the whole list until a valid overlay of the
        // same type is found — the None terminator and entries before the
        // current index are not a boundary; a full cycle ends the search.
        let start = index;
        loop {
            index += 1;
            if index == self.overlays.len() {
                index = 0;
            }
            if let Some(o) = &self.overlays[index] {
                if o.active && o.is_valid() && o.typ == cur_typ {
                    let o = o.clone();
                    self.set_overlay(Some(o));
                    break;
                }
            }
            if index == start {
                break;
            }
        }
    }

    /// TileMap.nextOverlayType — cycle to next overlay of a different type
    /// (None = overlay off).
    pub fn next_overlay_type(&mut self) {
        if self.disabled() { return; }
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
    }

    /// TileMap.clearOverlay
    pub fn clear_overlay(&mut self) {
        for t in self.tiles.values_mut() {
            t.overlay_data = None;
            t.overlay_loaded = false;
            t.overlay_render_image = None;
        }
        self.overlay_loading.clear();
    }

    /// ImagePool.requestImage. On a full miss the job isn't built here —
    /// a (region, zoom_level, priority) spec is pushed onto `renders` and
    /// dispatch_renders constructs + enqueues the Jobs on a pool worker.
    fn request_image(&mut self, ws: &crate::config::WorldState, region: Point2i, zoom_level: i32, renders: &mut Vec<(Point2i, i32, i64)>) {
        let key = region.as_long();
        if !self.regions.contains(&key) {
            if let Some(t) = self.tiles.get_mut(&key) {
                t.loaded = true;
            }
            return;
        }
        if self.loading.contains_key(&key) {
            return;
        }
        if self.loading.len() >= MAX_IN_FLIGHT_LOADS {
            return;
        }
        let hash = render_hash(&ws.cfg, ws.dim_kind());

        if let Some(p) = self.pool.get(&zoom_level).and_then(|m| m.get(&key)) {
            // exact pool hit: pixels + gpu image are already Arc'd — adopt
            // with refcount bumps, no per-tile BGRA repack on the UI thread.
            let argb = p.argb.clone();
            let render_img = p.render.clone();
            let t = self.tiles.get_mut(&key).unwrap();
            t.image = Some((argb, zoom_level));
            t.render_image = Some(render_img);
            t.loaded = true;
            self.request_structures(ws, region);
            return;
        }
        for zl in (1..=crate::config::MAX_ZOOM_LEVEL).step_by(1) {
            if (zl & (zl - 1)) != 0 || zl == zoom_level {
                continue;
            }
            if let Some(p) = self.pool.get(&zl).and_then(|m| m.get(&key)) {
                if zl < zoom_level {
                    // larger than needed -> scale down. The ~1MB rescale +
                    // BGRA repack run on a worker; the result lands through
                    // the normal Image path (stale-hash drop still applies —
                    // scaled pixels inherit the source render's settings —
                    // and the landing pushes the pool entry once).
                    let argb = p.argb.clone();
                    self.loading.insert(key, hash);
                    let tx = self.result_tx.clone();
                    let uuid = ws.uuid.clone();
                    let priority = self.tile_priority(region);
                    let from = TILE_SIZE / zl as usize;
                    let to = TILE_SIZE / zoom_level as usize;
                    jobs::pools().add_process(Job::new(
                        jobs::PRIORITY_LOW,
                        Some(Box::new(move || priority)),
                        move || {
                            let scaled = render::scale_down(&argb, from, to);
                            let gpu_img = argb_to_render_image(&scaled, to);
                            let _ = tx.send(TileResult::Image {
                                region,
                                image: Some(TileImage {
                                    argb: scaled,
                                    render: gpu_img,
                                }),
                                zoom: zoom_level,
                                structures: cache::get_structures(&uuid, region),
                            });
                        },
                    ));
                    return;
                } else {
                    // lower res: use as a progressive placeholder and keep
                    // looking on disk — but only if it beats what the tile
                    // already shows, and tag/render it at the image's own
                    // zoom, not the view's.
                    let dominated = self
                        .tiles
                        .get(&key)
                        .map(|t| t.image.is_some() && t.image_zoom_level() <= zl)
                        .unwrap_or(false);
                    if !dominated {
                        let argb = p.argb.clone();
                        let render_img = p.render.clone();
                        let t = self.tiles.get_mut(&key).unwrap();
                        t.render_image = Some(render_img);
                        t.image = Some((argb, zl));
                        t.loaded = true;
                    }
                    self.request_structures(ws, region);
                    break;
                }
            }
        }

        // everything below runs on a pool worker: cache probing, file
        // deletes, png decode, nbt render and cache writes are all file IO +
        // CPU work that must not stall the UI thread — and neither does the
        // Job construction itself, which the dispatcher now batches.
        self.loading.insert(key, hash);
        let priority = self.tile_priority(region);
        renders.push((region, zoom_level, priority));
    }

    /// structures-cache lookup for a ready tile: one-shot worker job (was a
    /// UI-side file read per tile). `structures_checked` is set at enqueue so
    /// update() can't queue duplicates; the Structures result fills
    /// `t.structures` without clobbering a fresher render-supplied map.
    fn request_structures(&mut self, ws: &crate::config::WorldState, region: Point2i) {
        let key = region.as_long();
        match self.tiles.get(&key) {
            Some(t) if t.structures.is_none() && !t.structures_checked => {}
            _ => return,
        }
        if let Some(t) = self.tiles.get_mut(&key) {
            t.structures_checked = true;
        }
        let tx = self.result_tx.clone();
        let uuid = ws.uuid.clone();
        let priority = self.tile_priority(region);
        // structure parsing itself piggybacks on the image job; this only
        // reads the small bincode cache file for pool-adopted tiles.
        jobs::pools().add_process(Job::new(
            jobs::PRIORITY_MEDIUM,
            Some(Box::new(move || priority)),
            move || {
                let s = cache::get_structures(&uuid, region);
                let _ = tx.send(TileResult::Structures { region, structures: s });
            },
        ));
    }

    /// the only pool writer — called once per landed worker result, so each
    /// pooled Arc is inserted exactly once (re-adopts read, never re-push).
    fn push_pool(&mut self, zoom: i32, region: Point2i, img: PooledImage) {
        let key = region.as_long();
        // trim removes every non-visible entry anyway, so position in `order`
        // is irrelevant — a plain deduped append keeps pushes O(1) instead of
        // the old O(pool size) retain-per-push.
        if self.pool.entry(zoom).or_default().insert(key, img).is_none() {
            self.pool_order.entry(zoom).or_default().push(key);
        }
        self.trim_pool(zoom);
    }

    /// run the full visible-set trim at most every Nth over-limit push — the
    /// trim itself is O(visible regions) and used to run per pooled image,
    /// which went quadratic while panning at far zoom.
    const POOL_TRIM_EVERY: u32 = 16;

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
        self.pool_trim_defer += 1;
        if self.pool_trim_defer < Self::POOL_TRIM_EVERY {
            return;
        }
        self.pool_trim_defer = 0;
        let scale = self.scale * 2.0;
        let (min, max) = {
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

    /// drop in-memory tile state for `region` (pool entries, load flags,
    /// overlays, structures) so the next update() re-requests it; the stale
    /// image stays on the tile as a placeholder like upstream's loaded=false.
    /// With `disk` also remove the cached pngs + meta (upstream clearTile).
    pub fn invalidate_region(&mut self, region: Point2i, disk: bool) {
        let key = region.as_long();
        for m in self.pool.values_mut() {
            m.remove(&key);
        }
        for o in self.pool_order.values_mut() {
            o.retain(|r| *r != key);
        }
        self.loading.remove(&key);
        self.overlay_loading.remove(&key);
        if let Some(t) = self.tiles.get_mut(&key) {
            t.loaded = false;
            t.overlay_data = None;
            t.overlay_loaded = false;
            t.overlay_render_image = None;
            if disk {
                t.structures = None;
            }
            t.structures_checked = false;
        }
        if disk {
            if let Some(ws) = crate::config::world() {
                cache::delete_data(&ws.uuid, region);
                for zl in ZOOM_LEVELS {
                    let _ = std::fs::remove_file(files::png_cache_path(&ws.cache_dir, zl, region));
                }
            }
        }
    }

    /// upstream CacheHelper.clearAllCaches -> TileMap.clearCache: images,
    /// overlays AND structure icons all come from caches — everything must be
    /// re-fetched after a clear (overlay_loaded/structures_checked would
    /// otherwise suppress re-enqueueing forever).
    pub fn mark_all_tiles_obsolete(&mut self) {
        for t in self.tiles.values_mut() {
            t.loaded = false;
            t.overlay_loaded = false;
            t.overlay_data = None;
            t.overlay_render_image = None;
            t.structures = None;
            t.structures_checked = false;
        }
        self.pool.clear();
        self.pool_order.clear();
        self.overlay_loading.clear();
    }

    /// Drain async tile results (call every UI tick).
    /// Returns true if anything changed.
    pub fn poll_results(&mut self) -> bool {
        let mut changed = false;
        let mut pending = std::mem::take(&mut self.pending_scratch);
        while let Ok(res) = self.results_rx.try_recv() {
            pending.push(res);
        }
        if pending.is_empty() {
            self.pending_scratch = pending;
            return false;
        }
        // world lookup hoisted: it takes a lock + clones strings, and used to
        // run 2-3x per drained result.
        let ws = crate::config::world();
        for res in pending.drain(..) {
            changed = true;
            match res {
                TileResult::Image {
                    region,
                    image,
                    zoom,
                    structures,
                } => {
                    let key = region.as_long();
                    let enq = self.loading.remove(&key);
                    // a result rendered (or a cached png loaded) under old
                    // render settings must not land — keep it unloaded so the
                    // next update re-renders under current settings.
                    if let (Some(h), Some(ws)) = (enq, &ws) {
                        if h != render_hash(&ws.cfg, ws.dim_kind()) {
                            if let Some(t) = self.tiles.get_mut(&key) {
                                t.loaded = false;
                            }
                            continue;
                        }
                    }
                    let Some(ti) = image else {
                        // worker-side render failure: leave the tile unloaded
                        // so update() re-requests. This arm used to set
                        // loaded=true because it doubled as the batch-op
                        // invalidation signal (now TileResult::Invalidate).
                        if let Some(t) = self.tiles.get_mut(&key) {
                            t.loaded = false;
                        }
                        continue;
                    };
                    // workers ship the gpu image prebuilt; the UI thread just
                    // moves Arcs.
                    let image = Arc::new(ti.argb);
                    let mut render = ti.render;
                    if render.is_none() {
                        let size = (image.len() as f64).sqrt() as usize;
                        render = argb_to_render_image(&image, size);
                    }
                    let view_zoom = get_zoom_level(self.scale);
                    // renders that land at a stale zoom are still valid
                    // data — keep them in the pool and adopt them as the
                    // tile's progressive image (paint_map stretches any
                    // resolution to the tile rect on the GPU), unless the
                    // tile already shows an image at least as sharp. An
                    // image at exactly the current zoom is never clobbered.
                    if zoom != view_zoom {
                        if let Some(ri) = &render {
                            self.push_pool(
                                zoom,
                                region,
                                PooledImage {
                                    argb: image.clone(),
                                    render: ri.clone(),
                                },
                            );
                        }
                        let adopt = match self.tiles.get(&key) {
                            Some(t) if t.image.is_some() => {
                                let cur = t.image_zoom_level();
                                cur != view_zoom && zoom <= cur
                            }
                            _ => true,
                        };
                        if adopt {
                            let t = self.tiles.entry(key).or_insert_with(|| Tile::new(region));
                            t.render_image = render.clone();
                            t.image = Some((image, zoom));
                            if let Some(s) = structures {
                                t.structures = Some(s);
                                t.structures_checked = true;
                            }
                            t.loaded = true;
                        }
                        continue;
                    }
                    if let Some(ri) = &render {
                        self.push_pool(
                            zoom,
                            region,
                            PooledImage {
                                argb: image.clone(),
                                render: ri.clone(),
                            },
                        );
                    }
                    if let Some(t) = self.tiles.get_mut(&key) {
                        if let Some(ri) = render {
                            t.render_image = Some(ri);
                        }
                        t.image = Some((image, zoom));
                        if let Some(s) = structures {
                            t.structures = Some(s);
                            t.structures_checked = true;
                        }
                        t.loaded = true;
                    }
                }
                TileResult::Invalidate { region } => {
                    let key = region.as_long();
                    self.loading.remove(&key);
                    self.overlay_loading.remove(&key);
                    for m in self.pool.values_mut() {
                        m.remove(&key);
                    }
                    for o in self.pool_order.values_mut() {
                        o.retain(|r| *r != key);
                    }
                    if let Some(t) = self.tiles.get_mut(&key) {
                        t.loaded = false;
                        t.image = None;
                        t.render_image = None;
                        t.structures = None;
                        t.structures_checked = false;
                        t.overlay_data = None;
                        t.overlay_loaded = false;
                        t.overlay_render_image = None;
                    }
                }
                TileResult::JobInfo(msg) => {
                    self.job_info = Some(msg);
                }
                TileResult::FilterResult { selection } => {
                    self.filter_result = Some(selection);
                }
                TileResult::Structures { region, structures } => {
                    if let Some(t) = self.tiles.get_mut(&region.as_long()) {
                        // don't clobber fresher structures a completed render
                        // result already installed.
                        if t.structures.is_none() {
                            t.structures = structures;
                        }
                        t.structures_checked = true;
                    }
                }
                TileResult::Presence { region, image } => {
                    if let Some(t) = self.tiles.get_mut(&region.as_long()) {
                        if !t.loaded {
                            // the presence placeholder is a fixed 32x32
                            // chunk-existence map = zoom 16, regardless of
                            // the view zoom that requested it.
                            t.render_image = argb_to_render_image(&image, 32);
                            t.image = Some((Arc::new(image), (TILE_SIZE / 32) as i32));
                        }
                    }
                }
                TileResult::Overlay { region, image, data } => {
                    self.overlay_loading.remove(&region.as_long());
                    if let Some(t) = self.tiles.get_mut(&region.as_long()) {
                        if let Some(img) = image {
                            if self.overlay.is_some() {
                                t.overlay_render_image = argb_to_render_image(&img, 32);
                                t.overlay_data = data;
                            }
                        }
                        t.overlay_loaded = true;
                    }
                }
            }
        }
        if pending.capacity() > MAX_PENDING_RESULTS {
            pending = Vec::new();
        }
        self.pending_scratch = pending;
        changed
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
    /// 16x16 corrupted-chunk overlay pixels. Contract: written once at app
    /// start and only ever read on the same (UI) thread at job-enqueue time —
    /// workers get the snapshot through ProbeArgs, never the thread_local.
    pub static CORRUPTED: std::cell::RefCell<[u32; 256]> = std::cell::RefCell::new([0; 256]);
}

pub fn set_corrupted_overlay(argb: [u32; 256]) {
    CORRUPTED.with(|c| *c.borrow_mut() = argb);
}


/// Build a gpu-side image from an argb buffer of size×size pixels.
/// Called on worker threads (tile jobs) — the only UI-thread callers left
/// are the tiny 32x32 Presence/Overlay images and view.rs icons.
pub fn argb_to_render_image(argb: &[u32], size: usize) -> Option<std::sync::Arc<gpui::RenderImage>> {
    // the blade atlas texture is Bgra8Unorm — bytes go B,G,R,A, which is
    // exactly the little-endian memory layout of a 0xAARRGGBB u32, so this
    // is a straight widen-and-copy (was a 4x per-byte push loop).
    let mut rgba = Vec::with_capacity(size * size * 4);
    rgba.extend(argb.iter().flat_map(|p| p.to_le_bytes()));
    let buf = image::RgbaImage::from_raw(size as u32, size as u32, rgba)?;
    Some(std::sync::Arc::new(gpui::RenderImage::new(
        smallvec::SmallVec::from_elem(image::Frame::new(buf), 1),
    )))
}

/// 0xAARRGGBB pixels -> RGBA bytes for the png writer (the tile buffer's
/// little-endian layout is BGRA — right for the gpu atlas, wrong for files).
pub fn argb_to_rgba(argb: &[u32]) -> Vec<u8> {
    let mut rgba = Vec::with_capacity(argb.len() * 4);
    rgba.extend(
        argb.iter()
            .flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, *p as u8, (p >> 24) as u8]),
    );
    rgba
}

// -- worker-side tile probe (runs on pool threads, not the UI) --------------

struct ProbeArgs {
    tx: std::sync::mpsc::Sender<TileResult>,
    uuid: String,
    cache_dir: std::path::PathBuf,
    region_dir: std::path::PathBuf,
    region: Point2i,
    zoom_level: i32,
    cfg: mc_core::render::RenderSettings,
    header_zoom: i32,
    lod_color: u32,
    hash: i64,
    corrupted: [u32; 256],
}

/// decode a cached png (+ downscale to target zoom) and build the gpu image
/// while still on the worker — returns (argb pixels, render image).
fn decode_png_scaled(
    path: &std::path::Path,
    load_zoom: i32,
    target_zoom: i32,
) -> Option<(Vec<u32>, std::sync::Arc<gpui::RenderImage>)> {
    let im = image::open(path).ok()?;
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
    let gpu_img = argb_to_render_image(&px, TILE_SIZE / target_zoom as usize)?;
    Some((px, gpu_img))
}

/// Everything the UI thread used to do synchronously per tile request:
/// staleness probe, file deletes, png/lod decode, chunk-presence read, the
/// mca render itself, and all cache writes. Sends at most one Image result
/// (plus an optional Presence placeholder for far zoom).
fn worker_probe_and_render(a: ProbeArgs) {
    let region = a.region;
    let key_path = a.region_dir.join(files::mca_file_name(region));
    let mt = std::fs::metadata(&key_path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);

    // staleness: render-settings hash mismatch (anything cached is stale), or
    // file time changed under the same settings.
    let outdated = if cache::get_render_hash(&a.uuid, region) != a.hash {
        cache::get_file_time(&a.uuid, region) != -1
            || files::png_cache_path(&a.cache_dir, 1, region).exists()
    } else {
        let t = cache::get_file_time(&a.uuid, region);
        t != -1 && t != mt
    };
    if outdated {
        cache::delete_data(&a.uuid, region);
        for zl in ZOOM_LEVELS {
            let _ = std::fs::remove_file(files::png_cache_path(&a.cache_dir, zl, region));
        }
    }

    if !outdated {
        if a.zoom_level >= 32 {
            if let Some(img) = cache::get_lod_image(&a.uuid, region) {
                let size = (img.len() as f64).sqrt() as usize;
                let _ = a.tx.send(TileResult::Image {
                    region,
                    image: Some(TileImage {
                        render: argb_to_render_image(&img, size),
                        argb: img,
                    }),
                    zoom: (TILE_SIZE / size) as i32,
                    structures: cache::get_structures(&a.uuid, region),
                });
                return;
            }
        } else {
            let png = files::png_cache_path(&a.cache_dir, a.zoom_level, region);
            if png.exists() {
                if let Some((img, gpu_img)) = decode_png_scaled(&png, a.zoom_level, a.zoom_level) {
                    let _ = a.tx.send(TileResult::Image {
                        region,
                        image: Some(TileImage {
                            argb: img,
                            render: Some(gpu_img),
                        }),
                        zoom: a.zoom_level,
                        structures: cache::get_structures(&a.uuid, region),
                    });
                    return;
                }
            }
            // finer cached images downscale exactly to the target zoom — done.
            for zl in ZOOM_LEVELS {
                if zl >= a.zoom_level {
                    continue;
                }
                let png = files::png_cache_path(&a.cache_dir, zl, region);
                if png.exists() {
                    if let Some((img, gpu_img)) = decode_png_scaled(&png, zl, a.zoom_level) {
                        let _ = a.tx.send(TileResult::Image {
                            region,
                            image: Some(TileImage {
                                argb: img,
                                render: Some(gpu_img),
                            }),
                            zoom: a.zoom_level,
                            structures: cache::get_structures(&a.uuid, region),
                        });
                        return;
                    }
                }
            }
            // coarser cached images are only placeholders: send one for
            // instant paint, then keep going and render the real zoom —
            // returning here left the tile tagged at the coarse zoom and
            // update() re-requested it forever (queue never drained).
            for zl in ZOOM_LEVELS {
                if zl <= a.zoom_level {
                    continue;
                }
                let png = files::png_cache_path(&a.cache_dir, zl, region);
                if png.exists() {
                    let img = decode_png_scaled(&png, zl, zl)
                        .map(|(argb, gpu_img)| TileImage {
                            argb,
                            render: Some(gpu_img),
                        });
                    let _ = a.tx.send(TileResult::Image {
                        region,
                        image: img,
                        zoom: zl,
                        structures: cache::get_structures(&a.uuid, region),
                    });
                    break;
                }
            }
        }
    }

    // far zoom: chunk-presence placeholder before the real render.
    if a.zoom_level >= a.header_zoom {
        if let Some(cs) = cache::get_chunks(&a.uuid, region) {
            let img = render::lod_image(&cs, a.lod_color);
            let _ = a.tx.send(TileResult::Presence { region, image: img });
        }
    }

    // real render
    let send_fail = |a: &ProbeArgs| {
        let _ = a.tx.send(TileResult::Image {
            region,
            image: None,
            zoom: a.zoom_level,
            structures: None,
        });
    };
    let mut m = match McaFile::new(key_path.clone()) {
        Ok(m) => m,
        Err(_) => return send_fail(&a),
    };
    if m.load(true).is_err() {
        return send_fail(&a);
    }
    let Some(img) = render::generate_image(&m, a.zoom_level, &a.cfg, &a.corrupted) else {
        return send_fail(&a);
    };
    let structures = m.structures();

    // persist: png pyramid (or lod) + meta. all worker-side; the UI just
    // adopts the pixels.
    if a.zoom_level >= 32 {
        cache::set_lod_image(&a.uuid, region, &img);
    } else {
        let size = TILE_SIZE / a.zoom_level as usize;
        let path = files::png_cache_path(&a.cache_dir, a.zoom_level, region);
        if let Some(p) = path.parent() {
            let _ = std::fs::create_dir_all(p);
        }
        let rgba = argb_to_rgba(&img);
        let _ = image::save_buffer(&path, &rgba, size as u32, size as u32, image::ColorType::Rgba8);
    }
    cache::set_file_time(&a.uuid, region, mt);
    cache::set_render_hash(&a.uuid, region, a.hash);
    cache::set_structures(&a.uuid, region, &structures);

    let _ = a.tx.send(TileResult::Image {
        region,
        image: Some(TileImage {
            render: argb_to_render_image(&img, TILE_SIZE / a.zoom_level as usize),
            argb: img,
        }),
        zoom: a.zoom_level,
        structures: Some(structures),
    });
}
