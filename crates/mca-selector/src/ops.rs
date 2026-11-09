//! Shared skeleton for the 12 per-region batch operations (delete, defragment,
//! export, copy, paste, import, swap, change-fields, sum, filter, save, edit).
//! Upstream: io/job/*SaveRun-style jobs iterated per selected region.

use mc_core::files::{self, mca_file_name};
use mc_core::jobs::{self, Job, Progress};
use mc_core::mca::{McaFile, WorldDirectories};
use mc_core::point::Point2i;
use mc_core::selection::{ChunkSet, Selection};
use std::path::PathBuf;

/// Iterate every selected region: load its mca file, run `f`, send a tile
/// invalidation for the region. `f` gets the region, the region's chunk mask
/// (None = whole region), the loaded file, its path and the world dirs, and
/// returns whether the file was modified (-> save).
pub fn for_each_selected_region<F>(
    sel: &Selection,
    dirs: WorldDirectories,
    title: &'static str,
    tx: std::sync::mpsc::Sender<crate::tilemap::TileResult>,
    f: F,
) where
    F: Fn(Point2i, &Option<ChunkSet>, &mut McaFile, &PathBuf, &WorldDirectories) -> bool
        + Send
        + 'static,
{
    let sel = sel.clone();
    jobs::pools().add_process(Job::new(jobs::PRIORITY_LOW, None, move || {
        let __p = Progress::new(title, sel.selection.len() as i64);
        for (&region_l, chunks) in &sel.selection {
            let __g = __p.inc_guard();
            let region = Point2i::from_long(region_l);
            __p.set_current(mca_file_name(region));
            if __p.is_cancelled() {
                break;
            }
            let Some(dir) = dirs.region.clone() else { continue };
            let path = dir.join(mca_file_name(region));
            if !path.exists() {
                continue;
            }
            let Ok(mut m) = McaFile::new(path.clone()) else { continue };
            if m.load(false).is_err() {
                continue;
            }
            let changed = f(region, chunks, &mut m, &path, &dirs);
            if changed {
                let _ = m.save_with_temp_file(&path);
            }
            let _ = tx.send(crate::tilemap::TileResult::Image {
                region,
                image: None,
                zoom: 0,
                structures: None,
                cache_write: false,
            });

        }
    }));
}
