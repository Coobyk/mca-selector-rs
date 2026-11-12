//! Port of tile.TileImage.generateImage + the shading passes and LOD images.

use crate::mca::McaFile;
use crate::nbt;
use crate::version::{self, RenderOut};
use rayon::prelude::*;

pub const TILE_SIZE: usize = 512;
pub const CHUNK_SIZE: usize = 16;
pub const SIZE_IN_CHUNKS: usize = 32;

/// Dimension kind — drives Xaero's per-dimension ambient color,
/// depth curve and (nether) the full-cave column scan.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum DimKind {
    #[default]
    Overworld,
    Nether,
    End,
}

#[derive(Clone, Copy)]
pub struct RenderSettings {
    pub render_height: i32,
    pub layer_only: bool,
    pub caves: bool,
    pub shade: bool,
    pub shade_water: bool,
    pub shade_altitude: bool,
    pub dim: DimKind,
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
            dim: DimKind::Overworld,
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
    // the overlay buffers are always allocated for surface rendering —
    // the xaero pipeline needs them unconditionally.
    let aux = !cfg.caves;
    let mut water_pixels = if aux { vec![0u32; pixels] } else { Vec::new() };
    let mut terrain_heights = vec![0i16; pixels];
    let mut water_heights = if aux { vec![0i16; pixels] } else { Vec::new() };

    // The renderers address the buffers with a row stride of
    // `512 >> ilog2(scale)`; splitting the buffers into per-chunk-row bands
    // is a pure partition only when that stride equals `size`, i.e. for
    // power-of-two scales that keep chunk_size > 0 (1,2,4,8,16). Anything
    // else runs the original sequential loop verbatim.
    let t0 = std::time::Instant::now();
    let xaero_used = if chunk_size > 0 && scale > 0 && (scale & (scale - 1)) == 0 {
        draw_bands(
            mca,
            scale,
            cfg,
            corrupted,
            size,
            chunk_size,
            &mut pixel_buffer,
            &mut terrain_heights,
            &mut water_pixels,
            &mut water_heights,
        )
    } else {
        draw_sequential(
            mca,
            scale,
            cfg,
            corrupted,
            size,
            chunk_size,
            pixels,
            &mut pixel_buffer,
            &mut terrain_heights,
            &mut water_pixels,
            &mut water_heights,
        )
    };
    let draw_ms = t0.elapsed().as_secs_f64() * 1e3;
    let t1 = std::time::Instant::now();

    if xaero_used && !cfg.caves && !cfg.layer_only {
        xaero_shade(
            &mut pixel_buffer,
            &water_pixels,
            &terrain_heights,
            &water_heights,
            scale,
            cfg,
        );
    } else if cfg.caves {
        flat_shade(&mut pixel_buffer, &terrain_heights, scale);
    } else if cfg.shade && !cfg.layer_only {
        shade(&mut pixel_buffer, &water_pixels, &terrain_heights, &water_heights, scale, cfg);
    }
    eprintln!("[bench] draw={:.1}ms shade={:.1}ms", draw_ms, t1.elapsed().as_secs_f64() * 1e3);
    Some(pixel_buffer)
}

/// Parallel chunk rendering: chunk row `crz` exclusively owns pixel rows
/// `crz*chunk_size .. (crz+1)*chunk_size` in every buffer, so
/// `par_chunks_mut` hands each rayon task a disjoint band. Inside a band
/// the draw calls run with z=0 (band-local rows) and x = crx*chunk_size —
/// the same absolute pixel_index, just relative to the band's start.
#[allow(clippy::too_many_arguments)]
fn draw_bands(
    mca: &McaFile,
    scale: i32,
    cfg: &RenderSettings,
    corrupted: &[u32; 256],
    size: usize,
    chunk_size: usize,
    pixel_buffer: &mut [u32],
    terrain_heights: &mut [i16],
    water_pixels: &mut [u32],
    water_heights: &mut [i16],
) -> bool {
    let band = chunk_size * size;
    if water_pixels.is_empty() {
        pixel_buffer
            .par_chunks_mut(band)
            .zip(terrain_heights.par_chunks_mut(band))
            .enumerate()
            .map(|(crz, (pb, th))| {
                draw_band(mca, crz, scale, cfg, corrupted, chunk_size, size, pb, th, None, None)
            })
            .reduce(|| false, |a, b| a || b)
    } else {
        pixel_buffer
            .par_chunks_mut(band)
            .zip(terrain_heights.par_chunks_mut(band))
            .zip(water_pixels.par_chunks_mut(band))
            .zip(water_heights.par_chunks_mut(band))
            .enumerate()
            .map(|(crz, (((pb, th), wp), wh))| {
                draw_band(
                    mca,
                    crz,
                    scale,
                    cfg,
                    corrupted,
                    chunk_size,
                    size,
                    pb,
                    th,
                    Some(wp),
                    Some(wh),
                )
            })
            .reduce(|| false, |a, b| a || b)
    }
}

