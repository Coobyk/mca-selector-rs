use mc_core::{mca::McaFile, render::{generate_image, RenderSettings, DimKind}};
fn main() {
    let path = std::env::args().nth(1).unwrap();
    let out = std::env::args().nth(2).unwrap_or("/tmp/water.png".into());
    let mut m = McaFile::new(path.into()).unwrap();
    m.load(true).unwrap();
    let cfg = RenderSettings { render_height: 319, layer_only: false, caves: false,
        shade: true, shade_water: true, shade_altitude: true, dim: DimKind::Overworld };
    let img = generate_image(&m, 1, &cfg, &[0;256]).unwrap();
    let rgba: Vec<u8> = img.iter().flat_map(|p| {
        [((p >> 16) & 0xff) as u8, ((p >> 8) & 0xff) as u8, (p & 0xff) as u8, 255]
    }).collect();
    image::save_buffer(&out, &rgba, 512, 512, image::ColorType::Rgba8).unwrap();
    // dump the most common colors to spot the water palette
    let mut hist = std::collections::HashMap::new();
    for &p in &img { if p != 0 { *hist.entry(p & 0xffffff).or_insert(0usize) += 1; } }
    let mut v: Vec<_> = hist.into_iter().collect();
    v.sort_by_key(|&(_, n)| std::cmp::Reverse(n));
    for (c, n) in v.iter().take(15) { println!("{c:06x} x{n}"); }
    println!("wrote {out}");
}
