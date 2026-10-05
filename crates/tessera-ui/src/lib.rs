mod autosave;
mod editor;
mod frame_image;
mod media_bin;
mod playhead;
mod recent;
mod sequence_dialog;
mod text_field;
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

const DIALOG_CONTEXT: &str = "Dialog";

const SHORTCUT_CONTEXT: &str = "Workspace && !TextField && !Dialog";

pub const NEW_PROJECT_NAME: &str = "Untitled";

const AUTOSAVE_DIRECTORY: &str = "autosave";

actions!(
    tessera,
    [
        Quit,
        NewProject,
        Save,
        SaveAs,
        Open,
        OpenRecent,
        Import,
        FocusSearch,
        OpenSequenceSettings,
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
        UnlinkClips,
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
        ToggleSafeAreas,
        Undo,
        Redo,
        SelectNext,
        SelectPrevious,
        Confirm,
        Dismiss
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
        KeyBinding::new("ctrl-shift-o", OpenRecent, None),
        KeyBinding::new("ctrl-i", Import, None),
        KeyBinding::new("ctrl-f", FocusSearch, Some(SHORTCUT_CONTEXT)),
        KeyBinding::new("ctrl-,", OpenSequenceSettings, Some(SHORTCUT_CONTEXT)),
        KeyBinding::new("space", PlayPause, Some(SHORTCUT_CONTEXT)),
        KeyBinding::new("j", ShuttleBackward, Some(SHORTCUT_CONTEXT)),
        KeyBinding::new("k", Pause, Some(SHORTCUT_CONTEXT)),
        KeyBinding::new("l", ShuttleForward, Some(SHORTCUT_CONTEXT)),
        KeyBinding::new("left", StepBackward, Some(SHORTCUT_CONTEXT)),
        KeyBinding::new("right", StepForward, Some(SHORTCUT_CONTEXT)),
        KeyBinding::new("=", ZoomIn, Some(SHORTCUT_CONTEXT)),
        KeyBinding::new("-", ZoomOut, Some(SHORTCUT_CONTEXT)),
        KeyBinding::new("shift-z", ZoomToFit, Some(SHORTCUT_CONTEXT)),
        KeyBinding::new("ctrl-k", SplitAtPlayhead, Some(SHORTCUT_CONTEXT)),
        KeyBinding::new("ctrl-l", UnlinkClips, Some(SHORTCUT_CONTEXT)),
        KeyBinding::new("ctrl-a", SelectAll, Some(SHORTCUT_CONTEXT)),
        KeyBinding::new("ctrl-c", CopyClips, Some(SHORTCUT_CONTEXT)),
        KeyBinding::new("ctrl-x", CutClips, Some(SHORTCUT_CONTEXT)),
        KeyBinding::new("ctrl-v", PasteClips, Some(SHORTCUT_CONTEXT)),
        KeyBinding::new("ctrl-d", DuplicateClips, Some(SHORTCUT_CONTEXT)),
        KeyBinding::new("m", AddMarker, Some(SHORTCUT_CONTEXT)),
        KeyBinding::new("shift-m", RemoveMarker, Some(SHORTCUT_CONTEXT)),
        KeyBinding::new("i", SetInPoint, Some(SHORTCUT_CONTEXT)),
        KeyBinding::new("o", SetOutPoint, Some(SHORTCUT_CONTEXT)),
        KeyBinding::new("alt-x", ClearInOut, Some(SHORTCUT_CONTEXT)),
        KeyBinding::new("down", NextEdit, Some(SHORTCUT_CONTEXT)),
        KeyBinding::new("up", PreviousEdit, Some(SHORTCUT_CONTEXT)),
        KeyBinding::new("ctrl-right", NextMarker, Some(SHORTCUT_CONTEXT)),
        KeyBinding::new("ctrl-left", PreviousMarker, Some(SHORTCUT_CONTEXT)),
        KeyBinding::new("escape", Cancel, Some(SHORTCUT_CONTEXT)),
        KeyBinding::new("delete", DeleteClip, Some(SHORTCUT_CONTEXT)),
        KeyBinding::new("backspace", DeleteClip, Some(SHORTCUT_CONTEXT)),
        KeyBinding::new("shift-delete", RippleDeleteClip, Some(SHORTCUT_CONTEXT)),
        KeyBinding::new("shift-backspace", RippleDeleteClip, Some(SHORTCUT_CONTEXT)),
        KeyBinding::new("n", ToggleSnapping, Some(SHORTCUT_CONTEXT)),
        KeyBinding::new("'", ToggleSafeAreas, Some(SHORTCUT_CONTEXT)),
        KeyBinding::new("ctrl-z", Undo, Some(SHORTCUT_CONTEXT)),
        KeyBinding::new("ctrl-shift-z", Redo, Some(SHORTCUT_CONTEXT)),
        KeyBinding::new("ctrl-y", Redo, Some(SHORTCUT_CONTEXT)),
        KeyBinding::new("down", SelectNext, Some(DIALOG_CONTEXT)),
        KeyBinding::new("up", SelectPrevious, Some(DIALOG_CONTEXT)),
        KeyBinding::new("enter", Confirm, Some(DIALOG_CONTEXT)),
        KeyBinding::new("escape", Dismiss, Some(DIALOG_CONTEXT)),
    ]);
}

pub fn use_state_directory(cx: &mut App) {
    let Some(state) = state_directory() else {
        tracing::warn!("no state directory, recent projects and autosave stay off");
        return;
    };
    cx.set_global(recent::RecentProjects::stored_in(
        state.join(recent::RECENT_FILE),
    ));
    cx.set_global(autosave::AutosaveDirectory(state.join(AUTOSAVE_DIRECTORY)));
}

fn state_directory() -> Option<std::path::PathBuf> {
    let state = std::env::var_os("XDG_STATE_HOME")
        .map(std::path::PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| std::env::home_dir().map(|home| home.join(".local").join("state")))?;
    Some(state.join(APP_ID))
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
        cx.new(|cx| {
            let mut workspace = Workspace::new(project, window, cx);
            workspace.offer_recovery(window, cx);
            workspace
        })
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
