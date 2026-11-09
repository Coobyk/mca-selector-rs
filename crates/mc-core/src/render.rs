//! Port of tile.TileImage.generateImage + the shading passes and LOD images.

use crate::mca::McaFile;
use crate::nbt;
use crate::version::{self, RenderOut};

pub const TILE_SIZE: usize = 512;
pub const CHUNK_SIZE: usize = 16;
pub const SIZE_IN_CHUNKS: usize = 32;

pub struct RenderSettings {
    pub render_height: i32,
    pub layer_only: bool,
    pub caves: bool,
    pub shade: bool,
    pub shade_water: bool,
    pub shade_altitude: bool,
}

impl Default for RenderSettings {
    fn default() -> Self {
        RenderSettings {
            render_height: 319,
            layer_only: false,
            caves: false,
            shade: true,
            shade_water: true,
            shade_altitude: true,
        }
    }
}

pub fn shade_color(argb: u32, shade: i32) -> u32 {
    let r = (((argb >> 16) & 0xFF) as i32 + shade).clamp(0, 255) as u32;
    let g = (((argb >> 8) & 0xFF) as i32 + shade).clamp(0, 255) as u32;
    let b = ((argb & 0xFF) as i32 + shade).clamp(0, 255) as u32;
    (argb & 0xFF00_0000) | (r << 16) | (g << 8) | b
}

pub fn blend(a: u32, b: u32, ratio: f32) -> u32 {
    let ir = ((a >> 16 & 0xFF) as f32 * ratio + (b >> 16 & 0xFF) as f32 * (1.0 - ratio)) as u32;
    let ig = ((a >> 8 & 0xFF) as f32 * ratio + (b >> 8 & 0xFF) as f32 * (1.0 - ratio)) as u32;
    let ib = ((a & 0xFF) as f32 * ratio + (b & 0xFF) as f32 * (1.0 - ratio)) as u32;
    0xFF00_0000 | (ir << 16) | (ig << 8) | ib
}

/// generateImage(mcaFile, scale) -> ARGB pixel buffer of 512/scale × 512/scale.
/// `corrupted` is a 16x16 argb overlay used when a chunk fails to render.
pub fn generate_image(
    mca: &McaFile,
    scale: i32,
    cfg: &RenderSettings,
    corrupted: &[u32; 256],
) -> Option<Vec<u32>> {
    let size = TILE_SIZE / scale as usize;
    let chunk_size = CHUNK_SIZE / scale as usize;
    let pixels = TILE_SIZE * TILE_SIZE / (scale * scale) as usize;

    let mut pixel_buffer = vec![0u32; pixels];
    let shade_water = cfg.shade && cfg.shade_water && !cfg.caves;
    let mut water_pixels = if shade_water { vec![0u32; pixels] } else { Vec::new() };
    let mut terrain_heights = vec![0i16; pixels];
    let mut water_heights = if shade_water { vec![0i16; pixels] } else { Vec::new() };

    for cx in 0..SIZE_IN_CHUNKS {
        for cz in 0..SIZE_IN_CHUNKS {
            let index = cz * SIZE_IN_CHUNKS + cx;
            let Some(chunk) = &mca.chunks[index] else { continue };
            let Some(data) = &chunk.data else { continue };
            let data_version = nbt::data_version(Some(data));
            let format = version::chunk_format(data_version);
            let cm = version::color_mapping(data_version);
            let x = cx * chunk_size;
            let z = cz * chunk_size;
            // upstream wraps each chunk's draw in try/catch and paints the
            // corrupted overlay on failure — catch_unwind is the equivalent.
            let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut out = RenderOut {
                    pixel_buffer: &mut pixel_buffer,
                    water_pixels: if shade_water { Some(&mut water_pixels) } else { None },
                    terrain_heights: &mut terrain_heights,
                    water_heights: if shade_water { Some(&mut water_heights) } else { None },
                    water: cfg.shade && cfg.shade_water,
                    height: cfg.render_height,
                };
                if cfg.caves {
                    version::draw_caves(data, &cm, format, x, z, scale, out.pixel_buffer, out.terrain_heights, cfg.render_height);
                } else if cfg.layer_only {
                    version::draw_layer(data, &cm, format, x, z, scale, out.pixel_buffer, cfg.render_height);
                } else {
                    version::draw_chunk(data, &cm, format, x, z, scale, &mut out);
                }
            }));
            if res.is_err() {
                // corruptedChunkOverlay + heights = 64 (upstream drawChunkImage)
                for cy in 0..CHUNK_SIZE {
                    for cxi in 0..CHUNK_SIZE {
                        let src = cy * CHUNK_SIZE + cxi;
                        let dst = (z + cy / scale as usize) * size + (x + cxi / scale as usize);
                        if dst < pixels {
                            pixel_buffer[dst] = corrupted[src];
                            terrain_heights[dst] = 64;
                            if !water_heights.is_empty() {
                                water_heights[dst] = 64;
                            }
                        }
                    }
                }
            }
        }
    }

    if cfg.caves {
        flat_shade(&mut pixel_buffer, &terrain_heights, scale);
    } else if cfg.shade && !cfg.layer_only {
        shade(&mut pixel_buffer, &water_pixels, &terrain_heights, &water_heights, scale, cfg);
    }
    Some(pixel_buffer)
}

