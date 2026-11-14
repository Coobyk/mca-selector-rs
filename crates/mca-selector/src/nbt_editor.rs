//! Port of ui.component.NBTTreeView: flattened tag tree with expand/collapse,
//! value editing via SNBT, add/remove tags.

use mc_core::nbt::{self, Compound, Tag};
use std::collections::HashSet;

pub const ICON_NAMES: [&str; 13] = [
    "end", "byte", "short", "int", "long", "float", "double", "byte_array",
    "string", "list", "compound", "int_array", "long_array",
];

#[derive(Clone)]
pub struct NbtRow {
    pub depth: usize,
    pub name: String,
    pub value: String,
    /// path into root, e.g. "sections[0].block_states"
    pub path: String,
    pub container: bool,
    pub expanded: bool,
    pub type_id: u8,
    /// true for named compound entries, false for list items (`[i]`) — list
    /// items cannot be renamed and display without a colon separator.
    pub named: bool,
}

/// flatten the tag tree honoring `expanded` set (paths of open containers)
pub fn flatten(root: &Compound, expanded: &HashSet<String>) -> Vec<NbtRow> {
    let mut rows = Vec::new();
    for (name, tag) in root {
        push_row(&mut rows, expanded, name, tag, "", 0, false);
    }
    rows
}

fn push_row(
    rows: &mut Vec<NbtRow>,
    expanded: &HashSet<String>,
    name: &str,
    tag: &Tag,
    parent: &str,
    depth: usize,
    list_item: bool,
) {
    // escape compound keys that collide with the path grammar: `[x]` is a
    // literal segment (dots stay literal inside), `\]`/`\\` escape inside.
    // list indices are passed already bracketed as `[i]` and need no escaping.
    let seg = if list_item {
        name.to_string()
    } else if name.contains(|c| matches!(c, '.' | '[' | ']' | '\\')) {
        format!("[{}]", name.replace('\\', "\\\\").replace(']', "\\]"))
    } else {
        name.to_string()
    };
    let path = if parent.is_empty() {
        seg
    } else if seg.starts_with('[') {
        format!("{parent}{seg}")
    } else {
        format!("{parent}.{seg}")
    };
    let (container, value) = match tag {
        Tag::Compound(c) => (true, format!("{} entries", c.len())),
        Tag::List(t, l) => (
            true,
            format!("{} entries of type {}", l.len(), icon_name(*t)),
        ),
        Tag::ByteArray(b) => (false, format!("[B;{} bytes]", b.len())),
        Tag::IntArray(v) => (false, format!("[I;{} ints]", v.len())),
        Tag::LongArray(v) => (false, format!("[L;{} longs]", v.len())),
        _ => (false, nbt::to_snbt(tag)),
    };
    let open = expanded.contains(&path);
    rows.push(NbtRow {
        depth,
        name: name.to_string(),
        value,
        path: path.clone(),
        container,
        expanded: open,
        type_id: tag.id(),
        named: !list_item,
    });
    if !open {
        return;
    }
    match tag {
        Tag::Compound(c) => {
            for (n, t) in c {
                push_row(rows, expanded, n, t, &path, depth + 1, false);
            }
        }
        Tag::List(_, l) => {
            for (i, t) in l.iter().enumerate() {
                push_row(rows, expanded, &format!("[{i}]"), t, &path, depth + 1, true);
            }
        }
        _ => {}
    }
}

pub fn icon_name(type_id: u8) -> &'static str {
    ICON_NAMES.get(type_id as usize).copied().unwrap_or("end")
}

/// a scalar-ish display value for the edit field (full SNBT for the tag)
pub fn tag_edit_text(t: &Tag) -> String {
    nbt::to_snbt(t)
}

/// apply a value edit: parse SNBT and replace the tag at `path`.
/// Container types are preserved only if the new value has the same id.
pub fn apply_value_edit(root: &mut Compound, path: &str, raw: &str) -> Result<(), String> {
    let new = nbt::parse_snbt(raw)?;
    let Some(old) = nbt::path_get_mut(root, path) else {
        return Err(format!("no tag at '{path}'"));
    };
    *old = new;
    Ok(())
}

/// apply an add: `name = snbt` into the container at `container_path`
/// (empty = root).
pub fn apply_add(
    root: &mut Compound,
    container_path: &str,
    raw: &str,
) -> Result<(), String> {
    let (name, val) = raw
        .split_once('=')
        .ok_or_else(|| "expected: name = value".to_string())?;
    let name = name.trim();
    if name.is_empty() {
        return Err("empty name".into());
    }
    let tag = nbt::parse_snbt(val)?;
    nbt::path_insert(root, container_path, name, tag)
}

/// apply a delete of the tag at `path`.
pub fn apply_delete(root: &mut Compound, path: &str) -> bool {
    nbt::path_remove(root, path)
}

