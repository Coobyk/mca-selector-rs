//! Export a chunk selection to a world folder from the CLI.
//! Usage: `cargo run --release --example export -- <world-dir> <dimension>
//! <out-dir> <cx,cz> [<cx,cz>...]`

use std::collections::BTreeSet;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (world_dir, dim_name, out_dir, rest) = match args.as_slice() {
        [w, d, o, rest @ ..] if !rest.is_empty() => (w, d, o, rest),
        _ => anyhow::bail!(
            "usage: export <world-dir> <dimension> <out-dir> <cx,cz> [...]"
        ),
    };
    let world = mc_core::world::World::open(world_dir)?;
    let dim = world
        .dimension(dim_name)
        .ok_or_else(|| anyhow::anyhow!("dimension {dim_name:?} not found"))?;
    let mut selection = BTreeSet::new();
    for spec in rest {
        let (cx, cz) = spec
            .split_once(',')
            .ok_or_else(|| anyhow::anyhow!("bad chunk spec {spec:?}"))?;
        selection.insert((cx.parse()?, cz.parse()?));
    }
    let stats = mc_core::export::export_selection(
        &world,
        dim,
        &selection,
        std::path::Path::new(out_dir),
    )?;
    println!("exported {} chunks into {} regions", stats.chunks, stats.regions);
    Ok(())
}
