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
            if let Ok(Ok(Some(paths))) = rx.await {
                if let Some(p) = paths.into_iter().next() {
                    let _ = this.update(&mut *cx, |v, cx| v.open_world_at(p, cx));
                }
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
        // upstream showWorldDialog: if not a valid dimension itself, scan it;
        // if it contains multiple dims, open dimension dialog; else open.
        if files::is_valid_dimension(&dir) {
            let dims = files::detect_dimension_directories(&dir);
            let mut ordered = vec![dir.clone()];
            ordered.extend(dims.iter().filter(|d| **d != dir).cloned());
            self.do_open(dir.clone(), ordered, cx);
            return;
        }
        let dims = files::detect_dimension_directories(&dir);
        if dims.len() > 1 {
            self.dialog = Some(Dialog::SelectDimension { dims });
            cx.notify();
        } else if let Some(d) = dims.into_iter().next() {
            // route through do_open_at_dir on the DETECTED dimension dir —
            // this used to pass `dir` (the base, not a valid dimension) and
            // dropped the dimension list entirely.
            self.do_open_at_dir(d, cx);
        } else {
            self.dialog = Some(Dialog::Error {
                msg: format!("no minecraft installation found in\n{}", dir.display()),
            });
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
        // `disabled` is derived from config::WORLD — setting the world above
        // enables the map.
        self.request_update();
        let mut g = config::global();
        g.add_recent_world(root);
        config::set_global(g);
        cx.notify();
    }

    pub(crate) fn do_open_at_dir(&mut self, dir: PathBuf, cx: &mut Context<Self>) {
        let dims = files::detect_dimension_directories(&dir);
        let mut ordered = vec![dir.clone()];
        ordered.extend(dims.iter().filter(|d| **d != dir).cloned());
        self.do_open(dir.clone(), ordered, cx);
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
