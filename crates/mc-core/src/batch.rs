//! Shared batch-region job runner: one Job+Progress skeleton for every
//! "iterate regions → load mca set → mutate → save dirty files" operation.

use crate::files::{mca_file_name, scan_region_files};
use crate::jobs::{self, Job, Progress, PRIORITY_LOW};
use crate::mca::{McaFile, WorldDirectories};
use crate::point::Point2i;
use crate::selection::Selection;
use std::path::PathBuf;

/// which of a region's three mca files a body modified.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum McKind {
    Region,
    Poi,
    Entities,
}

/// The mca files backing one region (region/poi/entities); `None` where the
/// file does not exist on disk. `touch` marks a file dirty; the runner saves
/// dirty files after the body returns.
pub struct RegionFiles {
    pub region: Option<McaFile>,
    pub poi: Option<McaFile>,
    pub entities: Option<McaFile>,
    dirty: u8,
    /// backup stamp for this batch — save_dirty snapshots every file it
    /// is about to overwrite into mca-selector-backups/<stamp>/.
    pub backup_stamp: Option<String>,
}

impl RegionFiles {
    pub fn load(region: Point2i, dirs: &WorldDirectories, raw: bool) -> Self {
        fn one(dir: &Option<PathBuf>, name: &str, raw: bool) -> Option<McaFile> {
            let p = dir.as_ref()?.join(name);
            if !p.exists() {
                return None;
            }
            McaFile::new(p).ok().and_then(|mut m| m.load(raw).ok().map(|_| m))
        }
        let name = mca_file_name(region);
        Self {
            region: one(&dirs.region, &name, raw),
            poi: one(&dirs.poi, &name, raw),
            entities: one(&dirs.entities, &name, raw),
            dirty: 0,
            backup_stamp: None,
        }
    }

    pub fn touch(&mut self, kind: McKind) {
        self.dirty |= 1 << kind as u8;
    }

    /// Save every touched file in place; returns how many were written.
    /// Failures are pushed onto `errors` (a false `Ok` from
    /// save_with_temp_file means "nothing to write" — not an error).
    pub fn save_dirty(&mut self, errors: &mut Vec<String>) -> usize {
        let mut written = 0;
        for (i, f) in [
            &mut self.region,
            &mut self.poi,
            &mut self.entities,
        ]
        .iter_mut()
        .enumerate()
        {
            if self.dirty & (1 << i) == 0 {
                continue;
            }
            if let Some(m) = f {
                let path = m.file.clone();
                if let Some(st) = &self.backup_stamp {
                    crate::files::backup_file(&path, st);
                }
                match m.save_with_temp_file(&path) {
                    Ok(true) => written += 1,
                    Ok(false) => {}
                    Err(e) => errors.push(format!("{}: {e}", path.display())),
                }
            }
        }
        self.dirty = 0;
        written
    }
}

/// per-region context handed to the body.
pub struct RegionJobCtx<'a> {
    pub region: Point2i,
    /// the selection being iterated; handles inverted masks internally.
    pub selection: &'a Selection,
}

impl<'a> RegionJobCtx<'a> {
    pub fn chunk_selected(&self, loc: Point2i) -> bool {
        self.selection.is_chunk_selected(loc)
    }
}

pub struct RegionJobSummary {
    pub processed: usize,
    pub errors: Vec<String>,
}

/// Effective (region, entry) pairs for a selection: its own map entries
/// normally, or every region file in `dirs` when inverted.
pub fn effective_regions(sel: &Selection, dirs: &WorldDirectories) -> Vec<Point2i> {
    if sel.inverted {
        dirs.region
            .iter()
            .flat_map(|d| scan_region_files(d))
            .map(Point2i::from_long)
            .collect()
    } else {
        sel.selection
            .iter()
            .map(|(&k, _)| Point2i::from_long(k))
            .collect()
    }
}

