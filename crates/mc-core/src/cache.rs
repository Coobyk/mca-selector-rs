//! Disk caches: PNG pyramid (world_cache/<zoom>/r.x.z.png) plus a fs-based
//! meta store replacing upstream's LevelDB (file times, chunk sets,
//! structures, far-zoom images).

use crate::point::Point2i;
use crate::selection::ChunkSet;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// meta db dir: base_cache_dir/cache  (same as upstream's cacheDBDir)
pub fn db_dir() -> PathBuf {
    crate::files::base_cache_dir().join("cache")
}

fn meta_path(world_uuid: &str, kind: &str, r: Point2i) -> PathBuf {
    db_dir()
        .join(world_uuid)
        .join(kind)
        .join(format!("r.{}.{}.bin", r.x, r.z))
}

/// image for zoom >= 32 stored in the db dir.
fn lod_image_path(world_uuid: &str, r: Point2i) -> PathBuf {
    db_dir()
        .join(world_uuid)
        .join("img32")
        .join(crate::files::png_file_name(r))
}

fn write(path: &Path, data: &[u8]) -> std::io::Result<()> {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    std::fs::write(path, data)
}

// -- file times ----------------------------------------------------------------

pub fn get_file_time(world_uuid: &str, r: Point2i) -> i64 {
    let p = meta_path(world_uuid, "time", r);
    std::fs::read(&p)
        .ok()
        .and_then(|b| b.try_into().ok().map(i64::from_le_bytes))
        .unwrap_or(-1)
}

pub fn set_file_time(world_uuid: &str, r: Point2i, t: i64) {
    let _ = write(&meta_path(world_uuid, "time", r), &t.to_le_bytes());
}

/// render settings hash recorded when the image was generated; a cached
/// image is stale when the world's current render settings differ.
pub fn get_render_hash(world_uuid: &str, r: Point2i) -> i64 {
    let p = meta_path(world_uuid, "rset", r);
    let b = std::fs::read(p).unwrap_or_default();
    let a: [u8; 8] = b.get(..8).unwrap_or(&[0; 8]).try_into().unwrap_or([0; 8]);
    i64::from_le_bytes(a)
}

pub fn set_render_hash(world_uuid: &str, r: Point2i, h: i64) {
    let _ = write(&meta_path(world_uuid, "rset", r), &h.to_le_bytes());
}

// -- chunk sets (header-only LOD data) ------------------------------------------

pub fn get_chunks(world_uuid: &str, r: Point2i) -> Option<ChunkSet> {
    let b = std::fs::read(meta_path(world_uuid, "chunks", r)).ok()?;
    if b.len() == 128 {
        Some(ChunkSet::from_bytes(&b))
    } else {
        None
    }
}

pub fn set_chunks(world_uuid: &str, r: Point2i, cs: &ChunkSet) {
    let _ = write(&meta_path(world_uuid, "chunks", r), &cs.to_bytes());
}

// -- structures (chunk-asLong -> structure names) --------------------------------

pub fn get_structures(world_uuid: &str, r: Point2i) -> Option<HashMap<i64, Vec<String>>> {
    let text = std::fs::read_to_string(meta_path(world_uuid, "structures", r)).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn set_structures(world_uuid: &str, r: Point2i, s: &HashMap<i64, Vec<String>>) {
    if let Ok(j) = serde_json::to_string(s) {
        let _ = write(&meta_path(world_uuid, "structures", r), j.as_bytes());
    }
}

// -- z>=32 lod images ------------------------------------------------------------

pub fn get_lod_image(world_uuid: &str, r: Point2i) -> Option<(Vec<u32>, usize)> {
    let b = std::fs::read(lod_image_path(world_uuid, r)).ok()?;
    if b.len() != 16 * 16 * 4 {
        return None;
    }
    let px: Vec<u32> = b
        .chunks_exact(4)
        .map(|c| ((c[3] as u32) << 24) | ((c[0] as u32) << 16) | ((c[1] as u32) << 8) | c[2] as u32)
        .collect();
    Some((px, LOD_IMAGE_SIZE))
}

/// fixed-resolution LOD image: one pixel per 2x2 chunks (upstream lodLevel).
pub const LOD_IMAGE_SIZE: usize = 16;

pub fn set_lod_image(world_uuid: &str, r: Point2i, argb: &[u32]) {
    let mut b = Vec::with_capacity(LOD_IMAGE_SIZE * LOD_IMAGE_SIZE * 4);
    for &p in argb.iter().take(LOD_IMAGE_SIZE * LOD_IMAGE_SIZE) {
        b.push((p >> 16) as u8);
        b.push((p >> 8) as u8);
        b.push(p as u8);
        b.push((p >> 24) as u8);
    }
    let _ = write(&lod_image_path(world_uuid, r), &b);
}

pub fn delete_data(world_uuid: &str, r: Point2i) {
    for kind in ["time", "chunks", "structures"] {
        let _ = std::fs::remove_file(meta_path(world_uuid, kind, r));
    }
    let _ = std::fs::remove_file(lod_image_path(world_uuid, r));
}

/// delete a world's whole cache entry
pub fn delete_all(world_uuid: &str) {
    let _ = std::fs::remove_dir_all(db_dir().join(world_uuid));
}
