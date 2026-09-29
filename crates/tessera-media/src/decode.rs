use std::{
    ffi::c_int,
    num::{NonZeroI32, NonZeroI64},
    path::{Path, PathBuf},
    sync::Arc,
};

use ffmpeg_next::{
    Rational, Rescale, codec, color, decoder,
    ffi::{
        AV_NOPTS_VALUE, AVColorSpace, AVSEEK_FLAG_BACKWARD, SWS_CS_ITU601, SWS_CS_ITU709,
        avformat_index_get_entry_from_timestamp, sws_getCoefficients, sws_setColorspaceDetails,
    },
    format::{self, Pixel},
    frame, media, rescale,
    software::scaling,
};
use tessera_timeline::{FLICKS_PER_SECOND, Time};

use crate::{
    Error,
    cache::FrameCache,
    hw::{self, HwAccel, PREFERRED_HW_ACCELS},
};

const OUTPUT_FORMAT: Pixel = Pixel::BGRA;
const BYTES_PER_PIXEL: usize = 4;
const DEFAULT_CACHE_BYTES: usize = 256 * 1024 * 1024;
const UNINDEXED_FORWARD_WINDOW: Time = Time::from_seconds(1);
const HELD_FRAMES: i32 = 2;
const HIGH_DEFINITION: (u32, u32) = (1280, 720);
const FULL_RANGE_OUTPUT: c_int = 1;
const NEUTRAL_BRIGHTNESS: c_int = 0;
const UNIT_CONTRAST: c_int = 1 << 16;
const UNIT_SATURATION: c_int = 1 << 16;

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
    time_base: TimeBase,
    start: i64,
    decoder: decoder::Video,
    hw_accel: Option<HwAccel>,
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
    source: Source,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Source {
    format: Pixel,
    width: u32,
    height: u32,
    matrix: c_int,
    full_range: bool,
}

unsafe impl Send for Scaler {}

impl VideoDecoder {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, Error> {
        Self::open_with(path, PREFERRED_HW_ACCELS)
    }