fn flat_shade(px: &mut [u32], heights: &[i16], _scale: i32) {
    for (i, p) in px.iter_mut().enumerate() {
        let alt = (heights[i] / 4).clamp(-50, 50);
        *p = shade_color(*p, (alt * 4) as i32);
    }
}

fn shade(
    px: &mut [u32],
    water_px: &[u32],
    terrain: &[i16],
    water: &[i16],
    scale: i32,
    cfg: &RenderSettings,
) {
    let water_heights: &[i16] = if cfg.shade_water && cfg.shade {
        water
    } else {
        terrain
    };
    let size = TILE_SIZE / scale as usize;
    let altitude_multiplier = if cfg.shade_altitude { 12.0 / 256.0 } else { 0.0 };
    for z in 0..size {
        for x in 0..size {
            let index = z * size + x;
            if px[index] == 0 {
                continue;
            }
            if terrain[index] != water_heights[index] {
                let ratio = 0.5 - 0.5 / 40.0 * (water_heights[index] - terrain[index]) as f32;
                px[index] = blend(px[index], water_px[index], ratio);
            } else {
                let z_shade = if z == 0 {
                    water_heights[index + size] - water_heights[index]
                } else if z == size - 1 {
                    water_heights[index] - water_heights[index - size]
                } else {
                    (water_heights[index + size] - water_heights[index - size]) * 2
                };
                let x_shade = if x == 0 {
                    water_heights[index + 1] - water_heights[index]
                } else if x == size - 1 {
                    water_heights[index] - water_heights[index - 1]
                } else {
                    (water_heights[index + 1] - water_heights[index - 1]) * 2
                };
                let mut shade = (x_shade + z_shade) as f32;
                shade = shade.clamp(-8.0, 8.0);
                let mut alt = (water_heights[index] - 64) as f32 * altitude_multiplier;
                alt = alt.clamp(-4.0, 12.0);
                shade += alt;
                px[index] = shade_color(px[index], (shade * 8.0) as i32);
            }
        }
    }
}

/// generateImageFromChunkSet — the green LOD tile used at high zoom levels.
pub fn lod_image(chunk_set: &crate::selection::ChunkSet, color: u32) -> Vec<u32> {
    let mut pixels = vec![0u32; 1024];
    for i in 0..1024 {
        if chunk_set.get(i) {
            pixels[i] = color;
        }
    }
    pixels
}

/// scaleDownFXImage — nearest neighbor downscale of a square argb buffer.
pub fn scale_down(img: &[u32], from_size: usize, to_size: usize) -> Vec<u32> {
    let factor = from_size / to_size;
    let mut out = vec![0u32; to_size * to_size];
    for y in 0..to_size {
        for x in 0..to_size {
            out[y * to_size + x] = img[(y * factor) * from_size + x * factor];
        }
    }
    out
}

/// The grey "empty tile" (EMPTY_COLOR = 0.2,0.2,0.2 alpha 1) — drawn when a
/// region doesn't exist and showNonexistentRegions is on.
pub fn empty_tile_argb() -> u32 {
    let v = (0.2f32 * 255.0) as u32;
    0xFF00_0000 | v << 16 | v << 8 | v
}
