use gpui::{
    ClickEvent, Context, ElementId, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder, px,
};
use tessera_timeline::{Timeline, Track, TrackKind};

use super::{TRACK_HEIGHT, TimelinePanel};
use crate::theme;

pub const HEADER_PADDING: f32 = 8.;
pub const HEADER_BUTTON_SIZE: f32 = 16.;
pub const HEADER_BUTTON_GAP: f32 = 2.;
pub const ADD_ROW_HEIGHT: f32 = 28.;
pub const ADD_BUTTON_WIDTH: f32 = 36.;
pub const ADD_BUTTON_GAP: f32 = 4.;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackRow {
    pub index: usize,
    pub label: String,
    pub above: Option<usize>,
    pub below: Option<usize>,
    pub removable: bool,
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
            let kind = timeline.tracks[index].kind;
            TrackRow {
                index,
                label: labels[index].clone(),
                above: same_kind_at(row.checked_sub(1), kind),
                below: same_kind_at(Some(row + 1), kind),
                removable: timeline.check_removable(index).is_ok(),
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

pub fn content_height(rows: usize) -> f32 {
    rows as f32 * TRACK_HEIGHT + ADD_ROW_HEIGHT
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

pub fn track_header(row: TrackRow, cx: &Context<TimelinePanel>) -> impl IntoElement {
    let TrackRow {
        index,
        label,
        above,
        below,
        removable,
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
    div()
        .h(px(TRACK_HEIGHT))
        .flex_none()
        .px(px(HEADER_PADDING))
        .flex()
        .items_center()
        .justify_between()
        .border_b_1()
        .border_color(theme::border())
        .text_color(theme::text_muted())
        .child(label)
        .child(
            div()
                .flex()
                .gap(px(HEADER_BUTTON_GAP))
                .child(header_button(("track-up", index), "↑", swap_with(above)))
                .child(header_button(("track-down", index), "↓", swap_with(below)))
                .child(header_button(("track-remove", index), "×", remove)),
        )
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
            header_button("add-video-track", "+ V", add(TrackKind::Video)).w(px(ADD_BUTTON_WIDTH)),
        )
        .child(
            header_button("add-audio-track", "+ A", add(TrackKind::Audio)).w(px(ADD_BUTTON_WIDTH)),
        )
}

fn header_button(
    id: impl Into<ElementId>,
    label: &'static str,
    on_click: Option<impl Fn(&ClickEvent, &mut Window, &mut gpui::App) + 'static>,
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
}
