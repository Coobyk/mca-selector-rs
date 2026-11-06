//! Render a world region range to a PNG — the visual test for the map
//! renderer. Usage: `cargo run --release --example render -- <world-dir>
//! [dimension] [out.png]`.

use std::collections::HashMap;
use std::time::Instant;

use mc_core::chunk::Chunk;
use mc_core::map::{render_tile, TILE_SIZE};
use rayon::prelude::*;

fn main() -> anyhow::Result<()> {
    let root = std::env::args()
        .nth(1)
        .expect("usage: render <world-dir> [dimension] [out.png]");
    let dim_name = std::env::args().nth(2).unwrap_or_else(|| "Overworld".into());
    let out = std::env::args()
        .nth(3)
        .unwrap_or_else(|| "/tmp/map.png".into());

    let world = mc_core::world::World::open(&root)?;
    let dim = world
        .dimension(&dim_name)
        .ok_or_else(|| anyhow::anyhow!("dimension {dim_name:?} not found"))?;

    let started = Instant::now();

    // Render each region's chunks in parallel.
    let files = world.region_files(dim)?;
    let tiles: Vec<(i32, i32, mc_core::map::ChunkTile)> = files
        .par_iter()
        .flat_map_iter(|(rx, rz, path)| {
            let Ok(region) = mc_core::region::Region::open(path) else {
                return Vec::new();
            };
            let mut out = Vec::new();
            for cz in 0..32u8 {
                for cx in 0..32u8 {
                    let tile = match region.chunk_data(cx, cz) {
                        Ok(bytes) => Chunk::from_nbt(&bytes)
                            .map(|c| render_tile(&c))
                            .unwrap_or_else(|_| mc_core::map::empty_tile()),
                        Err(_) => mc_core::map::empty_tile(),
                    };
                    out.push((
                        (*rx) * 32 + i32::from(cx),
                        (*rz) * 32 + i32::from(cz),
                        tile,
                    ));
                }
            }
            out
        })
        .collect();
    let render_time = started.elapsed();

    // Composite: one pixel per chunk (flat avg colors), so the whole scan
    // fits in one image.
    let min_cx = tiles.iter().map(|(x, _, _)| *x).min().unwrap();
    let max_cx = tiles.iter().map(|(x, _, _)| *x).max().unwrap();
    let min_cz = tiles.iter().map(|(_, z, _)| *z).min().unwrap();
    let max_cz = tiles.iter().map(|(_, z, _)| *z).max().unwrap();
    let width = (max_cx - min_cx + 1) as u32 * TILE_SIZE as u32;
    let height = (max_cz - min_cz + 1) as u32 * TILE_SIZE as u32;
    println!(
        "area: chunks ({min_cx},{min_cz})..({max_cx},{max_cz}), image {width}x{height}"
    );

    let mut image_vec = vec![0u8; (width * height * 4) as usize];
    for (cx, cz, tile) in &tiles {
        let ox = ((cx - min_cx) as u32 * TILE_SIZE as u32) as usize;
        let oz = ((cz - min_cz) as u32 * TILE_SIZE as u32) as usize;
        for y in 0..TILE_SIZE {
            for x in 0..TILE_SIZE {
                let (dx, dy) = (ox + x, oz + y);
                let di = (dy * width as usize + dx) * 4;
                let si = (y * TILE_SIZE + x) * 4;
                image_vec[di..di + 4].copy_from_slice(&tile.rgba[si..si + 4]);
            }
        }
    }

    // Write PNG via the image crate.
    let img = image::RgbaImage::from_raw(width, height, image_vec)
        .ok_or_else(|| anyhow::anyhow!("image size mismatch"))?;
    img.save(&out)?;
    println!("rendered {} chunk tiles in {:?}", tiles.len(), render_time);
    println!("saved {out}");
    Ok(())
}
