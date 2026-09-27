use gpui::{
    App, Bounds, Context, DispatchPhase, Entity, Hitbox, HitboxBehavior, IntoElement, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement, Pixels, Render, Rgba,
    SharedString, Styled, Window, canvas, div, fill, point, px, size,
};
use tessera_timeline::{
    Clip, FLICKS_PER_SECOND, FrameRate, Project, Time, Timecode, Track, TrackKind,
};

use crate::{playhead::Playhead, theme};

const PIXELS_PER_SECOND: f32 = 48.;
const TRACK_HEADER_WIDTH: f32 = 96.;
const TRACK_HEIGHT: f32 = 48.;
const RULER_HEIGHT: f32 = 24.;
const MIN_MAJOR_TICK_SPACING: f32 = 96.;
const MAJOR_TICK_HEIGHT: f32 = 10.;
const MINOR_TICK_HEIGHT: f32 = 4.;
const TICK_WIDTH: f32 = 1.;
const LABEL_INSET: f32 = 4.;
const PLAYHEAD_WIDTH: f32 = 1.;
const PLAYHEAD_CAP_WIDTH: f32 = 9.;
const PLAYHEAD_CAP_HEIGHT: f32 = 8.;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RulerStep {
    seconds: i64,
    subdivisions: i64,
}

impl RulerStep {
    const fn new(seconds: i64, subdivisions: i64) -> Self {
        Self {
            seconds,
            subdivisions,
        }
    }
}

const COARSEST_RULER_STEP: RulerStep = RulerStep::new(3600, 4);
const RULER_STEPS: [RulerStep; 13] = [
    RulerStep::new(1, 4),
    RulerStep::new(2, 4),
    RulerStep::new(5, 5),
    RulerStep::new(10, 5),
    RulerStep::new(15, 3),
    RulerStep::new(30, 6),
    RulerStep::new(60, 4),
    RulerStep::new(120, 4),
    RulerStep::new(300, 5),
    RulerStep::new(600, 5),
    RulerStep::new(900, 3),
    RulerStep::new(1800, 6),
    COARSEST_RULER_STEP,
];

pub struct TimelinePanel {
    project: Entity<Project>,
    playhead: Entity<Playhead>,
    scrubbing: bool,
}

impl TimelinePanel {
    pub fn new(
        project: Entity<Project>,
        playhead: Entity<Playhead>,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&project, |_, _, cx| cx.notify()).detach();
        cx.observe(&playhead, |_, _, cx| cx.notify()).detach();
        Self {
            project,
            playhead,
            scrubbing: false,
        }
    }

    fn scrub_to(&mut self, offset: Pixels, cx: &mut Context<Self>) {
        let frame_rate = self.project.read(cx).settings.frame_rate;
        let time = frame_rate.frame_start(time_at(offset));
        self.playhead
            .update(cx, |playhead, cx| playhead.seek(time, cx));
    }
}

impl Render for TimelinePanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let panel = cx.entity();
        let playhead = self.playhead.read(cx).time();
        let project = self.project.read(cx);
        let frame_rate = project.settings.frame_rate;
        let tracks = &project.timeline.tracks;
        div()
            .size_full()
            .flex()
            .bg(theme::panel())
            .child(
                div()
                    .w(px(TRACK_HEADER_WIDTH))
                    .flex_none()
                    .flex()
                    .flex_col()
                    .border_r_1()
                    .border_color(theme::border())
                    .child(timecode_readout(Timecode::new(playhead, frame_rate)))
                    .children(track_labels(tracks).map(track_header)),
            )
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .flex()
                    .flex_col()
                    .child(ruler(panel, frame_rate))
                    .children(tracks.iter().map(track_lane))
                    .child(playhead_marker(playhead)),
            )
    }
}

fn timecode_readout(timecode: Timecode) -> impl IntoElement {
    div()
        .h(px(RULER_HEIGHT))
        .flex_none()
        .px_2()
        .flex()
        .items_center()
        .border_b_1()
        .border_color(theme::border())
        .text_xs()
        .child(timecode.to_string())
}

fn track_header(label: String) -> impl IntoElement {
    div()
        .h(px(TRACK_HEIGHT))
        .flex_none()
        .px_3()
        .flex()
        .items_center()
        .border_b_1()
        .border_color(theme::border())
        .text_color(theme::text_muted())
        .child(label)
}

