mod header;
mod scrollbar;
mod snap;
mod viewport;

use std::collections::BTreeSet;

use gpui::{
    App, AppContext, Bounds, Context, CursorStyle, DispatchPhase, DragMoveEvent, Entity,
    FocusHandle, Hitbox, HitboxBehavior, InteractiveElement, IntoElement, Modifiers, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement, Pixels, Point, Render, Rgba,
    ScrollWheelEvent, SharedString, StatefulInteractiveElement, Styled, Subscription, Window,
    canvas, div, fill, point, prelude::FluentBuilder, px, size,
};
use tessera_timeline::{
    AssetId, Clip, ClipEdge, ClipId, Command, EditError, FrameRate, Gain, Marker, MarkerId,
    Project, Time, TimeRange, Timecode, Timeline, Track, TrackKind,
};

use self::{
    header::{
        add_track_row, content_height, display_order, next_height_of, row_height, track_header,
        track_rows,
    },
    scrollbar::{Axis, SCROLLBAR_THICKNESS, Thumb, scrollbar},
    snap::{Snap, SnapTargets},
    viewport::Viewport,
};
use crate::{
    editor::ProjectEditor,
    media_bin::{DraggedAsset, file_name},
    playhead::Playhead,
    text_field::{TextField, TextFieldEvent},
    theme,
};

const ZOOM_STEP: f32 = 2.;
const WHEEL_PIXELS_PER_DOUBLING: f32 = 120.;
const TRACK_HEADER_WIDTH: f32 = 128.;
const TRACK_HEIGHT: f32 = 48.;
const COMPACT_TRACK_HEIGHT: f32 = 32.;
const TALL_TRACK_HEIGHT: f32 = 80.;
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
const TRIM_HANDLE_WIDTH: f32 = 6.;
const SNAP_DISTANCE: f32 = 8.;
const SNAP_LINE_WIDTH: f32 = 1.;
const MARKER_WIDTH: f32 = 9.;
const MARKER_HEIGHT: f32 = 10.;
const IN_OUT_EDGE_WIDTH: f32 = 2.;
const AUTOSCROLL_EDGE: f32 = 32.;
const AUTOSCROLL_MAX_STEP: f32 = 24.;
const AUTOSCROLL_TICK: std::time::Duration = std::time::Duration::from_millis(16);
const MARKER_LABEL_GAP: f32 = 2.;
const MARKER_RENAME_WIDTH: f32 = 140.;

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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum DropMode {
    #[default]
    Place,
    Insert,
    Overwrite,
}

impl DropMode {
    fn of(modifiers: Modifiers) -> Self {
        if modifiers.control {
            Self::Insert
        } else if modifiers.alt {
            Self::Overwrite
        } else {
            Self::Place
        }
    }

    fn command(self) -> Command {
        match self {
            Self::Place => Command::PlaceClip,
            Self::Insert => Command::InsertClip,
            Self::Overwrite => Command::OverwriteClip,
        }
    }
}

