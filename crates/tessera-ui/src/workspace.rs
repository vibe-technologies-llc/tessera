use gpui::{
    AppContext, Context, Entity, FocusHandle, InteractiveElement, IntoElement, ParentElement,
    Render, Styled, Window, div, px,
};
use tessera_timeline::Project;

use crate::{
    Import, Pause, PlayPause, ShuttleBackward, ShuttleForward, StepBackward, StepForward,
    WORKSPACE_CONTEXT, ZoomIn, ZoomOut, ZoomToFit, media_bin::MediaBin, playhead::Playhead, theme,
    timeline::TimelinePanel, viewer::Viewer,
};

pub struct Workspace {
    project: Entity<Project>,
    focus_handle: FocusHandle,
    playhead: Entity<Playhead>,
    media_bin: Entity<MediaBin>,
    viewer: Entity<Viewer>,
    timeline: Entity<TimelinePanel>,
}

impl Workspace {
    pub fn new(project: Entity<Project>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle);
        let playhead = cx.new(|_| Playhead::new(project.clone()));
        Self {
            focus_handle,
            media_bin: cx.new(|cx| MediaBin::new(project.clone(), cx)),
            viewer: cx.new(|cx| Viewer::new(project.clone(), playhead.clone(), cx)),
            timeline: cx.new(|cx| TimelinePanel::new(project.clone(), playhead.clone(), cx)),
            playhead,
            project,
        }
    }

    pub fn project(&self) -> &Entity<Project> {
        &self.project
    }

    fn transport(
        &mut self,
        cx: &mut Context<Self>,
        control: impl FnOnce(&mut Playhead, &mut Context<Playhead>),
    ) {
        self.playhead.update(cx, control);
    }

    fn zoom(
        &mut self,
        cx: &mut Context<Self>,
        zoom: impl FnOnce(&mut TimelinePanel, &mut Context<TimelinePanel>),
    ) {
        self.timeline.update(cx, zoom);
    }
}

impl Render for Workspace {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .track_focus(&self.focus_handle)
            .key_context(WORKSPACE_CONTEXT)
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
                workspace.zoom(cx, TimelinePanel::zoom_in);
            }))
            .on_action(cx.listener(|workspace, _: &ZoomOut, _, cx| {
                workspace.zoom(cx, TimelinePanel::zoom_out);
            }))
            .on_action(cx.listener(|workspace, _: &ZoomToFit, _, cx| {
                workspace.zoom(cx, TimelinePanel::zoom_to_fit);
            }))
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
