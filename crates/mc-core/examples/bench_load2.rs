fn main() {
    let path = std::env::args().nth(1).unwrap();
    let raw: bool = std::env::args().nth(2).map(|s| s == "raw").unwrap_or(true);
    let t0 = std::time::Instant::now();
    let mut m = mc_core::mca::McaFile::new(std::path::PathBuf::from(path)).unwrap();
    m.load(raw).unwrap();
    eprintln!("load(raw={raw}): {:.1}ms", t0.elapsed().as_secs_f64() * 1e3);
}
