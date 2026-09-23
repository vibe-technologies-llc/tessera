use gpui::{
    AppContext, Context, Entity, IntoElement, ParentElement, Render, Styled, Window, div, px,
};
use tessera_timeline::Project;

use crate::{media_bin::MediaBin, theme, timeline::TimelinePanel, viewer::Viewer};

pub struct Workspace {
    project: Entity<Project>,
    media_bin: Entity<MediaBin>,
    viewer: Entity<Viewer>,
    timeline: Entity<TimelinePanel>,
}

impl Workspace {
    pub fn new(project: Entity<Project>, cx: &mut Context<Self>) -> Self {
        Self {
            media_bin: cx.new(|cx| MediaBin::new(project.clone(), cx)),
            viewer: cx.new(|cx| Viewer::new(project.clone(), cx)),
            timeline: cx.new(|cx| TimelinePanel::new(project.clone(), cx)),
            project,
        }
    }

    pub fn project(&self) -> &Entity<Project> {
        &self.project
    }
}

impl Render for Workspace {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
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
