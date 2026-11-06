//! Map pixel generation.
//!
//! [`render_tile_raw`] resolves the top block color of every column of a
//! chunk (see [`colors`]), [`shade_tile`] applies Xaero-style height
//! shading: light from the north-west, with the south-east neighbor height
//! supplied by the caller so shading crosses chunk borders seamlessly.

pub mod colors;
pub mod region;

use crate::chunk_fast::{FastChunk, FastSection};
use crate::packed::{palette_bits, PackedBits, HEIGHTMAP_BITS};

/// Side length of a chunk in blocks; also the tile resolution.
pub const TILE_SIZE: usize = 16;

/// Unshaded render of one chunk: per-column colors and surface heights.
pub struct TileRaw {
    /// RGB per column, row-major.
    pub colors: [[u8; 3]; 256],
    /// Surface height per column (`i32::MIN` = none).
    pub heights: [i32; 256],
    pub avg: [u8; 3],
    pub max_height: i32,
}

/// Resolve the top block color of each column of one chunk from the
/// fast-parsed model. Returns `None` when the chunk has no heightmap.
pub fn render_tile_raw(chunk: &FastChunk) -> Option<TileRaw> {
    let motion = chunk.motion_blocking.as_ref()?;

    // Section lookup by Y index (-4..=19 for 1.18+ worlds).
    let mut by_y: [Option<&FastSection>; 24] = Default::default();
    for s in &chunk.sections {
        let i = (s.y + 4) as usize;
        if i < 24 {
            by_y[i] = Some(s);
        }
    }

    let mut colors = [[0u8; 3]; 256];
    let mut heights = [i32::MIN; 256];
    let mut sum = [0u64; 3];
    let mut max_height = i32::MIN;
    let mut any = false;

    for z in 0..TILE_SIZE {
        for x in 0..TILE_SIZE {
            let idx = z * TILE_SIZE + x;
            let top_y = height_at(motion, x, z);
            heights[idx] = top_y;
            if top_y == i32::MIN {
                continue;
            }
            any = true;
            max_height = max_height.max(top_y - 1);

            let top_block = block_at(&by_y, x, top_y - 1, z);
            let mut color = match &top_block {
                Some(name) => colors::block_color(name),
                None => colors::map_colors::STONE,
            };

            // Water columns: blend floor color with water by depth.
            if top_block.map(|n| n == "water" || n == "bubble_column").unwrap_or(false) {
                let floor_y = chunk
                    .ocean_floor
                    .as_ref()
                    .map(|f| height_at(f, x, z))
                    .filter(|h| *h != i32::MIN)
                    .unwrap_or(top_y - 6);
                let depth = (top_y - floor_y).clamp(0, 32);
                let floor_block = block_at(&by_y, x, floor_y - 1, z);
                let floor_color = match &floor_block {
                    Some(name) => colors::block_color(name),
                    None => colors::map_colors::DIRT,
                };
                color = blend_water(floor_color, depth);
            }
            colors[idx] = [color[0], color[1], color[2]];
            sum[0] += u64::from(color[0]);
            sum[1] += u64::from(color[1]);
            sum[2] += u64::from(color[2]);
        }
    }

    if !any {
        return None;
    }

    Some(TileRaw {
        colors,
        heights,
        avg: [(sum[0] / 256) as u8, (sum[1] / 256) as u8, (sum[2] / 256) as u8],
        max_height,
    })
}

/// Apply height shading to a raw tile. `se_height(dx, dz)` resolves the
/// surface height of the column at chunk-relative offset `(dx, dz)` from
/// the tile origin, where `dx`/`dz` may be `16` (the first column of the
/// neighboring chunk to the east/south/south-east). Returning `None` falls
/// back to the column's own height, which is why callers should hand back
/// heights from neighboring chunks whenever they have them.
pub fn shade_tile(raw: &TileRaw, se_height: &dyn Fn(i32, i32) -> Option<i32>) -> [u8; 1024] {
    let mut rgba = [0u8; 1024];
    for z in 0..TILE_SIZE {
        for x in 0..TILE_SIZE {
            let idx = z * TILE_SIZE + x;
            let h = raw.heights[idx];
            if h == i32::MIN {
                continue;
            }
            let h_se = se_height(x as i32 + 1, z as i32 + 1)
                .filter(|h| *h != i32::MIN)
                .unwrap_or(h);
            let diff = (h - h_se).clamp(-6, 6) as f32;
            let factor = 1.0 + diff * 0.055;
            let c = colors::shade(
                [raw.colors[idx][0], raw.colors[idx][1], raw.colors[idx][2], 255],
                factor,
            );
            let o = idx * 4;
            rgba[o] = c[0];
            rgba[o + 1] = c[1];
            rgba[o + 2] = c[2];
            rgba[o + 3] = 255;
        }
    }
    rgba
}

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
pub fn height_at(map: &[i64], x: usize, z: usize) -> i32 {
    debug_assert!(x < 16 && z < 16);
    let bits = PackedBits::new(map, HEIGHTMAP_BITS);
    let raw = bits.get(z * 16 + x);
    raw as i32 - 64
}
