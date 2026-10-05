use std::{
    fs, io,
    path::{Path, PathBuf},
};

use gpui::{
    App, AppContext, Context, EventEmitter, FocusHandle, Focusable, Global, InteractiveElement,
    IntoElement, ParentElement, Render, StatefulInteractiveElement, Styled, Window, div,
    prelude::FluentBuilder, px,
};

use crate::{
    Confirm, DIALOG_CONTEXT, Dismiss, SelectNext, SelectPrevious, media_bin::file_name, theme,
};

pub const MAX_RECENT_PROJECTS: usize = 10;
const STATE_DIRECTORY: &str = "tessera";
const RECENT_FILE: &str = "recent-projects";
const DIALOG_WIDTH: f32 = 460.;
const NO_RECENT_PROJECTS: &str = "No recent projects";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RecentProjects {
    paths: Vec<PathBuf>,
    file: Option<PathBuf>,
}

impl Global for RecentProjects {}

impl RecentProjects {
    pub fn stored_in(file: PathBuf) -> Self {
        let paths = match fs::read_to_string(&file) {
            Ok(text) => parse(&text),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(error) => {
                tracing::warn!(file = %file.display(), %error, "cannot read the recent projects");
                Vec::new()
            }
        };
        Self {
            paths,
            file: Some(file),
        }
    }

    pub fn paths(&self) -> &[PathBuf] {
        &self.paths
    }

    pub fn remember(&mut self, path: PathBuf) {
        if path.to_str().is_none() {
            return;
        }
        self.paths.retain(|known| *known != path);
        self.paths.insert(0, path);
        self.paths.truncate(MAX_RECENT_PROJECTS);
    }

    pub fn forget(&mut self, path: &Path) {
        self.paths.retain(|known| known != path);
    }

    fn contents(&self) -> String {
        self.paths
            .iter()
            .filter_map(|path| path.to_str())
            .map(|path| format!("{path}\n"))
            .collect()
    }
}

fn parse(text: &str) -> Vec<PathBuf> {
    let mut recent = RecentProjects::default();
    for line in text.lines().rev().filter(|line| !line.trim().is_empty()) {
        recent.remember(PathBuf::from(line));
    }
    recent.paths
}

pub fn default_file() -> Option<PathBuf> {
    let state = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| std::env::home_dir().map(|home| home.join(".local").join("state")))?;
    Some(state.join(STATE_DIRECTORY).join(RECENT_FILE))
}

pub fn update(cx: &mut App, change: impl FnOnce(&mut RecentProjects)) {
    let recent = cx.default_global::<RecentProjects>();
    let before = recent.paths.clone();
    change(recent);
    if recent.paths == before {
        return;
    }
    let Some(file) = recent.file.clone() else {
        return;
    };
    let contents = recent.contents();
    cx.background_spawn(async move {
        if let Err(error) = write(&file, &contents) {
            tracing::warn!(file = %file.display(), %error, "cannot store the recent projects");
        }
    })
    .detach();
}

fn write(file: &Path, contents: &str) -> io::Result<()> {
    if let Some(directory) = file.parent() {
        fs::create_dir_all(directory)?;
    }
    fs::write(file, contents)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecentDialogEvent {
    Open(PathBuf),
    Cancelled,
}

pub struct RecentProjectsDialog {
    paths: Vec<PathBuf>,
    selected: usize,
    focus_handle: FocusHandle,
}

impl EventEmitter<RecentDialogEvent> for RecentProjectsDialog {}

impl Focusable for RecentProjectsDialog {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl RecentProjectsDialog {
    pub fn new(paths: Vec<PathBuf>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle);
        Self {
            paths,
            selected: 0,
            focus_handle,
        }
    }

    fn select_by(&mut self, step: isize, cx: &mut Context<Self>) {
        let Some(last) = self.paths.len().checked_sub(1) else {
            return;
        };
        self.selected = self.selected.saturating_add_signed(step).min(last);
        cx.notify();
    }

    fn open(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(path) = self.paths.get(index) {
            cx.emit(RecentDialogEvent::Open(path.clone()));
        }
    }

    fn row(&self, index: usize, path: &Path, cx: &mut Context<Self>) -> impl IntoElement {
        let directory = path
            .parent()
            .map(|directory| directory.display().to_string())
            .unwrap_or_default();
        let selected = index == self.selected;
        div()
            .id(("recent", index))
            .px_3()
            .py_1()
            .rounded_sm()
            .cursor_pointer()
            .when(selected, |row| row.bg(theme::hover()))
            .hover(|style| style.bg(theme::hover()))
            .on_click(cx.listener(move |dialog, _, _, cx| dialog.open(index, cx)))
            .child(div().truncate().child(file_name(path)))
            .child(
                div()
                    .text_xs()
                    .text_color(theme::text_muted())
                    .truncate()
                    .child(directory),
            )
    }
}

impl Render for RecentProjectsDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rows: Vec<_> = self
            .paths
            .iter()
            .enumerate()
            .map(|(index, path)| self.row(index, path, cx).into_any_element())
            .collect();
        let empty = rows.is_empty().then(|| {
            div()
                .px_3()
                .py_1()
                .text_color(theme::text_muted())
                .child(NO_RECENT_PROJECTS)
        });
        div()
            .id("recent-projects")
            .track_focus(&self.focus_handle)
            .key_context(DIALOG_CONTEXT)
            .on_action(cx.listener(|dialog, _: &SelectNext, _, cx| dialog.select_by(1, cx)))
            .on_action(cx.listener(|dialog, _: &SelectPrevious, _, cx| dialog.select_by(-1, cx)))
            .on_action(cx.listener(|dialog, _: &Confirm, _, cx| dialog.open(dialog.selected, cx)))
            .on_action(cx.listener(|_, _: &Dismiss, _, cx| cx.emit(RecentDialogEvent::Cancelled)))
            .w(px(DIALOG_WIDTH))
            .p_3()
            .flex()
            .flex_col()
            .gap_1()
            .rounded_md()
            .border_1()
            .border_color(theme::border())
            .bg(theme::panel())
            .child(
                div()
                    .pb_1()
                    .text_color(theme::text_muted())
                    .child("Open Recent"),
            )
            .children(rows)
            .children(empty)
    }
}

