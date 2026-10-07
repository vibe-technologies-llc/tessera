use std::{
    ffi::c_int,
    num::{NonZeroI32, NonZeroI64, NonZeroU32},
    path::{Path, PathBuf},
    sync::Arc,
};

use ffmpeg_next::{
    Packet, Rational, Rescale, codec, color, decoder,
    ffi::{
        AV_NOPTS_VALUE, AVColorSpace, AVSEEK_FLAG_BACKWARD, FF_THREAD_FRAME, FF_THREAD_SLICE,
        SWS_CS_ITU601, SWS_CS_ITU709, avformat_index_get_entries_count, avformat_index_get_entry,
        avformat_index_get_entry_from_timestamp, sws_getCoefficients, sws_setColorspaceDetails,
    },
    format::{self, Pixel},
    frame, media, rescale,
    software::scaling,
};
use tessera_timeline::{FLICKS_PER_SECOND, PixelAspect, Rotation, Time};

use crate::{
    Error,
    cache::FrameCache,
    hw::{self, HwAccel, PREFERRED_HW_ACCELS},
    probe,
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
const AUTOMATIC_THREAD_COUNT: c_int = 0;

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
    thread_count: c_int,
    hw_accel: Option<HwAccel>,
    converter: Converter,
    current: Option<frame::Video>,
    ahead: Option<frame::Video>,
    drained: bool,
    seeks: usize,
    cache: FrameCache,
}

struct Converter {
    bounds: Option<(u32, u32)>,
    shape: Shape,
    scaler: Option<Scaler>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Shape {
    rotation: Rotation,
    pixel_aspect: PixelAspect,
}

impl Shape {
    fn of(input: &format::context::Input, stream: &format::stream::Stream) -> Self {
        Self {
            rotation: probe::rotation(stream),
            pixel_aspect: probe::pixel_aspect(input, stream),
        }
    }

    fn scaled_bounds(self, bounds: Option<(u32, u32)>) -> Option<(u32, u32)> {
        bounds.map(|(width, height)| {
            if self.rotation.swaps_sides() {
                (height, width)
            } else {
                (width, height)
            }
        })
    }

    fn stretched_width(self, width: u32) -> u32 {
        NonZeroU32::new(width).map_or(width, |width| {
            self.pixel_aspect.stretched_width(width).get()
        })
    }
}

struct Scaler {
    context: scaling::Context,
    source: Source,
    scaled: frame::Video,
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
    pub fn open(path: impl AsRef<Path>, stream: usize) -> Result<Self, Error> {
        Self::open_with(path, stream, PREFERRED_HW_ACCELS)
    }

    pub fn open_with(
        path: impl AsRef<Path>,
        stream: usize,
        hw_accels: &[HwAccel],
    ) -> Result<Self, Error> {
        Self::open_threaded(path, stream, hw_accels, AUTOMATIC_THREAD_COUNT)
    }

    fn open_threaded(
        path: impl AsRef<Path>,
        stream: usize,
        hw_accels: &[HwAccel],
        thread_count: c_int,
    ) -> Result<Self, Error> {
        let path = path.as_ref().to_owned();
        let input = format::input(&path).map_err(|source| Error::Open {
            path: path.clone(),
            source,
        })?;
        let stream =
            stream_of_kind(&input, stream, media::Type::Video).ok_or_else(|| Error::NoVideo {
                path: path.clone(),
                index: stream,
            })?;
        let stream_index = stream.index();
        let time_base = TimeBase::of(&stream, &path)?;
        let start = stream_start(&input, &stream);
        let shape = Shape::of(&input, &stream);
        let (decoder, hw_accel) =
            open_decoder(&stream, hw_accels, thread_count).map_err(|source| Error::Stream {
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
            thread_count,
            hw_accel,
            converter: Converter {
                bounds: None,
                shape,
                scaler: None,
            },
            current: None,
            ahead: None,
            drained: false,
            seeks: 0,
            cache: FrameCache::new(DEFAULT_CACHE_BYTES),
        })
    }

