use gpui::{
    AppContext, Context, Entity, FocusHandle, InteractiveElement, IntoElement, ParentElement,
    Render, Styled, Window, div, px,
};
use tessera_timeline::Project;

use crate::{
    DeleteClip, Import, Pause, PlayPause, RippleDeleteClip, ShuttleBackward, ShuttleForward,
    SplitAtPlayhead, StepBackward, StepForward, WORKSPACE_CONTEXT, ZoomIn, ZoomOut, ZoomToFit,
    media_bin::MediaBin, playhead::Playhead, theme, timeline::TimelinePanel, viewer::Viewer,
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

#[cfg(test)]
mod tests {
    use gpui::{Modifiers, TestAppContext, VisualTestContext, point};
    use tessera_timeline::{MediaInfo, Stream, Time, VideoStream};

    use super::*;

    const TIMELINE_HEIGHT: f32 = 260.;
    const V1_BELOW_TIMELINE_TOP: f32 = 1. + 24. + 24.;
    const LANES_LEFT: f32 = 96.;
    const ONE_SECOND: f32 = 48.;

    fn starts(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> Vec<Time> {
        cx.read(|cx| {
            workspace.read(cx).project.read(cx).timeline.tracks[0]
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

    #[gpui::test]
    fn editing_keys_reach_the_timeline(cx: &mut TestAppContext) {
        cx.update(crate::init);
        let mut project = Project::new("test");
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
        let asset = project.add_asset("a.mkv".into(), info);
        for seconds in [1, 10] {
            project
                .place_clip(asset, 0, Time::from_seconds(seconds))
                .unwrap();
        }
        let project = cx.new(|_| project);
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
    }
}
