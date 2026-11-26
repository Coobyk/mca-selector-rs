//! Port of tile.TileImage.generateImage + the shading passes and LOD images.

use crate::mca::McaFile;
use crate::nbt;
use crate::version::{self, RenderOut};
#[cfg(not(target_arch = "wasm32"))]
use rayon::prelude::*;

#[cfg(not(target_arch = "wasm32"))]
fn tile_chunks<T: Send>(slice: &mut [T], size: usize) -> impl IndexedParallelIterator<Item = &mut [T]> {
    slice.par_chunks_mut(size)
}

#[cfg(target_arch = "wasm32")]
fn tile_chunks<T>(slice: &mut [T], size: usize) -> impl Iterator<Item = &mut [T]> {
    slice.chunks_mut(size)
}

#[cfg(not(target_arch = "wasm32"))]
fn any_drawn(bands: impl ParallelIterator<Item = bool>) -> bool {
    bands.reduce(|| false, |a, b| a || b)
}

#[cfg(target_arch = "wasm32")]
fn any_drawn(bands: impl Iterator<Item = bool>) -> bool {
    bands.fold(false, |a, b| a || b)
}

pub const TILE_SIZE: usize = 512;
pub const CHUNK_SIZE: usize = 16;
pub const SIZE_IN_CHUNKS: usize = 32;

/// shared-edge chunk lists the margins load/render — tilemap's
/// MarginFiles::load sparse-loads exactly these chunks of each neighbor.
const fn edge_row(cz: usize) -> [usize; SIZE_IN_CHUNKS] {
    let mut a = [0usize; SIZE_IN_CHUNKS];
    let mut cx = 0;
    while cx < SIZE_IN_CHUNKS { a[cx] = cz * SIZE_IN_CHUNKS + cx; cx += 1; }
    a
}
const fn edge_col(cx: usize) -> [usize; SIZE_IN_CHUNKS] {
    let mut a = [0usize; SIZE_IN_CHUNKS];
    let mut cz = 0;
    while cz < SIZE_IN_CHUNKS { a[cz] = cz * SIZE_IN_CHUNKS + cx; cz += 1; }
    a
}
pub const MARGIN_IDX_N: [usize; SIZE_IN_CHUNKS] = edge_row(31);
pub const MARGIN_IDX_S: [usize; SIZE_IN_CHUNKS] = edge_row(0);
pub const MARGIN_IDX_W: [usize; SIZE_IN_CHUNKS] = edge_col(31);
pub const MARGIN_IDX_E: [usize; SIZE_IN_CHUNKS] = edge_col(0);
pub const MARGIN_IDX_NW: [usize; 1] = [1023];

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

/// How water surfaces render, derived from `shade`/`shade_water` — the
/// three legal combinations of what used to be three separate booleans on
/// RenderOut (`water`, `flat`, `no_water`).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum WaterMode {
    /// water surface drawn, merged over the terrain with depth shading.
    Shaded,
    /// water surface drawn flat and opaque (shade off, water on).
    Flat,
    /// water skipped — the scanner keeps going to expose the seabed.
    Hidden,
}

impl RenderSettings {
    /// whether margin strips should be rendered (edge shading needs them;
    /// layer-only renders have no neighbors to blend with). The tilemap
    /// uses the same predicate to decide whether to load margin files.
    pub(crate) fn wants_margins(&self) -> bool {
        !self.layer_only && (self.shade || self.caves)
    }

    pub(crate) fn water_mode(&self) -> WaterMode {
        if !self.shade_water {
            WaterMode::Hidden
        } else if self.shade {
            WaterMode::Shaded
        } else {
            WaterMode::Flat
        }
    }
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

pub(crate) fn shade_color(argb: u32, shade: i32) -> u32 {
    let r = (((argb >> 16) & 0xFF) as i32 + shade).clamp(0, 255) as u32;
    let g = (((argb >> 8) & 0xFF) as i32 + shade).clamp(0, 255) as u32;
    let b = ((argb & 0xFF) as i32 + shade).clamp(0, 255) as u32;
    (argb & 0xFF00_0000) | (r << 16) | (g << 8) | b
}

pub(crate) fn blend(a: u32, b: u32, ratio: f32) -> u32 {
    let ir = ((a >> 16 & 0xFF) as f32 * ratio + (b >> 16 & 0xFF) as f32 * (1.0 - ratio)) as u32;
    let ig = ((a >> 8 & 0xFF) as f32 * ratio + (b >> 8 & 0xFF) as f32 * (1.0 - ratio)) as u32;
    let ib = ((a & 0xFF) as f32 * ratio + (b & 0xFF) as f32 * (1.0 - ratio)) as u32;
    0xFF00_0000 | (ir << 16) | (ig << 8) | ib
}

/// generateImage(mcaFile, scale) -> ARGB pixel buffer of 512/scale × 512/scale.
/// `corrupted` is a 16x16 argb overlay used when a chunk fails to render.
/// Neighbor region files supplying real cross-boundary heights so shading
/// doesn't have to fake it at tile edges. Load each with
/// `McaFile::load_sparse` — only the shared-edge chunks are needed (32 per
/// side, 1 for the corner).
#[derive(Default)]
pub struct Margins<'a> {
    pub n: Option<&'a McaFile>,
    pub s: Option<&'a McaFile>,
    pub e: Option<&'a McaFile>,
    pub w: Option<&'a McaFile>,
    pub nw: Option<&'a McaFile>,
}

