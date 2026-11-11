//! Filter-driven region jobs, ported from upstream `io.job.SelectFilterJob`,
//! `DeleteFilterJob` and `ExportFilterJob`.
//!
//! Each helper iterates the region locations present in any of the three
//! directories (region / poi / entities) of `WorldDirectories`, loads the
//! matching `McaFile`s and applies the compiled `filter::Expr` per chunk.
//! Deviations: upstream `Region.deleteChunks`/`keepChunks` pass the *region*
//! location to `Selection.isChunkSelected` (an upstream bug); we pass the
//! absolute chunk location like `getFilteredChunks` does.

use crate::files::{mca_file_name, parse_mca_file_name};
use crate::filter::{Expr, FilterCtx, MatchType};
use crate::mca::{Chunk, McaFile, WorldDirectories};
use crate::nbt;
use crate::point::Point2i;
use crate::selection::{ChunkSet, Selection};
use std::collections::BTreeSet;
use std::path::PathBuf;

/// upstream FilterChunksDialog.ApplyType for run_filter_select.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ApplyType {
    Overwrite,
    Add,
    Subtract,
}

#[derive(Default, Debug)]
pub struct FilterJobResult {
    /// region files processed (not skipped)
    pub regions_processed: usize,
    /// chunks matched by the filter (or touched by run_change_fields)
    pub chunks_matched: usize,
    /// region files whose contents were written back / written out
    pub files_written: usize,
    /// per-field errors collected while applying fields
    pub errors: usize,
}

/// all r.x.z region locations present in any of the three world dirs,
/// optionally restricted to regions touched by `selection`.
fn region_locations(
    wd: &WorldDirectories,
    selection: Option<&Selection>,
) -> Vec<Point2i> {
    let mut locs = BTreeSet::new();
    for dir in [&wd.region, &wd.poi, &wd.entities].into_iter().flatten() {
        let Ok(rd) = std::fs::read_dir(dir) else {
            continue;
        };
        for e in rd.flatten() {
            let Some(name) = e.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if let Some(r) = parse_mca_file_name(&name) {
                if selection
                    .map(|s| s.is_any_chunk_in_region_selected(r.as_long()))
                    .unwrap_or(true)
                {
                    locs.insert(r.as_long());
                }
            }
        }
    }
    locs.into_iter().map(Point2i::from_long).collect()
}

fn load_mca(path: &PathBuf) -> Option<McaFile> {
    let mut m = McaFile::new(path.clone()).ok()?;
    m.load(false).ok()?;
    Some(m)
}

fn dir_file(dir: &Option<PathBuf>, r: Point2i) -> Option<PathBuf> {
    dir.as_ref().map(|d| d.join(mca_file_name(r)))
}

struct RegionFiles {
    region: Option<(PathBuf, McaFile)>,
    poi: Option<(PathBuf, McaFile)>,
    entities: Option<(PathBuf, McaFile)>,
}

fn load_region_files(wd: &WorldDirectories, r: Point2i) -> RegionFiles {
    let load = |d: &Option<PathBuf>| dir_file(d, r).and_then(|p| load_mca(&p).map(|m| (p, m)));
    RegionFiles {
        region: load(&wd.region),
        poi: load(&wd.poi),
        entities: load(&wd.entities),
    }
}

fn chunk_at<'a>(files: &'a RegionFiles, i: usize) -> Option<&'a Chunk> {
    files
        .region
        .as_ref()
        .and_then(|(_, m)| m.chunks.get(i))
        .and_then(|c| c.as_ref())
        .filter(|c| c.data.is_some())
}

fn matches(expr: &Expr, files: &RegionFiles, i: usize, ctx: &mut FilterCtx) -> bool {
    let Some(c) = chunk_at(files, i) else {
        return false;
    };
    let dv = c.data.as_ref().map(|d| nbt::data_version(Some(d))).unwrap_or(0);
    expr.matches(c, dv, ctx)
}

