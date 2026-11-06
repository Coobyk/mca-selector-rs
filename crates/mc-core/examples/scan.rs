//! Scan a world and print per-dimension stats, using the fast chunk parser.
//! Usage: `cargo run --release --example scan -- <world-dir> [dimension]`.

use std::time::Instant;

use mc_core::chunk_fast::parse_chunk;
use mc_core::world::RegionBounds;

/// Per-region summary, produced in parallel and folded at the end.
#[derive(Default)]
struct RegionSummary {
    chunks: u64,
    versions: std::collections::BTreeMap<i32, u64>,
    statuses: std::collections::BTreeMap<String, u64>,
    sample: Option<(i32, i32, String)>,
}

fn main() -> anyhow::Result<()> {
    let root = std::env::args()
        .nth(1)
        .expect("usage: scan <world-dir> [dimension]");
    let want = std::env::args().nth(2);

    let world = mc_core::world::World::open(&root)?;
    println!("world: {}", world.root.display());
    for d in &world.dimensions {
        println!("  dimension {:?} at {}", d.name, d.region_dir.display());
    }

    for dim in &world.dimensions {
        if let Some(want) = &want {
            if &dim.name != want {
                continue;
            }
        }
        let started = Instant::now();
        let results = world.scan(dim, |region, _coords| {
            let mut summary = RegionSummary::default();
            for cz in 0..32u8 {
                for cx in 0..32u8 {
                    if !region.has_chunk(cx, cz).unwrap_or(false) {
                        continue;
                    }
                    let Ok(bytes) = region.chunk_data(cx, cz) else {
                        continue;
                    };
                    let Some(chunk) = parse_chunk(&bytes) else {
                        continue;
                    };
                    summary.chunks += 1;
                    *summary.versions.entry(chunk.data_version).or_insert(0) += 1;
                    summary.sample.get_or_insert_with(|| {
                        let top = chunk
                            .sections
                            .iter()
                            .max_by_key(|s| s.y)
                            .and_then(|s| s.names.first().map(|n| n.to_string()));
                        (chunk.x_pos, chunk.z_pos, top.unwrap_or_default())
                    });
                    // Status is not part of the fast parse; derive a rough
                    // one from the presence of a heightmap.
                    *summary
                        .statuses
                        .entry(if chunk.motion_blocking.is_some() {
                            "has-heightmaps".to_string()
                        } else {
                            "no-heightmaps".to_string()
                        })
                        .or_insert(0) += 1;
                }
            }
            Some(summary)
        })?;

        let mut regions = 0u32;
        let mut chunks = 0u64;
        let mut versions = std::collections::BTreeMap::new();
        let mut statuses = std::collections::BTreeMap::new();
        let mut coords = Vec::new();
        let mut sample = None;
        for (rx, rz, summary) in results
            .into_iter()
            .filter_map(|(rx, rz, s)| s.map(|s| (rx, rz, s)))
        {
            let summary = summary;
            regions += 1;
            chunks += summary.chunks;
            for (v, n) in summary.versions {
                *versions.entry(v).or_insert(0) += n;
            }
            for (s, n) in summary.statuses {
                *statuses.entry(s).or_insert(0) += n;
            }
            coords.push((rx, rz));
            sample = sample.or(summary.sample);
        }

        let bounds = RegionBounds::from_points(coords.iter().copied());
        println!("\ndimension {:?}:", dim.name);
        println!("  regions: {regions}");
        println!("  chunks:  {chunks}");
        println!(
            "  chunk bounds: {:?}",
            bounds.map(|b| (
                b.min_x * 32,
                b.min_z * 32,
                b.max_x * 32 + 31,
                b.max_z * 32 + 31
            ))
        );
        println!("  scan took: {:?}", started.elapsed());
        let top_versions: Vec<_> = versions.iter().rev().take(5).collect();
        println!("  data versions (newest 5): {top_versions:?}");
        println!("  statuses: {statuses:?}");
        if let Some((x, z, state)) = sample {
            println!("  first chunk sample: ({x},{z}) top-state {state:?}");
        }
    }
    Ok(())
}
