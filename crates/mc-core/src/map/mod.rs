//! Map pixel generation.
//!
//! [`tile`] renders one chunk into a 16x16 RGBA image in the style of
//! Xaero's world map: the topmost block of each column is mapped to a color
//! (see [`colors`]), water blends with depth, and columns are shaded by the
//! height difference towards the south-east neighbor (light from the
//! north-west).

pub mod colors;

use crate::chunk::{Chunk, Heightmaps, PaletteEntry, SECTION_BLOCKS};

/// Side length of a chunk in blocks; also the tile resolution.
pub const TILE_SIZE: usize = 16;

/// A rendered chunk tile: block-resolution RGBA plus the flat average color
/// used when zoomed out.
#[derive(Debug, Clone)]
pub struct ChunkTile {
    pub rgba: Vec<u8>,
    pub avg: colors::Rgba,
    /// Minimum and maximum surface height, for the altitude legend.
    pub min_height: i32,
    pub max_height: i32,
}

/// A rendered tile of a chunk that had no data (empty space in the region
/// file). Painted fully transparent so the background shows through.
pub fn empty_tile() -> ChunkTile {
    ChunkTile {
        rgba: vec![0; TILE_SIZE * TILE_SIZE * 4],
        avg: [0, 0, 0, 0],
        min_height: 0,
        max_height: 0,
    }
}

/// Render one chunk into a tile. Returns [`empty_tile`] when the chunk
/// carries no block data.
pub fn render_tile(chunk: &Chunk) -> ChunkTile {
    // Heights of the first free y above the surface, per column.
    let mut heights = [i32::MIN; TILE_SIZE * TILE_SIZE];
    let mut ocean_floors = [i32::MIN; TILE_SIZE * TILE_SIZE];
    if let Some(hm) = &chunk.heightmaps {
        if let Some(motion) = hm.get("MOTION_BLOCKING") {
            for z in 0..TILE_SIZE {
                for x in 0..TILE_SIZE {
                    heights[z * TILE_SIZE + x] = Heightmaps::height_at(motion, x, z);
                }
            }
        }
        if let Some(floor) = hm.get("OCEAN_FLOOR") {
            for z in 0..TILE_SIZE {
                for x in 0..TILE_SIZE {
                    ocean_floors[z * TILE_SIZE + x] = Heightmaps::height_at(floor, x, z);
                }
            }
        }
    }

    let mut rgba = vec![0u8; TILE_SIZE * TILE_SIZE * 4];
    let mut raw_colors = [[0u8; 3]; TILE_SIZE * TILE_SIZE];
    let mut filled = [false; TILE_SIZE * TILE_SIZE];
    let mut min_height = i32::MAX;
    let mut max_height = i32::MIN;
    let mut any = false;

    for z in 0..TILE_SIZE {
        for x in 0..TILE_SIZE {
            let idx = z * TILE_SIZE + x;
            let top_y = heights[idx];
            if top_y == i32::MIN {
                continue;
            }
            any = true;
            filled[idx] = true;
            min_height = min_height.min(top_y - 1);
            max_height = max_height.max(top_y - 1);

            // The block just below the surface.
            let top_block = block_at(chunk, x, top_y - 1, z);
            let mut color = match &top_block {
                Some(p) => colors::block_color(short_name(p)),
                None => colors::map_colors::STONE,
            };

            // Water columns: blend floor color with water by depth.
            if is_water(&top_block) {
                let floor_y = if ocean_floors[idx] != i32::MIN {
                    ocean_floors[idx]
                } else {
                    top_y - 6
                };
                let depth = (top_y - floor_y).clamp(0, 32);
                let floor_block = block_at(chunk, x, floor_y - 1, z);
                let floor_color = match &floor_block {
                    Some(p) => colors::block_color(short_name(p)),
                    None => colors::map_colors::DIRT,
                };
                color = blend_water(floor_color, depth);
            }
            raw_colors[idx] = [color[0], color[1], color[2]];
        }
    }

    if !any {
        return empty_tile();
    }

    // Height shading: light from the north-west. A column that rises towards
    // the south-east gets darker, one that falls gets brighter.
    for z in 0..TILE_SIZE {
        for x in 0..TILE_SIZE {
            let idx = z * TILE_SIZE + x;
            if !filled[idx] {
                continue;
            }
            let h = heights[idx];
            if h == i32::MIN {
                continue;
            }
            // Neighbor to the south-east; outside the chunk, reuse the own
            // height so chunk borders get no artificial seam.
            let h_se = heights
                .get(idx + TILE_SIZE + 1)
                .copied()
                .filter(|h| *h != i32::MIN)
                .unwrap_or(h);
            let diff = (h - h_se).clamp(-6, 6) as f32;
            let factor = 1.0 + diff * 0.055;
            let c = colors::shade(
                [raw_colors[idx][0], raw_colors[idx][1], raw_colors[idx][2], 255],
                factor,
            );
            let o = idx * 4;
            rgba[o] = c[0];
            rgba[o + 1] = c[1];
            rgba[o + 2] = c[2];
            rgba[o + 3] = 255;
        }
    }

    // Flat average color for the zoomed-out view.
    let mut sum = [0u64; 3];
    for c in &raw_colors {
        sum[0] += u64::from(c[0]);
        sum[1] += u64::from(c[1]);
        sum[2] += u64::from(c[2]);
    }
    let n = (TILE_SIZE * TILE_SIZE) as u64;
    let avg = [
        (sum[0] / n) as u8,
        (sum[1] / n) as u8,
        (sum[2] / n) as u8,
        255,
    ];

    ChunkTile {
        rgba,
        avg,
        min_height,
        max_height,
    }
}

/// Water color over a floor color; `depth` in blocks (0..=32). Shallow water
/// stays close to the floor color, deep water approaches a solid dark blue.
fn blend_water(floor: colors::Rgba, depth: i32) -> colors::Rgba {
    let water = colors::tints::WATER;
    // Deep-blue target gets darker with depth.
    let t = (depth as f32 / 32.0).clamp(0.0, 1.0);
    let alpha = 0.68 + 0.27 * t; // how much water covers the floor
    let mut out = [0u8; 4];
    for i in 0..3 {
        // Water color darkens towards the deep-blue end.
        let w = water[i] as f32 * (1.0 - 0.35 * t);
        out[i] = (floor[i] as f32 * (1.0 - alpha) + w * alpha).round() as u8;
    }
    out[3] = 255;
    out
}

fn short_name(entry: &PaletteEntry) -> &str {
    entry.name().strip_prefix("minecraft:").unwrap_or(entry.name())
}

fn is_water(entry: &Option<PaletteEntry>) -> bool {
    entry
        .as_ref()
        .is_some_and(|p| short_name(p) == "water" || short_name(p) == "bubble_column")
}

/// Block state of one position, found via the section that contains `y`.
/// Returns `None` when the section is absent or has no block states.
fn block_at(chunk: &Chunk, x: usize, y: i32, z: usize) -> Option<PaletteEntry> {
    if x >= SECTION_BLOCKS || z >= SECTION_BLOCKS {
        return None;
    }
    let section_y = i8::try_from(y.div_euclid(16)).ok()?;
    let section = chunk.section(section_y)?;
    let states = section.block_states.as_ref()?;
    let index = Chunk::block_palette_index(section, x, y.rem_euclid(16) as usize, z);
    states.palette.get(index as usize).cloned()
}
