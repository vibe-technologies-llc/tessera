use gpui::{
    App, Context, EventEmitter, FocusHandle, Focusable, InteractiveElement, IntoElement,
    ParentElement, Render, StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder,
    px,
};
use tessera_export::{Preset, Span};
use tessera_timeline::{Project, Timecode};

use crate::{
    Confirm, DIALOG_CONTEXT, Dismiss, SelectNext, SelectPrevious, sequence_dialog::choice, theme,
};

const DIALOG_WIDTH: f32 = 420.;
const LABEL_WIDTH: f32 = 88.;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportDialogEvent {
    Export { preset: Preset, span: Span },
    Cancelled,
}

pub struct ExportDialog {
    project: Project,
    preset: usize,
    span: Span,
    focus_handle: FocusHandle,
}

impl EventEmitter<ExportDialogEvent> for ExportDialog {}

impl Focusable for ExportDialog {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl ExportDialog {
    pub fn new(
        project: Project,
        preset: Preset,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle);
        let span = if Span::InToOut.range(&project).is_some() {
            Span::InToOut
        } else {
            Span::Timeline
        };
        Self {
            project,
            preset: Preset::ALL
                .iter()
                .position(|known| *known == preset)
                .unwrap_or(0),
            span,
            focus_handle,
        }
    }

    pub fn preset(&self) -> Preset {
        Preset::ALL[self.preset]
    }

    fn select_preset(&mut self, index: usize, cx: &mut Context<Self>) {
        self.preset = index.min(Preset::ALL.len() - 1);
        cx.notify();
    }

    fn select_by(&mut self, step: isize, cx: &mut Context<Self>) {
        self.select_preset(self.preset.saturating_add_signed(step), cx);
    }

    fn select_span(&mut self, span: Span, cx: &mut Context<Self>) {
        if span.range(&self.project).is_some() {
            self.span = span;
            cx.notify();
        }
    }

    fn confirm(&mut self, cx: &mut Context<Self>) {
        if self.span.range(&self.project).is_some() {
            cx.emit(ExportDialogEvent::Export {
                preset: self.preset(),
                span: self.span,
            });
        }
    }

    fn summary(&self) -> Option<String> {
        let settings = self.project.settings;
        let range = self.span.range(&self.project)?;
        Some(format!(
            "{}×{} · {:.3} fps · {}",
            settings.width,
            settings.height,
            settings.frame_rate.as_f64(),
            Timecode::new(range.duration, settings.frame_rate)
        ))
    }
}

fn span_label(span: Span) -> &'static str {
    match span {
        Span::Timeline => "Whole timeline",
        Span::InToOut => "In to out",
    }
}

impl Render for ExportDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let presets = Preset::ALL.iter().enumerate().map(|(index, preset)| {
            choice(
                ("export-preset", index),
                preset.label(),
                index == self.preset,
            )
            .on_click(cx.listener(move |dialog, _, _, cx| dialog.select_preset(index, cx)))
        });
        let spans = [Span::Timeline, Span::InToOut]
            .into_iter()
            .enumerate()
            .map(|(index, span)| {
                let available = span.range(&self.project).is_some();
                choice(
                    ("export-span", index),
                    span_label(span).into(),
                    span == self.span,
                )
                .when(!available, |button| button.text_color(theme::border()))
                .on_click(cx.listener(move |dialog, _, _, cx| dialog.select_span(span, cx)))
            });
        let labelled = |label: &'static str, content: gpui::Div| {
            div()
                .flex()
                .gap_2()
                .items_start()
                .child(
                    div()
                        .w(px(LABEL_WIDTH))
                        .flex_none()
                        .text_color(theme::text_muted())
                        .child(label),
                )
                .child(content)
        };
        let summary = self.summary();
        let ready = summary.is_some();
        div()
            .id("export-dialog")
            .track_focus(&self.focus_handle)
            .key_context(DIALOG_CONTEXT)
            .on_action(cx.listener(|dialog, _: &SelectNext, _, cx| dialog.select_by(1, cx)))
            .on_action(cx.listener(|dialog, _: &SelectPrevious, _, cx| dialog.select_by(-1, cx)))
            .on_action(cx.listener(|dialog, _: &Confirm, _, cx| dialog.confirm(cx)))
            .on_action(cx.listener(|_, _: &Dismiss, _, cx| cx.emit(ExportDialogEvent::Cancelled)))
            .w(px(DIALOG_WIDTH))
            .p_4()
            .flex()
            .flex_col()
            .gap_3()
            .rounded_md()
            .border_1()
            .border_color(theme::border())
            .bg(theme::panel())
            .text_color(theme::text())
            .text_sm()
            .child(
                div()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child("Export"),
            )
            .child(labelled(
                "Format",
                div().flex().flex_wrap().gap_1().children(presets),
            ))
            .child(labelled(
                "Range",
                div().flex().flex_wrap().gap_1().children(spans),
            ))
            .child(labelled(
                "Output",
                div()
                    .text_color(theme::text_muted())
                    .child(summary.unwrap_or_else(|| "Nothing to export".into())),
            ))
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        choice("export-cancel", "Cancel".into(), false).on_click(
                            cx.listener(|_, _, _, cx| cx.emit(ExportDialogEvent::Cancelled)),
                        ),
                    )
                    .child(
                        choice("export-confirm", "Export…".into(), ready)
                            .when(!ready, |button| button.text_color(theme::border()))
                            .on_click(cx.listener(|dialog, _, _, cx| dialog.confirm(cx))),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, rc::Rc};

    use gpui::TestAppContext;
    use tessera_media::{Container, VideoCodec};
    use tessera_timeline::Time;

    use super::*;

    fn events(
        dialog: &gpui::Entity<ExportDialog>,
        cx: &mut gpui::VisualTestContext,
    ) -> Rc<RefCell<Vec<ExportDialogEvent>>> {
        let events = Rc::new(RefCell::new(Vec::new()));
        cx.update(|_, cx| {
            let events = events.clone();
            cx.subscribe(dialog, move |_, event: &ExportDialogEvent, _| {
                events.borrow_mut().push(*event);
            })
            .detach();
        });
        events
    }

    #[gpui::test]
    fn arrow_keys_pick_a_format_and_enter_exports_the_marked_range(cx: &mut TestAppContext) {
        cx.update(crate::init);
        let mut project = Project::new("export");
        project.set_in_point(Time::from_seconds(1)).unwrap();
        project.set_out_point(Time::from_seconds(3)).unwrap();
        let (dialog, cx) = cx.add_window_view(|window, cx| {
            ExportDialog::new(project, Preset::default(), window, cx)
        });
        let events = events(&dialog, cx);

        cx.simulate_keystrokes("down down enter escape");

        assert_eq!(
            *events.borrow(),
            [
                ExportDialogEvent::Export {
                    preset: Preset::new(VideoCodec::Av1, Container::Mp4),
                    span: Span::InToOut,
                },
                ExportDialogEvent::Cancelled
            ]
        );
    }

    #[gpui::test]
    fn an_empty_timeline_cannot_be_exported(cx: &mut TestAppContext) {
        cx.update(crate::init);
        let (dialog, cx) = cx.add_window_view(|window, cx| {
            ExportDialog::new(Project::new("empty"), Preset::default(), window, cx)
        });
        let events = events(&dialog, cx);

        cx.simulate_keystrokes("enter");

        assert!(events.borrow().is_empty());
        assert_eq!(cx.read(|cx| dialog.read(cx).span), Span::Timeline);
    }
}
