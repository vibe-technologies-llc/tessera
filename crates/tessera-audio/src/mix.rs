use std::{
    collections::{HashMap, HashSet},
    num::NonZeroU32,
    ops::Range,
    path::{Path, PathBuf},
};

use tessera_media::AudioDecoder;
use tessera_timeline::{Clip, Gain, Project, Time, TimeRange, TrackKind};

use crate::CHANNELS;

#[derive(Clone, Debug, PartialEq)]
struct Span<'a> {
    path: &'a Path,
    stream: usize,
    track: usize,
    amplitude: f32,
    source_sample: i64,
    frames: Range<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Source {
    path: PathBuf,
    stream: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct DecoderKey {
    source: Source,
    track: usize,
}

pub struct Mixer {
    project: Project,
    decoders: HashMap<DecoderKey, AudioDecoder>,
    failed: HashSet<Source>,
}

impl Mixer {
    pub fn new(project: Project) -> Self {
        Self {
            project,
            decoders: HashMap::new(),
            failed: HashSet::new(),
        }
    }

    pub fn sample_rate(&self) -> NonZeroU32 {
        self.project.settings.sample_rate
    }

    pub fn set_project(&mut self, project: Project) {
        let rate_changed = project.settings.sample_rate != self.sample_rate();
        self.project = project;
        if rate_changed {
            self.decoders.clear();
        }
        let project = &self.project;
        let held = |source: &Source| {
            project.assets.iter().any(|asset| {
                asset.path == source.path
                    && asset.info.audio().any(|audio| audio.index == source.stream)
            })
        };
        self.decoders
            .retain(|key, _| held(&key.source) && key.track < project.timeline.tracks.len());
        self.failed.retain(held);
    }

    pub fn render(&mut self, first: i64, out: &mut [f32]) {
        out.fill(0.0);
        let sample_rate = self.sample_rate();
        let spans = spans(&self.project, sample_rate, first, out.len() / CHANNELS);
        for span in spans {
            let path = span.path;
            let source = Source {
                path: path.to_owned(),
                stream: span.stream,
            };
            if self.failed.contains(&source) {
                continue;
            }
            let key = DecoderKey {
                source,
                track: span.track,
            };
            let decoder = match self.decoders.get_mut(&key) {
                Some(decoder) => decoder,
                None => match AudioDecoder::open(path, span.stream, sample_rate) {
                    Ok(opened) => self.decoders.entry(key.clone()).or_insert(opened),
                    Err(error) => {
                        tracing::warn!(path = %path.display(), %error, "audio clip cannot be played");
                        self.failed.insert(key.source);
                        continue;
                    }
                },
            };
            match decoder.samples_from(span.source_sample, span.frames.len()) {
                Ok(buffer) => {
                    let mixed = &mut out[span.frames.start * CHANNELS..span.frames.end * CHANNELS];
                    for (mixed, sample) in mixed.iter_mut().zip(&buffer.samples) {
                        *mixed += sample * span.amplitude;
                    }
                }
                Err(error) => {
                    tracing::warn!(path = %path.display(), %error, "audio clip stopped playing");
                    self.decoders.remove(&key);
                    self.failed.insert(key.source);
                }
            }
        }
        for sample in out.iter_mut() {
            *sample = limited(*sample);
        }
    }
}

const LIMITER_KNEE: f32 = 0.9;

fn limited(sample: f32) -> f32 {
    let magnitude = sample.abs();
    if magnitude <= LIMITER_KNEE {
        sample
    } else {
        let headroom = 1.0 - LIMITER_KNEE;
        let eased = LIMITER_KNEE + headroom * ((magnitude - LIMITER_KNEE) / headroom).tanh();
        eased.copysign(sample)
    }
}

fn spans(project: &Project, sample_rate: NonZeroU32, first: i64, frames: usize) -> Vec<Span<'_>> {
    let end = first + frames as i64;
    let block = time_touching_samples(first..end, sample_rate);
    let audio = || {
        project
            .timeline
            .tracks
            .iter()
            .enumerate()
            .filter(|(_, track)| track.kind == TrackKind::Audio)
    };
    let any_solo = audio().any(|(_, track)| track.solo);
    audio()
        .filter(|(_, track)| !track.muted && (track.solo || !any_solo))
        .flat_map(|(index, track)| {
            track
                .clips_overlapping(block)
                .iter()
                .map(move |clip| (index, track.volume, clip))
        })
        .filter(|(_, volume, clip)| *volume != Gain::SILENT && clip.gain != Gain::SILENT)
        .filter_map(|(track, volume, clip)| {
            let asset = project.asset(clip.asset)?;
            let stream = asset.info.audio().next()?.index;
            let covered = clip_samples(clip, sample_rate);
            let start = covered.start.max(first);
            let stop = covered.end.min(end);
            (start < stop).then(|| Span {
                path: &asset.path,
                stream,
                track,
                amplitude: clip.gain.amplitude() * volume.amplitude(),
                source_sample: clip.source.start.to_samples(sample_rate) + (start - covered.start),
                frames: (start - first) as usize..(stop - first) as usize,
            })
        })
        .collect()
}

