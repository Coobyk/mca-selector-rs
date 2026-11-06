//! mca-selector-rs: a Minecraft region/chunk map editor built with GPUI.

mod map_state;
mod workspace;

use gpui::{App, Application, Bounds, WindowBounds, WindowOptions, px, size};

fn main() {
    env_logger::init();
    // Optional world path; without one the app starts on the landing
    // screen and asks for a folder.
    let world_arg = std::env::args().nth(1);

    Application::new().run(move |cx: &mut App| {
        let (world, open_error) = match &world_arg {
            Some(path) => match mc_core::world::World::open(path) {
                Ok(w) => (Some(w), None),
                Err(e) => (
                    None,
                    Some(format!("Could not open {path}: {e}")),
                ),
            },
            None => (None, None),
        };
        let title = match &world {
            Some(w) => format!("mca-selector-rs - {}", w.root.display()),
            None => "mca-selector-rs".to_string(),
        };
        let bounds = Bounds::centered(None, size(px(1400.), px(900.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(gpui::TitlebarOptions {
                    title: Some(title.into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            |_window, cx| workspace::Workspace::new(world, open_error, cx),
        )
        .unwrap();
        cx.activate(true);
    });
}
