use std::{ffi::OsStr, path::PathBuf};

use futures::{FutureExt, channel::oneshot, future::Shared};
use gpui::{
    App, AppContext, Context, Entity, FocusHandle, InteractiveElement, IntoElement, ParentElement,
    PathPromptOptions, PromptLevel, Render, Styled, Subscription, Task, Window, div, px,
};
use tessera_document::EXTENSION;
use tessera_timeline::{Command, Project, Revision, Time};

use crate::{
    AddMarker, Cancel, ClearInOut, CopyClips, CutClips, CycleAudioStream, DeleteClip,
    DuplicateClips, FocusSearch, Import, LowerClipGain, NEW_PROJECT_NAME, NewProject, NextEdit,
    NextMarker, Open, OpenRecent, OpenSequenceSettings, PasteClips, Pause, PlayPause, PreviousEdit,
    PreviousMarker, RaiseClipGain, Redo, RemoveMarker, RippleDeleteClip, Save, SaveAs, SelectAll,
    SetInPoint, SetOutPoint, ShuttleBackward, ShuttleForward, SplitAtPlayhead, StepBackward,
    StepForward, ToggleClipLink, ToggleSafeAreas, ToggleSnapping, Undo, WORKSPACE_CONTEXT, ZoomIn,
    ZoomOut, ZoomToFit,
    autosave::{AUTOSAVE_INTERVAL, AutosaveDirectory, Orphan, Slot, orphans},
    editor::ProjectEditor,
    media_bin::{MediaBin, file_name},
    playhead::Playhead,
    recent::{self, RecentDialogEvent, RecentProjects, RecentProjectsDialog},
    sequence_dialog::{SequenceDialogEvent, SequenceSettingsDialog},
    theme,
    timeline::TimelinePanel,
    viewer::Viewer,
    window_title,
};

const UNSAVED_MARK: &str = "• ";
const RECOVER: &str = "Recover";
const DISCARD_RECOVERY: &str = "Discard";
const RECOVERY_CHOICES: [&str; 3] = [RECOVER, DISCARD_RECOVERY, "Not Now"];

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
    dialog: Option<(Dialog, Subscription)>,
    autosave: Option<Slot>,
    autosaved: Option<Revision>,
    _autosave_timer: Option<Task<()>>,
}

enum Dialog {
    SequenceSettings(Entity<SequenceSettingsDialog>),
    RecentProjects(Entity<RecentProjectsDialog>),
}

