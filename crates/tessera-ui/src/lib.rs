mod frame_image;
mod media_bin;
mod playhead;
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

pub const WORKSPACE_CONTEXT: &str = "Workspace";

actions!(
    tessera,
    [
        Quit,
        Import,
        PlayPause,
        ShuttleBackward,
        Pause,
        ShuttleForward,
        StepBackward,
        StepForward,
        ZoomIn,
        ZoomOut,
        ZoomToFit,
        SplitAtPlayhead,
        DeleteClip,
        RippleDeleteClip
    ]
);

pub fn init(cx: &mut App) {
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.bind_keys([
        KeyBinding::new("ctrl-q", Quit, None),
        KeyBinding::new("ctrl-i", Import, None),
        KeyBinding::new("space", PlayPause, Some(WORKSPACE_CONTEXT)),
        KeyBinding::new("j", ShuttleBackward, Some(WORKSPACE_CONTEXT)),
        KeyBinding::new("k", Pause, Some(WORKSPACE_CONTEXT)),
        KeyBinding::new("l", ShuttleForward, Some(WORKSPACE_CONTEXT)),
        KeyBinding::new("left", StepBackward, Some(WORKSPACE_CONTEXT)),
        KeyBinding::new("right", StepForward, Some(WORKSPACE_CONTEXT)),
        KeyBinding::new("=", ZoomIn, Some(WORKSPACE_CONTEXT)),
        KeyBinding::new("-", ZoomOut, Some(WORKSPACE_CONTEXT)),
        KeyBinding::new("shift-z", ZoomToFit, Some(WORKSPACE_CONTEXT)),
        KeyBinding::new("ctrl-k", SplitAtPlayhead, Some(WORKSPACE_CONTEXT)),
        KeyBinding::new("delete", DeleteClip, Some(WORKSPACE_CONTEXT)),
        KeyBinding::new("backspace", DeleteClip, Some(WORKSPACE_CONTEXT)),
        KeyBinding::new("shift-delete", RippleDeleteClip, Some(WORKSPACE_CONTEXT)),
        KeyBinding::new("shift-backspace", RippleDeleteClip, Some(WORKSPACE_CONTEXT)),
    ]);
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
    cx.open_window(options, |window, cx| {
        let project = cx.new(|_| project);
        cx.new(|cx| Workspace::new(project, window, cx))
    })
}
