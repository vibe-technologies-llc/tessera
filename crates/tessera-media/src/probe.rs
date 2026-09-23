use std::path::Path;

use ffmpeg_next::{codec, ffi::AV_TIME_BASE, format, media};
use tessera_timeline::{FrameRate, Time};

use crate::Error;

#[derive(Clone, Debug, PartialEq)]
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

#[derive(Clone, Debug, PartialEq)]
pub enum Stream {
    Video(VideoStream),
    Audio(AudioStream),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VideoStream {
    pub index: usize,
    pub codec: &'static str,
    pub width: u32,
    pub height: u32,
    pub frame_rate: Option<FrameRate>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioStream {
    pub index: usize,
    pub codec: &'static str,
    pub sample_rate: u32,
    pub channels: u16,
}

pub fn probe(path: impl AsRef<Path>) -> Result<MediaInfo, Error> {
    let path = path.as_ref();
    let input = format::input(path).map_err(|source| Error::Open {
        path: path.to_owned(),
        source,
    })?;
    let duration = (input.duration() > 0)
        .then(|| Time::from_rational(input.duration(), i64::from(AV_TIME_BASE)));
    let streams = input
        .streams()
        .filter_map(|stream| describe(&stream).transpose())
        .collect::<Result<_, _>>()?;
    Ok(MediaInfo { duration, streams })
}

fn describe(stream: &format::stream::Stream) -> Result<Option<Stream>, Error> {
    let index = stream.index();
    let parameters = stream.parameters();
    let medium = parameters.medium();
    let codec = parameters.id().name();
    let decoder = codec::Context::from_parameters(parameters)
        .map_err(|source| Error::Stream { index, source })?
        .decoder();
    let described = match medium {
        media::Type::Video => {
            let video = decoder
                .video()
                .map_err(|source| Error::Stream { index, source })?;
            Some(Stream::Video(VideoStream {
                index,
                codec,
                width: video.width(),
                height: video.height(),
                frame_rate: frame_rate(stream.avg_frame_rate()),
            }))
        }
        media::Type::Audio => {
            let audio = decoder
                .audio()
                .map_err(|source| Error::Stream { index, source })?;
            Some(Stream::Audio(AudioStream {
                index,
                codec,
                sample_rate: audio.rate(),
                channels: audio.channels(),
            }))
        }
        _ => None,
    };
    Ok(described)
}

fn frame_rate(rate: ffmpeg_next::Rational) -> Option<FrameRate> {
    let numerator = u32::try_from(rate.numerator()).ok().filter(|&n| n > 0)?;
    let denominator = u32::try_from(rate.denominator()).ok().filter(|&d| d > 0)?;
    Some(FrameRate::new(numerator, denominator))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_is_an_open_error() {
        crate::init().unwrap();
        let error = probe("/nonexistent/tessera/clip.mkv").unwrap_err();
        assert!(matches!(error, Error::Open { .. }), "{error}");
    }

    #[test]
    fn invalid_rates_are_rejected() {
        assert_eq!(frame_rate(ffmpeg_next::Rational::new(0, 1)), None);
        assert_eq!(frame_rate(ffmpeg_next::Rational::new(30, 0)), None);
        assert_eq!(
            frame_rate(ffmpeg_next::Rational::new(30_000, 1_001)),
            Some(FrameRate::NTSC_30)
        );
    }
}
