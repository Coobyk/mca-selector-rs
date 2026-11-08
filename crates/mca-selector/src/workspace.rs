//! The main workspace: menu bar, toolbar, map canvas, status bar.
//!
//! Rendering pipeline: the main thread never parses chunks. Region renders
//! are requested over a channel to a single driver task (background), which
//! runs `mc_core::map::region::render_region_file` — the per-chunk work is
//! spread over the rayon pool. Results come back over a second channel and
//! are applied to the model on the main thread. All zoomed-out views paint
//! one image per region, so a frame is a few dozen sprites regardless of
//! world size.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use gpui::{
    App, Bounds, Context, Corners, Entity, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    ScrollWheelEvent, SharedString, Task, Window, canvas, div, fill, prelude::*, px, rgb, size,
};
use rayon::prelude::*;
use mc_core::chunk_fast::parse_chunk;
use mc_core::region::Region;

use crate::map_state::*;

const MENU_HEIGHT: f64 = 28.0;
const TOOLBAR_HEIGHT: f64 = 44.0;
const STATUS_HEIGHT: f64 = 26.0;
/// Colors of the chrome, close to the original MCA Selector dark theme.
const CHROME_BG: u32 = 0x2B2D31;
const CHROME_DARK: u32 = 0x1E1F22;
const ACCENT: u32 = 0x3C78D8;
const TEXT: u32 = 0xDDDDDD;
const MAP_BG: u32 = 0x101014;
const UNLOADED: u32 = 0x22252C;

/// Convert an RGBA byte color into gpui's paint color.
fn color4(c: [u8; 4]) -> gpui::Hsla {
    let hex = u32::from_be_bytes(c);
    gpui::rgba(hex).into()
}

fn empty_image() -> gpui::RenderImage {
    let img = image::RgbaImage::from_raw(1, 1, vec![0, 0, 0, 0]).unwrap();
    gpui::RenderImage::new(vec![image::Frame::new(img)])
}

/// GPUI's blade atlas stores polychrome textures as Bgra8Unorm and
/// `paint_image` uploads frame bytes verbatim, so manually built
/// RenderImage frames must already be BGRA (the img element swizzles
/// RGBA itself; paint_image does not).
fn to_bgra(mut rgba: Vec<u8>) -> Vec<u8> {
    for px in rgba.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    rgba
}

/// A request for one region render, stamped with the generation it was made
/// under so stale work can be dropped cheaply.
type RegionRequest = (u64, RegionCoord, PathBuf);

pub struct Workspace {
    pub map: MapState,
    /// Directory the export dialog starts from.
    pub export_dir: String,
    /// Open export dialog, if any.
    pub export_dialog: Option<ExportDialog>,
    /// Error from the last world-open attempt, shown on the landing screen.
    pub open_error: Option<String>,
    /// Sender for region render requests, consumed by the driver task.
    req_tx: async_channel::Sender<RegionRequest>,
    /// Cached selection overlay, rebuilt when the selection mutates so
    /// the paint loop does not iterate the whole set every frame:
    /// merged row spans (cz, x0, x1) plus perimeter edge masks
    /// (cx, cz, bits N=1 S=2 W=4 E=8).
    sel_spans: Vec<(i32, i32, i32)>,
    sel_edges: Vec<(i32, i32, u8)>,
    _tasks: Vec<Task<()>>,
}

impl Workspace {
    pub fn new(
        world: Option<mc_core::world::World>,
        open_error: Option<String>,
        cx: &mut App,
    ) -> Entity<Self> {
        // Channels between the UI thread and the render driver.
        let (req_tx, req_rx) = async_channel::unbounded::<RegionRequest>();
        let (res_tx, res_rx) =
            async_channel::unbounded::<(u64, mc_core::map::region::RegionRender)>();
        let gen = Arc::new(AtomicU64::new(0));

        // Drivers: a few background tasks pull region requests off the
        // shared channel so several regions render concurrently; each
        // render additionally spreads its chunks across the rayon pool.
        // Spawned below and kept alive in `_tasks` (dropping a gpui task
        // cancels it).
        let mut drivers = Vec::new();
        for _ in 0..4 {
            let gen_driver = gen.clone();
            let res_tx = res_tx.clone();
            let req_rx = req_rx.clone();
            drivers.push(cx.background_spawn(async move {
                while let Ok((req_gen, coord, path)) = req_rx.recv().await {
                    // Stale request (dimension switched meanwhile): skip.
                    if req_gen != gen_driver.load(Ordering::Relaxed) {
                        continue;
                    }
                    if let Some(rendered) =
                        mc_core::map::region::render_region_file(&path, coord)
                    {
                        if res_tx.send((req_gen, rendered)).await.is_err() {
                            break;
                        }
                    }
                }
            }));
        }

        let this_req_tx = req_tx.clone();
        let entity = cx.new(|cx| {
            let mut ws = Self {
                map: MapState {
                    world,
                    dimension: 0,
                    render_gen: gen.clone(),
                    regions: HashMap::new(),
                    region_base: HashMap::new(),
                    region_detail: DetailCache::new(200),
                    building_regions: Default::default(),
                    tiles: HashMap::new(),
                    pending: Default::default(),
                    upscaled: Default::default(),
                    selection: Default::default(),
                    view: ViewState::new(),
                    select_drag: None,
                    pan_drag: None,
                    mouse: None,
                    viewport: (800.0, 600.0),
                    show_chunk_grid: true,
                    show_region_grid: true,
                    needs_fit: false,
                    load_started: std::time::Instant::now(),
                    load_timed: false,
                },
                open_error,
                export_dir: std::env::var("MCA_EXPORT_DIR")
                    .unwrap_or_else(|_| "/tmp/mca-selector-export".to_string()),
                export_dialog: None,
                req_tx: this_req_tx,
                sel_spans: Vec::new(),
                sel_edges: Vec::new(),
                _tasks: Vec::new(),
            };
            // Prefer the overworld as the initial dimension.
            if let Some(i) = ws
                .map
                .world
                .as_ref()
                .and_then(|w| w.dimensions.iter().position(|d| d.name == "Overworld"))
            {
                ws.map.dimension = i;
            }
            if ws.map.world.is_some() {
                ws.load_dimension(cx);
            }
            ws
        });

        // Receiver: apply finished region renders to the model.
        let weak = entity.downgrade();
        let gen_receiver = gen.clone();
        let receiver = cx.spawn(async move |cx| {
            while let Ok((res_gen, rendered)) = res_rx.recv().await {
                let coord = rendered.coord;
                let _ = weak.update(cx, |ws, cx| {
                    ws.map.building_regions.remove(&coord);
                    if res_gen != gen_receiver.load(Ordering::Relaxed) {
                        return;
                    }
                    let base = image::RgbaImage::from_raw(
                        mc_core::map::region::BASE_IMG as u32,
                        mc_core::map::region::BASE_IMG as u32,
                        to_bgra(rendered.base256),
                    );
                    if let Some(img) = base {
                        ws.map.region_base.insert(
                            coord,
                            Arc::new(gpui::RenderImage::new(vec![image::Frame::new(img)])),
                        );
                    }
                    let detail = image::RgbaImage::from_raw(
                        mc_core::map::region::REGION_IMG as u32,
                        mc_core::map::region::REGION_IMG as u32,
                        to_bgra(rendered.img512),
                    );
                    if let Some(img) = detail {
                        ws.map
                            .region_detail
                            .insert(coord, Arc::new(gpui::RenderImage::new(vec![image::Frame::new(img)])));
                    }
                    if !ws.map.load_timed
                        && ws.map.regions.len() > 0
                        && ws.map.region_base.len() >= ws.map.regions.len()
                    {
                        ws.map.load_timed = true;
                        eprintln!(
                            "[mca-selector] full world ({} regions) rendered in {:?}",
                            ws.map.regions.len(),
                            ws.map.load_started.elapsed()
                        );
                    }
                    cx.notify();
                });
            }
        });
        entity.update(cx, |ws, _| {
            ws._tasks.extend(drivers);
            ws._tasks.push(receiver);
        });

        entity
    }

