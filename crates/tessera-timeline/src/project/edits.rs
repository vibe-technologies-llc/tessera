use std::collections::BTreeSet;

use super::{Clip, ClipEdge, ClipId, EditError, Project, TimeRange};
use crate::time::Time;

impl Project {
    fn source_limit(&self, clip: &Clip) -> Time {
        self.asset(clip.asset)
            .and_then(|asset| asset.info.duration)
            .map_or(clip.source.end(), |duration| {
                duration.max(clip.source.end())
            })
    }

    fn known_clips(&self, ids: &[ClipId]) -> Result<BTreeSet<ClipId>, EditError> {
        ids.iter()
            .map(|&id| {
                self.find_clip(id)
                    .map(|_| id)
                    .ok_or(EditError::UnknownClip(id))
            })
            .collect()
    }

    fn atomically<T>(
        &mut self,
        edit: impl FnOnce(&mut Self) -> Result<T, EditError>,
    ) -> Result<T, EditError> {
        let timeline = self.timeline.clone();
        let next_ids = self.next_ids;
        let edited = edit(self);
        if edited.is_err() {
            self.timeline = timeline;
            self.next_ids = next_ids;
        }
        edited
    }

    fn set_clip(&mut self, track: usize, clip: Clip) {
        let slot = self.timeline.tracks[track]
            .clips
            .iter_mut()
            .find(|other| other.id == clip.id);
        if let Some(slot) = slot {
            *slot = clip;
        }
    }

    fn shift_clips_from(
        &mut self,
        track: usize,
        from: Time,
        delta: Time,
        except: ClipId,
    ) -> Result<(), EditError> {
        for later in &mut self.timeline.tracks[track].clips {
            if later.start >= from && later.id != except {
                let shifted = Clip {
                    start: later
                        .start
                        .checked_add(delta)
                        .ok_or(super::InvalidClip::EndOverflows(later.id))?,
                    ..*later
                };
                shifted.check()?;
                *later = shifted;
            }
        }
        Ok(())
    }

    pub fn delete_clips(&mut self, ids: &[ClipId]) -> Result<Vec<Clip>, EditError> {
        let ids = self.known_clips(ids)?;
        let mut deleted = Vec::new();
        for track in &mut self.timeline.tracks {
            let (gone, kept) = std::mem::take(&mut track.clips)
                .into_iter()
                .partition(|clip| ids.contains(&clip.id));
            track.clips = kept;
            deleted.extend(gone);
        }
        Ok(deleted)
    }

    pub fn ripple_delete_clips(&mut self, ids: &[ClipId]) -> Result<Vec<Clip>, EditError> {
        let ids = self.known_clips(ids)?;
        let mut deleted = Vec::new();
        for track in &mut self.timeline.tracks {
            let mut pulled = Time::ZERO;
            let mut kept = Vec::new();
            for clip in std::mem::take(&mut track.clips) {
                if ids.contains(&clip.id) {
                    pulled = pulled + clip.source.duration;
                    deleted.push(clip);
                } else {
                    kept.push(Clip {
                        start: clip.start - pulled,
                        ..clip
                    });
                }
            }
            track.clips = kept;
        }
        Ok(deleted)
    }

    pub fn move_clips(&mut self, moves: &[(ClipId, usize, Time)]) -> Result<Vec<Clip>, EditError> {
        let mut named = BTreeSet::new();
        if let Some(&(id, ..)) = moves.iter().find(|(id, ..)| !named.insert(*id)) {
            return Err(EditError::RepeatedClip(id));
        }
        self.atomically(|project| {
            let mut planned = Vec::new();
            for &(id, track, start) in moves {
                let (_, clip) = project.located_clip(id)?;
                let asset = project
                    .asset(clip.asset)
                    .ok_or(EditError::UnknownAsset(clip.asset))?;
                project.track_accepting(asset, track)?;
                planned.push((
                    track,
                    Clip {
                        start: start.max(Time::ZERO),
                        ..clip
                    },
                ));
            }
            for (_, clip) in &planned {
                if let Some((from, _)) = project.find_clip(clip.id) {
                    project.timeline.tracks[from].remove(clip.id);
                }
            }
            for &(track, clip) in &planned {
                project.timeline.tracks[track].insert(clip)?;
            }
            Ok(planned.into_iter().map(|(_, clip)| clip).collect())
        })
    }

