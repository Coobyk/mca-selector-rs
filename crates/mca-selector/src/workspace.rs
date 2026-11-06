//! The main workspace: menu bar, toolbar, map canvas, status bar.

use std::collections::HashMap;
use std::sync::Arc;

use gpui::{
    App, Bounds, Context, Corners, Entity, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    ScrollWheelEvent, SharedString, Task, Window, canvas, div, fill, prelude::*, px, rgb, size,
};
use mc_core::chunk::Chunk;
use mc_core::map::colors::Rgba;
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
fn color4(c: Rgba) -> gpui::Hsla {
    let hex = u32::from_be_bytes(c);
    gpui::rgba(hex).into()
}

pub struct Workspace {
    pub map: MapState,
    /// Outstanding background tasks keep-alive.
    /// Directory the export dialog starts from.
    pub export_dir: String,
    /// Open export dialog, if any.
    pub export_dialog: Option<ExportDialog>,
    /// Error from the last world-open attempt, shown on the landing screen.
    pub open_error: Option<String>,
    _tasks: Vec<Task<()>>,
}

fn empty_image() -> gpui::RenderImage {
    let img = image::RgbaImage::from_raw(1, 1, vec![0, 0, 0, 0]).unwrap();
    gpui::RenderImage::new(vec![image::Frame::new(img)])
}