    /// Open a world folder via the platform directory picker and load it,
    /// replacing whatever is open.
    pub fn open_world_via_dialog(&mut self, cx: &mut Context<Self>) {
        let task = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Open a Minecraft world folder".into()),
        });
        let task = cx.spawn(async move |this, cx| {
            // The prompt resolves to Result<Result<Option<paths>>, Canceled>.
            let picked = match task.await {
                Ok(Ok(Some(paths))) => paths.into_iter().next(),
                Ok(Ok(None)) => None, // user cancelled
                Ok(Err(e)) => {
                    let _ = this.update(cx, |ws, _cx| {
                        ws.open_error = Some(format!(
                            "The system folder picker failed: {e}. \
                             Pass the world folder as a CLI argument instead."
                        ));
                    });
                    return;
                }
                Err(_) => return, // task cancelled
            };
            let Some(path) = picked else {
                return; // user cancelled
            };
            let opened = mc_core::world::World::open(&path);
            let _ = this.update(cx, |ws, cx| match opened {
                Ok(world) => {
                    ws.map.world = Some(world);
                    ws.map.dimension = 0;
                    if let Some(i) = ws
                        .map
                        .world
                        .as_ref()
                        .and_then(|w| w.dimensions.iter().position(|d| d.name == "Overworld"))
                    {
                        ws.map.dimension = i;
                    }
                    ws.open_error = None;
                    ws.load_dimension(cx);
                }
                Err(e) => {
                    ws.open_error = Some(format!("Could not open {}: {e}", path.display()));
                }
            });
        });
        self._tasks.push(task);
    }

    /// Load the region list of the current dimension (background, in
    /// parallel), then notify.
    fn load_dimension(&mut self, cx: &mut Context<Self>) {
        // Invalidate all in-flight work for the previous dimension.
        let gen = self
            .map
            .render_gen
            .fetch_add(1, Ordering::Relaxed)
            + 1;
        let _ = gen;
        self.map.tiles.clear();
        self.map.pending.clear();
        self.map.upscaled.borrow_mut().clear();
        self.map.region_base.clear();
        self.map.region_detail.clear();
        self.map.building_regions.clear();
        self.map.regions.clear();
        self.map.selection.clear();
        self.rebuild_selection_overlay();
        let Some(world) = self.map.world.clone() else {
            return;
        };
        let Some(dim) = world.dimensions.get(self.map.dimension).cloned() else {
            return;
        };
        let io = cx.background_spawn(async move {
            let files = dim.region_files().unwrap_or_default();
            // Presence bitmaps come straight from the 8 KiB sector tables;
            // scan them in parallel.
            let infos: Vec<((i32, i32), RegionInfo)> = files
                .into_par_iter()
                .filter_map(|(rx, rz, path)| {
                    let mut info = RegionInfo {
                        path,
                        ..Default::default()
                    };
                    if let Ok(region) = Region::open(&info.path) {
                        for lz in 0..32i32 {
                            let mut word = 0u64;
                            for lx in 0..32i32 {
                                if region.has_chunk(lx as u8, lz as u8).unwrap_or(false) {
                                    word |= 1 << lx;
                                }
                            }
                            info.present[lz as usize] = word;
                        }
                    } else {
                        return None;
                    }
                    Some(((rx, rz), info))
                })
                .collect();
            infos
        });
        let task = cx.spawn(async move |this, cx| {
            let infos = io.await;
            let _ = this.update(cx, |ws, cx| {
                for (coord, info) in infos {
                    ws.map.regions.insert(coord, info);
                }
                ws.map.needs_fit = true;
                ws.map.load_started = std::time::Instant::now();
                ws.map.load_timed = false;
                cx.notify();
            });
        });
        self._tasks.push(task);
    }

    pub fn set_dimension(&mut self, index: usize, cx: &mut Context<Self>) {
        let len = self
            .map
            .world
            .as_ref()
            .map(|w| w.dimensions.len())
            .unwrap_or(0);
        if index < len && index != self.map.dimension {
            self.map.dimension = index;
            self.load_dimension(cx);
        }
    }

    /// Center the view on the loaded regions and pick a zoom that shows all
    /// of them.
    fn fit_view(&mut self) {
        let Some(((min_rx, min_rz), (max_rx, max_rz))) = self
            .map
            .regions
            .keys()
            .copied()
            .fold(None, |acc: Option<((i32, i32), (i32, i32))>, c| match acc {
                None => Some((c, c)),
                Some((lo, hi)) => Some((
                    (c.0.min(lo.0), c.1.min(lo.1)),
                    (c.0.max(hi.0), c.1.max(hi.1)),
                )),
            })
        else {
            return;
        };
        let width_blocks = ((max_rx - min_rx + 1) * 512) as f64;
        let height_blocks = ((max_rz - min_rz + 1) * 512) as f64;
        self.map.view.center = (
            (min_rx * 512) as f64 + width_blocks / 2.0,
            (min_rz * 512) as f64 + height_blocks / 2.0,
        );
        // MCA_INIT_ZOOM is an intentional starting zoom; fit only centers.
        if std::env::var_os("MCA_INIT_ZOOM").is_some() {
            return;
        }
        let (vw, vh) = self.map.viewport;
        // Leave some margin.
        let zoom = ((vw - 40.0) / width_blocks)
            .min((vh - 40.0) / height_blocks)
            .clamp(1.0 / 512.0, 32.0);
        self.map.view.zoom = zoom;
    }

    /// Region coordinate range currently visible.
    fn visible_regions(&self) -> (i32, i32, i32, i32) {
        let vp = self.map.viewport;
        let tl = self.map.view.screen_to_block((0.0, 0.0), vp);
        let br = self.map.view.screen_to_block((vp.0, vp.1), vp);
        (
            (tl.0.floor() as i32).div_euclid(512) - 1,
            (br.0.floor() as i32).div_euclid(512) + 1,
            (tl.1.floor() as i32).div_euclid(512) - 1,
            (br.1.floor() as i32).div_euclid(512) + 1,
        )
    }

    /// Request rendering work for the current view.
    ///
    /// LOD rules:
    /// - chunk size on screen >= 32px: per-chunk tasks (few chunks visible);
    /// - otherwise: region renders through the driver, producing the
    ///   permanent 256px base image plus a 512px detail image, so the
    ///   canvas paints a handful of images per frame no matter how far out
    ///   the view is.
    fn request_tiles(&mut self, cx: &mut Context<Self>) {
        if self.map.world.is_none() {
            return;
        }
        // While the view is being dragged, do not re-enumerate and sort
        // the missing set on every mouse-move repaint — already-pending
        // tasks keep rendering; new requests resume on release.
        if self.map.pan_drag.is_some() {
            return;
        }
        let chunk_px = self.map.view.zoom * 16.0;
        let (min_rx, max_rx, min_rz, max_rz) = self.visible_regions();
        let gen = self.map.render_gen.load(Ordering::Relaxed);

        // Focus point for render priority: the cursor when it hovers the
        // map, else the viewport center.
        let vp = self.map.viewport;
        let focus = self
            .map
            .mouse
            .map(|m| self.map.view.screen_to_block(m, vp))
            .unwrap_or(self.map.view.center);

        if chunk_px >= 32.0 {
            const MAX_PENDING: usize = 256;
            if self.map.pending.len() >= MAX_PENDING {
                return;
            }
            // Only the chunks intersecting the viewport (+1 margin) are
            // queued, in one batched task per region.
            let tl = self.map.view.screen_to_block((0.0, 0.0), vp);
            let br = self.map.view.screen_to_block((vp.0, vp.1), vp);
            let min_cx = (tl.0.floor() as i32).div_euclid(16) - 1;
            let max_cx = (br.0.floor() as i32).div_euclid(16) + 1;
            let min_cz = (tl.1.floor() as i32).div_euclid(16) - 1;
            let max_cz = (br.1.floor() as i32).div_euclid(16) + 1;
            let mut missing: Vec<(ChunkCoord, f64)> = Vec::new();
            for czw in min_cz..=max_cz {
                for cxw in min_cx..=max_cx {
                    let (rx, rz) = (cxw.div_euclid(32), czw.div_euclid(32));
                    let Some(info) = self.map.regions.get(&(rx, rz)) else {
                        continue;
                    };
                    if !info.has(cxw.rem_euclid(32), czw.rem_euclid(32)) {
                        continue;
                    }
                    let coord = (cxw, czw);
                    if self.map.tiles.contains_key(&coord)
                        || self.map.pending.contains(&coord)
                    {
                        continue;
                    }
                    let dx = (cxw * 16 + 8) as f64 - focus.0;
                    let dz = (czw * 16 + 8) as f64 - focus.1;
                    missing.push((coord, dx * dx + dz * dz));
                }
            }
            missing.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
            let budget = MAX_PENDING - self.map.pending.len();
            let mut by_region: HashMap<(i32, i32), Vec<ChunkCoord>> = HashMap::new();
            for (coord, _) in missing.into_iter().take(budget) {
                by_region
                    .entry((coord.0.div_euclid(32), coord.1.div_euclid(32)))
                    .or_default()
                    .push(coord);
            }
            // Batches stay small so a moving cursor redirects the render
            // within one batch's lifetime instead of waiting for a whole
            // region queued under a stale focus point.
            for ((rx, rz), coords) in by_region {
                for batch in coords.chunks(96) {
                    self.spawn_region_tile_task(rx, rz, batch.to_vec(), cx);
                }
            }
            // Bound the zoom-in cache by dropping tiles outside the
            // visible range rather than wiping everything.
            if self.map.tiles.len() > 20_000 {
                self.map.tiles.retain(|&(x, z), _| {
                    x >= min_cx - 64 && x <= max_cx + 64 && z >= min_cz - 64 && z <= max_cz + 64
                });
            }
            return;
        }

        // Region-LOD: queue whole regions for the driver. Requests are
        // sorted by distance from the focus point so the visible area
        // completes first.
        let want_detail = chunk_px * 32.0 >= 256.0;
        let mut missing: Vec<(RegionCoord, f64, PathBuf)> = Vec::new();
        for rz in min_rz..=max_rz {
            for rx in min_rx..=max_rx {
                let Some(info) = self.map.regions.get(&(rx, rz)) else {
                    continue;
                };
                if info.count() == 0 || self.map.building_regions.contains(&(rx, rz)) {
                    continue;
                }
                let need_base = !self.map.region_base.contains_key(&(rx, rz));
                let need_detail = want_detail && !self.map.region_detail.contains(&(rx, rz));
                if !need_base && !need_detail {
                    continue;
                }
                let dx = (rx * 512 + 256) as f64 - focus.0;
                let dz = (rz * 512 + 256) as f64 - focus.1;
                missing.push(((rx, rz), dx * dx + dz * dz, info.path.clone()));
            }
        }
        missing.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        for (coord, _, path) in missing {
            if self.map.building_regions.insert(coord) {
                let _ = self.req_tx.try_send((gen, coord, path));
            }
        }
    }

    /// Render a batch of chunk tiles of one region (zoomed-in view) in a
    /// single background task: the file is opened once and each needed
    /// chunk is parsed once even when shared by several tiles.
    fn spawn_region_tile_task(
        &mut self,
        rx: i32,
        rz: i32,
        coords: Vec<ChunkCoord>,
        cx: &mut Context<Self>,
    ) {
        let Some(info) = self.map.regions.get(&(rx, rz)) else {
            return;
        };
        let path = info.path.clone();
        for coord in &coords {
            self.map.pending.insert(*coord);
        }
        let io = cx.background_spawn(async move { render_region_tiles(&path, &coords) });
        let task = cx.spawn(async move |this, cx| {
            let tiles = io.await;
            let _ = this.update(cx, |ws, cx| {
                for (coord, tile) in tiles {
                    ws.map.pending.remove(&coord);
                    ws.map.tiles.insert(coord, tile);
                }
                cx.notify();
            });
        });
        self._tasks.push(task);
    }

    pub fn select_chunks_in_rect(&mut self, drag: SelectDrag) {
        let x0 = drag.start.0.min(drag.current.0);
        let x1 = drag.start.0.max(drag.current.0);
        let z0 = drag.start.1.min(drag.current.1);
        let z1 = drag.start.1.max(drag.current.1);
        match drag.mode {
            SelectMode::Replace => {
                self.map.selection.clear();
                for z in z0..=z1 {
                    for x in x0..=x1 {
                        self.map.selection.insert((x, z));
                    }
                }
            }
            SelectMode::Add => {
                for z in z0..=z1 {
                    for x in x0..=x1 {
                        self.map.selection.insert((x, z));
                    }
                }
            }
            SelectMode::Subtract => {
                for z in z0..=z1 {
                    for x in x0..=x1 {
                        self.map.selection.remove(&(x, z));
                    }
                }
            }
        }
        self.rebuild_selection_overlay();
    }

    /// Rebuild the cached selection overlay (merged row spans and
    /// perimeter edges). Called after every selection mutation instead
    /// of doing this work per painted frame.
    fn rebuild_selection_overlay(&mut self) {
        self.sel_spans.clear();
        self.sel_edges.clear();
        let sel = &self.map.selection;
        if sel.is_empty() {
            return;
        }
        let mut rows: HashMap<i32, Vec<i32>> = HashMap::new();
        for &(cxw, czw) in sel.iter() {
            rows.entry(czw).or_default().push(cxw);
            let mut mask = 0u8;
            if !sel.contains(&(cxw, czw - 1)) {
                mask |= 1;
            }
            if !sel.contains(&(cxw, czw + 1)) {
                mask |= 2;
            }
            if !sel.contains(&(cxw - 1, czw)) {
                mask |= 4;
            }
            if !sel.contains(&(cxw + 1, czw)) {
                mask |= 8;
            }
            if mask != 0 {
                self.sel_edges.push((cxw, czw, mask));
            }
        }
        for (czw, mut xs) in rows {
            xs.sort_unstable();
            let mut x0 = xs[0];
            let mut prev = xs[0];
            for &x in &xs[1..] {
                if x > prev + 1 {
                    self.sel_spans.push((czw, x0, prev));
                    x0 = x;
                }
                prev = x;
            }
            self.sel_spans.push((czw, x0, prev));
        }
    }

    pub fn clear_selection(&mut self) {
        self.map.selection.clear();
        self.rebuild_selection_overlay();
    }

    pub fn invert_selection(&mut self) {
        let all: Vec<ChunkCoord> = self
            .map
            .regions
            .iter()
            .flat_map(|((rx, rz), info)| {
                (0..32i32).flat_map(move |lz| {
                    (0..32i32).filter_map(move |lx| {
                        if info.has(lx, lz) {
                            Some((rx * 32 + lx, rz * 32 + lz))
                        } else {
                            None
                        }
                    })
                })
            })
            .collect();
        for c in all {
            if !self.map.selection.remove(&c) {
                self.map.selection.insert(c);
            }
        }
        self.rebuild_selection_overlay();
    }

    pub fn zoom(&mut self, factor: f64) {
        let vp = self.map.viewport;
        self.map.view.zoom_at(factor, (vp.0 / 2.0, vp.1 / 2.0), vp);
    }

    // ---- rendering ----

    fn render_toolbar_button(&self, label: &str) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .text_size(px(13.0))
            .text_color(rgb(TEXT))
            .rounded_sm()
            .hover(|s| s.bg(rgb(0x3A3C42)))
            .cursor_pointer()
            .child(SharedString::from(label.to_string()))
    }

    fn render_map(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let vs = window.viewport_size();
        self.map.viewport = (
            vs.width.to_f64(),
            vs.height.to_f64() - MENU_HEIGHT - TOOLBAR_HEIGHT - STATUS_HEIGHT,
        );
        if self.map.needs_fit {
            self.map.needs_fit = false;
            self.fit_view();
        }
        self.request_tiles(cx);
        let entity = cx.entity();
        div()
            .id("map-area")
            .relative()
            .flex_1()
            .bg(rgb(MAP_BG))
            .overflow_hidden()
            .on_mouse_down(gpui::MouseButton::Left, cx.listener(Self::on_left_down))
            .on_mouse_down(gpui::MouseButton::Middle, cx.listener(Self::on_middle_down))
            .on_mouse_down(gpui::MouseButton::Right, cx.listener(Self::on_right_down))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_mouse_up(gpui::MouseButton::Left, cx.listener(Self::on_left_up))
            .on_mouse_up(gpui::MouseButton::Middle, cx.listener(Self::on_drag_end))
            .on_mouse_up(gpui::MouseButton::Right, cx.listener(Self::on_left_up))
            .on_scroll_wheel(cx.listener(Self::on_scroll))
            .child(canvas(
                move |bounds, _window, _cx| bounds,
                move |bounds, _state, window, cx| {
                    let ws = entity.read(cx);
                    ws.paint_map(bounds, window);
                },
            )
            .size_full())
    }

    fn paint_map(&self, bounds: Bounds<gpui::Pixels>, window: &mut gpui::Window) {
        // The canvas bounds are the paint viewport.
        let vp = (bounds.size.width.to_f64(), bounds.size.height.to_f64());
        let view = self.map.view;
        let ox = bounds.origin.x.to_f64();
        let oy = bounds.origin.y.to_f64();

        // Ocean-like background.
        window.paint_quad(fill(bounds, rgb(MAP_BG)));

        let (min_rx, max_rx, min_rz, max_rz) = self.visible_regions();
        let chunk_px = view.zoom * 16.0;
        let region_px = chunk_px * 32.0;

        for rz in min_rz..=max_rz {
            for rx in min_rx..=max_rx {
                let region_origin =
                    view.block_to_screen(((rx * 512) as f64, (rz * 512) as f64), vp);
                let region_bounds = bounds_at(
                    ox + region_origin.0,
                    oy + region_origin.1,
                    region_px,
                    region_px,
                );
                if offscreen(region_bounds, bounds) {
                    continue;
                }

                // Zoomed-in: paint per-chunk tiles.
                if chunk_px >= 32.0 {
                    let Some(info) = self.map.regions.get(&(rx, rz)) else {
                        continue;
                    };
                    for lz in 0..32i32 {
                        for lx in 0..32i32 {
                            if !info.has(lx, lz) {
                                continue;
                            }
                            let cxw = rx * 32 + lx;
                            let czw = rz * 32 + lz;
                            let origin = view.block_to_screen(
                                ((cxw * 16) as f64, (czw * 16) as f64),
                                vp,
                            );
                            let chunk_bounds =
                                bounds_at(ox + origin.0, oy + origin.1, chunk_px, chunk_px);
                            if offscreen(chunk_bounds, bounds) {
                                continue;
                            }
                            if let Some(tile) = self.map.tiles.get(&(cxw, czw)) {
                                // Pre-replicate the 16x16 tile so the
                                // GPU's linear sampler interpolates between
                                // identical pixels: crisp block edges.
                                let factor = next_pow2_scale(chunk_px);
                                let img = upscaled_image(
                                    &self.map.upscaled,
                                    (cxw, czw),
                                    &tile.image,
                                    factor,
                                );
                                let _ = window.paint_image(
                                    chunk_bounds,
                                    Corners::default(),
                                    img,
                                    0,
                                    false,
                                );
                            } else {
                                window.paint_quad(fill(chunk_bounds, rgb(UNLOADED)));
                            }
                        }
                    }
                } else {
                    // Zoomed-out: one image per region. The 512px detail
                    // image is used once a region is painted larger than
                    // its base resolution; the 256px base covers everything
                    // else. Missing images are dark placeholders until the
                    // background render finishes.
                    let image = if region_px >= 256.0 {
                        self.map.region_detail.get(&(rx, rz))
                    } else {
                        None
                    }
                    .or_else(|| self.map.region_base.get(&(rx, rz)).cloned());
                    match image {
                        Some(img) => {
                            let _ = window.paint_image(
                                region_bounds,
                                Corners::default(),
                                img,
                                0,
                                false,
                            );
                        }
                        None => {
                            window.paint_quad(fill(region_bounds, rgb(UNLOADED)));
                        }
                    }
                }

                // Chunk grid: thin 1px lines, the smaller cousins of the
                // region borders.
                if self.map.show_chunk_grid && chunk_px >= 8.0 {
                    let grid = gpui::rgba(0x00000040);
                    for i in 0..=32i32 {
                        let gx = ox + region_origin.0 + (i as f64) * chunk_px;
                        window.paint_quad(fill(
                            bounds_at(gx, oy + region_origin.1, 1.0, region_px),
                            grid,
                        ));
                        let gy = oy + region_origin.1 + (i as f64) * chunk_px;
                        window.paint_quad(fill(
                            bounds_at(ox + region_origin.0, gy, region_px, 1.0),
                            grid,
                        ));
                    }
                }
            }
        }

        // Region borders on top.
        if self.map.show_region_grid {
            let border = gpui::rgba(0x00000088);
            let thickness = 2.0_f64.min((region_px / 16.0).max(1.0));
            for rz in min_rz..=max_rz {
                for rx in min_rx..=max_rx {
                    if !self.map.regions.contains_key(&(rx, rz)) {
                        continue;
                    }
                    let origin =
                        view.block_to_screen(((rx * 512) as f64, (rz * 512) as f64), vp);
                    let x = ox + origin.0 - thickness / 2.0;
                    let y = oy + origin.1 - thickness / 2.0;
                    window.paint_quad(fill(
                        bounds_at(x, y, region_px + thickness, thickness),
                        border,
                    ));
                    window.paint_quad(fill(
                        bounds_at(x, y, thickness, region_px + thickness),
                        border,
                    ));
                    // Right/bottom edges only when no neighboring region
                    // draws its own top/left edge there.
                    if !self.map.regions.contains_key(&(rx + 1, rz)) {
                        window.paint_quad(fill(
                            bounds_at(x + region_px, y, thickness, region_px + thickness),
                            border,
                        ));
                    }
                    if !self.map.regions.contains_key(&(rx, rz + 1)) {
                        window.paint_quad(fill(
                            bounds_at(x, y + region_px, region_px + thickness, thickness),
                            border,
                        ));
                    }
                }
            }
        }

        // Selection overlay: the span/edge geometry is cached by
        // rebuild_selection_overlay on mutation; painting only maps
        // it to screen space. Fills first, then the perimeter border.
        let sel_fill = gpui::rgba(0x3C78D848);
        let sel_border = gpui::rgba(0x7FA8F0FF);
        for &(czw, x0, x1) in &self.sel_spans {
            let origin = view.block_to_screen(((x0 * 16) as f64, (czw * 16) as f64), vp);
            let w = ((x1 - x0 + 1) * 16) as f64 * view.zoom;
            let b = bounds_at(ox + origin.0, oy + origin.1, w, chunk_px);
            if !offscreen(b, bounds) {
                window.paint_quad(fill(b, sel_fill));
            }
        }
        let t = 1.0f64.max(chunk_px / 24.0).min(2.0);
        for &(cxw, czw, mask) in &self.sel_edges {
            let origin = view.block_to_screen(((cxw * 16) as f64, (czw * 16) as f64), vp);
            let x = ox + origin.0;
            let y = oy + origin.1;
            let mut edge = |b: Bounds<gpui::Pixels>| {
                if !offscreen(b, bounds) {
                    window.paint_quad(fill(b, sel_border));
                }
            };
            if mask & 1 != 0 {
                edge(bounds_at(x, y, chunk_px, t));
            }
            if mask & 2 != 0 {
                edge(bounds_at(x, y + chunk_px - t, chunk_px, t));
            }
            if mask & 4 != 0 {
                edge(bounds_at(x, y, t, chunk_px));
            }
            if mask & 8 != 0 {
                edge(bounds_at(x + chunk_px - t, y, t, chunk_px));
            }
        }

        // Active selection rectangle.
        if let Some(drag) = &self.map.select_drag {
            let x0 = drag.start.0.min(drag.current.0);
            let x1 = drag.start.0.max(drag.current.0);
            let z0 = drag.start.1.min(drag.current.1);
            let z1 = drag.start.1.max(drag.current.1);
            let origin = view.block_to_screen(((x0 * 16) as f64, (z0 * 16) as f64), vp);
            let w = ((x1 - x0 + 1) * 16) as f64 * view.zoom;
            let h = ((z1 - z0 + 1) * 16) as f64 * view.zoom;
            let rect = bounds_at(ox + origin.0, oy + origin.1, w, h);
            window.paint_quad(fill(rect, gpui::rgba(0xFFFFFF18)));
            paint_outline(window, rect, 1.5, gpui::rgba(0xFFFFFFFF).into());
        }
    }

    // ---- event handlers ----

    fn on_left_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let pos = self.map_local(event.position, window);
        let vp = self.map.viewport;
        // Left-drag adds to the selection; Alt subtracts; right-drag
        // subtracts (see on_right_down). Replace is gone: clear the
        // selection from the menu to start over.
        let mode = if event.modifiers.alt {
            SelectMode::Subtract
        } else {
            SelectMode::Add
        };
        let block = self.map.view.screen_to_block(pos, vp);
        let start = (
            (block.0.floor() as i32).div_euclid(16),
            (block.1.floor() as i32).div_euclid(16),
        );
        self.map.select_drag = Some(SelectDrag {
            start,
            current: start,
            mode,
        });
        cx.notify();
    }

    fn on_middle_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let pos = self.map_local(event.position, window);
        self.map.pan_drag = Some(PanDrag {
            start_center: self.map.view.center,
            start_pos: pos,
        });
        cx.notify();
    }

    /// Right-drag removes chunks from the selection.
    fn on_right_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let pos = self.map_local(event.position, window);
        let vp = self.map.viewport;
        let block = self.map.view.screen_to_block(pos, vp);
        let start = (
            (block.0.floor() as i32).div_euclid(16),
            (block.1.floor() as i32).div_euclid(16),
        );
        self.map.select_drag = Some(SelectDrag {
            start,
            current: start,
            mode: SelectMode::Subtract,
        });
        cx.notify();
    }

    fn on_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let pos = self.map_local(event.position, window);
        self.map.mouse = Some(pos);
        if let Some(drag) = self.map.pan_drag.as_mut() {
            self.map.view.center = (
                drag.start_center.0 - (pos.0 - drag.start_pos.0) / self.map.view.zoom,
                drag.start_center.1 - (pos.1 - drag.start_pos.1) / self.map.view.zoom,
            );
        }
        if let Some(drag) = self.map.select_drag.as_mut() {
            let block = self.map.view.screen_to_block(pos, self.map.viewport);
            drag.current = (
                (block.0.floor() as i32).div_euclid(16),
                (block.1.floor() as i32).div_euclid(16),
            );
        }
        cx.notify();
    }

    fn on_left_up(
        &mut self,
        _event: &MouseUpEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(drag) = self.map.select_drag.take() {
            self.select_chunks_in_rect(drag);
        }
        cx.notify();
    }

    fn on_drag_end(
        &mut self,
        _event: &MouseUpEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.map.pan_drag = None;
        self.request_tiles(cx);
        cx.notify();
    }

    fn on_scroll(
        &mut self,
        event: &ScrollWheelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let pos = self.map_local(event.position, window);
        let delta = event.delta.pixel_delta(px(1.0));
        let lines = (delta.y.to_f64() + delta.x.to_f64()) / 40.0;
        let factor = 1.7_f64.powf(lines.clamp(-6.0, 6.0));
        self.map.view.zoom_at(factor, pos, self.map.viewport);
        cx.notify();
    }

    /// Window mouse position mapped into the map area.
    fn map_local(&self, window_pos: gpui::Point<gpui::Pixels>, _window: &mut Window) -> (f64, f64) {
        (
            window_pos.x.to_f64(),
            window_pos.y.to_f64() - MENU_HEIGHT - TOOLBAR_HEIGHT,
        )
    }
}

