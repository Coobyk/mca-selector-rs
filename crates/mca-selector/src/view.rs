//! Main window view: TileMap canvas + status bar + dialogs.
//! Mirrors Window/TileMapBox/StatusBar/OptionBar behavior.

use crate::actions::*;
use crate::config::{self};
use crate::tilemap::*;
use gpui::*;
use mc_core::files;
use crate::nbt_editor;
use mc_core::point::{Point2f, Point2i};
use mc_core::selection::ChunkSet;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Clone, Copy, PartialEq, Debug)]
enum Drag {
    PasteMove,
    None,
    LeftSelect,
    RightSelect,
    MiddleMove,
}

impl Drag {
    /// upstream: primary marks, secondary unmarks; ctrl is not a modifier.
    fn mark(&self) -> Option<bool> {
        match self {
            Drag::LeftSelect => Some(true),
            Drag::RightSelect => Some(false),
            _ => None,
        }
    }
    fn is_move(&self) -> bool {
        matches!(self, Drag::MiddleMove)
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum NbtEdit {
    None,
    Value,
    Add,
    Rename,
}

/// upstream FilterChunksDialog.HandleType.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FilterAction {
    Select,
    Export,
    Delete,
}

/// upstream NBTEditorDialog: one state per mca kind (region/poi/entities).
pub struct NbtTab {
    pub root: Option<mc_core::nbt::Compound>,
    pub expanded: std::collections::HashSet<String>,
    pub sel: usize,
    pub dirty: bool,
    /// why there is nothing to edit ("region file does not exist" /
    /// "no chunk data found", matching upstream placeholders)
    pub note: Option<String>,
}

impl NbtTab {
    fn missing(note: &str) -> Self {
        NbtTab {
            root: None,
            expanded: std::collections::HashSet::new(),
            sel: 0,
            dirty: false,
            note: Some(note.to_string()),
        }
    }
}

/// one editable row of Dialog::OverlayEditor (upstream component.OverlayBox).
#[derive(Clone)]
pub struct OverlayRow {
    pub active: bool,
    pub typ: mc_core::overlay::OverlayType,
    pub min: String,
    pub max: String,
    pub data: String,
    pub min_hue: String,
    pub max_hue: String,
}

/// which text field of an OverlayRow currently holds keyboard focus.
#[derive(Clone, Copy, PartialEq)]
pub enum OverlayField {
    Min,
    Max,
    Data,
    MinHue,
    MaxHue,
}

pub enum Dialog {
    Goto { x: String, z: String, focus_x: bool, invalid: bool },
    About,
    Error { msg: String },
    Confirm { msg: String, action: Box<dyn FnOnce(&mut MainView, &mut Window, &mut Context<MainView>) + 'static> },
    SelectWorld { worlds: Vec<PathBuf> },
    SelectDimension { dims: Vec<PathBuf> },
    Settings { draft: config::GlobalConfig },
    RenderSettings { draft: config::WorldConfig },
    Info { msg: String },
    PasteConfirm { ox: i32, oz: i32, overwrite: bool, sel_only: bool },
    JobProgress,
    NbtEditor {
        region: i64,
        chunk_abs: Point2i,
        index: usize,
        /// 0 = region, 1 = poi, 2 = entities (upstream tab order)
        tab: usize,
        tabs: [NbtTab; 3],
        /// 0 = none, 1 = value edit, 2 = add child, 3 = rename
        editing: NbtEdit,
        buf: String,
        status: String,
    },
    TextInput {
        title: String,
        hint: String,
        text: String,
        on_done: Box<dyn Fn(&mut MainView, &mut Window, &mut Context<MainView>, String) + 'static>,
    },
    /// upstream FilterChunksDialog (options part — the GroupFilterBox is
    /// replaced by the raw query field, same as the old TextInput).
    Filter {
        query: String,
        action: FilterAction,
        apply: mc_core::filter_ops::ApplyType,
        radius: String,
        sel_only: bool,
        /// false = query field focused, true = radius field focused
        focus_radius: bool,
    },
    /// upstream ChangeNBTDialog.
    ChangeFields {
        text: String,
        force: bool,
        sel_only: bool,
    },
    /// upstream ImportConfirmationDialog (minus y-offset/section ranges).
    Import {
        dir: PathBuf,
        ox: String,
        oz: String,
        overwrite: bool,
        sel_only: bool,
        focus_x: bool,
        invalid: bool,
    },
    /// upstream OverlayEditorDialog (modal apply-on-OK instead of live).
    OverlayEditor {
        rows: Vec<OverlayRow>,
        focus: Option<(usize, OverlayField)>,
    },
}

/// one-shot Info dialog payload published by finished batch jobs (filtered
/// delete/export, change fields, import) — consumed by poll_tick.
static JOB_INFO: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// title line for the sum-selection Info dialog (upstream NumberDialog shows
/// the overlay type + its multi-values); stashed when the job starts because
/// the overlay may change before the result lands.
static SUM_TITLE: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

fn job_info(msg: impl Into<String>) {
    *JOB_INFO.lock().unwrap() = Some(msg.into());
}

/// cache key for shaped coordinate labels: edge labels hold a single axis
/// value, grid labels an (x, z) block-coord pair. Shaped lines are
/// position-independent, so one entry serves every frame.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum CoordKey {
    Edge(i64),
    /// first line of a two-line "x,\nz" grid label
    Grid(i64, i64),
    /// second line of a two-line "x,\nz" grid label
    GridB(i64, i64),
}

/// every input that can change the status-bar text; the formatted string is
/// rebuilt only when this differs from the cached key (see status_text).
#[derive(PartialEq)]
struct StatusKey {
    total_tiles: usize,
    hovered: Option<Point2i>,
    selected_chunks: i64,
    inverted: bool,
    unsaved_selection: bool,
    /// (names[0], len > 1) — only when structure icons are on and the
    /// hovered chunk has entries
    structure: Option<(String, bool)>,
    /// (typ, min, max, hovered value)
    overlay: Option<(mc_core::overlay::OverlayType, Option<i32>, Option<i32>, Option<i32>)>,
    jobs: usize,
    /// busy-spinner frame, cycles 0..4 while jobs are queued
    spin: u8,
}

pub struct MainView {
    pub map: TileMapState,
    focus: FocusHandle,
    drag: Drag,
    previous: Point2f,
    first: Point2f,
    pressed_keys: u8,
    dialog: Option<Dialog>,
    pub update_needed: bool,
    /// set alongside update_needed so map.update() runs at the next paint
    /// instead of the next 100ms poll; also coalesces bursts of
    /// scroll/zoom events into one O(visible-regions) update per frame.
    update_now: bool,
    height_slider_drag: bool,
    height_slider_bounds: std::rc::Rc<std::cell::RefCell<Option<Bounds<Pixels>>>>,
    icon_cache: std::collections::HashMap<String, Arc<RenderImage>>,
    /// reusable per-frame scratch: (x, z, key) of loaded tiles, kept sorted in
    /// raster order — avoids re-allocating the list every paint.
    visible_tiles_buf: Vec<(i32, i32, i64)>,
    /// shaped coordinate labels — shaping is the expensive part of the
    /// coordinate pass and the label set repeats across frames.
    coord_line_cache: std::collections::HashMap<CoordKey, ShapedLine>,
    /// last status-bar text + the inputs it was built from.
    status_cache: Option<(StatusKey, SharedString)>,
    /// paste-ghost images keyed by (offset.x, offset.z, color, dir); rebuilt
    /// only when the paste state changes, not once per region per frame.
    paste_overlay_cache:
        Option<(i32, i32, u32, PathBuf, std::collections::HashMap<i64, Arc<RenderImage>>)>,
    last_frame: std::time::Instant,
    /// held-key pan accumulator — upstream KeyActivator applies a fixed
    /// step every 33ms, so frame dt accumulates here between steps.
    pan_accum: std::time::Duration,
    /// shift held during held-key pan (10*scale instead of 5*scale)
    pan_shift: bool,
    /// status-bar busy-spinner frame
    spin: u8,
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
    fn request_update(&mut self) {
        self.update_needed = true;
        self.update_now = true;
    }

    fn poll_tick(&mut self, cx: &mut Context<Self>) {
        if let Some(sum) = self.map.take_sum_result() {
            // upstream NumberDialog: "<overlay type>(<multi values>) — <sum>"
            let title = SUM_TITLE.lock().unwrap().take().unwrap_or_else(|| "sum".into());
            self.dialog = Some(Dialog::Info { msg: format!("{title}: {sum}") });
        }
        if let Some(sel) = self.map.take_filter_result() {
            let n = sel.count();
            self.map.selection = sel;
            self.map.selected_chunks = n as i64;
            self.map.unsaved_selection = true;
            self.map.selection_version += 1;
            if JOB_INFO.lock().unwrap().is_none() {
                self.dialog = Some(Dialog::Info { msg: format!("filter matched {n} chunks") });
            }
        }
        // batch-job completion messages win over the generic filter text
        if let Some(m) = JOB_INFO.lock().unwrap().take() {
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

    // -- world opening --------------------------------------------------------

    pub fn open_world_dialog(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Select a world directory".into()),
        });
        cx.spawn(async move |this: WeakEntity<MainView>, cx: &mut AsyncApp| {
            if let Ok(Ok(Some(paths))) = rx.await {
                if let Some(p) = paths.into_iter().next() {
                    let _ = this.update(&mut *cx, |v, cx| v.open_world_at(p, cx));
                }
            }
        })
        .detach();
    }

    pub fn open_recent_dialog(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let worlds: Vec<PathBuf> = config::global()
            .recent_worlds
            .values()
            .rev()
            .map(|r| r.world.clone())
            .collect();
        if worlds.is_empty() {
            self.open_world_dialog(_window, cx);
            return;
        }
        self.dialog = Some(Dialog::SelectWorld { worlds });
        cx.notify();
    }

    pub fn open_world_at(&mut self, dir: PathBuf, cx: &mut Context<Self>) {
        // upstream: confirm before discarding an unsaved selection
        if self.map.unsaved_selection {
            let d = dir.clone();
            self.dialog = Some(Dialog::Confirm {
                msg: "There is an unsaved selection. Open the world anyway?".into(),
                action: Box::new(move |v, _w, cx| {
                    v.map.unsaved_selection = false;
                    v.open_world_at(d.clone(), cx);
                }),
            });
            cx.notify();
            return;
        }
        // upstream showWorldDialog: if not a valid dimension itself, scan it;
        // if it contains multiple dims, open dimension dialog; else open.
        if files::is_valid_dimension(&dir) {
            let dims = files::detect_dimension_directories(&dir);
            let mut ordered = vec![dir.clone()];
            ordered.extend(dims.iter().filter(|d| **d != dir).cloned());
            self.do_open(dir.clone(), ordered, cx);
            return;
        }
        let dims = files::detect_dimension_directories(&dir);
        if dims.len() > 1 {
            self.dialog = Some(Dialog::SelectDimension { dims });
            cx.notify();
        } else if let Some(d) = dims.into_iter().next() {
            let mut ordered = vec![d];
            let dirs = files::detect_world_directories(&dir);
            let _ = dirs;
            self.do_open_at_dir(dir.clone(), cx);
        } else {
            self.dialog = Some(Dialog::Error {
                msg: format!("no minecraft installation found in\n{}", dir.display()),
            });
            cx.notify();
        }
    }

    fn do_open(&mut self, root: PathBuf, dims: Vec<PathBuf>, cx: &mut Context<Self>) {
        let dirs = files::detect_world_directories(&root);
        if dirs.region.is_none() {
            self.dialog = Some(Dialog::Error {
                msg: format!("Could not find region folder in {}", root.display()),
            });
            cx.notify();
            return;
        }
        self.map.clear();
        mc_core::jobs::pools().clear_queues();
        let ws = config::open_world(dirs, dims.clone());
        self.map.regions = files::scan_region_files(ws.region_dir());
        self.map.disabled = false;
        self.map.clear_selection();
        self.map.unsaved_selection = false;
        self.request_update();
        let mut g = config::global();
        g.add_recent_world(root, dims);
        config::set_global(g);
        cx.notify();
    }

    fn do_open_at_dir(&mut self, dir: PathBuf, cx: &mut Context<Self>) {
        let dims = files::detect_dimension_directories(&dir);
        let mut ordered = vec![dir.clone()];
        ordered.extend(dims.iter().filter(|d| **d != dir).cloned());
        self.do_open(dir.clone(), ordered, cx);
    }

    pub fn open_dimension_dialog(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ws) = config::world() {
            if !ws.dimensions.is_empty() {
                self.dialog = Some(Dialog::SelectDimension { dims: ws.dimensions.clone() });
                cx.notify();
            }
        }
    }

    fn clear_all_caches(&mut self, cx: &mut Context<Self>) {
        if let Some(ws) = config::world() {
            let dir = ws.cache_dir.clone();
            let _ = std::fs::remove_dir_all(&dir);
            mc_core::cache::delete_all(&ws.uuid);
        }
        self.map.pool.clear();
        self.map.pool_order.clear();
        self.map.mark_all_tiles_obsolete();
        self.map.loading.clear();
        self.request_update();
        cx.notify();
    }

    // -- input ------------------------------------------------------------------

    fn on_scroll(&mut self, event: &ScrollWheelEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if self.map.disabled || self.dialog.is_some() {
            return;
        }
        // upstream: dy>0 (up) -> scale /= 1.03+dy/1000; dy<0 -> scale /= 0.97+dy/1000
        // (JavaFX deltaY ~40/notch; gpui lines are ~1/notch -> scale them up)
        let dy = match event.delta {
            ScrollDelta::Pixels(p) => f32::from(p.y),
            ScrollDelta::Lines(l) => l.y * 40.0,
        };
        if dy != 0.0 {
            let factor = if dy > 0.0 { 1.03 + dy / 1000.0 } else { 0.97 + dy / 1000.0 };
            let old = self.map.scale;
            self.map.scale /= factor;
            let pos = Point2f::new(f32::from(event.position.x), f32::from(event.position.y));
            if self.map.update_scale(old, pos) {
                // request fresh-resolution tiles at the next paint — a wheel
                // burst then costs one O(visible) update per frame instead
                // of one per event.
                self.request_update();
            }
            // update_scale only mutates the view when the scale actually
            // changed; a scroll fully eaten by the min/max clamp repaints
            // an identical frame, so skip it.
            if self.map.scale != old {
                cx.notify();
            }
        }
    }

