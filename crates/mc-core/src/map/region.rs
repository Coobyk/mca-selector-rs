//! Region-level rendering: parse and render every chunk of a region file in
//! parallel (rayon) and composite the tiles into one image. Shading runs
//! after all chunks are rendered, using cross-chunk height lookups, so
//! terrain reads as one continuous surface.

use std::path::Path;

use rayon::prelude::*;

use super::{render_tile_raw, shade_tile, TileRaw};
use crate::region::Region;

/// Side of the composited region image (one pixel per block).
pub const REGION_IMG: usize = 512;
/// Side of the permanently cached base image (2px per block).
pub const BASE_IMG: usize = 256;

/// Result of rendering one region file.
pub struct RegionRender {
    pub coord: (i32, i32),
    /// Full-resolution composite, RGBA, `REGION_IMG * REGION_IMG * 4` bytes.
    pub img512: Vec<u8>,
    /// Half-resolution base composite for the permanent cache.
    pub base256: Vec<u8>,
    pub chunk_count: u32,
    pub sum: [u64; 3],
}

/// Render one region file. The heavy work (zlib + NBT + tile colors) runs
/// on the rayon pool; this function blocks until the region is done, so it
/// belongs on a background thread.
pub fn render_region_file(path: &Path, coord: (i32, i32)) -> Option<RegionRender> {
    let region = Region::open(path).ok()?;

    // Collect present chunk slots first so rayon can spread them.
    let mut slots: Vec<(i32, i32)> = Vec::new();
    for lz in 0..32i32 {
        for lx in 0..32i32 {
            if region.has_chunk(lx as u8, lz as u8).unwrap_or(false) {
                slots.push((lx, lz));
            }
        }
    }

    // Pass 1: unshaded colors + heights per chunk, in parallel.
    let mut raws: Vec<Option<TileRaw>> = (0..32 * 32).map(|_| None).collect();
    let rendered: Vec<(usize, TileRaw)> = slots
        .into_par_iter()
        .filter_map(|(lx, lz)| {
            let bytes = region.chunk_data(lx as u8, lz as u8).ok()?;
            let chunk = crate::chunk_fast::parse_chunk(&bytes)?;
            let raw = render_tile_raw(&chunk)?;
            Some((((lz as usize) * 32 + lx as usize), raw))
        })
        .collect();
    for (slot, raw) in rendered {
        raws[slot] = Some(raw);
    }

    // Cross-chunk height lookup over the whole 512x512 region area. Columns
    // outside the region resolve to None, which falls back to the column's
    // own height; the drawn region border covers that seam.
    let height_at_region = |gx: i32, gz: i32| -> Option<i32> {
        if !(0..512).contains(&gx) || !(0..512).contains(&gz) {
            return None;
        }
        let raw = raws[((gz / 16) * 32 + gx / 16) as usize].as_ref()?;
        let h = raw.heights[((gz % 16) * 16 + gx % 16) as usize];
        if h == i32::MIN {
            None
        } else {
            Some(h)
        }
    };

    // Pass 2: shade with neighbor heights and composite.
    let mut img512 = vec![0u8; REGION_IMG * REGION_IMG * 4];
    let mut sum = [0u64; 3];
    let mut count = 0u32;
    for z in 0..REGION_IMG {
        for x in 0..REGION_IMG {
            let o = (z * REGION_IMG + x) * 4;
            img512[o] = 0x22;
            img512[o + 1] = 0x25;
            img512[o + 2] = 0x2c;
            img512[o + 3] = 0xff;
        }
    }
    for lz in 0..32i32 {
        for lx in 0..32i32 {
            let Some(raw) = raws[(lz * 32 + lx) as usize].as_ref() else {
                continue;
            };
            let rgba = shade_tile(raw, &|dx, dz| {
                height_at_region(lx * 16 + dx, lz * 16 + dz)
            });
            let ox = (lx * 16) as usize;
            let oz = (lz * 16) as usize;
            for py in 0..16usize {
                for px in 0..16usize {
                    let si = (py * 16 + px) * 4;
                    let di = ((oz + py) * REGION_IMG + ox + px) * 4;
                    img512[di..di + 4].copy_from_slice(&rgba[si..si + 4]);
                }
            }
            sum[0] += u64::from(raw.avg[0]);
            sum[1] += u64::from(raw.avg[1]);
            sum[2] += u64::from(raw.avg[2]);
            count += 1;
        }
    }

    // Box-filter downscale 512 -> 256 (2x2 average).
    let mut base256 = vec![0u8; BASE_IMG * BASE_IMG * 4];
    for z in 0..BASE_IMG {
        for x in 0..BASE_IMG {
            let mut acc = [0u32; 4];
            for dy in 0..2usize {
                for dx in 0..2usize {
                    let si = ((z * 2 + dy) * REGION_IMG + (x * 2 + dx)) * 4;
                    for c in 0..4 {
                        acc[c] += u32::from(img512[si + c]);
                    }
                }
            }
            let di = (z * BASE_IMG + x) * 4;
            for c in 0..4 {
                base256[di + c] = (acc[c] / 4) as u8;
            }
        }
    }

    Some(RegionRender {
        coord,
        img512,
        base256,
        chunk_count: count,
        sum,
    })
}
