use gpui::{
    AnyView, App, AppContext, ClickEvent, Context, ElementId, Entity, InteractiveElement,
    IntoElement, ParentElement, Render, ScrollWheelEvent, SharedString, StatefulInteractiveElement,
    Styled, Window, div, prelude::FluentBuilder, px,
};
use tessera_timeline::{Gain, Timeline, Track, TrackHeight, TrackKind};

use super::{
    COMPACT_TRACK_HEIGHT, RenameTarget, TALL_TRACK_HEIGHT, TRACK_HEIGHT, TimelinePanel, TrackFlag,
};
use crate::{text_field::TextField, theme};

pub const HEADER_PADDING: f32 = 8.;
pub const HEADER_BUTTON_SIZE: f32 = 16.;
pub const HEADER_BUTTON_GAP: f32 = 2.;
pub const ADD_ROW_HEIGHT: f32 = 28.;
pub const ADD_BUTTON_WIDTH: f32 = 36.;
pub const ADD_BUTTON_GAP: f32 = 4.;
pub const HEADER_LINE_HEIGHT: f32 = 24.;

const REMOVE_BLOCKED_BY_CLIPS: &str = "Remove the clips on this track first";
const REMOVE_BLOCKED_BY_LAST: &str = "The timeline needs at least one track of each kind";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackRow {
    pub index: usize,
    pub label: String,
    pub above: Option<usize>,
    pub below: Option<usize>,
    pub removable: bool,
    pub remove_hint: &'static str,
    pub kind: TrackKind,
    pub height: TrackHeight,
    pub locked: bool,
    pub muted: bool,
    pub solo: bool,
    pub volume: Gain,
}

pub const VOLUME_STEP_TENTHS: i32 = Gain::TENTHS_PER_DECIBEL;

pub fn row_height(height: TrackHeight) -> f32 {
    match height {
        TrackHeight::Compact => COMPACT_TRACK_HEIGHT,
        TrackHeight::Normal => TRACK_HEIGHT,
        TrackHeight::Tall => TALL_TRACK_HEIGHT,
    }
}

fn next_height(height: TrackHeight) -> TrackHeight {
    match height {
        TrackHeight::Compact => TrackHeight::Normal,
        TrackHeight::Normal => TrackHeight::Tall,
        TrackHeight::Tall => TrackHeight::Compact,
    }
}

pub fn next_height_of(track: &Track) -> TrackHeight {
    next_height(track.height)
}

pub fn track_rows(timeline: &Timeline) -> Vec<TrackRow> {
    let labels: Vec<_> = track_labels(&timeline.tracks).collect();
    let order = display_order(&timeline.tracks);
    let same_kind_at = |row: Option<usize>, kind: TrackKind| {
        row.and_then(|row| order.get(row))
            .copied()
            .filter(|&index| timeline.tracks[index].kind == kind)
    };
    order
        .iter()
        .enumerate()
        .map(|(row, &index)| {
            let track = &timeline.tracks[index];
            let kind = track.kind;
            let removal = timeline.check_removable(index);
            TrackRow {
                index,
                label: if track.name.is_empty() {
                    labels[index].clone()
                } else {
                    track.name.clone()
                },
                above: same_kind_at(row.checked_sub(1), kind),
                below: same_kind_at(Some(row + 1), kind),
                removable: removal.is_ok(),
                remove_hint: match removal {
                    Ok(_) => "Remove track",
                    Err(tessera_timeline::EditError::TrackNotEmpty(_)) => REMOVE_BLOCKED_BY_CLIPS,
                    Err(_) => REMOVE_BLOCKED_BY_LAST,
                },
                kind,
                height: track.height,
                locked: track.locked,
                muted: track.muted,
                solo: track.solo,
                volume: track.volume,
            }
        })
        .collect()
}

