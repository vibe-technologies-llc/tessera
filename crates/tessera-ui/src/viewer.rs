use gpui::{Context, Entity, IntoElement, ParentElement, Render, Styled, Window, div};
use tessera_timeline::{Project, Timecode};

use crate::{playhead::Playhead, theme};

pub struct Viewer {
    project: Entity<Project>,
    playhead: Entity<Playhead>,
}

impl Viewer {
    pub fn new(
        project: Entity<Project>,
        playhead: Entity<Playhead>,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&project, |_, _, cx| cx.notify()).detach();
        cx.observe(&playhead, |_, _, cx| cx.notify()).detach();
        Self { project, playhead }
    }
}

impl Render for Viewer {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let settings = self.project.read(cx).settings;
        let timecode = Timecode::new(self.playhead.read(cx).time(), settings.frame_rate);
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
            .gap_2()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(frame),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .justify_center()
                    .child(timecode.to_string()),
            )
    }
}
