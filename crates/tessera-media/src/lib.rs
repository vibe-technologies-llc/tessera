mod audio;
mod cache;
mod decode;
mod encode;
#[cfg(any(test, feature = "fixtures"))]
pub mod fixture;
mod hw;
mod probe;

pub use audio::{AudioBuffer, AudioDecoder};
pub use decode::{VideoDecoder, VideoFrame};
pub use encode::{
    Container, EncodeBackend, EncodeSettings, Encoder, PREFERRED_ENCODE_BACKENDS, VideoCodec,
};
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
    #[error("{path} has no video stream at index {index}")]
    NoVideo {
        path: std::path::PathBuf,
        index: usize,
    },
    #[error("{path} has no audio stream at index {index}")]
    NoAudio {
        path: std::path::PathBuf,
        index: usize,
    },
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
    #[error("failed to create {path}: {source}")]
    CreateOutput {
        path: std::path::PathBuf,
        #[source]
        source: FfmpegError,
    },
    #[error("no {} encoder could be opened", codec.label())]
    NoVideoEncoder { codec: VideoCodec },
    #[error("no audio encoder for {} at {sample_rate} Hz could be opened", container.label())]
    NoAudioEncoder {
        container: Container,
        sample_rate: std::num::NonZeroU32,
    },
    #[error("no {accel:?} device is available for encoding")]
    HardwareDevice { accel: HwAccel },
    #[error("failed to upload a frame to the hardware encoder: {source}")]
    HardwareUpload {
        #[source]
        source: FfmpegError,
    },
    #[error("a frame of {actual} bytes was given where {expected} were expected")]
    FrameSize { expected: usize, actual: usize },
    #[error("failed to encode: {source}")]
    Encode {
        #[source]
        source: FfmpegError,
    },
    #[error("failed to write the output: {source}")]
    Mux {
        #[source]
        source: FfmpegError,
    },
}

pub fn init() -> Result<(), Error> {
    ffmpeg_next::init().map_err(Error::Init)
}
