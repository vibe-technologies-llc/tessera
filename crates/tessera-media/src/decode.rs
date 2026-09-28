use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use ffmpeg_next::{
    Rational, Rescale, codec, decoder,
    ffi::{AV_NOPTS_VALUE, AVSEEK_FLAG_BACKWARD, avformat_index_get_entry_from_timestamp},
    format::{self, Pixel},
    frame, media, rescale,
    software::scaling,
};
use tessera_timeline::{FLICKS_PER_SECOND, Time};

use crate::{Error, cache::FrameCache};

const OUTPUT_FORMAT: Pixel = Pixel::BGRA;
const BYTES_PER_PIXEL: usize = 4;
const DEFAULT_CACHE_BYTES: usize = 256 * 1024 * 1024;
const UNINDEXED_FORWARD_WINDOW: Time = Time::from_seconds(1);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VideoFrame {
    pub width: u32,
    pub height: u32,
    pub time: Time,
    pub bgra: Vec<u8>,
}

pub struct VideoDecoder {
    path: PathBuf,
    input: format::context::Input,
    stream_index: usize,
    time_base: Rational,
    start: i64,
    decoder: decoder::Video,
    converter: Converter,
    current: Option<frame::Video>,
    ahead: Option<frame::Video>,
    drained: bool,
    cache: FrameCache,
}

struct Converter {
    bounds: Option<(u32, u32)>,
    scaler: Option<Scaler>,
}

struct Scaler {
    context: scaling::Context,
    source: (Pixel, u32, u32),
}

unsafe impl Send for Scaler {}

impl VideoDecoder {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, Error> {
        let path = path.as_ref().to_owned();
        let input = format::input(&path).map_err(|source| Error::Open {
            path: path.clone(),
            source,
        })?;
        let stream = input
            .streams()
            .best(media::Type::Video)
            .ok_or_else(|| Error::NoVideo { path: path.clone() })?;
        let stream_index = stream.index();
        let time_base = stream.time_base();
        let start = match stream.start_time() {
            AV_NOPTS_VALUE => 0,
            start => start,
        };
        let decoder = codec::Context::from_parameters(stream.parameters())
            .and_then(|context| context.decoder().video())
            .map_err(|source| Error::Stream {
                index: stream_index,
                source,
            })?;
        Ok(Self {
            path,
            input,
            stream_index,
            time_base,
            start,
            decoder,
            converter: Converter {
                bounds: None,
                scaler: None,
            },
            current: None,
            ahead: None,
            drained: false,
            cache: FrameCache::new(DEFAULT_CACHE_BYTES),
        })
    }

    pub fn fit_within(mut self, width: u32, height: u32) -> Self {
        self.converter = Converter {
            bounds: Some((width.max(1), height.max(1))),
            scaler: None,
        };
        self.cache.clear();
        self
    }

    pub fn cache_capacity(mut self, bytes: usize) -> Self {
        self.cache = FrameCache::new(bytes);
        self
    }

    pub fn frame_at(&mut self, time: Time) -> Result<Arc<VideoFrame>, Error> {
        let target = self.stream_ts(time);
        if let Some(cached) = self.cache.get(target) {
            return Ok(cached);
        }
        if self.needs_seek(target) {
            self.seek(target)?;
        }
        self.decode_until(target)?;
        let pts = |frame: &frame::Video| frame.timestamp();
        let (frame, span) = match (&self.current, &self.ahead) {
            (Some(current), Some(ahead)) => (current, pts(current).zip(pts(ahead))),
            (Some(current), None) if self.drained => {
                (current, pts(current).map(|start| (start, i64::MAX)))
            }
            (Some(current), None) => (current, None),
            (None, Some(ahead)) => (ahead, None),
            (None, None) => {
                return Err(Error::NoFrame {
                    path: self.path.clone(),
                });
            }
        };
        let frame_time = from_stream_ts(
            pts(frame).unwrap_or(self.start) - self.start,
            self.time_base,
        );
        let converted = Arc::new(self.converter.convert(frame, frame_time)?);
        if let Some((start, end)) = span {
            self.cache.insert(start..end, converted.clone());
        }
        Ok(converted)
    }

    fn stream_ts(&self, time: Time) -> i64 {
        self.start + to_stream_ts(time, self.time_base)
    }

    fn needs_seek(&self, target: i64) -> bool {
        let Some(position) = self.current.as_ref().and_then(|frame| frame.timestamp()) else {
            return true;
        };
        if target < position {
            return true;
        }
        if self.drained {
            return false;
        }
        match self.keyframe_at_or_before(target) {
            Some(keyframe) => keyframe > position,
            None => target - position > to_stream_ts(UNINDEXED_FORWARD_WINDOW, self.time_base),
        }
    }

    fn keyframe_at_or_before(&self, target: i64) -> Option<i64> {
        let stream = self.input.stream(self.stream_index)?;
        let entry = unsafe {
            avformat_index_get_entry_from_timestamp(
                stream.as_ptr().cast_mut(),
                target,
                AVSEEK_FLAG_BACKWARD,
            )
            .as_ref()
        };
        entry.map(|entry| entry.timestamp)
    }

