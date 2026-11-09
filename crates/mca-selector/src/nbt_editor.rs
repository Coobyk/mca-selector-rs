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
}

/// flatten the tag tree honoring `expanded` set (paths of open containers)
pub fn flatten(root: &Compound, expanded: &HashSet<String>) -> Vec<NbtRow> {
    let mut rows = Vec::new();
    for (name, tag) in root {
        push_row(&mut rows, expanded, name, tag, "", 0);
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
) {
    let path = if parent.is_empty() {
        name.to_string()
    } else if name.starts_with('[') {
        format!("{parent}{name}")
    } else {
        format!("{parent}.{name}")
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
    });
    if !open {
        return;
    }
    match tag {
        Tag::Compound(c) => {
            for (n, t) in c {
                push_row(rows, expanded, n, t, &path, depth + 1);
            }
        }
        Tag::List(_, l) => {
            for (i, t) in l.iter().enumerate() {
                push_row(rows, expanded, &format!("[{i}]"), t, &path, depth + 1);
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

/// rename a key inside a compound (path of the tag -> new leaf name)
pub fn apply_rename(root: &mut Compound, path: &str, new_name: &str) -> Result<(), String> {
    let new_name = new_name.trim();
    if new_name.is_empty() {
        return Err("empty name".into());
    }
    let Some(sep) = path.rfind('.') else {
        let old_key = path.to_string();
        let Some(v) = root.shift_remove(&old_key) else {
            return Err(format!("no tag at '{path}'"));
        };
        root.insert(new_name.to_string(), v);
        return Ok(());
    };
    let (parent_path, leaf) = path.split_at(sep);
    let leaf = &leaf[1..];
    if leaf.starts_with('[') {
        return Err("cannot rename list items".into());
    }
    let Some(Tag::Compound(c)) = nbt::path_get_mut(root, parent_path) else {
        return Err(format!("no tag at '{path}'"));
    };
    let Some(v) = c.shift_remove(leaf) else {
        return Err(format!("no tag at '{path}'"));
    };
    c.insert(new_name.to_string(), v);
    Ok(())
}
