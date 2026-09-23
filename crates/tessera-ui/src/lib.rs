mod media_bin;
mod theme;
mod timeline;
mod viewer;
mod workspace;

use gpui::{
    App, AppContext, Bounds, KeyBinding, TitlebarOptions, WindowBounds, WindowHandle,
    WindowOptions, actions, px, size,
};
use tessera_timeline::Project;
pub use workspace::Workspace;

pub const APP_ID: &str = "tessera";

actions!(tessera, [Quit]);

pub fn init(cx: &mut App) {
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.bind_keys([KeyBinding::new("ctrl-q", Quit, None)]);
}

pub fn open_main_window(project: Project, cx: &mut App) -> gpui::Result<WindowHandle<Workspace>> {
    let bounds = Bounds::centered(None, size(px(1600.), px(900.)), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(TitlebarOptions {
            title: Some(format!("{} — Tessera", project.name).into()),
            ..Default::default()
        }),
        app_id: Some(APP_ID.to_owned()),
        window_min_size: Some(size(px(960.), px(540.))),
        ..Default::default()
    };
    cx.open_window(options, |_, cx| {
        let project = cx.new(|_| project);
        cx.new(|cx| Workspace::new(project, cx))
    })
}
