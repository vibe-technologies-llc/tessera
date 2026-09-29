use std::{ffi::OsStr, path::PathBuf};

use futures::{FutureExt, channel::oneshot, future::Shared};
use gpui::{
    App, AppContext, Context, Entity, FocusHandle, InteractiveElement, IntoElement, ParentElement,
    PathPromptOptions, PromptLevel, Render, Styled, Task, Window, div, px,
};
use tessera_document::EXTENSION;
use tessera_timeline::{Project, Time};

use crate::{
    DeleteClip, Import, Open, Pause, PlayPause, Redo, RippleDeleteClip, Save, ShuttleBackward,
    ShuttleForward, SplitAtPlayhead, StepBackward, StepForward, ToggleSnapping, Undo,
    WORKSPACE_CONTEXT, ZoomIn, ZoomOut, ZoomToFit,
    editor::ProjectEditor,
    media_bin::{MediaBin, file_name},
    playhead::Playhead,
    theme,
    timeline::TimelinePanel,
    viewer::Viewer,
    window_title,
};

const UNSAVED_MARK: &str = "• ";

#[derive(Clone, Copy)]
enum UnsavedChanges {
    Save,
    Discard,
    Cancel,
}

impl UnsavedChanges {
    const CHOICES: [Self; 3] = [Self::Save, Self::Discard, Self::Cancel];

    fn label(self) -> &'static str {
        match self {
            Self::Save => "Save",
            Self::Discard => "Don't Save",
            Self::Cancel => "Cancel",
        }
    }
}

pub struct Workspace {
    editor: ProjectEditor,
    focus_handle: FocusHandle,
    playhead: Entity<Playhead>,
    media_bin: Entity<MediaBin>,
    viewer: Entity<Viewer>,
    timeline: Entity<TimelinePanel>,
    file: Option<PathBuf>,
    file_io: Shared<Task<()>>,
    asking_to_discard: bool,
}

