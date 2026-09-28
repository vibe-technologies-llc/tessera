use std::{
    collections::{HashMap, hash_map::Entry},
    mem,
    path::PathBuf,
    sync::Arc,
};

use gpui::{
    AnyElement, AppContext, Context, Entity, IntoElement, ObjectFit, ParentElement, Render,
    RenderImage, SharedString, Styled, StyledImage, Window, div, img,
};
use tessera_media::{VideoDecoder, VideoFrame};
use tessera_render::{Compositor, Frame, Layer};
use tessera_timeline::{Project, Time, Timecode};

use crate::{
    frame_image::render_image,
    playhead::{Playhead, Speed},
    theme,
};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct DecoderKey {
    path: PathBuf,
    bounds: (u32, u32),
}

type Decoders = HashMap<DecoderKey, VideoDecoder>;

#[derive(Clone, Debug, PartialEq, Eq)]
struct LayerRequest {
    decoder: DecoderKey,
    time: Time,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct FrameRequest {
    layers: Vec<LayerRequest>,
    sequence: (u32, u32),
}

impl FrameRequest {
    fn decoders(&self) -> impl Iterator<Item = &DecoderKey> {
        self.layers.iter().map(|layer| &layer.decoder)
    }
}

enum CompositorSlot {
    Uncreated,
    Ready(Box<Compositor>),
    Unavailable,
}

impl CompositorSlot {
    fn compositor(&mut self) -> Option<&mut Compositor> {
        if let Self::Uncreated = self {
            *self = match Compositor::new() {
                Ok(compositor) => {
                    let adapter = compositor.adapter_info().name;
                    tracing::info!(%adapter, "viewer compositor ready");
                    Self::Ready(Box::new(compositor))
                }
                Err(error) => {
                    tracing::warn!(%error, "no compositor, the viewer shows only the top video layer");
                    Self::Unavailable
                }
            };
        }
        match self {
            Self::Ready(compositor) => Some(compositor),
            Self::Uncreated | Self::Unavailable => None,
        }
    }
}

struct Renderer {
    decoders: Decoders,
    compositor: CompositorSlot,
}

impl Renderer {
    fn new() -> Self {
        Self {
            decoders: Decoders::new(),
            compositor: CompositorSlot::Uncreated,
        }
    }

    fn render(&mut self, request: &FrameRequest) -> Picture {
        let image = decode_layers(&mut self.decoders, &request.layers).and_then(|frames| {
            assemble_image(self.compositor.compositor(), request.sequence, frames)
        });
        match image {
            Ok(image) => Picture::Frame(image),
            Err(reason) => Picture::Failed(reason),
        }
    }
}

enum Picture {
    Empty,
    Frame(Arc<RenderImage>),
    Failed(SharedString),
}

pub struct Viewer {
    project: Entity<Project>,
    playhead: Entity<Playhead>,
    idle_renderer: Option<Renderer>,
    wanted: Option<FrameRequest>,
    shown: Option<FrameRequest>,
    picture: Picture,
    retired: Vec<Arc<RenderImage>>,
}

impl Viewer {
    pub fn new(
        project: Entity<Project>,
        playhead: Entity<Playhead>,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&project, |viewer, _, cx| {
            viewer.refresh(cx);
            cx.notify();
        })
        .detach();
        cx.observe(&playhead, |viewer, _, cx| {
            viewer.refresh(cx);
            cx.notify();
        })
        .detach();
        let mut viewer = Self {
            project,
            playhead,
            idle_renderer: Some(Renderer::new()),
            wanted: None,
            shown: None,
            picture: Picture::Empty,
            retired: Vec::new(),
        };
        viewer.refresh(cx);
        viewer
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let stale = self
            .shown
            .as_ref()
            .is_some_and(|shown| !self.is_live_request(shown, cx));
        if stale {
            self.show(None, Picture::Empty, cx);
        }
        let wanted = frame_request(self.project.read(cx), self.playhead.read(cx).time());
        if stale || wanted != self.wanted {
            self.wanted = wanted;
            self.render_wanted(cx);
        }
    }