    pub fn fit_within(mut self, width: u32, height: u32) -> Self {
        self.converter = Converter {
            bounds: Some((width.max(1), height.max(1))),
            shape: self.converter.shape,
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

    pub fn is_cached(&self, time: Time) -> bool {
        self.cache.contains(self.stream_ts(time))
    }

    pub fn frame_at(&mut self, time: Time) -> Result<Arc<VideoFrame>, Error> {
        let target = self.stream_ts(time);
        if let Some(cached) = self.cache.get(target) {
            return Ok(cached);
        }
        if self.needs_seek(target) {
            self.seek(target)?;
        }
        self.decode_with_fallback(target)?;
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

    fn decode_with_fallback(&mut self, target: i64) -> Result<(), Error> {
        match self.decode_until(target) {
            Err(Error::Decode { source }) if self.hw_accel.is_some() => {
                tracing::warn!(path = %self.path.display(), %source, "hardware decode failed, continuing in software");
                self.fall_back_to_software()?;
                self.seek(target)?;
                self.decode_until(target)
            }
            decoded => decoded,
        }
    }

    fn fall_back_to_software(&mut self) -> Result<(), Error> {
        let stream_error = |source| Error::Stream {
            index: self.stream_index,
            source,
        };
        let stream = self
            .input
            .stream(self.stream_index)
            .ok_or_else(|| stream_error(ffmpeg_next::Error::StreamNotFound))?;
        let (decoder, hw_accel) =
            open_decoder(&stream, &[], self.thread_count).map_err(stream_error)?;
        self.decoder = decoder;
        self.hw_accel = hw_accel;
        Ok(())
    }

    fn needs_seek(&self, target: i64) -> bool {
        let timestamp = |frame: &Option<frame::Video>| frame.as_ref().and_then(|f| f.timestamp());
        let (current, ahead) = (timestamp(&self.current), timestamp(&self.ahead));
        let Some(position) = current.or(ahead) else {
            return true;
        };
        if target < position {
            return current.is_some();
        }
        if self.drained {
            return false;
        }
        forward_seek_needed(
            position,
            target,
            self.keyframe_at_or_before(target),
            self.index_end(),
            self.time_base.to_ts(UNINDEXED_FORWARD_WINDOW),
        )
    }

    fn index_end(&self) -> Option<i64> {
        let stream = self.input.stream(self.stream_index)?;
        let entries = unsafe { avformat_index_get_entries_count(stream.as_ptr()) };
        let last = entries.checked_sub(1).filter(|last| *last >= 0)?;
        let entry = unsafe { avformat_index_get_entry(stream.as_ptr().cast_mut(), last).as_ref() };
        entry.map(|entry| entry.timestamp)
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
        self.seeks += 1;
        Ok(())
    }

    #[cfg(test)]
    fn seeks(&self) -> usize {
        self.seeks
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
            match self.decoder.receive_frame(&mut frame) {
                Ok(()) => {
                    if !hw::is_hardware_frame(&frame) {
                        self.hw_accel = None;
                    }
                    return Ok(Some(frame));
                }
                Err(ffmpeg_next::Error::Eof) => return Ok(None),
                Err(source) if !is_again(&source) => return Err(Error::Decode { source }),
                Err(_) => {}
            }
            if self.drained {
                return Ok(None);
            }
            self.feed()?;
        }
    }

    fn feed(&mut self) -> Result<(), Error> {
        match read_packet(&mut self.input, self.stream_index) {
            Ok(Some(packet)) => match self.decoder.send_packet(&packet) {
                Ok(()) | Err(ffmpeg_next::Error::InvalidData) => Ok(()),
                Err(source) => Err(Error::Decode { source }),
            },
            Ok(None) => {
                self.decoder
                    .send_eof()
                    .map_err(|source| Error::Decode { source })?;
                self.drained = true;
                Ok(())
            }
            Err(source) => Err(Error::Decode { source }),
        }
    }
}

pub(crate) fn read_packet(
    input: &mut format::context::Input,
    stream_index: usize,
) -> Result<Option<Packet>, ffmpeg_next::Error> {
    loop {
        let mut packet = Packet::empty();
        match packet.read(input) {
            Ok(()) if packet.stream() == stream_index => return Ok(Some(packet)),
            Ok(()) | Err(ffmpeg_next::Error::InvalidData) => {}
            Err(ffmpeg_next::Error::Eof) => return Ok(None),
            Err(error) => return Err(error),
        }
    }
}

pub(crate) fn is_again(error: &ffmpeg_next::Error) -> bool {
    *error
        == ffmpeg_next::Error::Other {
            errno: ffmpeg_next::error::EAGAIN,
        }
}

pub(crate) fn stream_of_kind(
    input: &format::context::Input,
    index: usize,
    medium: media::Type,
) -> Option<format::stream::Stream<'_>> {
    input
        .stream(index)
        .filter(|stream| stream.parameters().medium() == medium)
}

pub(crate) fn stream_start(input: &format::context::Input, stream: &format::stream::Stream) -> i64 {
    let container = unsafe { (*input.as_ptr()).start_time };
    if container != AV_NOPTS_VALUE {
        container.rescale(rescale::TIME_BASE, stream.time_base())
    } else if stream.start_time() != AV_NOPTS_VALUE {
        stream.start_time()
    } else {
        0
    }
}

fn open_decoder(
    stream: &format::stream::Stream,
    hw_accels: &[HwAccel],
    thread_count: c_int,
) -> Result<(decoder::Video, Option<HwAccel>), ffmpeg_next::Error> {
    let attempt = |accels: &[HwAccel]| {
        let mut context = codec::Context::from_parameters(stream.parameters())?;
        let codec = decoder::find(context.id()).ok_or(ffmpeg_next::Error::DecoderNotFound)?;
        let hw_accel = hw::attach_device(&mut context, codec, accels, HELD_FRAMES);
        set_threads(&mut context, thread_count);
        let opened = context
            .decoder()
            .open_as(codec)
            .and_then(|opened| opened.video())?;
        Ok((opened, hw_accel))
    };
    match attempt(hw_accels) {
        Err(error) if !hw_accels.is_empty() => {
            tracing::warn!(%error, "cannot open the hardware decoder, using software");
            attempt(&[])
        }
        opened => opened,
    }
}

fn forward_seek_needed(
    position: i64,
    target: i64,
    keyframe: Option<i64>,
    index_end: Option<i64>,
    window: i64,
) -> bool {
    match keyframe {
        Some(keyframe) if keyframe > position => true,
        Some(_) if index_end.is_none_or(|end| end >= target) => false,
        _ => target - position > window,
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
        let stretched = self.shape.stretched_width(source.width);
        let (width, height) = fitted_size(
            stretched,
            source.height,
            self.shape.scaled_bounds(self.bounds),
        );
        let scaler = match self.scaler.take() {
            Some(scaler) if scaler.source == source => scaler,
            _ => Scaler::new(source, width, height).map_err(|source| Error::Decode { source })?,
        };
        let Scaler {
            context, scaled, ..
        } = self.scaler.insert(scaler);
        context
            .run(frame, scaled)
            .map_err(|source| Error::Decode { source })?;
        let (width, height, bgra) = turned(
            scaled.width(),
            scaled.height(),
            packed_rows(scaled),
            self.shape.rotation,
        );
        Ok(VideoFrame {
            width,
            height,
            time,
            bgra,
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
        Ok(Self {
            context,
            source,
            scaled: frame::Video::empty(),
        })
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

fn set_threads(context: &mut codec::Context, thread_count: c_int) {
    unsafe {
        let context = context.as_mut_ptr();
        (*context).thread_type = FF_THREAD_FRAME | FF_THREAD_SLICE;
        (*context).thread_count = thread_count;
    }
}

fn packed_rows(frame: &frame::Video) -> Vec<u8> {
    let row_len = frame.width() as usize * BYTES_PER_PIXEL;
    let height = frame.height() as usize;
    let mut packed = Vec::with_capacity(row_len * height);
    for row in frame.data(0).chunks(frame.stride(0)).take(height) {
        packed.extend_from_slice(&row[..row_len]);
    }
    packed
}

fn turned(width: u32, height: u32, bgra: Vec<u8>, rotation: Rotation) -> (u32, u32, Vec<u8>) {
    if rotation == Rotation::Upright {
        return (width, height, bgra);
    }
    let (width, height) = (width as usize, height as usize);
    let pixel = |x: usize, y: usize| {
        let at = (y * width + x) * BYTES_PER_PIXEL;
        &bgra[at..at + BYTES_PER_PIXEL]
    };
    let (turned_width, turned_height) = if rotation.swaps_sides() {
        (height, width)
    } else {
        (width, height)
    };
    let mut turned = Vec::with_capacity(bgra.len());
    for y in 0..turned_height {
        for x in 0..turned_width {
            let (source_x, source_y) = match rotation {
                Rotation::Upright => (x, y),
                Rotation::Clockwise => (y, height - 1 - x),
                Rotation::UpsideDown => (width - 1 - x, height - 1 - y),
                Rotation::Counterclockwise => (width - 1 - y, x),
            };
            turned.extend_from_slice(pixel(source_x, source_y));
        }
    }
    (turned_width as u32, turned_height as u32, turned)
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
        let doubled = 2 * i128::from(time.flicks()) * i128::from(self.denominator.get());
        let per_tick = i128::from(self.numerator.get()) * i128::from(FLICKS_PER_SECOND);
        (doubled + per_tick)
            .div_euclid(2 * per_tick)
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
        let mut decoder = VideoDecoder::open(fixture.path(), fixture::VIDEO_STREAM).unwrap();
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
        let mut decoder = VideoDecoder::open(fixture.path(), fixture::VIDEO_STREAM)
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
    fn stepping_through_ntsc_frames_in_a_millisecond_time_base() {
        let rate = FrameRate::NTSC_30;
        let fixture = Fixture::generate_at_rate("ntsc_milliseconds", rate);
        let mut decoder = VideoDecoder::open(fixture.path(), fixture::VIDEO_STREAM)
            .unwrap()
            .cache_capacity(0);

        let time_base = decoder.time_base.rational();
        assert_eq!(time_base, Rational::new(1, 1000));

        let forward = 0..fixture::FRAME_COUNT;
        for index in forward.clone().chain(forward.rev()) {
            let frame = decoder.frame_at(rate.frame_to_time(index)).unwrap();
            assert_shows(&frame, index);
        }
    }

    #[test]
    fn software_decode_runs_on_every_core() {
        let fixture = Fixture::generate_without_audio("threads");
        let decoder = VideoDecoder::open_with(fixture.path(), fixture::VIDEO_STREAM, &[]).unwrap();

        let cores = std::thread::available_parallelism().unwrap().get();
        let threading = decoder.decoder.threading();

        assert!(threading.count >= cores.min(2), "{threading:?}");
    }

    #[test]
    fn nearby_frames_decode_forward_and_distant_ones_seek() {
        const ONE_THREAD: c_int = 1;

        let fixture = Fixture::generate("seek_policy");
        let mut decoder =
            VideoDecoder::open_threaded(fixture.path(), fixture::VIDEO_STREAM, &[], ONE_THREAD)
                .unwrap();

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
        let mut decoder = VideoDecoder::open(fixture.path(), fixture::VIDEO_STREAM).unwrap();
        let first = decoder
            .frame_at(fixture::FRAME_RATE.frame_to_time(4))
            .unwrap();
        decoder
            .frame_at(fixture::FRAME_RATE.frame_to_time(12))
            .unwrap();
        let middle = middle_of_frame(fixture::FRAME_RATE, 4);

        assert!(decoder.is_cached(middle));
        assert!(!decoder.is_cached(fixture::FRAME_RATE.frame_to_time(20)));

        let again = decoder.frame_at(middle).unwrap();

        assert!(Arc::ptr_eq(&first, &again));
    }

    #[test]
    fn time_past_the_end_gives_the_last_frame() {
        let fixture = Fixture::generate("past_the_end");
        let mut decoder = VideoDecoder::open(fixture.path(), fixture::VIDEO_STREAM).unwrap();
        let last = fixture::FRAME_COUNT - 1;
        let frame = decoder.frame_at(Time::from_seconds(60)).unwrap();
        assert_eq!(frame.time, fixture::FRAME_RATE.frame_to_time(last));
        assert_shows(&frame, last);
    }

    #[test]
    fn no_accelerator_offered_decodes_in_software() {
        let fixture = Fixture::generate("software_only");
        let mut decoder =
            VideoDecoder::open_with(fixture.path(), fixture::VIDEO_STREAM, &[]).unwrap();
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
        let mut software =
            VideoDecoder::open_with(fixture.path(), fixture::VIDEO_STREAM, &[]).unwrap();
        let mut preferred = VideoDecoder::open(fixture.path(), fixture::VIDEO_STREAM).unwrap();
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
        let mut decoder = VideoDecoder::open(fixture.path(), fixture::VIDEO_STREAM)
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
        let mut decoder = VideoDecoder::open(fixture.path(), fixture::VIDEO_STREAM)
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
            shape: Shape::default(),
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
            shape: Shape::default(),
            scaler: None,
        };
        let converted = converter.convert(&frame, Time::ZERO).unwrap();
        assert_eq!(converted.bgra[..4], [50, 100, 200, 255]);
    }

    fn numbered(width: u32, height: u32) -> Vec<u8> {
        (0..width * height)
            .flat_map(|pixel| [pixel as u8; BYTES_PER_PIXEL])
            .collect()
    }

    fn pixel_numbers(bgra: &[u8]) -> Vec<u8> {
        bgra.chunks(BYTES_PER_PIXEL).map(|pixel| pixel[0]).collect()
    }

    #[test]
    fn turning_moves_every_pixel_by_a_quarter_turn() {
        let turn = |rotation| {
            let (width, height, bgra) = turned(3, 2, numbered(3, 2), rotation);
            (width, height, pixel_numbers(&bgra))
        };

        assert_eq!(turn(Rotation::Upright), (3, 2, vec![0, 1, 2, 3, 4, 5]));
        assert_eq!(turn(Rotation::Clockwise), (2, 3, vec![3, 0, 4, 1, 5, 2]));
        assert_eq!(turn(Rotation::UpsideDown), (3, 2, vec![5, 4, 3, 2, 1, 0]));
        assert_eq!(
            turn(Rotation::Counterclockwise),
            (2, 3, vec![2, 5, 1, 4, 0, 3])
        );
    }

    fn generated(name: &str, filter: &str, rotation: Option<&str>) -> Option<PathBuf> {
        let directory = std::env::temp_dir();
        let plain = directory.join(format!(
            "tessera-media-{}-{name}-plain.mp4",
            std::process::id()
        ));
        let path = directory.join(format!("tessera-media-{}-{name}.mp4", std::process::id()));
        let encoded = std::process::Command::new("ffmpeg")
            .args(["-loglevel", "error", "-y", "-f", "lavfi", "-i"])
            .arg(format!("color=c=black:s=64x48:d=1,{filter}"))
            .args(["-c:v", "mpeg4", "-q:v", "2"])
            .arg(&plain)
            .status()
            .is_ok_and(|status| status.success());
        let tagged = encoded
            && std::process::Command::new("ffmpeg")
                .args(["-loglevel", "error", "-y"])
                .args(["-display_rotation", rotation.unwrap_or("0"), "-i"])
                .arg(&plain)
                .args(["-c", "copy"])
                .arg(&path)
                .status()
                .is_ok_and(|status| status.success());
        std::fs::remove_file(&plain).ok();
        crate::init().unwrap();
        tagged.then_some(path)
    }

    fn brightness(frame: &VideoFrame, x: u32, y: u32) -> u8 {
        frame.bgra[((y * frame.width + x) * 4) as usize]
    }

    #[test]
    fn phone_footage_is_turned_upright() {
        let filter = "drawbox=x=0:y=0:w=32:h=24:color=white:t=fill";
        let Some(path) = generated("turned", filter, Some("90")) else {
            return;
        };

        let decoded = VideoDecoder::open(&path, 0).and_then(|mut decoder| {
            let whole = decoder.frame_at(Time::ZERO)?;
            let fitted = decoder.fit_within(24, 24).frame_at(Time::ZERO)?;
            Ok((whole, fitted))
        });
        std::fs::remove_file(&path).ok();
        let (whole, fitted) = decoded.unwrap();

        assert_eq!((whole.width, whole.height), (48, 64));
        assert!(brightness(&whole, 8, 56) > 200);
        assert!(brightness(&whole, 40, 8) < 50);
        assert_eq!((fitted.width, fitted.height), (18, 24));
    }

    #[test]
    fn anamorphic_footage_is_stretched_to_its_display_width() {
        let Some(path) = generated("anamorphic", "setsar=2", None) else {
            return;
        };

        let frame =
            VideoDecoder::open(&path, 0).and_then(|mut decoder| decoder.frame_at(Time::ZERO));
        std::fs::remove_file(&path).ok();
        let frame = frame.unwrap();

        assert_eq!((frame.width, frame.height), (128, 48));
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
    fn stream_timestamps_round_to_the_nearest_tick() {
        let milliseconds = TimeBase::new(Rational::new(1, 1000)).unwrap();

        let second_frame = FrameRate::NTSC_30.frame_to_time(2);
        let before_second_frame = Time::from_flicks(-second_frame.flicks());
        let half_tick = FLICKS_PER_SECOND / 2000;

        assert_eq!(milliseconds.to_ts(FrameRate::NTSC_30.frame_to_time(1)), 33);
        assert_eq!(milliseconds.to_ts(second_frame), 67);
        assert_eq!(milliseconds.to_ts(before_second_frame), -67);
        assert_eq!(milliseconds.to_ts(Time::from_flicks(half_tick)), 1);
        assert_eq!(milliseconds.to_ts(Time::from_flicks(half_tick - 1)), 0);
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
    fn only_the_requested_stream_is_decoded_and_it_must_be_video() {
        let fixture = Fixture::generate("video_stream_index");

        let audio = VideoDecoder::open(fixture.path(), fixture::AUDIO_STREAM)
            .err()
            .unwrap();
        let absent = VideoDecoder::open(fixture.path(), 7).err().unwrap();

        assert!(
            matches!(audio, Error::NoVideo { index, .. } if index == fixture::AUDIO_STREAM),
            "{audio}"
        );
        assert!(
            matches!(absent, Error::NoVideo { index: 7, .. }),
            "{absent}"
        );
    }

    #[test]
    fn missing_file_is_an_open_error() {
        crate::init().unwrap();
        let error = VideoDecoder::open("/nonexistent/tessera/clip.mkv", 0)
            .err()
            .unwrap();
        assert!(matches!(error, Error::Open { .. }), "{error}");
    }

    #[test]
    fn a_stream_that_starts_late_still_begins_at_time_zero() {
        let fixture = Fixture::generate_starting_late("video_starting_late", 25, 0);
        let mut decoder = VideoDecoder::open(fixture.path(), fixture::VIDEO_STREAM).unwrap();

        for index in [0, 9, 4] {
            let frame = decoder
                .frame_at(middle_of_frame(fixture::FRAME_RATE, index))
                .unwrap();

            assert_shows(&frame, index);
        }
    }

    #[test]
    fn asking_for_a_time_before_the_first_frame_seeks_only_once() {
        let fixture = Fixture::generate("video_before_the_start");
        let mut decoder = VideoDecoder::open(fixture.path(), fixture::VIDEO_STREAM).unwrap();
        let before = Time::from_seconds(-1);

        let first = decoder.frame_at(before).unwrap();
        let second = decoder.frame_at(before - Time::from_seconds(1)).unwrap();

        assert_shows(&first, 0);
        assert_shows(&second, 0);
        assert_eq!(decoder.seeks(), 1);
    }

    #[test]
    fn a_forward_gap_seeks_when_the_index_stops_short_of_the_target() {
        let window = 100;

        assert!(forward_seek_needed(0, 500, Some(0), Some(200), window));
        assert!(!forward_seek_needed(450, 500, Some(0), Some(200), window));
        assert!(!forward_seek_needed(0, 500, Some(0), Some(900), window));
        assert!(forward_seek_needed(0, 500, Some(300), Some(900), window));
        assert!(forward_seek_needed(0, 500, None, None, window));
        assert!(!forward_seek_needed(450, 500, None, None, window));
        assert!(!forward_seek_needed(0, 50, Some(0), Some(10), window));
    }

    #[test]
    fn again_is_told_apart_from_real_decode_errors() {
        let again = ffmpeg_next::Error::Other {
            errno: ffmpeg_next::error::EAGAIN,
        };

        assert!(is_again(&again));
        assert!(!is_again(&ffmpeg_next::Error::InvalidData));
        assert!(!is_again(&ffmpeg_next::Error::Eof));
    }
}
