//! Render one region file to PNG for visual verification.
//! MCA_REGION_PATH=/path/to/r.x.z.mca MCA_OUT=/tmp/out.png cargo test -p mc-core render_to_png -- --nocapture --ignored

use mc_core::mca::McaFile;
use mc_core::render::{generate_image, DimKind, RenderSettings};
use std::path::PathBuf;

#[test]
#[ignore]
fn render_to_png() {
    let path = std::env::var("MCA_REGION_PATH").expect("MCA_REGION_PATH");
    let out = std::env::var("MCA_OUT").unwrap_or_else(|_| "/tmp/region.png".into());
    let mut m = McaFile::new(PathBuf::from(&path)).expect("region file");
    m.load(true).expect("load");
    let corrupted = [0u32; 256];
    let dim = match std::env::var("MCA_DIM").as_deref() {
        Ok("nether") => DimKind::Nether,
        Ok("end") => DimKind::End,
        _ => DimKind::Overworld,
    };
    let cfg = RenderSettings {
        dim,
        ..RenderSettings::default()
    };
    let img = generate_image(&m, 1, &cfg, &corrupted, &mc_core::render::Margins::EMPTY).expect("image");
    let mut rgba = Vec::with_capacity(img.len() * 4);
    for &p in img.iter() {
        rgba.push((p >> 16) as u8);
        rgba.push((p >> 8) as u8);
        rgba.push(p as u8);
        rgba.push((p >> 24) as u8);
    }
    image::save_buffer(&out, &rgba, 512, 512, image::ColorType::Rgba8).unwrap();
    eprintln!("wrote {out}");
}
