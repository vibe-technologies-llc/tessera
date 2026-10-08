use std::{
    num::{NonZeroI64, NonZeroU16, NonZeroU32},
    path::Path,
    ptr,
};

use ffmpeg_next::{
    codec,
    ffi::{
        AV_NOPTS_VALUE, AV_TIME_BASE, AVPacketSideDataType, av_display_rotation_get,
        av_guess_sample_aspect_ratio, av_packet_side_data_get,
    },
    format::{self, stream::Disposition},
    media,
};
use tessera_timeline::{
    AudioStream, FrameRate, MediaInfo, PixelAspect, Rotation, Stream, Time, VideoStream,
};

use crate::{Error, decode::stream_start};

const DISPLAY_MATRIX_BYTES: usize = 9 * size_of::<i32>();

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
        .filter_map(|stream| match describe(&input, &stream) {
            Ok(described) => described,
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "skipping a stream that cannot be decoded");
                None
            }
        })
        .collect();
    Ok(MediaInfo { duration, streams })
}

fn describe(
    input: &format::context::Input,
    stream: &format::stream::Stream,
) -> Result<Option<Stream>, Error> {
    let index = stream.index();
    let default = stream.disposition().contains(Disposition::DEFAULT);
    let start = start_offset(input, stream);
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
            let pixel_format = video
                .format()
                .descriptor()
                .map(|descriptor| descriptor.name().to_owned());
            size.map(|(width, height)| {
                Stream::Video(VideoStream {
                    frame_rate: frame_rate(stream.avg_frame_rate()),
                    rotation: rotation(stream),
                    pixel_aspect: pixel_aspect(input, stream),
                    pixel_format,
                    default,
                    start,
                    ..VideoStream::new(index, codec, width, height)
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
                    default,
                    start,
                    ..AudioStream::new(index, codec, sample_rate, channels)
                })
            })
        }
        _ => None,
    };
    Ok(described)
}

fn start_offset(input: &format::context::Input, stream: &format::stream::Stream) -> Time {
    let Some(time_base) = NonZeroI64::new(i64::from(stream.time_base().denominator()))
        .filter(|_| stream.time_base().numerator() > 0)
    else {
        return Time::ZERO;
    };
    let own = stream.start_time();
    if own == AV_NOPTS_VALUE {
        return Time::ZERO;
    }
    let offset = own - stream_start(input, stream);
    Time::from_rational(
        offset.saturating_mul(i64::from(stream.time_base().numerator())),
        time_base,
    )
}

pub(crate) fn rotation(stream: &format::stream::Stream) -> Rotation {
    let matrix = unsafe {
        let parameters = (*stream.as_ptr()).codecpar;
        av_packet_side_data_get(
            (*parameters).coded_side_data,
            (*parameters).nb_coded_side_data,
            AVPacketSideDataType::AV_PKT_DATA_DISPLAYMATRIX,
        )
        .as_ref()
    };
    match matrix {
        Some(side_data) if side_data.size >= DISPLAY_MATRIX_BYTES => {
            let counterclockwise = unsafe { av_display_rotation_get(side_data.data.cast()) };
            if counterclockwise.is_finite() {
                Rotation::nearest(-counterclockwise)
            } else {
                Rotation::Upright
            }
        }
        _ => Rotation::Upright,
    }
}

pub(crate) fn pixel_aspect(
    input: &format::context::Input,
    stream: &format::stream::Stream,
) -> PixelAspect {
    let guessed = unsafe {
        av_guess_sample_aspect_ratio(
            input.as_ptr().cast_mut(),
            stream.as_ptr().cast_mut(),
            ptr::null_mut(),
        )
    };
    u32::try_from(guessed.num)
        .ok()
        .zip(u32::try_from(guessed.den).ok())
        .and_then(|(numerator, denominator)| PixelAspect::new(numerator, denominator))
        .unwrap_or(PixelAspect::SQUARE)
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
    fn rotation_pixel_aspect_format_and_disposition_are_read() {
        let plain = std::env::temp_dir().join(format!(
            "tessera-media-{}-probe-anamorphic.mp4",
            std::process::id()
        ));
        let rotated = std::env::temp_dir().join(format!(
            "tessera-media-{}-probe-rotated.mp4",
            std::process::id()
        ));
        let anamorphic = std::process::Command::new("ffmpeg")
            .args(["-loglevel", "error", "-y"])
            .args(["-f", "lavfi", "-i", "color=c=red:s=64x48:d=1"])
            .args(["-vf", "setsar=32/27", "-c:v", "mpeg4"])
            .arg(&plain)
            .status();
        let turned = std::process::Command::new("ffmpeg")
            .args(["-loglevel", "error", "-y", "-display_rotation", "90", "-i"])
            .arg(&plain)
            .args(["-c", "copy"])
            .arg(&rotated)
            .status();
        let made = [anamorphic, turned]
            .into_iter()
            .all(|status| status.is_ok_and(|status| status.success()));
        if !made {
            std::fs::remove_file(&plain).ok();
            return;
        }
        crate::init().unwrap();

        let info = probe(&rotated).unwrap();
        std::fs::remove_file(&plain).ok();
        std::fs::remove_file(&rotated).ok();

        let video = info.video().next().unwrap();
        assert_eq!(video.rotation, Rotation::Counterclockwise);
        assert_eq!(video.pixel_aspect, PixelAspect::new(32, 27).unwrap());
        assert_eq!(video.pixel_format.as_deref(), Some("yuv420p"));
        assert!(video.default);
        assert_eq!(video.start, Time::ZERO);
        assert_eq!(
            video.display_size(),
            (NonZeroU32::new(48).unwrap(), NonZeroU32::new(76).unwrap())
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

    #[test]
    fn several_audio_streams_are_all_listed() {
        let fixture = Fixture::generate_with_two_audio_streams("probe_two_audio_streams");
        let info = probe(fixture.path()).unwrap();
        let indices: Vec<_> = info.audio().map(|audio| audio.index).collect();
        assert_eq!(
            indices,
            [fixture::AUDIO_STREAM, fixture::SECOND_AUDIO_STREAM]
        );
    }

    #[test]
    fn audio_only_files_have_a_duration_and_no_video() {
        let fixture = Fixture::generate_audio_only("probe_audio_only");
        let info = probe(fixture.path()).unwrap();
        assert_eq!(info.video().count(), 0);
        assert_eq!(info.audio().count(), 1);
        assert!(info.duration.is_some_and(|duration| duration > Time::ZERO));
    }

    #[test]
    fn planar_stereo_is_probed_with_both_channels() {
        let fixture = Fixture::generate_planar_stereo("probe_planar_stereo");
        let info = probe(fixture.path()).unwrap();
        let audio = info.audio().next().unwrap();
        assert_eq!(audio.channels.get(), 2);
        assert_eq!(audio.codec, "pcm_s16le_planar");
    }

    #[test]
    fn a_still_image_has_a_video_stream_and_no_duration() {
        let fixture = Fixture::generate_still("probe_still");
        let info = probe(fixture.path()).unwrap();
        let video = info.video().next().unwrap();
        assert_eq!(video.index, fixture::STILL_STREAM);
        assert_eq!(
            (video.width.get(), video.height.get()),
            (fixture::WIDTH, fixture::HEIGHT)
        );
        assert_eq!(info.duration, None);
    }
}
