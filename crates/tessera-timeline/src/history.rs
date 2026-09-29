use std::{collections::VecDeque, fmt};

use crate::project::Project;

pub const HISTORY_DEPTH: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Command {
    ImportMedia,
    PlaceClip,
    MoveClip,
    TrimClip,
    SplitClips,
    DeleteClip,
    RippleDeleteClip,
    AddTrack,
    RemoveTrack,
    SwapTracks,
}

impl fmt::Display for Command {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::ImportMedia => "Import Media",
            Self::PlaceClip => "Place Clip",
            Self::MoveClip => "Move Clip",
            Self::TrimClip => "Trim Clip",
            Self::SplitClips => "Split Clips",
            Self::DeleteClip => "Delete Clip",
            Self::RippleDeleteClip => "Ripple Delete Clip",
            Self::AddTrack => "Add Track",
            Self::RemoveTrack => "Remove Track",
            Self::SwapTracks => "Swap Tracks",
        })
    }
}

#[derive(Clone, Debug)]
struct Entry {
    command: Command,
    project: Project,
}

#[derive(Clone, Debug, Default)]
pub struct History {
    undo: VecDeque<Entry>,
    redo: Vec<Entry>,
}

impl History {
    pub fn apply<T, E>(
        &mut self,
        command: Command,
        project: &mut Project,
        edit: impl FnOnce(&mut Project) -> Result<T, E>,
    ) -> Result<T, E> {
        let before = project.clone();
        let edited = edit(project);
        if edited.is_err() {
            *project = before;
        } else if *project != before {
            if self.undo.len() == HISTORY_DEPTH {
                self.undo.pop_front();
            }
            self.undo.push_back(Entry {
                command,
                project: before,
            });
            self.redo.clear();
        }
        edited
    }

    pub fn undo(&mut self, project: &mut Project) -> Option<Command> {
        let entry = self.undo.pop_back()?;
        self.redo.push(swap_in(entry, project));
        self.redo.last().map(|entry| entry.command)
    }

    pub fn redo(&mut self, project: &mut Project) -> Option<Command> {
        let entry = self.redo.pop()?;
        self.undo.push_back(swap_in(entry, project));
        self.undo.back().map(|entry| entry.command)
    }
}

fn swap_in(entry: Entry, project: &mut Project) -> Entry {
    let swapped_out = std::mem::replace(project, entry.project);
    project.next_ids = project.next_ids.covering(swapped_out.next_ids);
    Entry {
        command: entry.command,
        project: swapped_out,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EditError, FrameRate, MediaInfo, Stream, Time, TrackKind, VideoStream};

    fn add_track(history: &mut History, project: &mut Project) {
        history
            .apply(Command::AddTrack, project, |project| {
                project.timeline.add_track(TrackKind::Video);
                Ok::<_, EditError>(())
            })
            .unwrap();
    }

    fn track_count(project: &Project) -> usize {
        project.timeline.tracks.len()
    }

    #[test]
    fn undo_and_redo_step_through_recorded_commands() {
        let mut history = History::default();
        let mut project = Project::new("test");
        add_track(&mut history, &mut project);
        add_track(&mut history, &mut project);
        assert_eq!(track_count(&project), 4);
        assert_eq!(history.undo(&mut project), Some(Command::AddTrack));
        assert_eq!(track_count(&project), 3);
        assert_eq!(history.undo(&mut project), Some(Command::AddTrack));
        assert_eq!(project, Project::new("test"));
        assert_eq!(history.undo(&mut project), None);
        assert_eq!(history.redo(&mut project), Some(Command::AddTrack));
        assert_eq!(track_count(&project), 3);
        assert_eq!(history.redo(&mut project), Some(Command::AddTrack));
        assert_eq!(history.redo(&mut project), None);
        assert_eq!(track_count(&project), 4);
    }

    #[test]
    fn a_new_command_discards_the_redo_stack() {
        let mut history = History::default();
        let mut project = Project::new("test");
        add_track(&mut history, &mut project);
        history.undo(&mut project);
        add_track(&mut history, &mut project);
        assert_eq!(history.redo(&mut project), None);
        assert_eq!(track_count(&project), 3);
    }

    #[test]
    fn failed_and_empty_edits_are_rolled_back_and_not_recorded() {
        let mut history = History::default();
        let mut project = Project::new("test");
        let failed = history.apply(Command::AddTrack, &mut project, |project| {
            project.timeline.add_track(TrackKind::Audio);
            project.timeline.remove_track(99)
        });
        assert_eq!(failed, Err(EditError::UnknownTrack(99)));
        assert_eq!(project, Project::new("test"));
        let unchanged = history.apply(Command::SwapTracks, &mut project, |project| {
            project.timeline.swap_tracks(0, 0)
        });
        assert_eq!(unchanged, Ok(()));
        assert_eq!(history.undo(&mut project), None);
    }

    #[test]
    fn the_oldest_commands_fall_off_past_the_depth() {
        let mut history = History::default();
        let mut project = Project::new("test");
        for _ in 0..HISTORY_DEPTH + 2 {
            add_track(&mut history, &mut project);
        }
        while history.undo(&mut project).is_some() {}
        assert_eq!(track_count(&project), 4);
    }

    fn clip_project() -> Project {
        let mut project = Project::new("test");

        project.add_asset(
            "a.mkv".into(),
            MediaInfo {
                duration: Some(Time::from_seconds(4)),
                streams: vec![Stream::Video(VideoStream {
                    index: 0,
                    codec: "h264".into(),
                    width: 1280,
                    height: 720,
                    frame_rate: Some(FrameRate::FPS_30),
                })],
            },
        );
        project
    }

    #[test]
    fn undone_clips_and_assets_keep_their_ids_retired() {
        let mut history = History::default();
        let mut project = clip_project();

        let asset = project.assets[0].id;
        let place = |history: &mut History, project: &mut Project| {
            history
                .apply(Command::PlaceClip, project, |project| {
                    project.place_clip(asset, 0, Time::ZERO)
                })
                .unwrap()
                .id
        };
        let import = |history: &mut History, project: &mut Project| {
            history.apply(Command::ImportMedia, project, |project| {
                Ok::<_, EditError>(project.add_asset("b.mkv".into(), MediaInfo::default()))
            })
        };
        let split = |history: &mut History, project: &mut Project, clip, seconds| {
            history
                .apply(Command::SplitClips, project, |project| {
                    project.split_clip(clip, Time::from_seconds(seconds))
                })
                .map(|(_, tail)| tail.id)
        };

        let first = place(&mut history, &mut project);
        history.undo(&mut project);
        let second = place(&mut history, &mut project);
        let tail = split(&mut history, &mut project, second, 1).unwrap();
        history.undo(&mut project);
        history.undo(&mut project);
        history.redo(&mut project);
        let retail = split(&mut history, &mut project, second, 2).unwrap();
        let imported = import(&mut history, &mut project).unwrap();
        history.undo(&mut project);
        let reimported = import(&mut history, &mut project).unwrap();

        assert_ne!(first, second);
        assert!(![first, second, tail].contains(&retail));
        assert_ne!(imported, reimported);
    }
}
