mod header;
mod snap;
mod viewport;

use std::collections::BTreeSet;

use gpui::{
    App, AppContext, Bounds, Context, CursorStyle, DispatchPhase, DragMoveEvent, Entity,
    FocusHandle, Hitbox, HitboxBehavior, InteractiveElement, IntoElement, Modifiers, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement, Pixels, Render, Rgba,
    ScrollWheelEvent, SharedString, StatefulInteractiveElement, Styled, Subscription, Window,
    canvas, div, fill, point, prelude::FluentBuilder, px, size,
};
use tessera_timeline::{
    AssetId, Clip, ClipEdge, ClipId, Command, EditError, FrameRate, Project, Time, TimeRange,
    Timecode, Timeline, Track, TrackKind,
};

use self::{
    header::{add_track_row, content_height, next_height_of, row_height, track_header, track_rows},
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
    drop_preview: Option<DropPreview>,
    viewport: Viewport,
    lanes: Bounds<Pixels>,
    selection: BTreeSet<ClipId>,
    grab: Pixels,
    track_scroll: Pixels,
    tracks_view: Bounds<Pixels>,
    snapping: bool,
    clipboard: Vec<(Clip, usize)>,
    renaming: Option<TrackRename>,
    focus_return: Option<FocusHandle>,
}

struct TrackRename {
    track: usize,
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

    fn start_rename(&mut self, track: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(current) = self
            .project
            .read(cx)
            .timeline
            .tracks
            .get(track)
            .map(|track| track.name.clone())
        else {
            return;
        };
        let field = cx.new(|cx| TextField::new(current, "Track name", cx));
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
        self.renaming = Some(TrackRename {
            track,
            field,
            _subscriptions: subscriptions,
        });
        cx.notify();
    }

    fn finish_rename(&mut self, name: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(renaming) = self.renaming.take() else {
            return;
        };
        if let Some(name) = name {
            self.edit_track(Command::RenameTrack, renaming.track, cx, |track| {
                track.name = name;
            });
        }
        if let Some(handle) = &self.focus_return {
            window.focus(handle);
        }
        cx.notify();
    }

