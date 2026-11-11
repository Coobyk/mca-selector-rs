//! In-window menu bar. Upstream renders a Swing MenuBar inside the window;
//! the vendored gpui only exposes OS menus via cx.set_menus (invisible on
//! Linux/Wayland), so the same crate::menus() model is rendered as a bar
//! row + dropdown panels here. Actions dispatch through
//! window.dispatch_action so they hit the same .on_action handlers as the
//! keyboard accelerators.

use crate::view::MainView;
use crate::dialogs::{h_flex, v_flex};
use gpui::*;

const BAR_H: f32 = 24.0;
const ROW_H: f32 = 24.0;
const PANEL_W: f32 = 300.0;
const SEP_H: f32 = 9.0;

impl MainView {
    /// The top bar row. Dropdown + click-away backdrop live in
    /// render_menu_overlay (absolute, over the map container).
    pub(crate) fn render_menu_bar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let menus = crate::menus(cx);
        let mut bar = h_flex()
            .h(px(BAR_H))
            .w_full()
            .bg(rgb(0x2b2b2b))
            .border_b_1()
            .border_color(rgb(0x1a1a1a))
            .text_size(px(12.))
            .text_color(rgb(0xcccccc));
        for (i, m) in menus.iter().enumerate() {
            // fixed-width buttons so the dropdown x can be computed
            let w = m.name.len() as f32 * 8.0 + 24.0;
            let open = self.open_menu == Some(i);
            bar = bar.child(
                div()
                    .id(SharedString::from(format!("menu-btn-{i}")))
                    .w(px(w))
                    .h_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(if open { rgb(0x4a4d4f) } else { rgb(0x2b2b2b) })
                    .hover(|s| s.bg(rgb(0x3c3f41)))
                    .cursor_pointer()
                    .on_mouse_move(cx.listener(move |v, _e, _w, cx| {
                        // once a menu is open, hovering switches menus
                        // (upstream MenuBar does the same)
                        if v.open_menu.is_some() && v.open_menu != Some(i) {
                            v.open_menu = Some(i);
                            v.open_submenu = None;
                            cx.notify();
                        }
                    }))
                    .on_mouse_down(MouseButton::Left, cx.listener(move |v, _e, _w, cx| {
                        v.open_menu = if v.open_menu == Some(i) { None } else { Some(i) };
                        v.open_submenu = None;
                        cx.notify();
                    }))
                    .child(m.name.clone()),
            );
        }
        bar
    }

    /// Click-away backdrop + open dropdown (+submenu flyout). Painted via
    /// deferred+anchored so they land above the map canvas; positions are
    /// window coordinates, so the bar height is part of the y offset.
    pub(crate) fn render_menu_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let Some(open) = self.open_menu else {
            return vec![];
        };
        let menus = crate::menus(cx);
        let mut xs = Vec::with_capacity(menus.len());
        let mut x = 0.0f32;
        for m in &menus {
            xs.push(x);
            x += m.name.len() as f32 * 8.0 + 24.0;
        }
        let Some(menu) = menus.into_iter().nth(open) else {
            return vec![];
        };
        let drop_x = xs[open];
        let vp = window.viewport_size();

        let backdrop = deferred(
            anchored()
                .position(point(px(0.), px(BAR_H)))
                .snap_to_window()
                .child(
                    div()
                        .id("menu-backdrop")
                        .w(vp.width)
                        .h(px(f32::from(vp.height) - BAR_H))
                        .on_mouse_down(MouseButton::Left, cx.listener(|v, _e, _w, cx| {
                            v.open_menu = None;
                            v.open_submenu = None;
                            cx.notify();
                        })),
                ),
        )
        .priority(1)
        .into_any_element();

        let mut panel = v_flex()
            .w(px(PANEL_W))
            .bg(rgb(0x2b2b2b))
            .border_1()
            .border_color(rgb(0x555555))
            .rounded_sm()
            .py_1()
            .text_size(px(12.))
            .text_color(rgb(0xdddddd));
        let mut flyout: Option<AnyElement> = None;
        let mut y = 4.0f32; // py_1
        for (ii, item) in menu.items.into_iter().enumerate() {
            let item_top = y;
            match item {
                MenuItem::Separator => {
                    y += SEP_H;
                    panel = panel.child(
                        div().h(px(SEP_H)).flex().items_center().child(
                            div().h(px(1.)).w_full().bg(rgb(0x555555)),
                        ),
                    );
                }
                MenuItem::Submenu(sub) => {
                    y += ROW_H;
                    let sub_open = self.open_submenu == Some(ii);
                    panel = panel.child(menu_row(
                        SharedString::from(format!("menu-item-{open}-{ii}")),
                        SharedString::from(format!("{} ▸", sub.name)),
                        sub_open,
                        cx,
                        move |v, _w, cx| {
                            v.open_submenu = Some(ii);
                            cx.notify();
                        },
                        move |v, cx| {
                            if v.open_submenu != Some(ii) {
                                v.open_submenu = Some(ii);
                                cx.notify();
                            }
                        },
                    ));
                    if sub_open {
                        flyout = Some(
                            deferred(
                                anchored()
                                    .position(point(px(drop_x + PANEL_W), px(BAR_H + item_top)))
                                    .child(self.submenu_panel(sub, open, ii, cx)),
                            )
                            .priority(3)
                            .into_any_element(),
                        );
                    }
                }
                MenuItem::Action { name, action, .. } => {
                    y += ROW_H;
                    // Fn can't move the box — hand it over on first click.
                    let action = std::sync::Mutex::new(Some(action));
                    panel = panel.child(menu_row(
                        SharedString::from(format!("menu-item-{open}-{ii}")),
                        name,
                        false,
                        cx,
                        move |v, w, cx| {
                            v.open_menu = None;
                            v.open_submenu = None;
                            cx.notify();
                            if let Some(a) = action.lock().unwrap().take() {
                                w.dispatch_action(a, cx);
                            }
                        },
                        |_, _| {},
                    ));
                }
                MenuItem::SystemMenu(_) => {}
            }
        }

        let dropdown = deferred(
            anchored()
                .position(point(px(drop_x), px(BAR_H)))
                .child(panel),
        )
        .priority(2)
        .into_any_element();

        let mut out = vec![backdrop, dropdown];
        out.extend(flyout);
        out
    }

    /// A submenu's flyout panel. Positioned by the caller's anchored().
    fn submenu_panel(
        &mut self,
        sub: Menu,
        top: usize,
        sub_idx: usize,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let mut panel = v_flex()
            .w(px(PANEL_W))
            .bg(rgb(0x2b2b2b))
            .border_1()
            .border_color(rgb(0x555555))
            .rounded_sm()
            .py_1()
            .text_size(px(12.))
            .text_color(rgb(0xdddddd));
        for (si, item) in sub.items.into_iter().enumerate() {
            match item {
                MenuItem::Separator => {
                    panel = panel.child(
                        div().h(px(SEP_H)).flex().items_center().child(
                            div().h(px(1.)).w_full().bg(rgb(0x555555)),
                        ),
                    );
                }
                MenuItem::Action { name, action, .. } => {
                    let action = std::sync::Mutex::new(Some(action));
                    panel = panel.child(menu_row(
                        SharedString::from(format!("menu-sub-{top}-{sub_idx}-{si}")),
                        name,
                        false,
                        cx,
                        move |v, w, cx| {
                            v.open_menu = None;
                            v.open_submenu = None;
                            cx.notify();
                            if let Some(a) = action.lock().unwrap().take() {
                                w.dispatch_action(a, cx);
                            }
                        },
                        |_, _| {},
                    ));
                }
                _ => {}
            }
        }
        panel
    }
}

/// One clickable dropdown row. `on_click` fires on mouse-down; `on_hover`
/// fires on mouse-enter (used to open submenu flyouts without a click).
fn menu_row(
    id: impl Into<ElementId>,
    label: SharedString,
    active: bool,
    cx: &mut Context<MainView>,
    on_click: impl Fn(&mut MainView, &mut Window, &mut Context<MainView>) + 'static,
    on_hover: impl Fn(&mut MainView, &mut Context<MainView>) + 'static,
) -> Stateful<Div> {
    div()
        .id(id)
        .h(px(ROW_H))
        .px_3()
        .flex()
        .items_center()
        .bg(if active { rgb(0x4a4d4f) } else { rgb(0x2b2b2b) })
        .hover(|s| s.bg(rgb(0x3c3f41)))
        .cursor_pointer()
        .on_mouse_move(cx.listener(move |v, _e, _w, cx| on_hover(v, cx)))
        .on_mouse_down(MouseButton::Left, cx.listener(move |v, _e, w, cx| on_click(v, w, cx)))
        .child(label)
}
