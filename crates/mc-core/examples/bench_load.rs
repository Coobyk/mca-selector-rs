// quick load benchmark
fn main() {
    let path = std::env::args().nth(1).unwrap();
    let t0 = std::time::Instant::now();
    let mut m = mc_core::mca::McaFile::new(std::path::PathBuf::from(path)).unwrap();
    m.load(true).unwrap();
    eprintln!("load: {:.1}ms", t0.elapsed().as_secs_f64() * 1e3);
    let corrupted = [0u32; 256];
    let cfg = mc_core::render::RenderSettings::default();
    let t1 = std::time::Instant::now();
    let _img = mc_core::render::generate_image(&m, 1, &cfg, &corrupted).unwrap();
    eprintln!("render: {:.1}ms", t1.elapsed().as_secs_f64() * 1e3);
}
