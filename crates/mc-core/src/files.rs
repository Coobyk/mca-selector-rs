//! Port of io.FileHelper + config directory resolution (Config.BASE_*).

use crate::mca::WorldDirectories;
use crate::point::Point2i;
use std::path::{Path, PathBuf};

pub fn parse_mca_file_name(name: &str) -> Option<Point2i> {
    let mut it = name.split('.');
    if it.next()? != "r" {
        return None;
    }
    let x: i32 = it.next()?.parse().ok()?;
    let z: i32 = it.next()?.parse().ok()?;
    if it.next()? != "mca" || it.next().is_some() {
        return None;
    }
    Some(Point2i::new(x, z))
}

pub fn mca_file_name(r: Point2i) -> String {
    format!("r.{}.{}.mca", r.x, r.z)
}
pub fn mcc_file_name(c: Point2i) -> String {
    format!("c.{}.{}.mcc", c.x, c.z)
}
pub fn png_file_name(r: Point2i) -> String {
    format!("r.{}.{}.png", r.x, r.z)
}

/// Java UUID.nameUUIDFromBytes (MD5, version 3, IETF variant).
pub fn name_uuid(bytes: &[u8]) -> String {
    use md5::{Digest, Md5};
    let mut d = Md5::digest(bytes).to_vec();
    d[6] = (d[6] & 0x0F) | 0x30;
    d[8] = (d[8] & 0x3F) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        d[0], d[1], d[2], d[3], d[4], d[5], d[6], d[7], d[8], d[9], d[10], d[11], d[12], d[13], d[14], d[15]
    )
}

pub fn world_uuid(region_dir: &Path) -> String {
    name_uuid(region_dir.to_string_lossy().as_bytes()).replace('-', "")
}

fn env_or(env: &str, def: &str) -> PathBuf {
    std::env::var(env).map(PathBuf::from).unwrap_or_else(|_| {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        PathBuf::from(def.replace('~', &home))
    })
}

/// BASE_CACHE_DIR (linux branch: XDG_CACHE_HOME/mcaselector or ~/.cache/mcaselector)
pub fn base_cache_dir() -> PathBuf {
    let dir = std::env::var("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| env_or("HOME", "~").join(".cache"));
    let p = dir.join("mcaselector");
    let _ = std::fs::create_dir_all(&p);
    p
}

/// BASE_CONFIG_FILE dir (XDG_CONFIG_HOME/mcaselector or ~/.mcaselector/mcaselector)
pub fn base_config_dir() -> PathBuf {
    let dir = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| env_or("HOME", "~").join(".mcaselector"));
    let p = dir.join("mcaselector");
    let _ = std::fs::create_dir_all(&p);
    p
}

pub fn base_log_dir() -> PathBuf {
    let dir = std::env::var("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| env_or("HOME", "~").join(".local/share"));
    let p = dir.join("mcaselector/log");
    let _ = std::fs::create_dir_all(&p);
    p
}

/// world cache dir: base_cache_dir/<world-uuid>
pub fn world_cache_dir(region_dir: &Path) -> PathBuf {
    base_cache_dir().join(world_uuid(region_dir))
}

/// per-zoom-level png cache: world_cache/<zoom>/r.x.z.png
pub fn png_cache_path(world_cache: &Path, zoom: i32, r: Point2i) -> PathBuf {
    world_cache.join(zoom.to_string()).join(png_file_name(r))
}

pub fn has_mca_files(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten().any(|e| {
                parse_mca_file_name(e.file_name().to_str().unwrap_or("")).is_some()
            })
        })
        .unwrap_or(false)
}

pub fn is_valid_dimension(dir: &Path) -> bool {
    dir.join("region").is_dir() && has_mca_files(&dir.join("region"))
}

/// detectWorldDirectories: region dir must contain mca files.
pub fn detect_world_directories(dir: &Path) -> WorldDirectories {
    let mut w = WorldDirectories::default();
    let region = dir.join("region");
    if region.is_dir() && has_mca_files(&region) {
        w.region = Some(region);
    }
    let poi = dir.join("poi");
    if poi.is_dir() {
        w.poi = Some(poi);
    }
    let entities = dir.join("entities");
    if entities.is_dir() {
        w.entities = Some(entities);
    }
    w
}

/// detectDimensionDirectories — dimension candidates under a world root.
pub fn detect_dimension_directories(dir: &Path) -> Vec<PathBuf> {
    let mut result = Vec::new();
    for sub in [
        "dimensions/minecraft/overworld",
        "dimensions/minecraft/the_nether",
        "dimensions/minecraft/the_end",
    ] {
        let p = dir.join(sub);
        if is_valid_dimension(&p) {
            result.push(p);
        }
    }
    // datapack dimensions (limited depth scan like upstream)
    let dims = dir.join("dimensions");
    if dims.is_dir() {
        detect_datapack_dimensions(&dims, 0, &mut result);
    }
    if is_valid_dimension(dir) {
        result.push(dir.to_path_buf());
    }
    for sub in ["DIM-1", "DIM1"] {
        let p = dir.join(sub);
        if is_valid_dimension(&p) {
            result.push(p);
        }
    }
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if name == "DIM-1" || name == "DIM1" {
                continue;
            }
            if is_dim_folder(&name) && e.path().is_dir() && is_valid_dimension(&e.path()) {
                result.push(e.path());
            }
        }
    }
    result
}

fn is_dim_folder(name: &str) -> bool {
    // ^DIM-?\d+$
    if let Some(rest) = name.strip_prefix("DIM") {
        let rest = rest.strip_prefix('-').unwrap_or(rest);
        return !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit());
    }
    false
}

fn detect_datapack_dimensions(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth >= 4 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if !p.is_dir() {
            continue;
        }
        let name = e.file_name().to_string_lossy().to_string();
        if depth == 1 && matches!(name.as_str(), "overworld" | "the_nether" | "the_end") {
            continue;
        }
        if is_valid_dimension(&p) {
            out.push(p);
        } else {
            detect_datapack_dimensions(&p, depth + 1, out);
        }
    }
}

/// scan region dir for r.*.mca files > header size -> region coords set.
pub fn scan_region_files(region_dir: &Path) -> std::collections::HashSet<i64> {
    let mut set = std::collections::HashSet::new();
    if let Ok(rd) = std::fs::read_dir(region_dir) {
        for e in rd.flatten() {
            let ok = e
                .metadata()
                .map(|m| m.len() > 8192)
                .unwrap_or(false);
            if !ok {
                continue;
            }
            if let Some(p) = parse_mca_file_name(e.file_name().to_str().unwrap_or("")) {
                set.insert(p.as_long());
            }
        }
    }
    set
}