impl Margins<'_> {
    pub const EMPTY: Margins<'static> = Margins {
        n: None,
        s: None,
        e: None,
        w: None,
        nw: None,
    };

    fn any(&self) -> bool {
        self.n.is_some()
            || self.s.is_some()
            || self.e.is_some()
            || self.w.is_some()
            || self.nw.is_some()
    }
}

/// Surface heights one pixel beyond each tile edge, sampled from the
/// neighbors' real edge chunks at this tile's pixel resolution.
/// `n` holds size+1 entries: index 0 is the NW corner column (-1,-1),
/// indices 1..=size are the (x,-1) row. s/e/w hold `size` entries for
/// the (x,size), (size,z) and (-1,z) edges. Empty vecs mean "no margin"
/// — callers fall back to edge clamping.
#[derive(Default)]
struct MarginStrips {
    n: Vec<i16>,
    s: Vec<i16>,
    e: Vec<i16>,
    w: Vec<i16>,
}

#[derive(Default)]
struct MarginSet {
    terrain: MarginStrips,
    water: MarginStrips,
}

pub fn generate_image(
    mca: &McaFile,
    scale: i32,
    cfg: &RenderSettings,
    corrupted: &[u32; 256],
    margins: &Margins,
) -> Option<Vec<u32>> {
    let size = TILE_SIZE / scale as usize;
    let chunk_size = CHUNK_SIZE / scale as usize;
    // at scale 32 a chunk spans half a pixel (chunk_size=0): the per-chunk
    // loops collapse onto pixel 0 and emit an empty image. Render at scale
    // 16 and box-average down instead — keeps real terrain at max zoom-out.
    if chunk_size == 0 {
        let fine = generate_image(mca, scale / 2, cfg, corrupted, margins)?;
        return Some(scale_down(&fine, TILE_SIZE / (scale / 2) as usize, size));
    }
    let pixels = TILE_SIZE * TILE_SIZE / (scale * scale) as usize;

    let mut pixel_buffer = vec![0u32; pixels];
    // the xaero surface pipeline writes these unconditionally; cave and
    // layer-only renders never read them.
    let aux = !cfg.caves && !cfg.layer_only;
    let mut water_pixels = if aux { vec![0u32; pixels] } else { Vec::new() };
    let mut terrain_heights = vec![0i16; pixels];
    let mut water_heights = if aux { vec![0i16; pixels] } else { Vec::new() };

    // The renderers address the buffers with a row stride of
    // `512 >> ilog2(scale)`; splitting the buffers into per-chunk-row bands
    // is a pure partition only when that stride equals `size`, i.e. for
    // power-of-two scales that keep chunk_size > 0 (1,2,4,8,16). Anything
    // else runs the original sequential loop verbatim.
    // The renderers address the buffers with a row stride of
    // `512 >> ilog2(scale)`, which equals `size` only for power-of-two
    // scales — the only values get_zoom_level() produces.
    let xaero_used = draw_bands(
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
    );
    // Real heights one pixel past each edge: without them the shading
    // passes can't compute the true slope at region boundaries and draw a
    // visible 512px grid (the "region seams" issue). Costs ~130 edge-chunk
    // scans spread over the 5 margin files.
    let mset = if margins.any() && cfg.wants_margins() {
        margin_strips(margins, scale, cfg, corrupted, size, chunk_size)
    } else {
        MarginSet::default()
    };

    if xaero_used && !cfg.caves && !cfg.layer_only {
        xaero_shade(
            &mut pixel_buffer,
            &water_pixels,
            &terrain_heights,
            &water_heights,
            scale,
            cfg,
            &mset.terrain,
        );
    } else if cfg.caves {
        flat_shade(&mut pixel_buffer, &terrain_heights, scale, &mset.terrain);
    } else if cfg.shade && !cfg.layer_only {
        let ms = if cfg.shade_water {
            &mset.water
        } else {
            &mset.terrain
        };
        shade(
            &mut pixel_buffer,
            &water_pixels,
            &terrain_heights,
            &water_heights,
            scale,
            cfg,
            ms,
        );
    }
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
        any_drawn(tile_chunks(pixel_buffer, band)
            .zip(tile_chunks(terrain_heights, band))
            .enumerate()
            .map(|(crz, (pb, th))| {
                draw_band(mca, crz, scale, cfg, corrupted, chunk_size, size, pb, th, None, None)
            }))
    } else {
        any_drawn(tile_chunks(pixel_buffer, band)
            .zip(tile_chunks(terrain_heights, band))
            .zip(tile_chunks(water_pixels, band))
            .zip(tile_chunks(water_heights, band))
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
            }))
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
            water: cfg.water_mode(),
            flat: !cfg.shade,
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

