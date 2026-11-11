mod actions;
mod config;
mod dialogs;
mod input;
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
    let mut file_items = vec![
        MenuItem::action("Open World", OpenWorldAction),
        MenuItem::action("Open Dimension", OpenDimensionAction),
        MenuItem::submenu(Menu {
            name: "Open recent World".into(),
            items: if recent.is_empty() {
                vec![MenuItem::action("Open World", OpenWorldAction)]
            } else {
                recent
            },
        }),
        MenuItem::separator(),
        MenuItem::action("Settings", SettingsAction),
        MenuItem::action("Render Settings", RenderSettingsAction),
        MenuItem::separator(),
        MenuItem::action("Quit", QuitAction),
    ];
    let _ = &mut file_items;
    vec![
        Menu {
            name: "File".into(),
            items: file_items,
        },
        Menu {
            name: "View".into(),
            items: vec![
                MenuItem::action("Reload", ReloadAction),
                MenuItem::separator(),
                MenuItem::action(format!("{}Chunk Grid", on(g.show_chunk_grid)), ChunkGridAction),
                MenuItem::action(format!("{}Region Grid", on(g.show_region_grid)), RegionGridAction),
                MenuItem::action(format!("{}Coordinates", on(g.show_coordinates)), CoordinatesAction),
                MenuItem::submenu(Menu {
                    name: "Coordinate Style".into(),
                    items: vec![
                        MenuItem::action(format!("{}Dynamic", on(style == "DYNAMIC")), CoordStyleDynamicAction),
                        MenuItem::action(format!("{}Edge", on(style == "EDGE")), CoordStyleEdgeAction),
                        MenuItem::action(format!("{}Grid", on(style == "GRID")), CoordStyleGridAction),
                    ],
                }),
                MenuItem::action(format!("{}Structures", on(g.show_structure_icons)), StructureIconsAction),
                MenuItem::submenu(Menu {
                    name: "Select Structures".into(),
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
                MenuItem::action("Goto", GotoAction),
                MenuItem::action("Reset zoom", ResetZoomAction),
                MenuItem::separator(),
                MenuItem::action("Save screenshot", SaveScreenshotAction),
                MenuItem::separator(),
                MenuItem::action("Clear cache", ClearViewCacheAction),
                MenuItem::action("Clear all cache", ClearAllCacheAction),
            ],
        },
        Menu {
            name: "Selection".into(),
            items: vec![
                MenuItem::action("Clear", ClearSelectionAction),
                MenuItem::action("Invert", InvertSelectionAction),
                MenuItem::action("Invert selected regions", InvertRegionsAction),
                MenuItem::separator(),
                MenuItem::action("Copy chunks", CopyChunksAction),
                MenuItem::action("Paste chunks", PasteChunksAction),
                MenuItem::separator(),
                MenuItem::action("Defragment selected regions", DefragmentAction),
                MenuItem::action("Export selected chunks", ExportChunksAction),
                MenuItem::action("Delete selected chunks", DeleteChunksAction),
                MenuItem::separator(),
                MenuItem::action("Import selection from .csv", ImportSelectionAction),
                MenuItem::action("Export selection as .csv", ExportSelectionAction),
                MenuItem::separator(),
                MenuItem::action("Export as image", ExportImageAction),
                MenuItem::separator(),
                MenuItem::action("Clear cache", ClearSelectionCacheAction),
            ],
        },
        Menu {
            name: "Tools".into(),
            items: vec![
                MenuItem::action("Import chunks", ImportChunksAction),
                MenuItem::action("Filter chunks", FilterChunksAction),
                MenuItem::action("Change fields", ChangeFieldsAction),
                MenuItem::action("Edit chunk", EditNBTAction),
                MenuItem::separator(),
                MenuItem::action("Swap chunks", SwapChunksAction),
                MenuItem::separator(),
                MenuItem::action("Edit overlays", EditOverlaysAction),
                MenuItem::action("Next overlay", NextOverlayAction),
                MenuItem::action("Next overlay type", NextOverlayTypeAction),
                MenuItem::action("Sum selection", SumSelectionAction),
            ],
        },
        Menu {
            name: "About".into(),
            items: vec![MenuItem::action("About MCA Selector", AboutAction)],
        },
    ]
}

fn bindings() -> Vec<KeyBinding> {
    vec![
        KeyBinding::new("ctrl-q", QuitAction, None),
        KeyBinding::new("ctrl-o", OpenWorldAction, None),
        KeyBinding::new("ctrl-comma", SettingsAction, None),
        KeyBinding::new("e", RenderSettingsAction, None),
        KeyBinding::new("f5", ReloadAction, None),
        KeyBinding::new("ctrl-t", ChunkGridAction, None),
        KeyBinding::new("ctrl-r", RegionGridAction, None),
        KeyBinding::new("ctrl-z", CoordinatesAction, None),
        KeyBinding::new("ctrl-y", StructureIconsAction, None),
        KeyBinding::new("ctrl-g", GotoAction, None),
        KeyBinding::new("ctrl-0", ResetZoomAction, None),
        KeyBinding::new("ctrl-p", SaveScreenshotAction, None),
        KeyBinding::new("ctrl-shift-k", ClearAllCacheAction, None),
        KeyBinding::new("ctrl-k", ClearViewCacheAction, None),
        KeyBinding::new("ctrl-l", ClearSelectionAction, None),
        KeyBinding::new("ctrl-u", InvertSelectionAction, None),
        KeyBinding::new("ctrl-shift-u", InvertRegionsAction, None),
        KeyBinding::new("ctrl-c", CopyChunksAction, None),
        KeyBinding::new("ctrl-v", PasteChunksAction, None),
        KeyBinding::new("ctrl-shift-e", ExportChunksAction, None),
        KeyBinding::new("ctrl-shift-i", ImportChunksAction, None),
        KeyBinding::new("ctrl-d", DeleteChunksAction, None),
        KeyBinding::new("ctrl-i", ImportSelectionAction, None),
        KeyBinding::new("ctrl-e", ExportSelectionAction, None),
        KeyBinding::new("ctrl-j", ClearSelectionCacheAction, None),
        KeyBinding::new("ctrl-f", FilterChunksAction, None),
        KeyBinding::new("ctrl-n", ChangeFieldsAction, None),
        KeyBinding::new("ctrl-b", EditNBTAction, None),
        KeyBinding::new("ctrl-m", SwapChunksAction, None),
        KeyBinding::new("ctrl-h", EditOverlaysAction, None),
        KeyBinding::new("o", NextOverlayAction, None),
        KeyBinding::new("n", NextOverlayTypeAction, None),
    ]
}

fn main() {
    env_logger::init();
    config::set_global(config::GlobalConfig::load());
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

        // open world from argv if given
        if let Some(arg) = std::env::args().nth(1) {
            let p = std::path::PathBuf::from(arg);
            if p.is_dir() {
                if let Some(w) = cx.windows().first().cloned() {
                    let _ = w.update(cx, |root, _w, cx| {
                        if let Ok(v) = root.downcast::<view::MainView>() {
                            v.update(cx, |v, cx| v.open_world_at(p.clone(), cx));
                        }
                    });
                }
            }
        }
        cx.activate(true);
    });
}
