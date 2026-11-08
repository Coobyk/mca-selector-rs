//! Region-level rendering: parse and render every chunk of a region file
//! in parallel (rayon), assemble the region-wide column grid, and run the
//! Xaero color math (which needs cross-chunk biome blending and neighbor
//! heights) over it.

use std::path::Path;

use rayon::prelude::*;

use super::{chunk_columns, render_columns, ChunkColumns, ColumnGrid};
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
}

/// Render one region file. The heavy work (zlib + NBT) runs on the rayon
/// pool; this function blocks until the region is done, so it belongs on a
/// background thread.
pub fn render_region_file(path: &Path, coord: (i32, i32)) -> Option<RegionRender> {
    let region = Region::open(path).ok()?;

    let mut slots: Vec<(i32, i32)> = Vec::new();
    for lz in 0..32i32 {
        for lx in 0..32i32 {
            if region.has_chunk(lx as u8, lz as u8).unwrap_or(false) {
                slots.push((lx, lz));
            }
        }
    }

    // Pass 1: per-chunk column data, in parallel.
    let rendered: Vec<(usize, ChunkColumns, Vec<String>)> = slots
        .into_par_iter()
        .filter_map(|(lx, lz)| {
            let bytes = region.chunk_data(lx as u8, lz as u8).ok()?;
            let chunk = crate::chunk_fast::parse_chunk(&bytes)?;
            // Biome ids are localized per chunk here and remapped below.
            let mut names: Vec<String> = Vec::new();
            let mut id = |n: &str| -> u16 {
                if let Some(i) = names.iter().position(|b| b == n) {
                    return i as u16;
                }
                names.push(n.to_string());
                (names.len() - 1) as u16
            };
            let cols = chunk_columns(&chunk, &mut id)?;
            Some(((lz as usize) * 32 + lx as usize, cols, names))
        })
        .collect();

    if rendered.is_empty() {
        return None;
    }

    // Assemble the region-wide column grid with a shared biome table.
    let mut grid = ColumnGrid::empty(REGION_IMG, REGION_IMG);
    for (slot, cols, names) in &rendered {
        let lx = (*slot % 32) as usize;
        let lz = (*slot / 32) as usize;
        let ox = lx * 16;
        let oz = lz * 16;
        for z in 0..16usize {
            for x in 0..16usize {
                let si = z * 16 + x;
                let di = (oz + z) * REGION_IMG + ox + x;
                // Chunks without any biome data (partially generated)
                // still render; their tint falls back to the defaults.
                let biome_name = names
                    .get(cols.biome[si] as usize)
                    .map(|s| s.as_str())
                    .unwrap_or("plains");
                let gid = grid.biome_id(biome_name);
                grid.base[di] = cols.base[si];
                grid.tint[di] = cols.tint[si];
                grid.water_depth[di] = cols.water_depth[si];
                grid.floor_height[di] = cols.floor_height[si];
                grid.biome[di] = gid;
                grid.glowing[di] = cols.glowing[si];
            }
        }
    }

    let img512 = render_columns(&grid)?;

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
        chunk_count: rendered.len() as u32,
    })
}