const OVERLAP_REASON: &str = "Overlaps another clip";
const ASSET_OVERLAP_REASON: &str = "Overlaps a clip: Ctrl inserts, Alt overwrites";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DropPreview {
    track: usize,
    range: TimeRange,
    fits: bool,
    reason: Option<&'static str>,
    mode: DropMode,
    snapped_to: Option<Time>,
    group: Option<GroupShift>,
    partner: Option<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct GroupShift {
    shift: Time,
    from: usize,
}

impl DropPreview {
    fn snapped(self, snap: Option<Snap>) -> Self {
        let snapped_to = snap
            .map(|snap| snap.target)
            .filter(|&target| target == self.range.start || target == self.range.end());
        Self { snapped_to, ..self }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TrackFlag {
    Locked,
    Muted,
    Solo,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Grip {
    Body,
    Edge(ClipEdge),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DraggedClip {
    id: ClipId,
    grip: Grip,
}

impl Render for DraggedClip {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

pub struct TimelinePanel {
    editor: ProjectEditor,
    project: Entity<Project>,
    playhead: Entity<Playhead>,
    scrubbing: bool,
    marker_drag: Option<MarkerDrag>,
    marquee: Option<Marquee>,
    drag_hover: Option<DragHover>,
    autoscroll_step: Pixels,
    autoscrolling: bool,
    scrolling: Option<(Axis, f32)>,
    drop_preview: Option<DropPreview>,
    viewport: Viewport,
    lanes: Bounds<Pixels>,
    selection: BTreeSet<ClipId>,
    grab: Pixels,
    track_scroll: Pixels,
    tracks_view: Bounds<Pixels>,
    snapping: bool,
    clipboard: Vec<(Clip, usize)>,
    renaming: Option<Rename>,
    focus_return: Option<FocusHandle>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct MarkerDrag {
    id: MarkerId,
    grab: Pixels,
    time: Time,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Dragging {
    Asset { id: AssetId, mode: DropMode },
    Clip(DraggedClip),
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct DragHover {
    track: usize,
    offset: Pixels,
    dragging: Dragging,
}

#[derive(Clone, Debug, PartialEq)]
struct Marquee {
    anchor: MarqueeCorner,
    reach: MarqueeCorner,
    base: BTreeSet<ClipId>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct MarqueeCorner {
    time: Time,
    content_y: Pixels,
}

impl Marquee {
    fn times(&self) -> (Time, Time) {
        let (a, b) = (self.anchor.time, self.reach.time);
        (a.min(b), a.max(b))
    }

    fn content_ys(&self) -> (Pixels, Pixels) {
        let (a, b) = (self.anchor.content_y, self.reach.content_y);
        (a.min(b), a.max(b))
    }

    fn enclosed(&self, timeline: &Timeline) -> BTreeSet<ClipId> {
        let (first, last) = self.times();
        let (top, bottom) = self.content_ys();
        let mut row_top = px(0.);
        let mut enclosed = self.base.clone();
        for index in display_order(&timeline.tracks) {
            let track = &timeline.tracks[index];
            let row_bottom = row_top + px(row_height(track.height));
            if row_top < bottom.max(top + px(1.)) && row_bottom > top {
                enclosed.extend(
                    track
                        .clips()
                        .iter()
                        .filter(|clip| {
                            let range = clip.timeline_range();
                            range.start <= last && range.end() > first
                        })
                        .map(|clip| clip.id),
                );
            }
            row_top = row_bottom;
        }
        enclosed
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RenameTarget {
    Track(usize),
    Marker(MarkerId),
}

struct Rename {
    target: RenameTarget,
    field: Entity<TextField>,
    _subscriptions: [Subscription; 2],
}

impl TimelinePanel {
    pub fn new(editor: ProjectEditor, playhead: Entity<Playhead>, cx: &mut Context<Self>) -> Self {
        let project = editor.project().clone();
        cx.observe(&project, |panel, _, cx| panel.project_changed(cx))
            .detach();
        cx.observe(&playhead, |panel, _, cx| {
            panel.follow_playhead(cx);
            cx.notify();
        })
        .detach();
        Self {
            editor,
            project,
            playhead,
            scrubbing: false,
            marker_drag: None,
            marquee: None,
            drag_hover: None,
            autoscroll_step: px(0.),
            autoscrolling: false,
            scrolling: None,
            drop_preview: None,
            viewport: Viewport::default(),
            lanes: Bounds::default(),
            selection: BTreeSet::new(),
            grab: px(0.),
            track_scroll: px(0.),
            tracks_view: Bounds::default(),
            snapping: true,
            clipboard: Vec::new(),
            renaming: None,
            focus_return: None,
        }
    }

    fn project_changed(&mut self, cx: &mut Context<Self>) {
        let project = self.project.read(cx);
        self.selection.retain(|id| project.find_clip(*id).is_some());
        self.viewport = self.viewport.clamped(self.scroll_limit(cx));
        cx.notify();
    }

    pub fn select_all(&mut self, cx: &mut Context<Self>) {
        self.selection = self
            .project
            .read(cx)
            .timeline
            .tracks
            .iter()
            .flat_map(|track| track.clips())
            .map(|clip| clip.id)
            .collect();
        cx.notify();
    }

    pub fn return_focus_to(&mut self, handle: FocusHandle) {
        self.focus_return = Some(handle);
    }

    fn start_rename(&mut self, target: RenameTarget, window: &mut Window, cx: &mut Context<Self>) {
        let project = self.project.read(cx);
        let (current, placeholder) = match target {
            RenameTarget::Track(track) => (
                project.timeline.tracks.get(track).map(|track| &track.name),
                "Track name",
            ),
            RenameTarget::Marker(id) => {
                (project.marker(id).map(|marker| &marker.name), "Marker name")
            }
        };
        let Some(current) = current.cloned() else {
            return;
        };
        let field = cx.new(|cx| TextField::new(current, placeholder, cx));
        let focus_handle = field.read(cx).focus_handle().clone();
        let subscriptions = [
            cx.subscribe_in(&field, window, |panel, _, event, window, cx| match event {
                TextFieldEvent::Changed(_) => {}
                TextFieldEvent::Submitted(name) => {
                    panel.finish_rename(Some(name.trim().to_owned()), window, cx);
                }
                TextFieldEvent::Cancelled => panel.finish_rename(None, window, cx),
            }),
            cx.on_focus_out(&focus_handle, window, |panel, _, window, cx| {
                panel.finish_rename(None, window, cx);
            }),
        ];
        field.read(cx).focus(window);
        self.renaming = Some(Rename {
            target,
            field,
            _subscriptions: subscriptions,
        });
        cx.notify();
    }

    fn finish_rename(&mut self, name: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(renaming) = self.renaming.take() else {
            return;
        };
        match (name, renaming.target) {
            (Some(name), RenameTarget::Track(track)) => {
                self.edit_track(Command::RenameTrack, track, cx, |track| track.name = name);
            }
            (Some(name), RenameTarget::Marker(id)) => {
                let renamed = self.editor.apply(Command::RenameMarker, cx, |project| {
                    project.rename_marker(id, name)
                });
                if let Err(error) = renamed {
                    tracing::warn!(%error, "could not rename the marker");
                }
            }
            (None, _) => {}
        }
        if let Some(handle) = &self.focus_return {
            window.focus(handle);
        }
        cx.notify();
    }

    pub fn drag_cancelled(&mut self, cx: &mut Context<Self>) {
        self.drop_preview = None;
        self.drag_hover = None;
        self.autoscroll_step = px(0.);
        self.marker_drag = None;
        self.marquee = None;
        cx.notify();
    }

    fn marker_at(&self, offset: Point<Pixels>, cx: &App) -> Option<(MarkerId, Time)> {
        if offset.y < px(RULER_HEIGHT - MARKER_HEIGHT) {
            return None;
        }
        let reach = px(MARKER_WIDTH / 2.);
        self.project
            .read(cx)
            .markers
            .iter()
            .map(|marker| (marker, (self.viewport.x_at(marker.time) - offset.x).abs()))
            .filter(|(_, distance)| *distance <= reach)
            .min_by(|(_, a), (_, b)| f32::from(*a).total_cmp(&f32::from(*b)))
            .map(|(marker, _)| (marker.id, marker.time))
    }

    fn press_ruler(
        &mut self,
        offset: Point<Pixels>,
        click_count: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.marker_at(offset, cx) {
            Some((id, _)) if click_count >= 2 => {
                self.marker_drag = None;
                self.start_rename(RenameTarget::Marker(id), window, cx);
            }
            Some((id, time)) => {
                self.marker_drag = Some(MarkerDrag {
                    id,
                    grab: offset.x - self.viewport.x_at(time),
                    time,
                });
            }
            None => {
                self.scrubbing = true;
                self.scrub_to(offset.x, cx);
            }
        }
    }

    fn drag_on_ruler(&mut self, offset: Pixels, cx: &mut Context<Self>) {
        if let Some(drag) = self.marker_drag {
            let time = self.frame_at(offset - drag.grab, cx).max(Time::ZERO);
            if time != drag.time {
                self.marker_drag = Some(MarkerDrag { time, ..drag });
                cx.notify();
            }
        } else if self.scrubbing {
            self.scrub_to(offset, cx);
        }
    }

    fn release_ruler(&mut self, cx: &mut Context<Self>) {
        self.scrubbing = false;
        let Some(drag) = self.marker_drag.take() else {
            return;
        };
        let moved = self
            .project
            .read(cx)
            .marker(drag.id)
            .is_some_and(|marker| marker.time != drag.time);
        if moved {
            let result = self.editor.apply(Command::MoveMarker, cx, |project| {
                project.move_marker(drag.id, drag.time)
            });
            if let Err(error) = result {
                tracing::warn!(%error, "could not move the marker");
            }
        }
        cx.notify();
    }

    fn ruler_pressed(&self) -> bool {
        self.scrubbing || self.marker_drag.is_some()
    }

    fn selected_clips(&self, cx: &App) -> Vec<(Clip, usize)> {
        let project = self.project.read(cx);
        let mut clips: Vec<(Clip, usize)> = self
            .selection
            .iter()
            .filter_map(|id| project.find_clip(*id))
            .map(|(track, clip)| (*clip, track))
            .collect();
        clips.sort_by_key(|(clip, track)| (clip.start, *track));
        clips
    }

    pub fn copy_selection(&mut self, cx: &mut Context<Self>) {
        let copied = self.selected_clips(cx);
        if !copied.is_empty() {
            self.clipboard = copied;
        }
    }

    pub fn cut_selection(&mut self, cx: &mut Context<Self>) {
        self.copy_selection(cx);
        self.delete_selection(cx);
    }

    pub fn paste(&mut self, cx: &mut Context<Self>) {
        let at = self.playhead.read(cx).time();
        self.paste_clips(self.clipboard.clone(), at, cx);
    }

    pub fn duplicate_selection(&mut self, cx: &mut Context<Self>) {
        let clips = self.selected_clips(cx);
        let end = clips
            .iter()
            .map(|(clip, _)| clip.timeline_range().end())
            .max();
        if let Some(end) = end {
            self.paste_clips(clips, end, cx);
        }
    }

    fn paste_clips(&mut self, clips: Vec<(Clip, usize)>, at: Time, cx: &mut Context<Self>) {
        let Some(anchor) = clips.iter().map(|(clip, _)| clip.start).min() else {
            return;
        };
        let pastes: Vec<(Clip, usize, Time)> = clips
            .iter()
            .map(|(clip, track)| (*clip, *track, at + (clip.start - anchor)))
            .collect();
        match self.editor.apply(Command::PasteClip, cx, |project| {
            project.paste_clips(&pastes)
        }) {
            Ok(pasted) => self.selection = pasted.iter().map(|clip| clip.id).collect(),
            Err(error) => tracing::warn!(%error, "could not paste the clips"),
        }
        cx.notify();
    }

    pub fn add_marker_at_playhead(&mut self, cx: &mut Context<Self>) {
        let time = self.playhead.read(cx).time();
        self.editor.perform(Command::AddMarker, cx, |project| {
            project.add_marker(time, "");
        });
    }

    pub fn remove_marker_at_playhead(&mut self, cx: &mut Context<Self>) {
        let time = self.playhead.read(cx).time();
        let frame_rate = self.project.read(cx).settings.frame_rate;
        let marker = self
            .project
            .read(cx)
            .markers
            .iter()
            .find(|marker| frame_rate.frame_start(marker.time) == time)
            .map(|marker| marker.id);
        if let Some(marker) = marker {
            let removed = self.editor.apply(Command::RemoveMarker, cx, |project| {
                project.remove_marker(marker)
            });
            if let Err(error) = removed {
                tracing::warn!(%error, "could not remove the marker");
            }
        }
    }

    pub fn set_in_point_at_playhead(&mut self, cx: &mut Context<Self>) {
        let time = self.playhead.read(cx).time();
        let set = self.editor.apply(Command::SetInPoint, cx, |project| {
            project.set_in_point(time)
        });
        if let Err(error) = set {
            tracing::warn!(%error, "could not set the in point");
        }
    }

    pub fn set_out_point_at_playhead(&mut self, cx: &mut Context<Self>) {
        let time = self.playhead.read(cx).time();
        let set = self.editor.apply(Command::SetOutPoint, cx, |project| {
            project.set_out_point(time)
        });
        if let Err(error) = set {
            tracing::warn!(%error, "could not set the out point");
        }
    }

    pub fn clear_in_out(&mut self, cx: &mut Context<Self>) {
        self.editor
            .perform(Command::ClearInOut, cx, Project::clear_in_out);
    }

    fn seek_to(&mut self, time: Option<Time>, cx: &mut Context<Self>) {
        if let Some(time) = time {
            self.playhead
                .update(cx, |playhead, cx| playhead.seek(time, cx));
        }
    }

    pub fn go_to_next_edit(&mut self, cx: &mut Context<Self>) {
        let at = self.playhead.read(cx).time();
        let next = self.project.read(cx).timeline.next_edit_after(at);
        self.seek_to(next, cx);
    }

    pub fn go_to_previous_edit(&mut self, cx: &mut Context<Self>) {
        let at = self.playhead.read(cx).time();
        let previous = self.project.read(cx).timeline.previous_edit_before(at);
        self.seek_to(previous, cx);
    }

    pub fn go_to_next_marker(&mut self, cx: &mut Context<Self>) {
        let at = self.playhead.read(cx).time();
        let next = self
            .project
            .read(cx)
            .next_marker_after(at)
            .map(|marker| marker.time);
        self.seek_to(next, cx);
    }

    pub fn go_to_previous_marker(&mut self, cx: &mut Context<Self>) {
        let at = self.playhead.read(cx).time();
        let previous = self
            .project
            .read(cx)
            .previous_marker_before(at)
            .map(|marker| marker.time);
        self.seek_to(previous, cx);
    }

    pub fn project_replaced(&mut self, cx: &mut Context<Self>) {
        self.selection.clear();
        self.drop_preview = None;
        self.renaming = None;
        self.marker_drag = None;
        self.marquee = None;
        self.clipboard.clear();
        self.viewport = Viewport::default();
        self.track_scroll = px(0.);
        cx.notify();
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

    pub fn toggle_snapping(&mut self, cx: &mut Context<Self>) {
        self.snapping = !self.snapping;
        cx.notify();
    }

    pub fn split_at_playhead(&mut self, cx: &mut Context<Self>) {
        let time = self.playhead.read(cx).time();
        let project = self.project.read(cx);
        let editable = |track: usize| !project.timeline.tracks[track].locked;
        let selected: Vec<ClipId> = self
            .selection
            .iter()
            .filter_map(|id| project.find_clip(*id))
            .filter(|(track, clip)| editable(*track) && clip.is_cut_by(time))
            .map(|(_, clip)| clip.id)
            .collect();
        let targets: Vec<ClipId> = if selected.is_empty() {
            project
                .timeline
                .tracks
                .iter()
                .filter(|track| !track.locked)
                .filter_map(|track| track.clip_at(time))
                .filter(|clip| clip.is_cut_by(time))
                .map(|clip| clip.id)
                .collect()
        } else {
            selected
        };
        let split = self.editor.apply(Command::SplitClips, cx, |project| {
            project.split_clips(&targets, time).map(drop)
        });
        if let Err(error) = split {
            tracing::warn!(%error, "could not split the clips");
        }
    }

    pub fn raise_clip_gain(&mut self, cx: &mut Context<Self>) {
        self.adjust_clip_gain(header::VOLUME_STEP_TENTHS, cx);
    }

    pub fn lower_clip_gain(&mut self, cx: &mut Context<Self>) {
        self.adjust_clip_gain(-header::VOLUME_STEP_TENTHS, cx);
    }

    fn adjust_clip_gain(&mut self, tenths: i32, cx: &mut Context<Self>) {
        let project = self.project.read(cx);
        let ids: Vec<ClipId> = self
            .selection
            .iter()
            .copied()
            .filter(|id| {
                project.find_clip(*id).is_some_and(|(track, _)| {
                    let track = &project.timeline.tracks[track];
                    track.kind == TrackKind::Audio && !track.locked
                })
            })
            .collect();
        if ids.is_empty() {
            return;
        }
        let adjusted = self.editor.apply(Command::SetClipGain, cx, |project| {
            project.adjust_clip_gains(&ids, tenths)
        });
        if let Err(error) = adjusted {
            tracing::warn!(%error, "could not change the clip gain");
        }
    }

    fn adjust_track_volume(&mut self, index: usize, tenths: i32, cx: &mut Context<Self>) {
        self.edit_track(Command::SetTrackVolume, index, cx, |track| {
            track.volume = track.volume.adjusted(tenths);
        });
    }

    fn reset_track_volume(&mut self, index: usize, cx: &mut Context<Self>) {
        self.edit_track(Command::SetTrackVolume, index, cx, |track| {
            track.volume = Gain::UNITY;
        });
    }

    pub fn toggle_link_selection(&mut self, cx: &mut Context<Self>) {
        let ids: Vec<ClipId> = self.selection.iter().copied().collect();
        if ids.is_empty() {
            return;
        }
        let project = self.project.read(cx);
        let any_linked = ids.iter().any(|&id| {
            project
                .find_clip(id)
                .is_some_and(|(_, clip)| clip.link.is_some())
        });
        let toggled = if any_linked {
            self.editor.apply(Command::UnlinkClips, cx, |project| {
                project.unlink_clips(&ids)
            })
        } else {
            self.editor
                .apply(Command::LinkClips, cx, |project| project.link_clips(&ids))
                .map(drop)
        };
        if let Err(error) = toggled {
            tracing::warn!(%error, "could not link or unlink the clips");
        }
    }

    pub fn delete_selection(&mut self, cx: &mut Context<Self>) {
        self.remove_selection(Command::DeleteClip, Project::delete_clips, cx);
    }

    pub fn ripple_delete_selection(&mut self, cx: &mut Context<Self>) {
        self.remove_selection(Command::RippleDeleteClip, Project::ripple_delete_clips, cx);
    }

    fn remove_selection(
        &mut self,
        command: Command,
        remove: impl FnOnce(&mut Project, &[ClipId]) -> Result<Vec<Clip>, EditError>,
        cx: &mut Context<Self>,
    ) {
        let project = self.project.read(cx);
        let ids: Vec<ClipId> = self
            .selection
            .iter()
            .copied()
            .filter(|id| {
                project
                    .find_clip(*id)
                    .is_some_and(|(track, _)| !project.timeline.tracks[track].locked)
            })
            .collect();
        if ids.is_empty() {
            return;
        }
        self.selection.retain(|id| !ids.contains(id));
        if let Err(error) = self
            .editor
            .apply(command, cx, |project| remove(project, &ids))
        {
            tracing::warn!(%error, "could not delete the clips");
        }
        cx.notify();
    }

    fn add_track(&mut self, kind: TrackKind, cx: &mut Context<Self>) {
        self.editor.perform(Command::AddTrack, cx, |project| {
            project.timeline.add_track(kind)
        });
    }

    fn remove_track(&mut self, index: usize, cx: &mut Context<Self>) {
        self.edit_tracks(Command::RemoveTrack, cx, |timeline| {
            timeline.remove_track(index).map(drop)
        });
    }

    fn swap_tracks(&mut self, first: usize, second: usize, cx: &mut Context<Self>) {
        self.edit_tracks(Command::SwapTracks, cx, |timeline| {
            timeline.swap_tracks(first, second)
        });
    }

    fn edit_tracks(
        &mut self,
        command: Command,
        cx: &mut Context<Self>,
        edit: impl FnOnce(&mut Timeline) -> Result<(), EditError>,
    ) {
        let edited = self
            .editor
            .apply(command, cx, |project| edit(&mut project.timeline));
        if let Err(error) = edited {
            tracing::warn!(%error, "could not change the tracks");
        }
    }

    fn scroll_tracks(&mut self, delta: Pixels, content: f32, cx: &mut Context<Self>) {
        let scroll = self.clamped_track_scroll(self.track_scroll - delta, content);
        if scroll != self.track_scroll {
            self.track_scroll = scroll;
            cx.notify();
        }
    }

    fn clamped_track_scroll(&self, scroll: Pixels, content: f32) -> Pixels {
        let overflow = px(content) - self.tracks_view.size.height;
        scroll.min(overflow).max(px(0.))
    }

    fn toggle_track(&mut self, index: usize, flag: TrackFlag, cx: &mut Context<Self>) {
        let command = match flag {
            TrackFlag::Locked => Command::LockTrack,
            TrackFlag::Muted => Command::MuteTrack,
            TrackFlag::Solo => Command::SoloTrack,
        };
        self.edit_track(command, index, cx, |track| {
            let value = match flag {
                TrackFlag::Locked => &mut track.locked,
                TrackFlag::Muted => &mut track.muted,
                TrackFlag::Solo => &mut track.solo,
            };
            *value = !*value;
        });
    }

    fn cycle_track_height(&mut self, index: usize, cx: &mut Context<Self>) {
        self.edit_track(Command::ResizeTrack, index, cx, |track| {
            track.height = next_height_of(track);
        });
    }

    fn edit_track(
        &mut self,
        command: Command,
        index: usize,
        cx: &mut Context<Self>,
        edit: impl FnOnce(&mut Track),
    ) {
        self.editor.perform(command, cx, |project| {
            if let Some(track) = project.timeline.tracks.get_mut(index) {
                edit(track);
            }
        });
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
        cx.stop_propagation();
        if event.modifiers.shift {
            let content = content_height(&self.project.read(cx).timeline.tracks);
            self.scroll_tracks(delta.x + delta.y, content, cx);
            return;
        }
        let limit = self.scroll_limit(cx);
        let viewport = if event.modifiers.control {
            let factor = 2_f32.powf(f32::from(delta.y) / WHEEL_PIXELS_PER_DOUBLING);
            let anchor = event.position.x - self.lanes.left();
            self.viewport.zoomed(factor, anchor, limit)
        } else {
            self.viewport.scrolled_by(-(delta.x + delta.y), limit)
        };
        self.set_viewport(viewport, cx);
        self.rehover(cx);
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

    fn press_lane(
        &mut self,
        track: usize,
        position: Point<Pixels>,
        extend: bool,
        cx: &mut Context<Self>,
    ) {
        let offset = position.x - self.lanes.left();
        let time = self.viewport.time_at(offset);
        let pressed = self
            .project
            .read(cx)
            .timeline
            .tracks
            .get(track)
            .and_then(|track| track.clip_at(time))
            .copied();
        if let Some(clip) = pressed {
            self.grab = offset - self.viewport.x_at(clip.start);
        }
        let before = self.selection.clone();
        let group = |clip: Clip| self.project.read(cx).with_partners(&[clip.id]);
        match pressed {
            Some(clip) if extend => {
                let group = group(clip);
                if self.selection.contains(&clip.id) {
                    for id in group {
                        self.selection.remove(&id);
                    }
                } else {
                    self.selection.extend(group);
                }
            }
            Some(clip) => {
                if !self.selection.contains(&clip.id) {
                    self.selection = group(clip).into_iter().collect();
                }
            }
            None => {
                if !extend {
                    self.selection.clear();
                }
                let corner = self.marquee_corner(position);
                self.marquee = Some(Marquee {
                    anchor: corner,
                    reach: corner,
                    base: self.selection.clone(),
                });
            }
        }
        if self.selection != before {
            cx.notify();
        }
    }

    fn marquee_corner(&self, position: Point<Pixels>) -> MarqueeCorner {
        MarqueeCorner {
            time: self.viewport.time_at(position.x - self.lanes.left()),
            content_y: position.y - self.tracks_view.top() + self.track_scroll,
        }
    }

    fn extend_marquee(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let reach = self.marquee_corner(position);
        let Some(marquee) = &mut self.marquee else {
            return;
        };
        if marquee.reach == reach {
            return;
        }
        marquee.reach = reach;
        let project = self.project.read(cx);
        let enclosed: Vec<ClipId> = marquee.enclosed(&project.timeline).into_iter().collect();
        self.selection = project.with_partners(&enclosed).into_iter().collect();
        cx.notify();
    }

    fn end_marquee(&mut self, cx: &mut Context<Self>) {
        if self.marquee.take().is_some() {
            cx.notify();
        }
    }

    fn drag_asset_over_lane(
        &mut self,
        track: usize,
        event: &DragMoveEvent<DraggedAsset>,
        cx: &mut Context<Self>,
    ) {
        let dragging = Dragging::Asset {
            id: event.drag(cx).id,
            mode: DropMode::of(event.event.modifiers),
        };
        self.drag_over_lane(track, dragging, event.event.position, event.bounds, cx);
    }

    fn drag_clip_over_lane(
        &mut self,
        track: usize,
        event: &DragMoveEvent<DraggedClip>,
        cx: &mut Context<Self>,
    ) {
        let dragging = Dragging::Clip(*event.drag(cx));
        self.drag_over_lane(track, dragging, event.event.position, event.bounds, cx);
    }

    fn drag_over_lane(
        &mut self,
        track: usize,
        dragging: Dragging,
        position: Point<Pixels>,
        lane: Bounds<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let trimming_here = match dragging {
            Dragging::Clip(DraggedClip {
                id,
                grip: Grip::Edge(_),
            }) => {
                let clip_track = self.project.read(cx).find_clip(id).map(|(track, _)| track);
                if clip_track != Some(track) {
                    return;
                }
                true
            }
            _ => false,
        };
        if !trimming_here && !lane.contains(&position) {
            if self
                .drop_preview
                .is_some_and(|preview| preview.track == track)
            {
                self.drag_hover = None;
                self.autoscroll_step = px(0.);
                self.show_preview(None, cx);
            }
            return;
        }
        let hover = DragHover {
            track,
            offset: position.x - lane.left(),
            dragging,
        };
        self.drag_hover = Some(hover);
        self.autoscroll_step = autoscroll_step(hover.offset, self.lanes.size.width);
        self.start_autoscroll(cx);
        let preview = self.hover_preview(hover, cx);
        self.show_preview(preview, cx);
    }

    fn hover_preview(&self, hover: DragHover, cx: &App) -> Option<DropPreview> {
        let DragHover {
            track,
            offset,
            dragging,
        } = hover;
        let project = self.project.read(cx);
        match dragging {
            Dragging::Asset { id, mode } => {
                let duration = project
                    .asset(id)
                    .and_then(|asset| asset.default_clip_duration());
                let (start, snap) =
                    self.snapped_start(self.frame_at(offset, cx), duration, &[], cx);
                asset_drop_preview(project, id, track, start, snap, mode)
            }
            Dragging::Clip(DraggedClip {
                id,
                grip: Grip::Body,
            }) => {
                let group: Vec<ClipId> = if self.selection.contains(&id) {
                    self.selection.iter().copied().collect()
                } else {
                    vec![id]
                };
                let (clip_track, clip) = project.find_clip(id)?;
                let start = self.frame_at(offset - self.grab, cx);
                let (start, snap) =
                    self.snapped_start(start, Some(clip.source.duration), &group, cx);
                if group.len() == 1 {
                    return placement(
                        track,
                        project.moved_clip(id, track, start),
                        snap,
                        DropMode::Place,
                    );
                }
                let moves = group_moves(project, &group, start - clip.start, clip_track, track);
                let fits = match project.moved_clips(&moves) {
                    Ok(_) => true,
                    Err(EditError::Overlapping(_)) => false,
                    Err(_) => return None,
                };
                let preview = DropPreview {
                    track,
                    range: TimeRange::new(start, clip.source.duration),
                    fits,
                    reason: (!fits).then_some(OVERLAP_REASON),
                    mode: DropMode::Place,
                    snapped_to: None,
                    group: Some(GroupShift {
                        shift: start - clip.start,
                        from: clip_track,
                    }),
                    partner: None,
                };
                Some(preview.snapped(snap))
            }
            Dragging::Clip(DraggedClip {
                id,
                grip: Grip::Edge(edge),
            }) => {
                let (_, clip) = project.find_clip(id)?;
                let anchor = match edge {
                    ClipEdge::Start => self.grab,
                    ClipEdge::End => self.grab - self.viewport.width_of(clip.source.duration),
                };
                let to = self.frame_at(offset - anchor, cx);
                let snap = self.snap(&[to], &[id], cx);
                let to = to + snap.map_or(Time::ZERO, |snap| snap.shift);
                let partner = project.partners(id).first().map(|(partner, _)| *partner);
                placement(
                    track,
                    project.trimmed_clip(id, edge, to),
                    snap,
                    DropMode::Place,
                )
                .map(|preview| DropPreview { partner, ..preview })
            }
        }
    }

    fn start_autoscroll(&mut self, cx: &mut Context<Self>) {
        if self.autoscrolling || self.autoscroll_step == px(0.) {
            return;
        }
        self.autoscrolling = true;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(AUTOSCROLL_TICK).await;
                let Ok(true) = this.update(cx, |panel, cx| panel.autoscroll(cx)) else {
                    break;
                };
            }
        })
        .detach();
    }

    fn autoscroll(&mut self, cx: &mut Context<Self>) -> bool {
        let hover = self.drag_hover.filter(|_| cx.has_active_drag());
        let step = self.autoscroll_step;
        if hover.is_none() || step == px(0.) {
            self.autoscrolling = false;
            return false;
        }
        let limit = self.scroll_limit(cx) + self.viewport.duration_of(self.lanes.size.width);
        let viewport = self.viewport.scrolled_by(step, limit);
        if viewport == self.viewport {
            self.autoscrolling = false;
            return false;
        }
        self.set_viewport(viewport, cx);
        self.rehover(cx);
        true
    }

    fn rehover(&mut self, cx: &mut Context<Self>) {
        if let Some(hover) = self.drag_hover.filter(|_| cx.has_active_drag()) {
            let preview = self.hover_preview(hover, cx);
            self.show_preview(preview, cx);
        }
    }

    fn horizontal_thumb(&self, cx: &App) -> Option<Thumb> {
        let visible = self.viewport.duration_of(self.lanes.size.width);
        let extent = self.scroll_limit(cx) + visible;
        Thumb::new(
            self.viewport.start().as_seconds_f64(),
            visible.as_seconds_f64(),
            extent.as_seconds_f64(),
        )
    }

    fn vertical_thumb(&self, cx: &App) -> Option<Thumb> {
        let content = content_height(&self.project.read(cx).timeline.tracks);
        Thumb::new(
            f64::from(f32::from(self.track_scroll)),
            f64::from(f32::from(self.tracks_view.size.height)),
            f64::from(content),
        )
    }

    fn thumb(&self, axis: Axis, cx: &App) -> Option<Thumb> {
        match axis {
            Axis::Horizontal => self.horizontal_thumb(cx),
            Axis::Vertical => self.vertical_thumb(cx),
        }
    }

    fn is_scrolling_with(&self, axis: Axis) -> bool {
        self.scrolling
            .is_some_and(|(scrolling, _)| scrolling == axis)
    }

    fn press_scrollbar(&mut self, axis: Axis, fraction: f32, cx: &mut Context<Self>) {
        let Some(thumb) = self.thumb(axis, cx) else {
            return;
        };
        let grab = thumb.grab_at(fraction);
        self.scrolling = Some((axis, grab));
        self.scroll_thumb_to(axis, thumb.start_for(fraction, grab), cx);
    }

    fn drag_scrollbar(&mut self, axis: Axis, fraction: f32, cx: &mut Context<Self>) {
        let Some((_, grab)) = self.scrolling.filter(|(scrolling, _)| *scrolling == axis) else {
            return;
        };
        if let Some(thumb) = self.thumb(axis, cx) {
            self.scroll_thumb_to(axis, thumb.start_for(fraction, grab), cx);
        }
    }

    fn release_scrollbar(&mut self, cx: &mut Context<Self>) {
        if self.scrolling.take().is_some() {
            cx.notify();
        }
    }

    fn scroll_thumb_to(&mut self, axis: Axis, start: f32, cx: &mut Context<Self>) {
        match axis {
            Axis::Horizontal => {
                let limit = self.scroll_limit(cx);
                let extent = limit + self.viewport.duration_of(self.lanes.size.width);
                let time = Time::from_flicks((extent.flicks() as f64 * f64::from(start)) as i64);
                let viewport = self.viewport.scrolled_to(time, limit);
                self.set_viewport(viewport, cx);
            }
            Axis::Vertical => {
                let content = content_height(&self.project.read(cx).timeline.tracks);
                let scroll = self.clamped_track_scroll(px(content * start), content);
                if scroll != self.track_scroll {
                    self.track_scroll = scroll;
                    cx.notify();
                }
            }
        }
    }

    fn show_preview(&mut self, preview: Option<DropPreview>, cx: &mut Context<Self>) {
        if preview != self.drop_preview {
            self.drop_preview = preview;
            cx.notify();
        }
    }

    fn drop_asset_on_lane(&mut self, track: usize, dragged: &DraggedAsset, cx: &mut Context<Self>) {
        let asset = dragged.id;
        let mode = self
            .drop_preview
            .map_or(DropMode::Place, |preview| preview.mode);
        self.commit_preview(
            mode.command(),
            |preview| preview.track == track,
            cx,
            |project, preview| {
                let start = preview.range.start;
                match preview.mode {
                    DropMode::Place => project.place_linked(asset, preview.track, start),
                    DropMode::Insert => project.insert_linked(asset, preview.track, start),
                    DropMode::Overwrite => project.overwrite_linked(asset, preview.track, start),
                }
            },
        );
    }

    fn drop_clip_edge(&mut self, dragged: &DraggedClip, cx: &mut Context<Self>) {
        let DraggedClip { id, grip } = *dragged;
        let Grip::Edge(edge) = grip else {
            return;
        };
        let trimmed_track = self.project.read(cx).find_clip(id).map(|(track, _)| track);
        self.commit_preview(
            Command::TrimClip,
            |preview| Some(preview.track) == trimmed_track,
            cx,
            |project, preview| {
                let to = match edge {
                    ClipEdge::Start => preview.range.start,
                    ClipEdge::End => preview.range.end(),
                };
                project.trim_clip(id, edge, to).map(|clip| vec![clip])
            },
        );
    }

    fn drop_clip_on_lane(&mut self, track: usize, dragged: &DraggedClip, cx: &mut Context<Self>) {
        let DraggedClip { id, grip } = *dragged;
        if grip != Grip::Body {
            self.drop_clip_edge(dragged, cx);
            return;
        }
        let grouped = self.selection.len() > 1 && self.selection.contains(&id);
        let group: Vec<ClipId> = self.selection.iter().copied().collect();
        self.commit_preview(
            Command::MoveClip,
            |preview| preview.track == track,
            cx,
            |project, preview| {
                if grouped {
                    let (from, clip) = project.find_clip(id).ok_or(EditError::UnknownClip(id))?;
                    let delta = preview.range.start - clip.start;
                    project.move_clips(&group_moves(project, &group, delta, from, track))
                } else {
                    project
                        .move_clip(id, preview.track, preview.range.start)
                        .map(|clip| vec![clip])
                }
            },
        );
    }

    fn commit_preview(
        &mut self,
        command: Command,
        accepts: impl FnOnce(&DropPreview) -> bool,
        cx: &mut Context<Self>,
        edit: impl FnOnce(&mut Project, DropPreview) -> Result<Vec<Clip>, EditError>,
    ) {
        self.drag_hover = None;
        self.autoscroll_step = px(0.);
        let preview = self
            .drop_preview
            .take()
            .filter(|preview| preview.fits && accepts(preview));
        if let Some(preview) = preview {
            match self
                .editor
                .apply(command, cx, |project| edit(project, preview))
            {
                Ok(clips) => {
                    let ids: Vec<ClipId> = clips.iter().map(|clip| clip.id).collect();
                    self.selection = self
                        .project
                        .read(cx)
                        .with_partners(&ids)
                        .into_iter()
                        .collect();
                }
                Err(error) => tracing::warn!(%error, "could not edit the timeline"),
            }
        }
        cx.notify();
    }

    fn snapped_start(
        &self,
        start: Time,
        duration: Option<Time>,
        moving: &[ClipId],
        cx: &App,
    ) -> (Time, Option<Snap>) {
        let edges: Vec<Time> = [Some(start), duration.map(|duration| start + duration)]
            .into_iter()
            .flatten()
            .collect();
        let snap = self.snap(&edges, moving, cx);
        let snapped = start + snap.map_or(Time::ZERO, |snap| snap.shift);
        let end_snapped = snap
            .zip(duration)
            .is_some_and(|(snap, duration)| snap.target == snapped + duration);
        let start = if end_snapped {
            self.project
                .read(cx)
                .settings
                .frame_rate
                .frame_start(snapped)
        } else {
            snapped
        };
        (start, snap)
    }

    fn snap(&self, edges: &[Time], moving: &[ClipId], cx: &App) -> Option<Snap> {
        if !self.snapping {
            return None;
        }
        let playhead = self.playhead.read(cx).time();
        let targets = SnapTargets::new(&self.project.read(cx).timeline, playhead, moving);
        targets.snap(edges, self.viewport.duration_of(px(SNAP_DISTANCE)))
    }

    fn frame_at(&self, offset: Pixels, cx: &App) -> Time {
        let frame_rate = self.project.read(cx).settings.frame_rate;
        frame_rate.frame_start(self.viewport.time_at(offset))
    }

    fn scrub_to(&mut self, offset: Pixels, cx: &mut Context<Self>) {
        let time = self.frame_at(offset, cx);
        self.playhead
            .update(cx, |playhead, cx| playhead.seek(time, cx));
    }
}

impl Render for TimelinePanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !cx.has_active_drag() {
            self.drop_preview = None;
            self.drag_hover = None;
            self.autoscroll_step = px(0.);
        }
        let panel = cx.entity();
        let horizontal_thumb = self.horizontal_thumb(cx);
        let vertical_thumb = self.vertical_thumb(cx);
        let drop_preview = self.drop_preview;
        let snap_line = drop_preview
            .and_then(|preview| preview.snapped_to)
            .map(|time| snap_line(self.viewport.x_at(time)));
        let viewport = self.viewport;
        let selection = &self.selection;
        let visible = (self.lanes.size.width > px(0.)).then(|| {
            TimeRange::new(
                viewport.start(),
                viewport.duration_of(self.lanes.size.width),
            )
        });
        let playhead = self.playhead.read(cx).time();
        let project = self.project.read(cx);
        let frame_rate = project.settings.frame_rate;
        let tracks = &project.timeline.tracks;
        let rows = track_rows(&project.timeline);
        self.track_scroll = self.clamped_track_scroll(self.track_scroll, content_height(tracks));
        let scroll = self.track_scroll;
        let lanes = rows.iter().map(|row| {
            let lane = Lane {
                index: row.index,
                track: &tracks[row.index],
                viewport,
                drop_preview,
                selection,
                visible,
            };
            track_lane(lane, project, cx)
        });
        let marquee = self
            .marquee
            .as_ref()
            .map(|marquee| marquee_rect(marquee, viewport, scroll));
        let lanes = scrolled_tracks(scroll, lanes)
            .children(marquee)
            .child(tracks_view_probe(panel.clone()))
            .child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .right_0()
                    .child(scrollbar(panel.clone(), Axis::Vertical, vertical_thumb)),
            );
        let horizontal_scrollbar = scrollbar(panel.clone(), Axis::Horizontal, horizontal_thumb);
        let renaming = self
            .renaming
            .as_ref()
            .and_then(|renaming| match renaming.target {
                RenameTarget::Track(track) => Some((track, renaming.field.clone())),
                RenameTarget::Marker(_) => None,
            });
        let marker_rename = self
            .renaming
            .as_ref()
            .and_then(|renaming| match renaming.target {
                RenameTarget::Marker(id) => Some((id, renaming.field.clone())),
                RenameTarget::Track(_) => None,
            });
        let marker_drag = self.marker_drag;
        let marker_time = |marker: &Marker| {
            marker_drag
                .filter(|drag| drag.id == marker.id)
                .map_or(marker.time, |drag| drag.time)
        };
        let markers = project.markers.iter().map(|marker| {
            let x = viewport.x_at(marker_time(marker));
            let editing = marker_rename
                .as_ref()
                .filter(|(id, _)| *id == marker.id)
                .map(|(_, field)| field.clone());
            marker_flag(x, marker.name.clone(), editing)
        });
        let headers = rows
            .into_iter()
            .map(|row| {
                let editing = renaming
                    .as_ref()
                    .filter(|(track, _)| *track == row.index)
                    .map(|(_, field)| field);
                track_header(row, editing, cx)
            })
            .map(IntoElement::into_any_element)
            .chain([add_track_row(cx).into_any_element()]);
        div()
            .size_full()
            .flex()
            .bg(theme::panel())
            .on_drop(cx.listener(|panel, dragged: &DraggedClip, _, cx| {
                panel.drop_clip_edge(dragged, cx);
            }))
            .on_scroll_wheel(cx.listener(|panel, event, window, cx| {
                panel.scroll_wheel(event, window, cx);
            }))
            .child(
                div()
                    .w(px(TRACK_HEADER_WIDTH))
                    .flex_none()
                    .flex()
                    .flex_col()
                    .border_r_1()
                    .border_color(theme::border())
                    .child(timecode_readout(Timecode::new(playhead, frame_rate)))
                    .child(scrolled_tracks(scroll, headers))
                    .child(div().h(px(SCROLLBAR_THICKNESS)).flex_none()),
            )
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .flex()
                    .flex_col()
                    .child(ruler(panel, viewport, frame_rate))
                    .child(lanes)
                    .child(horizontal_scrollbar)
                    .children(in_out_range(viewport, project.in_point, project.out_point))
                    .children(markers)
                    .children(snap_line)
                    .child(playhead_marker(viewport.x_at(playhead))),
            )
    }
}

fn scrolled_tracks(scroll: Pixels, rows: impl IntoIterator<Item = impl IntoElement>) -> gpui::Div {
    div().relative().flex_1().min_h_0().overflow_hidden().child(
        div()
            .absolute()
            .top(-scroll)
            .left_0()
            .right_0()
            .flex()
            .flex_col()
            .children(rows),
    )
}

fn tracks_view_probe(panel: Entity<TimelinePanel>) -> impl IntoElement {
    canvas(
        {
            let panel = panel.clone();
            move |bounds, _, cx| panel.update(cx, |panel, _| panel.tracks_view = bounds)
        },
        move |_, _, window, _| listen_for_marquee(panel, window),
    )
    .absolute()
    .size_full()
}

fn listen_for_marquee(panel: Entity<TimelinePanel>, window: &mut Window) {
    window.on_mouse_event({
        let panel = panel.clone();
        move |event: &MouseMoveEvent, phase, _, cx| {
            if phase == DispatchPhase::Bubble && panel.read(cx).marquee.is_some() {
                panel.update(cx, |panel, cx| {
                    if event.dragging() {
                        panel.extend_marquee(event.position, cx);
                    } else {
                        panel.end_marquee(cx);
                    }
                });
            }
        }
    });
    window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
        if phase == DispatchPhase::Bubble
            && event.button == MouseButton::Left
            && panel.read(cx).marquee.is_some()
        {
            panel.update(cx, |panel, cx| panel.end_marquee(cx));
        }
    });
}

fn marquee_rect(marquee: &Marquee, viewport: Viewport, scroll: Pixels) -> impl IntoElement {
    let (first, last) = marquee.times();
    let (top, bottom) = marquee.content_ys();
    let left = viewport.x_at(first);
    div()
        .absolute()
        .left(left)
        .top(top - scroll)
        .w(viewport.x_at(last) - left)
        .h(bottom - top)
        .border_1()
        .border_color(theme::selection())
        .bg(theme::marquee())
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

struct Lane<'a> {
    index: usize,
    track: &'a Track,
    viewport: Viewport,
    drop_preview: Option<DropPreview>,
    selection: &'a BTreeSet<ClipId>,
    visible: Option<TimeRange>,
}

fn track_lane(lane: Lane, project: &Project, cx: &Context<TimelinePanel>) -> impl IntoElement {
    let Lane {
        index,
        track,
        viewport,
        drop_preview,
        selection,
        visible,
    } = lane;
    let color = match track.kind {
        TrackKind::Video => theme::video_clip(),
        TrackKind::Audio => theme::audio_clip(),
    };
    let ghosts = ghosts_on(index, &project.timeline.tracks, drop_preview, selection)
        .into_iter()
        .map(|preview| drop_ghost(preview, viewport));
    div()
        .h(px(row_height(track.height)))
        .flex_none()
        .relative()
        .border_b_1()
        .border_color(theme::border())
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |panel, event: &MouseDownEvent, _, cx| {
                panel.press_lane(index, event.position, event.modifiers.shift, cx);
            }),
        )
        .on_drag_move(cx.listener(move |panel, event, _, cx| {
            panel.drag_asset_over_lane(index, event, cx);
        }))
        .on_drag_move(cx.listener(move |panel, event, _, cx| {
            panel.drag_clip_over_lane(index, event, cx);
        }))
        .on_drop(cx.listener(move |panel, dragged, _, cx| {
            panel.drop_asset_on_lane(index, dragged, cx);
        }))
        .on_drop(cx.listener(move |panel, dragged, _, cx| {
            panel.drop_clip_on_lane(index, dragged, cx);
        }))
        .children(visible_clips(track, visible).iter().map(|clip| {
            let look = ClipLook {
                label: clip_label(project, clip),
                color,
                selected: selection.contains(&clip.id),
            };
            clip_block(clip, look, viewport)
        }))
        .children(ghosts)
}

fn ghosts_on(
    index: usize,
    tracks: &[Track],
    preview: Option<DropPreview>,
    selection: &BTreeSet<ClipId>,
) -> Vec<DropPreview> {
    let Some(preview) = preview else {
        return Vec::new();
    };
    let Some(GroupShift { shift, from }) = preview.group else {
        let on_lane = preview.track == index || preview.partner == Some(index);
        return on_lane
            .then(|| DropPreview {
                track: index,
                reason: preview.reason.filter(|_| preview.track == index),
                ..preview
            })
            .into_iter()
            .collect();
    };
    let source = if index == preview.track {
        from
    } else if index == from {
        return Vec::new();
    } else {
        index
    };
    let Some(track) = tracks.get(source) else {
        return Vec::new();
    };
    track
        .clips()
        .iter()
        .filter(|clip| selection.contains(&clip.id))
        .map(|clip| {
            let range = TimeRange::new(clip.start + shift, clip.source.duration);
            let dragged = index == preview.track && range == preview.range;
            DropPreview {
                track: index,
                range,
                reason: preview.reason.filter(|_| dragged),
                ..preview
            }
        })
        .collect()
}

fn visible_clips(track: &Track, visible: Option<TimeRange>) -> &[Clip] {
    visible.map_or(track.clips(), |range| track.clips_overlapping(range))
}

fn clip_label(project: &Project, clip: &Clip) -> SharedString {
    let name = project
        .asset(clip.asset)
        .map(|asset| file_name(&asset.path))
        .unwrap_or_default();
    if clip.gain == Gain::UNITY {
        name
    } else {
        format!("{name} · {}", clip.gain).into()
    }
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

fn autoscroll_step(offset: Pixels, width: Pixels) -> Pixels {
    let edge = AUTOSCROLL_EDGE;
    let (offset, width) = (f32::from(offset), f32::from(width));
    if width <= edge * 2. {
        return px(0.);
    }
    let depth = if offset < edge {
        offset - edge
    } else if offset > width - edge {
        offset - (width - edge)
    } else {
        return px(0.);
    };
    px((depth / edge).clamp(-1., 1.) * AUTOSCROLL_MAX_STEP)
}

fn group_moves(
    project: &Project,
    group: &[ClipId],
    delta: Time,
    from: usize,
    to: usize,
) -> Vec<(ClipId, usize, Time)> {
    group
        .iter()
        .filter_map(|id| project.find_clip(*id))
        .map(|(track, clip)| {
            let track = if track == from { to } else { track };
            (clip.id, track, clip.start + delta)
        })
        .collect()
}

fn placement(
    track: usize,
    placed: Result<Clip, EditError>,
    snap: Option<Snap>,
    mode: DropMode,
) -> Option<DropPreview> {
    let (range, fits) = match placed {
        Ok(clip) => (clip.timeline_range(), true),
        Err(EditError::Overlapping(overlap)) => (overlap.inserted, mode != DropMode::Place),
        Err(_) => return None,
    };
    let preview = DropPreview {
        track,
        range,
        fits,
        reason: (!fits).then_some(OVERLAP_REASON),
        mode,
        snapped_to: None,
        group: None,
        partner: None,
    };
    Some(preview.snapped(snap))
}

fn asset_drop_preview(
    project: &Project,
    asset: AssetId,
    track: usize,
    start: Time,
    snap: Option<Snap>,
    mode: DropMode,
) -> Option<DropPreview> {
    if mode != DropMode::Place {
        let preview = placement(track, project.clip_for(asset, track, start), snap, mode)?;
        return Some(DropPreview {
            reason: preview.reason.map(|_| ASSET_OVERLAP_REASON),
            partner: project.linked_partner_track(asset, track),
            ..preview
        });
    }
    let linked = project.linked_clips_for(asset, track, start);
    let partner = match &linked {
        Ok(clips) => clips.get(1).map(|(partner, _)| *partner),
        Err(_) => project.partner_track(track).filter(|&partner| {
            project
                .asset(asset)
                .is_some_and(|asset| asset.has_stream(project.timeline.tracks[partner].kind))
        }),
    };
    let primary = linked.map(|clips| clips[0].1);
    let preview = placement(track, primary, snap, mode)?;
    Some(DropPreview {
        reason: preview.reason.map(|_| ASSET_OVERLAP_REASON),
        partner,
        ..preview
    })
}

fn drop_ghost(preview: DropPreview, viewport: Viewport) -> impl IntoElement {
    let color = if preview.fits {
        theme::drop_ghost()
    } else {
        theme::drop_ghost_blocked()
    };
    clip_frame(preview.range, viewport)
        .bg(color)
        .overflow_hidden()
        .px_1()
        .text_xs()
        .text_color(theme::text())
        .children(preview.reason.map(|reason| div().truncate().child(reason)))
}

struct ClipLook {
    label: SharedString,
    color: Rgba,
    selected: bool,
}

fn clip_block(clip: &Clip, look: ClipLook, viewport: Viewport) -> impl IntoElement {
    let body = DraggedClip {
        id: clip.id,
        grip: Grip::Body,
    };
    let handle = trim_handle_width(viewport.width_of(clip.source.duration));
    clip_frame(clip.timeline_range(), viewport)
        .id(("clip", clip.id.0))
        .overflow_hidden()
        .px_1()
        .bg(look.color)
        .text_xs()
        .text_color(theme::text())
        .when(look.selected, |frame| {
            frame.border_2().border_color(theme::selection())
        })
        .on_drag(body, |dragged, _, _, cx| cx.new(|_| *dragged))
        .child(div().truncate().child(look.label))
        .child(trim_handle(clip.id, ClipEdge::Start, handle))
        .child(trim_handle(clip.id, ClipEdge::End, handle))
}

fn trim_handle_width(clip_width: Pixels) -> Pixels {
    px(TRIM_HANDLE_WIDTH).min(clip_width / 3.)
}

fn trim_handle(id: ClipId, edge: ClipEdge, width: Pixels) -> impl IntoElement {
    let dragged = DraggedClip {
        id,
        grip: Grip::Edge(edge),
    };
    let handle = match edge {
        ClipEdge::Start => div().id(("clip-start", id.0)).left_0(),
        ClipEdge::End => div().id(("clip-end", id.0)).right_0(),
    };
    handle
        .absolute()
        .top_0()
        .bottom_0()
        .w(width)
        .cursor(CursorStyle::ResizeLeftRight)
        .on_drag(dragged, |dragged, _, _, cx| cx.new(|_| *dragged))
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
                let offset = event.position - bounds.origin;
                panel.update(cx, |panel, cx| {
                    panel.press_ruler(offset, event.click_count, window, cx);
                });
            }
        }
    });
    window.on_mouse_event({
        let panel = panel.clone();
        move |event: &MouseMoveEvent, phase, _, cx| {
            if phase == DispatchPhase::Bubble && panel.read(cx).ruler_pressed() {
                panel.update(cx, |panel, cx| {
                    if event.dragging() {
                        panel.drag_on_ruler(event.position.x - bounds.left(), cx);
                    } else {
                        panel.release_ruler(cx);
                    }
                });
            }
        }
    });
    window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
        if phase == DispatchPhase::Bubble
            && event.button == MouseButton::Left
            && panel.read(cx).ruler_pressed()
        {
            panel.update(cx, |panel, cx| panel.release_ruler(cx));
        }
    });
}

