use std::path::PathBuf;

use thiserror::Error;

use crate::{
    media::MediaInfo,
    time::{FrameRate, Time, TimeRange},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AssetId(pub u64);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Asset {
    pub id: AssetId,
    pub path: PathBuf,
    pub info: MediaInfo,
}

impl Asset {
    pub fn has_stream(&self, kind: TrackKind) -> bool {
        match kind {
            TrackKind::Video => self.info.video().next().is_some(),
            TrackKind::Audio => self.info.audio().next().is_some(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SequenceSettings {
    pub width: u32,
    pub height: u32,
    pub frame_rate: FrameRate,
}

impl Default for SequenceSettings {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            frame_rate: FrameRate::FPS_30,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TrackKind {
    Video,
    Audio,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ClipId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ClipEdge {
    Start,
    End,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Clip {
    pub id: ClipId,
    pub asset: AssetId,
    pub source: TimeRange,
    pub start: Time,
}

impl Clip {
    pub fn timeline_range(&self) -> TimeRange {
        TimeRange::new(self.start, self.source.duration)
    }

    pub fn source_time_at(&self, time: Time) -> Option<Time> {
        self.timeline_range()
            .contains(time)
            .then(|| self.source.start + (time - self.start))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
#[error("clip at {:?} overlaps an existing clip at {:?}", .inserted.start, .existing.start)]
pub struct OverlappingClip {
    pub inserted: TimeRange,
    pub existing: TimeRange,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum PlaceClipError {
    #[error("there is no asset {0:?}")]
    UnknownAsset(AssetId),
    #[error("there is no clip {0:?}")]
    UnknownClip(ClipId),
    #[error("there is no track {0}")]
    UnknownTrack(usize),
    #[error("the asset has no known duration")]
    NoDuration,
    #[error("the asset has no {0:?} stream")]
    MissingStream(TrackKind),
    #[error(transparent)]
    Overlapping(#[from] OverlappingClip),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Track {
    pub kind: TrackKind,
    clips: Vec<Clip>,
}

impl Track {
    pub fn new(kind: TrackKind) -> Self {
        Self {
            kind,
            clips: Vec::new(),
        }
    }

    pub fn clips(&self) -> &[Clip] {
        &self.clips
    }

    pub fn check_free(&self, inserted: TimeRange) -> Result<(), OverlappingClip> {
        self.check_free_of_others(inserted, None)
    }

    fn check_free_of_others(
        &self,
        inserted: TimeRange,
        moving: Option<ClipId>,
    ) -> Result<(), OverlappingClip> {
        match self
            .clips
            .iter()
            .filter(|clip| Some(clip.id) != moving)
            .map(Clip::timeline_range)
            .find(|existing| existing.overlaps(inserted))
        {
            Some(existing) => Err(OverlappingClip { inserted, existing }),
            None => Ok(()),
        }
    }

    pub fn insert(&mut self, clip: Clip) -> Result<(), OverlappingClip> {
        self.check_free(clip.timeline_range())?;
        let index = self.clips.partition_point(|other| other.start < clip.start);
        self.clips.insert(index, clip);
        Ok(())
    }

    pub fn clip(&self, id: ClipId) -> Option<&Clip> {
        self.clips.iter().find(|clip| clip.id == id)
    }

    fn remove(&mut self, id: ClipId) -> Option<Clip> {
        let index = self.clips.iter().position(|clip| clip.id == id)?;
        Some(self.clips.remove(index))
    }

    fn end_before(&self, time: Time) -> Time {
        let index = self.clips.partition_point(|clip| clip.start < time);
        index.checked_sub(1).map_or(Time::ZERO, |previous| {
            self.clips[previous].timeline_range().end()
        })
    }

    fn start_after(&self, time: Time) -> Option<Time> {
        let index = self.clips.partition_point(|clip| clip.start <= time);
        self.clips.get(index).map(|clip| clip.start)
    }

    pub fn clip_at(&self, time: Time) -> Option<&Clip> {
        let index = self.clips.partition_point(|clip| clip.start <= time);
        let candidate = self.clips.get(index.checked_sub(1)?)?;
        candidate
            .timeline_range()
            .contains(time)
            .then_some(candidate)
    }

    pub fn end(&self) -> Time {
        self.clips
            .last()
            .map_or(Time::ZERO, |clip| clip.timeline_range().end())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Timeline {
    pub tracks: Vec<Track>,
}

impl Timeline {
    pub fn duration(&self) -> Time {
        self.tracks
            .iter()
            .map(Track::end)
            .max()
            .unwrap_or(Time::ZERO)
    }

    pub fn top_video_clip_at(&self, time: Time) -> Option<&Clip> {
        self.tracks
            .iter()
            .rev()
            .filter(|track| track.kind == TrackKind::Video)
            .find_map(|track| track.clip_at(time))
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Project {
    pub name: String,
    pub settings: SequenceSettings,
    pub assets: Vec<Asset>,
    pub timeline: Timeline,
}

impl Project {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            settings: SequenceSettings::default(),
            assets: Vec::new(),
            timeline: Timeline {
                tracks: vec![Track::new(TrackKind::Video), Track::new(TrackKind::Audio)],
            },
        }
    }

    pub fn asset(&self, id: AssetId) -> Option<&Asset> {
        self.assets.iter().find(|asset| asset.id == id)
    }

    pub fn add_asset(&mut self, path: PathBuf, info: MediaInfo) -> AssetId {
        let id = AssetId(
            self.assets
                .iter()
                .map(|asset| asset.id.0 + 1)
                .max()
                .unwrap_or(0),
        );
        self.assets.push(Asset { id, path, info });
        id
    }

    pub fn find_clip(&self, id: ClipId) -> Option<(usize, &Clip)> {
        self.timeline
            .tracks
            .iter()
            .enumerate()
            .find_map(|(index, track)| Some((index, track.clip(id)?)))
    }

    fn next_clip_id(&self) -> ClipId {
        ClipId(
            self.timeline
                .tracks
                .iter()
                .flat_map(Track::clips)
                .map(|clip| clip.id.0 + 1)
                .max()
                .unwrap_or(0),
        )
    }

    fn track_accepting(&self, asset: &Asset, track: usize) -> Result<&Track, PlaceClipError> {
        let track_ref = self
            .timeline
            .tracks
            .get(track)
            .ok_or(PlaceClipError::UnknownTrack(track))?;
        if asset.has_stream(track_ref.kind) {
            Ok(track_ref)
        } else {
            Err(PlaceClipError::MissingStream(track_ref.kind))
        }
    }

    fn located_clip(&self, id: ClipId) -> Result<(usize, Clip), PlaceClipError> {
        self.find_clip(id)
            .map(|(track, clip)| (track, *clip))
            .ok_or(PlaceClipError::UnknownClip(id))
    }

    pub fn clip_for(
        &self,
        asset: AssetId,
        track: usize,
        start: Time,
    ) -> Result<Clip, PlaceClipError> {
        let asset = self
            .asset(asset)
            .ok_or(PlaceClipError::UnknownAsset(asset))?;
        let track_ref = self.track_accepting(asset, track)?;
        let duration = asset
            .info
            .duration
            .filter(|duration| *duration > Time::ZERO)
            .ok_or(PlaceClipError::NoDuration)?;
        let clip = Clip {
            id: self.next_clip_id(),
            asset: asset.id,
            source: TimeRange::new(Time::ZERO, duration),
            start: start.max(Time::ZERO),
        };
        track_ref.check_free(clip.timeline_range())?;
        Ok(clip)
    }

    pub fn place_clip(
        &mut self,
        asset: AssetId,
        track: usize,
        start: Time,
    ) -> Result<Clip, PlaceClipError> {
        let clip = self.clip_for(asset, track, start)?;
        self.timeline.tracks[track].insert(clip)?;
        Ok(clip)
    }

    pub fn moved_clip(
        &self,
        id: ClipId,
        track: usize,
        start: Time,
    ) -> Result<Clip, PlaceClipError> {
        let (_, clip) = self.located_clip(id)?;
        let asset = self
            .asset(clip.asset)
            .ok_or(PlaceClipError::UnknownAsset(clip.asset))?;
        let moved = Clip {
            start: start.max(Time::ZERO),
            ..clip
        };
        self.track_accepting(asset, track)?
            .check_free_of_others(moved.timeline_range(), Some(id))?;
        Ok(moved)
    }

    pub fn move_clip(
        &mut self,
        id: ClipId,
        track: usize,
        start: Time,
    ) -> Result<Clip, PlaceClipError> {
        let moved = self.moved_clip(id, track, start)?;
        self.replace_clip(moved, track)
    }

    pub fn trimmed_clip(
        &self,
        id: ClipId,
        edge: ClipEdge,
        to: Time,
    ) -> Result<Clip, PlaceClipError> {
        let (track, clip) = self.located_clip(id)?;
        let track = &self.timeline.tracks[track];
        let range = clip.timeline_range();
        let shortest = self.settings.frame_rate.frame_duration();
        match edge {
            ClipEdge::Start => {
                let earliest = (clip.start - clip.source.start)
                    .max(track.end_before(clip.start))
                    .max(Time::ZERO);
                let latest = (range.end() - shortest).max(clip.start);
                let start = to.clamp(earliest, latest);
                Ok(Clip {
                    start,
                    source: TimeRange::new(
                        clip.source.start + (start - clip.start),
                        range.end() - start,
                    ),
                    ..clip
                })
            }
            ClipEdge::End => {
                let media_end = self
                    .asset(clip.asset)
                    .and_then(|asset| asset.info.duration)
                    .map_or(range.end(), |duration| {
                        clip.start + (duration - clip.source.start)
                    });
                let earliest = (clip.start + shortest).min(range.end());
                let latest = track
                    .start_after(clip.start)
                    .map_or(media_end, |next| next.min(media_end))
                    .max(range.end());
                let end = to.clamp(earliest, latest);
                Ok(Clip {
                    source: TimeRange::new(clip.source.start, end - clip.start),
                    ..clip
                })
            }
        }
    }

    pub fn trim_clip(
        &mut self,
        id: ClipId,
        edge: ClipEdge,
        to: Time,
    ) -> Result<Clip, PlaceClipError> {
        let trimmed = self.trimmed_clip(id, edge, to)?;
        let (track, _) = self.located_clip(id)?;
        self.replace_clip(trimmed, track)
    }

    fn replace_clip(&mut self, clip: Clip, track: usize) -> Result<Clip, PlaceClipError> {
        let (from, _) = self.located_clip(clip.id)?;
        let removed = self.timeline.tracks[from].remove(clip.id);
        if let Err(overlap) = self.timeline.tracks[track].insert(clip) {
            if let Some(removed) = removed {
                self.timeline.tracks[from].insert(removed)?;
            }
            return Err(overlap.into());
        }
        Ok(clip)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::{AudioStream, Stream, VideoStream};

    fn clip(start: i64, duration: i64) -> Clip {
        Clip {
            id: ClipId(start.unsigned_abs()),
            asset: AssetId(0),
            source: TimeRange::new(Time::from_seconds(10), Time::from_seconds(duration)),
            start: Time::from_seconds(start),
        }
    }

    #[test]
    fn clips_stay_sorted_by_start() {
        let mut track = Track::new(TrackKind::Video);
        track.insert(clip(5, 2)).unwrap();
        track.insert(clip(0, 2)).unwrap();
        track.insert(clip(2, 3)).unwrap();
        let starts: Vec<_> = track.clips().iter().map(|clip| clip.start).collect();
        assert_eq!(starts, [0, 2, 5].map(Time::from_seconds).to_vec());
        assert_eq!(track.end(), Time::from_seconds(7));
    }

    #[test]
    fn overlapping_insert_is_refused() {
        let mut track = Track::new(TrackKind::Video);
        track.insert(clip(0, 4)).unwrap();
        let error = track.insert(clip(3, 2)).unwrap_err();
        assert_eq!(error.existing.start, Time::from_seconds(0));
        assert_eq!(track.clips().len(), 1);
    }

    #[test]
    fn clip_at_finds_the_covering_clip() {
        let mut track = Track::new(TrackKind::Audio);
        track.insert(clip(0, 2)).unwrap();
        track.insert(clip(4, 2)).unwrap();
        assert_eq!(
            track.clip_at(Time::from_seconds(1)).map(|c| c.start),
            Some(Time::ZERO)
        );
        assert_eq!(track.clip_at(Time::from_seconds(3)), None);
        assert_eq!(
            track.clip_at(Time::from_seconds(4)).map(|c| c.start),
            Some(Time::from_seconds(4))
        );
        assert_eq!(track.clip_at(Time::from_seconds(6)), None);
    }

    #[test]
    fn source_time_is_offset_by_the_clip_start() {
        let clip = clip(4, 2);
        assert_eq!(
            clip.source_time_at(Time::from_seconds(5)),
            Some(Time::from_seconds(11))
        );
        assert_eq!(clip.source_time_at(Time::from_seconds(6)), None);
    }

    #[test]
    fn asset_ids_are_unique() {
        let mut project = Project::new("test");
        let a = project.add_asset("a.mkv".into(), MediaInfo::default());
        let b = project.add_asset("b.mkv".into(), MediaInfo::default());
        assert_ne!(a, b);
        assert_eq!(
            project.asset(b).map(|asset| asset.path.clone()),
            Some("b.mkv".into())
        );
    }

    fn video_info(seconds: i64) -> MediaInfo {
        MediaInfo {
            duration: Some(Time::from_seconds(seconds)),
            streams: vec![Stream::Video(VideoStream {
                index: 0,
                codec: "h264".into(),
                width: 1280,
                height: 720,
                frame_rate: Some(FrameRate::FPS_30),
            })],
        }
    }

    fn audio_info(seconds: i64) -> MediaInfo {
        MediaInfo {
            duration: Some(Time::from_seconds(seconds)),
            streams: vec![Stream::Audio(AudioStream {
                index: 0,
                codec: "opus".into(),
                sample_rate: 48_000,
                channels: 2,
            })],
        }
    }

    #[test]
    fn placed_clip_covers_the_whole_asset() {
        let mut project = Project::new("test");
        let asset = project.add_asset("a.mkv".into(), video_info(3));
        let clip = project.place_clip(asset, 0, Time::from_seconds(2)).unwrap();
        assert_eq!(
            clip.source,
            TimeRange::new(Time::ZERO, Time::from_seconds(3))
        );
        assert_eq!(clip.start, Time::from_seconds(2));
        assert_eq!(project.timeline.tracks[0].clips(), [clip]);
        assert_eq!(project.timeline.duration(), Time::from_seconds(5));
    }

    #[test]
    fn placement_needs_a_matching_stream() {
        let mut project = Project::new("test");
        let video = project.add_asset("a.mkv".into(), video_info(3));
        let audio = project.add_asset("b.opus".into(), audio_info(3));
        assert_eq!(
            project.place_clip(video, 1, Time::ZERO),
            Err(PlaceClipError::MissingStream(TrackKind::Audio))
        );
        assert_eq!(
            project.place_clip(audio, 0, Time::ZERO),
            Err(PlaceClipError::MissingStream(TrackKind::Video))
        );
        assert!(project.place_clip(audio, 1, Time::ZERO).is_ok());
    }

    #[test]
    fn placement_refuses_unknown_targets_and_durations() {
        let mut project = Project::new("test");
        let asset = project.add_asset("a.mkv".into(), video_info(3));
        let untimed = project.add_asset(
            "b.mkv".into(),
            MediaInfo {
                duration: None,
                ..video_info(0)
            },
        );
        assert_eq!(
            project.place_clip(AssetId(9), 0, Time::ZERO),
            Err(PlaceClipError::UnknownAsset(AssetId(9)))
        );
        assert_eq!(
            project.place_clip(asset, 7, Time::ZERO),
            Err(PlaceClipError::UnknownTrack(7))
        );
        assert_eq!(
            project.place_clip(untimed, 0, Time::ZERO),
            Err(PlaceClipError::NoDuration)
        );
    }

    #[test]
    fn placement_keeps_clips_apart() {
        let mut project = Project::new("test");
        let asset = project.add_asset("a.mkv".into(), video_info(3));
        project.place_clip(asset, 0, Time::ZERO).unwrap();
        let before = project.timeline.clone();
        assert!(matches!(
            project.clip_for(asset, 0, Time::from_seconds(2)),
            Err(PlaceClipError::Overlapping(_))
        ));
        assert!(matches!(
            project.place_clip(asset, 0, Time::from_seconds(2)),
            Err(PlaceClipError::Overlapping(_))
        ));
        assert_eq!(project.timeline, before);
        assert!(project.place_clip(asset, 0, Time::from_seconds(3)).is_ok());
    }

    #[test]
    fn placement_clamps_to_the_timeline_start() {
        let mut project = Project::new("test");
        let asset = project.add_asset("a.mkv".into(), video_info(1));
        let clip = project
            .place_clip(asset, 0, Time::from_seconds(-2))
            .unwrap();
        assert_eq!(clip.start, Time::ZERO);
    }

    #[test]
    fn later_video_tracks_sit_on_top() {
        let mut timeline = Timeline {
            tracks: [TrackKind::Video, TrackKind::Video, TrackKind::Audio]
                .map(Track::new)
                .to_vec(),
        };
        let lower = Clip {
            asset: AssetId(1),
            ..clip(0, 4)
        };
        let upper = Clip {
            asset: AssetId(2),
            ..clip(2, 1)
        };
        let audio = Clip {
            asset: AssetId(3),
            ..clip(0, 8)
        };
        timeline.tracks[0].insert(lower).unwrap();
        timeline.tracks[1].insert(upper).unwrap();
        timeline.tracks[2].insert(audio).unwrap();
        let top = |seconds| {
            timeline
                .top_video_clip_at(Time::from_seconds(seconds))
                .map(|clip| clip.asset)
        };
        assert_eq!(top(1), Some(AssetId(1)));
        assert_eq!(top(2), Some(AssetId(2)));
        assert_eq!(top(3), Some(AssetId(1)));
        assert_eq!(top(5), None);
    }

    fn two_clip_project() -> (Project, AssetId, ClipId, ClipId) {
        let mut project = Project::new("test");
        let asset = project.add_asset("a.mkv".into(), video_info(10));
        let first = project.place_clip(asset, 0, Time::ZERO).unwrap().id;
        let second = project
            .place_clip(asset, 0, Time::from_seconds(20))
            .unwrap()
            .id;
        (project, asset, first, second)
    }

    #[test]
    fn placed_clips_get_distinct_ids() {
        let (project, _, first, second) = two_clip_project();
        assert_ne!(first, second);
        assert_eq!(
            project
                .find_clip(second)
                .map(|(track, clip)| (track, clip.start)),
            Some((0, Time::from_seconds(20)))
        );
        assert_eq!(project.find_clip(ClipId(99)), None);
    }

    #[test]
    fn moving_keeps_the_source_and_refuses_overlaps() {
        let (mut project, _, first, second) = two_clip_project();
        let moved = project.move_clip(first, 0, Time::from_seconds(5)).unwrap();
        assert_eq!(moved.start, Time::from_seconds(5));
        assert_eq!(
            moved.source,
            TimeRange::new(Time::ZERO, Time::from_seconds(10))
        );
        let before = project.timeline.clone();
        assert!(matches!(
            project.move_clip(first, 0, Time::from_seconds(15)),
            Err(PlaceClipError::Overlapping(_))
        ));
        assert_eq!(project.timeline, before);
        project.move_clip(second, 0, Time::ZERO).unwrap_err();
        project.move_clip(first, 0, Time::from_seconds(30)).unwrap();
        project.move_clip(second, 0, Time::ZERO).unwrap();
        let order: Vec<_> = project.timeline.tracks[0]
            .clips()
            .iter()
            .map(|clip| clip.id)
            .collect();
        assert_eq!(order, [second, first]);
    }

    #[test]
    fn moving_across_tracks_needs_a_matching_stream() {
        let (mut project, _, first, _) = two_clip_project();
        project.timeline.tracks.push(Track::new(TrackKind::Video));
        assert_eq!(
            project.move_clip(first, 1, Time::ZERO),
            Err(PlaceClipError::MissingStream(TrackKind::Audio))
        );
        let moved = project.move_clip(first, 2, Time::from_seconds(-3)).unwrap();
        assert_eq!(moved.start, Time::ZERO);
        assert_eq!(project.find_clip(first).map(|(track, _)| track), Some(2));
        assert_eq!(project.timeline.tracks[0].clips().len(), 1);
        assert_eq!(
            project.move_clip(ClipId(99), 0, Time::ZERO),
            Err(PlaceClipError::UnknownClip(ClipId(99)))
        );
    }

    #[test]
    fn trimming_the_start_moves_the_source_in_step() {
        let (mut project, _, _, second) = two_clip_project();
        let trimmed = project
            .trim_clip(second, ClipEdge::Start, Time::from_seconds(23))
            .unwrap();
        assert_eq!(trimmed.start, Time::from_seconds(23));
        assert_eq!(
            trimmed.source,
            TimeRange::new(Time::from_seconds(3), Time::from_seconds(7))
        );
        assert_eq!(trimmed.timeline_range().end(), Time::from_seconds(30));
        let extended = project
            .trim_clip(second, ClipEdge::Start, Time::from_seconds(5))
            .unwrap();
        assert_eq!(extended.start, Time::from_seconds(20));
        assert_eq!(extended.source.start, Time::ZERO);
    }

    #[test]
    fn trimming_stops_at_the_neighbours() {
        let (mut project, _, first, second) = two_clip_project();
        project
            .trim_clip(first, ClipEdge::End, Time::from_seconds(5))
            .unwrap();
        project
            .trim_clip(second, ClipEdge::Start, Time::from_seconds(25))
            .unwrap();
        project.move_clip(second, 0, Time::from_seconds(8)).unwrap();
        let first_end = project
            .trim_clip(first, ClipEdge::End, Time::from_seconds(40))
            .unwrap();
        assert_eq!(first_end.timeline_range().end(), Time::from_seconds(8));
        let second_start = project
            .trim_clip(second, ClipEdge::Start, Time::ZERO)
            .unwrap();
        assert_eq!(second_start.start, Time::from_seconds(8));
        assert_eq!(second_start.source.start, Time::from_seconds(5));
        project
            .trim_clip(first, ClipEdge::End, Time::from_seconds(5))
            .unwrap();
        let second_start = project
            .trim_clip(second, ClipEdge::Start, Time::ZERO)
            .unwrap();
        assert_eq!(second_start.start, Time::from_seconds(5));
        assert_eq!(second_start.source.start, Time::from_seconds(2));
    }

    #[test]
    fn trimming_keeps_at_least_one_frame_within_the_media() {
        let (mut project, _, first, _) = two_clip_project();
        let frame = project.settings.frame_rate.frame_duration();
        let shortest = project
            .trim_clip(first, ClipEdge::End, Time::from_seconds(-4))
            .unwrap();
        assert_eq!(shortest.source.duration, frame);
        let longest = project
            .trim_clip(first, ClipEdge::End, Time::from_seconds(19))
            .unwrap();
        assert_eq!(longest.timeline_range().end(), Time::from_seconds(10));
        let latest = project
            .trim_clip(first, ClipEdge::Start, Time::from_seconds(19))
            .unwrap();
        assert_eq!(latest.start, Time::from_seconds(10) - frame);
        assert_eq!(latest.source.duration, frame);
    }
}
