//! Map pixel generation.
//!
//! [`render_tile_rgba`] renders one chunk into a 16x16 RGBA image in the
//! style of Xaero's world map: the topmost block of each column is mapped to
//! a color (see [`colors`]), water blends with depth, and columns are
//! shaded by the height difference towards the south-east neighbor (light
//! from the north-west).

pub mod colors;
pub mod region;

use crate::chunk_fast::{FastChunk, FastSection};
use crate::packed::{palette_bits, PackedBits, HEIGHTMAP_BITS};

/// Side length of a chunk in blocks; also the tile resolution.
pub const TILE_SIZE: usize = 16;

/// A rendered chunk tile: RGBA plus the flat average color used when zoomed
/// out.
pub struct TileOut {
    pub rgba: [u8; 1024],
    pub avg: [u8; 3],
    pub max_height: i32,
}

/// Render one chunk from the fast-parsed model. Returns `None` when the
/// chunk has no heightmap (nothing to draw).
pub fn render_tile_rgba(chunk: &FastChunk) -> Option<TileOut> {
    let motion = chunk.motion_blocking.as_ref()?;
    let ocean_floor = chunk.ocean_floor.as_ref();

    // Section lookup by Y index (-4..=19 for 1.18+ worlds).
    let mut by_y: [Option<&FastSection>; 24] = Default::default();
    for s in &chunk.sections {
        let i = (s.y + 4) as usize;
        if i < 24 {
            by_y[i] = Some(s);
        }
    }

    let mut heights = [0i32; TILE_SIZE * TILE_SIZE];
    let mut floors = [0i32; TILE_SIZE * TILE_SIZE];
    for z in 0..TILE_SIZE {
        for x in 0..TILE_SIZE {
            heights[z * TILE_SIZE + x] = height_at(motion, x, z);
            floors[z * TILE_SIZE + x] = ocean_floor
                .map(|f| height_at(f, x, z))
                .unwrap_or(i32::MIN);
        }
    }

    let mut raw_colors = [[0u8; 3]; TILE_SIZE * TILE_SIZE];
    let mut filled = [false; TILE_SIZE * TILE_SIZE];
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
            max_height = max_height.max(top_y - 1);

            let top_block = block_at(&by_y, x, top_y - 1, z);
            let mut color = match &top_block {
                Some(name) => colors::block_color(name),
                None => colors::map_colors::STONE,
            };

            // Water columns: blend floor color with water by depth.
            if top_block.map(|n| n == "water" || n == "bubble_column").unwrap_or(false) {
                let floor_y = if floors[idx] != i32::MIN {
                    floors[idx]
                } else {
                    top_y - 6
                };
                let depth = (top_y - floor_y).clamp(0, 32);
                let floor_block = block_at(&by_y, x, floor_y - 1, z);
                let floor_color = match &floor_block {
                    Some(name) => colors::block_color(name),
                    None => colors::map_colors::DIRT,
                };
                color = blend_water(floor_color, depth);
            }
            raw_colors[idx] = [color[0], color[1], color[2]];
        }
    }

    if !any {
        return None;
    }

    let mut rgba = [0u8; 1024];
    let mut sum = [0u64; 3];
    for z in 0..TILE_SIZE {
        for x in 0..TILE_SIZE {
            let idx = z * TILE_SIZE + x;
            if !filled[idx] {
                continue;
            }
            let h = heights[idx];
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
            sum[0] += u64::from(raw_colors[idx][0]);
            sum[1] += u64::from(raw_colors[idx][1]);
            sum[2] += u64::from(raw_colors[idx][2]);
        }
    }

    Some(TileOut {
        rgba,
        avg: [(sum[0] / 256) as u8, (sum[1] / 256) as u8, (sum[2] / 256) as u8],
        max_height,
    })
}

// The `filled` mask is set during the heightmap pass above and consumed by
// the shading pass.

/// Water color over a floor color; `depth` in blocks (0..=32). Shallow water
/// stays close to the floor color, deep water approaches a solid dark blue.
fn blend_water(floor: colors::Rgba, depth: i32) -> colors::Rgba {
    let water = colors::tints::WATER;
    let t = (depth as f32 / 32.0).clamp(0.0, 1.0);
    let alpha = 0.68 + 0.27 * t;
    let mut out = [0u8; 4];
    for i in 0..3 {
        let w = water[i] as f32 * (1.0 - 0.35 * t);
        out[i] = (floor[i] as f32 * (1.0 - alpha) + w * alpha).round() as u8;
    }
    out[3] = 255;
    out
}

/// Block name at one position, found via the section that contains `y`.
fn block_at<'a>(
    by_y: &'a [Option<&'a FastSection>; 24],
    x: usize,
    y: i32,
    z: usize,
) -> Option<&'a str> {
    if x >= TILE_SIZE || z >= TILE_SIZE {
        return None;
    }
    let sy = y.div_euclid(16) + 4;
    if !(0..24).contains(&sy) {
        return None;
    }
    let section = by_y[sy as usize]?;
    let bits = palette_bits(section.names.len());
    let index = match &section.data {
        None => 0,
        Some(data) => PackedBits::new(data, bits)
            .get((y.rem_euclid(16) as usize * 16 + z) * 16 + x),
    };
    section.names.get(index as usize).map(|n| &**n)
}

/// Read height of the column `(x, z)` from a packed heightmap array.
/// Heights are stored as `y - world_min_y` (`y + 64` in 1.18); this returns
/// the plain world Y.
fn height_at(map: &[i64], x: usize, z: usize) -> i32 {
    debug_assert!(x < 16 && z < 16);
    let bits = PackedBits::new(map, HEIGHTMAP_BITS);
    let raw = bits.get(z * 16 + x);
    raw as i32 - 64
}
