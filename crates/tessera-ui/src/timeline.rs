mod viewport;

use gpui::{
    App, Bounds, Context, DispatchPhase, DragMoveEvent, Entity, Hitbox, HitboxBehavior,
    InteractiveElement, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    ParentElement, Pixels, Render, Rgba, ScrollWheelEvent, SharedString, Styled, Window, canvas,
    div, fill, point, px, size,
};
use tessera_timeline::{
    AssetId, Clip, FrameRate, PlaceClipError, Project, Time, TimeRange, Timecode, Track, TrackKind,
};

use self::viewport::Viewport;
use crate::{
    media_bin::{DraggedAsset, file_name},
    playhead::Playhead,
    theme,
};

const ZOOM_STEP: f32 = 2.;
const WHEEL_PIXELS_PER_DOUBLING: f32 = 120.;
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
const CLIP_INSET: f32 = 4.;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RulerSpan {
    Frames(i64),
    Seconds(i64),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RulerStep {
    span: RulerSpan,
    subdivisions: i64,
}

impl RulerStep {
    const fn frames(frames: i64, subdivisions: i64) -> Self {
        Self {
            span: RulerSpan::Frames(frames),
            subdivisions,
        }
    }

    const fn seconds(seconds: i64, subdivisions: i64) -> Self {
        Self {
            span: RulerSpan::Seconds(seconds),
            subdivisions,
        }
    }

    fn major(self, frame_rate: FrameRate) -> Time {
        match self.span {
            RulerSpan::Frames(frames) => frame_rate.frame_to_time(frames),
            RulerSpan::Seconds(seconds) => Time::from_seconds(seconds),
        }
    }

    fn minor(self, frame_rate: FrameRate) -> Time {
        Time::from_flicks(self.major(frame_rate).flicks() / self.subdivisions)
    }
}

const COARSEST_RULER_STEP: RulerStep = RulerStep::seconds(3600, 4);
const RULER_STEPS: [RulerStep; 17] = [
    RulerStep::frames(1, 1),
    RulerStep::frames(2, 2),
    RulerStep::frames(5, 5),
    RulerStep::frames(10, 5),
    RulerStep::seconds(1, 4),
    RulerStep::seconds(2, 4),
    RulerStep::seconds(5, 5),
    RulerStep::seconds(10, 5),
    RulerStep::seconds(15, 3),
    RulerStep::seconds(30, 6),
    RulerStep::seconds(60, 4),
    RulerStep::seconds(120, 4),
    RulerStep::seconds(300, 5),
    RulerStep::seconds(600, 5),
    RulerStep::seconds(900, 3),
    RulerStep::seconds(1800, 6),
    COARSEST_RULER_STEP,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DropPreview {
    track: usize,
    range: TimeRange,
    fits: bool,
}

pub struct TimelinePanel {
    project: Entity<Project>,
    playhead: Entity<Playhead>,
    scrubbing: bool,
    drop_preview: Option<DropPreview>,
    viewport: Viewport,
    lanes: Bounds<Pixels>,
}

impl TimelinePanel {
    pub fn new(
        project: Entity<Project>,
        playhead: Entity<Playhead>,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&project, |_, _, cx| cx.notify()).detach();
        cx.observe(&playhead, |panel, _, cx| {
            panel.follow_playhead(cx);
            cx.notify();
        })
        .detach();
        Self {
            project,
            playhead,
            scrubbing: false,
            drop_preview: None,
            viewport: Viewport::default(),
            lanes: Bounds::default(),
        }
    }

    pub fn zoom_in(&mut self, cx: &mut Context<Self>) {
        self.zoom_around_playhead(ZOOM_STEP, cx);
    }

    pub fn zoom_out(&mut self, cx: &mut Context<Self>) {
        self.zoom_around_playhead(ZOOM_STEP.recip(), cx);
    }

    pub fn zoom_to_fit(&mut self, cx: &mut Context<Self>) {
        let duration = self.project.read(cx).timeline.duration();
        self.set_viewport(Viewport::fitting(duration, self.lanes.size.width), cx);
    }

    fn zoom_around_playhead(&mut self, factor: f32, cx: &mut Context<Self>) {
        let playhead = self.playhead.read(cx).time();
        let anchor = self
            .viewport
            .x_at(playhead)
            .clamp(px(0.), self.lanes.size.width);
        let viewport = self.viewport.zoomed(factor, anchor, self.scroll_limit(cx));
        self.set_viewport(viewport, cx);
    }

    fn scroll_wheel(&mut self, event: &ScrollWheelEvent, window: &Window, cx: &mut Context<Self>) {
        let delta = event.delta.pixel_delta(window.line_height());
        let limit = self.scroll_limit(cx);
        let viewport = if event.modifiers.control {
            let factor = 2_f32.powf(f32::from(delta.y) / WHEEL_PIXELS_PER_DOUBLING);
            let anchor = event.position.x - self.lanes.left();
            self.viewport.zoomed(factor, anchor, limit)
        } else {
            self.viewport.scrolled_by(-(delta.x + delta.y), limit)
        };
        self.set_viewport(viewport, cx);
        cx.stop_propagation();
    }

    fn follow_playhead(&mut self, cx: &App) {
        let followed_width = self.lanes.size.width - px(PLAYHEAD_CAP_WIDTH);
        if self.scrubbing || followed_width <= px(0.) {
            return;
        }
        let playhead = self.playhead.read(cx).time();
        self.viewport = self
            .viewport
            .following(playhead, followed_width, self.scroll_limit(cx));
    }

    fn scroll_limit(&self, cx: &App) -> Time {
        let duration = self.project.read(cx).timeline.duration();
        duration.max(self.playhead.read(cx).time())
    }

    fn set_viewport(&mut self, viewport: Viewport, cx: &mut Context<Self>) {
        if viewport != self.viewport {
            self.viewport = viewport;
            cx.notify();
        }
    }

    fn drag_over_lane(
        &mut self,
        track: usize,
        event: &DragMoveEvent<DraggedAsset>,
        cx: &mut Context<Self>,
    ) {
        let position = event.event.position;
        let preview = if event.bounds.contains(&position) {
            let asset = event.drag(cx).id;
            self.preview_drop(asset, track, position.x - event.bounds.left(), cx)
        } else if self
            .drop_preview
            .is_some_and(|preview| preview.track == track)
        {
            None
        } else {
            return;
        };
        if preview != self.drop_preview {
            self.drop_preview = preview;
            cx.notify();
        }
    }

    fn preview_drop(
        &self,
        asset: AssetId,
        track: usize,
        offset: Pixels,
        cx: &App,
    ) -> Option<DropPreview> {
        let project = self.project.read(cx);
        let start = project
            .settings
            .frame_rate
            .frame_start(self.viewport.time_at(offset));
        let (range, fits) = match project.clip_for(asset, track, start) {
            Ok(clip) => (clip.timeline_range(), true),
            Err(PlaceClipError::Overlapping(overlap)) => (overlap.inserted, false),
            Err(_) => return None,
        };
        Some(DropPreview { track, range, fits })
    }

    fn drop_on_lane(&mut self, track: usize, dragged: &DraggedAsset, cx: &mut Context<Self>) {
        let Some(preview) = self
            .drop_preview
            .take()
            .filter(|preview| preview.track == track && preview.fits)
        else {
            cx.notify();
            return;
        };
        self.project.update(cx, |project, cx| {
            match project.place_clip(dragged.id, track, preview.range.start) {
                Ok(_) => cx.notify(),
                Err(error) => tracing::warn!(%error, "could not place the clip"),
            }
        });
        cx.notify();
    }

    fn scrub_to(&mut self, offset: Pixels, cx: &mut Context<Self>) {
        let frame_rate = self.project.read(cx).settings.frame_rate;
        let time = frame_rate.frame_start(self.viewport.time_at(offset));
        self.playhead
            .update(cx, |playhead, cx| playhead.seek(time, cx));
    }
}

impl Render for TimelinePanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !cx.has_active_drag() {
            self.drop_preview = None;
        }
        let panel = cx.entity();
        let drop_preview = self.drop_preview;
        let viewport = self.viewport;
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
                    .on_scroll_wheel(cx.listener(|panel, event, window, cx| {
                        panel.scroll_wheel(event, window, cx);
                    }))
                    .child(ruler(panel, viewport, frame_rate))
                    .children(tracks.iter().enumerate().map(|(index, track)| {
                        track_lane(index, track, project, viewport, drop_preview, cx)
                    }))
                    .child(playhead_marker(viewport.x_at(playhead))),
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

fn track_lane(
    index: usize,
    track: &Track,
    project: &Project,
    viewport: Viewport,
    drop_preview: Option<DropPreview>,
    cx: &Context<TimelinePanel>,
) -> impl IntoElement {
    let color = match track.kind {
        TrackKind::Video => theme::video_clip(),
        TrackKind::Audio => theme::audio_clip(),
    };
    let ghost = drop_preview
        .filter(|preview| preview.track == index)
        .map(|preview| drop_ghost(preview, viewport));
    div()
        .h(px(TRACK_HEIGHT))
        .flex_none()
        .relative()
        .border_b_1()
        .border_color(theme::border())
        .on_drag_move(cx.listener(move |panel, event, _, cx| {
            panel.drag_over_lane(index, event, cx);
        }))
        .on_drop(cx.listener(move |panel, dragged, _, cx| {
            panel.drop_on_lane(index, dragged, cx);
        }))
        .children(
            track
                .clips()
                .iter()
                .map(|clip| clip_block(clip, clip_label(project, clip), color, viewport)),
        )
        .children(ghost)
}

fn clip_label(project: &Project, clip: &Clip) -> SharedString {
    project
        .asset(clip.asset)
        .map(|asset| file_name(&asset.path))
        .unwrap_or_default()
}

fn clip_frame(range: TimeRange, viewport: Viewport) -> gpui::Div {
    div()
        .absolute()
        .top(px(CLIP_INSET))
        .bottom(px(CLIP_INSET))
        .left(viewport.x_at(range.start))
        .w(viewport.width_of(range.duration))
        .rounded_sm()
}

fn drop_ghost(preview: DropPreview, viewport: Viewport) -> impl IntoElement {
    let color = if preview.fits {
        theme::drop_ghost()
    } else {
        theme::drop_ghost_blocked()
    };
    clip_frame(preview.range, viewport).bg(color)
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

fn clip_block(
    clip: &Clip,
    label: SharedString,
    color: Rgba,
    viewport: Viewport,
) -> impl IntoElement {
    clip_frame(clip.timeline_range(), viewport)
        .overflow_hidden()
        .px_1()
        .bg(color)
        .text_xs()
        .text_color(theme::text())
        .child(div().truncate().child(label))
}

fn ruler(
    panel: Entity<TimelinePanel>,
    viewport: Viewport,
    frame_rate: FrameRate,
) -> impl IntoElement {
    div()
        .h(px(RULER_HEIGHT))
        .flex_none()
        .border_b_1()
        .border_color(theme::border())
        .text_xs()
        .text_color(theme::text_muted())
        .child(
            canvas(
                {
                    let panel = panel.clone();
                    move |bounds, window, cx| {
                        panel.update(cx, |panel, _| panel.lanes = bounds);
                        window.insert_hitbox(bounds, HitboxBehavior::Normal)
                    }
                },
                move |bounds, hitbox, window, cx| {
                    paint_ticks(bounds, viewport, frame_rate, window, cx);
                    listen_for_scrub(panel, bounds, hitbox, window);
                },
            )
            .size_full(),
        )
}

fn paint_ticks(
    bounds: Bounds<Pixels>,
    viewport: Viewport,
    frame_rate: FrameRate,
    window: &mut Window,
    cx: &mut App,
) {
    let step = ruler_step(viewport.pixels_per_second(), frame_rate);
    let minor = step.minor(frame_rate);
    let text_style = window.text_style();
    let font_size = text_style.font_size.to_pixels(window.rem_size());
    let line_height = window.line_height();
    let first = viewport.start().flicks().div_euclid(minor.flicks());
    let ticks = (first..)
        .map(|index| (index, minor * index))
        .take_while(|&(_, time)| viewport.x_at(time) <= bounds.size.width);
    for (index, time) in ticks {
        let x = bounds.left() + viewport.x_at(time);
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

fn playhead_marker(x: Pixels) -> impl IntoElement {
    div()
        .absolute()
        .top_0()
        .bottom_0()
        .left(x - px(PLAYHEAD_CAP_WIDTH / 2.))
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

fn ruler_step(pixels_per_second: f32, frame_rate: FrameRate) -> RulerStep {
    RULER_STEPS
        .into_iter()
        .find(|step| {
            step.major(frame_rate).as_seconds_f64() as f32 * pixels_per_second
                >= MIN_MAJOR_TICK_SPACING
        })
        .unwrap_or(COARSEST_RULER_STEP)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ruler_step_keeps_major_ticks_apart() {
        let rate = FrameRate::FPS_30;
        assert_eq!(
            ruler_step(MIN_MAJOR_TICK_SPACING * 31., rate),
            RULER_STEPS[0]
        );
        assert_eq!(
            ruler_step(MIN_MAJOR_TICK_SPACING * 3.5, rate),
            RulerStep::frames(10, 5)
        );
        assert_eq!(
            ruler_step(MIN_MAJOR_TICK_SPACING * 1.1, rate),
            RulerStep::seconds(1, 4)
        );
        assert_eq!(ruler_step(48., rate), RulerStep::seconds(2, 4));
        assert_eq!(
            ruler_step(MIN_MAJOR_TICK_SPACING / 7., rate),
            RulerStep::seconds(10, 5)
        );
        assert_eq!(ruler_step(0.001, rate), COARSEST_RULER_STEP);
    }

    #[test]
    fn ruler_subdivisions_are_exact() {
        for rate in [FrameRate::FPS_25, FrameRate::NTSC_30, FrameRate::NTSC_60] {
            for step in RULER_STEPS {
                assert_eq!(
                    step.minor(rate).flicks() * step.subdivisions,
                    step.major(rate).flicks(),
                    "{step:?} at {rate:?}"
                );
            }
        }
    }

    #[test]
    fn tracks_are_numbered_per_kind() {
        let tracks = [TrackKind::Video, TrackKind::Audio, TrackKind::Video].map(Track::new);
        let labels: Vec<_> = track_labels(&tracks).collect();
        assert_eq!(labels, ["V1", "A1", "V2"]);
    }
}