    fn is_live(&self, decoder: &DecoderKey, cx: &Context<Self>) -> bool {
        let project = self.project.read(cx);
        decoder.bounds == sequence_bounds(project)
            && project
                .assets
                .iter()
                .any(|asset| asset.path == decoder.path)
    }

    fn is_live_request(&self, request: &FrameRequest, cx: &Context<Self>) -> bool {
        request.decoders().all(|decoder| self.is_live(decoder, cx))
    }

    fn render_wanted(&mut self, cx: &mut Context<Self>) {
        if self.wanted == self.shown {
            return;
        }
        let Some(request) = self.wanted.clone() else {
            self.show(None, Picture::Empty, cx);
            return;
        };
        let Some(mut renderer) = self.idle_renderer.take() else {
            return;
        };
        renderer.decoders.retain(|key, _| self.is_live(key, cx));
        let rendered = cx.background_spawn({
            let request = request.clone();
            async move {
                let picture = renderer.render(&request);
                (renderer, picture)
            }
        });
        cx.spawn(async move |this, cx| {
            let (renderer, picture) = rendered.await;
            this.update(cx, |viewer, cx| {
                viewer.idle_renderer = Some(renderer);
                if viewer.is_live_request(&request, cx) {
                    viewer.show(Some(request), picture, cx);
                }
                viewer.render_wanted(cx);
            })
            .ok();
        })
        .detach();
    }

    fn show(&mut self, shown: Option<FrameRequest>, picture: Picture, cx: &mut Context<Self>) {
        if let Picture::Frame(replaced) = mem::replace(&mut self.picture, picture) {
            self.retired.push(replaced);
        }
        self.shown = shown;
        cx.notify();
    }

    fn picture(&self, placeholder: String) -> AnyElement {
        match &self.picture {
            Picture::Empty => div().child(placeholder).into_any_element(),
            Picture::Frame(image) => img(image.clone())
                .size_full()
                .object_fit(ObjectFit::Contain)
                .into_any_element(),
            Picture::Failed(reason) => div()
                .px_4()
                .text_color(theme::error())
                .child(reason.clone())
                .into_any_element(),
        }
    }
}

impl Render for Viewer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        for image in self.retired.drain(..) {
            if let Err(error) = window.drop_image(image) {
                tracing::warn!(%error, "failed to release a viewer frame");
            }
        }
        let settings = self.project.read(cx).settings;
        let playhead = self.playhead.read(cx);
        let timecode = Timecode::new(playhead.time(), settings.frame_rate);
        let speed = speed_label(playhead.speed());
        let aspect_ratio = settings.width as f32 / settings.height as f32;
        let placeholder = format!(
            "{}×{} · {:.3} fps",
            settings.width,
            settings.height,
            settings.frame_rate.as_f64()
        );
        let mut frame = div()
            .max_w_full()
            .max_h_full()
            .h_full()
            .flex()
            .items_center()
            .justify_center()
            .overflow_hidden()
            .bg(theme::frame())
            .text_color(theme::text_muted())
            .child(self.picture(placeholder));
        frame.style().aspect_ratio = Some(aspect_ratio);
        div()
            .size_full()
            .p_4()
            .gap_2()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(frame),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .justify_center()
                    .gap_3()
                    .child(timecode.to_string())
                    .children(speed.map(|speed| div().text_color(theme::playhead()).child(speed))),
            )
    }
}

fn sequence_bounds(project: &Project) -> (u32, u32) {
    (project.settings.width, project.settings.height)
}

fn frame_request(project: &Project, time: Time) -> Option<FrameRequest> {
    let sequence = sequence_bounds(project);
    let layers: Vec<LayerRequest> = project
        .timeline
        .video_clips_at(time)
        .filter_map(|clip| {
            let asset = project.asset(clip.asset)?;
            Some(LayerRequest {
                decoder: DecoderKey {
                    path: asset.path.clone(),
                    bounds: sequence,
                },
                time: clip.source_time_at(time)?,
            })
        })
        .collect();
    (!layers.is_empty()).then_some(FrameRequest { layers, sequence })
}

