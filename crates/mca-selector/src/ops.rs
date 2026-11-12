//! Chunk/selection/file operations over the open world (upstream io/job/*).
//! Region-iterating jobs share mc_core::batch::spawn_region_job: it loads
//! the region+poi+entities mca set, runs the body, saves touched files and
//! reports progress; on_region invalidates the tile through the unified
//! TileResult channel (no side-channel statics).

use crate::config;
use crate::dialogs::{Dialog, FilterAction};
use crate::tilemap::{TileResult, TILE_SIZE};
use crate::view::MainView;
use gpui::*;
use mc_core::batch::{self, McKind};
use mc_core::files::{self, mca_file_name};
use mc_core::jobs::{self, Job, Progress};
use mc_core::point::{Point2f, Point2i};
use mc_core::selection::{ChunkSet, Selection};
use std::path::PathBuf;
use std::sync::mpsc::Sender;

/// send a batch-job completion message through the tile-result channel —
/// the single back-channel for "job finished" Info dialogs.
fn job_info(tx: &Sender<TileResult>, msg: impl Into<String>) {
    let _ = tx.send(TileResult::JobInfo(msg.into()));
}

/// tile invalidation sender handed to spawn_region_job's on_region.
fn invalidate_on_region(tx: &Sender<TileResult>) -> Box<dyn Fn(Point2i) + Send> {
    let tx = tx.clone();
    Box::new(move |region| {
        let _ = tx.send(TileResult::Invalidate { region });
    })
}

/// default on_done for spawn_region_job: surface collected errors as an
/// Info dialog instead of dropping them.
fn report_done(tx: Sender<TileResult>) -> Box<dyn Fn(batch::RegionJobSummary) + Send> {
    Box::new(move |sum| {
        if !sum.errors.is_empty() {
            job_info(&tx, format!("Finished with {} error(s):\n{}", sum.errors.len(), sum.errors.join("\n")));
        }
    })
}

impl MainView {
    /// upstream saveScreenshot: render the currently visible region bounds
    /// into one PNG through the same export-image machinery as
    /// "Export as image".
    pub(crate) fn save_screenshot(&mut self, cx: &mut Context<Self>) {
        if self.map.disabled() {
            return;
        }
        let (min, max) = self.map.visible_region_bounds(Point2f::ZERO);
        let w = (max.x - min.x + 1) as usize * TILE_SIZE;
        let h = (max.z - min.z + 1) as usize * TILE_SIZE;
        let (minx, minz) = (min.x, min.z);
        // whole-region selection over the visible bounds
        let mut sel = Selection::new();
        for rx in min.x..=max.x {
            for rz in min.z..=max.z {
                sel.set_region(Point2i::new(rx, rz).as_long(), None);
            }
        }
        let rx = cx.prompt_for_new_path(
            &PathBuf::from(std::env::var("HOME").unwrap_or_default()),
            Some("screenshot.png"),
        );
        cx.spawn(async move |this: WeakEntity<MainView>, cx: &mut AsyncApp| {
            if let Ok(Ok(Some(path))) = rx.await {
                let _ = this.update(&mut *cx, |v, _cx| {
                    v.do_export_image(sel.clone(), path.clone(), w, h, minx, minz);
                });
            }
        })
        .detach();
    }