    pub fn insert_asset(
        &mut self,
        asset: super::AssetId,
        track: usize,
        start: Time,
    ) -> Result<Clip, EditError> {
        let clip = self.whole_clip(asset, track, start)?;
        self.insert_template(clip, track)
    }

    pub fn insert_clip(
        &mut self,
        clip: &Clip,
        track: usize,
        start: Time,
    ) -> Result<Clip, EditError> {
        let template = self.copied_clip(clip, track, start)?;
        self.insert_template(template, track)
    }

    pub fn overwrite_asset(
        &mut self,
        asset: super::AssetId,
        track: usize,
        start: Time,
    ) -> Result<Clip, EditError> {
        let clip = self.whole_clip(asset, track, start)?;
        self.overwrite_template(clip, track)
    }

    pub fn overwrite_clip(
        &mut self,
        clip: &Clip,
        track: usize,
        start: Time,
    ) -> Result<Clip, EditError> {
        let template = self.copied_clip(clip, track, start)?;
        self.overwrite_template(template, track)
    }

    fn insert_template(&mut self, template: Clip, track: usize) -> Result<Clip, EditError> {
        template.check()?;
        self.atomically(|project| {
            let placed = Clip {
                id: project.next_ids.take_clip(),
                ..template
            };
            let cut = project.timeline.tracks[track]
                .clip_at(placed.start)
                .filter(|clip| clip.is_cut_by(placed.start))
                .map(|clip| clip.id);
            if let Some(cut) = cut {
                project.split_clip(cut, placed.start)?;
            }
            project.shift_clips_from(track, placed.start, placed.source.duration, placed.id)?;
            project.timeline.tracks[track].insert(placed)?;
            Ok(placed)
        })
    }

    fn overwrite_template(&mut self, template: Clip, track: usize) -> Result<Clip, EditError> {
        template.check()?;
        self.atomically(|project| {
            let placed = Clip {
                id: project.next_ids.take_clip(),
                ..template
            };
            project.clear_range(track, placed.timeline_range())?;
            project.timeline.tracks[track].insert(placed)?;
            Ok(placed)
        })
    }

    fn clear_range(&mut self, track: usize, range: TimeRange) -> Result<(), EditError> {
        let covered = self.timeline.tracks[track]
            .clips_overlapping(range)
            .to_vec();
        for clip in covered {
            self.timeline.tracks[track].remove(clip.id);
            let kept_head = clip.start < range.start;
            if kept_head {
                let head = Clip {
                    source: TimeRange::new(clip.source.start, range.start - clip.start),
                    ..clip
                };
                self.timeline.tracks[track].insert(head)?;
            }
            if clip.timeline_range().end() > range.end() {
                let cut = range.end() - clip.start;
                let tail = Clip {
                    id: if kept_head {
                        self.next_ids.take_clip()
                    } else {
                        clip.id
                    },
                    start: range.end(),
                    source: TimeRange::new(clip.source.start + cut, clip.source.duration - cut),
                    ..clip
                };
                self.timeline.tracks[track].insert(tail)?;
            }
        }
        Ok(())
    }

    pub fn ripple_trim_clip(
        &mut self,
        id: ClipId,
        edge: ClipEdge,
        to: Time,
    ) -> Result<Clip, EditError> {
        let (track, clip) = self.located_clip(id)?;
        let shortest = self.settings.frame_rate.frame_duration();
        let trimmed = match edge {
            ClipEdge::Start => {
                let shift = (to - clip.start).clamp(
                    Time::ZERO - clip.source.start,
                    (clip.source.duration - shortest).max(Time::ZERO),
                );
                Clip {
                    source: TimeRange::new(clip.source.start + shift, clip.source.duration - shift),
                    ..clip
                }
            }
            ClipEdge::End => {
                let longest = self.source_limit(&clip) - clip.source.start;
                let duration = (to - clip.start).clamp(
                    shortest.min(clip.source.duration),
                    longest.max(clip.source.duration),
                );
                Clip {
                    source: TimeRange::new(clip.source.start, duration),
                    ..clip
                }
            }
        };
        let delta = trimmed.source.duration - clip.source.duration;
        self.atomically(|project| {
            project.shift_clips_from(track, clip.timeline_range().end(), delta, id)?;
            project.set_clip(track, trimmed);
            Ok(trimmed)
        })
    }

