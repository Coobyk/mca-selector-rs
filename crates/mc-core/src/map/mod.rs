//! Map pixel generation, replicating Xaero's World Map.
//!
//! Pipeline: `ChunkColumns` collects the per-column data of a chunk
//! (visible block color, water depth, floor height, biome); a region grid
//! of columns then runs Xaero's exact color math in [`render_columns`]:
//! vanilla map color x blended biome tint, translucent water overlays over
//! the floor, glowing-block brightening, terrain depth, and the "fancy
//! slopes" lighting (north + north-west neighbor heights, quantized direct
//! light, ambient 0.2 / white 0.5, overworld shadow 1,1,1).

pub mod colors;
pub mod region;

use crate::chunk_fast::{FastChunk, FastSection};
use crate::packed::{palette_bits, PackedBits};

/// Side length of a chunk in blocks.
pub const TILE_SIZE: usize = 16;

pub use colors::TintKind;

/// Per-column data of one chunk (16x16), indexed `[z * 16 + x]`.
pub struct ChunkColumns {
    /// Base map color of the visible block (the floor block for water
    /// columns), before tinting.
    pub base: [[u8; 3]; 256],
    pub tint: [TintKind; 256],
    /// Water depth in blocks (0 = no water on this column).
    pub water_depth: [i32; 256],
    /// Y of the highest solid (ocean-floor) block; used for shading.
    pub floor_height: [i32; 256],
    /// Biome of the column, as an index into a region-wide biome table.
    pub biome: [u16; 256],
    pub glowing: [bool; 256],
}

/// Grid of columns covering a rectangular area (a region, or a chunk plus
/// its NW margin for per-chunk tiles). All columns indexed
/// `[z * stride + x]`.
pub struct ColumnGrid {
    pub stride: usize,
    pub rows: usize,
    pub base: Vec<[u8; 3]>,
    pub tint: Vec<TintKind>,
    pub water_depth: Vec<i32>,
    pub floor_height: Vec<i32>,
    pub biome: Vec<u16>,
    pub glowing: Vec<bool>,
    /// Biome names referenced by `biome` indices.
    pub biomes: Vec<String>,
}

impl ColumnGrid {
    pub fn empty(stride: usize, rows: usize) -> Self {
        Self {
            stride,
            rows,
            base: vec![[0, 0, 0]; stride * rows],
            tint: vec![TintKind::None; stride * rows],
            water_depth: vec![0; stride * rows],
            floor_height: vec![i32::MIN; stride * rows],
            biome: vec![0; stride * rows],
            glowing: vec![false; stride * rows],
            biomes: Vec::new(),
        }
    }

    pub fn biome_id(&mut self, name: &str) -> u16 {
        if let Some(i) = self.biomes.iter().position(|b| b == name) {
            return i as u16;
        }
        self.biomes.push(name.to_string());
        (self.biomes.len() - 1) as u16
    }
}

/// Extract the per-column data of one chunk. `biome_id` resolves biome
/// names to grid indices.
pub fn chunk_columns(
    chunk: &FastChunk,
    biome_id: &mut dyn FnMut(&str) -> u16,
) -> Option<ChunkColumns> {
    // Section lookup by Y index (-4..=19 for 1.18+ worlds).
    let mut by_y: [Option<&FastSection>; 24] = Default::default();
    for s in &chunk.sections {
        let i = (s.y + 4) as usize;
        if i < 24 {
            by_y[i] = Some(s);
        }
    }

    let motion = chunk.motion_blocking.as_ref()?;
    let ocean_floor = chunk.ocean_floor.as_ref();

    let mut cols = ChunkColumns {
        base: [[0, 0, 0]; 256],
        tint: [TintKind::None; 256],
        water_depth: [0; 256],
        floor_height: [i32::MIN; 256],
        biome: [0; 256],
        glowing: [false; 256],
    };

    for z in 0..TILE_SIZE {
        for x in 0..TILE_SIZE {
            let idx = z * TILE_SIZE + x;
            let surface_y = height_at(motion, x, z);
            if surface_y == i32::MIN {
                continue;
            }
            let floor_y = ocean_floor
                .map(|f| height_at(f, x, z))
                .filter(|h| *h != i32::MIN)
                .unwrap_or(surface_y);
            cols.floor_height[idx] = floor_y;

            // Biome of the column, at the surface.
            let sy = (surface_y - 1).div_euclid(16) + 4;
            if let Some(section) = by_y.get(sy as usize).and_then(|s| *s) {
                if let Some(biomes) = &section.biomes {
                    cols.biome[idx] =
                        biome_id(biomes.at(x, (surface_y - 1).rem_euclid(16) as usize, z));
                }
            }

            // Block name at one position; None when the section or palette
            // is missing.
            let block_name = |y: i32| -> Option<&str> {
                let sy = y.div_euclid(16) + 4;
                let section = by_y.get(sy as usize).and_then(|s| *s)?;
                let names = &section.names;
                if names.is_empty() {
                    return None;
                }
                let states = section.data.as_ref();
                let bits = palette_bits(names.len());
                let index = match states {
                    None => 0,
                    Some(data) => PackedBits::new(data, bits)
                        .get((y.rem_euclid(16) as usize * 16 + z) * 16 + x),
                };
                names.get(index as usize).map(|s| &**s)
            };

            // Water column? The surface block is water: the visible color
            // comes from the floor below, and the water becomes an overlay
            // with depth `surface_y - floor_y`.
            let surface_block = block_name(surface_y - 1);
            let is_water = surface_block == Some("water") || surface_block == Some("bubble_column");
            if is_water {
                cols.water_depth[idx] = (surface_y - floor_y).max(1);
            }

            // Walk down from the surface to the first visible block with a
            // map color. Water blocks above the floor are skipped (they are
            // drawn as the overlay), as are Xaero's invisible blocks.
            let mut y = surface_y - 1;
            let min_y = (surface_y - 64).max(0);
            loop {
                if y < min_y {
                    break;
                }
                let Some(name) = block_name(y) else {
                    break;
                };
                let below_surface = y < surface_y - 1;
                if is_water && below_surface && y >= floor_y {
                    // Water between the surface and the floor.
                    if y == 0 {
                        break;
                    }
                    y -= 1;
                    continue;
                }
                if colors::is_invisible(name) {
                    if y == 0 {
                        break;
                    }
                    y -= 1;
                    continue;
                }
                if let Some(color) = colors::block_map_color(name) {
                    cols.base[idx] = [
                        ((color >> 16) & 0xFF) as u8,
                        ((color >> 8) & 0xFF) as u8,
                        (color & 0xFF) as u8,
                    ];
                    cols.tint[idx] = colors::tint_kind_of(name);
                    cols.glowing[idx] = colors::is_glowing(name);
                }
                break;
            }
        }
    }
    Some(cols)
}