pub fn display_order(tracks: &[Track]) -> Vec<usize> {
    let of_kind = |kind| {
        tracks
            .iter()
            .enumerate()
            .filter(move |(_, track)| track.kind == kind)
            .map(|(index, _)| index)
    };
    of_kind(TrackKind::Video)
        .rev()
        .chain(of_kind(TrackKind::Audio))
        .collect()
}

pub fn content_height(tracks: &[Track]) -> f32 {
    tracks
        .iter()
        .map(|track| row_height(track.height))
        .sum::<f32>()
        + ADD_ROW_HEIGHT
}

fn track_labels(tracks: &[Track]) -> impl Iterator<Item = String> {
    let mut video = 0;
    let mut audio = 0;
    tracks.iter().map(move |track| match track.kind {
        TrackKind::Video => {
            video += 1;
            format!("V{video}")
        }
        TrackKind::Audio => {
            audio += 1;
            format!("A{audio}")
        }
    })
}

pub fn track_header(
    row: TrackRow,
    editing: Option<&Entity<TextField>>,
    cx: &Context<TimelinePanel>,
) -> impl IntoElement {
    let TrackRow {
        index,
        label,
        above,
        below,
        removable,
        remove_hint,
        kind,
        height,
        locked,
        muted,
        solo,
        volume,
    } = row;
    let swap_with = |neighbour: Option<usize>| {
        neighbour.map(|other| {
            cx.listener(move |panel: &mut TimelinePanel, _: &ClickEvent, _, cx| {
                panel.swap_tracks(index, other, cx);
            })
        })
    };
    let remove = removable.then(|| {
        cx.listener(move |panel: &mut TimelinePanel, _: &ClickEvent, _, cx| {
            panel.remove_track(index, cx);
        })
    });
    let flag = |flag: TrackFlag| {
        Some(
            cx.listener(move |panel: &mut TimelinePanel, _: &ClickEvent, _, cx| {
                panel.toggle_track(index, flag, cx);
            }),
        )
    };
    let resize = Some(
        cx.listener(move |panel: &mut TimelinePanel, _: &ClickEvent, _, cx| {
            panel.cycle_track_height(index, cx);
        }),
    );
    let (mute_label, mute_hint) = match kind {
        TrackKind::Video => ("H", "Hide the track"),
        TrackKind::Audio => ("M", "Mute the track"),
    };
    let toggles = div()
        .flex()
        .gap(px(HEADER_BUTTON_GAP))
        .child(
            header_button(
                ("track-lock", index),
                "L",
                flag(TrackFlag::Locked),
                "Lock the track",
            )
            .when(locked, active_button),
        )
        .child(
            header_button(
                ("track-mute", index),
                mute_label,
                flag(TrackFlag::Muted),
                mute_hint,
            )
            .when(muted, active_button),
        )
        .when(kind == TrackKind::Audio, |toggles| {
            toggles.child(
                header_button(
                    ("track-solo", index),
                    "S",
                    flag(TrackFlag::Solo),
                    "Solo the track",
                )
                .when(solo, active_button),
            )
        })
        .child(header_button(
            ("track-height", index),
            "↕",
            resize,
            "Change the track height",
        ));
    let reorder = div()
        .flex()
        .gap(px(HEADER_BUTTON_GAP))
        .child(header_button(
            ("track-up", index),
            "↑",
            swap_with(above),
            "Move the track up",
        ))
        .child(header_button(
            ("track-down", index),
            "↓",
            swap_with(below),
            "Move the track down",
        ))
        .child(header_button(
            ("track-remove", index),
            "×",
            remove,
            remove_hint,
        ));
    let name = match editing {
        Some(field) => div()
            .flex_1()
            .min_w_0()
            .child(field.clone())
            .into_any_element(),
        None => div()
            .id(("track-name", index))
            .flex_1()
            .min_w_0()
            .truncate()
            .on_click(cx.listener(
                move |panel: &mut TimelinePanel, event: &ClickEvent, window, cx| {
                    if event.click_count() == 2 {
                        panel.start_rename(RenameTarget::Track(index), window, cx);
                    }
                },
            ))
            .child(label)
            .into_any_element(),
    };
    let (first_line_end, second_line) = if height == TrackHeight::Compact {
        (toggles, None)
    } else {
        (reorder, Some(toggles))
    };
    div()
        .h(px(row_height(height)))
        .flex_none()
        .px(px(HEADER_PADDING))
        .flex()
        .flex_col()
        .justify_center()
        .border_b_1()
        .border_color(theme::border())
        .text_color(theme::text_muted())
        .child(
            div()
                .h(px(HEADER_LINE_HEIGHT))
                .flex()
                .items_center()
                .justify_between()
                .child(name)
                .child(first_line_end),
        )
        .children(second_line.map(|toggles| {
            div()
                .h(px(HEADER_LINE_HEIGHT))
                .flex()
                .items_center()
                .justify_between()
                .child(toggles)
                .when(kind == TrackKind::Audio, |line| {
                    line.child(volume_readout(index, volume, cx))
                })
        }))
}