fn applies_to_region(expr: &Expr, r: Point2i, ctx: &mut FilterCtx) -> bool {
    !matches!(expr.matches_region(r, ctx), MatchType::None)
}

fn chunk_index_loc(r: Point2i, i: usize) -> Point2i {
    r.region_to_chunk().add_p(Point2i::from_chunk_index(i as i32))
}

/// upstream SelectFilterJob: collect chunks matching `expr` (restricted to
/// `selection` when given), expand by `radius` (bounded by `selection`),
/// then merge into `target` per `apply`. Returns matched-chunk count.
pub fn run_filter_select(
    expr: &Expr,
    wd: &WorldDirectories,
    selection: Option<&Selection>,
    apply: ApplyType,
    radius: i32,
    target: &mut Selection,
) -> Result<FilterJobResult, String> {
    let mut res = FilterJobResult::default();
    let mut ctx = FilterCtx::new(wd.region.clone(), wd.entities.clone());
    for r in region_locations(wd, selection) {
        if !applies_to_region(expr, r, &mut ctx) {
            continue;
        }
        res.regions_processed += 1;
        let files = load_region_files(wd, r);
        let mut chunks = ChunkSet::new();
        for i in 0..1024 {
            let loc = chunk_index_loc(r, i);
            if selection.map(|s| s.is_chunk_selected(loc)).unwrap_or(true)
                && matches(expr, &files, i, &mut ctx)
            {
                chunks.set(i);
            }
        }
        if chunks.is_empty() {
            continue;
        }
        res.chunks_matched += chunks.size();
        // Selection.addAll(location, chunks): size==1024 -> whole region
        let mut per = Selection::new();
        if chunks.size() == 1024 {
            per.add_region(r.as_long());
        } else {
            per.selection.insert(r.as_long(), Some(chunks));
        }
        per.add_radius(radius, selection);
        match apply {
            ApplyType::Overwrite | ApplyType::Add => target.merge(&per),
            ApplyType::Subtract => {
                for (region, cs) in &per.selection {
                    match cs {
                        None => {
                            target.remove_region(*region);
                        }
                        Some(cs) => {
                            let base = Point2i::from_long(*region).region_to_chunk();
                            for i in cs.iter() {
                                target.remove_chunk(
                                    base.add_p(Point2i::from_chunk_index(i as i32)),
                                );
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(res)
}

/// upstream DeleteFilterJob: delete every matching chunk from all three
/// files of a region, save files that changed. Returns counts.
pub fn run_filter_delete(
    expr: &Expr,
    wd: &WorldDirectories,
    selection: Option<&Selection>,
) -> Result<FilterJobResult, String> {
    let mut res = FilterJobResult::default();
    let mut ctx = FilterCtx::new(wd.region.clone(), wd.entities.clone());
    for r in region_locations(wd, selection) {
        if !applies_to_region(expr, r, &mut ctx) {
            continue;
        }
        res.regions_processed += 1;
        let mut files = load_region_files(wd, r);
        let mut to_delete = ChunkSet::new();
        for i in 0..1024 {
            let loc = chunk_index_loc(r, i);
            if selection.map(|s| s.is_chunk_selected(loc)).unwrap_or(true)
                && matches(expr, &files, i, &mut ctx)
            {
                to_delete.set(i);
            }
        }
        if to_delete.is_empty() {
            continue;
        }
        res.chunks_matched += to_delete.size();
        for f in [&mut files.region, &mut files.poi, &mut files.entities]
            .into_iter()
            .flatten()
        {
            f.1.delete_chunks(&to_delete);
            let p = f.0.clone();
            if f.1.save_with_temp_file(&p).unwrap_or(false) {
                res.files_written += 1;
            }
        }
    }
    Ok(res)
}

/// upstream ChangeFieldsJob + Region.applyFieldChanges: apply parsed change
/// fields to every chunk that has data in at least one of the three files,
/// then save all files that were modified.
pub fn run_change_fields(
    fields: &[crate::changer::Field],
    force: bool,
    wd: &WorldDirectories,
    selection: Option<&Selection>,
) -> Result<FilterJobResult, String> {
    let mut res = FilterJobResult::default();
    for r in region_locations(wd, selection) {
        let mut files = load_region_files(wd, r);
        res.regions_processed += 1;
        for i in 0..1024 {
            let loc = chunk_index_loc(r, i);
            if !selection.map(|s| s.is_chunk_selected(loc)).unwrap_or(true) {
                continue;
            }
            fn chunk_mut<'x>(
                m: &'x mut Option<(PathBuf, McaFile)>,
                i: usize,
            ) -> Option<&'x mut Chunk> {
                m.as_mut()
                    .and_then(|(_, m)| m.chunks.get_mut(i))
                    .and_then(|c| c.as_mut())
            }
            let has_data = [&files.region, &files.poi, &files.entities]
                .into_iter()
                .flatten()
                .any(|(_, m)| {
                    m.chunks
                        .get(i)
                        .and_then(|c| c.as_ref())
                        .map(|c| c.data.is_some())
                        .unwrap_or(false)
                });
            if !has_data {
                continue;
            }
            let mut refs = crate::changer::ChunkRefs {
                region: chunk_mut(&mut files.region, i),
                poi: chunk_mut(&mut files.poi, i),
                entities: chunk_mut(&mut files.entities, i),
            };
            let cr = crate::changer::apply_change(&mut refs, fields, force);
            res.chunks_matched += 1;
            res.errors += cr.errors.len();
        }
        for f in [&mut files.region, &mut files.poi, &mut files.entities]
            .into_iter()
            .flatten()
        {
            let p = f.0.clone();
            if f.1.save_with_temp_file(&p).unwrap_or(false) {
                res.files_written += 1;
            }
        }
    }
    Ok(res)
}

/// upstream ExportFilterJob: write matching chunks into `dest`
/// WorldDirectories. Existing destination files are never overwritten
/// (the whole region is skipped when any of the three files exists).
pub fn run_filter_export(
    expr: &Expr,
    wd: &WorldDirectories,
    selection: Option<&Selection>,
    dest: &WorldDirectories,
) -> Result<FilterJobResult, String> {
    let mut res = FilterJobResult::default();
    let mut ctx = FilterCtx::new(wd.region.clone(), wd.entities.clone());
    for r in region_locations(wd, selection) {
        if !applies_to_region(expr, r, &mut ctx) {
            continue;
        }
        let to_region = dir_file(&dest.region, r);
        let to_poi = dir_file(&dest.poi, r);
        let to_entities = dir_file(&dest.entities, r);
        if [&to_region, &to_poi, &to_entities]
            .into_iter()
            .flatten()
            .any(|p| p.exists())
        {
            continue; // upstream: "exists, not overwriting"
        }
        res.regions_processed += 1;
        let mut files = load_region_files(wd, r);
        // keepChunks: delete everything that does NOT match
        let mut keep = ChunkSet::new();
        for i in 0..1024 {
            let loc = chunk_index_loc(r, i);
            if selection.map(|s| s.is_chunk_selected(loc)).unwrap_or(true)
                && matches(expr, &files, i, &mut ctx)
            {
                keep.set(i);
            }
        }
        res.chunks_matched += keep.size();
        let mut discard = ChunkSet::new();
        discard.fill();
        discard.and_not(&keep);
        let writes: [(&mut Option<(PathBuf, McaFile)>, Option<PathBuf>); 3] = [
            (&mut files.region, to_region),
            (&mut files.poi, to_poi),
            (&mut files.entities, to_entities),
        ];
        for (src, dst) in writes {
            let (Some((_, m)), Some(dst)) = (src.as_mut(), dst) else {
                continue;
            };
            m.delete_chunks(&discard);
            m.defragment(&dst).map_err(|e| e.to_string())?;
            res.files_written += 1;
        }
    }
    Ok(res)
}
