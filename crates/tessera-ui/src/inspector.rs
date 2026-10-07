use gpui::{
    AppContext, ClickEvent, Context, Entity, FocusHandle, InteractiveElement, IntoElement,
    ParentElement, Render, ScrollWheelEvent, SharedString, StatefulInteractiveElement, Styled,
    Subscription, Window, div, px,
};
use tessera_timeline::{Clip, Command, Opacity, Project, TrackKind, Transform, TransformField};

use crate::{
    editor::ProjectEditor,
    text_field::{TextField, TextFieldEvent},
    theme,
    timeline::TimelinePanel,
};

const NOTHING_SELECTED: &str = "Select a video clip to adjust its picture";
const COARSE_STEPS: i64 = 10;
const PERMILLE_PER_PERCENT: i64 = 10;
const TENTHS_PER_DEGREE: i64 = 10;
const ROW_HEIGHT: f32 = 24.;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Property {
    Transform(TransformField),
    Opacity,
}

impl Property {
    pub const ALL: [Self; 9] = [
        Self::Transform(TransformField::X),
        Self::Transform(TransformField::Y),
        Self::Transform(TransformField::Scale),
        Self::Transform(TransformField::Rotation),
        Self::Transform(TransformField::CropLeft),
        Self::Transform(TransformField::CropTop),
        Self::Transform(TransformField::CropRight),
        Self::Transform(TransformField::CropBottom),
        Self::Opacity,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Transform(field) => field.label(),
            Self::Opacity => "Opacity",
        }
    }

    fn command(self) -> Command {
        match self {
            Self::Transform(_) => Command::TransformClip,
            Self::Opacity => Command::SetClipOpacity,
        }
    }

    fn raw(self, clip: &Clip) -> i64 {
        match self {
            Self::Transform(field) => clip.transform.get(field),
            Self::Opacity => clip.opacity.permille().into(),
        }
    }

    fn default_raw(self) -> i64 {
        match self {
            Self::Transform(field) => Transform::IDENTITY.get(field),
            Self::Opacity => Opacity::OPAQUE.permille().into(),
        }
    }

    fn per_unit(self) -> i64 {
        match self {
            Self::Transform(TransformField::X | TransformField::Y) => 1,
            Self::Transform(TransformField::Rotation) => TENTHS_PER_DEGREE,
            Self::Transform(_) | Self::Opacity => PERMILLE_PER_PERCENT,
        }
    }

    fn unit(self) -> &'static str {
        match self {
            Self::Transform(TransformField::X | TransformField::Y) => " px",
            Self::Transform(TransformField::Rotation) => "°",
            Self::Transform(_) | Self::Opacity => "%",
        }
    }

    pub fn shown(self, raw: i64) -> String {
        let per_unit = self.per_unit();
        let whole = raw / per_unit;
        let part = (raw % per_unit).abs();
        let sign = if raw < 0 && whole == 0 { "-" } else { "" };
        if part == 0 {
            format!("{sign}{whole}{}", self.unit())
        } else {
            format!("{sign}{whole}.{part}{}", self.unit())
        }
    }

    pub fn parsed(self, text: &str) -> Option<i64> {
        let number = text
            .trim()
            .trim_end_matches(self.unit().trim())
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|number| number.is_finite())?;
        Some((number * self.per_unit() as f64).round() as i64)
    }

    pub fn applied(self, clip: &Clip, raw: i64) -> (Transform, Opacity) {
        match self {
            Self::Transform(field) => (clip.transform.with(field, raw), clip.opacity),
            Self::Opacity => (clip.transform, Opacity::saturating(raw)),
        }
    }
}

struct Editing {
    property: Property,
    field: Entity<TextField>,
    _subscriptions: [Subscription; 2],
}

pub struct Inspector {
    editor: ProjectEditor,
    project: Entity<Project>,
    timeline: Entity<TimelinePanel>,
    editing: Option<Editing>,
    focus_return: Option<FocusHandle>,
}

impl Inspector {
    pub fn new(
        editor: ProjectEditor,
        timeline: Entity<TimelinePanel>,
        cx: &mut Context<Self>,
    ) -> Self {
        let project = editor.project().clone();
        cx.observe(&project, |_, _, cx| cx.notify()).detach();
        cx.observe(&timeline, |_, _, cx| cx.notify()).detach();
        Self {
            editor,
            project,
            timeline,
            editing: None,
            focus_return: None,
        }
    }

    pub fn return_focus_to(&mut self, handle: FocusHandle) {
        self.focus_return = Some(handle);
    }

    fn selected_pictures(&self, cx: &Context<Self>) -> Vec<Clip> {
        let project = self.project.read(cx);
        self.timeline
            .read(cx)
            .selection()
            .iter()
            .filter_map(|&id| project.find_clip(id))
            .filter(|(track, _)| project.timeline.tracks[*track].kind == TrackKind::Video)
            .map(|(_, clip)| *clip)
            .collect()
    }

