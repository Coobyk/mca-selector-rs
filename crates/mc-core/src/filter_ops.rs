//! Filter-driven region jobs, ported from upstream `io.job.SelectFilterJob`,
//! `DeleteFilterJob` and `ExportFilterJob` over the shared `batch` skeleton
//! (upstream `WorldDirectories.listRegions` + `Region.loadRegion`).
//!
//! Deviations: upstream `Region.deleteChunks`/`keepChunks` pass the *region*
//! location to `Selection.isChunkSelected` (an upstream bug); we pass the
//! absolute chunk location like `getFilteredChunks` does.

use crate::batch::{self, RegionFiles};
use crate::filter::{Expr, FilterCtx, MatchType};
use crate::mca::{Chunk, McaFile, WorldDirectories};
use crate::nbt;
use crate::point::Point2i;
use crate::selection::{ChunkSet, Selection};
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
    /// chunks matched by the filter (or touched by run_change_fields)
    pub chunks_matched: usize,
    /// region files whose contents were written back / written out
    pub files_written: usize,
    /// per-field errors collected while applying fields + save failures
    pub errors: usize,
}

/// `selection` as a `&Selection` usable in a `RegionJobCtx`: `None` maps to
/// `Selection::all()` — the "whole world" encoding (inverted, no
/// exclusions), so `ctx.chunk_selected` answers correctly either way.
fn effective_sel<'a>(selection: Option<&'a Selection>, all: &'a Selection) -> &'a Selection {
    selection.unwrap_or(all)
}

fn chunk_at<'a>(files: &'a RegionFiles, i: usize) -> Option<&'a Chunk> {
    files
        .region
        .as_ref()
        .and_then(|m| m.chunks.get(i))
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

fn dir_file(dir: &Option<PathBuf>, r: Point2i) -> Option<PathBuf> {
    dir.as_ref().map(|d| d.join(crate::files::mca_file_name(r)))
}

fn chunk_mut(m: &mut Option<McaFile>, i: usize) -> Option<&mut Chunk> {
    m.as_mut()
        .and_then(|m| m.chunks.get_mut(i))
        .and_then(|c| c.as_mut())
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
) -> FilterJobResult {
    let mut res = FilterJobResult::default();
    let mut fctx = FilterCtx::new(wd.region.clone(), wd.entities.clone());
    let all = Selection::all();
    let sel = effective_sel(selection, &all);
    // region-level filter early-out happens before any file load (upstream
    // SelectFilterJob skips the load when the filter can't match).
    let regions: Vec<_> = batch::list_regions(wd, selection)
        .into_iter()
        .filter(|r| applies_to_region(expr, *r, &mut fctx))
        .collect();
    let sum = batch::run_region_loop(wd, sel, regions, false, |files, ctx, _errs| {
        let r = ctx.region;
        let mut chunks = ChunkSet::new();
        for i in 0..1024 {
            if ctx.chunk_selected(chunk_index_loc(r, i)) && matches(expr, files, i, &mut fctx) {
                chunks.set(i);
            }
        }
        if chunks.is_empty() {
            return;
        }
        res.chunks_matched += chunks.size();
        let mut per = Selection::new();
        per.set_region(r.as_long(), Some(chunks));
        per.add_radius(radius, selection);
        match apply {
            ApplyType::Overwrite | ApplyType::Add => target.merge(&per),
            ApplyType::Subtract => {
                for (region, cs) in per.regions() {
                    match cs {
                        None => {
                            target.remove_region(region);
                        }
                        Some(cs) => {
                            let base = Point2i::from_long(region).region_to_chunk();
                            for i in cs.iter() {
                                target
                                    .remove_chunk(base.add_p(Point2i::from_chunk_index(i as i32)));
                            }
                        }
                    }
                }
            }
        }
    });
    res.errors += sum.errors.len();
    res
}

/// upstream DeleteFilterJob: delete every matching chunk from all three
/// files of a region, save files that changed. Returns counts.
pub fn run_filter_delete(
    expr: &Expr,
    wd: &WorldDirectories,
    selection: Option<&Selection>,
) -> FilterJobResult {
    let mut res = FilterJobResult::default();
    let mut fctx = FilterCtx::new(wd.region.clone(), wd.entities.clone());
    let all = Selection::all();
    let sel = effective_sel(selection, &all);
    let regions: Vec<_> = batch::list_regions(wd, selection)
        .into_iter()
        .filter(|r| applies_to_region(expr, *r, &mut fctx))
        .collect();
    let sum = batch::run_region_loop(wd, sel, regions, false, |files, ctx, errs| {
        let r = ctx.region;
        let mut to_delete = ChunkSet::new();
        for i in 0..1024 {
            if ctx.chunk_selected(chunk_index_loc(r, i)) && matches(expr, files, i, &mut fctx) {
                to_delete.set(i);
            }
        }
        if to_delete.is_empty() {
            return;
        }
        res.chunks_matched += to_delete.size();
        for kind in batch::MCA_KINDS {
            if let Some(m) = files.file_mut(kind) {
                m.delete_chunks(&to_delete);
                files.touch(kind);
            }
        }
        // count actual writes here — save failures land in `errs`, not in
        // files_written.
        res.files_written += files.save_dirty(errs);
    });
    res.errors += sum.errors.len();
    res
}

