use std::num::NonZeroU32;

use gpui::{
    AppContext, Context, Entity, EventEmitter, InteractiveElement, IntoElement, ParentElement,
    Render, StatefulInteractiveElement, Styled, Subscription, Window, div, prelude::FluentBuilder,
    px,
};
use tessera_timeline::{FrameRate, SequenceSettings};

use crate::{
    text_field::{TextField, TextFieldEvent},
    theme,
};

const MAX_SIDE: u32 = 16_384;
const SAMPLE_RATES: [u32; 4] = [44_100, 48_000, 88_200, 96_000];
const DIALOG_WIDTH: f32 = 380.;
const FIELD_WIDTH: f32 = 72.;
const FIELD_HEIGHT: f32 = 24.;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SequenceDialogEvent {
    Apply(SequenceSettings),
    Cancelled,
}

pub struct SequenceSettingsDialog {
    width: Entity<TextField>,
    height: Entity<TextField>,
    frame_rate: FrameRate,
    sample_rate: NonZeroU32,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<SequenceDialogEvent> for SequenceSettingsDialog {}

impl SequenceSettingsDialog {
    pub fn new(settings: SequenceSettings, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let width = cx.new(|cx| TextField::new(settings.width.to_string(), "Width", cx));
        let height = cx.new(|cx| TextField::new(settings.height.to_string(), "Height", cx));
        let subscriptions = vec![
            cx.subscribe_in(&width, window, |dialog, _, event, window, cx| match event {
                TextFieldEvent::Changed(_) => cx.notify(),
                TextFieldEvent::Submitted(_) => dialog.height.read(cx).focus(window),
                TextFieldEvent::Cancelled => cx.emit(SequenceDialogEvent::Cancelled),
            }),
            cx.subscribe_in(&height, window, |dialog, _, event, _, cx| match event {
                TextFieldEvent::Changed(_) => cx.notify(),
                TextFieldEvent::Submitted(_) => dialog.apply(cx),
                TextFieldEvent::Cancelled => cx.emit(SequenceDialogEvent::Cancelled),
            }),
        ];
        width.read(cx).focus(window);
        Self {
            width,
            height,
            frame_rate: settings.frame_rate,
            sample_rate: settings.sample_rate,
            _subscriptions: subscriptions,
        }
    }

    pub fn select_frame_rate(&mut self, frame_rate: FrameRate, cx: &mut Context<Self>) {
        self.frame_rate = frame_rate;
        cx.notify();
    }

    pub fn select_sample_rate(&mut self, sample_rate: NonZeroU32, cx: &mut Context<Self>) {
        self.sample_rate = sample_rate;
        cx.notify();
    }

    fn side(
        field: &Entity<TextField>,
        name: &'static str,
        cx: &Context<Self>,
    ) -> Result<NonZeroU32, String> {
        let text = field.read(cx).value().trim().to_owned();
        text.parse::<u32>()
            .ok()
            .and_then(NonZeroU32::new)
            .filter(|side| side.get() <= MAX_SIDE)
            .ok_or_else(|| format!("{name} must be a whole number from 1 to {MAX_SIDE}"))
    }

    fn settings(&self, cx: &Context<Self>) -> Result<SequenceSettings, String> {
        Ok(SequenceSettings {
            width: Self::side(&self.width, "Width", cx)?,
            height: Self::side(&self.height, "Height", cx)?,
            frame_rate: self.frame_rate,
            sample_rate: self.sample_rate,
        })
    }

    fn apply(&mut self, cx: &mut Context<Self>) {
        if let Ok(settings) = self.settings(cx) {
            cx.emit(SequenceDialogEvent::Apply(settings));
        }
    }
}

fn rate_label(rate: f64) -> String {
    let text = format!("{rate:.3}");
    text.trim_end_matches('0').trim_end_matches('.').to_owned()
}

pub(crate) fn choice(
    id: impl Into<gpui::ElementId>,
    label: String,
    selected: bool,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .px_2()
        .rounded_sm()
        .cursor_pointer()
        .border_1()
        .border_color(if selected {
            theme::selection()
        } else {
            theme::border()
        })
        .hover(|style| style.bg(theme::hover()))
        .child(label)
}

impl Render for SequenceSettingsDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let settings = self.settings(cx);
        let frame_rates = FrameRate::STANDARD
            .into_iter()
            .enumerate()
            .map(|(index, rate)| {
                choice(
                    ("frame-rate", index),
                    rate_label(rate.as_f64()),
                    rate == self.frame_rate,
                )
                .on_click(cx.listener(move |dialog, _, _, cx| dialog.select_frame_rate(rate, cx)))
            });
        let sample_rates = SAMPLE_RATES.into_iter().enumerate().map(|(index, rate)| {
            let rate = NonZeroU32::new(rate).expect("the offered sample rates are not zero");
            choice(
                ("sample-rate", index),
                format!("{} kHz", rate_label(f64::from(rate.get()) / 1000.)),
                rate == self.sample_rate,
            )
            .on_click(cx.listener(move |dialog, _, _, cx| dialog.select_sample_rate(rate, cx)))
        });
        let labelled = |label: &'static str, content: gpui::Div| {
            div()
                .flex()
                .gap_2()
                .items_start()
                .child(
                    div()
                        .w(px(88.))
                        .text_color(theme::text_muted())
                        .child(label),
                )
                .child(content)
        };
        let field = |field: &Entity<TextField>| {
            div()
                .w(px(FIELD_WIDTH))
                .h(px(FIELD_HEIGHT))
                .child(field.clone())
        };
        div()
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
                    .child("Sequence settings"),
            )
            .child(labelled(
                "Size",
                div()
                    .flex()
                    .gap_2()
                    .items_center()
                    .child(field(&self.width))
                    .child("×")
                    .child(field(&self.height)),
            ))
            .child(labelled(
                "Frame rate",
                div().flex().flex_wrap().gap_1().children(frame_rates),
            ))
            .child(labelled(
                "Sample rate",
                div().flex().flex_wrap().gap_1().children(sample_rates),
            ))
            .children(
                settings
                    .as_ref()
                    .err()
                    .map(|error| div().text_color(theme::error()).child(error.clone())),
            )
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(choice("sequence-cancel", "Cancel".into(), false).on_click(
                        cx.listener(|_, _, _, cx| cx.emit(SequenceDialogEvent::Cancelled)),
                    ))
                    .child(
                        choice("sequence-apply", "Apply".into(), settings.is_ok())
                            .when(settings.is_err(), |button| {
                                button.text_color(theme::border())
                            })
                            .on_click(cx.listener(|dialog, _, _, cx| dialog.apply(cx))),
                    ),
            )
    }
}