fn speed_label(speed: Speed) -> Option<String> {
    let factor = speed.factor();
    match factor {
        0 => None,
        1.. => Some(format!("▶ {factor}×")),
        _ => Some(format!("◀ {}×", factor.unsigned_abs())),
    }
}

fn decode_layers(
    decoders: &mut Decoders,
    layers: &[LayerRequest],
) -> Result<Vec<Arc<VideoFrame>>, SharedString> {
    layers
        .iter()
        .map(|layer| {
            decode_layer(decoders, layer).map_err(|error| {
                tracing::warn!(path = %layer.decoder.path.display(), %error, "viewer decode failed");
                SharedString::from(error.to_string())
            })
        })
        .collect()
}

fn decode_layer(
    decoders: &mut Decoders,
    layer: &LayerRequest,
) -> Result<Arc<VideoFrame>, tessera_media::Error> {
    let (decoder, newly_opened) = match decoders.entry(layer.decoder.clone()) {
        Entry::Occupied(entry) => (entry.into_mut(), false),
        Entry::Vacant(entry) => {
            let DecoderKey { path, bounds } = entry.key();
            let (width, height) = *bounds;
            let opened = VideoDecoder::open(path)?.fit_within(width, height);
            (entry.insert(opened), true)
        }
    };
    let frame = decoder.frame_at(layer.time)?;
    if newly_opened {
        tracing::debug!(
            path = %layer.decoder.path.display(),
            hw_accel = ?decoder.hw_accel(),
            "viewer decoder opened"
        );
    }
    Ok(frame)
}

fn assemble_image(
    compositor: Option<&mut Compositor>,
    sequence: (u32, u32),
    mut frames: Vec<Arc<VideoFrame>>,
) -> Result<Arc<RenderImage>, SharedString> {
    let image = match compositor {
        Some(compositor) => {
            let composited = composite_frames(compositor, sequence, &frames).map_err(|error| {
                tracing::warn!(%error, "viewer compositing failed");
                SharedString::from(error.to_string())
            })?;
            render_image(composited)
        }
        None => {
            let top = frames.pop().ok_or("No video clip is under the playhead")?;
            render_image(top)
        }
    };
    image.ok_or_else(|| "The decoded frame has an unexpected size".into())
}

fn composite_frames(
    compositor: &mut Compositor,
    (width, height): (u32, u32),
    frames: &[Arc<VideoFrame>],
) -> Result<Frame, tessera_render::Error> {
    let layers: Vec<Layer<'_>> = frames
        .iter()
        .map(|frame| Layer {
            width: frame.width,
            height: frame.height,
            bgra: &frame.bgra,
        })
        .collect();
    compositor.composite(width, height, &layers)
}

#[cfg(test)]
mod tests {
    use gpui::{TestAppContext, VisualTestContext};
    use tessera_timeline::{AudioStream, MediaInfo, Stream, TrackKind, VideoStream};

    use super::*;
    use crate::editor::ProjectEditor;

    const RED: [u8; 4] = [0, 0, 255, 255];
    const BLUE: [u8; 4] = [255, 0, 0, 255];
    const OPAQUE_BLACK: [u8; 4] = [0, 0, 0, 255];

    fn video_info(seconds: i64) -> MediaInfo {
        MediaInfo {
            duration: Some(Time::from_seconds(seconds)),
            streams: vec![Stream::Video(VideoStream {
                index: 0,
                codec: "h264".into(),
                width: 640,
                height: 360,
                frame_rate: None,
            })],
        }
    }

    fn audio_info(seconds: i64) -> MediaInfo {
        MediaInfo {
            duration: Some(Time::from_seconds(seconds)),
            streams: vec![Stream::Audio(AudioStream {
                index: 0,
                codec: "flac".into(),
                sample_rate: 48_000,
                channels: 2,
            })],
        }
    }

