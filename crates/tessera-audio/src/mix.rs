use std::{
    collections::{HashMap, HashSet},
    num::NonZeroU32,
    ops::Range,
    path::{Path, PathBuf},
};

use tessera_media::AudioDecoder;
use tessera_timeline::{Clip, Project, Time, TimeRange, TrackKind};

use crate::CHANNELS;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Span<'a> {
    path: &'a Path,
    source: Time,
    frames: Range<usize>,
}

pub struct Mixer {
    project: Project,
    decoders: HashMap<PathBuf, AudioDecoder>,
    failed: HashSet<PathBuf>,
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
        let held = |path: &PathBuf| project.assets.iter().any(|asset| asset.path == *path);
        self.decoders.retain(|path, _| held(path));
        self.failed.retain(held);
    }

    pub fn render(&mut self, first: i64, out: &mut [f32]) {
        out.fill(0.0);
        let sample_rate = self.sample_rate();
        let spans = spans(&self.project, sample_rate, first, out.len() / CHANNELS);
        for span in spans {
            let path = span.path;
            if self.failed.contains(path) {
                continue;
            }
            let decoder = match self.decoders.get_mut(path) {
                Some(decoder) => decoder,
                None => match AudioDecoder::open(path, sample_rate) {
                    Ok(opened) => self.decoders.entry(path.to_owned()).or_insert(opened),
                    Err(error) => {
                        tracing::warn!(path = %path.display(), %error, "audio clip cannot be played");
                        self.failed.insert(path.to_owned());
                        continue;
                    }
                },
            };
            match decoder.samples(span.source, span.frames.len()) {
                Ok(buffer) => {
                    let mixed = &mut out[span.frames.start * CHANNELS..span.frames.end * CHANNELS];
                    for (mixed, sample) in mixed.iter_mut().zip(&buffer.samples) {
                        *mixed += sample;
                    }
                }
                Err(error) => {
                    tracing::warn!(path = %path.display(), %error, "audio clip stopped playing");
                    self.decoders.remove(path);
                    self.failed.insert(path.to_owned());
                }
            }
        }
    }
}

fn spans(project: &Project, sample_rate: NonZeroU32, first: i64, frames: usize) -> Vec<Span<'_>> {
    let end = first + frames as i64;
    let block = time_touching_samples(first..end, sample_rate);
    project
        .timeline
        .tracks
        .iter()
        .filter(|track| track.kind == TrackKind::Audio)
        .flat_map(|track| track.clips_overlapping(block))
        .filter_map(|clip| {
            let path = &project.asset(clip.asset)?.path;
            let covered = clip_samples(clip, sample_rate);
            let start = covered.start.max(first);
            let stop = covered.end.min(end);
            (start < stop).then(|| {
                let offset_in_clip =
                    (Time::from_samples(start, sample_rate) - clip.start).max(Time::ZERO);
                Span {
                    path,
                    source: clip.source.start + offset_in_clip,
                    frames: (start - first) as usize..(stop - first) as usize,
                }
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

fn clip_samples(clip: &Clip, sample_rate: NonZeroU32) -> Range<i64> {
    let range = clip.timeline_range();
    range.start.to_samples(sample_rate)..range.end().to_samples(sample_rate)
}

#[cfg(test)]
mod tests {
    use tessera_timeline::{AssetId, AudioStream, ClipEdge, MediaInfo, Stream};

    use super::*;

    const RATE: NonZeroU32 = NonZeroU32::new(48_000).unwrap();
    const ONE_SECOND: i64 = RATE.get() as i64;
    const TONE: &str = "/media/tone.wav";

    fn project_with_tone() -> (Project, AssetId, usize) {
        let mut project = Project::new("mix");
        let info = MediaInfo {
            duration: Some(Time::from_seconds(2)),
            streams: vec![Stream::Audio(AudioStream {
                index: 0,
                codec: "pcm_s16le".into(),
                sample_rate: RATE.get(),
                channels: 2,
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
        assert_eq!(spans[0].source, Time::ZERO);
        assert_eq!(spans[0].frames, 100..1024);
    }

    #[test]
    fn consecutive_blocks_continue_in_the_source() {
        let (mut project, asset, track) = project_with_tone();
        project.place_clip(asset, track, Time::ZERO).unwrap();
        let first = spans(&project, RATE, 0, 1024);
        let second = spans(&project, RATE, 1024, 1024);
        assert_eq!(first[0].frames, 0..1024);
        assert_eq!(second[0].source, Time::from_samples(1024, RATE));
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
        assert_eq!(spans[0].source, half_second);
        assert_eq!(
            spans[0].frames,
            RATE.get() as usize / 2..RATE.get() as usize
        );
    }

    #[test]
    fn a_clip_starting_on_the_flick_a_block_ends_at_still_sounds_its_first_sample() {
        let (mut project, asset, track) = project_with_tone();
        let rate = NonZeroU32::new(11).unwrap();
        let block_end = Time::from_samples(1, rate);

        project.place_clip(asset, track, block_end).unwrap();
        let spans = spans(&project, rate, 0, 1);

        assert_eq!(block_end.to_samples(rate), 0);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].frames, 0..1);
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
    fn replacing_the_project_forgets_failures_for_media_it_no_longer_holds() {
        let (project, _, _) = project_with_tone();
        let mut mixer = Mixer::new(project.clone());
        mixer.failed.insert("/media/gone.wav".into());
        mixer.failed.insert(TONE.into());
        mixer.set_project(project);
        assert_eq!(mixer.failed, HashSet::from([PathBuf::from(TONE)]));
    }
}