/// Render a strip of already-loaded neighbor edge chunks through draw_one
/// and extract the shared-edge heights. Strips draw into a full `size`-row
/// scratch (the chunk renderers hardcode that row stride): N/S place
/// chunks along a row at z=0 and take their last/first pixel row; W/E
/// place chunks down a column at x=0 and take their last/first pixel
/// column; the NW corner is a single chunk's bottom-right pixel.
#[allow(clippy::too_many_arguments)]
fn margin_strips(
    margins: &Margins,
    scale: i32,
    cfg: &RenderSettings,
    corrupted: &[u32; 256],
    size: usize,
    chunk_size: usize,
) -> MarginSet {
    let mut set = MarginSet::default();
    let want_water = cfg.shade && cfg.shade_water;
    let cs = chunk_size;
    let len = size * size;
    let mut px = vec![0u32; len];
    let mut th = vec![0i16; len];
    let mut wp = vec![0u32; len];
    let mut wh = vec![0i16; len];
    let mut dispatch = Dispatch::new();
    let mut run = |m: &McaFile,
                   chunks: &[(usize, usize, usize)],
                   px: &mut [u32],
                   th: &mut [i16],
                   wp: &mut [u32],
                   wh: &mut [i16]| {
        th.fill(0);
        wh.fill(0);
        for &(i, x, z) in chunks {
            let Some(chunk) = &m.chunks[i] else { continue };
            let Some(data) = &chunk.data else { continue };
            let (format, cm) = dispatch.get(nbt::data_version(Some(data)));
            draw_one(
                data,
                cm,
                format,
                x,
                z,
                scale,
                cfg,
                corrupted,
                size,
                len,
                px,
                th,
                Some(&mut *wp),
                Some(&mut *wh),
            );
        }
    };
    // N: the neighbor's southernmost chunk row (cz=31); shared edge is the
    // strip's last row. NW corner prepends the (-1,-1) column height.
    if let Some(m) = margins.n {
        let chunks: Vec<(usize, usize, usize)> =
            (0..32).map(|cx| (31 * 32 + cx, cx * cs, 0)).collect();
        run(m, &chunks, &mut px, &mut th, &mut wp, &mut wh);
        set.terrain.n = th[(cs - 1) * size..cs * size].to_vec();
        if want_water {
            set.water.n = wh[(cs - 1) * size..cs * size].to_vec();
        }
    }
    if let Some(m) = margins.nw {
        run(m, &[(1023, 0, 0)], &mut px, &mut th, &mut wp, &mut wh);
        let last = (cs - 1) * size + cs - 1;
        set.terrain.n.insert(0, th[last]);
        if want_water && !set.water.n.is_empty() {
            set.water.n.insert(0, wh[last]);
        }
    }
    // S: northernmost row (cz=0); shared edge is the strip's first row.
    if let Some(m) = margins.s {
        let chunks: Vec<(usize, usize, usize)> =
            (0..32).map(|cx| (cx, cx * cs, 0)).collect();
        run(m, &chunks, &mut px, &mut th, &mut wp, &mut wh);
        set.terrain.s = th[..size].to_vec();
        if want_water {
            set.water.s = wh[..size].to_vec();
        }
    }
    // W: easternmost column (cx=31); shared edge is the strip's last column.
    if let Some(m) = margins.w {
        let chunks: Vec<(usize, usize, usize)> =
            (0..32).map(|cz| (cz * 32 + 31, 0, cz * cs)).collect();
        run(m, &chunks, &mut px, &mut th, &mut wp, &mut wh);
        set.terrain.w = (0..size).map(|z| th[z * size + cs - 1]).collect();
        if want_water {
            set.water.w = (0..size).map(|z| wh[z * size + cs - 1]).collect();
        }
    }
    // E: westernmost column (cx=0); shared edge is the strip's first column.
    if let Some(m) = margins.e {
        let chunks: Vec<(usize, usize, usize)> =
            (0..32).map(|cz| (cz * 32, 0, cz * cs)).collect();
        run(m, &chunks, &mut px, &mut th, &mut wp, &mut wh);
        set.terrain.e = (0..size).map(|z| th[z * size]).collect();
        if want_water {
            set.water.e = (0..size).map(|z| wh[z * size]).collect();
        }
    }
    set
}