/// Render a batch of chunk tiles of one region: the file is opened once
/// and each needed chunk is parsed once, even when shared as the
/// N/W/NW neighbor of several tiles.
fn render_region_tiles(
    path: &std::path::Path,
    coords: &[ChunkCoord],
) -> Vec<(ChunkCoord, Arc<TileEntry>)> {
    let mut out = Vec::with_capacity(coords.len());
    let Ok(region) = Region::open(path) else {
        return out;
    };
    let mut chunks: HashMap<(i32, i32), Option<Arc<mc_core::chunk_fast::FastChunk>>> =
        HashMap::new();
    let mut load = |nx: i32, nz: i32| -> Option<Arc<mc_core::chunk_fast::FastChunk>> {
        if !(0..32).contains(&nx) || !(0..32).contains(&nz) {
            return None;
        }
        if let Some(c) = chunks.get(&(nx, nz)) {
            return c.clone();
        }
        let c = region
            .chunk_data(nx as u8, nz as u8)
            .ok()
            .and_then(|b| parse_chunk(&b))
            .map(Arc::new);
        chunks.insert((nx, nz), c.clone());
        c
    };
    for &coord in coords {
        let lx = coord.0.rem_euclid(32);
        let lz = coord.1.rem_euclid(32);
        let center = load(lx, lz);
        let north = load(lx, lz - 1);
        let west = load(lx - 1, lz);
        let nw = load(lx - 1, lz - 1);

        // Grid covering the chunk plus its N/W/NW margin, so Xaero's
        // slope (north/north-west) and biome-blending lookups work
        // across the chunk borders.
        let tile = (|| {
            let mut grid = mc_core::map::ColumnGrid::empty(48, 48);
            for (gx, gz, chunk) in [
                (16i32, 16i32, center.as_ref()),
                (16, 0, north.as_ref()),
                (0, 16, west.as_ref()),
                (0, 0, nw.as_ref()),
            ]
            .into_iter()
            .filter_map(|(gx, gz, c)| c.map(|c| (gx, gz, c)))
            {
                if let Some(cols) = mc_core::map::chunk_columns(chunk, &mut |n| {
                    grid.biome_id(n)
                }) {
                    for z in 0..16usize {
                        for x in 0..16usize {
                            let si = z * 16 + x;
                            let di = (gz as usize + z) * 48 + gx as usize + x;
                            grid.base[di] = cols.base[si];
                            grid.tint[di] = cols.tint[si];
                            grid.water_depth[di] = cols.water_depth[si];
                            grid.floor_height[di] = cols.floor_height[si];
                            grid.biome[di] = cols.biome[si];
                            grid.glowing[di] = cols.glowing[si];
                        }
                    }
                }
            }
            let full = mc_core::map::render_columns(&grid)?;
            let mut rgba = Vec::with_capacity(16 * 16 * 4);
            for z in 16..32usize {
                for x in 16..32usize {
                    let si = (z * 48 + x) * 4;
                    rgba.extend_from_slice(&full[si..si + 4]);
                }
            }
            let img = image::RgbaImage::from_raw(16, 16, to_bgra(rgba))?;
            let render_image = gpui::RenderImage::new(vec![image::Frame::new(img)]);
            Some(Arc::new(TileEntry {
                image: Arc::new(render_image),
            }))
        })()
        .unwrap_or_else(|| {
            Arc::new(TileEntry {
                image: Arc::new(empty_image()),
            })
        });
        out.push((coord, tile));
    }
    out
}

