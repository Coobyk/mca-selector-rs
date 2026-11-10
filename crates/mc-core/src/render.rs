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

    // chunks in a region almost always share a DataVersion — cache the
    // last dispatch (chunk_format + color_mapping) instead of re-walking
    // the tables and OnceLocks per chunk.
    let mut last_dv: Option<i32> = None;
    let mut format = version::ChunkFormat::Null;
    let mut cm = version::ColorMap::Null;
    for cx in 0..SIZE_IN_CHUNKS {
        for cz in 0..SIZE_IN_CHUNKS {
            let index = cz * SIZE_IN_CHUNKS + cx;
            let Some(chunk) = &mca.chunks[index] else { continue };
            let Some(data) = &chunk.data else { continue };
            let data_version = nbt::data_version(Some(data));
            if last_dv != Some(data_version) {
                last_dv = Some(data_version);
                format = version::chunk_format(data_version);
                cm = version::color_mapping(data_version);
            }
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
        // row slices: bounds checks hoisted out of the per-pixel loop.
        let hrow = &water_heights[z * size..(z + 1) * size];
        let hup = if z == 0 {
            hrow
        } else {
            &water_heights[(z - 1) * size..z * size]
        };
        let hdn = if z == size - 1 && size > 1 {
            hrow
        } else {
            &water_heights[(z + 1) * size..(z + 2) * size]
        };
        let trow = &terrain[z * size..(z + 1) * size];
        let wrow: &[u32] = if water_px.is_empty() {
            &[]
        } else {
            &water_px[z * size..(z + 1) * size]
        };
        let prow = &mut px[z * size..(z + 1) * size];
        for x in 0..size {
            if prow[x] == 0 {
                continue;
            }
            if trow[x] != hrow[x] {
                let ratio = 0.5 - 0.5 / 40.0 * (hrow[x] - trow[x]) as f32;
                prow[x] = blend(prow[x], wrow[x], ratio);
            } else {
                let z_shade = if z == 0 {
                    hdn[x] - hrow[x]
                } else if z == size - 1 {
                    hrow[x] - hup[x]
                } else {
                    (hdn[x] - hup[x]) * 2
                };
                let x_shade = if x == 0 {
                    hrow[1] - hrow[0]
                } else if x == size - 1 {
                    hrow[x] - hrow[x - 1]
                } else {
                    (hrow[x + 1] - hrow[x - 1]) * 2
                };
                let mut shade = (x_shade + z_shade) as f32;
                shade = shade.clamp(-8.0, 8.0);
                let mut alt = (hrow[x] - 64) as f32 * altitude_multiplier;
                alt = alt.clamp(-4.0, 12.0);
                shade += alt;
                prow[x] = shade_color(prow[x], (shade * 8.0) as i32);
            }
        }
    }
}

/// generateImageFromChunkSet — the green LOD tile used at high zoom levels.
pub fn lod_image(chunk_set: &crate::selection::ChunkSet, color: u32) -> Vec<u32> {
    let mut pixels = vec![0u32; 1024];
    for i in chunk_set.iter() {
        pixels[i] = color;
    }
    pixels
}

/// scaleDownFXImage — nearest neighbor downscale of a square argb buffer.
pub fn scale_down(img: &[u32], from_size: usize, to_size: usize) -> Vec<u32> {
    let factor = from_size / to_size;
    let mut out = vec![0u32; to_size * to_size];
    if factor == 0 {
        // upscale: every pixel samples img[0] (same as the old x*0 indexing)
        out.fill(img[0]);
        return out;
    }
    for (y, row) in out.chunks_exact_mut(to_size).enumerate() {
        let base = y * factor * from_size;
        let src = &img[base..base + (to_size - 1) * factor + 1];
        for (p, v) in row.iter_mut().zip(src.iter().step_by(factor)) {
            *p = *v;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mca::McaFile;

    /// Timing harness for renderer changes. Needs a real region dir:
    ///   cargo test -p mc-core --release render_timing -- --ignored --nocapture
    /// MCA_WORLD_DIR overrides the default world path; MCA_REGION_PATH picks
    /// a single file.
    #[test]
    #[ignore]
    fn render_timing() {
        let dir = std::env::var("MCA_WORLD_DIR").unwrap_or_else(|_| {
            "/home/coobyk/Documents/Chunkyied/dimensions/minecraft/overworld".into()
        });
        let path = if let Ok(p) = std::env::var("MCA_REGION_PATH") {
            std::path::PathBuf::from(p)
        } else {
            std::fs::read_dir(&dir)
                .expect("world dir")
                .filter_map(|e| e.ok().map(|e| e.path()))
                .find(|p| p.extension().map(|e| e == "mca").unwrap_or(false))
                .expect("a .mca region file")
        };
        eprintln!("rendering {}", path.display());
        let mut m = McaFile::new(path).expect("region file");
        m.load(true).expect("load");
        let corrupted = [0u32; 256];
        let mut checksum = 0u64;
        for i in 0..5 {
            let t = std::time::Instant::now();
            let img = generate_image(&m, 1, &RenderSettings::default(), &corrupted).expect("image");
            let el = t.elapsed();
            checksum = img
                .iter()
                .fold(0u64, |a, &p| a.wrapping_mul(31).wrapping_add(p as u64));
            eprintln!("iter {i}: {el:?} ({} px, checksum {checksum:016x})", img.len());
        }
    }
}



#[cfg(test)]
mod tests3 {
    use super::*;
    use crate::mca::McaFile;
    use std::collections::HashMap;
    #[test]
    #[ignore]
    fn mansion_probe() {
        let path = std::path::PathBuf::from(std::env::var("MCA_REGION_PATH").expect("MCA_REGION_PATH"));
        let mut m = McaFile::new(path).expect("region");
        m.load(true).expect("load");
        let corrupted = [0u32; 256];
        let img = generate_image(&m, 1, &RenderSettings::default(), &corrupted).expect("img");
        // mansion ~ chunk(-46,-49) -> local (18,15); sample 260..340 x 220..290
        let mut hist: HashMap<u32, usize> = HashMap::new();
        for y in 220..290usize {
            for x in 260..340usize {
                *hist.entry(img[y * 512 + x]).or_default() += 1;
            }
        }
        let mut v: Vec<_> = hist.into_iter().collect();
        v.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
        for (c, n) in v.iter().take(12) {
            eprintln!("color {c:08x} x{n}");
        }
        let checksum = img.iter().fold(0u64, |a, &p| a.wrapping_mul(31).wrapping_add(p as u64));
        eprintln!("checksum {checksum:016x}");
        // dump mansion area at several zoom levels
        for zl in [1i32, 2, 4, 8, 16] {
            let zi = generate_image(&m, zl, &RenderSettings::default(), &corrupted).expect("img");
            let sz = 512 / zl as usize;
            let mut rgba = Vec::with_capacity(sz * sz * 4);
            for &p in zi.iter() {
                rgba.extend_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, p as u8, (p >> 24) as u8]);
            }
            image::save_buffer(format!("/tmp/zl{zl}.png"), &rgba, sz as u32, sz as u32, image::ColorType::Rgba8).unwrap();
        }
        let (x0, y0, w, h) = (200usize, 180usize, 250usize, 160usize);
        let mut rgba = Vec::with_capacity(w * h * 4);
        for y in y0..y0 + h {
            for x in x0..x0 + w {
                let p = img[y * 512 + x];
                rgba.extend_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, p as u8, (p >> 24) as u8]);
            }
        }
        image::save_buffer("/tmp/mansion.png", &rgba, w as u32, h as u32, image::ColorType::Rgba8).unwrap();
    }
}