    fn seek(&mut self, target: i64) -> Result<(), Error> {
        let seek_ts = target.rescale(self.time_base, rescale::TIME_BASE);
        self.input
            .seek(seek_ts, ..seek_ts)
            .map_err(|source| Error::Seek {
                path: self.path.clone(),
                source,
            })?;
        self.decoder.flush();
        self.current = None;
        self.ahead = None;
        self.drained = false;
        Ok(())
    }

    fn decode_until(&mut self, target: i64) -> Result<(), Error> {
        while let Some(frame) = self.next_frame()? {
            match frame.timestamp() {
                Some(pts) if pts > target => {
                    self.ahead = Some(frame);
                    return Ok(());
                }
                Some(_) => self.current = Some(frame),
                None => {
                    self.current = Some(frame);
                    return Ok(());
                }
            }
        }
        Ok(())
    }

    fn next_frame(&mut self) -> Result<Option<frame::Video>, Error> {
        if let Some(ahead) = self.ahead.take() {
            return Ok(Some(ahead));
        }
        let mut frame = frame::Video::empty();
        loop {
            if self.decoder.receive_frame(&mut frame).is_ok() {
                return Ok(Some(frame));
            }
            if self.drained {
                return Ok(None);
            }
            self.feed()?;
        }
    }

    fn feed(&mut self) -> Result<(), Error> {
        for (stream, packet) in self.input.packets() {
            if stream.index() != self.stream_index {
                continue;
            }
            return match self.decoder.send_packet(&packet) {
                Ok(()) | Err(ffmpeg_next::Error::InvalidData) => Ok(()),
                Err(source) => Err(Error::Decode { source }),
            };
        }
        self.decoder
            .send_eof()
            .map_err(|source| Error::Decode { source })?;
        self.drained = true;
        Ok(())
    }
}

impl Converter {
    fn convert(&mut self, frame: &frame::Video, time: Time) -> Result<VideoFrame, Error> {
        let source = (frame.format(), frame.width(), frame.height());
        let (width, height) = fitted_size(frame.width(), frame.height(), self.bounds);
        let scaler = match self.scaler.take() {
            Some(scaler) if scaler.source == source => scaler,
            _ => Scaler {
                context: scaling::Context::get(
                    source.0,
                    source.1,
                    source.2,
                    OUTPUT_FORMAT,
                    width,
                    height,
                    scaling::Flags::BILINEAR,
                )
                .map_err(|source| Error::Decode { source })?,
                source,
            },
        };
        let scaler = self.scaler.insert(scaler);
        let mut scaled = frame::Video::empty();
        scaler
            .context
            .run(frame, &mut scaled)
            .map_err(|source| Error::Decode { source })?;
        Ok(VideoFrame {
            width: scaled.width(),
            height: scaled.height(),
            time,
            bgra: packed_rows(&scaled),
        })
    }
}

fn packed_rows(frame: &frame::Video) -> Vec<u8> {
    let row_len = frame.width() as usize * BYTES_PER_PIXEL;
    let stride = frame.stride(0);
    frame
        .data(0)
        .chunks(stride)
        .take(frame.height() as usize)
        .flat_map(|row| &row[..row_len])
        .copied()
        .collect()
}

fn fitted_size(width: u32, height: u32, bounds: Option<(u32, u32)>) -> (u32, u32) {
    let Some((max_width, max_height)) = bounds else {
        return (width, height);
    };
    if width <= max_width && height <= max_height {
        return (width, height);
    }
    let (width, height) = (u64::from(width), u64::from(height));
    let (max_width, max_height) = (u64::from(max_width), u64::from(max_height));
    let fitted = if width * max_height > height * max_width {
        (max_width, height * max_width / width)
    } else {
        (width * max_height / height, max_height)
    };
    (fitted.0.max(1) as u32, fitted.1.max(1) as u32)
}

pub(crate) fn to_stream_ts(time: Time, time_base: Rational) -> i64 {
    let numerator = i128::from(time.flicks()) * i128::from(time_base.denominator());
    let denominator = i128::from(time_base.numerator()) * i128::from(FLICKS_PER_SECOND);
    numerator.div_euclid(denominator) as i64
}

pub(crate) fn from_stream_ts(ts: i64, time_base: Rational) -> Time {
    Time::from_rational(
        ts * i64::from(time_base.numerator()),
        i64::from(time_base.denominator()),
    )
}

#[cfg(test)]
mod tests {
    use tessera_timeline::FrameRate;

    use super::*;
    use crate::fixture::{self, Fixture};

    #[test]
    fn decoder_moves_between_threads() {
        fn assert_send<T: Send>() {}
        assert_send::<VideoDecoder>();
    }

    fn middle_of_frame(rate: FrameRate, frame: i64) -> Time {
        rate.frame_to_time(frame) + Time::from_flicks(rate.frame_duration().flicks() / 2)
    }

    fn assert_shows(frame: &VideoFrame, index: i64) {
        let expected = i32::from(fixture::expected_grey(index));
        let actual = i32::from(frame.bgra[0]);
        assert!(
            (actual - expected).abs() <= 4,
            "expected frame {index} (grey {expected}), got grey {actual}"
        );
    }

