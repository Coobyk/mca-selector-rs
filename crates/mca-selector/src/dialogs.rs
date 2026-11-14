//! Dialog state, keyboard handling, rendering and widget helpers
//! (upstream modal Alert/Dialog chains: GotoBox, FilterChunksDialog,
//! ChangeNBTDialog, ImportConfirmationDialog, OverlayEditorDialog,
//! ProgressDialog …).

use crate::config;
use crate::paint::nbt_icon;
use crate::view::MainView;
use gpui::*;
use mc_core::point::Point2i;
use std::path::PathBuf;

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
    pub(crate) fn missing(note: &str) -> Self {
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
    /// Find-structure locator: background scan batches stream into
    /// `entries` as (canonical name, chunk-as-long) while `scanning`.
    StructureLocator {
        entries: Vec<(String, i64)>,
        scanning: bool,
        sel: usize,
        filter: String,
    },
}

impl Dialog {
    /// short name for trace logging — avoids Debug-formatting the payloads.
    pub(crate) fn name(&self) -> &'static str {
        match self {
            Dialog::Goto { .. } => "Goto",
            Dialog::About => "About",
            Dialog::Error { .. } => "Error",
            Dialog::Confirm { .. } => "Confirm",
            Dialog::SelectDimension { .. } => "SelectDimension",
            Dialog::Settings { .. } => "Settings",
            Dialog::RenderSettings { .. } => "RenderSettings",
            Dialog::Info { .. } => "Info",
            Dialog::PasteConfirm { .. } => "PasteConfirm",
            Dialog::JobProgress => "JobProgress",
            Dialog::NbtEditor { .. } => "NbtEditor",
            Dialog::TextInput { .. } => "TextInput",
            Dialog::Filter { .. } => "Filter",
            Dialog::ChangeFields { .. } => "ChangeFields",
            Dialog::Import { .. } => "Import",
            Dialog::OverlayEditor { .. } => "OverlayEditor",
            Dialog::StructureLocator { .. } => "StructureLocator",
        }
    }
}

/// locator list model: (display name, canonical name, chunk, block dist)
/// sorted by distance from the view center and name-filtered.
fn locator_rows(
    entries: &[(String, i64)],
    filter: &str,
    center: Point2i,
) -> Vec<(String, String, Point2i, f32)> {
    let reg = mc_core::structures::structures();
    let f = filter.to_lowercase();
    let mut rows: Vec<_> = entries
        .iter()
        .filter_map(|(name, long)| {
            let c = Point2i::from_long(*long);
            let disp = reg
                .icons
                .get(name)
                .map(|d| d.display.clone())
                .unwrap_or_else(|| name.clone());
            if !f.is_empty() && !disp.to_lowercase().contains(&f) && !name.to_lowercase().contains(&f)
            {
                return None;
            }
            let dx = c.x * 16 + 8 - center.x;
            let dz = c.z * 16 + 8 - center.z;
            Some((disp, name.clone(), c, (dx * dx + dz * dz) as f32))
        })
        .collect();
    rows.sort_by(|a, b| a.3.partial_cmp(&b.3).unwrap_or(std::cmp::Ordering::Equal));
    rows
}

// -- keyboard ---------------------------------------------------------------