/// Smallest power-of-two replication factor that covers the on-screen
/// chunk size (chunk_px / 16 rounded up), capped so tiles stay small
/// and the upscale cache does not churn at extreme zoom.
fn next_pow2_scale(chunk_px: f64) -> u32 {
    ((chunk_px / 16.0).ceil() as usize)
        .max(1)
        .next_power_of_two()
        .clamp(1, 16) as u32
}

/// Nearest-neighbor `factor`x upscale of a 16x16 tile image, cached per
/// (chunk, factor). Linear sampling inside the replicated runs of identical
/// pixels cannot blur block edges.
fn upscaled_image(
    cache: &std::cell::RefCell<
        HashMap<(ChunkCoord, u32), Arc<gpui::RenderImage>>,
    >,
    coord: ChunkCoord,
    tile: &gpui::RenderImage,
    factor: u32,
) -> Arc<gpui::RenderImage> {
    let key = (coord, factor);
    if let Some(img) = cache.borrow().get(&key) {
        return img.clone();
    }
    let n = 16usize;
    let f = factor as usize;
    let shift = f.trailing_zeros();
    let src = tile.as_bytes(0).unwrap_or(&[]).to_vec();
    let mut out = vec![0u8; n * f * n * f * 4];
    for y in 0..n * f {
        let sy = (y >> shift) * n * 4;
        for x in 0..n * f {
            let sx = (x >> shift) * 4;
            let di = (y * n * f + x) * 4;
            out[di..di + 4].copy_from_slice(&src[sy + sx..sy + sx + 4]);
        }
    }
    let img = Arc::new(gpui::RenderImage::new(vec![image::Frame::new(
        image::RgbaImage::from_raw((n * f) as u32, (n * f) as u32, out)
            .expect("upscaled tile size mismatch"),
    )]));
    let mut c = cache.borrow_mut();
    if c.len() > 2048 {
        c.clear();
    }
    c.insert(key, img.clone());
    img
}

