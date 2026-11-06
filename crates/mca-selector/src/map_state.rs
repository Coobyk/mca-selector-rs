//! The map view model: world data, tile cache, selection and view transform.

use std::collections::HashMap;
use std::sync::Arc;

use gpui::{App, Context, Entity, Task, Window};
use mc_core::chunk::Chunk;
use mc_core::map::{ChunkTile, render_tile};
use mc_core::region::Region;
use mc_core::world::World;

/// Chunk coordinates.
pub type ChunkCoord = (i32, i32);
/// Region coordinates.
pub type RegionCoord = (i32, i32);

pub const REGION_CHUNKS: i32 = 32;

/// A rendered chunk tile plus its flat average color, ready to paint.
pub struct TileEntry {
    pub image: Arc<gpui::RenderImage>,
    pub avg: mc_core::map::colors::Rgba,
    pub height: i32,
}

/// Which chunks exist in a region file, from its (cheap) sector table.
#[derive(Default)]
pub struct RegionInfo {
    /// Bit set per present chunk, index `z * 32 + x` with local coords.
    pub present: [u64; 32],
    pub path: std::path::PathBuf,
}

impl RegionInfo {
    pub fn has(&self, lx: i32, lz: i32) -> bool {
        self.present[lz as usize] >> lx & 1 == 1
    }

    pub fn count(&self) -> u32 {
        self.present.iter().map(|w| w.count_ones()).sum()
    }
}

/// Running region average, updated as chunk tiles arrive.
#[derive(Default)]
pub struct RegionSummary {
    pub sum: [u64; 3],
    pub count: u32,
}

impl RegionSummary {
    pub fn avg(&self) -> Option<mc_core::map::colors::Rgba> {
        if self.count == 0 {
            return None;
        }
        Some([
            (self.sum[0] / u64::from(self.count)) as u8,
            (self.sum[1] / u64::from(self.count)) as u8,
            (self.sum[2] / u64::from(self.count)) as u8,
            255,
        ])
    }
}

/// The pan/zoom transform: `center` is the block coordinate in the middle of
/// the viewport, `zoom` is pixels per block.
#[derive(Clone, Copy, Debug)]
pub struct ViewState {
    pub center: (f64, f64),
    pub zoom: f64,
}

impl ViewState {
    pub fn new() -> Self {
        Self {
            center: (0.0, 0.0),
            zoom: 0.5,
        }
    }

    /// Screen position of a block coordinate, with `viewport` the map size.
    pub fn block_to_screen(&self, block: (f64, f64), viewport: (f64, f64)) -> (f64, f64) {
        (
            (block.0 - self.center.0) * self.zoom + viewport.0 / 2.0,
            (block.1 - self.center.1) * self.zoom + viewport.1 / 2.0,
        )
    }

    /// Block coordinate under a screen position.
    pub fn screen_to_block(&self, screen: (f64, f64), viewport: (f64, f64)) -> (f64, f64) {
        (
            (screen.0 - viewport.0 / 2.0) / self.zoom + self.center.0,
            (screen.1 - viewport.1 / 2.0) / self.zoom + self.center.1,
        )
    }

    /// Zoom by `factor`, keeping `anchor` (screen position) fixed.
    pub fn zoom_at(&mut self, factor: f64, anchor: (f64, f64), viewport: (f64, f64)) {
        let before = self.screen_to_block(anchor, viewport);
        self.zoom = (self.zoom * factor).clamp(1.0 / 512.0, 32.0);
        let after = self.screen_to_block(anchor, viewport);
        self.center.0 += before.0 - after.0;
        self.center.1 += before.1 - after.1;
    }
}

/// Selection rectangle in chunk coordinates while dragging.
#[derive(Clone, Copy, Debug)]
pub struct SelectDrag {
    pub start: ChunkCoord,
    pub current: ChunkCoord,
    /// add / subtract / replace
    pub mode: SelectMode,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SelectMode {
    Replace,
    Add,
    Subtract,
}

/// Panning drag state.
#[derive(Clone, Copy, Debug)]
pub struct PanDrag {
    pub start_center: (f64, f64),
    pub start_pos: (f64, f64),
}

/// Everything the map screen shows.
pub struct MapState {
    pub world: World,
    pub dimension: usize,
    /// Region info per dimension index (loaded lazily per dimension).
    pub regions: HashMap<RegionCoord, RegionInfo>,
    pub region_summaries: HashMap<RegionCoord, RegionSummary>,
    pub tiles: HashMap<ChunkCoord, Arc<TileEntry>>,
    pub pending: std::collections::HashSet<ChunkCoord>,
    pub loading_regions: std::collections::HashSet<RegionCoord>,
    pub selection: std::collections::HashSet<ChunkCoord>,
    pub view: ViewState,
    pub select_drag: Option<SelectDrag>,
    pub pan_drag: Option<PanDrag>,
    /// Last known mouse position in map-local coordinates, for the status bar.
    pub mouse: Option<(f64, f64)>,
    pub viewport: (f64, f64),
    pub show_chunk_grid: bool,
    pub show_region_grid: bool,
}