impl Workspace {
    pub fn new(project: Entity<Project>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle);
        let playhead = cx.new(|cx| Playhead::new(project.clone(), cx));
        let editor = ProjectEditor::new(project.clone(), cx);
        cx.observe_in(editor.history(), window, |workspace, _, window, cx| {
            window.set_window_title(&workspace.title(cx));
        })
        .detach();
        let this = cx.entity().downgrade();
        window.on_window_should_close(cx, move |window, cx| {
            this.update(cx, |workspace, cx| workspace.should_close(window, cx))
                .unwrap_or(true)
        });
        Self {
            focus_handle,
            media_bin: cx.new(|cx| MediaBin::new(editor.clone(), cx)),
            viewer: cx.new(|cx| Viewer::new(project, playhead.clone(), cx)),
            timeline: cx.new(|cx| TimelinePanel::new(editor.clone(), playhead.clone(), cx)),
            playhead,
            editor,
            file: None,
            file_io: Task::ready(()).shared(),
            asking_to_discard: false,
        }
    }

    pub fn project(&self) -> &Entity<Project> {
        self.editor.project()
    }

    pub fn confirm_discard(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Task<bool> {
        if self.editor.history().read(cx).is_saved() {
            return Task::ready(true);
        }
        if self.asking_to_discard {
            return Task::ready(false);
        }
        self.asking_to_discard = true;
        window.activate_window();
        let answer = window.prompt(
            PromptLevel::Warning,
            &format!("Save the changes to “{}”?", self.name(cx)),
            Some("Your changes will be lost if you don't save them."),
            &UnsavedChanges::CHOICES.map(UnsavedChanges::label),
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            let choice = answer
                .await
                .ok()
                .and_then(|index| UnsavedChanges::CHOICES.get(index).copied())
                .unwrap_or(UnsavedChanges::Cancel);
            let proceeding = this.update_in(cx, |workspace, window, cx| {
                workspace.asking_to_discard = false;
                match choice {
                    UnsavedChanges::Save => workspace.save(window, cx),
                    UnsavedChanges::Discard => Task::ready(true),
                    UnsavedChanges::Cancel => Task::ready(false),
                }
            });
            match proceeding {
                Ok(proceeding) => proceeding.await,
                Err(_) => false,
            }
        })
    }

    fn should_close(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.editor.history().read(cx).is_saved() {
            return true;
        }
        let confirming = self.confirm_discard(window, cx);
        cx.spawn_in(window, async move |_, cx| {
            if confirming.await {
                cx.update(|window, _| window.remove_window()).ok();
            }
        })
        .detach();
        false
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Task<bool> {
        match self.file.clone() {
            Some(path) => self.save_to(path, window, cx),
            None => self.prompt_save(window, cx),
        }
    }

    fn prompt_save(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Task<bool> {
        let suggested_name = format!("{}.{EXTENSION}", self.project().read(cx).name);
        let chosen = cx.prompt_for_new_path(&default_directory(), Some(&suggested_name));
        cx.spawn_in(window, async move |this, cx| {
            let Ok(chosen) = chosen.await else {
                return false;
            };
            let saving = this.update_in(cx, |workspace, window, cx| match chosen {
                Ok(Some(path)) => workspace.save_to(with_project_extension(path), window, cx),
                Ok(None) => Task::ready(false),
                Err(error) => {
                    report_failure(
                        "Could not show the save dialog",
                        &format!("{error:#}"),
                        window,
                        cx,
                    );
                    Task::ready(false)
                }
            });
            match saving {
                Ok(saving) => saving.await,
                Err(_) => false,
            }
        })
    }

    pub fn save_to(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<bool> {
        let project = self.project().read(cx).clone();
        let revision = self.editor.revision(cx);
        let saving = self.queue_file_io(cx, {
            let path = path.clone();
            move || tessera_document::save(&project, &path)
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(saved) = saving.await else {
                return false;
            };
            this.update_in(cx, |workspace, window, cx| match saved {
                Ok(()) => {
                    tracing::info!(path = %path.display(), "saved the project");
                    workspace.editor.mark_saved(revision, cx);
                    workspace.set_file(path, window, cx);
                    true
                }
                Err(error) => {
                    report_failure("Could not save the project", &error.to_string(), window, cx);
                    false
                }
            })
            .unwrap_or(false)
        })
    }

    fn prompt_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Open".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(chosen) = chosen.await else {
                return;
            };
            this.update_in(cx, |workspace, window, cx| match chosen {
                Ok(Some(paths)) => {
                    if let Some(path) = paths.into_iter().next() {
                        workspace.open_from(path, window, cx);
                    }
                }
                Ok(None) => {}
                Err(error) => report_failure(
                    "Could not show the open dialog",
                    &format!("{error:#}"),
                    window,
                    cx,
                ),
            })
            .ok();
        })
        .detach();
    }

    pub fn open_from(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let opening = self.queue_file_io(cx, {
            let path = path.clone();
            move || tessera_document::open(&path)
        });
        cx.spawn_in(window, async move |this, cx| {
            let project = match opening.await {
                Ok(Ok(project)) => project,
                Ok(Err(error)) => {
                    this.update_in(cx, |_, window, cx| {
                        report_failure("Could not open the project", &error.to_string(), window, cx)
                    })
                    .ok();
                    return;
                }
                Err(oneshot::Canceled) => return,
            };
            let Ok(confirming) = this.update_in(cx, |workspace, window, cx| {
                workspace.confirm_discard(window, cx)
            }) else {
                return;
            };
            if !confirming.await {
                return;
            }
            this.update_in(cx, |workspace, window, cx| {
                tracing::info!(path = %path.display(), "opened the project");
                workspace.replace_project(project, cx);
                workspace.set_file(path, window, cx);
            })
            .ok();
        })
        .detach();
    }

    fn queue_file_io<R: Send + 'static>(
        &mut self,
        cx: &mut Context<Self>,
        job: impl FnOnce() -> R + Send + 'static,
    ) -> oneshot::Receiver<R> {
        let (sender, receiver) = oneshot::channel();
        let previous = self.file_io.clone();
        self.file_io = cx
            .background_spawn(async move {
                previous.await;
                sender.send(job()).ok();
            })
            .shared();
        receiver
    }

    fn replace_project(&mut self, project: Project, cx: &mut Context<Self>) {
        self.playhead
            .update(cx, |playhead, cx| playhead.seek(Time::ZERO, cx));
        self.editor.replace(project, cx);
        self.timeline.update(cx, TimelinePanel::project_replaced);
        self.media_bin.update(cx, MediaBin::project_replaced);
    }

    fn set_file(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.file = Some(path);
        window.set_window_title(&self.title(cx));
    }

    fn name(&self, cx: &App) -> String {
        match &self.file {
            Some(path) => file_name(path).to_string(),
            None => self.project().read(cx).name.clone(),
        }
    }

    fn title(&self, cx: &App) -> String {
        let name = self.name(cx);
        if self.editor.history().read(cx).is_saved() {
            window_title(&name)
        } else {
            window_title(&format!("{UNSAVED_MARK}{name}"))
        }
    }

    fn transport(
        &mut self,
        cx: &mut Context<Self>,
        control: impl FnOnce(&mut Playhead, &mut Context<Playhead>),
    ) {
        self.playhead.update(cx, control);
    }

    fn on_timeline(
        &mut self,
        cx: &mut Context<Self>,
        command: impl FnOnce(&mut TimelinePanel, &mut Context<TimelinePanel>),
    ) {
        self.timeline.update(cx, command);
    }
}

