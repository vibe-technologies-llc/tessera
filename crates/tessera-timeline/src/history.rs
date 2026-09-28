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
    Entry {
        command: entry.command,
        project: std::mem::replace(project, entry.project),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EditError, TrackKind};

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
}
