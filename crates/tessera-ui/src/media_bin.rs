use std::{
    collections::{HashMap, HashSet, VecDeque},
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use gpui::{
    AnyElement, AppContext, Context, ElementId, Entity, ExternalPaths, FontWeight,
    InteractiveElement, IntoElement, ObjectFit, ParentElement, PathPromptOptions, Render,
    RenderImage, SharedString, StatefulInteractiveElement, Styled, StyledImage, Window, div, img,
    px,
};
use tessera_media::{VideoDecoder, VideoFrame, probe};
use tessera_timeline::{Asset, AssetId, Command, FLICKS_PER_SECOND, MediaInfo, Project, Time};

use crate::{editor::ProjectEditor, frame_image::render_image, theme};

const THUMBNAIL_WIDTH: u32 = 96;
const THUMBNAIL_HEIGHT: u32 = 54;
const THUMBNAIL_PIXEL_DENSITY: u32 = 2;
const THUMBNAIL_POSITION_DIVISOR: i64 = 10;
const UNKNOWN_DURATION: &str = "--:--";
const MAX_RUNNING_JOBS: usize = 4;
const MAX_FAILURE_LINES: usize = 3;
const MAX_FOLDER_DEPTH: usize = 8;
const MEDIA_EXTENSIONS: &[&str] = &[
    "mkv", "mp4", "m4v", "mov", "avi", "webm", "mpg", "mpeg", "ts", "mts", "m2ts", "wmv", "flv",
    "ogv", "3gp", "mxf", "mp3", "wav", "flac", "ogg", "opus", "aac", "m4a", "aif", "aiff", "wma",
];

#[derive(Clone)]
pub struct DraggedAsset {
    pub id: AssetId,
    name: SharedString,
}

impl DraggedAsset {
    pub(crate) fn of(asset: &Asset) -> Self {
        Self {
            id: asset.id,
            name: file_name(&asset.path),
        }
    }
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

enum Job {
    Probe(PathBuf),
    Scan(PathBuf),
    Relink {
        id: AssetId,
        path: PathBuf,
    },
    Exists {
        id: AssetId,
        path: PathBuf,
    },
    Thumbnail {
        id: AssetId,
        path: PathBuf,
        duration: Option<Time>,
    },
}

enum Finished {
    Probed {
        path: PathBuf,
        info: Result<MediaInfo, tessera_media::Error>,
    },
    Scanned {
        folder: PathBuf,
        files: Vec<PathBuf>,
    },
    Relinked {
        id: AssetId,
        path: PathBuf,
        info: Result<MediaInfo, tessera_media::Error>,
    },
    Exists {
        id: AssetId,
        path: PathBuf,
        present: bool,
    },
    Thumbnail {
        id: AssetId,
        path: PathBuf,
        image: Option<Arc<RenderImage>>,
    },
}

impl Job {
    fn run(self) -> Finished {
        match self {
            Self::Probe(path) => Finished::Probed {
                info: probe(&path),
                path,
            },
            Self::Scan(folder) => Finished::Scanned {
                files: media_files_under(&folder),
                folder,
            },
            Self::Relink { id, path } => Finished::Relinked {
                info: probe(&path),
                id,
                path,
            },
            Self::Exists { id, path } => Finished::Exists {
                present: path.exists(),
                id,
                path,
            },
            Self::Thumbnail { id, path, duration } => Finished::Thumbnail {
                image: thumbnail(&path, duration),
                id,
                path,
            },
        }
    }
}

struct Presence {
    path: PathBuf,
    present: Option<bool>,
}

struct ImportFailure {
    source: SharedString,
    reason: SharedString,
}

pub struct MediaBin {
    editor: ProjectEditor,
    project: Entity<Project>,
    probing: Vec<PathBuf>,
    failures: Vec<ImportFailure>,
    thumbnails: HashMap<AssetId, Arc<RenderImage>>,
    thumbnail_requested: HashMap<AssetId, PathBuf>,
    presence: HashMap<AssetId, Presence>,
    retired: Vec<Arc<RenderImage>>,
    waiting: VecDeque<Job>,
    running: usize,
    generation: u64,
}

impl MediaBin {
    pub fn new(editor: ProjectEditor, cx: &mut Context<Self>) -> Self {
        let project = editor.project().clone();
        cx.observe(&project, |bin, _, cx| {
            bin.sync_assets(cx);
            cx.notify();
        })
        .detach();
        let mut bin = Self {
            editor,
            project,
            probing: Vec::new(),
            failures: Vec::new(),
            thumbnails: HashMap::new(),
            thumbnail_requested: HashMap::new(),
            presence: HashMap::new(),
            retired: Vec::new(),
            waiting: VecDeque::new(),
            running: 0,
            generation: 0,
        };
        bin.sync_assets(cx);
        bin
    }

    pub fn project_replaced(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        self.running = 0;
        self.waiting.clear();
        self.probing.clear();
        self.failures.clear();
        self.thumbnail_requested.clear();
        self.presence.clear();
        self.retired
            .extend(self.thumbnails.drain().map(|(_, thumbnail)| thumbnail));
        self.sync_assets(cx);
        cx.notify();
    }

    fn sync_assets(&mut self, cx: &mut Context<Self>) {
        let project = self.project.read(cx);
        let gone = |id: &AssetId| project.asset(*id).is_none();
        let removed: Vec<AssetId> = self
            .thumbnail_requested
            .keys()
            .chain(self.presence.keys())
            .copied()
            .filter(gone)
            .collect();
        let mut wanted = Vec::new();
        for asset in project.assets.iter() {
            let has_video = asset.info.video().next().is_some();
            let thumbnail_stale =
                has_video && self.thumbnail_requested.get(&asset.id) != Some(&asset.path);
            if thumbnail_stale {
                wanted.push(Job::Thumbnail {
                    id: asset.id,
                    path: asset.path.clone(),
                    duration: asset.info.duration,
                });
            }
            if self.presence.get(&asset.id).map(|known| &known.path) != Some(&asset.path) {
                wanted.push(Job::Exists {
                    id: asset.id,
                    path: asset.path.clone(),
                });
            }
        }
        for id in removed {
            self.thumbnail_requested.remove(&id);
            self.presence.remove(&id);
            self.retired.extend(self.thumbnails.remove(&id));
        }
        for job in wanted {
            match &job {
                Job::Thumbnail { id, path, .. } => {
                    self.thumbnail_requested.insert(*id, path.clone());
                    self.retired.extend(self.thumbnails.remove(id));
                }
                Job::Exists { id, path } => {
                    self.presence.insert(
                        *id,
                        Presence {
                            path: path.clone(),
                            present: None,
                        },
                    );
                }
                _ => {}
            }
            self.waiting.push_back(job);
        }
        self.pump(cx);
    }

    fn pump(&mut self, cx: &mut Context<Self>) {
        while self.running < MAX_RUNNING_JOBS {
            let Some(job) = self.waiting.pop_front() else {
                break;
            };
            self.running += 1;
            let generation = self.generation;
            let finished = cx.background_spawn(async move { job.run() });
            cx.spawn(async move |this, cx| {
                let finished = finished.await;
                this.update(cx, |bin, cx| {
                    if bin.generation == generation {
                        bin.running -= 1;
                        bin.finish(finished, cx);
                        bin.pump(cx);
                    }
                })
                .ok();
            })
            .detach();
        }
    }

    fn finish(&mut self, finished: Finished, cx: &mut Context<Self>) {
        match finished {
            Finished::Probed { path, info } => self.finish_probe(path, info, cx),
            Finished::Scanned { folder, files } => {
                if self.probing.contains(&folder) {
                    self.probing.retain(|probing| *probing != folder);
                    self.import(files, cx);
                }
            }
            Finished::Relinked { id, path, info } => self.finish_relink(id, path, info, cx),
            Finished::Exists { id, path, present } => {
                if let Some(known) = self
                    .presence
                    .get_mut(&id)
                    .filter(|known| known.path == path)
                {
                    known.present = Some(present);
                    cx.notify();
                }
            }
            Finished::Thumbnail { id, path, image } => self.finish_thumbnail(id, &path, image, cx),
        }
    }

    fn finish_thumbnail(
        &mut self,
        id: AssetId,
        path: &Path,
        thumbnail: Option<Arc<RenderImage>>,
        cx: &mut Context<Self>,
    ) {
        let still_held = self
            .project
            .read(cx)
            .asset(id)
            .is_some_and(|asset| asset.path == path);
        if let Some(thumbnail) = thumbnail.filter(|_| still_held) {
            self.retired.extend(self.thumbnails.insert(id, thumbnail));
            cx.notify();
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
            if path.to_str().is_none() {
                self.fail(
                    file_name(&path),
                    "the path is not valid UTF-8, so a project could not save it".into(),
                    cx,
                );
                continue;
            }
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
            self.waiting.push_back(if path.is_dir() {
                Job::Scan(path)
            } else {
                Job::Probe(path)
            });
        }
        self.pump(cx);
        cx.notify();
    }

    fn finish_probe(
        &mut self,
        path: PathBuf,
        probed: Result<MediaInfo, tessera_media::Error>,
        cx: &mut Context<Self>,
    ) {
        if !self.probing.contains(&path) {
            return;
        }
        self.probing.retain(|probing| *probing != path);
        match probed {
            Ok(info) if info.streams.is_empty() => {
                self.fail(file_name(&path), "no audio or video streams".into(), cx);
            }
            Ok(info) => {
                self.editor.perform(Command::ImportMedia, cx, |project| {
                    project.add_asset(path, info)
                });
            }
            Err(error) => self.fail(file_name(&path), error.to_string(), cx),
        }
        cx.notify();
    }

    pub fn prompt_relink(&mut self, id: AssetId, cx: &mut Context<Self>) {
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Relink".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = chosen.await else {
                return;
            };
            if let Some(path) = paths.into_iter().next() {
                this.update(cx, |bin, cx| bin.relink(id, path, cx)).ok();
            }
        })
        .detach();
    }

    fn relink(&mut self, id: AssetId, path: PathBuf, cx: &mut Context<Self>) {
        if path.to_str().is_none() {
            self.fail(
                file_name(&path),
                "the path is not valid UTF-8, so a project could not save it".into(),
                cx,
            );
            return;
        }
        self.waiting.push_back(Job::Relink { id, path });
        self.pump(cx);
    }

    fn finish_relink(
        &mut self,
        id: AssetId,
        path: PathBuf,
        probed: Result<MediaInfo, tessera_media::Error>,
        cx: &mut Context<Self>,
    ) {
        let relinked = probed.map_err(|error| error.to_string()).and_then(|info| {
            self.editor
                .apply(Command::RelinkAsset, cx, |project| {
                    project.relink_asset(id, path.clone(), info)
                })
                .map_err(|error| error.to_string())
        });
        if let Err(reason) = relinked {
            self.fail(file_name(&path), reason, cx);
        }
    }

    fn remove_asset(&mut self, id: AssetId, cx: &mut Context<Self>) {
        let name = self
            .project
            .read(cx)
            .asset(id)
            .map(|asset| file_name(&asset.path));
        let removed = self.editor.apply(Command::RemoveAsset, cx, |project| {
            project.remove_asset(id).map(drop)
        });
        if let Err(error) = removed {
            self.fail(name.unwrap_or_default(), error.to_string(), cx);
        }
    }

    fn prune_assets(&mut self, cx: &mut Context<Self>) {
        self.editor
            .perform(Command::PruneAssets, cx, |project| project.prune_assets());
    }

    fn fail(&mut self, source: SharedString, reason: String, cx: &mut Context<Self>) {
        tracing::warn!(%source, %reason, "import failed");
        self.failures.push(ImportFailure {
            source,
            reason: reason.into(),
        });
        cx.notify();
    }

    fn dismiss_failures(&mut self, cx: &mut Context<Self>) {
        self.failures.clear();
        cx.notify();
    }

    fn failure_row(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let first = self.failures.first()?;
        let title = match self.failures.len() {
            1 => first.source.clone(),
            count => format!("{count} imports failed").into(),
        };
        let shown = self.failures.iter().take(MAX_FAILURE_LINES);
        let hidden = self.failures.len().saturating_sub(MAX_FAILURE_LINES);
        let detail =
            |text: SharedString| div().text_xs().text_color(theme::text_muted()).child(text);
        let lines: Vec<AnyElement> = match self.failures.len() {
            1 => vec![detail(first.reason.clone()).into_any_element()],
            _ => shown
                .map(|failure| {
                    detail(format!("{}: {}", failure.source, failure.reason).into())
                        .into_any_element()
                })
                .chain(
                    (hidden > 0)
                        .then(|| detail(format!("and {hidden} more").into()).into_any_element()),
                )
                .collect(),
        };
        Some(
            div()
                .id("import-failures")
                .px_3()
                .py_1()
                .cursor_pointer()
                .hover(|style| style.bg(theme::hover()))
                .on_click(cx.listener(|bin, _, _, cx| bin.dismiss_failures(cx)))
                .child(
                    div()
                        .text_color(theme::error())
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(title),
                )
                .children(lines)
                .into_any_element(),
        )
    }

    fn body(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let failures = self.failure_row(cx);
        let assets = &self.project.read(cx).assets;
        if assets.is_empty() && self.probing.is_empty() && failures.is_none() {
            return vec![
                div()
                    .p_3()
                    .text_color(theme::text_muted())
                    .child("Drop media here or press Ctrl+I to import")
                    .into_any_element(),
            ];
        }
        let used = used_assets(self.project.read(cx));
        let held = self.project.read(cx).assets.clone();
        let assets = held.iter().map(|asset| {
            let state = RowState {
                used: used.contains(&asset.id),
                missing: self
                    .presence
                    .get(&asset.id)
                    .is_some_and(|known| known.present == Some(false)),
            };
            asset_row(asset, self.thumbnails.get(&asset.id), state, cx).into_any_element()
        });
        let probing = self.probing.iter().map(|path| {
            row(file_name(path))
                .flex()
                .justify_between()
                .text_color(theme::text_muted())
                .child("Probing…")
                .into_any_element()
        });
        assets.chain(probing).chain(failures).collect()
    }
}

