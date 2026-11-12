//! Main window view: TileMap canvas + status bar + dialogs.
//! Mirrors Window/TileMapBox/StatusBar/OptionBar behavior.
//! Shell only: input handling lives in input.rs, canvas painting in
//! paint.rs, dialogs in dialogs.rs, batch ops in ops.rs, world open in
//! world.rs, status bar in statusbar.rs, the height slider in slider.rs.

use crate::actions::*;
use crate::config::{self};
use crate::dialogs::Dialog;
use crate::input::Drag;
use crate::paint::CoordKey;
use crate::statusbar::StatusKey;
use crate::tilemap::*;
use gpui::*;
use mc_core::files;
use mc_core::point::{Point2f, Point2i};
use std::path::PathBuf;
use std::sync::Arc;

pub struct MainView {
    pub map: TileMapState,
    pub(crate) focus: FocusHandle,
    pub(crate) drag: Drag,
    pub(crate) previous: Point2f,
    pub(crate) first: Point2f,
    pub(crate) pressed_keys: u8,
    pub(crate) dialog: Option<Dialog>,
    /// index into crate::menus() of the open top-level dropdown, and of a
    /// submenu row inside it showing its flyout.
    pub(crate) open_menu: Option<usize>,
    pub(crate) open_submenu: Option<usize>,
    pub update_needed: bool,
    /// set alongside update_needed so map.update() runs at the next paint
    /// instead of the next 100ms poll; also coalesces bursts of
    /// scroll/zoom events into one O(visible-regions) update per frame.
    pub(crate) update_now: bool,
    pub(crate) height_slider_drag: bool,
    pub(crate) height_slider_bounds: std::rc::Rc<std::cell::RefCell<Option<Bounds<Pixels>>>>,
    pub(crate) icon_cache: std::collections::HashMap<String, Arc<RenderImage>>,
    /// reusable per-frame scratch: (x, z, key) of loaded tiles, kept sorted in
    /// raster order — avoids re-allocating the list every paint.
    pub(crate) visible_tiles_buf: Vec<(i32, i32, i64)>,
    /// shaped coordinate labels — shaping is the expensive part of the
    /// coordinate pass and the label set repeats across frames.
    pub(crate) coord_line_cache: std::collections::HashMap<CoordKey, ShapedLine>,
    /// last status-bar text + the inputs it was built from.
    pub(crate) status_cache: Option<(StatusKey, SharedString)>,
    /// paste-ghost images keyed by (offset.x, offset.z, color, dir); rebuilt
    /// only when the paste state changes, not once per region per frame.
    pub(crate) paste_overlay_cache:
        Option<(i32, i32, u32, PathBuf, std::collections::HashMap<i64, Arc<RenderImage>>)>,
    pub(crate) last_frame: std::time::Instant,
    /// held-key pan accumulator — upstream KeyActivator applies a fixed
    /// step every 33ms, so frame dt accumulates here between steps.
    pub(crate) pan_accum: std::time::Duration,
    /// shift held during held-key pan (10*scale instead of 5*scale)
    pub(crate) pan_shift: bool,
    /// status-bar busy-spinner frame
    pub(crate) spin: u8,
}