/// Bounds at device coordinates `x, y` with size `w, h` in pixels.
fn bounds_at(x: f64, y: f64, w: f64, h: f64) -> Bounds<gpui::Pixels> {
    Bounds::new(
        gpui::Point {
            x: px(x as f32),
            y: px(y as f32),
        },
        gpui::Size {
            width: px(w.max(0.1) as f32),
            height: px(h.max(0.1) as f32),
        },
    )
}

fn offscreen(b: Bounds<gpui::Pixels>, viewport: Bounds<gpui::Pixels>) -> bool {
    b.origin.x > viewport.origin.x + viewport.size.width
        || b.origin.y > viewport.origin.y + viewport.size.height
        || b.origin.x + b.size.width < viewport.origin.x
        || b.origin.y + b.size.height < viewport.origin.y
}

/// Paint a 1px-wide outline of `bounds` as four thin quads.
fn paint_outline(
    window: &mut gpui::Window,
    bounds: Bounds<gpui::Pixels>,
    width: f32,
    color: gpui::Hsla,
) {
    let w = px(width);
    let background: gpui::Hsla = color;
    let sides = [
        Bounds::new(bounds.origin, size(bounds.size.width, w)),
        Bounds::new(
            gpui::point(bounds.origin.x, bounds.origin.y + bounds.size.height - w),
            size(bounds.size.width, w),
        ),
        Bounds::new(
            gpui::point(bounds.origin.x, bounds.origin.y + w),
            size(w, (bounds.size.height - w * 2.0).max(px(0.0))),
        ),
        Bounds::new(
            gpui::point(bounds.origin.x + bounds.size.width - w, bounds.origin.y + w),
            size(w, (bounds.size.height - w * 2.0).max(px(0.0))),
        ),
    ];
    for side in sides {
        window.paint_quad(fill(side, background));
    }
}

