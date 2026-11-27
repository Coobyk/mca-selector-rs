#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

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

/// One row per user action: menu label key, action type, shortcut
/// (`""` = menu only). `menus()` and `bindings()` both expand this table,
/// so a menu item, its label and its shortcut can never drift apart.
/// Handlers stay matched to actions by the compiler via `.on_action`.
macro_rules! action_table {
    ($emit:ident) => {
        $emit! {
            "menu.file.quit"              => QuitAction, "ctrl-q";
            "menu.file.open_world"        => OpenWorldAction, "ctrl-o";
            "menu.file.settings"          => SettingsAction, "ctrl-comma";
            "menu.file.render_settings"   => RenderSettingsAction, "e";
            "menu.file.open_dimension"    => OpenDimensionAction, "";
            "menu.view.reload"            => ReloadAction, "f5";
            "menu.view.chunk_grid"        => ChunkGridAction, "ctrl-t";
            "menu.view.region_grid"       => RegionGridAction, "ctrl-r";
            "menu.view.coordinates"       => CoordinatesAction, "ctrl-z";
            "menu.view.coord_dynamic"     => CoordStyleDynamicAction, "";
            "menu.view.coord_edge"        => CoordStyleEdgeAction, "";
            "menu.view.coord_grid"        => CoordStyleGridAction, "";
            "menu.view.structures"        => StructureIconsAction, "ctrl-y";
            "menu.view.goto"              => GotoAction, "ctrl-g";
            "menu.view.find_structure"    => FindStructureAction, "ctrl-shift-g";
            "menu.view.reset_zoom"        => ResetZoomAction, "ctrl-0";
            "menu.view.save_screenshot"   => SaveScreenshotAction, "ctrl-p";
            "menu.view.clear_cache"       => ClearViewCacheAction, "ctrl-k";
            "menu.view.clear_all_cache"   => ClearAllCacheAction, "ctrl-shift-k";
            "menu.selection.clear"        => ClearSelectionAction, "ctrl-l";
            "menu.selection.invert"       => InvertSelectionAction, "ctrl-u";
            "menu.selection.invert_regions" => InvertRegionsAction, "ctrl-shift-u";
            "menu.selection.copy_chunks"  => CopyChunksAction, "ctrl-c";
            "menu.selection.paste_chunks" => PasteChunksAction, "ctrl-v";
            "menu.selection.defragment"   => DefragmentAction, "";
            "menu.selection.export_chunks" => ExportChunksAction, "ctrl-shift-e";
            "menu.selection.delete_chunks" => DeleteChunksAction, "ctrl-d";
            "menu.selection.import_selection" => ImportSelectionAction, "ctrl-i";
            "menu.selection.export_selection" => ExportSelectionAction, "ctrl-e";
            "menu.selection.export_image" => ExportImageAction, "";
            "menu.selection.clear_cache"  => ClearSelectionCacheAction, "ctrl-j";
            "menu.tools.restore_backup"   => RestoreBackupAction, "";
            "menu.tools.import_chunks"    => ImportChunksAction, "ctrl-shift-i";
            "menu.tools.filter_chunks"    => FilterChunksAction, "ctrl-f";
            "menu.tools.change_fields"    => ChangeFieldsAction, "ctrl-n";
            "menu.tools.relight"          => RelightAction, "";
            "menu.tools.edit_chunk"       => EditNBTAction, "ctrl-b";
            "menu.tools.swap_chunks"      => SwapChunksAction, "ctrl-m";
            "menu.tools.edit_overlays"    => EditOverlaysAction, "ctrl-h";
            "menu.tools.next_overlay"     => NextOverlayAction, "o";
            "menu.tools.next_overlay_type" => NextOverlayTypeAction, "n";
            "menu.tools.sum_selection"    => SumSelectionAction, "";
            "menu.about.about"            => AboutAction, "";
        }
    };
}

fn named_item(key: &'static str, name: String) -> MenuItem {
    macro_rules! items {
        ($($key:literal => $act:ident, $keys:literal;)*) => {
            match key {
                $($key => MenuItem::action(name.clone(), $act),)*
                _ => unreachable!("menu item '{key}' missing from action_table"),
            }
        }
    }
    action_table!(items)
}

fn action_item(key: &'static str) -> MenuItem {
    let name = lang::t(key).to_string();
    named_item(key, name)
}

