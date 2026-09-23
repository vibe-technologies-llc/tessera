use std::path::PathBuf;

use thiserror::Error;

use crate::time::{FrameRate, Time, TimeRange};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AssetId(pub u64);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Asset {
    pub id: AssetId,
    pub path: PathBuf,
    pub duration: Time,
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

    pub fn insert(&mut self, clip: Clip) -> Result<(), OverlappingClip> {
        let inserted = clip.timeline_range();
        if let Some(existing) = self
            .clips
            .iter()
            .map(Clip::timeline_range)
            .find(|existing| existing.overlaps(inserted))
        {
            return Err(OverlappingClip { inserted, existing });
        }
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

    pub fn add_asset(&mut self, path: PathBuf, duration: Time) -> AssetId {
        let id = AssetId(
            self.assets
                .iter()
                .map(|asset| asset.id.0 + 1)
                .max()
                .unwrap_or(0),
        );
        self.assets.push(Asset { id, path, duration });
        id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let a = project.add_asset("a.mkv".into(), Time::from_seconds(1));
        let b = project.add_asset("b.mkv".into(), Time::from_seconds(1));
        assert_ne!(a, b);
        assert_eq!(
            project.asset(b).map(|asset| asset.path.clone()),
            Some("b.mkv".into())
        );
    }
}
