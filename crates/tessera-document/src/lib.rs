mod error;
mod migration;
mod v1;

use std::{
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
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
    encode(project, None)
}

fn encode(project: &Project, directory: Option<&Path>) -> Result<String, FormatError> {
    let envelope = Envelope {
        format: FORMAT,
        version: CURRENT_VERSION,
        project: current::Project::encode(project, directory)?,
    };
    let mut text = serde_json::to_string_pretty(&envelope).map_err(FormatError::Encode)?;
    text.push('\n');
    Ok(text)
}

pub fn from_str(text: &str) -> Result<Project, FormatError> {
    decode(text, None)
}

fn decode(text: &str, directory: Option<&Path>) -> Result<Project, FormatError> {
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
    Ok(project.decode(directory)?)
}

fn envelope_version(envelope: &Map<String, Value>) -> Result<u64, FormatError> {
    let version = envelope.get("version").ok_or(FormatError::MissingVersion)?;
    version
        .as_u64()
        .ok_or_else(|| FormatError::InvalidVersion(version.clone()))
}

pub fn save(project: &Project, path: &Path) -> Result<(), Error> {
    let target = resolve_symlinks(path)?;
    let directory = containing_directory(&target).map_err(|source| Error::Write {
        path: path.to_owned(),
        source,
    })?;
    let text = encode(project, Some(&directory)).map_err(|source| Error::Format {
        path: path.to_owned(),
        source,
    })?;
    write_atomically(&target, text.as_bytes()).map_err(|source| Error::Write {
        path: path.to_owned(),
        source,
    })
}

pub fn open(path: &Path) -> Result<Project, Error> {
    let text = fs::read_to_string(path).map_err(|source| Error::Read {
        path: path.to_owned(),
        source,
    })?;
    let target = resolve_symlinks(path).unwrap_or_else(|_| path.to_owned());
    let directory = containing_directory(&target).map_err(|source| Error::Read {
        path: path.to_owned(),
        source,
    })?;
    decode(&text, Some(&directory)).map_err(|source| Error::Format {
        path: path.to_owned(),
        source,
    })
}

fn containing_directory(file: &Path) -> io::Result<PathBuf> {
    let absolute = std::path::absolute(file)?;
    Ok(absolute.parent().map_or_else(PathBuf::new, Path::to_owned))
}

const MAX_SYMLINK_HOPS: usize = 40;

