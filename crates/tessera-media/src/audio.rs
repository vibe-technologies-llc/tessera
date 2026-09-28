use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
};

use ffmpeg_next::{
    ChannelLayout, Rational, Rescale, codec, decoder,
    ffi::{AV_NOPTS_VALUE, swr_get_out_samples},
    format::{self, sample},
    frame, media, rescale,
    software::resampling,
};
use tessera_timeline::{FLICKS_PER_SECOND, Time};

use crate::{
    Error,
    decode::{from_stream_ts, to_stream_ts},
};

const OUTPUT_FORMAT: format::Sample = format::Sample::F32(sample::Type::Packed);
const FORWARD_DECODE_WINDOW: Time = Time::from_seconds(1);
const SEEK_PREROLL: Time = Time::from_flicks(FLICKS_PER_SECOND / 10);

#[derive(Clone, Debug, PartialEq)]
pub struct AudioBuffer {
    pub sample_rate: u32,
    pub samples: Vec<f32>,
}

impl AudioBuffer {
    pub const CHANNELS: usize = 2;

    pub fn frames(&self) -> usize {
        self.samples.len() / Self::CHANNELS
    }
}

pub struct AudioDecoder {
    path: PathBuf,
    input: format::context::Input,
    stream_index: usize,
    time_base: Rational,
    start: i64,
    sample_rate: u32,
    decoder: decoder::Audio,
    resampler: Option<Resampler>,
    decoded: DecodedSamples,
    position: Option<i64>,
    drained: bool,
}

struct Resampler {
    context: resampling::Context,
    source: SourceFormat,
    channels: ResampledChannels,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct SourceFormat {
    format: format::Sample,
    rate: u32,
    channels: u16,
    layout_bits: u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ResampledChannels {
    Mono,
    Stereo,
}

#[derive(Default)]
struct DecodedSamples {
    start: Option<i64>,
    interleaved: VecDeque<f32>,
}

impl AudioDecoder {
    pub fn open(path: impl AsRef<Path>, sample_rate: u32) -> Result<Self, Error> {
        let path = path.as_ref().to_owned();
        let input = format::input(&path).map_err(|source| Error::Open {
            path: path.clone(),
            source,
        })?;
        let stream = input
            .streams()
            .best(media::Type::Audio)
            .ok_or_else(|| Error::NoAudio { path: path.clone() })?;
        let stream_index = stream.index();
        let time_base = stream.time_base();
        let start = match stream.start_time() {
            AV_NOPTS_VALUE => 0,
            start => start,
        };
        let decoder = codec::Context::from_parameters(stream.parameters())
            .and_then(|context| {
                let mut decoder = context.decoder();
                decoder.set_packet_time_base(time_base);
                decoder.audio()
            })
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
            sample_rate,
            decoder,
            resampler: None,
            decoded: DecodedSamples::default(),
            position: None,
            drained: false,
        })
    }

    pub fn samples(&mut self, start: Time, frames: usize) -> Result<AudioBuffer, Error> {
        let first = start.to_samples(self.sample_rate);
        let end = first + frames as i64;
        if self.needs_seek(first) {
            self.seek(first)?;
        }
        self.decoded.discard_before(first);
        while self.decoded.end().is_none_or(|decoded| decoded < end) && self.decode_more()? {
            self.decoded.discard_before(first);
        }
        let mut samples = vec![0.0; frames * AudioBuffer::CHANNELS];
        self.decoded.copy_into(&mut samples, first);
        self.decoded.discard_before(end);
        self.position = Some(end);
        Ok(AudioBuffer {
            sample_rate: self.sample_rate,
            samples,
        })
    }

    fn needs_seek(&self, target: i64) -> bool {
        let Some(position) = self.position else {
            return true;
        };
        if target < position {
            return true;
        }
        if self.drained {
            return false;
        }
        let decoded_end = self.decoded.end().unwrap_or(position);
        target - decoded_end > FORWARD_DECODE_WINDOW.to_samples(self.sample_rate)
    }

    fn seek(&mut self, target: i64) -> Result<(), Error> {
        let preroll_start =
            (Time::from_samples(target, self.sample_rate) - SEEK_PREROLL).max(Time::ZERO);
        let stream_ts = self.start + to_stream_ts(preroll_start, self.time_base);
        let seek_ts = stream_ts.rescale(self.time_base, rescale::TIME_BASE);
        self.input
            .seek(seek_ts, ..seek_ts)
            .map_err(|source| Error::Seek {
                path: self.path.clone(),
                source,
            })?;
        self.decoder.flush();
        self.resampler = None;
        self.decoded = DecodedSamples::default();
        self.drained = false;
        self.position = Some(target);
        Ok(())
    }