/// byte index where the last path segment starts. Bracket segments are
/// literals and may contain dots, so a plain `rfind('.')` can split inside
/// `[...]` — the walk below mirrors `nbt::path_segments`' bracket/escape
/// handling and only treats dots/brackets OUTSIDE a literal as boundaries.
fn last_segment_start(path: &str) -> usize {
    let b = path.as_bytes();
    let n = b.len();
    let mut i = 0;
    let mut start = 0;
    while i < n {
        match b[i] {
            b'.' => {
                i += 1;
                start = i;
            }
            b'[' => {
                start = i;
                i += 1;
                while i < n {
                    match b[i] {
                        b'\\' => i += 2,
                        b']' => {
                            i += 1;
                            break;
                        }
                        _ => i += 1,
                    }
                }
            }
            _ => i += 1,
        }
    }
    start
}

/// (parent path, leaf segment) — leaf may be a `...]`-quoted literal or a
/// list index like `[3]`; callers that rename check `leaf.starts_with('[')`.
pub fn parent_path(path: &str) -> (&str, &str) {
    let s = last_segment_start(path);
    path.split_at(s)
}

/// decode a `...]`-quoted leaf segment back to its literal key, honoring the
/// `\]` / `\\` escapes of `nbt::path_segments`. Plain segments pass through.
fn unquote_leaf(seg: &str) -> String {
    if !seg.starts_with('[') || !seg.ends_with(']') {
        return seg.to_string();
    }
    let inner = &seg[1..seg.len() - 1];
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some(n) if n == ']' || n == '\\' => out.push(n),
                Some(n) => {
                    out.push('\\');
                    out.push(n);
                }
                None => out.push('\\'),
            },
            _ => out.push(c),
        }
    }
    out
}

/// rename a key inside a compound (path of the tag -> new leaf name)
pub fn apply_rename(root: &mut Compound, path: &str, new_name: &str) -> Result<(), String> {
    let new_name = new_name.trim();
    if new_name.is_empty() {
        return Err("empty name".into());
    }
    let (pp, leaf) = parent_path(path);
    // a `[x]` leaf is a literal: it may be a list index OR an escaped compound
    // key — the parent container decides which (mirrors `nbt::path_get_mut`)
    let container: &mut Compound = if pp.is_empty() {
        root
    } else {
        match nbt::path_get_mut(root, pp) {
            Some(Tag::Compound(c)) => c,
            _ => return Err("cannot rename list items".into()),
        }
    };
    let old_key = unquote_leaf(leaf);
    let Some(v) = container.shift_remove(&old_key) else {
        return Err(format!("no tag at '{path}'"));
    };
    container.insert(new_name.to_string(), v);
    Ok(())
}

// -- the editor dialog (upstream NBTEditorDialog) -----------------------------
// The Dialog state lives in dialogs::Dialog::NbtEditor; these methods hold
// the keyboard handling plus the edit/save actions so the whole tree
// interaction stays next to the flatten/path helpers it depends on.

use crate::dialogs::{Dialog, NbtEdit, NbtTab};
use crate::view::MainView;
use gpui::{Context, KeyDownEvent};
use mc_core::point::Point2i;
use std::path::PathBuf;