/// Margin lookup: a present strip entry is used as-is (0 = missing chunk,
/// consistent with interior empty-column handling); a missing entry —
/// strip not loaded or too short — falls back to the border pixel.
#[inline]
fn mg(strip: &[i16], i: usize, fallback: i16) -> i16 {
    strip.get(i).copied().unwrap_or(fallback)
}

/// Caves: absolute-height lightness (deep = dark) PLUS the same
/// edge-aware slope shading as the surface pass — pure altitude shading
/// left the cave floor looking flat.
fn flat_shade(px: &mut [u32], heights: &[i16], scale: i32, m: &MarginStrips) {
    let size = TILE_SIZE / scale as usize;
    if size == 0 {
        return;
    }
    let n = (size * size).min(px.len());
    tile_chunks(&mut px[..n], size).enumerate().for_each(|(z, prow)| {
        let hrow = &heights[z * size..(z + 1) * size];
        let hup = if z == 0 {
            hrow
        } else {
            &heights[(z - 1) * size..z * size]
        };
        let hdn = if z == size - 1 && size > 1 {
            hrow
        } else {
            &heights[(z + 1) * size..(z + 2) * size]
        };
        for x in 0..size {
            let h = hrow[x];
            if h <= 0 || prow[x] == 0 {
                continue;
            }
            let hup_x = if z == 0 { mg(&m.n, x + 1, h) } else { hup[x] };
            let hdn_x = if z == size - 1 { mg(&m.s, x, h) } else { hdn[x] };
            let hl = if x == 0 { mg(&m.w, z, h) } else { hrow[x - 1] };
            let hr = if x == size - 1 { mg(&m.e, z, h) } else { hrow[x + 1] };
            let slope = ((hdn_x - hup_x) + (hr - hl)).clamp(-16, 16);
            let alt = (h / 4).clamp(-50, 50) * 4;
            prow[x] = shade_color(prow[x], (alt + slope) as i32);
        }
    });
}

fn shade(
    px: &mut [u32],
    water_px: &[u32],
    terrain: &[i16],
    water: &[i16],
    scale: i32,
    cfg: &RenderSettings,
    m: &MarginStrips,
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
    tile_chunks(&mut px[..size * size], size)
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
                    // margin-aware centered diff: heights one pixel beyond
                    // the tile come from the neighbor region's real edge
                    // chunks; a missing margin falls back to edge clamping
                    // (flat), never to the half-amplitude one-sided diffs
                    // that drew the 512px region grid.
                    let hup_x = if z == 0 { mg(&m.n, x + 1, hrow[x]) } else { hup[x] };
                    let hdn_x = if z == size - 1 { mg(&m.s, x, hrow[x]) } else { hdn[x] };
                    let z_shade = (hdn_x - hup_x) * 2;
                    let hl = if x == 0 { mg(&m.w, z, hrow[x]) } else { hrow[x - 1] };
                    let hr = if x == size - 1 { mg(&m.e, z, hrow[x]) } else { hrow[x + 1] };
                    let x_shade = (hr - hl) * 2;
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
    m: &MarginStrips,
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
    tile_chunks(&mut px[..size * size], size)
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
                    // edge-clamped sampling: the N/NW neighbor outside the
                    // tile replicates the border pixel (flat slope), so the
                    // ambient term still applies — leaving edge pixels
                    // unshaded made a brighter 1px line per region (the
                    // visible "region grid"). Empty neighbors stay unshaded.
                    // N/NW heights one pixel past the tile edge come from
                    // the neighbor's real edge chunks (m.n[0] is the NW
                    // corner column). An absent margin falls back to the
                    // border pixel (flat); a margin of 0 = missing chunk,
                    // unshaded like interior empty columns.
                    let hn = if z == 0 {
                        mg(&m.n, x + 1, hrow[x]) as f32
                    } else {
                        hup[x] as f32
                    };
                    let hnw = if z == 0 && x == 0 {
                        mg(&m.n, 0, hrow[x]) as f32
                    } else if z == 0 {
                        mg(&m.n, x, hrow[x]) as f32
                    } else if x == 0 {
                        mg(&m.w, z - 1, hrow[x]) as f32
                    } else {
                        hup[x - 1] as f32
                    };
                    let (vs, ds, known) = if hn != 0.0 && hnw != 0.0 {
                        (h - hn, h - hnw, true)
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
