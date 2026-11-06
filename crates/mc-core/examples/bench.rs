//! Per-stage benchmark for the map pipeline over one region file, plus a
//! dump of the first chunk that fails to fast-parse.
//! Usage: `cargo run --release --example bench -- <region.mca>`

use std::time::Instant;

use mc_core::chunk_fast::parse_chunk;
use mc_core::map::{render_tile_raw, shade_tile};
use mc_core::region::Region;

fn main() -> anyhow::Result<()> {
    let path = std::env::args().nth(1).expect("usage: bench <region.mca>");
    let region = Region::open(&path)?;

    let mut slots = Vec::new();
    for cz in 0..32u8 {
        for cx in 0..32u8 {
            if region.has_chunk(cx, cz).unwrap_or(false) {
                slots.push((cx, cz));
            }
        }
    }
    println!("{} present chunks", slots.len());

    // Decompress only.
    let started = Instant::now();
    let payloads: Vec<Vec<u8>> = slots
        .iter()
        .map(|(cx, cz)| region.chunk_data(*cx, *cz).unwrap_or_default())
        .collect();
    let decompress = started.elapsed();
    let total: usize = payloads.iter().map(|p| p.len()).sum();
    println!("decompress: {:?} ({} KiB total)", decompress, total / 1024);

    // Parse only.
    let started = Instant::now();
    let parsed: Vec<Option<_>> = payloads.iter().map(|p| parse_chunk(p)).collect();
    let parse = started.elapsed();
    let failures = parsed.iter().filter(|p| p.is_none()).count();
    println!("parse: {:?} ({} failures)", parse, failures);

    // Render only.
    let started = Instant::now();
    let mut rendered = 0usize;
    for c in parsed.iter().flatten() {
        if let Some(raw) = render_tile_raw(c) {
            let _ = shade_tile(&raw, &|_, _| None);
            rendered += 1;
        }
    }
    println!("render: {:?} ({} tiles)", started.elapsed(), rendered);

    // Report why chunks fail to parse and dump their payloads.
    for (n, (i, p)) in parsed
        .iter()
        .enumerate()
        .filter(|(_, p)| p.is_none())
        .enumerate()
    {
        let reason = mc_core::chunk_fast::parse_chunk_traced(&payloads[i])
            .err()
            .unwrap_or_default();
        println!(
            "chunk ({}, {}) fails: {} (dumped /tmp/bad{n}.nbt)",
            slots[i].0,
            slots[i].1,
            reason
        );
        std::fs::write(format!("/tmp/bad{n}.nbt"), &payloads[i]).ok();
    }
    Ok(())
}
