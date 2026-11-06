//! Scan a world and print per-dimension stats — the manual test for
//! region/chunk parsing. Usage: `cargo run --example scan -- <world-dir> [dimension]`.

use std::time::Instant;

use mc_core::chunk::Chunk;
use mc_core::world::RegionBounds;

/// Per-region summary, produced in parallel and folded at the end.
#[derive(Default)]
struct RegionSummary {
    chunks: u64,
    errors: u64,
    first_error: Option<String>,
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
        let results = world.scan(dim, |region, (rx, rz)| {
            let mut summary = RegionSummary::default();
            for cz in 0..32u8 {
                for cx in 0..32u8 {
                    if !region.has_chunk(cx, cz).unwrap_or(false) {
                        continue;
                    }
                    let Ok(bytes) = region.chunk_data(cx, cz) else {
                        continue;
                    };
                    let Ok(chunk) = Chunk::from_nbt(&bytes) else {
                        summary.errors += 1;
                        if summary.first_error.is_none() {
                            summary.first_error =
                                Some(format!("({cx},{cz}): {}", Chunk::debug_nbt_error(&bytes)));
                        }
                        continue;
                    };
                    summary.chunks += 1;
                    *summary.versions.entry(chunk.data_version).or_insert(0) += 1;
                    *summary.statuses.entry(chunk.status.clone()).or_insert(0) += 1;
                    if summary.sample.is_none() {
                        let top = chunk.top_section().map(|s| {
                            s.block_states
                                .as_ref()
                                .map(|b| b.palette[0].state_string())
                                .unwrap_or_default()
                        });
                        summary.sample = Some((chunk.x_pos, chunk.z_pos, top.unwrap_or_default()));
                    }                }
            }
            Some(summary)
        })?;

        let mut regions = 0u32;
        let mut chunks = 0u64;
        let mut errors = 0u64;
        let mut first_error = None;
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
            errors += summary.errors;
            first_error = first_error.or(summary.first_error);
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
        println!("  chunk errors: {errors}");
        if let Some(e) = first_error {
            println!("  first error: {e}");
        }
        let top_versions: Vec<_> = versions.iter().rev().take(5).collect();
        println!("  data versions (newest 5): {top_versions:?}");
        println!("  statuses: {statuses:?}");
        if let Some((x, z, state)) = sample {
            println!("  first chunk sample: ({x},{z}) top-state {state:?}");
        }
    }
    Ok(())
}
