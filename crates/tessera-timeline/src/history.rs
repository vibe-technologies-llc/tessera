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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Revision(u64);

impl Revision {
    fn following(self) -> Self {
        Self(self.0 + 1)
    }
}

#[derive(Clone, Debug)]
struct Entry {
    command: Command,
    project: Project,
    revision: Revision,
}

#[derive(Clone, Debug, Default)]
pub struct History {
    undo: VecDeque<Entry>,
    redo: Vec<Entry>,
    current: Revision,
    newest: Revision,
    saved: Revision,
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
                revision: self.current,
            });
            self.redo.clear();
            self.newest = self.newest.following();
            self.current = self.newest;
        }
        edited
    }

    pub fn undo(&mut self, project: &mut Project) -> Option<Command> {
        let entry = self.undo.pop_back()?;
        let undone = self.swap_in(entry, project);
        self.redo.push(undone);
        self.next_redo()
    }

    pub fn redo(&mut self, project: &mut Project) -> Option<Command> {
        let entry = self.redo.pop()?;
        let redone = self.swap_in(entry, project);
        self.undo.push_back(redone);
        self.next_undo()
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn next_undo(&self) -> Option<Command> {
        self.undo.back().map(|entry| entry.command)
    }

    pub fn next_redo(&self) -> Option<Command> {
        self.redo.last().map(|entry| entry.command)
    }

    pub fn revision(&self) -> Revision {
        self.current
    }

    pub fn mark_saved(&mut self, revision: Revision) {
        self.saved = revision;
    }

    pub fn is_saved(&self) -> bool {
        self.saved == self.current
    }

    fn swap_in(&mut self, entry: Entry, project: &mut Project) -> Entry {
        let swapped_out = std::mem::replace(project, entry.project);
        project.next_ids = project.next_ids.covering(swapped_out.next_ids);
        let revision = std::mem::replace(&mut self.current, entry.revision);
        Entry {
            command: entry.command,
            project: swapped_out,
            revision,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZero;

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

    fn remove_track(history: &mut History, project: &mut Project, index: usize) {
        history
            .apply(Command::RemoveTrack, project, |project| {
                project.timeline.remove_track(index)
            })
            .unwrap();
    }

    #[test]
    fn the_next_steps_are_named_before_they_are_taken() {
        let mut history = History::default();
        let mut project = Project::new("test");

        assert!(!history.can_undo());
        assert!(!history.can_redo());
        assert_eq!(history.next_undo(), None);
        assert_eq!(history.next_redo(), None);

        add_track(&mut history, &mut project);
        remove_track(&mut history, &mut project, 0);

        assert!(history.can_undo());
        assert_eq!(history.next_undo(), Some(Command::RemoveTrack));

        history.undo(&mut project);

        assert!(history.can_undo());
        assert!(history.can_redo());
        assert_eq!(history.next_undo(), Some(Command::AddTrack));
        assert_eq!(history.next_redo(), Some(Command::RemoveTrack));

        history.undo(&mut project);

        assert!(!history.can_undo());
        assert_eq!(history.next_redo(), Some(Command::AddTrack));
    }

    #[test]
    fn the_saved_revision_is_found_again_by_undo_and_redo() {
        let mut history = History::default();
        let mut project = Project::new("test");

        assert!(history.is_saved());

        add_track(&mut history, &mut project);

        assert!(!history.is_saved());

        history.mark_saved(history.revision());
        add_track(&mut history, &mut project);

        assert!(!history.is_saved());

        history.undo(&mut project);

        assert!(history.is_saved());

        history.undo(&mut project);

        assert!(!history.is_saved());

        history.redo(&mut project);

        assert!(history.is_saved());

        let _ = history.apply(Command::RemoveTrack, &mut project, |project| {
            project.timeline.remove_track(99)
        });

        assert!(history.is_saved());
    }

    #[test]
    fn a_new_command_after_undoing_past_the_save_never_returns_to_it() {
        let mut history = History::default();
        let mut project = Project::new("test");

        add_track(&mut history, &mut project);
        history.mark_saved(history.revision());
        history.undo(&mut project);
        add_track(&mut history, &mut project);

        assert_eq!(project.timeline.tracks.len(), 3);
        assert!(!history.is_saved());

        history.undo(&mut project);

        assert!(!history.is_saved());
    }

    #[test]
    fn a_save_marks_the_revision_it_wrote_even_after_later_edits() {
        let mut history = History::default();
        let mut project = Project::new("test");

        add_track(&mut history, &mut project);
        let written = history.revision();
        add_track(&mut history, &mut project);
        history.mark_saved(written);

        assert!(!history.is_saved());

        history.undo(&mut project);

        assert!(history.is_saved());
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
                    width: NonZero::new(1280).unwrap(),
                    height: NonZero::new(720).unwrap(),
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
