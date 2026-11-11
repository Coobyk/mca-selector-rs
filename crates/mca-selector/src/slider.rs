//! Render-height slider (upstream HeightSlider + NumberTextField): drag
//! thumb, scroll nudge and click-to-type numeric entry.

use crate::config;
use crate::dialogs::Dialog;
use crate::view::MainView;
use gpui::*;

impl MainView {
    pub(crate) fn height_from_slider_x(&self, x: f32) -> i32 {
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
    pub(crate) fn set_render_height(&mut self, h: i32, persist: bool, cx: &mut Context<Self>) {
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

    pub(crate) fn render_height_slider(&self, cx: &mut Context<Self>) -> AnyElement {
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
}
