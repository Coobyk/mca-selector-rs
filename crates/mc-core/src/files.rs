//! Port of io.FileHelper + config directory resolution (Config.BASE_*).

#[cfg(not(target_arch = "wasm32"))]
use crate::mca::WorldDirectories;
#[cfg(not(target_arch = "wasm32"))]
use crate::nbt::Tag;
use crate::point::Point2i;
#[cfg(not(target_arch = "wasm32"))]
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

pub fn png_file_name(r: Point2i) -> String {
    format!("r.{}.{}.png", r.x, r.z)
}

/// Java UUID.nameUUIDFromBytes (MD5, version 3, IETF variant).
#[cfg(not(target_arch = "wasm32"))]
fn name_uuid(bytes: &[u8]) -> String {
    use md5::{Digest, Md5};
    let mut d = Md5::digest(bytes).to_vec();
    d[6] = (d[6] & 0x0F) | 0x30;
    d[8] = (d[8] & 0x3F) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        d[0], d[1], d[2], d[3], d[4], d[5], d[6], d[7], d[8], d[9], d[10], d[11], d[12], d[13], d[14], d[15]
    )
}

#[cfg(not(target_arch = "wasm32"))]
pub fn world_uuid(region_dir: &Path) -> String {
    name_uuid(region_dir.to_string_lossy().as_bytes()).replace('-', "")
}

#[cfg(not(target_arch = "wasm32"))]
fn home_dir() -> PathBuf {
    std::env::var("HOME").map(PathBuf::from).unwrap_or_else(|_| ".".into())
}

/// BASE_CACHE_DIR (linux branch: XDG_CACHE_HOME/mcaselector or ~/.cache/mcaselector)
/// Memoized: cache helpers call this once per file op; avoids a create_dir_all
/// syscall each time. Env vars are process-stable, so caching is safe.
#[cfg(not(target_arch = "wasm32"))]
pub fn base_cache_dir() -> PathBuf {
    static P: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    P.get_or_init(|| {
        let dir = std::env::var("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| home_dir().join(".cache"));
        let p = dir.join("mcaselector");
        let _ = std::fs::create_dir_all(&p);
        p
    })
    .clone()
}

/// BASE_CONFIG_FILE dir (XDG_CONFIG_HOME/mcaselector or ~/.mcaselector/mcaselector)
#[cfg(not(target_arch = "wasm32"))]
pub fn base_config_dir() -> PathBuf {
    static P: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    P.get_or_init(|| {
        let dir = std::env::var("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| home_dir().join(".mcaselector"));
        let p = dir.join("mcaselector");
        let _ = std::fs::create_dir_all(&p);
        p
    })
    .clone()
}



/// world cache dir: base_cache_dir/<world-uuid>
#[cfg(not(target_arch = "wasm32"))]
pub fn world_cache_dir(region_dir: &Path) -> PathBuf {
    base_cache_dir().join(world_uuid(region_dir))
}

/// per-zoom-level png cache: world_cache/<zoom>/r.x.z.png
#[cfg(not(target_arch = "wasm32"))]
pub fn png_cache_path(world_cache: &Path, zoom: i32, r: Point2i) -> PathBuf {
    // one join+format (2 allocs) instead of zoom.to_string()+two joins (~4);
    // called ~8x per tile request while probing the png pyramid.
    world_cache.join(format!("{}/{}", zoom, png_file_name(r)))
}

#[cfg(not(target_arch = "wasm32"))]
fn has_mca_files(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten().any(|e| {
                parse_mca_file_name(e.file_name().to_str().unwrap_or("")).is_some()
            })
        })
        .unwrap_or(false)
}

#[cfg(not(target_arch = "wasm32"))]
pub fn is_valid_dimension(dir: &Path) -> bool {
    dir.join("region").is_dir() && has_mca_files(&dir.join("region"))
}

/// detectWorldDirectories: region dir must contain mca files.
#[cfg(not(target_arch = "wasm32"))]
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
#[cfg(not(target_arch = "wasm32"))]
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
    for sub in LEGACY_DIMS {
        let p = dir.join(sub);
        if is_valid_dimension(&p) {
            result.push(p);
        }
    }
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if LEGACY_DIMS.contains(&name.as_str()) {
                continue;
            }
            if is_dim_folder(&name) && e.path().is_dir() && is_valid_dimension(&e.path()) {
                result.push(e.path());
            }
        }
    }
    result
}

/// pre-1.13 saves keep the nether/end dims directly under the world root
#[cfg(not(target_arch = "wasm32"))]
const LEGACY_DIMS: [&str; 2] = ["DIM-1", "DIM1"];

#[cfg(not(target_arch = "wasm32"))]
fn is_dim_folder(name: &str) -> bool {
    // ^DIM-?\d+$
    if let Some(rest) = name.strip_prefix("DIM") {
        let rest = rest.strip_prefix('-').unwrap_or(rest);
        return !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit());
    }
    false
}

#[cfg(not(target_arch = "wasm32"))]
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
        // vanilla dims under dimensions/minecraft/ are covered by the
        // explicit candidates above — skip them only in that namespace
        if depth == 1
            && dir.file_name().map(|n| n == "minecraft").unwrap_or(false)
            && matches!(name.as_str(), "overworld" | "the_nether" | "the_end")
        {
            continue;
        }
        if is_valid_dimension(&p) {
            out.push(p);
        } else {
            detect_datapack_dimensions(&p, depth + 1, out);
        }
    }
}