/// upstream ChangeFieldsJob + Region.applyFieldChanges: apply parsed change
/// fields to every chunk that has data in at least one of the three files,
/// then save all files that were modified.
pub fn run_change_fields(
    fields: &[crate::changer::Field],
    force: bool,
    wd: &WorldDirectories,
    selection: Option<&Selection>,
) -> FilterJobResult {
    let mut res = FilterJobResult::default();
    let all = Selection::all();
    let sel = effective_sel(selection, &all);
    let regions = batch::list_regions(wd, selection);
    let sum = batch::run_region_loop(wd, sel, regions, false, |files, ctx, errs| {
        let r = ctx.region;
        for i in 0..1024 {
            if !ctx.chunk_selected(chunk_index_loc(r, i)) {
                continue;
            }
            let has_data = [&files.region, &files.poi, &files.entities]
                .into_iter()
                .flatten()
                .any(|m| {
                    m.chunks
                        .get(i)
                        .and_then(|c| c.as_ref())
                        .map(|c| c.data.is_some())
                        .unwrap_or(false)
                });
            if !has_data {
                continue;
            }
            let mut refs = crate::chunk::ChunkRefs {
                region: chunk_mut(&mut files.region, i),
                poi: chunk_mut(&mut files.poi, i),
                entities: chunk_mut(&mut files.entities, i),
            };
            let cr = crate::changer::apply_change(&mut refs, fields, force);
            if cr.applied > 0 {
                res.chunks_matched += 1;
                for kind in batch::MCA_KINDS {
                    files.touch(kind);
                }
            }
            errs.extend(cr.errors);
        }
        res.files_written += files.save_dirty(errs);
    });
    res.errors += sum.errors.len();
    res
}

/// upstream ExportFilterJob: write matching chunks into `dest`
/// WorldDirectories. Existing destination files are never overwritten
/// (the whole region is skipped when any of the three files exists).
pub fn run_filter_export(
    expr: &Expr,
    wd: &WorldDirectories,
    selection: Option<&Selection>,
    dest: &WorldDirectories,
) -> FilterJobResult {
    let mut res = FilterJobResult::default();
    let mut fctx = FilterCtx::new(wd.region.clone(), wd.entities.clone());
    let all = Selection::all();
    let sel = effective_sel(selection, &all);
    let regions: Vec<_> = batch::list_regions(wd, selection)
        .into_iter()
        .filter(|r| applies_to_region(expr, *r, &mut fctx))
        .filter(|r| {
            // upstream: "exists, not overwriting" — the skip precedes the load
            ![&dest.region, &dest.poi, &dest.entities]
                .into_iter()
                .flatten()
                .map(|d| d.join(crate::files::mca_file_name(*r)))
                .any(|p| p.exists())
        })
        .collect();
    let sum = batch::run_region_loop(wd, sel, regions, false, |files, ctx, errs| {
        let r = ctx.region;
        // keepChunks: delete everything that does NOT match
        let mut keep = ChunkSet::new();
        for i in 0..1024 {
            if ctx.chunk_selected(chunk_index_loc(r, i)) && matches(expr, files, i, &mut fctx) {
                keep.set(i);
            }
        }
        res.chunks_matched += keep.size();
        let mut discard = ChunkSet::new();
        discard.fill();
        discard.and_not(&keep);
        let writes: [(&mut Option<McaFile>, Option<PathBuf>); 3] = [
            (&mut files.region, dir_file(&dest.region, r)),
            (&mut files.poi, dir_file(&dest.poi, r)),
            (&mut files.entities, dir_file(&dest.entities, r)),
        ];
        for (src, dst) in writes {
            let (Some(m), Some(dst)) = (src, dst) else {
                continue;
            };
            m.delete_chunks(&discard);
            // upstream ExportFilterJob logs+continues on per-region failure
            match m.defragment(&dst) {
                Ok(()) => res.files_written += 1,
                Err(e) => errs.push(format!("{}: {e}", dst.display())),
            }
        }
    });
    res.errors += sum.errors.len();
    res
}
