use std::{collections::HashSet, path::PathBuf};

use serde::{Deserialize, Serialize};
use tessera_timeline::{
    self as model, AssetId, ClipId, FrameRate, MediaInfo, NextIds, Time, TimeRange, Timeline,
};

use crate::{FormatError, ValidationError};

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Project {
    name: String,
    settings: SequenceSettings,
    next_asset_id: u64,
    next_clip_id: u64,
    assets: Vec<Asset>,
    tracks: Vec<Track>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SequenceSettings {
    width: u32,
    height: u32,
    frame_rate: Rational,
    sample_rate: u32,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Rational {
    numerator: u32,
    denominator: u32,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Asset {
    id: u64,
    path: String,
    duration_flicks: Option<i64>,
    streams: Vec<Stream>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Stream {
    Video {
        index: usize,
        codec: String,
        width: u32,
        height: u32,
        frame_rate: Option<Rational>,
    },
    Audio {
        index: usize,
        codec: String,
        sample_rate: u32,
        channels: u16,
    },
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum TrackKind {
    Video,
    Audio,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Track {
    kind: TrackKind,
    clips: Vec<Clip>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Clip {
    id: u64,
    asset: u64,
    start_flicks: i64,
    source_start_flicks: i64,
    source_duration_flicks: i64,
}

impl TryFrom<&model::Project> for Project {
    type Error = FormatError;

    fn try_from(project: &model::Project) -> Result<Self, FormatError> {
        Ok(Self {
            name: project.name.clone(),
            settings: project.settings.into(),
            next_asset_id: project.next_ids.asset.0,
            next_clip_id: project.next_ids.clip.0,
            assets: project
                .assets
                .iter()
                .map(Asset::try_from)
                .collect::<Result<_, _>>()?,
            tracks: project.timeline.tracks.iter().map(Track::from).collect(),
        })
    }
}

impl From<model::SequenceSettings> for SequenceSettings {
    fn from(settings: model::SequenceSettings) -> Self {
        Self {
            width: settings.width,
            height: settings.height,
            frame_rate: settings.frame_rate.into(),
            sample_rate: settings.sample_rate,
        }
    }
}

impl From<FrameRate> for Rational {
    fn from(rate: FrameRate) -> Self {
        Self {
            numerator: rate.numerator,
            denominator: rate.denominator,
        }
    }
}

impl TryFrom<&model::Asset> for Asset {
    type Error = FormatError;

    fn try_from(asset: &model::Asset) -> Result<Self, FormatError> {
        let path = asset
            .path
            .to_str()
            .ok_or_else(|| FormatError::NonUnicodePath(asset.path.clone()))?;
        Ok(Self {
            id: asset.id.0,
            path: path.to_owned(),
            duration_flicks: asset.info.duration.map(Time::flicks),
            streams: asset.info.streams.iter().map(Stream::from).collect(),
        })
    }
}

impl From<&model::Stream> for Stream {
    fn from(stream: &model::Stream) -> Self {
        match stream {
            model::Stream::Video(video) => Self::Video {
                index: video.index,
                codec: video.codec.clone(),
                width: video.width,
                height: video.height,
                frame_rate: video.frame_rate.map(Rational::from),
            },
            model::Stream::Audio(audio) => Self::Audio {
                index: audio.index,
                codec: audio.codec.clone(),
                sample_rate: audio.sample_rate,
                channels: audio.channels,
            },
        }
    }
}

impl From<model::TrackKind> for TrackKind {
    fn from(kind: model::TrackKind) -> Self {
        match kind {
            model::TrackKind::Video => Self::Video,
            model::TrackKind::Audio => Self::Audio,
        }
    }
}

impl From<TrackKind> for model::TrackKind {
    fn from(kind: TrackKind) -> Self {
        match kind {
            TrackKind::Video => Self::Video,
            TrackKind::Audio => Self::Audio,
        }
    }
}

impl From<&model::Track> for Track {
    fn from(track: &model::Track) -> Self {
        Self {
            kind: track.kind.into(),
            clips: track.clips().iter().map(Clip::from).collect(),
        }
    }
}

impl From<&model::Clip> for Clip {
    fn from(clip: &model::Clip) -> Self {
        Self {
            id: clip.id.0,
            asset: clip.asset.0,
            start_flicks: clip.start.flicks(),
            source_start_flicks: clip.source.start.flicks(),
            source_duration_flicks: clip.source.duration.flicks(),
        }
    }
}

impl TryFrom<Project> for model::Project {
    type Error = ValidationError;

    fn try_from(project: Project) -> Result<Self, ValidationError> {
        let mut rebuilt = Self {
            name: project.name,
            settings: project.settings.try_into()?,
            assets: Vec::with_capacity(project.assets.len()),
            timeline: Timeline::default(),
            next_ids: NextIds {
                asset: AssetId(project.next_asset_id),
                clip: ClipId(project.next_clip_id),
            },
        };
        for asset in project.assets {
            let asset = model::Asset::try_from(asset)?;
            if rebuilt.asset(asset.id).is_some() {
                return Err(ValidationError::DuplicateAsset(asset.id));
            }
            if !rebuilt.next_ids.has_issued_asset(asset.id) {
                return Err(ValidationError::UnissuedAsset {
                    asset: asset.id,
                    next: rebuilt.next_ids.asset,
                });
            }
            rebuilt.assets.push(asset);
        }
        let mut clip_ids = HashSet::new();
        for track in project.tracks {
            let track = track.rebuilt(&rebuilt)?;
            if let Some(repeated) = track.clips().iter().find(|clip| !clip_ids.insert(clip.id)) {
                return Err(ValidationError::DuplicateClip(repeated.id));
            }
            if let Some(unissued) = track
                .clips()
                .iter()
                .find(|clip| !rebuilt.next_ids.has_issued_clip(clip.id))
            {
                return Err(ValidationError::UnissuedClip {
                    clip: unissued.id,
                    next: rebuilt.next_ids.clip,
                });
            }
            rebuilt.timeline.tracks.push(track);
        }
        for kind in [model::TrackKind::Video, model::TrackKind::Audio] {
            if !rebuilt
                .timeline
                .tracks
                .iter()
                .any(|track| track.kind == kind)
            {
                return Err(ValidationError::NoTrack(kind));
            }
        }
        Ok(rebuilt)
    }
}

impl TryFrom<SequenceSettings> for model::SequenceSettings {
    type Error = ValidationError;

    fn try_from(settings: SequenceSettings) -> Result<Self, ValidationError> {
        let Rational {
            numerator,
            denominator,
        } = settings.frame_rate;
        let frame_rate =
            settings
                .frame_rate
                .positive()
                .ok_or(ValidationError::SequenceFrameRate {
                    numerator,
                    denominator,
                })?;
        if settings.sample_rate == 0 {
            return Err(ValidationError::ZeroSampleRate);
        }
        Ok(Self {
            width: settings.width,
            height: settings.height,
            frame_rate,
            sample_rate: settings.sample_rate,
        })
    }
}

impl Rational {
    fn positive(self) -> Option<FrameRate> {
        (self.numerator > 0 && self.denominator > 0)
            .then(|| FrameRate::new(self.numerator, self.denominator))
    }
}

impl TryFrom<Asset> for model::Asset {
    type Error = ValidationError;

    fn try_from(asset: Asset) -> Result<Self, ValidationError> {
        let id = AssetId(asset.id);
        Ok(Self {
            id,
            path: PathBuf::from(asset.path),
            info: MediaInfo {
                duration: asset.duration_flicks.map(Time::from_flicks),
                streams: asset
                    .streams
                    .into_iter()
                    .map(|stream| stream.rebuilt(id))
                    .collect::<Result<_, _>>()?,
            },
        })
    }
}

impl Stream {
    fn rebuilt(self, asset: AssetId) -> Result<model::Stream, ValidationError> {
        Ok(match self {
            Self::Video {
                index,
                codec,
                width,
                height,
                frame_rate,
            } => model::Stream::Video(model::VideoStream {
                index,
                codec,
                width,
                height,
                frame_rate: frame_rate
                    .map(|rate| {
                        rate.positive().ok_or(ValidationError::StreamFrameRate {
                            asset,
                            stream: index,
                            numerator: rate.numerator,
                            denominator: rate.denominator,
                        })
                    })
                    .transpose()?,
            }),
            Self::Audio {
                index,
                codec,
                sample_rate,
                channels,
            } => model::Stream::Audio(model::AudioStream {
                index,
                codec,
                sample_rate,
                channels,
            }),
        })
    }
}

impl Track {
    fn rebuilt(self, project: &model::Project) -> Result<model::Track, ValidationError> {
        let mut track = model::Track::new(self.kind.into());
        for clip in self.clips {
            let clip = clip.rebuilt(project, track.kind)?;
            track
                .insert(clip)
                .map_err(|source| ValidationError::Overlapping {
                    clip: clip.id,
                    source,
                })?;
        }
        Ok(track)
    }
}

impl Clip {
    fn rebuilt(
        self,
        project: &model::Project,
        kind: model::TrackKind,
    ) -> Result<model::Clip, ValidationError> {
        let clip = model::Clip {
            id: ClipId(self.id),
            asset: AssetId(self.asset),
            source: TimeRange::new(
                Time::from_flicks(self.source_start_flicks),
                Time::from_flicks(self.source_duration_flicks),
            ),
            start: Time::from_flicks(self.start_flicks),
        };
        let asset = project
            .asset(clip.asset)
            .ok_or(ValidationError::UnknownAsset {
                clip: clip.id,
                asset: clip.asset,
            })?;
        if !asset.has_stream(kind) {
            return Err(ValidationError::MissingStream {
                clip: clip.id,
                asset: asset.id,
                kind,
            });
        }
        if clip.source.duration <= Time::ZERO {
            return Err(ValidationError::EmptyClip(clip.id));
        }
        if clip.start < Time::ZERO || clip.source.start < Time::ZERO {
            return Err(ValidationError::NegativeTime(clip.id));
        }
        if asset
            .info
            .duration
            .is_some_and(|duration| clip.source.end() > duration)
        {
            return Err(ValidationError::BeyondMedia {
                clip: clip.id,
                asset: asset.id,
            });
        }
        Ok(clip)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};
    use tessera_timeline::TrackKind::{Audio, Video};

    use super::*;

    const SECOND: i64 = tessera_timeline::FLICKS_PER_SECOND;

    fn clip(id: u64, asset: u64, start: i64, source_start: i64, duration: i64) -> Value {
        json!({
            "id": id,
            "asset": asset,
            "start_flicks": start * SECOND,
            "source_start_flicks": source_start * SECOND,
            "source_duration_flicks": duration * SECOND,
        })
    }

    fn valid() -> Value {
        json!({
            "name": "Checked",
            "settings": {
                "width": 1920,
                "height": 1080,
                "frame_rate": { "numerator": 25, "denominator": 1 },
                "sample_rate": 48000,
            },
            "next_asset_id": 8,
            "next_clip_id": 3,
            "assets": [
                {
                    "id": 4,
                    "path": "/media/a.mkv",
                    "duration_flicks": 10 * SECOND,
                    "streams": [
                        {
                            "kind": "video",
                            "index": 0,
                            "codec": "vp9",
                            "width": 1920,
                            "height": 1080,
                            "frame_rate": { "numerator": 25, "denominator": 1 },
                        },
                    ],
                },
                {
                    "id": 7,
                    "path": "/media/b.opus",
                    "duration_flicks": null,
                    "streams": [
                        {
                            "kind": "audio",
                            "index": 0,
                            "codec": "opus",
                            "sample_rate": 48000,
                            "channels": 2,
                        },
                    ],
                },
            ],
            "tracks": [
                { "kind": "video", "clips": [clip(0, 4, 0, 0, 4), clip(1, 4, 4, 4, 6)] },
                { "kind": "audio", "clips": [clip(2, 7, 0, 0, 30)] },
            ],
        })
    }

    fn rebuilt(document: Value) -> Result<model::Project, ValidationError> {
        serde_json::from_value::<Project>(document)
            .unwrap()
            .try_into()
    }

    fn refused(edit: impl FnOnce(&mut Value)) -> ValidationError {
        let mut document = valid();
        edit(&mut document);
        rebuilt(document).unwrap_err()
    }

    #[test]
    fn a_consistent_document_rebuilds_the_project() {
        let project = rebuilt(valid()).unwrap();
        assert_eq!(project.settings.frame_rate, FrameRate::FPS_25);
        assert_eq!(project.asset(AssetId(7)).unwrap().info.duration, None);
        assert_eq!(
            project.next_ids,
            NextIds {
                asset: AssetId(8),
                clip: ClipId(3)
            }
        );
        let ids: Vec<Vec<ClipId>> = project
            .timeline
            .tracks
            .iter()
            .map(|track| track.clips().iter().map(|clip| clip.id).collect())
            .collect();
        assert_eq!(ids, [vec![ClipId(0), ClipId(1)], vec![ClipId(2)]]);
    }

    #[test]
    fn frame_and_sample_rates_must_be_positive() {
        assert_eq!(
            refused(|document| document["settings"]["frame_rate"]["numerator"] = json!(0)),
            ValidationError::SequenceFrameRate {
                numerator: 0,
                denominator: 1
            }
        );
        assert_eq!(
            refused(|document| document["settings"]["frame_rate"]["denominator"] = json!(0)),
            ValidationError::SequenceFrameRate {
                numerator: 25,
                denominator: 0
            }
        );
        assert_eq!(
            refused(|document| {
                document["assets"][0]["streams"][0]["frame_rate"]["denominator"] = json!(0);
            }),
            ValidationError::StreamFrameRate {
                asset: AssetId(4),
                stream: 0,
                numerator: 25,
                denominator: 0
            }
        );
        assert_eq!(
            refused(|document| document["settings"]["sample_rate"] = json!(0)),
            ValidationError::ZeroSampleRate
        );
    }

    #[test]
    fn asset_and_clip_ids_must_be_unique() {
        assert_eq!(
            refused(|document| document["assets"][1]["id"] = json!(4)),
            ValidationError::DuplicateAsset(AssetId(4))
        );
        assert_eq!(
            refused(|document| document["tracks"][1]["clips"][0]["id"] = json!(1)),
            ValidationError::DuplicateClip(ClipId(1))
        );
        assert_eq!(
            refused(|document| document["tracks"][0]["clips"][1]["id"] = json!(0)),
            ValidationError::DuplicateClip(ClipId(0))
        );
    }

    #[test]
    fn every_id_must_have_been_issued_before_the_next_ones() {
        assert_eq!(
            refused(|document| document["next_asset_id"] = json!(7)),
            ValidationError::UnissuedAsset {
                asset: AssetId(7),
                next: AssetId(7)
            }
        );
        assert_eq!(
            refused(|document| document["next_clip_id"] = json!(1)),
            ValidationError::UnissuedClip {
                clip: ClipId(1),
                next: ClipId(1)
            }
        );
        assert_eq!(
            refused(|document| document["tracks"][1]["clips"][0]["id"] = json!(u64::MAX)),
            ValidationError::UnissuedClip {
                clip: ClipId(u64::MAX),
                next: ClipId(3)
            }
        );
    }

    #[test]
    fn clips_must_use_a_held_asset_with_a_stream_of_their_track_kind() {
        assert_eq!(
            refused(|document| document["tracks"][1]["clips"][0]["asset"] = json!(9)),
            ValidationError::UnknownAsset {
                clip: ClipId(2),
                asset: AssetId(9)
            }
        );
        assert_eq!(
            refused(|document| document["tracks"][1]["clips"][0]["asset"] = json!(4)),
            ValidationError::MissingStream {
                clip: ClipId(2),
                asset: AssetId(4),
                kind: Audio
            }
        );
    }

    #[test]
    fn clips_must_have_a_length_and_lie_within_their_media() {
        assert_eq!(
            refused(|document| document["tracks"][0]["clips"][1] = clip(1, 4, 4, 4, 0)),
            ValidationError::EmptyClip(ClipId(1))
        );
        assert_eq!(
            refused(|document| document["tracks"][0]["clips"][0] = clip(0, 4, -1, 0, 1)),
            ValidationError::NegativeTime(ClipId(0))
        );
        assert_eq!(
            refused(|document| document["tracks"][0]["clips"][0] = clip(0, 4, 0, -1, 4)),
            ValidationError::NegativeTime(ClipId(0))
        );
        assert_eq!(
            refused(|document| document["tracks"][0]["clips"][1] = clip(1, 4, 4, 5, 6)),
            ValidationError::BeyondMedia {
                clip: ClipId(1),
                asset: AssetId(4)
            }
        );
    }

    #[test]
    fn clips_on_a_track_must_not_overlap() {
        let error = refused(|document| document["tracks"][0]["clips"][1] = clip(1, 4, 3, 4, 6));
        assert!(matches!(
            error,
            ValidationError::Overlapping { clip: ClipId(1), source }
                if source.existing.start == Time::ZERO
        ));
    }

    #[test]
    fn the_timeline_keeps_a_track_of_each_kind() {
        assert_eq!(
            refused(|document| {
                document["tracks"].as_array_mut().unwrap().remove(1);
            }),
            ValidationError::NoTrack(Audio)
        );
        assert_eq!(
            refused(|document| document["tracks"] = json!([])),
            ValidationError::NoTrack(Video)
        );
    }
}
