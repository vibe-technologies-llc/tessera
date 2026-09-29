mod audio;
mod cache;
mod decode;
#[cfg(test)]
mod fixture;
mod hw;
mod probe;

pub use audio::{AudioBuffer, AudioDecoder};
pub use decode::{VideoDecoder, VideoFrame};
pub use ffmpeg_next::Error as FfmpegError;
pub use hw::{HwAccel, PREFERRED_HW_ACCELS, available_hw_accels};
pub use probe::probe;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("failed to initialise FFmpeg: {0}")]
    Init(#[source] FfmpegError),
    #[error("failed to open {path}: {source}")]
    Open {
        path: std::path::PathBuf,
        #[source]
        source: FfmpegError,
    },
    #[error("failed to read stream {index}: {source}")]
    Stream {
        index: usize,
        #[source]
        source: FfmpegError,
    },
    #[error("{path} has no video stream")]
    NoVideo { path: std::path::PathBuf },
    #[error("{path} has no audio stream")]
    NoAudio { path: std::path::PathBuf },
    #[error("failed to seek in {path}: {source}")]
    Seek {
        path: std::path::PathBuf,
        #[source]
        source: FfmpegError,
    },
    #[error("failed to decode: {source}")]
    Decode {
        #[source]
        source: FfmpegError,
    },
    #[error("failed to resample audio: {source}")]
    Resample {
        #[source]
        source: FfmpegError,
    },
    #[error(
        "stream {index} of {path} has the time base {numerator}/{denominator}, which is not positive"
    )]
    InvalidTimeBase {
        path: std::path::PathBuf,
        index: usize,
        numerator: i32,
        denominator: i32,
    },
    #[error("no video frame could be decoded from {path}")]
    NoFrame { path: std::path::PathBuf },
}

pub fn init() -> Result<(), Error> {
    ffmpeg_next::init().map_err(Error::Init)
}