impl Workspace {
    pub fn new(world: Option<mc_core::world::World>, open_error: Option<String>, cx: &mut App) -> Entity<Self> {
        cx.new(|cx| {
            let mut ws = Self {
                map: MapState {
                    world,
                    dimension: 0,
                    regions: HashMap::new(),
                    region_summaries: HashMap::new(),
                    region_images: HashMap::new(),
                    building_regions: Default::default(),
                    tiles: HashMap::new(),
                    pending: Default::default(),
                    loading_regions: Default::default(),
                    selection: Default::default(),
                    view: ViewState::new(),
                    select_drag: None,
                    pan_drag: None,
                    mouse: None,
                    viewport: (800.0, 600.0),
                    show_chunk_grid: false,
                    show_region_grid: true,
                    needs_fit: false,
                },
                open_error,
                export_dir: std::env::var("MCA_EXPORT_DIR")
                    .unwrap_or_else(|_| "/tmp/mca-selector-export".to_string()),
                export_dialog: None,
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
        })
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

    /// Load the region list of the current dimension (background), then
    /// notify.
    fn load_dimension(&mut self, cx: &mut Context<Self>) {
        self.map.tiles.clear();
        self.map.region_summaries.clear();
        self.map.region_images.clear();
        self.map.building_regions.clear();
        self.map.regions.clear();
        self.map.pending.clear();
        self.map.selection.clear();
        let Some(world) = &self.map.world else {
            return;
        };
        let Some(dim) = world.dimensions.get(self.map.dimension).cloned() else {
            return;
        };
        let io = cx.background_spawn(async move {
            let files = dim.region_files().unwrap_or_default();
            let mut infos = Vec::with_capacity(files.len());
            for (rx, rz, path) in files {
                let mut info = RegionInfo {
                    path,
                    ..Default::default()
                };
                // The presence bitmap comes straight from the sector table,
                // which is cheap.
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
                }
                infos.push(((rx, rz), info));
            }
            infos
        });
        let task = cx.spawn(async move |this, cx| {
            let infos = io.await;
            let _ = this.update(cx, |ws, cx| {
                for (coord, info) in infos {
                    ws.map.regions.insert(coord, info);
                }
                ws.map.needs_fit = true;
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
        let (vw, vh) = self.map.viewport;
        // Leave some margin.
        let zoom = ((vw - 40.0) / width_blocks)
            .min((vh - 40.0) / height_blocks)
            .clamp(1.0 / 512.0, 32.0);
        self.map.view.zoom = zoom;
        self.map.view.center = (
            (min_rx * 512) as f64 + width_blocks / 2.0,
            (min_rz * 512) as f64 + height_blocks / 2.0,
        );
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
    /// - below that: one background task per region renders all of its
    ///   chunks and composites them into a single 512x512 image, so the
    ///   canvas paints a handful of images per frame no matter how far out
    ///   the view is. This keeps pan/zoom on the main thread.
    fn request_tiles(&mut self, cx: &mut Context<Self>) {
        if self.map.world.is_none() {
            return;
        }
        let chunk_px = self.map.view.zoom * 16.0;
        let (min_rx, max_rx, min_rz, max_rz) = self.visible_regions();

        if chunk_px >= 32.0 {
            const MAX_PENDING: usize = 512;
            if self.map.pending.len() >= MAX_PENDING {
                return;
            }
            let mut missing: Vec<(ChunkCoord, f64)> = Vec::new();
            for rz in min_rz..=max_rz {
                for rx in min_rx..=max_rx {
                    let Some(info) = self.map.regions.get(&(rx, rz)) else {
                        continue;
                    };
                    for lz in 0..32i32 {
                        for lx in 0..32i32 {
                            if !info.has(lx, lz) {
                                continue;
                            }
                            let coord = (rx * 32 + lx, rz * 32 + lz);
                            if self.map.tiles.contains_key(&coord)
                                || self.map.pending.contains(&coord)
                            {
                                continue;
                            }
                            let dx = (coord.0 * 16 + 8) as f64 - self.map.view.center.0;
                            let dz = (coord.1 * 16 + 8) as f64 - self.map.view.center.1;
                            missing.push((coord, dx * dx + dz * dz));
                        }
                    }
                }
            }
            missing.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
            let budget = MAX_PENDING - self.map.pending.len();
            for (coord, _) in missing.into_iter().take(budget) {
                self.spawn_tile_task(coord, cx);
            }
            return;
        }

        // Region-LOD: composite whole regions in the background.
        const MAX_BUILDING: usize = 8;
        for rz in min_rz..=max_rz {
            for rx in min_rx..=max_rx {
                if self.map.building_regions.len() >= MAX_BUILDING {
                    return;
                }
                let Some(info) = self.map.regions.get(&(rx, rz)) else {
                    continue;
                };
                if info.count() == 0
                    || self.map.region_images.contains_key(&(rx, rz))
                    || self.map.building_regions.contains(&(rx, rz))
                {
                    continue;
                }
                self.spawn_region_tile_task((rx, rz), cx);
            }
        }
    }

    /// Render every chunk of one region in the background and composite the
    /// tiles into a 512x512 RGBA image (1px per block).
    fn spawn_region_tile_task(&mut self, coord: RegionCoord, cx: &mut Context<Self>) {
        let (rx, rz) = coord;
        let Some(info) = self.map.regions.get(&coord) else {
            return;
        };
        let path = info.path.clone();
        self.map.building_regions.insert(coord);
        let io = cx.background_spawn(async move {
            let Ok(region) = Region::open(&path) else {
                return None;
            };
            let mut tiles: Vec<(ChunkCoord, Arc<TileEntry>)> = Vec::new();
            let mut sum = [0u64; 3];
            let mut count = 0u32;
            let mut img = vec![0u8; 512 * 512 * 4];
            for lz in 0..32i32 {
                for lx in 0..32i32 {
                    // Placeholder color for chunks without data.
                    let base = &mut img[(lz * 512 + lx * 16) as usize * 4..];
                    for py in 0..16usize {
                        for px in 0..16usize {
                            let o = (py * 512 + px) * 4;
                            base[o] = 0x22;
                            base[o + 1] = 0x25;
                            base[o + 2] = 0x2c;
                            base[o + 3] = 0xff;
                        }
                    }
                    if !region.has_chunk(lx as u8, lz as u8).unwrap_or(false) {
                        continue;
                    }
                    let Ok(bytes) = region.chunk_data(lx as u8, lz as u8) else {
                        continue;
                    };
                    let Ok(chunk) = Chunk::from_nbt(&bytes) else {
                        continue;
                    };
                    let tile = mc_core::map::render_tile(&chunk);
                    let ox = (lx * 16) as usize;
                    let oz = (lz * 16) as usize;
                    for py in 0..16usize {
                        for px in 0..16usize {
                            let si = (py * 16 + px) * 4;
                            let di = ((oz + py) * 512 + ox + px) * 4;
                            img[di..di + 4].copy_from_slice(&tile.rgba[si..si + 4]);
                        }
                    }
                    sum[0] += u64::from(tile.avg[0]);
                    sum[1] += u64::from(tile.avg[1]);
                    sum[2] += u64::from(tile.avg[2]);
                    count += 1;
                    let render_image =
                        gpui::RenderImage::new(vec![image::Frame::new(
                            image::RgbaImage::from_raw(16, 16, tile.rgba.clone())?,
                        )]);
                    tiles.push((
                        (rx * 32 + lx, rz * 32 + lz),
                        Arc::new(TileEntry {
                            image: Arc::new(render_image),
                            avg: tile.avg,
                            height: tile.max_height,
                        }),
                    ));
                }
            }
            let region_image = image::RgbaImage::from_raw(512, 512, img)?;
            Some((coord, tiles, (sum, count), region_image))
        });
        let task = cx.spawn(async move |this, cx| {
            if let Some((coord, tiles, (sum, count), region_image)) = io.await {
                let _ = this.update(cx, |ws, cx| {
                    for (chunk_coord, tile) in tiles {
                        ws.map.tiles.insert(chunk_coord, tile);
                    }
                    let summary = ws.map.region_summaries.entry(coord).or_default();
                    summary.sum[0] += sum[0];
                    summary.sum[1] += sum[1];
                    summary.sum[2] += sum[2];
                    summary.count += count;
                    let render_image =
                        gpui::RenderImage::new(vec![image::Frame::new(region_image)]);
                    ws.map.region_images.insert(coord, Arc::new(render_image));
                    ws.map.building_regions.remove(&coord);
                    cx.notify();
                });
            } else {
                let _ = this.update(cx, |ws, _cx| {
                    ws.map.building_regions.remove(&coord);
                });
            }
        });
        self._tasks.push(task);
    }

    fn spawn_tile_task(&mut self, coord: ChunkCoord, cx: &mut Context<Self>) {
        let (rx, rz) = (coord.0.div_euclid(32), coord.1.div_euclid(32));
        let Some(info) = self.map.regions.get(&(rx, rz)) else {
            return;
        };
        let path = info.path.clone();
        self.map.pending.insert(coord);
        let io = cx.background_spawn(async move {
            let tile = (|| -> Option<Arc<TileEntry>> {
                let region = Region::open(&path).ok()?;
                let bytes = region
                    .chunk_data(coord.0.rem_euclid(32) as u8, coord.1.rem_euclid(32) as u8)
                    .ok()?;
                let chunk = Chunk::from_nbt(&bytes).ok()?;
                let tile = mc_core::map::render_tile(&chunk);
                let img = image::RgbaImage::from_raw(16, 16, tile.rgba.clone())?;
                let render_image = gpui::RenderImage::new(vec![image::Frame::new(img)]);
                Some(Arc::new(TileEntry {
                    image: Arc::new(render_image),
                    avg: tile.avg,
                    height: tile.max_height,
                }))
            })()
            .unwrap_or_else(|| {
                Arc::new(TileEntry {
                    image: Arc::new(empty_image()),
                    avg: [0, 0, 0, 0],
                    height: 0,
                })
            });
            (coord, tile)
        });
        let task = cx.spawn(async move |this, cx| {
            let (coord, tile) = io.await;
            let _ = this.update(cx, |ws, cx| {
                ws.map.pending.remove(&coord);
                let (rx, rz) = (coord.0.div_euclid(32), coord.1.div_euclid(32));
                let summary = ws.map.region_summaries.entry((rx, rz)).or_default();
                summary.sum[0] += u64::from(tile.avg[0]);
                summary.sum[1] += u64::from(tile.avg[1]);
                summary.sum[2] += u64::from(tile.avg[2]);
                summary.count += 1;
                ws.map.tiles.insert(coord, tile);
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
    }

    pub fn clear_selection(&mut self) {
        self.map.selection.clear();
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
            .on_mouse_down(gpui::MouseButton::Right, cx.listener(Self::on_middle_down))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_mouse_up(gpui::MouseButton::Left, cx.listener(Self::on_left_up))
            .on_mouse_up(gpui::MouseButton::Middle, cx.listener(Self::on_drag_end))
            .on_mouse_up(gpui::MouseButton::Right, cx.listener(Self::on_drag_end))
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

                // Region-level LOD when regions are small on screen.
                if region_px < 48.0 {
                    let avg = self
                        .map
                        .region_summaries
                        .get(&(rx, rz))
                        .and_then(|s| s.avg());
                    let color = avg.unwrap_or([0x22, 0x25, 0x2c, 255]);
                    window.paint_quad(fill(region_bounds, color4(color)));
                    continue;
                }

                let Some(info) = self.map.regions.get(&(rx, rz)) else {
                    continue;
                };

                // Zoomed out: paint the whole region as one composited
                // 512x512 image (1px per block) so each frame is a handful
                // of images instead of thousands of chunk tiles. Falls back
                // to chunk-average quads while the region image is built.
                if chunk_px < 32.0 {
                    match self.map.region_images.get(&(rx, rz)) {
                        Some(img) => {
                            let _ = window.paint_image(
                                region_bounds,
                                Corners::default(),
                                img.clone(),
                                0,
                                false,
                            );
                        }
                        None => {
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
                                    let chunk_bounds = bounds_at(
                                        ox + origin.0,
                                        oy + origin.1,
                                        chunk_px,
                                        chunk_px,
                                    );
                                    if offscreen(chunk_bounds, bounds) {
                                        continue;
                                    }
                                    let color = self
                                        .map
                                        .tiles
                                        .get(&(cxw, czw))
                                        .map(|t| t.avg)
                                        .unwrap_or([0x22, 0x25, 0x2c, 255]);
                                    window.paint_quad(fill(chunk_bounds, color4(color)));
                                }
                            }
                        }
                    }
                } else {
                    for lz in 0..32i32 {
                        for lx in 0..32i32 {
                            if !info.has(lx, lz) {
                                continue;
                            }
                            let cxw = rx * 32 + lx;
                            let czw = rz * 32 + lz;
                            let origin = view
                                .block_to_screen(((cxw * 16) as f64, (czw * 16) as f64), vp);
                            let chunk_bounds = bounds_at(
                                ox + origin.0,
                                oy + origin.1,
                                chunk_px,
                                chunk_px,
                            );
                            if offscreen(chunk_bounds, bounds) {
                                continue;
                            }
                            if let Some(tile) = self.map.tiles.get(&(cxw, czw)) {
                                if let Err(e) = window.paint_image(
                                    chunk_bounds,
                                    Corners::default(),
                                    tile.image.clone(),
                                    0,
                                    false,
                                ) {
                                    static ONCE: std::sync::Once = std::sync::Once::new();
                                    ONCE.call_once(|| eprintln!("paint_image error: {e}"));
                                }
                            } else {
                                // Not yet rendered: neutral placeholder.
                                window.paint_quad(fill(chunk_bounds, rgb(UNLOADED)));
                            }
                        }
                    }
                }

                // Chunk grid.
                if self.map.show_chunk_grid && chunk_px >= 8.0 {
                    let grid = gpui::rgba(0xFFFFFF28);
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
                }
            }
        }

        // Selection overlay, drawn as horizontal spans so adjacent chunks
        // form one seamless rectangle.
        let sel_fill = gpui::rgba(0x3C78D848);
        let sel_border = gpui::rgba(0x7FA8F0FF);
        let mut spans: HashMap<i32, Vec<(i32, i32)>> = HashMap::new();
        for &(cxw, czw) in self.map.selection.iter() {
            spans.entry(czw).or_default().push((cxw, cxw));
        }
        let mut span_bounds: Vec<Bounds<gpui::Pixels>> = Vec::new();
        for (czw, mut runs) in spans {
            runs.sort_by_key(|r| r.0);
            // Merge contiguous runs.
            let mut merged: Vec<(i32, i32)> = Vec::with_capacity(runs.len());
            for (x0, x1) in runs {
                match merged.last_mut() {
                    Some((_, last_x1)) if x0 <= *last_x1 + 1 => {
                        *last_x1 = (*last_x1).max(x1);
                    }
                    _ => merged.push((x0, x1)),
                }
            }
            for (x0, x1) in merged {
                let origin = view.block_to_screen(((x0 * 16) as f64, (czw * 16) as f64), vp);
                let w = ((x1 - x0 + 1) * 16) as f64 * view.zoom;
                let b = bounds_at(ox + origin.0, oy + origin.1, w, chunk_px);
                if offscreen(b, bounds) {
                    continue;
                }
                span_bounds.push(b);
            }
        }
        span_bounds.sort_by_key(|b| {
            (b.origin.x.to_f64() as i64) * 4 + (b.origin.y.to_f64() as i64)
        });
        for b in &span_bounds {
            window.paint_quad(fill(*b, sel_fill));
            paint_outline(window, *b, 1.0, sel_border.into());
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
        let mode = if event.modifiers.control {
            SelectMode::Add
        } else if event.modifiers.alt {
            SelectMode::Subtract
        } else {
            SelectMode::Replace
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
        let factor = 1.2_f64.powf(lines.clamp(-6.0, 6.0));
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
    let sides = [
        // top
        Bounds::new(bounds.origin, size(bounds.size.width, w)),
        // bottom
        Bounds::new(
            gpui::point(bounds.origin.x, bounds.origin.y + bounds.size.height - w),
            size(bounds.size.width, w),
        ),
        // left
        Bounds::new(
            gpui::point(bounds.origin.x, bounds.origin.y + w),
            size(w, (bounds.size.height - w * 2.0).max(px(0.0))),
        ),
        // right
        Bounds::new(
            gpui::point(bounds.origin.x + bounds.size.width - w, bounds.origin.y + w),
            size(w, (bounds.size.height - w * 2.0).max(px(0.0))),
        ),
    ];
    for side in sides {
        window.paint_quad(fill(side, color));
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
                        Some(dim) => format!(
                            "{} · {} regions · {} tiles ({} pending)",
                            dim.name,
                            self.map.regions.len(),
                            self.map.tiles.len(),
                            self.map.pending.len()
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

/// Modal state for exporting the selection.
#[derive(Clone)]
pub struct ExportDialog {
    pub path: String,
    pub message: Option<String>,
}

impl Workspace {
    pub fn open_export_dialog(&mut self) {
        if self.map.selection.is_empty() {
            // Nothing selected: surface via dialog message anyway.
        }
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
