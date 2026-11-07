//! The map view model: world data, tile cache, selection and view transform.

use std::collections::HashMap;
use std::sync::Arc;

use mc_core::world::World;

/// Chunk coordinates.
pub type ChunkCoord = (i32, i32);
/// Region coordinates.
pub type RegionCoord = (i32, i32);

pub const REGION_CHUNKS: i32 = 32;

/// A rendered chunk tile plus its flat average color, ready to paint.
pub struct TileEntry {
    pub image: Arc<gpui::RenderImage>,
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

/// The pan/zoom transform: `center` is the block coordinate in the middle of
/// the viewport, `zoom` is pixels per block.
#[derive(Clone, Copy, Debug)]
pub struct ViewState {
    pub center: (f64, f64),
    pub zoom: f64,
}

impl ViewState {
    pub fn new() -> Self {
        let zoom = std::env::var("MCA_INIT_ZOOM")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(0.5);
        Self {
            center: (0.0, 0.0),
            zoom: zoom.clamp(1.0 / 512.0, 32.0),
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

/// Bounded detail-image cache: the newest-inserted entries win.
pub struct DetailCache {
    pub map: HashMap<RegionCoord, (Arc<gpui::RenderImage>, u64)>,
    clock: u64,
    pub cap: usize,
}

impl DetailCache {
    pub fn new(cap: usize) -> Self {
        Self {
            map: HashMap::new(),
            clock: 0,
            cap,
        }
    }

    pub fn insert(&mut self, coord: RegionCoord, img: Arc<gpui::RenderImage>) {
        self.clock += 1;
        if self.map.len() >= self.cap && !self.map.contains_key(&coord) {
            // Evict the oldest entry.
            if let Some(oldest) = self
                .map
                .iter()
                .min_by_key(|(_, (_, c))| *c)
                .map(|(k, _)| *k)
            {
                self.map.remove(&oldest);
            }
        }
        self.map.insert(coord, (img, self.clock));
    }

    pub fn get(&self, coord: &RegionCoord) -> Option<Arc<gpui::RenderImage>> {
        self.map.get(coord).map(|(img, _)| img.clone())
    }

    pub fn contains(&self, coord: &RegionCoord) -> bool {
        self.map.contains_key(coord)
    }

    pub fn clear(&mut self) {
        self.map.clear();
    }
}

/// Everything the map screen shows.
pub struct MapState {
    /// The opened world, or `None` while the landing screen asks for one.
    pub world: Option<World>,
    pub dimension: usize,
    /// Generation counter, bumped whenever the dimension (or world) changes;
    /// background results from older generations are dropped.
    pub render_gen: std::sync::Arc<std::sync::atomic::AtomicU64>,
    /// Region info per dimension index (loaded lazily per dimension).
    pub regions: HashMap<RegionCoord, RegionInfo>,
    /// Permanently cached 256x256 base image per region (2px per block).
    pub region_base: HashMap<RegionCoord, Arc<gpui::RenderImage>>,
    /// Full-resolution 512x512 images for regions on screen while zoomed
    /// in, bounded by an LRU.
    pub region_detail: DetailCache,
    /// Regions whose rendering task is currently queued or running.
    pub building_regions: std::collections::HashSet<RegionCoord>,
    /// Per-chunk tiles for the zoomed-in view, capped to bound memory.
    pub tiles: HashMap<ChunkCoord, Arc<TileEntry>>,
    pub pending: std::collections::HashSet<ChunkCoord>,
    /// Nearest-neighbor upscaled copies of chunk tiles, keyed by
    /// `(chunk, factor)`. gpui samples all images with a linear filter;
    /// painting a pre-replicated image keeps block edges crisp at high
    /// zoom. Only touched on the main thread.
    pub upscaled: std::cell::RefCell<HashMap<(ChunkCoord, u32), Arc<gpui::RenderImage>>>,
    pub selection: std::collections::HashSet<ChunkCoord>,
    pub view: ViewState,
    pub select_drag: Option<SelectDrag>,
    pub pan_drag: Option<PanDrag>,
    /// Last known mouse position in map-local coordinates, for the status bar.
    pub mouse: Option<(f64, f64)>,
    pub viewport: (f64, f64),
    pub show_chunk_grid: bool,
    pub show_region_grid: bool,
    /// Set when regions were loaded and the view should fit them once the
    /// real viewport size is known.
    pub needs_fit: bool,
    /// When the current dimension's region list was loaded (for the
    /// full-world render timing log).
    pub load_started: std::time::Instant,
    /// Whether the full-render timing was already reported.
    pub load_timed: bool,
}