    #[test]
    fn frames_are_found_across_keyframes_in_any_order() {
        let fixture = Fixture::generate("frames_in_any_order");
        let mut decoder = VideoDecoder::open(fixture.path()).unwrap();
        for index in [0, 7, 3, 12, 19, 11, 5] {
            let frame = decoder
                .frame_at(middle_of_frame(fixture::FRAME_RATE, index))
                .unwrap();
            assert_eq!(
                (frame.width, frame.height),
                (fixture::WIDTH, fixture::HEIGHT)
            );
            assert_eq!(
                frame.bgra.len(),
                (fixture::WIDTH * fixture::HEIGHT * 4) as usize
            );
            assert_eq!(frame.time, fixture::FRAME_RATE.frame_to_time(index));
            assert_shows(&frame, index);
        }
    }

    #[test]
    fn stepping_through_every_frame_without_a_cache() {
        let fixture = Fixture::generate("stepping");
        let mut decoder = VideoDecoder::open(fixture.path())
            .unwrap()
            .cache_capacity(0);
        let forward = 0..fixture::FRAME_COUNT;
        for index in forward.clone().chain(forward.rev()) {
            let frame = decoder
                .frame_at(fixture::FRAME_RATE.frame_to_time(index))
                .unwrap();
            assert_eq!(frame.time, fixture::FRAME_RATE.frame_to_time(index));
            assert_shows(&frame, index);
        }
    }

    #[test]
    fn nearby_frames_decode_forward_and_distant_ones_seek() {
        let fixture = Fixture::generate("seek_policy");
        let mut decoder = VideoDecoder::open(fixture.path()).unwrap();
        let ts = |decoder: &VideoDecoder, index| {
            decoder.stream_ts(middle_of_frame(fixture::FRAME_RATE, index))
        };
        assert!(decoder.needs_seek(ts(&decoder, 0)));
        decoder
            .frame_at(middle_of_frame(fixture::FRAME_RATE, 7))
            .unwrap();
        assert!(!decoder.needs_seek(ts(&decoder, 7)));
        assert!(!decoder.needs_seek(ts(&decoder, 9)));
        assert!(decoder.needs_seek(ts(&decoder, 6)));
        assert!(decoder.needs_seek(ts(&decoder, 10)));
        assert!(decoder.needs_seek(ts(&decoder, 17)));
    }

    #[test]
    fn repeated_requests_within_a_frame_hit_the_cache() {
        let fixture = Fixture::generate("cache_hits");
        let mut decoder = VideoDecoder::open(fixture.path()).unwrap();
        let first = decoder
            .frame_at(fixture::FRAME_RATE.frame_to_time(4))
            .unwrap();
        decoder
            .frame_at(fixture::FRAME_RATE.frame_to_time(12))
            .unwrap();
        let again = decoder
            .frame_at(middle_of_frame(fixture::FRAME_RATE, 4))
            .unwrap();
        assert!(Arc::ptr_eq(&first, &again));
    }

    #[test]
    fn time_past_the_end_gives_the_last_frame() {
        let fixture = Fixture::generate("past_the_end");
        let mut decoder = VideoDecoder::open(fixture.path()).unwrap();
        let last = fixture::FRAME_COUNT - 1;
        let frame = decoder.frame_at(Time::from_seconds(60)).unwrap();
        assert_eq!(frame.time, fixture::FRAME_RATE.frame_to_time(last));
        assert_shows(&frame, last);
    }

    #[test]
    fn fitting_keeps_the_aspect_ratio() {
        let fixture = Fixture::generate("fitting");
        let mut decoder = VideoDecoder::open(fixture.path())
            .unwrap()
            .fit_within(16, 16);
        let frame = decoder.frame_at(Time::ZERO).unwrap();
        assert_eq!((frame.width, frame.height), (16, 12));
        assert_eq!(frame.bgra.len(), 16 * 12 * 4);
    }

    #[test]
    fn sizes_fit_the_tighter_bound() {
        assert_eq!(fitted_size(1920, 1080, Some((160, 160))), (160, 90));
        assert_eq!(fitted_size(1080, 1920, Some((160, 160))), (90, 160));
        assert_eq!(fitted_size(100, 50, Some((160, 160))), (100, 50));
        assert_eq!(fitted_size(4000, 1, Some((10, 10))), (10, 1));
        assert_eq!(fitted_size(1920, 1080, None), (1920, 1080));
    }

    #[test]
    fn stream_timestamps_round_trip() {
        let time_base = Rational::new(1, 90_000);
        let time = FrameRate::NTSC_30.frame_to_time(1234);
        let ts = to_stream_ts(time, time_base);
        assert_eq!(ts, 1234 * 3003);
        assert_eq!(from_stream_ts(ts, time_base), time);
    }

    #[test]
    fn missing_file_is_an_open_error() {
        crate::init().unwrap();
        let error = VideoDecoder::open("/nonexistent/tessera/clip.mkv")
            .err()
            .unwrap();
        assert!(matches!(error, Error::Open { .. }), "{error}");
    }
}
