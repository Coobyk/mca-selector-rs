mod actions;
mod config;
mod dialogs;
mod input;
mod lang;
mod menubar;
mod nbt_editor;
mod ops;
mod paint;
mod slider;
mod statusbar;
mod tilemap;
mod view;
mod world;

use actions::*;
use gpui::*;

fn menus(cx: &App) -> Vec<Menu> {
    let _ = cx;
    // the vendored gpui MenuItem (patches/gpui/src/platform/app_menu.rs)
    // has no checked/disabled state — checkmarks are faked with a "✓ "
    // name prefix and selection-dependent items can't be disabled, so
    // upstream's item disabling is skipped. The view rebuilds menus via
    // cx.set_menus(menus(cx)) after every toggle that changes these.
    let g = config::global();
    let style = g.coordinate_style.to_ascii_uppercase();
    let on = |b: bool| if b { "✓ " } else { "  " };
    let recent: Vec<MenuItem> = g
        .recent_worlds
        .values()
        .rev()
        .map(|r| {
            MenuItem::action(
                r.world.display().to_string(),
                OpenRecentWorld { path: r.world.to_string_lossy().to_string() },
            )
        })
        .collect();
    // dimension flyout — same interaction as Open recent World: rows are
    // the dimension dir names (upstream FileNameListCell style).
    let dims: Vec<MenuItem> = config::world()
        .map(|w| {
            w.dimensions
                .iter()
                .map(|d| {
                    let label = d
                        .file_name()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_else(|| d.display().to_string());
                    MenuItem::action(
                        label,
                        OpenDim { path: d.to_string_lossy().into_owned() },
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    let dim_item = if dims.len() > 1 {
        MenuItem::submenu(Menu {
            name: lang::t("menu.file.open_dimension").into(),
            items: dims,
        })
    } else {
        MenuItem::action(lang::t("menu.file.open_dimension"), OpenDimensionAction)
    };
    use lang::t;
    let mut file_items = vec![
        MenuItem::action(t("menu.file.open_world"), OpenWorldAction),
        dim_item,
        MenuItem::submenu(Menu {
            name: t("menu.file.open_recent").into(),
            items: if recent.is_empty() {
                vec![MenuItem::action(t("menu.file.open_world"), OpenWorldAction)]
            } else {
                recent
            },
        }),
        MenuItem::separator(),
        MenuItem::action(t("menu.file.settings"), SettingsAction),
        MenuItem::action(t("menu.file.render_settings"), RenderSettingsAction),
        MenuItem::separator(),
        MenuItem::action(t("menu.file.quit"), QuitAction),
    ];
    let _ = &mut file_items;
    vec![
        Menu {
            name: t("menu.file").into(),
            items: file_items,
        },
        Menu {
            name: t("menu.view").into(),
            items: vec![
                MenuItem::action(t("menu.view.reload"), ReloadAction),
                MenuItem::separator(),
                MenuItem::action(format!("{}{}", on(g.show_chunk_grid), t("menu.view.chunk_grid")), ChunkGridAction),
                MenuItem::action(format!("{}{}", on(g.show_region_grid), t("menu.view.region_grid")), RegionGridAction),
                MenuItem::action(format!("{}{}", on(g.show_coordinates), t("menu.view.coordinates")), CoordinatesAction),
                MenuItem::submenu(Menu {
                    name: t("menu.view.coord_style").into(),
                    items: vec![
                        MenuItem::action(format!("{}{}", on(style == "DYNAMIC"), t("menu.view.coord_dynamic")), CoordStyleDynamicAction),
                        MenuItem::action(format!("{}{}", on(style == "EDGE"), t("menu.view.coord_edge")), CoordStyleEdgeAction),
                        MenuItem::action(format!("{}{}", on(style == "GRID"), t("menu.view.coord_grid")), CoordStyleGridAction),
                    ],
                }),
                MenuItem::action(format!("{}{}", on(g.show_structure_icons), t("menu.view.structures")), StructureIconsAction),
                MenuItem::submenu(Menu {
                    name: t("menu.view.select_structures").into(),
                    items: mc_core::structures::structures()
                        .display_names
                        .iter()
                        .map(|(display, id)| MenuItem::action(
                            format!("{}{}", on(g.structure_icons.get(id).copied().unwrap_or(true)), display),
                            ToggleStructureIcon { id: id.clone() },
                        ))
                        .collect(),
                }),
                MenuItem::separator(),
                MenuItem::action(t("menu.view.goto"), GotoAction),
                MenuItem::action(t("menu.view.find_structure"), FindStructureAction),
                MenuItem::action(t("menu.view.reset_zoom"), ResetZoomAction),
                MenuItem::separator(),
                MenuItem::action(t("menu.view.save_screenshot"), SaveScreenshotAction),
                MenuItem::separator(),
                MenuItem::action(t("menu.view.clear_cache"), ClearViewCacheAction),
                MenuItem::action(t("menu.view.clear_all_cache"), ClearAllCacheAction),
            ],
        },
        Menu {
            name: t("menu.selection").into(),
            items: vec![
                MenuItem::action(t("menu.selection.clear"), ClearSelectionAction),
                MenuItem::action(t("menu.selection.invert"), InvertSelectionAction),
                MenuItem::action(t("menu.selection.invert_regions"), InvertRegionsAction),
                MenuItem::separator(),
                MenuItem::action(t("menu.selection.copy_chunks"), CopyChunksAction),
                MenuItem::action(t("menu.selection.paste_chunks"), PasteChunksAction),
                MenuItem::separator(),
                MenuItem::action(t("menu.selection.defragment"), DefragmentAction),
                MenuItem::action(t("menu.selection.export_chunks"), ExportChunksAction),
                MenuItem::action(t("menu.selection.delete_chunks"), DeleteChunksAction),
                MenuItem::separator(),
                MenuItem::action(t("menu.selection.import_selection"), ImportSelectionAction),
                MenuItem::action(t("menu.selection.export_selection"), ExportSelectionAction),
                MenuItem::separator(),
                MenuItem::action(t("menu.selection.export_image"), ExportImageAction),
                MenuItem::separator(),
                MenuItem::action(t("menu.selection.clear_cache"), ClearSelectionCacheAction),
            ],
        },
        Menu {
            name: t("menu.tools").into(),
            items: vec![
                MenuItem::action(t("menu.tools.restore_backup"), RestoreBackupAction),
                MenuItem::separator(),
                MenuItem::action(t("menu.tools.import_chunks"), ImportChunksAction),
                MenuItem::action(t("menu.tools.filter_chunks"), FilterChunksAction),
                MenuItem::action(t("menu.tools.change_fields"), ChangeFieldsAction),
                MenuItem::action(t("menu.tools.relight"), RelightAction),
                MenuItem::action(t("menu.tools.edit_chunk"), EditNBTAction),
                MenuItem::separator(),
                MenuItem::action(t("menu.tools.swap_chunks"), SwapChunksAction),
                MenuItem::separator(),
                MenuItem::action(t("menu.tools.edit_overlays"), EditOverlaysAction),
                MenuItem::action(t("menu.tools.next_overlay"), NextOverlayAction),
                MenuItem::action(t("menu.tools.next_overlay_type"), NextOverlayTypeAction),
                MenuItem::action(t("menu.tools.sum_selection"), SumSelectionAction),
            ],
        },
        Menu {
            name: t("menu.about").into(),
            items: vec![MenuItem::action(t("menu.about.about"), AboutAction)],
        },
    ]
}

fn bindings() -> Vec<KeyBinding> {
    vec![
        KeyBinding::new("ctrl-q", QuitAction, Some("app")),
        KeyBinding::new("ctrl-o", OpenWorldAction, Some("app")),
        KeyBinding::new("ctrl-comma", SettingsAction, Some("app")),
        KeyBinding::new("e", RenderSettingsAction, Some("app")),
        KeyBinding::new("f5", ReloadAction, Some("app")),
        KeyBinding::new("ctrl-t", ChunkGridAction, Some("app")),
        KeyBinding::new("ctrl-r", RegionGridAction, Some("app")),
        KeyBinding::new("ctrl-z", CoordinatesAction, Some("app")),
        KeyBinding::new("ctrl-y", StructureIconsAction, Some("app")),
        KeyBinding::new("ctrl-g", GotoAction, Some("app")),
        KeyBinding::new("ctrl-shift-g", FindStructureAction, Some("app")),
        KeyBinding::new("ctrl-0", ResetZoomAction, Some("app")),
        KeyBinding::new("ctrl-p", SaveScreenshotAction, Some("app")),
        KeyBinding::new("ctrl-shift-k", ClearAllCacheAction, Some("app")),
        KeyBinding::new("ctrl-k", ClearViewCacheAction, Some("app")),
        KeyBinding::new("ctrl-l", ClearSelectionAction, Some("app")),
        KeyBinding::new("ctrl-u", InvertSelectionAction, Some("app")),
        KeyBinding::new("ctrl-shift-u", InvertRegionsAction, Some("app")),
        KeyBinding::new("ctrl-c", CopyChunksAction, Some("app")),
        KeyBinding::new("ctrl-v", PasteChunksAction, Some("app")),
        KeyBinding::new("ctrl-shift-e", ExportChunksAction, Some("app")),
        KeyBinding::new("ctrl-shift-i", ImportChunksAction, Some("app")),
        KeyBinding::new("ctrl-d", DeleteChunksAction, Some("app")),
        KeyBinding::new("ctrl-i", ImportSelectionAction, Some("app")),
        KeyBinding::new("ctrl-e", ExportSelectionAction, Some("app")),
        KeyBinding::new("ctrl-j", ClearSelectionCacheAction, Some("app")),
        KeyBinding::new("ctrl-f", FilterChunksAction, Some("app")),
        KeyBinding::new("ctrl-n", ChangeFieldsAction, Some("app")),
        KeyBinding::new("ctrl-b", EditNBTAction, Some("app")),
        KeyBinding::new("ctrl-m", SwapChunksAction, Some("app")),
        KeyBinding::new("ctrl-h", EditOverlaysAction, Some("app")),
        KeyBinding::new("o", NextOverlayAction, Some("app")),
        KeyBinding::new("n", NextOverlayTypeAction, Some("app")),
    ]
}

fn main() {
    env_logger::init();
    config::set_global(config::GlobalConfig::load());
    lang::init(&config::global().language);
    mc_core::jobs::set_process_threads(config::global().process_threads as usize);
    Application::new().run(|cx: &mut App| {
        cx.set_menus(menus(cx));
        cx.bind_keys(bindings());

        let bounds = Bounds::centered(None, size(px(800.), px(600.)), cx);
        let opts = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                appears_transparent: false,
                title: None,
                traffic_light_position: None,
            }),
            ..Default::default()
        };
        cx.open_window(opts, |w, cx| {
            let v = cx.new(view::MainView::new);
            w.set_window_title("MCA Selector");
            v
        })
        .expect("open window");

        // open world from argv if given — deferred to the next frame:
        // notify() during initial window setup can be dropped before the
        // first frame is ever scheduled, leaving the world/dialog unpainted.
        if let Some(arg) = std::env::args().nth(1) {
            let p = std::path::PathBuf::from(arg);
            if p.is_dir() {
                if let Some(w) = cx.windows().first().cloned() {
                    let _ = w.update(cx, |root, window, cx| {
                        if let Ok(v) = root.downcast::<view::MainView>() {
                            let p = p.clone();
                            let _ = v.update(cx, |_v, cx| {
                                cx.on_next_frame(window, move |v, _w, cx| {
                                    v.open_world_at(p.clone(), cx)
                                });
                            });
                        }
                    });
                }
            }
        }
        cx.activate(true);
    });
}
