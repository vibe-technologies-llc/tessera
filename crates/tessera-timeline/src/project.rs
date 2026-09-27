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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Clip {
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
        match self
            .clips
            .iter()
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

    pub fn clip_for(
        &self,
        asset: AssetId,
        track: usize,
        start: Time,
    ) -> Result<Clip, PlaceClipError> {
        let asset = self
            .asset(asset)
            .ok_or(PlaceClipError::UnknownAsset(asset))?;
        let track_ref = self
            .timeline
            .tracks
            .get(track)
            .ok_or(PlaceClipError::UnknownTrack(track))?;
        if !asset.has_stream(track_ref.kind) {
            return Err(PlaceClipError::MissingStream(track_ref.kind));
        }
        let duration = asset
            .info
            .duration
            .filter(|duration| *duration > Time::ZERO)
            .ok_or(PlaceClipError::NoDuration)?;
        let clip = Clip {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::{AudioStream, Stream, VideoStream};

    fn clip(start: i64, duration: i64) -> Clip {
        Clip {
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
}