/// Modal state for exporting the selection.
#[derive(Clone)]
pub struct ExportDialog {
    pub path: String,
    pub message: Option<String>,
}

impl Workspace {
    pub fn open_export_dialog(&mut self) {
        self.export_dialog = Some(ExportDialog {
            path: self.export_dir.clone(),
            message: None,
        });
    }

    pub fn run_export(&mut self, path: String, cx: &mut Context<Self>) {
        self.export_dir = path.clone();
        let selection: std::collections::BTreeSet<ChunkCoord> =
            self.map.selection.iter().copied().collect();
        if selection.is_empty() {
            if let Some(d) = self.export_dialog.as_mut() {
                d.message = Some("Nothing is selected.".into());
            }
            cx.notify();
            return;
        }
        let Some(world) = self.map.world.clone() else {
            if let Some(d) = self.export_dialog.as_mut() {
                d.message = Some("No world is open.".into());
            }
            cx.notify();
            return;
        };
        let Some(dim) = world.dimensions.get(self.map.dimension).cloned() else {
            return;
        };
        let io = cx.background_spawn(async move {
            std::fs::create_dir_all(&path).ok();
            mc_core::export::export_selection(&world, &dim, &selection, std::path::Path::new(&path))
        });
        let task = cx.spawn(async move |this, cx| {
            let result = io.await;
            let _ = this.update(cx, |ws, cx| {
                match result {
                    Ok(stats) => {
                        if let Some(d) = ws.export_dialog.as_mut() {
                            d.message = Some(format!(
                                "Exported {} chunks into {} region files at {}",
                                stats.chunks, stats.regions, ws.export_dir
                            ));
                        }
                    }
                    Err(e) => {
                        if let Some(d) = ws.export_dialog.as_mut() {
                            d.message = Some(format!("Export failed: {e}"));
                        }
                    }
                }
                cx.notify();
            });
        });
        self._tasks.push(task);
    }
}

