//! Exporting a chunk selection into a new world folder.
//!
//! The output is a standalone world skeleton: `region/r.X.Z.mca` files
//! containing exactly the selected chunks (copied verbatim with their
//! timestamps) and, when the source world has one, a copy of `level.dat`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::region::Region;
use crate::region_writer::RegionWriter;
use crate::world::{Dimension, World};

/// Stats of one export run.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ExportStats {
    pub chunks: u64,
    pub regions: u32,
}

/// Export `selection` (world chunk coordinates) of `dimension` into
/// `out_root`, creating `<out_root>/region/`. Chunks are copied with their
/// original data and timestamps. Returns how much was written.
pub fn export_selection(
    world: &World,
    dimension: &Dimension,
    selection: &std::collections::BTreeSet<(i32, i32)>,
    out_root: &Path,
) -> Result<ExportStats> {
    // Group the selection by region.
    let mut by_region: BTreeMap<(i32, i32), Vec<(u8, u8, i32, i32)>> = BTreeMap::new();
    for &(cx, cz) in selection {
        let (rx, rz) = (cx.div_euclid(32), cz.div_euclid(32));
        let lx = cx.rem_euclid(32) as u8;
        let lz = cz.rem_euclid(32) as u8;
        by_region.entry((rx, rz)).or_default().push((lx, lz, cx, cz));
    }

    let mut stats = ExportStats {
        regions: by_region.len() as u32,
        ..Default::default()
    };

    // Cache open source regions.
    let mut open_regions: std::collections::HashMap<(i32, i32), Option<Region>> =
        std::collections::HashMap::new();

    for ((rx, rz), chunks) in by_region {
        let source = open_regions
            .entry((rx, rz))
            .or_insert_with(|| {
                world
                    .region_files(dimension)
                    .ok()
                    .and_then(|files| {
                        files
                            .into_iter()
                            .find(|(x, z, _)| (*x, *z) == (rx, rz))
                            .map(|(_, _, path)| path)
                    })
                    .and_then(|path| Region::open(path).ok())
            });
        let Some(region) = source else {
            continue;
        };
        let out_path = out_root.join("region").join(format!("r.{rx}.{rz}.mca"));
        let mut writer = RegionWriter::create(&out_path)?;
        for (lx, lz, _, _) in chunks {
            let nbt = region.chunk_data(lx, lz)?;
            let timestamp = region.chunk_timestamp(lx, lz)?;
            writer.write_chunk(lx, lz, &nbt, timestamp)?;
            stats.chunks += 1;
        }
        writer.finish()?;
    }

    // Copy level.dat when present so the output opens as a world.
    for name in ["level.dat", "icon.png"] {
        let src = world.root.join(name);
        if src.is_file() {
            let _ = std::fs::copy(&src, out_root.join(name));
        }
    }

    Ok(stats)
}

/// Convenience: the default export location next to nothing in particular —
/// used by the UI when the user did not pick one.
pub fn default_export_dir() -> PathBuf {
    std::env::temp_dir().join("mca-selector-export")
}