    fn project_showing(media: &str) -> Project {
        let mut project = Project::new(media);
        let asset = project.add_asset(media.into(), video_info(4));
        project.place_clip(asset, 0, Time::ZERO).unwrap();
        project
    }

    fn stacked_project() -> Project {
        let mut project = Project::new("stacked");
        let middle = project.timeline.add_track(TrackKind::Video);
        let top = project.timeline.add_track(TrackKind::Video);
        let music = project.timeline.add_track(TrackKind::Audio);
        let placements = [
            ("/missing/bottom.mkv", video_info(8), 0, 0),
            ("/missing/middle.mkv", video_info(2), middle, 3),
            ("/missing/top.mkv", video_info(4), top, 1),
            ("/missing/music.flac", audio_info(8), music, 0),
        ];
        for (path, info, track, start) in placements {
            let asset = project.add_asset(path.into(), info);
            project
                .place_clip(asset, track, Time::from_seconds(start))
                .unwrap();
        }
        project
    }

    fn layer_paths(request: Option<&FrameRequest>) -> Vec<PathBuf> {
        request
            .into_iter()
            .flat_map(FrameRequest::decoders)
            .map(|decoder| decoder.path.clone())
            .collect()
    }

    fn wanted_paths(viewer: &Entity<Viewer>, cx: &mut VisualTestContext) -> Vec<PathBuf> {
        cx.read(|cx| layer_paths(viewer.read(cx).wanted.as_ref()))
    }

    fn solid(width: u32, height: u32, bgra: [u8; 4]) -> Arc<VideoFrame> {
        Arc::new(VideoFrame {
            width,
            height,
            time: Time::ZERO,
            bgra: bgra.repeat(width as usize * height as usize),
        })
    }

    fn pixel(frame: &Frame, x: u32, y: u32) -> [u8; 4] {
        let start = ((y * frame.width + x) * 4) as usize;
        frame.bgra[start..start + 4].try_into().unwrap()
    }

    #[test]
    fn the_renderer_can_move_to_a_background_task() {
        fn assert_send<T: Send>() {}
        assert_send::<Compositor>();
        assert_send::<Renderer>();
    }

    #[test]
    fn the_request_stacks_video_clips_from_the_bottom_track_up() {
        let project = stacked_project();
        let paths =
            |seconds| layer_paths(frame_request(&project, Time::from_seconds(seconds)).as_ref());
        assert_eq!(
            paths(3),
            [
                PathBuf::from("/missing/bottom.mkv"),
                "/missing/middle.mkv".into(),
                "/missing/top.mkv".into()
            ]
        );
        assert_eq!(
            paths(2),
            [
                PathBuf::from("/missing/bottom.mkv"),
                "/missing/top.mkv".into()
            ]
        );
        assert_eq!(paths(0), [PathBuf::from("/missing/bottom.mkv")]);
        assert!(frame_request(&project, Time::from_seconds(9)).is_none());

        let request = frame_request(&project, Time::from_seconds(4)).unwrap();
        assert_eq!(request.sequence, sequence_bounds(&project));
        let times: Vec<Time> = request.layers.iter().map(|layer| layer.time).collect();
        assert_eq!(
            times,
            [
                Time::from_seconds(4),
                Time::from_seconds(1),
                Time::from_seconds(3)
            ]
        );
    }