impl MainView {
    /// keyboard handling for the NBT tree editor (NBTTreeView edit mode).
    /// "1"/"2"/"3" switch the region/poi/entities tab (upstream NBTEditorDialog
    /// tab pane); everything else acts on the current tab.
    pub(crate) fn nbt_editor_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
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
                            let path = flatten(root, &t.expanded)
                                .get(t.sel)
                                .map(|r| r.path.clone())
                                .unwrap_or_default();
                            match mode {
                                NbtEdit::Value => apply_value_edit(root, &path, buf),
                                NbtEdit::Add => {
                                    let rows = flatten(root, &t.expanded);
                                    let container_path: String = rows
                                        .get(t.sel)
                                        .map(|r| {
                                            if r.container {
                                                r.path.clone()
                                            } else {
                                                // bracket-aware: a leaf like
                                                // `a.b[0]` has parent `a.b`;
                                                // rfind('.') would split
                                                // inside a literal `[x.y]`.
                                                parent_path(&r.path).0.to_string()
                                            }
                                        })
                                        .unwrap_or_default();
                                    apply_add(root, &container_path, buf)
                                }
                                NbtEdit::Rename => apply_rename(root, &path, buf),
                                // NbtEdit::None is unreachable — the guard
                                // above only enters when editing != None.
                                NbtEdit::None => unreachable!(),
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
        let rows = flatten(root, expanded);
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
                    } else {
                        // jump to the parent row — bracket-aware like
                        // apply_rename (a literal `[x.y]` is one segment).
                        let parent = parent_path(&r.path).0.to_string();
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
                            .map(tag_edit_text)
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
                    if r.named {
                        *editing = NbtEdit::Rename;
                        *buf = r.name.clone();
                    }
                }
            }
            "delete" => {
                if let Some(r) = rows.get(*sel) {
                    if apply_delete(root, &r.path) {
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

    /// double-click / enter on a row: containers toggle, leaves open the
    /// value editor (same semantics as the "enter"/"e" key path).
    pub(crate) fn nbt_row_activate(&mut self, idx: usize, cx: &mut Context<Self>) {
        let Some(Dialog::NbtEditor { tab, tabs, editing, buf, .. }) = &mut self.dialog else {
            return;
        };
        let t = &mut tabs[*tab];
        let Some(root) = t.root.as_mut() else { return };
        let rows = flatten(root, &t.expanded);
        t.sel = idx.min(rows.len().saturating_sub(1));
        let Some(r) = rows.get(t.sel) else { return };
        if r.container {
            if r.expanded {
                t.expanded.remove(&r.path);
            } else {
                t.expanded.insert(r.path.clone());
            }
        } else {
            *editing = NbtEdit::Value;
            *buf = mc_core::nbt::path_get(root, &r.path)
                .map(tag_edit_text)
                .unwrap_or_default();
        }
        cx.notify();
    }

    /// wheel-scroll inside the row list — moves the viewport, not the
    /// selection (JScrollPane semantics).
    pub(crate) fn nbt_scroll(&mut self, delta_rows: i64, cx: &mut Context<Self>) {
        let Some(Dialog::NbtEditor { tab, tabs, .. }) = &mut self.dialog else {
            return;
        };
        let t = &mut tabs[*tab];
        let n = t.root.as_ref()
            .map(|r| flatten(r, &t.expanded).len())
            .unwrap_or(0);
        let max = n.saturating_sub(22) as i64;
        t.scroll = (t.scroll as i64 + delta_rows).clamp(0, max.max(0)) as usize;
        cx.notify();
    }

    /// Tools > Edit chunk (upstream editNBT): needs exactly 1 selected chunk.
    pub(crate) fn edit_nbt(&mut self, cx: &mut Context<Self>) {
        let sel = &self.map.selection;
        if sel.inverted || self.map.selected_chunks != 1 {
            self.dialog = Some(Dialog::Error {
                msg: "select exactly one chunk to edit".into(),
            });
            cx.notify();
            return;
        }
        let mut found: Option<(i64, usize)> = None;
        for (rl, cs) in sel.regions() {
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
        let Some(dir) = crate::config::world().and_then(|w| w.dirs.region.clone()) else { return };
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
                    NbtTab { root: Some(d), expanded, sel: 0, scroll: 0, dirty: false, note: None }
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
                                NbtTab { root: Some(d), expanded, sel: 0, scroll: 0, dirty: false, note: None }
                            }
                            None if p.exists() => NbtTab::missing("no chunk data found"),
                            _ => NbtTab::missing("region file does not exist"),
                        }
                    }
                },
            }
        };
        let wdirs = crate::config::world().map(|w| w.dirs.clone()).unwrap_or_default();
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

    /// save every dirty tab back to its mca file through the shared batch
    /// runner — all three files of the region load in one pass and only the
    /// touched ones are written back.
    pub(crate) fn nbt_editor_save(&mut self, cx: &mut Context<Self>) {
        let Some(Dialog::NbtEditor { region, chunk_abs, index, tab, mut tabs, status, .. }) = self.dialog.take() else {
            return;
        };
        // region/poi/entities roots in tab order
        let mut writes: [Option<mc_core::nbt::Compound>; 3] = [None, None, None];
        for (i, t) in tabs.iter_mut().enumerate() {
            if t.dirty {
                writes[i] = t.root.clone();
                t.dirty = false;
            }
        }
        let saving = writes.iter().any(|w| w.is_some());
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
        if !saving {
            cx.notify();
            return;
        }
        let rp = Point2i::from_long(region);
        let Some(dirs) = crate::config::world().map(|w| w.dirs.clone()) else {
            cx.notify();
            return;
        };
        let tx = self.map.result_tx.clone();
        mc_core::batch::spawn_region_job(
            "Saving chunk",
            dirs,
            mc_core::selection::Selection::all(),
            vec![rp],
            false,
            Box::new(move |region| {
                let _ = tx.send(crate::tilemap::TileResult::Invalidate { region });
            }),
            Box::new(|_| {}),
            move |files, ctx, _errs| {
                for (i, w) in writes.iter().enumerate() {
                    let Some(root) = w else { continue };
                    let kind = match i {
                        0 => mc_core::batch::McKind::Region,
                        1 => mc_core::batch::McKind::Poi,
                        _ => mc_core::batch::McKind::Entities,
                    };
                    let m = match i {
                        0 => files.region.as_mut(),
                        1 => files.poi.as_mut(),
                        _ => files.entities.as_mut(),
                    };
                    let Some(m) = m else { continue };
                    // upstream saveSingleChunk creates the chunk entry when
                    // the file has none at the index.
                    let c = m.chunks[index].get_or_insert_with(|| {
                        let loc = ctx
                            .region
                            .region_to_chunk()
                            .add_p(Point2i::from_chunk_index(index as i32));
                        mc_core::mca::Chunk::new(loc)
                    });
                    c.data = Some(root.clone());
                    c.timestamp = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|t| t.as_secs() as i32)
                        .unwrap_or(0);
                    files.touch(kind);
                }
            },
        );
        cx.notify();
    }
}