impl Render for Workspace {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .track_focus(&self.focus_handle)
            .key_context(WORKSPACE_CONTEXT)
            .on_action(cx.listener(|workspace, _: &Save, window, cx| {
                workspace.save(window, cx).detach();
            }))
            .on_action(cx.listener(|workspace, _: &Open, window, cx| {
                workspace.prompt_open(window, cx);
            }))
            .on_action(cx.listener(|workspace, _: &Import, _, cx| {
                workspace
                    .media_bin
                    .update(cx, |media_bin, cx| media_bin.prompt_import(cx));
            }))
            .on_action(cx.listener(|workspace, _: &PlayPause, _, cx| {
                workspace.transport(cx, Playhead::toggle_play);
            }))
            .on_action(cx.listener(|workspace, _: &ShuttleBackward, _, cx| {
                workspace.transport(cx, Playhead::shuttle_backward);
            }))
            .on_action(cx.listener(|workspace, _: &Pause, _, cx| {
                workspace.transport(cx, Playhead::pause);
            }))
            .on_action(cx.listener(|workspace, _: &ShuttleForward, _, cx| {
                workspace.transport(cx, Playhead::shuttle_forward);
            }))
            .on_action(cx.listener(|workspace, _: &StepBackward, _, cx| {
                workspace.transport(cx, |playhead, cx| playhead.step(-1, cx));
            }))
            .on_action(cx.listener(|workspace, _: &StepForward, _, cx| {
                workspace.transport(cx, |playhead, cx| playhead.step(1, cx));
            }))
            .on_action(cx.listener(|workspace, _: &ZoomIn, _, cx| {
                workspace.on_timeline(cx, TimelinePanel::zoom_in);
            }))
            .on_action(cx.listener(|workspace, _: &ZoomOut, _, cx| {
                workspace.on_timeline(cx, TimelinePanel::zoom_out);
            }))
            .on_action(cx.listener(|workspace, _: &ZoomToFit, _, cx| {
                workspace.on_timeline(cx, TimelinePanel::zoom_to_fit);
            }))
            .on_action(cx.listener(|workspace, _: &SplitAtPlayhead, _, cx| {
                workspace.on_timeline(cx, TimelinePanel::split_at_playhead);
            }))
            .on_action(cx.listener(|workspace, _: &DeleteClip, _, cx| {
                workspace.on_timeline(cx, TimelinePanel::delete_selection);
            }))
            .on_action(cx.listener(|workspace, _: &RippleDeleteClip, _, cx| {
                workspace.on_timeline(cx, TimelinePanel::ripple_delete_selection);
            }))
            .on_action(cx.listener(|workspace, _: &ToggleSnapping, _, cx| {
                workspace.on_timeline(cx, TimelinePanel::toggle_snapping);
            }))
            .on_action(cx.listener(|workspace, _: &Undo, _, cx| workspace.editor.undo(cx)))
            .on_action(cx.listener(|workspace, _: &Redo, _, cx| workspace.editor.redo(cx)))
            .size_full()
            .flex()
            .flex_col()
            .bg(theme::background())
            .text_color(theme::text())
            .text_sm()
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .child(
                        div()
                            .w(px(280.))
                            .flex_none()
                            .border_r_1()
                            .border_color(theme::border())
                            .child(self.media_bin.clone()),
                    )
                    .child(div().flex_1().min_w_0().child(self.viewer.clone())),
            )
            .child(
                div()
                    .h(px(260.))
                    .flex_none()
                    .border_t_1()
                    .border_color(theme::border())
                    .child(self.timeline.clone()),
            )
    }
}

