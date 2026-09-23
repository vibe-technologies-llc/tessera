use gpui::{Context, Entity, IntoElement, ParentElement, Render, Styled, Window, div};
use tessera_timeline::Project;

use crate::theme;

pub struct Viewer {
    project: Entity<Project>,
}

impl Viewer {
    pub fn new(project: Entity<Project>, cx: &mut Context<Self>) -> Self {
        cx.observe(&project, |_, _, cx| cx.notify()).detach();
        Self { project }
    }
}

impl Render for Viewer {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let settings = self.project.read(cx).settings;
        let aspect_ratio = settings.width as f32 / settings.height as f32;
        let label = format!(
            "{}×{} · {:.3} fps",
            settings.width,
            settings.height,
            settings.frame_rate.as_f64()
        );
        let mut frame = div()
            .max_w_full()
            .max_h_full()
            .h_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(theme::frame())
            .text_color(theme::text_muted())
            .child(label);
        frame.style().aspect_ratio = Some(aspect_ratio);
        div()
            .size_full()
            .p_4()
            .flex()
            .items_center()
            .justify_center()
            .child(frame)
    }
}