/// DataVersion -> (chunk_format, color_mapping) memoization — chunks in a
/// region almost always share a DataVersion, so this avoids re-walking the
/// dispatch tables and OnceLocks per chunk.
struct Dispatch {
    last_dv: Option<i32>,
    format: version::ChunkFormat,
    cm: version::ColorMap,
}

impl Dispatch {
    fn new() -> Self {
        Dispatch {
            last_dv: None,
            format: version::ChunkFormat::Null,
            cm: version::ColorMap::Null,
        }
    }

    fn get(&mut self, data_version: i32) -> (version::ChunkFormat, &version::ColorMap) {
        if self.last_dv != Some(data_version) {
            self.last_dv = Some(data_version);
            self.format = version::chunk_format(data_version);
            self.cm = version::color_mapping(data_version);
        }
        (self.format, &self.cm)
    }
}

/// One band = one chunk row; renders its 32 chunks sequentially into the
/// band slices.
#[allow(clippy::too_many_arguments)]
fn draw_band(
    mca: &McaFile,
    crz: usize,
    scale: i32,
    cfg: &RenderSettings,
    corrupted: &[u32; 256],
    chunk_size: usize,
    size: usize,
    pixel_band: &mut [u32],
    terrain_band: &mut [i16],
    mut water_px_band: Option<&mut [u32]>,
    mut water_h_band: Option<&mut [i16]>,
) -> bool {
    let mut dispatch = Dispatch::new();
    let mut xaero = false;
    for crx in 0..SIZE_IN_CHUNKS {
        let index = crz * SIZE_IN_CHUNKS + crx;
        let Some(chunk) = &mca.chunks[index] else { continue };
        let Some(data) = &chunk.data else { continue };
        let (format, cm) = dispatch.get(nbt::data_version(Some(data)));
        let x = crx * chunk_size;
        xaero |= draw_one(
            data,
            cm,
            format,
            x,
            0,
            scale,
            cfg,
            corrupted,
            size,
            pixel_band.len(),
            pixel_band,
            terrain_band,
            water_px_band.as_deref_mut(),
            water_h_band.as_deref_mut(),
        );
    }
    xaero
}

/// The original sequential loop, kept verbatim for scales where the
/// renderers' internal row stride (`512 >> ilog2(scale)`) does not equal
/// `size` — a band split would not be a pure partition there.
#[allow(clippy::too_many_arguments)]
fn draw_sequential(
    mca: &McaFile,
    scale: i32,
    cfg: &RenderSettings,
    corrupted: &[u32; 256],
    size: usize,
    chunk_size: usize,
    pixels: usize,
    pixel_buffer: &mut [u32],
    terrain_heights: &mut [i16],
    water_pixels: &mut Vec<u32>,
    water_heights: &mut Vec<i16>,
) -> bool {
    let mut dispatch = Dispatch::new();
    let mut xaero = false;
    for cx in 0..SIZE_IN_CHUNKS {
        for cz in 0..SIZE_IN_CHUNKS {
            let index = cz * SIZE_IN_CHUNKS + cx;
            let Some(chunk) = &mca.chunks[index] else { continue };
            let Some(data) = &chunk.data else { continue };
            let (format, cm) = dispatch.get(nbt::data_version(Some(data)));
            let x = cx * chunk_size;
            let z = cz * chunk_size;
            xaero |= draw_one(
                data,
                cm,
                format,
                x,
                z,
                scale,
                cfg,
                corrupted,
                size,
                pixels,
                pixel_buffer,
                terrain_heights,
                if !water_pixels.is_empty() {
                    Some(water_pixels.as_mut_slice())
                } else {
                    None
                },
                if !water_heights.is_empty() {
                    Some(water_heights.as_mut_slice())
                } else {
                    None
                },
            );
        }
    }
    xaero
}

