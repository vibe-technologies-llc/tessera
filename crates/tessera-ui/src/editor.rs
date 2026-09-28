use gpui::{App, AppContext, Entity};
use tessera_timeline::{Command, History, Project};

#[derive(Clone)]
pub struct ProjectEditor {
    project: Entity<Project>,
    history: Entity<History>,
}

impl ProjectEditor {
    pub fn new(project: Entity<Project>, cx: &mut App) -> Self {
        Self {
            project,
            history: cx.new(|_| History::default()),
        }
    }

    pub fn project(&self) -> &Entity<Project> {
        &self.project
    }

    pub fn apply<T, E>(
        &self,
        command: Command,
        cx: &mut App,
        edit: impl FnOnce(&mut Project) -> Result<T, E>,
    ) -> Result<T, E> {
        self.project.update(cx, |project, cx| {
            let edited = self
                .history
                .update(cx, |history, _| history.apply(command, project, edit));
            if edited.is_ok() {
                cx.notify();
            }
            edited
        })
    }

    pub fn perform<T>(
        &self,
        command: Command,
        cx: &mut App,
        edit: impl FnOnce(&mut Project) -> T,
    ) -> T {
        let Ok(performed) = self.apply(command, cx, |project| {
            Ok::<_, std::convert::Infallible>(edit(project))
        });
        performed
    }

    pub fn replace(&self, project: Project, cx: &mut App) {
        self.history
            .update(cx, |history, _| *history = History::default());
        self.project.update(cx, |current, cx| {
            *current = project;
            cx.notify();
        });
    }

    pub fn undo(&self, cx: &mut App) {
        self.step(cx, "undid", History::undo);
    }

    pub fn redo(&self, cx: &mut App) {
        self.step(cx, "redid", History::redo);
    }

    fn step(
        &self,
        cx: &mut App,
        done: &'static str,
        step: impl FnOnce(&mut History, &mut Project) -> Option<Command>,
    ) {
        self.project.update(cx, |project, cx| {
            let stepped = self.history.update(cx, |history, _| step(history, project));
            if let Some(command) = stepped {
                tracing::debug!(%command, "{done}");
                cx.notify();
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use gpui::TestAppContext;
    use tessera_timeline::TrackKind;

    use super::*;

    #[gpui::test]
    fn replacing_swaps_the_project_and_forgets_its_history(cx: &mut TestAppContext) {
        let project = cx.new(|_| Project::new("first"));
        let editor = cx.update(|cx| ProjectEditor::new(project.clone(), cx));
        let add_track = |cx: &mut App| {
            editor.perform(Command::AddTrack, cx, |project| {
                project.timeline.add_track(TrackKind::Video);
            });
        };
        cx.update(add_track);
        cx.update(add_track);
        cx.update(|cx| editor.undo(cx));
        let mut replacement = Project::new("second");
        replacement.timeline.add_track(TrackKind::Audio);
        cx.update(|cx| editor.replace(replacement.clone(), cx));
        let current = |cx: &mut TestAppContext| cx.read(|cx| project.read(cx).clone());
        assert_eq!(current(cx), replacement);
        cx.update(|cx| editor.undo(cx));
        assert_eq!(current(cx), replacement);
        cx.update(|cx| editor.redo(cx));
        assert_eq!(current(cx), replacement);
    }
}
