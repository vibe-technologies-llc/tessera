use std::num::{NonZeroU16, NonZeroU32};

use crate::time::{FrameRate, Time};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MediaInfo {
    pub duration: Option<Time>,
    pub streams: Vec<Stream>,
}

impl MediaInfo {
    pub fn video(&self) -> impl Iterator<Item = &VideoStream> {
        self.streams.iter().filter_map(|stream| match stream {
            Stream::Video(video) => Some(video),
            Stream::Audio(_) => None,
        })
    }

    pub fn audio(&self) -> impl Iterator<Item = &AudioStream> {
        self.streams.iter().filter_map(|stream| match stream {
            Stream::Audio(audio) => Some(audio),
            Stream::Video(_) => None,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Stream {
    Video(VideoStream),
    Audio(AudioStream),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VideoStream {
    pub index: usize,
    pub codec: String,
    pub width: NonZeroU32,
    pub height: NonZeroU32,
    pub frame_rate: Option<FrameRate>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioStream {
    pub index: usize,
    pub codec: String,
    pub sample_rate: NonZeroU32,
    pub channels: NonZeroU16,
}

#[cfg(test)]
mod tests {
    use std::num::NonZero;

    use super::*;

    #[test]
    fn streams_are_split_by_kind() {
        let info = MediaInfo {
            duration: Some(Time::from_seconds(3)),
            streams: vec![
                Stream::Audio(AudioStream {
                    index: 0,
                    codec: "opus".into(),
                    sample_rate: NonZero::new(48_000).unwrap(),
                    channels: NonZero::new(2).unwrap(),
                }),
                Stream::Video(VideoStream {
                    index: 1,
                    codec: "av1".into(),
                    width: NonZero::new(1920).unwrap(),
                    height: NonZero::new(1080).unwrap(),
                    frame_rate: Some(FrameRate::FPS_24),
                }),
            ],
        };
        let video: Vec<_> = info.video().map(|video| video.index).collect();
        let audio: Vec<_> = info.audio().map(|audio| audio.index).collect();
        assert_eq!(video, [1]);
        assert_eq!(audio, [0]);
    }
}