/// Tint of one column: the biome color of its tint kind, averaged over the
/// 5-sample cross (center + 4 neighbors) like Xaero's biome blending.
fn blended_tint(grid: &ColumnGrid, x: i32, z: i32, kind: TintKind) -> [u32; 3] {
    if let TintKind::Fixed(c) = kind {
        return [((c >> 16) & 0xFF) as u32, ((c >> 8) & 0xFF) as u32, (c & 0xFF) as u32];
    }
    if kind == TintKind::None {
        return [255, 255, 255];
    }
    let mut sum = [0u64; 3];
    let mut total = 0u64;
    for (ox, oz) in [(0i32, 0i32), (1, 0), (-1, 0), (0, 1), (0, -1)] {
        let nx = x + ox;
        let nz = z + oz;
        if nx < 0 || nz < 0 || nx >= grid.stride as i32 || nz >= grid.rows as i32 {
            continue;
        }
        let i = nz as usize * grid.stride + nx as usize;
        let biome = &grid.biomes[grid.biome[i] as usize];
        let [g, f, w] = colors::biome_tint(biome);
        let c = match kind {
            TintKind::Grass => g,
            TintKind::Foliage => f,
            TintKind::DryFoliage => f,
            TintKind::Water => w,
            TintKind::Fixed(c) => c,
            TintKind::None => 0xFFFFFF,
        };
        sum[0] += u64::from((c >> 16) & 0xFF);
        sum[1] += u64::from((c >> 8) & 0xFF);
        sum[2] += u64::from(c & 0xFF);
        total += 1;
    }
    if total == 0 {
        return [255, 255, 255];
    }
    [
        (sum[0] / total) as u32,
        (sum[1] / total) as u32,
        (sum[2] / total) as u32,
    ]
}

/// Xaero's slope + terrain-depth brightness for one column.
/// `height` is the column's own floor height; `h_north` and `h_nw` the
/// floor heights of the north and north-west neighbor columns.
fn slope_brightness(height: i32, h_north: Option<i32>, h_nw: Option<i32>, glowing: bool) -> f32 {
    let mut depth_brightness = (height as f32 / 63.0).clamp(0.9, 1.0);

    // Slopes (fancy): verticalSlope = h - h(north), diagonal = h - h(NW).
    // Without both neighbors Xaero skips the slope shading (slopeUnknown),
    // keeping only the terrain-depth term.
    let (Some(h_n), Some(h_nw)) = (h_north, h_nw) else {
        return depth_brightness;
    };
    let vs = (height - h_n).clamp(-128, 127);
    let ds = (height - h_nw).clamp(-128, 127);

    let ambient_light_colored: f32 = if glowing { 0.0 } else { 0.2 };
    let ambient_light_white: f32 = if glowing { 1.0 } else { 0.5 };
    let max_direct_light: f32 = if glowing { 2.0 / 9.0 } else { 2.0 / 3.0 };

    let mut cos = 0.0f32;
    let cross_z = -vs as f32;
    if cross_z < 1.0 {
        if vs == 1 && ds == 1 {
            cos = 1.0;
        } else {
            let cross_x = vs as f32 - ds as f32;
            let cast = 1.0 - cross_z;
            let cross_magnitude = (cross_x * cross_x + 1.0 + cross_z * cross_z).sqrt();
            cos = (cast / cross_magnitude) / std::f32::consts::SQRT_2;
        }
    }
    let direct_light_clamped = if cos == 1.0 {
        max_direct_light
    } else if cos > 0.0 {
        (cos * 10.0).ceil() / 10.0 * max_direct_light * 0.88388
    } else {
        0.0
    };
    let white_light = ambient_light_white + direct_light_clamped;
    let shadow = 1.0f32; // overworld shadow multipliers
    (shadow * ambient_light_colored + white_light) * depth_brightness
}

