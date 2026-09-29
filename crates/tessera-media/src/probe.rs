use std::{num::NonZeroI64, path::Path};

use ffmpeg_next::{codec, ffi::AV_TIME_BASE, format, media};
use tessera_timeline::{AudioStream, FrameRate, MediaInfo, Stream, Time, VideoStream};

use crate::Error;

const CONTAINER_TIME_BASE: NonZeroI64 =
    NonZeroI64::new(AV_TIME_BASE as i64).expect("FFmpeg's time base is not zero");

pub fn probe(path: impl AsRef<Path>) -> Result<MediaInfo, Error> {
    let path = path.as_ref();
    let input = format::input(path).map_err(|source| Error::Open {
        path: path.to_owned(),
        source,
    })?;
    let duration =
        (input.duration() > 0).then(|| Time::from_rational(input.duration(), CONTAINER_TIME_BASE));
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
    let codec = parameters.id().name().to_owned();
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
    FrameRate::new(
        u32::try_from(rate.numerator()).ok()?,
        u32::try_from(rate.denominator()).ok()?,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::{self, Fixture};

    #[test]
    fn fixture_has_one_video_and_one_audio_stream() {
        let fixture = Fixture::generate("probe_streams");
        let info = probe(fixture.path()).unwrap();
        let video: Vec<_> = info.video().collect();
        let audio: Vec<_> = info.audio().collect();
        assert_eq!(video.len(), 1);
        assert_eq!(
            (video[0].width, video[0].height),
            (fixture::WIDTH, fixture::HEIGHT)
        );
        assert_eq!(audio.len(), 1);
        assert_eq!(audio[0].codec, "pcm_s16le");
        assert_eq!(
            (audio[0].sample_rate, audio[0].channels),
            (fixture::AUDIO_SAMPLE_RATE.get(), fixture::AUDIO_CHANNELS)
        );
    }

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