fn resolve_symlinks(path: &Path) -> Result<PathBuf, Error> {
    let mut resolved = path.to_owned();
    for _ in 0..MAX_SYMLINK_HOPS {
        let is_symlink = match fs::symlink_metadata(&resolved) {
            Ok(metadata) => metadata.is_symlink(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => false,
            Err(source) => {
                return Err(Error::Write {
                    path: path.to_owned(),
                    source,
                });
            }
        };
        if !is_symlink {
            return Ok(resolved);
        }
        let target = fs::read_link(&resolved).map_err(|source| Error::Write {
            path: path.to_owned(),
            source,
        })?;
        resolved = resolved
            .parent()
            .map_or_else(PathBuf::new, Path::to_owned)
            .join(target);
    }
    Err(Error::SymlinkLoop {
        path: path.to_owned(),
    })
}

fn write_atomically(path: &Path, contents: &[u8]) -> io::Result<()> {
    let (temporary, mut file) = create_temporary_sibling(path)?;
    let written = write_synced(&mut file, path, contents)
        .and_then(|()| fs::rename(&temporary, path))
        .and_then(|()| sync_directory(path));
    if written.is_err() {
        fs::remove_file(&temporary).ok();
    }
    written
}

fn write_synced(file: &mut File, target: &Path, contents: &[u8]) -> io::Result<()> {
    match fs::metadata(target) {
        Ok(existing) => file.set_permissions(existing.permissions())?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    file.write_all(contents)?;
    file.sync_all()
}

fn sync_directory(path: &Path) -> io::Result<()> {
    let directory = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    File::open(directory)?.sync_all()
}

fn create_temporary_sibling(path: &Path) -> io::Result<(PathBuf, File)> {
    static NEXT_TEMPORARY: AtomicU64 = AtomicU64::new(0);

    loop {
        let temporary = temporary_sibling(path, NEXT_TEMPORARY.fetch_add(1, Ordering::Relaxed));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
        {
            Ok(file) => return Ok((temporary, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
}

fn temporary_sibling(path: &Path, sequence: u64) -> PathBuf {
    let mut name = OsString::from(".");
    name.push(path.file_name().unwrap_or_default());
    name.push(format!(".{}.{sequence}.tmp", std::process::id()));
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use std::{
        ffi::OsStr,
        num::{NonZero, NonZeroU32},
        os::unix::{
            ffi::OsStrExt,
            fs::{PermissionsExt, symlink},
        },
    };

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
            width: NonZero::new(1920).unwrap(),
            height: NonZero::new(1080).unwrap(),
            frame_rate,
        })
    }

    fn audio_stream(index: usize, sample_rate: u32, channels: u16) -> Stream {
        Stream::Audio(AudioStream {
            index,
            codec: "aac".into(),
            sample_rate: NonZero::new(sample_rate).unwrap(),
            channels: NonZero::new(channels).unwrap(),
        })
    }

    fn golden_project() -> Project {
        let mut project = Project::new("Golden");
        project.settings.frame_rate = FrameRate::NTSC_30;
        project.settings.sample_rate = NonZeroU32::new(44_100).unwrap();
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
        project
            .place_linked(asset, 0, Time::from_seconds(1))
            .unwrap();
        project.add_marker(Time::from_seconds(4), "Cut here");
        project.set_in_point(Time::from_seconds(1)).unwrap();
        project.set_out_point(Time::from_seconds(8)).unwrap();
        project.timeline.tracks[0].name = "Interview".into();
        project.timeline.tracks[1].muted = true;
        project
    }

    fn rich_project() -> Project {
        let mut project = Project::new("Rich");
        project.settings = tessera_timeline::SequenceSettings {
            width: NonZero::new(3840).unwrap(),
            height: NonZero::new(2160).unwrap(),
            frame_rate: FrameRate::NTSC_60,
            sample_rate: NonZeroU32::new(96_000).unwrap(),
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
            "/media/still.png".into(),
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
        project.add_marker(Time::from_seconds(20), "Second \"act\"");
        let early = project.add_marker(Time::from_seconds(30), "");
        project.move_marker(early, Time::from_seconds(2)).unwrap();
        project.set_out_point(Time::from_seconds(25)).unwrap();
        let track = &mut project.timeline.tracks[upper];
        track.locked = true;
        track.solo = true;
        track.height = tessera_timeline::TrackHeight::Tall;
        project.timeline.tracks[second_audio].height = tessera_timeline::TrackHeight::Compact;
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
    fn ids_of_deleted_clips_stay_retired_across_a_save() {
        let mut project = golden_project();

        let newest = project.timeline.tracks[1].clips()[0].id;
        project.delete_clip(newest).unwrap();

        let mut reopened = from_str(&to_string(&project).unwrap()).unwrap();
        let asset = reopened.assets[0].id;
        let placed = reopened.place_clip(asset, 1, Time::ZERO).unwrap();

        assert_eq!(
            reopened.next_ids,
            project.next_ids.covering(reopened.next_ids)
        );
        assert_ne!(placed.id, newest);
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
        std::sync::Arc::make_mut(&mut project.assets)[0].path = path.clone();
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

    fn project_with_media(path: PathBuf) -> Project {
        let mut project = Project::new("Portable");
        project.add_asset(
            path,
            MediaInfo {
                duration: Some(Time::from_seconds(2)),
                streams: vec![video_stream(0, None)],
            },
        );
        project
    }

    fn stored_media_path(file: &Path) -> String {
        let text = fs::read_to_string(file).unwrap();
        let envelope: serde_json::Value = serde_json::from_str(&text).unwrap();
        envelope["project"]["assets"][0]["path"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    #[test]
    fn media_under_the_project_directory_is_stored_relative_to_it() {
        let dir = ScratchDir::new("relative-media");
        let path = dir.0.join("edit.tessera");
        let project = project_with_media(dir.0.join("footage").join("a.mkv"));

        save(&project, &path).unwrap();

        assert_eq!(stored_media_path(&path), "footage/a.mkv");
        assert_eq!(open(&path).unwrap(), project);
    }

    #[test]
    fn a_moved_project_directory_keeps_finding_its_media() {
        let dir = ScratchDir::new("moved-media");
        let first = dir.0.join("first");
        let second = dir.0.join("second");
        fs::create_dir_all(&first).unwrap();
        fs::create_dir_all(&second).unwrap();
        save(
            &project_with_media(first.join("a.mkv")),
            &first.join("edit.tessera"),
        )
        .unwrap();
        fs::rename(first.join("edit.tessera"), second.join("edit.tessera")).unwrap();

        let opened = open(&second.join("edit.tessera")).unwrap();

        assert_eq!(opened.assets[0].path, second.join("a.mkv"));
    }

    #[test]
    fn media_elsewhere_is_stored_with_its_absolute_path() {
        let dir = ScratchDir::new("absolute-media");
        let path = dir.0.join("edit.tessera");
        let project = project_with_media("/media/elsewhere/a.mkv".into());

        save(&project, &path).unwrap();

        assert_eq!(stored_media_path(&path), "/media/elsewhere/a.mkv");
        assert_eq!(open(&path).unwrap(), project);
    }

    #[test]
    fn a_path_relative_to_the_working_directory_is_stored_absolute() {
        let dir = ScratchDir::new("cwd-media");
        let path = dir.0.join("edit.tessera");
        let relative = PathBuf::from("relative/still.png");
        let absolute = std::path::absolute(&relative).unwrap();

        save(&project_with_media(relative), &path).unwrap();

        assert_eq!(open(&path).unwrap().assets[0].path, absolute);
    }

    #[test]
    fn the_pure_text_functions_leave_paths_as_they_are() {
        let project = project_with_media("relative/still.png".into());

        let text = to_string(&project).unwrap();

        assert_eq!(from_str(&text).unwrap(), project);
    }

    #[test]
    fn a_project_saved_through_a_symlink_is_relative_to_the_file_it_points_at() {
        let dir = ScratchDir::new("symlink-media");
        let real = dir.0.join("real");
        fs::create_dir_all(&real).unwrap();
        symlink(real.join("edit.tessera"), dir.0.join("link.tessera")).unwrap();
        let project = project_with_media(real.join("a.mkv"));

        save(&project, &dir.0.join("link.tessera")).unwrap();

        assert_eq!(stored_media_path(&real.join("edit.tessera")), "a.mkv");
        assert_eq!(open(&dir.0.join("link.tessera")).unwrap(), project);
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
    fn saves_running_at_once_each_write_their_own_temporary_file() {
        let dir = ScratchDir::new("concurrent-save");
        let path = dir.0.join("edit.tessera");
        let projects = [golden_project(), rich_project()];

        std::thread::scope(|scope| {
            let saves: Vec<_> = (0..8)
                .map(|index| {
                    let project = &projects[index % projects.len()];
                    let path = &path;
                    scope.spawn(move || save(project, path))
                })
                .collect();
            for saving in saves {
                saving.join().unwrap().unwrap();
            }
        });

        assert_eq!(dir.entries(), ["edit.tessera"]);
        assert!(projects.contains(&open(&path).unwrap()));
    }

    #[test]
    fn saving_keeps_the_permissions_of_the_file_it_replaces() {
        let dir = ScratchDir::new("permissions");
        let path = dir.0.join("edit.tessera");
        fs::write(&path, "an older project").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();

        save(&golden_project(), &path).unwrap();

        assert_eq!(mode(&path), 0o640);
        assert_eq!(fs::read_to_string(&path).unwrap(), GOLDEN_V1);
    }

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn saving_through_a_symlink_replaces_its_target_and_keeps_the_link() {
        let dir = ScratchDir::new("symlink");
        let projects = dir.0.join("projects");
        fs::create_dir(&projects).unwrap();
        let target = projects.join("edit.tessera");
        let link = dir.0.join("link.tessera");
        let hop = dir.0.join("hop.tessera");
        fs::write(&target, "an older project").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
        symlink("projects/edit.tessera", &hop).unwrap();
        symlink(&hop, &link).unwrap();

        save(&golden_project(), &link).unwrap();

        assert!(fs::symlink_metadata(&link).unwrap().is_symlink());
        assert!(fs::symlink_metadata(&hop).unwrap().is_symlink());
        assert_eq!(fs::read_to_string(&target).unwrap(), GOLDEN_V1);
        assert_eq!(mode(&target), 0o600);
        assert_eq!(fs::read_dir(&projects).unwrap().count(), 1);
    }

    #[test]
    fn saving_through_a_dangling_symlink_creates_its_target() {
        let dir = ScratchDir::new("dangling-symlink");
        let link = dir.0.join("link.tessera");
        symlink("edit.tessera", &link).unwrap();

        save(&golden_project(), &link).unwrap();

        assert!(fs::symlink_metadata(&link).unwrap().is_symlink());
        assert_eq!(
            fs::read_to_string(dir.0.join("edit.tessera")).unwrap(),
            GOLDEN_V1
        );
    }

    #[test]
    fn saving_through_a_symlink_loop_is_refused() {
        let dir = ScratchDir::new("symlink-loop");
        let first = dir.0.join("first.tessera");
        let second = dir.0.join("second.tessera");
        symlink(&second, &first).unwrap();
        symlink(&first, &second).unwrap();

        let error = save(&golden_project(), &first).unwrap_err();

        assert!(matches!(error, Error::SymlinkLoop { ref path } if *path == first));
        assert_eq!(dir.entries().len(), 2);
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
