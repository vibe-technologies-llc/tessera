mod mix;
mod output;
mod playback;

pub use mix::Mixer;
pub use output::Output;
pub use playback::TimelinePlayback;
use thiserror::Error;

pub const CHANNELS: usize = tessera_media::AudioBuffer::CHANNELS;

#[derive(Debug, Error)]
pub enum Error {
    #[error("PipeWire is unavailable: {0}")]
    PipeWire(#[from] pipewire::Error),
    #[error("failed to describe the audio format: {0}")]
    Format(String),
    #[error("the audio output thread stopped before it was ready")]
    OutputThread,
}
