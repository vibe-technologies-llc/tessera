use std::{
    collections::{BTreeMap, HashSet},
    num::{NonZeroU16, NonZeroU32},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use tessera_timeline::{
    self as model, AssetId, ClipId, FrameRate, InsertError, LinkId, MarkerId, MediaInfo, NextIds,
    Time, TimeRange, Timeline,
};

use crate::{FormatError, ValidationError};

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Project {
    name: String,
    settings: SequenceSettings,
    next_asset_id: u64,
    next_clip_id: u64,
    next_marker_id: u64,
    next_link_id: u64,
    assets: Vec<Asset>,
    tracks: Vec<Track>,
    markers: Vec<Marker>,
    in_point_flicks: Option<i64>,
    out_point_flicks: Option<i64>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Marker {
    id: u64,
    time_flicks: i64,
    name: String,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum TrackHeight {
    Compact,
    Normal,
    Tall,
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
    name: String,
    locked: bool,
    muted: bool,
    solo: bool,
    height: TrackHeight,
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
    link: Option<u64>,
}

impl TryFrom<&model::Project> for Project {
    type Error = FormatError;

    fn try_from(project: &model::Project) -> Result<Self, FormatError> {
        Self::encode(project, None)
    }
}

impl Project {
    pub fn encode(project: &model::Project, directory: Option<&Path>) -> Result<Self, FormatError> {
        let mut encoded = Self::from_model(project)?;
        if let Some(directory) = directory {
            for asset in &mut encoded.assets {
                if let Some(stored) = stored_path(Path::new(&asset.path), directory) {
                    asset.path = stored;
                }
            }
        }
        Ok(encoded)
    }

    pub fn decode(self, directory: Option<&Path>) -> Result<model::Project, ValidationError> {
        let mut project = model::Project::try_from(self)?;
        if let Some(directory) = directory {
            for asset in std::sync::Arc::make_mut(&mut project.assets) {
                if asset.path.is_relative() {
                    asset.path = directory.join(&asset.path);
                }
            }
        }
        Ok(project)
    }

    fn from_model(project: &model::Project) -> Result<Self, FormatError> {
        Ok(Self {
            name: project.name.clone(),
            settings: project.settings.into(),
            next_asset_id: project.next_ids.asset.0,
            next_clip_id: project.next_ids.clip.0,
            next_marker_id: project.next_ids.marker.0,
            next_link_id: project.next_ids.link.0,
            assets: project
                .assets
                .iter()
                .map(Asset::try_from)
                .collect::<Result<_, _>>()?,
            tracks: project.timeline.tracks.iter().map(Track::from).collect(),
            markers: project.markers.iter().map(Marker::from).collect(),
            in_point_flicks: project.in_point.map(Time::flicks),
            out_point_flicks: project.out_point.map(Time::flicks),
        })
    }
}

fn stored_path(path: &Path, directory: &Path) -> Option<String> {
    let absolute = std::path::absolute(path).ok()?;
    let stored = match absolute.strip_prefix(directory) {
        Ok(relative) if !relative.as_os_str().is_empty() => relative,
        _ => &absolute,
    };
    stored.to_str().map(str::to_owned)
}

impl From<model::SequenceSettings> for SequenceSettings {
    fn from(settings: model::SequenceSettings) -> Self {
        Self {
            width: settings.width.get(),
            height: settings.height.get(),
            frame_rate: settings.frame_rate.into(),
            sample_rate: settings.sample_rate.get(),
        }
    }
}

impl From<FrameRate> for Rational {
    fn from(rate: FrameRate) -> Self {
        Self {
            numerator: rate.numerator(),
            denominator: rate.denominator(),
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
                width: video.width.get(),
                height: video.height.get(),
                frame_rate: video.frame_rate.map(Rational::from),
            },
            model::Stream::Audio(audio) => Self::Audio {
                index: audio.index,
                codec: audio.codec.clone(),
                sample_rate: audio.sample_rate.get(),
                channels: audio.channels.get(),
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

impl From<&model::Marker> for Marker {
    fn from(marker: &model::Marker) -> Self {
        Self {
            id: marker.id.0,
            time_flicks: marker.time.flicks(),
            name: marker.name.clone(),
        }
    }
}

impl From<model::TrackHeight> for TrackHeight {
    fn from(height: model::TrackHeight) -> Self {
        match height {
            model::TrackHeight::Compact => Self::Compact,
            model::TrackHeight::Normal => Self::Normal,
            model::TrackHeight::Tall => Self::Tall,
        }
    }
}

impl From<TrackHeight> for model::TrackHeight {
    fn from(height: TrackHeight) -> Self {
        match height {
            TrackHeight::Compact => Self::Compact,
            TrackHeight::Normal => Self::Normal,
            TrackHeight::Tall => Self::Tall,
        }
    }
}

impl From<&model::Track> for Track {
    fn from(track: &model::Track) -> Self {
        Self {
            kind: track.kind.into(),
            name: track.name.clone(),
            locked: track.locked,
            muted: track.muted,
            solo: track.solo,
            height: track.height.into(),
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
            link: clip.link.map(|link| link.0),
        }
    }
}

impl TryFrom<Project> for model::Project {
    type Error = ValidationError;

    fn try_from(project: Project) -> Result<Self, ValidationError> {
        let mut rebuilt = Self {
            name: project.name,
            settings: project.settings.try_into()?,
            assets: Default::default(),
            timeline: Timeline::default(),
            markers: Vec::with_capacity(project.markers.len()),
            in_point: project.in_point_flicks.map(Time::from_flicks),
            out_point: project.out_point_flicks.map(Time::from_flicks),
            next_ids: NextIds {
                asset: AssetId(project.next_asset_id),
                clip: ClipId(project.next_clip_id),
                marker: MarkerId(project.next_marker_id),
                link: LinkId(project.next_link_id),
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
            std::sync::Arc::make_mut(&mut rebuilt.assets).push(asset);
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
        check_links(&rebuilt)?;
        let mut marker_ids = HashSet::new();
        for marker in project.markers {
            let id = MarkerId(marker.id);
            if !marker_ids.insert(id) {
                return Err(ValidationError::DuplicateMarker(id));
            }
            if !rebuilt.next_ids.has_issued_marker(id) {
                return Err(ValidationError::UnissuedMarker {
                    marker: id,
                    next: rebuilt.next_ids.marker,
                });
            }
            if marker.time_flicks < 0 {
                return Err(ValidationError::NegativeMarker {
                    marker: id,
                    flicks: marker.time_flicks,
                });
            }
            rebuilt.markers.push(model::Marker {
                id,
                time: Time::from_flicks(marker.time_flicks),
                name: marker.name,
            });
        }
        rebuilt
            .markers
            .sort_by_key(|marker| (marker.time, marker.id));
        let in_out_valid = [rebuilt.in_point, rebuilt.out_point]
            .into_iter()
            .flatten()
            .all(|point| point >= Time::ZERO)
            && match (rebuilt.in_point, rebuilt.out_point) {
                (Some(start), Some(end)) => start < end,
                _ => true,
            };
        if !in_out_valid {
            return Err(ValidationError::InOutPoints);
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

fn check_links(project: &model::Project) -> Result<(), ValidationError> {
    let mut groups: BTreeMap<LinkId, Vec<(usize, model::Clip)>> = BTreeMap::new();
    for (track, clip) in project
        .timeline
        .tracks
        .iter()
        .enumerate()
        .flat_map(|(index, track)| track.clips().iter().map(move |clip| (index, *clip)))
    {
        if let Some(link) = clip.link {
            if !project.next_ids.has_issued_link(link) {
                return Err(ValidationError::UnissuedLink {
                    clip: clip.id,
                    link,
                    next: project.next_ids.link,
                });
            }
            groups.entry(link).or_default().push((track, clip));
        }
    }
    for (link, members) in groups {
        let [(first_track, first), rest @ ..] = members.as_slice() else {
            continue;
        };
        if rest.is_empty() {
            return Err(ValidationError::LoneLink(link));
        }
        let mut tracks = HashSet::from([*first_track]);
        for (track, member) in rest {
            if !tracks.insert(*track) {
                return Err(ValidationError::LinkedOnOneTrack {
                    link,
                    track: *track,
                });
            }
            let in_step = member.asset == first.asset
                && member.start == first.start
                && member.source == first.source;
            if !in_step {
                return Err(ValidationError::LinkOutOfStep(link));
            }
        }
    }
    Ok(())
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
                .frame_rate()
                .ok_or(ValidationError::SequenceFrameRate {
                    numerator,
                    denominator,
                })?;
        let sample_rate =
            NonZeroU32::new(settings.sample_rate).ok_or(ValidationError::ZeroSampleRate)?;
        let (width, height) = NonZeroU32::new(settings.width)
            .zip(NonZeroU32::new(settings.height))
            .ok_or(ValidationError::SequenceSize {
                width: settings.width,
                height: settings.height,
            })?;
        Ok(Self {
            width,
            height,
            frame_rate,
            sample_rate,
        })
    }
}

impl Rational {
    fn frame_rate(self) -> Option<FrameRate> {
        FrameRate::new(self.numerator, self.denominator)
    }
}

impl TryFrom<Asset> for model::Asset {
    type Error = ValidationError;

    fn try_from(asset: Asset) -> Result<Self, ValidationError> {
        let id = AssetId(asset.id);
        if let Some(flicks) = asset.duration_flicks.filter(|&flicks| flicks <= 0) {
            return Err(ValidationError::NonPositiveDuration { asset: id, flicks });
        }
        let mut indices = HashSet::new();
        if let Some(repeated) = asset
            .streams
            .iter()
            .map(Stream::index)
            .find(|&index| !indices.insert(index))
        {
            return Err(ValidationError::DuplicateStream {
                asset: id,
                stream: repeated,
            });
        }
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
    fn index(&self) -> usize {
        match *self {
            Self::Video { index, .. } | Self::Audio { index, .. } => index,
        }
    }

    fn rebuilt(self, asset: AssetId) -> Result<model::Stream, ValidationError> {
        Ok(match self {
            Self::Video {
                index,
                codec,
                width,
                height,
                frame_rate,
            } => {
                let (nonzero_width, nonzero_height) = NonZeroU32::new(width)
                    .zip(NonZeroU32::new(height))
                    .ok_or(ValidationError::StreamSize {
                        asset,
                        stream: index,
                        width,
                        height,
                    })?;
                model::Stream::Video(model::VideoStream {
                    index,
                    codec,
                    width: nonzero_width,
                    height: nonzero_height,
                    frame_rate: frame_rate
                        .map(|rate| {
                            rate.frame_rate().ok_or(ValidationError::StreamFrameRate {
                                asset,
                                stream: index,
                                numerator: rate.numerator,
                                denominator: rate.denominator,
                            })
                        })
                        .transpose()?,
                })
            }
            Self::Audio {
                index,
                codec,
                sample_rate,
                channels,
            } => model::Stream::Audio(model::AudioStream {
                index,
                codec,
                sample_rate: NonZeroU32::new(sample_rate).ok_or(
                    ValidationError::ZeroStreamSampleRate {
                        asset,
                        stream: index,
                    },
                )?,
                channels: NonZeroU16::new(channels).ok_or(ValidationError::NoChannels {
                    asset,
                    stream: index,
                })?,
            }),
        })
    }
}

impl Track {
    fn rebuilt(self, project: &model::Project) -> Result<model::Track, ValidationError> {
        let mut track = model::Track::new(self.kind.into());
        track.name = self.name;
        track.locked = self.locked;
        track.muted = self.muted;
        track.solo = self.solo;
        track.height = self.height.into();
        for clip in self.clips {
            let clip = clip.rebuilt(project, track.kind)?;
            track.insert(clip).map_err(|refused| match refused {
                InsertError::Invalid(invalid) => ValidationError::InvalidClip(invalid),
                InsertError::Overlapping(source) => ValidationError::Overlapping {
                    clip: clip.id,
                    source,
                },
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
            link: self.link.map(LinkId),
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
        clip.check()?;
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
    use tessera_timeline::{
        InvalidClip,
        TrackKind::{Audio, Video},
    };

    use super::*;

    const SECOND: i64 = tessera_timeline::FLICKS_PER_SECOND;

    fn clip(id: u64, asset: u64, start: i64, source_start: i64, duration: i64) -> Value {
        json!({
            "id": id,
            "asset": asset,
            "start_flicks": start * SECOND,
            "source_start_flicks": source_start * SECOND,
            "source_duration_flicks": duration * SECOND,
            "link": null,
        })
    }

    fn track(kind: &str, name: &str, clips: Vec<Value>) -> Value {
        json!({
            "kind": kind,
            "name": name,
            "locked": false,
            "muted": false,
            "solo": false,
            "height": "normal",
            "clips": clips,
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
            "next_marker_id": 5,
            "next_link_id": 2,
            "markers": [
                { "id": 4, "time_flicks": 6 * SECOND, "name": "later" },
                { "id": 1, "time_flicks": 2 * SECOND, "name": "earlier" },
            ],
            "in_point_flicks": SECOND,
            "out_point_flicks": 9 * SECOND,
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
                track("video", "Main", vec![clip(0, 4, 0, 0, 4), clip(1, 4, 4, 4, 6)]),
                track("audio", "", vec![clip(2, 7, 0, 0, 30)]),
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
                clip: ClipId(3),
                marker: MarkerId(5),
                link: LinkId(2),
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
    fn sizes_sample_rates_and_channel_counts_must_not_be_zero() {
        assert_eq!(
            refused(|document| document["settings"]["width"] = json!(0)),
            ValidationError::SequenceSize {
                width: 0,
                height: 1080
            }
        );
        assert_eq!(
            refused(|document| document["settings"]["height"] = json!(0)),
            ValidationError::SequenceSize {
                width: 1920,
                height: 0
            }
        );
        assert_eq!(
            refused(|document| document["assets"][0]["streams"][0]["width"] = json!(0)),
            ValidationError::StreamSize {
                asset: AssetId(4),
                stream: 0,
                width: 0,
                height: 1080
            }
        );
        assert_eq!(
            refused(|document| document["assets"][0]["streams"][0]["height"] = json!(0)),
            ValidationError::StreamSize {
                asset: AssetId(4),
                stream: 0,
                width: 1920,
                height: 0
            }
        );
        assert_eq!(
            refused(|document| document["assets"][1]["streams"][0]["sample_rate"] = json!(0)),
            ValidationError::ZeroStreamSampleRate {
                asset: AssetId(7),
                stream: 0
            }
        );
        assert_eq!(
            refused(|document| document["assets"][1]["streams"][0]["channels"] = json!(0)),
            ValidationError::NoChannels {
                asset: AssetId(7),
                stream: 0
            }
        );
    }

    #[test]
    fn asset_durations_must_be_positive() {
        assert_eq!(
            refused(|document| document["assets"][0]["duration_flicks"] = json!(-SECOND)),
            ValidationError::NonPositiveDuration {
                asset: AssetId(4),
                flicks: -SECOND
            }
        );
        assert_eq!(
            refused(|document| document["assets"][1]["duration_flicks"] = json!(0)),
            ValidationError::NonPositiveDuration {
                asset: AssetId(7),
                flicks: 0
            }
        );
    }

    #[test]
    fn an_asset_lists_each_stream_index_once() {
        assert_eq!(
            refused(|document| {
                let audio = document["assets"][1]["streams"][0].clone();
                document["assets"][0]["streams"]
                    .as_array_mut()
                    .unwrap()
                    .push(audio);
            }),
            ValidationError::DuplicateStream {
                asset: AssetId(4),
                stream: 0
            }
        );

        let mut document = valid();
        let mut audio = document["assets"][1]["streams"][0].clone();
        audio["index"] = json!(1);
        document["assets"][0]["streams"]
            .as_array_mut()
            .unwrap()
            .push(audio);

        assert!(rebuilt(document).is_ok());
    }

    #[test]
    fn frame_rates_may_be_arbitrary_up_to_a_frame_per_flick() {
        let mut document = valid();
        document["settings"]["frame_rate"] = json!({ "numerator": 44, "denominator": 1 });
        document["assets"][0]["streams"][0]["frame_rate"] =
            json!({ "numerator": 60, "denominator": 2 });

        let project = rebuilt(document).unwrap();
        let stream = project
            .asset(AssetId(4))
            .unwrap()
            .info
            .video()
            .next()
            .unwrap();

        assert_eq!(project.settings.frame_rate, FrameRate::new(44, 1).unwrap());
        assert_eq!(stream.frame_rate, Some(FrameRate::FPS_30));
        assert_eq!(
            refused(|document| {
                document["settings"]["frame_rate"] =
                    json!({ "numerator": 705_600_001, "denominator": 1 });
            }),
            ValidationError::SequenceFrameRate {
                numerator: 705_600_001,
                denominator: 1
            }
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
    fn linked_clips_must_be_issued_paired_on_separate_tracks_and_in_step() {
        let link = |track: usize, clip: usize, link: u64| {
            move |document: &mut Value| {
                document["tracks"][track]["clips"][clip]["link"] = json!(link);
            }
        };

        assert_eq!(
            refused(link(0, 0, 2)),
            ValidationError::UnissuedLink {
                clip: ClipId(0),
                link: LinkId(2),
                next: LinkId(2)
            }
        );
        assert_eq!(refused(link(0, 0, 1)), ValidationError::LoneLink(LinkId(1)));
        assert_eq!(
            refused(|document| {
                link(0, 0, 1)(document);
                link(0, 1, 1)(document);
            }),
            ValidationError::LinkedOnOneTrack {
                link: LinkId(1),
                track: 0
            }
        );
        assert_eq!(
            refused(|document| {
                link(0, 0, 0)(document);
                link(1, 0, 0)(document);
            }),
            ValidationError::LinkOutOfStep(LinkId(0))
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
            ValidationError::InvalidClip(InvalidClip::Empty(ClipId(1)))
        );
        assert_eq!(
            refused(|document| document["tracks"][0]["clips"][0] = clip(0, 4, -1, 0, 1)),
            ValidationError::InvalidClip(InvalidClip::NegativeTime(ClipId(0)))
        );
        assert_eq!(
            refused(|document| document["tracks"][0]["clips"][0] = clip(0, 4, 0, -1, 4)),
            ValidationError::InvalidClip(InvalidClip::NegativeTime(ClipId(0)))
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
    fn clip_times_must_not_overflow_when_summed() {
        assert_eq!(
            refused(|document| {
                document["tracks"][1]["clips"][0]["start_flicks"] = json!(i64::MAX - SECOND);
            }),
            ValidationError::InvalidClip(InvalidClip::EndOverflows(ClipId(2)))
        );
        assert_eq!(
            refused(|document| {
                document["tracks"][1]["clips"][0]["source_start_flicks"] = json!(i64::MAX - SECOND);
            }),
            ValidationError::InvalidClip(InvalidClip::EndOverflows(ClipId(2)))
        );
        assert_eq!(
            refused(|document| {
                document["tracks"][0]["clips"][1]["source_start_flicks"] = json!(i64::MAX - SECOND);
            }),
            ValidationError::InvalidClip(InvalidClip::EndOverflows(ClipId(1)))
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

    #[test]
    fn markers_and_in_out_points_are_rebuilt_in_order() {
        let project = rebuilt(valid()).unwrap();

        assert_eq!(
            project
                .markers
                .iter()
                .map(|marker| (marker.id, marker.name.as_str()))
                .collect::<Vec<_>>(),
            [(MarkerId(1), "earlier"), (MarkerId(4), "later")]
        );
        assert_eq!(project.in_point, Some(Time::from_seconds(1)));
        assert_eq!(project.out_point, Some(Time::from_seconds(9)));
        assert_eq!(project.timeline.tracks[0].name, "Main");
    }

    #[test]
    fn markers_need_unique_issued_ids_and_times_from_zero() {
        assert_eq!(
            refused(|document| document["markers"][1]["id"] = json!(4)),
            ValidationError::DuplicateMarker(MarkerId(4))
        );
        assert_eq!(
            refused(|document| document["next_marker_id"] = json!(4)),
            ValidationError::UnissuedMarker {
                marker: MarkerId(4),
                next: MarkerId(4)
            }
        );
        assert_eq!(
            refused(|document| document["markers"][0]["time_flicks"] = json!(-1)),
            ValidationError::NegativeMarker {
                marker: MarkerId(4),
                flicks: -1
            }
        );
    }

    #[test]
    fn the_in_point_must_come_before_the_out_point_and_neither_before_zero() {
        for (start, end) in [(9, 9), (9, 1)] {
            assert_eq!(
                refused(|document| {
                    document["in_point_flicks"] = json!(start * SECOND);
                    document["out_point_flicks"] = json!(end * SECOND);
                }),
                ValidationError::InOutPoints
            );
        }
        assert_eq!(
            refused(|document| document["in_point_flicks"] = json!(-1)),
            ValidationError::InOutPoints
        );
        assert!(
            rebuilt({
                let mut document = valid();
                document["in_point_flicks"] = Value::Null;
                document["out_point_flicks"] = json!(SECOND);
                document
            })
            .is_ok()
        );
    }
}