/// Render a grid of columns into RGBA pixels (`4 * stride * rows` bytes).
/// Returns (pixels, width, height) or None when the grid is empty.
pub fn render_columns(grid: &ColumnGrid) -> Option<Vec<u8>> {
    let stride = grid.stride;
    let rows = grid.rows;
    let mut out = vec![0u8; stride * rows * 4];

    // Precompute blended tints per column (5-sample cross).
    let mut tints: Vec<[u32; 3]> = vec![[255, 255, 255]; stride * rows];
    for z in 0..rows {
        for x in 0..stride {
            let i = z * stride + x;
            if grid.floor_height[i] == i32::MIN {
                continue;
            }
            tints[i] = blended_tint(grid, x as i32, z as i32, grid.tint[i]);
        }
    }

    let mut any = false;
    for z in 0..rows {
        for x in 0..stride {
            let i = z * stride + x;
            let h = grid.floor_height[i];
            if h == i32::MIN {
                continue;
            }
            any = true;
            let [tr, tg, tb] = tints[i];
            let base = grid.base[i];
            let mut r = ((tr as u32 * (base[0] as u32)) / 255) as i64;
            let mut g = ((tg as u32 * (base[1] as u32)) / 255) as i64;
            let mut b = ((tb as u32 * (base[2] as u32)) / 255) as i64;

            // Glowing: brighten until the channels sum to >= 407.
            if grid.glowing[i] {
                let total = r + g + b;
                let brightener = 407.0f64 / total as f64;
                let brightener = brightener.max(1.0);
                r = (r as f64 * brightener) as i64;
                g = (g as f64 * brightener) as i64;
                b = (b as f64 * brightener) as i64;
            }

            // Slope + depth brightness against N / NW neighbors.
            let valid = |h: i32| (h != i32::MIN).then_some(h);
            let h_north = if z > 0 {
                valid(grid.floor_height[(z - 1) * stride + x])
            } else {
                None
            };
            let h_nw = if x > 0 && z > 0 {
                valid(grid.floor_height[(z - 1) * stride + x - 1])
            } else {
                None
            };
            let brightness =
                slope_brightness(h, h_north, h_nw, grid.glowing[i]) as f64;
            r = (r as f64 * brightness) as i64;
            g = (g as f64 * brightness) as i64;
            b = (b as f64 * brightness) as i64;

            // Water overlay: translucent layer over the floor.
            let depth = grid.water_depth[i];
            if depth > 0 {
                let wcolor_i = {
                    // Water tint of this column's biome (blended).
                    let [wr, wg, wb] = tints[i];
                    // The water overlay's own map color, tinted by the
                    // biome water color.
                    let wc = colors::block_map_color("water").unwrap_or(0x4040FF);
                    [
                        (wr as u32 * ((wc >> 16) & 0xFF)) / 255,
                        (wg as u32 * ((wc >> 8) & 0xFF)) / 255,
                        (wb as u32 * (wc & 0xFF)) / 255,
                    ]
                };
                let transparency = 127.0f64 / 255.0;
                let skylight_above = 15.0f64;
                let sun = (15 - depth.max(0)).max(0) as f64;
                let intensity =
                    (9.0 + skylight_above.max(sun)) / 24.0 * transparency;
                // Base dimmed by skylight below the water.
                // Skylight just above the floor block: 16 - depth
                // (capped at 15), i.e. the floor dims with depth.
                let pixel_light = (16.0 - depth as f64).min(15.0).max(0.0);
                let dim = (9.0 + pixel_light.max(sun)) / 24.0;
                r = (r as f64 * dim * (1.0 - transparency)) as i64
                    + (wcolor_i[0] as f64 * intensity) as i64;
                g = (g as f64 * dim * (1.0 - transparency)) as i64
                    + (wcolor_i[1] as f64 * intensity) as i64;
                b = (b as f64 * dim * (1.0 - transparency)) as i64
                    + (wcolor_i[2] as f64 * intensity) as i64;
            }

            let o = i * 4;
            out[o] = r.clamp(0, 255) as u8;
            out[o + 1] = g.clamp(0, 255) as u8;
            out[o + 2] = b.clamp(0, 255) as u8;
            out[o + 3] = 255;
        }
    }
    if !any {
        return None;
    }
    Some(out)
}


/// Read height of the column `(x, z)` from a packed heightmap array.
/// Heights are stored as `y - world_min_y` (`y + 64` in 1.18); this returns
/// the plain world Y (the first free y above the surface).
pub fn height_at(map: &[i64], x: usize, z: usize) -> i32 {
    debug_assert!(x < 16 && z < 16);
    let bits = PackedBits::new(map, 9);
    (bits.get(z * 16 + x) as i32) - 64
}
