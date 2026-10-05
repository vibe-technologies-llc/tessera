use std::{
    collections::{HashMap, HashSet, VecDeque},
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use gpui::{
    AnyElement, AppContext, Context, Entity, ExternalPaths, FontWeight, InteractiveElement,
    IntoElement, ObjectFit, ParentElement, PathPromptOptions, Render, RenderImage, SharedString,
    StatefulInteractiveElement, Styled, StyledImage, Window, div, img, px,
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
            Self::Thumbnail { id, path, duration } => Finished::Thumbnail {
                image: thumbnail(&path, duration),
                id,
                path,
            },
        }
    }
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
    thumbnail_requested: HashSet<AssetId>,
    retired: Vec<Arc<RenderImage>>,
    waiting: VecDeque<Job>,
    running: usize,
    generation: u64,
}

impl MediaBin {
    pub fn new(editor: ProjectEditor, cx: &mut Context<Self>) -> Self {
        let project = editor.project().clone();
        cx.observe(&project, |bin, _, cx| {
            bin.sync_thumbnails(cx);
            cx.notify();
        })
        .detach();
        let mut bin = Self {
            editor,
            project,
            probing: Vec::new(),
            failures: Vec::new(),
            thumbnails: HashMap::new(),
            thumbnail_requested: HashSet::new(),
            retired: Vec::new(),
            waiting: VecDeque::new(),
            running: 0,
            generation: 0,
        };
        bin.sync_thumbnails(cx);
        bin
    }

    pub fn project_replaced(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        self.running = 0;
        self.waiting.clear();
        self.probing.clear();
        self.failures.clear();
        self.thumbnail_requested.clear();
        self.retired
            .extend(self.thumbnails.drain().map(|(_, thumbnail)| thumbnail));
        self.sync_thumbnails(cx);
        cx.notify();
    }

    fn sync_thumbnails(&mut self, cx: &mut Context<Self>) {
        let project = self.project.read(cx);
        let removed: Vec<AssetId> = self
            .thumbnail_requested
            .iter()
            .copied()
            .filter(|id| project.asset(*id).is_none())
            .collect();
        let wanted: Vec<Job> = project
            .assets
            .iter()
            .filter(|asset| asset.info.video().next().is_some())
            .filter(|asset| !self.thumbnail_requested.contains(&asset.id))
            .map(|asset| Job::Thumbnail {
                id: asset.id,
                path: asset.path.clone(),
                duration: asset.info.duration,
            })
            .collect();
        for id in removed {
            self.thumbnail_requested.remove(&id);
            self.retired.extend(self.thumbnails.remove(&id));
        }
        for job in wanted {
            if let Job::Thumbnail { id, .. } = &job {
                self.thumbnail_requested.insert(*id);
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

        cx.read(|cx| assert!(bin.read(cx).thumbnail_requested.contains(&id)));

        bin.update(cx, |bin, cx| bin.editor.undo(cx));
        cx.run_until_parked();

        cx.read(|cx| {
            let bin = bin.read(cx);
            assert!(!bin.thumbnails.contains_key(&id));
            assert!(!bin.thumbnail_requested.contains(&id));
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
}
