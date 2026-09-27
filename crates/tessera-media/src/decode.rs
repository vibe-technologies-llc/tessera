use std::{
    mem,
    path::{Path, PathBuf},
};

use ffmpeg_next::{
    Rational, Rescale, codec, decoder,
    ffi::AV_NOPTS_VALUE,
    format::{self, Pixel},
    frame, media, rescale,
    software::scaling,
};
use tessera_timeline::{FLICKS_PER_SECOND, Time};

use crate::Error;

const OUTPUT_FORMAT: Pixel = Pixel::BGRA;
const BYTES_PER_PIXEL: usize = 4;

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
    bounds: Option<(u32, u32)>,
    scaler: Option<Scaler>,
}

struct Scaler {
    context: scaling::Context,
    source: (Pixel, u32, u32),
}

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
            bounds: None,
            scaler: None,
        })
    }

    pub fn fit_within(mut self, width: u32, height: u32) -> Self {
        self.bounds = Some((width.max(1), height.max(1)));
        self.scaler = None;
        self
    }

    pub fn frame_at(&mut self, time: Time) -> Result<VideoFrame, Error> {
        let target = self.start + to_stream_ts(time, self.time_base);
        let seek_ts = target.rescale(self.time_base, rescale::TIME_BASE);
        self.input
            .seek(seek_ts, ..seek_ts)
            .map_err(|source| Error::Seek {
                path: self.path.clone(),
                source,
            })?;
        self.decoder.flush();
        let decoded = self.decode_until(target)?;
        let frame = decoded.ok_or_else(|| Error::NoFrame {
            path: self.path.clone(),
        })?;
        self.convert(&frame)
    }

    fn decode_until(&mut self, target: i64) -> Result<Option<frame::Video>, Error> {
        let mut candidate = None;
        let mut decoded = frame::Video::empty();
        let mut packets = self.input.packets();
        loop {
            let exhausted = match packets.next() {
                Some((stream, _)) if stream.index() != self.stream_index => continue,
                Some((_, packet)) => {
                    match self.decoder.send_packet(&packet) {
                        Ok(()) | Err(ffmpeg_next::Error::InvalidData) => {}
                        Err(source) => return Err(Error::Decode { source }),
                    }
                    false
                }
                None => {
                    self.decoder
                        .send_eof()
                        .map_err(|source| Error::Decode { source })?;
                    true
                }
            };
            while self.decoder.receive_frame(&mut decoded).is_ok() {
                match decoded.timestamp() {
                    Some(pts) if pts > target => return Ok(Some(candidate.unwrap_or(decoded))),
                    Some(_) => candidate = Some(mem::replace(&mut decoded, frame::Video::empty())),
                    None => return Ok(Some(decoded)),
                }
            }
            if exhausted {
                return Ok(candidate);
            }
        }
    }

    fn convert(&mut self, frame: &frame::Video) -> Result<VideoFrame, Error> {
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
        let time = from_stream_ts(
            frame.timestamp().unwrap_or(self.start) - self.start,
            self.time_base,
        );
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

fn to_stream_ts(time: Time, time_base: Rational) -> i64 {
    let numerator = i128::from(time.flicks()) * i128::from(time_base.denominator());
    let denominator = i128::from(time_base.numerator()) * i128::from(FLICKS_PER_SECOND);
    numerator.div_euclid(denominator) as i64
}

fn from_stream_ts(ts: i64, time_base: Rational) -> Time {
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
