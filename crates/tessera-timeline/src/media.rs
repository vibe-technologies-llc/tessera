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
    pub width: u32,
    pub height: u32,
    pub frame_rate: Option<FrameRate>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioStream {
    pub index: usize,
    pub codec: String,
    pub sample_rate: u32,
    pub channels: u16,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streams_are_split_by_kind() {
        let info = MediaInfo {
            duration: Some(Time::from_seconds(3)),
            streams: vec![
                Stream::Audio(AudioStream {
                    index: 0,
                    codec: "opus".into(),
                    sample_rate: 48_000,
                    channels: 2,
                }),
                Stream::Video(VideoStream {
                    index: 1,
                    codec: "av1".into(),
                    width: 1920,
                    height: 1080,
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
