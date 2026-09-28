mod error;
mod migration;
mod v1;

use std::{
    ffi::OsString,
    fs::{self, File},
    io::{self, Write},
    path::{Path, PathBuf},
};

pub use error::{Error, FormatError, ValidationError};
use serde::Serialize;
use serde_json::{Map, Value};
use tessera_timeline::Project;

use crate::{
    migration::{MIGRATIONS, latest_version, migrate},
    v1 as current,
};

pub const EXTENSION: &str = "tessera";

pub const FORMAT: &str = "tessera-project";

pub const CURRENT_VERSION: u64 = latest_version(MIGRATIONS);

#[derive(Serialize)]
struct Envelope<P> {
    format: &'static str,
    version: u64,
    project: P,
}

pub fn to_string(project: &Project) -> Result<String, FormatError> {
    let envelope = Envelope {
        format: FORMAT,
        version: CURRENT_VERSION,
        project: current::Project::try_from(project)?,
    };
    let mut text = serde_json::to_string_pretty(&envelope).map_err(FormatError::Encode)?;
    text.push('\n');
    Ok(text)
}

pub fn from_str(text: &str) -> Result<Project, FormatError> {
    let Value::Object(mut envelope) = serde_json::from_str(text).map_err(FormatError::Syntax)?
    else {
        return Err(FormatError::NotAProject);
    };
    if envelope.get("format").and_then(Value::as_str) != Some(FORMAT) {
        return Err(FormatError::NotAProject);
    }
    let version = envelope_version(&envelope)?;
    let project = envelope
        .remove("project")
        .ok_or(FormatError::MissingProject)?;
    let project = migrate(project, version, MIGRATIONS)?;
    let project: current::Project =
        serde_json::from_value(project).map_err(|source| FormatError::Shape {
            version: CURRENT_VERSION,
            source,
        })?;
    Ok(project.try_into()?)
}

fn envelope_version(envelope: &Map<String, Value>) -> Result<u64, FormatError> {
    let version = envelope.get("version").ok_or(FormatError::MissingVersion)?;
    version
        .as_u64()
        .ok_or_else(|| FormatError::InvalidVersion(version.clone()))
}

pub fn save(project: &Project, path: &Path) -> Result<(), Error> {
    let text = to_string(project).map_err(|source| Error::Format {
        path: path.to_owned(),
        source,
    })?;
    write_atomically(path, text.as_bytes()).map_err(|source| Error::Write {
        path: path.to_owned(),
        source,
    })
}

pub fn open(path: &Path) -> Result<Project, Error> {
    let text = fs::read_to_string(path).map_err(|source| Error::Read {
        path: path.to_owned(),
        source,
    })?;
    from_str(&text).map_err(|source| Error::Format {
        path: path.to_owned(),
        source,
    })
}

fn write_atomically(path: &Path, contents: &[u8]) -> io::Result<()> {
    let temporary = temporary_sibling(path);
    let written = write_synced(&temporary, contents).and_then(|()| fs::rename(&temporary, path));
    if written.is_err() {
        fs::remove_file(&temporary).ok();
    }
    written
}

fn write_synced(path: &Path, contents: &[u8]) -> io::Result<()> {
    let mut file = File::create(path)?;
    file.write_all(contents)?;
    file.sync_all()
}