/// upstream WorldDirectories.listRegions: every r.x.z file present in any
/// of the three dirs, prefiltered to regions with any selected chunk
/// (`None` selection = all). The `ChunkSet` is the selection's raw map
/// entry for that region (None entry = whole region).
pub fn list_regions(dirs: &WorldDirectories, selection: Option<&Selection>) -> Vec<Point2i> {
    let mut locs = std::collections::BTreeSet::new();
    for dir in [&dirs.region, &dirs.poi, &dirs.entities]
        .into_iter()
        .flatten()
    {
        for k in scan_region_files(dir) {
            if selection
                .map(|s| s.is_any_chunk_in_region_selected(k))
                .unwrap_or(true)
            {
                locs.insert(k);
            }
        }
    }
    locs.into_iter().map(Point2i::from_long).collect()
}

/// One region iteration shared by [`run_region_loop`] and
/// [`spawn_region_job`]: load the `RegionFiles`, skip when nothing loaded,
/// run `body`, save touched files. Returns false when skipped.
fn run_one<F>(
    dirs: &WorldDirectories,
    sel: &Selection,
    region: Point2i,
    raw: bool,
    stamp: Option<&str>,
    sum: &mut RegionJobSummary,
    body: &mut F,
) -> bool
where
    F: FnMut(&mut RegionFiles, &RegionJobCtx, &mut Vec<String>),
{
    let mut files = RegionFiles::load(region, dirs, raw);
    if files.region.is_none() && files.poi.is_none() && files.entities.is_none() {
        return false;
    }
    files.backup_stamp = stamp.map(str::to_string);
    let ctx = RegionJobCtx {
        region,
        selection: sel,
    };
    body(&mut files, &ctx, &mut sum.errors);
    let dirty = files.dirty != 0;
    files.save_dirty(&mut sum.errors);
    sum.processed += 1;
    dirty
}

/// Synchronous variant of [`spawn_region_job`]'s skeleton, for callers
/// already running inside a job: load `RegionFiles` per region, run `body`,
/// save touched files. `sel` supplies the `RegionJobCtx` selection view —
/// pass `Selection::all()` for "no selection" (every chunk selected).
pub fn run_region_loop<F>(
    dirs: &WorldDirectories,
    sel: &Selection,
    regions: Vec<Point2i>,
    raw: bool,
    mut body: F,
) -> RegionJobSummary
where
    F: FnMut(&mut RegionFiles, &RegionJobCtx, &mut Vec<String>),
{
    let mut sum = RegionJobSummary {
        processed: 0,
        errors: Vec::new(),
    };
    let stamp = crate::files::backup_stamp();
    for region in regions {
        run_one(dirs, sel, region, raw, Some(&stamp), &mut sum, &mut body);
    }
    sum
}

/// Spawn a LOW-priority worker job iterating `regions` in order. For each:
/// loads its `RegionFiles`, runs `body`, saves touched files, calls
/// `on_region`. `on_done` fires once at the end, even when cancelled.
pub fn spawn_region_job<F>(
    title: &'static str,
    dirs: WorldDirectories,
    sel: Selection,
    regions: Vec<Point2i>,
    raw: bool,
    on_region: Box<dyn Fn(Point2i) + Send>,
    on_done: Box<dyn Fn(RegionJobSummary) + Send>,
    mut body: F,
) where
    F: FnMut(&mut RegionFiles, &RegionJobCtx, &mut Vec<String>) + Send + 'static,
{
    jobs::pools().add_process(Job::new(PRIORITY_LOW, None, move || {
        let p = Progress::new(title, regions.len() as i64);
        let mut sum = RegionJobSummary {
            processed: 0,
            errors: Vec::new(),
        };
        let stamp = crate::files::backup_stamp();
        for region in regions {
            let _g = p.inc_guard();
            p.set_current(mca_file_name(region));
            if p.is_cancelled() {
                break;
            }
            if run_one(&dirs, &sel, region, raw, Some(&stamp), &mut sum, &mut body) {
                on_region(region);
            }
        }
        on_done(sum);
    }));
}