    pub fn drag_cancelled(&mut self, cx: &mut Context<Self>) {
        self.drop_preview = None;
        cx.notify();
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
            targets
                .into_iter()
                .try_for_each(|id| project.split_clip(id, time).map(drop))
        });
        if let Err(error) = split {
            tracing::warn!(%error, "could not split the clips");
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

    fn press_lane(&mut self, track: usize, offset: Pixels, extend: bool, cx: &mut Context<Self>) {
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
        match pressed {
            Some(clip) if extend => {
                if !self.selection.remove(&clip.id) {
                    self.selection.insert(clip.id);
                }
            }
            Some(clip) => {
                if !self.selection.contains(&clip.id) {
                    self.selection = BTreeSet::from([clip.id]);
                }
            }
            None if !extend => self.selection.clear(),
            None => {}
        }
        if self.selection != before {
            cx.notify();
        }
    }

    fn drag_asset_over_lane(
        &mut self,
        track: usize,
        event: &DragMoveEvent<DraggedAsset>,
        cx: &mut Context<Self>,
    ) {
        let asset = event.drag(cx).id;
        let mode = DropMode::of(event.event.modifiers);
        self.hover_lane(track, event, cx, |panel, offset, cx| {
            let project = panel.project.read(cx);
            let duration = project
                .asset(asset)
                .and_then(|asset| asset.default_clip_duration());
            let (start, snap) = panel.snapped_start(panel.frame_at(offset, cx), duration, &[], cx);
            asset_drop_preview(project, asset, track, start, snap, mode)
        });
    }

    fn drag_clip_over_lane(
        &mut self,
        track: usize,
        event: &DragMoveEvent<DraggedClip>,
        cx: &mut Context<Self>,
    ) {
        let DraggedClip { id, grip } = *event.drag(cx);
        match grip {
            Grip::Body => {
                let grab = self.grab;
                let group: Vec<ClipId> = if self.selection.contains(&id) {
                    self.selection.iter().copied().collect()
                } else {
                    vec![id]
                };
                self.hover_lane(track, event, cx, |panel, offset, cx| {
                    let project = panel.project.read(cx);
                    let (clip_track, clip) = project.find_clip(id)?;
                    let start = panel.frame_at(offset - grab, cx);
                    let (start, snap) =
                        panel.snapped_start(start, Some(clip.source.duration), &group, cx);
                    if group.len() == 1 {
                        return placement(
                            track,
                            project.moved_clip(id, track, start),
                            snap,
                            DropMode::Place,
                        );
                    }
                    if clip_track != track {
                        return None;
                    }
                    let moved =
                        project.moved_clips(&group_moves(project, &group, start - clip.start));
                    let fits = match moved {
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
                    };
                    Some(preview.snapped(snap))
                });
            }
            Grip::Edge(edge) => {
                let offset = event.event.position.x - event.bounds.left();
                let project = self.project.read(cx);
                let Some((clip_track, clip)) = project.find_clip(id) else {
                    return;
                };
                if clip_track != track {
                    return;
                }
                let anchor = match edge {
                    ClipEdge::Start => self.grab,
                    ClipEdge::End => self.grab - self.viewport.width_of(clip.source.duration),
                };
                let to = self.frame_at(offset - anchor, cx);
                let snap = self.snap(&[to], &[id], cx);
                let to = to + snap.map_or(Time::ZERO, |snap| snap.shift);
                let project = self.project.read(cx);
                let preview = placement(
                    track,
                    project.trimmed_clip(id, edge, to),
                    snap,
                    DropMode::Place,
                );
                self.show_preview(preview, cx);
            }
        }
    }

    fn hover_lane<T: 'static>(
        &mut self,
        track: usize,
        event: &DragMoveEvent<T>,
        cx: &mut Context<Self>,
        preview: impl FnOnce(&Self, Pixels, &App) -> Option<DropPreview>,
    ) {
        let position = event.event.position;
        let preview = if event.bounds.contains(&position) {
            preview(self, position.x - event.bounds.left(), cx)
        } else if self
            .drop_preview
            .is_some_and(|preview| preview.track == track)
        {
            None
        } else {
            return;
        };
        self.show_preview(preview, cx);
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
                    DropMode::Place => project.place_clip(asset, preview.track, start),
                    DropMode::Insert => project.insert_asset(asset, preview.track, start),
                    DropMode::Overwrite => project.overwrite_asset(asset, preview.track, start),
                }
                .map(|clip| vec![clip])
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
                    let start = project.find_clip(id).map(|(_, clip)| clip.start);
                    let delta = preview.range.start - start.ok_or(EditError::UnknownClip(id))?;
                    project.move_clips(&group_moves(project, &group, delta))
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
        let preview = self
            .drop_preview
            .take()
            .filter(|preview| preview.fits && accepts(preview));
        if let Some(preview) = preview {
            match self
                .editor
                .apply(command, cx, |project| edit(project, preview))
            {
                Ok(clips) => self.selection = clips.iter().map(|clip| clip.id).collect(),
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
        }
        let panel = cx.entity();
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
        let lanes = scrolled_tracks(scroll, lanes).child(tracks_view_probe(panel.clone()));
        let renaming = self
            .renaming
            .as_ref()
            .map(|renaming| (renaming.track, renaming.field.clone()));
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
                    .child(scrolled_tracks(scroll, headers)),
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
                    .children(in_out_range(viewport, project.in_point, project.out_point))
                    .children(
                        project
                            .markers
                            .iter()
                            .map(|marker| marker_flag(viewport.x_at(marker.time))),
                    )
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
        move |bounds, _, cx| panel.update(cx, |panel, _| panel.tracks_view = bounds),
        |_, _, _, _| {},
    )
    .absolute()
    .size_full()
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
    let ghost = drop_preview
        .filter(|preview| preview.track == index)
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
                let offset = event.position.x - panel.lanes.left();
                panel.press_lane(index, offset, event.modifiers.shift, cx);
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
        .children(ghost)
}

fn visible_clips(track: &Track, visible: Option<TimeRange>) -> &[Clip] {
    visible.map_or(track.clips(), |range| track.clips_overlapping(range))
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

fn group_moves(project: &Project, group: &[ClipId], delta: Time) -> Vec<(ClipId, usize, Time)> {
    group
        .iter()
        .filter_map(|id| project.find_clip(*id))
        .map(|(track, clip)| (clip.id, track, clip.start + delta))
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
    let preview = placement(track, project.clip_for(asset, track, start), snap, mode)?;
    Some(DropPreview {
        reason: preview.reason.map(|_| ASSET_OVERLAP_REASON),
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

fn marker_flag(x: Pixels) -> impl IntoElement {
    div()
        .absolute()
        .top(px(RULER_HEIGHT - MARKER_HEIGHT))
        .left(x - px(MARKER_WIDTH / 2.))
        .w(px(MARKER_WIDTH))
        .h(px(MARKER_HEIGHT))
        .rounded_t_sm()
        .bg(theme::marker())
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
}
