//! Status bar text (upstream StatusBar): hovered block/chunk/region,
//! selection count, tile/queue counters, overlay + structure readouts and
//! the busy spinner.

use crate::view::MainView;
use gpui::SharedString;
use mc_core::point::Point2i;
use std::fmt::Write;

/// every input that can change the status-bar text; the formatted string is
/// rebuilt only when this differs from the cached key (see status_text).
#[derive(PartialEq)]
pub(crate) struct StatusKey {
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

impl MainView {
    pub(crate) fn status_text(&mut self) -> SharedString {
        // upstream status bar: hovered structure names
        let structure = if self.map.show_structure_icons {
            self.map.hovered_block.and_then(|h| {
                let c = h.block_to_chunk();
                let r = c.chunk_to_region();
                self.map
                    .tiles
                    .get(&r.as_long())
                    .and_then(|t| t.structures.as_ref())
                    .and_then(|m| m.chunks.get(&c.as_long()))
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
