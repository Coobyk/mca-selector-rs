//! Render-height slider (upstream HeightSlider + NumberTextField): drag
//! thumb, scroll nudge and click-to-type numeric entry.

use crate::config;
use crate::dialogs::Dialog;
use crate::view::MainView;
use gpui::*;

/// debounce window for render-height invalidation: changes arriving inside
/// it extend the deadline rather than each triggering a full tile
/// re-render (upstream re-renders per change — fine for clicks, ruinous
/// for 60Hz slider drags).
const RENDER_SETTLE: std::time::Duration = std::time::Duration::from_millis(250);

impl MainView {
    pub(crate) fn height_from_slider_x(&self, x: f32) -> i32 {
        let w = config::WORLD.read().unwrap().as_ref()
            .map(|w| (w.cfg.render_height, w.height_range()))
            .unwrap_or((319, (-64, 319)));
        let (cur, (lo, hi)) = w;
        let Some(b) = *self.height_slider_bounds.borrow() else {
            return cur;
        };
        let bw = f32::from(b.size.width);
        if bw <= 0. {
            return cur;
        }
        let frac = ((x - f32::from(b.origin.x)) / bw).clamp(0., 1.);
        (lo as f32 + frac * (hi - lo) as f32).round() as i32
    }

    /// upstream HeightSlider: the slider pins at -64..319 but the numeric
    /// field accepts -64..2047 — render_height keeps the typed value while
    /// the slider thumb clamps (frac clamps to 0..1 in render).
    pub(crate) fn set_render_height(&mut self, h: i32, persist: bool, cx: &mut Context<Self>) {
        let h = h.clamp(-64, 2047);
        let cur = config::WORLD.read().unwrap().as_ref()
            .map(|w| w.cfg.render_height).unwrap_or(319);
        if h == cur && !persist {
            return;
        }
        let changed = h != cur;
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
        if changed {
            match self.render_settle {
                // a change burst is in flight (slider drag / scroll burst):
                // just extend the settle window — the flush re-renders the
                // tile set ONCE at the final height instead of re-
                // invalidating every visible region per input tick (~200ms
                // of render work each). Tiles keep their old pixels as
                // placeholders in the meantime, like upstream.
                Some((_, last)) => {
                    self.render_settle = Some((
                        std::time::Instant::now() + RENDER_SETTLE,
                        last,
                    ));
                }
                // first change of a burst: invalidate now like upstream —
                // a one-shot height change re-renders with no added delay —
                // then coalesce follow-up changes inside the window.
                None => {
                    self.map.mark_all_tiles_obsolete();
                    let new_hash = config::WORLD.read().unwrap().as_ref()
                        .map(|w| crate::tilemap::render_hash(&w.cfg, w.dim_kind()))
                        .unwrap_or(0);
                    self.render_settle = Some((
                        std::time::Instant::now() + RENDER_SETTLE,
                        new_hash,
                    ));
                    self.request_update();
                }
            }
        }
        cx.notify();
    }

    /// The settle window expired (or a slider drag ended): if the effective
    /// render settings moved since the last invalidation, invalidate once
    /// at the final values — a drag/scroll burst costs exactly one extra
    /// tile re-render generation instead of one per input tick.
    pub(crate) fn settle_render_height(&mut self) {
        if let Some((_, last_hash)) = self.render_settle.take() {
            let cur = config::WORLD.read().unwrap().as_ref()
                .map(|w| crate::tilemap::render_hash(&w.cfg, w.dim_kind()));
            if Some(last_hash) != cur {
                self.map.mark_all_tiles_obsolete();
                self.request_update();
            }
        }
    }

    /// slider drag end (shared by the canvas and root-level mouse-up
    /// handlers — whichever fires first): persist the height and flush a
    /// pending settle immediately instead of waiting out the deadline.
    pub(crate) fn finish_height_drag(&mut self) {
        if !self.height_slider_drag {
            return;
        }
        self.height_slider_drag = false;
        if let Some(ws) = config::world() {
            ws.cfg.save(&ws.cache_dir);
        }
        self.settle_render_height();
    }

    pub(crate) fn render_height_slider(&self, cx: &mut Context<Self>) -> AnyElement {
        let (h, (lo, hi)) = config::WORLD.read().unwrap().as_ref()
            .map(|w| (w.cfg.render_height, w.height_range()))
            .unwrap_or((319, (-64, 319)));
        let frac = (((h - lo) as f32) / (hi - lo) as f32).clamp(0., 1.);
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
            .child(div().w(px(30.)).flex_none().child(format!("{lo}")))
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
                    .on_scroll_wheel(cx.listener(move |v, e: &ScrollWheelEvent, _w, cx| {
                        let dy = match e.delta {
                            ScrollDelta::Pixels(p) => f32::from(p.y),
                            ScrollDelta::Lines(l) => l.y,
                        };
                        if dy != 0.0 {
                            let cur = config::WORLD.read().unwrap().as_ref()
                                .map(|w| w.cfg.render_height).unwrap_or(319);
                            let next = cur + if dy > 0.0 { 1 } else { -1 };
                            v.set_render_height(next.clamp(lo, hi), true, cx);
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
            .child(div().w(px(30.)).flex_none().child(format!("{hi}")))
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
}
