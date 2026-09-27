use std::path::{Path, PathBuf};

use ffmpeg_next::{Packet, Rational, codec, encoder, format, frame};
use tessera_timeline::FrameRate;

pub const WIDTH: u32 = 64;
pub const HEIGHT: u32 = 48;
pub const FRAME_COUNT: i64 = 20;
pub const FRAME_RATE: FrameRate = FrameRate::FPS_25;
const KEYFRAME_INTERVAL: u32 = 5;
const NEUTRAL_CHROMA: u8 = 128;

pub fn luma(index: i64) -> u8 {
    u8::try_from(20 + index * 10).expect("fixture luma stays in range")
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

impl Fixture {
    pub fn generate(name: &str) -> Self {
        crate::init().unwrap();
        let path =
            std::env::temp_dir().join(format!("tessera-media-{}-{name}.mkv", std::process::id()));
        encode(&path).unwrap();
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

fn encode(path: &Path) -> Result<(), ffmpeg_next::Error> {
    let time_base = Rational::new(FRAME_RATE.denominator as i32, FRAME_RATE.numerator as i32);
    let mut output = format::output(path)?;
    let codec = encoder::find(codec::Id::MPEG4).ok_or(ffmpeg_next::Error::EncoderNotFound)?;
    let global_header = output
        .format()
        .flags()
        .contains(format::Flags::GLOBAL_HEADER);
    let mut video = codec::Context::new_with_codec(codec).encoder().video()?;
    video.set_width(WIDTH);
    video.set_height(HEIGHT);
    video.set_format(format::Pixel::YUV420P);
    video.set_time_base(time_base);
    video.set_frame_rate(Some(time_base.invert()));
    video.set_gop(KEYFRAME_INTERVAL);
    if global_header {
        video.set_flags(codec::Flags::GLOBAL_HEADER);
    }
    let mut encoder = video.open_as(codec)?;
    output.add_stream(codec)?.set_parameters(&encoder);
    output.write_header()?;
    let stream_time_base = output.stream(0).expect("stream was added").time_base();
    let write_ready = |encoder: &mut encoder::Video, output: &mut format::context::Output| {
        let mut packet = Packet::empty();
        while encoder.receive_packet(&mut packet).is_ok() {
            packet.set_stream(0);
            packet.rescale_ts(time_base, stream_time_base);
            packet.write_interleaved(output)?;
        }
        Ok::<_, ffmpeg_next::Error>(())
    };
    for index in 0..FRAME_COUNT {
        let mut picture = frame::Video::new(format::Pixel::YUV420P, WIDTH, HEIGHT);
        picture.data_mut(0).fill(luma(index));
        picture.data_mut(1).fill(NEUTRAL_CHROMA);
        picture.data_mut(2).fill(NEUTRAL_CHROMA);
        picture.set_pts(Some(index));
        encoder.send_frame(&picture)?;
        write_ready(&mut encoder, &mut output)?;
    }
    encoder.send_eof()?;
    write_ready(&mut encoder, &mut output)?;
    output.write_trailer()
}
