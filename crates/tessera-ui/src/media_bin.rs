use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use gpui::{
    AnyElement, AppContext, Context, Entity, ExternalPaths, FontWeight, InteractiveElement,
    IntoElement, ObjectFit, ParentElement, PathPromptOptions, Render, RenderImage, SharedString,
    StatefulInteractiveElement, Styled, StyledImage, Window, div, img, px,
};
use tessera_media::{VideoDecoder, VideoFrame, probe};
use tessera_timeline::{Asset, AssetId, FLICKS_PER_SECOND, MediaInfo, Project, Time};

use crate::theme;

const THUMBNAIL_WIDTH: u32 = 96;
const THUMBNAIL_HEIGHT: u32 = 54;
const THUMBNAIL_PIXEL_DENSITY: u32 = 2;
const THUMBNAIL_POSITION_DIVISOR: i64 = 10;
const UNKNOWN_DURATION: &str = "--:--";

#[derive(Clone)]
pub struct DraggedAsset {
    pub id: AssetId,
    name: SharedString,
}

impl Render for DraggedAsset {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .rounded_sm()
            .border_1()
            .border_color(theme::border())
            .bg(theme::hover())
            .text_sm()
            .text_color(theme::text())
            .child(self.name.clone())
    }
}

struct Imported {
    info: MediaInfo,
    thumbnail: Option<Arc<RenderImage>>,
}

struct ImportFailure {
    source: SharedString,
    reason: SharedString,
}

pub struct MediaBin {
    project: Entity<Project>,
    probing: Vec<PathBuf>,
    failures: Vec<ImportFailure>,
    thumbnails: HashMap<AssetId, Arc<RenderImage>>,
}

impl MediaBin {
    pub fn new(project: Entity<Project>, cx: &mut Context<Self>) -> Self {
        cx.observe(&project, |_, _, cx| cx.notify()).detach();
        Self {
            project,
            probing: Vec::new(),
            failures: Vec::new(),
            thumbnails: HashMap::new(),
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
                async move { import_file(&path) }
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
        probed: Result<Imported, tessera_media::Error>,
        cx: &mut Context<Self>,
    ) {
        self.probing.retain(|probing| *probing != path);
        match probed {
            Ok(imported) if imported.info.streams.is_empty() => {
                self.fail(file_name(&path), "no audio or video streams".into(), cx);
            }
            Ok(Imported { info, thumbnail }) => {
                let id = self.project.update(cx, |project, cx| {
                    cx.notify();
                    project.add_asset(path, info)
                });
                if let Some(thumbnail) = thumbnail {
                    self.thumbnails.insert(id, thumbnail);
                }
            }
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
            .map(|asset| asset_row(asset, self.thumbnails.get(&asset.id)).into_any_element());
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

fn asset_row(asset: &Asset, thumbnail: Option<&Arc<RenderImage>>) -> impl IntoElement {
    let duration = asset
        .info
        .duration
        .map_or_else(|| UNKNOWN_DURATION.to_owned(), duration_label);
    let dragged = DraggedAsset {
        id: asset.id,
        name: file_name(&asset.path),
    };
    div()
        .id(("asset", asset.id.0))
        .px_3()
        .py_1()
        .flex()
        .items_center()
        .gap_2()
        .cursor_grab()
        .hover(|style| style.bg(theme::hover()))
        .on_drag(dragged, |dragged, _, _, cx| cx.new(|_| dragged.clone()))
        .child(thumbnail_frame(asset, thumbnail))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(div().truncate().child(file_name(&asset.path)))
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::text_muted())
                        .child(duration),
                ),
        )
}

fn thumbnail_frame(asset: &Asset, thumbnail: Option<&Arc<RenderImage>>) -> impl IntoElement {
    let frame = div()
        .w(px(THUMBNAIL_WIDTH as f32))
        .h(px(THUMBNAIL_HEIGHT as f32))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded_sm()
        .overflow_hidden()
        .bg(theme::frame())
        .text_xs()
        .text_color(theme::text_muted());
    match thumbnail {
        Some(thumbnail) => frame.child(
            img(thumbnail.clone())
                .size_full()
                .object_fit(ObjectFit::Contain),
        ),
        None if asset.info.video().next().is_none() => frame.child("Audio"),
        None => frame,
    }
}

fn import_file(path: &Path) -> Result<Imported, tessera_media::Error> {
    let info = probe(path)?;
    let has_video = info.video().next().is_some();
    let thumbnail = has_video.then(|| thumbnail(path, info.duration)).flatten();
    Ok(Imported { info, thumbnail })
}

fn thumbnail(path: &Path, duration: Option<Time>) -> Option<Arc<RenderImage>> {
    match decode_thumbnail(path, duration) {
        Ok(frame) => render_image(frame),
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "thumbnail failed");
            None
        }
    }
}

fn decode_thumbnail(
    path: &Path,
    duration: Option<Time>,
) -> Result<Arc<VideoFrame>, tessera_media::Error> {
    let time = duration.map_or(Time::ZERO, |duration| {
        Time::from_flicks(duration.flicks() / THUMBNAIL_POSITION_DIVISOR)
    });
    VideoDecoder::open(path)?
        .fit_within(
            THUMBNAIL_WIDTH * THUMBNAIL_PIXEL_DENSITY,
            THUMBNAIL_HEIGHT * THUMBNAIL_PIXEL_DENSITY,
        )
        .frame_at(time)
}

fn render_image(frame: Arc<VideoFrame>) -> Option<Arc<RenderImage>> {
    let frame = Arc::unwrap_or_clone(frame);
    let buffer = image::RgbaImage::from_raw(frame.width, frame.height, frame.bgra)?;
    Some(Arc::new(RenderImage::new([image::Frame::new(buffer)])))
}

fn duration_label(duration: Time) -> String {
    let seconds = (duration.flicks() + FLICKS_PER_SECOND / 2).div_euclid(FLICKS_PER_SECOND);
    let (hours, minutes, seconds) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

pub fn file_name(path: &Path) -> SharedString {
    path.file_name()
        .map_or_else(
            || path.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        )
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_round_to_the_nearest_second() {
        let label = |flicks| duration_label(Time::from_flicks(flicks));
        assert_eq!(label(0), "0:00");
        assert_eq!(label(FLICKS_PER_SECOND / 2 - 1), "0:00");
        assert_eq!(label(FLICKS_PER_SECOND / 2), "0:01");
        assert_eq!(duration_label(Time::from_seconds(59)), "0:59");
        assert_eq!(duration_label(Time::from_seconds(61)), "1:01");
        assert_eq!(duration_label(Time::from_seconds(3600 + 62)), "1:01:02");
    }
}