impl gpui::Render for Workspace {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Top menu bar (in-window; the original's File/View/Selection menus).
        let menu_labels = ["File", "View", "Selection", "Tools"];
        let menu_bar = div()
            .flex()
            .items_center()
            .h(px(MENU_HEIGHT as f32))
            .px_2()
            .gap_4()
            .bg(rgb(CHROME_DARK))
            .child(
                div()
                    .text_size(px(13.0))
                    .text_color(rgb(0xFFFFFF))
                    .font_weight(gpui::FontWeight::BOLD)
                    .child("mca-selector-rs"),
            )
            .children(menu_labels.map(|label| {
                div()
                    .px_2()
                    .py_0p5()
                    .rounded_sm()
                    .text_size(px(13.0))
                    .text_color(rgb(TEXT))
                    .hover(|s| s.bg(rgb(0x3A3C42)))
                    .cursor_pointer()
                    .child(label)
            }));

        // Toolbar.
        let dim = self
            .map
            .world
            .as_ref()
            .map(|w| w.dimensions[self.map.dimension].clone());
        let mut toolbar = div()
            .flex()
            .items_center()
            .h(px(TOOLBAR_HEIGHT as f32))
            .px_2()
            .gap_1()
            .bg(rgb(CHROME_BG))
            .border_b_1()
            .border_color(rgb(0x111214))
            .child(
                div()
                    .id("btn-open")
                    .px_2()
                    .py_1()
                    .text_size(px(13.0))
                    .text_color(rgb(TEXT))
                    .rounded_sm()
                    .hover(|s| s.bg(rgb(0x3A3C42)))
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _ev, _window, cx| {
                        this.open_world_via_dialog(cx);
                    }))
                    .child("Open world…"),
            )
            .child(
                div()
                    .id("btn-export")
                    .px_2()
                    .py_1()
                    .text_size(px(13.0))
                    .text_color(rgb(TEXT))
                    .rounded_sm()
                    .hover(|s| s.bg(rgb(0x3A3C42)))
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _ev, _window, cx| {
                        this.open_export_dialog();
                        cx.notify();
                    }))
                    .child("Export selection"),
            )
            .child(self.render_toolbar_button("Import chunks"))
            .child(div().w_2())
            .children(dim.is_some().then(|| {
                div()
                    .text_size(px(12.0))
                    .text_color(rgb(0x888888))
                    .child("Dimension:")
            }));
        if let Some(world) = &self.map.world {
            for (i, d) in world.dimensions.iter().enumerate() {
                let active = i == self.map.dimension;
                let name = d.name.clone();
                toolbar = toolbar.child(
                    div()
                        .id(SharedString::from(format!("dim-{i}")))
                        .px_2()
                        .py_1()
                        .text_size(px(13.0))
                        .rounded_sm()
                        .cursor_pointer()
                        .when(active, |s| s.bg(rgb(ACCENT)).text_color(rgb(0xFFFFFF)))
                        .when(!active, |s| {
                            s.text_color(rgb(TEXT)).hover(|s| s.bg(rgb(0x3A3C42)))
                        })
                        .on_click(cx.listener(move |this, _ev, _window, cx| {
                            this.set_dimension(i, cx);
                        }))
                        .child(SharedString::from(name)),
                );
            }
        }
        toolbar = toolbar
            .child(div().w_2())
            .child(self.render_toolbar_button("Goto"))
            .child(
                div()
                    .id("toggle-chunk-grid")
                    .px_2()
                    .py_1()
                    .text_size(px(13.0))
                    .rounded_sm()
                    .cursor_pointer()
                    .when(self.map.show_chunk_grid, |s| {
                        s.bg(rgb(ACCENT)).text_color(rgb(0xFFFFFF))
                    })
                    .when(!self.map.show_chunk_grid, |s| {
                        s.text_color(rgb(TEXT)).hover(|s| s.bg(rgb(0x3A3C42)))
                    })
                    .on_click(cx.listener(|this, _ev, _window, cx| {
                        this.map.show_chunk_grid = !this.map.show_chunk_grid;
                        cx.notify();
                    }))
                    .child("Chunk grid"),
            )
            .child(
                div()
                    .id("toggle-region-grid")
                    .px_2()
                    .py_1()
                    .text_size(px(13.0))
                    .rounded_sm()
                    .cursor_pointer()
                    .when(self.map.show_region_grid, |s| {
                        s.bg(rgb(ACCENT)).text_color(rgb(0xFFFFFF))
                    })
                    .when(!self.map.show_region_grid, |s| {
                        s.text_color(rgb(TEXT)).hover(|s| s.bg(rgb(0x3A3C42)))
                    })
                    .on_click(cx.listener(|this, _ev, _window, cx| {
                        this.map.show_region_grid = !this.map.show_region_grid;
                        cx.notify();
                    }))
                    .child("Region grid"),
            )
            .child(div().flex_1())
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(rgb(0x888888))
                    .child(SharedString::from(match &dim {
                        Some(_) => format!(
                            "{} · {}/{} regions rendered",
                            self.map
                                .world
                                .as_ref()
                                .map(|w| w.dimensions[self.map.dimension].name.clone())
                                .unwrap_or_default(),
                            self.map.region_base.len(),
                            self.map.regions.len(),
                        ),
                        None => "No world open".to_string(),
                    })),
            );

        // Status bar, like the original: selection info left, coordinates
        // and zoom right.
        let zoom_pct = self.map.view.zoom * 100.0;
        let (block_text, chunk_text) = match self.map.mouse {
            Some((mx, my)) => {
                let b = self.map.view.screen_to_block((mx, my), self.map.viewport);
                let bx = b.0.floor() as i64;
                let bz = b.1.floor() as i64;
                let chunk = (bx.div_euclid(16), bz.div_euclid(16));
                (
                    format!("Block: {bx}, {bz}"),
                    format!(
                        "Chunk: {}, {} in region {}, {}",
                        chunk.0,
                        chunk.1,
                        chunk.0.div_euclid(32),
                        chunk.1.div_euclid(32)
                    ),
                )
            }
            None => ("Block: –".to_string(), "Chunk: –".to_string()),
        };
        let status = div()
            .flex()
            .items_center()
            .h(px(STATUS_HEIGHT as f32))
            .px_2()
            .gap_4()
            .bg(rgb(CHROME_DARK))
            .text_size(px(12.0))
            .text_color(rgb(TEXT))
            .child(SharedString::from(format!(
                "Selected chunks: {}",
                self.map.selection.len()
            )))
            .child(div().flex_1())
            .child(block_text)
            .child(chunk_text)
            .child(SharedString::from(format!("Zoom: {zoom_pct:.1}%")));

        let map = self.render_map(_window, cx);

        let mut root = div()
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(CHROME_BG))
            .text_color(rgb(TEXT))
            .child(menu_bar)
            .child(toolbar)
            .child(map)
            .child(status);

        // Landing screen while no world is open: ask the user to pick a
        // world folder.
        if self.map.world.is_none() {
            let error = self.open_error.clone();
            root = root.child(
                div()
                    .absolute()
                    .inset_0()
                    .occlude()
                    .bg(gpui::rgba(0x000000AA))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .w(px(460.0))
                            .bg(rgb(CHROME_BG))
                            .border_1()
                            .border_color(rgb(0x4A4C52))
                            .rounded_lg()
                            .p_6()
                            .flex()
                            .flex_col()
                            .items_center()
                            .gap_3()
                            .child(
                                div()
                                    .text_size(px(20.0))
                                    .font_weight(gpui::FontWeight::BOLD)
                                    .text_color(rgb(0xFFFFFF))
                                    .child("mca-selector-rs"),
                            )
                            .child(
                                div()
                                    .text_size(px(13.0))
                                    .text_color(rgb(0x999999))
                                    .text_center()
                                    .child(
                                        "Open a Minecraft world folder to view its chunks, \
                                         select regions of the map and export them.",
                                    ),
                            )
                            .children(error.clone().map(|e| {
                                div()
                                    .text_size(px(13.0))
                                    .text_color(rgb(0xE07A6A))
                                    .text_center()
                                    .child(SharedString::from(e))
                            }))
                            .child(
                                div()
                                    .id("landing-open")
                                    .mt_2()
                                    .px_4()
                                    .py_2()
                                    .rounded_md()
                                    .text_size(px(14.0))
                                    .text_color(rgb(0xFFFFFF))
                                    .bg(rgb(ACCENT))
                                    .cursor_pointer()
                                    .hover(|s| s.bg(rgb(0x4A88E8)))
                                    .on_click(cx.listener(|this, _ev, _window, cx| {
                                        this.open_world_via_dialog(cx);
                                    }))
                                    .child("Open world…"),
                            ),
                    ),
            );
        }

        // Export modal, when open.
        if let Some(dialog) = &self.export_dialog {
            let path = dialog.path.clone();
            let message = dialog.message.clone();
            root = root.child(
                div()
                    .absolute()
                    .inset_0()
                    .occlude()
                    .bg(gpui::rgba(0x00000066))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .w(px(520.0))
                            .bg(rgb(CHROME_BG))
                            .border_1()
                            .border_color(rgb(0x4A4C52))
                            .rounded_lg()
                            .p_4()
                            .flex()
                            .flex_col()
                            .gap_3()
                            .child(
                                div()
                                    .text_size(px(15.0))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(rgb(0xFFFFFF))
                                    .child(format!(
                                        "Export {} selected chunks",
                                        self.map.selection.len()
                                    )),
                            )
                            .child(
                                div()
                                    .text_size(px(13.0))
                                    .text_color(rgb(TEXT))
                                    .child(SharedString::from(format!(
                                        "Destination folder: {path}"
                                    ))),
                            )
                            .children(message.clone().map(|m| {
                                div()
                                    .text_size(px(13.0))
                                    .text_color(rgb(0x8FBC8F))
                                    .child(SharedString::from(m))
                            }))
                            .child(
                                div()
                                    .flex()
                                    .justify_end()
                                    .gap_2()
                                    .child(
                                        div()
                                            .id("dialog-cancel")
                                            .px_3()
                                            .py_1()
                                            .rounded_sm()
                                            .text_size(px(13.0))
                                            .text_color(rgb(TEXT))
                                            .bg(rgb(0x3A3C42))
                                            .cursor_pointer()
                                            .on_click(cx.listener(|this, _ev, _window, cx| {
                                                this.export_dialog = None;
                                                cx.notify();
                                            }))
                                            .child("Cancel"),
                                    )
                                    .child(
                                        div()
                                            .id("dialog-export")
                                            .px_3()
                                            .py_1()
                                            .rounded_sm()
                                            .text_size(px(13.0))
                                            .text_color(rgb(0xFFFFFF))
                                            .bg(rgb(ACCENT))
                                            .cursor_pointer()
                                            .on_click(cx.listener(
                                                |this, _ev, _window, cx| {
                                                    let path = this
                                                        .export_dialog
                                                        .as_ref()
                                                        .map(|d| d.path.clone())
                                                        .unwrap_or_default();
                                                    this.run_export(path, cx);
                                                    cx.notify();
                                                },
                                            ))
                                            .child("Export"),
                                    ),
                            ),
                    ),
            );
        }

        root
    }
}
