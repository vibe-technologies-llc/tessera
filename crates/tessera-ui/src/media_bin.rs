use std::path::{Path, PathBuf};

use gpui::{
    AnyElement, AppContext, Context, Entity, ExternalPaths, FontWeight, InteractiveElement,
    IntoElement, ParentElement, PathPromptOptions, Render, SharedString,
    StatefulInteractiveElement, Styled, Window, div,
};
use tessera_media::probe;
use tessera_timeline::{MediaInfo, Project};

use crate::theme;

struct ImportFailure {
    source: SharedString,
    reason: SharedString,
}

pub struct MediaBin {
    project: Entity<Project>,
    probing: Vec<PathBuf>,
    failures: Vec<ImportFailure>,
}

impl MediaBin {
    pub fn new(project: Entity<Project>, cx: &mut Context<Self>) -> Self {
        cx.observe(&project, |_, _, cx| cx.notify()).detach();
        Self {
            project,
            probing: Vec::new(),
            failures: Vec::new(),
        }
    }

    pub fn prompt_import(&mut self, cx: &mut Context<Self>) {
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Import".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(chosen) = chosen.await else {
                return;
            };
            this.update(cx, |bin, cx| match chosen {
                Ok(Some(paths)) => bin.import(paths, cx),
                Ok(None) => {}
                Err(error) => bin.fail("File dialog".into(), format!("{error:#}"), cx),
            })
            .ok();
        })
        .detach();
    }

    fn import(&mut self, paths: impl IntoIterator<Item = PathBuf>, cx: &mut Context<Self>) {
        for path in paths {
            let known = self.probing.contains(&path)
                || self
                    .project
                    .read(cx)
                    .assets
                    .iter()
                    .any(|asset| asset.path == path);
            if known {
                continue;
            }
            self.probing.push(path.clone());
            let probed = cx.background_spawn({
                let path = path.clone();
                async move { probe(path) }
            });
            cx.spawn(async move |this, cx| {
                let probed = probed.await;
                this.update(cx, |bin, cx| bin.finish_probe(path, probed, cx))
                    .ok();
            })
            .detach();
        }
        cx.notify();
    }

    fn finish_probe(
        &mut self,
        path: PathBuf,
        probed: Result<MediaInfo, tessera_media::Error>,
        cx: &mut Context<Self>,
    ) {
        self.probing.retain(|probing| *probing != path);
        match probed {
            Ok(info) if info.streams.is_empty() => {
                self.fail(file_name(&path), "no audio or video streams".into(), cx);
            }
            Ok(info) => self.project.update(cx, |project, cx| {
                project.add_asset(path, info);
                cx.notify();
            }),
            Err(error) => self.fail(file_name(&path), error.to_string(), cx),
        }
        cx.notify();
    }

    fn fail(&mut self, source: SharedString, reason: String, cx: &mut Context<Self>) {
        tracing::warn!(%source, %reason, "import failed");
        self.failures.push(ImportFailure {
            source,
            reason: reason.into(),
        });
        cx.notify();
    }

    fn dismiss_failure(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.failures.len() {
            self.failures.remove(index);
            cx.notify();
        }
    }

    fn body(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let assets = &self.project.read(cx).assets;
        if assets.is_empty() && self.probing.is_empty() && self.failures.is_empty() {
            return vec![
                div()
                    .p_3()
                    .text_color(theme::text_muted())
                    .child("Drop media here or press Ctrl+I to import")
                    .into_any_element(),
            ];
        }
        let assets = assets
            .iter()
            .map(|asset| row(file_name(&asset.path)).into_any_element());
        let probing = self.probing.iter().map(|path| {
            row(file_name(path))
                .flex()
                .justify_between()
                .text_color(theme::text_muted())
                .child("Probing…")
                .into_any_element()
        });
        let failures = self.failures.iter().enumerate().map(|(index, failure)| {
            div()
                .id(("import-failure", index))
                .px_3()
                .py_1()
                .cursor_pointer()
                .hover(|style| style.bg(theme::hover()))
                .on_click(cx.listener(move |bin, _, _, cx| bin.dismiss_failure(index, cx)))
                .child(
                    div()
                        .text_color(theme::error())
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(failure.source.clone()),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::text_muted())
                        .child(failure.reason.clone()),
                )
                .into_any_element()
        });
        assets.chain(probing).chain(failures).collect()
    }
}

impl Render for MediaBin {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = self.body(cx);
        div()
            .id("media-bin")
            .size_full()
            .flex()
            .flex_col()
            .bg(theme::panel())
            .drag_over::<ExternalPaths>(|style, _, _, _| style.bg(theme::drop_target()))
            .on_drop(cx.listener(|bin, dropped: &ExternalPaths, _, cx| {
                bin.import(dropped.paths().iter().cloned(), cx);
            }))
            .child(
                div()
                    .px_3()
                    .py_2()
                    .flex()
                    .justify_between()
                    .items_center()
                    .border_b_1()
                    .border_color(theme::border())
                    .text_color(theme::text_muted())
                    .child("Media")
                    .child(
                        div()
                            .id("import")
                            .px_2()
                            .rounded_sm()
                            .cursor_pointer()
                            .hover(|style| style.bg(theme::hover()).text_color(theme::text()))
                            .on_click(cx.listener(|bin, _, _, cx| bin.prompt_import(cx)))
                            .child("Import…"),
                    ),
            )
            .child(
                div()
                    .id("media-bin-items")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .children(body),
            )
    }
}

fn row(name: SharedString) -> gpui::Div {
    div().px_3().py_1().child(name)
}

fn file_name(path: &Path) -> SharedString {
    path.file_name()
        .map_or_else(
            || path.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        )
        .into()
}