/// One chunk's draw_chunk/draw_layer/draw_caves + the corrupted-overlay
/// fallback. `x`/`z` are pixel offsets into the given buffer slices, which
/// address `buf_size`-wide rows; overlay writes are clipped at `buf_len`
/// (the full buffer length sequentially, the band length in `draw_bands`).
/// upstream wraps each chunk's draw in try/catch and paints the corrupted
/// overlay on failure — catch_unwind is the equivalent.
#[allow(clippy::too_many_arguments)]
fn draw_one(
    data: &nbt::Compound,
    cm: &version::ColorMap,
    format: version::ChunkFormat,
    x: usize,
    z: usize,
    scale: i32,
    cfg: &RenderSettings,
    corrupted: &[u32; 256],
    buf_size: usize,
    buf_len: usize,
    pixel_buffer: &mut [u32],
    terrain_heights: &mut [i16],
    mut water_pixels: Option<&mut [u32]>,
    mut water_heights: Option<&mut [i16]>,
) -> bool {
    let mut xaero_used = false;
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut out = RenderOut {
            pixel_buffer: &mut *pixel_buffer,
            water_pixels: water_pixels.as_deref_mut(),
            terrain_heights: &mut *terrain_heights,
            water_heights: water_heights.as_deref_mut(),
            water: cfg.shade && cfg.shade_water,
            height: cfg.render_height,
            full_cave: cfg.dim == DimKind::Nether,
            xaero: false,
        };
        if cfg.caves {
            version::draw_caves(data, cm, format, x, z, scale, out.pixel_buffer, out.terrain_heights, cfg.render_height);
        } else if cfg.layer_only {
            version::draw_layer(data, cm, format, x, z, scale, out.pixel_buffer, cfg.render_height);
        } else {
            version::draw_chunk(data, cm, format, x, z, scale, &mut out);
            xaero_used = out.xaero;
        }
    }));
    if res.is_err() {
        // corruptedChunkOverlay + heights = 64 (upstream drawChunkImage)
        for cy in 0..CHUNK_SIZE {
            for cxi in 0..CHUNK_SIZE {
                let src = cy * CHUNK_SIZE + cxi;
                let dst = (z + cy / scale as usize) * buf_size + (x + cxi / scale as usize);
                if dst < buf_len {
                    pixel_buffer[dst] = corrupted[src];
                    terrain_heights[dst] = 64;
                    if let Some(wh) = water_heights.as_deref_mut() {
                        wh[dst] = 64;
                    }
                }
            }
        }
    }
    xaero_used
}

fn flat_shade(px: &mut [u32], heights: &[i16], _scale: i32) {
    px.par_iter_mut().enumerate().for_each(|(i, p)| {
        let alt = (heights[i] / 4).clamp(-50, 50);
        *p = shade_color(*p, (alt * 4) as i32);
    });
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
    if size == 0 {
        return;
    }
    let altitude_multiplier = if cfg.shade_altitude { 12.0 / 256.0 } else { 0.0 };
    // rows only read shared height/color buffers — disjoint prow bands make
    // the pass trivially parallel with identical per-pixel math. Only the
    // first size*size pixels form rows (for odd scales pixels can be
    // larger); the tail stays untouched exactly like the sequential loop.
    px[..size * size]
        .par_chunks_mut(size)
        .enumerate()
        .for_each(|(z, prow)| {
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
        });
}