    pub fn set(&mut self, property: Property, value: impl Fn(i64) -> i64, cx: &mut Context<Self>) {
        let changes: Vec<_> = self
            .selected_pictures(cx)
            .iter()
            .map(|clip| {
                let (transform, opacity) = property.applied(clip, value(property.raw(clip)));
                (clip.id, transform, opacity)
            })
            .collect();
        if changes.is_empty() {
            return;
        }
        let changed = self.editor.apply(property.command(), cx, |project| {
            project.set_clip_pictures(&changes)
        });
        if let Err(error) = changed {
            tracing::warn!(%error, "could not change the clip picture");
        }
    }

    pub fn step(&mut self, property: Property, steps: i64, cx: &mut Context<Self>) {
        let delta = steps * property.per_unit();
        self.set(property, |raw| raw.saturating_add(delta), cx);
    }

    pub fn reset(&mut self, property: Property, cx: &mut Context<Self>) {
        self.set(property, |_| property.default_raw(), cx);
    }

    pub fn reset_all(&mut self, cx: &mut Context<Self>) {
        let changes: Vec<_> = self
            .selected_pictures(cx)
            .iter()
            .map(|clip| (clip.id, Transform::IDENTITY, Opacity::OPAQUE))
            .collect();
        if changes.is_empty() {
            return;
        }
        let reset = self.editor.apply(Command::TransformClip, cx, |project| {
            project.set_clip_pictures(&changes)
        });
        if let Err(error) = reset {
            tracing::warn!(%error, "could not reset the clip picture");
        }
    }

    fn start_editing(&mut self, property: Property, window: &mut Window, cx: &mut Context<Self>) {
        let Some(clip) = self.selected_pictures(cx).first().copied() else {
            return;
        };
        let shown = property.shown(property.raw(&clip));
        let value = shown.trim_end_matches(property.unit()).to_owned();
        let field = cx.new(|cx| TextField::new(value, property.label(), cx));
        let focus_handle = field.read(cx).focus_handle().clone();
        let subscriptions = [
            cx.subscribe_in(
                &field,
                window,
                move |inspector, _, event, window, cx| match event {
                    TextFieldEvent::Changed(_) => {}
                    TextFieldEvent::Submitted(text) => {
                        let text = text.clone();
                        inspector.finish_editing(Some(text), window, cx);
                    }
                    TextFieldEvent::Cancelled => inspector.finish_editing(None, window, cx),
                },
            ),
            cx.on_focus_out(&focus_handle, window, |inspector, _, window, cx| {
                inspector.finish_editing(None, window, cx);
            }),
        ];
        field.read(cx).focus(window);
        self.editing = Some(Editing {
            property,
            field,
            _subscriptions: subscriptions,
        });
        cx.notify();
    }

    fn finish_editing(
        &mut self,
        text: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(editing) = self.editing.take() else {
            return;
        };
        let typed = text.and_then(|text| editing.property.parsed(&text));
        if let Some(raw) = typed {
            self.set(editing.property, |_| raw, cx);
        }
        if let Some(handle) = &self.focus_return {
            window.focus(handle);
        }
        cx.notify();
    }

    fn row(&self, property: Property, clip: &Clip, cx: &Context<Self>) -> impl IntoElement {
        let raw = property.raw(clip);
        let editing = self
            .editing
            .as_ref()
            .filter(|editing| editing.property == property)
            .map(|editing| editing.field.clone());
        let value = match editing {
            Some(field) => div().w(px(88.)).child(field).into_any_element(),
            None => div()
                .id(SharedString::from(format!("inspect-{property:?}")))
                .w(px(88.))
                .px_1()
                .rounded_sm()
                .cursor_ns_resize()
                .hover(|style| style.bg(theme::hover()))
                .on_scroll_wheel(cx.listener(
                    move |inspector, event: &ScrollWheelEvent, window, cx| {
                        cx.stop_propagation();
                        let delta = event.delta.pixel_delta(window.line_height()).y;
                        let direction = match f32::from(delta) {
                            up if up > 0. => 1,
                            down if down < 0. => -1,
                            _ => return,
                        };
                        let steps = if event.modifiers.shift {
                            COARSE_STEPS
                        } else {
                            1
                        };
                        inspector.step(property, direction * steps, cx);
                    },
                ))
                .on_click(cx.listener(move |inspector, _: &ClickEvent, window, cx| {
                    inspector.start_editing(property, window, cx);
                }))
                .child(property.shown(raw))
                .into_any_element(),
        };
        let reset = div()
            .id(SharedString::from(format!("reset-{property:?}")))
            .w(px(16.))
            .text_color(theme::text_muted())
            .cursor_pointer();
        let reset = if raw == property.default_raw() {
            reset
        } else {
            reset
                .child("↺")
                .on_click(cx.listener(move |inspector, _: &ClickEvent, _, cx| {
                    inspector.reset(property, cx);
                }))
        };
        div()
            .h(px(ROW_HEIGHT))
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .flex_1()
                    .text_color(theme::text_muted())
                    .child(property.label()),
            )
            .child(value)
            .child(reset)
    }
}

