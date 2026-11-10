//! Main window view: TileMap canvas + status bar + dialogs.
//! Mirrors Window/TileMapBox/StatusBar/OptionBar behavior.

use crate::actions::*;
use crate::config::{self};
use crate::tilemap::*;
use gpui::*;
use mc_core::files;
use crate::nbt_editor;
use mc_core::point::{Point2f, Point2i};
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

pub enum Dialog {
    Goto { x: String, z: String, focus_x: bool },
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
        root: mc_core::nbt::Compound,
        expanded: std::collections::HashSet<String>,
        sel: usize,
        /// 0 = none, 1 = value edit, 2 = add child, 3 = rename
        editing: NbtEdit,
        buf: String,
        status: String,
        dirty: bool,
    },
    TextInput {
        title: String,
        hint: String,
        text: String,
        on_done: Box<dyn Fn(&mut MainView, &mut Window, &mut Context<MainView>, String) + 'static>,
    },
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
    icon_cache: std::collections::HashMap<String, Arc<RenderImage>>,
    last_frame: std::time::Instant,
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
            icon_cache: std::collections::HashMap::new(),
            last_frame: std::time::Instant::now(),
        };
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

    fn poll_tick(&mut self, cx: &mut Context<Self>) {
        if let Some(sum) = self.map.take_sum_result() {
            self.dialog = Some(Dialog::Info { msg: format!("sum: {sum}") });
        }
        if let Some(sel) = self.map.take_filter_result() {
            let n = sel.count();
            self.map.selection = sel;
            self.map.selected_chunks = n as i64;
            self.map.unsaved_selection = true;
            self.map.selection_version += 1;
            self.dialog = Some(Dialog::Info { msg: format!("filter matched {n} chunks") });
        }
        let changed = self.map.poll_results();
        if self.update_needed || changed {
            self.map.update();
            self.update_needed = false;
        }
        let busy = mc_core::jobs::current_progress().is_some();
        if busy && self.dialog.is_none() {
            self.dialog = Some(Dialog::JobProgress);
        } else if !busy && matches!(self.dialog, Some(Dialog::JobProgress)) {
            self.dialog = None;
        }
        if self.pressed_keys != 0 || self.drag.is_move() || busy
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
        self.update_needed = true;
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
        self.update_needed = true;
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
                // request fresh-resolution tiles now, not on the next 100ms poll.
                self.map.update();
                self.update_needed = true;
            }
            cx.notify();
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
                if self.map.pasted.is_some() {
                    self.map.cancel_paste();
                    cx.notify();
                    return;
                }
                self.drag = Drag::RightSelect;
            }
            MouseButton::Middle => self.drag = Drag::MiddleMove,
            _ => {}
        }
        self.first = pos;
        self.previous = pos;
        cx.notify();
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let pos = Point2f::new(f32::from(event.position.x), f32::from(event.position.y));
        self.map.hovered_block = Some(self.map.mouse_block(pos.x, pos.z));
        if self.map.disabled || self.dialog.is_some() {
            self.previous = pos;
            cx.notify();
            return;
        }
        let pressed = event.pressed_button;
        if self.drag == Drag::PasteMove {
            self.map.drag_paste_offset(pos);
            self.previous = pos;
            cx.notify();
            return;
        }
        if self.drag.is_move() || pressed == Some(MouseButton::Middle) {
            let dx = (self.previous.x - pos.x) * self.map.scale;
            let dz = (self.previous.z - pos.z) * self.map.scale;
            self.map.offset = self.map.offset.add(dx, dz);
        } else if pressed.is_some() && self.drag != Drag::None {
            if let Some(mark) = self.drag.mark() {
                // upstream: paintMode = SHIFT held (other keys ignored)
                let paint_mode = event.modifiers.shift;
                self.map.mark(pos.x, pos.z, mark, self.first, paint_mode);
                self.map.selection_version += 1;
            }
        }
        self.previous = pos;
        cx.notify();
    }

    fn on_mouse_up(&mut self, _event: &MouseUpEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(p) = &mut self.map.pasted {
            p.first_mouse = None;
        }
        self.drag = Drag::None;
        self.update_needed = true;
        cx.notify();
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if self.dialog.is_some() {
            self.dialog_key(event, _window, cx);
            return;
        }
        let k = event.keystroke.key.as_str();
        match k {
            "w" | "up" => self.pressed_keys |= 1,
            "a" | "left" => self.pressed_keys |= 2,
            "s" | "down" => self.pressed_keys |= 4,
            "d" | "right" => self.pressed_keys |= 8,

            "+" | "=" => self.zoom_key(2.0, cx),
            "-" => self.zoom_key(0.5, cx),
            "escape" => {}
            _ => {}
        }
        cx.notify();
    }

    fn on_key_up(&mut self, event: &KeyUpEvent, _window: &mut Window, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "w" | "up" => self.pressed_keys &= !1,
            "a" | "left" => self.pressed_keys &= !2,
            "s" | "down" => self.pressed_keys &= !4,
            "d" | "right" => self.pressed_keys &= !8,

            _ => {}
        }
        cx.notify();
    }

    fn dialog_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let k = event.keystroke.key.as_str();
        if let Some(Dialog::Goto { x, z, focus_x }) = &mut self.dialog {
            match k {
                "enter" => {
                    if let (Ok(xi), Ok(zi)) = (x.parse::<i32>(), z.parse::<i32>()) {
                        self.map.go_to(xi, zi);
                        self.update_needed = true;
                        self.dialog = None;
                    }
                }
                "escape" => self.dialog = None,
                "tab" => *focus_x = !*focus_x,
                "backspace" => {
                    let f = if *focus_x { x } else { z };
                    f.pop();
                }
                "space" => *focus_x = !*focus_x,
                _ => {
                    if let Some(c) = k.chars().next() {
                        if k.len() == 1 && (c.is_ascii_digit() || c == '-') {
                            let f = if *focus_x { x } else { z };
                            f.push(c);
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
        if k == "escape" {
            if self.map.pasted.is_some() {
                self.map.cancel_paste();
            }
            self.dialog = None;
        }
        cx.notify();
    }

    /// keyboard handling for the NBT tree editor (NBTTreeView edit mode).
    fn nbt_editor_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let k = event.keystroke.key.as_str();
        let ctrl = event.keystroke.modifiers.control;
        let Some(Dialog::NbtEditor {
            root, expanded, sel, editing, buf, status, dirty, region, chunk_abs, index,
        }) = &mut self.dialog
        else {
            return;
        };
        if *editing != NbtEdit::None {
            match k {
                "enter" => {
                    let mode = *editing;
                    let path = nbt_editor::flatten(root, expanded)
                        .get(*sel)
                        .map(|r| r.path.clone())
                        .unwrap_or_default();
                    let res = match mode {
                        NbtEdit::Value => nbt_editor::apply_value_edit(root, &path, buf),
                        NbtEdit::Add => {
                            let rows = nbt_editor::flatten(root, expanded);
                            let container_path: String = rows
                                .get(*sel)
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
                    };
                    match res {
                        Ok(()) => {
                            *dirty = true;
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
            "s" if ctrl => {
                let _ = (region, chunk_abs, index);
                let _ = status;
                let _ = dirty;
                // handled after borrow ends below
            }
            "escape" => self.dialog = None,
            _ => {}
        }
        if k == "s" && ctrl {
            self.nbt_editor_save(cx);
        }
    }

    fn zoom_key(&mut self, zoom: f32, cx: &mut Context<Self>) {
        if self.map.disabled {
            return;
        }
        let (min, max) = self.map.visible_region_bounds(Point2f::ZERO);
        let mid = min.region_to_block().add_p(max.region_to_block()).div(2);
        let center = if self.map.regions.is_empty() {
            Point2f::new(self.map.width / 2.0, self.map.height / 2.0)
        } else {
            mid.to_point2f().sub(self.map.offset).mul(1.0 / self.map.scale)
        };
        let old = self.map.scale;
        self.map.scale *= zoom;
        if self.map.update_scale(old, center) {
            self.update_needed = true;
        }
        cx.notify();
    }

    // -- canvas painting ---------------------------------------------------------

    fn paint_map(&mut self, bounds: Bounds<Pixels>, window: &mut Window, _cx: &mut App) {
        self.map.width = f32::from(bounds.size.width);
        self.map.height = f32::from(bounds.size.height);
        self.last_frame = std::time::Instant::now();

        window.paint_quad(fill(bounds, rgba(0x000000ff)));

        if self.pressed_keys != 0 && !self.map.disabled {
            let speed = 10.0;
            let dx = if self.pressed_keys & 2 != 0 { -speed } else if self.pressed_keys & 8 != 0 { speed } else { 0.0 };
            let dz = if self.pressed_keys & 1 != 0 { -speed } else if self.pressed_keys & 4 != 0 { speed } else { 0.0 };
            self.map.offset = self.map.offset.add(dx * self.map.scale, dz * self.map.scale);
            window.on_next_frame(|w, _| w.refresh());
        }

        let scale = self.map.scale;
        let show_empty = self.map.show_nonexistent_regions
            && config::world().map(|w| w.cfg.show_nonexistent_regions).unwrap_or(true);
        // upstream draws the empty tile image under every unloaded/missing
        // region — one fullscreen quad does the same job.
        if show_empty {
            window.paint_quad(fill(bounds, rgba(EMPTY_COLOR)));
        }

        let (vmin, vmax) = self.map.visible_region_bounds(Point2f::ZERO);
        for rx in vmin.x..=vmax.x {
            for rz in vmin.z..=vmax.z {
            let region = Point2i::new(rx, rz);
            let key = region.as_long();
            let offset = Point2f::new(
                region.x as f32 * TILE_SIZE as f32 - self.map.offset.x,
                region.z as f32 * TILE_SIZE as f32 - self.map.offset.z,
            );
            let w = TILE_SIZE as f32 / scale;
            let b = Bounds {
                origin: point(px(offset.x), px(offset.z)),
                size: size(px(w), px(w)),
            };
            let pasted_cs = self.map.pasted_chunks_for_region(region);
            let Some(tile) = self.map.tiles.get(&key) else { continue };
            let loaded = tile.image.is_some() && tile.loaded;
            if !loaded {
                continue;
            }
            if b.size.width < px(0.5) || b.size.height < px(0.5) {
                continue;
            }
            if let Some(ri) = tile.render_image.clone() {
                let _ = window.paint_image(b, px(0.).into(), ri, 0, false);
            }
            if self.map.overlay.is_some() {
                if let Some(oi) = tile.overlay_render_image.clone() {
                    let _ = window.paint_image(b, px(0.).into(), oi, 0, false);
                }
            }
            if let Some(ov) = selection_overlay_image(tile, &self.map.selection, self.map.selection_version) {
                let _ = window.paint_image(b, px(0.).into(), ov, 0, false);
            }
            if let Some(cs) = &pasted_cs {
                let _ = cs; let _ = tile;
                let color = config::global().paste_chunks_color.to_argb();
                let mut pxa = [0u32; 32 * 32];
                for i in cs.iter() { pxa[i] = color; }
                if let Some(pv) = paste_argb_image(&pxa) {
                    let _ = window.paint_image(b, px(0.).into(), pv, 0, false);
                }
            }
            if self.map.show_structure_icons {
                let structures = tile.structures.clone();
                if let Some(st) = structures {
                    self.paint_structure_icons(&st, region, scale, window);
                }
            }
            }
        }

        self.paint_grids(bounds, vmin, vmax, window);

        // coordinates (upstream TileMap.drawCoordinates): block coords,
        // EDGE/DYNAMIC edge labels when zoomed, GRID/dynamic intersections otherwise.
        if self.map.show_coordinates && scale > 1.0 {
            let ts = window.text_system().clone();
            let style = self.map.coordinate_style;
            let zoom = get_zoom_level(scale);
            let edge = style == crate::tilemap::CoordinateStyle::Edge
                || (style == crate::tilemap::CoordinateStyle::Dynamic && scale > 30.0);
            let mut paint = |text: String, sx: f32, sz: f32| {
                let line = ts.shape_line(
                    SharedString::from(text),
                    px(12.),
                    &[TextRun {
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
                    }],
                    None,
                );
                let _ = line.paint(point(px(sx), px(sz)), px(14.), window, _cx);
            };
            if edge {
                // edge labels: x along top, z along left (upstream rotates x;
                // gpui can't rotate text - horizontal is close enough)
                let mul = ((zoom / 4).max(1) * 512) as i64;
                for rx in vmin.x..=vmax.x + 1 {
                    let bx = rx as i64 * 512;
                    if bx % mul == 0 {
                        let sx = (rx as f32 * 512.0 - self.map.offset.x) / scale + 2.0;
                        if sx >= 0.0 && sx <= self.map.width {
                            paint(abbrev_coord(bx as i32), sx, 2.0);
                        }
                    }
                }
                for rz in vmin.z..=vmax.z + 1 {
                    let bz = rz as i64 * 512;
                    if bz % mul == 0 {
                        let sz = (rz as f32 * 512.0 - self.map.offset.z) / scale + 2.0;
                        if sz >= 0.0 && sz <= self.map.height {
                            paint(abbrev_coord(bz as i32), 2.0, sz);
                        }
                    }
                }
            } else {
                let mul = if scale > 7.0 || style == crate::tilemap::CoordinateStyle::Grid {
                    (zoom / 2).max(1) * 512
                } else {
                    512
                } as i64;
                for rx in vmin.x..=vmax.x + 1 {
                    for rz in vmin.z..=vmax.z + 1 {
                        let (bx, bz) = (rx as i64 * 512, rz as i64 * 512);
                        if bx % mul != 0 || bz % mul != 0 {
                            continue;
                        }
                        let sx = (rx as f32 * 512.0 - self.map.offset.x) / scale + 2.0;
                        let sz = (rz as f32 * 512.0 - self.map.offset.z) / scale + 2.0;
                        if sx < 0.0 || sz < 0.0 || sx > self.map.width || sz > self.map.height {
                            continue;
                        }
                        paint(format!("{}, {}", abbrev_coord(bx as i32), abbrev_coord(bz as i32)), sx, sz);
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

    fn paint_structure_icons(&mut self, structures: &std::collections::HashMap<i64, Vec<String>>, region: Point2i, scale: f32, window: &mut Window) {
        let _ = region;
        let registry = mc_core::structures::structures();
        let g = config::global();
        let chunk_size = 16.0 / scale;
        let icon_size = config::world().map(|w| w.cfg.structure_icon_size).unwrap_or(32) as f32 / scale;
        for (chunk_l, names) in structures {
            let chunk = Point2i::from_long(*chunk_l);
            let off = chunk
                .chunk_to_block()
                .to_point2f()
                .sub(self.map.offset)
                .mul(1.0 / scale);
            for name in names {
                // SelectStructures submenu persists per-type toggles
                if g.structure_icons.get(name).copied().unwrap_or(true) == false {
                    continue;
                }
                let Some(sd) = registry.icons.get(name) else { continue };
                if sd.max_scale < scale {
                    continue;
                }
                let canon = format!("minecraft:{}", sd.name);
                if !g.structure_icons.get(&canon).copied().unwrap_or(true) {
                    continue;
                }
                if let Some(icon) = self.icon(&sd.icon) {
                    let b = Bounds {
                        origin: point(
                            px(off.x + (chunk_size - icon_size) / 2.0),
                            px(off.z + (chunk_size - icon_size) / 2.0),
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

    fn icon(&mut self, icon: &str) -> Option<Arc<RenderImage>> {
        if let Some(i) = self.icon_cache.get(icon) {
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
        let img = image::load_from_memory(bytes).ok()?.to_rgba8();
        let r = Arc::new(RenderImage::new(smallvec::SmallVec::from_elem(image::Frame::new(img), 1)));
        self.icon_cache.insert(icon.to_string(), r.clone());
        Some(r)
    }

    fn render_layer_only_only(&self) -> bool {
        config::world().map(|w| w.cfg.render_layer_only).unwrap_or(false)
    }

    fn status_text(&mut self) -> String {
        let (min, max) = self.map.visible_region_bounds(Point2f::ZERO);
        let visible_regions = ((max.x - min.x + 1) * (max.z - min.z + 1)).max(0) as usize;
        let mut parts = vec![
            format!("{} visible tiles", self.map.visible_tiles()),
            format!("{} visible regions", visible_regions),
            format!("{} visible chunks", self.map.visible_tiles() as i64 * 1024),
            format!("zoom {}", self.map.scale),
        ];
        if let Some(h) = self.map.hovered_block {
            parts.push(format!("{} {}", h.x, h.z));
        }
        parts.push(format!("{} selected chunks", self.map.selected_chunks));
        // upstream status bar: hovered structure names
        if self.map.show_structure_icons {
            let st = self.map.hovered_block.and_then(|h| {
                let c = h.block_to_chunk();
                let r = c.chunk_to_region();
                self.map
                    .tiles
                    .get(&r.as_long())
                    .and_then(|t| t.structures.as_ref())
                    .and_then(|m| m.get(&c.as_long()))
            });
            if let Some(names) = st {
                if !names.is_empty() {
                    parts.push(format!(
                        "structures: {}{}",
                        names[0],
                        if names.len() > 1 { ", ..." } else { "" }
                    ));
                }
            }
        }
        if self.map.unsaved_selection {
            parts.push("selection not saved".into());
        }
        if let Some(ov) = &self.map.overlay {
            // upstream: "type(min, max), hoveredValue"
            let val = self.map.hovered_block.and_then(|h| {
                let c = h.block_to_chunk();
                let r = c.chunk_to_region();
                let rel = c.sub(r.region_to_chunk());
                self.map
                    .tiles
                    .get(&r.as_long())
                    .and_then(|t| t.overlay_data.as_ref())
                    .map(|d| d[(rel.z * 32 + rel.x) as usize])
            });
            let mm = match (ov.min, ov.max) {
                (Some(a), Some(b)) => format!("{}, {}", a, b),
                _ => "-".to_string(),
            };
            parts.push(format!(
                "overlay: {}({}), {}",
                ov.typ.display_name(),
                mm,
                val.map(|v| v.to_string()).unwrap_or_else(|| "-".into())
            ));
        }
        parts.push(format!("{} jobs in queue", mc_core::jobs::active_jobs()));
        parts.join(" | ")
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
    let img = image::load_from_memory(bytes).ok()?.to_rgba8();
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

fn selection_overlay_image(tile: &Tile, sel: &mc_core::selection::Selection, version: u64) -> Option<Arc<RenderImage>> {
    let key = tile.location.as_long();
    if !sel.is_any_chunk_in_region_selected(key) && !sel.is_region_selected(key) {
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
    if sel.is_region_selected(key) {
        // upstream: whole-region selection fills with regionSelectionColor
        let color = g.region_selection_color.to_argb();
        for p in px.iter_mut() {
            *p = color;
        }
    } else if let Some(Some(cs)) = sel.selected_chunks(tile.location) {
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
        let status = self.status_text();
        let show_bar = config::world().is_some();
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
            .bg(rgb(0x333333))
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
                    v.map.mark_all_tiles_obsolete();
                    v.update_needed = true;
                }
                cx.notify();
            }))
            .on_action(cx.listener(|v, _: &ChunkGridAction, _w, cx| {
                v.map.show_chunk_grid = !v.map.show_chunk_grid;
                let mut g = config::global();
                g.show_chunk_grid = v.map.show_chunk_grid;
                config::set_global(g);
                cx.notify();
            }))
            .on_action(cx.listener(|v, _: &RegionGridAction, _w, cx| {
                v.map.show_region_grid = !v.map.show_region_grid;
                let mut g = config::global();
                g.show_region_grid = v.map.show_region_grid;
                config::set_global(g);
                cx.notify();
            }))
            .on_action(cx.listener(|v, _: &CoordStyleDynamicAction, _w, cx| {
                v.map.coordinate_style = crate::tilemap::CoordinateStyle::Dynamic;
                let mut g = config::global();
                g.coordinate_style = "DYNAMIC".into();
                config::set_global(g);
                cx.notify();
            }))
            .on_action(cx.listener(|v, _: &CoordStyleEdgeAction, _w, cx| {
                v.map.coordinate_style = crate::tilemap::CoordinateStyle::Edge;
                let mut g = config::global();
                g.coordinate_style = "EDGE".into();
                config::set_global(g);
                cx.notify();
            }))
            .on_action(cx.listener(|v, _: &CoordStyleGridAction, _w, cx| {
                v.map.coordinate_style = crate::tilemap::CoordinateStyle::Grid;
                let mut g = config::global();
                g.coordinate_style = "GRID".into();
                config::set_global(g);
                cx.notify();
            }))
            .on_action(cx.listener(|v, _: &CoordinatesAction, _w, cx| {
                v.map.show_coordinates = !v.map.show_coordinates;
                let mut g = config::global();
                g.show_coordinates = v.map.show_coordinates;
                config::set_global(g);
                cx.notify();
            }))
            .on_action(cx.listener(|v, a: &ToggleStructureIcon, _w, cx| {
                let mut g = config::global();
                let cur = g.structure_icons.get(&a.id).copied().unwrap_or(true);
                g.structure_icons.insert(a.id.clone(), !cur);
                config::set_global(g);
                cx.notify();
            }))
            .on_action(cx.listener(|v, _: &StructureIconsAction, _w, cx| {
                v.map.show_structure_icons = !v.map.show_structure_icons;
                cx.notify();
            }))
            .on_action(cx.listener(|v, _: &GotoAction, _w, cx| {
                v.dialog = Some(Dialog::Goto { x: String::new(), z: String::new(), focus_x: true });
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
                v.update_needed = true;
                cx.notify();
            }))
            .on_action(cx.listener(|v, _: &SaveScreenshotAction, _w, cx| {
                v.save_screenshot(cx);
            }))
            .on_action(cx.listener(|v, _: &ClearViewCacheAction, _w, cx| {
                if let Some(ws) = config::world() {
                    let _ = std::fs::remove_dir_all(&ws.cache_dir);
                }
                v.map.mark_all_tiles_obsolete();
                v.update_needed = true;
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
                let sel = v.map.selection.clone();
                let keys: Vec<i64> = sel.selection.keys().copied().collect();
                for k in keys {
                    v.map.discard_cached_image(Point2i::from_long(k));
                }
                v.map.mark_all_tiles_obsolete();
                v.update_needed = true;
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
                cx.notify();
            }))
            .on_action(cx.listener(|v, _: &NextOverlayTypeAction, _w, cx| {
                v.map.next_overlay_type();
                cx.notify();
            }))
            .on_action(cx.listener(|v, _: &AboutAction, _w, cx| {
                v.dialog = Some(Dialog::About);
                cx.notify();
            }))
            .on_key_down(cx.listener(|v, e, w, cx| v.on_key_down(e, w, cx)))
            .on_key_up(cx.listener(|v, e, w, cx| v.on_key_up(e, w, cx)))
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
            .children(if show_bar && !self.render_layer_only_only() {
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
                    .child(if show_bar { status } else { "no world loaded — File > Open World".to_string() }),
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



    fn render_height_slider(&self, cx: &mut Context<Self>) -> AnyElement {
        let h = config::world().map(|w| w.cfg.render_height).unwrap_or(319);
        div()
            .h(px(22.))
            .w_full()
            .bg(rgb(0x242424))
            .flex()
            .items_center()
            .px_2()
            .gap_2()
            .text_size(px(11.))
            .text_color(rgb(0xbbbbbb))
            .child(format!("height {}", h))
            .child(
                // simple - and + controls matching slider behavior (16px step)
                h_flex().gap_1()
                    .child(button("-", cx, |v, _w, cx| {
                        if let Some(mut ws) = config::world() {
                            ws.cfg.render_height = (ws.cfg.render_height - 16).max(-64);
                            ws.cfg.save(&ws.cache_dir);
                            config::set_world(Some(ws));
                        }
                        v.map.mark_all_tiles_obsolete();
                        v.update_needed = true;
                        cx.notify();
                    }))
                    .child(button("+", cx, |v, _w, cx| {
                        if let Some(mut ws) = config::world() {
                            ws.cfg.render_height = (ws.cfg.render_height + 16).min(319);
                            ws.cfg.save(&ws.cache_dir);
                            config::set_world(Some(ws));
                        }
                        v.map.mark_all_tiles_obsolete();
                        v.update_needed = true;
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
        self.dialog = Some(Dialog::Confirm {
            msg: "Delete selected chunks?".into(),
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
        self.dialog = Some(Dialog::Confirm {
            msg: "Defragment selected regions?".into(),
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
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: false, directories: true, multiple: false,
            prompt: Some("Select world directory to export to".into()),
        });
        cx.spawn(async move |this: WeakEntity<MainView>, cx: &mut AsyncApp| {
            if let Ok(Ok(Some(paths))) = rx.await {
                if let Some(dir) = paths.into_iter().next() {
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
            }
        }).detach();
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

    fn paste_chunks(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
        let vs = window.window_bounds().get_bounds().size;
        let size = Point2f::new(f32::from(vs.width), f32::from(vs.height));
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
        mc_core::jobs::pools().add_process(mc_core::jobs::Job::new(
            mc_core::jobs::PRIORITY_LOW, None,
            move || {
                let Ok(rd) = std::fs::read_dir(&clip) else { return };
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
                    // chunks to paste from this region
                    let sel_cs = pasted_sel.as_ref().and_then(|s| {
                        if s.inverted {
                            s.selection.get(&src_region.as_long()).map(|c| {
                                match c { Some(cc) => cc.clone().flip(), None => mc_core::selection::ChunkSet::new() }
                            })
                        } else {
                            match s.selection.get(&src_region.as_long()) {
                                Some(Some(c)) => Some(c.clone()),
                                Some(None) => Some({ let mut f = mc_core::selection::ChunkSet::new(); f.fill(); f }),
                                None => None,
                            }
                        }
                    }).or_else(|| { let mut f = mc_core::selection::ChunkSet::new(); f.fill(); Some(f) });
                    let Some(sel_cs) = sel_cs else { continue };
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
                        for (i, c) in chunks {
                            if let Some(chunk) = &src.chunks[i] {
                                let rel = c.sub(trel);
                                if rel.x < 0 || rel.x > 31 || rel.z < 0 || rel.z > 31 { continue; }
                                let di = (rel.z * 32 + rel.x) as usize;
                                if d.chunks[di].is_some() && !overwrite { continue; }
                                let mut cc = chunk.clone();
                                cc.timestamp = std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .map(|t| t.as_secs() as i32).unwrap_or(0);
                                d.chunks[di] = Some(cc);
                            }
                        }
                        let _ = d.save_with_temp_file(&dst);
                        let _ = tx.send(crate::tilemap::TileResult::Image {
                            region: tregion, image: None, zoom: 0, structures: None, cache_write: false,
                        });
                    }
                }
            }));
        cx.notify();
    }
    fn export_image(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let sel = self.map.selection.clone();
        if sel.is_empty() {
            self.dialog = Some(Dialog::Error { msg: "no selection".into() });
            cx.notify();
            return;
        }
        let rx = cx.prompt_for_new_path(
            &PathBuf::from(std::env::var("HOME").unwrap_or_default()),
            Some("selection.png"),
        );
        let tx = self.map.result_tx.clone();
        let _ = tx;
        cx.spawn(async move |this: WeakEntity<MainView>, cx: &mut AsyncApp| {
            if let Ok(Ok(Some(path))) = rx.await {
                        let mut minx = i32::MAX; let mut minz = i32::MAX;
                let mut maxx = i32::MIN; let mut maxz = i32::MIN;
                for &region_l in sel.selection.keys() {
                    let r = mc_core::point::Point2i::from_long(region_l);
                    minx = minx.min(r.x); minz = minz.min(r.z);
                    maxx = maxx.max(r.x); maxz = maxz.max(r.z);
                }
                let w = (maxx - minx + 1) as usize * 512;
                let h = (maxz - minz + 1) as usize * 512;
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
        }).detach();
    }


    /// upstream SumSelectionJob: sums the ACTIVE OVERLAY's parseValue over all
    /// selected chunks (that's why it's disabled without an overlay).
    fn sum_selection(&mut self, cx: &mut Context<Self>) {
        let sel = self.map.selection.clone();
        let Some(ov) = self.map.overlay.clone() else { return };
        if sel.is_empty() { return; }
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

    fn import_chunks(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true, directories: false, multiple: true,
            prompt: Some("Select region files to import".into()),
        });
        let tx = self.map.result_tx.clone();
        cx.spawn(async move |this: WeakEntity<MainView>, cx: &mut AsyncApp| {
            if let Ok(Ok(Some(paths))) = rx.await {
                mc_core::jobs::pools().add_process(mc_core::jobs::Job::new(
                    mc_core::jobs::PRIORITY_LOW, None,
                    move || {
                        let __p = mc_core::jobs::Progress::new("Importing chunks", paths.len() as i64);
                        for p in paths {
                            let __g = __p.inc_guard();
                            if __p.is_cancelled() { break; }
                            let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                            __p.set_current(name.clone());
                            let Some(region) = mc_core::files::parse_mca_file_name(&name) else { continue };
                            let Some(dir) = crate::config::world().and_then(|w| w.dirs.region.clone()) else { return };
                            let dst = dir.join(&name);
                            let Ok(mut src) = mc_core::mca::McaFile::new(p) else { continue };
                            let mut dstf = if dst.exists() {
                                mc_core::mca::McaFile::new(dst.clone()).ok()
                            } else {
                                Some(mc_core::mca::McaFile::empty(region, dst.clone()))
                            };
                            if src.load(false).is_err() { continue; }
                            if let Some(d) = dstf.as_mut() {
                                if dst.exists() && d.load(false).is_err() { continue; }
                                for (i, c) in src.chunks.iter().enumerate() {
                                    if c.is_some() {
                                        d.chunks[i] = c.clone();
                                        if let Some(cc) = &mut d.chunks[i] {
                                            cc.timestamp = std::time::SystemTime::now()
                                                .duration_since(std::time::UNIX_EPOCH)
                                                .map(|t| t.as_secs() as i32).unwrap_or(0);
                                        }
                                    }
                                }
                                let _ = d.save_with_temp_file(&dst);
                            }
                            let _ = tx.send(crate::tilemap::TileResult::Image {
                                region, image: None, zoom: 0, structures: None, cache_write: false,
                            });
                        }
                    }));
                let _ = this.update(&mut *cx, |v, cx| {
                    v.map.mark_all_tiles_obsolete();
                    v.update_needed = true;
                    cx.notify();
                });
            }
        }).detach();
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
        self.dialog = Some(Dialog::Confirm {
            msg: "Swap selected chunks?".into(),
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

    fn change_fields(&mut self, cx: &mut Context<Self>) {
        // upstream dialog edits NBT fields on selected chunks (ChangeFieldsDialog);
        // we accept "path = snbt_value" typed inline
        self.dialog = Some(Dialog::TextInput {
            title: "Change fields".into(),
            hint: "path = value (e.g. Status = \"minecraft:full\")".into(),
            text: String::new(),
            on_done: Box::new(|v, w, cx, text| {
                v.apply_change_fields(text, w, cx);
            }),
        });
        cx.notify();
    }

    fn apply_change_fields(&mut self, text: String, _w: &mut Window, cx: &mut Context<Self>) {
        // parse "path = value" where value is SNBT literal
        let Some((path, raw)) = text.split_once('=') else {
            self.dialog = Some(Dialog::Error { msg: "expected: path = value".into() });
            cx.notify();
            return;
        };
        let path = path.trim().to_string();
        let raw = raw.trim().to_string();
        let sel = self.map.selection.clone();
        let tx = self.map.result_tx.clone();
        let dirs = crate::config::world().map(|w| w.dirs.clone()).unwrap_or_default();
        crate::ops::for_each_selected_region(
            &sel, dirs, "Changing fields", tx,
            move |_region, chunks, m, _path, _dirs| {
                let val = mc_core::nbt::parse_snbt(&raw)
                    .unwrap_or_else(|_| mc_core::nbt::Tag::String(raw.clone()));
                let mut changed = false;
                for i in 0..1024 {
                    if !chunks.as_ref().map(|c| c.get(i)).unwrap_or(true) { continue; }
                    let Some(c) = m.chunks[i].as_mut() else { continue };
                    let Some(d) = c.data.as_mut() else { continue };
                    mc_core::nbt::path_set(d, &path, val.clone());
                    changed = true;
                }
                changed
            },
        );
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
        let mut expanded = std::collections::HashSet::new();
        for k in data.keys() {
            expanded.insert(k.clone());
        }
        self.dialog = Some(Dialog::NbtEditor {
            region: region_l,
            chunk_abs,
            index,
            root: data,
            expanded,
            sel: 0,
            editing: NbtEdit::None,
            buf: String::new(),
            status: "↑↓ navigate  ←→ collapse/expand  enter edit  a add  r rename  del delete  ctrl-s save  esc close".into(),
            dirty: false,
        });
        cx.notify();
    }

    /// save the edited chunk NBT back to disk through the process pool.
    fn nbt_editor_save(&mut self, cx: &mut Context<Self>) {
        let Some(Dialog::NbtEditor { region, chunk_abs, index, root, expanded, sel, status, dirty, .. }) = self.dialog.take() else {
            return;
        };
        self.dialog = Some(Dialog::NbtEditor {
            region,
            chunk_abs,
            index,
            root: root.clone(),
            expanded,
            sel,
            editing: NbtEdit::None,
            buf: String::new(),
            status: if dirty { "saving...".into() } else { status },
            dirty: false,
        });
        let tx = self.map.result_tx.clone();
        mc_core::jobs::pools().add_process(mc_core::jobs::Job::new(
            mc_core::jobs::PRIORITY_LOW, None,
            move || {
                let rp = Point2i::from_long(region);
                let Some(dir) = crate::config::world().and_then(|w| w.dirs.region.clone()) else { return };
                let path = dir.join(mc_core::files::mca_file_name(rp));
                let Ok(mut m) = mc_core::mca::McaFile::new(path.clone()) else { return };
                if m.load(false).is_err() { return; }
                if let Some(c) = m.chunks[index].as_mut() {
                    c.data = Some(root);
                    let _ = m.save_with_temp_file(&path);
                }
                let _ = tx.send(crate::tilemap::TileResult::Image {
                    region: rp, image: None, zoom: 0, structures: None, cache_write: false,
                });
            }));
        cx.notify();
    }

    fn edit_overlays(&mut self, cx: &mut Context<Self>) {
        // serialize current overlays as editable lines: "Type min max [data]"
        let cur: Vec<String> = self
            .map
            .overlays
            .iter()
            .flatten()
            .map(|o| {
                let mut l = format!(
                    "{} {} {}",
                    o.typ.display_name(),
                    o.min.map(|v| v.to_string()).unwrap_or_default(),
                    o.max.map(|v| v.to_string()).unwrap_or_default(),
                );
                if !o.raw_multi.is_empty() {
                    l.push(' ');
                    l.push_str(&o.raw_multi);
                }
                l
            })
            .collect();
        self.dialog = Some(Dialog::TextInput {
            title: "Edit overlays".into(),
            hint: "one per line, '|' separated: Type min max [data]  e.g. InhabitedTime 0 1000 | Blocks 0 500 minecraft:stone | Custom 0 10 region/sections.size".into(),
            text: cur.join(" | "),
            on_done: Box::new(|v, w, cx, text| {
                v.apply_overlays(text, w, cx);
            }),
        });
        cx.notify();
    }

    fn apply_overlays(&mut self, text: String, _w: &mut Window, cx: &mut Context<Self>) {
        let text = text.trim().to_string();
        if text.is_empty() {
            self.map.set_overlays(Vec::new());
            cx.notify();
            return;
        }
        let mut list = Vec::new();
        for line in text.split(['|', '\n']) {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            match mc_core::overlay::parse_overlay_line(line) {
                Ok(o) => list.push(o),
                Err(e) => {
                    self.dialog = Some(Dialog::Error { msg: format!("{e} (in '{line}')") });
                    cx.notify();
                    return;
                }
            }
        }
        self.map.set_overlays(list);
        // select the first valid overlay like upstream's initial selection flow:
        // cycling with N activates it
        self.map.set_overlay(None);
        cx.notify();
    }

    fn filter_chunks(&mut self, cx: &mut Context<Self>) {
        self.dialog = Some(Dialog::TextInput {
            title: "Filter chunks".into(),
            hint: "query e.g.: xPos >= 0 AND Palette contains \"minecraft:stone\"  |  fields: xPos zPos yPos DataVersion InhabitedTime LastUpdate Timestamp Status Palette Biome Compression Entities #Entities #ProtoEntities #TileEntities Structures Circle Border Selection PlayerLocation PlayerSpawn LightPopulated <nbt.path>".into(),
            text: String::new(),
            on_done: Box::new(|v, w, cx, text| {
                v.apply_filter(text, w, cx);
            }),
        });
        cx.notify();
    }

    fn apply_filter(&mut self, query: String, _w: &mut Window, cx: &mut Context<Self>) {
        let expr = match mc_core::filter::parse(&query) {
            Ok(e) => e,
            Err(e) => {
                self.dialog = Some(Dialog::Error { msg: e });
                cx.notify();
                return;
            }
        };
        let tx = self.map.result_tx.clone();
        let sel = self.map.selection.clone();
        mc_core::jobs::pools().add_process(mc_core::jobs::Job::new(
            mc_core::jobs::PRIORITY_LOW, None,
            move || {
                // evaluate over all regions of the current world (filtered to
                // current selection if non-empty)
                let mut result = mc_core::selection::Selection::new();
                let regions: Vec<i64> = if sel.is_empty() {
                    crate::config::world()
                        .map(|w| files::scan_region_files(w.region_dir()).into_iter().collect())
                        .unwrap_or_default()
                } else {
                    sel.selection.keys().copied().collect()
                };
                let __p = mc_core::jobs::Progress::new("Filtering chunks", regions.len() as i64);
                let dirs = crate::config::world().map(|w| w.dirs.clone());
                let mut ctx = mc_core::filter::FilterCtx::new(
                    dirs.as_ref().and_then(|d| d.region.clone()),
                    dirs.as_ref().and_then(|d| d.entities.clone()),
                );
                for rl in regions {
                    let __g = __p.inc_guard();
                    let region = mc_core::point::Point2i::from_long(rl);
                    __p.set_current(mc_core::files::mca_file_name(region));
                    if __p.is_cancelled() { break; }
                    // RegionMatcher: classify the whole region without loading
                    match expr.matches_region(region, &mut ctx) {
                        mc_core::filter::MatchType::None => continue,
                        mc_core::filter::MatchType::Full => {
                            // every chunk matches; apply the within-selection mask
                            match sel.selection.get(&rl) {
                                Some(Some(cs)) => {
                                    let mut set = result.selection.entry(rl).or_insert_with(|| {
                                        let mut e = mc_core::selection::ChunkSet::new();
                                        e.clear_all();
                                        Some(e)
                                    });
                                    if let Some(set) = &mut set { set.or(cs); }
                                }
                                Some(None) => {
                                    result.selection.insert(rl, None);
                                }
                                None => {
                                    if sel.is_empty() {
                                        result.selection.insert(rl, None);
                                    }
                                }
                            }
                            continue;
                        }
                        mc_core::filter::MatchType::Partial => {}
                    }
                    let Some(dir) = crate::config::world().and_then(|w| w.dirs.region.clone()) else { continue };
                    let p = dir.join(mc_core::files::mca_file_name(region));
                    if !p.exists() { continue; }
                    let Ok(mut m) = mc_core::mca::McaFile::new(p) else { continue };
                    if m.load(false).is_err() { continue; }
                    ctx.insert_chunk_set(rl, m.offsets_chunk_set());
                    for i in 0..1024 {
                        let chunk = region.region_to_chunk().add_p(mc_core::point::Point2i::from_chunk_index(i as i32));
                        let within = sel.is_empty() || sel.is_chunk_selected(chunk);
                        if !within { continue; }
                        let Some(c) = &m.chunks[i] else { continue };
                        let Some(d) = &c.data else { continue };
                        let dv = mc_core::nbt::data_version(Some(d));
                        if expr.matches(c, dv, &mut ctx) {
                            let chunk = region.region_to_chunk().add_p(mc_core::point::Point2i::from_chunk_index(i as i32));
                            result.add_chunk(chunk);
                        }
                    }
                    let _ = tx.send(crate::tilemap::TileResult::Image {
                        region, image: None, zoom: 0, structures: None, cache_write: false,
                    });
                }
                // send the resulting selection back through a tile result? use a
                // dedicated mechanism: store into a shared slot
                *crate::tilemap::FILTER_RESULT.lock().unwrap() = Some(result);
            }));
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
            Dialog::Goto { x, z, focus_x } => {
                let fx = *focus_x;
                let x_c = x.clone();
                let z_c = z.clone();
                panel(
                    v_flex().gap_2()
                        .child(text_el("Goto", 16.))
                        .child(
                            h_flex().gap_2()
                                .child(input_box("x", &x_c, fx))
                                .child(input_box("z", &z_c, !fx)),
                        )
                        .child(
                            h_flex().gap_2()
                                .child(button("Go to", cx, |v, _w, cx| {
                                    if let Some(Dialog::Goto { x, z, .. }) = &v.dialog {
                                        if let (Ok(xi), Ok(zi)) = (x.parse::<i32>(), z.parse::<i32>()) {
                                            v.map.go_to(xi, zi);
                                            v.update_needed = true;
                                        }
                                    }
                                    v.dialog = None;
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
                    .child(text_el("MCA Selector", 16.))
                    .child(text_el(concat!("v", env!("CARGO_PKG_VERSION"), " — a faithful Rust + GPUI port of MCA Selector."), 12.))
                    .child(text_el("https://github.com/Querz/mcaselector", 12.))
                    .child(text_el("Copyright (c) 2016-2026 Querz — GPLv3", 12.))
                    .child(button("Close", cx, |v, _w, cx| {
                        v.dialog = None;
                        cx.notify();
                    })),
            ),
            Dialog::Error { msg } => panel(
                v_flex().gap_2()
                    .child(text_el("Error", 16.))
                    .child(text_el(msg.clone(), 12.))
                    .child(button("OK", cx, |v, _w, cx| {
                        v.dialog = None;
                        cx.notify();
                    })),
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
                                    v.update_needed = true;
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
                root, expanded, sel, editing, buf, status, chunk_abs, dirty, ..
            } => {
                let rows = nbt_editor::flatten(root, expanded);
                let n = rows.len();
                let vis = 22usize;
                let start = if *sel >= vis { sel + 1 - vis } else { 0 };
                let end = (start + vis).min(n);
                let mut list = v_flex()
                    .h(px(vis as f32 * 20.))
                    .overflow_hidden()
                    .gap(px(1.));
                for (i, r) in rows[start..end].iter().enumerate() {
                    let idx = start + i;
                    let selected = idx == *sel;
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
                            if let Some(Dialog::NbtEditor { sel, .. }) = &mut v.dialog {
                                *sel = idx;
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
                        format!("NBT editor — chunk {}, {} {}", chunk_abs.x, chunk_abs.z, if *dirty { "(modified)" } else { "" }),
                        15.,
                    ))
                    .child(list)
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
                v_flex().child(text_el(msg.clone(), 14.)),
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
        };
        Some(
            overlay
                .on_mouse_down(MouseButton::Left, cx.listener(|v, e, _w, cx| {
                    let _ = e;
                    match v.dialog {
                        Some(Dialog::Confirm { .. }) | Some(Dialog::Info { .. }) | Some(Dialog::Goto { .. })
                            | Some(Dialog::JobProgress) | Some(Dialog::NbtEditor { .. }) | Some(Dialog::TextInput { .. }) => {}
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

fn input_box(id: &'static str, value: &str, focused: bool) -> Stateful<Div> {
    div()
        .id(id)
        .w(px(200.))
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