fn temporary_sibling(path: &Path) -> PathBuf {
    let mut name = OsString::from(".");
    name.push(path.file_name().unwrap_or_default());
    name.push(format!(".{}.tmp", std::process::id()));
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use std::{ffi::OsStr, os::unix::ffi::OsStrExt};

    use serde_json::json;
    use tessera_timeline::{
        AudioStream, ClipEdge, FrameRate, MediaInfo, Stream, Time, TrackKind, VideoStream,
    };

    use super::*;

    const GOLDEN_V1: &str = include_str!("../fixtures/v1.tessera");

    fn video_stream(index: usize, frame_rate: Option<FrameRate>) -> Stream {
        Stream::Video(VideoStream {
            index,
            codec: "h264".into(),
            width: 1920,
            height: 1080,
            frame_rate,
        })
    }

    fn audio_stream(index: usize, sample_rate: u32, channels: u16) -> Stream {
        Stream::Audio(AudioStream {
            index,
            codec: "aac".into(),
            sample_rate,
            channels,
        })
    }

    fn golden_project() -> Project {
        let mut project = Project::new("Golden");
        project.settings.frame_rate = FrameRate::NTSC_30;
        project.settings.sample_rate = 44_100;
        let asset = project.add_asset(
            "/media/interview.mkv".into(),
            MediaInfo {
                duration: Some(Time::from_seconds(10)),
                streams: vec![
                    video_stream(0, Some(FrameRate::NTSC_30)),
                    audio_stream(1, 48_000, 2),
                ],
            },
        );
        project.place_clip(asset, 0, Time::from_seconds(1)).unwrap();
        project.place_clip(asset, 1, Time::from_seconds(1)).unwrap();
        project
    }

    fn rich_project() -> Project {
        let mut project = Project::new("Rich");
        project.settings = tessera_timeline::SequenceSettings {
            width: 3840,
            height: 2160,
            frame_rate: FrameRate::NTSC_60,
            sample_rate: 96_000,
        };
        let camera = project.add_asset(
            "/media/camera.mov".into(),
            MediaInfo {
                duration: Some(Time::from_seconds(30)),
                streams: vec![
                    audio_stream(0, 48_000, 2),
                    video_stream(1, Some(FrameRate::NTSC_60)),
                    audio_stream(2, 44_100, 1),
                ],
            },
        );
        let music = project.add_asset(
            "/media/music ♪.flac".into(),
            MediaInfo {
                duration: Some(Time::from_flicks(12_345_678_901)),
                streams: vec![audio_stream(0, 96_000, 6)],
            },
        );
        project.add_asset(
            "relative/still.png".into(),
            MediaInfo {
                duration: None,
                streams: vec![video_stream(0, None)],
            },
        );
        let upper = project.timeline.add_track(TrackKind::Video);
        let second_audio = project.timeline.add_track(TrackKind::Audio);
        let base = project.place_clip(camera, 0, Time::ZERO).unwrap();
        project.split_clip(base.id, Time::from_seconds(12)).unwrap();
        let (_, tail) = project.split_clip(base.id, Time::from_seconds(5)).unwrap();
        project
            .trim_clip(tail.id, ClipEdge::Start, Time::from_seconds(7))
            .unwrap();
        let overlay = project
            .place_clip(camera, upper, Time::from_seconds(40))
            .unwrap();
        project
            .move_clip(overlay.id, upper, FrameRate::NTSC_60.frame_to_time(1_001))
            .unwrap();
        let audio_track = project
            .timeline
            .tracks
            .iter()
            .position(|track| track.kind == TrackKind::Audio)
            .unwrap();
        project.place_clip(camera, audio_track, Time::ZERO).unwrap();
        project
            .place_clip(music, second_audio, Time::from_seconds(3))
            .unwrap();
        project
    }

    #[test]
    fn a_rich_project_round_trips_exactly() {
        let project = rich_project();
        assert_eq!(project.timeline.tracks.len(), 4);
        assert_eq!(project.timeline.tracks[0].clips().len(), 3);
        let text = to_string(&project).unwrap();
        assert_eq!(from_str(&text).unwrap(), project);
    }

    #[test]
    fn the_golden_v1_file_loads_and_is_written_unchanged() {
        assert_eq!(from_str(GOLDEN_V1).unwrap(), golden_project());
        assert_eq!(to_string(&golden_project()).unwrap(), GOLDEN_V1);
    }

    fn golden_envelope() -> Map<String, Value> {
        match serde_json::from_str(GOLDEN_V1).unwrap() {
            Value::Object(envelope) => envelope,
            _ => unreachable!(),
        }
    }

    fn load_edited(edit: impl FnOnce(&mut Map<String, Value>)) -> Result<Project, FormatError> {
        let mut envelope = golden_envelope();
        edit(&mut envelope);
        from_str(&Value::Object(envelope).to_string())
    }

    #[test]
    fn text_that_is_not_json_is_a_syntax_error() {
        assert!(matches!(
            from_str("{ \"format\": "),
            Err(FormatError::Syntax(_))
        ));
    }

    #[test]
    fn a_missing_or_foreign_format_marker_is_not_a_project() {
        assert!(matches!(from_str("[]"), Err(FormatError::NotAProject)));
        assert!(matches!(
            load_edited(|envelope| {
                envelope.remove("format");
            }),
            Err(FormatError::NotAProject)
        ));
        assert!(matches!(
            load_edited(|envelope| {
                envelope.insert("format".into(), json!("kdenlive"));
            }),
            Err(FormatError::NotAProject)
        ));
    }

    #[test]
    fn the_version_must_be_present_known_and_not_newer() {
        assert!(matches!(
            load_edited(|envelope| {
                envelope.remove("version");
            }),
            Err(FormatError::MissingVersion)
        ));
        assert!(matches!(
            load_edited(|envelope| {
                envelope.insert("version".into(), json!("one"));
            }),
            Err(FormatError::InvalidVersion(version)) if version == "one"
        ));
        assert!(matches!(
            load_edited(|envelope| {
                envelope.insert("version".into(), json!(CURRENT_VERSION + 1));
            }),
            Err(FormatError::NewerVersion { version, supported: CURRENT_VERSION })
                if version == CURRENT_VERSION + 1
        ));
    }

    #[test]
    fn the_envelope_must_hold_a_project_of_the_right_shape() {
        assert!(matches!(
            load_edited(|envelope| {
                envelope.remove("project");
            }),
            Err(FormatError::MissingProject)
        ));
        assert!(matches!(
            load_edited(|envelope| {
                envelope["project"]["tracks"][0]["clips"][0]
                    .as_object_mut()
                    .unwrap()
                    .remove("start_flicks");
            }),
            Err(FormatError::Shape { version: 1, .. })
        ));
        assert!(matches!(
            load_edited(|envelope| {
                envelope["project"]["settings"]["frame_rate"] = json!(29.97);
            }),
            Err(FormatError::Shape { .. })
        ));
        assert!(matches!(
            load_edited(|envelope| {
                envelope["project"]["colour"] = json!("rec709");
            }),
            Err(FormatError::Shape { .. })
        ));
        assert!(matches!(
            load_edited(|envelope| {
                envelope["project"]["assets"][0]["streams"][1]["bitrate"] = json!(128_000);
            }),
            Err(FormatError::Shape { .. })
        ));
    }

    #[test]
    fn an_inconsistent_project_is_refused() {
        assert!(matches!(
            load_edited(|envelope| {
                envelope["project"]["settings"]["sample_rate"] = json!(0);
            }),
            Err(FormatError::Invalid(ValidationError::ZeroSampleRate))
        ));
    }

    #[test]
    fn a_media_path_that_is_not_unicode_cannot_be_saved() {
        let mut project = golden_project();
        let path = PathBuf::from(OsStr::from_bytes(b"/media/\xff.mkv"));
        project.assets[0].path = path.clone();
        assert!(matches!(
            to_string(&project),
            Err(FormatError::NonUnicodePath(refused)) if refused == path
        ));
    }

    struct ScratchDir(PathBuf);

    impl ScratchDir {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir()
                .join(format!("tessera-document-{}-{name}", std::process::id()));
            fs::remove_dir_all(&path).ok();
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn entries(&self) -> Vec<String> {
            fs::read_dir(&self.0)
                .unwrap()
                .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                .collect()
        }
    }

    impl Drop for ScratchDir {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).ok();
        }
    }

    #[test]
    fn saving_replaces_the_file_atomically_and_opens_back() {
        let dir = ScratchDir::new("save");
        let path = dir.0.join("edit.tessera");
        fs::write(&path, "an older project").unwrap();
        save(&rich_project(), &path).unwrap();
        assert_eq!(dir.entries(), ["edit.tessera"]);
        assert_eq!(open(&path).unwrap(), rich_project());
        save(&golden_project(), &path).unwrap();
        assert_eq!(dir.entries(), ["edit.tessera"]);
        assert_eq!(fs::read_to_string(&path).unwrap(), GOLDEN_V1);
    }

    #[test]
    fn a_failed_save_leaves_no_temporary_file() {
        let dir = ScratchDir::new("failed-save");
        let target = dir.0.join("taken");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("inside"), "").unwrap();
        let error = save(&golden_project(), &target).unwrap_err();
        assert!(matches!(error, Error::Write { ref path, .. } if *path == target));
        assert_eq!(dir.entries(), ["taken"]);
    }

    #[test]
    fn opening_reports_the_path_it_could_not_read_or_parse() {
        let dir = ScratchDir::new("open");
        let missing = dir.0.join("missing.tessera");
        let error = open(&missing).unwrap_err();
        assert!(matches!(
            error,
            Error::Read { ref path, ref source }
                if *path == missing && source.kind() == io::ErrorKind::NotFound
        ));
        assert!(error.to_string().contains("missing.tessera"));
        let garbage = dir.0.join("garbage.tessera");
        fs::write(&garbage, "not a project").unwrap();
        assert!(matches!(
            open(&garbage),
            Err(Error::Format { path, source: FormatError::Syntax(_) }) if path == garbage
        ));
    }
}