/// Menu item with a fake checkmark prefix — the vendored MenuItem has no
/// checked state.
fn checked(key: &'static str, on: bool) -> MenuItem {
    named_item(key, format!("{}{}", if on { "✓ " } else { "  " }, lang::t(key)))
}

fn bindings() -> Vec<KeyBinding> {
    macro_rules! binds {
        ($($key:literal => $act:ident, $keys:literal;)*) => {
            [$( (!$keys.is_empty()).then(|| KeyBinding::new($keys, $act, Some("app"))) ),*]
                .into_iter()
                .flatten()
                .collect()
        }
    }
    action_table!(binds)
}

fn menus(cx: &App) -> Vec<Menu> {
    let _ = cx;
    let g = config::global();
    let style = g.coordinate_style.to_ascii_uppercase();
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
        action_item("menu.file.open_dimension")
    };
    use lang::t;
    vec![
        Menu {
            name: t("menu.file").into(),
            items: vec![
                action_item("menu.file.open_world"),
                dim_item,
                MenuItem::submenu(Menu {
                    name: t("menu.file.open_recent").into(),
                    items: if recent.is_empty() {
                        vec![action_item("menu.file.open_world")]
                    } else {
                        recent
                    },
                }),
                MenuItem::separator(),
                action_item("menu.file.settings"),
                action_item("menu.file.render_settings"),
                MenuItem::separator(),
                action_item("menu.file.quit"),
            ],
        },
        Menu {
            name: t("menu.view").into(),
            items: vec![
                action_item("menu.view.reload"),
                MenuItem::separator(),
                checked("menu.view.chunk_grid", g.show_chunk_grid),
                checked("menu.view.region_grid", g.show_region_grid),
                checked("menu.view.coordinates", g.show_coordinates),
                MenuItem::submenu(Menu {
                    name: t("menu.view.coord_style").into(),
                    items: vec![
                        checked("menu.view.coord_dynamic", style == "DYNAMIC"),
                        checked("menu.view.coord_edge", style == "EDGE"),
                        checked("menu.view.coord_grid", style == "GRID"),
                    ],
                }),
                checked("menu.view.structures", g.show_structure_icons),
                MenuItem::submenu(Menu {
                    name: t("menu.view.select_structures").into(),
                    items: mc_core::structures::structures()
                        .display_names
                        .iter()
                        .map(|(display, id)| MenuItem::action(
                            format!("{}{}", if g.structure_icons.get(id).copied().unwrap_or(true) { "✓ " } else { "  " }, display),
                            ToggleStructureIcon { id: id.clone() },
                        ))
                        .collect(),
                }),
                MenuItem::separator(),
                action_item("menu.view.goto"),
                action_item("menu.view.find_structure"),
                action_item("menu.view.reset_zoom"),
                MenuItem::separator(),
                action_item("menu.view.save_screenshot"),
                MenuItem::separator(),
                action_item("menu.view.clear_cache"),
                action_item("menu.view.clear_all_cache"),
            ],
        },
        Menu {
            name: t("menu.selection").into(),
            items: vec![
                action_item("menu.selection.clear"),
                action_item("menu.selection.invert"),
                action_item("menu.selection.invert_regions"),
                MenuItem::separator(),
                action_item("menu.selection.copy_chunks"),
                action_item("menu.selection.paste_chunks"),
                MenuItem::separator(),
                action_item("menu.selection.defragment"),
                action_item("menu.selection.export_chunks"),
                action_item("menu.selection.delete_chunks"),
                MenuItem::separator(),
                action_item("menu.selection.import_selection"),
                action_item("menu.selection.export_selection"),
                MenuItem::separator(),
                action_item("menu.selection.export_image"),
                MenuItem::separator(),
                action_item("menu.selection.clear_cache"),
            ],
        },
        Menu {
            name: t("menu.tools").into(),
            items: vec![
                action_item("menu.tools.restore_backup"),
                MenuItem::separator(),
                action_item("menu.tools.import_chunks"),
                action_item("menu.tools.filter_chunks"),
                action_item("menu.tools.change_fields"),
                action_item("menu.tools.relight"),
                action_item("menu.tools.edit_chunk"),
                MenuItem::separator(),
                action_item("menu.tools.swap_chunks"),
                MenuItem::separator(),
                action_item("menu.tools.edit_overlays"),
                action_item("menu.tools.next_overlay"),
                action_item("menu.tools.next_overlay_type"),
                action_item("menu.tools.sum_selection"),
            ],
        },
        Menu {
            name: t("menu.about").into(),
            items: vec![action_item("menu.about.about")],
        },
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
