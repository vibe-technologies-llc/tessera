use std::{
    num::NonZeroU32,
    path::{Path, PathBuf},
};

use ffmpeg_next::{
    ChannelLayout, Dictionary, Packet, Rational, codec, encoder,
    format::{self, sample},
    frame,
};
use tessera_timeline::FrameRate;

pub const WIDTH: u32 = 128;
pub const HEIGHT: u32 = 96;
pub const FRAME_COUNT: i64 = 20;
pub const FRAME_RATE: FrameRate = FrameRate::FPS_25;
pub const AUDIO_SAMPLE_RATE: NonZeroU32 = NonZeroU32::new(44_100).unwrap();
pub const AUDIO_CHANNELS: u16 = 1;
const AUDIO_FORMAT: format::Sample = format::Sample::I16(sample::Type::Packed);
const KEYFRAME_INTERVAL: u32 = 5;
const NEUTRAL_CHROMA: u8 = 128;
const SCENE_CUT_DETECTION_OFF: &str = "1000000000";
const H264_ENCODER: &str = "libx264";
const H264_FIXED_GOP: &str = "keyint=5:min-keyint=5:scenecut=0:open-gop=0:log=-1";

pub fn luma(index: i64) -> u8 {
    u8::try_from(20 + index * 10).expect("fixture luma stays in range")
}

pub fn audio_level(index: i64) -> f32 {
    0.04 * (index + 1) as f32
}

pub fn audio_frame_start(index: i64, sample_rate: NonZeroU32) -> i64 {
    FRAME_RATE.frame_to_time(index).to_samples(sample_rate)
}

pub fn expected_grey(index: i64) -> u8 {
    let limited_range_black = 16;
    let limited_range_span = 219;
    let scaled = (i64::from(luma(index)) - limited_range_black) * 255 / limited_range_span;
    u8::try_from(scaled).expect("fixture grey stays in range")
}

pub struct Fixture {
    path: PathBuf,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Streams {
    VideoAndAudio,
    VideoOnly,
}

#[derive(Clone, Copy)]
struct Recipe {
    streams: Streams,
    codec: VideoCodec,
    frame_rate: FrameRate,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum VideoCodec {
    Mpeg4,
    H264,
}

impl Fixture {
    pub fn generate(name: &str) -> Self {
        Self::generate_with(
            name,
            Recipe {
                streams: Streams::VideoAndAudio,
                codec: VideoCodec::Mpeg4,
                frame_rate: FRAME_RATE,
            },
        )
    }

    pub fn generate_without_audio(name: &str) -> Self {
        Self::generate_at_rate(name, FRAME_RATE)
    }

    pub fn generate_at_rate(name: &str, frame_rate: FrameRate) -> Self {
        Self::generate_with(
            name,
            Recipe {
                streams: Streams::VideoOnly,
                codec: VideoCodec::Mpeg4,
                frame_rate,
            },
        )
    }

    pub fn generate_h264(name: &str) -> Option<Self> {
        crate::init().unwrap();
        encoder::find_by_name(H264_ENCODER)?;
        Some(Self::generate_with(
            name,
            Recipe {
                streams: Streams::VideoOnly,
                codec: VideoCodec::H264,
                frame_rate: FRAME_RATE,
            },
        ))
    }