impl Dialog {
    fn view(&self) -> gpui::AnyView {
        match self {
            Self::SequenceSettings(dialog) => dialog.clone().into(),
            Self::RecentProjects(dialog) => dialog.clone().into(),
        }
    }
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
        let autosave = cx
            .try_global::<AutosaveDirectory>()
            .map(|directory| Slot::claim(&directory.0));
        let autosave_timer = autosave.is_some().then(|| {
            cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(AUTOSAVE_INTERVAL).await;
                    if this.update(cx, Workspace::autosave).is_err() {
                        break;
                    }
                }
            })
        });
        let timeline_focus_return = focus_handle.clone();
        let bin_focus_return = focus_handle.clone();
        Self {
            focus_handle,
            media_bin: cx.new(|cx| {
                let mut bin = MediaBin::new(editor.clone(), cx);
                bin.return_focus_to(bin_focus_return);
                bin
            }),
            viewer: cx.new(|cx| Viewer::new(project, playhead.clone(), window, cx)),
            timeline: cx.new(|cx| {
                let mut timeline = TimelinePanel::new(editor.clone(), playhead.clone(), cx);
                timeline.return_focus_to(timeline_focus_return);
                timeline
            }),
            playhead,
            editor,
            file: None,
            file_io: Task::ready(()).shared(),
            asking_to_discard: false,
            dialog: None,
            autosave,
            autosaved: None,
            _autosave_timer: autosave_timer,
        }
    }

    fn autosave(&mut self, cx: &mut Context<Self>) {
        let Some(slot) = self.autosave.clone() else {
            return;
        };
        let history = self.editor.history().read(cx);
        if history.is_saved() {
            self.discard_autosave(cx);
            return;
        }
        let revision = history.revision();
        if self.autosaved == Some(revision) {
            return;
        }
        self.autosaved = Some(revision);
        let project = self.project().read(cx).clone();
        let file = self.file.clone();
        let writing = self.queue_file_io(cx, move || slot.write(&project, file.as_deref()));
        cx.spawn(async move |this, cx| {
            if let Ok(Err(error)) = writing.await {
                tracing::warn!(%error, "could not autosave the project");
                this.update(cx, |workspace, _| {
                    if workspace.autosaved == Some(revision) {
                        workspace.autosaved = None;
                    }
                })
                .ok();
            }
        })
        .detach();
    }

    fn discard_autosave(&mut self, cx: &mut Context<Self>) {
        let Some(slot) = self.autosave.clone() else {
            return;
        };
        if self.autosaved.take().is_some() {
            drop(self.queue_file_io(cx, move || slot.remove()));
        }
    }

    pub fn offer_recovery(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(AutosaveDirectory(directory)) = cx.try_global::<AutosaveDirectory>().cloned()
        else {
            return;
        };
        let found = cx.background_spawn(async move {
            let orphan = orphans(&directory).into_iter().next()?;
            let recovered = orphan.recover();
            Some((orphan, recovered))
        });
        cx.spawn_in(window, async move |this, cx| {
            let Some((orphan, recovered)) = found.await else {
                return;
            };
            let project = match recovered {
                Ok(project) => project,
                Err(error) => {
                    tracing::warn!(%error, "an autosave cannot be read, discarding it");
                    orphan.discard();
                    return;
                }
            };
            let name = orphan
                .file
                .as_deref()
                .map_or_else(|| project.name.clone(), |file| file_name(file).to_string());
            let Ok(answer) = this.update_in(cx, |_, window, cx| {
                window.prompt(
                    PromptLevel::Warning,
                    &format!("Recover the unsaved changes to “{name}”?"),
                    Some("Tessera stopped before they were saved."),
                    &RECOVERY_CHOICES,
                    cx,
                )
            }) else {
                return;
            };
            match answer
                .await
                .ok()
                .and_then(|index| RECOVERY_CHOICES.get(index))
            {
                Some(&RECOVER) => {
                    let Ok(confirming) = this.update_in(cx, |workspace, window, cx| {
                        workspace.confirm_discard(window, cx)
                    }) else {
                        return;
                    };
                    if confirming.await {
                        this.update_in(cx, |workspace, window, cx| {
                            workspace.recover(orphan, project, window, cx);
                        })
                        .ok();
                    }
                }
                Some(&DISCARD_RECOVERY) => {
                    cx.background_spawn(async move { orphan.discard() }).await;
                }
                _ => {}
            }
        })
        .detach();
    }

    fn recover(
        &mut self,
        orphan: Orphan,
        project: Project,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        tracing::info!(autosave = %orphan.slot.project().display(), "recovered unsaved changes");
        self.replace_project(project, cx);
        self.editor.mark_unsaved(cx);
        match orphan.file.clone() {
            Some(file) => self.set_file(file, window, cx),
            None => {
                self.file = None;
                window.set_window_title(&self.title(cx));
            }
        }
        let Some(slot) = self.autosave.clone() else {
            return;
        };
        self.autosaved = Some(self.editor.revision(cx));
        let adopting = self.queue_file_io(cx, move || slot.adopt(&orphan));
        cx.spawn(async move |_, _| {
            if let Ok(Err(error)) = adopting.await {
                tracing::warn!(%error, "could not take over the recovered autosave");
            }
        })
        .detach();
    }

    fn open_sequence_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.dialog.is_some() {
            return;
        }
        let settings = self.project().read(cx).settings;
        let dialog = cx.new(|cx| SequenceSettingsDialog::new(settings, window, cx));
        let subscription = cx.subscribe_in(&dialog, window, |workspace, _, event, window, cx| {
            if let SequenceDialogEvent::Apply(settings) = *event {
                workspace
                    .editor
                    .perform(Command::SetSequenceSettings, cx, |project| {
                        project.set_settings(settings);
                    });
            }
            workspace.close_dialog(window, cx);
        });
        self.dialog = Some((Dialog::SequenceSettings(dialog), subscription));
        cx.notify();
    }

    fn open_recent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.dialog.is_some() {
            return;
        }
        let paths = cx.default_global::<RecentProjects>().paths().to_vec();
        let dialog = cx.new(|cx| RecentProjectsDialog::new(paths, window, cx));
        let subscription = cx.subscribe_in(&dialog, window, |workspace, _, event, window, cx| {
            workspace.close_dialog(window, cx);
            if let RecentDialogEvent::Open(path) = event {
                workspace.open_from(path.clone(), window, cx);
            }
        });
        self.dialog = Some((Dialog::RecentProjects(dialog), subscription));
        cx.notify();
    }

    fn close_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.dialog = None;
        window.focus(&self.focus_handle);
        cx.notify();
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
                    UnsavedChanges::Discard => {
                        workspace.discard_autosave(cx);
                        Task::ready(true)
                    }
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
                Ok(Some(path)) => workspace.save_chosen(path, window, cx),
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

    fn save_chosen(
        &mut self,
        chosen: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<bool> {
        let target = with_project_extension(chosen.clone());
        if target == chosen || !target.exists() {
            return self.save_to(target, window, cx);
        }
        let answer = window.prompt(
            PromptLevel::Warning,
            &format!("“{}” already exists. Replace it?", file_name(&target)),
            Some("The dialog did not ask about this file, because Tessera added its extension."),
            &["Replace", "Cancel"],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            if answer.await.ok() != Some(0) {
                return false;
            }
            match this.update_in(cx, |workspace, window, cx| {
                workspace.save_to(target, window, cx)
            }) {
                Ok(saving) => saving.await,
                Err(_) => false,
            }
        })
    }

    fn new_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let confirming = self.confirm_discard(window, cx);
        cx.spawn_in(window, async move |this, cx| {
            if !confirming.await {
                return;
            }
            this.update_in(cx, |workspace, window, cx| {
                workspace.replace_project(Project::new(NEW_PROJECT_NAME), cx);
                workspace.file = None;
                window.set_window_title(&workspace.title(cx));
            })
            .ok();
        })
        .detach();
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
                    if workspace.editor.history().read(cx).is_saved() {
                        workspace.discard_autosave(cx);
                    }
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
                        recent::update(cx, |recent| recent.forget(&path));
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
        recent::update(cx, |recent| recent.remember(path.clone()));
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
            .on_action(cx.listener(|workspace, _: &NewProject, window, cx| {
                workspace.new_project(window, cx);
            }))
            .on_action(cx.listener(|workspace, _: &Save, window, cx| {
                workspace.save(window, cx).detach();
            }))
            .on_action(cx.listener(|workspace, _: &SaveAs, window, cx| {
                workspace.prompt_save(window, cx).detach();
            }))
            .on_action(cx.listener(|workspace, _: &Open, window, cx| {
                workspace.prompt_open(window, cx);
            }))
            .on_action(cx.listener(|workspace, _: &OpenRecent, window, cx| {
                workspace.open_recent(window, cx);
            }))
            .on_action(
                cx.listener(|workspace, _: &OpenSequenceSettings, window, cx| {
                    workspace.open_sequence_settings(window, cx);
                }),
            )
            .on_action(cx.listener(|workspace, _: &FocusSearch, window, cx| {
                workspace
                    .media_bin
                    .update(cx, |media_bin, cx| media_bin.focus_search(window, cx));
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
            .on_action(cx.listener(|workspace, _: &RaiseClipGain, _, cx| {
                workspace.on_timeline(cx, TimelinePanel::raise_clip_gain);
            }))
            .on_action(cx.listener(|workspace, _: &LowerClipGain, _, cx| {
                workspace.on_timeline(cx, TimelinePanel::lower_clip_gain);
            }))
            .on_action(cx.listener(|workspace, _: &CycleAudioStream, _, cx| {
                workspace.on_timeline(cx, TimelinePanel::cycle_audio_stream);
            }))
            .on_action(cx.listener(|workspace, _: &ToggleClipLink, _, cx| {
                workspace.on_timeline(cx, TimelinePanel::toggle_link_selection);
            }))
            .on_action(cx.listener(|workspace, _: &SelectAll, _, cx| {
                workspace.on_timeline(cx, TimelinePanel::select_all);
            }))
            .on_action(cx.listener(|workspace, _: &CopyClips, _, cx| {
                workspace.on_timeline(cx, TimelinePanel::copy_selection);
            }))
            .on_action(cx.listener(|workspace, _: &CutClips, _, cx| {
                workspace.on_timeline(cx, TimelinePanel::cut_selection);
            }))
            .on_action(cx.listener(|workspace, _: &PasteClips, _, cx| {
                workspace.on_timeline(cx, TimelinePanel::paste);
            }))
            .on_action(cx.listener(|workspace, _: &DuplicateClips, _, cx| {
                workspace.on_timeline(cx, TimelinePanel::duplicate_selection);
            }))
            .on_action(cx.listener(|workspace, _: &AddMarker, _, cx| {
                workspace.on_timeline(cx, TimelinePanel::add_marker_at_playhead);
            }))
            .on_action(cx.listener(|workspace, _: &RemoveMarker, _, cx| {
                workspace.on_timeline(cx, TimelinePanel::remove_marker_at_playhead);
            }))
            .on_action(cx.listener(|workspace, _: &SetInPoint, _, cx| {
                workspace.on_timeline(cx, TimelinePanel::set_in_point_at_playhead);
            }))
            .on_action(cx.listener(|workspace, _: &SetOutPoint, _, cx| {
                workspace.on_timeline(cx, TimelinePanel::set_out_point_at_playhead);
            }))
            .on_action(cx.listener(|workspace, _: &ClearInOut, _, cx| {
                workspace.on_timeline(cx, TimelinePanel::clear_in_out);
            }))
            .on_action(cx.listener(|workspace, _: &NextEdit, _, cx| {
                workspace.on_timeline(cx, TimelinePanel::go_to_next_edit);
            }))
            .on_action(cx.listener(|workspace, _: &PreviousEdit, _, cx| {
                workspace.on_timeline(cx, TimelinePanel::go_to_previous_edit);
            }))
            .on_action(cx.listener(|workspace, _: &NextMarker, _, cx| {
                workspace.on_timeline(cx, TimelinePanel::go_to_next_marker);
            }))
            .on_action(cx.listener(|workspace, _: &PreviousMarker, _, cx| {
                workspace.on_timeline(cx, TimelinePanel::go_to_previous_marker);
            }))
            .on_action(cx.listener(|workspace, _: &Cancel, window, cx| {
                cx.stop_active_drag(window);
                workspace.on_timeline(cx, TimelinePanel::drag_cancelled);
            }))
            .on_action(cx.listener(|workspace, _: &DeleteClip, _, cx| {
                workspace.on_timeline(cx, TimelinePanel::delete_selection);
            }))
            .on_action(cx.listener(|workspace, _: &RippleDeleteClip, _, cx| {
                workspace.on_timeline(cx, TimelinePanel::ripple_delete_selection);
            }))
            .on_action(cx.listener(|workspace, _: &ToggleSafeAreas, _, cx| {
                workspace.viewer.update(cx, Viewer::toggle_safe_areas);
            }))
            .on_action(cx.listener(|workspace, _: &ToggleSnapping, _, cx| {
                workspace.on_timeline(cx, TimelinePanel::toggle_snapping);
            }))
            .on_action(cx.listener(|workspace, _: &Undo, _, cx| workspace.editor.undo(cx)))
            .on_action(cx.listener(|workspace, _: &Redo, _, cx| workspace.editor.redo(cx)))
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme::background())
            .text_color(theme::text())
            .text_sm()
            .children(self.dialog.as_ref().map(|(dialog, _)| {
                div()
                    .absolute()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .occlude()
                    .bg(theme::backdrop())
                    .child(dialog.view())
            }))
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
    use std::num::NonZero;

    use gpui::{Modifiers, MouseButton, TestAppContext, VisualTestContext, point};
    use tessera_timeline::{MediaInfo, Stream, Time, VideoStream};

    use super::*;
    use crate::playhead::Speed;

    const TIMELINE_HEIGHT: f32 = 260.;
    const V1_BELOW_TIMELINE_TOP: f32 = 1. + 24. + 24.;
    const LANES_LEFT: f32 = 128.;
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
            streams: vec![Stream::Video(VideoStream::new(
                0,
                "h264",
                NonZero::new(1920).unwrap(),
                NonZero::new(1080).unwrap(),
            ))],
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

    fn recent_paths(cx: &mut VisualTestContext) -> Vec<PathBuf> {
        cx.update(|_, cx| cx.default_global::<RecentProjects>().paths().to_vec())
    }

    #[gpui::test]
    fn a_saved_project_reopens_from_the_recent_list(cx: &mut TestAppContext) {
        cx.update(crate::init);
        let dir = ScratchDir::new("recent");
        let (first, second) = (dir.0.join("first.tessera"), dir.0.join("second.tessera"));
        let saved = sample_project("First", "/media/first.mkv");
        let project = cx.new(|_| saved.clone());
        let (workspace, cx) = cx.add_window_view(|window, cx| Workspace::new(project, window, cx));

        for path in [&first, &second] {
            workspace.update_in(cx, |workspace, window, cx| {
                workspace.save_to(path.clone(), window, cx).detach();
            });
            cx.run_until_parked();
        }

        assert_eq!(recent_paths(cx), [second.clone(), first.clone()]);

        workspace.update(cx, |workspace, cx| {
            workspace.replace_project(sample_project("Other", "/media/other.mkv"), cx);
            workspace
                .editor
                .mark_saved(workspace.editor.revision(cx), cx);
        });
        cx.simulate_keystrokes("ctrl-shift-o");

        assert!(cx.read(|cx| workspace.read(cx).dialog.is_some()));

        cx.simulate_keystrokes("down enter");
        cx.run_until_parked();

        assert!(cx.read(|cx| workspace.read(cx).dialog.is_none()));
        assert_eq!(current(&workspace, cx), saved);
        assert_eq!(recent_paths(cx), [first.clone(), second.clone()]);

        std::fs::remove_file(&second).unwrap();
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.open_from(second.clone(), window, cx);
        });
        cx.run_until_parked();
        cx.simulate_prompt_answer("Ok");

        assert_eq!(recent_paths(cx), [first]);
    }

    fn autosave_path(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> PathBuf {
        cx.read(|cx| {
            workspace
                .read(cx)
                .autosave
                .as_ref()
                .unwrap()
                .project()
                .to_owned()
        })
    }

    #[gpui::test]
    fn unsaved_changes_are_autosaved_until_they_are_saved_or_discarded(cx: &mut TestAppContext) {
        cx.update(crate::init);
        let dir = ScratchDir::new("autosave");
        cx.update(|cx| cx.set_global(AutosaveDirectory(dir.0.join("autosave"))));
        let project = cx.new(|_| sample_project("Cut", "/media/cut.mkv"));
        let (workspace, cx) = cx.add_window_view(|window, cx| Workspace::new(project, window, cx));
        let autosave = autosave_path(&workspace, cx);
        let autosave_now = |cx: &mut VisualTestContext| {
            workspace.update(cx, Workspace::autosave);
            cx.run_until_parked();
        };

        autosave_now(cx);

        assert!(!autosave.exists());

        split_at(&workspace, 4, cx);
        autosave_now(cx);

        assert_eq!(
            tessera_document::open(&autosave).unwrap(),
            current(&workspace, cx)
        );

        let path = dir.0.join("cut.tessera");
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.save_to(path.clone(), window, cx).detach();
        });
        cx.run_until_parked();

        assert!(!autosave.exists());

        split_at(&workspace, 12, cx);
        autosave_now(cx);

        assert!(autosave.exists());

        cx.simulate_keystrokes("ctrl-n");
        cx.run_until_parked();
        cx.simulate_prompt_answer("Don't Save");
        cx.run_until_parked();

        assert!(!autosave.exists());
    }

    #[gpui::test]
    fn changes_left_by_a_crashed_session_can_be_recovered(cx: &mut TestAppContext) {
        cx.update(crate::init);
        let dir = ScratchDir::new("recovery");
        let directory = dir.0.join("autosave");
        cx.update(|cx| cx.set_global(AutosaveDirectory(directory.clone())));
        let lost = sample_project("Lost", "/media/lost.mkv");
        let file = dir.0.join("lost.tessera");
        let crashed = Slot::named(&directory, &format!("{}-0", u32::MAX - 1));
        crashed.write(&lost, Some(&file)).unwrap();
        let project = cx.new(|_| Project::new(NEW_PROJECT_NAME));
        let (workspace, cx) = cx.add_window_view(|window, cx| Workspace::new(project, window, cx));

        workspace.update_in(cx, Workspace::offer_recovery);
        cx.run_until_parked();
        cx.simulate_prompt_answer(RECOVER);
        cx.run_until_parked();

        assert_eq!(current(&workspace, cx), lost);
        assert_eq!(
            cx.window_title().as_deref(),
            Some("• lost.tessera — Tessera")
        );
        assert!(!crashed.project().exists());
        assert!(autosave_path(&workspace, cx).exists());
        assert!(orphans(&directory).is_empty());
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

    #[gpui::test]
    fn an_added_extension_that_would_replace_a_file_asks_first(cx: &mut TestAppContext) {
        cx.update(crate::init);
        let dir = ScratchDir::new("overwrite");
        let chosen = dir.0.join("cut");
        let existing = dir.0.join("cut.tessera");
        std::fs::write(&existing, "an older project").unwrap();
        let project = cx.new(|_| sample_project("Cut", "/media/cut.mkv"));
        let (workspace, cx) = cx.add_window_view(|window, cx| Workspace::new(project, window, cx));
        let save_as = |cx: &mut VisualTestContext| {
            workspace.update_in(cx, |workspace, window, cx| {
                workspace.prompt_save(window, cx).detach();
            });
            cx.run_until_parked();
            cx.simulate_new_path_selection(|_| Some(chosen.clone()));
            cx.run_until_parked();
        };

        save_as(cx);

        assert_eq!(
            cx.pending_prompt().map(|(message, _)| message).as_deref(),
            Some("“cut.tessera” already exists. Replace it?")
        );

        cx.simulate_prompt_answer("Cancel");
        cx.run_until_parked();

        assert_eq!(
            std::fs::read_to_string(&existing).unwrap(),
            "an older project"
        );
        assert_eq!(cx.read(|cx| workspace.read(cx).file.clone()), None);

        save_as(cx);
        cx.simulate_prompt_answer("Replace");
        cx.run_until_parked();

        assert!(tessera_document::open(&existing).is_ok());
        assert_eq!(
            cx.read(|cx| workspace.read(cx).file.clone()),
            Some(existing)
        );
    }

    #[gpui::test]
    fn a_chosen_name_that_already_has_the_extension_needs_no_confirmation(cx: &mut TestAppContext) {
        cx.update(crate::init);
        let dir = ScratchDir::new("no-overwrite-prompt");
        let chosen = dir.0.join("cut.tessera");
        std::fs::write(&chosen, "an older project").unwrap();
        let project = cx.new(|_| sample_project("Cut", "/media/cut.mkv"));
        let (workspace, cx) = cx.add_window_view(|window, cx| Workspace::new(project, window, cx));

        workspace.update_in(cx, |workspace, window, cx| {
            workspace.prompt_save(window, cx).detach();
        });
        cx.run_until_parked();
        cx.simulate_new_path_selection(|_| Some(chosen.clone()));
        cx.run_until_parked();

        assert!(!cx.has_pending_prompt());
        assert!(tessera_document::open(&chosen).is_ok());
    }

    #[gpui::test]
    fn a_new_project_replaces_the_current_one_after_asking_about_changes(cx: &mut TestAppContext) {
        cx.update(crate::init);
        let dir = ScratchDir::new("new-project");
        let path = dir.0.join("cut.tessera");
        let project = cx.new(|_| sample_project("Cut", "/media/cut.mkv"));
        let (workspace, cx) = cx.add_window_view(|window, cx| Workspace::new(project, window, cx));
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.save_to(path.clone(), window, cx).detach();
        });
        cx.run_until_parked();
        split_at(&workspace, 4, cx);

        cx.simulate_keystrokes("ctrl-n");
        cx.run_until_parked();

        assert_eq!(
            cx.pending_prompt().map(|(message, _)| message).as_deref(),
            Some("Save the changes to “cut.tessera”?")
        );

        cx.simulate_prompt_answer("Cancel");
        cx.run_until_parked();

        assert_eq!(starts(&workspace, cx), [1, 4, 10].map(Time::from_seconds));

        cx.simulate_keystrokes("ctrl-n");
        cx.run_until_parked();
        cx.simulate_prompt_answer("Don't Save");
        cx.run_until_parked();

        assert_eq!(current(&workspace, cx), Project::new(NEW_PROJECT_NAME));
        assert_eq!(cx.read(|cx| workspace.read(cx).file.clone()), None);
        assert_eq!(cx.window_title().as_deref(), Some("Untitled — Tessera"));
    }

    #[gpui::test]
    fn save_as_asks_for_a_path_even_when_the_project_has_a_file(cx: &mut TestAppContext) {
        cx.update(crate::init);
        let dir = ScratchDir::new("save-as");
        let first = dir.0.join("first.tessera");
        let second = dir.0.join("second.tessera");
        let project = cx.new(|_| sample_project("Cut", "/media/cut.mkv"));
        let (workspace, cx) = cx.add_window_view(|window, cx| Workspace::new(project, window, cx));
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.save_to(first.clone(), window, cx).detach();
        });
        cx.run_until_parked();

        cx.simulate_keystrokes("ctrl-shift-s");
        cx.run_until_parked();
        cx.simulate_new_path_selection(|_| Some(second.clone()));
        cx.run_until_parked();

        assert!(second.exists());
        assert_eq!(cx.read(|cx| workspace.read(cx).file.clone()), Some(second));
    }

    fn seek_to(workspace: &Entity<Workspace>, seconds: i64, cx: &mut VisualTestContext) {
        let playhead = cx.read(|cx| workspace.read(cx).playhead.clone());
        playhead.update(cx, |playhead, cx| {
            playhead.seek(Time::from_seconds(seconds), cx);
        });
    }

    fn playhead_time(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> Time {
        cx.read(|cx| workspace.read(cx).playhead.read(cx).time())
    }

    fn seconds_of(values: &[i64]) -> Vec<Time> {
        values.iter().copied().map(Time::from_seconds).collect()
    }

    #[gpui::test]
    fn copy_cut_paste_and_duplicate_move_clips_around_the_playhead(cx: &mut TestAppContext) {
        cx.update(crate::init);
        let project = cx.new(|_| sample_project("test", "a.mkv"));
        let (workspace, cx) = cx.add_window_view(|window, cx| Workspace::new(project, window, cx));

        click_v1(cx, 2.);
        cx.simulate_keystrokes("ctrl-c");
        seek_to(&workspace, 30, cx);
        cx.simulate_keystrokes("ctrl-v");

        assert_eq!(starts(&workspace, cx), seconds_of(&[1, 10, 30]));

        cx.simulate_keystrokes("ctrl-v");

        assert_eq!(starts(&workspace, cx), seconds_of(&[1, 10, 30]));

        click_v1(cx, 11.);
        cx.simulate_keystrokes("ctrl-d");

        assert_eq!(starts(&workspace, cx), seconds_of(&[1, 10, 18, 30]));

        click_v1(cx, 11.);
        cx.simulate_keystrokes("ctrl-x");

        assert_eq!(starts(&workspace, cx), seconds_of(&[1, 18, 30]));

        seek_to(&workspace, 50, cx);
        cx.simulate_keystrokes("ctrl-v");

        assert_eq!(starts(&workspace, cx), seconds_of(&[1, 18, 30, 50]));

        cx.simulate_keystrokes("ctrl-z");
        cx.simulate_keystrokes("ctrl-z");

        assert_eq!(starts(&workspace, cx), seconds_of(&[1, 10, 18, 30]));
    }

    #[gpui::test]
    fn markers_and_in_out_points_follow_the_playhead_keys(cx: &mut TestAppContext) {
        cx.update(crate::init);
        let project = cx.new(|_| sample_project("test", "a.mkv"));
        let (workspace, cx) = cx.add_window_view(|window, cx| Workspace::new(project, window, cx));
        let marker_times = |cx: &mut VisualTestContext| -> Vec<Time> {
            cx.read(|cx| {
                workspace
                    .read(cx)
                    .project()
                    .read(cx)
                    .markers
                    .iter()
                    .map(|marker| marker.time)
                    .collect()
            })
        };

        for second in [4, 7] {
            seek_to(&workspace, second, cx);
            cx.simulate_keystrokes("m");
        }

        assert_eq!(marker_times(cx), seconds_of(&[4, 7]));

        seek_to(&workspace, 0, cx);
        cx.simulate_keystrokes("ctrl-right");

        assert_eq!(playhead_time(&workspace, cx), Time::from_seconds(4));

        cx.simulate_keystrokes("ctrl-right ctrl-right");

        assert_eq!(playhead_time(&workspace, cx), Time::from_seconds(7));

        cx.simulate_keystrokes("ctrl-left");

        assert_eq!(playhead_time(&workspace, cx), Time::from_seconds(4));

        cx.simulate_keystrokes("shift-m");

        assert_eq!(marker_times(cx), seconds_of(&[7]));

        seek_to(&workspace, 2, cx);
        cx.simulate_keystrokes("i");
        seek_to(&workspace, 6, cx);
        cx.simulate_keystrokes("o");

        let in_out = |cx: &mut VisualTestContext| {
            cx.read(|cx| {
                let project = workspace.read(cx).project().read(cx);
                (project.in_point, project.out_point)
            })
        };

        assert_eq!(
            in_out(cx),
            (Some(Time::from_seconds(2)), Some(Time::from_seconds(6)))
        );

        seek_to(&workspace, 1, cx);
        cx.simulate_keystrokes("o");

        assert_eq!(
            in_out(cx),
            (Some(Time::from_seconds(2)), Some(Time::from_seconds(6)))
        );

        cx.simulate_keystrokes("alt-x");

        assert_eq!(in_out(cx), (None, None));
    }

    #[gpui::test]
    fn up_and_down_jump_between_edits(cx: &mut TestAppContext) {
        cx.update(crate::init);
        let project = cx.new(|_| sample_project("test", "a.mkv"));
        let (workspace, cx) = cx.add_window_view(|window, cx| Workspace::new(project, window, cx));

        let mut visited = Vec::new();
        for key in ["down", "down", "down", "down", "down", "up", "up"] {
            cx.simulate_keystrokes(key);
            visited.push(playhead_time(&workspace, cx));
        }

        assert_eq!(visited, seconds_of(&[1, 9, 10, 18, 18, 10, 9]));
    }

    #[gpui::test]
    fn escape_cancels_a_clip_drag(cx: &mut TestAppContext) {
        cx.update(crate::init);
        let project = cx.new(|_| sample_project("test", "a.mkv"));
        let (workspace, cx) = cx.add_window_view(|window, cx| Workspace::new(project, window, cx));
        let height = cx.update(|window, _| window.viewport_size().height);
        let y = height - px(TIMELINE_HEIGHT) + px(V1_BELOW_TIMELINE_TOP);
        let at = |seconds: f32| point(px(LANES_LEFT + seconds * ONE_SECOND), y);
        let none = Modifiers::none();

        cx.simulate_mouse_down(at(2.), MouseButton::Left, none);
        cx.simulate_mouse_move(at(2.1), MouseButton::Left, none);
        cx.simulate_mouse_move(at(30.), MouseButton::Left, none);
        cx.simulate_keystrokes("escape");
        cx.simulate_mouse_up(at(30.), MouseButton::Left, none);

        assert_eq!(starts(&workspace, cx), seconds_of(&[1, 10]));
    }

    #[gpui::test]
    fn the_apostrophe_key_toggles_the_viewer_safe_areas(cx: &mut TestAppContext) {
        cx.update(crate::init);
        let project = cx.new(|_| sample_project("test", "a.mkv"));
        let (workspace, cx) = cx.add_window_view(|window, cx| Workspace::new(project, window, cx));
        let shown = |cx: &mut VisualTestContext| {
            cx.read(|cx| workspace.read(cx).viewer.read(cx).safe_areas())
        };

        assert!(!shown(cx));

        cx.simulate_keystrokes("'");

        assert!(shown(cx));

        cx.simulate_keystrokes("'");

        assert!(!shown(cx));
    }

    fn dialog_of(
        workspace: &Entity<Workspace>,
        cx: &mut VisualTestContext,
    ) -> Option<Entity<SequenceSettingsDialog>> {
        cx.read(|cx| match &workspace.read(cx).dialog {
            Some((Dialog::SequenceSettings(dialog), _)) => Some(dialog.clone()),
            _ => None,
        })
    }

    fn settings_of(
        workspace: &Entity<Workspace>,
        cx: &mut VisualTestContext,
    ) -> tessera_timeline::SequenceSettings {
        cx.read(|cx| workspace.read(cx).project().read(cx).settings)
    }

    #[gpui::test]
    fn the_sequence_settings_dialog_applies_a_new_size_rate_and_can_be_undone(
        cx: &mut TestAppContext,
    ) {
        cx.update(crate::init);
        let project = cx.new(|_| sample_project("test", "a.mkv"));
        let (workspace, cx) = cx.add_window_view(|window, cx| Workspace::new(project, window, cx));
        let original = settings_of(&workspace, cx);

        cx.simulate_keystrokes("ctrl-,");

        let dialog = dialog_of(&workspace, cx).expect("the dialog opens");
        dialog.update(cx, |dialog, cx| {
            dialog.select_frame_rate(tessera_timeline::FrameRate::FPS_24, cx);
            dialog.select_sample_rate(NonZero::new(96_000).unwrap(), cx);
        });
        cx.simulate_keystrokes("backspace backspace backspace backspace 1 2 8 0 enter");
        cx.simulate_keystrokes("backspace backspace backspace backspace 7 2 0 enter");

        let applied = settings_of(&workspace, cx);
        assert_eq!((applied.width.get(), applied.height.get()), (1280, 720));
        assert_eq!(applied.frame_rate, tessera_timeline::FrameRate::FPS_24);
        assert_eq!(applied.sample_rate.get(), 96_000);
        assert!(dialog_of(&workspace, cx).is_none());

        cx.simulate_keystrokes("ctrl-z");

        assert_eq!(settings_of(&workspace, cx), original);
    }

    #[gpui::test]
    fn the_sequence_settings_dialog_refuses_a_zero_size_and_escape_closes_it(
        cx: &mut TestAppContext,
    ) {
        cx.update(crate::init);
        let project = cx.new(|_| sample_project("test", "a.mkv"));
        let (workspace, cx) = cx.add_window_view(|window, cx| Workspace::new(project, window, cx));
        let original = settings_of(&workspace, cx);

        cx.simulate_keystrokes("ctrl-,");
        cx.simulate_keystrokes("backspace backspace backspace backspace 0 enter enter");

        assert!(dialog_of(&workspace, cx).is_some());
        assert_eq!(settings_of(&workspace, cx), original);

        cx.simulate_keystrokes("escape");

        assert!(dialog_of(&workspace, cx).is_none());
        assert_eq!(settings_of(&workspace, cx), original);

        cx.simulate_keystrokes("space");

        assert!(cx.read(|cx| !workspace.read(cx).playhead.read(cx).speed().is_paused()));
    }

    #[gpui::test]
    fn typing_in_a_text_field_does_not_trigger_the_single_key_shortcuts(cx: &mut TestAppContext) {
        cx.update(crate::init);
        let project = cx.new(|_| sample_project("test", "a.mkv"));
        let (workspace, cx) = cx.add_window_view(|window, cx| Workspace::new(project, window, cx));

        cx.simulate_keystrokes("ctrl-,");
        cx.simulate_keystrokes("m j l space n i o");

        let (markers, paused, snapping_points) = cx.read(|cx| {
            let workspace = workspace.read(cx);
            let project = workspace.project().read(cx);
            (
                project.markers.len(),
                workspace.playhead.read(cx).speed().is_paused(),
                (project.in_point, project.out_point),
            )
        });
        assert_eq!(markers, 0);
        assert!(paused);
        assert_eq!(snapping_points, (None, None));
    }
}
