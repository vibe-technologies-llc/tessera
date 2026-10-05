mod editor;
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

pub const NEW_PROJECT_NAME: &str = "Untitled";

actions!(
    tessera,
    [
        Quit,
        NewProject,
        Save,
        SaveAs,
        Open,
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
        SelectAll,
        CopyClips,
        CutClips,
        PasteClips,
        DuplicateClips,
        AddMarker,
        RemoveMarker,
        SetInPoint,
        SetOutPoint,
        ClearInOut,
        NextEdit,
        PreviousEdit,
        NextMarker,
        PreviousMarker,
        Cancel,
        DeleteClip,
        RippleDeleteClip,
        ToggleSnapping,
        Undo,
        Redo
    ]
);

pub fn init(cx: &mut App) {
    cx.on_action(quit);
    cx.bind_keys([
        KeyBinding::new("ctrl-q", Quit, None),
        KeyBinding::new("ctrl-n", NewProject, None),
        KeyBinding::new("ctrl-s", Save, None),
        KeyBinding::new("ctrl-shift-s", SaveAs, None),
        KeyBinding::new("ctrl-o", Open, None),
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
        KeyBinding::new("ctrl-a", SelectAll, Some(WORKSPACE_CONTEXT)),
        KeyBinding::new("ctrl-c", CopyClips, Some(WORKSPACE_CONTEXT)),
        KeyBinding::new("ctrl-x", CutClips, Some(WORKSPACE_CONTEXT)),
        KeyBinding::new("ctrl-v", PasteClips, Some(WORKSPACE_CONTEXT)),
        KeyBinding::new("ctrl-d", DuplicateClips, Some(WORKSPACE_CONTEXT)),
        KeyBinding::new("m", AddMarker, Some(WORKSPACE_CONTEXT)),
        KeyBinding::new("shift-m", RemoveMarker, Some(WORKSPACE_CONTEXT)),
        KeyBinding::new("i", SetInPoint, Some(WORKSPACE_CONTEXT)),
        KeyBinding::new("o", SetOutPoint, Some(WORKSPACE_CONTEXT)),
        KeyBinding::new("alt-x", ClearInOut, Some(WORKSPACE_CONTEXT)),
        KeyBinding::new("down", NextEdit, Some(WORKSPACE_CONTEXT)),
        KeyBinding::new("up", PreviousEdit, Some(WORKSPACE_CONTEXT)),
        KeyBinding::new("ctrl-right", NextMarker, Some(WORKSPACE_CONTEXT)),
        KeyBinding::new("ctrl-left", PreviousMarker, Some(WORKSPACE_CONTEXT)),
        KeyBinding::new("escape", Cancel, Some(WORKSPACE_CONTEXT)),
        KeyBinding::new("delete", DeleteClip, Some(WORKSPACE_CONTEXT)),
        KeyBinding::new("backspace", DeleteClip, Some(WORKSPACE_CONTEXT)),
        KeyBinding::new("shift-delete", RippleDeleteClip, Some(WORKSPACE_CONTEXT)),
        KeyBinding::new("shift-backspace", RippleDeleteClip, Some(WORKSPACE_CONTEXT)),
        KeyBinding::new("n", ToggleSnapping, Some(WORKSPACE_CONTEXT)),
        KeyBinding::new("ctrl-z", Undo, Some(WORKSPACE_CONTEXT)),
        KeyBinding::new("ctrl-shift-z", Redo, Some(WORKSPACE_CONTEXT)),
        KeyBinding::new("ctrl-y", Redo, Some(WORKSPACE_CONTEXT)),
    ]);
}

pub fn open_main_window(project: Project, cx: &mut App) -> gpui::Result<WindowHandle<Workspace>> {
    let bounds = Bounds::centered(None, size(px(1600.), px(900.)), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(TitlebarOptions {
            title: Some(window_title(&project.name).into()),
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

fn quit(_: &Quit, cx: &mut App) {
    let workspaces: Vec<WindowHandle<Workspace>> = cx
        .windows()
        .into_iter()
        .filter_map(|window| window.downcast())
        .collect();
    cx.spawn(async move |cx| {
        for workspace in workspaces {
            let Ok(confirming) = workspace.update(cx, |workspace, window, cx| {
                workspace.confirm_discard(window, cx)
            }) else {
                continue;
            };
            if !confirming.await {
                return;
            }
        }
        cx.update(|cx| cx.quit()).ok();
    })
    .detach();
}

fn window_title(name: &str) -> String {
    format!("{name} — Tessera")
}