fn marker_flag(x: Pixels, name: String, editing: Option<Entity<TextField>>) -> gpui::Div {
    let flag = div()
        .absolute()
        .top(px(RULER_HEIGHT - MARKER_HEIGHT))
        .left(x - px(MARKER_WIDTH / 2.))
        .w(px(MARKER_WIDTH))
        .h(px(MARKER_HEIGHT))
        .rounded_t_sm()
        .bg(theme::marker());
    let label_left = x + px(MARKER_WIDTH / 2. + MARKER_LABEL_GAP);
    let label = match editing {
        Some(field) => Some(
            div()
                .absolute()
                .top_0()
                .left(label_left)
                .w(px(MARKER_RENAME_WIDTH))
                .h(px(RULER_HEIGHT - 1.))
                .bg(theme::panel())
                .child(field),
        ),
        None => (!name.is_empty()).then(|| {
            div()
                .absolute()
                .top_0()
                .left(label_left)
                .px_1()
                .rounded_sm()
                .bg(theme::panel())
                .text_xs()
                .text_color(theme::text())
                .whitespace_nowrap()
                .child(name)
        }),
    };
    div().absolute().size_full().child(flag).children(label)
}

fn in_out_range(
    viewport: Viewport,
    in_point: Option<Time>,
    out_point: Option<Time>,
) -> Vec<gpui::Div> {
    let edge = |time: Time| {
        div()
            .absolute()
            .top_0()
            .bottom_0()
            .left(viewport.x_at(time) - px(IN_OUT_EDGE_WIDTH / 2.))
            .w(px(IN_OUT_EDGE_WIDTH))
            .bg(theme::marker())
    };
    let mut elements: Vec<gpui::Div> = in_point.into_iter().chain(out_point).map(edge).collect();
    if let Some((start, end)) = in_point.zip(out_point) {
        elements.push(
            div()
                .absolute()
                .top(px(RULER_HEIGHT))
                .bottom_0()
                .left(viewport.x_at(start))
                .w(viewport.width_of(end - start))
                .bg(theme::in_out_range()),
        );
    }
    elements
}

