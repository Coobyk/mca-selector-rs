//! Render a region with + without margin heights and compare the edge
//! columns: if margins work, the boundary pixels shade like interior
//! pixels (no systematic bright/dark line).
//! MCA_REGION_DIR=<region dir> cargo run -p mc-core --example dbg_margins

use mc_core::mca::McaFile;
use mc_core::point::Point2i;
use mc_core::render::{generate_image, DimKind, Margins, RenderSettings};
use std::path::PathBuf;

fn load(dir: &std::path::Path, r: Point2i, indices: &[usize]) -> Option<McaFile> {
    let p = dir.join(format!("r.{}.{}.mca", r.x, r.z));
    if !p.exists() {
        return None;
    }
    let mut m = McaFile::new(p).ok()?;
    if indices.is_empty() {
        m.load(true).ok()?;
    } else {
        m.load_sparse(indices, true).ok()?;
    }
    Some(m)
}

fn save(img: &[u32], path: &str) {
    let mut rgba = Vec::with_capacity(img.len() * 4);
    for &p in img.iter() {
        rgba.extend_from_slice(&[
            (p >> 16) as u8,
            (p >> 8) as u8,
            p as u8,
            (p >> 24) as u8,
        ]);
    }
    image::save_buffer(path, &rgba, 512, 512, image::ColorType::Rgba8).unwrap();
}

/// mean per-channel abs diff between two columns of a 512px image
fn coldiff(img: &[u32], a: usize, b: usize) -> f64 {
    let mut sum = 0f64;
    for z in 0..512 {
        let (pa, pb) = (img[z * 512 + a], img[z * 512 + b]);
        for ch in 0..3 {
            sum += (((pa >> (ch * 8)) & 0xFF) as i32 - ((pb >> (ch * 8)) & 0xFF) as i32).abs() as f64;
        }
    }
    sum / (512.0 * 3.0)
}

fn main() {
    let dir = PathBuf::from(std::env::var("MCA_REGION_DIR").expect("MCA_REGION_DIR"));
    let r = Point2i::new(
        std::env::var("RX").unwrap_or("0".into()).parse().unwrap(),
        std::env::var("RZ").unwrap_or("0".into()).parse().unwrap(),
    );
    let cfg = RenderSettings { dim: DimKind::Overworld, ..RenderSettings::default() };
    let corrupted = [0u32; 256];
    let m = load(&dir, r, &[]).expect("region");

    let without = generate_image(&m, 1, &cfg, &corrupted, &Margins::EMPTY).unwrap();
    save(&without, "/tmp/nomargin.png");

    let row = |cz: i32| (0..32).map(|cx| (cz * 32 + cx) as usize).collect::<Vec<_>>();
    let col = |cx: i32| (0..32).map(|cz| (cz * 32 + cx) as usize).collect::<Vec<_>>();
    let files = [
        load(&dir, Point2i::new(r.x, r.z - 1), &row(31)),
        load(&dir, Point2i::new(r.x, r.z + 1), &row(0)),
        load(&dir, Point2i::new(r.x + 1, r.z), &col(0)),
        load(&dir, Point2i::new(r.x - 1, r.z), &col(31)),
        load(&dir, Point2i::new(r.x - 1, r.z - 1), &[1023]),
    ];
    let margins = Margins {
        n: files[0].as_ref(),
        s: files[1].as_ref(),
        e: files[2].as_ref(),
        w: files[3].as_ref(),
        nw: files[4].as_ref(),
    };
    let with = generate_image(&m, 1, &cfg, &corrupted, &margins).unwrap();
    save(&with, "/tmp/margin.png");

    // continuity metric: |col 510 -> 511| should look like an interior
    // transition |col 300 -> 301|, not a discontinuity.
    for (name, img) in [("no-margin", &without), ("margin", &with)] {
        let interior = coldiff(img, 300, 301);
        let edge = coldiff(img, 510, 511);
        let west = coldiff(img, 0, 1);
        println!("{name}: interior {interior:.2}  east-edge {edge:.2}  west-edge {west:.2}");
    }
    // water-off: render a region known to have ocean (r.-1.-1 has lake at
    // east edge) — underwater terrain should render where water was.
    let cfg_nowater = RenderSettings {
        dim: DimKind::Overworld,
        shade_water: false,
        ..RenderSettings::default()
    };
    let img_nw = generate_image(&m, 1, &cfg_nowater, &corrupted, &margins).unwrap();
    save(&img_nw, "/tmp/nowater.png");
    let mut water_gone = 0usize;
    for z in 0..512usize {
        for x in 0..512usize {
            let a = with[z * 512 + x];
            let b = img_nw[z * 512 + x];
            if a != b {
                water_gone += 1;
            }
        }
    }
    println!("water-off changed {water_gone} px of {}", with.len());
    // stitch a 2x2 grid rendered with margins vs without — the visual test.
    for use_margins in [false, true] {
        let mut big = vec![0u32; 1024 * 1024];
        for dz in 0..2i32 {
            for dx in 0..2i32 {
                let rr = Point2i::new(r.x + dx, r.z + dz);
                let Some(mm) = load(&dir, rr, &[]) else { continue };
                let files2 = [
                    load(&dir, Point2i::new(rr.x, rr.z - 1), &row(31)),
                    load(&dir, Point2i::new(rr.x, rr.z + 1), &row(0)),
                    load(&dir, Point2i::new(rr.x + 1, rr.z), &col(0)),
                    load(&dir, Point2i::new(rr.x - 1, rr.z), &col(31)),
                    load(&dir, Point2i::new(rr.x - 1, rr.z - 1), &[1023]),
                ];
                let mg = if use_margins {
                    Margins {
                        n: files2[0].as_ref(),
                        s: files2[1].as_ref(),
                        e: files2[2].as_ref(),
                        w: files2[3].as_ref(),
                        nw: files2[4].as_ref(),
                    }
                } else {
                    Margins::EMPTY
                };
                let img = generate_image(&mm, 1, &cfg, &corrupted, &mg).unwrap();
                for z in 0..512usize {
                    let src = &img[z * 512..(z + 1) * 512];
                    let dst = &mut big
                        [(dz as usize * 512 + z) * 1024 + dx as usize * 512..][..512];
                    dst.copy_from_slice(src);
                }
            }
        }
        let mut rgba = Vec::with_capacity(big.len() * 4);
        for &p in big.iter() {
            rgba.extend_from_slice(&[
                (p >> 16) as u8,
                (p >> 8) as u8,
                p as u8,
                (p >> 24) as u8,
            ]);
        }
        let name = if use_margins { "/tmp/stitch_m.png" } else { "/tmp/stitch_nm.png" };
        image::save_buffer(name, &rgba, 1024, 1024, image::ColorType::Rgba8).unwrap();
        println!("wrote {name}");
    }
}