fn volume_readout(index: usize, volume: Gain, cx: &Context<TimelinePanel>) -> impl IntoElement {
    div()
        .id(("track-volume", index))
        .text_xs()
        .cursor_ns_resize()
        .tooltip(tooltip(
            "Scroll to change the volume, double-click to reset it",
        ))
        .on_scroll_wheel(cx.listener(
            move |panel: &mut TimelinePanel, event: &ScrollWheelEvent, window, cx| {
                cx.stop_propagation();
                let delta = event.delta.pixel_delta(window.line_height()).y;
                let step = match f32::from(delta) {
                    up if up > 0. => VOLUME_STEP_TENTHS,
                    down if down < 0. => -VOLUME_STEP_TENTHS,
                    _ => return,
                };
                panel.adjust_track_volume(index, step, cx);
            },
        ))
        .on_click(cx.listener(
            move |panel: &mut TimelinePanel, event: &ClickEvent, _, cx| {
                if event.click_count() == 2 {
                    panel.reset_track_volume(index, cx);
                }
            },
        ))
        .child(volume.to_string())
}

fn active_button(button: gpui::Stateful<gpui::Div>) -> gpui::Stateful<gpui::Div> {
    button.bg(theme::selection()).text_color(theme::text())
}

struct TextTooltip(SharedString);

impl Render for TextTooltip {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .rounded_sm()
            .border_1()
            .border_color(theme::border())
            .bg(theme::panel())
            .text_xs()
            .text_color(theme::text())
            .child(self.0.clone())
    }
}

fn tooltip(text: &'static str) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    move |_, cx| cx.new(|_| TextTooltip(text.into())).into()
}

pub fn add_track_row(cx: &Context<TimelinePanel>) -> impl IntoElement {
    let add = |kind| {
        Some(
            cx.listener(move |panel: &mut TimelinePanel, _: &ClickEvent, _, cx| {
                panel.add_track(kind, cx);
            }),
        )
    };
    div()
        .h(px(ADD_ROW_HEIGHT))
        .flex_none()
        .px(px(HEADER_PADDING))
        .flex()
        .items_center()
        .gap(px(ADD_BUTTON_GAP))
        .child(
            header_button(
                "add-video-track",
                "+ V",
                add(TrackKind::Video),
                "Add a video track",
            )
            .w(px(ADD_BUTTON_WIDTH)),
        )
        .child(
            header_button(
                "add-audio-track",
                "+ A",
                add(TrackKind::Audio),
                "Add an audio track",
            )
            .w(px(ADD_BUTTON_WIDTH)),
        )
}