impl Render for MediaBin {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        for thumbnail in self.retired.drain(..) {
            if let Err(error) = window.drop_image(thumbnail) {
                tracing::warn!(%error, "failed to release a thumbnail");
            }
        }
        let body = self.body(cx);
        let has_unused = {
            let project = self.project.read(cx);
            let used = used_assets(project);
            project.assets.iter().any(|asset| !used.contains(&asset.id))
        };
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
                            .flex()
                            .gap_1()
                            .children(has_unused.then(|| {
                                bin_button("prune", "Remove unused")
                                    .on_click(cx.listener(|bin, _, _, cx| bin.prune_assets(cx)))
                            }))
                            .child(
                                bin_button("import", "Import…")
                                    .on_click(cx.listener(|bin, _, _, cx| bin.prompt_import(cx))),
                            ),
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

struct RowState {
    used: bool,
    missing: bool,
}

fn used_assets(project: &Project) -> HashSet<AssetId> {
    project
        .timeline
        .tracks
        .iter()
        .flat_map(|track| track.clips())
        .map(|clip| clip.asset)
        .collect()
}

fn bin_button(id: impl Into<ElementId>, label: &'static str) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .px_2()
        .rounded_sm()
        .cursor_pointer()
        .hover(|style| style.bg(theme::hover()).text_color(theme::text()))
        .child(label)
}

fn asset_row(
    asset: &Asset,
    thumbnail: Option<&Arc<RenderImage>>,
    state: RowState,
    cx: &mut Context<MediaBin>,
) -> impl IntoElement {
    let id = asset.id;
    let duration = asset
        .info
        .duration
        .map_or_else(|| UNKNOWN_DURATION.to_owned(), duration_label);
    let dragged = DraggedAsset::of(asset);
    let name = div().truncate().child(file_name(&asset.path));
    let name = if state.missing {
        name.text_color(theme::error())
    } else {
        name
    };
    let status = if state.missing {
        Some("Missing")
    } else if !state.used {
        Some("Unused")
    } else {
        None
    };
    let actions = div()
        .flex_none()
        .flex()
        .flex_col()
        .items_end()
        .text_xs()
        .text_color(theme::text_muted())
        .children(status)
        .child(if state.missing {
            bin_button(("relink", id.0), "Relink…")
                .on_click(cx.listener(move |bin, _, _, cx| bin.prompt_relink(id, cx)))
                .into_any_element()
        } else {
            bin_button(("remove", id.0), "Remove")
                .on_click(cx.listener(move |bin, _, _, cx| bin.remove_asset(id, cx)))
                .into_any_element()
        });
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
                .child(name)
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::text_muted())
                        .truncate()
                        .child(format!("{duration} · {}", describe(&asset.info))),
                ),
        )
        .child(actions)
}