impl Render for Inspector {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let selected = self.selected_pictures(cx);
        let panel = div()
            .size_full()
            .p_3()
            .flex()
            .flex_col()
            .gap_1()
            .bg(theme::panel())
            .text_xs()
            .child(div().text_sm().pb_1().child("Picture"));
        let Some(first) = selected.first() else {
            return panel.child(
                div()
                    .text_color(theme::text_muted())
                    .child(NOTHING_SELECTED),
            );
        };
        let subject = match selected.len() {
            1 => "1 clip".to_owned(),
            count => format!("{count} clips"),
        };
        panel
            .child(div().text_color(theme::text_muted()).pb_1().child(subject))
            .children(Property::ALL.map(|property| self.row(property, first, cx)))
            .child(
                div()
                    .id("reset-picture")
                    .mt_2()
                    .px_2()
                    .py_1()
                    .rounded_sm()
                    .border_1()
                    .border_color(theme::border())
                    .cursor_pointer()
                    .hover(|style| style.bg(theme::hover()))
                    .on_click(cx.listener(|inspector, _: &ClickEvent, _, cx| {
                        inspector.reset_all(cx);
                    }))
                    .child("Reset Picture"),
            )
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZero;

    use gpui::{TestAppContext, VisualTestContext};
    use tessera_timeline::{MediaInfo, Stream, Time, VideoStream};

    use super::*;
    use crate::playhead::Playhead;

    #[test]
    fn values_show_and_parse_in_their_units() {
        let x = Property::Transform(TransformField::X);
        let scale = Property::Transform(TransformField::Scale);
        let rotation = Property::Transform(TransformField::Rotation);

        assert_eq!(x.shown(-12), "-12 px");
        assert_eq!(scale.shown(1_000), "100%");
        assert_eq!(scale.shown(1_255), "125.5%");
        assert_eq!(rotation.shown(-5), "-0.5°");
        assert_eq!(Property::Opacity.shown(500), "50%");
        assert_eq!(scale.parsed("125.5"), Some(1_255));
        assert_eq!(scale.parsed(" 80 % "), Some(800));
        assert_eq!(rotation.parsed("-90°"), Some(-900));
        assert_eq!(x.parsed("twelve"), None);
        assert_eq!(x.parsed("NaN"), None);
    }

    fn inspected(
        cx: &mut TestAppContext,
    ) -> (
        Entity<Inspector>,
        Entity<TimelinePanel>,
        &mut VisualTestContext,
        Clip,
    ) {
        let mut project = Project::new("inspect");
        let asset = project.add_asset(
            "a.mkv".into(),
            MediaInfo {
                duration: Some(Time::from_seconds(8)),
                streams: vec![Stream::Video(VideoStream::new(
                    0,
                    "h264",
                    NonZero::new(1920).unwrap(),
                    NonZero::new(1080).unwrap(),
                ))],
            },
        );
        let clip = project.place_clip(asset, 0, Time::ZERO).unwrap();
        let project = cx.new(|_| project);
        let editor = cx.update(|cx| ProjectEditor::new(project.clone(), cx));
        let timeline_editor = editor.clone();
        let (timeline, cx) = cx.add_window_view(|_, cx| {
            let playhead = cx.new(|cx| Playhead::new(project.clone(), cx));
            TimelinePanel::new(timeline_editor, playhead, cx)
        });
        let inspector =
            cx.update(|_, cx| cx.new(|cx| Inspector::new(editor, timeline.clone(), cx)));
        (inspector, timeline, cx, clip)
    }

    fn clip_now(inspector: &Entity<Inspector>, cx: &mut VisualTestContext, clip: &Clip) -> Clip {
        cx.read(|cx| {
            *inspector
                .read(cx)
                .project
                .read(cx)
                .find_clip(clip.id)
                .unwrap()
                .1
        })
    }

    #[gpui::test]
    fn stepping_setting_and_resetting_the_selected_clip(cx: &mut TestAppContext) {
        let (inspector, timeline, cx, clip) = inspected(cx);
        let scale = Property::Transform(TransformField::Scale);

        inspector.update(cx, |inspector, cx| inspector.step(scale, 5, cx));

        assert_eq!(
            clip_now(&inspector, cx, &clip).transform,
            Transform::IDENTITY
        );

        timeline.update(cx, TimelinePanel::select_all);
        inspector.update(cx, |inspector, cx| {
            inspector.step(scale, -25, cx);
            inspector.set(Property::Opacity, |_| 400, cx);
        });
        let changed = clip_now(&inspector, cx, &clip);

        assert_eq!(changed.transform.scale, 750);
        assert_eq!(changed.opacity.permille(), 400);

        inspector.update(cx, |inspector, cx| inspector.reset(scale, cx));

        assert_eq!(clip_now(&inspector, cx, &clip).transform.scale, 1_000);

        inspector.update(cx, Inspector::reset_all);

        assert_eq!(clip_now(&inspector, cx, &clip).opacity, Opacity::OPAQUE);
    }
}
