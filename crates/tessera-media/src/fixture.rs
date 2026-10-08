use std::{
    num::NonZeroU32,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
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
pub const VIDEO_STREAM: usize = 0;
pub const AUDIO_STREAM: usize = 1;
pub const SECOND_AUDIO_STREAM: usize = 2;
pub const AUDIO_ONLY_STREAM: usize = 0;
pub const STILL_STREAM: usize = 0;
const PACKED_FORMAT: format::Sample = format::Sample::I16(sample::Type::Packed);
const PLANAR_FORMAT: format::Sample = format::Sample::I16(sample::Type::Planar);
const KEYFRAME_INTERVAL: u32 = 5;
const B_FRAMES: i32 = 2;
const NEUTRAL_CHROMA: u8 = 128;
const SCENE_CUT_DETECTION_OFF: &str = "1000000000";
const H264_ENCODER: &str = "libx264";
const H264_FIXED_GOP: &str = "keyint=5:min-keyint=5:scenecut=0:open-gop=0:log=-1";

static GENERATED: AtomicU64 = AtomicU64::new(0);

pub fn luma(index: i64) -> u8 {
    u8::try_from(20 + index * 10).expect("fixture luma stays in range")
}

pub fn audio_level(index: i64) -> f32 {
    0.04 * (index + 1) as f32
}

pub fn right_level(index: i64) -> f32 {
    -audio_level(index) / 2.0
}

pub fn second_stream_level(index: i64) -> f32 {
    -audio_level(index)
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AudioGap {
    pub after: i64,
    pub frames: i64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum VideoCodec {
    Mpeg4,
    Mpeg4WithBFrames,
    H264,
    Png,
}

impl VideoCodec {
    fn pixel_format(self) -> format::Pixel {
        match self {
            Self::Png => format::Pixel::RGB24,
            Self::Mpeg4 | Self::Mpeg4WithBFrames | Self::H264 => format::Pixel::YUV420P,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AudioTrack {
    Mono,
    SecondMono,
    PlanarStereo,
}

impl AudioTrack {
    fn layout(self) -> ChannelLayout {
        match self {
            Self::Mono | Self::SecondMono => ChannelLayout::default(i32::from(AUDIO_CHANNELS)),
            Self::PlanarStereo => ChannelLayout::STEREO,
        }
    }

    fn format(self) -> format::Sample {
        match self {
            Self::Mono | Self::SecondMono => PACKED_FORMAT,
            Self::PlanarStereo => PLANAR_FORMAT,
        }
    }

    fn codec(self) -> codec::Id {
        match self {
            Self::Mono | Self::SecondMono => codec::Id::PCM_S16LE,
            Self::PlanarStereo => codec::Id::PCM_S16LE_PLANAR,
        }
    }

    fn levels(self, index: i64) -> Vec<f32> {
        match self {
            Self::Mono => vec![audio_level(index)],
            Self::SecondMono => vec![second_stream_level(index)],
            Self::PlanarStereo => vec![audio_level(index), right_level(index)],
        }
    }
}

#[derive(Clone)]
struct Recipe {
    extension: &'static str,
    video: Option<VideoCodec>,
    audio: Vec<AudioTrack>,
    frame_rate: FrameRate,
    frames: i64,
    start_frames: i64,
    audio_delay_frames: i64,
    audio_gap: Option<AudioGap>,
}

impl Recipe {
    fn video_and_audio() -> Self {
        Self {
            extension: "mkv",
            video: Some(VideoCodec::Mpeg4),
            audio: vec![AudioTrack::Mono],
            frame_rate: FRAME_RATE,
            frames: FRAME_COUNT,
            start_frames: 0,
            audio_delay_frames: 0,
            audio_gap: None,
        }
    }

    fn video_only(video: VideoCodec, frame_rate: FrameRate) -> Self {
        Self {
            video: Some(video),
            audio: Vec::new(),
            frame_rate,
            ..Self::video_and_audio()
        }
    }

    fn audio_only(extension: &'static str, track: AudioTrack) -> Self {
        Self {
            extension,
            video: None,
            audio: vec![track],
            ..Self::video_and_audio()
        }
    }
}

impl Fixture {
    pub fn generate(name: &str) -> Self {
        Self::generate_with(name, Recipe::video_and_audio())
    }

    pub fn generate_starting_late(name: &str, start_frames: i64, audio_delay_frames: i64) -> Self {
        Self::generate_with(
            name,
            Recipe {
                start_frames,
                audio_delay_frames,
                ..Recipe::video_and_audio()
            },
        )
    }

    pub fn generate_without_audio(name: &str) -> Self {
        Self::generate_at_rate(name, FRAME_RATE)
    }

    pub fn generate_at_rate(name: &str, frame_rate: FrameRate) -> Self {
        Self::generate_with(name, Recipe::video_only(VideoCodec::Mpeg4, frame_rate))
    }

    pub fn generate_with_b_frames(name: &str) -> Self {
        Self::generate_with(
            name,
            Recipe::video_only(VideoCodec::Mpeg4WithBFrames, FRAME_RATE),
        )
    }

    pub fn generate_h264(name: &str) -> Option<Self> {
        crate::init().unwrap();
        encoder::find_by_name(H264_ENCODER)?;
        Some(Self::generate_with(
            name,
            Recipe::video_only(VideoCodec::H264, FRAME_RATE),
        ))
    }

    pub fn generate_still(name: &str) -> Self {
        Self::generate_with(
            name,
            Recipe {
                extension: "png",
                frames: 1,
                ..Recipe::video_only(VideoCodec::Png, FRAME_RATE)
            },
        )
    }

    pub fn generate_audio_only(name: &str) -> Self {
        Self::generate_with(name, Recipe::audio_only("mka", AudioTrack::Mono))
    }

    pub fn generate_planar_stereo(name: &str) -> Self {
        Self::generate_with(name, Recipe::audio_only("nut", AudioTrack::PlanarStereo))
    }

    pub fn generate_with_two_audio_streams(name: &str) -> Self {
        Self::generate_with(
            name,
            Recipe {
                audio: vec![AudioTrack::Mono, AudioTrack::SecondMono],
                ..Recipe::video_and_audio()
            },
        )
    }

    pub fn generate_with_audio_gap(name: &str, audio_gap: AudioGap) -> Self {
        Self::generate_with(
            name,
            Recipe {
                audio_gap: Some(audio_gap),
                ..Recipe::video_and_audio()
            },
        )
    }

    fn generate_with(name: &str, recipe: Recipe) -> Self {
        crate::init().unwrap();
        let path = std::env::temp_dir().join(format!(
            "tessera-media-{}-{}-{name}.{}",
            std::process::id(),
            GENERATED.fetch_add(1, Ordering::Relaxed),
            recipe.extension
        ));
        encode(&path, &recipe).unwrap();
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

struct Muxed {
    encoder: encoder::Encoder,
    stream: usize,
    time_base: Rational,
}

impl Muxed {
    fn send(
        &mut self,
        frame: &frame::Frame,
        output: &mut format::context::Output,
    ) -> Result<(), ffmpeg_next::Error> {
        self.encoder.send_frame(frame)?;
        self.drain(output)
    }

    fn finish(&mut self, output: &mut format::context::Output) -> Result<(), ffmpeg_next::Error> {
        self.encoder.send_eof()?;
        self.drain(output)
    }

    fn drain(&mut self, output: &mut format::context::Output) -> Result<(), ffmpeg_next::Error> {
        let stream_time_base = output
            .stream(self.stream)
            .expect("stream was added")
            .time_base();
        let mut packet = Packet::empty();
        while self.encoder.receive_packet(&mut packet).is_ok() {
            packet.set_stream(self.stream);
            packet.rescale_ts(self.time_base, stream_time_base);
            packet.write_interleaved(output)?;
        }
        Ok(())
    }
}

fn encode(path: &Path, recipe: &Recipe) -> Result<(), ffmpeg_next::Error> {
    let mut output = format::output(path)?;
    let mut video = recipe
        .video
        .map(|codec| add_video(&mut output, codec, recipe.frame_rate))
        .transpose()?;
    let mut audio = recipe
        .audio
        .iter()
        .map(|&track| add_audio(&mut output, track).map(|muxed| (track, muxed)))
        .collect::<Result<Vec<_>, _>>()?;
    output.write_header()?;
    for index in 0..recipe.frames {
        if let (Some(video), Some(codec)) = (&mut video, recipe.video) {
            let picture = picture(index, recipe.start_frames, codec.pixel_format());
            video.send(&picture, &mut output)?;
        }
        for (track, muxed) in &mut audio {
            let offset = recipe.start_frames
                + recipe.audio_delay_frames
                + recipe
                    .audio_gap
                    .filter(|gap| index >= gap.after)
                    .map_or(0, |gap| gap.frames);
            muxed.send(&audio_block(*track, index, offset), &mut output)?;
        }
    }
    if let Some(video) = &mut video {
        video.finish(&mut output)?;
    }
    for (_, muxed) in &mut audio {
        muxed.finish(&mut output)?;
    }
    output.write_trailer()
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
) -> Result<Muxed, ffmpeg_next::Error> {
    let time_base = Rational::new(
        frame_rate.denominator() as i32,
        frame_rate.numerator() as i32,
    );
    let (codec, options) = match video_codec {
        VideoCodec::Mpeg4 | VideoCodec::Mpeg4WithBFrames => (
            encoder::find(codec::Id::MPEG4),
            Dictionary::from_iter([("sc_threshold", SCENE_CUT_DETECTION_OFF)]),
        ),
        VideoCodec::H264 => (
            encoder::find_by_name(H264_ENCODER),
            Dictionary::from_iter([("x264-params", H264_FIXED_GOP)]),
        ),
        VideoCodec::Png => (encoder::find(codec::Id::PNG), Dictionary::new()),
    };
    let codec = codec.ok_or(ffmpeg_next::Error::EncoderNotFound)?;
    let mut video = codec::Context::new_with_codec(codec).encoder().video()?;
    video.set_width(WIDTH);
    video.set_height(HEIGHT);
    video.set_format(video_codec.pixel_format());
    video.set_time_base(time_base);
    video.set_frame_rate(Some(time_base.invert()));
    video.set_gop(KEYFRAME_INTERVAL);
    if video_codec == VideoCodec::Mpeg4WithBFrames {
        video.set_max_b_frames(B_FRAMES as usize);
    }
    if wants_global_header(output) {
        video.set_flags(codec::Flags::GLOBAL_HEADER);
    }
    let encoder = video.open_as_with(codec, options)?;
    let mut stream = output.add_stream(codec)?;
    stream.set_parameters(&encoder);
    Ok(Muxed {
        encoder: encoder.0.0,
        stream: stream.index(),
        time_base,
    })
}

fn add_audio(
    output: &mut format::context::Output,
    track: AudioTrack,
) -> Result<Muxed, ffmpeg_next::Error> {
    let time_base = Rational::new(1, AUDIO_SAMPLE_RATE.get() as i32);
    let codec = encoder::find(track.codec()).ok_or(ffmpeg_next::Error::EncoderNotFound)?;
    let mut audio = codec::Context::new_with_codec(codec).encoder().audio()?;
    audio.set_rate(AUDIO_SAMPLE_RATE.get() as i32);
    audio.set_channel_layout(track.layout());
    audio.set_format(track.format());
    audio.set_time_base(time_base);
    if wants_global_header(output) {
        audio.set_flags(codec::Flags::GLOBAL_HEADER);
    }
    let encoder = audio.open_as(codec)?;
    let mut stream = output.add_stream(codec)?;
    stream.set_parameters(&encoder);
    Ok(Muxed {
        encoder: encoder.0.0,
        stream: stream.index(),
        time_base,
    })
}

fn picture(index: i64, start_frames: i64, pixel_format: format::Pixel) -> frame::Video {
    let mut picture = frame::Video::new(pixel_format, WIDTH, HEIGHT);
    if pixel_format == format::Pixel::RGB24 {
        picture.data_mut(0).fill(expected_grey(index));
    } else {
        picture.data_mut(0).fill(luma(index));
        picture.data_mut(1).fill(NEUTRAL_CHROMA);
        picture.data_mut(2).fill(NEUTRAL_CHROMA);
    }
    picture.set_pts(Some(start_frames + index));
    picture
}

fn audio_block(track: AudioTrack, index: i64, offset_frames: i64) -> frame::Audio {
    let start = audio_frame_start(index, AUDIO_SAMPLE_RATE);
    let end = audio_frame_start(index + 1, AUDIO_SAMPLE_RATE);
    let samples = usize::try_from(end - start).expect("fixture frames move forward");
    let mut block = frame::Audio::new(track.format(), samples, track.layout());
    block.set_rate(AUDIO_SAMPLE_RATE.get());
    let levels: Vec<i16> = track
        .levels(index)
        .into_iter()
        .map(|level| (level * f32::from(i16::MAX)).round() as i16)
        .collect();
    if track.format().is_planar() {
        for (plane, &level) in levels.iter().enumerate() {
            block.plane_mut::<i16>(plane).fill(level);
        }
    } else {
        let (slots, _) = block.data_mut(0).as_chunks_mut::<{ size_of::<i16>() }>();
        for (slot, &level) in slots.iter_mut().zip(levels.iter().cycle()) {
            *slot = level.to_ne_bytes();
        }
    }
    block.set_pts(Some(audio_frame_start(
        index + offset_frames,
        AUDIO_SAMPLE_RATE,
    )));
    block
}