    fn generate_with(name: &str, recipe: Recipe) -> Self {
        crate::init().unwrap();
        let path =
            std::env::temp_dir().join(format!("tessera-media-{}-{name}.mkv", std::process::id()));
        encode(&path, recipe).unwrap();
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_file(&self.path).ok();
    }
}

struct Muxed<E> {
    encoder: E,
    stream: usize,
    time_base: Rational,
}

fn encode(path: &Path, recipe: Recipe) -> Result<(), ffmpeg_next::Error> {
    let mut output = format::output(path)?;
    let mut video = add_video(&mut output, recipe.codec, recipe.frame_rate)?;
    let mut audio = match recipe.streams {
        Streams::VideoAndAudio => Some(add_audio(&mut output)?),
        Streams::VideoOnly => None,
    };
    output.write_header()?;
    for index in 0..FRAME_COUNT {
        video.encoder.send_frame(&picture(index))?;
        drain(
            &mut video.encoder,
            video.stream,
            video.time_base,
            &mut output,
        )?;
        if let Some(audio) = &mut audio {
            audio.encoder.send_frame(&audio_block(index))?;
            drain(
                &mut audio.encoder,
                audio.stream,
                audio.time_base,
                &mut output,
            )?;
        }
    }
    video.encoder.send_eof()?;
    drain(
        &mut video.encoder,
        video.stream,
        video.time_base,
        &mut output,
    )?;
    if let Some(audio) = &mut audio {
        audio.encoder.send_eof()?;
        drain(
            &mut audio.encoder,
            audio.stream,
            audio.time_base,
            &mut output,
        )?;
    }
    output.write_trailer()
}

fn drain(
    encoder: &mut encoder::Encoder,
    stream: usize,
    time_base: Rational,
    output: &mut format::context::Output,
) -> Result<(), ffmpeg_next::Error> {
    let stream_time_base = output.stream(stream).expect("stream was added").time_base();
    let mut packet = Packet::empty();
    while encoder.receive_packet(&mut packet).is_ok() {
        packet.set_stream(stream);
        packet.rescale_ts(time_base, stream_time_base);
        packet.write_interleaved(output)?;
    }
    Ok(())
}

fn wants_global_header(output: &format::context::Output) -> bool {
    output
        .format()
        .flags()
        .contains(format::Flags::GLOBAL_HEADER)
}

fn add_video(
    output: &mut format::context::Output,
    video_codec: VideoCodec,
    frame_rate: FrameRate,
) -> Result<Muxed<encoder::video::Encoder>, ffmpeg_next::Error> {
    let time_base = Rational::new(
        frame_rate.denominator() as i32,
        frame_rate.numerator() as i32,
    );
    let (codec, options) = match video_codec {
        VideoCodec::Mpeg4 => (
            encoder::find(codec::Id::MPEG4),
            Dictionary::from_iter([("sc_threshold", SCENE_CUT_DETECTION_OFF)]),
        ),
        VideoCodec::H264 => (
            encoder::find_by_name(H264_ENCODER),
            Dictionary::from_iter([("x264-params", H264_FIXED_GOP)]),
        ),
    };
    let codec = codec.ok_or(ffmpeg_next::Error::EncoderNotFound)?;
    let mut video = codec::Context::new_with_codec(codec).encoder().video()?;
    video.set_width(WIDTH);
    video.set_height(HEIGHT);
    video.set_format(format::Pixel::YUV420P);
    video.set_time_base(time_base);
    video.set_frame_rate(Some(time_base.invert()));
    video.set_gop(KEYFRAME_INTERVAL);
    if wants_global_header(output) {
        video.set_flags(codec::Flags::GLOBAL_HEADER);
    }
    let encoder = video.open_as_with(codec, options)?;
    let mut stream = output.add_stream(codec)?;
    stream.set_parameters(&encoder);
    Ok(Muxed {
        encoder,
        stream: stream.index(),
        time_base,
    })
}

fn add_audio(
    output: &mut format::context::Output,
) -> Result<Muxed<encoder::audio::Encoder>, ffmpeg_next::Error> {
    let time_base = Rational::new(1, AUDIO_SAMPLE_RATE.get() as i32);
    let codec = encoder::find(codec::Id::PCM_S16LE).ok_or(ffmpeg_next::Error::EncoderNotFound)?;
    let mut audio = codec::Context::new_with_codec(codec).encoder().audio()?;
    audio.set_rate(AUDIO_SAMPLE_RATE.get() as i32);
    audio.set_channel_layout(audio_layout());
    audio.set_format(AUDIO_FORMAT);
    audio.set_time_base(time_base);
    if wants_global_header(output) {
        audio.set_flags(codec::Flags::GLOBAL_HEADER);
    }
    let encoder = audio.open_as(codec)?;
    let mut stream = output.add_stream(codec)?;
    stream.set_parameters(&encoder);
    Ok(Muxed {
        encoder,
        stream: stream.index(),
        time_base,
    })
}

fn audio_layout() -> ChannelLayout {
    ChannelLayout::default(i32::from(AUDIO_CHANNELS))
}

fn picture(index: i64) -> frame::Video {
    let mut picture = frame::Video::new(format::Pixel::YUV420P, WIDTH, HEIGHT);
    picture.data_mut(0).fill(luma(index));
    picture.data_mut(1).fill(NEUTRAL_CHROMA);
    picture.data_mut(2).fill(NEUTRAL_CHROMA);
    picture.set_pts(Some(index));
    picture
}

fn audio_block(index: i64) -> frame::Audio {
    let start = audio_frame_start(index, AUDIO_SAMPLE_RATE);
    let end = audio_frame_start(index + 1, AUDIO_SAMPLE_RATE);
    let samples = usize::try_from(end - start).expect("fixture frames move forward");
    let mut block = frame::Audio::new(AUDIO_FORMAT, samples, audio_layout());
    block.set_rate(AUDIO_SAMPLE_RATE.get());
    let level = (audio_level(index) * f32::from(i16::MAX)).round() as i16;
    block.plane_mut::<i16>(0).fill(level);
    block.set_pts(Some(start));
    block
}
