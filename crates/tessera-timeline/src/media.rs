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

    pub fn default_video(&self) -> Option<&VideoStream> {
        self.video()
            .find(|video| video.default)
            .or_else(|| self.video().next())
    }

    pub fn default_audio(&self) -> Option<&AudioStream> {
        self.audio()
            .find(|audio| audio.default)
            .or_else(|| self.audio().next())
    }

    pub fn audio_stream(&self, index: usize) -> Option<&AudioStream> {
        self.audio().find(|audio| audio.index == index)
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
    pub rotation: Rotation,
    pub pixel_aspect: PixelAspect,
    pub pixel_format: Option<String>,
    pub default: bool,
    pub start: Time,
}

impl VideoStream {
    pub fn new(
        index: usize,
        codec: impl Into<String>,
        width: NonZeroU32,
        height: NonZeroU32,
    ) -> Self {
        Self {
            index,
            codec: codec.into(),
            width,
            height,
            frame_rate: None,
            rotation: Rotation::Upright,
            pixel_aspect: PixelAspect::SQUARE,
            pixel_format: None,
            default: false,
            start: Time::ZERO,
        }
    }

    pub fn display_size(&self) -> (NonZeroU32, NonZeroU32) {
        let stretched = self.pixel_aspect.stretched_width(self.width);
        if self.rotation.swaps_sides() {
            (self.height, stretched)
        } else {
            (stretched, self.height)
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioStream {
    pub index: usize,
    pub codec: String,
    pub sample_rate: NonZeroU32,
    pub channels: NonZeroU16,
    pub default: bool,
    pub start: Time,
}

impl AudioStream {
    pub fn new(
        index: usize,
        codec: impl Into<String>,
        sample_rate: NonZeroU32,
        channels: NonZeroU16,
    ) -> Self {
        Self {
            index,
            codec: codec.into(),
            sample_rate,
            channels,
            default: false,
            start: Time::ZERO,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Rotation {
    #[default]
    Upright,
    Clockwise,
    UpsideDown,
    Counterclockwise,
}

impl Rotation {
    pub const ALL: [Self; 4] = [
        Self::Upright,
        Self::Clockwise,
        Self::UpsideDown,
        Self::Counterclockwise,
    ];

    pub fn from_degrees(degrees: u16) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|rotation| rotation.degrees() == degrees)
    }

    pub fn nearest(clockwise_degrees: f64) -> Self {
        let quarters = (clockwise_degrees / 90.0).round().rem_euclid(4.0) as usize;
        Self::ALL[quarters % 4]
    }

    pub fn degrees(self) -> u16 {
        match self {
            Self::Upright => 0,
            Self::Clockwise => 90,
            Self::UpsideDown => 180,
            Self::Counterclockwise => 270,
        }
    }

    pub fn swaps_sides(self) -> bool {
        matches!(self, Self::Clockwise | Self::Counterclockwise)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PixelAspect {
    numerator: NonZeroU32,
    denominator: NonZeroU32,
}

impl PixelAspect {
    pub const SQUARE: Self = Self {
        numerator: NonZeroU32::MIN,
        denominator: NonZeroU32::MIN,
    };

    pub fn new(numerator: u32, denominator: u32) -> Option<Self> {
        let (numerator, denominator) = (NonZeroU32::new(numerator)?, NonZeroU32::new(denominator)?);
        let divisor = gcd(numerator.get(), denominator.get());
        Some(Self {
            numerator: NonZeroU32::new(numerator.get() / divisor)?,
            denominator: NonZeroU32::new(denominator.get() / divisor)?,
        })
    }

    pub fn numerator(self) -> u32 {
        self.numerator.get()
    }

    pub fn denominator(self) -> u32 {
        self.denominator.get()
    }

    pub fn is_square(self) -> bool {
        self == Self::SQUARE
    }

    pub fn stretched_width(self, width: NonZeroU32) -> NonZeroU32 {
        let numerator = u64::from(self.numerator.get());
        let denominator = u64::from(self.denominator.get());
        let stretched = (u64::from(width.get()) * numerator + denominator / 2) / denominator;
        NonZeroU32::new(u32::try_from(stretched).unwrap_or(u32::MAX)).unwrap_or(NonZeroU32::MIN)
    }
}

impl Default for PixelAspect {
    fn default() -> Self {
        Self::SQUARE
    }
}

fn gcd(mut a: u32, mut b: u32) -> u32 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
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
                Stream::Audio(AudioStream::new(
                    0,
                    "opus",
                    NonZero::new(48_000).unwrap(),
                    NonZero::new(2).unwrap(),
                )),
                Stream::Video(VideoStream {
                    frame_rate: Some(FrameRate::FPS_24),
                    ..VideoStream::new(
                        1,
                        "av1",
                        NonZero::new(1920).unwrap(),
                        NonZero::new(1080).unwrap(),
                    )
                }),
            ],
        };
        let video: Vec<_> = info.video().map(|video| video.index).collect();
        let audio: Vec<_> = info.audio().map(|audio| audio.index).collect();
        assert_eq!(video, [1]);
        assert_eq!(audio, [0]);
    }

    fn stereo(index: usize, default: bool) -> Stream {
        Stream::Audio(AudioStream {
            default,
            ..AudioStream::new(
                index,
                "aac",
                NonZero::new(48_000).unwrap(),
                NonZero::new(2).unwrap(),
            )
        })
    }

    #[test]
    fn the_default_stream_is_the_one_marked_default_else_the_first() {
        let unmarked = MediaInfo {
            duration: None,
            streams: vec![stereo(1, false), stereo(2, false)],
        };
        let marked = MediaInfo {
            duration: None,
            streams: vec![stereo(1, false), stereo(2, true)],
        };

        assert_eq!(unmarked.default_audio().map(|audio| audio.index), Some(1));
        assert_eq!(marked.default_audio().map(|audio| audio.index), Some(2));
        assert_eq!(marked.audio_stream(1).map(|audio| audio.index), Some(1));
        assert_eq!(marked.audio_stream(3), None);
        assert!(marked.default_video().is_none());
    }

    #[test]
    fn rotations_snap_to_quarter_turns() {
        assert_eq!(Rotation::nearest(0.0), Rotation::Upright);
        assert_eq!(Rotation::nearest(89.7), Rotation::Clockwise);
        assert_eq!(Rotation::nearest(-90.0), Rotation::Counterclockwise);
        assert_eq!(Rotation::nearest(540.0), Rotation::UpsideDown);
        assert_eq!(
            Rotation::from_degrees(270),
            Some(Rotation::Counterclockwise)
        );
        assert_eq!(Rotation::from_degrees(45), None);
    }

    #[test]
    fn the_display_size_stretches_by_the_pixel_aspect_and_turns_with_the_rotation() {
        let dv = VideoStream {
            pixel_aspect: PixelAspect::new(32, 27).unwrap(),
            ..VideoStream::new(
                0,
                "dvvideo",
                NonZero::new(720).unwrap(),
                NonZero::new(480).unwrap(),
            )
        };
        let phone = VideoStream {
            rotation: Rotation::Clockwise,
            ..VideoStream::new(
                0,
                "hevc",
                NonZero::new(1920).unwrap(),
                NonZero::new(1080).unwrap(),
            )
        };

        assert_eq!(
            dv.display_size(),
            (NonZero::new(853).unwrap(), NonZero::new(480).unwrap())
        );
        assert_eq!(
            phone.display_size(),
            (NonZero::new(1080).unwrap(), NonZero::new(1920).unwrap())
        );
        assert_eq!(PixelAspect::new(64, 54), PixelAspect::new(32, 27));
        assert!(PixelAspect::new(10, 10).unwrap().is_square());
        assert_eq!(PixelAspect::new(0, 1), None);
    }
}