fn report_failure(message: &str, detail: &str, window: &mut Window, cx: &mut App) {
    tracing::error!(detail, "{message}");
    drop(window.prompt(PromptLevel::Critical, message, Some(detail), &["OK"], cx));
}

fn default_directory() -> PathBuf {
    std::env::home_dir()
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_default()
}

fn with_project_extension(path: PathBuf) -> PathBuf {
    if path.extension() == Some(OsStr::new(EXTENSION)) {
        return path;
    }
    let mut path = path.into_os_string();
    path.push(format!(".{EXTENSION}"));
    path.into()
}

#[cfg(test)]
mod tests {
    use gpui::{Modifiers, TestAppContext, VisualTestContext, point};
    use tessera_timeline::{MediaInfo, Stream, Time, VideoStream};

    use super::*;
    use crate::playhead::Speed;

    const TIMELINE_HEIGHT: f32 = 260.;
    const V1_BELOW_TIMELINE_TOP: f32 = 1. + 24. + 24.;
    const LANES_LEFT: f32 = 96.;
    const ONE_SECOND: f32 = 48.;

    fn starts(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> Vec<Time> {
        cx.read(|cx| {
            workspace.read(cx).project().read(cx).timeline.tracks[0]
                .clips()
                .iter()
                .map(|clip| clip.start)
                .collect()
        })
    }

    fn click_v1(cx: &mut VisualTestContext, seconds: f32) {
        let height = cx.update(|window, _| window.viewport_size().height);
        let y = height - px(TIMELINE_HEIGHT) + px(V1_BELOW_TIMELINE_TOP);
        let x = px(LANES_LEFT + seconds * ONE_SECOND);
        cx.simulate_click(point(x, y), Modifiers::none());
    }

    fn sample_project(name: &str, media: &str) -> Project {
        let mut project = Project::new(name);
        let info = MediaInfo {
            duration: Some(Time::from_seconds(8)),
            streams: vec![Stream::Video(VideoStream {
                index: 0,
                codec: "h264".into(),
                width: 1920,
                height: 1080,
                frame_rate: None,
            })],
        };
        let asset = project.add_asset(media.into(), info);
        for seconds in [1, 10] {
            project
                .place_clip(asset, 0, Time::from_seconds(seconds))
                .unwrap();
        }
        project
    }

    #[gpui::test]
    fn editing_keys_reach_the_timeline(cx: &mut TestAppContext) {
        cx.update(crate::init);
        let project = cx.new(|_| sample_project("test", "a.mkv"));
        let (workspace, cx) = cx.add_window_view(|window, cx| Workspace::new(project, window, cx));
        let playhead = cx.read(|cx| workspace.read(cx).playhead.clone());
        playhead.update(cx, |playhead, cx| {
            playhead.seek(Time::from_seconds(4), cx);
        });
        let seconds = |values: &[i64]| -> Vec<Time> {
            values.iter().copied().map(Time::from_seconds).collect()
        };

        cx.simulate_keystrokes("ctrl-k");
        assert_eq!(starts(&workspace, cx), seconds(&[1, 4, 10]));

        click_v1(cx, 5.);
        cx.simulate_keystrokes("delete");
        assert_eq!(starts(&workspace, cx), seconds(&[1, 10]));

        click_v1(cx, 2.);
        cx.simulate_keystrokes("shift-backspace");
        assert_eq!(starts(&workspace, cx), seconds(&[7]));

        cx.simulate_keystrokes("ctrl-z");
        assert_eq!(starts(&workspace, cx), seconds(&[1, 10]));
        cx.simulate_keystrokes("ctrl-z");
        assert_eq!(starts(&workspace, cx), seconds(&[1, 4, 10]));
        cx.simulate_keystrokes("ctrl-shift-z");
        assert_eq!(starts(&workspace, cx), seconds(&[1, 10]));
        cx.simulate_keystrokes("ctrl-y");
        assert_eq!(starts(&workspace, cx), seconds(&[7]));
    }

    struct ScratchDir(PathBuf);

    impl ScratchDir {
        fn new(name: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("tessera-ui-{}-{name}", std::process::id()));
            std::fs::remove_dir_all(&path).ok();
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for ScratchDir {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }

    fn current(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> Project {
        cx.read(|cx| workspace.read(cx).project().read(cx).clone())
    }

    #[gpui::test]
    fn saving_then_opening_replaces_the_project_in_place(cx: &mut TestAppContext) {
        cx.update(crate::init);
        let dir = ScratchDir::new("save-open");
        let path = dir.0.join("cut.tessera");
        let saved = sample_project("Cut", "/media/cut.mkv");
        let project = cx.new(|_| saved.clone());
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::new(project.clone(), window, cx));

        workspace.update_in(cx, |workspace, window, cx| {
            workspace.save_to(path.clone(), window, cx).detach();
        });
        cx.run_until_parked();
        assert_eq!(cx.window_title().as_deref(), Some("cut.tessera — Tessera"));
        assert_eq!(tessera_document::open(&path).unwrap(), saved);

        let other = sample_project("Other", "/media/other.mkv");
        workspace.update(cx, |workspace, cx| {
            workspace.replace_project(other.clone(), cx);
        });
        let playhead = cx.read(|cx| workspace.read(cx).playhead.clone());
        playhead.update(cx, |playhead, cx| playhead.seek(Time::from_seconds(4), cx));
        cx.simulate_keystrokes("ctrl-k");
        click_v1(cx, 2.);
        cx.simulate_keystrokes("l");
        assert_eq!(starts(&workspace, cx), [1, 4, 10].map(Time::from_seconds));

        workspace.update_in(cx, |workspace, window, cx| {
            workspace.open_from(path.clone(), window, cx);
        });
        cx.run_until_parked();
        cx.simulate_prompt_answer("Don't Save");
        cx.run_until_parked();
        assert_eq!(current(&workspace, cx), saved);
        assert_eq!(cx.read(|cx| workspace.read(cx).project().clone()), project);
        let time_and_speed = |cx: &mut VisualTestContext| {
            cx.read(|cx| {
                let playhead = playhead.read(cx);
                (playhead.time(), playhead.speed())
            })
        };
        assert_eq!(time_and_speed(cx), (Time::ZERO, Speed::PAUSED));
        cx.simulate_keystrokes("ctrl-z");
        assert_eq!(current(&workspace, cx), saved);
        cx.simulate_keystrokes("delete");
        assert_eq!(current(&workspace, cx), saved);

        std::fs::remove_file(&path).unwrap();
        cx.simulate_keystrokes("ctrl-s");
        cx.run_until_parked();
        assert_eq!(tessera_document::open(&path).unwrap(), saved);
    }

    #[gpui::test]
    fn saves_and_opens_run_in_the_order_they_were_asked_for(cx: &mut TestAppContext) {
        cx.update(crate::init);
        let dir = ScratchDir::new("file-io-order");
        let path = dir.0.join("cut.tessera");
        let project = cx.new(|_| sample_project("Cut", "/media/cut.mkv"));
        let (workspace, cx) = cx.add_window_view(|window, cx| Workspace::new(project, window, cx));
        let save = |cx: &mut VisualTestContext| {
            workspace.update_in(cx, |workspace, window, cx| {
                workspace.save_to(path.clone(), window, cx).detach();
            });
        };

        save(cx);
        split_at(&workspace, 4, cx);
        let edited = current(&workspace, cx);
        save(cx);
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.open_from(path.clone(), window, cx);
        });
        cx.run_until_parked();

        assert!(!cx.has_pending_prompt());
        assert_eq!(tessera_document::open(&path).unwrap(), edited);
        assert_eq!(current(&workspace, cx), edited);
        assert_eq!(cx.window_title().as_deref(), Some("cut.tessera — Tessera"));
    }