fn snap_line(x: Pixels) -> impl IntoElement {
    div()
        .absolute()
        .top_0()
        .bottom_0()
        .left(x - px(SNAP_LINE_WIDTH / 2.))
        .w(px(SNAP_LINE_WIDTH))
        .bg(theme::snap_line())
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
    use std::num::{NonZero, NonZeroI64};

    use gpui::{Modifiers, Point, ScrollDelta, TestAppContext, TouchPhase, VisualTestContext};
    use tessera_timeline::{MediaInfo, Stream, TrackHeight, VideoStream};

    use super::{header::*, *};

    const ONE_SECOND: f32 = 48.;
    const V1_ROW: usize = 1;
    const V1: f32 = RULER_HEIGHT + TRACK_HEIGHT * (V1_ROW as f32 + 0.5);
    const HEADER_RIGHT: f32 = TRACK_HEADER_WIDTH - 1.;

    fn row_y(row: usize) -> Pixels {
        px(RULER_HEIGHT + TRACK_HEIGHT * (row as f32 + 0.5))
    }

    fn header_line_y(row: usize, line: usize) -> Pixels {
        px(RULER_HEIGHT + TRACK_HEIGHT * row as f32 + HEADER_LINE_HEIGHT * (line as f32 + 0.5))
    }

    fn header_toggle_x(from_left: usize) -> Pixels {
        let step = HEADER_BUTTON_SIZE + HEADER_BUTTON_GAP;
        px(HEADER_PADDING + HEADER_BUTTON_SIZE / 2. + step * from_left as f32)
    }

    fn header_button_x(from_right: usize) -> Pixels {
        let step = HEADER_BUTTON_SIZE + HEADER_BUTTON_GAP;
        px(HEADER_RIGHT - HEADER_PADDING - HEADER_BUTTON_SIZE / 2. - step * from_right as f32)
    }

    const REMOVE: usize = 0;
    const DOWN: usize = 1;

    fn click_add(cx: &mut VisualTestContext, rows: usize, kind: TrackKind) {
        let nth = match kind {
            TrackKind::Video => 0.,
            TrackKind::Audio => 1.,
        };
        let x = HEADER_PADDING + ADD_BUTTON_WIDTH / 2. + nth * (ADD_BUTTON_WIDTH + ADD_BUTTON_GAP);
        let y = RULER_HEIGHT + TRACK_HEIGHT * rows as f32 + ADD_ROW_HEIGHT / 2.;
        cx.simulate_click(point(px(x), px(y)), Modifiers::none());
    }

    fn kinds_and_clip_track(
        panel: &Entity<TimelinePanel>,
        cx: &mut VisualTestContext,
        clip: ClipId,
    ) -> (Vec<TrackKind>, Option<usize>) {
        cx.read(|cx| {
            let project = panel.read(cx).project.read(cx);
            let kinds = project
                .timeline
                .tracks
                .iter()
                .map(|track| track.kind)
                .collect();
            (kinds, project.find_clip(clip).map(|(track, _)| track))
        })
    }

    fn at(seconds: f32) -> Pixels {
        px(TRACK_HEADER_WIDTH + seconds * ONE_SECOND)
    }

    const V2: usize = 1;

    fn timeline_with_a_clip(
        cx: &mut TestAppContext,
    ) -> (Entity<TimelinePanel>, &mut VisualTestContext, ClipId) {
        let (panel, cx, clips) = timeline_with_clips(cx, &[(0, 1)]);
        (panel, cx, clips[0])
    }

    fn timeline_with_clips<'a>(
        cx: &'a mut TestAppContext,
        clips: &[(usize, i64)],
    ) -> (
        Entity<TimelinePanel>,
        &'a mut VisualTestContext,
        Vec<ClipId>,
    ) {
        let mut project = Project::new("test");
        project.timeline.add_track(TrackKind::Video);
        let info = MediaInfo {
            duration: Some(Time::from_seconds(8)),
            streams: vec![Stream::Video(VideoStream {
                index: 0,
                codec: "h264".into(),
                width: NonZero::new(1920).unwrap(),
                height: NonZero::new(1080).unwrap(),
                frame_rate: None,
            })],
        };
        let asset = project.add_asset("a.mkv".into(), info);
        let clips = clips
            .iter()
            .map(|&(track, seconds)| {
                project
                    .place_clip(asset, track, Time::from_seconds(seconds))
                    .unwrap()
                    .id
            })
            .collect();
        let project = cx.new(|_| project);
        let (panel, cx) = cx.add_window_view(|_, cx| {
            let playhead = cx.new(|cx| Playhead::new(project.clone(), cx));
            TimelinePanel::new(ProjectEditor::new(project, cx), playhead, cx)
        });
        (panel, cx, clips)
    }

    fn seek(panel: &Entity<TimelinePanel>, cx: &mut VisualTestContext, seconds: i64) {
        let playhead = cx.read(|cx| panel.read(cx).playhead.clone());
        playhead.update(cx, |playhead, cx| {
            playhead.seek(Time::from_seconds(seconds), cx);
        });
    }

    fn starts_on(
        panel: &Entity<TimelinePanel>,
        cx: &mut VisualTestContext,
        track: usize,
    ) -> Vec<Time> {
        cx.read(|cx| {
            panel.read(cx).project.read(cx).timeline.tracks[track]
                .clips()
                .iter()
                .map(|clip| clip.start)
                .collect()
        })
    }

    fn seconds(values: &[i64]) -> Vec<Time> {
        values.iter().copied().map(Time::from_seconds).collect()
    }

    fn drag(cx: &mut VisualTestContext, from: Pixels, to: Pixels) {
        let none = Modifiers::none();
        cx.simulate_mouse_down(point(from, px(V1)), MouseButton::Left, none);
        cx.simulate_mouse_move(point(from + px(4.), px(V1)), MouseButton::Left, none);
        cx.simulate_mouse_move(point(to, px(V1)), MouseButton::Left, none);
        cx.simulate_mouse_up(point(to, px(V1)), MouseButton::Left, none);
    }

    fn selected(panel: &Entity<TimelinePanel>, cx: &mut VisualTestContext) -> BTreeSet<ClipId> {
        cx.read(|cx| panel.read(cx).selection.clone())
    }

    fn clip_of(panel: &Entity<TimelinePanel>, cx: &mut VisualTestContext, id: ClipId) -> Clip {
        cx.read(|cx| *panel.read(cx).project.read(cx).find_clip(id).unwrap().1)
    }

    #[gpui::test]
    fn pressing_a_clip_selects_it_and_empty_lane_clears(cx: &mut TestAppContext) {
        let (panel, cx, clip) = timeline_with_a_clip(cx);
        cx.simulate_click(point(at(4.), px(V1)), Modifiers::none());
        assert_eq!(selected(&panel, cx), BTreeSet::from([clip]));
        cx.simulate_click(point(at(12.), px(V1)), Modifiers::none());
        assert_eq!(selected(&panel, cx), BTreeSet::new());
    }

    #[gpui::test]
    fn dragging_a_clip_moves_it_by_the_pointer_travel(cx: &mut TestAppContext) {
        let (panel, cx, clip) = timeline_with_a_clip(cx);
        drag(cx, at(4.), at(6.));
        let moved = clip_of(&panel, cx, clip);
        assert_eq!(moved.start, Time::from_seconds(3));
        assert_eq!(moved.source.start, Time::ZERO);
        assert_eq!(selected(&panel, cx), BTreeSet::from([clip]));
    }

    #[gpui::test]
    fn dragging_the_edges_trims_the_clip(cx: &mut TestAppContext) {
        let (panel, cx, clip) = timeline_with_a_clip(cx);
        drag(cx, at(9.) - px(2.), at(6.) - px(2.));
        let trimmed = clip_of(&panel, cx, clip);
        assert_eq!(trimmed.timeline_range().end(), Time::from_seconds(6));
        drag(cx, at(1.) + px(2.), at(3.) + px(2.));
        let trimmed = clip_of(&panel, cx, clip);
        assert_eq!(trimmed.start, Time::from_seconds(3));
        assert_eq!(
            trimmed.source,
            TimeRange::new(Time::from_seconds(2), Time::from_seconds(3))
        );
    }

    #[gpui::test]
    fn moving_a_clip_snaps_its_edges_to_nearby_clip_edges(cx: &mut TestAppContext) {
        let (panel, cx, clips) = timeline_with_clips(cx, &[(0, 1), (V2, 12)]);
        let travel = 2.95;
        drag(cx, at(4.), at(4. + travel));
        assert_eq!(clip_of(&panel, cx, clips[0]).start, Time::from_seconds(4));
        panel.update(cx, TimelinePanel::toggle_snapping);
        drag(cx, at(5.), at(5. - 0.05));
        assert_eq!(
            clip_of(&panel, cx, clips[0]).start,
            FrameRate::FPS_30.frame_start(Time::from_rational(395, NonZeroI64::new(100).unwrap()))
        );
    }

    #[gpui::test]
    fn trimming_snaps_to_the_playhead(cx: &mut TestAppContext) {
        let (panel, cx, clip) = timeline_with_a_clip(cx);
        seek(&panel, cx, 5);
        drag(cx, at(9.) - px(2.), at(5.1));
        let trimmed = clip_of(&panel, cx, clip);
        assert_eq!(trimmed.timeline_range().end(), Time::from_seconds(5));
    }

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

    #[gpui::test]
    fn splitting_cuts_the_selected_clip_or_every_clip_under_the_playhead(cx: &mut TestAppContext) {
        let (panel, cx, _) = timeline_with_clips(cx, &[(0, 1), (V2, 2)]);
        seek(&panel, cx, 4);
        panel.update(cx, TimelinePanel::split_at_playhead);
        assert_eq!(starts_on(&panel, cx, 0), seconds(&[1, 4]));
        assert_eq!(starts_on(&panel, cx, V2), seconds(&[2, 4]));
        cx.simulate_click(point(at(2.), px(V1)), Modifiers::none());
        seek(&panel, cx, 3);
        panel.update(cx, TimelinePanel::split_at_playhead);
        assert_eq!(starts_on(&panel, cx, 0), seconds(&[1, 3, 4]));
        assert_eq!(starts_on(&panel, cx, V2), seconds(&[2, 4]));
    }

    #[gpui::test]
    fn deleting_removes_the_selected_clip_and_leaves_a_gap(cx: &mut TestAppContext) {
        let (panel, cx, _) = timeline_with_clips(cx, &[(0, 1), (0, 10)]);
        panel.update(cx, TimelinePanel::delete_selection);
        assert_eq!(starts_on(&panel, cx, 0), seconds(&[1, 10]));
        cx.simulate_click(point(at(2.), px(V1)), Modifiers::none());
        panel.update(cx, TimelinePanel::delete_selection);
        assert_eq!(starts_on(&panel, cx, 0), seconds(&[10]));
        assert_eq!(selected(&panel, cx), BTreeSet::new());
    }

    #[gpui::test]
    fn ripple_delete_closes_the_gap(cx: &mut TestAppContext) {
        let (panel, cx, _) = timeline_with_clips(cx, &[(0, 1), (0, 10)]);
        cx.simulate_click(point(at(2.), px(V1)), Modifiers::none());
        panel.update(cx, TimelinePanel::ripple_delete_selection);
        assert_eq!(starts_on(&panel, cx, 0), seconds(&[2]));
    }

    #[gpui::test]
    fn header_buttons_add_swap_and_remove_tracks(cx: &mut TestAppContext) {
        use TrackKind::{Audio, Video};
        let (panel, cx, clip) = timeline_with_a_clip(cx);
        click_add(cx, 3, Audio);
        assert_eq!(
            kinds_and_clip_track(&panel, cx, clip),
            (vec![Video, Video, Audio, Audio], Some(0))
        );
        cx.simulate_click(
            point(header_button_x(DOWN), header_line_y(0, 0)),
            Modifiers::none(),
        );
        assert_eq!(
            kinds_and_clip_track(&panel, cx, clip),
            (vec![Video, Video, Audio, Audio], Some(1))
        );
        cx.simulate_click(
            point(header_button_x(REMOVE), header_line_y(0, 0)),
            Modifiers::none(),
        );
        assert_eq!(
            kinds_and_clip_track(&panel, cx, clip),
            (vec![Video, Video, Audio, Audio], Some(1))
        );
        cx.simulate_click(
            point(header_button_x(REMOVE), header_line_y(1, 0)),
            Modifiers::none(),
        );
        assert_eq!(
            kinds_and_clip_track(&panel, cx, clip),
            (vec![Video, Audio, Audio], Some(0))
        );
        click_add(cx, 3, Video);
        assert_eq!(
            kinds_and_clip_track(&panel, cx, clip),
            (vec![Video, Video, Audio, Audio], Some(0))
        );
    }

    #[gpui::test]
    fn shift_wheel_scrolls_the_tracks_within_their_height(cx: &mut TestAppContext) {
        let (panel, cx, clip) = timeline_with_a_clip(cx);
        for _ in 0..30 {
            panel.update(cx, |panel, cx| panel.add_track(TrackKind::Audio, cx));
        }
        let wheel = |cx: &mut VisualTestContext, pixels: f32| {
            cx.simulate_event(ScrollWheelEvent {
                position: point(at(4.), px(V1)),
                delta: ScrollDelta::Pixels(point(px(0.), px(pixels))),
                modifiers: Modifiers::shift(),
                touch_phase: TouchPhase::Moved,
            });
        };
        let scroll = |cx: &mut VisualTestContext| cx.read(|cx| panel.read(cx).track_scroll);
        wheel(cx, -TRACK_HEIGHT);
        assert_eq!(scroll(cx), px(TRACK_HEIGHT));
        cx.simulate_click(point(at(4.), row_y(V1_ROW - 1)), Modifiers::none());
        assert_eq!(selected(&panel, cx), BTreeSet::from([clip]));
        wheel(cx, -100_000.);
        let (content, view) = cx.read(|cx| {
            let panel = panel.read(cx);
            (
                content_height(&panel.project.read(cx).timeline.tracks),
                panel.tracks_view,
            )
        });
        assert_eq!(scroll(cx), px(content) - view.size.height);
        wheel(cx, 100_000.);
        assert_eq!(scroll(cx), px(0.));
        assert_eq!(cx.read(|cx| panel.read(cx).viewport), Viewport::default());
    }

    #[gpui::test]
    fn a_replaced_project_clears_the_selection_and_the_view(cx: &mut TestAppContext) {
        let (panel, cx, clip) = timeline_with_a_clip(cx);
        cx.simulate_click(point(at(4.), px(V1)), Modifiers::none());
        panel.update(cx, TimelinePanel::zoom_in);
        assert_eq!(selected(&panel, cx), BTreeSet::from([clip]));
        assert_ne!(cx.read(|cx| panel.read(cx).viewport), Viewport::default());
        let editor = cx.read(|cx| panel.read(cx).editor.clone());
        cx.update(|_, cx| editor.replace(Project::new("other"), cx));
        panel.update(cx, TimelinePanel::project_replaced);
        assert_eq!(selected(&panel, cx), BTreeSet::new());
        assert_eq!(cx.read(|cx| panel.read(cx).viewport), Viewport::default());
        assert!(starts_on(&panel, cx, 0).is_empty());
    }

    #[gpui::test]
    fn shift_clicking_adds_and_removes_clips_from_the_selection(cx: &mut TestAppContext) {
        let (panel, cx, clips) = timeline_with_clips(cx, &[(0, 1), (0, 12)]);
        let (first, second) = (clips[0], clips[1]);

        cx.simulate_click(point(at(4.), px(V1)), Modifiers::none());
        cx.simulate_click(point(at(14.), px(V1)), Modifiers::shift());

        assert_eq!(selected(&panel, cx), BTreeSet::from([first, second]));

        cx.simulate_click(point(at(4.), px(V1)), Modifiers::shift());

        assert_eq!(selected(&panel, cx), BTreeSet::from([second]));

        cx.simulate_click(point(at(10.), px(V1)), Modifiers::shift());

        assert_eq!(selected(&panel, cx), BTreeSet::from([second]));

        cx.simulate_click(point(at(10.), px(V1)), Modifiers::none());

        assert!(selected(&panel, cx).is_empty());
    }

    #[gpui::test]
    fn select_all_picks_every_clip_on_every_track(cx: &mut TestAppContext) {
        let (panel, cx, clips) = timeline_with_clips(cx, &[(0, 1), (0, 12), (1, 3)]);

        panel.update(cx, TimelinePanel::select_all);

        assert_eq!(selected(&panel, cx), clips.into_iter().collect());
    }

    #[gpui::test]
    fn dragging_one_selected_clip_moves_the_whole_selection(cx: &mut TestAppContext) {
        let (panel, cx, clips) = timeline_with_clips(cx, &[(0, 1), (0, 12)]);
        panel.update(cx, TimelinePanel::select_all);

        drag(cx, at(4.), at(6.));

        assert_eq!(starts_on(&panel, cx, 0), seconds(&[3, 14]));
        assert_eq!(selected(&panel, cx), clips.into_iter().collect());
    }

    #[gpui::test]
    fn a_group_move_that_would_collide_changes_nothing(cx: &mut TestAppContext) {
        let (panel, cx, _) = timeline_with_clips(cx, &[(0, 1), (0, 10), (0, 30)]);
        panel.update(cx, |panel, cx| {
            panel.selection = cx.read_entity(&panel.project, |project, _| {
                project.timeline.tracks[0]
                    .clips()
                    .iter()
                    .take(2)
                    .map(|clip| clip.id)
                    .collect()
            });
        });

        drag(cx, at(4.), at(20.));

        assert_eq!(starts_on(&panel, cx, 0), seconds(&[1, 10, 30]));
    }

    #[gpui::test]
    fn pressing_an_unselected_clip_replaces_the_selection(cx: &mut TestAppContext) {
        let (panel, cx, clips) = timeline_with_clips(cx, &[(0, 1), (0, 12)]);
        cx.simulate_click(point(at(4.), px(V1)), Modifiers::none());

        cx.simulate_click(point(at(14.), px(V1)), Modifiers::none());

        assert_eq!(selected(&panel, cx), BTreeSet::from([clips[1]]));
    }

    #[gpui::test]
    fn deleting_the_selection_removes_every_selected_clip(cx: &mut TestAppContext) {
        let (panel, cx, _) = timeline_with_clips(cx, &[(0, 1), (0, 12), (0, 30)]);
        cx.simulate_click(point(at(4.), px(V1)), Modifiers::none());
        cx.simulate_click(point(at(14.), px(V1)), Modifiers::shift());

        panel.update(cx, TimelinePanel::ripple_delete_selection);

        assert_eq!(starts_on(&panel, cx, 0), seconds(&[14]));
        assert!(selected(&panel, cx).is_empty());
    }

    #[gpui::test]
    fn a_clip_that_goes_away_leaves_the_selection(cx: &mut TestAppContext) {
        let (panel, cx, clips) = timeline_with_clips(cx, &[(0, 1), (0, 12)]);
        panel.update(cx, TimelinePanel::select_all);

        panel.update(cx, |panel, cx| {
            let gone = clips[0];
            panel
                .editor
                .perform(Command::DeleteClip, cx, |project| project.delete_clip(gone))
                .ok();
        });

        assert_eq!(selected(&panel, cx), BTreeSet::from([clips[1]]));

        panel.update(cx, |panel, cx| panel.editor.undo(cx));

        assert_eq!(selected(&panel, cx), BTreeSet::from([clips[1]]));
    }

    #[gpui::test]
    fn the_view_comes_back_when_the_timeline_shrinks_under_it(cx: &mut TestAppContext) {
        let (panel, cx, clips) = timeline_with_clips(cx, &[(0, 1), (0, 12)]);
        panel.update(cx, |panel, cx| {
            let scrolled =
                Viewport::default().scrolled_by(px(ONE_SECOND * 15.), Time::from_seconds(20));
            panel.set_viewport(scrolled, cx);
        });

        assert_eq!(
            cx.read(|cx| panel.read(cx).viewport.start()),
            Time::from_seconds(15)
        );

        panel.update(cx, |panel, cx| {
            panel
                .editor
                .perform(Command::DeleteClip, cx, |project| {
                    project.delete_clips(&clips).map(drop)
                })
                .ok();
        });

        assert_eq!(cx.read(|cx| panel.read(cx).viewport.start()), Time::ZERO);
    }

    fn drag_to(cx: &mut VisualTestContext, from: Pixels, to: Point<Pixels>) {
        let none = Modifiers::none();
        cx.simulate_mouse_down(point(from, px(V1)), MouseButton::Left, none);
        cx.simulate_mouse_move(point(from + px(4.), px(V1)), MouseButton::Left, none);
        cx.simulate_mouse_move(to, MouseButton::Left, none);
        cx.simulate_mouse_up(to, MouseButton::Left, none);
    }

    #[gpui::test]
    fn a_trim_released_over_another_lane_or_the_ruler_still_commits(cx: &mut TestAppContext) {
        let (panel, cx, clip) = timeline_with_a_clip(cx);

        drag_to(cx, at(9.) - px(2.), point(at(7.) - px(2.), row_y(0)));

        assert_eq!(
            clip_of(&panel, cx, clip).timeline_range().end(),
            Time::from_seconds(7)
        );

        drag_to(cx, at(7.) - px(2.), point(at(5.) - px(2.), px(4.)));

        assert_eq!(
            clip_of(&panel, cx, clip).timeline_range().end(),
            Time::from_seconds(5)
        );
    }

    #[gpui::test]
    fn grabbing_a_trim_handle_off_the_edge_does_not_jump_the_edge(cx: &mut TestAppContext) {
        let (panel, cx, clip) = timeline_with_a_clip(cx);

        drag(cx, at(9.) - px(3.), at(9.) - px(3.) - px(ONE_SECOND));

        assert_eq!(
            clip_of(&panel, cx, clip).timeline_range().end(),
            Time::from_seconds(8)
        );
    }

    #[test]
    fn narrow_clips_keep_room_for_their_body() {
        assert_eq!(trim_handle_width(px(300.)), px(TRIM_HANDLE_WIDTH));
        assert_eq!(trim_handle_width(px(9.)), px(3.));
        assert_eq!(trim_handle_width(px(0.)), px(0.));
    }

    #[gpui::test]
    fn snapping_an_end_leaves_the_start_on_the_frame_grid(cx: &mut TestAppContext) {
        let (panel, cx, _) = timeline_with_clips(cx, &[(0, 12)]);
        let duration = Time::from_rational(351, NonZeroI64::new(100).unwrap());

        let (start, snap) = cx.read(|cx| {
            panel.read(cx).snapped_start(
                Time::from_rational(850, NonZeroI64::new(100).unwrap()),
                Some(duration),
                &[],
                cx,
            )
        });

        let frame_rate = FrameRate::FPS_30;
        assert!(snap.is_some());
        assert_eq!(start, frame_rate.frame_start(start));
        assert!(start + duration <= Time::from_seconds(12));
    }

    fn overlapped_project() -> (Project, AssetId) {
        let (mut project, asset) = {
            let mut project = Project::new("test");
            let info = MediaInfo {
                duration: Some(Time::from_seconds(8)),
                streams: vec![Stream::Video(VideoStream {
                    index: 0,
                    codec: "h264".into(),
                    width: NonZero::new(1920).unwrap(),
                    height: NonZero::new(1080).unwrap(),
                    frame_rate: None,
                })],
            };
            let asset = project.add_asset("a.mkv".into(), info);
            (project, asset)
        };
        project.place_clip(asset, 0, Time::ZERO).unwrap();
        (project, asset)
    }

    #[test]
    fn an_overlapping_drop_is_red_with_a_reason_unless_inserting_or_overwriting() {
        let (project, asset) = overlapped_project();
        let at_four = Time::from_seconds(4);

        let place = asset_drop_preview(&project, asset, 0, at_four, None, DropMode::Place).unwrap();
        let insert =
            asset_drop_preview(&project, asset, 0, at_four, None, DropMode::Insert).unwrap();
        let overwrite =
            asset_drop_preview(&project, asset, 0, at_four, None, DropMode::Overwrite).unwrap();

        assert!(!place.fits);
        assert_eq!(place.reason, Some(ASSET_OVERLAP_REASON));
        assert!(insert.fits && insert.reason.is_none());
        assert!(overwrite.fits && overwrite.reason.is_none());
        assert_eq!(insert.range.start, at_four);
        assert_eq!(
            asset_drop_preview(&project, asset, 1, at_four, None, DropMode::Insert),
            None
        );
    }

    #[test]
    fn modifier_keys_pick_the_drop_mode() {
        assert_eq!(DropMode::of(Modifiers::none()), DropMode::Place);
        assert_eq!(DropMode::of(Modifiers::control()), DropMode::Insert);
        assert_eq!(DropMode::of(Modifiers::alt()), DropMode::Overwrite);
    }

    #[gpui::test]
    fn dropping_an_asset_in_insert_mode_pushes_the_clips_after_it_right(cx: &mut TestAppContext) {
        let (panel, cx, _) = timeline_with_clips(cx, &[(0, 1)]);
        let asset = cx.read(|cx| panel.read(cx).project.read(cx).assets[0].clone());
        let (preview, dragged) = cx.read(|cx| {
            let project = panel.read(cx).project.read(cx);
            let preview = asset_drop_preview(
                project,
                asset.id,
                0,
                Time::from_seconds(4),
                None,
                DropMode::Insert,
            )
            .unwrap();
            (preview, DraggedAsset::of(&asset))
        });

        panel.update(cx, |panel, cx| {
            panel.drop_preview = Some(preview);
            panel.drop_asset_on_lane(0, &dragged, cx);
        });

        assert_eq!(starts_on(&panel, cx, 0), seconds(&[1, 4, 12]));
    }

    #[gpui::test]
    fn dropping_an_asset_in_overwrite_mode_trims_what_it_covers(cx: &mut TestAppContext) {
        let (panel, cx, _) = timeline_with_clips(cx, &[(0, 1)]);
        let asset = cx.read(|cx| panel.read(cx).project.read(cx).assets[0].clone());
        let (preview, dragged) = cx.read(|cx| {
            let project = panel.read(cx).project.read(cx);
            let preview = asset_drop_preview(
                project,
                asset.id,
                0,
                Time::from_seconds(4),
                None,
                DropMode::Overwrite,
            )
            .unwrap();
            (preview, DraggedAsset::of(&asset))
        });

        panel.update(cx, |panel, cx| {
            panel.drop_preview = Some(preview);
            panel.drop_asset_on_lane(0, &dragged, cx);
        });

        assert_eq!(starts_on(&panel, cx, 0), seconds(&[1, 4]));
    }

    #[test]
    fn only_the_clips_in_view_are_laid_out() {
        let mut track = Track::new(TrackKind::Video);
        for (id, start) in [(0, 0), (1, 10), (2, 20), (3, 30)] {
            track
                .insert(Clip {
                    id: ClipId(id),
                    asset: AssetId(0),
                    source: TimeRange::new(Time::ZERO, Time::from_seconds(5)),
                    start: Time::from_seconds(start),
                    link: None,
                    gain: tessera_timeline::Gain::UNITY,
                })
                .unwrap();
        }
        let view = TimeRange::new(Time::from_seconds(12), Time::from_seconds(10));

        let ids = |visible| -> Vec<ClipId> {
            visible_clips(&track, visible)
                .iter()
                .map(|clip| clip.id)
                .collect()
        };

        assert_eq!(ids(Some(view)), [ClipId(1), ClipId(2)]);
        assert_eq!(ids(None), [ClipId(0), ClipId(1), ClipId(2), ClipId(3)]);
    }

    fn track_of(panel: &Entity<TimelinePanel>, cx: &mut VisualTestContext, index: usize) -> Track {
        cx.read(|cx| panel.read(cx).project.read(cx).timeline.tracks[index].clone())
    }

    #[gpui::test]
    fn header_toggles_lock_mute_solo_and_resize_a_track(cx: &mut TestAppContext) {
        let (panel, cx, _) = timeline_with_a_clip(cx);
        click_add(cx, 3, TrackKind::Audio);
        let audio_row = 2;
        let audio = 2;
        let click = |cx: &mut VisualTestContext, row: usize, nth: usize| {
            cx.simulate_click(
                point(header_toggle_x(nth), header_line_y(row, 1)),
                Modifiers::none(),
            );
        };

        click(cx, V1_ROW, 0);

        assert!(track_of(&panel, cx, 0).locked);

        click(cx, V1_ROW, 0);

        assert!(!track_of(&panel, cx, 0).locked);

        click(cx, audio_row, 1);
        click(cx, audio_row, 2);

        let track = track_of(&panel, cx, audio);
        assert!(track.muted && track.solo);

        click(cx, V1_ROW, 1);

        assert!(track_of(&panel, cx, 0).muted);

        click(cx, V1_ROW, 2);

        assert_eq!(track_of(&panel, cx, 0).height, TrackHeight::Tall);

        panel.update(cx, |panel, cx| panel.editor.undo(cx));

        assert_eq!(track_of(&panel, cx, 0).height, TrackHeight::Normal);
    }

    #[gpui::test]
    fn a_shorter_track_pulls_the_rows_below_it_up(cx: &mut TestAppContext) {
        let (panel, cx, clip) = timeline_with_a_clip(cx);
        panel.update(cx, |panel, cx| {
            panel.edit_track(Command::ResizeTrack, 1, cx, |track| {
                track.height = TrackHeight::Compact;
            });
        });
        let row_center = RULER_HEIGHT + COMPACT_TRACK_HEIGHT + TRACK_HEIGHT * 0.5;

        cx.simulate_click(point(at(4.), px(row_center)), Modifiers::none());

        assert_eq!(selected(&panel, cx), BTreeSet::from([clip]));
    }

    #[gpui::test]
    fn commands_skip_the_clips_of_locked_tracks(cx: &mut TestAppContext) {
        let (panel, cx, clips) = timeline_with_clips(cx, &[(0, 1), (1, 1)]);
        seek(&panel, cx, 3);
        panel.update(cx, |panel, cx| {
            panel.edit_track(Command::LockTrack, 0, cx, |track| track.locked = true);
        });

        panel.update(cx, TimelinePanel::split_at_playhead);

        assert_eq!(starts_on(&panel, cx, 0), seconds(&[1]));
        assert_eq!(starts_on(&panel, cx, 1), seconds(&[1, 3]));

        panel.update(cx, TimelinePanel::select_all);
        panel.update(cx, TimelinePanel::delete_selection);

        assert_eq!(starts_on(&panel, cx, 0), seconds(&[1]));
        assert!(starts_on(&panel, cx, 1).is_empty());
        assert_eq!(selected(&panel, cx), BTreeSet::from([clips[0]]));
    }

    fn double_click(cx: &mut VisualTestContext, position: Point<Pixels>) {
        let (modifiers, button) = (Modifiers::none(), MouseButton::Left);
        cx.simulate_event(MouseDownEvent {
            button,
            position,
            modifiers,
            click_count: 2,
            first_mouse: false,
        });
        cx.simulate_event(MouseUpEvent {
            button,
            position,
            modifiers,
            click_count: 2,
        });
    }

    #[gpui::test]
    fn double_clicking_a_track_name_renames_it(cx: &mut TestAppContext) {
        let (panel, cx, _) = timeline_with_a_clip(cx);
        let name_at = point(px(HEADER_PADDING + 6.), header_line_y(V1_ROW, 0));

        double_click(cx, name_at);

        assert!(cx.read(|cx| panel.read(cx).renaming.is_some()));

        cx.simulate_keystrokes("b - r o l l enter");

        assert_eq!(track_of(&panel, cx, 0).name, "b-roll");
        assert!(cx.read(|cx| panel.read(cx).renaming.is_none()));

        panel.update(cx, |panel, cx| panel.editor.undo(cx));

        assert_eq!(track_of(&panel, cx, 0).name, "");
    }

    #[gpui::test]
    fn escape_leaves_a_track_name_as_it_was(cx: &mut TestAppContext) {
        let (panel, cx, _) = timeline_with_a_clip(cx);

        double_click(cx, point(px(HEADER_PADDING + 6.), header_line_y(V1_ROW, 0)));
        cx.simulate_keystrokes("x escape");

        assert_eq!(track_of(&panel, cx, 0).name, "");
        assert!(cx.read(|cx| panel.read(cx).renaming.is_none()));
    }

    fn sweep(cx: &mut VisualTestContext, from: Point<Pixels>, to: &[Point<Pixels>], shift: bool) {
        let modifiers = Modifiers {
            shift,
            ..Modifiers::none()
        };
        cx.simulate_mouse_down(from, MouseButton::Left, modifiers);
        for &point in to {
            cx.simulate_mouse_move(point, MouseButton::Left, modifiers);
        }
        let end = to.last().copied().unwrap_or(from);
        cx.simulate_mouse_up(end, MouseButton::Left, modifiers);
    }

    #[gpui::test]
    fn sweeping_empty_lane_space_selects_the_clips_it_touches(cx: &mut TestAppContext) {
        let (panel, cx, clips) = timeline_with_clips(cx, &[(0, 1), (V2, 4), (0, 10)]);
        let (v1_early, v2, v1_late) = (clips[0], clips[1], clips[2]);
        let none = Modifiers::none();

        cx.simulate_mouse_down(point(at(0.5), row_y(0)), MouseButton::Left, none);
        cx.simulate_mouse_move(point(at(2.), px(V1)), MouseButton::Left, none);

        assert_eq!(selected(&panel, cx), BTreeSet::from([v1_early]));
        assert!(cx.read(|cx| panel.read(cx).marquee.is_some()));

        cx.simulate_mouse_move(point(at(5.), px(V1)), MouseButton::Left, none);
        cx.simulate_mouse_up(point(at(5.), px(V1)), MouseButton::Left, none);

        assert_eq!(selected(&panel, cx), BTreeSet::from([v1_early, v2]));
        assert!(cx.read(|cx| panel.read(cx).marquee.is_none()));

        sweep(
            cx,
            point(at(0.5), row_y(0)),
            &[point(at(0.8), px(V1))],
            false,
        );

        assert_eq!(selected(&panel, cx), BTreeSet::new());

        cx.simulate_click(point(at(12.), px(V1)), none);
        sweep(cx, point(at(0.5), row_y(0)), &[point(at(2.), px(V1))], true);

        assert_eq!(selected(&panel, cx), BTreeSet::from([v1_early, v1_late]));
    }

    #[test]
    fn a_group_move_shows_a_ghost_for_every_selected_clip() {
        let mut project = Project::new("ghosts");
        let info = MediaInfo {
            duration: Some(Time::from_seconds(2)),
            streams: vec![Stream::Video(VideoStream {
                index: 0,
                codec: "h264".into(),
                width: NonZero::new(64).unwrap(),
                height: NonZero::new(64).unwrap(),
                frame_rate: None,
            })],
        };
        let asset = project.add_asset("a.mkv".into(), info);
        let first = project.place_clip(asset, 0, Time::ZERO).unwrap();
        let second = project.place_clip(asset, 0, Time::from_seconds(4)).unwrap();
        project.place_clip(asset, 0, Time::from_seconds(8)).unwrap();
        let selection = BTreeSet::from([first.id, second.id]);
        let preview = DropPreview {
            track: 0,
            range: TimeRange::new(Time::from_seconds(1), Time::from_seconds(2)),
            fits: false,
            reason: Some(OVERLAP_REASON),
            mode: DropMode::Place,
            snapped_to: None,
            group: Some(GroupShift {
                shift: Time::from_seconds(1),
                from: 0,
            }),
            partner: None,
        };
        let tracks = &project.timeline.tracks;

        let ghosts = ghosts_on(0, tracks, Some(preview), &selection);

        let starts: Vec<Time> = ghosts.iter().map(|ghost| ghost.range.start).collect();
        let reasons: Vec<_> = ghosts.iter().map(|ghost| ghost.reason).collect();
        assert_eq!(starts, seconds(&[1, 5]));
        assert_eq!(reasons, [Some(OVERLAP_REASON), None]);
        assert!(ghosts.iter().all(|ghost| !ghost.fits));
        assert!(ghosts_on(1, tracks, Some(preview), &BTreeSet::new()).is_empty());
    }

    fn with_sound(info: MediaInfo) -> MediaInfo {
        let mut streams = info.streams;
        streams.push(Stream::Audio(tessera_timeline::AudioStream {
            index: 1,
            codec: "aac".into(),
            sample_rate: NonZero::new(48_000).unwrap(),
            channels: NonZero::new(2).unwrap(),
        }));
        MediaInfo { streams, ..info }
    }

    fn timeline_with_a_linked_pair(
        cx: &mut TestAppContext,
    ) -> (Entity<TimelinePanel>, &mut VisualTestContext, Clip, Clip) {
        let mut project = Project::new("linked");
        project.timeline.add_track(TrackKind::Video);
        let info = with_sound(MediaInfo {
            duration: Some(Time::from_seconds(8)),
            streams: vec![Stream::Video(VideoStream {
                index: 0,
                codec: "h264".into(),
                width: NonZero::new(1920).unwrap(),
                height: NonZero::new(1080).unwrap(),
                frame_rate: None,
            })],
        });
        let asset = project.add_asset("a.mkv".into(), info);
        let pair = project
            .place_linked(asset, 0, Time::from_seconds(1))
            .unwrap();
        let project = cx.new(|_| project);
        let (panel, cx) = cx.add_window_view(|_, cx| {
            let playhead = cx.new(|cx| Playhead::new(project.clone(), cx));
            TimelinePanel::new(ProjectEditor::new(project, cx), playhead, cx)
        });
        (panel, cx, pair[0], pair[1])
    }

    const A1_ROW: usize = 2;

    #[gpui::test]
    fn pressing_a_linked_clip_selects_its_partner_and_dragging_moves_both(cx: &mut TestAppContext) {
        let (panel, cx, picture, sound) = timeline_with_a_linked_pair(cx);

        cx.simulate_click(point(at(4.), row_y(A1_ROW)), Modifiers::none());

        assert_eq!(selected(&panel, cx), BTreeSet::from([picture.id, sound.id]));

        cx.simulate_click(point(at(4.), px(V1)), Modifiers::shift());

        assert!(selected(&panel, cx).is_empty());

        drag(cx, at(4.), at(6.));

        assert_eq!(clip_of(&panel, cx, picture.id).start, Time::from_seconds(3));
        assert_eq!(clip_of(&panel, cx, sound.id).start, Time::from_seconds(3));
    }

    #[gpui::test]
    fn splitting_and_unlinking_a_linked_pair(cx: &mut TestAppContext) {
        let (panel, cx, picture, sound) = timeline_with_a_linked_pair(cx);
        seek(&panel, cx, 4);
        cx.simulate_click(point(at(2.), px(V1)), Modifiers::none());

        panel.update(cx, TimelinePanel::split_at_playhead);

        assert_eq!(starts_on(&panel, cx, 0), seconds(&[1, 4]));
        assert_eq!(starts_on(&panel, cx, 2), seconds(&[1, 4]));

        panel.update(cx, TimelinePanel::toggle_link_selection);
        cx.simulate_click(point(at(12.), px(V1)), Modifiers::none());
        cx.simulate_click(point(at(2.), px(V1)), Modifiers::none());

        assert_eq!(selected(&panel, cx), BTreeSet::from([picture.id]));
        assert_eq!(clip_of(&panel, cx, sound.id).link, None);

        cx.simulate_click(point(at(2.), row_y(A1_ROW)), Modifiers::shift());
        panel.update(cx, TimelinePanel::toggle_link_selection);
        cx.simulate_click(point(at(12.), px(V1)), Modifiers::none());
        cx.simulate_click(point(at(2.), px(V1)), Modifiers::none());

        assert_eq!(selected(&panel, cx), BTreeSet::from([picture.id, sound.id]));
    }

    #[test]
    fn dropping_media_with_sound_previews_its_partner_on_an_audio_track() {
        let mut project = Project::new("drop");
        let info = with_sound(MediaInfo {
            duration: Some(Time::from_seconds(2)),
            streams: vec![Stream::Video(VideoStream {
                index: 0,
                codec: "h264".into(),
                width: NonZero::new(64).unwrap(),
                height: NonZero::new(64).unwrap(),
                frame_rate: None,
            })],
        });
        let asset = project.add_asset("a.mkv".into(), info);

        let preview =
            asset_drop_preview(&project, asset, 0, Time::ZERO, None, DropMode::Place).unwrap();
        let ghosts = ghosts_on(1, &project.timeline.tracks, Some(preview), &BTreeSet::new());

        assert_eq!(preview.partner, Some(1));
        assert_eq!(ghosts.len(), 1);
        assert_eq!(ghosts[0].reason, None);

        let inserting =
            asset_drop_preview(&project, asset, 0, Time::ZERO, None, DropMode::Insert).unwrap();

        assert_eq!(inserting.partner, Some(1));
    }

    #[gpui::test]
    fn scrolling_the_volume_readout_turns_an_audio_track_up_and_double_click_resets_it(
        cx: &mut TestAppContext,
    ) {
        let (panel, cx, _) = timeline_with_a_clip(cx);
        let readout = point(
            px(HEADER_RIGHT - HEADER_PADDING - 8.),
            header_line_y(A1_ROW, 1),
        );
        let scroll = |cx: &mut VisualTestContext, lines: f32| {
            cx.simulate_event(ScrollWheelEvent {
                position: readout,
                delta: ScrollDelta::Lines(point(0., lines)),
                modifiers: Modifiers::none(),
                touch_phase: TouchPhase::Moved,
            });
        };
        let volume = |cx: &mut VisualTestContext| track_of(&panel, cx, 2).volume.tenths();

        scroll(cx, 1.);
        scroll(cx, 1.);

        assert_eq!(volume(cx), 20);

        scroll(cx, -1.);

        assert_eq!(volume(cx), 10);

        double_click(cx, readout);

        assert_eq!(volume(cx), 0);
    }

    #[gpui::test]
    fn alt_arrows_change_the_gain_of_selected_audio_clips_only(cx: &mut TestAppContext) {
        let (panel, cx, picture, sound) = timeline_with_a_linked_pair(cx);
        cx.simulate_click(point(at(4.), px(V1)), Modifiers::none());

        panel.update(cx, TimelinePanel::raise_clip_gain);
        panel.update(cx, TimelinePanel::raise_clip_gain);
        panel.update(cx, TimelinePanel::lower_clip_gain);

        assert_eq!(clip_of(&panel, cx, sound.id).gain.tenths(), 10);
        assert_eq!(clip_of(&panel, cx, picture.id).gain, Gain::UNITY);
    }

    fn viewport_of(panel: &Entity<TimelinePanel>, cx: &mut VisualTestContext) -> Viewport {
        cx.read(|cx| panel.read(cx).viewport)
    }

    fn lanes_of(panel: &Entity<TimelinePanel>, cx: &mut VisualTestContext) -> Bounds<Pixels> {
        cx.read(|cx| panel.read(cx).lanes)
    }

    #[gpui::test]
    fn dragging_near_the_right_edge_scrolls_and_the_ghost_stays_under_the_pointer(
        cx: &mut TestAppContext,
    ) {
        let (panel, cx, _) = timeline_with_a_clip(cx);
        let lanes = lanes_of(&panel, cx);
        let none = Modifiers::none();
        let near_edge = lanes.right() - px(4.);

        cx.simulate_mouse_down(point(at(4.), px(V1)), MouseButton::Left, none);
        cx.simulate_mouse_move(point(at(4.) + px(4.), px(V1)), MouseButton::Left, none);
        cx.simulate_mouse_move(point(near_edge, px(V1)), MouseButton::Left, none);
        cx.executor().advance_clock(AUTOSCROLL_TICK * 10);
        cx.run_until_parked();

        let viewport = viewport_of(&panel, cx);
        let ghost = cx.read(|cx| panel.read(cx).drop_preview.unwrap().range.start);
        let grab = at(4.) - at(1.);

        assert!(viewport.start() > Time::ZERO);
        assert_eq!(
            ghost,
            FrameRate::FPS_30.frame_start(viewport.time_at(near_edge - lanes.left() - grab))
        );

        cx.simulate_mouse_up(point(near_edge, px(V1)), MouseButton::Left, none);
    }

    #[gpui::test]
    fn dragging_the_horizontal_scrollbar_scrolls_the_timeline(cx: &mut TestAppContext) {
        let (panel, cx, _) = timeline_with_a_clip(cx);
        for _ in 0..4 {
            panel.update(cx, TimelinePanel::zoom_in);
        }
        cx.run_until_parked();
        let lanes = lanes_of(&panel, cx);
        let height = cx.update(|window, _| window.viewport_size().height);
        let bar_y = height - px(SCROLLBAR_THICKNESS / 2.);
        let none = Modifiers::none();

        assert_eq!(viewport_of(&panel, cx).start(), Time::ZERO);

        cx.simulate_mouse_down(point(lanes.left() + px(2.), bar_y), MouseButton::Left, none);
        cx.simulate_mouse_move(
            point(lanes.left() + lanes.size.width / 2., bar_y),
            MouseButton::Left,
            none,
        );
        cx.simulate_mouse_up(
            point(lanes.left() + lanes.size.width / 2., bar_y),
            MouseButton::Left,
            none,
        );

        assert!(viewport_of(&panel, cx).start() > Time::ZERO);
        assert!(cx.read(|cx| panel.read(cx).scrolling.is_none()));
    }

    const MARKER_Y: f32 = RULER_HEIGHT - MARKER_HEIGHT / 2.;

    fn add_marker(
        panel: &Entity<TimelinePanel>,
        cx: &mut VisualTestContext,
        seconds: i64,
    ) -> MarkerId {
        panel.update(cx, |panel, cx| {
            panel.editor.perform(Command::AddMarker, cx, |project| {
                project.add_marker(Time::from_seconds(seconds), "")
            })
        })
    }

    fn marker_of(
        panel: &Entity<TimelinePanel>,
        cx: &mut VisualTestContext,
        id: MarkerId,
    ) -> Marker {
        cx.read(|cx| panel.read(cx).project.read(cx).marker(id).unwrap().clone())
    }

    fn playhead_time(panel: &Entity<TimelinePanel>, cx: &mut VisualTestContext) -> Time {
        cx.read(|cx| panel.read(cx).playhead.read(cx).time())
    }

    #[gpui::test]
    fn dragging_a_marker_flag_moves_the_marker_and_leaves_the_playhead(cx: &mut TestAppContext) {
        let (panel, cx, _) = timeline_with_a_clip(cx);
        let marker = add_marker(&panel, cx, 2);
        let (none, left, y) = (Modifiers::none(), MouseButton::Left, px(MARKER_Y));

        cx.simulate_mouse_down(point(at(2.), y), left, none);
        cx.simulate_mouse_move(point(at(3.), y), left, none);

        assert_eq!(marker_of(&panel, cx, marker).time, Time::from_seconds(2));
        assert_eq!(
            cx.read(|cx| panel.read(cx).marker_drag.map(|drag| drag.time)),
            Some(Time::from_seconds(3))
        );

        cx.simulate_mouse_move(point(at(5.), y), left, none);
        cx.simulate_mouse_up(point(at(5.), y), left, none);

        assert_eq!(marker_of(&panel, cx, marker).time, Time::from_seconds(5));
        assert_eq!(playhead_time(&panel, cx), Time::ZERO);
        assert!(cx.read(|cx| panel.read(cx).marker_drag.is_none()));

        panel.update(cx, |panel, cx| panel.editor.undo(cx));

        assert_eq!(marker_of(&panel, cx, marker).time, Time::from_seconds(2));
    }

    #[gpui::test]
    fn double_clicking_a_marker_flag_names_it(cx: &mut TestAppContext) {
        let (panel, cx, _) = timeline_with_a_clip(cx);
        let marker = add_marker(&panel, cx, 2);

        double_click(cx, point(at(2.), px(MARKER_Y)));

        assert_eq!(
            cx.read(|cx| panel
                .read(cx)
                .renaming
                .as_ref()
                .map(|renaming| renaming.target)),
            Some(RenameTarget::Marker(marker))
        );

        cx.simulate_keystrokes("i n t r o enter");

        assert_eq!(marker_of(&panel, cx, marker).name, "intro");
        assert_eq!(marker_of(&panel, cx, marker).time, Time::from_seconds(2));
        assert!(cx.read(|cx| panel.read(cx).renaming.is_none()));
    }

    #[gpui::test]
    fn pressing_the_ruler_away_from_a_marker_scrubs(cx: &mut TestAppContext) {
        let (panel, cx, _) = timeline_with_a_clip(cx);
        let marker = add_marker(&panel, cx, 2);

        cx.simulate_click(point(at(4.), px(MARKER_Y)), Modifiers::none());

        assert_eq!(playhead_time(&panel, cx), Time::from_seconds(4));
        assert_eq!(marker_of(&panel, cx, marker).time, Time::from_seconds(2));

        cx.simulate_click(
            point(at(2.), px(MARKER_Y - MARKER_HEIGHT)),
            Modifiers::none(),
        );

        assert_eq!(playhead_time(&panel, cx), Time::from_seconds(2));
    }
}