fn header_button(
    id: impl Into<ElementId>,
    label: &'static str,
    on_click: Option<impl Fn(&ClickEvent, &mut Window, &mut gpui::App) + 'static>,
    hint: &'static str,
) -> gpui::Stateful<gpui::Div> {
    let enabled = on_click.is_some();
    div()
        .id(id)
        .size(px(HEADER_BUTTON_SIZE))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded_sm()
        .text_xs()
        .tooltip(tooltip(hint))
        .when(!enabled, |button| button.text_color(theme::border()))
        .when_some(on_click, |button, on_click| {
            button
                .cursor_pointer()
                .hover(|style| style.bg(theme::hover()).text_color(theme::text()))
                .on_click(on_click)
        })
        .child(label)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timeline(kinds: &[TrackKind]) -> Timeline {
        Timeline {
            tracks: kinds.iter().copied().map(Track::new).collect(),
        }
    }

    #[test]
    fn tracks_are_numbered_per_kind() {
        let tracks = [TrackKind::Video, TrackKind::Audio, TrackKind::Video].map(Track::new);
        let labels: Vec<_> = track_labels(&tracks).collect();
        assert_eq!(labels, ["V1", "A1", "V2"]);
    }

    #[test]
    fn higher_video_tracks_are_drawn_above_and_audio_below() {
        use TrackKind::{Audio, Video};
        let tracks = timeline(&[Video, Audio, Video, Audio, Video]).tracks;
        assert_eq!(display_order(&tracks), [4, 2, 0, 1, 3]);
    }

    #[test]
    fn rows_only_offer_swaps_within_a_kind() {
        use TrackKind::{Audio, Video};
        let rows = track_rows(&timeline(&[Video, Video, Audio]));
        let summary: Vec<_> = rows
            .iter()
            .map(|row| (row.label.as_str(), row.above, row.below, row.removable))
            .collect();
        assert_eq!(
            summary,
            [
                ("V2", None, Some(0), true),
                ("V1", Some(1), None, true),
                ("A1", None, None, false),
            ]
        );
    }

    #[test]
    fn rows_show_a_custom_name_instead_of_the_numbered_label() {
        let mut timeline = timeline(&[TrackKind::Video, TrackKind::Audio]);
        timeline.tracks[0].name = "Cutaways".into();

        let labels: Vec<_> = track_rows(&timeline)
            .into_iter()
            .map(|row| row.label)
            .collect();

        assert_eq!(labels, ["Cutaways", "A1"]);
    }

    #[test]
    fn a_disabled_remove_button_says_why() {
        use TrackKind::{Audio, Video};
        let mut timeline = timeline(&[Video, Video, Audio]);
        let mut clip_track = Track::new(Video);
        clip_track
            .insert(tessera_timeline::Clip {
                id: tessera_timeline::ClipId(0),
                asset: tessera_timeline::AssetId(0),
                source: tessera_timeline::TimeRange::new(
                    tessera_timeline::Time::ZERO,
                    tessera_timeline::Time::from_seconds(1),
                ),
                start: tessera_timeline::Time::ZERO,
                link: None,
                gain: tessera_timeline::Gain::UNITY,
                audio_stream: None,
            })
            .unwrap();
        timeline.tracks[1] = clip_track;

        let hints: Vec<_> = track_rows(&timeline)
            .into_iter()
            .map(|row| row.remove_hint)
            .collect();

        assert_eq!(
            hints,
            [
                REMOVE_BLOCKED_BY_CLIPS,
                "Remove track",
                REMOVE_BLOCKED_BY_LAST
            ]
        );
    }

    #[test]
    fn heights_cycle_and_add_up() {
        let mut timeline = timeline(&[TrackKind::Video, TrackKind::Audio]);
        timeline.tracks[0].height = TrackHeight::Tall;
        timeline.tracks[1].height = TrackHeight::Compact;

        assert_eq!(
            content_height(&timeline.tracks),
            TALL_TRACK_HEIGHT + COMPACT_TRACK_HEIGHT + ADD_ROW_HEIGHT
        );
        assert_eq!(next_height(TrackHeight::Compact), TrackHeight::Normal);
        assert_eq!(next_height(TrackHeight::Normal), TrackHeight::Tall);
        assert_eq!(next_height(TrackHeight::Tall), TrackHeight::Compact);
    }
}
