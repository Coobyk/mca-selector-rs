//! Locating the dimensions of a save folder and enumerating its region
//! files.
//!
//! Two layouts are understood:
//!
//! - the classic layout: `region/` for the overworld, `DIM-1/region/` for the
//!   nether and `DIM1/region/` for the end (used by vanilla and by
//!   mca-selector);
//! - the datapack layout: `dimensions/minecraft/overworld/region/` and so
//!   on (used by some world tools; the Chunkyied test world uses it).

use std::path::{Path, PathBuf};

use rayon::prelude::*;

use crate::error::{Error, Result};
use crate::region::parse_region_name;

/// A known dimension of a world.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dimension {
    /// Display name, e.g. `Overworld`, `Nether`, `End`, or a namespace path
    /// for custom dimensions.
    pub name: String,
    /// Directory containing `region/*.mca`.
    pub region_dir: PathBuf,
}

impl Dimension {
    /// All region files of this dimension, parsed as
    /// `(x, z, path)` and sorted by coordinates.
    pub fn region_files(&self) -> Result<Vec<(i32, i32, PathBuf)>> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(&self.region_dir).map_err(|source| Error::Io {
            path: self.region_dir.clone().into(),
            source,
        })? {
            let entry = entry.map_err(|source| Error::Io {
                path: self.region_dir.clone().into(),
                source,
            })?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("mca") {
                if let Some((x, z)) = parse_region_name(&path) {
                    out.push((x, z, path));
                }
            }
        }
        out.sort_by_key(|(x, z, _)| (*x, *z));
        Ok(out)
    }
}

/// Opened world: the save folder plus the dimensions that were found.
#[derive(Debug, Clone)]
pub struct World {
    pub root: PathBuf,
    pub dimensions: Vec<Dimension>,
}

impl World {
    /// Inspect a save folder and discover its dimensions. Returns an error
    /// when no region directory is found anywhere.
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref();
        if !root.is_dir() {
            return Err(Error::Io {
                path: root.into(),
                source: std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "world folder not found",
                ),
            });
        }

        let mut dims: Vec<Dimension> = Vec::new();

        // Classic layout.
        let classic: &[(&str, &str)] = &[
            ("", "Overworld"),
            ("DIM-1", "Nether"),
            ("DIM1", "End"),
        ];
        for (dir, name) in classic {
            let region_dir = if dir.is_empty() {
                root.join("region")
            } else {
                root.join(dir).join("region")
            };
            if region_dir.is_dir() {
                dims.push(Dimension {
                    name: name.to_string(),
                    region_dir,
                });
            }
        }

        // Datapack layout: dimensions/<namespace>/<path>/region.
        let dimensions_root = root.join("dimensions");
        if dimensions_root.is_dir() {
            let mut stack = vec![dimensions_root.clone()];
            while let Some(dir) = stack.pop() {
                let Ok(entries) = std::fs::read_dir(&dir) else {
                    continue;
                };
                for entry in entries.flatten() {
                    let path = entry.path();
                    if !path.is_dir() {
                        continue;
                    }
                    if path.join("region").is_dir() {
                        let name = path
                            .strip_prefix(&dimensions_root)
                            .unwrap_or(&path)
                            .components()
                            .map(|c| c.as_os_str().to_string_lossy())
                            .collect::<Vec<_>>()
                            .join("/");
                        let name = match name.as_str() {
                            "minecraft/overworld" => "Overworld".to_string(),
                            "minecraft/the_nether" => "Nether".to_string(),
                            "minecraft/the_end" => "End".to_string(),
                            other => other.to_string(),
                        };
                        dims.push(Dimension {
                            name,
                            region_dir: path.join("region"),
                        });
                    } else {
                        stack.push(path);
                    }
                }
            }
        }

        // Deduplicate by region dir, keep first naming.
        let mut seen = std::collections::HashSet::new();
        dims.retain(|d| seen.insert(d.region_dir.clone()));
        dims.sort_by(|a, b| a.name.cmp(&b.name));

        if dims.is_empty() {
            return Err(Error::BadRegionName(format!(
                "no region folders found under {}",
                root.display()
            )));
        }

        Ok(Self {
            root: root.to_path_buf(),
            dimensions: dims,
        })
    }

    pub fn dimension(&self, name: &str) -> Option<&Dimension> {
        self.dimensions.iter().find(|d| d.name == name)
    }

    /// All region files of a dimension, parsed as
    /// `(x, z, path)` and sorted by coordinates.
    pub fn region_files(&self, dimension: &Dimension) -> Result<Vec<(i32, i32, PathBuf)>> {
        dimension.region_files()
    }

    /// Scan every region file of a dimension in parallel, mapping `f` over
    /// each region. `f` receives the opened region and its region
    /// coordinates and returns the per-region result (or `None` when the
    /// region should be skipped). Regions that fail to open come back as
    /// `None` results too, so a corrupt file never breaks the whole scan.
    pub fn scan<T, F>(&self, dimension: &Dimension, f: F) -> Result<Vec<(i32, i32, Option<T>)>>
    where
        T: Send,
        F: Fn(&crate::region::Region, (i32, i32)) -> Option<T> + Send + Sync,
    {
        let files = self.region_files(dimension)?;
        let results = files
            .into_par_iter()
            .map(|(rx, rz, path)| {
                let value = crate::region::Region::open(&path)
                    .ok()
                    .and_then(|region| f(&region, (rx, rz)));
                (rx, rz, value)
            })
            .collect();
        Ok(results)
    }
}

/// Bounds over region coordinates.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RegionBounds {
    pub min_x: i32,
    pub min_z: i32,
    pub max_x: i32,
    pub max_z: i32,
}

impl RegionBounds {
    pub fn from_points(points: impl IntoIterator<Item = (i32, i32)>) -> Option<Self> {
        let mut it = points.into_iter();
        let (x, z) = it.next()?;
        let mut b = RegionBounds {
            min_x: x,
            min_z: z,
            max_x: x,
            max_z: z,
        };
        for (x, z) in it {
            b.min_x = b.min_x.min(x);
            b.min_z = b.min_z.min(z);
            b.max_x = b.max_x.max(x);
            b.max_z = b.max_z.max(z);
        }
        Some(b)
    }

    pub fn width(&self) -> i32 {
        self.max_x - self.min_x + 1
    }

    pub fn height(&self) -> i32 {
        self.max_z - self.min_z + 1
    }
}