    #[gpui::test]
    fn the_title_marks_unsaved_changes_until_undo_or_redo_returns_to_the_save(
        cx: &mut TestAppContext,
    ) {
        cx.update(crate::init);
        let dir = ScratchDir::new("saved-revision");
        let path = dir.0.join("cut.tessera");
        let project = cx.new(|_| sample_project("Cut", "/media/cut.mkv"));
        let (workspace, cx) = cx.add_window_view(|window, cx| Workspace::new(project, window, cx));
        let playhead = cx.read(|cx| workspace.read(cx).playhead.clone());

        playhead.update(cx, |playhead, cx| playhead.seek(Time::from_seconds(4), cx));
        cx.simulate_keystrokes("ctrl-k");

        assert_eq!(cx.window_title().as_deref(), Some("• Cut — Tessera"));

        workspace.update_in(cx, |workspace, window, cx| {
            workspace.save_to(path.clone(), window, cx).detach();
        });
        cx.run_until_parked();

        assert_eq!(cx.window_title().as_deref(), Some("cut.tessera — Tessera"));

        cx.simulate_keystrokes("ctrl-z");

        assert_eq!(
            cx.window_title().as_deref(),
            Some("• cut.tessera — Tessera")
        );

        cx.simulate_keystrokes("ctrl-shift-z");

        assert_eq!(cx.window_title().as_deref(), Some("cut.tessera — Tessera"));
    }