/// World seed from the nearest level.dat (walk up from the region dir —
/// dim dirs sit at world/dimensions/ns/name, plain saves at world/).
/// Modern layout: Data.WorldGenSettings.seed; pre-1.16: Data.RandomSeed.
#[cfg(not(target_arch = "wasm32"))]
pub fn world_seed(region_dir: &Path) -> Option<i64> {
    for dir in region_dir.ancestors().take(6) {
        let p = dir.join("level.dat");
        if !p.is_file() {
            continue;
        }
        let Ok(bytes) = std::fs::read(&p) else { continue };
        let Some(root) = crate::mca::read_compressed(&bytes) else { continue };
        let data = root.get("Data").and_then(Tag::as_compound);
        let seed = data
            .and_then(|d| d.get("WorldGenSettings"))
            .and_then(Tag::as_compound)
            .and_then(|w| w.get("seed"))
            .or_else(|| data.and_then(|d| d.get("RandomSeed")))
            .and_then(Tag::as_i64);
        if seed.is_some() {
            return seed;
        }
    }
    None
}

/// backups root for a file inside a dimension dir:
/// `<dimension-root>/mca-selector-backups`.
#[cfg(not(target_arch = "wasm32"))]
pub fn backup_root_for(path: &Path) -> Option<PathBuf> {
    path.parent()?.parent().map(|d| d.join("mca-selector-backups"))
}

/// per-operation stamp — one backup set per batch job.
#[cfg(not(target_arch = "wasm32"))]
pub fn backup_stamp() -> String {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let ms = millis % 1000;
    let secs = millis / 1000;
    // civil-from-days (Howard Hinnant), UTC
    let days = (secs / 86400) as i64;
    let rem = secs % 86400;
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mo <= 2 { y + 1 } else { y };
    format!("{y:04}{mo:02}{d:02}-{h:02}{m:02}{s:02}{ms:03}")
}

/// Snapshot `path` before a destructive write: hardlink (free — saves go
/// through temp+rename, so the old inode survives) with a copy fallback.
/// Files that don't exist yet are recorded in `created.txt` so a restore
/// can delete files the operation made. Returns the backup path.
#[cfg(not(target_arch = "wasm32"))]
pub fn backup_file(path: &Path, stamp: &str) -> Option<PathBuf> {
    let dir = backup_root_for(path)?.join(stamp);
    if path.is_file() {
        let kind = path.parent()?.file_name()?;
        let dst = dir.join(kind).join(path.file_name()?);
        std::fs::create_dir_all(dst.parent()?).ok()?;
        if std::fs::hard_link(path, &dst).is_err() {
            std::fs::copy(path, &dst).ok()?;
        }
        Some(dst)
    } else {
        std::fs::create_dir_all(&dir).ok()?;
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("created.txt"))
            .ok()?;
        writeln!(f, "{}", path.display()).ok()?;
        None
    }
}

/// every backup stamp dir under a dimension root, oldest first.
#[cfg(not(target_arch = "wasm32"))]
pub fn list_backups(dimension_root: &Path) -> Vec<PathBuf> {
    let root = dimension_root.join("mca-selector-backups");
    let mut v: Vec<PathBuf> = std::fs::read_dir(&root)
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
                .map(|e| e.path())
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

/// restore one backup set: copy every backed-up file back over its origin
/// and delete files the operation created. Returns (restored, deleted).
#[cfg(not(target_arch = "wasm32"))]
pub fn restore_backup(dir: &Path) -> std::io::Result<(usize, usize)> {
    let Some(dim) = dir.parent().and_then(|p| p.parent()).map(|d| d.to_path_buf()) else {
        return Ok((0, 0));
    };
    let mut restored = 0;
    let mut deleted = 0;
    for kind in std::fs::read_dir(dir)? {
        let kind = kind?;
        if !kind.file_type()?.is_dir() {
            continue;
        }
        for f in std::fs::read_dir(kind.path())? {
            let f = f?;
            if !f.file_type()?.is_file() {
                continue;
            }
            let dst = dim.join(kind.file_name()).join(f.file_name());
            if let Some(p) = dst.parent() {
                std::fs::create_dir_all(p)?;
            }
            std::fs::copy(f.path(), &dst)?;
            restored += 1;
        }
    }
    if let Ok(txt) = std::fs::read_to_string(dir.join("created.txt")) {
        for line in txt.lines() {
            let p = Path::new(line);
            // only delete files under the dimension root — a hand-edited
            // manifest can never reach outside the world.
            if p.starts_with(&dim) && std::fs::remove_file(p).is_ok() {
                deleted += 1;
            }
        }
    }
    Ok((restored, deleted))
}

/// scan region dir for r.*.mca files > header size -> region coords set.
#[cfg(not(target_arch = "wasm32"))]
pub fn scan_region_files(region_dir: &Path) -> std::collections::HashSet<i64> {
    let mut set = std::collections::HashSet::new();
    if let Ok(rd) = std::fs::read_dir(region_dir) {
        for e in rd.flatten() {
            // cheap filename check first — avoids a stat on non-mca entries
            let Some(p) = parse_mca_file_name(e.file_name().to_str().unwrap_or("")) else {
                continue;
            };
            let ok = e
                .metadata()
                .map(|m| m.len() > crate::mca::HEADER_SIZE as u64)
                .unwrap_or(false);
            if ok {
                set.insert(p.as_long());
            }
        }
    }
    set
}
