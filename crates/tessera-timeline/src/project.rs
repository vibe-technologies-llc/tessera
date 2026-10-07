use std::{num::NonZeroU32, path::PathBuf, sync::Arc};

use thiserror::Error;

mod edits;
mod links;
mod markers;

use crate::{
    gain::Gain,
    media::{AudioStream, MediaInfo},
    picture::{InvalidTransform, Opacity, Transform},
    time::{FrameRate, Time, TimeRange},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AssetId(pub u64);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Asset {
    pub id: AssetId,
    pub path: PathBuf,
    pub info: MediaInfo,
}

impl Asset {
    pub const STILL_DURATION: Time = Time::from_seconds(5);

    pub fn is_still(&self) -> bool {
        self.info.duration.is_none() && self.info.video().next().is_some()
    }

    pub fn default_clip_duration(&self) -> Option<Time> {
        self.info
            .duration
            .filter(|duration| *duration > Time::ZERO)
            .or_else(|| self.is_still().then_some(Self::STILL_DURATION))
    }

    pub fn audio_stream_for(&self, clip: &Clip) -> Option<&AudioStream> {
        clip.audio_stream
            .and_then(|index| self.info.audio_stream(index))
            .or_else(|| self.info.default_audio())
    }

    pub fn next_audio_stream(&self, clip: &Clip) -> Option<usize> {
        let playing = self.audio_stream_for(clip)?.index;
        let streams: Vec<usize> = self.info.audio().map(|audio| audio.index).collect();
        let position = streams.iter().position(|&index| index == playing)?;
        let next = streams[(position + 1) % streams.len()];
        (next != playing).then_some(next)
    }

    pub fn has_stream(&self, kind: TrackKind) -> bool {
        match kind {
            TrackKind::Video => self.info.video().next().is_some(),
            TrackKind::Audio => self.info.audio().next().is_some(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SequenceSettings {
    pub width: NonZeroU32,
    pub height: NonZeroU32,
    pub frame_rate: FrameRate,
    pub sample_rate: NonZeroU32,
}

impl SequenceSettings {
    pub const DEFAULT_WIDTH: NonZeroU32 =
        NonZeroU32::new(1920).expect("the default width is not zero");
    pub const DEFAULT_HEIGHT: NonZeroU32 =
        NonZeroU32::new(1080).expect("the default height is not zero");
    pub const DEFAULT_SAMPLE_RATE: NonZeroU32 =
        NonZeroU32::new(48_000).expect("the default sample rate is not zero");
}

impl Default for SequenceSettings {
    fn default() -> Self {
        Self {
            width: Self::DEFAULT_WIDTH,
            height: Self::DEFAULT_HEIGHT,
            frame_rate: FrameRate::FPS_30,
            sample_rate: Self::DEFAULT_SAMPLE_RATE,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TrackKind {
    Video,
    Audio,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ClipId(pub u64);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MarkerId(pub u64);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LinkId(pub u64);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NextIds {
    pub asset: AssetId,
    pub clip: ClipId,
    pub marker: MarkerId,
    pub link: LinkId,
}

impl NextIds {
    pub fn has_issued_asset(&self, id: AssetId) -> bool {
        id < self.asset
    }

    pub fn has_issued_clip(&self, id: ClipId) -> bool {
        id < self.clip
    }

    pub fn has_issued_marker(&self, id: MarkerId) -> bool {
        id < self.marker
    }

    pub fn has_issued_link(&self, id: LinkId) -> bool {
        id < self.link
    }

    pub fn covering(self, other: Self) -> Self {
        Self {
            asset: self.asset.max(other.asset),
            clip: self.clip.max(other.clip),
            marker: self.marker.max(other.marker),
            link: self.link.max(other.link),
        }
    }

    fn take_asset(&mut self) -> AssetId {
        let id = self.asset;
        self.asset = AssetId(id.0 + 1);
        id
    }

    fn take_clip(&mut self) -> ClipId {
        let id = self.clip;
        self.clip = ClipId(id.0 + 1);
        id
    }

    fn take_marker(&mut self) -> MarkerId {
        let id = self.marker;
        self.marker = MarkerId(id.0 + 1);
        id
    }

    fn take_link(&mut self) -> LinkId {
        let id = self.link;
        self.link = LinkId(id.0 + 1);
        id
    }
}

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
    pub link: Option<LinkId>,
    pub gain: Gain,
    pub audio_stream: Option<usize>,
    pub transform: Transform,
    pub opacity: Opacity,
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

    pub fn is_cut_by(&self, time: Time) -> bool {
        self.start < time && time < self.timeline_range().end()
    }

    pub fn check(&self) -> Result<(), InvalidClip> {
        if self.source.duration <= Time::ZERO {
            Err(InvalidClip::Empty(self.id))
        } else if self.start < Time::ZERO || self.source.start < Time::ZERO {
            Err(InvalidClip::NegativeTime(self.id))
        } else if self.timeline_range().checked_end().is_none()
            || self.source.checked_end().is_none()
        {
            Err(InvalidClip::EndOverflows(self.id))
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum InvalidClip {
    #[error("clip {} has no length", .0.0)]
    Empty(ClipId),
    #[error("clip {} starts before zero on the timeline or in its media", .0.0)]
    NegativeTime(ClipId),
    #[error("clip {} ends past the latest time Tessera can represent", .0.0)]
    EndOverflows(ClipId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
#[error("clip at {:?} overlaps an existing clip at {:?}", .inserted.start, .existing.start)]
pub struct OverlappingClip {
    pub inserted: TimeRange,
    pub existing: TimeRange,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum InsertError {
    #[error(transparent)]
    Invalid(#[from] InvalidClip),
    #[error(transparent)]
    Overlapping(#[from] OverlappingClip),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum EditError {
    #[error("there is no asset {0:?}")]
    UnknownAsset(AssetId),
    #[error("there is no clip {0:?}")]
    UnknownClip(ClipId),
    #[error("asset {0:?} is still used by a clip")]
    AssetInUse(AssetId),
    #[error("clip {0:?} reaches past the end of the new media")]
    BeyondMedia(ClipId),
    #[error("there is no track {0}")]
    UnknownTrack(usize),
    #[error("{time:?} does not fall inside clip {clip:?}")]
    OutsideClip { clip: ClipId, time: Time },
    #[error("track {0} still holds clips")]
    TrackNotEmpty(usize),
    #[error("the timeline needs at least one {0:?} track")]
    LastTrack(TrackKind),
    #[error("cannot swap a {first:?} track with a {second:?} track")]
    MixedTrackKinds { first: TrackKind, second: TrackKind },
    #[error("there is no marker {0:?}")]
    UnknownMarker(MarkerId),
    #[error("the in point must come before the out point")]
    InvalidInOut,
    #[error("track {0} is locked")]
    TrackLocked(usize),
    #[error("clip {0:?} is named more than once")]
    RepeatedClip(ClipId),
    #[error("clips {left:?} and {right:?} do not meet on one track")]
    NotAdjacent { left: ClipId, right: ClipId },
    #[error("the asset has no known duration")]
    NoDuration,
    #[error("none of the clips play the same media at the same time on another track")]
    NothingToLink,
    #[error("asset {asset:?} has no audio stream {stream}")]
    UnknownAudioStream { asset: AssetId, stream: usize },
    #[error("the asset has no {0:?} stream")]
    MissingStream(TrackKind),
    #[error(transparent)]
    Invalid(#[from] InvalidClip),
    #[error(transparent)]
    Overlapping(#[from] OverlappingClip),
    #[error(transparent)]
    InvalidTransform(#[from] InvalidTransform),
}

impl From<InsertError> for EditError {
    fn from(error: InsertError) -> Self {
        match error {
            InsertError::Invalid(invalid) => Self::Invalid(invalid),
            InsertError::Overlapping(overlapping) => Self::Overlapping(overlapping),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum TrackHeight {
    Compact,
    #[default]
    Normal,
    Tall,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Track {
    pub kind: TrackKind,
    pub name: String,
    pub locked: bool,
    pub muted: bool,
    pub solo: bool,
    pub height: TrackHeight,
    pub volume: Gain,
    clips: Arc<Vec<Clip>>,
}

impl Track {
    pub fn new(kind: TrackKind) -> Self {
        Self {
            kind,
            name: String::new(),
            locked: false,
            muted: false,
            solo: false,
            height: TrackHeight::default(),
            volume: Gain::UNITY,
            clips: Arc::default(),
        }
    }

    pub fn clips(&self) -> &[Clip] {
        &self.clips
    }

    fn clips_mut(&mut self) -> &mut Vec<Clip> {
        Arc::make_mut(&mut self.clips)
    }

    pub fn check_insert(&self, clip: &Clip) -> Result<(), InsertError> {
        self.check_insert_moving(clip, None)
    }

    fn check_insert_moving(&self, clip: &Clip, moving: Option<ClipId>) -> Result<(), InsertError> {
        clip.check()?;
        let inserted = clip.timeline_range();
        match self
            .clips_overlapping(inserted)
            .iter()
            .find(|clip| Some(clip.id) != moving)
            .map(Clip::timeline_range)
        {
            Some(existing) => Err(OverlappingClip { inserted, existing }.into()),
            None => Ok(()),
        }
    }

    pub fn insert(&mut self, clip: Clip) -> Result<(), InsertError> {
        self.check_insert(&clip)?;
        let index = self.clips.partition_point(|other| other.start < clip.start);
        self.clips_mut().insert(index, clip);
        Ok(())
    }

    pub fn clips_overlapping(&self, range: TimeRange) -> &[Clip] {
        let first = self
            .clips
            .partition_point(|clip| clip.timeline_range().end() <= range.start);
        let past_last = self.clips.partition_point(|clip| clip.start < range.end());
        &self.clips[first..past_last.max(first)]
    }

    pub fn clip(&self, id: ClipId) -> Option<&Clip> {
        self.clips.iter().find(|clip| clip.id == id)
    }

    fn remove(&mut self, id: ClipId) -> Option<Clip> {
        let index = self.clips.iter().position(|clip| clip.id == id)?;
        Some(self.clips_mut().remove(index))
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
    pub fn add_track(&mut self, kind: TrackKind) -> usize {
        let index = self
            .tracks
            .iter()
            .rposition(|track| track.kind == kind)
            .map_or(
                match kind {
                    TrackKind::Video => 0,
                    TrackKind::Audio => self.tracks.len(),
                },
                |last| last + 1,
            );
        self.tracks.insert(index, Track::new(kind));
        index
    }

    pub fn check_removable(&self, index: usize) -> Result<&Track, EditError> {
        let track = self
            .tracks
            .get(index)
            .ok_or(EditError::UnknownTrack(index))?;
        let same_kind = self
            .tracks
            .iter()
            .filter(|other| other.kind == track.kind)
            .count();
        if !track.clips.is_empty() {
            Err(EditError::TrackNotEmpty(index))
        } else if same_kind == 1 {
            Err(EditError::LastTrack(track.kind))
        } else {
            Ok(track)
        }
    }

    pub fn remove_track(&mut self, index: usize) -> Result<Track, EditError> {
        self.check_removable(index)?;
        Ok(self.tracks.remove(index))
    }

    pub fn swap_tracks(&mut self, first: usize, second: usize) -> Result<(), EditError> {
        let kind_of = |index: usize| {
            self.tracks
                .get(index)
                .map(|track| track.kind)
                .ok_or(EditError::UnknownTrack(index))
        };
        let (first_kind, second_kind) = (kind_of(first)?, kind_of(second)?);
        if first_kind != second_kind {
            return Err(EditError::MixedTrackKinds {
                first: first_kind,
                second: second_kind,
            });
        }
        self.tracks.swap(first, second);
        Ok(())
    }

    pub fn duration(&self) -> Time {
        self.tracks
            .iter()
            .map(Track::end)
            .max()
            .unwrap_or(Time::ZERO)
    }

    pub fn video_layers_at(&self, time: Time) -> impl DoubleEndedIterator<Item = (usize, &Clip)> {
        self.tracks
            .iter()
            .enumerate()
            .filter(|(_, track)| track.kind == TrackKind::Video && !track.muted)
            .filter_map(move |(index, track)| Some((index, track.clip_at(time)?)))
    }

    pub fn video_clips_at(&self, time: Time) -> impl DoubleEndedIterator<Item = &Clip> {
        self.video_layers_at(time).map(|(_, clip)| clip)
    }

    pub fn top_video_clip_at(&self, time: Time) -> Option<&Clip> {
        self.video_clips_at(time).next_back()
    }

    pub fn next_edit_after(&self, time: Time) -> Option<Time> {
        self.edit_points().filter(|point| *point > time).min()
    }

    pub fn previous_edit_before(&self, time: Time) -> Option<Time> {
        self.edit_points().filter(|point| *point < time).max()
    }

    fn edit_points(&self) -> impl Iterator<Item = Time> {
        self.tracks
            .iter()
            .flat_map(|track| track.clips())
            .flat_map(|clip| [clip.start, clip.timeline_range().end()])
    }

    pub fn clips_cut_by(&self, time: Time) -> impl Iterator<Item = &Clip> {
        self.tracks
            .iter()
            .filter_map(move |track| track.clip_at(time))
            .filter(move |clip| clip.is_cut_by(time))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Marker {
    pub id: MarkerId,
    pub time: Time,
    pub name: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Project {
    pub name: String,
    pub settings: SequenceSettings,
    pub assets: Arc<Vec<Asset>>,
    pub timeline: Timeline,
    pub markers: Vec<Marker>,
    pub in_point: Option<Time>,
    pub out_point: Option<Time>,
    pub next_ids: NextIds,
}

impl Project {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            settings: SequenceSettings::default(),
            assets: Arc::default(),
            timeline: Timeline {
                tracks: vec![Track::new(TrackKind::Video), Track::new(TrackKind::Audio)],
            },
            markers: Vec::new(),
            in_point: None,
            out_point: None,
            next_ids: NextIds::default(),
        }
    }

    pub fn set_settings(&mut self, settings: SequenceSettings) -> SequenceSettings {
        std::mem::replace(&mut self.settings, settings)
    }

    pub fn asset(&self, id: AssetId) -> Option<&Asset> {
        self.assets.iter().find(|asset| asset.id == id)
    }

    pub fn add_asset(&mut self, path: PathBuf, info: MediaInfo) -> AssetId {
        let id = self.next_ids.take_asset();
        Arc::make_mut(&mut self.assets).push(Asset { id, path, info });
        id
    }

    pub fn remove_asset(&mut self, id: AssetId) -> Result<Asset, EditError> {
        let index = self
            .assets
            .iter()
            .position(|asset| asset.id == id)
            .ok_or(EditError::UnknownAsset(id))?;
        if self.is_asset_used(id) {
            return Err(EditError::AssetInUse(id));
        }
        Ok(Arc::make_mut(&mut self.assets).remove(index))
    }

    pub fn relink_asset(
        &mut self,
        id: AssetId,
        path: PathBuf,
        info: MediaInfo,
    ) -> Result<(), EditError> {
        let relinked = Asset { id, path, info };
        for track in &self.timeline.tracks {
            for clip in track.clips().iter().filter(|clip| clip.asset == id) {
                if !relinked.has_stream(track.kind) {
                    return Err(EditError::MissingStream(track.kind));
                }
                if relinked
                    .info
                    .duration
                    .is_some_and(|duration| clip.source.end() > duration)
                {
                    return Err(EditError::BeyondMedia(clip.id));
                }
                if let Some(stream) = clip
                    .audio_stream
                    .filter(|&stream| relinked.info.audio_stream(stream).is_none())
                {
                    return Err(EditError::UnknownAudioStream { asset: id, stream });
                }
            }
        }
        let slot = Arc::make_mut(&mut self.assets)
            .iter_mut()
            .find(|asset| asset.id == id)
            .ok_or(EditError::UnknownAsset(id))?;
        *slot = relinked;
        Ok(())
    }

    pub fn prune_assets(&mut self) -> Vec<Asset> {
        let (used, unused) = Arc::unwrap_or_clone(std::mem::take(&mut self.assets))
            .into_iter()
            .partition(|asset| self.is_asset_used(asset.id));
        self.assets = Arc::new(used);
        unused
    }

    pub fn is_asset_used(&self, id: AssetId) -> bool {
        self.timeline
            .tracks
            .iter()
            .flat_map(|track| track.clips())
            .any(|clip| clip.asset == id)
    }

    pub fn find_clip(&self, id: ClipId) -> Option<(usize, &Clip)> {
        self.timeline
            .tracks
            .iter()
            .enumerate()
            .find_map(|(index, track)| Some((index, track.clip(id)?)))
    }

    fn track_accepting(&self, asset: &Asset, track: usize) -> Result<&Track, EditError> {
        let track_ref = self
            .timeline
            .tracks
            .get(track)
            .ok_or(EditError::UnknownTrack(track))?;
        if track_ref.locked {
            Err(EditError::TrackLocked(track))
        } else if asset.has_stream(track_ref.kind) {
            Ok(track_ref)
        } else {
            Err(EditError::MissingStream(track_ref.kind))
        }
    }

    fn located_clip(&self, id: ClipId) -> Result<(usize, Clip), EditError> {
        let (track, clip) = self.find_clip(id).ok_or(EditError::UnknownClip(id))?;
        if self.timeline.tracks[track].locked {
            Err(EditError::TrackLocked(track))
        } else {
            Ok((track, *clip))
        }
    }

    fn whole_clip(&self, asset: AssetId, track: usize, start: Time) -> Result<Clip, EditError> {
        let asset = self.asset(asset).ok_or(EditError::UnknownAsset(asset))?;
        self.track_accepting(asset, track)?;
        let duration = asset.default_clip_duration().ok_or(EditError::NoDuration)?;
        Ok(Clip {
            id: self.next_ids.clip,
            asset: asset.id,
            source: TimeRange::new(Time::ZERO, duration),
            start: start.max(Time::ZERO),
            link: None,
            gain: Gain::UNITY,
            audio_stream: None,
            transform: crate::Transform::IDENTITY,
            opacity: crate::Opacity::OPAQUE,
        })
    }

    pub fn clip_for(&self, asset: AssetId, track: usize, start: Time) -> Result<Clip, EditError> {
        let clip = self.whole_clip(asset, track, start)?;
        self.timeline.tracks[track].check_insert(&clip)?;
        Ok(clip)
    }

    pub fn place_clip(
        &mut self,
        asset: AssetId,
        track: usize,
        start: Time,
    ) -> Result<Clip, EditError> {
        let previewed = self.clip_for(asset, track, start)?;
        let clip = Clip {
            id: self.next_ids.take_clip(),
            ..previewed
        };
        self.timeline.tracks[track].insert(clip)?;
        Ok(clip)
    }

    fn copied_clip(&self, clip: &Clip, track: usize, start: Time) -> Result<Clip, EditError> {
        let asset = self
            .asset(clip.asset)
            .ok_or(EditError::UnknownAsset(clip.asset))?;
        self.track_accepting(asset, track)?;
        Ok(Clip {
            id: self.next_ids.clip,
            start: start.max(Time::ZERO),
            link: None,
            ..*clip
        })
    }

    pub fn pasted_clip(&self, clip: &Clip, track: usize, start: Time) -> Result<Clip, EditError> {
        let pasted = self.copied_clip(clip, track, start)?;
        self.timeline.tracks[track].check_insert(&pasted)?;
        Ok(pasted)
    }

    pub fn paste_clip(
        &mut self,
        clip: &Clip,
        track: usize,
        start: Time,
    ) -> Result<Clip, EditError> {
        let previewed = self.pasted_clip(clip, track, start)?;
        let pasted = Clip {
            id: self.next_ids.take_clip(),
            ..previewed
        };
        self.timeline.tracks[track].insert(pasted)?;
        Ok(pasted)
    }

    pub fn moved_clip(&self, id: ClipId, track: usize, start: Time) -> Result<Clip, EditError> {
        self.clone().move_clip(id, track, start)
    }

    pub fn move_clip(&mut self, id: ClipId, track: usize, start: Time) -> Result<Clip, EditError> {
        let moved = self.move_clips(&[(id, track, start)])?;
        moved
            .into_iter()
            .find(|clip| clip.id == id)
            .ok_or(EditError::UnknownClip(id))
    }

    pub fn trimmed_clip(&self, id: ClipId, edge: ClipEdge, to: Time) -> Result<Clip, EditError> {
        let (_, clip) = self.located_clip(id)?;
        let edge_of = |clip: &Clip| match edge {
            ClipEdge::Start => clip.start,
            ClipEdge::End => clip.timeline_range().end(),
        };
        let current = edge_of(&clip);
        let mut reached = edge_of(&self.trimmed_alone(id, edge, to)?);
        for (_, partner) in self.partners(id) {
            let allowed = edge_of(&self.trimmed_alone(partner.id, edge, to)?);
            if (allowed - current).flicks().abs() < (reached - current).flicks().abs() {
                reached = allowed;
            }
        }
        self.trimmed_alone(id, edge, reached)
    }

    fn trimmed_alone(&self, id: ClipId, edge: ClipEdge, to: Time) -> Result<Clip, EditError> {
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
                let media_end = match self.asset(clip.asset) {
                    Some(asset) if asset.is_still() => Time::MAX,
                    Some(asset) => asset.info.duration.map_or(range.end(), |duration| {
                        clip.start + (duration - clip.source.start)
                    }),
                    None => range.end(),
                };
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

    pub fn trim_clip(&mut self, id: ClipId, edge: ClipEdge, to: Time) -> Result<Clip, EditError> {
        let trimmed = self.trimmed_clip(id, edge, to)?;
        let reached = match edge {
            ClipEdge::Start => trimmed.start,
            ClipEdge::End => trimmed.timeline_range().end(),
        };
        let partners = self.partners(id);
        self.atomically(|project| {
            let (track, _) = project.located_clip(id)?;
            project.replace_clip(trimmed, track)?;
            for (track, partner) in partners {
                let trimmed = project.trimmed_alone(partner.id, edge, reached)?;
                project.replace_clip(trimmed, track)?;
            }
            Ok(trimmed)
        })
    }

    pub fn split_clip(&mut self, id: ClipId, at: Time) -> Result<(Clip, Clip), EditError> {
        let split = self.split_clips(&[id], at)?;
        split
            .into_iter()
            .find(|(head, _)| head.id == id)
            .ok_or(EditError::UnknownClip(id))
    }

    fn split_alone(
        &mut self,
        id: ClipId,
        at: Time,
        tail_link: Option<LinkId>,
    ) -> Result<(Clip, Clip), EditError> {
        let (track, clip) = self.located_clip(id)?;
        if !clip.is_cut_by(at) {
            return Err(EditError::OutsideClip { clip: id, time: at });
        }
        let head_duration = at - clip.start;
        let head = Clip {
            source: TimeRange::new(clip.source.start, head_duration),
            ..clip
        };
        let tail = Clip {
            id: self.next_ids.take_clip(),
            start: at,
            source: TimeRange::new(
                clip.source.start + head_duration,
                clip.source.duration - head_duration,
            ),
            link: tail_link,
            ..clip
        };
        let track = &mut self.timeline.tracks[track];
        track.remove(id);
        track.insert(head)?;
        track.insert(tail)?;
        Ok((head, tail))
    }

    pub fn delete_clip(&mut self, id: ClipId) -> Result<Clip, EditError> {
        let deleted = self.delete_clips(&[id])?;
        deleted
            .into_iter()
            .find(|clip| clip.id == id)
            .ok_or(EditError::UnknownClip(id))
    }

    pub fn ripple_delete_clip(&mut self, id: ClipId) -> Result<Clip, EditError> {
        let deleted = self.ripple_delete_clips(&[id])?;
        deleted
            .into_iter()
            .find(|clip| clip.id == id)
            .ok_or(EditError::UnknownClip(id))
    }

    fn replace_clip(&mut self, clip: Clip, track: usize) -> Result<Clip, EditError> {
        let (from, _) = self.located_clip(clip.id)?;
        let removed = self.timeline.tracks[from].remove(clip.id);
        if let Err(refused) = self.timeline.tracks[track].insert(clip) {
            if let Some(removed) = removed {
                self.timeline.tracks[from].insert(removed)?;
            }
            return Err(refused.into());
        }
        Ok(clip)
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZero;

    use super::*;
    use crate::media::{AudioStream, Stream, VideoStream};

    fn clip(start: i64, duration: i64) -> Clip {
        Clip {
            id: ClipId(start.unsigned_abs()),
            asset: AssetId(0),
            source: TimeRange::new(Time::from_seconds(10), Time::from_seconds(duration)),
            start: Time::from_seconds(start),
            link: None,
            gain: Gain::UNITY,
            audio_stream: None,
            transform: crate::Transform::IDENTITY,
            opacity: crate::Opacity::OPAQUE,
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
        assert!(matches!(
            error,
            InsertError::Overlapping(overlap) if overlap.existing.start == Time::ZERO
        ));
        assert_eq!(track.clips().len(), 1);
    }

    #[test]
    fn inserted_clips_need_a_length_and_times_from_zero() {
        let mut track = Track::new(TrackKind::Video);

        let empty = clip(0, 0);
        let backwards = clip(0, -1);
        let before_zero = clip(-1, 2);
        let before_media = Clip {
            source: TimeRange::new(Time::from_seconds(-1), Time::from_seconds(2)),
            ..clip(0, 2)
        };

        assert_eq!(
            track.insert(empty),
            Err(InvalidClip::Empty(empty.id).into())
        );
        assert_eq!(
            track.insert(backwards),
            Err(InvalidClip::Empty(backwards.id).into())
        );
        assert_eq!(
            track.insert(before_zero),
            Err(InvalidClip::NegativeTime(before_zero.id).into())
        );
        assert_eq!(
            track.insert(before_media),
            Err(InvalidClip::NegativeTime(before_media.id).into())
        );
        assert!(track.clips().is_empty());
    }

    #[test]
    fn inserted_clips_must_end_within_representable_time() {
        let mut track = Track::new(TrackKind::Video);

        let past_the_end = Clip {
            start: Time::MAX - Time::from_seconds(1),
            ..clip(0, 2)
        };
        let source_past_the_end = Clip {
            source: TimeRange::new(Time::MAX - Time::from_seconds(1), Time::from_seconds(2)),
            ..clip(0, 2)
        };
        let at_the_end = Clip {
            start: Time::MAX - Time::from_seconds(2),
            ..clip(0, 2)
        };

        assert_eq!(
            track.insert(past_the_end),
            Err(InvalidClip::EndOverflows(past_the_end.id).into())
        );
        assert_eq!(
            track.insert(source_past_the_end),
            Err(InvalidClip::EndOverflows(source_past_the_end.id).into())
        );
        assert_eq!(track.insert(at_the_end), Ok(()));
        assert_eq!(track.end(), Time::MAX);
    }

    #[test]
    fn edits_refuse_clips_that_would_end_past_representable_time() {
        let (mut project, asset, first, _) = two_clip_project();
        let late = Time::MAX - Time::from_seconds(5);

        assert_eq!(
            project.clip_for(asset, 0, late),
            Err(EditError::Invalid(InvalidClip::EndOverflows(
                project.next_ids.clip
            )))
        );
        assert_eq!(
            project.place_clip(asset, 0, late),
            Err(EditError::Invalid(InvalidClip::EndOverflows(
                project.next_ids.clip
            )))
        );
        assert_eq!(
            project.move_clip(first, 0, late),
            Err(EditError::Invalid(InvalidClip::EndOverflows(first)))
        );
        assert_eq!(
            project.find_clip(first).map(|(_, clip)| clip.start),
            Some(Time::ZERO)
        );
    }

    #[test]
    fn overlapping_clips_are_those_sharing_time_with_the_range() {
        let mut track = Track::new(TrackKind::Video);
        for (start, duration) in [(0, 2), (2, 1), (5, 3), (10, 1)] {
            track.insert(clip(start, duration)).unwrap();
        }

        for start in -1..12 {
            for duration in -1..5 {
                let range = TimeRange::new(Time::from_seconds(start), Time::from_seconds(duration));
                let scanned: Vec<Clip> = track
                    .clips()
                    .iter()
                    .filter(|clip| clip.timeline_range().overlaps(range))
                    .copied()
                    .collect();

                assert_eq!(track.clips_overlapping(range), scanned, "{range:?}");
            }
        }
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
                frame_rate: Some(FrameRate::FPS_30),
                ..VideoStream::new(
                    0,
                    "h264",
                    NonZero::new(1280).unwrap(),
                    NonZero::new(720).unwrap(),
                )
            })],
        }
    }

    fn audio_info(seconds: i64) -> MediaInfo {
        MediaInfo {
            duration: Some(Time::from_seconds(seconds)),
            streams: vec![Stream::Audio(AudioStream::new(
                0,
                "opus",
                NonZero::new(48_000).unwrap(),
                NonZero::new(2).unwrap(),
            ))],
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
            Err(EditError::MissingStream(TrackKind::Audio))
        );
        assert_eq!(
            project.place_clip(audio, 0, Time::ZERO),
            Err(EditError::MissingStream(TrackKind::Video))
        );
        assert!(project.place_clip(audio, 1, Time::ZERO).is_ok());
    }

    #[test]
    fn placement_refuses_unknown_targets_and_durations() {
        let mut project = Project::new("test");
        let asset = project.add_asset("a.mkv".into(), video_info(3));
        let untimed = project.add_asset(
            "b.opus".into(),
            MediaInfo {
                duration: None,
                ..audio_info(0)
            },
        );
        assert_eq!(
            project.place_clip(AssetId(9), 0, Time::ZERO),
            Err(EditError::UnknownAsset(AssetId(9)))
        );
        assert_eq!(
            project.place_clip(asset, 7, Time::ZERO),
            Err(EditError::UnknownTrack(7))
        );
        assert_eq!(
            project.place_clip(untimed, 1, Time::ZERO),
            Err(EditError::NoDuration)
        );
    }

    #[test]
    fn a_still_gets_a_default_length_that_can_be_stretched() {
        let mut project = Project::new("test");
        let still = project.add_asset(
            "poster.png".into(),
            MediaInfo {
                duration: None,
                ..video_info(0)
            },
        );
        let after = project.add_asset("a.mkv".into(), video_info(3));

        let placed = project.place_clip(still, 0, Time::ZERO).unwrap();

        assert_eq!(placed.source.duration, Asset::STILL_DURATION);
        assert!(project.asset(still).unwrap().is_still());
        assert!(!project.asset(after).unwrap().is_still());

        project
            .place_clip(after, 0, Time::from_seconds(60))
            .unwrap();
        let stretched = project
            .trim_clip(placed.id, ClipEdge::End, Time::from_seconds(40))
            .unwrap();

        assert_eq!(stretched.timeline_range().end(), Time::from_seconds(40));

        let capped = project
            .trim_clip(placed.id, ClipEdge::End, Time::from_seconds(90))
            .unwrap();

        assert_eq!(capped.timeline_range().end(), Time::from_seconds(60));
    }

    #[test]
    fn placement_keeps_clips_apart() {
        let mut project = Project::new("test");
        let asset = project.add_asset("a.mkv".into(), video_info(3));
        project.place_clip(asset, 0, Time::ZERO).unwrap();
        let before = project.timeline.clone();
        assert!(matches!(
            project.clip_for(asset, 0, Time::from_seconds(2)),
            Err(EditError::Overlapping(_))
        ));
        assert!(matches!(
            project.place_clip(asset, 0, Time::from_seconds(2)),
            Err(EditError::Overlapping(_))
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

    #[test]
    fn video_clips_stack_from_the_bottom_track_up() {
        let mut timeline = Timeline {
            tracks: [
                TrackKind::Video,
                TrackKind::Video,
                TrackKind::Audio,
                TrackKind::Video,
            ]
            .map(Track::new)
            .to_vec(),
        };
        let clips = [(0, 1, 0, 6), (1, 2, 2, 2), (2, 3, 0, 8), (3, 4, 1, 4)];
        for (track, asset, start, duration) in clips {
            let clip = Clip {
                asset: AssetId(asset),
                ..clip(start, duration)
            };
            timeline.tracks[track].insert(clip).unwrap();
        }
        let stack = |seconds| {
            timeline
                .video_clips_at(Time::from_seconds(seconds))
                .map(|clip| clip.asset)
                .collect::<Vec<_>>()
        };
        assert_eq!(stack(0), [AssetId(1)]);
        assert_eq!(stack(1), [AssetId(1), AssetId(4)]);
        assert_eq!(stack(2), [AssetId(1), AssetId(2), AssetId(4)]);
        assert_eq!(stack(5), [AssetId(1)]);
        assert_eq!(stack(7), []);
        for seconds in 0..8 {
            assert_eq!(
                timeline.top_video_clip_at(Time::from_seconds(seconds)),
                timeline.video_clips_at(Time::from_seconds(seconds)).last()
            );
        }
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
    fn deleted_ids_are_not_given_out_again() {
        let (mut project, asset, _, second) = two_clip_project();

        project.delete_clip(second).unwrap();
        let placed = project
            .place_clip(asset, 0, Time::from_seconds(20))
            .unwrap();
        let (_, tail) = project
            .split_clip(placed.id, Time::from_seconds(25))
            .unwrap();
        project.ripple_delete_clip(tail.id).unwrap();
        let (_, retail) = project
            .split_clip(placed.id, Time::from_seconds(22))
            .unwrap();

        assert_ne!(placed.id, second);
        assert!(![second, placed.id, tail.id].contains(&retail.id));
        assert_eq!(project.next_ids.clip, ClipId(retail.id.0 + 1));
        assert!(project.next_ids.has_issued_clip(retail.id));
        assert!(!project.next_ids.has_issued_clip(project.next_ids.clip));
    }

    #[test]
    fn previewing_a_placement_gives_out_no_id() {
        let mut project = Project::new("test");

        let asset = project.add_asset("a.mkv".into(), video_info(3));
        let preview = project.clip_for(asset, 0, Time::ZERO).unwrap();
        let again = project.clip_for(asset, 0, Time::from_seconds(5)).unwrap();
        let placed = project.place_clip(asset, 0, Time::ZERO).unwrap();

        assert_eq!(preview.id, again.id);
        assert_eq!(placed, preview);
        assert_eq!(project.next_ids.clip, ClipId(placed.id.0 + 1));
        assert_eq!(project.next_ids.asset, AssetId(asset.0 + 1));
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
            Err(EditError::Overlapping(_))
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
            Err(EditError::MissingStream(TrackKind::Audio))
        );
        let moved = project.move_clip(first, 2, Time::from_seconds(-3)).unwrap();
        assert_eq!(moved.start, Time::ZERO);
        assert_eq!(project.find_clip(first).map(|(track, _)| track), Some(2));
        assert_eq!(project.timeline.tracks[0].clips().len(), 1);
        assert_eq!(
            project.move_clip(ClipId(99), 0, Time::ZERO),
            Err(EditError::UnknownClip(ClipId(99)))
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

    #[test]
    fn splitting_leaves_two_clips_that_play_the_same_frames() {
        let (mut project, _, first, _) = two_clip_project();
        let (head, tail) = project.split_clip(first, Time::from_seconds(4)).unwrap();
        assert_eq!(head.id, first);
        assert_ne!(tail.id, first);
        assert_eq!(
            head.source,
            TimeRange::new(Time::ZERO, Time::from_seconds(4))
        );
        assert_eq!(tail.start, Time::from_seconds(4));
        assert_eq!(
            tail.source,
            TimeRange::new(Time::from_seconds(4), Time::from_seconds(6))
        );
        let track = &project.timeline.tracks[0];
        assert_eq!(track.clips().len(), 3);
        for seconds in [1, 3, 4, 9] {
            let time = Time::from_seconds(seconds);
            let source = track
                .clip_at(time)
                .and_then(|clip| clip.source_time_at(time));
            assert_eq!(source, Some(time));
        }
    }

    #[test]
    fn splitting_needs_a_time_strictly_inside_the_clip() {
        let (mut project, _, first, _) = two_clip_project();
        let before = project.timeline.clone();
        for seconds in [0, 10, 15] {
            let time = Time::from_seconds(seconds);
            assert_eq!(
                project.split_clip(first, time),
                Err(EditError::OutsideClip { clip: first, time })
            );
        }
        assert_eq!(project.timeline, before);
    }

    #[test]
    fn clips_cut_by_a_time_skip_clips_starting_or_ending_there() {
        let (mut project, asset, first, _) = two_clip_project();
        let video = project.timeline.tracks.len();
        project.timeline.tracks.push(Track::new(TrackKind::Video));
        let upper = project
            .place_clip(asset, video, Time::from_seconds(5))
            .unwrap()
            .id;
        let cut = |project: &Project, seconds| -> Vec<ClipId> {
            project
                .timeline
                .clips_cut_by(Time::from_seconds(seconds))
                .map(|clip| clip.id)
                .collect()
        };
        assert_eq!(cut(&project, 7), [first, upper]);
        assert_eq!(cut(&project, 10), [upper]);
        assert_eq!(cut(&project, 5), [first]);
        assert!(cut(&project, 20).is_empty());
    }

    #[test]
    fn deleting_leaves_a_gap() {
        let (mut project, _, first, second) = two_clip_project();
        let deleted = project.delete_clip(first).unwrap();
        assert_eq!(deleted.id, first);
        assert_eq!(project.find_clip(first), None);
        assert_eq!(
            project.find_clip(second).map(|(_, clip)| clip.start),
            Some(Time::from_seconds(20))
        );
        assert_eq!(
            project.delete_clip(first),
            Err(EditError::UnknownClip(first))
        );
    }

    #[test]
    fn ripple_delete_pulls_later_clips_back_by_the_clip_length() {
        let (mut project, asset, first, second) = two_clip_project();
        let third = project
            .place_clip(asset, 0, Time::from_seconds(30))
            .unwrap()
            .id;
        project.split_clip(first, Time::from_seconds(4)).unwrap();
        project.ripple_delete_clip(first).unwrap();
        let starts: Vec<_> = project.timeline.tracks[0]
            .clips()
            .iter()
            .map(|clip| clip.start)
            .collect();
        assert_eq!(starts, [0, 16, 26].map(Time::from_seconds).to_vec());
        assert_eq!(
            project.find_clip(second).map(|(_, clip)| clip.source.start),
            Some(Time::ZERO)
        );
        project.ripple_delete_clip(second).unwrap();
        assert_eq!(
            project.find_clip(third).map(|(_, clip)| clip.start),
            Some(Time::from_seconds(16))
        );
        assert_eq!(project.timeline.duration(), Time::from_seconds(26));
    }

    fn kinds(timeline: &Timeline) -> Vec<TrackKind> {
        timeline.tracks.iter().map(|track| track.kind).collect()
    }

    #[test]
    fn added_tracks_join_the_others_of_their_kind() {
        use TrackKind::{Audio, Video};
        let mut timeline = Project::new("test").timeline;
        assert_eq!(timeline.add_track(Video), 1);
        assert_eq!(timeline.add_track(Audio), 3);
        assert_eq!(timeline.add_track(Video), 2);
        assert_eq!(kinds(&timeline), [Video, Video, Video, Audio, Audio]);
        let mut empty = Timeline::default();
        assert_eq!(empty.add_track(Audio), 0);
        assert_eq!(empty.add_track(Video), 0);
        assert_eq!(kinds(&empty), [Video, Audio]);
    }

    #[test]
    fn only_empty_tracks_with_a_sibling_can_be_removed() {
        let (mut project, _, first, _) = two_clip_project();
        let timeline = &mut project.timeline;
        assert_eq!(
            timeline.remove_track(1),
            Err(EditError::LastTrack(TrackKind::Audio))
        );
        assert_eq!(timeline.remove_track(5), Err(EditError::UnknownTrack(5)));
        let spare = timeline.add_track(TrackKind::Video);
        assert_eq!(timeline.remove_track(0), Err(EditError::TrackNotEmpty(0)));
        assert!(timeline.remove_track(spare).unwrap().clips().is_empty());
        assert_eq!(project.find_clip(first).map(|(track, _)| track), Some(0));
    }

    #[test]
    fn swapping_tracks_keeps_their_clips_and_kinds_apart() {
        let (mut project, _, first, _) = two_clip_project();
        let upper = project.timeline.add_track(TrackKind::Video);
        assert_eq!(
            project.timeline.swap_tracks(0, 2),
            Err(EditError::MixedTrackKinds {
                first: TrackKind::Video,
                second: TrackKind::Audio,
            })
        );
        project.timeline.swap_tracks(0, upper).unwrap();
        assert_eq!(
            project.find_clip(first).map(|(track, _)| track),
            Some(upper)
        );
        assert!(project.timeline.tracks[0].clips().is_empty());
    }

    #[test]
    fn pasting_copies_a_clip_under_a_new_id() {
        let (mut project, _, first, _) = two_clip_project();
        let original = project.find_clip(first).map(|(_, clip)| *clip).unwrap();
        let next = project.next_ids.clip;

        let preview = project
            .pasted_clip(&original, 0, Time::from_seconds(10))
            .unwrap();
        let pasted = project
            .paste_clip(&original, 0, Time::from_seconds(10))
            .unwrap();

        assert_eq!(preview, pasted);
        assert_eq!(pasted.id, next);
        assert_eq!(pasted.asset, original.asset);
        assert_eq!(pasted.source, original.source);
        assert_eq!(pasted.start, Time::from_seconds(10));
        assert_eq!(project.next_ids.clip, ClipId(next.0 + 1));
        assert_eq!(
            project.find_clip(first).map(|(_, clip)| *clip),
            Some(original)
        );
    }

    #[test]
    fn pasting_follows_the_placement_rules() {
        let (mut project, _, first, _) = two_clip_project();
        let original = project.find_clip(first).map(|(_, clip)| *clip).unwrap();
        let before = project.clone();

        assert!(matches!(
            project.paste_clip(&original, 0, Time::from_seconds(5)),
            Err(EditError::Overlapping(_))
        ));
        assert_eq!(
            project.paste_clip(&original, 1, Time::from_seconds(5)),
            Err(EditError::MissingStream(TrackKind::Audio))
        );
        assert_eq!(
            project.paste_clip(&original, 7, Time::from_seconds(5)),
            Err(EditError::UnknownTrack(7))
        );
        assert_eq!(
            project.paste_clip(
                &Clip {
                    asset: AssetId(9),
                    ..original
                },
                0,
                Time::from_seconds(10)
            ),
            Err(EditError::UnknownAsset(AssetId(9)))
        );
        assert_eq!(project, before);

        let clamped = project
            .paste_clip(&original, 0, Time::from_seconds(-40))
            .unwrap_err();
        assert!(matches!(clamped, EditError::Overlapping(_)));
    }

    #[test]
    fn assets_in_use_cannot_be_removed() {
        let (mut project, asset, first, second) = two_clip_project();
        let spare = project.add_asset("b.mkv".into(), video_info(2));

        assert_eq!(
            project.remove_asset(asset),
            Err(EditError::AssetInUse(asset))
        );
        assert_eq!(
            project.remove_asset(AssetId(9)),
            Err(EditError::UnknownAsset(AssetId(9)))
        );
        assert_eq!(project.remove_asset(spare).map(|asset| asset.id), Ok(spare));
        assert_eq!(project.asset(spare), None);

        project.delete_clip(first).unwrap();
        assert!(project.is_asset_used(asset));

        project.delete_clip(second).unwrap();
        assert!(!project.is_asset_used(asset));
        assert_eq!(project.remove_asset(asset).map(|asset| asset.id), Ok(asset));
        assert!(project.assets.is_empty());
    }

    #[test]
    fn removed_asset_ids_are_not_given_out_again() {
        let mut project = Project::new("test");

        let first = project.add_asset("a.mkv".into(), video_info(1));
        project.remove_asset(first).unwrap();
        let second = project.add_asset("b.mkv".into(), video_info(1));

        assert_ne!(first, second);
        assert!(project.next_ids.has_issued_asset(first));
    }

    #[test]
    fn pruning_removes_only_the_unused_assets() {
        let (mut project, used, _, _) = two_clip_project();
        let first_spare = project.add_asset("b.mkv".into(), video_info(2));
        let second_spare = project.add_asset("c.opus".into(), audio_info(2));

        let pruned = project.prune_assets();

        assert_eq!(
            pruned.iter().map(|asset| asset.id).collect::<Vec<_>>(),
            [first_spare, second_spare]
        );
        assert_eq!(
            project
                .assets
                .iter()
                .map(|asset| asset.id)
                .collect::<Vec<_>>(),
            [used]
        );
        assert!(project.prune_assets().is_empty());
    }

    #[test]
    fn a_locked_track_refuses_every_edit_of_its_clips() {
        let (mut project, asset, first, _) = two_clip_project();
        let other = project.timeline.add_track(TrackKind::Video);
        let moved_in = project.place_clip(asset, other, Time::ZERO).unwrap().id;
        project.timeline.tracks[0].locked = true;

        let before = project.clone();

        assert_eq!(
            project.place_clip(asset, 0, Time::from_seconds(40)),
            Err(EditError::TrackLocked(0))
        );
        assert_eq!(
            project.move_clip(first, other, Time::from_seconds(50)),
            Err(EditError::TrackLocked(0))
        );
        assert_eq!(
            project.move_clip(moved_in, 0, Time::from_seconds(50)),
            Err(EditError::TrackLocked(0))
        );
        assert_eq!(
            project.trim_clip(first, ClipEdge::End, Time::from_seconds(4)),
            Err(EditError::TrackLocked(0))
        );
        assert_eq!(
            project.split_clip(first, Time::from_seconds(4)),
            Err(EditError::TrackLocked(0))
        );
        assert_eq!(project.delete_clip(first), Err(EditError::TrackLocked(0)));
        assert_eq!(
            project.ripple_delete_clip(first),
            Err(EditError::TrackLocked(0))
        );
        assert_eq!(
            project.delete_clips(&[first, moved_in]),
            Err(EditError::TrackLocked(0))
        );
        assert_eq!(project, before);

        project.timeline.tracks[0].locked = false;

        assert!(project.delete_clip(first).is_ok());
    }

    #[test]
    fn tracks_start_unnamed_unlocked_and_audible() {
        let track = Track::new(TrackKind::Audio);

        assert_eq!(track.name, "");
        assert!(!track.locked && !track.muted && !track.solo);
        assert_eq!(track.height, TrackHeight::Normal);
    }

    #[test]
    fn a_muted_video_track_is_left_out_of_the_composite() {
        let mut timeline = Timeline {
            tracks: [TrackKind::Video, TrackKind::Video]
                .map(Track::new)
                .to_vec(),
        };
        for (track, asset) in [(0, 1), (1, 2)] {
            let clip = Clip {
                asset: AssetId(asset),
                ..clip(0, 4)
            };
            timeline.tracks[track].insert(clip).unwrap();
        }

        timeline.tracks[1].muted = true;

        assert_eq!(
            timeline
                .top_video_clip_at(Time::from_seconds(1))
                .map(|clip| clip.asset),
            Some(AssetId(1))
        );

        timeline.tracks[0].muted = true;

        assert_eq!(timeline.top_video_clip_at(Time::from_seconds(1)), None);
    }

    #[test]
    fn edit_points_are_the_starts_and_ends_of_clips_on_any_track() {
        let (mut project, asset, _, _) = two_clip_project();
        let upper = project.timeline.add_track(TrackKind::Video);
        project
            .place_clip(asset, upper, Time::from_seconds(12))
            .unwrap();
        let timeline = &project.timeline;
        let next = |seconds| timeline.next_edit_after(Time::from_seconds(seconds));
        let previous = |seconds| timeline.previous_edit_before(Time::from_seconds(seconds));

        assert_eq!(next(-1), Some(Time::ZERO));
        assert_eq!(next(0), Some(Time::from_seconds(10)));
        assert_eq!(next(10), Some(Time::from_seconds(12)));
        assert_eq!(next(12), Some(Time::from_seconds(20)));
        assert_eq!(next(22), Some(Time::from_seconds(30)));
        assert_eq!(next(30), None);
        assert_eq!(previous(0), None);
        assert_eq!(previous(10), Some(Time::ZERO));
        assert_eq!(previous(30), Some(Time::from_seconds(22)));
        assert_eq!(previous(99), Some(Time::from_seconds(30)));
    }

    #[test]
    fn relinking_swaps_the_media_when_every_clip_still_fits() {
        let (mut project, asset, first, _) = two_clip_project();

        project
            .relink_asset(asset, "/moved/a.mkv".into(), video_info(12))
            .unwrap();

        assert_eq!(
            project.asset(asset).map(|asset| asset.path.clone()),
            Some("/moved/a.mkv".into())
        );
        assert_eq!(
            project.asset(asset).and_then(|asset| asset.info.duration),
            Some(Time::from_seconds(12))
        );
        assert!(project.find_clip(first).is_some());
    }

    #[test]
    fn relinking_refuses_media_that_breaks_a_clip() {
        let (mut project, asset, first, _) = two_clip_project();
        let before = project.clone();

        assert_eq!(
            project.relink_asset(asset, "/short.mkv".into(), video_info(5)),
            Err(EditError::BeyondMedia(first))
        );
        assert_eq!(
            project.relink_asset(asset, "/sound.opus".into(), audio_info(20)),
            Err(EditError::MissingStream(TrackKind::Video))
        );
        assert_eq!(
            project.relink_asset(AssetId(9), "/x.mkv".into(), video_info(20)),
            Err(EditError::UnknownAsset(AssetId(9)))
        );
        assert_eq!(project, before);
    }
}