    pub fn open_with(path: impl AsRef<Path>, hw_accels: &[HwAccel]) -> Result<Self, Error> {
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
        let time_base = TimeBase::of(&stream, &path)?;
        let start = match stream.start_time() {
            AV_NOPTS_VALUE => 0,
            start => start,
        };
        let stream_error = |source| Error::Stream {
            index: stream_index,
            source,
        };
        let mut context =
            codec::Context::from_parameters(stream.parameters()).map_err(stream_error)?;
        let codec = decoder::find(context.id())
            .ok_or(ffmpeg_next::Error::DecoderNotFound)
            .map_err(stream_error)?;
        let hw_accel = hw::attach_device(&mut context, codec, hw_accels, HELD_FRAMES);
        let decoder = context
            .decoder()
            .open_as(codec)
            .and_then(|opened| opened.video())
            .map_err(stream_error)?;
        Ok(Self {
            path,
            input,
            stream_index,
            time_base,
            start,
            decoder,
            hw_accel,
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

    pub fn hw_accel(&self) -> Option<HwAccel> {
        self.hw_accel
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
        let frame_time = self
            .time_base
            .to_time(pts(frame).unwrap_or(self.start) - self.start);
        let converted = Arc::new(self.converter.convert(frame, frame_time)?);
        if let Some((start, end)) = span {
            self.cache.insert(start..end, converted.clone());
        }
        Ok(converted)
    }

    fn stream_ts(&self, time: Time) -> i64 {
        self.start + self.time_base.to_ts(time)
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
            None => target - position > self.time_base.to_ts(UNINDEXED_FORWARD_WINDOW),
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
        let seek_ts = target.rescale(self.time_base.rational(), rescale::TIME_BASE);
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
                if !hw::is_hardware_frame(&frame) {
                    self.hw_accel = None;
                }
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
        let (matrix, range) = (frame.color_space(), frame.color_range());
        let downloaded;
        let frame = if hw::is_hardware_frame(frame) {
            downloaded = hw::download(frame).map_err(|source| Error::Decode { source })?;
            &downloaded
        } else {
            frame
        };
        let source = Source::new(frame.format(), frame.width(), frame.height(), matrix, range);
        let (width, height) = fitted_size(source.width, source.height, self.bounds);
        let scaler = match self.scaler.take() {
            Some(scaler) if scaler.source == source => scaler,
            _ => Scaler::new(source, width, height).map_err(|source| Error::Decode { source })?,
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

impl Source {
    fn new(
        format: Pixel,
        width: u32,
        height: u32,
        space: color::Space,
        range: color::Range,
    ) -> Self {
        Self {
            format,
            width,
            height,
            matrix: yuv_matrix(space, width, height),
            full_range: range == color::Range::JPEG || is_full_range_format(format),
        }
    }
}

impl Scaler {
    fn new(source: Source, width: u32, height: u32) -> Result<Self, ffmpeg_next::Error> {
        let mut context = scaling::Context::get(
            source.format,
            source.width,
            source.height,
            OUTPUT_FORMAT,
            width,
            height,
            scaling::Flags::BILINEAR,
        )?;
        let applied = unsafe {
            let coefficients = sws_getCoefficients(source.matrix);
            sws_setColorspaceDetails(
                context.as_mut_ptr(),
                coefficients,
                c_int::from(source.full_range),
                coefficients,
                FULL_RANGE_OUTPUT,
                NEUTRAL_BRIGHTNESS,
                UNIT_CONTRAST,
                UNIT_SATURATION,
            )
        };
        if applied < 0 {
            return Err(ffmpeg_next::Error::from(applied));
        }
        Ok(Self { context, source })
    }
}

fn yuv_matrix(space: color::Space, width: u32, height: u32) -> c_int {
    match space {
        color::Space::Unspecified | color::Space::Reserved => {
            let (hd_width, hd_height) = HIGH_DEFINITION;
            if width >= hd_width || height >= hd_height {
                SWS_CS_ITU709
            } else {
                SWS_CS_ITU601
            }
        }
        space => AVColorSpace::from(space) as c_int,
    }
}

fn is_full_range_format(format: Pixel) -> bool {
    matches!(
        format,
        Pixel::YUVJ420P | Pixel::YUVJ422P | Pixel::YUVJ444P | Pixel::YUVJ440P | Pixel::YUVJ411P
    )
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TimeBase {
    numerator: NonZeroI32,
    denominator: NonZeroI32,
}

impl TimeBase {
    pub(crate) fn of(stream: &format::stream::Stream, path: &Path) -> Result<Self, Error> {
        let time_base = stream.time_base();
        Self::new(time_base).ok_or_else(|| Error::InvalidTimeBase {
            path: path.to_owned(),
            index: stream.index(),
            numerator: time_base.numerator(),
            denominator: time_base.denominator(),
        })
    }

    fn new(time_base: Rational) -> Option<Self> {
        let positive = |part| NonZeroI32::new(part).filter(|part| part.is_positive());
        Some(Self {
            numerator: positive(time_base.numerator())?,
            denominator: positive(time_base.denominator())?,
        })
    }

    pub(crate) fn rational(self) -> Rational {
        Rational::new(self.numerator.get(), self.denominator.get())
    }

    pub(crate) fn to_ts(self, time: Time) -> i64 {
        let scaled = i128::from(time.flicks()) * i128::from(self.denominator.get());
        let per_tick = i128::from(self.numerator.get()) * i128::from(FLICKS_PER_SECOND);
        scaled
            .div_euclid(per_tick)
            .clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
    }

    pub(crate) fn to_time(self, ts: i64) -> Time {
        Time::from_rational(
            ts.saturating_mul(i64::from(self.numerator.get())),
            NonZeroI64::from(self.denominator),
        )
    }
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
    fn no_accelerator_offered_decodes_in_software() {
        let fixture = Fixture::generate("software_only");
        let mut decoder = VideoDecoder::open_with(fixture.path(), &[]).unwrap();
        assert_eq!(decoder.hw_accel(), None);
        for index in [0, 9, 4] {
            let frame = decoder
                .frame_at(middle_of_frame(fixture::FRAME_RATE, index))
                .unwrap();
            assert_shows(&frame, index);
        }
        assert_eq!(decoder.hw_accel(), None);
    }

    #[test]
    fn preferred_accelerators_decode_like_software() {
        let Some(fixture) = Fixture::generate_h264("h264_accelerated") else {
            return;
        };
        let mut software = VideoDecoder::open_with(fixture.path(), &[]).unwrap();
        let mut preferred = VideoDecoder::open(fixture.path()).unwrap();
        for index in [0, 7, 3, 12, 19, 11, 5] {
            let time = middle_of_frame(fixture::FRAME_RATE, index);
            let expected = software.frame_at(time).unwrap();
            let actual = preferred.frame_at(time).unwrap();
            assert_eq!(
                (actual.width, actual.height, actual.time),
                (expected.width, expected.height, expected.time)
            );
            assert_shows(&expected, index);
            assert_shows(&actual, index);
        }
        if let Some(accel) = preferred.hw_accel() {
            assert!(PREFERRED_HW_ACCELS.contains(&accel), "{accel:?}");
        }
    }

    #[test]
    fn stepping_through_every_frame_with_preferred_accelerators() {
        let Some(fixture) = Fixture::generate_h264("h264_stepping") else {
            return;
        };
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
    fn fitting_keeps_the_aspect_ratio() {
        let fixture = Fixture::generate("fitting");
        let mut decoder = VideoDecoder::open(fixture.path())
            .unwrap()
            .fit_within(16, 16);
        let frame = decoder.frame_at(Time::ZERO).unwrap();
        assert_eq!((frame.width, frame.height), (16, 12));
        assert_eq!(frame.bgra.len(), 16 * 12 * 4);
    }

    const BT709_RED: [u8; 3] = [63, 102, 240];
    const LIMITED_BLACK: [u8; 3] = [16, 128, 128];

    fn converted_rgb(
        ycbcr: [u8; 3],
        (width, height): (u32, u32),
        space: color::Space,
        range: color::Range,
    ) -> [u8; 3] {
        let mut frame = frame::Video::new(Pixel::YUV444P, width, height);
        for (plane, value) in ycbcr.into_iter().enumerate() {
            frame.data_mut(plane).fill(value);
        }
        frame.set_color_space(space);
        frame.set_color_range(range);
        let mut converter = Converter {
            bounds: None,
            scaler: None,
        };
        let converted = converter.convert(&frame, Time::ZERO).unwrap();
        let [blue, green, red, _] = converted.bgra[..4].try_into().unwrap();
        [red, green, blue]
    }

    fn assert_near(actual: [u8; 3], expected: [u8; 3]) {
        let near = actual
            .iter()
            .zip(expected)
            .all(|(&actual, expected)| actual.abs_diff(expected) <= 3);
        assert!(near, "expected about {expected:?}, got {actual:?}");
    }

    #[test]
    fn yuv_converts_with_the_tagged_matrix() {
        crate::init().unwrap();
        let small = (16, 16);
        let limited = color::Range::MPEG;
        let red = converted_rgb(BT709_RED, small, color::Space::BT709, limited);
        assert_near(red, [255, 0, 0]);
        let misread = converted_rgb(BT709_RED, small, color::Space::BT470BG, limited);
        assert!(misread[0] < 240, "{misread:?}");
        let bt2020 = converted_rgb(BT709_RED, small, color::Space::BT2020NCL, limited);
        assert_ne!(bt2020, red);
    }

    #[test]
    fn untagged_yuv_is_read_as_bt709_from_high_definition_up() {
        crate::init().unwrap();
        let untagged = color::Space::Unspecified;
        let limited = color::Range::Unspecified;
        let hd = converted_rgb(BT709_RED, (1280, 720), untagged, limited);
        assert_near(hd, [255, 0, 0]);
        let sd = converted_rgb(BT709_RED, (720, 576), untagged, limited);
        assert_eq!(
            sd,
            converted_rgb(BT709_RED, (16, 16), color::Space::BT470BG, limited)
        );
    }

    #[test]
    fn full_range_yuv_keeps_its_levels() {
        crate::init().unwrap();
        let small = (16, 16);
        let limited = converted_rgb(
            LIMITED_BLACK,
            small,
            color::Space::BT709,
            color::Range::MPEG,
        );
        assert_near(limited, [0, 0, 0]);
        let full = converted_rgb(
            LIMITED_BLACK,
            small,
            color::Space::BT709,
            color::Range::JPEG,
        );
        assert_near(full, [16, 16, 16]);
    }

    #[test]
    fn rgb_sources_convert_without_a_matrix() {
        crate::init().unwrap();
        let mut frame = frame::Video::new(Pixel::RGB24, 16, 16);
        let stride = frame.stride(0);
        for row in frame.data_mut(0).chunks_mut(stride) {
            for pixel in row[..16 * 3].chunks_mut(3) {
                pixel.copy_from_slice(&[200, 100, 50]);
            }
        }
        let mut converter = Converter {
            bounds: None,
            scaler: None,
        };
        let converted = converter.convert(&frame, Time::ZERO).unwrap();
        assert_eq!(converted.bgra[..4], [50, 100, 200, 255]);
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
        let time_base = TimeBase::new(Rational::new(1, 90_000)).unwrap();
        let time = FrameRate::NTSC_30.frame_to_time(1234);
        let ts = time_base.to_ts(time);
        assert_eq!(ts, 1234 * 3003);
        assert_eq!(time_base.to_time(ts), time);
        assert_eq!(time_base.rational(), Rational::new(1, 90_000));
    }

    #[test]
    fn time_bases_need_positive_parts() {
        for (numerator, denominator) in [(0, 1), (1, 0), (-1, 1000), (1, -1000), (0, 0)] {
            assert_eq!(TimeBase::new(Rational::new(numerator, denominator)), None);
        }
    }

    #[test]
    fn stream_timestamps_saturate() {
        let time_base = TimeBase::new(Rational::new(1, i32::MAX)).unwrap();
        let coarse = TimeBase::new(Rational::new(i32::MAX, 1)).unwrap();
        assert_eq!(time_base.to_ts(Time::MAX), i64::MAX);
        assert_eq!(time_base.to_ts(Time::MIN), i64::MIN);
        assert_eq!(coarse.to_time(i64::MAX), Time::MAX);
        assert_eq!(coarse.to_time(i64::MIN), Time::MIN);
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
