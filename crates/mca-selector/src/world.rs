//! World open/dimension switching and cache clearing (upstream
//! showWorldDialog / OptionBar world picker / CacheHelper.clearAllCaches).

use crate::config;
use crate::dialogs::Dialog;
use crate::view::MainView;
use gpui::*;
use mc_core::files;
use std::path::PathBuf;

impl MainView {
    pub fn open_world_dialog(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Select a world directory".into()),
        });
        cx.spawn(async move |this: WeakEntity<MainView>, cx: &mut AsyncApp| {
            match rx.await {
                Ok(Ok(Some(paths))) => {
                    if let Some(p) = paths.into_iter().next() {
                        let _ = this.update(&mut *cx, |v, cx| v.open_world_at(p, cx));
                    }
                }
                // a portal/picker failure used to drop silently — File >
                // Open World then appears to do nothing at all.
                Ok(Err(e)) => {
                    let _ = this.update(&mut *cx, |v, cx| {
                        v.dialog = Some(Dialog::Error {
                            msg: format!("could not open file picker:\n{e}"),
                        });
                        cx.notify();
                    });
                }
                _ => {}
            }
        })
        .detach();
    }

    pub fn open_world_at(&mut self, dir: PathBuf, cx: &mut Context<Self>) {
        // upstream: confirm before discarding an unsaved selection
        if self.map.unsaved_selection {
            let d = dir.clone();
            self.dialog = Some(Dialog::Confirm {
                msg: "There is an unsaved selection. Open the world anyway?".into(),
                action: Box::new(move |v, _w, cx| {
                    v.map.unsaved_selection = false;
                    v.open_world_at(d.clone(), cx);
                }),
            });
            cx.notify();
            return;
        }
        // upstream openWorld: detect dims under the picked dir (the dir
        // itself counts if it holds a valid region folder), open the sole
        // one directly, or show the SelectWorldDialog picker.
        let dims = files::detect_dimension_directories(&dir);
        if dims.is_empty() {
            self.dialog = Some(Dialog::Error {
                msg: format!("no dimensions found in {}", dir.display()),
            });
            cx.notify();
        } else if dims.len() == 1 {
            self.do_open(dims[0].clone(), dims, cx);
        } else {
            self.dialog = Some(Dialog::SelectDimension { dims });
            cx.notify();
        }
    }

    pub(crate) fn do_open(&mut self, root: PathBuf, dims: Vec<PathBuf>, cx: &mut Context<Self>) {
        let dirs = files::detect_world_directories(&root);
        if dirs.region.is_none() {
            self.dialog = Some(Dialog::Error {
                msg: format!("Could not find region folder in {}", root.display()),
            });
            cx.notify();
            return;
        }
        self.map.clear();
        // cancel_jobs also drops loading/overlay bookkeeping — a plain
        // clear_queues() left those flags set forever.
        self.map.cancel_jobs();
        let ws = config::open_world(dirs, dims.clone());
        self.map.regions = files::scan_region_files(ws.region_dir());
        // upstream keeps the camera across opens — but if the current view
        // shows none of this world's regions it looks like nothing opened.
        // Center on the mean loaded region only in that case.
        let (vmin, vmax) = self
            .map
            .visible_region_bounds(mc_core::point::Point2f::new(0.0, 0.0));
        let overlaps = self.map.regions.iter().any(|&k| {
            let r = mc_core::point::Point2i::from_long(k);
            r.x >= vmin.x - 1 && r.x <= vmax.x + 1 && r.z >= vmin.z - 1 && r.z <= vmax.z + 1
        });
        if !overlaps && !self.map.regions.is_empty() {
            let n = self.map.regions.len() as i64;
            let (mut sx, mut sz) = (0i64, 0i64);
            for &k in &self.map.regions {
                let r = mc_core::point::Point2i::from_long(k);
                sx += r.x as i64;
                sz += r.z as i64;
            }
            self.map
                .go_to((sx / n) as i32 * 512 + 256, (sz / n) as i32 * 512 + 256);
        }
        // `disabled` is derived from config::WORLD — setting the world above
        // enables the map.
        self.request_update();
        let mut g = config::global();
        g.add_recent_world(root);
        config::set_global(g);
        cx.notify();
    }

    pub fn open_dimension_dialog(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ws) = config::world() {
            if !ws.dimensions.is_empty() {
                self.dialog = Some(Dialog::SelectDimension { dims: ws.dimensions.clone() });
                cx.notify();
            }
        }
    }

    pub(crate) fn clear_all_caches(&mut self, cx: &mut Context<Self>) {
        if let Some(ws) = config::world() {
            let dir = ws.cache_dir.clone();
            let _ = std::fs::remove_dir_all(&dir);
            mc_core::cache::delete_all(&ws.uuid);
        }
        self.map.pool.clear();
        self.map.pool_order.clear();
        self.map.mark_all_tiles_obsolete();
        self.map.loading.clear();
        self.request_update();
        cx.notify();
    }
}
