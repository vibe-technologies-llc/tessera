use std::{io, path::PathBuf};

use serde_json::Value;
use tessera_timeline::{AssetId, ClipId, InvalidClip, MarkerId, OverlappingClip, TrackKind};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("could not read {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("could not write {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("could not write {path}: it passes through too many symbolic links")]
    SymlinkLoop { path: PathBuf },
    #[error("{path}: {source}")]
    Format {
        path: PathBuf,
        #[source]
        source: FormatError,
    },
}

#[derive(Debug, Error)]
pub enum FormatError {
    #[error("the file is not valid JSON: {0}")]
    Syntax(#[source] serde_json::Error),
    #[error("the file is not a Tessera project")]
    NotAProject,
    #[error("the project file has no format version")]
    MissingVersion,
    #[error("{0} is not a project format version")]
    InvalidVersion(Value),
    #[error(
        "the project uses format version {version}, but this version of Tessera reads only up to {supported}"
    )]
    NewerVersion { version: u64, supported: u64 },
    #[error("the project file holds no project")]
    MissingProject,
    #[error("could not migrate the project from format version {from}: {reason}")]
    Migration { from: u64, reason: String },
    #[error("the project does not match format version {version}: {source}")]
    Shape {
        version: u64,
        #[source]
        source: serde_json::Error,
    },
    #[error("the project is inconsistent: {0}")]
    Invalid(#[from] ValidationError),
    #[error("the media path {0} is not valid UTF-8, so it cannot be saved")]
    NonUnicodePath(PathBuf),
    #[error("could not encode the project: {0}")]
    Encode(#[source] serde_json::Error),
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum ValidationError {
    #[error(
        "the sequence frame rate {numerator}/{denominator} is not a positive rate of at most one frame per flick"
    )]
    SequenceFrameRate { numerator: u32, denominator: u32 },
    #[error(
        "stream {stream} of asset {} has the frame rate {numerator}/{denominator}, which is not a positive rate of at most one frame per flick",
        .asset.0
    )]
    StreamFrameRate {
        asset: AssetId,
        stream: usize,
        numerator: u32,
        denominator: u32,
    },
    #[error("the sequence sample rate is zero")]
    ZeroSampleRate,
    #[error("the sequence size {width}×{height} has a side of zero")]
    SequenceSize { width: u32, height: u32 },
    #[error("stream {stream} of asset {} has the size {width}×{height}, which has a side of zero", .asset.0)]
    StreamSize {
        asset: AssetId,
        stream: usize,
        width: u32,
        height: u32,
    },
    #[error("stream {stream} of asset {} has a sample rate of zero", .asset.0)]
    ZeroStreamSampleRate { asset: AssetId, stream: usize },
    #[error("stream {stream} of asset {} has no channels", .asset.0)]
    NoChannels { asset: AssetId, stream: usize },
    #[error("asset {} lasts {flicks} flicks, which is not a positive duration", .asset.0)]
    NonPositiveDuration { asset: AssetId, flicks: i64 },
    #[error("asset {} lists stream {stream} more than once", .asset.0)]
    DuplicateStream { asset: AssetId, stream: usize },
    #[error("asset {} appears more than once", .0.0)]
    DuplicateAsset(AssetId),
    #[error("clip {} appears more than once", .0.0)]
    DuplicateClip(ClipId),
    #[error("asset {} is not below the next asset id {}", .asset.0, .next.0)]
    UnissuedAsset { asset: AssetId, next: AssetId },
    #[error("clip {} is not below the next clip id {}", .clip.0, .next.0)]
    UnissuedClip { clip: ClipId, next: ClipId },
    #[error("clip {} uses asset {}, which the project does not hold", .clip.0, .asset.0)]
    UnknownAsset { clip: ClipId, asset: AssetId },
    #[error("clip {} sits on a {kind:?} track, but asset {} has no {kind:?} stream", .clip.0, .asset.0)]
    MissingStream {
        clip: ClipId,
        asset: AssetId,
        kind: TrackKind,
    },
    #[error(transparent)]
    InvalidClip(#[from] InvalidClip),
    #[error("clip {} reaches past the end of asset {}", .clip.0, .asset.0)]
    BeyondMedia { clip: ClipId, asset: AssetId },
    #[error("clip {}: {source}", .clip.0)]
    Overlapping {
        clip: ClipId,
        #[source]
        source: OverlappingClip,
    },
    #[error("marker {} appears more than once", .0.0)]
    DuplicateMarker(MarkerId),
    #[error("marker {} is not below the next marker id {}", .marker.0, .next.0)]
    UnissuedMarker { marker: MarkerId, next: MarkerId },
    #[error("marker {} sits at {flicks} flicks, before the start of the timeline", .marker.0)]
    NegativeMarker { marker: MarkerId, flicks: i64 },
    #[error("the in and out points are before zero or not in order")]
    InOutPoints,
    #[error("the timeline has no {0:?} track")]
    NoTrack(TrackKind),
}