    fn sample_index(&self, pts: i64) -> i64 {
        from_stream_ts(pts - self.start, self.time_base).to_samples(self.sample_rate)
    }

    fn decode_more(&mut self) -> Result<bool, Error> {
        let mut frame = frame::Audio::empty();
        loop {
            if self.decoder.receive_frame(&mut frame).is_ok() {
                specify_channel_layout(&mut frame);
                self.resample(&frame)?;
                return Ok(true);
            }
            if self.drained {
                return self.flush_resampler();
            }
            self.feed()?;
        }
    }

    fn resample(&mut self, frame: &frame::Audio) -> Result<(), Error> {
        let source = SourceFormat::of(frame);
        if self
            .resampler
            .as_ref()
            .is_some_and(|resampler| resampler.source != source)
        {
            self.flush_resampler()?;
        }
        let index = frame
            .timestamp()
            .map(|pts| self.sample_index(pts))
            .or(self.position)
            .unwrap_or(0);
        self.decoded.anchor(index);
        let resampler = match self.resampler.take() {
            Some(resampler) => resampler,
            None => Resampler::new(frame, source, self.sample_rate)?,
        };
        self.resampler
            .insert(resampler)
            .run(frame, &mut self.decoded)
    }

    fn flush_resampler(&mut self) -> Result<bool, Error> {
        let Some(mut resampler) = self.resampler.take() else {
            return Ok(false);
        };
        resampler.flush(&mut self.decoded)?;
        Ok(true)
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

fn specify_channel_layout(frame: &mut frame::Audio) {
    let layout = frame.channel_layout();
    if layout.is_empty() {
        frame.set_channel_layout(ChannelLayout::default(layout.channels()));
    }
}

impl SourceFormat {
    fn of(frame: &frame::Audio) -> Self {
        Self {
            format: frame.format(),
            rate: frame.rate(),
            channels: frame.channels(),
            layout_bits: frame.channel_layout().bits(),
        }
    }
}

impl ResampledChannels {
    fn for_source(channels: u16) -> Self {
        if channels == 1 {
            Self::Mono
        } else {
            Self::Stereo
        }
    }

    fn layout(self) -> ChannelLayout {
        match self {
            Self::Mono => ChannelLayout::MONO,
            Self::Stereo => ChannelLayout::STEREO,
        }
    }
}

impl Resampler {
    fn new(frame: &frame::Audio, source: SourceFormat, sample_rate: u32) -> Result<Self, Error> {
        let channels = ResampledChannels::for_source(source.channels);
        let context = resampling::Context::get(
            source.format,
            frame.channel_layout(),
            source.rate,
            OUTPUT_FORMAT,
            channels.layout(),
            sample_rate,
        )
        .map_err(|source| Error::Resample { source })?;
        Ok(Self {
            context,
            source,
            channels,
        })
    }

    fn run(&mut self, frame: &frame::Audio, decoded: &mut DecodedSamples) -> Result<(), Error> {
        let mut output = self.output_frame(frame.samples());
        self.context
            .run(frame, &mut output)
            .map_err(|source| Error::Resample { source })?;
        decoded.push(self.channels, output.plane(0));
        Ok(())
    }

    fn flush(&mut self, decoded: &mut DecodedSamples) -> Result<(), Error> {
        loop {
            let mut output = self.output_frame(0);
            self.context
                .flush(&mut output)
                .map_err(|source| Error::Resample { source })?;
            if output.samples() == 0 {
                return Ok(());
            }
            decoded.push(self.channels, output.plane(0));
        }
    }

    fn output_frame(&mut self, input_samples: usize) -> frame::Audio {
        let input_samples = i32::try_from(input_samples).unwrap_or(i32::MAX);
        let bound = unsafe { swr_get_out_samples(self.context.as_mut_ptr(), input_samples) };
        let capacity = usize::try_from(bound).unwrap_or(0).max(1);
        frame::Audio::new(OUTPUT_FORMAT, capacity, self.channels.layout())
    }
}

impl DecodedSamples {
    fn frames(&self) -> usize {
        self.interleaved.len() / AudioBuffer::CHANNELS
    }

    fn end(&self) -> Option<i64> {
        self.start.map(|start| start + self.frames() as i64)
    }

    fn anchor(&mut self, index: i64) {
        self.start.get_or_insert(index);
    }

    fn push(&mut self, channels: ResampledChannels, samples: &[f32]) {
        match channels {
            ResampledChannels::Mono => self
                .interleaved
                .extend(samples.iter().flat_map(|&sample| [sample, sample])),
            ResampledChannels::Stereo => self.interleaved.extend(samples),
        }
    }

    fn discard_before(&mut self, index: i64) {
        let Some(start) = self.start else {
            return;
        };
        let excess = (index - start).clamp(0, self.frames() as i64);
        self.interleaved
            .drain(..excess as usize * AudioBuffer::CHANNELS);
        self.start = Some(start + excess);
    }

    fn copy_into(&self, output: &mut [f32], first: i64) {
        let Some(start) = self.start else {
            return;
        };
        let leading_silence = usize::try_from(start - first).unwrap_or(0);
        let skipped = usize::try_from(first - start).unwrap_or(0);
        let Some(output) = output.get_mut(leading_silence.saturating_mul(AudioBuffer::CHANNELS)..)
        else {
            return;
        };
        if skipped >= self.frames() {
            return;
        }
        let decoded = self.interleaved.range(skipped * AudioBuffer::CHANNELS..);
        for (slot, &sample) in output.iter_mut().zip(decoded) {
            *slot = sample;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::{self, Fixture};

    const OUTPUT_RATE: u32 = 48_000;
    const BLOCK: usize = 256;
    const LEVEL_TOLERANCE: f32 = 0.01;

    #[test]
    fn decoder_moves_between_threads() {
        fn assert_send<T: Send>() {}
        assert_send::<AudioDecoder>();
    }

    fn fixture_duration() -> Time {
        fixture::FRAME_RATE.frame_to_time(fixture::FRAME_COUNT)
    }

    fn block_inside_frame(index: i64, rate: u32) -> Time {
        let start = fixture::audio_frame_start(index, rate);
        let end = fixture::audio_frame_start(index + 1, rate);
        Time::from_samples((start + end) / 2 - BLOCK as i64 / 2, rate)
    }

    fn assert_level(samples: &[f32], expected: f32) {
        for (offset, &sample) in samples.iter().enumerate() {
            assert!(
                (sample - expected).abs() <= LEVEL_TOLERANCE,
                "expected level {expected}, got {sample} at sample {offset}"
            );
        }
    }

    fn assert_frame_level(buffer: &AudioBuffer, index: i64) {
        assert_eq!(buffer.frames(), BLOCK);
        assert_level(&buffer.samples, fixture::audio_level(index));
    }

    #[test]
    fn levels_are_found_in_any_order() {
        let fixture = Fixture::generate("audio_any_order");
        let mut decoder = AudioDecoder::open(fixture.path(), OUTPUT_RATE).unwrap();
        for index in [0, 7, 3, 12, 19, 11, 5, 6, 18] {
            let block = decoder
                .samples(block_inside_frame(index, OUTPUT_RATE), BLOCK)
                .unwrap();
            assert_frame_level(&block, index);
        }
    }

    #[test]
    fn output_is_stereo_at_the_requested_rate() {
        let fixture = Fixture::generate("audio_rates");
        for rate in [22_050, 44_100, 48_000, 96_000] {
            let mut decoder = AudioDecoder::open(fixture.path(), rate).unwrap();
            let block = decoder.samples(block_inside_frame(9, rate), BLOCK).unwrap();
            assert_eq!(block.sample_rate, rate);
            assert_eq!(block.samples.len(), BLOCK * AudioBuffer::CHANNELS);
            assert_frame_level(&block, 9);
        }
    }

    #[test]
    fn consecutive_reads_continue_without_seeking() {
        let fixture = Fixture::generate("audio_seek_policy");
        let mut decoder = AudioDecoder::open(fixture.path(), OUTPUT_RATE).unwrap();
        assert!(decoder.needs_seek(0));
        let start = Time::from_samples(10_000, OUTPUT_RATE);
        decoder.samples(start, 1_000).unwrap();
        assert!(!decoder.needs_seek(11_000));
        assert!(!decoder.needs_seek(11_500));
        assert!(decoder.needs_seek(10_999));
        assert!(decoder.needs_seek(10_000));
        assert!(decoder.needs_seek(11_000 + 2 * i64::from(OUTPUT_RATE)));
    }

    #[test]
    fn consecutive_reads_match_one_long_read() {
        let fixture = Fixture::generate("audio_chunked");
        let length = fixture_duration().to_samples(OUTPUT_RATE) as usize + 500;
        let start = Time::from_samples(-250, OUTPUT_RATE);
        let whole = AudioDecoder::open(fixture.path(), OUTPUT_RATE)
            .unwrap()
            .samples(start, length)
            .unwrap();
        let mut decoder = AudioDecoder::open(fixture.path(), OUTPUT_RATE).unwrap();
        let mut chunked = Vec::new();
        let mut first = -250;
        for chunk in [1, 480, 1_023, 4_096, 7, 20_000, 64].into_iter().cycle() {
            let remaining = length - chunked.len() / AudioBuffer::CHANNELS;
            if remaining == 0 {
                break;
            }
            let frames = chunk.min(remaining);
            let block = decoder
                .samples(Time::from_samples(first, OUTPUT_RATE), frames)
                .unwrap();
            chunked.extend(block.samples);
            first += frames as i64;
        }
        assert_eq!(chunked, whole.samples);
    }

    #[test]
    fn frame_boundaries_land_on_their_sample_after_a_seek() {
        let fixture = Fixture::generate("audio_boundaries");
        let mut decoder = AudioDecoder::open(fixture.path(), OUTPUT_RATE).unwrap();
        let before_boundary = 32;
        for index in [12, 4, 17, 1, 9] {
            let boundary = fixture::audio_frame_start(index, OUTPUT_RATE);
            let block = decoder
                .samples(
                    Time::from_samples(boundary - before_boundary, OUTPUT_RATE),
                    2 * before_boundary as usize,
                )
                .unwrap();
            let midpoint = (fixture::audio_level(index - 1) + fixture::audio_level(index)) / 2.0;
            for channel in 0..AudioBuffer::CHANNELS {
                let crossing = block
                    .samples
                    .iter()
                    .skip(channel)
                    .step_by(AudioBuffer::CHANNELS)
                    .position(|&sample| sample > midpoint)
                    .unwrap() as i64;
                assert!(
                    (crossing - before_boundary).abs() <= 2,
                    "frame {index} starts at sample {crossing}, expected {before_boundary}"
                );
            }
        }
    }

    #[test]
    fn reads_past_the_end_are_silent() {
        let fixture = Fixture::generate("audio_past_the_end");
        let mut decoder = AudioDecoder::open(fixture.path(), OUTPUT_RATE).unwrap();
        let block = decoder.samples(Time::from_seconds(60), BLOCK).unwrap();
        assert_silent(&block.samples);
        let end = fixture_duration().to_samples(OUTPUT_RATE);
        let straddling = decoder
            .samples(Time::from_samples(end - 1_000, OUTPUT_RATE), 2_000)
            .unwrap();
        let (audible, silent) = straddling.samples.split_at(1_000 * AudioBuffer::CHANNELS);
        assert_level(
            &audible[..900 * AudioBuffer::CHANNELS],
            fixture::audio_level(fixture::FRAME_COUNT - 1),
        );
        assert_silent(&silent[100 * AudioBuffer::CHANNELS..]);
    }

    fn assert_silent(samples: &[f32]) {
        assert!(samples.iter().all(|&sample| sample == 0.0));
    }

    #[test]
    fn reads_straddling_the_start_begin_with_silence() {
        let fixture = Fixture::generate("audio_before_the_start");
        let mut decoder = AudioDecoder::open(fixture.path(), OUTPUT_RATE).unwrap();
        let block = decoder
            .samples(Time::from_samples(-1_000, OUTPUT_RATE), 2_000)
            .unwrap();
        let (silent, audible) = block.samples.split_at(1_000 * AudioBuffer::CHANNELS);
        assert_silent(silent);
        assert_level(audible, fixture::audio_level(0));
    }

    #[test]
    fn a_file_without_audio_is_rejected() {
        let fixture = Fixture::generate_without_audio("audio_missing_stream");
        let error = AudioDecoder::open(fixture.path(), OUTPUT_RATE)
            .err()
            .unwrap();
        assert!(matches!(error, Error::NoAudio { .. }), "{error}");
    }

    #[test]
    fn missing_file_is_an_open_error() {
        crate::init().unwrap();
        let error = AudioDecoder::open("/nonexistent/tessera/clip.mkv", OUTPUT_RATE)
            .err()
            .unwrap();
        assert!(matches!(error, Error::Open { .. }), "{error}");
    }
}
