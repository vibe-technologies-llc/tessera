mod picture;

use std::{
    collections::{HashMap, hash_map::Entry},
    ffi::OsString,
    fs, io,
    num::NonZeroU32,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread,
};

pub use picture::placement;
use tessera_audio::Mixer;
use tessera_media::{
    Container, EncodeBackend, EncodeSettings, Encoder, PREFERRED_ENCODE_BACKENDS, VideoCodec,
    VideoDecoder,
};
use tessera_render::{Compositor, Layer};
use tessera_timeline::{FrameRate, Project, Time, TimeRange};
use thiserror::Error;

const QUEUED_FRAMES: usize = 4;
const ENCODER_THREAD_NAME: &str = "tessera-export-encoder";
const PARTIAL_SUFFIX: &str = "part";

#[derive(Debug, Error)]
pub enum Error {
    #[error("there is nothing to export")]
    EmptyRange,
    #[error("the export was cancelled")]
    Cancelled,
    #[error(transparent)]
    Encode(tessera_media::Error),
    #[error("cannot open {path} for export: {source}")]
    OpenMedia {
        path: PathBuf,
        #[source]
        source: tessera_media::Error,
    },
    #[error("cannot decode {path}: {source}")]
    DecodeMedia {
        path: PathBuf,
        #[source]
        source: tessera_media::Error,
    },
    #[error("the sound of {path} cannot be played")]
    Audio { path: PathBuf },
    #[error("no GPU compositor: {0}")]
    Compositor(#[source] tessera_render::Error),
    #[error("compositing a frame failed: {0}")]
    Composite(#[source] tessera_render::Error),
    #[error("the encoding thread stopped unexpectedly")]
    EncoderThread,
    #[error("cannot move the finished export to {path}: {source}")]
    Finish {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Preset {
    pub codec: VideoCodec,
    pub container: Container,
}

impl Preset {
    pub const ALL: [Self; 6] = [
        Self::new(VideoCodec::H264, Container::Mp4),
        Self::new(VideoCodec::Hevc, Container::Mp4),
        Self::new(VideoCodec::Av1, Container::Mp4),
        Self::new(VideoCodec::H264, Container::Matroska),
        Self::new(VideoCodec::Hevc, Container::Matroska),
        Self::new(VideoCodec::Av1, Container::Matroska),
    ];

    pub const fn new(codec: VideoCodec, container: Container) -> Self {
        Self { codec, container }
    }

    pub fn label(self) -> String {
        format!("{} · {}", self.codec.label(), self.container.label())
    }

    pub fn extension(self) -> &'static str {
        self.container.extension()
    }
}

impl Default for Preset {
    fn default() -> Self {
        Self::ALL[0]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Span {
    Timeline,
    InToOut,
}

impl Span {
    pub fn range(self, project: &Project) -> Option<TimeRange> {
        let range = match (self, project.in_point, project.out_point) {
            (Self::InToOut, Some(start), Some(end)) => TimeRange::new(start, end - start),
            (Self::InToOut, _, _) => return None,
            (Self::Timeline, _, _) => TimeRange::new(Time::ZERO, project.timeline.duration()),
        };
        (range.duration > Time::ZERO).then_some(range)
    }
}

#[derive(Debug, Default)]
pub struct Control {
    cancelled: AtomicBool,
    done: AtomicU64,
    total: AtomicU64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Progress {
    pub done: u64,
    pub total: u64,
}

impl Progress {
    pub fn fraction(self) -> f32 {
        if self.total == 0 {
            0.
        } else {
            self.done as f32 / self.total as f32
        }
    }
}

impl Control {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }

    pub fn progress(&self) -> Progress {
        Progress {
            done: self.done.load(Ordering::Relaxed),
            total: self.total.load(Ordering::Relaxed),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Exported {
    pub path: PathBuf,
    pub frames: u64,
    pub backend: EncodeBackend,
    pub video_encoder: &'static str,
    pub audio_encoder: &'static str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Frames {
    first: i64,
    end: i64,
}

impl Frames {
    fn covering(range: TimeRange, frame_rate: FrameRate) -> Self {
        Self {
            first: frame_rate.time_to_frame(range.start),
            end: frame_rate.time_to_frame(range.end() - Time::from_flicks(1)) + 1,
        }
    }

    fn count(self) -> u64 {
        (self.end - self.first).max(0) as u64
    }
}

fn samples_of_frame(frame: i64, frame_rate: FrameRate, sample_rate: NonZeroU32) -> (i64, usize) {
    let start = frame_rate.frame_to_time(frame).to_samples(sample_rate);
    let end = frame_rate.frame_to_time(frame + 1).to_samples(sample_rate);
    (start, (end - start).max(0) as usize)
}

pub fn export(
    project: &Project,
    span: Span,
    preset: Preset,
    path: &Path,
    control: &Control,
) -> Result<Exported, Error> {
    export_with(
        project,
        span,
        preset,
        path,
        control,
        PREFERRED_ENCODE_BACKENDS,
    )
}

pub fn export_with(
    project: &Project,
    span: Span,
    preset: Preset,
    path: &Path,
    control: &Control,
    backends: &[EncodeBackend],
) -> Result<Exported, Error> {
    let range = span.range(project).ok_or(Error::EmptyRange)?;
    let settings = project.settings;
    let frames = Frames::covering(range, settings.frame_rate);
    control.total.store(frames.count(), Ordering::Relaxed);
    control.done.store(0, Ordering::Relaxed);
    let partial = partial_path(path);
    let encoder = Encoder::create(
        &partial,
        &EncodeSettings {
            container: preset.container,
            codec: preset.codec,
            width: settings.width,
            height: settings.height,
            frame_rate: settings.frame_rate,
            sample_rate: settings.sample_rate,
        },
        backends,
    )
    .map_err(Error::Encode)?;
    let described = (
        encoder.backend(),
        encoder.video_encoder(),
        encoder.audio_encoder(),
    );
    let rendered = render(project, frames, encoder, control);
    let finished = rendered.and_then(|()| {
        fs::rename(&partial, path).map_err(|source| Error::Finish {
            path: path.to_owned(),
            source,
        })
    });
    if let Err(error) = finished {
        fs::remove_file(&partial).ok();
        return Err(error);
    }
    let (backend, video_encoder, audio_encoder) = described;
    Ok(Exported {
        path: path.to_owned(),
        frames: frames.count(),
        backend,
        video_encoder,
        audio_encoder,
    })
}

fn partial_path(path: &Path) -> PathBuf {
    let mut name = OsString::from(".");
    name.push(path.file_name().unwrap_or_default());
    name.push(format!(".{}.{PARTIAL_SUFFIX}", std::process::id()));
    path.with_file_name(name)
}

struct Chunk {
    picture: Vec<u8>,
    sound: Vec<f32>,
}

fn render(
    project: &Project,
    frames: Frames,
    encoder: Encoder,
    control: &Control,
) -> Result<(), Error> {
    let (sender, receiver) = mpsc::sync_channel::<Chunk>(QUEUED_FRAMES);
    let encoding = thread::Builder::new()
        .name(ENCODER_THREAD_NAME.into())
        .spawn(move || encode(encoder, &receiver))
        .map_err(|_| Error::EncoderThread)?;
    let produced = produce(project, frames, control, &sender);
    drop(sender);
    let encoded = encoding.join().map_err(|_| Error::EncoderThread)?;
    match (produced, encoded) {
        (Err(Error::EncoderThread) | Ok(()), Err(error)) | (Err(error), _) => Err(error),
        (Ok(()), Ok(encoder)) => encoder.finish().map(drop).map_err(Error::Encode),
    }
}

fn encode(mut encoder: Encoder, chunks: &mpsc::Receiver<Chunk>) -> Result<Encoder, Error> {
    for chunk in chunks {
        encoder.push_video(&chunk.picture).map_err(Error::Encode)?;
        encoder.push_audio(&chunk.sound).map_err(Error::Encode)?;
    }
    Ok(encoder)
}

fn produce(
    project: &Project,
    frames: Frames,
    control: &Control,
    chunks: &mpsc::SyncSender<Chunk>,
) -> Result<(), Error> {
    let settings = project.settings;
    let mut pictures = Pictures::new(project)?;
    let mut mixer = Mixer::new(project.clone());
    for frame in frames.first..frames.end {
        if control.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let time = settings.frame_rate.frame_to_time(frame);
        let picture = pictures.at(time)?;
        let (first_sample, sample_count) =
            samples_of_frame(frame, settings.frame_rate, settings.sample_rate);
        let mut sound = vec![0.0; sample_count * tessera_audio::CHANNELS];
        mixer.render(first_sample, &mut sound);
        if let Some(path) = mixer.failed_media().next() {
            return Err(Error::Audio {
                path: path.to_owned(),
            });
        }
        chunks
            .send(Chunk { picture, sound })
            .map_err(|_| Error::EncoderThread)?;
        control.done.fetch_add(1, Ordering::Relaxed);
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct DecoderKey {
    path: PathBuf,
    stream: usize,
    track: usize,
}

struct Pictures<'a> {
    project: &'a Project,
    size: (u32, u32),
    decoders: HashMap<DecoderKey, VideoDecoder>,
    compositor: Compositor,
}

impl<'a> Pictures<'a> {
    fn new(project: &'a Project) -> Result<Self, Error> {
        Ok(Self {
            project,
            size: (project.settings.width.get(), project.settings.height.get()),
            decoders: HashMap::new(),
            compositor: Compositor::new().map_err(Error::Compositor)?,
        })
    }

    fn at(&mut self, time: Time) -> Result<Vec<u8>, Error> {
        let project = self.project;
        let (width, height) = self.size;
        let mut decoded = Vec::new();
        for (track, clip) in project.timeline.video_layers_at(time) {
            let (Some(asset), Some(source_time)) =
                (project.asset(clip.asset), clip.source_time_at(time))
            else {
                continue;
            };
            let Some(stream) = asset.info.default_video() else {
                continue;
            };
            let key = DecoderKey {
                path: asset.path.clone(),
                stream: stream.index,
                track,
            };
            let decoder = match self.decoders.entry(key) {
                Entry::Occupied(entry) => entry.into_mut(),
                Entry::Vacant(entry) => {
                    let opened =
                        VideoDecoder::open(&asset.path, stream.index).map_err(|source| {
                            Error::OpenMedia {
                                path: asset.path.clone(),
                                source,
                            }
                        })?;
                    entry.insert(opened.fit_within(width, height).cache_capacity(0))
                }
            };
            let frame = decoder
                .frame_at(source_time)
                .map_err(|source| Error::DecodeMedia {
                    path: asset.path.clone(),
                    source,
                })?;
            decoded.push((frame, placement(clip.transform, clip.opacity, self.size)));
        }
        let layers: Vec<Layer<'_>> = decoded
            .iter()
            .map(|(frame, placement)| Layer {
                width: frame.width,
                height: frame.height,
                bgra: &frame.bgra,
                placement: *placement,
            })
            .collect();
        let composited = self
            .compositor
            .composite(width, height, &layers)
            .map_err(Error::Composite)?;
        Ok(composited.bgra)
    }
}

#[cfg(test)]
mod tests {
    use tessera_media::{
        AudioDecoder,
        fixture::{self, Fixture},
        probe,
    };
    use tessera_timeline::SequenceSettings;

    use super::*;

    const SAMPLE_RATE: NonZeroU32 = NonZeroU32::new(48_000).unwrap();

    fn project_of(fixture: &Fixture) -> Project {
        let mut project = Project::new("export");
        project.set_settings(SequenceSettings {
            width: NonZeroU32::new(fixture::WIDTH).unwrap(),
            height: NonZeroU32::new(fixture::HEIGHT).unwrap(),
            frame_rate: fixture::FRAME_RATE,
            sample_rate: SAMPLE_RATE,
        });
        let info = probe(fixture.path()).unwrap();
        let asset = project.add_asset(fixture.path().to_owned(), info);
        project.place_linked(asset, 0, Time::ZERO).unwrap();
        project
    }

    fn exported(project: &Project, span: Span, output: &Fixture) -> Result<Exported, Error> {
        export_with(
            project,
            span,
            Preset::new(VideoCodec::H264, Container::Mp4),
            output.path(),
            &Control::default(),
            &[EncodeBackend::Software],
        )
    }

    fn grey_at(path: &Path, frame: i64) -> u8 {
        let info = probe(path).unwrap();
        let stream = info.default_video().unwrap().index;
        let mut decoder = VideoDecoder::open_with(path, stream, &[]).unwrap();
        let picture = decoder
            .frame_at(fixture::FRAME_RATE.frame_to_time(frame))
            .unwrap();
        let centre = ((picture.height / 2 * picture.width + picture.width / 2) * 4) as usize;
        picture.bgra[centre]
    }

    fn level_at(path: &Path, frame: i64) -> f32 {
        let info = probe(path).unwrap();
        let stream = info.default_audio().unwrap().index;
        let mut decoder = AudioDecoder::open(path, stream, SAMPLE_RATE).unwrap();
        let middle = Time::from_flicks(
            (fixture::FRAME_RATE.frame_to_time(frame).flicks()
                + fixture::FRAME_RATE.frame_to_time(frame + 1).flicks())
                / 2,
        );
        let buffer = decoder.samples(middle, 64).unwrap();
        buffer.samples.iter().sum::<f32>() / buffer.samples.len() as f32
    }

    #[test]
    fn the_timeline_exports_its_pictures_and_sound() {
        let source = Fixture::generate("export-source");
        let output = Fixture::reserve("export-timeline", "mp4");
        let project = project_of(&source);

        let Ok(exported) = exported(&project, Span::Timeline, &output) else {
            eprintln!("skipping: no H.264 software encoder");
            return;
        };

        assert_eq!(exported.frames, fixture::FRAME_COUNT as u64);
        assert_eq!(exported.backend, EncodeBackend::Software);

        let info = probe(output.path()).unwrap();
        let seconds = info.duration.unwrap().as_seconds_f64();

        assert!((seconds - 0.8).abs() < 0.05, "{seconds}");
        for frame in [0, 7, fixture::FRAME_COUNT - 1] {
            let grey = grey_at(output.path(), frame);
            assert!(
                grey.abs_diff(fixture::expected_grey(frame)) <= 3,
                "frame {frame}: {grey}"
            );
        }
        for frame in [3, 12] {
            let level = level_at(output.path(), frame);
            assert!(
                (level - fixture::audio_level(frame)).abs() < 0.02,
                "frame {frame}: {level}"
            );
        }
    }

    #[test]
    fn in_to_out_exports_only_the_marked_range() {
        let source = Fixture::generate("export-range-source");
        let output = Fixture::reserve("export-range", "mp4");
        let mut project = project_of(&source);
        let rate = fixture::FRAME_RATE;
        project.set_in_point(rate.frame_to_time(5)).unwrap();
        project.set_out_point(rate.frame_to_time(15)).unwrap();

        let Ok(exported) = exported(&project, Span::InToOut, &output) else {
            return;
        };

        assert_eq!(exported.frames, 10);

        let grey = grey_at(output.path(), 0);

        assert!(grey.abs_diff(fixture::expected_grey(5)) <= 3, "{grey}");
    }

    #[test]
    fn a_cancelled_export_leaves_no_file() {
        let source = Fixture::generate("export-cancel-source");
        let output = Fixture::reserve("export-cancel", "mkv");
        let project = project_of(&source);
        let control = Control::default();
        control.cancel();

        let cancelled = export_with(
            &project,
            Span::Timeline,
            Preset::default(),
            output.path(),
            &control,
            &[EncodeBackend::Software],
        );

        assert!(matches!(
            cancelled,
            Err(Error::Cancelled | Error::Encode(_))
        ));
        assert!(!output.path().exists());
        assert!(!partial_path(output.path()).exists());
    }

    #[test]
    fn missing_media_fails_the_export_and_leaves_no_file() {
        let source = Fixture::generate("export-missing-source");
        let output = Fixture::reserve("export-missing", "mp4");
        let project = project_of(&source);
        drop(source);

        let failed = exported(&project, Span::Timeline, &output);

        assert!(matches!(
            failed,
            Err(Error::OpenMedia { .. } | Error::Encode(_))
        ));
        assert!(!output.path().exists());
        assert!(!partial_path(output.path()).exists());
    }

    #[test]
    fn an_empty_range_has_nothing_to_export() {
        let output = Fixture::reserve("export-empty", "mp4");
        let mut project = Project::new("empty");

        assert!(matches!(
            exported(&project, Span::Timeline, &output),
            Err(Error::EmptyRange)
        ));

        project.set_in_point(Time::from_seconds(1)).unwrap();

        assert!(matches!(
            exported(&project, Span::InToOut, &output),
            Err(Error::EmptyRange)
        ));
    }

    #[test]
    fn frames_cover_every_part_of_the_range() {
        let rate = FrameRate::FPS_25;
        let ragged = TimeRange::new(
            rate.frame_to_time(2) + Time::from_flicks(1),
            rate.frame_to_time(3),
        );

        assert_eq!(Frames::covering(ragged, rate), Frames { first: 2, end: 6 });
        assert_eq!(
            Frames::covering(TimeRange::new(Time::ZERO, rate.frame_to_time(4)), rate).count(),
            4
        );
    }

    #[test]
    fn consecutive_frames_take_consecutive_samples() {
        let rate = FrameRate::NTSC_30;
        let mut next = 0;
        for frame in 0..100 {
            let (first, count) = samples_of_frame(frame, rate, SAMPLE_RATE);
            assert_eq!(first, next);
            next = first + count as i64;
        }
        assert_eq!(next, rate.frame_to_time(100).to_samples(SAMPLE_RATE));
    }
}
