use std::{
    ffi::c_int,
    num::NonZeroU32,
    path::{Path, PathBuf},
};

use ffmpeg_next::{
    ChannelLayout, Codec, Dictionary, Packet, Rational, codec, color, encoder,
    ffi::{
        AVBufferRef, AVHWFramesContext, AVPixelFormat, SWS_CS_ITU709, av_buffer_ref,
        av_buffer_unref, av_hwframe_ctx_alloc, av_hwframe_ctx_init, av_hwframe_get_buffer,
        av_hwframe_transfer_data, sws_getCoefficients, sws_setColorspaceDetails,
    },
    format::{self, Pixel, sample},
    frame,
    software::scaling,
};
use tessera_timeline::FrameRate;

use crate::{Error, HwAccel, audio::AudioBuffer, hw};

const INPUT_FORMAT: Pixel = Pixel::BGRA;
const BYTES_PER_PIXEL: usize = 4;
const SOFTWARE_FORMAT: Pixel = Pixel::YUV420P;
const UPLOAD_FORMAT: Pixel = Pixel::NV12;
const HARDWARE_POOL_FRAMES: c_int = 16;
const KEYFRAME_SECONDS: u32 = 2;
const AUDIO_BIT_RATE: usize = 192_000;
const FALLBACK_AUDIO_FRAME: usize = 1024;
const FULL_RANGE: c_int = 1;
const LIMITED_RANGE: c_int = 0;
const NEUTRAL_BRIGHTNESS: c_int = 0;
const UNIT_CONTRAST: c_int = 1 << 16;
const UNIT_SATURATION: c_int = 1 << 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum VideoCodec {
    H264,
    Hevc,
    Av1,
}

impl VideoCodec {
    pub const ALL: [Self; 3] = [Self::H264, Self::Hevc, Self::Av1];

