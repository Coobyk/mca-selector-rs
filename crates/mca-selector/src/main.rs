//! mca-selector-rs: a Minecraft region/chunk map editor built with GPUI.

mod map_state;
mod workspace;

use gpui::{App, Application, Bounds, WindowBounds, WindowOptions, px, size};

fn main() {
    let world_path = std::env::args().nth(1).unwrap_or_else(|| {
        eprintln!("usage: mca-selector <world-folder>");
        std::process::exit(2);
    });

    Application::new().run(move |cx: &mut App| {
        let world = match mc_core::world::World::open(&world_path) {
            Ok(w) => w,
            Err(e) => {
                eprintln!("failed to open world {world_path:?}: {e}");
                std::process::exit(1);
            }
        };
        let bounds = Bounds::centered(None, size(px(1400.), px(900.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(gpui::TitlebarOptions {
                    title: Some(format!("mca-selector-rs — {}", world.root.display()).into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            |_window, cx| workspace::Workspace::open_world(world, cx),
        )
        .unwrap();
        cx.activate(true);
    });
}