impl MainView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let mut corrupted = [0u32; 256];
        if let Ok(img) = image::load_from_memory(include_bytes!("../resources/img/corrupted.png")) {
            let rgba = img
                .resize_exact(16, 16, image::imageops::FilterType::Nearest)
                .to_rgba8();
            for (i, c) in rgba.chunks_exact(4).enumerate() {
                corrupted[i] = ((c[3] as u32) << 24) | ((c[0] as u32) << 16) | ((c[1] as u32) << 8) | c[2] as u32;
            }
        }
        set_corrupted_overlay(corrupted);

        let focus = cx.focus_handle();
        let mut v = MainView {
            map: TileMapState::new(),
            focus,
            drag: Drag::None,
            previous: Point2f::ZERO,
            first: Point2f::ZERO,
            pressed_keys: 0,
            dialog: None,
            open_menu: None,
            open_submenu: None,
            update_needed: true,
            update_now: true,
            height_slider_drag: false,
            height_slider_bounds: Default::default(),
            icon_cache: std::collections::HashMap::new(),
            visible_tiles_buf: Vec::new(),
            coord_line_cache: std::collections::HashMap::new(),
            status_cache: None,
            paste_overlay_cache: None,
            last_frame: std::time::Instant::now(),
            pan_accum: std::time::Duration::ZERO,
            pan_shift: false,
            spin: 0,
        };
        // upstream loads the persisted overlay config (OverlayConfig) globally;
        // restore serialized overlay lines saved by the overlay editor.
        let saved: Vec<mc_core::overlay::Overlay> = config::global()
            .overlays
            .iter()
            .filter_map(|l| mc_core::overlay::parse_config_line(l).ok())
            .collect();
        if !saved.is_empty() {
            v.map.set_overlays(saved);
        }
        cx.spawn(async move |this: WeakEntity<MainView>, cx: &mut AsyncApp| {
            loop {
                smol::Timer::after(std::time::Duration::from_millis(100)).await;
                if this.update(&mut *cx, |v, cx| v.poll_tick(cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
        v
    }

    /// mark the tile set dirty: the next paint runs map.update() so new
    /// tiles are requested frame-paced, and the 100ms poll still re-runs
    /// update() + evict_invisible() as before.
    ///
    /// The one map.update() contract: it runs at most once per paint
    /// (update_now) and once per 100ms poll (update_needed || results
    /// landed). Both flags are armed here; per-paint is the latency path,
    /// the poll is the steady-state sweep + tile eviction.
    pub(crate) fn request_update(&mut self) {
        self.update_needed = true;
        self.update_now = true;
    }

    pub(crate) fn poll_tick(&mut self, cx: &mut Context<Self>) {
        // batch-job completion messages + a finished filter selection
        // (TileResult::JobInfo / FilterResult, drained by poll_results into
        // the map's pending fields)
        let mut info = self.map.job_info.take();
        if let Some(sel) = self.map.filter_result.take() {
            let n = sel.count();
            self.map.selection = sel;
            self.map.selected_chunks = n as i64;
            self.map.unsaved_selection = true;
            self.map.selection_version += 1;
            if info.is_none() {
                info = Some(format!("filter matched {n} chunks"));
            }
        }
        if let Some(m) = info {
            self.dialog = Some(Dialog::Info { msg: m });
        }
        let changed = self.map.poll_results();
        if self.update_needed || changed {
            self.map.update();
            self.map.evict_invisible();
            self.update_needed = false;
        }
        let jobs = mc_core::jobs::active_jobs();
        self.spin = if jobs > 0 { (self.spin + 1) % 4 } else { 0 };
        let busy = mc_core::jobs::current_progress().is_some();
        if busy && self.dialog.is_none() {
            self.dialog = Some(Dialog::JobProgress);
        } else if !busy && matches!(self.dialog, Some(Dialog::JobProgress)) {
            self.dialog = None;
        }
        // `changed` covers tile results that landed after the job counter
        // already drained — without it the last batch waits for the next
        // unrelated repaint before it shows.
        if self.pressed_keys != 0 || self.drag.is_move() || busy || changed
            || matches!(self.dialog, Some(Dialog::JobProgress)) || mc_core::jobs::active_jobs() > 0
        {
            cx.notify();
        }
    }

    /// rebuild the OS menus so checkmark prefixes track toggle state.
    pub(crate) fn rebuild_menus(cx: &mut Context<Self>) {
        cx.set_menus(crate::menus(cx));
    }
}

impl Render for MainView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // read-guard: config::world() clones the whole WorldState per call.
        let show_bar = config::WORLD.read().unwrap().is_some();
        let status: SharedString = if show_bar {
            self.status_text()
        } else {
            "no world loaded — File > Open World".into()
        };
        self.focus.focus(window);
        let view = cx.entity();

        let map_el = canvas(
            move |_b, _w, _cx| view,
            move |bounds, view, window, cx| {
                let _ = view.update(cx, |v, app| {
                    v.paint_map(bounds, window, app);
                });
            },
        )
        .size_full();

        div()
            .id("root")
            .key_context("app")
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(0x0a0a0a))
            .on_action(cx.listener(|v, _: &QuitAction, _w, cx| {
                if v.map.unsaved_selection {
                    v.dialog = Some(Dialog::Confirm {
                        msg: "There is an unsaved selection. If you quit now, it will be lost.".into(),
                        action: Box::new(|_v, _w, cx| cx.quit()),
                    });
                    cx.notify();
                } else {
                    cx.quit();
                }
            }))
            .on_action(cx.listener(|v, _: &OpenWorldAction, w, cx| v.open_world_dialog(w, cx)))
            .on_action(cx.listener(|v, a: &OpenRecentWorld, _w, cx| {
                v.open_world_at(std::path::PathBuf::from(&a.path), cx);
            }))
            .on_action(cx.listener(|v, _: &OpenDimensionAction, w, cx| v.open_dimension_dialog(w, cx)))
            .on_action(cx.listener(|v, _: &SettingsAction, _w, cx| {
                v.dialog = Some(Dialog::Settings { draft: config::global() });
                cx.notify();
            }))
            .on_action(cx.listener(|v, _: &RenderSettingsAction, _w, cx| {
                if let Some(ws) = config::world() {
                    v.dialog = Some(Dialog::RenderSettings { draft: ws.cfg.clone() });
                }
                cx.notify();
            }))
            .on_action(cx.listener(|v, _: &ReloadAction, _w, cx| {
                if let Some(ws) = config::world() {
                    v.map.regions = files::scan_region_files(ws.region_dir());
                    // upstream reload() invalidates only visible regions
                    let (min, max) = v.map.visible_region_bounds(Point2f::ZERO);
                    for rx in min.x..=max.x {
                        for rz in min.z..=max.z {
                            v.map.invalidate_region(Point2i::new(rx, rz), false);
                        }
                    }
                    v.request_update();
                }
                cx.notify();
            }))
            .on_action(cx.listener(|v, _: &ChunkGridAction, _w, cx| {
                v.map.show_chunk_grid = !v.map.show_chunk_grid;
                let mut g = config::global();
                g.show_chunk_grid = v.map.show_chunk_grid;
                config::set_global(g);
                Self::rebuild_menus(cx);
                cx.notify();
            }))
            .on_action(cx.listener(|v, _: &RegionGridAction, _w, cx| {
                v.map.show_region_grid = !v.map.show_region_grid;
                let mut g = config::global();
                g.show_region_grid = v.map.show_region_grid;
                config::set_global(g);
                Self::rebuild_menus(cx);
                cx.notify();
            }))
            .on_action(cx.listener(|v, _: &CoordStyleDynamicAction, _w, cx| {
                v.map.coordinate_style = crate::tilemap::CoordinateStyle::Dynamic;
                let mut g = config::global();
                g.coordinate_style = "DYNAMIC".into();
                config::set_global(g);
                Self::rebuild_menus(cx);
                cx.notify();
            }))
            .on_action(cx.listener(|v, _: &CoordStyleEdgeAction, _w, cx| {
                v.map.coordinate_style = crate::tilemap::CoordinateStyle::Edge;
                let mut g = config::global();
                g.coordinate_style = "EDGE".into();
                config::set_global(g);
                Self::rebuild_menus(cx);
                cx.notify();
            }))
            .on_action(cx.listener(|v, _: &CoordStyleGridAction, _w, cx| {
                v.map.coordinate_style = crate::tilemap::CoordinateStyle::Grid;
                let mut g = config::global();
                g.coordinate_style = "GRID".into();
                config::set_global(g);
                Self::rebuild_menus(cx);
                cx.notify();
            }))
            .on_action(cx.listener(|v, _: &CoordinatesAction, _w, cx| {
                v.map.show_coordinates = !v.map.show_coordinates;
                let mut g = config::global();
                g.show_coordinates = v.map.show_coordinates;
                config::set_global(g);
                Self::rebuild_menus(cx);
                cx.notify();
            }))
            .on_action(cx.listener(|_v, a: &ToggleStructureIcon, _w, cx| {
                let mut g = config::global();
                let cur = g.structure_icons.get(&a.id).copied().unwrap_or(true);
                g.structure_icons.insert(a.id.clone(), !cur);
                config::set_global(g);
                Self::rebuild_menus(cx);
                cx.notify();
            }))
            .on_action(cx.listener(|v, _: &StructureIconsAction, _w, cx| {
                v.map.show_structure_icons = !v.map.show_structure_icons;
                let mut g = config::global();
                g.show_structure_icons = v.map.show_structure_icons;
                config::set_global(g);
                Self::rebuild_menus(cx);
                cx.notify();
            }))
            .on_action(cx.listener(|v, _: &GotoAction, _w, cx| {
                v.dialog = Some(Dialog::Goto { x: String::new(), z: String::new(), focus_x: true, invalid: false });
                cx.notify();
            }))
            .on_action(cx.listener(|v, _: &ResetZoomAction, _w, cx| {
                let (min, max) = v.map.visible_region_bounds(Point2f::ZERO);
                let center = min
                    .region_to_block()
                    .add_p(max.region_to_block())
                    .div(2)
                    .to_point2f()
                    .sub(v.map.offset)
                    .mul(1.0 / v.map.scale);
                let old = v.map.scale;
                v.map.scale = 1.0;
                v.map.update_scale(old, center);
                v.request_update();
                cx.notify();
            }))
            .on_action(cx.listener(|v, _: &SaveScreenshotAction, _w, cx| {
                v.save_screenshot(cx);
            }))
            .on_action(cx.listener(|v, _: &ClearViewCacheAction, _w, cx| {
                // upstream CacheHelper.clearViewCache: cached PNGs of the
                // VISIBLE regions only, not the whole world cache dir.
                let (min, max) = v.map.visible_region_bounds(Point2f::ZERO);
                for rx in min.x..=max.x {
                    for rz in min.z..=max.z {
                        v.map.invalidate_region(Point2i::new(rx, rz), true);
                    }
                }
                v.request_update();
                cx.notify();
            }))
            .on_action(cx.listener(|v, _: &ClearAllCacheAction, _w, cx| v.clear_all_caches(cx)))
            .on_action(cx.listener(|v, _: &ClearSelectionAction, _w, cx| {
                v.map.clear_selection();
                cx.notify();
            }))
            .on_action(cx.listener(|v, _: &InvertSelectionAction, _w, cx| {
                v.map.invert_selection();
                cx.notify();
            }))
            .on_action(cx.listener(|v, _: &InvertRegionsAction, _w, cx| {
                v.map.invert_regions_with_selection();
                cx.notify();
            }))
            .on_action(cx.listener(|v, _: &ImportSelectionAction, w, cx| v.import_selection(w, cx)))
            .on_action(cx.listener(|v, _: &ExportSelectionAction, w, cx| v.export_selection(w, cx)))
            .on_action(cx.listener(|v, _: &ExportImageAction, w, cx| v.export_image(w, cx)))
            .on_action(cx.listener(|v, _: &ClearSelectionCacheAction, _w, cx| {
                // upstream clearSelectionCache uses the TRUE selection —
                // an inverted selection covers every region file in the
                // world (region + poi + entities dirs).
                let sel = v.map.selection.clone();
                if let Some(ws) = config::world() {
                    for r in mc_core::batch::list_regions(&ws.dirs, Some(&sel)) {
                        v.map.invalidate_region(r, true);
                    }
                }
                v.request_update();
                cx.notify();
            }))
            .on_action(cx.listener(|v, _: &DeleteChunksAction, _w, cx| v.delete_chunks(cx)))
            .on_action(cx.listener(|v, _: &DefragmentAction, _w, cx| v.defragment(cx)))
            .on_action(cx.listener(|v, _: &ExportChunksAction, _w, cx| v.export_chunks(cx)))
            .on_action(cx.listener(|v, _: &CopyChunksAction, _w, cx| v.copy_chunks(cx)))
            .on_action(cx.listener(|v, _: &PasteChunksAction, w, cx| v.paste_chunks(w, cx)))
            .on_action(cx.listener(|v, _: &SumSelectionAction, _w, cx| v.sum_selection(cx)))
            .on_action(cx.listener(|v, _: &SwapChunksAction, _w, cx| v.swap_chunks(cx)))
            .on_action(cx.listener(|v, _: &FilterChunksAction, _w, cx| v.filter_chunks(cx)))
            .on_action(cx.listener(|v, _: &EditNBTAction, _w, cx| v.edit_nbt(cx)))
            .on_action(cx.listener(|v, _: &ChangeFieldsAction, _w, cx| v.change_fields(cx)))
            .on_action(cx.listener(|v, _: &ImportChunksAction, _w, cx| v.import_chunks(cx)))
            .on_action(cx.listener(|v, _: &EditOverlaysAction, _w, cx| v.edit_overlays(cx)))
            .on_action(cx.listener(|v, _: &NextOverlayAction, _w, cx| {
                v.map.next_overlay();
                v.request_update();
                cx.notify();
            }))
            .on_action(cx.listener(|v, _: &NextOverlayTypeAction, _w, cx| {
                v.map.next_overlay_type();
                v.request_update();
                cx.notify();
            }))
            .on_action(cx.listener(|v, _: &AboutAction, _w, cx| {
                v.dialog = Some(Dialog::About);
                cx.notify();
            }))
            .on_key_down(cx.listener(|v, e, w, cx| v.on_key_down(e, w, cx)))
            .on_key_up(cx.listener(|v, e, w, cx| v.on_key_up(e, w, cx)))
            .on_mouse_move(cx.listener(|v, e: &MouseMoveEvent, _w, cx| {
                if v.height_slider_drag {
                    let h = v.height_from_slider_x(f32::from(e.position.x));
                    v.set_render_height(h, false, cx);
                    return;
                }
                // gpui has no mouse-exit event: clear the hovered block when
                // the cursor leaves the canvas rect (over the slider, status
                // bar or window edge) so the status bar shows "-, -". Moves
                // leaving the window entirely are never delivered, so a
                // stale hover can persist until the next move event.
                let x = f32::from(e.position.x) - v.map.canvas_origin.x;
                let y = f32::from(e.position.y) - v.map.canvas_origin.z;
                let inside = x >= 0.0 && y >= 0.0 && x < v.map.width && y < v.map.height;
                if !inside && v.map.hovered_block.take().is_some() {
                    cx.notify();
                }
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(|v, _e: &MouseUpEvent, _w, _cx| {
                if v.height_slider_drag {
                    v.height_slider_drag = false;
                    if let Some(ws) = config::world() {
                        ws.cfg.save(&ws.cache_dir);
                    }
                }
            }))
            .child(self.render_menu_bar(cx))
            .child(
                div()
                    .flex_grow()
                    .relative()
                    .child(
                        div()
                            .size_full()
                            .on_mouse_down(MouseButton::Left, cx.listener(|v, e, w, cx| v.on_mouse_down(e, w, cx)))
                            .on_mouse_down(MouseButton::Right, cx.listener(|v, e, w, cx| v.on_mouse_down(e, w, cx)))
                            .on_mouse_down(MouseButton::Middle, cx.listener(|v, e, w, cx| v.on_mouse_down(e, w, cx)))
                            .on_mouse_up(MouseButton::Left, cx.listener(|v, e, w, cx| v.on_mouse_up(e, w, cx)))
                            .on_mouse_up(MouseButton::Right, cx.listener(|v, e, w, cx| v.on_mouse_up(e, w, cx)))
                            .on_mouse_up(MouseButton::Middle, cx.listener(|v, e, w, cx| v.on_mouse_up(e, w, cx)))
                            .on_mouse_move(cx.listener(|v, e, w, cx| v.on_mouse_move(e, w, cx)))
                            .on_scroll_wheel(cx.listener(|v, e, w, cx| v.on_scroll(e, w, cx)))
                            .child(map_el),
                    )
                    .children(self.render_menu_overlay(window, cx))
                    .children(self.render_dialog(window, cx)),
            )
            .children(if show_bar {
                vec![self.render_height_slider(cx)]
            } else {
                vec![]
            })
            .child(
                div()
                    .h(px(22.))
                    .w_full()
                    .bg(rgb(0x242424))
                    .flex()
                    .items_center()
                    .px_2()
                    .text_size(px(11.))
                    .text_color(rgb(0xbbbbbb))
                    .child(status),
            )
    }
}