    pub fn roll_clips(
        &mut self,
        left: ClipId,
        right: ClipId,
        to: Time,
    ) -> Result<(Clip, Clip), EditError> {
        let (left_track, before) = self.located_clip(left)?;
        let (right_track, after) = self.located_clip(right)?;
        let cut = before.timeline_range().end();
        if left_track != right_track || cut != after.start {
            return Err(EditError::NotAdjacent { left, right });
        }
        let shortest = self.settings.frame_rate.frame_duration();
        let earliest = (before.start + shortest).max(after.start - after.source.start);
        let latest = (before.start + (self.source_limit(&before) - before.source.start))
            .min(after.timeline_range().end() - shortest);
        let to = to.clamp(earliest.min(cut), latest.max(cut));
        let rolled_left = Clip {
            source: TimeRange::new(before.source.start, to - before.start),
            ..before
        };
        let rolled_right = Clip {
            start: to,
            source: TimeRange::new(
                after.source.start + (to - after.start),
                after.timeline_range().end() - to,
            ),
            ..after
        };
        self.set_clip(left_track, rolled_left);
        self.set_clip(right_track, rolled_right);
        Ok((rolled_left, rolled_right))
    }

    pub fn slip_clip(&mut self, id: ClipId, by: Time) -> Result<Clip, EditError> {
        let (track, clip) = self.located_clip(id)?;
        let latest = (self.source_limit(&clip) - clip.source.duration)
            .max(clip.source.start)
            .max(Time::ZERO);
        let slipped = Clip {
            source: TimeRange::new(
                (clip.source.start + by).clamp(Time::ZERO, latest),
                clip.source.duration,
            ),
            ..clip
        };
        self.set_clip(track, slipped);
        Ok(slipped)
    }