/// Xaero's MapPixel.getPixelColours shading, applied as a tile post-pass
/// (their updateBuffers step). Per pixel:
///   brightness = (9 + max(sun_left, 0)) / 24   when overlays are present
///   * depth    = height/63 clamped [0.9, 1.0]   (terrain_slopes=2 bounds)
///   * slope    = shadow*0.2 + 0.5 + directLight — cosine of the surface
///     direction (vs = h - hN, ds = h - hNW) against a diagonal light
///   final rgb  = base*brightness*acc_tr + overlay_rgb
/// water_pixels carries accTr<<24|premult overlay rgb; water_heights
/// carries topHeight|sun<<12; pixel alpha 0xFE marks glowing blocks.
fn xaero_shade(
    px: &mut [u32],
    overlay_px: &[u32],
    terrain: &[i16],
    top: &[i16],
    scale: i32,
    cfg: &RenderSettings,
) {
    let size = TILE_SIZE / scale as usize;
    if size == 0 || overlay_px.is_empty() {
        return;
    }
    // MapDimension ambient shadow + dimension height for the depth curve.
    let (shadow, dim_h) = match cfg.dim {
        DimKind::Overworld => ([0.518f32, 0.678, 1.0], 384.0f32),
        DimKind::Nether => ([1.0f32, 0.0, 0.0], 256.0f32),
        DimKind::End => ([1.0f32, 1.0, 1.0], 256.0f32),
    };
    let max_direct = 0.666_666_7f32;
    px[..size * size]
        .par_chunks_mut(size)
        .enumerate()
        .for_each(|(z, prow)| {
            let hrow = &terrain[z * size..(z + 1) * size];
            let hup = if z == 0 {
                hrow
            } else {
                &terrain[(z - 1) * size..z * size]
            };
            let orow = &overlay_px[z * size..(z + 1) * size];
            let trow = &top[z * size..(z + 1) * size];
            for x in 0..size {
                let argb = prow[x];
                if argb == 0 {
                    continue;
                }
                let glowing = (argb >> 24) == 0xFE;
                let h = hrow[x] as f32;
                let ov = orow[x];
                let (mut br, mut bg, mut bb, acc_tr, or_, og_, ob_) = if ov != 0 {
                    let acc_tr = (ov >> 24 & 0xFF) as f32 / 255.0;
                    let sun = ((trow[x] as u16) >> 12) as i32;
                    let b = if glowing {
                        1.0
                    } else {
                        (9 + sun.max(0)) as f32 / 24.0
                    };
                    (
                        b,
                        b,
                        b,
                        acc_tr,
                        (ov >> 16 & 0xFF) as f32,
                        (ov >> 8 & 0xFF) as f32,
                        (ov & 0xFF) as f32,
                    )
                } else {
                    (1.0, 1.0, 1.0, 1.0, 0.0, 0.0, 0.0)
                };
                if cfg.shade_altitude && !glowing {
                    // terrainDepth — nether caves use the dim-height curve.
                    let depth = if cfg.dim == DimKind::Nether {
                        0.7 + 0.3 * h / dim_h
                    } else {
                        h / 63.0
                    };
                    let depth = depth.clamp(0.9, 1.0);
                    br *= depth;
                    bg *= depth;
                    bb *= depth;
                }
                if cfg.shade {
                    // slopeUnknown when a neighbor pixel is empty (or at a
                    // tile edge) — Xaero leaves those unshaded.
                    let (vs, ds, known) = if z > 0 && x > 0 && hup[x] != 0 && hup[x - 1] != 0 {
                        (
                            h - hup[x] as f32,
                            h - hup[x - 1] as f32,
                            true,
                        )
                    } else {
                        (0.0, 0.0, false)
                    };
                    if known {
                        let (amb_c, amb_w, md) = if glowing {
                            (0.0, 1.0, 0.222_222_24f32)
                        } else {
                            (0.2, 0.5, max_direct)
                        };
                        // terrain_slopes == 2: N+NW cross-product vs light dir
                        let cross_z = -vs;
                        let cos = if cross_z < 1.0 {
                            if vs == 1.0 && ds == 1.0 {
                                1.0
                            } else {
                                let cross_x = vs - ds;
                                let cast = 1.0 - cross_z;
                                let mag = (cross_x * cross_x + 1.0 + cross_z * cross_z).sqrt();
                                cast / mag / std::f32::consts::SQRT_2
                            }
                        } else {
                            0.0
                        };
                        let direct = if cos == 1.0 {
                            md
                        } else if cos > 0.0 {
                            (cos * 10.0).ceil() / 10.0 * md * 0.883_88
                        } else {
                            0.0
                        };
                        let white = amb_w + direct;
                        br *= shadow[0] * amb_c + white;
                        bg *= shadow[1] * amb_c + white;
                        bb *= shadow[2] * amb_c + white;
                    }
                }
                let r = ((argb >> 16 & 0xFF) as f32 * br * acc_tr + or_).min(255.0) as u32;
                let g = ((argb >> 8 & 0xFF) as f32 * bg * acc_tr + og_).min(255.0) as u32;
                let b = ((argb & 0xFF) as f32 * bb * acc_tr + ob_).min(255.0) as u32;
                prow[x] = 0xFF00_0000 | r << 16 | g << 8 | b;
            }
        });
}

/// generateImageFromChunkSet — the green LOD tile used at high zoom levels.
pub fn lod_image(chunk_set: &crate::selection::ChunkSet, color: u32) -> Vec<u32> {
    let mut pixels = vec![0u32; 1024];
    for i in chunk_set.iter() {
        pixels[i] = color;
    }
    pixels
}

/// scaleDownFXImage — box-average downscale of a square argb buffer
/// (Xaero's branch textures are recursive 2x2 box averages).
pub fn scale_down(img: &[u32], from_size: usize, to_size: usize) -> Vec<u32> {
    let factor = from_size / to_size;
    let mut out = vec![0u32; to_size * to_size];
    if factor == 0 {
        // upscale: every pixel samples img[0]
        out.fill(img[0]);
        return out;
    }
    for (y, row) in out.chunks_exact_mut(to_size).enumerate() {
        for (x, p) in row.iter_mut().enumerate() {
            let mut a = 0u32;
            let mut r = 0u32;
            let mut g = 0u32;
            let mut b = 0u32;
            for dy in 0..factor {
                let base = (y * factor + dy) * from_size + x * factor;
                for v in &img[base..base + factor] {
                    a += v >> 24 & 0xFF;
                    r += v >> 16 & 0xFF;
                    g += v >> 8 & 0xFF;
                    b += v & 0xFF;
                }
            }
            let n = (factor * factor) as u32;
            *p = (a / n) << 24 | (r / n) << 16 | (g / n) << 8 | (b / n);
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
