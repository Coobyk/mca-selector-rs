use mc_core::{mca::McaFile, render::{generate_image, RenderSettings, DimKind}};
fn main() {
    let dir = std::env::args().nth(1).unwrap();
    let path = format!("{dir}/region/r.0.0.mca");
    let mut m = McaFile::new(path.into()).unwrap();
    m.load(true).unwrap();
    let cfg = RenderSettings { render_height: 319, layer_only: false, caves: false,
        shade: true, shade_water: true, shade_altitude: true, dim: DimKind::Overworld };
    for zoom in [8, 32] {
        let img = generate_image(&m, zoom, &cfg, &[0;256], &mc_core::render::Margins::EMPTY).unwrap();
        let side = (512 / zoom) as usize;
        let csz = 16 / zoom.max(1);
        let mut mism = 0;
        for cz in 0..32 { for cx in 0..32 {
            let exists = m.chunks[cz*32+cx].is_some();
            // sample the pixel nearest chunk center; at zoom 32 chunk<1px
            let px_x = ((cx*16 + 8) as usize / zoom as usize).min(side-1);
            let px_y = ((cz*16 + 8) as usize / zoom as usize).min(side-1);
            let v = img[px_y*side + px_x];
            let lit = v & 0xFF_FFFF != 0;
            if exists && !lit { mism+=1; println!("  zoom{zoom} chunk {cx},{cz} exists but px({px_x},{px_y}) dark {v:08x}"); }
        }}
        println!("zoom {zoom}: {mism} dark-pixel mismatches (csz={csz})");
    }
}