fn time_touching_samples(samples: Range<i64>, sample_rate: NonZeroU32) -> TimeRange {
    let start = Time::from_samples(samples.start, sample_rate);
    let last_flick_flooring_inside = Time::from_samples(samples.end, sample_rate);
    TimeRange::new(
        start,
        last_flick_flooring_inside - start + Time::from_flicks(1),
    )
}

fn ceil_samples(time: Time, sample_rate: NonZeroU32) -> i64 {
    let floor = time.to_samples(sample_rate);
    if Time::from_samples(floor, sample_rate) < time {
        floor + 1
    } else {
        floor
    }
}

fn clip_samples(clip: &Clip, sample_rate: NonZeroU32) -> Range<i64> {
    let range = clip.timeline_range();
    ceil_samples(range.start, sample_rate)..ceil_samples(range.end(), sample_rate)
}

#[cfg(test)]
mod tests {
    use std::num::NonZero;

    use tessera_timeline::{AssetId, AudioStream, ClipEdge, MediaInfo, Stream};

    use super::*;

    const RATE: NonZeroU32 = NonZeroU32::new(48_000).unwrap();
    const ONE_SECOND: i64 = RATE.get() as i64;
    const TONE: &str = "/media/tone.wav";
    const TONE_STREAM: usize = 1;

    fn project_with_tone() -> (Project, AssetId, usize) {
        let mut project = Project::new("mix");
        let info = MediaInfo {
            duration: Some(Time::from_seconds(2)),
            streams: vec![Stream::Audio(AudioStream {
                index: TONE_STREAM,
                codec: "pcm_s16le".into(),
                sample_rate: RATE,
                channels: NonZero::new(2).unwrap(),
            })],
        };
        let asset = project.add_asset(TONE.into(), info);
        let track = project
            .timeline
            .tracks
            .iter()
            .position(|track| track.kind == TrackKind::Audio)
            .unwrap();
        (project, asset, track)
    }

    #[test]
    fn blocks_before_and_after_a_clip_are_silent() {
        let (mut project, asset, track) = project_with_tone();
        project
            .place_clip(asset, track, Time::from_seconds(1))
            .unwrap();
        assert!(spans(&project, RATE, 0, 1024).is_empty());
        assert!(spans(&project, RATE, 3 * ONE_SECOND, 1024).is_empty());
    }

