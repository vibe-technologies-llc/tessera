use gpui::{Context, Entity, IntoElement, ParentElement, Render, Styled, Window, div};
use tessera_timeline::Project;

use crate::theme;

pub struct MediaBin {
    project: Entity<Project>,
}

impl MediaBin {
    pub fn new(project: Entity<Project>, cx: &mut Context<Self>) -> Self {
        cx.observe(&project, |_, _, cx| cx.notify()).detach();
        Self { project }
    }
}

impl Render for MediaBin {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let project = self.project.read(cx);
        let body = if project.assets.is_empty() {
            div()
                .p_3()
                .text_color(theme::text_muted())
                .child("No media imported")
        } else {
            div()
                .flex()
                .flex_col()
                .children(project.assets.iter().map(|asset| {
                    let name = asset.path.file_name().map_or_else(
                        || asset.path.display().to_string(),
                        |name| name.to_string_lossy().into_owned(),
                    );
                    div().px_3().py_1().child(name)
                }))
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme::panel())
            .child(
                div()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(theme::border())
                    .text_color(theme::text_muted())
                    .child("Media"),
            )
            .child(body)
    }
}