    pub fn slide_clip(&mut self, id: ClipId, to: Time) -> Result<Vec<Clip>, EditError> {
        let (track, clip) = self.located_clip(id)?;
        let shortest = self.settings.frame_rate.frame_duration();
        let clips = self.timeline.tracks[track].clips();
        let index = clips.iter().position(|other| other.id == id);
        let previous = index
            .and_then(|index| index.checked_sub(1))
            .map(|index| clips[index]);
        let next = index.and_then(|index| clips.get(index + 1)).copied();

        let mut earliest = Time::ZERO;
        let mut latest = Time::MAX - clip.source.duration;
        if let Some(previous) = previous {
            if previous.timeline_range().end() == clip.start {
                let room = self.source_limit(&previous) - previous.source.end();
                earliest = earliest.max(previous.start + shortest);
                latest = latest.min(clip.start + room);
            } else {
                earliest = earliest.max(previous.timeline_range().end());
            }
        }
        if let Some(next) = next {
            if clip.timeline_range().end() == next.start {
                earliest = earliest.max(clip.start - next.source.start);
                latest = latest.min(clip.start + (next.source.duration - shortest));
            } else {
                latest = latest.min(next.start - clip.source.duration);
            }
        }

        let start = to.clamp(earliest.min(clip.start), latest.max(clip.start));
        let shift = start - clip.start;
        let mut changed = Vec::new();
        if let Some(previous) =
            previous.filter(|previous| previous.timeline_range().end() == clip.start)
        {
            changed.push(Clip {
                source: TimeRange::new(previous.source.start, previous.source.duration + shift),
                ..previous
            });
        }
        let slid = Clip { start, ..clip };
        changed.push(slid);
        if let Some(next) = next.filter(|next| next.start == clip.timeline_range().end()) {
            changed.push(Clip {
                start: next.start + shift,
                source: TimeRange::new(next.source.start + shift, next.source.duration - shift),
                ..next
            });
        }
        for clip in &changed {
            self.set_clip(track, *clip);
        }
        Ok(changed)
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZero;

    use super::*;
    use crate::{
        media::{MediaInfo, Stream, VideoStream},
        project::{AssetId, InvalidClip, TrackKind},
        time::FrameRate,
    };

    fn seconds(seconds: i64) -> Time {
        Time::from_seconds(seconds)
    }

    fn project_with_asset() -> (Project, AssetId) {
        let mut project = Project::new("test");
        let asset = project.add_asset(
            "a.mkv".into(),
            MediaInfo {
                duration: Some(seconds(10)),
                streams: vec![Stream::Video(VideoStream {
                    index: 0,
                    codec: "h264".into(),
                    width: NonZero::new(1280).unwrap(),
                    height: NonZero::new(720).unwrap(),
                    frame_rate: Some(FrameRate::FPS_30),
                })],
            },
        );
        (project, asset)
    }

    fn place(project: &mut Project, asset: AssetId, start: i64) -> ClipId {
        project.place_clip(asset, 0, seconds(start)).unwrap().id
    }

    fn put(
        project: &mut Project,
        asset: AssetId,
        start: i64,
        source_start: i64,
        duration: i64,
    ) -> ClipId {
        let id = project.next_ids.take_clip();
        let clip = Clip {
            id,
            asset,
            start: seconds(start),
            source: TimeRange::new(seconds(source_start), seconds(duration)),
        };
        project.timeline.tracks[0].insert(clip).unwrap();
        id
    }

    fn layout(project: &Project) -> Vec<(i64, i64, i64)> {
        project.timeline.tracks[0]
            .clips()
            .iter()
            .map(|clip| {
                (
                    clip.start.flicks() / crate::time::FLICKS_PER_SECOND,
                    clip.source.start.flicks() / crate::time::FLICKS_PER_SECOND,
                    clip.source.duration.flicks() / crate::time::FLICKS_PER_SECOND,
                )
            })
            .collect()
    }

    #[test]
    fn several_clips_are_deleted_at_once_and_unknown_ones_refuse_the_lot() {
        let (mut project, asset) = project_with_asset();
        let a = place(&mut project, asset, 0);
        let b = place(&mut project, asset, 10);
        let c = place(&mut project, asset, 20);

        let before = project.clone();

        assert_eq!(
            project.delete_clips(&[a, ClipId(99)]),
            Err(EditError::UnknownClip(ClipId(99)))
        );
        assert_eq!(project, before);

        let deleted = project.delete_clips(&[c, a, c]).unwrap();

        assert_eq!(
            deleted.iter().map(|clip| clip.id).collect::<Vec<_>>(),
            [a, c]
        );
        assert_eq!(project.timeline.tracks[0].clips().len(), 1);
        assert!(project.find_clip(b).is_some());
    }

    #[test]
    fn rippling_several_clips_pulls_the_rest_back_by_their_combined_length() {
        let (mut project, asset) = project_with_asset();
        let first = place(&mut project, asset, 0);
        place(&mut project, asset, 10);
        let third = place(&mut project, asset, 20);
        place(&mut project, asset, 30);
        project.split_clip(third, seconds(25)).unwrap();

        project.ripple_delete_clips(&[first, third]).unwrap();

        assert_eq!(layout(&project), [(0, 0, 10), (10, 5, 5), (15, 0, 10)]);
    }

    #[test]
    fn rippling_leaves_other_tracks_alone() {
        let (mut project, asset) = project_with_asset();
        let upper = project.timeline.add_track(TrackKind::Video);
        let first = place(&mut project, asset, 0);
        let other = project.place_clip(asset, upper, seconds(15)).unwrap().id;

        project.ripple_delete_clips(&[first]).unwrap();

        assert_eq!(
            project.find_clip(other).map(|(_, clip)| clip.start),
            Some(seconds(15))
        );
    }

    #[test]
    fn a_group_move_may_land_on_the_clips_it_leaves() {
        let (mut project, asset) = project_with_asset();
        let first = place(&mut project, asset, 0);
        let second = place(&mut project, asset, 10);

        let moved = project
            .move_clips(&[(first, 0, seconds(10)), (second, 0, seconds(20))])
            .unwrap();

        assert_eq!(moved.len(), 2);
        assert_eq!(layout(&project), [(10, 0, 10), (20, 0, 10)]);
    }

    #[test]
    fn a_refused_group_move_changes_nothing() {
        let (mut project, asset) = project_with_asset();
        let first = place(&mut project, asset, 0);
        let second = place(&mut project, asset, 10);
        place(&mut project, asset, 30);

        let before = project.clone();

        assert!(matches!(
            project.move_clips(&[(first, 0, seconds(5)), (second, 0, seconds(25))]),
            Err(EditError::Overlapping(_))
        ));
        assert_eq!(project, before);
        assert_eq!(
            project.move_clips(&[(first, 0, seconds(5)), (first, 0, seconds(6))]),
            Err(EditError::RepeatedClip(first))
        );
        assert_eq!(
            project.move_clips(&[(first, 1, seconds(5))]),
            Err(EditError::MissingStream(TrackKind::Audio))
        );
        assert_eq!(project, before);
    }

    #[test]
    fn inserting_splits_the_clip_under_the_start_and_pushes_the_rest_right() {
        let (mut project, asset) = project_with_asset();
        place(&mut project, asset, 0);
        place(&mut project, asset, 10);

        let inserted = project.insert_asset(asset, 0, seconds(4)).unwrap();

        assert_eq!(inserted.start, seconds(4));
        assert_eq!(
            layout(&project),
            [(0, 0, 4), (4, 0, 10), (14, 4, 6), (20, 0, 10)]
        );
    }

    #[test]
    fn inserting_at_a_cut_splits_nothing() {
        let (mut project, asset) = project_with_asset();
        place(&mut project, asset, 0);
        place(&mut project, asset, 10);

        project.insert_asset(asset, 0, seconds(10)).unwrap();

        assert_eq!(layout(&project), [(0, 0, 10), (10, 0, 10), (20, 0, 10)]);
    }

    #[test]
    fn an_insert_that_would_overflow_changes_nothing() {
        let (mut project, asset) = project_with_asset();
        put(&mut project, asset, 0, 0, 1);
        let late = project.next_ids.take_clip();
        project.timeline.tracks[0]
            .insert(Clip {
                id: late,
                asset,
                start: Time::MAX - seconds(5),
                source: TimeRange::new(Time::ZERO, seconds(4)),
            })
            .unwrap();

        let before = project.clone();

        assert_eq!(
            project.insert_asset(asset, 0, seconds(1)),
            Err(EditError::Invalid(InvalidClip::EndOverflows(late)))
        );
        assert_eq!(project, before);
    }

    #[test]
    fn overwriting_trims_the_edges_and_removes_what_it_covers() {
        let (mut project, asset) = project_with_asset();
        place(&mut project, asset, 0);
        place(&mut project, asset, 10);
        place(&mut project, asset, 20);

        project.overwrite_asset(asset, 0, seconds(5)).unwrap();

        assert_eq!(
            layout(&project),
            [(0, 0, 5), (5, 0, 10), (15, 5, 5), (20, 0, 10)]
        );
    }

    #[test]
    fn overwriting_inside_a_clip_splits_it_around_the_new_one() {
        let (mut project, asset) = project_with_asset();
        let original = place(&mut project, asset, 0);
        let copy = Clip {
            id: ClipId(0),
            asset,
            start: Time::ZERO,
            source: TimeRange::new(seconds(2), seconds(3)),
        };

        project.overwrite_clip(&copy, 0, seconds(3)).unwrap();

        assert_eq!(layout(&project), [(0, 0, 3), (3, 2, 3), (6, 6, 4)]);
        assert_eq!(project.timeline.tracks[0].clips()[0].id, original);
        let ids: BTreeSet<_> = project.timeline.tracks[0]
            .clips()
            .iter()
            .map(|clip| clip.id)
            .collect();
        assert_eq!(ids.len(), 3);
    }

    #[test]
    fn overwriting_the_start_of_a_clip_trims_it_and_keeps_its_id() {
        let (mut project, asset) = project_with_asset();
        let covered = place(&mut project, asset, 2);

        project.overwrite_asset(asset, 0, seconds(0)).unwrap();

        assert_eq!(layout(&project), [(0, 0, 10), (10, 8, 2)]);
        assert_eq!(project.timeline.tracks[0].clips()[1].id, covered);
    }

    #[test]
    fn a_ripple_trim_moves_the_later_clips_with_the_edge() {
        let (mut project, asset) = project_with_asset();
        let first = place(&mut project, asset, 0);
        place(&mut project, asset, 10);

        project
            .ripple_trim_clip(first, ClipEdge::End, seconds(6))
            .unwrap();

        assert_eq!(layout(&project), [(0, 0, 6), (6, 0, 10)]);

        project
            .ripple_trim_clip(first, ClipEdge::End, seconds(99))
            .unwrap();

        assert_eq!(layout(&project), [(0, 0, 10), (10, 0, 10)]);

        project
            .ripple_trim_clip(first, ClipEdge::Start, seconds(3))
            .unwrap();

        assert_eq!(layout(&project), [(0, 3, 7), (7, 0, 10)]);

        project
            .ripple_trim_clip(first, ClipEdge::Start, seconds(-5))
            .unwrap();

        assert_eq!(layout(&project), [(0, 0, 10), (10, 0, 10)]);
    }

    #[test]
    fn a_ripple_trim_keeps_one_frame() {
        let (mut project, asset) = project_with_asset();
        let first = place(&mut project, asset, 0);
        let frame = project.settings.frame_rate.frame_duration();

        let trimmed = project
            .ripple_trim_clip(first, ClipEdge::End, seconds(-4))
            .unwrap();

        assert_eq!(trimmed.source.duration, frame);
    }

    #[test]
    fn rolling_moves_the_cut_between_neighbours() {
        let (mut project, asset) = project_with_asset();
        let left = put(&mut project, asset, 0, 0, 6);
        let right = put(&mut project, asset, 6, 2, 8);

        let (rolled_left, rolled_right) = project.roll_clips(left, right, seconds(8)).unwrap();

        assert_eq!(rolled_left.timeline_range().end(), seconds(8));
        assert_eq!(rolled_right.start, seconds(8));
        assert_eq!(layout(&project), [(0, 0, 8), (8, 4, 6)]);

        project.roll_clips(left, right, seconds(99)).unwrap();

        assert_eq!(layout(&project), [(0, 0, 10), (10, 6, 4)]);

        project.roll_clips(left, right, seconds(-99)).unwrap();

        assert_eq!(layout(&project), [(0, 0, 4), (4, 0, 10)]);
    }

    #[test]
    fn rolling_needs_clips_that_touch() {
        let (mut project, asset) = project_with_asset();
        let first = place(&mut project, asset, 0);
        let second = place(&mut project, asset, 12);

        assert_eq!(
            project.roll_clips(first, second, seconds(5)),
            Err(EditError::NotAdjacent {
                left: first,
                right: second
            })
        );
        assert_eq!(
            project.roll_clips(second, first, seconds(5)),
            Err(EditError::NotAdjacent {
                left: second,
                right: first
            })
        );
    }

    #[test]
    fn slipping_moves_the_source_window_within_the_media() {
        let (mut project, asset) = project_with_asset();
        let id = put(&mut project, asset, 0, 0, 4);

        assert_eq!(
            project.slip_clip(id, seconds(3)).unwrap().source,
            TimeRange::new(seconds(3), seconds(4))
        );
        assert_eq!(
            project.slip_clip(id, seconds(99)).unwrap().source.start,
            seconds(6)
        );
        assert_eq!(
            project.slip_clip(id, seconds(-99)).unwrap().source.start,
            Time::ZERO
        );
        assert_eq!(layout(&project), [(0, 0, 4)]);
    }

    #[test]
    fn sliding_keeps_the_clip_and_adjusts_the_touching_neighbours() {
        let (mut project, asset) = project_with_asset();
        let middle = {
            put(&mut project, asset, 0, 0, 8);
            let middle = put(&mut project, asset, 8, 0, 6);
            put(&mut project, asset, 14, 2, 8);
            middle
        };

        let changed = project.slide_clip(middle, seconds(10)).unwrap();

        assert_eq!(changed.len(), 3);
        assert_eq!(layout(&project), [(0, 0, 10), (10, 0, 6), (16, 4, 6)]);

        project.slide_clip(middle, seconds(0)).unwrap();

        assert_eq!(layout(&project), [(0, 0, 6), (6, 0, 6), (12, 0, 10)]);
    }

    #[test]
    fn sliding_stops_at_the_media_and_at_one_frame() {
        let (mut project, asset) = project_with_asset();
        put(&mut project, asset, 0, 0, 5);
        let middle = put(&mut project, asset, 5, 0, 2);
        let frame = project.settings.frame_rate.frame_duration();

        project.slide_clip(middle, seconds(99)).unwrap();

        assert_eq!(layout(&project), [(0, 0, 10), (10, 0, 2)]);

        project.slide_clip(middle, seconds(-99)).unwrap();

        assert_eq!(project.timeline.tracks[0].clips()[0].source.duration, frame);
        assert_eq!(project.timeline.tracks[0].clips()[1].start, frame);
    }

    #[test]
    fn sliding_a_loose_clip_stops_at_its_neighbours() {
        let slid_to = |to: i64| {
            let (mut project, asset) = project_with_asset();
            put(&mut project, asset, 0, 0, 4);
            let loose = put(&mut project, asset, 10, 0, 3);
            put(&mut project, asset, 26, 0, 4);

            project.slide_clip(loose, seconds(to)).unwrap();

            project.find_clip(loose).unwrap().1.start
        };

        assert_eq!(slid_to(0), seconds(4));
        assert_eq!(slid_to(99), seconds(23));
        assert_eq!(slid_to(12), seconds(12));
    }
}
