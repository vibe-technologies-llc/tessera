mod hw;
mod probe;

pub use ffmpeg_next::Error as FfmpegError;
pub use hw::{HwAccel, available_hw_accels};
pub use probe::{AudioStream, MediaInfo, Stream, VideoStream, probe};
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
}

pub fn init() -> Result<(), Error> {
    ffmpeg_next::init().map_err(Error::Init)
}