fn track_lane(track: &Track) -> impl IntoElement {
    let color = match track.kind {
        TrackKind::Video => theme::video_clip(),
        TrackKind::Audio => theme::audio_clip(),
    };
    div()
        .h(px(TRACK_HEIGHT))
        .flex_none()
        .relative()
        .border_b_1()
        .border_color(theme::border())
        .children(track.clips().iter().map(|clip| clip_block(clip, color)))
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

fn clip_block(clip: &Clip, color: Rgba) -> impl IntoElement {
    let range = clip.timeline_range();
    div()
        .absolute()
        .top(px(4.))
        .bottom(px(4.))
        .left(x_at(range.start))
        .w(x_at(range.duration))
        .rounded_sm()
        .bg(color)
}

fn ruler(panel: Entity<TimelinePanel>, frame_rate: FrameRate) -> impl IntoElement {
    div()
        .h(px(RULER_HEIGHT))
        .flex_none()
        .border_b_1()
        .border_color(theme::border())
        .text_xs()
        .text_color(theme::text_muted())
        .child(
            canvas(
                |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
                move |bounds, hitbox, window, cx| {
                    paint_ticks(bounds, frame_rate, window, cx);
                    listen_for_scrub(panel, bounds, hitbox, window);
                },
            )
            .size_full(),
        )
}

fn paint_ticks(bounds: Bounds<Pixels>, frame_rate: FrameRate, window: &mut Window, cx: &mut App) {
    let step = ruler_step(PIXELS_PER_SECOND);
    let minor_spacing = step.seconds as f32 * PIXELS_PER_SECOND / step.subdivisions as f32;
    let text_style = window.text_style();
    let font_size = text_style.font_size.to_pixels(window.rem_size());
    let line_height = window.line_height();
    let offsets = (0..)
        .map(|index| (index, px(minor_spacing * index as f32)))
        .take_while(|&(_, offset)| offset <= bounds.size.width);
    for (index, offset) in offsets {
        let x = bounds.left() + offset;
        let major = index % step.subdivisions == 0;
        let height = px(if major {
            MAJOR_TICK_HEIGHT
        } else {
            MINOR_TICK_HEIGHT
        });
        window.paint_quad(fill(
            Bounds::new(
                point(x, bounds.bottom() - height),
                size(px(TICK_WIDTH), height),
            ),
            text_style.color,
        ));
        if major {
            let time = Time::from_seconds(step.seconds * (index / step.subdivisions));
            let label = SharedString::from(Timecode::new(time, frame_rate).to_string());
            let run = text_style.to_run(label.len());
            let line = window
                .text_system()
                .shape_line(label, font_size, &[run], None);
            let origin = point(x + px(LABEL_INSET), bounds.top());
            if let Err(error) = line.paint(origin, line_height, window, cx) {
                tracing::warn!(%error, "failed to paint a ruler label");
            }
        }
    }
}

fn listen_for_scrub(
    panel: Entity<TimelinePanel>,
    bounds: Bounds<Pixels>,
    hitbox: Hitbox,
    window: &mut Window,
) {
    window.on_mouse_event({
        let panel = panel.clone();
        move |event: &MouseDownEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble
                && event.button == MouseButton::Left
                && hitbox.is_hovered(window)
            {
                panel.update(cx, |panel, cx| {
                    panel.scrubbing = true;
                    panel.scrub_to(event.position.x - bounds.left(), cx);
                });
            }
        }
    });
    window.on_mouse_event({
        let panel = panel.clone();
        move |event: &MouseMoveEvent, phase, _, cx| {
            if phase == DispatchPhase::Bubble && panel.read(cx).scrubbing {
                panel.update(cx, |panel, cx| {
                    if event.dragging() {
                        panel.scrub_to(event.position.x - bounds.left(), cx);
                    } else {
                        panel.scrubbing = false;
                    }
                });
            }
        }
    });
    window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
        if phase == DispatchPhase::Bubble && event.button == MouseButton::Left {
            panel.update(cx, |panel, _| panel.scrubbing = false);
        }
    });
}

fn playhead_marker(time: Time) -> impl IntoElement {
    div()
        .absolute()
        .top_0()
        .bottom_0()
        .left(x_at(time) - px(PLAYHEAD_CAP_WIDTH / 2.))
        .w(px(PLAYHEAD_CAP_WIDTH))
        .flex()
        .flex_col()
        .items_center()
        .child(
            div()
                .w_full()
                .h(px(PLAYHEAD_CAP_HEIGHT))
                .flex_none()
                .rounded_b_sm()
                .bg(theme::playhead()),
        )
        .child(div().flex_1().w(px(PLAYHEAD_WIDTH)).bg(theme::playhead()))
}

fn ruler_step(pixels_per_second: f32) -> RulerStep {
    RULER_STEPS
        .into_iter()
        .find(|step| step.seconds as f32 * pixels_per_second >= MIN_MAJOR_TICK_SPACING)
        .unwrap_or(COARSEST_RULER_STEP)
}

fn x_at(time: Time) -> Pixels {
    px(time.as_seconds_f64() as f32 * PIXELS_PER_SECOND)
}

fn time_at(offset: Pixels) -> Time {
    let seconds = f64::from(f32::from(offset)) / f64::from(PIXELS_PER_SECOND);
    Time::from_flicks((seconds * FLICKS_PER_SECOND as f64).round() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ruler_step_keeps_major_ticks_apart() {
        assert_eq!(ruler_step(MIN_MAJOR_TICK_SPACING), RULER_STEPS[0]);
        assert_eq!(ruler_step(PIXELS_PER_SECOND), RulerStep::new(2, 4));
        assert_eq!(
            ruler_step(MIN_MAJOR_TICK_SPACING / 7.),
            RulerStep::new(10, 5)
        );
        assert_eq!(ruler_step(0.001), COARSEST_RULER_STEP);
    }

    #[test]
    fn offsets_and_times_convert_both_ways() {
        let time = Time::from_seconds(3);
        assert_eq!(x_at(time), px(3. * PIXELS_PER_SECOND));
        assert_eq!(time_at(x_at(time)), time);
        assert_eq!(
            time_at(px(PIXELS_PER_SECOND / 2.)),
            Time::from_flicks(FLICKS_PER_SECOND / 2)
        );
    }

    #[test]
    fn tracks_are_numbered_per_kind() {
        let tracks = [TrackKind::Video, TrackKind::Audio, TrackKind::Video].map(Track::new);
        let labels: Vec<_> = track_labels(&tracks).collect();
        assert_eq!(labels, ["V1", "A1", "V2"]);
    }
}