    pub(crate) fn import_selection(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Select a csv file".into()),
        });
        cx.spawn(async move |this: WeakEntity<MainView>, cx: &mut AsyncApp| {
            if let Ok(Ok(Some(paths))) = rx.await {
                if let Some(p) = paths.into_iter().next() {
                    let _ = this.update(&mut *cx, |v, cx| {
                        match mc_core::selection::Selection::read_csv(&p) {
                            Ok(sel) => {
                                v.map.selection.merge(&sel);
                                v.map.mark_prev = None;
                                v.map.selected_chunks = v.map.selection.count() as i64;
                                v.map.unsaved_selection = true;
                                v.map.selection_version += 1;
                            }
                            Err(e) => {
                                v.dialog = Some(Dialog::Error { msg: e.to_string() });
                            }
                        }
                        cx.notify();
                    });
                }
            }
        })
        .detach();
    }

    pub(crate) fn export_selection(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_new_path(
            &PathBuf::from(std::env::var("HOME").unwrap_or_default()),
            Some("selection.csv"),
        );
        let sel = self.map.selection.clone();
        cx.spawn(async move |_this, _cx| {
            if let Ok(Ok(Some(path))) = rx.await {
                let _ = sel.save_csv(&path);
            }
        })
        .detach();
    }

    // -- chunk operations -------------------------------------------------------

    pub(crate) fn delete_chunks(&mut self, cx: &mut Context<Self>) {
        let sel = self.map.selection.clone();
        if sel.is_empty() { return; }
        // upstream DeleteConfirmationDialog: verbose header unless inverted
        let msg = if sel.inverted {
            "You are about to delete an unknown number of chunks from this world.".to_string()
        } else {
            format!("You are about to delete {} chunks from this world.", self.map.selected_chunks)
        };
        self.dialog = Some(Dialog::Confirm {
            msg: format!("{msg}\nAre you sure?"),
            action: Box::new(move |v, _w, cx| {
                let dirs = crate::config::world().map(|w| w.dirs.clone()).unwrap_or_default();
                // upstream DeleteSelectionJob iterates the TRUE selection —
                // an inverted selection covers every region file on disk.
                let regions = batch::effective_regions(&sel, &dirs);
                let tx = v.map.result_tx.clone();
                batch::spawn_region_job(
                    "Deleting chunks",
                    dirs,
                    sel.clone(),
                    regions,
                    false,
                    invalidate_on_region(&tx),
                    report_done(tx.clone()),
                    |files, ctx, errs| {
                        match ctx.selection.selected_chunks(ctx.region) {
                            // whole region selected: upstream deletes all
                            // three mca files outright.
                            Some(None) => {
                                for m in [
                                    files.region.take(),
                                    files.poi.take(),
                                    files.entities.take(),
                                ]
                                .into_iter()
                                .flatten()
                                {
                                    if let Err(e) = std::fs::remove_file(&m.file) {
                                        errs.push(format!("{}: {e}", m.file.display()));
                                    }
                                }
                            }
                            Some(Some(cs)) => {
                                if cs.is_empty() {
                                    return;
                                }
                                // upstream deletes the chunks from region,
                                // poi AND entities files.
                                if let Some(m) = files.region.as_mut() {
                                    m.delete_chunks(&cs);
                                    files.touch(McKind::Region);
                                }
                                if let Some(m) = files.poi.as_mut() {
                                    m.delete_chunks(&cs);
                                    files.touch(McKind::Poi);
                                }
                                if let Some(m) = files.entities.as_mut() {
                                    m.delete_chunks(&cs);
                                    files.touch(McKind::Entities);
                                }
                            }
                            None => {}
                        }
                    },
                );
                v.dialog = None;
                cx.notify();
            }),
        });
        cx.notify();
    }

    pub(crate) fn defragment(&mut self, cx: &mut Context<Self>) {
        let sel = self.map.selection.clone();
        if sel.is_empty() { return; }
        // upstream DefragmentConfirmationDialog: verbose header unless inverted
        let msg = if sel.inverted {
            "You are about to defragment an unknown number of regions.".to_string()
        } else {
            format!("You are about to defragment {} regions.", sel.regions().count())
        };
        self.dialog = Some(Dialog::Confirm {
            msg: format!("{msg}\nAre you sure?"),
            action: Box::new(move |v, _w, cx| {
                let dirs = crate::config::world().map(|w| w.dirs.clone()).unwrap_or_default();
                let regions = batch::effective_regions(&sel, &dirs);
                let tx = v.map.result_tx.clone();
                batch::spawn_region_job(
                    "Defragmenting",
                    dirs,
                    sel.clone(),
                    regions,
                    false,
                    invalidate_on_region(&tx),
                    report_done(tx.clone()),
                    |files, _ctx, errs| {
                        // upstream Region.deFragment rewrites every loaded
                        // file (region/poi/entities).
                        for (m, name) in [
                            (files.region.as_mut(), "region"),
                            (files.poi.as_mut(), "poi"),
                            (files.entities.as_mut(), "entities"),
                        ] {
                            if let Some(m) = m {
                                let p = m.file.clone();
                                if let Err(e) = m.defragment(&p) {
                                    errs.push(format!("{name} {}: {e}", p.display()));
                                }
                            }
                        }
                    },
                );
                v.dialog = None;
                cx.notify();
            }),
        });
        cx.notify();
    }

    pub(crate) fn export_chunks(&mut self, cx: &mut Context<Self>) {
        let sel = self.map.selection.clone();
        if sel.is_empty() { return; }
        let n = self.map.selected_chunks;
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: false, directories: true, multiple: false,
            prompt: Some("Select world directory to export to".into()),
        });
        // upstream ExportConfirmationDialog after the directory pick
        cx.spawn(async move |this: WeakEntity<MainView>, cx: &mut AsyncApp| {
            if let Ok(Ok(Some(paths))) = rx.await {
                if let Some(dir) = paths.into_iter().next() {
                    let msg = if sel.inverted {
                        "You are about to export an unknown number of chunks from this world.".to_string()
                    } else {
                        format!("You are about to export {n} chunks from this world.")
                    };
                    let _ = this.update(&mut *cx, |v, cx| {
                        v.dialog = Some(Dialog::Confirm {
                            msg: format!("{msg}\nto {}\nAre you sure?", dir.display()),
                            action: Box::new(move |v, _w, cx| {
                                v.do_export_chunks(sel.clone(), dir.clone());
                                v.dialog = None;
                                cx.notify();
                            }),
                        });
                        cx.notify();
                    });
                }
            }
        }).detach();
    }

    /// upstream ExportChunkSelectionJob: copies whole region files for
    /// fully-selected regions; partial selections get a new file with only
    /// the selected chunks.
    pub(crate) fn do_export_chunks(&mut self, sel: Selection, dir: PathBuf) {
        let region_dir = dir.join("region");
        let _ = std::fs::create_dir_all(&region_dir);
        let dirs = crate::config::world().map(|w| w.dirs.clone()).unwrap_or_default();
        let regions = batch::effective_regions(&sel, &dirs);
        let tx = self.map.result_tx.clone();
        batch::spawn_region_job(
            "Exporting chunks",
            dirs,
            sel.clone(),
            regions,
            false,
            Box::new(|_| {}),
            report_done(tx.clone()),
            move |files, ctx, errs| {
                let dst = region_dir.join(mca_file_name(ctx.region));
                match ctx.selection.selected_chunks(ctx.region) {
                    Some(None) => {
                        // whole region: copy the file verbatim
                        if let Some(m) = files.region.as_ref() {
                            if let Err(e) = std::fs::copy(&m.file, &dst) {
                                errs.push(format!("{}: {e}", dst.display()));
                            }
                        }
                    }
                    Some(Some(cs)) => {
                        if cs.is_empty() {
                            return;
                        }
                        let Some(m) = files.region.as_mut() else { return };
                        // keep only selected chunks
                        m.delete_chunks(&cs.flip());
                        if let Err(e) = m.save_with_temp_file(&dst) {
                            errs.push(format!("{}: {e}", dst.display()));
                        }
                    }
                    None => {}
                }
            },
        );
    }

    pub(crate) fn copy_chunks(&mut self, cx: &mut Context<Self>) {
        // upstream copies selected chunks into cache-dir clipboard world
        let sel = self.map.selection.clone();
        if sel.is_empty() { return; }
        let clip = files::base_cache_dir().join("clipboard").join("region");
        let _ = std::fs::remove_dir_all(&clip);
        let _ = std::fs::create_dir_all(&clip);
        let Some(origin_l) = sel.regions().next().map(|(k, _)| k) else { return };
        let origin = Point2i::from_long(origin_l);
        let dirs = crate::config::world().map(|w| w.dirs.clone()).unwrap_or_default();
        let regions = batch::effective_regions(&sel, &dirs);
        let clip_sel = sel.clone();
        let tx = self.map.result_tx.clone();
        batch::spawn_region_job(
            "Copying chunks",
            dirs,
            sel.clone(),
            regions,
            false,
            Box::new(|_| {}),
            Box::new(move |sum| {
                if !sum.errors.is_empty() {
                    job_info(&tx, format!("Finished with {} error(s):\n{}", sum.errors.len(), sum.errors.join("\n")));
                }
                // record origin + selection for offset paste
                let clip = files::base_cache_dir().join("clipboard").join("region");
                let _ = std::fs::write(clip.join("../origin.txt"), format!("{},{}", origin.x, origin.z));
                let _ = clip_sel.save_csv(&clip.join("../selection.csv"));
            }),
            move |files, ctx, errs| {
                let dst = clip.join(mca_file_name(ctx.region));
                match ctx.selection.selected_chunks(ctx.region) {
                    Some(None) => {
                        if let Some(m) = files.region.as_ref() {
                            if let Err(e) = std::fs::copy(&m.file, &dst) {
                                errs.push(format!("{}: {e}", dst.display()));
                            }
                        }
                    }
                    Some(Some(cs)) => {
                        if cs.is_empty() {
                            return;
                        }
                        let Some(m) = files.region.as_mut() else { return };
                        m.delete_chunks(&cs.flip());
                        if let Err(e) = m.save_with_temp_file(&dst) {
                            errs.push(format!("{}: {e}", dst.display()));
                        }
                    }
                    None => {}
                }
            },
        );
        cx.notify();
    }

    pub(crate) fn paste_chunks(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.map.pasted.is_some() {
            let o = self.map.pasted.as_ref().unwrap().offset;
            self.dialog = Some(Dialog::PasteConfirm {
                ox: o.x, oz: o.z, overwrite: true, sel_only: false,
            });
            cx.notify();
            return;
        }
        let clip = files::base_cache_dir().join("clipboard").join("region");
        if !clip.is_dir() { return; }
        // upstream pastes via SelectionData on the system clipboard — ours is
        // the clipboard region dir + selection.csv.
        let sel = Selection::read_csv(
            &files::base_cache_dir().join("clipboard").join("selection.csv"),
        ).ok();
        // upstream centers the ghost using the CANVAS size in chunks —
        // map.width/height track the painted canvas rect (window minus
        // title/status/option bars), not the raw window bounds.
        let size = Point2f::new(self.map.width, self.map.height);
        self.map.set_pasted(clip, sel, size);
        cx.notify();
    }

    /// import clipboard chunks at offset (upstream ImportJob via PasteChunks confirm)
    pub(crate) fn do_paste_import(&mut self, ox: i32, oz: i32, overwrite: bool, sel_only: bool, cx: &mut Context<Self>) {
        let clip = files::base_cache_dir().join("clipboard").join("region");
        if !clip.is_dir() { return; }
        let target_sel = if sel_only { Some(self.map.selection.clone()) } else { None };
        let pasted_sel = self.map.pasted.as_ref().and_then(|p| p.selection.clone());
        let offset = Point2i::new(ox, oz);
        let tx = self.map.result_tx.clone();
        self.map.cancel_paste();
        import_mca_dir(clip, pasted_sel, offset, overwrite, target_sel, tx);
        cx.notify();
    }

    pub(crate) fn export_image(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let sel = self.map.selection.clone();
        if sel.is_empty() {
            self.dialog = Some(Dialog::Error { msg: "no selection".into() });
            cx.notify();
            return;
        }
        // upstream generateImageFromSelection guards the pixel count before
        // the file pick; ours is region-aligned (512px per region).
        let dirs = crate::config::world().map(|w| w.dirs.clone()).unwrap_or_default();
        let regions = batch::effective_regions(&sel, &dirs);
        let mut minx = i32::MAX; let mut minz = i32::MAX;
        let mut maxx = i32::MIN; let mut maxz = i32::MIN;
        for r in &regions {
            minx = minx.min(r.x); minz = minz.min(r.z);
            maxx = maxx.max(r.x); maxz = maxz.max(r.z);
        }
        let w = (maxx - minx + 1) as usize * 512;
        let h = (maxz - minz + 1) as usize * 512;
        if (w as i64) * (h as i64) > i32::MAX as i64 {
            self.dialog = Some(Dialog::Error {
                msg: format!("dimensions are too large to generate an image: {w}x{h}"),
            });
            cx.notify();
            return;
        }
        let rx = cx.prompt_for_new_path(
            &PathBuf::from(std::env::var("HOME").unwrap_or_default()),
            Some("selection.png"),
        );
        // upstream ImageExportConfirmationDialog after the file pick
        cx.spawn(async move |this: WeakEntity<MainView>, cx: &mut AsyncApp| {
            if let Ok(Ok(Some(path))) = rx.await {
                let _ = this.update(&mut *cx, |v, cx| {
                    v.dialog = Some(Dialog::Confirm {
                        msg: format!(
                            "You are about to create an image with the dimensions {w}x{h}.\n{}\nAre you sure?",
                            path.display()
                        ),
                        action: Box::new(move |v, _w, cx| {
                            v.do_export_image(sel.clone(), path.clone(), w, h, minx, minz);
                            v.dialog = None;
                            cx.notify();
                        }),
                    });
                    cx.notify();
                });
            }
        }).detach();
    }

    /// upstream ExportImageJob: render every selected region at zoom 1 into
    /// one big PNG. Regions are enumerated through the batch runner; the
    /// pixel buffer is shared with the body because PNG assembly happens in
    /// on_done.
    pub(crate) fn do_export_image(&mut self, sel: Selection, path: PathBuf, w: usize, h: usize, minx: i32, minz: i32) {
        let cfg = crate::config::world().map(|w| {
            mc_core::render::RenderSettings {
                render_height: w.cfg.render_height,
                layer_only: w.cfg.render_layer_only,
                caves: w.cfg.render_caves,
                shade: w.cfg.shade,
                shade_water: w.cfg.shade_water,
                shade_altitude: w.cfg.shade_altitude,
                dim: w.dim_kind(),
            }
        }).unwrap_or_default();
        let dirs = crate::config::world().map(|w| w.dirs.clone()).unwrap_or_default();
        let regions = batch::effective_regions(&sel, &dirs);
        let px = std::sync::Arc::new(std::sync::Mutex::new(vec![0u32; w * h]));
        let px_body = px.clone();
        // CORRUPTED is a UI-thread local — snapshot it now; the worker
        // thread would see the default zeros.
        let corrupted = crate::tilemap::CORRUPTED.with(|c| *c.borrow());
        let tx = self.map.result_tx.clone();
        batch::spawn_region_job(
            "Exporting image",
            dirs,
            sel,
            regions,
            true,
            Box::new(|_| {}),
            Box::new(move |sum| {
                if !sum.errors.is_empty() {
                    job_info(&tx, format!("Finished with {} error(s):\n{}", sum.errors.len(), sum.errors.join("\n")));
                }
                let px = px.lock().unwrap();
                let rgba = crate::tilemap::argb_to_rgba(&px);
                let _ = image::save_buffer(&path, &rgba, w as u32, h as u32, image::ColorType::Rgba8);
            }),
            move |files, ctx, errs| {
                let Some(m) = files.region.as_ref() else { return };
                if let Some(img) = mc_core::render::generate_image(m, 1, &cfg, &corrupted) {
                    let ox = ((ctx.region.x - minx) * 512) as usize;
                    let oz = ((ctx.region.z - minz) * 512) as usize;
                    let mut px = px_body.lock().unwrap();
                    for z in 0..512usize {
                        for x in 0..512usize {
                            px[(oz + z) * w + ox + x] = img[z * 512 + x];
                        }
                    }
                } else {
                    errs.push(format!("{}", mca_file_name(ctx.region)));
                }
            },
        );
    }

    /// upstream SumSelectionJob: sums the ACTIVE OVERLAY's parseValue over all
    /// selected chunks (that's why it's disabled without an overlay).
    pub(crate) fn sum_selection(&mut self, cx: &mut Context<Self>) {
        let sel = self.map.selection.clone();
        let Some(ov) = self.map.overlay.clone() else { return };
        if sel.is_empty() { return; }
        // upstream NumberDialog title: "<type>(<short multi values>)" —
        // carried inside the JobInfo payload so no side channel is needed.
        let mv = ov.raw_multi.trim();
        let title = if mv.is_empty() {
            ov.typ.display_name().to_string()
        } else {
            format!("{}({})", ov.typ.display_name(), mv)
        };
        let dirs = crate::config::world().map(|w| w.dirs.clone()).unwrap_or_default();
        let regions = batch::effective_regions(&sel, &dirs);
        let tx = self.map.result_tx.clone();
        let sum = std::sync::Arc::new(std::sync::Mutex::new(0i64));
        let sum_body = sum.clone();
        batch::spawn_region_job(
            "Summing selection",
            dirs,
            sel,
            regions,
            false,
            Box::new(|_| {}),
            Box::new(move |s| {
                let sum = *sum.lock().unwrap();
                let mut msg = format!("{title}: {sum}");
                if !s.errors.is_empty() {
                    msg += &format!("\n{} error(s):\n{}", s.errors.len(), s.errors.join("\n"));
                }
                job_info(&tx, msg);
            }),
            move |files, ctx, _errs| {
                // upstream clamps the run at -1 on overflow; once the flag
                // is set the remaining regions are skipped.
                if *sum_body.lock().unwrap() < 0 {
                    return;
                }
                for i in 0..1024 {
                    let loc = ctx.region.region_to_chunk().add_p(Point2i::from_chunk_index(i as i32));
                    if !ctx.chunk_selected(loc) {
                        continue;
                    }
                    let Some(c) = files.region.as_ref().and_then(|m| m.chunks[i].as_ref()) else { continue };
                    let Some(d) = &c.data else { continue };
                    let pd = files.poi.as_ref().and_then(|m| m.chunks[i].as_ref()).and_then(|ch| ch.data.as_ref());
                    let ed = files.entities.as_ref().and_then(|m| m.chunks[i].as_ref()).and_then(|ch| ch.data.as_ref());
                    let dv = mc_core::nbt::data_version(Some(d));
                    let mut s = sum_body.lock().unwrap();
                    *s += ov.parse_value(d, pd, ed, c.timestamp, dv) as i64;
                    if *s < 0 {
                        *s = -1;
                        return;
                    }
                }
            },
        );
        cx.notify();
    }

    /// upstream importChunks: pick a source WORLD directory, then the
    /// ImportConfirmationDialog (offset, overwrite, selection only) — the
    /// y-offset and section-range options are not ported (they would need
    /// section-level NBT rewriting).
    pub(crate) fn import_chunks(&mut self, cx: &mut Context<Self>) {
        if self.map.disabled() { return; }
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: false, directories: true, multiple: false,
            prompt: Some("Select world directory to import from".into()),
        });
        cx.spawn(async move |this: WeakEntity<MainView>, cx: &mut AsyncApp| {
            if let Ok(Ok(Some(paths))) = rx.await {
                if let Some(dir) = paths.into_iter().next() {
                    let _ = this.update(&mut *cx, |v, cx| {
                        let src = mc_core::files::detect_world_directories(&dir);
                        if src.region.is_none() {
                            v.dialog = Some(Dialog::Error {
                                msg: "invalid world directory, missing 'region' directory".into(),
                            });
                        } else {
                            v.dialog = Some(Dialog::Import {
                                dir,
                                ox: "0".into(),
                                oz: "0".into(),
                                overwrite: true,
                                sel_only: false,
                                focus_x: true,
                                invalid: false,
                            });
                        }
                        cx.notify();
                    });
                }
            }
        }).detach();
    }

    /// run the import after the confirmation dialog (upstream ImportJob):
    /// merge every chunk of the source world's region dir into the open
    /// world, shifted by the chunk offset.
    pub(crate) fn do_import(&mut self, dir: PathBuf, ox: i32, oz: i32, overwrite: bool, sel_only: bool, cx: &mut Context<Self>) {
        let src_region_dir = dir.join("region");
        let target_sel = if sel_only { Some(self.map.selection.clone()) } else { None };
        let tx = self.map.result_tx.clone();
        import_mca_dir(
            src_region_dir,
            None,
            Point2i::new(ox, oz),
            overwrite,
            target_sel,
            tx,
        );
        cx.notify();
    }

    pub(crate) fn swap_chunks(&mut self, cx: &mut Context<Self>) {
        // swap the two selected chunks (upstream dialog asks for the pair;
        // we swap the first two chunks in selection order)
        let sel = self.map.selection.clone();
        let mut coords: Vec<(i64, usize)> = Vec::new();
        for (rl, mask) in sel.regions() {
            match mask {
                Some(cs) => {
                    for i in cs.iter() {
                        coords.push((rl, i));
                        if coords.len() >= 2 { break; }
                    }
                }
                None => {
                    coords.push((rl, 0));
                    coords.push((rl, 1));
                    break;
                }
            }
            if coords.len() >= 2 { break; }
        }
        if coords.len() < 2 || sel.inverted {
            self.dialog = Some(Dialog::Error { msg: "select exactly two chunks".into() });
            cx.notify();
            return;
        }
        let (r1, i1) = coords[0];
        let (r2, i2) = coords[1];
        // upstream SwapConfirmationDialog names both chunks
        let c1 = Point2i::from_long(r1).region_to_chunk().add_p(Point2i::from_chunk_index(i1 as i32));
        let c2 = Point2i::from_long(r2).region_to_chunk().add_p(Point2i::from_chunk_index(i2 as i32));
        self.dialog = Some(Dialog::Confirm {
            msg: format!(
                "You are about to swap the chunks {}, {} and {}, {}.\nAre you sure?",
                c1.x, c1.z, c2.x, c2.z
            ),
            action: Box::new(move |v, _w, cx| {
                let tx = v.map.result_tx.clone();
                // two regions must be loaded simultaneously — does not fit
                // the one-region-at-a-time runner, so it keeps its own job.
                jobs::pools().add_process(Job::new(
                    jobs::PRIORITY_LOW, None,
                    move || {
                        let _p = Progress::new("Swapping chunks", 2);
                        let load = |rl: i64| -> Option<(std::path::PathBuf, mc_core::mca::McaFile)> {
                            let r = Point2i::from_long(rl);
                            let dir = crate::config::world()?.dirs.region.clone()?;
                            let p = dir.join(mca_file_name(r));
                            let mut m = mc_core::mca::McaFile::new(p.clone()).ok()?;
                            m.load(false).ok()?;
                            Some((p, m))
                        };
                        let mut fail = |m: &str| job_info(&tx, format!("swap failed: {m}"));
                        let (p1, mut m1) = match load(r1) { Some(v) => v, None => return fail("could not load first region") };
                        _p.inc();
                        let (p2, mut m2) = match load(r2) { Some(v) => v, None => return fail("could not load second region") };
                        let c1 = m1.chunks[i1].take();
                        let c2 = m2.chunks[i2].take();
                        m2.chunks[i2] = c1;
                        m1.chunks[i1] = c2;
                        _p.inc();
                        if let Err(e) = m1.save_with_temp_file(&p1) { return fail(&e.to_string()); }
                        if let Err(e) = m2.save_with_temp_file(&p2) { return fail(&e.to_string()); }
                        let _ = tx.send(TileResult::Invalidate {
                            region: Point2i::from_long(r1),
                        });
                        let _ = tx.send(TileResult::Invalidate {
                            region: Point2i::from_long(r2),
                        });
                    }));
                v.dialog = None;
                cx.notify();
            }),
        });
        cx.notify();
    }

    /// upstream ChangeNBTDialog: comma-separated `Type = value` fields with
    /// change/force and selection-only options.
    pub(crate) fn change_fields(&mut self, cx: &mut Context<Self>) {
        self.dialog = Some(Dialog::ChangeFields {
            text: String::new(),
            force: false,
            sel_only: !self.map.selection.is_empty(),
        });
        cx.notify();
    }

    /// OK on the ChangeFields dialog -> ChangeFieldsConfirmationDialog ->
    /// ChangeFieldsJob (mc_core::filter_ops::run_change_fields handles the
    /// per-region poi/entities iteration itself).
    pub(crate) fn run_change_fields_dialog(&mut self, text: String, force: bool, sel_only: bool, cx: &mut Context<Self>) {
        if text.trim().is_empty() {
            self.dialog = Some(Dialog::Error { msg: "empty field list".into() });
            cx.notify();
            return;
        }
        let fields = match mc_core::changer::parse_fields(&text) {
            Ok(f) => f,
            Err(e) => {
                self.dialog = Some(Dialog::Error { msg: e });
                cx.notify();
                return;
            }
        };
        let field_list = fields
            .iter()
            .map(|f| format!("{} = {}", f.field_type.name(), f.value))
            .collect::<Vec<_>>()
            .join(", ");
        let mut sel = self.map.selection.clone();
        if !sel_only || sel.is_empty() {
            sel = Selection::new();
        }
        let sel_opt = if sel_only && !sel.is_empty() { Some(sel) } else { None };
        let tx = self.map.result_tx.clone();
        // upstream ChangeFieldsConfirmationDialog (short header — the count is
        // unknown until the job runs)
        self.dialog = Some(Dialog::Confirm {
            msg: format!(
                "You are about to change data in an unknown number of chunks in this world.\n{field_list}\nAre you sure?"
            ),
            action: Box::new(move |v, _w, cx| {
                let dirs = crate::config::world().map(|w| w.dirs.clone()).unwrap_or_default();
                // upstream only clears the tile cache when a changed field
                // affects rendering (FieldType.requiresClearCache)
                let inv = if mc_core::changer::requires_clear_cache(&fields) {
                    mc_core::batch::list_regions(&dirs, None)
                } else {
                    Vec::new()
                };
                let tx = tx.clone();
                jobs::pools().add_process(Job::new(
                    jobs::PRIORITY_LOW, None,
                    move || {
                        let __p = Progress::new("Changing NBT data", -1);
                        match mc_core::filter_ops::run_change_fields(&fields, force, &dirs, sel_opt.as_ref()) {
                            Ok(r) => job_info(&tx, format!(
                                "changed data in {} chunks ({} files written, {} errors)",
                                r.chunks_matched, r.files_written, r.errors
                            )),
                            Err(e) => job_info(&tx, format!("change fields failed: {e}")),
                        }
                        for region in inv {
                            let _ = tx.send(TileResult::Invalidate { region });
                        }
                    }));
                v.dialog = None;
                cx.notify();
            }),
        });
        cx.notify();
    }

    /// upstream FilterChunksDialog — query field prefilled with the last
    /// query, options mirrored from the dialog's statics.
    pub(crate) fn filter_chunks(&mut self, cx: &mut Context<Self>) {
        self.dialog = Some(Dialog::Filter {
            query: config::global().filter_query,
            action: FilterAction::Select,
            apply: mc_core::filter_ops::ApplyType::Overwrite,
            radius: String::new(),
            sel_only: !self.map.selection.is_empty(),
            focus_radius: false,
        });
        cx.notify();
    }

    /// OK on the filter dialog: select runs SelectFilterJob directly;
    /// delete/export go through the upstream confirmation + dir pick first.
    pub(crate) fn run_filter_dialog(
        &mut self,
        query: String,
        action: FilterAction,
        apply: mc_core::filter_ops::ApplyType,
        radius: String,
        sel_only: bool,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let radius = radius.trim().parse::<i32>().unwrap_or(0).max(0);
        // upstream persists the last query per session; we write it to config
        let mut g = config::global();
        g.filter_query = query.clone();
        config::set_global(g);
        if query.trim().is_empty() {
            // upstream: "filter is empty, won't delete everything"
            self.dialog = Some(Dialog::Error { msg: "empty filter query".into() });
            cx.notify();
            return;
        }
        let expr = match mc_core::filter::parse(&query) {
            Ok(e) => e,
            Err(e) => {
                self.dialog = Some(Dialog::Error { msg: e });
                cx.notify();
                return;
            }
        };
        let sel = self.map.selection.clone();
        let sel_bound = if sel_only && !sel.is_empty() { Some(sel.clone()) } else { None };
        let dirs = crate::config::world().map(|w| w.dirs.clone()).unwrap_or_default();
        match action {
            FilterAction::Select => {
                let mut target = match apply {
                    // upstream clears the selection first for OVERWRITE
                    mc_core::filter_ops::ApplyType::Overwrite => Selection::new(),
                    _ => sel.clone(),
                };
                // upstream's "apply to selection only" bounds the scan for
                // every apply type; OVERWRITE only changes the target.
                let bound = sel_bound;
                let tx = self.map.result_tx.clone();
                jobs::pools().add_process(Job::new(
                    jobs::PRIORITY_LOW, None,
                    move || {
                        let __p = Progress::new("Filtering chunks", -1);
                        match mc_core::filter_ops::run_filter_select(
                            &expr, &dirs, bound.as_ref(), apply, radius, &mut target,
                        ) {
                            Ok(r) => job_info(&tx, format!("{} chunks matched the filter", r.chunks_matched)),
                            Err(e) => job_info(&tx, format!("filter failed: {e}")),
                        }
                        let _ = tx.send(TileResult::FilterResult { selection: target });
                    }));
            }
            FilterAction::Delete => {
                // upstream DeleteConfirmationDialog with a null tile map
                self.dialog = Some(Dialog::Confirm {
                    msg: "You are about to delete an unknown number of chunks from this world.\nAre you sure?".into(),
                    action: Box::new(move |v, _w, cx| {
                        let dirs = crate::config::world().map(|w| w.dirs.clone()).unwrap_or_default();
                        let inv = mc_core::batch::list_regions(&dirs, None);
                        let tx = v.map.result_tx.clone();
                        jobs::pools().add_process(Job::new(
                            jobs::PRIORITY_LOW, None,
                            move || {
                                let __p = Progress::new("Deleting filtered chunks", -1);
                                match mc_core::filter_ops::run_filter_delete(&expr, &dirs, sel_bound.as_ref()) {
                                    Ok(r) => job_info(&tx, format!(
                                        "deleted {} chunks ({} files written)",
                                        r.chunks_matched, r.files_written
                                    )),
                                    Err(e) => job_info(&tx, format!("delete failed: {e}")),
                                }
                                for region in inv {
                                    let _ = tx.send(TileResult::Invalidate { region });
                                }
                            }));
                        // upstream clears the selection after a filtered delete
                        if sel_only {
                            v.map.clear_selection();
                        }
                        v.dialog = None;
                        cx.notify();
                    }),
                });
            }
            FilterAction::Export => {
                let rx = cx.prompt_for_paths(PathPromptOptions {
                    files: false, directories: true, multiple: false,
                    prompt: Some("Select world directory to export to".into()),
                });
                let tx0 = self.map.result_tx.clone();
                cx.spawn(async move |this: WeakEntity<MainView>, cx: &mut AsyncApp| {
                    if let Ok(Ok(Some(paths))) = rx.await {
                        if let Some(dir) = paths.into_iter().next() {
                            let _ = this.update(&mut *cx, |v, cx| {
                                // upstream ExportConfirmationDialog (null tile map)
                                let tx = tx0.clone();
                                v.dialog = Some(Dialog::Confirm {
                                    msg: format!(
                                        "You are about to export an unknown number of chunks from this world.\nto {}\nAre you sure?",
                                        dir.display()
                                    ),
                                    action: Box::new(move |v, _w, cx| {
                                        // upstream FileHelper.createWorldDirectories
                                        let dest = mc_core::mca::WorldDirectories {
                                            region: Some(dir.join("region")),
                                            poi: Some(dir.join("poi")),
                                            entities: Some(dir.join("entities")),
                                        };
                                        for d in [&dest.region, &dest.poi, &dest.entities].into_iter().flatten() {
                                            let _ = std::fs::create_dir_all(d);
                                        }
                                        let tx = tx.clone();
                                        jobs::pools().add_process(Job::new(
                                            jobs::PRIORITY_LOW, None,
                                            move || {
                                                let __p = Progress::new("Exporting filtered chunks", -1);
                                                match mc_core::filter_ops::run_filter_export(
                                                    &expr, &dirs, sel_bound.as_ref(), &dest,
                                                ) {
                                                    Ok(r) => job_info(&tx, format!(
                                                        "exported {} chunks ({} files written)",
                                                        r.chunks_matched, r.files_written
                                                    )),
                                                    Err(e) => job_info(&tx, format!("export failed: {e}")),
                                                }
                                            }));
                                        v.dialog = None;
                                        cx.notify();
                                    }),
                                });
                                cx.notify();
                            });
                        }
                    }
                }).detach();
            }
        }
        cx.notify();
    }
}