    #[test]
    fn a_block_straddling_the_clip_start_reads_from_the_source_start() {
        let (mut project, asset, track) = project_with_tone();
        project
            .place_clip(asset, track, Time::from_seconds(1))
            .unwrap();
        let spans = spans(&project, RATE, ONE_SECOND - 100, 1024);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].source_sample, 0);
        assert_eq!(spans[0].frames, 100..1024);
    }

    #[test]
    fn consecutive_blocks_continue_in_the_source() {
        let (mut project, asset, track) = project_with_tone();
        project.place_clip(asset, track, Time::ZERO).unwrap();
        let first = spans(&project, RATE, 0, 1024);
        let second = spans(&project, RATE, 1024, 1024);
        assert_eq!(first[0].frames, 0..1024);
        assert_eq!(second[0].source_sample, 1024);
        assert_eq!(second[0].frames, 0..1024);
    }

    #[test]
    fn a_trimmed_clip_starts_inside_its_source() {
        let (mut project, asset, track) = project_with_tone();
        let clip = project.place_clip(asset, track, Time::ZERO).unwrap();
        let half_second = Time::from_samples(ONE_SECOND / 2, RATE);
        project
            .trim_clip(clip.id, ClipEdge::Start, half_second)
            .unwrap();
        let spans = spans(&project, RATE, 0, RATE.get() as usize);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].source_sample, ONE_SECOND / 2);
        assert_eq!(
            spans[0].frames,
            RATE.get() as usize / 2..RATE.get() as usize
        );
    }

    #[test]
    fn a_clip_starting_between_samples_sounds_from_the_next_sample() {
        let (mut project, asset, track) = project_with_tone();
        let rate = NonZeroU32::new(11).unwrap();
        let start = Time::from_samples(1, rate);

        project.place_clip(asset, track, start).unwrap();

        assert_eq!(start.to_samples(rate), 0);
        assert!(spans(&project, rate, 0, 1).is_empty());

        let next = spans(&project, rate, 1, 1);

        assert_eq!(next.len(), 1);
        assert_eq!(next[0].source_sample, 0);
    }

    #[test]
    fn blocks_continue_exactly_when_the_clip_starts_between_samples() {
        let (mut project, asset, track) = project_with_tone();
        let start = tessera_timeline::FrameRate::NTSC_30.frame_to_time(1);
        let first_sample = ceil_samples(start, RATE);
        project.place_clip(asset, track, start).unwrap();

        assert_ne!(Time::from_samples(first_sample, RATE), start);

        let mut next_source = 0;
        for block in 0..4 {
            let spans = spans(&project, RATE, block * 1024, 1024);
            let Some(span) = spans.first() else {
                continue;
            };
            assert_eq!(span.source_sample, next_source, "block {block}");
            next_source = span.source_sample + span.frames.len() as i64;
        }

        assert_eq!(next_source, 4 * 1024 - first_sample);
    }

    #[test]
    fn two_clips_of_one_file_on_different_tracks_read_separately() {
        let (mut project, asset, track) = project_with_tone();
        let second_track = project.timeline.add_track(TrackKind::Audio);
        project.place_clip(asset, track, Time::ZERO).unwrap();
        project.place_clip(asset, second_track, Time::ZERO).unwrap();

        let spans = spans(&project, RATE, 0, 1024);

        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].path, spans[1].path);
        assert_eq!(spans[0].stream, TONE_STREAM);
        assert_ne!(spans[0].track, spans[1].track);
    }

    #[test]
    fn a_block_ending_inside_a_clip_stops_at_its_end() {
        let (mut project, asset, track) = project_with_tone();
        project.place_clip(asset, track, Time::ZERO).unwrap();
        let spans = spans(&project, RATE, 2 * ONE_SECOND - 10, 1024);
        assert_eq!(spans[0].frames, 0..10);
    }

    #[test]
    fn every_audio_track_contributes() {
        let (mut project, asset, track) = project_with_tone();
        let second_track = project.timeline.add_track(TrackKind::Audio);
        project.place_clip(asset, track, Time::ZERO).unwrap();
        project.place_clip(asset, second_track, Time::ZERO).unwrap();
        assert_eq!(spans(&project, RATE, 0, 1024).len(), 2);
    }

    #[test]
    fn clip_gain_and_track_volume_scale_a_clip_and_silence_leaves_it_out() {
        let (mut project, asset, track) = project_with_tone();
        let clip = project.place_clip(asset, track, Time::ZERO).unwrap();
        project.adjust_clip_gains(&[clip.id], 60).unwrap();
        project.timeline.tracks[track].volume = Gain::from_tenths(-60).unwrap();

        let amplitude = spans(&project, RATE, 0, 16)[0].amplitude;

        assert!((amplitude - 1.0).abs() < 1e-6);

        project.timeline.tracks[track].volume = Gain::SILENT;

        assert!(spans(&project, RATE, 0, 16).is_empty());
    }

    #[test]
    fn replacing_the_project_forgets_failures_for_media_it_no_longer_holds() {
        let (project, _, _) = project_with_tone();
        let source = |path: &str, stream| Source {
            path: path.into(),
            stream,
        };
        let mut mixer = Mixer::new(project.clone());
        mixer.failed.extend([
            source("/media/gone.wav", 0),
            source(TONE, 0),
            source(TONE, TONE_STREAM),
        ]);

        mixer.set_project(project);

        assert_eq!(mixer.failed, HashSet::from([source(TONE, TONE_STREAM)]));
    }

    #[test]
    fn muted_tracks_stay_silent_and_solo_silences_the_others() {
        let (mut project, asset, track) = project_with_tone();
        let second = project.timeline.add_track(TrackKind::Audio);
        let third = project.timeline.add_track(TrackKind::Audio);
        for track in [track, second, third] {
            project.place_clip(asset, track, Time::ZERO).unwrap();
        }

        project.timeline.tracks[second].muted = true;

        assert_eq!(spans(&project, RATE, 0, 1024).len(), 2);

        project.timeline.tracks[third].solo = true;

        assert_eq!(spans(&project, RATE, 0, 1024).len(), 1);

        project.timeline.tracks[second].solo = true;
        project.timeline.tracks[second].muted = false;

        assert_eq!(spans(&project, RATE, 0, 1024).len(), 2);
    }

    #[test]
    fn the_limiter_leaves_quiet_samples_alone_and_never_exceeds_full_scale() {
        for sample in [0.0, 0.5, -0.5, LIMITER_KNEE, -LIMITER_KNEE] {
            assert_eq!(limited(sample), sample);
        }
        for sample in [0.95, 1.0, 1.7, 40.0, f32::MAX] {
            let limited_up = limited(sample);
            assert!(limited_up > LIMITER_KNEE && limited_up <= 1.0, "{sample}");
            assert_eq!(limited(-sample), -limited_up);
        }
        assert!(limited(0.95) < limited(1.7));
    }
}