    pub fn label(self) -> &'static str {
        match self {
            Self::H264 => "H.264",
            Self::Hevc => "HEVC",
            Self::Av1 => "AV1",
        }
    }

    fn encoders(self, backend: EncodeBackend) -> &'static [&'static str] {
        match (self, backend) {
            (Self::H264, EncodeBackend::Vaapi) => &["h264_vaapi"],
            (Self::Hevc, EncodeBackend::Vaapi) => &["hevc_vaapi"],
            (Self::Av1, EncodeBackend::Vaapi) => &["av1_vaapi"],
            (Self::H264, EncodeBackend::Nvenc) => &["h264_nvenc"],
            (Self::Hevc, EncodeBackend::Nvenc) => &["hevc_nvenc"],
            (Self::Av1, EncodeBackend::Nvenc) => &["av1_nvenc"],
            (Self::H264, EncodeBackend::Vulkan) => &["h264_vulkan"],
            (Self::Hevc, EncodeBackend::Vulkan) => &["hevc_vulkan"],
            (Self::Av1, EncodeBackend::Vulkan) => &["av1_vulkan"],
            (Self::H264, EncodeBackend::Software) => &["libx264"],
            (Self::Hevc, EncodeBackend::Software) => &["libx265"],
            (Self::Av1, EncodeBackend::Software) => &["libsvtav1", "libaom-av1"],
        }
    }

    fn options(self, backend: EncodeBackend) -> Dictionary<'static> {
        let pairs: &[(&str, &str)] = match (self, backend) {
            (Self::H264, EncodeBackend::Software) => &[("preset", "medium"), ("crf", "20")],
            (Self::Hevc, EncodeBackend::Software) => &[
                ("preset", "medium"),
                ("crf", "22"),
                ("x265-params", "log-level=error"),
            ],
            (Self::Av1, EncodeBackend::Software) => &[("preset", "8"), ("crf", "30")],
            (Self::H264, EncodeBackend::Nvenc) => &[("preset", "p5"), ("rc", "vbr"), ("cq", "23")],
            (Self::Hevc, EncodeBackend::Nvenc) => &[("preset", "p5"), ("rc", "vbr"), ("cq", "25")],
            (Self::Av1, EncodeBackend::Nvenc) => &[("preset", "p5"), ("rc", "vbr"), ("cq", "30")],
            (_, EncodeBackend::Vaapi | EncodeBackend::Vulkan) => &[],
        };
        pairs.iter().copied().collect()
    }

    fn hardware_quality(self) -> i32 {
        match self {
            Self::H264 => 23,
            Self::Hevc => 25,
            Self::Av1 => 110,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Container {
    Mp4,
    Matroska,
}

impl Container {
    pub const ALL: [Self; 2] = [Self::Mp4, Self::Matroska];

    pub fn label(self) -> &'static str {
        match self {
            Self::Mp4 => "MP4",
            Self::Matroska => "MKV",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Self::Mp4 => "mp4",
            Self::Matroska => "mkv",
        }
    }

    fn muxer(self) -> &'static str {
        match self {
            Self::Mp4 => "mp4",
            Self::Matroska => "matroska",
        }
    }

    fn audio_encoders(self) -> &'static [&'static str] {
        match self {
            Self::Mp4 => &["aac"],
            Self::Matroska => &["libopus", "aac"],
        }
    }

    fn header_options(self) -> Dictionary<'static> {
        match self {
            Self::Mp4 => [("movflags", "+faststart")].into_iter().collect(),
            Self::Matroska => Dictionary::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EncodeBackend {
    Vaapi,
    Nvenc,
    Vulkan,
    Software,
}

pub const PREFERRED_ENCODE_BACKENDS: &[EncodeBackend] = &[
    EncodeBackend::Vaapi,
    EncodeBackend::Nvenc,
    EncodeBackend::Vulkan,
    EncodeBackend::Software,
];

impl EncodeBackend {
    pub fn label(self) -> &'static str {
        match self {
            Self::Vaapi => "VAAPI",
            Self::Nvenc => "NVENC",
            Self::Vulkan => "Vulkan Video",
            Self::Software => "software",
        }
    }

    fn upload(self) -> Option<(HwAccel, AVPixelFormat)> {
        match self {
            Self::Vaapi => Some((HwAccel::Vaapi, AVPixelFormat::AV_PIX_FMT_VAAPI)),
            Self::Vulkan => Some((HwAccel::Vulkan, AVPixelFormat::AV_PIX_FMT_VULKAN)),
            Self::Nvenc | Self::Software => None,
        }
    }

    fn input_format(self) -> Pixel {
        match self.upload() {
            Some(_) => UPLOAD_FORMAT,
            None => SOFTWARE_FORMAT,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EncodeSettings {
    pub container: Container,
    pub codec: VideoCodec,
    pub width: NonZeroU32,
    pub height: NonZeroU32,
    pub frame_rate: FrameRate,
    pub sample_rate: NonZeroU32,
}

pub struct Encoder {
    path: PathBuf,
    output: format::context::Output,
    video: VideoTrack,
    audio: AudioTrack,
}

struct VideoTrack {
    encoder: encoder::Encoder,
    stream: usize,
    time_base: Rational,
    backend: EncodeBackend,
    name: &'static str,
    converter: Converter,
    upload: Option<HwFrames>,
    next_pts: i64,
}

struct Converter {
    context: scaling::Context,
    source: frame::Video,
    format: Pixel,
}

unsafe impl Send for Converter {}

struct HwFrames(*mut AVBufferRef);

unsafe impl Send for HwFrames {}

impl Drop for HwFrames {
    fn drop(&mut self) {
        unsafe { av_buffer_unref(&mut self.0) };
    }
}

struct AudioTrack {
    encoder: encoder::Encoder,
    stream: usize,
    time_base: Rational,
    name: &'static str,
    format: format::Sample,
    frame_frames: usize,
    small_last_frame: bool,
    pending: Vec<f32>,
    next_pts: i64,
}

impl Encoder {
    pub fn create(
        path: impl AsRef<Path>,
        settings: &EncodeSettings,
        backends: &[EncodeBackend],
    ) -> Result<Self, Error> {
        let path = path.as_ref().to_owned();
        let mut output =
            format::output_as(&path, settings.container.muxer()).map_err(|source| {
                Error::CreateOutput {
                    path: path.clone(),
                    source,
                }
            })?;
        let global_header = output
            .format()
            .flags()
            .contains(format::Flags::GLOBAL_HEADER);
        let (video_encoder, backend, name, upload) = backends
            .iter()
            .flat_map(|&backend| {
                settings
                    .codec
                    .encoders(backend)
                    .iter()
                    .map(move |&name| (backend, name))
            })
            .find_map(
                |(backend, name)| match open_video(settings, backend, name, global_header) {
                    Ok((encoder, upload)) => Some((encoder, backend, name, upload)),
                    Err(error) => {
                        tracing::debug!(encoder = name, %error, "video encoder unavailable");
                        None
                    }
                },
            )
            .ok_or(Error::NoVideoEncoder {
                codec: settings.codec,
            })?;
        let (audio_encoder, audio_name) = settings
            .container
            .audio_encoders()
            .iter()
            .find_map(|&name| {
                open_audio(settings.sample_rate, name, global_header)
                    .inspect_err(
                        |error| tracing::debug!(encoder = name, %error, "audio encoder unavailable"),
                    )
                    .ok()
                    .map(|encoder| (encoder, name))
            })
            .ok_or(Error::NoAudioEncoder {
                container: settings.container,
                sample_rate: settings.sample_rate,
            })?;
        let video_time_base = frame_time_base(settings.frame_rate);
        let video_stream = {
            let codec = encoder::find_by_name(name).ok_or(Error::NoVideoEncoder {
                codec: settings.codec,
            })?;
            let mut stream = output
                .add_stream(codec)
                .map_err(|source| Error::Mux { source })?;
            stream.set_parameters(&video_encoder);
            stream.set_time_base(video_time_base);
            stream.set_rate(video_time_base.invert());
            stream.set_avg_frame_rate(video_time_base.invert());
            stream.index()
        };
        let audio_time_base = Rational::new(1, settings.sample_rate.get() as i32);
        let audio_stream = {
            let codec = encoder::find_by_name(audio_name).ok_or(Error::NoAudioEncoder {
                container: settings.container,
                sample_rate: settings.sample_rate,
            })?;
            let mut stream = output
                .add_stream(codec)
                .map_err(|source| Error::Mux { source })?;
            stream.set_parameters(&audio_encoder);
            stream.set_time_base(audio_time_base);
            stream.index()
        };
        output
            .write_header_with(settings.container.header_options())
            .map_err(|source| Error::Mux { source })?;
        let converter = Converter::new(
            settings.width.get(),
            settings.height.get(),
            backend.input_format(),
        )
        .map_err(|source| Error::Encode { source })?;
        let audio_format = audio_encoder.format();
        let frame_frames = match audio_encoder.frame_size() {
            0 => FALLBACK_AUDIO_FRAME,
            size => size as usize,
        };
        let small_last_frame = encoder::find_by_name(audio_name).is_some_and(|codec| {
            codec.capabilities().intersects(
                codec::capabilities::Capabilities::SMALL_LAST_FRAME
                    | codec::capabilities::Capabilities::VARIABLE_FRAME_SIZE,
            )
        });
        let video_encoder = video_encoder.0.0;
        let audio_encoder = audio_encoder.0.0;
        tracing::info!(
            path = %path.display(),
            video = name,
            audio = audio_name,
            "export encoders opened"
        );
        Ok(Self {
            path,
            output,
            video: VideoTrack {
                encoder: video_encoder,
                stream: video_stream,
                time_base: video_time_base,
                backend,
                name,
                converter,
                upload,
                next_pts: 0,
            },
            audio: AudioTrack {
                encoder: audio_encoder,
                stream: audio_stream,
                time_base: audio_time_base,
                name: audio_name,
                format: audio_format,
                frame_frames,
                small_last_frame,
                pending: Vec::new(),
                next_pts: 0,
            },
        })
    }

    pub fn backend(&self) -> EncodeBackend {
        self.video.backend
    }

    pub fn video_encoder(&self) -> &'static str {
        self.video.name
    }

    pub fn audio_encoder(&self) -> &'static str {
        self.audio.name
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn push_video(&mut self, bgra: &[u8]) -> Result<(), Error> {
        let picture = self.video.prepare(bgra)?;
        self.video
            .encoder
            .send_frame(&picture)
            .map_err(|source| Error::Encode { source })?;
        drain(
            &mut self.video.encoder,
            self.video.stream,
            self.video.time_base,
            &mut self.output,
        )
    }

    pub fn push_audio(&mut self, interleaved: &[f32]) -> Result<(), Error> {
        self.audio.pending.extend_from_slice(interleaved);
        let whole = self.audio.frame_frames * AudioBuffer::CHANNELS;
        while self.audio.pending.len() >= whole {
            let block: Vec<f32> = self.audio.pending.drain(..whole).collect();
            self.audio.send(&block, &mut self.output)?;
        }
        Ok(())
    }

    pub fn finish(mut self) -> Result<PathBuf, Error> {
        self.video
            .encoder
            .send_eof()
            .map_err(|source| Error::Encode { source })?;
        drain(
            &mut self.video.encoder,
            self.video.stream,
            self.video.time_base,
            &mut self.output,
        )?;
        let mut rest = std::mem::take(&mut self.audio.pending);
        if !rest.is_empty() {
            if !self.audio.small_last_frame {
                rest.resize(self.audio.frame_frames * AudioBuffer::CHANNELS, 0.0);
            }
            self.audio.send(&rest, &mut self.output)?;
        }
        self.audio
            .encoder
            .send_eof()
            .map_err(|source| Error::Encode { source })?;
        drain(
            &mut self.audio.encoder,
            self.audio.stream,
            self.audio.time_base,
            &mut self.output,
        )?;
        self.output
            .write_trailer()
            .map_err(|source| Error::Mux { source })?;
        Ok(self.path)
    }
}

fn frame_time_base(frame_rate: FrameRate) -> Rational {
    Rational::new(
        frame_rate.denominator() as i32,
        frame_rate.numerator() as i32,
    )
}

fn open_video(
    settings: &EncodeSettings,
    backend: EncodeBackend,
    name: &str,
    global_header: bool,
) -> Result<(encoder::video::Encoder, Option<HwFrames>), Error> {
    let codec = encoder::find_by_name(name).ok_or(Error::NoVideoEncoder {
        codec: settings.codec,
    })?;
    let (width, height) = (settings.width.get(), settings.height.get());
    let time_base = frame_time_base(settings.frame_rate);
    let mut video = codec::Context::new_with_codec(codec)
        .encoder()
        .video()
        .map_err(|source| Error::Encode { source })?;
    video.set_width(width);
    video.set_height(height);
    video.set_time_base(time_base);
    video.set_frame_rate(Some(time_base.invert()));
    video.set_gop(gop_length(settings.frame_rate));
    video.set_colorspace(color::Space::BT709);
    video.set_color_range(color::Range::MPEG);
    video.set_color_primaries(color::Primaries::BT709);
    video.set_color_transfer_characteristic(color::TransferCharacteristic::BT709);
    if global_header {
        video.set_flags(codec::Flags::GLOBAL_HEADER);
    }
    let upload = match backend.upload() {
        Some((accel, hardware_format)) => {
            let frames = HwFrames::new(accel, hardware_format, width, height)?;
            video.set_format(Pixel::from(hardware_format));
            video.set_global_quality(settings.codec.hardware_quality());
            unsafe { (*video.as_mut_ptr()).hw_frames_ctx = av_buffer_ref(frames.0) };
            Some(frames)
        }
        None => {
            video.set_format(SOFTWARE_FORMAT);
            None
        }
    };
    let opened = video
        .open_as_with(codec, settings.codec.options(backend))
        .map_err(|source| Error::Encode { source })?;
    Ok((opened, upload))
}

fn gop_length(frame_rate: FrameRate) -> u32 {
    let per_second = frame_rate.as_f64().round().max(1.) as u32;
    per_second.saturating_mul(KEYFRAME_SECONDS)
}

fn open_audio(
    sample_rate: NonZeroU32,
    name: &str,
    global_header: bool,
) -> Result<encoder::audio::Encoder, ffmpeg_next::Error> {
    let codec = encoder::find_by_name(name).ok_or(ffmpeg_next::Error::EncoderNotFound)?;
    let format = audio_format(codec, sample_rate)?;
    let mut audio = codec::Context::new_with_codec(codec).encoder().audio()?;
    audio.set_rate(sample_rate.get() as i32);
    audio.set_channel_layout(ChannelLayout::STEREO);
    audio.set_format(format);
    audio.set_bit_rate(AUDIO_BIT_RATE);
    audio.set_time_base(Rational::new(1, sample_rate.get() as i32));
    if global_header {
        audio.set_flags(codec::Flags::GLOBAL_HEADER);
    }
    audio.open_as(codec)
}

fn audio_format(
    codec: Codec,
    sample_rate: NonZeroU32,
) -> Result<format::Sample, ffmpeg_next::Error> {
    let audio = codec.audio()?;
    let rate_supported = audio.rates().is_none_or(|mut rates| {
        rates.any(|rate| u32::try_from(rate).is_ok_and(|rate| rate == sample_rate.get()))
    });
    if !rate_supported {
        return Err(ffmpeg_next::Error::PatchWelcome);
    }
    let planar = format::Sample::F32(sample::Type::Planar);
    let packed = format::Sample::F32(sample::Type::Packed);
    let formats: Vec<format::Sample> = audio.formats().map_or_else(Vec::new, Iterator::collect);
    [planar, packed]
        .into_iter()
        .find(|format| formats.contains(format))
        .ok_or(ffmpeg_next::Error::PatchWelcome)
}

fn drain(
    encoder: &mut encoder::Encoder,
    stream: usize,
    time_base: Rational,
    output: &mut format::context::Output,
) -> Result<(), Error> {
    let stream_time_base = output
        .stream(stream)
        .map(|stream| stream.time_base())
        .ok_or(Error::Mux {
            source: ffmpeg_next::Error::StreamNotFound,
        })?;
    let mut packet = Packet::empty();
    loop {
        match encoder.receive_packet(&mut packet) {
            Ok(()) => {
                packet.set_stream(stream);
                packet.rescale_ts(time_base, stream_time_base);
                packet
                    .write_interleaved(output)
                    .map_err(|source| Error::Mux { source })?;
            }
            Err(ffmpeg_next::Error::Eof) => return Ok(()),
            Err(error) if crate::decode::is_again(&error) => return Ok(()),
            Err(source) => return Err(Error::Encode { source }),
        }
    }
}

impl VideoTrack {
    fn prepare(&mut self, bgra: &[u8]) -> Result<frame::Video, Error> {
        let converted = self.converter.convert(bgra)?;
        let mut picture = match &self.upload {
            Some(frames) => frames
                .upload(&converted)
                .map_err(|source| Error::HardwareUpload { source })?,
            None => converted,
        };
        picture.set_pts(Some(self.next_pts));
        self.next_pts += 1;
        Ok(picture)
    }
}

impl Converter {
    fn new(width: u32, height: u32, format: Pixel) -> Result<Self, ffmpeg_next::Error> {
        let mut context = scaling::Context::get(
            INPUT_FORMAT,
            width,
            height,
            format,
            width,
            height,
            scaling::Flags::BICUBIC | scaling::Flags::ACCURATE_RND | scaling::Flags::FULL_CHR_H_INP,
        )?;
        let applied = unsafe {
            let coefficients = sws_getCoefficients(SWS_CS_ITU709);
            sws_setColorspaceDetails(
                context.as_mut_ptr(),
                coefficients,
                FULL_RANGE,
                coefficients,
                LIMITED_RANGE,
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
            source: frame::Video::new(INPUT_FORMAT, width, height),
            format,
        })
    }

    fn convert(&mut self, bgra: &[u8]) -> Result<frame::Video, Error> {
        let (width, height) = (self.source.width(), self.source.height());
        let row = width as usize * BYTES_PER_PIXEL;
        let expected = row * height as usize;
        if bgra.len() != expected {
            return Err(Error::FrameSize {
                expected,
                actual: bgra.len(),
            });
        }
        let stride = self.source.stride(0);
        let data = self.source.data_mut(0);
        for (target, source) in data.chunks_mut(stride).zip(bgra.chunks_exact(row)) {
            target[..row].copy_from_slice(source);
        }
        let mut converted = frame::Video::new(self.format, width, height);
        self.context
            .run(&self.source, &mut converted)
            .map_err(|source| Error::Encode { source })?;
        tag_bt709(&mut converted);
        Ok(converted)
    }
}

fn tag_bt709(picture: &mut frame::Video) {
    picture.set_color_range(color::Range::MPEG);
    picture.set_color_space(color::Space::BT709);
    picture.set_color_primaries(color::Primaries::BT709);
    picture.set_color_transfer_characteristic(color::TransferCharacteristic::BT709);
}

impl HwFrames {
    fn new(
        accel: HwAccel,
        hardware_format: AVPixelFormat,
        width: u32,
        height: u32,
    ) -> Result<Self, Error> {
        let device = hw::device_reference(accel).ok_or(Error::HardwareDevice { accel })?;
        let mut device = device;
        let frames = unsafe { av_hwframe_ctx_alloc(device) };
        unsafe { av_buffer_unref(&mut device) };
        if frames.is_null() {
            return Err(Error::HardwareDevice { accel });
        }
        let frames = Self(frames);
        let initialised = unsafe {
            let context = (*frames.0).data.cast::<AVHWFramesContext>();
            (*context).format = hardware_format;
            (*context).sw_format = AVPixelFormat::from(UPLOAD_FORMAT);
            (*context).width = width as c_int;
            (*context).height = height as c_int;
            (*context).initial_pool_size = HARDWARE_POOL_FRAMES;
            av_hwframe_ctx_init(frames.0)
        };
        if initialised < 0 {
            return Err(Error::HardwareUpload {
                source: ffmpeg_next::Error::from(initialised),
            });
        }
        Ok(frames)
    }

    fn upload(&self, software: &frame::Video) -> Result<frame::Video, ffmpeg_next::Error> {
        let mut hardware = frame::Video::empty();
        let allocated = unsafe { av_hwframe_get_buffer(self.0, hardware.as_mut_ptr(), 0) };
        if allocated < 0 {
            return Err(ffmpeg_next::Error::from(allocated));
        }
        let transferred =
            unsafe { av_hwframe_transfer_data(hardware.as_mut_ptr(), software.as_ptr(), 0) };
        if transferred < 0 {
            return Err(ffmpeg_next::Error::from(transferred));
        }
        tag_bt709(&mut hardware);
        Ok(hardware)
    }
}

impl AudioTrack {
    fn send(
        &mut self,
        interleaved: &[f32],
        output: &mut format::context::Output,
    ) -> Result<(), Error> {
        let frames = interleaved.len() / AudioBuffer::CHANNELS;
        let mut block = frame::Audio::new(self.format, frames, ChannelLayout::STEREO);
        block.set_rate(self.time_base.denominator() as u32);
        if self.format.is_planar() {
            for channel in 0..AudioBuffer::CHANNELS {
                let (slots, _) = block
                    .data_mut(channel)
                    .as_chunks_mut::<{ size_of::<f32>() }>();
                let samples = interleaved
                    .iter()
                    .skip(channel)
                    .step_by(AudioBuffer::CHANNELS);
                for (slot, sample) in slots.iter_mut().zip(samples) {
                    *slot = sample.to_ne_bytes();
                }
            }
        } else {
            let (slots, _) = block.data_mut(0).as_chunks_mut::<{ size_of::<f32>() }>();
            for (slot, sample) in slots.iter_mut().zip(interleaved) {
                *slot = sample.to_ne_bytes();
            }
        }
        block.set_pts(Some(self.next_pts));
        self.next_pts += frames as i64;
        self.encoder
            .send_frame(&block)
            .map_err(|source| Error::Encode { source })?;
        drain(&mut self.encoder, self.stream, self.time_base, output)
    }
}

#[cfg(test)]
mod tests {
    use tessera_timeline::Stream;

    use super::*;
    use crate::{VideoDecoder, fixture::Fixture, probe};

    const WIDTH: u32 = 128;
    const HEIGHT: u32 = 96;
    const FRAMES: usize = 30;
    const MID_GREY: [u8; 4] = [128, 128, 128, 255];

    fn settings(codec: VideoCodec, container: Container, sample_rate: u32) -> EncodeSettings {
        EncodeSettings {
            container,
            codec,
            width: NonZeroU32::new(WIDTH).unwrap(),
            height: NonZeroU32::new(HEIGHT).unwrap(),
            frame_rate: FrameRate::FPS_25,
            sample_rate: NonZeroU32::new(sample_rate).unwrap(),
        }
    }

    fn solid(bgra: [u8; 4]) -> Vec<u8> {
        bgra.repeat((WIDTH * HEIGHT) as usize)
    }

    fn encode_grey(encoder: &mut Encoder, sample_rate: u32) {
        let per_frame = sample_rate as usize / 25 * AudioBuffer::CHANNELS;
        for _ in 0..FRAMES {
            encoder.push_video(&solid(MID_GREY)).unwrap();
            encoder.push_audio(&vec![0.25; per_frame]).unwrap();
        }
    }

    fn created(
        name: &str,
        settings: &EncodeSettings,
        backends: &[EncodeBackend],
    ) -> Option<(Fixture, Encoder)> {
        let output = Fixture::reserve(name, settings.container.extension());
        match Encoder::create(output.path(), settings, backends) {
            Ok(encoder) => Some((output, encoder)),
            Err(error @ Error::NoVideoEncoder { .. }) => {
                eprintln!("skipping {name}: {error}");
                None
            }
            Err(error) => panic!("{name}: {error}"),
        }
    }

    fn check_output(path: &Path, codec: &str, audio_codec: &str) {
        let info = probe(path).unwrap();
        let video = info.video().next().unwrap();
        let audio = info.audio().next().unwrap();
        let seconds = info.duration.unwrap().as_seconds_f64();

        assert_eq!(video.codec, codec);
        assert_eq!((video.width.get(), video.height.get()), (WIDTH, HEIGHT));
        assert_eq!(video.frame_rate, Some(FrameRate::FPS_25));
        assert_eq!(audio.codec, audio_codec);
        assert_eq!(audio.channels.get(), 2);
        assert!((seconds - FRAMES as f64 / 25.).abs() < 0.1, "{seconds}");
        assert!(
            info.streams
                .iter()
                .all(|stream| matches!(stream, Stream::Video(_) | Stream::Audio(_)))
        );

        let mut decoder = VideoDecoder::open_with(path, video.index, &[]).unwrap();
        let frame = decoder.frame_at(tessera_timeline::Time::ZERO).unwrap();
        let centre = ((HEIGHT / 2 * WIDTH + WIDTH / 2) * 4) as usize;
        for (channel, &expected) in frame.bgra[centre..centre + 3].iter().zip(&MID_GREY) {
            assert!(
                channel.abs_diff(expected) <= 3,
                "{:?}",
                &frame.bgra[centre..centre + 4]
            );
        }
    }

    #[test]
    fn software_h264_in_mp4_round_trips_picture_and_sound() {
        let settings = settings(VideoCodec::H264, Container::Mp4, 48_000);
        let Some((output, mut encoder)) =
            created("export-h264", &settings, &[EncodeBackend::Software])
        else {
            return;
        };

        assert_eq!(encoder.backend(), EncodeBackend::Software);
        assert_eq!(encoder.video_encoder(), "libx264");

        encode_grey(&mut encoder, 48_000);
        let written = encoder.finish().unwrap();

        assert_eq!(written, output.path());
        check_output(output.path(), "h264", "aac");
    }

    #[test]
    fn every_codec_and_container_has_a_software_encoder() {
        for codec in VideoCodec::ALL {
            for container in Container::ALL {
                let settings = settings(codec, container, 48_000);
                let name = format!("export-{}-{}", codec.label(), container.extension());
                let Some((output, mut encoder)) =
                    created(&name, &settings, &[EncodeBackend::Software])
                else {
                    continue;
                };
                let audio = match encoder.audio_encoder() {
                    "libopus" => "opus",
                    other => other,
                }
                .to_owned();
                encode_grey(&mut encoder, 48_000);
                encoder.finish().unwrap();
                let video = match codec {
                    VideoCodec::H264 => "h264",
                    VideoCodec::Hevc => "hevc",
                    VideoCodec::Av1 => "av1",
                };
                check_output(output.path(), video, &audio);
            }
        }
    }

    #[test]
    fn matroska_falls_back_to_aac_where_opus_lacks_the_sample_rate() {
        let settings = settings(VideoCodec::H264, Container::Matroska, 44_100);
        let Some((output, mut encoder)) =
            created("export-44k", &settings, &[EncodeBackend::Software])
        else {
            return;
        };

        assert_eq!(encoder.audio_encoder(), "aac");

        encode_grey(&mut encoder, 44_100);
        encoder.finish().unwrap();
        check_output(output.path(), "h264", "aac");
    }

    #[test]
    fn hardware_encoders_round_trip_where_the_machine_has_them() {
        for backend in [
            EncodeBackend::Vaapi,
            EncodeBackend::Nvenc,
            EncodeBackend::Vulkan,
        ] {
            let settings = settings(VideoCodec::H264, Container::Mp4, 48_000);
            let name = format!("export-{}", backend.label());
            let Some((output, mut encoder)) = created(&name, &settings, &[backend]) else {
                continue;
            };
            assert_eq!(encoder.backend(), backend);
            encode_grey(&mut encoder, 48_000);
            encoder.finish().unwrap();
            check_output(output.path(), "h264", "aac");
        }
    }

    #[test]
    fn a_frame_of_the_wrong_size_is_refused() {
        let settings = settings(VideoCodec::H264, Container::Mp4, 48_000);
        let Some((_output, mut encoder)) =
            created("export-size", &settings, &[EncodeBackend::Software])
        else {
            return;
        };

        let refused = encoder.push_video(&[0; 16]);

        assert!(matches!(
            refused,
            Err(Error::FrameSize {
                expected,
                actual: 16
            }) if expected == (WIDTH * HEIGHT * 4) as usize
        ));
    }

    #[test]
    fn no_backend_means_no_encoder() {
        let settings = settings(VideoCodec::H264, Container::Mp4, 48_000);
        let output = Fixture::reserve("export-none", "mp4");

        let refused = Encoder::create(output.path(), &settings, &[]);

        assert!(matches!(
            refused,
            Err(Error::NoVideoEncoder {
                codec: VideoCodec::H264
            })
        ));
    }
}