fn describe(info: &MediaInfo) -> String {
    let video = info.video().next().map(|video| {
        let rate = video
            .frame_rate
            .map(|rate| format!(" {} fps", trimmed_rate(rate.as_f64())))
            .unwrap_or_default();
        format!("{}×{} {}{rate}", video.width, video.height, video.codec)
    });
    let audio = info.audio().next().map(|audio| {
        format!(
            "{} {} kHz {}",
            audio.codec,
            trimmed_rate(f64::from(audio.sample_rate.get()) / 1000.),
            channel_label(audio.channels.get())
        )
    });
    let parts: Vec<String> = video.into_iter().chain(audio).collect();
    if parts.is_empty() {
        "no streams".to_owned()
    } else {
        parts.join(" · ")
    }
}

fn trimmed_rate(rate: f64) -> String {
    let text = format!("{rate:.2}");
    text.trim_end_matches('0').trim_end_matches('.').to_owned()
}

fn channel_label(channels: u16) -> String {
    match channels {
        1 => "mono".to_owned(),
        2 => "stereo".to_owned(),
        count => format!("{count} ch"),
    }
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

fn media_files_under(folder: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    collect_media_files(folder, MAX_FOLDER_DEPTH, &mut found);
    found.sort();
    found
}

fn collect_media_files(folder: &Path, depth: usize, found: &mut Vec<PathBuf>) {
    let entries = match fs::read_dir(folder) {
        Ok(entries) => entries,
        Err(error) => {
            tracing::warn!(folder = %folder.display(), %error, "cannot list the folder");
            return;
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        match entry.file_type() {
            Ok(kind) if kind.is_dir() => {
                if let Some(deeper) = depth.checked_sub(1) {
                    collect_media_files(&path, deeper, found);
                }
            }
            Ok(kind) if kind.is_file() && has_media_extension(&path) => found.push(path),
            _ => {}
        }
    }
}

fn has_media_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            MEDIA_EXTENSIONS
                .iter()
                .any(|known| known.eq_ignore_ascii_case(extension))
        })
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
    VideoDecoder::open_with(path, &[])?
        .fit_within(
            THUMBNAIL_WIDTH * THUMBNAIL_PIXEL_DENSITY,
            THUMBNAIL_HEIGHT * THUMBNAIL_PIXEL_DENSITY,
        )
        .frame_at(time)
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
    use std::{num::NonZero, os::unix::ffi::OsStrExt};

    use gpui::TestAppContext;
    use tessera_timeline::{AudioStream, Stream, VideoStream};

    use super::*;

    fn video_info() -> MediaInfo {
        MediaInfo {
            duration: Some(Time::from_seconds(4)),
            streams: vec![Stream::Video(VideoStream {
                index: 0,
                codec: "h264".into(),
                width: NonZero::new(640).unwrap(),
                height: NonZero::new(360).unwrap(),
                frame_rate: None,
            })],
        }
    }

    fn audio_info() -> MediaInfo {
        MediaInfo {
            duration: Some(Time::from_seconds(4)),
            streams: vec![Stream::Audio(AudioStream {
                index: 0,
                codec: "opus".into(),
                sample_rate: NonZero::new(48_000).unwrap(),
                channels: NonZero::new(2).unwrap(),
            })],
        }
    }

    fn blank_thumbnail() -> Arc<RenderImage> {
        Arc::new(RenderImage::new([image::Frame::new(
            image::RgbaImage::new(1, 1),
        )]))
    }

    #[gpui::test]
    fn a_replaced_project_drops_the_old_thumbnails_and_imports(cx: &mut TestAppContext) {
        let mut first = Project::new("first");
        let old = first.add_asset("/missing/old.mkv".into(), video_info());
        let project = cx.new(|_| first);
        let (bin, cx) =
            cx.add_window_view(|_, cx| MediaBin::new(ProjectEditor::new(project, cx), cx));
        bin.update(cx, |bin, cx| {
            bin.thumbnails.insert(old, blank_thumbnail());
            bin.probing.push("/missing/late.mkv".into());
            bin.fail("broken.mkv".into(), "no streams".into(), cx);
        });

        let mut second = Project::new("second");
        second.add_asset("/missing/new.mkv".into(), video_info());
        second.add_asset("/missing/new.opus".into(), audio_info());
        bin.update(cx, |bin, cx| {
            bin.editor.replace(second.clone(), cx);
            bin.project_replaced(cx);
            bin.finish_probe("/missing/late.mkv".into(), Ok(video_info()), cx);
        });
        cx.run_until_parked();

        cx.read(|cx| {
            let bin = bin.read(cx);
            assert!(bin.thumbnails.is_empty());
            assert!(bin.probing.is_empty());
            assert!(bin.failures.is_empty());
            assert!(bin.waiting.is_empty());
            assert_eq!(bin.project.read(cx).assets, second.assets);
        });
    }

    fn scratch_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tessera-bin-{}-{name}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn failure_sources(bin: &MediaBin) -> Vec<String> {
        let mut sources: Vec<String> = bin
            .failures
            .iter()
            .map(|failure| failure.source.to_string())
            .collect();
        sources.sort();
        sources
    }

    #[gpui::test]
    fn paths_that_are_not_utf8_are_refused_at_import(cx: &mut TestAppContext) {
        let project = cx.new(|_| Project::new("test"));
        let (bin, cx) =
            cx.add_window_view(|_, cx| MediaBin::new(ProjectEditor::new(project, cx), cx));
        let bad = PathBuf::from(std::ffi::OsStr::from_bytes(b"/media/\xff.mkv"));

        bin.update(cx, |bin, cx| bin.import([bad], cx));

        cx.read(|cx| {
            let bin = bin.read(cx);
            assert_eq!(bin.failures.len(), 1);
            assert!(bin.probing.is_empty());
            assert!(bin.waiting.is_empty());
        });
    }

    #[gpui::test]
    fn only_a_few_imports_run_at_once(cx: &mut TestAppContext) {
        let project = cx.new(|_| Project::new("test"));
        let (bin, cx) =
            cx.add_window_view(|_, cx| MediaBin::new(ProjectEditor::new(project, cx), cx));
        let files: Vec<PathBuf> = (0..10)
            .map(|index| format!("/missing/{index}.mkv").into())
            .collect();

        bin.update(cx, |bin, cx| bin.import(files, cx));

        cx.read(|cx| {
            let bin = bin.read(cx);
            assert_eq!(bin.running, MAX_RUNNING_JOBS);
            assert_eq!(bin.waiting.len(), 10 - MAX_RUNNING_JOBS);
        });

        cx.run_until_parked();

        cx.read(|cx| {
            let bin = bin.read(cx);
            assert_eq!(bin.running, 0);
            assert!(bin.waiting.is_empty() && bin.probing.is_empty());
            assert_eq!(bin.failures.len(), 10);
        });
    }

    #[gpui::test]
    fn a_dropped_folder_imports_the_media_files_inside_it(cx: &mut TestAppContext) {
        let folder = scratch_dir("folder");
        fs::create_dir_all(folder.join("nested")).unwrap();
        for file in ["a.mkv", "notes.txt", "nested/b.WAV", "nested/cover.jpg"] {
            fs::write(folder.join(file), "").unwrap();
        }
        let project = cx.new(|_| Project::new("test"));
        let (bin, cx) =
            cx.add_window_view(|_, cx| MediaBin::new(ProjectEditor::new(project, cx), cx));

        bin.update(cx, |bin, cx| bin.import([folder.clone()], cx));
        cx.run_until_parked();

        cx.read(|cx| {
            let bin = bin.read(cx);
            assert_eq!(failure_sources(bin), ["a.mkv", "b.WAV"]);
            assert!(bin.probing.is_empty());
        });
        fs::remove_dir_all(&folder).ok();
    }

    #[gpui::test]
    fn undoing_an_import_releases_its_thumbnail(cx: &mut TestAppContext) {
        let project = cx.new(|_| Project::new("test"));
        let (bin, cx) =
            cx.add_window_view(|_, cx| MediaBin::new(ProjectEditor::new(project, cx), cx));

        let id = bin.update(cx, |bin, cx| {
            let id = bin.editor.perform(Command::ImportMedia, cx, |project| {
                project.add_asset("/missing/a.mkv".into(), video_info())
            });
            bin.thumbnails.insert(id, blank_thumbnail());
            id
        });
        cx.run_until_parked();

        cx.read(|cx| assert!(bin.read(cx).thumbnail_requested.contains_key(&id)));

        bin.update(cx, |bin, cx| bin.editor.undo(cx));
        cx.run_until_parked();

        cx.read(|cx| {
            let bin = bin.read(cx);
            assert!(!bin.thumbnails.contains_key(&id));
            assert!(!bin.thumbnail_requested.contains_key(&id));
        });
    }

    #[gpui::test]
    fn failures_collapse_into_one_row_that_dismisses_them_all(cx: &mut TestAppContext) {
        let project = cx.new(|_| Project::new("test"));
        let (bin, cx) =
            cx.add_window_view(|_, cx| MediaBin::new(ProjectEditor::new(project, cx), cx));

        bin.update(cx, |bin, cx| {
            for name in ["a", "b", "c", "d", "e"] {
                bin.fail(format!("{name}.mkv").into(), "no streams".into(), cx);
            }
        });

        cx.read(|cx| assert_eq!(bin.read(cx).failures.len(), 5));

        bin.update(cx, |bin, cx| bin.dismiss_failures(cx));

        cx.read(|cx| assert!(bin.read(cx).failures.is_empty()));
    }

    #[gpui::test]
    fn thumbnails_land_only_on_the_asset_they_were_decoded_for(cx: &mut TestAppContext) {
        let mut project = Project::new("test");
        let asset = project.add_asset("/missing/a.mkv".into(), video_info());
        let project = cx.new(|_| project);
        let (bin, cx) =
            cx.add_window_view(|_, cx| MediaBin::new(ProjectEditor::new(project, cx), cx));
        bin.update(cx, |bin, cx| {
            bin.finish_thumbnail(
                asset,
                Path::new("/missing/b.mkv"),
                Some(blank_thumbnail()),
                cx,
            );
            bin.finish_thumbnail(
                AssetId(9),
                Path::new("/missing/a.mkv"),
                Some(blank_thumbnail()),
                cx,
            );
        });
        assert!(cx.read(|cx| bin.read(cx).thumbnails.is_empty()));
        bin.update(cx, |bin, cx| {
            bin.finish_thumbnail(
                asset,
                Path::new("/missing/a.mkv"),
                Some(blank_thumbnail()),
                cx,
            );
        });
        assert!(cx.read(|cx| bin.read(cx).thumbnails.contains_key(&asset)));
    }

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

    #[test]
    fn streams_are_described_in_a_line() {
        assert_eq!(describe(&video_info()), "640×360 h264");
        assert_eq!(describe(&audio_info()), "opus 48 kHz stereo");
        assert_eq!(describe(&MediaInfo::default()), "no streams");

        let mut both = video_info();
        both.streams.extend(audio_info().streams);
        assert_eq!(describe(&both), "640×360 h264 · opus 48 kHz stereo");
    }

    #[test]
    fn rates_drop_trailing_zeros() {
        assert_eq!(trimmed_rate(29.97002997), "29.97");
        assert_eq!(trimmed_rate(30.0), "30");
        assert_eq!(trimmed_rate(44.1), "44.1");
        assert_eq!(channel_label(6), "6 ch");
    }

    #[gpui::test]
    fn removing_an_asset_in_use_is_refused_and_an_unused_one_goes(cx: &mut TestAppContext) {
        let mut project = Project::new("test");
        let used = project.add_asset("/missing/used.mkv".into(), video_info());
        let spare = project.add_asset("/missing/spare.mkv".into(), video_info());
        project.place_clip(used, 0, Time::ZERO).unwrap();
        let project = cx.new(|_| project);
        let (bin, cx) =
            cx.add_window_view(|_, cx| MediaBin::new(ProjectEditor::new(project, cx), cx));

        bin.update(cx, |bin, cx| bin.remove_asset(used, cx));

        cx.read(|cx| {
            let bin = bin.read(cx);
            assert_eq!(bin.failures.len(), 1);
            assert!(bin.project.read(cx).asset(used).is_some());
        });

        bin.update(cx, |bin, cx| bin.remove_asset(spare, cx));

        cx.read(|cx| assert!(bin.read(cx).project.read(cx).asset(spare).is_none()));
    }

    #[gpui::test]
    fn pruning_removes_every_unused_asset_and_can_be_undone(cx: &mut TestAppContext) {
        let mut project = Project::new("test");
        let used = project.add_asset("/missing/used.mkv".into(), video_info());
        project.add_asset("/missing/spare.mkv".into(), video_info());
        project.add_asset("/missing/other.opus".into(), audio_info());
        project.place_clip(used, 0, Time::ZERO).unwrap();
        let project = cx.new(|_| project);
        let (bin, cx) =
            cx.add_window_view(|_, cx| MediaBin::new(ProjectEditor::new(project, cx), cx));

        bin.update(cx, |bin, cx| bin.prune_assets(cx));

        cx.read(|cx| assert_eq!(bin.read(cx).project.read(cx).assets.len(), 1));

        bin.update(cx, |bin, cx| bin.editor.undo(cx));

        cx.read(|cx| assert_eq!(bin.read(cx).project.read(cx).assets.len(), 3));
    }

    #[gpui::test]
    fn media_that_is_not_on_disk_is_marked_missing(cx: &mut TestAppContext) {
        let dir = scratch_dir("presence");
        let present = dir.join("here.mkv");
        fs::write(&present, "").unwrap();
        let mut project = Project::new("test");
        let here = project.add_asset(present.clone(), audio_info());
        let gone = project.add_asset(dir.join("gone.mkv"), audio_info());
        let project = cx.new(|_| project);
        let (bin, cx) =
            cx.add_window_view(|_, cx| MediaBin::new(ProjectEditor::new(project, cx), cx));

        cx.run_until_parked();

        cx.read(|cx| {
            let bin = bin.read(cx);
            assert_eq!(bin.presence[&here].present, Some(true));
            assert_eq!(bin.presence[&gone].present, Some(false));
        });
        fs::remove_dir_all(&dir).ok();
    }

    #[gpui::test]
    fn a_relinked_asset_is_checked_and_decoded_again(cx: &mut TestAppContext) {
        let mut project = Project::new("test");
        let asset = project.add_asset("/missing/old.mkv".into(), video_info());
        let project = cx.new(|_| project);
        let (bin, cx) =
            cx.add_window_view(|_, cx| MediaBin::new(ProjectEditor::new(project, cx), cx));
        cx.run_until_parked();

        bin.update(cx, |bin, cx| {
            bin.thumbnails.insert(asset, blank_thumbnail());
            bin.finish_relink(asset, "/missing/new.mkv".into(), Ok(video_info()), cx);
        });
        cx.run_until_parked();

        cx.read(|cx| {
            let bin = bin.read(cx);
            let path = std::path::PathBuf::from("/missing/new.mkv");
            assert_eq!(bin.project.read(cx).asset(asset).unwrap().path, path);
            assert_eq!(bin.presence[&asset].path, path);
            assert_eq!(bin.thumbnail_requested[&asset], path);
            assert!(!bin.thumbnails.contains_key(&asset));
        });
    }

    #[gpui::test]
    fn media_that_cannot_stand_in_for_the_old_is_refused_on_relink(cx: &mut TestAppContext) {
        let mut project = Project::new("test");
        let asset = project.add_asset("/missing/old.mkv".into(), video_info());
        project.place_clip(asset, 0, Time::ZERO).unwrap();
        let project = cx.new(|_| project);
        let (bin, cx) =
            cx.add_window_view(|_, cx| MediaBin::new(ProjectEditor::new(project, cx), cx));

        bin.update(cx, |bin, cx| {
            bin.finish_relink(asset, "/missing/sound.opus".into(), Ok(audio_info()), cx);
        });

        cx.read(|cx| {
            let bin = bin.read(cx);
            assert_eq!(bin.failures.len(), 1);
            assert_eq!(
                bin.project.read(cx).asset(asset).unwrap().path,
                std::path::PathBuf::from("/missing/old.mkv")
            );
        });
    }
}
