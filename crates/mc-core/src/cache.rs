//! Disk caches: PNG pyramid (world_cache/<zoom>/r.x.z.png) plus a fs-based
//! meta store replacing upstream's LevelDB (file times, chunk sets,
//! structures, far-zoom images).

use crate::point::Point2i;
use crate::selection::ChunkSet;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// meta db dir: base_cache_dir/cache  (same as upstream's cacheDBDir)
fn db_dir() -> PathBuf {
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
    std::fs::read(meta_path(world_uuid, "rset", r))
        .ok()
        .and_then(|b| b.try_into().ok().map(i64::from_le_bytes))
        .unwrap_or(0)
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

// -- structures (chunk-asLong -> structure names, plus bounding boxes) ----------

/// per-region structure data: `chunks` maps chunk-asLong -> start names for
/// the icon pass; `boxes` carries each start's BB for the box overlay and
/// the find-structure locator. Old caches (bare chunk map) fail to parse —
/// treated as a miss and re-derived.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct StructuresData {
    pub chunks: HashMap<i64, Vec<String>>,
    /// (structure name, [minX, minY, minZ, maxX, maxY, maxZ] block coords)
    pub boxes: Vec<(String, [i32; 6])>,
}

pub fn get_structures(world_uuid: &str, r: Point2i) -> Option<StructuresData> {
    let text = std::fs::read_to_string(meta_path(world_uuid, "structures", r)).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn set_structures(world_uuid: &str, r: Point2i, s: &StructuresData) {
    if let Ok(j) = serde_json::to_string(s) {
        let _ = write(&meta_path(world_uuid, "structures", r), j.as_bytes());
    }
}

// -- z>=32 lod images ------------------------------------------------------------

/// pixels are always LOD_IMAGE_SIZE x LOD_IMAGE_SIZE.
/// RGBA bytes -> u32 ARGB pixels (the format both the lod db and gpui
/// images use).
pub fn rgba_to_argb(b: &[u8]) -> Vec<u32> {
    b.chunks_exact(4)
        .map(|c| ((c[3] as u32) << 24) | ((c[0] as u32) << 16) | ((c[1] as u32) << 8) | c[2] as u32)
        .collect()
}

pub fn get_lod_image(world_uuid: &str, r: Point2i) -> Option<Vec<u32>> {
    let b = std::fs::read(lod_image_path(world_uuid, r)).ok()?;
    if b.len() != LOD_IMAGE_SIZE * LOD_IMAGE_SIZE * 4 {
        return None;
    }
    Some(rgba_to_argb(&b))
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
    for kind in ["time", "rset", "chunks", "structures"] {
        let _ = std::fs::remove_file(meta_path(world_uuid, kind, r));
    }
    let _ = std::fs::remove_file(lod_image_path(world_uuid, r));
}

/// delete a world's whole cache entry
pub fn delete_all(world_uuid: &str) {
    let _ = std::fs::remove_dir_all(db_dir().join(world_uuid));
}