    fn on_mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.map.disabled || self.dialog.is_some() {
            return;
        }
        self.focus.focus(window);
        let pos = Point2f::new(f32::from(event.position.x), f32::from(event.position.y));
        let shift = event.modifiers.shift;
        let meta = event.modifiers.platform;
        match event.button {
            MouseButton::Left => {
                // upstream: PRIMARY+COMMAND pans; ctrl is not a modifier
                if meta && self.map.pasted.is_none() {
                    self.drag = Drag::MiddleMove;
                    self.first = pos;
                    self.previous = pos;
                    cx.notify();
                    return;
                }
                if self.map.pasted.is_some() {
                    self.drag = Drag::PasteMove;
                    if let Some(p) = &mut self.map.pasted {
                        p.first_mouse = Some(pos);
                        p.first_offset = p.offset;
                    }
                    self.first = pos;
                    self.previous = pos;
                    cx.notify();
                    return;
                }
                self.drag = Drag::LeftSelect;
                if let Some(mark) = self.drag.mark() {
                    if self.map.scale <= CHUNK_GRID_SCALE {
                        let chunk = self.map.mouse_chunk(pos.x, pos.z);
                        if self.map.selection.is_chunk_selected(chunk) != mark {
                            if mark {
                                self.map.selection.add_chunk(chunk);
                                self.map.selected_chunks += if self.map.selection.is_inverted() { -1 } else { 1 };
                            } else {
                                self.map.selection.remove_chunk(chunk);
                                self.map.selected_chunks += if self.map.selection.is_inverted() { 1 } else { -1 };
                            }
                            let _ = chunk.chunk_to_region();
                        }
                    } else {
                        let region = self.map.mouse_region(pos.x, pos.z);
                        let d = if mark {
                            self.map.selection.add_region(region.as_long()) as i64
                        } else {
                            self.map.selection.remove_region(region.as_long()) as i64
                        };
                        self.map.selected_chunks += if self.map.selection.is_inverted() {
                            if mark { -d } else { d }
                        } else if mark {
                            d
                        } else {
                            -d
                        };
                    }
                    self.map.unsaved_selection = true;
                    self.map.selection_version += 1;
                }
            }
            MouseButton::Right => {
                // upstream: a right-press unmarks immediately — including
                // while pasting (paste is cancelled by ESC, not right-click);
                // holding the button continues as a right-drag unmark.
                self.drag = Drag::RightSelect;
                self.map.mark(pos.x, pos.z, false, pos, false);
                self.map.selection_version += 1;
            }
            MouseButton::Middle => self.drag = Drag::MiddleMove,
            _ => {}
        }
        self.first = pos;
        self.previous = pos;
        cx.notify();
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if self.height_slider_drag {
            let h = self.height_from_slider_x(f32::from(event.position.x));
            self.set_render_height(h, false, cx);
            return;
        }
        let pos = Point2f::new(f32::from(event.position.x), f32::from(event.position.y));
        // repaint only when something visible actually changed: mouse moves
        // that keep the same hovered block / paste offset / selection used
        // to repaint the full scene for every event.
        let hovered = self.map.mouse_block(pos.x, pos.z);
        let mut dirty = self.map.hovered_block != Some(hovered);
        self.map.hovered_block = Some(hovered);
        if self.map.disabled || self.dialog.is_some() {
            self.previous = pos;
            if dirty {
                cx.notify();
            }
            return;
        }
        let pressed = event.pressed_button;
        if self.drag == Drag::PasteMove {
            // the ghost is quantized to whole chunks — most sub-chunk moves
            // leave the pasted offset (and the frame) unchanged.
            let off = self.map.pasted.as_ref().map(|p| p.offset);
            self.map.drag_paste_offset(pos);
            dirty |= off != self.map.pasted.as_ref().map(|p| p.offset);
            self.previous = pos;
            if dirty {
                cx.notify();
            }
            return;
        }
        if self.drag.is_move() || pressed == Some(MouseButton::Middle) {
            let dx = (self.previous.x - pos.x) * self.map.scale;
            let dz = (self.previous.z - pos.z) * self.map.scale;
            if dx != 0.0 || dz != 0.0 {
                self.map.offset = self.map.offset.add(dx, dz);
                dirty = true;
            }
        } else if pressed.is_some() && self.drag != Drag::None {
            if let Some(mark) = self.drag.mark() {
                // upstream: paintMode = SHIFT held (other keys ignored)
                let paint_mode = event.modifiers.shift;
                // mark() only touches regions in the drag rect — hash their
                // effective selection before/after and repaint only when the
                // painted overlay would differ (selected_chunks alone can't
                // see no-op drags over already-marked territory, or the rare
                // Some(full-set) -> whole-region color swap).
                let (a, b) = if self.map.scale > CHUNK_GRID_SCALE {
                    let mouse = self.map.mouse_region(pos.x, pos.z);
                    let first = if paint_mode {
                        mouse
                    } else {
                        self.map.mouse_region(self.first.x, self.first.z)
                    };
                    (mouse, first)
                } else {
                    let mouse = self.map.mouse_chunk(pos.x, pos.z).chunk_to_region();
                    let first = if paint_mode {
                        mouse
                    } else {
                        self.map.mouse_chunk(self.first.x, self.first.z).chunk_to_region()
                    };
                    (mouse, first)
                };
                let (rx0, rx1) = (a.x.min(b.x), a.x.max(b.x));
                let (rz0, rz1) = (a.z.min(b.z), a.z.max(b.z));
                let rect_area = (rx1 - rx0 + 1) as i64 * (rz1 - rz0 + 1) as i64;
                if rect_area > 64 {
                    // hashing every affected region costs more than the
                    // repaint it would save — mark() on a rect this big is
                    // already the dominant cost.
                    self.map.mark(pos.x, pos.z, mark, self.first, paint_mode);
                    self.map.selection_version += 1;
                    dirty = true;
                } else {
                    // FNV-1a over what the selection overlay paints per region:
                    // region_selected flag + effective chunk bitmask.
                    let sel_hash = |sel: &mc_core::selection::Selection| -> u64 {
                        let mut h = 0xcbf29ce484222325u64;
                        for x in rx0..=rx1 {
                            for z in rz0..=rz1 {
                                let p = Point2i::new(x, z);
                                let l = p.as_long();
                                h = (h ^ l as u64).wrapping_mul(0x100000001b3);
                                h = (h ^ sel.is_region_selected(l) as u64).wrapping_mul(0x100000001b3);
                                match sel.selected_chunks(p) {
                                    None => h = (h ^ 0xff).wrapping_mul(0x100000001b3),
                                    Some(None) => h = (h ^ 0xaa).wrapping_mul(0x100000001b3),
                                    Some(Some(cs)) => {
                                        for i in cs.iter() {
                                            h = (h ^ i as u64).wrapping_mul(0x100000001b3);
                                        }
                                        h = (h ^ 0xbb).wrapping_mul(0x100000001b3);
                                    }
                                }
                            }
                        }
                        h
                    };
                    let sel_state = (
                        sel_hash(&self.map.selection),
                        self.map.selected_chunks,
                        self.map.unsaved_selection,
                    );
                    self.map.mark(pos.x, pos.z, mark, self.first, paint_mode);
                    self.map.selection_version += 1;
                    dirty |= sel_state
                        != (
                            sel_hash(&self.map.selection),
                            self.map.selected_chunks,
                            self.map.unsaved_selection,
                        );
                }
            }
        }
        self.previous = pos;
        if dirty {
            cx.notify();
        }
    }

    fn on_mouse_up(&mut self, _event: &MouseUpEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if self.height_slider_drag {
            self.height_slider_drag = false;
            if let Some(ws) = config::world() {
                ws.cfg.save(&ws.cache_dir);
            }
        }
        if let Some(p) = &mut self.map.pasted {
            p.first_mouse = None;
        }
        self.drag = Drag::None;
        self.request_update();
        cx.notify();
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        // held-key pan speed modifier (upstream: SHIFT -> 10*scale)
        self.pan_shift = event.keystroke.modifiers.shift;
        if self.dialog.is_some() {
            self.dialog_key(event, _window, cx);
            return;
        }
        let k = event.keystroke.key.as_str();
        let before = self.pressed_keys;
        match k {
            "w" | "up" => self.pressed_keys |= 1,
            "a" | "left" => self.pressed_keys |= 2,
            "s" | "down" => self.pressed_keys |= 4,
            "d" | "right" => self.pressed_keys |= 8,

            // zoom_key notifies internally when it changes the view
            "+" | "=" | "add" => { self.zoom_key(1.05, cx); return; }
            "-" | "subtract" => { self.zoom_key(0.95, cx); return; }
            "escape" => {
                // upstream: ESC clears the paste state
                if self.map.pasted.is_some() {
                    self.map.cancel_paste();
                    cx.notify();
                }
            }
            _ => {}
        }
        // held-key repeats and untracked keys change nothing on screen
        if self.pressed_keys != before {
            cx.notify();
        }
    }

    fn on_key_up(&mut self, event: &KeyUpEvent, _window: &mut Window, cx: &mut Context<Self>) {
        self.pan_shift = event.keystroke.modifiers.shift;
        let before = self.pressed_keys;
        match event.keystroke.key.as_str() {
            "w" | "up" => self.pressed_keys &= !1,
            "a" | "left" => self.pressed_keys &= !2,
            "s" | "down" => self.pressed_keys &= !4,
            "d" | "right" => self.pressed_keys &= !8,

            _ => {}
        }
        if self.pressed_keys != before {
            cx.notify();
        }
    }

    fn dialog_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let k = event.keystroke.key.as_str();
        if let Some(Dialog::Goto { x, z, focus_x, invalid }) = &mut self.dialog {
            match k {
                "enter" => {
                    // upstream disables OK on invalid input; we keep the dialog
                    // open and flag it instead.
                    if let (Ok(xi), Ok(zi)) = (x.parse::<i32>(), z.parse::<i32>()) {
                        self.map.go_to(xi, zi);
                        self.request_update();
                        self.dialog = None;
                    } else {
                        *invalid = true;
                    }
                }
                "escape" => self.dialog = None,
                "tab" => *focus_x = !*focus_x,
                "backspace" => {
                    let f = if *focus_x { x } else { z };
                    f.pop();
                    *invalid = false;
                }
                "space" => *focus_x = !*focus_x,
                _ => {
                    if let Some(c) = k.chars().next() {
                        if k.len() == 1 && (c.is_ascii_digit() || c == '-') {
                            let f = if *focus_x { x } else { z };
                            f.push(c);
                            *invalid = false;
                        }
                    }
                }
            }
            cx.notify();
            return;
        }
        if let Some(Dialog::PasteConfirm { overwrite, sel_only, .. }) = &mut self.dialog {
            match k {
                "enter" => {
                    if let Some(Dialog::PasteConfirm { ox, oz, overwrite, sel_only, .. }) = self.dialog.take() {
                        self.do_paste_import(ox, oz, overwrite, sel_only, cx);
                    }
                    cx.notify();
                    return;
                }
                "escape" => { self.dialog = None; cx.notify(); return; }
                "o" => { *overwrite = !*overwrite; cx.notify(); return; }
                "s" => { *sel_only = !*sel_only; cx.notify(); return; }
                _ => {}
            }
        }
        if let Some(Dialog::Filter { query, radius, focus_radius, .. }) = &mut self.dialog {
            match k {
                "escape" => self.dialog = None,
                "tab" => *focus_radius = !*focus_radius,
                "backspace" => {
                    if *focus_radius { radius.pop(); } else { query.pop(); }
                }
                "enter" => {
                    // upstream disables OK on an invalid filter — ours stays
                    // open (the red hint line shows the parse error)
                    let valid = !query.trim().is_empty() && mc_core::filter::parse(query).is_ok();
                    if valid {
                        if let Some(Dialog::Filter { query, action, apply, radius, sel_only, .. }) = self.dialog.take() {
                            self.run_filter_dialog(query, action, apply, radius, sel_only, window, cx);
                        }
                    }
                    cx.notify();
                    return;
                }
                _ => {
                    if *focus_radius {
                        // upstream validates digits-only on the radius field
                        if k.len() == 1 && k.as_bytes()[0].is_ascii_digit() {
                            radius.push_str(k);
                        }
                    } else {
                        let c = &event.keystroke.key_char;
                        if let Some(c) = c {
                            for ch in c.chars() { query.push(ch); }
                        } else if k.len() == 1 {
                            query.push_str(k);
                        } else if k == "space" {
                            query.push(' ');
                        }
                    }
                }
            }
            cx.notify();
            return;
        }
        if let Some(Dialog::ChangeFields { text, .. }) = &mut self.dialog {
            match k {
                "escape" => self.dialog = None,
                "backspace" => { text.pop(); }
                "enter" => {
                    // upstream disables OK on invalid input — stay open
                    let valid = matches!(mc_core::changer::parse_fields(text), Ok(f) if !f.is_empty());
                    if valid {
                        if let Some(Dialog::ChangeFields { text, force, sel_only, .. }) = self.dialog.take() {
                            self.run_change_fields_dialog(text, force, sel_only, cx);
                        }
                    }
                    cx.notify();
                    return;
                }
                _ => {
                    let c = &event.keystroke.key_char;
                    if let Some(c) = c {
                        for ch in c.chars() { text.push(ch); }
                    } else if k.len() == 1 {
                        text.push_str(k);
                    } else if k == "space" {
                        text.push(' ');
                    }
                }
            }
            cx.notify();
            return;
        }
        if let Some(Dialog::Import { ox, oz, focus_x, overwrite, sel_only, invalid, .. }) = &mut self.dialog {
            match k {
                "escape" => self.dialog = None,
                "tab" => *focus_x = !*focus_x,
                "backspace" => {
                    let f = if *focus_x { ox } else { oz };
                    f.pop();
                    *invalid = false;
                }
                "o" => { *overwrite = !*overwrite; }
                "s" => { *sel_only = !*sel_only; }
                "enter" => {
                    let ok = ox.trim().parse::<i32>().is_ok() && oz.trim().parse::<i32>().is_ok();
                    if ok {
                        if let Some(Dialog::Import { dir, ox, oz, overwrite, sel_only, .. }) = self.dialog.take() {
                            self.do_import(dir, ox.trim().parse().unwrap_or(0), oz.trim().parse().unwrap_or(0), overwrite, sel_only, cx);
                        }
                    } else {
                        *invalid = true;
                    }
                }
                _ => {
                    if let Some(c) = k.chars().next() {
                        if k.len() == 1 && (c.is_ascii_digit() || c == '-') {
                            let f = if *focus_x { ox } else { oz };
                            f.push(c);
                            *invalid = false;
                        }
                    }
                }
            }
            cx.notify();
            return;
        }
        if let Some(Dialog::OverlayEditor { rows, focus }) = &mut self.dialog {
            match k {
                "escape" => {
                    if focus.is_some() {
                        *focus = None;
                    } else {
                        self.dialog = None;
                    }
                }
                "enter" => {
                    if focus.is_some() {
                        *focus = None;
                    } else if let Some(Dialog::OverlayEditor { rows, .. }) = self.dialog.take() {
                        self.apply_overlay_editor(rows, cx);
                    }
                    cx.notify();
                    return;
                }
                "tab" => {
                    const FIELDS: [OverlayField; 5] = [
                        OverlayField::Min, OverlayField::Max, OverlayField::Data,
                        OverlayField::MinHue, OverlayField::MaxHue,
                    ];
                    *focus = match *focus {
                        None => Some((0, OverlayField::Min)),
                        Some((r, f)) => {
                            let fi = FIELDS.iter().position(|x| *x == f).unwrap_or(0) + 1;
                            if fi < FIELDS.len() {
                                Some((r, FIELDS[fi]))
                            } else if r + 1 < rows.len() {
                                Some((r + 1, OverlayField::Min))
                            } else {
                                Some((0, OverlayField::Min))
                            }
                        }
                    };
                }
                "backspace" => {
                    if let Some((r, f)) = *focus {
                        if let Some(row) = rows.get_mut(r) {
                            overlay_field(row, f).pop();
                        }
                    }
                }
                "a" if focus.is_none() => {
                    rows.push(default_overlay_row());
                }
                _ => {
                    if let Some((r, f)) = *focus {
                        let c = &event.keystroke.key_char;
                        let push = |t: &mut String| {
                            if let Some(c) = c {
                                for ch in c.chars() { t.push(ch); }
                            } else if k.len() == 1 {
                                t.push_str(k);
                            } else if k == "space" {
                                t.push(' ');
                            }
                        };
                        if let Some(row) = rows.get_mut(r) {
                            push(overlay_field(row, f));
                        }
                    }
                }
            }
            cx.notify();
            return;
        }
        if let Some(Dialog::TextInput { text, .. }) = &mut self.dialog {
            match k {
                "backspace" => { text.pop(); }
                "escape" => self.dialog = None,
                "enter" => {
                    if let Some(Dialog::TextInput { text, on_done, .. }) = self.dialog.take() {
                        (on_done)(self, window, cx, text);
                    }
                    cx.notify();
                    return;
                }
                _ => {
                    let c = &event.keystroke.key_char;
                    if let Some(c) = c {
                        for ch in c.chars() { text.push(ch); }
                    } else if k.len() == 1 {
                        text.push_str(k);
                    } else if k == "space" {
                        text.push(' ');
                    }
                }
            }
            cx.notify();
            return;
        }
        if matches!(self.dialog, Some(Dialog::NbtEditor { .. })) {
            self.nbt_editor_key(event, cx);
            cx.notify();
            return;
        }
        // upstream confirmation dialogs: the default button fires on Enter
        if let Some(Dialog::Confirm { .. }) = &self.dialog {
            match k {
                "enter" => {
                    if let Some(Dialog::Confirm { action, .. }) = self.dialog.take() {
                        (action)(self, window, cx);
                    }
                }
                "escape" => self.dialog = None,
                _ => {}
            }
            cx.notify();
            return;
        }
        // Info/Error/About all have a single default OK/Close button upstream
        if matches!(self.dialog, Some(Dialog::Info { .. }) | Some(Dialog::Error { .. }) | Some(Dialog::About)) {
            if k == "enter" || k == "escape" {
                self.dialog = None;
            }
            cx.notify();
            return;
        }
        if k == "escape" {
            if self.map.pasted.is_some() {
                self.map.cancel_paste();
            }
            self.dialog = None;
        }
        cx.notify();
    }

    /// keyboard handling for the NBT tree editor (NBTTreeView edit mode).
    /// "1"/"2"/"3" switch the region/poi/entities tab (upstream NBTEditorDialog
    /// tab pane); everything else acts on the current tab.
    fn nbt_editor_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let k = event.keystroke.key.as_str();
        let ctrl = event.keystroke.modifiers.control;
        let Some(Dialog::NbtEditor {
            tab, tabs, editing, buf, status, region, chunk_abs, index,
        }) = &mut self.dialog
        else {
            return;
        };
        let _ = (region, chunk_abs, index);
        if *editing != NbtEdit::None {
            let t = &mut tabs[*tab];
            match k {
                "enter" => {
                    let mode = *editing;
                    let res = match t.root.as_mut() {
                        Some(root) => {
                            let path = nbt_editor::flatten(root, &t.expanded)
                                .get(t.sel)
                                .map(|r| r.path.clone())
                                .unwrap_or_default();
                            match mode {
                                NbtEdit::Value => nbt_editor::apply_value_edit(root, &path, buf),
                                NbtEdit::Add => {
                                    let rows = nbt_editor::flatten(root, &t.expanded);
                                    let container_path: String = rows
                                        .get(t.sel)
                                        .map(|r| {
                                            if r.container {
                                                r.path.clone()
                                            } else {
                                                r.path
                                                    .rsplit_once('.')
                                                    .map(|(p, _)| p.to_string())
                                                    .or_else(|| {
                                                        r.path.rfind('[').map(|i| r.path[..i].to_string())
                                                    })
                                                    .unwrap_or_default()
                                            }
                                        })
                                        .unwrap_or_default();
                                    nbt_editor::apply_add(root, &container_path, buf)
                                }
                                NbtEdit::Rename => nbt_editor::apply_rename(root, &path, buf),
                                NbtEdit::None => Ok(()),
                            }
                        }
                        None => Ok(()),
                    };
                    match res {
                        Ok(()) => {
                            t.dirty = true;
                            *status = "modified — ctrl-s to save".into();
                        }
                        Err(e) => *status = format!("error: {e}"),
                    }
                    *editing = NbtEdit::None;
                    buf.clear();
                }
                "escape" => {
                    *editing = NbtEdit::None;
                    buf.clear();
                }
                "backspace" => {
                    buf.pop();
                }
                _ => {
                    if let Some(c) = &event.keystroke.key_char {
                        buf.push_str(c);
                    } else if k == "space" {
                        buf.push(' ');
                    } else if k.len() == 1 {
                        buf.push_str(k);
                    }
                }
            }
            return;
        }
        match k {
            "1" | "2" | "3" => {
                let n = (k.as_bytes()[0] - b'1') as usize;
                if n != *tab {
                    *tab = n;
                    *editing = NbtEdit::None;
                    buf.clear();
                }
                return;
            }
            "escape" => {
                self.dialog = None;
                return;
            }
            _ => {}
        }
        let t = &mut tabs[*tab];
        let Some(root) = t.root.as_mut() else {
            // no data on this tab — only tab switching/close keys apply
            return;
        };
        let sel = &mut t.sel;
        let expanded = &mut t.expanded;
        let dirty = &mut t.dirty;
        let rows = nbt_editor::flatten(root, expanded);
        match k {
            "up" => *sel = sel.saturating_sub(1),
            "down" => *sel = (*sel + 1).min(rows.len().saturating_sub(1)),
            "pageup" => *sel = sel.saturating_sub(20),
            "pagedown" => *sel = (*sel + 20).min(rows.len().saturating_sub(1)),
            "right" => {
                if let Some(r) = rows.get(*sel) {
                    if r.container {
                        expanded.insert(r.path.clone());
                    }
                }
            }
            "left" => {
                if let Some(r) = rows.get(*sel) {
                    if r.container && r.expanded {
                        expanded.remove(&r.path);
                    } else if let Some(pos) = r.path.rfind(|c| c == '.' || c == '[') {
                        let parent = r.path[..pos].to_string();
                        if let Some(pi) = rows.iter().position(|x| x.path == parent) {
                            *sel = pi;
                        }
                    }
                }
            }
            "enter" | "e" => {
                if let Some(r) = rows.get(*sel) {
                    if r.container {
                        if r.expanded {
                            expanded.remove(&r.path);
                        } else {
                            expanded.insert(r.path.clone());
                        }
                    } else {
                        *editing = NbtEdit::Value;
                        let cur = mc_core::nbt::path_get(root, &r.path)
                            .map(nbt_editor::tag_edit_text)
                            .unwrap_or_default();
                        *buf = cur;
                    }
                }
            }
            "a" => {
                *editing = NbtEdit::Add;
                buf.clear();
                *status = "name = snbt_value".into();
            }
            "r" => {
                if let Some(r) = rows.get(*sel) {
                    if !r.name.starts_with('[') {
                        *editing = NbtEdit::Rename;
                        *buf = r.name.clone();
                    }
                }
            }
            "delete" => {
                if let Some(r) = rows.get(*sel) {
                    if nbt_editor::apply_delete(root, &r.path) {
                        *dirty = true;
                        *sel = sel.saturating_sub(1).min(rows.len().saturating_sub(2));
                        *status = "deleted — ctrl-s to save".into();
                    }
                }
            }
            _ => {}
        }
        let _ = status;
        if k == "s" && ctrl {
            self.nbt_editor_save(cx);
        }
    }

    /// upstream zoomFactor: scale /= factor, anchored at the canvas center.
    fn zoom_key(&mut self, factor: f32, cx: &mut Context<Self>) {
        if self.map.disabled {
            return;
        }
        let center = Point2f::new(self.map.width / 2.0, self.map.height / 2.0);
        let old = self.map.scale;
        self.map.scale /= factor;
        if self.map.update_scale(old, center) {
            self.request_update();
        }
        cx.notify();
    }

    // -- canvas painting ---------------------------------------------------------

    fn paint_map(&mut self, bounds: Bounds<Pixels>, window: &mut Window, _cx: &mut App) {
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

        // frame-paced tile dispatch: scroll/zoom/view changes arm update_now
        // per input event; running the O(visible-regions) update once here
        // coalesces bursts (scroll wheels can fire many events per frame).
        if self.update_now {
            self.map.update();
            self.update_now = false;
        }

        // held-key pan: upstream KeyActivator ticks every 33ms applying
        // 5*scale per tick (10*scale while shift is held).
        if self.pressed_keys != 0 && !self.map.disabled {
            self.pan_accum += dt;
            if self.pan_accum >= std::time::Duration::from_millis(33) {
                self.pan_accum = std::time::Duration::ZERO;
                let speed = if self.pan_shift { 10.0 } else { 5.0 };
                let dx = if self.pressed_keys & 2 != 0 { -speed } else if self.pressed_keys & 8 != 0 { speed } else { 0.0 };
                let dz = if self.pressed_keys & 1 != 0 { -speed } else if self.pressed_keys & 4 != 0 { speed } else { 0.0 };
                self.map.offset = self.map.offset.add(dx * self.map.scale, dz * self.map.scale);
            }
            window.on_next_frame(|w, _| w.refresh());
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
            window.paint_quad(fill(bounds, rgba(EMPTY_COLOR)));
        } else {
            window.paint_quad(fill(bounds, rgba(0x000000ff)));
        }

        let (vmin, vmax) = self.map.visible_region_bounds(Point2f::ZERO);

        // -- tile pass ---------------------------------------------------------
        // upstream draws only tiles whose rect intersects the canvas. Iterate
        // the bounded tile map instead of the visible region grid: at max
        // zoom-out the grid is ~32k cells while tile count is capped by
        // MAX_TILE_REQUESTS, and hash lookups dominate the per-frame cost.
        // Collect + sort so paint order is identical to the old rx/rz raster
        // scan (x-major, z inner).
        let tile_px = TILE_SIZE as f32 / scale;
        {
            let map = &self.map;
            let buf = &mut self.visible_tiles_buf;
            let paste_cache = &mut self.paste_overlay_cache;

            buf.clear();
            buf.extend(map.tiles.values().filter_map(|t| {
                let l = t.location;
                // same region range the old raster loop visited — truncating
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
            // config reads used to run per tile (each clones BTreeMaps);
            // now a read-guard held for the tile pass — zero clones, still
            // only taken when something needs it.
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
                // cull tiles fully outside the canvas before any paint work
                if ox + tile_px <= 0.0
                    || oz + tile_px <= 0.0
                    || ox >= map.width
                    || oz >= map.height
                {
                    continue;
                }
                if tile_px < 0.5 {
                    continue;
                }
                let b = Bounds {
                    origin: point(px(ox), px(oz)),
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
                            origin: point(px(ox), px(oz)),
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
                                window.paint_quad(fill(b, rgba(region_color)));
                            }
                        } else if sel.is_region_selected(key) {
                            window.paint_quad(fill(b, rgba(region_color)));
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
                        for &key in sel.selection.keys() {
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
        if !self.map.disabled && self.map.show_structure_icons && scale <= header_zoom as f32 {
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
                color: rgba(COORDINATES_COLOR).into(),
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
                            Bounds { origin: point(sx, px(0.)), size: size(px(GRID_LINE_WIDTH), bounds.size.height) },
                            rgba(CHUNK_GRID_COLOR),
                        ));
                    }
                }
            }
            for rz in rmin.z..=rmax.z + 1 {
                for i in 0..32 {
                    let sz = px(((rz * 512 + i * 16) as f32 - self.map.offset.z) / scale);
                    if sz >= px(0.) && sz <= bounds.size.height {
                        window.paint_quad(fill(
                            Bounds { origin: point(px(0.), sz), size: size(bounds.size.width, px(GRID_LINE_WIDTH)) },
                            rgba(CHUNK_GRID_COLOR),
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
                        Bounds { origin: point(sx, px(0.)), size: size(px(GRID_LINE_WIDTH), bounds.size.height) },
                        rgba(REGION_GRID_COLOR),
                    ));
                }
            }
            for rz in rmin.z..=rmax.z + 1 {
                let sz = px((rz as f32 * 512.0 - self.map.offset.z) / scale);
                if sz >= px(0.) && sz <= bounds.size.height {
                    window.paint_quad(fill(
                        Bounds { origin: point(px(0.), sz), size: size(bounds.size.width, px(GRID_LINE_WIDTH)) },
                        rgba(REGION_GRID_COLOR),
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
    let _ = line.paint(point(px(sx), px(sz)), px(14.), window, cx);
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
                    origin: point(px(icon_l), px(icon_t)),
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

/// pasted-chunks ghost images keyed by target region long. Same chunk
/// mapping as TileMapState::pasted_chunks_for_region, computed for all
/// affected regions at once so the per-tile loop is a hash lookup.
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
            for (&region_l, cs) in &sel.selection {
                let base = Point2i::from_long(region_l).region_to_chunk();
                // effective set = cs or full region; complement when inverted
                let eff: Option<ChunkSet> = match cs {
                    Some(c) => Some(if sel.inverted { c.flip() } else { c.clone() }),
                    None => {
                        if sel.inverted { Some(ChunkSet::new()) } else { None }
                    }
                };
                match eff {
                    Some(c) => {
                        for i in c.iter() {
                            put(base.add(i as i32 % 32, i as i32 / 32).add_p(p.offset));
                        }
                    }
                    None => {
                        for i in 0..1024i32 {
                            put(base.add(i % 32, i / 32).add_p(p.offset));
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
                            put(base.add(i % 32, i / 32).add_p(p.offset));
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

impl MainView {
    /// rebuild the OS menus so checkmark prefixes track toggle state.
    fn rebuild_menus(cx: &mut Context<Self>) {
        cx.set_menus(crate::menus(cx));
    }

    /// status-bar text (upstream StatusBar): hovered Block/Chunk/Region,
    /// Selected (∞ when inverted), Total tiles, Queue, Overlay value,
    /// Structures; "-" placeholders without a hover; a spinner runs while
    /// jobs are queued. Cached, rebuilt only when a StatusKey input differs.
    fn status_text(&mut self) -> SharedString {
        // upstream status bar: hovered structure names
        let structure = if self.map.show_structure_icons {
            self.map.hovered_block.and_then(|h| {
                let c = h.block_to_chunk();
                let r = c.chunk_to_region();
                self.map
                    .tiles
                    .get(&r.as_long())
                    .and_then(|t| t.structures.as_ref())
                    .and_then(|m| m.get(&c.as_long()))
                    .filter(|names| !names.is_empty())
                    .map(|names| (names[0].clone(), names.len() > 1))
            })
        } else {
            None
        };
        // upstream: "type(min, max), hoveredValue"
        let overlay = self.map.overlay.as_ref().map(|ov| {
            let val = if !ov.active {
                None
            } else {
                self.map.hovered_block.and_then(|h| {
                    let c = h.block_to_chunk();
                    let r = c.chunk_to_region();
                    let rel = c.sub(r.region_to_chunk());
                    self.map
                        .tiles
                        .get(&r.as_long())
                        .and_then(|t| t.overlay_data.as_ref())
                        .map(|d| d[(rel.z * 32 + rel.x) as usize])
                })
            };
            (ov.typ, ov.min, ov.max, val)
        });
        let key = StatusKey {
            total_tiles: self.map.tiles.len(),
            hovered: self.map.hovered_block,
            selected_chunks: self.map.selected_chunks,
            inverted: self.map.selection.is_inverted(),
            unsaved_selection: self.map.unsaved_selection,
            structure,
            overlay,
            jobs: mc_core::jobs::active_jobs(),
            spin: self.spin,
        };
        if let Some((k, s)) = &self.status_cache {
            if *k == key {
                return s.clone();
            }
        }
        use std::fmt::Write;
        let mut s = String::with_capacity(160);
        if let Some(h) = key.hovered {
            let c = h.block_to_chunk();
            let r = h.block_to_region();
            let _ = write!(
                s,
                "Block: {}, {}  Chunk: {}, {}  Region: {}, {}",
                h.x, h.z, c.x, c.z, r.x, r.z
            );
        } else {
            s.push_str("Block: -, -  Chunk: -, -  Region: -, -");
        }
        let sel = if key.inverted {
            "∞".to_string()
        } else {
            key.selected_chunks.to_string()
        };
        // upstream has no dirty marker; the trailing * is our
        // unsaved-selection indicator.
        let _ = write!(
            s,
            "  Selected: {}{}  Total: {}  Queue: {}",
            sel,
            if key.unsaved_selection { "*" } else { "" },
            key.total_tiles,
            key.jobs
        );
        match &key.overlay {
            Some((typ, mn, mx, val)) => {
                let mm = match (mn, mx) {
                    (Some(a), Some(b)) => format!("{}, {}", a, b),
                    _ => "-, -".to_string(),
                };
                let _ = write!(
                    s,
                    "  Overlay: {}({}), {}",
                    typ.display_name(),
                    mm,
                    val.map(|v| v.to_string()).unwrap_or_else(|| "-".into())
                );
            }
            None => s.push_str("  Overlay: -, -"),
        }
        match &key.structure {
            Some((name, more)) => {
                let _ = write!(s, "  Structures: {}{}", name, if *more { ", ..." } else { "" });
            }
            None => s.push_str("  Structures: -"),
        }
        if key.jobs > 0 {
            // cheap busy indicator — the upstream status bar shows a
            // rotating load icon while the job queue is non-empty.
            const SPIN: [char; 4] = ['-', '\\', '|', '/'];
            let _ = write!(s, "  {}", SPIN[key.spin as usize % 4]);
        }
        let s = SharedString::from(s);
        self.status_cache = Some((key, s.clone()));
        s
    }
}

/// static nbt type icons (img/nbt/*.png), lazily decoded once.
fn nbt_icon(type_id: u8) -> Option<Arc<RenderImage>> {
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
            .on_action(cx.listener(|v, _: &OpenRecentAction, w, cx| v.open_recent_dialog(w, cx)))
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
            .on_action(cx.listener(|v, a: &ToggleStructureIcon, _w, cx| {
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
                let keys: Vec<i64> = if sel.inverted {
                    let mut k: std::collections::HashSet<i64> = std::collections::HashSet::new();
                    if let Some(ws) = config::world() {
                        k.extend(files::scan_region_files(ws.region_dir()));
                        for d in [&ws.dirs.poi, &ws.dirs.entities].into_iter().flatten() {
                            k.extend(files::scan_region_files(d));
                        }
                    }
                    k.into_iter().collect()
                } else {
                    sel.selection.keys().copied().collect()
                };
                for k in keys {
                    v.map.invalidate_region(Point2i::from_long(k), true);
                }
                v.request_update();
                cx.notify();
            }))
            .on_action(cx.listener(|v, _: &DeleteChunksAction, _w, cx| v.delete_chunks(cx)))
            .on_action(cx.listener(|v, _: &DefragmentAction, _w, cx| v.defragment(cx)))
            .on_action(cx.listener(|v, _: &ExportChunksAction, _w, cx| v.export_chunks(cx)))
            .on_action(cx.listener(|v, _: &CopyChunksAction, _w, cx| v.copy_chunks(cx)))
            .on_action(cx.listener(|v, _: &PasteChunksAction, w, cx| v.paste_chunks(w, cx)))
            .on_action(cx.listener(|v, _: &ExportImageAction, w, cx| v.export_image(w, cx)))
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
                let x = f32::from(e.position.x);
                let y = f32::from(e.position.y);
                let inside = x >= 0.0 && y >= 0.0 && x < v.map.width && y < v.map.height;
                if !inside && v.map.hovered_block.take().is_some() {
                    cx.notify();
                }
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(|v, _e: &MouseUpEvent, _w, cx| {
                if v.height_slider_drag {
                    v.height_slider_drag = false;
                    if let Some(ws) = config::world() {
                        ws.cfg.save(&ws.cache_dir);
                    }
                }
            }))
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
                    .children(self.render_dialog(cx)),
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

impl MainView {
    fn save_screenshot(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_new_path(
            &PathBuf::from(std::env::var("HOME").unwrap_or_default()),
            Some("screenshot.png"),
        );
        cx.spawn(async move |this: WeakEntity<MainView>, cx: &mut AsyncApp| {
            if let Ok(Ok(Some(path))) = rx.await {
                let _ = this.update(&mut *cx, |v, cx| {
                    let size = TILE_SIZE;
                    let mut img = vec![0u32; size * size];
                    // render visible tiles into a screenshot at current zoom=1? upstream
                    let cfg = config::world().map(|w| w.cfg.clone());
                    let Some(cfg) = cfg else { return };
                    let rs = mc_core::render::RenderSettings {
                        render_height: cfg.render_height,
                        layer_only: cfg.render_layer_only,
                        caves: cfg.render_caves,
                        shade: cfg.shade,
                        shade_water: cfg.shade_water,
                        shade_altitude: cfg.shade_altitude,
                    };
                    let _ = rs;
                    let mut rgba = Vec::with_capacity(size * size * 4);
                    for &p in img.iter() {
                        rgba.push((p >> 16) as u8);
                        rgba.push((p >> 8) as u8);
                        rgba.push(p as u8);
                        rgba.push((p >> 24) as u8);
                    }
                    let _ = image::save_buffer(&path, &rgba, size as u32, size as u32, image::ColorType::Rgba8);
                    let _ = cx;
                });
            }
        })
        .detach();
    }

    fn import_selection(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Select a csv file".into()),
        });
        cx.spawn(async move |this: WeakEntity<MainView>, cx: &mut AsyncApp| {
            if let Ok(Ok(Some(paths))) = rx.await {
                if let Some(p) = paths.into_iter().next() {
                    let _ = this.update(&mut *cx, |v, cx| {
                        match mc_core::selection::Selection::read_csv(&p) {
                            Ok(sel) => {
                                v.map.selection.merge(&sel);
                                v.map.selected_chunks = v.map.selection.count() as i64;
                                v.map.unsaved_selection = true;
                                v.map.selection_version += 1;
                            }
                            Err(e) => {
                                v.dialog = Some(Dialog::Error { msg: e.to_string() });
                            }
                        }
                        cx.notify();
                    });
                }
            }
        })
        .detach();
    }

    fn export_selection(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_new_path(
            &PathBuf::from(std::env::var("HOME").unwrap_or_default()),
            Some("selection.csv"),
        );
        let sel = self.map.selection.clone();
        cx.spawn(async move |_this, _cx| {
            if let Ok(Ok(Some(path))) = rx.await {
                let _ = sel.save_csv(&path);
            }
        })
        .detach();
    }



    fn height_from_slider_x(&self, x: f32) -> i32 {
        let cur = config::WORLD.read().unwrap().as_ref()
            .map(|w| w.cfg.render_height).unwrap_or(319);
        let Some(b) = *self.height_slider_bounds.borrow() else {
            return cur;
        };
        let w = f32::from(b.size.width);
        if w <= 0. {
            return cur;
        }
        let frac = ((x - f32::from(b.origin.x)) / w).clamp(0., 1.);
        (-64. + frac * 383.).round() as i32
    }

    /// upstream HeightSlider: the slider pins at -64..319 but the numeric
    /// field accepts -64..2047 — render_height keeps the typed value while
    /// the slider thumb clamps (frac clamps to 0..1 in render).
    fn set_render_height(&mut self, h: i32, persist: bool, cx: &mut Context<Self>) {
        let h = h.clamp(-64, 2047);
        let cur = config::WORLD.read().unwrap().as_ref()
            .map(|w| w.cfg.render_height).unwrap_or(319);
        if h == cur && !persist {
            return;
        }
        // mutate under the write lock — cloning WorldState + set_world per
        // slider-move event was avoidable alloc work on the drag path.
        // (save runs after the lock is released, as before.)
        if let Some(ws) = config::WORLD.write().unwrap().as_mut() {
            ws.cfg.render_height = h;
        }
        if persist {
            if let Some(ws) = config::WORLD.read().unwrap().as_ref() {
                ws.cfg.save(&ws.cache_dir);
            }
        }
        self.map.mark_all_tiles_obsolete();
        self.request_update();
        cx.notify();
    }

    fn render_height_slider(&self, cx: &mut Context<Self>) -> AnyElement {
        let h = config::WORLD.read().unwrap().as_ref()
            .map(|w| w.cfg.render_height).unwrap_or(319);
        let frac = (((h + 64) as f32) / 383.).clamp(0., 1.);
        let slot = self.height_slider_bounds.clone();
        div()
            .h(px(32.))
            .w_full()
            .bg(rgb(0x242424))
            .flex()
            .items_center()
            .px_2()
            .gap_2()
            .text_size(px(12.))
            .text_color(rgb(0xbbbbbb))
            // upstream: min label, slider, max label, numeric field
            .child(div().w(px(30.)).flex_none().child("-64"))
            .child(
                div()
                    .id("height-slider")
                    .flex_grow()
                    .h_full()
                    .cursor_pointer()
                    .on_mouse_down(MouseButton::Left, cx.listener(|v, e: &MouseDownEvent, _w, cx| {
                        v.height_slider_drag = true;
                        let hh = v.height_from_slider_x(f32::from(e.position.x));
                        v.set_render_height(hh, false, cx);
                    }))
                    // upstream slider scroll adjusts the height by +/-1
                    .on_scroll_wheel(cx.listener(|v, e: &ScrollWheelEvent, _w, cx| {
                        let dy = match e.delta {
                            ScrollDelta::Pixels(p) => f32::from(p.y),
                            ScrollDelta::Lines(l) => l.y,
                        };
                        if dy != 0.0 {
                            let cur = config::WORLD.read().unwrap().as_ref()
                                .map(|w| w.cfg.render_height).unwrap_or(319);
                            let next = cur + if dy > 0.0 { 1 } else { -1 };
                            v.set_render_height(next.clamp(-64, 319), true, cx);
                        }
                    }))
                    .child(canvas(
                        move |b, _w, _cx| {
                            *slot.borrow_mut() = Some(b);
                        },
                        move |b, _t, window, _cx| {
                            let cy = b.center().y;
                            let tx = b.origin.x + b.size.width * frac;
                            window.paint_quad(fill(
                                Bounds {
                                    origin: point(b.origin.x, cy - px(3.)),
                                    size: gpui::size(b.size.width, px(6.)),
                                },
                                rgb(0x181818),
                            ));
                            window.paint_quad(quad(
                                Bounds {
                                    origin: point(tx - px(7.), cy - px(13.)),
                                    size: gpui::size(px(14.), px(26.)),
                                },
                                px(3.),
                                rgb(0xcccccc),
                                px(1.),
                                rgb(0x555555),
                                BorderStyle::Solid,
                            ));
                        },
                    )),
            )
            .child(div().w(px(30.)).flex_none().child("319"))
            // upstream NumberTextField(-64..2047): click the value to type a
            // height — values above 319 pin the slider thumb at max but set
            // render_height to the typed number.
            .child(
                div()
                    .id("height-field")
                    .w(px(76.))
                    .flex_none()
                    .cursor_pointer()
                    .child(format!("height {}", h))
                    .on_click(cx.listener(|v, _, _w, cx| {
                        let cur = config::WORLD.read().unwrap().as_ref()
                            .map(|w| w.cfg.render_height).unwrap_or(319);
                        v.dialog = Some(Dialog::TextInput {
                            title: "Render height".into(),
                            hint: "-64 .. 2047".into(),
                            text: cur.to_string(),
                            on_done: Box::new(|v, _w, cx, t| {
                                match t.trim().parse::<i32>() {
                                    Ok(hh) => v.set_render_height(hh, true, cx),
                                    Err(_) => {
                                        v.dialog = Some(Dialog::Error {
                                            msg: format!("not a number: {t}"),
                                        });
                                        cx.notify();
                                    }
                                }
                            }),
                        });
                        cx.notify();
                    })),
            )
            .into_any_element()
    }


    /// toggle button for a RenderSettings draft field; `set` picks the field.
    fn toggle_render(
        &self,
        label: &'static str,
        val: bool,
        set: fn(&mut crate::config::WorldConfig) -> &mut bool,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        div()
            .id(SharedString::from(format!("rs-{label}")))
            .px_2().py_1()
            .bg(if val { rgb(0x2a4a2a) } else { rgb(0x3c3f41) })
            .border_1().border_color(rgb(0x555555))
            .rounded_sm()
            .cursor_pointer()
            .text_size(px(12.))
            .child(format!("{}: {}", label, if val { "on" } else { "off" }))
            .on_click(cx.listener(move |v, _, _w, cx| {
                if let Some(Dialog::RenderSettings { draft }) = &mut v.dialog {
                    let f = set(draft);
                    *f = !*f;
                }
                cx.notify();
            }))
    }

    /// toggle button for a GlobalConfig draft field; `set` picks the field.
    fn toggle_global(
        &self,
        label: &'static str,
        val: bool,
        set: fn(&mut crate::config::GlobalConfig) -> &mut bool,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        div()
            .id(SharedString::from(format!("gs-{label}")))
            .px_2().py_1()
            .bg(if val { rgb(0x2a4a2a) } else { rgb(0x3c3f41) })
            .border_1().border_color(rgb(0x555555))
            .rounded_sm()
            .cursor_pointer()
            .text_size(px(12.))
            .child(format!("{}: {}", label, if val { "on" } else { "off" }))
            .on_click(cx.listener(move |v, _, _w, cx| {
                if let Some(Dialog::Settings { draft }) = &mut v.dialog {
                    let f = set(draft);
                    *f = !*f;
                }
                cx.notify();
            }))
    }

    // -- chunk operations -------------------------------------------------------

    fn delete_chunks(&mut self, cx: &mut Context<Self>) {
        let sel = self.map.selection.clone();
        if sel.is_empty() { return; }
        // upstream DeleteConfirmationDialog: verbose header unless inverted
        let msg = if sel.inverted {
            "You are about to delete an unknown number of chunks from this world.".to_string()
        } else {
            format!("You are about to delete {} chunks from this world.", self.map.selected_chunks)
        };
        self.dialog = Some(Dialog::Confirm {
            msg: format!("{msg}\nAre you sure?"),
            action: Box::new(move |v, _w, cx| {
                let dirs = crate::config::world().map(|w| w.dirs.clone()).unwrap_or_default();
                crate::ops::for_each_selected_region(
                    &sel, dirs, "Deleting chunks", v.map.result_tx.clone(),
                    |_region, chunks, m, path, _dirs| {
                        match chunks {
                            None => {
                                let _ = std::fs::remove_file(path);
                                false
                            }
                            Some(cs) => {
                                m.delete_chunks(cs);
                                true
                            }
                        }
                    },
                );
                v.dialog = None;
                cx.notify();
            }),
        });
        cx.notify();
    }

    fn defragment(&mut self, cx: &mut Context<Self>) {
        let sel = self.map.selection.clone();
        if sel.is_empty() { return; }
        // upstream DefragmentConfirmationDialog: verbose header unless inverted
        let msg = if sel.inverted {
            "You are about to defragment an unknown number of regions.".to_string()
        } else {
            format!("You are about to defragment {} regions.", sel.selection.len())
        };
        self.dialog = Some(Dialog::Confirm {
            msg: format!("{msg}\nAre you sure?"),
            action: Box::new(move |v, _w, cx| {
                let tx = v.map.result_tx.clone();
                mc_core::jobs::pools().add_process(mc_core::jobs::Job::new(
                    mc_core::jobs::PRIORITY_LOW, None,
                    move || {
                        let __p = mc_core::jobs::Progress::new("Defragmenting", sel.selection.len() as i64);
                        for &region_l in sel.selection.keys() {
                            let __g = __p.inc_guard();
                            let region = mc_core::point::Point2i::from_long(region_l);
                            __p.set_current(mc_core::files::mca_file_name(region));
                            if __p.is_cancelled() { break; }
                            let Some(dir) = crate::config::world().and_then(|w| w.dirs.region.clone()) else { continue };
                            let path = dir.join(mc_core::files::mca_file_name(region));
                            if !path.exists() { continue; }
                            if let Ok(mut m) = mc_core::mca::McaFile::new(path.clone()) {
                                if m.load_offsets().is_ok() {
                                    let _ = m.defragment(&path);
                                }
                            }
                            let _ = tx.send(crate::tilemap::TileResult::Image {
                                region, image: None, zoom: 0, structures: None, cache_write: false,
                            });
                        }
                    }));
                v.dialog = None;
                cx.notify();
            }),
        });
        cx.notify();
    }

    fn export_chunks(&mut self, cx: &mut Context<Self>) {
        let sel = self.map.selection.clone();
        if sel.is_empty() { return; }
        let n = self.map.selected_chunks;
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: false, directories: true, multiple: false,
            prompt: Some("Select world directory to export to".into()),
        });
        // upstream ExportConfirmationDialog after the directory pick
        cx.spawn(async move |this: WeakEntity<MainView>, cx: &mut AsyncApp| {
            if let Ok(Ok(Some(paths))) = rx.await {
                if let Some(dir) = paths.into_iter().next() {
                    let msg = if sel.inverted {
                        "You are about to export an unknown number of chunks from this world.".to_string()
                    } else {
                        format!("You are about to export {n} chunks from this world.")
                    };
                    let _ = this.update(&mut *cx, |v, cx| {
                        v.dialog = Some(Dialog::Confirm {
                            msg: format!("{msg}\nto {}\nAre you sure?", dir.display()),
                            action: Box::new(move |v, _w, cx| {
                                v.do_export_chunks(sel.clone(), dir.clone());
                                v.dialog = None;
                                cx.notify();
                            }),
                        });
                        cx.notify();
                    });
                }
            }
        }).detach();
    }

    fn do_export_chunks(&mut self, sel: mc_core::selection::Selection, dir: PathBuf) {
        let region_dir = dir.join("region");
        let _ = std::fs::create_dir_all(&region_dir);
        mc_core::jobs::pools().add_process(mc_core::jobs::Job::new(
            mc_core::jobs::PRIORITY_LOW, None,
            move || {
                let __p = mc_core::jobs::Progress::new("Exporting chunks", sel.selection.len() as i64);
                for (&region_l, chunks) in &sel.selection {
                    let __g = __p.inc_guard();
                    let region = mc_core::point::Point2i::from_long(region_l);
                    __p.set_current(mc_core::files::mca_file_name(region));
                    if __p.is_cancelled() { break; }
                    let Some(src_dir) = crate::config::world().and_then(|w| w.dirs.region.clone()) else { continue };
                    let src = src_dir.join(mc_core::files::mca_file_name(region));
                    let dst = region_dir.join(mc_core::files::mca_file_name(region));
                    if !src.exists() { continue; }
                    match chunks {
                        None => { let _ = std::fs::copy(&src, &dst); }
                        Some(cs) => {
                            let mut m = match mc_core::mca::McaFile::new(src) { Ok(m) => m, Err(_) => continue };
                            if m.load(false).is_err() { continue; }
                            // keep only selected chunks
                            let mut keep = mc_core::selection::ChunkSet::new();
                            for i in 0..1024 { if !cs.get(i) { keep.set(i); } }
                            m.delete_chunks(&keep);
                            let _ = m.save_with_temp_file(&dst);
                        }
                    }
                }
            }));
    }

    fn copy_chunks(&mut self, cx: &mut Context<Self>) {
        // upstream copies selected chunks into cache-dir clipboard world
        let sel = self.map.selection.clone();
        if sel.is_empty() { return; }
        let clip = mc_core::files::base_cache_dir().join("clipboard").join("region");
        let _ = std::fs::remove_dir_all(&clip);
        let _ = std::fs::create_dir_all(&clip);
        let origin = sel.selection.keys().next().copied();
        let Some(origin_l) = origin else { return };
        let origin = mc_core::point::Point2i::from_long(origin_l);
        let total = sel.selection.len() as i64;
        mc_core::jobs::pools().add_process(mc_core::jobs::Job::new(
            mc_core::jobs::PRIORITY_LOW, None,
            move || {
                let __p = mc_core::jobs::Progress::new("Copying chunks", total);
                for (&region_l, chunks) in &sel.selection {
                    let __g = __p.inc_guard();
                    let region = mc_core::point::Point2i::from_long(region_l);
                    __p.set_current(mc_core::files::mca_file_name(region));
                    if __p.is_cancelled() { break; }
                    let Some(src_dir) = crate::config::world().and_then(|w| w.dirs.region.clone()) else { continue };
                    let src = src_dir.join(mc_core::files::mca_file_name(region));
                    let dst = clip.join(mc_core::files::mca_file_name(region));
                    if !src.exists() { continue; }
                    match chunks {
                        None => { let _ = std::fs::copy(&src, &dst); }
                        Some(cs) => {
                            let mut m = match mc_core::mca::McaFile::new(src) { Ok(m) => m, Err(_) => continue };
                            if m.load(false).is_err() { continue; }
                            let mut keep = mc_core::selection::ChunkSet::new();
                            for i in 0..1024 { if !cs.get(i) { keep.set(i); } }
                            m.delete_chunks(&keep);
                            let _ = m.save_with_temp_file(&dst);
                        }
                    }
                }
                // record origin + selection for offset paste
                let _ = std::fs::write(clip.join("../origin.txt"), format!("{},{}", origin.x, origin.z));
                let _ = sel.save_csv(&clip.join("../selection.csv"));
            }));
        cx.notify();
    }

    fn paste_chunks(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.map.pasted.is_some() {
            let o = self.map.pasted.as_ref().unwrap().offset;
            self.dialog = Some(Dialog::PasteConfirm {
                ox: o.x, oz: o.z, overwrite: true, sel_only: false,
            });
            cx.notify();
            return;
        }
        let clip = mc_core::files::base_cache_dir().join("clipboard").join("region");
        if !clip.is_dir() { return; }
        // upstream pastes via SelectionData on the system clipboard — ours is
        // the clipboard region dir + selection.csv.
        let sel = mc_core::selection::Selection::read_csv(
            &mc_core::files::base_cache_dir().join("clipboard").join("selection.csv"),
        ).ok();
        // upstream centers the ghost using the CANVAS size in chunks —
        // map.width/height track the painted canvas rect (window minus
        // title/status/option bars), not the raw window bounds.
        let size = Point2f::new(self.map.width, self.map.height);
        self.map.set_pasted(clip, sel, size);
        cx.notify();
    }

    /// import clipboard chunks at offset (upstream ImportJob via PasteChunks confirm)
    fn do_paste_import(&mut self, ox: i32, oz: i32, overwrite: bool, sel_only: bool, cx: &mut Context<Self>) {
        let clip = mc_core::files::base_cache_dir().join("clipboard").join("region");
        if !clip.is_dir() { return; }
        let target_sel = if sel_only { Some(self.map.selection.clone()) } else { None };
        let pasted_sel = self.map.pasted.as_ref().and_then(|p| p.selection.clone());
        let offset = mc_core::point::Point2i::new(ox, oz);
        let tx = self.map.result_tx.clone();
        self.map.cancel_paste();
        import_mca_dir(clip, pasted_sel, offset, overwrite, target_sel, tx);
        cx.notify();
    }
    fn export_image(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let sel = self.map.selection.clone();
        if sel.is_empty() {
            self.dialog = Some(Dialog::Error { msg: "no selection".into() });
            cx.notify();
            return;
        }
        // upstream generateImageFromSelection guards the pixel count before
        // the file pick; ours is region-aligned (512px per region).
        let mut minx = i32::MAX; let mut minz = i32::MAX;
        let mut maxx = i32::MIN; let mut maxz = i32::MIN;
        for &region_l in sel.selection.keys() {
            let r = mc_core::point::Point2i::from_long(region_l);
            minx = minx.min(r.x); minz = minz.min(r.z);
            maxx = maxx.max(r.x); maxz = maxz.max(r.z);
        }
        let w = (maxx - minx + 1) as usize * 512;
        let h = (maxz - minz + 1) as usize * 512;
        if (w as i64) * (h as i64) > i32::MAX as i64 {
            self.dialog = Some(Dialog::Error {
                msg: format!("dimensions are too large to generate an image: {w}x{h}"),
            });
            cx.notify();
            return;
        }
        let rx = cx.prompt_for_new_path(
            &PathBuf::from(std::env::var("HOME").unwrap_or_default()),
            Some("selection.png"),
        );
        // upstream ImageExportConfirmationDialog after the file pick
        cx.spawn(async move |this: WeakEntity<MainView>, cx: &mut AsyncApp| {
            if let Ok(Ok(Some(path))) = rx.await {
                let _ = this.update(&mut *cx, |v, cx| {
                    v.dialog = Some(Dialog::Confirm {
                        msg: format!(
                            "You are about to create an image with the dimensions {w}x{h}.\n{}\nAre you sure?",
                            path.display()
                        ),
                        action: Box::new(move |v, _w, cx| {
                            v.do_export_image(sel.clone(), path.clone(), w, h, minx, minz);
                            v.dialog = None;
                            cx.notify();
                        }),
                    });
                    cx.notify();
                });
            }
        }).detach();
    }

    fn do_export_image(&mut self, sel: mc_core::selection::Selection, path: PathBuf, w: usize, h: usize, minx: i32, minz: i32) {
        {
                let cfg = crate::config::world().map(|w| {
                    mc_core::render::RenderSettings {
                        render_height: w.cfg.render_height,
                        layer_only: w.cfg.render_layer_only,
                        caves: w.cfg.render_caves,
                        shade: w.cfg.shade,
                        shade_water: w.cfg.shade_water,
                        shade_altitude: w.cfg.shade_altitude,
                    }
                }).unwrap_or_default();
                let sel2 = sel.clone();
                mc_core::jobs::pools().add_process(mc_core::jobs::Job::new(
                    mc_core::jobs::PRIORITY_LOW, None,
                    move || {
                        let __p = mc_core::jobs::Progress::new("Exporting image", sel2.selection.len() as i64);
                        let mut px = vec![0u32; w * h];
                        let corrupted = [0u32; 256];
                        for &region_l in sel2.selection.keys() {
                            let __g = __p.inc_guard();
                            let region = mc_core::point::Point2i::from_long(region_l);
                            __p.set_current(mc_core::files::mca_file_name(region));
                            if __p.is_cancelled() { break; }
                            let Some(dir) = crate::config::world().and_then(|w| w.dirs.region.clone()) else { continue };
                            let p = dir.join(mc_core::files::mca_file_name(region));
                            if !p.exists() { continue; }
                            let Ok(mut m) = mc_core::mca::McaFile::new(p) else { continue };
                            if m.load(true).is_err() { continue; }
                            if let Some(img) = mc_core::render::generate_image(&m, 1, &cfg, &corrupted) {
                                let ox = ((region.x - minx) * 512) as usize;
                                let oz = ((region.z - minz) * 512) as usize;
                                for z in 0..512usize {
                                    for x in 0..512usize {
                                        px[(oz + z) * w + ox + x] = img[z * 512 + x];
                                    }
                                }
                            }
                        }
                        let mut rgba = Vec::with_capacity(px.len() * 4);
                        for &p in px.iter() {
                            rgba.push((p >> 16) as u8);
                            rgba.push((p >> 8) as u8);
                            rgba.push(p as u8);
                            rgba.push((p >> 24) as u8);
                        }
                        let _ = image::save_buffer(&path, &rgba, w as u32, h as u32, image::ColorType::Rgba8);
                    }));
        }
    }


    /// upstream SumSelectionJob: sums the ACTIVE OVERLAY's parseValue over all
    /// selected chunks (that's why it's disabled without an overlay).
    fn sum_selection(&mut self, cx: &mut Context<Self>) {
        let sel = self.map.selection.clone();
        let Some(ov) = self.map.overlay.clone() else { return };
        if sel.is_empty() { return; }
        // upstream NumberDialog title: "<type>(<short multi values>)"
        let mv = ov.raw_multi.trim();
        *SUM_TITLE.lock().unwrap() = Some(if mv.is_empty() {
            ov.typ.display_name().to_string()
        } else {
            format!("{}({})", ov.typ.display_name(), mv)
        });
        let dirs = crate::config::world().map(|w| w.dirs.clone());
        mc_core::jobs::pools().add_process(mc_core::jobs::Job::new(
            mc_core::jobs::PRIORITY_LOW, None,
            move || {
                let __p = mc_core::jobs::Progress::new("Summing selection", sel.selection.len() as i64);
                let mut sum: i64 = 0;
                'regions: for (&region_l, chunks) in &sel.selection {
                    let __g = __p.inc_guard();
                    if __p.is_cancelled() { break; }
                    let region = mc_core::point::Point2i::from_long(region_l);
                    let name = mc_core::files::mca_file_name(region);
                    __p.set_current(format!("{} ({sum})", name));
                    let Some(dirs) = &dirs else { break };
                    let load = |d: Option<&std::path::PathBuf>| -> Option<mc_core::mca::McaFile> {
                        let p = d?.join(&name);
                        if !p.exists() { return None; }
                        mc_core::mca::McaFile::new(p).ok().and_then(|mut m| m.load(false).ok().map(|_| m))
                    };
                    let rm = load(dirs.region.as_ref());
                    let pm = load(dirs.poi.as_ref());
                    let em = load(dirs.entities.as_ref());
                    for i in 0..1024 {
                        let abs = region.region_to_chunk().add(i as i32 % 32, i as i32 / 32);
                        // mask: sel.selection holds the chunk-set (None = all)
                        let present = match chunks {
                            Some(cs) => cs.get(i),
                            None => true,
                        };
                        if sel.inverted == present { continue; }
                        let _ = abs;
                        let Some(c) = rm.as_ref().and_then(|m| m.chunks[i].as_ref()) else { continue };
                        let Some(d) = &c.data else { continue };
                        let pd = pm.as_ref().and_then(|m| m.chunks[i].as_ref()).and_then(|ch| ch.data.as_ref());
                        let ed = em.as_ref().and_then(|m| m.chunks[i].as_ref()).and_then(|ch| ch.data.as_ref());
                        let dv = mc_core::nbt::data_version(Some(d));
                        sum += ov.parse_value(d, pd, ed, c.timestamp, dv) as i64;
                        if sum < 0 {
                            *crate::tilemap::SUM_RESULT.lock().unwrap() = Some(-1);
                            break 'regions;
                        }
                    }
                }
                *crate::tilemap::SUM_RESULT.lock().unwrap() = Some(sum);
            }));
        cx.notify();
    }

    /// upstream importChunks: pick a source WORLD directory, then the
    /// ImportConfirmationDialog (offset, overwrite, selection only) — the
    /// y-offset and section-range options are not ported (they would need
    /// section-level NBT rewriting).
    fn import_chunks(&mut self, cx: &mut Context<Self>) {
        if self.map.disabled { return; }
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: false, directories: true, multiple: false,
            prompt: Some("Select world directory to import from".into()),
        });
        cx.spawn(async move |this: WeakEntity<MainView>, cx: &mut AsyncApp| {
            if let Ok(Ok(Some(paths))) = rx.await {
                if let Some(dir) = paths.into_iter().next() {
                    let _ = this.update(&mut *cx, |v, cx| {
                        let src = mc_core::files::detect_world_directories(&dir);
                        if src.region.is_none() {
                            v.dialog = Some(Dialog::Error {
                                msg: "invalid world directory, missing 'region' directory".into(),
                            });
                        } else {
                            v.dialog = Some(Dialog::Import {
                                dir,
                                ox: "0".into(),
                                oz: "0".into(),
                                overwrite: true,
                                sel_only: false,
                                focus_x: true,
                                invalid: false,
                            });
                        }
                        cx.notify();
                    });
                }
            }
        }).detach();
    }

    /// run the import after the confirmation dialog (upstream ImportJob):
    /// merge every chunk of the source world's region dir into the open
    /// world, shifted by the chunk offset.
    fn do_import(&mut self, dir: PathBuf, ox: i32, oz: i32, overwrite: bool, sel_only: bool, cx: &mut Context<Self>) {
        let src_region_dir = dir.join("region");
        let target_sel = if sel_only { Some(self.map.selection.clone()) } else { None };
        let tx = self.map.result_tx.clone();
        import_mca_dir(
            src_region_dir,
            None,
            Point2i::new(ox, oz),
            overwrite,
            target_sel,
            tx,
        );
        cx.notify();
    }


    fn swap_chunks(&mut self, cx: &mut Context<Self>) {
        // swap the two selected chunks (upstream dialog asks for the pair;
        // we swap the first two chunks in selection order)
        let sel = self.map.selection.clone();
        let mut coords: Vec<(i64, usize)> = Vec::new();
        for (&r, c) in &sel.selection {
            match c {
                None => {}
                Some(cs) => { for i in cs.iter() { coords.push((r, i)); } }
            }
            if coords.len() >= 2 { break; }
        }
        if coords.len() < 2 {
            self.dialog = Some(Dialog::Error { msg: "select exactly two chunks".into() });
            cx.notify();
            return;
        }
        let (r1, i1) = coords[0];
        let (r2, i2) = coords[1];
        // upstream SwapConfirmationDialog names both chunks
        let c1 = Point2i::from_long(r1).region_to_chunk().add_p(Point2i::from_chunk_index(i1 as i32));
        let c2 = Point2i::from_long(r2).region_to_chunk().add_p(Point2i::from_chunk_index(i2 as i32));
        self.dialog = Some(Dialog::Confirm {
            msg: format!(
                "You are about to swap the chunks {}, {} and {}, {}.\nAre you sure?",
                c1.x, c1.z, c2.x, c2.z
            ),
            action: Box::new(move |v, _w, cx| {
                let tx = v.map.result_tx.clone();
                mc_core::jobs::pools().add_process(mc_core::jobs::Job::new(
                    mc_core::jobs::PRIORITY_LOW, None,
                    move || {
                        let load = |rl: i64| -> Option<(mc_core::point::Point2i, std::path::PathBuf, mc_core::mca::McaFile)> {
                            let r = mc_core::point::Point2i::from_long(rl);
                            let dir = crate::config::world()?.dirs.region.clone()?;
                            let p = dir.join(mc_core::files::mca_file_name(r));
                            let mut m = mc_core::mca::McaFile::new(p.clone()).ok()?;
                            m.load(false).ok()?;
                            Some((r, p, m))
                        };
                        let (r1p, p1, mut m1) = match load(r1) { Some(v) => v, None => return };
                        let (r2p, p2, mut m2) = match load(r2) { Some(v) => v, None => return };
                        let _ = (r1p, r2p);
                        let c1 = m1.chunks[i1].take();
                        let c2 = m2.chunks[i2].take();
                        m2.chunks[i2] = c1;
                        m1.chunks[i1] = c2;
                        let _ = m1.save_with_temp_file(&p1);
                        let _ = m2.save_with_temp_file(&p2);
                        let _ = tx.send(crate::tilemap::TileResult::Image {
                            region: mc_core::point::Point2i::from_long(r1), image: None, zoom: 0, structures: None, cache_write: false,
                        });
                        let _ = tx.send(crate::tilemap::TileResult::Image {
                            region: mc_core::point::Point2i::from_long(r2), image: None, zoom: 0, structures: None, cache_write: false,
                        });
                    }));
                v.dialog = None;
                cx.notify();
            }),
        });
        cx.notify();
    }

    /// upstream ChangeNBTDialog: comma-separated `Type = value` fields with
    /// change/force and selection-only options.
    fn change_fields(&mut self, cx: &mut Context<Self>) {
        self.dialog = Some(Dialog::ChangeFields {
            text: String::new(),
            force: false,
            sel_only: !self.map.selection.is_empty(),
        });
        cx.notify();
    }

    /// OK on the ChangeFields dialog -> ChangeFieldsConfirmationDialog ->
    /// ChangeFieldsJob (mc_core::filter_ops::run_change_fields handles the
    /// per-region poi/entities iteration itself).
    fn run_change_fields_dialog(&mut self, text: String, force: bool, sel_only: bool, cx: &mut Context<Self>) {
        if text.trim().is_empty() {
            self.dialog = Some(Dialog::Error { msg: "empty field list".into() });
            cx.notify();
            return;
        }
        let fields = match mc_core::changer::parse_fields(&text) {
            Ok(f) => f,
            Err(e) => {
                self.dialog = Some(Dialog::Error { msg: e });
                cx.notify();
                return;
            }
        };
        let field_list = fields
            .iter()
            .map(|f| format!("{} = {}", f.field_type.name(), f.value))
            .collect::<Vec<_>>()
            .join(", ");
        let mut sel = self.map.selection.clone();
        if !sel_only || sel.is_empty() {
            sel = mc_core::selection::Selection::new();
        }
        let sel_opt = if sel_only && !sel.is_empty() { Some(sel) } else { None };
        let tx = self.map.result_tx.clone();
        // upstream ChangeFieldsConfirmationDialog (short header — the count is
        // unknown until the job runs)
        self.dialog = Some(Dialog::Confirm {
            msg: format!(
                "You are about to change data in an unknown number of chunks in this world.\n{field_list}\nAre you sure?"
            ),
            action: Box::new(move |v, _w, cx| {
                let dirs = crate::config::world().map(|w| w.dirs.clone()).unwrap_or_default();
                // upstream only clears the tile cache when a changed field
                // affects rendering (FieldType.requiresClearCache)
                let inv = if mc_core::changer::requires_clear_cache(&fields) {
                    all_mca_regions(&dirs)
                } else {
                    Vec::new()
                };
                let tx = tx.clone();
                mc_core::jobs::pools().add_process(mc_core::jobs::Job::new(
                    mc_core::jobs::PRIORITY_LOW, None,
                    move || {
                        let __p = mc_core::jobs::Progress::new("Changing NBT data", -1);
                        match mc_core::filter_ops::run_change_fields(&fields, force, &dirs, sel_opt.as_ref()) {
                            Ok(r) => job_info(format!(
                                "changed data in {} chunks ({} files written, {} errors)",
                                r.chunks_matched, r.files_written, r.errors
                            )),
                            Err(e) => job_info(format!("change fields failed: {e}")),
                        }
                        for region in inv {
                            let _ = tx.send(crate::tilemap::TileResult::Image {
                                region, image: None, zoom: 0, structures: None, cache_write: false,
                            });
                        }
                        let _ = __p;
                    }));
                v.dialog = None;
                cx.notify();
            }),
        });
        cx.notify();
    }


    /// Tools > Edit chunk (upstream editNBT): needs exactly 1 selected chunk.
    fn edit_nbt(&mut self, cx: &mut Context<Self>) {
        let sel = &self.map.selection;
        if sel.inverted || self.map.selected_chunks != 1 {
            self.dialog = Some(Dialog::Error {
                msg: "select exactly one chunk to edit".into(),
            });
            cx.notify();
            return;
        }
        let mut found: Option<(i64, usize)> = None;
        for (&rl, cs) in &sel.selection {
            if let Some(cs) = cs {
                for i in cs.iter() {
                    found = Some((rl, i));
                    break;
                }
            }
            if found.is_some() {
                break;
            }
        }
        let Some((region_l, index)) = found else {
            self.dialog = Some(Dialog::Error { msg: "select exactly one chunk to edit".into() });
            cx.notify();
            return;
        };
        let region = Point2i::from_long(region_l);
        let rel = Point2i::from_chunk_index(index as i32);
        let chunk_abs = region.region_to_chunk().add_p(rel);
        let Some(dir) = config::world().and_then(|w| w.dirs.region.clone()) else { return };
        let path = dir.join(mc_core::files::mca_file_name(region));
        let Ok(m) = mc_core::mca::McaFile::new(path) else { return };
        let loaded = m.load_single_chunk(chunk_abs, false).ok().flatten();
        let Some(chunk) = loaded else {
            self.dialog = Some(Dialog::Error { msg: "failed to load chunk".into() });
            cx.notify();
            return;
        };
        let Some(data) = chunk.data else {
            self.dialog = Some(Dialog::Error { msg: "chunk has no data".into() });
            cx.notify();
            return;
        };
        // upstream NBTEditorDialog: region/poi/entities tabs, each backed by
        // its own mca file of the same r.x.z name.
        let mk_tab = |dir: Option<PathBuf>, root: Option<mc_core::nbt::Compound>| -> NbtTab {
            match root {
                Some(d) => {
                    let mut expanded = std::collections::HashSet::new();
                    for k in d.keys() {
                        expanded.insert(k.clone());
                    }
                    NbtTab { root: Some(d), expanded, sel: 0, dirty: false, note: None }
                }
                None => match dir {
                    None => NbtTab::missing("no region file"),
                    Some(d) => {
                        let p = d.join(mc_core::files::mca_file_name(region));
                        let data = if p.exists() {
                            mc_core::mca::McaFile::new(p.clone())
                                .ok()
                                .and_then(|m| m.load_single_chunk(chunk_abs, false).ok().flatten())
                                .and_then(|c| c.data)
                        } else {
                            None
                        };
                        match data {
                            Some(d) => {
                                let mut expanded = std::collections::HashSet::new();
                                for k in d.keys() {
                                    expanded.insert(k.clone());
                                }
                                NbtTab { root: Some(d), expanded, sel: 0, dirty: false, note: None }
                            }
                            None if p.exists() => NbtTab::missing("no chunk data found"),
                            _ => NbtTab::missing("region file does not exist"),
                        }
                    }
                },
            }
        };
        let wdirs = config::world().map(|w| w.dirs.clone()).unwrap_or_default();
        let tabs = [
            mk_tab(wdirs.region.clone(), Some(data)),
            mk_tab(wdirs.poi.clone(), None),
            mk_tab(wdirs.entities.clone(), None),
        ];
        self.dialog = Some(Dialog::NbtEditor {
            region: region_l,
            chunk_abs,
            index,
            tab: 0,
            tabs,
            editing: NbtEdit::None,
            buf: String::new(),
            status: "1/2/3 tab  ↑↓ navigate  ←→ collapse/expand  enter edit  a add  r rename  del delete  ctrl-s save  esc close".into(),
        });
        cx.notify();
    }

    /// save every dirty tab back to its mca file through the process pool.
    fn nbt_editor_save(&mut self, cx: &mut Context<Self>) {
        let Some(Dialog::NbtEditor { region, chunk_abs, index, tab, mut tabs, status, .. }) = self.dialog.take() else {
            return;
        };
        // region/poi/entities dirs in tab order
        let mut writes: Vec<(usize, mc_core::nbt::Compound)> = Vec::new();
        for (i, t) in tabs.iter_mut().enumerate() {
            if t.dirty {
                if let Some(r) = t.root.take() {
                    writes.push((i, r.clone()));
                    t.root = Some(r);
                }
                t.dirty = false;
            }
        }
        let saving = !writes.is_empty();
        self.dialog = Some(Dialog::NbtEditor {
            region,
            chunk_abs,
            index,
            tab,
            tabs,
            editing: NbtEdit::None,
            buf: String::new(),
            status: if saving { "saving...".into() } else { status },
        });
        if writes.is_empty() {
            cx.notify();
            return;
        }
        let tx = self.map.result_tx.clone();
        mc_core::jobs::pools().add_process(mc_core::jobs::Job::new(
            mc_core::jobs::PRIORITY_LOW, None,
            move || {
                let rp = Point2i::from_long(region);
                let __p = mc_core::jobs::Progress::new("Saving chunk", writes.len() as i64);
                for (kind, root) in writes {
                    let __g = __p.inc_guard();
                    let dir = match kind {
                        0 => crate::config::world().and_then(|w| w.dirs.region.clone()),
                        1 => crate::config::world().and_then(|w| w.dirs.poi.clone()),
                        _ => crate::config::world().and_then(|w| w.dirs.entities.clone()),
                    };
                    let Some(dir) = dir else { continue };
                    let path = dir.join(mc_core::files::mca_file_name(rp));
                    __p.set_current(path.display().to_string());
                    let Ok(mut m) = mc_core::mca::McaFile::new(path.clone()) else { continue };
                    if m.load(false).is_err() { continue; }
                    if let Some(c) = m.chunks[index].as_mut() {
                        c.data = Some(root);
                        let _ = m.save_with_temp_file(&path);
                    }
                }
                let _ = tx.send(crate::tilemap::TileResult::Image {
                    region: rp, image: None, zoom: 0, structures: None, cache_write: false,
                });
            }));
        cx.notify();
    }

    /// upstream OverlayEditorDialog — the port is modal: rows are applied and
    /// persisted on OK instead of live-editing the TileMap.
    fn edit_overlays(&mut self, cx: &mut Context<Self>) {
        let rows: Vec<OverlayRow> = self
            .map
            .overlays
            .iter()
            .flatten()
            .map(|o| OverlayRow {
                active: o.active,
                typ: o.typ,
                min: o.raw_min.clone(),
                max: o.raw_max.clone(),
                data: o.raw_multi.clone(),
                min_hue: fmt_hue(o.min_hue),
                max_hue: fmt_hue(o.max_hue),
            })
            .collect();
        self.dialog = Some(Dialog::OverlayEditor { rows, focus: None });
        cx.notify();
    }

    fn apply_overlay_editor(&mut self, rows: Vec<OverlayRow>, cx: &mut Context<Self>) {
        let list: Vec<mc_core::overlay::Overlay> = rows.iter().map(overlay_from_row).collect();
        // upstream persists the overlay list in OverlayConfig
        let mut g = config::global();
        g.overlays = list.iter().map(mc_core::overlay::serialize_overlay_line).collect();
        config::set_global(g);
        self.map.set_overlays(list);
        // select the first valid overlay like upstream's initial selection flow:
        // cycling with N activates it
        self.map.set_overlay(None);
        self.request_update();
        cx.notify();
    }

    /// upstream FilterChunksDialog — query field prefilled with the last
    /// query, options mirrored from the dialog's statics.
    fn filter_chunks(&mut self, cx: &mut Context<Self>) {
        self.dialog = Some(Dialog::Filter {
            query: config::global().filter_query,
            action: FilterAction::Select,
            apply: mc_core::filter_ops::ApplyType::Overwrite,
            radius: String::new(),
            sel_only: !self.map.selection.is_empty(),
            focus_radius: false,
        });
        cx.notify();
    }

    /// OK on the filter dialog: select runs SelectFilterJob directly;
    /// delete/export go through the upstream confirmation + dir pick first.
    fn run_filter_dialog(
        &mut self,
        query: String,
        action: FilterAction,
        apply: mc_core::filter_ops::ApplyType,
        radius: String,
        sel_only: bool,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let radius = radius.trim().parse::<i32>().unwrap_or(0).max(0);
        // upstream persists the last query per session; we write it to config
        let mut g = config::global();
        g.filter_query = query.clone();
        config::set_global(g);
        if query.trim().is_empty() {
            // upstream: "filter is empty, won't delete everything"
            self.dialog = Some(Dialog::Error { msg: "empty filter query".into() });
            cx.notify();
            return;
        }
        let expr = match mc_core::filter::parse(&query) {
            Ok(e) => e,
            Err(e) => {
                self.dialog = Some(Dialog::Error { msg: e });
                cx.notify();
                return;
            }
        };
        let sel = self.map.selection.clone();
        let sel_bound = if sel_only && !sel.is_empty() { Some(sel.clone()) } else { None };
        let dirs = crate::config::world().map(|w| w.dirs.clone()).unwrap_or_default();
        match action {
            FilterAction::Select => {
                let mut target = match apply {
                    // upstream clears the selection first for OVERWRITE
                    mc_core::filter_ops::ApplyType::Overwrite => mc_core::selection::Selection::new(),
                    _ => sel.clone(),
                };
                // upstream's "apply to selection only" bounds the scan for
                // every apply type; OVERWRITE only changes the target.
                let bound = sel_bound;
                mc_core::jobs::pools().add_process(mc_core::jobs::Job::new(
                    mc_core::jobs::PRIORITY_LOW, None,
                    move || {
                        let __p = mc_core::jobs::Progress::new("Filtering chunks", -1);
                        let _ = __p;
                        match mc_core::filter_ops::run_filter_select(
                            &expr, &dirs, bound.as_ref(), apply, radius, &mut target,
                        ) {
                            Ok(r) => job_info(format!("{} chunks matched the filter", r.chunks_matched)),
                            Err(e) => job_info(format!("filter failed: {e}")),
                        }
                        *crate::tilemap::FILTER_RESULT.lock().unwrap() = Some(target);
                    }));
            }
            FilterAction::Delete => {
                // upstream DeleteConfirmationDialog with a null tile map
                self.dialog = Some(Dialog::Confirm {
                    msg: "You are about to delete an unknown number of chunks from this world.\nAre you sure?".into(),
                    action: Box::new(move |v, _w, cx| {
                        let dirs = crate::config::world().map(|w| w.dirs.clone()).unwrap_or_default();
                        let inv = all_mca_regions(&dirs);
                        let tx = v.map.result_tx.clone();
                        mc_core::jobs::pools().add_process(mc_core::jobs::Job::new(
                            mc_core::jobs::PRIORITY_LOW, None,
                            move || {
                                let __p = mc_core::jobs::Progress::new("Deleting filtered chunks", -1);
                                let _ = __p;
                                match mc_core::filter_ops::run_filter_delete(&expr, &dirs, sel_bound.as_ref()) {
                                    Ok(r) => job_info(format!(
                                        "deleted {} chunks ({} files written)",
                                        r.chunks_matched, r.files_written
                                    )),
                                    Err(e) => job_info(format!("delete failed: {e}")),
                                }
                                for region in inv {
                                    let _ = tx.send(crate::tilemap::TileResult::Image {
                                        region, image: None, zoom: 0, structures: None, cache_write: false,
                                    });
                                }
                            }));
                        // upstream clears the selection after a filtered delete
                        if sel_only {
                            v.map.clear_selection();
                        }
                        v.dialog = None;
                        cx.notify();
                    }),
                });
            }
            FilterAction::Export => {
                let rx = cx.prompt_for_paths(PathPromptOptions {
                    files: false, directories: true, multiple: false,
                    prompt: Some("Select world directory to export to".into()),
                });
                cx.spawn(async move |this: WeakEntity<MainView>, cx: &mut AsyncApp| {
                    if let Ok(Ok(Some(paths))) = rx.await {
                        if let Some(dir) = paths.into_iter().next() {
                            let _ = this.update(&mut *cx, |v, cx| {
                                // upstream ExportConfirmationDialog (null tile map)
                                v.dialog = Some(Dialog::Confirm {
                                    msg: format!(
                                        "You are about to export an unknown number of chunks from this world.\nto {}\nAre you sure?",
                                        dir.display()
                                    ),
                                    action: Box::new(move |v, _w, cx| {
                                        // upstream FileHelper.createWorldDirectories
                                        let dest = mc_core::mca::WorldDirectories {
                                            region: Some(dir.join("region")),
                                            poi: Some(dir.join("poi")),
                                            entities: Some(dir.join("entities")),
                                        };
                                        for d in [&dest.region, &dest.poi, &dest.entities].into_iter().flatten() {
                                            let _ = std::fs::create_dir_all(d);
                                        }
                                        mc_core::jobs::pools().add_process(mc_core::jobs::Job::new(
                                            mc_core::jobs::PRIORITY_LOW, None,
                                            move || {
                                                let __p = mc_core::jobs::Progress::new("Exporting filtered chunks", -1);
                                                let _ = __p;
                                                match mc_core::filter_ops::run_filter_export(
                                                    &expr, &dirs, sel_bound.as_ref(), &dest,
                                                ) {
                                                    Ok(r) => job_info(format!(
                                                        "exported {} chunks ({} files written)",
                                                        r.chunks_matched, r.files_written
                                                    )),
                                                    Err(e) => job_info(format!("export failed: {e}")),
                                                }
                                            }));
                                        v.dialog = None;
                                        cx.notify();
                                    }),
                                });
                                cx.notify();
                            });
                        }
                    }
                }).detach();
            }
        }
        cx.notify();
    }

    fn render_dialog(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let d = self.dialog.as_ref()?;
        let overlay = div()
            .id("modal-backdrop")
            .absolute()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(rgba(0x00000066));
        let panel = |inner: Div| -> Div {
            v_flex()
                .w(px(480.))
                .bg(rgb(0x2b2b2b))
                .border_1()
                .border_color(rgb(0x555555))
                .rounded_sm()
                .p_4()
                .gap_2()
                .text_color(rgb(0xdddddd))
                .text_size(px(13.))
                .child(inner)
        };
        let body: Div = match d {
            Dialog::Goto { x, z, focus_x, invalid } => {
                let fx = *focus_x;
                let x_c = x.clone();
                let z_c = z.clone();
                let inv = *invalid;
                panel(
                    v_flex().gap_2()
                        .child(text_el("Goto", 16.))
                        .child(
                            h_flex().gap_2()
                                .child(
                                    input_box("x", &x_c, fx)
                                        .on_click(cx.listener(|v, _, _w, cx| {
                                            if let Some(Dialog::Goto { focus_x, .. }) = &mut v.dialog {
                                                *focus_x = true;
                                            }
                                            cx.notify();
                                        })),
                                )
                                .child(
                                    input_box("z", &z_c, !fx)
                                        .on_click(cx.listener(|v, _, _w, cx| {
                                            if let Some(Dialog::Goto { focus_x, .. }) = &mut v.dialog {
                                                *focus_x = false;
                                            }
                                            cx.notify();
                                        })),
                                ),
                        )
                        .child(if inv {
                            warn_el("invalid coordinates", 11.)
                        } else {
                            text_el(" ", 11.)
                        })
                        .child(
                            h_flex().gap_2()
                                .child(button("Go to", cx, |v, _w, cx| {
                                    let mut ok = false;
                                    if let Some(Dialog::Goto { x, z, invalid, .. }) = &mut v.dialog {
                                        if let (Ok(xi), Ok(zi)) = (x.parse::<i32>(), z.parse::<i32>()) {
                                            v.map.go_to(xi, zi);
                                            v.request_update();
                                            ok = true;
                                        } else {
                                            *invalid = true;
                                        }
                                    }
                                    if ok {
                                        v.dialog = None;
                                    }
                                    cx.notify();
                                }))
                                .child(button("Cancel", cx, |v, _w, cx| {
                                    v.dialog = None;
                                    cx.notify();
                                })),
                        ),
                )
            }
            Dialog::About => panel(
                v_flex().gap_2()
                    .child(text_el("About MCA Selector", 16.))
                    .child(text_el(concat!("Version: ", env!("CARGO_PKG_VERSION"), " (Rust + GPUI port)"), 12.))
                    .child(text_el("License: MIT", 12.))
                    .child(text_el("Copyright: © Querz", 12.))
                    .child(text_el("Source: https://github.com/Querz/mcaselector", 12.))
                    .child(text_el("NOT AN OFFICIAL MINECRAFT PRODUCT. NOT APPROVED BY OR ASSOCIATED WITH MOJANG OR MICROSOFT.", 10.))
                    .child(button("Close", cx, |v, _w, cx| {
                        v.dialog = None;
                        cx.notify();
                    })),
            ),
            Dialog::Error { msg } => panel(
                v_flex().gap_2()
                    .child(text_el("Error", 16.))
                    .child(text_el(msg.clone(), 12.))
                    .child(
                        h_flex().gap_2()
                            .child(button("OK", cx, |v, _w, cx| {
                                v.dialog = None;
                                cx.notify();
                            }))
                            .child(button("Copy", cx, |v, _w, cx| {
                                if let Some(Dialog::Error { msg }) = &v.dialog {
                                    cx.write_to_clipboard(ClipboardItem::new_string(msg.clone()));
                                }
                            })),
                    ),
            ),
            Dialog::PasteConfirm { ox, oz, overwrite, sel_only, .. } => panel(
                v_flex().gap_2()
                    .child(text_el(format!("Import chunks at offset ({}, {})", ox, oz), 13.))
                    .child(text_el(format!("[o] overwrite existing: {}", if *overwrite { "on" } else { "off" }), 12.))
                    .child(text_el(format!("[s] selection only: {}", if *sel_only { "on" } else { "off" }), 12.))
                    .child(text_el("(offset = dragged position; enter to import)", 10.))
                    .child(
                        h_flex().gap_2()
                            .child(button("Import", cx, |v, w, cx| {
                                if let Some(Dialog::PasteConfirm { ox, oz, overwrite, sel_only, .. }) = v.dialog.take() {
                                    v.do_paste_import(ox, oz, overwrite, sel_only, cx);
                                }
                                let _ = w;
                            }))
                            .child(button("Cancel", cx, |v, _w, cx| {
                                v.dialog = None;
                                cx.notify();
                            })),
                    ),
            ),
            Dialog::Confirm { msg, .. } => panel(
                v_flex().gap_2()
                    .child(text_el(msg.clone(), 13.))
                    .child(
                        h_flex().gap_2()
                            .child(button("Yes", cx, |v, w, cx| {
                                if let Some(Dialog::Confirm { action, .. }) = v.dialog.take() {
                                    (action)(v, w, cx);
                                }
                                cx.notify();
                            }))
                            .child(button("No", cx, |v, _w, cx| {
                                v.dialog = None;
                                cx.notify();
                            })),
                    ),
            ),
            Dialog::Settings { draft } => panel(
                v_flex().gap_1()
                    .child(text_el("Settings", 16.))
                    .child(text_el(format!("Region selection color: rgba({:.2},{:.2},{:.2},{:.2})", draft.region_selection_color.r, draft.region_selection_color.g, draft.region_selection_color.b, draft.region_selection_color.a), 12.))
                    .child(text_el(format!("Chunk selection color: rgba({:.2},{:.2},{:.2},{:.2})", draft.chunk_selection_color.r, draft.chunk_selection_color.g, draft.chunk_selection_color.b, draft.chunk_selection_color.a), 12.))
                    .child(text_el(format!("Paste chunks color: rgba({:.2},{:.2},{:.2},{:.2})", draft.paste_chunks_color.r, draft.paste_chunks_color.g, draft.paste_chunks_color.b, draft.paste_chunks_color.a), 12.))
                    .child(text_el(format!("Process threads: {}  Write threads: {}  Max loaded files: {}", draft.process_threads, draft.write_threads, draft.max_loaded_files), 12.))
                    .child(text_el(format!("Minecraft saves dir: {}", draft.mc_saves_dir), 12.))
                    .child(
                        h_flex().gap_1().flex_wrap()
                            .child(self.toggle_global("Chunk grid", draft.show_chunk_grid, |d| &mut d.show_chunk_grid, cx))
                            .child(self.toggle_global("Region grid", draft.show_region_grid, |d| &mut d.show_region_grid, cx))
                            .child(self.toggle_global("Coordinates", draft.show_coordinates, |d| &mut d.show_coordinates, cx))
                            .child(self.toggle_global("Debug", draft.debug, |d| &mut d.debug, cx)),
                    )
                    .child(
                        h_flex().gap_2()
                            .child(button("Save", cx, |v, _w, cx| {
                                if let Some(Dialog::Settings { draft }) = v.dialog.take() {
                                    config::set_global(draft);
                                    v.map.mark_all_tiles_obsolete();
                                }
                                cx.notify();
                            }))
                            .child(button("Cancel", cx, |v, _w, cx| {
                                v.dialog = None;
                                cx.notify();
                            })),
                    ),
            ),
            Dialog::RenderSettings { draft } => panel(
                v_flex().gap_1()
                    .child(text_el("Render settings", 16.))
                    .child(text_el(format!("Render height: {}", draft.render_height), 12.))
                    .child(
                        h_flex().gap_1().flex_wrap()
                            .child(self.toggle_render("Shade", draft.shade, |d| &mut d.shade, cx))
                            .child(self.toggle_render("Water", draft.shade_water, |d| &mut d.shade_water, cx))
                            .child(self.toggle_render("Altitude", draft.shade_altitude, |d| &mut d.shade_altitude, cx)),
                    )
                    .child(
                        h_flex().gap_1().flex_wrap()
                            .child(self.toggle_render("Smooth render", draft.smooth_rendering, |d| &mut d.smooth_rendering, cx))
                            .child(self.toggle_render("Smooth overlays", draft.smooth_overlays, |d| &mut d.smooth_overlays, cx)),
                    )
                    .child(
                        h_flex().gap_1().flex_wrap()
                            .child(self.toggle_render("Layer only", draft.render_layer_only, |d| &mut d.render_layer_only, cx))
                            .child(self.toggle_render("Caves", draft.render_caves, |d| &mut d.render_caves, cx))
                            .child(self.toggle_render("Nonexistent", draft.show_nonexistent_regions, |d| &mut d.show_nonexistent_regions, cx)),
                    )
                    .child(text_el(format!("Header-only zoom: {}", draft.render_header_only_zoom_level), 12.))
                    .child(
                        h_flex().gap_2()
                            .child(button("Save", cx, |v, _w, cx| {
                                if let Some(Dialog::RenderSettings { draft }) = v.dialog.take() {
                                    if let Some(mut ws) = config::world() {
                                        ws.cfg = draft;
                                        ws.cfg.save(&ws.cache_dir);
                                        config::set_world(Some(ws));
                                    }
                                    v.clear_all_caches(cx);
                                }
                                cx.notify();
                            }))
                            .child(button("Cancel", cx, |v, _w, cx| {
                                v.dialog = None;
                                cx.notify();
                            })),
                    ),
            ),
            Dialog::SelectWorld { worlds } => {
                let mut list = v_flex().gap_1().child(text_el("Open recent world", 16.));
                for w in worlds {
                    let path = w.clone();
                    list = list.child(
                        div()
                            .id(SharedString::from(path.display().to_string()))
                            .px_2().py_1().rounded_sm()
                            .hover(|s| s.bg(rgb(0x3a3a3a)))
                            .child(text_el(path.display().to_string(), 12.))
                            .on_click(cx.listener(move |v, _, _w, cx| {
                                v.open_world_at(path.clone(), cx);
                                v.dialog = None;
                            })),
                    );
                }
                panel(list.child(button("Cancel", cx, |v, _w, cx| {
                    v.dialog = None;
                    cx.notify();
                })))
            }
            Dialog::SelectDimension { dims } => {
                let mut list = v_flex().gap_1().child(text_el("Select dimension", 16.));
                for d in dims {
                    let label = d.display().to_string();
                    let dp = d.clone();
                    list = list.child(
                        div()
                            .id(SharedString::from(label.clone()))
                            .px_2().py_1().rounded_sm()
                            .hover(|s| s.bg(rgb(0x3a3a3a)))
                            .child(text_el(label, 12.))
                            .on_click(cx.listener(move |v, _, _w, cx| {
                                let dirs = files::detect_world_directories(&dp);
                                if dirs.region.is_some() {
                                    let dims2 = files::detect_dimension_directories(&dp);
                                    let ws = config::open_world(dirs, if dims2.is_empty() { vec![dp.clone()] } else { dims2 });
                                    v.map.clear();
                                    v.map.regions = files::scan_region_files(ws.region_dir());
                                    v.map.disabled = false;
                                    v.request_update();
                                }
                                v.dialog = None;
                                cx.notify();
                            })),
                    );
                }
                panel(list.child(button("Cancel", cx, |v, _w, cx| {
                    v.dialog = None;
                    cx.notify();
                })))
            }
            Dialog::NbtEditor {
                tab, tabs, editing, buf, status, chunk_abs, ..
            } => {
                const KINDS: [&str; 3] = ["region", "poi", "entities"];
                let cur_tab = *tab;
                let t = &tabs[cur_tab];
                let mut tab_row = h_flex().gap_1();
                for (i, name) in KINDS.iter().enumerate() {
                    let dirty = tabs[i].dirty;
                    tab_row = tab_row.child(radio_btn(
                        SharedString::from(format!("nbt-tab-{name}")),
                        SharedString::from(format!("{name}{}", if dirty { " *" } else { "" })),
                        i == cur_tab,
                        cx,
                        move |v, _w, cx| {
                            if let Some(Dialog::NbtEditor { tab, editing, buf, .. }) = &mut v.dialog {
                                *tab = i;
                                *editing = NbtEdit::None;
                                buf.clear();
                            }
                            cx.notify();
                        },
                    ));
                }
                let t_sel = t.sel;
                let rows = t.root.as_ref()
                    .map(|r| nbt_editor::flatten(r, &t.expanded))
                    .unwrap_or_default();
                let n = rows.len();
                let vis = 22usize;
                let start = if t_sel >= vis { t_sel + 1 - vis } else { 0 };
                let end = (start + vis).min(n);
                let mut list = v_flex()
                    .h(px(vis as f32 * 20.))
                    .overflow_hidden()
                    .gap(px(1.));
                for (i, r) in rows[start..end].iter().enumerate() {
                    let idx = start + i;
                    let selected = idx == t_sel;
                    let indent = r.depth as f32 * 14.0;
                    let mut row = div()
                        .id(("row", idx))
                        .flex()
                        .items_center()
                        .pl(px(4. + indent))
                        .h(px(20.))
                        .text_size(px(12.))
                        .bg(if selected { rgb(0x3a5a8a) } else { rgba(0x00000000) })
                        .on_mouse_down(MouseButton::Left, cx.listener(move |v, _e: &MouseDownEvent, _w, cx| {
                            cx.stop_propagation();
                            if let Some(Dialog::NbtEditor { tab, tabs, .. }) = &mut v.dialog {
                                tabs[*tab].sel = idx;
                            }
                            cx.notify();
                        }));
                    if r.container {
                        row = row.child(text_el(if r.expanded { "▾ " } else { "▸ " }, 11.));
                    } else {
                        row = row.child(text_el("  ", 11.));
                    }
                    if let Some(ic) = nbt_icon(r.type_id) {
                        row = row.child(
                            img(ImageSource::Render(ic))
                                .size(px(12.))
                                .mr(px(3.)),
                        );
                    }
                    let label = if r.value.is_empty() {
                        r.name.clone()
                    } else if r.name.starts_with('[') {
                        format!("{} {}", r.name, r.value)
                    } else {
                        format!("{}: {}", r.name, r.value)
                    };
                    row = row.child(
                        div()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .child(label),
                    );
                    list = list.child(row);
                }
                let edit_line = if *editing != NbtEdit::None {
                    let hint = match editing {
                        NbtEdit::Value => "value (snbt)",
                        NbtEdit::Add => "new tag: name = value",
                        _ => "new name",
                    };
                    v_flex().gap_1()
                        .child(text_el(hint, 10.))
                        .child(div().w_full().min_w(px(560.)).child(input_box("nbt-edit", buf, true)))
                        .into_any_element()
                } else {
                    div().h(px(0.)).into_any_element()
                };
                v_flex()
                    .w(px(700.))
                    .bg(rgb(0x2b2b2b))
                    .border_1()
                    .border_color(rgb(0x555555))
                    .rounded_sm()
                    .p_4()
                    .gap_2()
                    .text_color(rgb(0xdddddd))
                    .child(text_el(
                        format!("NBT editor — chunk {}, {} {}", chunk_abs.x, chunk_abs.z, if t.dirty { "(modified)" } else { "" }),
                        15.,
                    ))
                    .child(tab_row)
                    .child(if t.root.is_some() {
                        list.into_any_element()
                    } else {
                        // upstream placeholder when the file/chunk is missing
                        v_flex()
                            .h(px(22. * 20.))
                            .child(text_el(
                                t.note.clone().unwrap_or_else(|| "no chunk data found".into()),
                                12.,
                            ))
                            .into_any_element()
                    })
                    .child(edit_line)
                    .child(text_el(status.clone(), 10.))
                    .child(
                        h_flex().gap_2()
                            .child(button("Save", cx, |v, _w, cx| v.nbt_editor_save(cx)))
                            .child(button("Close", cx, |v, _w, cx| {
                                v.dialog = None;
                                cx.notify();
                            })),
                    )
            }
            Dialog::Info { msg } => panel(
                v_flex().gap_2()
                    .child(text_el(msg.clone(), 14.))
                    .child(
                        h_flex().gap_2()
                            .child(button("OK", cx, |v, _w, cx| {
                                v.dialog = None;
                                cx.notify();
                            }))
                            .child(button("Copy", cx, |v, _w, cx| {
                                if let Some(Dialog::Info { msg }) = &v.dialog {
                                    cx.write_to_clipboard(ClipboardItem::new_string(msg.clone()));
                                }
                            })),
                    ),
            ),
            Dialog::JobProgress => {
                let pr = mc_core::jobs::current_progress();
                let (title, frac, cur) = match &pr {
                    Some(p) => {
                        (p.title.clone(), p.fraction(), p.current.lock().clone())
                    }
                    None => ("Working".to_string(), -1.0, String::new()),
                };
                let pct = if frac < 0.0 { "".to_string() } else { format!(" {:.0}%", frac * 100.0) };
                let bar_w = 280.0_f32;
                let fill_w = if frac < 0.0 { 0.0 } else { bar_w * frac };
                panel(
                    v_flex().gap_2()
                        .child(text_el(format!("{title}{pct}"), 14.))
                        .child(
                            div().w(px(bar_w)).h(px(10.)).bg(rgb(0x2a2a2a))
                                .child(div().w(px(fill_w)).h_full().bg(rgb(0x3a7bd5)))
                        )
                        .child(text_el(cur, 10.))
                        .child(button("Cancel", cx, |v, _w, cx| {
                            mc_core::jobs::cancel_progress();
                            v.dialog = None;
                            cx.notify();
                        })),
                )
            }
            Dialog::TextInput { title, hint, text, .. } => panel(
                v_flex().gap_2()
                    .child(text_el(title.clone(), 16.))
                    .child(text_el(hint.clone(), 11.))
                    .child(input_box("input", text, true))
                    .child(
                        h_flex().gap_2()
                            .child(button("OK", cx, |v, w, cx| {
                                if let Some(Dialog::TextInput { text, on_done, .. }) = v.dialog.take() {
                                    (on_done)(v, w, cx, text);
                                }
                                cx.notify();
                            }))
                            .child(button("Cancel", cx, |v, _w, cx| {
                                v.dialog = None;
                                cx.notify();
                            })),
                    ),
            ),
            Dialog::Filter { query, action, apply, radius, sel_only, focus_radius } => {
                use mc_core::filter_ops::ApplyType;
                let q = query.clone();
                let fr = *focus_radius;
                let act = *action;
                let ap = *apply;
                let so = *sel_only;
                // live-validate the query like upstream's OK-disable
                let parse_err = if q.trim().is_empty() {
                    Some("empty query".to_string())
                } else {
                    mc_core::filter::parse(&q).err()
                };
                let ok_enabled = parse_err.is_none();
                let opts = if act == FilterAction::Select {
                    h_flex().gap_1()
                        .child(radio_btn("fa-overwrite", "Overwrite", ap == ApplyType::Overwrite, cx, |v, _w, cx| {
                            if let Some(Dialog::Filter { apply, .. }) = &mut v.dialog { *apply = ApplyType::Overwrite; }
                            cx.notify();
                        }))
                        .child(radio_btn("fa-add", "Add to selection", ap == ApplyType::Add, cx, |v, _w, cx| {
                            if let Some(Dialog::Filter { apply, .. }) = &mut v.dialog { *apply = ApplyType::Add; }
                            cx.notify();
                        }))
                        .child(radio_btn("fa-subtract", "Subtract from selection", ap == ApplyType::Subtract, cx, |v, _w, cx| {
                            if let Some(Dialog::Filter { apply, .. }) = &mut v.dialog { *apply = ApplyType::Subtract; }
                            cx.notify();
                        }))
                        .child(text_el("  radius", 11.))
                        .child(
                            input_box_w("filter-radius", radius, fr, 56.)
                                .on_click(cx.listener(|v, _, _w, cx| {
                                    if let Some(Dialog::Filter { focus_radius, .. }) = &mut v.dialog {
                                        *focus_radius = true;
                                    }
                                    cx.notify();
                                })),
                        )
                        .into_any_element()
                } else {
                    div().h(px(0.)).into_any_element()
                };
                panel(
                    v_flex().gap_2()
                        .child(text_el("Filter chunks", 16.))
                        .child(
                            input_box_w("filter-query", &q, !fr, 440.)
                                .on_click(cx.listener(|v, _, _w, cx| {
                                    if let Some(Dialog::Filter { focus_radius, .. }) = &mut v.dialog {
                                        *focus_radius = false;
                                    }
                                    cx.notify();
                                })),
                        )
                        .child(text_el("fields: xPos zPos yPos DataVersion InhabitedTime LastUpdate Timestamp Status Palette Biome Compression Entities #Entities #ProtoEntities #TileEntities Structures Circle Border Selection PlayerLocation PlayerSpawn LightPopulated <nbt.path>", 9.))
                        .child(
                            h_flex().gap_1()
                                .child(radio_btn("ft-select", "Select chunks", act == FilterAction::Select, cx, |v, _w, cx| {
                                    if let Some(Dialog::Filter { action, .. }) = &mut v.dialog { *action = FilterAction::Select; }
                                    cx.notify();
                                }))
                                .child(radio_btn("ft-export", "Export chunks", act == FilterAction::Export, cx, |v, _w, cx| {
                                    if let Some(Dialog::Filter { action, .. }) = &mut v.dialog { *action = FilterAction::Export; }
                                    cx.notify();
                                }))
                                .child(radio_btn("ft-delete", "Delete chunks", act == FilterAction::Delete, cx, |v, _w, cx| {
                                    if let Some(Dialog::Filter { action, .. }) = &mut v.dialog { *action = FilterAction::Delete; }
                                    cx.notify();
                                })),
                        )
                        .child(opts)
                        .child(opt_toggle("filter-sel-only", "Apply to selection only", so, cx, |v, _w, cx| {
                            if let Some(Dialog::Filter { sel_only, .. }) = &mut v.dialog { *sel_only = !*sel_only; }
                            cx.notify();
                        }))
                        .child(match parse_err {
                            Some(e) => warn_el(e, 10.),
                            None => text_el(" ", 10.),
                        })
                        .child(
                            h_flex().gap_2()
                                .child(button("OK", cx, move |v, w, cx| {
                                    // upstream disables OK on an invalid filter
                                    if ok_enabled {
                                        if let Some(Dialog::Filter { query, action, apply, radius, sel_only, .. }) = v.dialog.take() {
                                            v.run_filter_dialog(query, action, apply, radius, sel_only, w, cx);
                                        }
                                    }
                                    cx.notify();
                                }))
                                .child(button("Cancel", cx, |v, _w, cx| {
                                    v.dialog = None;
                                    cx.notify();
                                })),
                        ),
                )
            }
            Dialog::ChangeFields { text, force, sel_only } => {
                let parse = mc_core::changer::parse_fields(text);
                let status = match &parse {
                    Ok(f) => format!("{} field(s) parsed", f.len()),
                    Err(e) => e.clone(),
                };
                let ok_enabled = matches!(&parse, Ok(f) if !f.is_empty());
                let names = mc_core::changer::FieldType::VALUES
                    .iter()
                    .map(|t| if t.requires_clear_cache() { format!("{}*", t.name()) } else { t.name().to_string() })
                    .collect::<Vec<_>>()
                    .join(", ");
                panel(
                    v_flex().gap_2()
                        .child(text_el("Change NBT", 16.))
                        .child(text_el("Type = value, comma separated  e.g. Status = minecraft:full, InhabitedTime = 0", 10.))
                        .child(input_box_w("cf-text", text, true, 440.))
                        .child(text_el(format!("fields: {names}"), 9.))
                        .child(text_el("(* clears render cache)", 9.))
                        .child(
                            h_flex().gap_1()
                                .child(radio_btn("cf-change", "Change", !*force, cx, |v, _w, cx| {
                                    if let Some(Dialog::ChangeFields { force, .. }) = &mut v.dialog { *force = false; }
                                    cx.notify();
                                }))
                                .child(radio_btn("cf-force", "Force", *force, cx, |v, _w, cx| {
                                    if let Some(Dialog::ChangeFields { force, .. }) = &mut v.dialog { *force = true; }
                                    cx.notify();
                                }))
                                .child(text_el("  force creates the tag if missing", 10.)),
                        )
                        .child(opt_toggle("cf-sel-only", "Apply to selection only", *sel_only, cx, |v, _w, cx| {
                            if let Some(Dialog::ChangeFields { sel_only, .. }) = &mut v.dialog { *sel_only = !*sel_only; }
                            cx.notify();
                        }))
                        .child(if parse.is_err() { warn_el(status, 10.) } else { text_el(status, 10.) })
                        .child(
                            h_flex().gap_2()
                                .child(button("OK", cx, move |v, _w, cx| {
                                    // upstream disables OK on invalid input
                                    if ok_enabled {
                                        if let Some(Dialog::ChangeFields { text, force, sel_only, .. }) = v.dialog.take() {
                                            v.run_change_fields_dialog(text, force, sel_only, cx);
                                        }
                                    }
                                    cx.notify();
                                }))
                                .child(button("Cancel", cx, |v, _w, cx| {
                                    v.dialog = None;
                                    cx.notify();
                                })),
                        ),
                )
            }
            Dialog::Import { dir, ox, oz, overwrite, sel_only, focus_x, invalid } => {
                let fx = *focus_x;
                let ox_c = ox.clone();
                let oz_c = oz.clone();
                panel(
                    v_flex().gap_2()
                        .child(text_el("Import chunks", 16.))
                        .child(text_el("You are about to import an unknown number of chunks to this world.", 12.))
                        .child(text_el(format!("from {}", dir.display()), 10.))
                        .child(
                            h_flex().gap_2()
                                .child(text_el("offset x", 11.))
                                .child(
                                    input_box("import-ox", &ox_c, fx)
                                        .on_click(cx.listener(|v, _, _w, cx| {
                                            if let Some(Dialog::Import { focus_x, .. }) = &mut v.dialog { *focus_x = true; }
                                            cx.notify();
                                        })),
                                )
                                .child(text_el("z", 11.))
                                .child(
                                    input_box("import-oz", &oz_c, !fx)
                                        .on_click(cx.listener(|v, _, _w, cx| {
                                            if let Some(Dialog::Import { focus_x, .. }) = &mut v.dialog { *focus_x = false; }
                                            cx.notify();
                                        })),
                                ),
                        )
                        .child(opt_toggle("import-overwrite", "[o] overwrite existing chunks", *overwrite, cx, |v, _w, cx| {
                            if let Some(Dialog::Import { overwrite, .. }) = &mut v.dialog { *overwrite = !*overwrite; }
                            cx.notify();
                        }))
                        .child(opt_toggle("import-sel-only", "[s] apply to selection only", *sel_only, cx, |v, _w, cx| {
                            if let Some(Dialog::Import { sel_only, .. }) = &mut v.dialog { *sel_only = !*sel_only; }
                            cx.notify();
                        }))
                        .child(if *invalid {
                            warn_el("invalid offset", 11.)
                        } else {
                            text_el("Are you sure?", 11.)
                        })
                        .child(
                            h_flex().gap_2()
                                .child(button("Import", cx, |v, _w, cx| {
                                    let mut done = false;
                                    if let Some(Dialog::Import { ox, oz, invalid, .. }) = &mut v.dialog {
                                        if ox.trim().parse::<i32>().is_ok() && oz.trim().parse::<i32>().is_ok() {
                                            done = true;
                                        } else {
                                            *invalid = true;
                                        }
                                    }
                                    if done {
                                        if let Some(Dialog::Import { dir, ox, oz, overwrite, sel_only, .. }) = v.dialog.take() {
                                            v.do_import(dir, ox.trim().parse().unwrap_or(0), oz.trim().parse().unwrap_or(0), overwrite, sel_only, cx);
                                        }
                                    }
                                    cx.notify();
                                }))
                                .child(button("Cancel", cx, |v, _w, cx| {
                                    v.dialog = None;
                                    cx.notify();
                                })),
                        ),
                )
            }
            Dialog::OverlayEditor { rows, focus } => {
                let mut list = v_flex().gap_1();
                for (i, r) in rows.iter().enumerate() {
                    let focused = |f: OverlayField| *focus == Some((i, f));
                    let active = r.active;
                    let typ = r.typ;
                    let rm = r.min.clone();
                    let rx = r.max.clone();
                    let rd = r.data.clone();
                    let rminh = r.min_hue.clone();
                    let rmaxh = r.max_hue.clone();
                    list = list.child(
                        h_flex().gap_1()
                            .child(opt_toggle(
                                SharedString::from(format!("ov-active-{i}")),
                                "on", active, cx,
                                move |v, _w, cx| {
                                    if let Some(Dialog::OverlayEditor { rows, .. }) = &mut v.dialog {
                                        if let Some(r) = rows.get_mut(i) { r.active = !r.active; }
                                    }
                                    cx.notify();
                                },
                            ))
                            .child(radio_btn(
                                SharedString::from(format!("ov-type-{i}")),
                                SharedString::from(format!("{:<17}", typ.display_name())),
                                false, cx,
                                move |v, _w, cx| {
                                    // upstream combo box; click cycles the type list
                                    if let Some(Dialog::OverlayEditor { rows, .. }) = &mut v.dialog {
                                        if let Some(r) = rows.get_mut(i) {
                                            let vals = mc_core::overlay::OverlayType::VALUES;
                                            let cur = vals.iter().position(|t| *t == r.typ).unwrap_or(0);
                                            r.typ = vals[(cur + 1) % vals.len()];
                                        }
                                    }
                                    cx.notify();
                                },
                            ))
                            .child(ov_text_field(cx, i, OverlayField::Min, &rm, focused(OverlayField::Min), 74., format!("ov-min-{i}")))
                            .child(ov_text_field(cx, i, OverlayField::Max, &rx, focused(OverlayField::Max), 74., format!("ov-max-{i}")))
                            .child(ov_text_field(cx, i, OverlayField::Data, &rd, focused(OverlayField::Data), 170., format!("ov-data-{i}")))
                            .child(ov_text_field(cx, i, OverlayField::MinHue, &rminh, focused(OverlayField::MinHue), 56., format!("ov-minh-{i}")))
                            .child(ov_text_field(cx, i, OverlayField::MaxHue, &rmaxh, focused(OverlayField::MaxHue), 56., format!("ov-maxh-{i}")))
                            .child(radio_btn(
                                SharedString::from(format!("ov-flip-{i}")),
                                "⇄", false, cx,
                                move |v, _w, cx| {
                                    // upstream flip icon swaps the hue range
                                    if let Some(Dialog::OverlayEditor { rows, .. }) = &mut v.dialog {
                                        if let Some(r) = rows.get_mut(i) {
                                            std::mem::swap(&mut r.min_hue, &mut r.max_hue);
                                        }
                                    }
                                    cx.notify();
                                },
                            ))
                            .child(radio_btn(
                                SharedString::from(format!("ov-del-{i}")),
                                "x", false, cx,
                                move |v, _w, cx| {
                                    if let Some(Dialog::OverlayEditor { rows, focus, .. }) = &mut v.dialog {
                                        if i < rows.len() { rows.remove(i); }
                                        *focus = None;
                                    }
                                    cx.notify();
                                },
                            )),
                    );
                }
                v_flex()
                    .w(px(760.))
                    .bg(rgb(0x2b2b2b))
                    .border_1()
                    .border_color(rgb(0x555555))
                    .rounded_sm()
                    .p_4()
                    .gap_2()
                    .text_color(rgb(0xdddddd))
                    .child(text_el("Edit overlays", 16.))
                    .child(text_el("on | type | min | max | data | minHue | maxHue | flip | delete  —  [tab] next field, [a] add row", 9.))
                    .child(list)
                    .child(
                        h_flex().gap_2()
                            .child(button("Add overlay", cx, |v, _w, cx| {
                                if let Some(Dialog::OverlayEditor { rows, .. }) = &mut v.dialog {
                                    rows.push(default_overlay_row());
                                }
                                cx.notify();
                            }))
                            .child(button("OK", cx, |v, _w, cx| {
                                if let Some(Dialog::OverlayEditor { rows, .. }) = v.dialog.take() {
                                    v.apply_overlay_editor(rows, cx);
                                }
                                cx.notify();
                            }))
                            .child(button("Cancel", cx, |v, _w, cx| {
                                v.dialog = None;
                                cx.notify();
                            })),
                    )
            }
        };
        Some(
            overlay
                .on_mouse_down(MouseButton::Left, cx.listener(|v, e, _w, cx| {
                    let _ = e;
                    match v.dialog {
                        Some(Dialog::Confirm { .. }) | Some(Dialog::Info { .. }) | Some(Dialog::Goto { .. })
                            | Some(Dialog::JobProgress) | Some(Dialog::NbtEditor { .. }) | Some(Dialog::TextInput { .. })
                            | Some(Dialog::Filter { .. }) | Some(Dialog::ChangeFields { .. })
                            | Some(Dialog::Import { .. }) | Some(Dialog::OverlayEditor { .. })
                            | Some(Dialog::PasteConfirm { .. }) => {}
                        _ => v.dialog = None,
                    }
                    cx.notify();
                }))
                .child(body)
                .into_any_element(),
        )
    }
}

fn text_el(s: impl Into<SharedString>, size: f32) -> Div {
    div().text_size(px(size)).child(s.into())
}

/// error/hint text rendered in red.
fn warn_el(s: impl Into<SharedString>, size: f32) -> Div {
    div().text_size(px(size)).text_color(rgb(0xff5555)).child(s.into())
}

/// checkbox-style toggle button (same style as toggle_render/toggle_global)
/// with a free-form click action and dynamic label.
fn opt_toggle(
    id: impl Into<ElementId>,
    label: impl Into<String>,
    val: bool,
    cx: &mut Context<MainView>,
    on: impl Fn(&mut MainView, &mut Window, &mut Context<MainView>) + 'static,
) -> Stateful<Div> {
    let label = label.into();
    div()
        .id(id)
        .px_2().py_1()
        .bg(if val { rgb(0x2a4a2a) } else { rgb(0x3c3f41) })
        .border_1().border_color(rgb(0x555555))
        .rounded_sm()
        .cursor_pointer()
        .text_size(px(12.))
        .child(format!("{}: {}", label, if val { "on" } else { "off" }))
        .on_click(cx.listener(move |v, _, w, cx| (on)(v, w, cx)))
}

/// radio-button style selector (highlighted when `selected`).
fn radio_btn(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    selected: bool,
    cx: &mut Context<MainView>,
    on: impl Fn(&mut MainView, &mut Window, &mut Context<MainView>) + 'static,
) -> Stateful<Div> {
    div()
        .id(id)
        .px_2().py_1()
        .bg(if selected { rgb(0x2a4a6a) } else { rgb(0x3c3f41) })
        .border_1().border_color(rgb(0x555555))
        .rounded_sm()
        .cursor_pointer()
        .text_size(px(12.))
        .child(label.into())
        .on_click(cx.listener(move |v, _, w, cx| (on)(v, w, cx)))
}

/// one text field of the overlay editor — a free fn so each call's `cx`
/// borrow ends immediately (the closure form kept `*cx` borrowed).
fn ov_text_field(
    cx: &mut Context<MainView>,
    i: usize,
    f: OverlayField,
    val: &str,
    focused: bool,
    w: f32,
    fid: String,
) -> Stateful<Div> {
    input_box_w(SharedString::from(fid), val, focused, w)
        .on_click(cx.listener(move |v, _, _w, cx| {
            if let Some(Dialog::OverlayEditor { focus, .. }) = &mut v.dialog {
                *focus = Some((i, f));
            }
            cx.notify();
        }))
}

fn input_box(id: impl Into<ElementId>, value: &str, focused: bool) -> Stateful<Div> {
    input_box_w(id, value, focused, 200.)
}

fn input_box_w(id: impl Into<ElementId>, value: &str, focused: bool, width: f32) -> Stateful<Div> {
    div()
        .id(id)
        .w(px(width))
        .h(px(26.))
        .bg(rgb(0x1e1e1e))
        .border_1()
        .border_color(if focused { rgb(0x4a9eff) } else { rgb(0x555555) })
        .rounded_sm()
        .px_2()
        .flex()
        .items_center()
        .text_color(rgb(0xffffff))
        .text_size(px(12.))
        .child(SharedString::from(if value.is_empty() { " ".to_string() } else { value.to_string() }))
}

fn button(
    label: &'static str,
    cx: &mut Context<MainView>,
    on: impl Fn(&mut MainView, &mut Window, &mut Context<MainView>) + 'static,
) -> Stateful<Div> {
    div()
        .id(SharedString::from(label.to_string()))
        .px_3()
        .py_1()
        .bg(rgb(0x3c3f41))
        .border_1()
        .border_color(rgb(0x555555))
        .rounded_sm()
        .hover(|s| s.bg(rgb(0x4a4d4f)))
        .cursor_pointer()
        .text_size(px(12.))
        .text_color(rgb(0xffffff))
        .child(label)
        .on_click(cx.listener(move |v, _, w, cx| (on)(v, w, cx)))
}

fn v_flex() -> Div { div().flex().flex_col() }
fn h_flex() -> Div { div().flex() }

/// all region locations present in any of the three world directories
/// (region/poi/entities) — used to invalidate tiles after jobs that rewrite
/// files outside a per-region callback.
fn all_mca_regions(dirs: &mc_core::mca::WorldDirectories) -> Vec<Point2i> {
    let mut locs = std::collections::BTreeSet::new();
    for d in [&dirs.region, &dirs.poi, &dirs.entities].into_iter().flatten() {
        locs.extend(files::scan_region_files(d));
    }
    locs.into_iter().map(Point2i::from_long).collect()
}

fn fmt_hue(h: f32) -> String {
    let s = format!("{h:.4}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

fn default_overlay_row() -> OverlayRow {
    OverlayRow {
        active: true,
        typ: mc_core::overlay::OverlayType::InhabitedTime,
        min: String::new(),
        max: String::new(),
        data: String::new(),
        min_hue: fmt_hue(0.666_666_7),
        max_hue: fmt_hue(0.0),
    }
}

fn overlay_field(row: &mut OverlayRow, f: OverlayField) -> &mut String {
    match f {
        OverlayField::Min => &mut row.min,
        OverlayField::Max => &mut row.max,
        OverlayField::Data => &mut row.data,
        OverlayField::MinHue => &mut row.min_hue,
        OverlayField::MaxHue => &mut row.max_hue,
    }
}

/// build an Overlay from an editor row — invalid bounds are kept (upstream
/// stores invalid overlays too; they just don't draw).
fn overlay_from_row(r: &OverlayRow) -> mc_core::overlay::Overlay {
    let mut o = mc_core::overlay::Overlay::new(r.typ);
    o.set_min_str(r.min.trim());
    o.set_max_str(r.max.trim());
    o.set_multi_values_str(r.data.trim());
    if let Ok(h) = r.min_hue.trim().parse::<f32>() {
        o.min_hue = h;
    }
    if let Ok(h) = r.max_hue.trim().parse::<f32>() {
        o.max_hue = h;
    }
    o.active = r.active;
    o
}

/// upstream ImportJob: merge every mca file of `src_dir` into the open
/// world's region dir, shifting chunk positions by `offset` (chunks).
/// `src_sel` limits which source chunks are imported (paste imports pass the
/// copied selection); `target_sel` masks the destination (selection only).
/// Chunks crossing region borders are split by target region.
fn import_mca_dir(
    src_dir: PathBuf,
    src_sel: Option<mc_core::selection::Selection>,
    offset: Point2i,
    overwrite: bool,
    target_sel: Option<mc_core::selection::Selection>,
    tx: std::sync::mpsc::Sender<crate::tilemap::TileResult>,
) {
    mc_core::jobs::pools().add_process(mc_core::jobs::Job::new(
        mc_core::jobs::PRIORITY_LOW, None,
        move || {
            let Ok(rd) = std::fs::read_dir(&src_dir) else { return };
            let files: Vec<_> = rd.flatten().collect();
            let __p = mc_core::jobs::Progress::new("Importing chunks", files.len() as i64);
            for e in files {
                let __g = __p.inc_guard();
                if __p.is_cancelled() { break; }
                let name = e.file_name().to_string_lossy().to_string();
                __p.set_current(name.clone());
                let Some(src_region) = mc_core::files::parse_mca_file_name(&name) else { continue };
                let Some(dst_dir) = crate::config::world().and_then(|w| w.dirs.region.clone()) else { return };
                let Ok(mut src) = mc_core::mca::McaFile::new(e.path()) else { continue };
                if src.load(false).is_err() { continue; }
                // chunks to import from this region (Selection::selected_chunks
                // handles inverted selections; None = no selection -> whole file)
                let sel_cs = match &src_sel {
                    None => { let mut f = mc_core::selection::ChunkSet::new(); f.fill(); f }
                    Some(s) => match s.selected_chunks(src_region) {
                        Some(Some(cs)) => cs,
                        Some(None) => { let mut f = mc_core::selection::ChunkSet::new(); f.fill(); f }
                        None => continue,
                    },
                };
                if sel_cs.is_empty() { continue; }
                // split by target region (chunks may cross region borders)
                let mut targets: std::collections::HashMap<mc_core::point::Point2i, Vec<(usize, mc_core::point::Point2i)>> = std::collections::HashMap::new();
                for i in sel_cs.iter() {
                    let c = src_region.region_to_chunk().add(i as i32 % 32, i as i32 / 32).add_p(offset);
                    if let Some(ts) = &target_sel {
                        if !ts.is_chunk_selected(c) { continue; }
                    }
                    targets.entry(c.chunk_to_region()).or_default().push((i, c));
                }
                for (tregion, chunks) in targets {
                    let dst = dst_dir.join(mc_core::files::mca_file_name(tregion));
                    let mut d = if dst.exists() {
                        let Ok(mut m) = mc_core::mca::McaFile::new(dst.clone()) else { continue };
                        if m.load(false).is_err() { continue; }
                        m
                    } else {
                        mc_core::mca::McaFile::empty(tregion, dst.clone())
                    };
                    let trel = tregion.region_to_chunk();
                    let mut changed = false;
                    for (i, c) in chunks {
                        if let Some(chunk) = &src.chunks[i] {
                            let rel = c.sub(trel);
                            if rel.x < 0 || rel.x > 31 || rel.z < 0 || rel.z > 31 { continue; }
                            let di = (rel.z * 32 + rel.x) as usize;
                            if d.chunks[di].is_some() && !overwrite { continue; }
                            let mut cc = chunk.clone();
                            cc.absolute_location = c;
                            cc.timestamp = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|t| t.as_secs() as i32).unwrap_or(0);
                            d.chunks[di] = Some(cc);
                            changed = true;
                        }
                    }
                    if changed {
                        let _ = d.save_with_temp_file(&dst);
                        let _ = tx.send(crate::tilemap::TileResult::Image {
                            region: tregion, image: None, zoom: 0, structures: None, cache_write: false,
                        });
                    }
                }
            }
        }));
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