/// upstream ImportJob: merge every mca file of `src_dir` into the open
/// world's region dir, shifting chunk positions by `offset` (chunks).
/// `src_sel` limits which source chunks are imported (paste imports pass the
/// copied selection); `target_sel` masks the destination (selection only).
/// Chunks crossing region borders are split by target region.
fn import_mca_dir(
    src_dir: PathBuf,
    src_sel: Option<Selection>,
    offset: Point2i,
    overwrite: bool,
    target_sel: Option<Selection>,
    tx: Sender<TileResult>,
) {
    jobs::pools().add_process(Job::new(
        jobs::PRIORITY_LOW, None,
        move || {
            let rd = match std::fs::read_dir(&src_dir) {
                Ok(r) => r,
                Err(e) => { job_info(&tx, format!("Import failed: {src_dir:?}: {e}")); return; }
            };
            let files: Vec<_> = rd.flatten().collect();
            let mut errs: Vec<String> = Vec::new();
            let __p = Progress::new("Importing chunks", files.len() as i64);
            for e in files {
                let __g = __p.inc_guard();
                if __p.is_cancelled() { break; }
                let name = e.file_name().to_string_lossy().to_string();
                __p.set_current(name.clone());
                let Some(src_region) = mc_core::files::parse_mca_file_name(&name) else { continue };
                let Some(dst_dir) = crate::config::world().and_then(|w| w.dirs.region.clone()) else {
                    errs.push("no world region dir".into());
                    break;
                };
                let Ok(mut src) = mc_core::mca::McaFile::new(e.path()) else { continue };
                if let Err(e) = src.load(false) { errs.push(format!("{name}: {e}")); continue; }
                // chunks to import from this region (Selection::selected_chunks
                // handles inverted selections; None = no selection -> whole file)
                let sel_cs = match &src_sel {
                    None => { ChunkSet::full() }
                    Some(s) => match s.selected_chunks(src_region) {
                        Some(Some(cs)) => cs,
                        Some(None) => { ChunkSet::full() }
                        None => continue,
                    },
                };
                if sel_cs.is_empty() { continue; }
                // split by target region (chunks may cross region borders)
                let mut targets: std::collections::HashMap<Point2i, Vec<(usize, Point2i)>> = std::collections::HashMap::new();
                for i in sel_cs.iter() {
                    let c = src_region.region_to_chunk().add_p(Point2i::from_chunk_index(i as i32)).add_p(offset);
                    if let Some(ts) = &target_sel {
                        if !ts.is_chunk_selected(c) { continue; }
                    }
                    targets.entry(c.chunk_to_region()).or_default().push((i, c));
                }
                for (tregion, chunks) in targets {
                    let dst = dst_dir.join(mca_file_name(tregion));
                    let mut d = if dst.exists() {
                        let Ok(mut m) = mc_core::mca::McaFile::new(dst.clone()) else { continue };
                        if let Err(e) = m.load(false) { errs.push(format!("{}: {e}", dst.display())); continue; }
                        m
                    } else {
                        mc_core::mca::McaFile::empty(tregion, dst.clone())
                    };
                    let trel = tregion.region_to_chunk();
                    let mut changed = false;
                    for (i, c) in chunks {
                        if let Some(chunk) = &src.chunks[i] {
                            let rel = c.sub(trel);
                            if rel.x < 0 || rel.x > 31 || rel.z < 0 || rel.z > 31 { continue; }
                            let di = (rel.z * 32 + rel.x) as usize;
                            if d.chunks[di].is_some() && !overwrite { continue; }
                            let mut cc = chunk.clone();
                            cc.absolute_location = c;
                            cc.timestamp = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|t| t.as_secs() as i32).unwrap_or(0);
                            d.chunks[di] = Some(cc);
                            changed = true;
                        }
                    }
                    if changed {
                        if let Err(e) = d.save_with_temp_file(&dst) {
                            errs.push(format!("{}: {e}", dst.display()));
                        }
                        let _ = tx.send(TileResult::Invalidate { region: tregion });
                    }
                }
            }
            if !errs.is_empty() {
                job_info(&tx, format!("Import finished with {} error(s):\n{}", errs.len(), errs.join("\n")));
            }
        }));
}