    #[gpui::test]
    fn the_viewer_asks_for_every_video_layer_under_the_playhead(cx: &mut TestAppContext) {
        let project = cx.new(|_| stacked_project());
        let playhead = cx.new(|_| Playhead::new(project.clone()));
        let (viewer, cx) =
            cx.add_window_view(|_, cx| Viewer::new(project.clone(), playhead.clone(), cx));
        cx.run_until_parked();
        assert_eq!(
            wanted_paths(&viewer, cx),
            [PathBuf::from("/missing/bottom.mkv")]
        );

        cx.update(|_, cx| {
            playhead.update(cx, |playhead, cx| playhead.seek(Time::from_seconds(3), cx))
        });
        let wanted = wanted_paths(&viewer, cx);
        assert_eq!(
            wanted,
            [
                PathBuf::from("/missing/bottom.mkv"),
                "/missing/middle.mkv".into(),
                "/missing/top.mkv".into()
            ]
        );
        cx.run_until_parked();
        let shown = cx.read(|cx| layer_paths(viewer.read(cx).shown.as_ref()));
        assert_eq!(shown, wanted);

        cx.update(|_, cx| {
            playhead.update(cx, |playhead, cx| playhead.seek(Time::from_seconds(9), cx))
        });
        cx.run_until_parked();
        assert!(cx.read(|cx| viewer.read(cx).wanted.is_none()));
        assert!(cx.read(|cx| viewer.read(cx).shown.is_none()));
        assert!(cx.read(|cx| matches!(viewer.read(cx).picture, Picture::Empty)));
    }

    #[gpui::test]
    fn a_replaced_project_asks_for_frames_of_its_own_media(cx: &mut TestAppContext) {
        let project = cx.new(|_| project_showing("/missing/first.mkv"));
        let (viewer, cx) = cx.add_window_view(|_, cx| {
            let playhead = cx.new(|_| Playhead::new(project.clone()));
            Viewer::new(project.clone(), playhead, cx)
        });
        cx.run_until_parked();
        assert_eq!(
            wanted_paths(&viewer, cx),
            [PathBuf::from("/missing/first.mkv")]
        );

        let second = project_showing("/missing/second.mkv");
        assert_eq!(
            second.assets[0].id,
            project.read_with(cx, |project, _| project.assets[0].id)
        );
        cx.update(|_, cx| ProjectEditor::new(project.clone(), cx).replace(second, cx));
        assert!(cx.read(|cx| viewer.read(cx).shown.is_none()));
        assert_eq!(
            wanted_paths(&viewer, cx),
            [PathBuf::from("/missing/second.mkv")]
        );
        cx.run_until_parked();
        let shown = cx.read(|cx| layer_paths(viewer.read(cx).shown.as_ref()));
        assert_eq!(shown, [PathBuf::from("/missing/second.mkv")]);
    }

    #[test]
    fn frames_composite_bottom_first_into_the_sequence() {
        let mut compositor = Compositor::new().expect("a Vulkan adapter");
        let frames = [solid(8, 4, RED), solid(4, 4, BLUE)];
        let frame = composite_frames(&mut compositor, (8, 4), &frames).unwrap();
        assert_eq!((frame.width, frame.height), (8, 4));
        assert_eq!(pixel(&frame, 4, 2), BLUE);
        assert_eq!(pixel(&frame, 0, 2), RED);
        assert_eq!(pixel(&frame, 7, 2), RED);

        let frame = composite_frames(&mut compositor, (8, 4), &frames[1..]).unwrap();
        assert_eq!(pixel(&frame, 4, 2), BLUE);
        assert_eq!(pixel(&frame, 0, 2), OPAQUE_BLACK);
    }

    #[test]
    fn the_assembled_image_comes_at_sequence_size() {
        let mut compositor = Compositor::new().expect("a Vulkan adapter");
        let frames = vec![solid(8, 4, RED), solid(4, 4, BLUE)];
        let image = assemble_image(Some(&mut compositor), (8, 4), frames).unwrap();
        assert_eq!(image.size(0), gpui::size(8.into(), 4.into()));
        assert_eq!(image.as_bytes(0).unwrap()[..4], RED);
    }

    #[test]
    fn without_a_compositor_the_top_frame_is_shown_as_decoded() {
        let frames = vec![solid(8, 4, RED), solid(4, 4, BLUE)];
        let image = assemble_image(None, (8, 4), frames).unwrap();
        assert_eq!(image.size(0), gpui::size(4.into(), 4.into()));
        assert_eq!(image.as_bytes(0).unwrap()[..4], BLUE);
    }
}
