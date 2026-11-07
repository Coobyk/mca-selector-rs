//! Render a world to a PNG using the Xaero-parity region pipeline.
//! Usage: `cargo run --release --example render -- <world-dir> [dimension] [out.png]`

use std::time::Instant;

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
    let files = world.region_files(dim)?;
    let regions: Vec<(i32, i32, Vec<u8>)> = files
        .par_iter()
        .filter_map(|(rx, rz, path)| {
            let rendered = mc_core::map::region::render_region_file(path, (*rx, *rz))?;
            Some((*rx, *rz, rendered.img512))
        })
        .collect();
    println!(
        "rendered {} regions in {:?}",
        regions.len(),
        started.elapsed()
    );

    let min_rx = regions.iter().map(|(x, _, _)| *x).min().unwrap();
    let max_rx = regions.iter().map(|(x, _, _)| *x).max().unwrap();
    let min_rz = regions.iter().map(|(_, z, _)| *z).min().unwrap();
    let max_rz = regions.iter().map(|(_, z, _)| *z).max().unwrap();
    let rsize = mc_core::map::region::REGION_IMG as u32;
    let width = (max_rx - min_rx + 1) as u32 * rsize;
    let height = (max_rz - min_rz + 1) as u32 * rsize;
    let mut image_vec = vec![0u8; (width * height * 4) as usize];
    for (rx, rz, img) in &regions {
        let ox = ((rx - min_rx) as u32 * rsize) as usize;
        let oz = ((rz - min_rz) as u32 * rsize) as usize;
        for y in 0..rsize as usize {
            let di = ((oz + y) * width as usize + ox) * 4;
            let si = y * rsize as usize * 4;
            let len = rsize as usize * 4;
            image_vec[di..di + len].copy_from_slice(&img[si..si + len]);
        }
    }
    let img = image::RgbaImage::from_raw(width, height, image_vec)
        .ok_or_else(|| anyhow::anyhow!("image size mismatch"))?;
    img.save(&out)?;
    println!("saved {out}");
    Ok(())
}
