use gpui::{Context, Entity, IntoElement, ParentElement, Render, Rgba, Styled, Window, div, px};
use tessera_timeline::{Clip, Project, Track, TrackKind};

use crate::theme;

const PIXELS_PER_SECOND: f32 = 48.;
const TRACK_HEADER_WIDTH: f32 = 96.;
const TRACK_HEIGHT: f32 = 48.;

pub struct TimelinePanel {
    project: Entity<Project>,
}

impl TimelinePanel {
    pub fn new(project: Entity<Project>, cx: &mut Context<Self>) -> Self {
        cx.observe(&project, |_, _, cx| cx.notify()).detach();
        Self { project }
    }
}

impl Render for TimelinePanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tracks = &self.project.read(cx).timeline.tracks;
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme::panel())
            .children(numbered(tracks).map(|(label, track)| track_row(label, track)))
    }
}

fn numbered(tracks: &[Track]) -> impl Iterator<Item = (String, &Track)> {
    let mut video = 0;
    let mut audio = 0;
    tracks.iter().map(move |track| {
        let label = match track.kind {
            TrackKind::Video => {
                video += 1;
                format!("V{video}")
            }
            TrackKind::Audio => {
                audio += 1;
                format!("A{audio}")
            }
        };
        (label, track)
    })
}

fn track_row(label: String, track: &Track) -> impl IntoElement {
    let color = match track.kind {
        TrackKind::Video => theme::video_clip(),
        TrackKind::Audio => theme::audio_clip(),
    };
    div()
        .h(px(TRACK_HEIGHT))
        .flex()
        .border_b_1()
        .border_color(theme::border())
        .child(
            div()
                .w(px(TRACK_HEADER_WIDTH))
                .flex_none()
                .px_3()
                .flex()
                .items_center()
                .border_r_1()
                .border_color(theme::border())
                .text_color(theme::text_muted())
                .child(label),
        )
        .child(
            div()
                .relative()
                .flex_1()
                .overflow_hidden()
                .children(track.clips().iter().map(|clip| clip_block(clip, color))),
        )
}

fn clip_block(clip: &Clip, color: Rgba) -> impl IntoElement {
    let range = clip.timeline_range();
    div()
        .absolute()
        .top(px(4.))
        .bottom(px(4.))
        .left(px(range.start.as_seconds_f64() as f32 * PIXELS_PER_SECOND))
        .w(px(
            range.duration.as_seconds_f64() as f32 * PIXELS_PER_SECOND
        ))
        .rounded_sm()
        .bg(color)
}
