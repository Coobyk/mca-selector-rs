use std::time::Instant;
use mc_core::chunk_fast::parse_chunk;
use mc_core::region::Region;
use rayon::prelude::*;

fn main() {
    let mut t_read = 0u128;
    let mut t_parse = 0u128;
    let mut t_cols = 0u128;
    let mut t_render = 0u128;
    let mut n = 0;
    for f in std::env::args().skip(1) {
        let region = Region::open(&f).unwrap();
        let mut gbid = |n: &str| -> u16 { 0 };
        for lz in 0..32u8 {
            for lx in 0..32u8 {
                let t = Instant::now();
                let Some(raw) = region.chunk_data(lx, lz).ok() else { continue };
                t_read += t.elapsed().as_nanos();
                let t = Instant::now();
                let Some(chunk) = parse_chunk(&raw) else { continue };
                t_parse += t.elapsed().as_nanos();
                let t = Instant::now();
                let mut grid = mc_core::map::ColumnGrid::empty(16, 16);
                let Some(cols) = mc_core::map::chunk_columns(&chunk, &mut |n| grid.biome_id(n)) else { continue };
                t_cols += t.elapsed().as_nanos();
                let t = Instant::now();
                for z in 0..16usize { for x in 0..16usize {
                    let si = z*16+x; let di = z*16+x;
                    grid.base[di]=cols.base[si]; grid.tint[di]=cols.tint[si];
                    grid.water_depth[di]=cols.water_depth[si]; grid.floor_height[di]=cols.floor_height[si];
                    grid.biome[di]=cols.biome[si]; grid.glowing[di]=cols.glowing[si];
                }}
                // biome names are chunk-local indices already resolved via closure
                let _ = mc_core::map::render_columns(&grid);
                t_render += t.elapsed().as_nanos();
                n += 1;
            }
        }
    }
    println!("chunks: {}", n);
    println!("read+decompress: {:>6.1}ms", t_read as f64/1e6);
    println!("parse_chunk:     {:>6.1}ms", t_parse as f64/1e6);
    println!("chunk_columns:   {:>6.1}ms", t_cols as f64/1e6);
    println!("render_columns:  {:>6.1}ms", t_render as f64/1e6);
}
