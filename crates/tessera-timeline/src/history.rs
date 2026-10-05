use std::{collections::VecDeque, fmt};

use crate::project::Project;

pub const HISTORY_DEPTH: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Command {
    ImportMedia,
    RemoveAsset,
    PruneAssets,
    RelinkAsset,
    PlaceClip,
    PasteClip,
    MoveClip,
    TrimClip,
    InsertClip,
    OverwriteClip,
    RippleTrimClip,
    RollClips,
    SlipClip,
    SlideClip,
    SplitClips,
    DeleteClip,
    RippleDeleteClip,
    SetSequenceSettings,
    AddMarker,
    MoveMarker,
    RenameMarker,
    RemoveMarker,
    SetInPoint,
    SetOutPoint,
    ClearInOut,
    RenameTrack,
    LockTrack,
    MuteTrack,
    SoloTrack,
    ResizeTrack,
    AddTrack,
    RemoveTrack,
    SwapTracks,
}

impl fmt::Display for Command {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::ImportMedia => "Import Media",
            Self::RemoveAsset => "Remove Asset",
            Self::PruneAssets => "Prune Unused Assets",
            Self::RelinkAsset => "Relink Media",
            Self::PlaceClip => "Place Clip",
            Self::PasteClip => "Paste Clip",
            Self::MoveClip => "Move Clip",
            Self::TrimClip => "Trim Clip",
            Self::InsertClip => "Insert Clip",
            Self::OverwriteClip => "Overwrite Clip",
            Self::RippleTrimClip => "Ripple Trim Clip",
            Self::RollClips => "Roll Edit",
            Self::SlipClip => "Slip Clip",
            Self::SlideClip => "Slide Clip",
            Self::SplitClips => "Split Clips",
            Self::DeleteClip => "Delete Clip",
            Self::RippleDeleteClip => "Ripple Delete Clip",
            Self::SetSequenceSettings => "Sequence Settings",
            Self::AddMarker => "Add Marker",
            Self::MoveMarker => "Move Marker",
            Self::RenameMarker => "Rename Marker",
            Self::RemoveMarker => "Remove Marker",
            Self::SetInPoint => "Set In Point",
            Self::SetOutPoint => "Set Out Point",
            Self::ClearInOut => "Clear In and Out",
            Self::RenameTrack => "Rename Track",
            Self::LockTrack => "Lock Track",
            Self::MuteTrack => "Mute Track",
            Self::SoloTrack => "Solo Track",
            Self::ResizeTrack => "Resize Track",
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

#[derive(Clone, Debug)]
pub struct History {
    undo: VecDeque<Entry>,
    redo: Vec<Entry>,
    current: Revision,
    newest: Revision,
    saved: Option<Revision>,
}

impl Default for History {
    fn default() -> Self {
        Self {
            undo: VecDeque::new(),
            redo: Vec::new(),
            current: Revision::default(),
            newest: Revision::default(),
            saved: Some(Revision::default()),
        }
    }
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
        self.saved = Some(revision);
    }

    pub fn mark_unsaved(&mut self) {
        self.saved = None;
    }

    pub fn is_saved(&self) -> bool {
        self.saved == Some(self.current)
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
    use crate::{
        EditError, FrameRate, MediaInfo, SequenceSettings, Stream, Time, TrackKind, VideoStream,
    };

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
    fn a_project_marked_unsaved_stays_unsaved_through_undo_and_redo() {
        let mut history = History::default();
        let mut project = Project::new("test");

        history.mark_unsaved();

        assert!(!history.is_saved());

        add_track(&mut history, &mut project);
        history.undo(&mut project);

        assert!(!history.is_saved());

        history.mark_saved(history.revision());

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

    #[test]
    fn undoing_a_pruned_asset_brings_it_back() {
        let mut history = History::default();
        let mut project = clip_project();

        let removed = history
            .apply(Command::PruneAssets, &mut project, |project| {
                Ok::<_, EditError>(project.prune_assets())
            })
            .unwrap();

        assert_eq!(removed.len(), 1);
        assert!(project.assets.is_empty());
        assert_eq!(history.next_undo(), Some(Command::PruneAssets));

        history.undo(&mut project);

        assert_eq!(*project.assets, removed);

        let refused = history.apply(Command::RemoveAsset, &mut project, |project| {
            project.place_clip(removed[0].id, 0, Time::ZERO)?;
            project.remove_asset(removed[0].id)
        });

        assert_eq!(refused, Err(EditError::AssetInUse(removed[0].id)));
        assert_eq!(*project.assets, removed);
        assert!(project.timeline.tracks[0].clips().is_empty());
    }

    #[test]
    fn sequence_settings_changes_undo_and_unchanged_ones_are_not_recorded() {
        let mut history = History::default();
        let mut project = Project::new("test");

        let original = project.settings;
        let changed = SequenceSettings {
            frame_rate: FrameRate::FPS_24,
            width: NonZero::new(3840).unwrap(),
            ..original
        };
        let set = |history: &mut History, project: &mut Project, settings| {
            history
                .apply(Command::SetSequenceSettings, project, |project| {
                    Ok::<_, EditError>(project.set_settings(settings))
                })
                .unwrap()
        };

        assert_eq!(set(&mut history, &mut project, changed), original);
        assert_eq!(project.settings, changed);
        assert_eq!(history.next_undo(), Some(Command::SetSequenceSettings));

        set(&mut history, &mut project, changed);
        history.undo(&mut project);

        assert_eq!(project.settings, original);
        assert!(!history.can_undo());

        history.redo(&mut project);

        assert_eq!(project.settings, changed);
    }

    #[test]
    fn snapshots_share_the_assets_and_tracks_an_edit_leaves_alone() {
        let mut history = History::default();
        let mut project = clip_project();
        let asset = project.assets[0].id;
        let upper = project.timeline.add_track(TrackKind::Video);
        let lower_clip = project.place_clip(asset, 0, Time::ZERO).unwrap().id;
        project.place_clip(asset, upper, Time::ZERO).unwrap();

        history
            .apply(Command::MoveClip, &mut project, |project| {
                project.move_clip(lower_clip, 0, Time::from_seconds(10))
            })
            .unwrap();

        let snapshot = &history.undo.back().unwrap().project;
        assert!(std::sync::Arc::ptr_eq(&snapshot.assets, &project.assets));
        assert_eq!(
            snapshot.timeline.tracks[upper].clips().as_ptr(),
            project.timeline.tracks[upper].clips().as_ptr()
        );
        assert_ne!(
            snapshot.timeline.tracks[0].clips().as_ptr(),
            project.timeline.tracks[0].clips().as_ptr()
        );
        assert_eq!(snapshot.timeline.tracks[0].clips()[0].start, Time::ZERO);
    }
}
