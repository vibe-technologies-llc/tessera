use std::{
    num::{NonZeroI64, NonZeroU16, NonZeroU32},
    path::Path,
};

use ffmpeg_next::{
    codec,
    ffi::AV_TIME_BASE,
    format::{self, stream::Disposition},
    media,
};
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
        .filter(|stream| !stream.disposition().contains(Disposition::ATTACHED_PIC))
        .filter_map(|stream| match describe(&stream) {
            Ok(described) => described,
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "skipping a stream that cannot be decoded");
                None
            }
        })
        .collect();
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
            let size = NonZeroU32::new(video.width()).zip(NonZeroU32::new(video.height()));
            size.map(|(width, height)| {
                Stream::Video(VideoStream {
                    index,
                    codec,
                    width,
                    height,
                    frame_rate: frame_rate(stream.avg_frame_rate()),
                })
            })
        }
        media::Type::Audio => {
            let audio = decoder
                .audio()
                .map_err(|source| Error::Stream { index, source })?;
            let layout = NonZeroU32::new(audio.rate()).zip(NonZeroU16::new(audio.channels()));
            layout.map(|(sample_rate, channels)| {
                Stream::Audio(AudioStream {
                    index,
                    codec,
                    sample_rate,
                    channels,
                })
            })
        }
        _ => None,
    };
    Ok(described)
}

fn frame_rate(rate: ffmpeg_next::Rational) -> Option<FrameRate> {
    FrameRate::nearest(
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
            (video[0].width.get(), video[0].height.get()),
            (fixture::WIDTH, fixture::HEIGHT)
        );
        assert_eq!(audio.len(), 1);
        assert_eq!(audio[0].codec, "pcm_s16le");
        assert_eq!(
            (audio[0].sample_rate.get(), audio[0].channels.get()),
            (fixture::AUDIO_SAMPLE_RATE.get(), fixture::AUDIO_CHANNELS)
        );
    }

    #[test]
    fn cover_art_is_not_listed_as_a_video_stream() {
        let path =
            std::env::temp_dir().join(format!("tessera-media-{}-cover.m4a", std::process::id()));
        let made = std::process::Command::new("ffmpeg")
            .args(["-loglevel", "error", "-y"])
            .args(["-f", "lavfi", "-i", "sine=frequency=440:duration=1"])
            .args(["-f", "lavfi", "-i", "color=c=red:s=64x64:d=1"])
            .args(["-frames:v", "1", "-map", "0:a", "-map", "1:v"])
            .args(["-c:a", "aac", "-c:v", "mjpeg"])
            .args(["-disposition:v", "attached_pic"])
            .arg(&path)
            .status();
        if !made.is_ok_and(|status| status.success()) {
            return;
        }
        crate::init().unwrap();

        let info = probe(&path).unwrap();
        std::fs::remove_file(&path).ok();

        assert_eq!(info.video().count(), 0);
        assert_eq!(info.audio().count(), 1);
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
        assert_eq!(frame_rate(ffmpeg_next::Rational::new(-30, 1)), None);
        assert_eq!(
            frame_rate(ffmpeg_next::Rational::new(30_000, 1_001)),
            Some(FrameRate::NTSC_30)
        );
    }

    #[test]
    fn average_rates_snap_to_a_standard_rate_close_by() {
        assert_eq!(
            frame_rate(ffmpeg_next::Rational::new(2_997, 100)),
            Some(FrameRate::NTSC_30)
        );
        assert_eq!(
            frame_rate(ffmpeg_next::Rational::new(44, 1)),
            FrameRate::new(44, 1)
        );
    }
}