impl MainView {
    pub(crate) fn dialog_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let k = event.keystroke.key.as_str();
        eprintln!(
            "[mca] dialog_key {k} dialog={}",
            self.dialog.as_ref().map(|d| d.name()).unwrap_or("-")
        );
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
        if let Some(Dialog::StructureLocator { entries, sel, filter, .. }) = &mut self.dialog {
            let center = self.map.center_block();
            match k {
                "escape" => self.dialog = None,
                "up" => *sel = sel.saturating_sub(1),
                "down" => {
                    let n = locator_rows(entries, filter, center).len();
                    *sel = (*sel + 1).min(n.saturating_sub(1));
                }
                "pageup" => *sel = sel.saturating_sub(20),
                "pagedown" => {
                    let n = locator_rows(entries, filter, center).len();
                    *sel = (*sel + 20).min(n.saturating_sub(1));
                }
                "backspace" => {
                    filter.pop();
                    *sel = 0;
                }
                "enter" => {
                    if let Some(Dialog::StructureLocator { entries, sel, filter, .. }) = &self.dialog {
                        let rows = locator_rows(entries, filter, center);
                        if let Some((_, _, c, _)) = rows.get(*sel) {
                            let (bx, bz) = (c.x * 16 + 8, c.z * 16 + 8);
                            self.map.go_to(bx, bz);
                            self.request_update();
                            self.dialog = None;
                        }
                    }
                }
                _ => {
                    let c = &event.keystroke.key_char;
                    if let Some(c) = c {
                        for ch in c.chars() {
                            if !ch.is_control() {
                                filter.push(ch);
                            }
                        }
                        *sel = 0;
                    } else if k == "space" {
                        filter.push(' ');
                        *sel = 0;
                    }
                }
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
}

// -- rendering --------------------------------------------------------------

impl MainView {
    pub(crate) fn render_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let d = self.dialog.as_ref()?;
        let vp = window.viewport_size();
        let overlay = div()
            .id("modal-backdrop")
            .w(vp.width)
            .h(vp.height)
            .occlude()
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
                        .child(text_el(crate::lang::t("dialog.title.goto"), 16.))
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
                                .child(button(crate::lang::t("dialog.go_to"), cx, |v, _w, cx| {
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
                                .child(button(crate::lang::t("dialog.cancel"), cx, |v, _w, cx| {
                                    v.dialog = None;
                                    cx.notify();
                                })),
                        ),
                )
            }
            Dialog::About => panel(
                v_flex().gap_2()
                    .child(text_el(crate::lang::t("dialog.title.about"), 16.))
                    .child(text_el(concat!("Version: ", env!("CARGO_PKG_VERSION"), " (Rust + GPUI port)"), 12.))
                    .child(text_el("License: MIT", 12.))
                    .child(text_el("Copyright: © Querz", 12.))
                    .child(text_el("Source: https://github.com/Querz/mcaselector", 12.))
                    .child(text_el("NOT AN OFFICIAL MINECRAFT PRODUCT. NOT APPROVED BY OR ASSOCIATED WITH MOJANG OR MICROSOFT.", 10.))
                    .child(button(crate::lang::t("dialog.close"), cx, |v, _w, cx| {
                        v.dialog = None;
                        cx.notify();
                    })),
            ),
            Dialog::Error { msg } => panel(
                v_flex().gap_2()
                    .child(text_el(crate::lang::t("dialog.title.error"), 16.))
                    .child(text_el(msg.clone(), 12.))
                    .child(
                        h_flex().gap_2()
                            .child(button(crate::lang::t("dialog.ok"), cx, |v, _w, cx| {
                                v.dialog = None;
                                cx.notify();
                            }))
                            .child(button(crate::lang::t("dialog.copy"), cx, |v, _w, cx| {
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
                            .child(button(crate::lang::t("dialog.import"), cx, |v, w, cx| {
                                if let Some(Dialog::PasteConfirm { ox, oz, overwrite, sel_only, .. }) = v.dialog.take() {
                                    v.do_paste_import(ox, oz, overwrite, sel_only, cx);
                                }
                                let _ = w;
                            }))
                            .child(button(crate::lang::t("dialog.cancel"), cx, |v, _w, cx| {
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
                            .child(button(crate::lang::t("dialog.yes"), cx, |v, w, cx| {
                                if let Some(Dialog::Confirm { action, .. }) = v.dialog.take() {
                                    (action)(v, w, cx);
                                }
                                cx.notify();
                            }))
                            .child(button(crate::lang::t("dialog.no"), cx, |v, _w, cx| {
                                v.dialog = None;
                                cx.notify();
                            })),
                    ),
            ),
            Dialog::Settings { draft } => panel(
                v_flex().gap_1()
                    .child(text_el(crate::lang::t("dialog.title.settings"), 16.))
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
                        h_flex().gap_1()
                            .child(text_el(format!("{}:", crate::lang::t("settings.language")), 12.))
                            .child(
                                div()
                                    .id("gs-language")
                                    .px_2().py_1()
                                    .bg(rgb(0x3c3f41))
                                    .border_1().border_color(rgb(0x555555))
                                    .rounded_sm()
                                    .cursor_pointer()
                                    .text_size(px(12.))
                                    .child(crate::lang::display_name(&draft.language))
                                    .on_click(cx.listener(|v, _, _w, cx| {
                                        if let Some(Dialog::Settings { draft }) = &mut v.dialog {
                                            draft.language = crate::lang::next_code(&draft.language).to_string();
                                            cx.notify();
                                        }
                                    })),
                            ),
                    )
                    .child(
                        h_flex().gap_2()
                            .child(button(crate::lang::t("dialog.save"), cx, |v, _w, cx| {
                                if let Some(Dialog::Settings { draft }) = v.dialog.take() {
                                    let lang = draft.language.clone();
                                    config::set_global(draft);
                                    crate::lang::init(&lang);
                                    cx.set_menus(crate::menus(cx));
                                    v.map.mark_all_tiles_obsolete();
                                    // mark_all_tiles_obsolete alone does not
                                    // schedule a re-render — request_update
                                    // makes the new settings take effect at
                                    // the next frame.
                                    v.request_update();
                                }
                                cx.notify();
                            }))
                            .child(button(crate::lang::t("dialog.cancel"), cx, |v, _w, cx| {
                                v.dialog = None;
                                cx.notify();
                            })),
                    ),
            ),
            Dialog::RenderSettings { draft } => panel(
                v_flex().gap_1()
                    .child(text_el(crate::lang::t("dialog.title.render_settings"), 16.))
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
                            .child(button(crate::lang::t("dialog.save"), cx, |v, _w, cx| {
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
                            .child(button(crate::lang::t("dialog.cancel"), cx, |v, _w, cx| {
                                v.dialog = None;
                                cx.notify();
                            })),
                    ),
            ),
            Dialog::SelectDimension { dims } => {
                let mut list = v_flex().gap_1().child(text_el(crate::lang::t("dialog.title.select_dimension"), 16.));
                let all_dims = dims.clone();
                for d in dims {
                    // upstream FileNameListCell shows file.getName(), not the path
                    let label = d
                        .file_name()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_else(|| d.display().to_string());
                    let dp = d.clone();
                    let dp_log = d.clone();
                    let dims_for_click = all_dims.clone();
                    list = list.child(
                        div()
                            .id(SharedString::from(label.clone()))
                            .px_2().py_1().rounded_sm()
                            .hover(|s| s.bg(rgb(0x3a3a3a)))
                            .child(text_el(label, 12.))
                            .on_mouse_down(MouseButton::Left, cx.listener(move |_v, e: &MouseDownEvent, _w, _cx| {
                                eprintln!(
                                    "[mca] dim row mousedown {} ({}, {})",
                                    dp_log.display(),
                                    f32::from(e.position.x) as i32,
                                    f32::from(e.position.y) as i32
                                );
                            }))
                            .on_click(cx.listener(move |v, _, _w, cx| {
                                eprintln!("[mca] dimension picked: {}", dp.display());
                                // close the picker FIRST — after do_open,
                                // `dialog = None` used to erase its error
                                // dialog, turning a failed open into a
                                // silent nothing.
                                v.dialog = None;
                                v.do_open(dp.clone(), dims_for_click.clone(), cx);
                                cx.notify();
                            })),
                    );
                }
                panel(list.child(button(crate::lang::t("dialog.cancel"), cx, |v, _w, cx| {
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
                    .map(|r| crate::nbt_editor::flatten(r, &t.expanded))
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
                    } else if !r.named {
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
                            .child(button(crate::lang::t("dialog.save"), cx, |v, _w, cx| v.nbt_editor_save(cx)))
                            .child(button(crate::lang::t("dialog.close"), cx, |v, _w, cx| {
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
                            .child(button(crate::lang::t("dialog.ok"), cx, |v, _w, cx| {
                                v.dialog = None;
                                cx.notify();
                            }))
                            .child(button(crate::lang::t("dialog.copy"), cx, |v, _w, cx| {
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
                        .child(button(crate::lang::t("dialog.cancel"), cx, |v, _w, cx| {
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
                            .child(button(crate::lang::t("dialog.ok"), cx, |v, w, cx| {
                                if let Some(Dialog::TextInput { text, on_done, .. }) = v.dialog.take() {
                                    (on_done)(v, w, cx, text);
                                }
                                cx.notify();
                            }))
                            .child(button(crate::lang::t("dialog.cancel"), cx, |v, _w, cx| {
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
                                .child(button(crate::lang::t("dialog.ok"), cx, move |v, w, cx| {
                                    // upstream disables OK on an invalid filter
                                    if ok_enabled {
                                        if let Some(Dialog::Filter { query, action, apply, radius, sel_only, .. }) = v.dialog.take() {
                                            v.run_filter_dialog(query, action, apply, radius, sel_only, w, cx);
                                        }
                                    }
                                    cx.notify();
                                }))
                                .child(button(crate::lang::t("dialog.cancel"), cx, |v, _w, cx| {
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
                                .child(button(crate::lang::t("dialog.ok"), cx, move |v, _w, cx| {
                                    // upstream disables OK on invalid input
                                    if ok_enabled {
                                        if let Some(Dialog::ChangeFields { text, force, sel_only, .. }) = v.dialog.take() {
                                            v.run_change_fields_dialog(text, force, sel_only, cx);
                                        }
                                    }
                                    cx.notify();
                                }))
                                .child(button(crate::lang::t("dialog.cancel"), cx, |v, _w, cx| {
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
                                .child(button(crate::lang::t("dialog.import"), cx, |v, _w, cx| {
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
                                .child(button(crate::lang::t("dialog.cancel"), cx, |v, _w, cx| {
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
                            .child(button(crate::lang::t("dialog.add_overlay"), cx, |v, _w, cx| {
                                if let Some(Dialog::OverlayEditor { rows, .. }) = &mut v.dialog {
                                    rows.push(default_overlay_row());
                                }
                                cx.notify();
                            }))
                            .child(button(crate::lang::t("dialog.ok"), cx, |v, _w, cx| {
                                if let Some(Dialog::OverlayEditor { rows, .. }) = v.dialog.take() {
                                    v.apply_overlay_editor(rows, cx);
                                }
                                cx.notify();
                            }))
                            .child(button(crate::lang::t("dialog.cancel"), cx, |v, _w, cx| {
                                v.dialog = None;
                                cx.notify();
                            })),
                    )
            }
            Dialog::StructureLocator { entries, scanning, sel, filter } => {
                let center = self.map.center_block();
                let rows = locator_rows(entries, filter, center);
                let n = rows.len();
                let sel = (*sel).min(n.saturating_sub(1));
                const VIS: usize = 18;
                let start = if sel >= VIS { sel + 1 - VIS } else { 0 };
                let mut list = v_flex().h(px(VIS as f32 * 22. + 8.)).overflow_hidden().gap_0p5();
                for (i, (disp, _canon, c, dist2)) in rows.iter().enumerate().skip(start).take(VIS) {
                    let (bx, bz) = (c.x * 16 + 8, c.z * 16 + 8);
                    let label = format!(
                        "{}   {},{}   ({} blocks)",
                        disp,
                        bx,
                        bz,
                        (*dist2).sqrt() as i32,
                    );
                    let selected = i == sel;
                    let mut row = div()
                        .id(SharedString::from(format!("loc-{i}")))
                        .px_2()
                        .py_0p5()
                        .rounded_sm()
                        .bg(if selected { rgb(0x3d5a99) } else { rgba(0x00000000) });
                    if !selected {
                        row = row.hover(|s| s.bg(rgb(0x3a3a3a)));
                    }
                    list = list.child(
                        row.child(text_el(label, 12.))
                            .on_click(cx.listener(move |v, _, _w, cx| {
                                v.map.go_to(bx, bz);
                                v.request_update();
                                v.dialog = None;
                                cx.notify();
                            })),
                    );
                }
                panel(
                    v_flex()
                        .gap_2()
                        .child(text_el("Find structure", 16.))
                        .child(text_el(
                            format!(
                                "filter: {filter}_   —   {} found{}",
                                n,
                                if *scanning { ", scanning…" } else { "" },
                            ),
                            11.,
                        ))
                        .child(list)
                        .child(text_el("↑/↓/pgup/pgdn navigate, enter jumps, type to filter", 9.))
                        .child(h_flex().gap_2().child(button(crate::lang::t("dialog.close"), cx, |v, _w, cx| {
                            v.dialog = None;
                            cx.notify();
                        }))),
                )
            }
        };
        let overlay = overlay
            .on_mouse_down(MouseButton::Left, cx.listener(|v, e: &MouseDownEvent, _w, _cx| {
                // upstream dialogs are modal — an outside click never
                // dismisses them (SelectDimension used to vanish here).
                eprintln!(
                    "[mca] backdrop mousedown ({}, {}) dialog={}",
                    f32::from(e.position.x) as i32,
                    f32::from(e.position.y) as i32,
                    v.dialog.as_ref().map(|d| d.name()).unwrap_or("-")
                );
            }))
            .child(body);
        Some(
            deferred(
                anchored()
                    .position(point(px(0.), px(0.)))
                    .snap_to_window()
                    .child(overlay),
            )
            .priority(10)
            .into_any_element(),
        )
    }
}

// -- settings/overlay editor support ----------------------------------------

impl MainView {
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

    /// upstream OverlayEditorDialog — the port is modal: rows are applied and
    /// persisted on OK instead of live-editing the TileMap.
    pub(crate) fn edit_overlays(&mut self, cx: &mut Context<Self>) {
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

    pub(crate) fn apply_overlay_editor(&mut self, rows: Vec<OverlayRow>, cx: &mut Context<Self>) {
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

    /// open the structure locator and kick off a background scan of every
    /// region file — cached structure data is reused, uncached regions are
    /// parsed once and backfill the cache (which also speeds up icons).
    pub(crate) fn find_structure(&mut self, cx: &mut Context<Self>) {
        let Some(ws) = config::world() else { return };
        self.dialog = Some(Dialog::StructureLocator {
            entries: Vec::new(),
            scanning: true,
            sel: 0,
            filter: String::new(),
        });
        let regions: Vec<i64> = self.map.regions.iter().copied().collect();
        let dir = ws.region_dir().to_path_buf();
        let uuid = ws.uuid.clone();
        let tx = self.map.result_tx.clone();
        mc_core::jobs::pools().add_process(mc_core::jobs::Job::new(
            mc_core::jobs::PRIORITY_LOW,
            None,
            move || {
                let mut batch: Vec<(String, i64)> = Vec::new();
                for (i, key) in regions.iter().enumerate() {
                    let r = Point2i::from_long(*key);
                    let found = mc_core::cache::get_structures(&uuid, r).or_else(|| {
                        let path = dir.join(mc_core::files::mca_file_name(r));
                        let mut m = mc_core::mca::McaFile::new(path).ok()?;
                        m.load(true).ok()?;
                        let s = m.structures();
                        mc_core::cache::set_structures(&uuid, r, &s);
                        Some(s)
                    });
                    if let Some(map) = found {
                        for (chunk_long, names) in map {
                            for name in names {
                                batch.push((name, chunk_long));
                            }
                        }
                    }
                    // stream every ~8 regions so the dialog fills live
                    if i % 8 == 7 {
                        let _ = tx.send(crate::tilemap::TileResult::StructureScan {
                            entries: std::mem::take(&mut batch),
                            done: false,
                        });
                    }
                }
                let _ = tx.send(crate::tilemap::TileResult::StructureScan {
                    entries: batch,
                    done: true,
                });
            },
        ));
        cx.notify();
    }
}

// -- widget helpers ----------------------------------------------------------

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

pub(crate) fn v_flex() -> Div { div().flex().flex_col() }
pub(crate) fn h_flex() -> Div { div().flex() }

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