    #[gpui::test]
    fn a_file_that_fails_to_open_is_reported_and_changes_nothing(cx: &mut TestAppContext) {
        cx.update(crate::init);
        let dir = ScratchDir::new("open-failure");
        let missing = dir.0.join("missing.tessera");
        let kept = sample_project("Kept", "/media/kept.mkv");
        let project = cx.new(|_| kept.clone());
        let (workspace, cx) = cx.add_window_view(|window, cx| Workspace::new(project, window, cx));
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.open_from(missing.clone(), window, cx);
        });
        cx.run_until_parked();
        let (message, detail) = cx.pending_prompt().unwrap();
        assert_eq!(message, "Could not open the project");
        assert!(detail.contains("missing.tessera"), "{detail}");
        assert_eq!(current(&workspace, cx), kept);
        assert_eq!(cx.read(|cx| workspace.read(cx).file.clone()), None);
    }

    fn split_at(workspace: &Entity<Workspace>, seconds: i64, cx: &mut VisualTestContext) {
        let playhead = cx.read(|cx| workspace.read(cx).playhead.clone());
        playhead.update(cx, |playhead, cx| {
            playhead.seek(Time::from_seconds(seconds), cx)
        });
        cx.simulate_keystrokes("ctrl-k");
    }

    #[gpui::test]
    fn closing_asks_before_discarding_unsaved_changes(cx: &mut TestAppContext) {
        cx.update(crate::init);
        let project = cx.new(|_| sample_project("Cut", "/media/cut.mkv"));
        let (workspace, cx) = cx.add_window_view(|window, cx| Workspace::new(project, window, cx));

        split_at(&workspace, 4, cx);

        assert!(!cx.simulate_close());
        assert_eq!(
            cx.pending_prompt().map(|(message, _)| message).as_deref(),
            Some("Save the changes to “Cut”?")
        );

        cx.simulate_prompt_answer("Cancel");
        cx.run_until_parked();

        assert_eq!(cx.windows().len(), 1);
        assert_eq!(starts(&workspace, cx), [1, 4, 10].map(Time::from_seconds));

        assert!(!cx.simulate_close());
        cx.simulate_prompt_answer("Don't Save");
        cx.run_until_parked();

        assert!(cx.windows().is_empty());
    }

    #[gpui::test]
    fn closing_a_saved_project_asks_nothing(cx: &mut TestAppContext) {
        cx.update(crate::init);
        let project = cx.new(|_| sample_project("Cut", "/media/cut.mkv"));
        let (_workspace, cx) = cx.add_window_view(|window, cx| Workspace::new(project, window, cx));

        assert!(cx.simulate_close());
        assert!(!cx.has_pending_prompt());
    }

    #[gpui::test]
    fn opening_asks_before_replacing_unsaved_changes(cx: &mut TestAppContext) {
        cx.update(crate::init);
        let dir = ScratchDir::new("open-unsaved");
        let current_path = dir.0.join("cut.tessera");
        let other_path = dir.0.join("other.tessera");
        let other = sample_project("Other", "/media/other.mkv");
        tessera_document::save(&other, &other_path).unwrap();
        let project = cx.new(|_| sample_project("Cut", "/media/cut.mkv"));
        let (workspace, cx) = cx.add_window_view(|window, cx| Workspace::new(project, window, cx));

        workspace.update_in(cx, |workspace, window, cx| {
            workspace.save_to(current_path.clone(), window, cx).detach();
        });
        cx.run_until_parked();
        split_at(&workspace, 4, cx);
        let edited = current(&workspace, cx);
        let open_other = |cx: &mut VisualTestContext| {
            workspace.update_in(cx, |workspace, window, cx| {
                workspace.open_from(other_path.clone(), window, cx);
            });
            cx.run_until_parked();
        };

        open_other(cx);

        assert_eq!(
            cx.pending_prompt().map(|(message, _)| message).as_deref(),
            Some("Save the changes to “cut.tessera”?")
        );

        cx.simulate_prompt_answer("Cancel");
        cx.run_until_parked();

        assert_eq!(current(&workspace, cx), edited);
        assert_eq!(
            cx.window_title().as_deref(),
            Some("• cut.tessera — Tessera")
        );

        open_other(cx);
        cx.simulate_prompt_answer("Save");
        cx.run_until_parked();

        assert_eq!(tessera_document::open(&current_path).unwrap(), edited);
        assert_eq!(current(&workspace, cx), other);
        assert_eq!(
            cx.window_title().as_deref(),
            Some("other.tessera — Tessera")
        );
    }

    #[gpui::test]
    fn quitting_can_save_an_untitled_project_first(cx: &mut TestAppContext) {
        cx.update(crate::init);
        let dir = ScratchDir::new("quit-save");
        let path = dir.0.join("cut");
        let project = cx.new(|_| sample_project("Cut", "/media/cut.mkv"));
        let (workspace, cx) = cx.add_window_view(|window, cx| Workspace::new(project, window, cx));

        split_at(&workspace, 4, cx);
        let edited = current(&workspace, cx);
        cx.simulate_keystrokes("ctrl-q");
        cx.run_until_parked();

        assert_eq!(
            cx.pending_prompt().map(|(message, _)| message).as_deref(),
            Some("Save the changes to “Cut”?")
        );

        cx.simulate_prompt_answer("Save");
        cx.run_until_parked();
        cx.simulate_new_path_selection(|_| Some(path.clone()));
        cx.run_until_parked();

        assert_eq!(
            tessera_document::open(&dir.0.join("cut.tessera")).unwrap(),
            edited
        );
        assert_eq!(cx.window_title().as_deref(), Some("cut.tessera — Tessera"));
    }

    #[test]
    fn saved_paths_gain_the_project_extension() {
        assert_eq!(
            with_project_extension("/work/cut".into()),
            PathBuf::from("/work/cut.tessera")
        );
        assert_eq!(
            with_project_extension("/work/cut.v2".into()),
            PathBuf::from("/work/cut.v2.tessera")
        );
        assert_eq!(
            with_project_extension("/work/cut.tessera".into()),
            PathBuf::from("/work/cut.tessera")
        );
    }
}