#[cfg(test)]
mod tests {
    use gpui::TestAppContext;

    use super::*;

    fn paths(recent: &RecentProjects) -> Vec<&str> {
        recent
            .paths()
            .iter()
            .map(|path| path.to_str().unwrap())
            .collect()
    }

    #[test]
    fn the_latest_project_comes_first_once_and_the_list_is_capped() {
        let mut recent = RecentProjects::default();

        recent.remember("/a.tessera".into());
        recent.remember("/b.tessera".into());
        recent.remember("/a.tessera".into());

        assert_eq!(paths(&recent), ["/a.tessera", "/b.tessera"]);

        for index in 0..MAX_RECENT_PROJECTS {
            recent.remember(format!("/{index}.tessera").into());
        }

        assert_eq!(recent.paths().len(), MAX_RECENT_PROJECTS);
        assert_eq!(recent.paths()[0], PathBuf::from("/9.tessera"));

        recent.forget(Path::new("/9.tessera"));

        assert_eq!(recent.paths()[0], PathBuf::from("/8.tessera"));
    }

    #[test]
    fn the_stored_list_reads_back_in_order_without_blank_lines_or_repeats() {
        assert_eq!(
            parse("/b.tessera\n\n/a.tessera\n/b.tessera\n"),
            [PathBuf::from("/b.tessera"), "/a.tessera".into()]
        );

        let mut recent = RecentProjects::default();
        recent.remember("/a.tessera".into());
        recent.remember("/b.tessera".into());

        assert_eq!(parse(&recent.contents()), recent.paths());
    }

    #[test]
    fn the_list_is_stored_in_its_file_and_read_back() {
        let directory = std::env::temp_dir().join(format!("tessera-recent-{}", std::process::id()));
        fs::remove_dir_all(&directory).ok();
        let file = directory.join("nested").join(RECENT_FILE);

        assert!(RecentProjects::stored_in(file.clone()).paths().is_empty());

        let mut recent = RecentProjects::stored_in(file.clone());
        recent.remember("/a.tessera".into());
        write(&file, &recent.contents()).unwrap();

        assert_eq!(RecentProjects::stored_in(file).paths(), recent.paths());
        fs::remove_dir_all(&directory).ok();
    }

    #[gpui::test]
    fn arrow_keys_pick_a_project_and_enter_opens_it(cx: &mut TestAppContext) {
        cx.update(crate::init);
        let paths: Vec<PathBuf> = vec!["/a.tessera".into(), "/b.tessera".into()];
        let (dialog, cx) =
            cx.add_window_view(|window, cx| RecentProjectsDialog::new(paths.clone(), window, cx));
        let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        cx.update(|_, cx| {
            let events = events.clone();
            cx.subscribe(&dialog, move |_, event: &RecentDialogEvent, _| {
                events.borrow_mut().push(event.clone());
            })
            .detach();
        });

        cx.simulate_keystrokes("down down enter escape");

        assert_eq!(
            *events.borrow(),
            [
                RecentDialogEvent::Open("/b.tessera".into()),
                RecentDialogEvent::Cancelled
            ]
        );
    }
}
