//! Mouse/scroll/keyboard input for the map canvas (upstream TileMap mouse
//! handlers + KeyActivator held-key pan). Dialog keystrokes are dispatched
//! from on_key_down into dialogs::dialog_key.

use crate::config;
use crate::tilemap::CHUNK_GRID_SCALE;
use crate::view::MainView;
use gpui::*;
use mc_core::point::{Point2f, Point2i};

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Drag {
    PasteMove,
    None,
    LeftSelect,
    RightSelect,
    MiddleMove,
}

impl Drag {
    /// upstream: primary marks, secondary unmarks; ctrl is not a modifier.
    pub(crate) fn mark(&self) -> Option<bool> {
        match self {
            Drag::LeftSelect => Some(true),
            Drag::RightSelect => Some(false),
            _ => None,
        }
    }
    pub(crate) fn is_move(&self) -> bool {
        matches!(self, Drag::MiddleMove)
    }
}

impl MainView {
    pub(crate) fn on_scroll(&mut self, event: &ScrollWheelEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if self.map.disabled() || self.dialog.is_some() {
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
            let pos = self.map.canvas_pos(f32::from(event.position.x), f32::from(event.position.y));
            if self.map.update_scale(old, pos) {
                // request fresh-resolution tiles at the next paint — a wheel
                // burst then costs one O(visible) update per frame instead
                // of one per event.
                self.request_update();
            }
            // a scroll fully eaten by the min/max clamp repaints an
            // identical frame, so skip it.
            if self.map.scale != old {
                cx.notify();
            }
        }
    }

    pub(crate) fn on_mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.map.disabled() || self.dialog.is_some() {
            return;
        }
        self.focus.focus(window);
        let pos = self.map.canvas_pos(f32::from(event.position.x), f32::from(event.position.y));
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

    pub(crate) fn on_mouse_move(&mut self, event: &MouseMoveEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if self.height_slider_drag {
            let h = self.height_from_slider_x(f32::from(event.position.x));
            self.set_render_height(h, false, cx);
            return;
        }
        let pos = self.map.canvas_pos(f32::from(event.position.x), f32::from(event.position.y));
        // repaint only when something visible actually changed: mouse moves
        // that keep the same hovered block / paste offset / selection would
        // repaint the full scene for every event.
        let hovered = self.map.mouse_block(pos.x, pos.z);
        let mut dirty = self.map.hovered_block != Some(hovered);
        self.map.hovered_block = Some(hovered);
        if self.map.disabled() || self.dialog.is_some() {
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

    pub(crate) fn on_mouse_up(&mut self, _event: &MouseUpEvent, _window: &mut Window, cx: &mut Context<Self>) {
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

    pub(crate) fn on_key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        // held-key pan speed modifier (upstream: SHIFT -> 10*scale)
        self.pan_shift = event.keystroke.modifiers.shift;
        if self.dialog.is_some() {
            self.dialog_key(event, _window, cx);
            return;
        }
        let k = event.keystroke.key.as_str();
        if k == "escape" && self.open_menu.take().is_some() {
            self.open_submenu = None;
            cx.notify();
            return;
        }
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

    pub(crate) fn on_key_up(&mut self, event: &KeyUpEvent, _window: &mut Window, cx: &mut Context<Self>) {
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

    /// upstream zoomFactor: scale /= factor, anchored at the canvas center.
    pub(crate) fn zoom_key(&mut self, factor: f32, cx: &mut Context<Self>) {
        if self.map.disabled() {
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

    /// held-key pan step, called once per paint with the frame delta
    /// (upstream KeyActivator: 5*scale every 33ms, 10*scale with shift).
    /// Requests the next frame while keys are held.
    pub(crate) fn held_key_pan(&mut self, dt: std::time::Duration, window: &mut Window) {
        if self.pressed_keys == 0 || self.map.disabled() {
            return;
        }
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
}
