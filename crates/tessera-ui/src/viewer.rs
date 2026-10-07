use std::{
    collections::HashMap,
    mem,
    panic::{self, AssertUnwindSafe},
    path::PathBuf,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use gpui::{
    AnyElement, AnyWindowHandle, AppContext, Context, Entity, IntoElement, ObjectFit,
    ParentElement, Pixels, Render, RenderImage, SharedString, Size, Styled, StyledImage, Window,
    canvas, div, img, relative,
};
use tessera_media::{VideoDecoder, VideoFrame};
use tessera_render::{Compositor, Frame, Layer};
use tessera_timeline::{FrameRate, Project, Time, Timecode};

use crate::{
    frame_image::render_image,
    playhead::{Playhead, Speed, last_frame},
    theme,
};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct DecoderKey {
    path: PathBuf,
    stream: usize,
    track: usize,
    bounds: (u32, u32),
}

type Decoders = HashMap<DecoderKey, VideoDecoder>;
type FailedMedia = HashMap<DecoderKey, SharedString>;

const LATENCY_SMOOTHING: u32 = 4;
const SIZE_STEPS: f32 = 8.;
const SAFE_AREAS: [f32; 2] = [0.9, 0.8];
const COMPOSITOR_STRIKES: u32 = 3;
const RENDERER_PANICKED: &str = "The renderer stopped unexpectedly";
const NO_VIDEO: &str = "No video clip is under the playhead";

#[derive(Clone, Debug, PartialEq, Eq)]
struct LayerRequest {
    decoder: DecoderKey,
    time: Time,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct FrameRequest {
    layers: Vec<LayerRequest>,
    bounds: (u32, u32),
}

impl FrameRequest {
    fn decoders(&self) -> impl Iterator<Item = &DecoderKey> {
        self.layers.iter().map(|layer| &layer.decoder)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Target {
    time: Time,
    speed: Speed,
    request: Option<FrameRequest>,
}

struct Rendered {
    target: Target,
    picture: Picture,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fate {
    Show,
    Hold,
    Drop,
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
    failed: FailedMedia,
    compositor: CompositorSlot,
    compositor_strikes: u32,
}

impl Renderer {
    fn new() -> Self {
        Self {
            decoders: Decoders::new(),
            failed: FailedMedia::new(),
            compositor: CompositorSlot::Uncreated,
            compositor_strikes: 0,
        }
    }

    fn retain(&mut self, mut live: impl FnMut(&DecoderKey) -> bool) {
        self.decoders.retain(|key, _| live(key));
        self.failed.retain(|key, _| live(key));
    }

    fn render(&mut self, request: &FrameRequest) -> Rendering {
        let decoded = decode_layers(&mut self.decoders, &mut self.failed, &request.layers);
        let picture = self.assemble(request.bounds, decoded.frames, decoded.first_failure);
        Rendering {
            picture,
            decoded_anew: decoded.decoded_anew,
        }
    }

    fn assemble(
        &mut self,
        bounds: (u32, u32),
        frames: Vec<Arc<VideoFrame>>,
        first_failure: Option<SharedString>,
    ) -> Picture {
        if frames.is_empty() {
            let reason = first_failure.unwrap_or_else(|| NO_VIDEO.into());
            return Picture::Failed(reason);
        }
        let assembled = assemble_image(self.compositor.compositor(), bounds, frames);
        if assembled.compositing_failed {
            self.compositor_strikes += 1;
            self.compositor = if self.compositor_strikes >= COMPOSITOR_STRIKES {
                CompositorSlot::Unavailable
            } else {
                CompositorSlot::Uncreated
            };
        } else {
            self.compositor_strikes = 0;
        }
        match assembled.image {
            Ok(image) => Picture::Frame(image),
            Err(reason) => Picture::Failed(reason),
        }
    }
}

fn render_guarded<R>(
    mut renderer: R,
    render: impl FnOnce(&mut R) -> Rendering,
    replacement: impl FnOnce() -> R,
) -> (R, Rendering) {
    match panic::catch_unwind(AssertUnwindSafe(|| render(&mut renderer))) {
        Ok(rendering) => (renderer, rendering),
        Err(_) => {
            tracing::error!("the viewer renderer panicked, starting a new one");
            let panicked = Rendering {
                picture: Picture::Failed(RENDERER_PANICKED.into()),
                decoded_anew: true,
            };
            (replacement(), panicked)
        }
    }
}

struct Rendering {
    picture: Picture,
    decoded_anew: bool,
}

enum Picture {
    Empty,
    Black,
    Frame(Arc<RenderImage>),
    Failed(SharedString),
}

pub struct Viewer {
    project: Entity<Project>,
    playhead: Entity<Playhead>,
    idle_renderer: Option<Renderer>,
    latency: Duration,
    view: Option<(f32, f32)>,
    safe_areas: bool,
    bounds: (u32, u32),
    wanted: Target,
    pending: Option<Rendered>,
    shown: Option<FrameRequest>,
    picture: Picture,
    window: AnyWindowHandle,
}

impl Viewer {
    pub fn new(
        project: Entity<Project>,
        playhead: Entity<Playhead>,
        window: &Window,
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
        let bounds = sequence_bounds(project.read(cx));
        let mut viewer = Self {
            project,
            playhead,
            idle_renderer: Some(Renderer::new()),
            latency: Duration::ZERO,
            view: None,
            safe_areas: false,
            bounds,
            wanted: Target::default(),
            pending: None,
            shown: None,
            picture: Picture::Empty,
            window: window.window_handle(),
        };
        viewer.refresh(cx);
        viewer
    }

    pub fn toggle_safe_areas(&mut self, cx: &mut Context<Self>) {
        self.safe_areas = !self.safe_areas;
        cx.notify();
    }

    #[cfg(test)]
    pub fn safe_areas(&self) -> bool {
        self.safe_areas
    }

    fn set_view(&mut self, size: Size<Pixels>, scale: f32, cx: &mut Context<Self>) {
        let view = (
            f32::from(size.width) * scale,
            f32::from(size.height) * scale,
        );
        if self.view != Some(view) {
            self.view = Some(view);
            self.refresh(cx);
        }
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.bounds = render_bounds(sequence_bounds(self.project.read(cx)), self.view);
        let stale = self
            .shown
            .as_ref()
            .is_some_and(|shown| !self.is_live_request(shown, cx));
        if stale {
            self.show(None, Picture::Black, cx);
        }
        if let Some(pending) = self.pending.take()
            && pending
                .target
                .request
                .as_ref()
                .is_none_or(|request| self.is_live_request(request, cx))
        {
            self.settle(pending, cx);
        }
        let project = self.project.read(cx);
        let playhead = self.playhead.read(cx);
        let speed = playhead.speed();
        let time = presentation_time(
            playhead.time(),
            speed,
            self.latency,
            project.settings.frame_rate,
            last_frame(project.timeline.duration(), project.settings.frame_rate),
        );
        self.wanted = Target {
            time,
            speed,
            request: frame_request(project, time, self.bounds),
        };
        self.render_wanted(cx);
    }

    fn is_live(&self, decoder: &DecoderKey, cx: &Context<Self>) -> bool {
        let project = self.project.read(cx);
        decoder.bounds == self.bounds
            && project
                .timeline
                .tracks
                .get(decoder.track)
                .is_some_and(|track| {
                    track.clips().iter().any(|clip| {
                        project.asset(clip.asset).is_some_and(|asset| {
                            asset.path == decoder.path
                                && asset
                                    .info
                                    .video()
                                    .any(|video| video.index == decoder.stream)
                        })
                    })
                })
    }

    fn is_live_request(&self, request: &FrameRequest, cx: &Context<Self>) -> bool {
        request.decoders().all(|decoder| self.is_live(decoder, cx))
    }

    fn render_wanted(&mut self, cx: &mut Context<Self>) {
        let latest = self
            .pending
            .as_ref()
            .map_or(&self.shown, |pending| &pending.target.request);
        if *latest == self.wanted.request {
            return;
        }
        let target = self.wanted.clone();
        let Some(request) = target.request.clone() else {
            self.pending = None;
            self.settle(
                Rendered {
                    target,
                    picture: Picture::Black,
                },
                cx,
            );
            return;
        };
        let Some(mut renderer) = self.idle_renderer.take() else {
            return;
        };
        renderer.retain(|key| self.is_live(key, cx));
        let started = Instant::now();
        let rendered = cx.background_spawn(async move {
            render_guarded(
                renderer,
                |renderer| renderer.render(&request),
                Renderer::new,
            )
        });
        cx.spawn(async move |this, cx| {
            let (renderer, rendering) = rendered.await;
            let elapsed = started.elapsed();
            this.update(cx, |viewer, cx| {
                viewer.idle_renderer = Some(renderer);
                if rendering.decoded_anew {
                    viewer.latency = smoothed_latency(viewer.latency, elapsed);
                }
                let live = target
                    .request
                    .as_ref()
                    .is_some_and(|request| viewer.is_live_request(request, cx));
                if live {
                    let picture = rendering.picture;
                    viewer.settle(Rendered { target, picture }, cx);
                }
                viewer.render_wanted(cx);
            })
            .ok();
        })
        .detach();
    }

    fn settle(&mut self, rendered: Rendered, cx: &mut Context<Self>) {
        let playhead = self.playhead.read(cx);
        let superseded =
            playhead.speed().is_paused() && rendered.target.request != self.wanted.request;
        let fate = if superseded {
            Fate::Drop
        } else {
            fate(&rendered.target, playhead.time(), playhead.speed())
        };
        match fate {
            Fate::Show => {
                self.pending = None;
                self.show(rendered.target.request, rendered.picture, cx);
            }
            Fate::Hold => self.pending = Some(rendered),
            Fate::Drop => {}
        }
    }

    fn show(&mut self, shown: Option<FrameRequest>, picture: Picture, cx: &mut Context<Self>) {
        if let Picture::Frame(replaced) = mem::replace(&mut self.picture, picture) {
            self.release(replaced, cx);
        }
        self.shown = shown;
        cx.notify();
    }

    fn release(&self, image: Arc<RenderImage>, cx: &mut Context<Self>) {
        let window = self.window;
        cx.defer(move |cx| {
            let released = window.update(cx, |_, window, _| window.drop_image(image));
            if let Ok(Err(error)) = released {
                tracing::warn!(%error, "failed to release a viewer frame");
            }
        });
    }

    fn picture(&self, placeholder: String) -> AnyElement {
        match &self.picture {
            Picture::Empty => div().child(placeholder).into_any_element(),
            Picture::Black => div().size_full().bg(theme::black()).into_any_element(),
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
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let settings = self.project.read(cx).settings;
        let playhead = self.playhead.read(cx);
        let timecode = Timecode::new(playhead.time(), settings.frame_rate);
        let speed = speed_label(playhead.speed());
        let aspect_ratio = settings.width.get() as f32 / settings.height.get() as f32;
        let placeholder = format!(
            "{}×{} · {:.3} fps",
            settings.width,
            settings.height,
            settings.frame_rate.as_f64()
        );
        let viewer = cx.entity();
        let mut frame = div()
            .relative()
            .max_w_full()
            .max_h_full()
            .h_full()
            .flex()
            .items_center()
            .justify_center()
            .overflow_hidden()
            .bg(theme::frame())
            .text_color(theme::text_muted())
            .child(self.picture(placeholder))
            .children(
                self.safe_areas
                    .then(|| SAFE_AREAS.map(safe_area))
                    .into_iter()
                    .flatten(),
            )
            .child(
                canvas(
                    move |bounds, window, cx| {
                        let scale = window.scale_factor();
                        viewer.update(cx, |viewer, cx| viewer.set_view(bounds.size, scale, cx));
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            );
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

fn safe_area(share: f32) -> gpui::Div {
    let margin = (1. - share) / 2.;
    div()
        .absolute()
        .left(relative(margin))
        .top(relative(margin))
        .w(relative(share))
        .h(relative(share))
        .border_1()
        .border_color(theme::safe_area())
}

fn sequence_bounds(project: &Project) -> (u32, u32) {
    (project.settings.width.get(), project.settings.height.get())
}

fn render_bounds(sequence: (u32, u32), view: Option<(f32, f32)>) -> (u32, u32) {
    let Some((view_width, view_height)) =
        view.filter(|(width, height)| *width > 0. && *height > 0.)
    else {
        return sequence;
    };
    let (width, height) = (sequence.0 as f32, sequence.1 as f32);
    let fit = (view_width / width).min(view_height / height).min(1.);
    let stepped = ((fit * SIZE_STEPS).ceil() / SIZE_STEPS).clamp(1. / SIZE_STEPS, 1.);
    let scaled = |side: f32, original: u32| ((side * stepped).round() as u32).clamp(1, original);
    (scaled(width, sequence.0), scaled(height, sequence.1))
}

fn frame_request(project: &Project, time: Time, bounds: (u32, u32)) -> Option<FrameRequest> {
    let layers: Vec<LayerRequest> = project
        .timeline
        .video_layers_at(time)
        .filter_map(|(track, clip)| {
            let asset = project.asset(clip.asset)?;
            Some(LayerRequest {
                decoder: DecoderKey {
                    path: asset.path.clone(),
                    stream: asset.info.default_video()?.index,
                    track,
                    bounds,
                },
                time: clip.source_time_at(time)?,
            })
        })
        .collect();
    (!layers.is_empty()).then_some(FrameRequest { layers, bounds })
}

fn presentation_time(
    playhead: Time,
    speed: Speed,
    latency: Duration,
    frame_rate: FrameRate,
    last: Option<Time>,
) -> Time {
    let Some(last) = last.filter(|_| !speed.is_paused()) else {
        return playhead;
    };
    let ahead = frame_rate.frame_start(playhead + Time::from_duration(latency) * speed.factor());
    ahead.clamp(Time::ZERO, last.max(playhead))
}

fn fate(target: &Target, playhead: Time, speed: Speed) -> Fate {
    let due = match speed.factor() {
        0 => target.time == playhead,
        1.. => target.time <= playhead,
        _ => target.time >= playhead,
    };
    if due {
        Fate::Show
    } else if target.speed == speed && !speed.is_paused() {
        Fate::Hold
    } else {
        Fate::Drop
    }
}

fn smoothed_latency(previous: Duration, sample: Duration) -> Duration {
    (previous * (LATENCY_SMOOTHING - 1) + sample) / LATENCY_SMOOTHING
}

fn speed_label(speed: Speed) -> Option<String> {
    let factor = speed.factor();
    match factor {
        0 => None,
        1.. => Some(format!("▶ {factor}×")),
        _ => Some(format!("◀ {}×", factor.unsigned_abs())),
    }
}

struct Decoded {
    frames: Vec<Arc<VideoFrame>>,
    first_failure: Option<SharedString>,
    decoded_anew: bool,
}

enum LayerFailure {
    Remembered(SharedString),
    Open(tessera_media::Error),
    Frame(tessera_media::Error),
}

struct LayerDecode {
    decoder: Option<VideoDecoder>,
    frame: Result<Arc<VideoFrame>, LayerFailure>,
    decoded_anew: bool,
}

fn decode_layers(
    decoders: &mut Decoders,
    failed: &mut FailedMedia,
    layers: &[LayerRequest],
) -> Decoded {
    let jobs: Vec<(&LayerRequest, Result<Option<VideoDecoder>, SharedString>)> = layers
        .iter()
        .map(|layer| match failed.get(&layer.decoder) {
            Some(reason) => (layer, Err(reason.clone())),
            None => (layer, Ok(decoders.remove(&layer.decoder))),
        })
        .collect();
    let outcomes: Vec<LayerDecode> = if layers.len() > 1 {
        thread::scope(|scope| {
            let running: Vec<_> = jobs
                .into_iter()
                .map(|(layer, decoder)| scope.spawn(move || decode_layer(layer, decoder)))
                .collect();
            running
                .into_iter()
                .map(|running| {
                    running.join().unwrap_or_else(|_| LayerDecode {
                        decoder: None,
                        frame: Err(LayerFailure::Remembered(RENDERER_PANICKED.into())),
                        decoded_anew: true,
                    })
                })
                .collect()
        })
    } else {
        jobs.into_iter()
            .map(|(layer, decoder)| decode_layer(layer, decoder))
            .collect()
    };
    let mut decoded = Decoded {
        frames: Vec::new(),
        first_failure: None,
        decoded_anew: false,
    };
    for (layer, outcome) in layers.iter().zip(outcomes) {
        decoded.decoded_anew |= outcome.decoded_anew;
        if let Some(decoder) = outcome.decoder {
            decoders.insert(layer.decoder.clone(), decoder);
        }
        let path = layer.decoder.path.display();
        match outcome.frame {
            Ok(frame) => decoded.frames.push(frame),
            Err(failure) => {
                let reason = match failure {
                    LayerFailure::Remembered(reason) => reason,
                    LayerFailure::Open(error) => {
                        tracing::warn!(%path, %error, "viewer cannot open the media");
                        let reason = SharedString::from(error.to_string());
                        failed.insert(layer.decoder.clone(), reason.clone());
                        reason
                    }
                    LayerFailure::Frame(error) => {
                        tracing::warn!(%path, %error, "viewer decode failed");
                        error.to_string().into()
                    }
                };
                decoded.first_failure.get_or_insert(reason);
            }
        }
    }
    decoded
}

fn decode_layer(
    layer: &LayerRequest,
    decoder: Result<Option<VideoDecoder>, SharedString>,
) -> LayerDecode {
    let mut decoder = match decoder {
        Err(reason) => {
            return LayerDecode {
                decoder: None,
                frame: Err(LayerFailure::Remembered(reason)),
                decoded_anew: false,
            };
        }
        Ok(Some(decoder)) => decoder,
        Ok(None) => {
            let DecoderKey {
                path,
                stream,
                bounds,
                ..
            } = &layer.decoder;
            let (width, height) = *bounds;
            match VideoDecoder::open(path, *stream) {
                Ok(opened) => {
                    let opened = opened.fit_within(width, height);
                    tracing::debug!(
                        path = %path.display(),
                        hw_accel = ?opened.hw_accel(),
                        "viewer decoder opened"
                    );
                    opened
                }
                Err(error) => {
                    return LayerDecode {
                        decoder: None,
                        frame: Err(LayerFailure::Open(error)),
                        decoded_anew: true,
                    };
                }
            }
        }
    };
    let decoded_anew = !decoder.is_cached(layer.time);
    let frame = decoder.frame_at(layer.time).map_err(LayerFailure::Frame);
    LayerDecode {
        decoder: Some(decoder),
        frame,
        decoded_anew,
    }
}

struct Assembled {
    image: Result<Arc<RenderImage>, SharedString>,
    compositing_failed: bool,
}

fn fills_the_sequence(frames: &[Arc<VideoFrame>], (width, height): (u32, u32)) -> bool {
    matches!(frames, [only] if only.width == width && only.height == height)
}

fn assemble_image(
    compositor: Option<&mut Compositor>,
    bounds: (u32, u32),
    mut frames: Vec<Arc<VideoFrame>>,
) -> Assembled {
    let mut compositing_failed = false;
    let composited = match compositor {
        Some(compositor) if !fills_the_sequence(&frames, bounds) => {
            match composite_frames(compositor, bounds, &frames) {
                Ok(composited) => Some(composited),
                Err(error) => {
                    tracing::warn!(%error, "viewer compositing failed, showing the top layer as decoded");
                    compositing_failed = true;
                    None
                }
            }
        }
        _ => None,
    };
    let image = match composited {
        Some(composited) => render_image(composited),
        None => frames.pop().and_then(render_image),
    };
    Assembled {
        image: image.ok_or_else(|| SharedString::from("The decoded frame has an unexpected size")),
        compositing_failed,
    }
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
    use std::num::NonZero;

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
            streams: vec![Stream::Video(VideoStream::new(
                0,
                "h264",
                NonZero::new(640).unwrap(),
                NonZero::new(360).unwrap(),
            ))],
        }
    }

    fn audio_info(seconds: i64) -> MediaInfo {
        MediaInfo {
            duration: Some(Time::from_seconds(seconds)),
            streams: vec![Stream::Audio(AudioStream::new(
                0,
                "flac",
                NonZero::new(48_000).unwrap(),
                NonZero::new(2).unwrap(),
            ))],
        }
    }

    #[test]
    fn layers_decode_the_probed_video_stream() {
        let mut project = Project::new("streams");
        let mut info = audio_info(4);
        info.streams.extend(
            video_info(4)
                .streams
                .into_iter()
                .map(|stream| match stream {
                    Stream::Video(video) => Stream::Video(VideoStream { index: 1, ..video }),
                    audio @ Stream::Audio(_) => audio,
                }),
        );
        let asset = project.add_asset("/missing/both.mkv".into(), info);
        project.place_clip(asset, 0, Time::ZERO).unwrap();

        let request = frame_request(&project, Time::ZERO, sequence_bounds(&project)).unwrap();

        assert_eq!(request.layers[0].decoder.stream, 1);
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
        cx.read(|cx| layer_paths(viewer.read(cx).wanted.request.as_ref()))
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
        let paths = |seconds| {
            layer_paths(
                frame_request(
                    &project,
                    Time::from_seconds(seconds),
                    sequence_bounds(&project),
                )
                .as_ref(),
            )
        };
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
        assert!(
            frame_request(&project, Time::from_seconds(9), sequence_bounds(&project)).is_none()
        );

        let request =
            frame_request(&project, Time::from_seconds(4), sequence_bounds(&project)).unwrap();
        assert_eq!(request.bounds, sequence_bounds(&project));
        assert!(request.decoders().all(|decoder| decoder.stream == 0));
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

    #[test]
    fn playing_asks_for_the_frame_due_once_the_render_lands() {
        let rate = FrameRate::FPS_30;
        let last = Some(rate.frame_to_time(299));
        let at = |frame, speed, millis| {
            let latency = Duration::from_millis(millis);
            presentation_time(rate.frame_to_time(frame), speed, latency, rate, last)
        };
        assert_eq!(at(30, Speed::FORWARD, 100), rate.frame_to_time(33));
        assert_eq!(at(30, Speed::BACKWARD, 100), rate.frame_to_time(27));
        assert_eq!(at(30, Speed::FORWARD, 10), rate.frame_to_time(30));
        assert_eq!(at(30, Speed::PAUSED, 100), rate.frame_to_time(30));
        assert_eq!(at(298, Speed::FORWARD, 500), rate.frame_to_time(299));
        assert_eq!(at(2, Speed::BACKWARD, 500), Time::ZERO);
        assert_eq!(
            presentation_time(
                rate.frame_to_time(30),
                Speed::FORWARD,
                Duration::from_secs(1),
                rate,
                None
            ),
            rate.frame_to_time(30)
        );
    }

    #[test]
    fn a_rendered_frame_waits_for_the_playhead_and_is_dropped_when_playback_changes() {
        let rate = FrameRate::FPS_30;
        let target = |frame, speed| Target {
            time: rate.frame_to_time(frame),
            speed,
            request: None,
        };
        let now = rate.frame_to_time(30);
        assert_eq!(
            fate(&target(33, Speed::FORWARD), now, Speed::FORWARD),
            Fate::Hold
        );
        assert_eq!(
            fate(&target(30, Speed::FORWARD), now, Speed::FORWARD),
            Fate::Show
        );
        assert_eq!(
            fate(&target(28, Speed::FORWARD), now, Speed::FORWARD),
            Fate::Show
        );
        assert_eq!(
            fate(&target(27, Speed::BACKWARD), now, Speed::BACKWARD),
            Fate::Hold
        );
        assert_eq!(
            fate(&target(31, Speed::BACKWARD), now, Speed::BACKWARD),
            Fate::Show
        );
        assert_eq!(
            fate(&target(33, Speed::FORWARD), now, Speed::PAUSED),
            Fate::Drop
        );
        assert_eq!(
            fate(&target(30, Speed::FORWARD), now, Speed::PAUSED),
            Fate::Show
        );
        assert_eq!(
            fate(&target(30, Speed::PAUSED), now, Speed::PAUSED),
            Fate::Show
        );
        assert_eq!(
            fate(&target(12, Speed::PAUSED), now, Speed::PAUSED),
            Fate::Drop
        );
        assert_eq!(
            fate(&target(27, Speed::FORWARD), now, Speed::BACKWARD),
            Fate::Drop
        );
        assert_eq!(
            fate(&target(33, Speed::PAUSED), now, Speed::FORWARD),
            Fate::Drop
        );
    }

    #[test]
    fn the_latency_estimate_follows_render_times_smoothly() {
        let latency = smoothed_latency(Duration::ZERO, Duration::from_millis(40));
        assert_eq!(latency, Duration::from_millis(10));
        let settled = (0..40).fold(latency, |latency, _| {
            smoothed_latency(latency, Duration::from_millis(40))
        });
        assert!(settled > Duration::from_millis(39) && settled <= Duration::from_millis(40));
    }

    #[gpui::test]
    fn the_viewer_asks_for_every_video_layer_under_the_playhead(cx: &mut TestAppContext) {
        let project = cx.new(|_| stacked_project());
        let playhead = cx.new(|cx| Playhead::new(project.clone(), cx));
        let (viewer, cx) = cx.add_window_view(|window, cx| {
            Viewer::new(project.clone(), playhead.clone(), window, cx)
        });
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
        assert!(cx.read(|cx| viewer.read(cx).wanted.request.is_none()));
        assert!(cx.read(|cx| viewer.read(cx).shown.is_none()));
        assert!(cx.read(|cx| matches!(viewer.read(cx).picture, Picture::Black)));
    }

    #[gpui::test]
    fn a_replaced_project_asks_for_frames_of_its_own_media(cx: &mut TestAppContext) {
        let project = cx.new(|_| project_showing("/missing/first.mkv"));
        let (viewer, cx) = cx.add_window_view(|window, cx| {
            let playhead = cx.new(|cx| Playhead::new(project.clone(), cx));
            Viewer::new(project.clone(), playhead, window, cx)
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
        let image = assemble_image(Some(&mut compositor), (8, 4), frames)
            .image
            .unwrap();
        assert_eq!(image.size(0), gpui::size(8.into(), 4.into()));
        assert_eq!(image.as_bytes(0).unwrap()[..4], RED);
    }

    #[test]
    fn without_a_compositor_the_top_frame_is_shown_as_decoded() {
        let frames = vec![solid(8, 4, RED), solid(4, 4, BLUE)];
        let image = assemble_image(None, (8, 4), frames).image.unwrap();
        assert_eq!(image.size(0), gpui::size(4.into(), 4.into()));
        assert_eq!(image.as_bytes(0).unwrap()[..4], BLUE);
    }

    #[test]
    fn a_single_layer_of_the_sequence_size_skips_the_gpu_pass() {
        let sequence = (8, 4);

        assert!(fills_the_sequence(&[solid(8, 4, RED)], sequence));
        assert!(!fills_the_sequence(&[solid(4, 4, RED)], sequence));
        assert!(!fills_the_sequence(
            &[solid(8, 4, RED), solid(8, 4, BLUE)],
            sequence
        ));
        assert!(!fills_the_sequence(&[], sequence));

        let image = assemble_image(None, sequence, vec![solid(8, 4, BLUE)]);

        assert!(!image.compositing_failed);
        assert_eq!(image.image.unwrap().as_bytes(0).unwrap()[..4], BLUE);
    }

    #[test]
    fn rendering_scales_the_sequence_down_to_the_view_but_never_up() {
        let sequence = (3840, 2160);

        assert_eq!(render_bounds(sequence, None), sequence);
        assert_eq!(render_bounds(sequence, Some((0., 0.))), sequence);
        assert_eq!(render_bounds(sequence, Some((8000., 5000.))), sequence);
        assert_eq!(render_bounds(sequence, Some((1920., 1080.))), (1920, 1080));
        assert_eq!(render_bounds(sequence, Some((1000., 1000.))), (1440, 810));

        let (width, height) = render_bounds(sequence, Some((10., 10.)));
        assert!(width < 600 && height < 340);
        assert_eq!(
            render_bounds(sequence, Some((1000., 1000.))),
            render_bounds(sequence, Some((990., 990.)))
        );
    }

    #[test]
    fn two_clips_of_one_file_on_different_tracks_get_their_own_decoders() {
        let mut project = Project::new("twins");
        let upper = project.timeline.add_track(TrackKind::Video);
        let asset = project.add_asset("/missing/twin.mkv".into(), video_info(8));
        project.place_clip(asset, 0, Time::ZERO).unwrap();
        project.place_clip(asset, upper, Time::ZERO).unwrap();

        let request =
            frame_request(&project, Time::from_seconds(1), sequence_bounds(&project)).unwrap();
        let tracks: Vec<usize> = request.decoders().map(|decoder| decoder.track).collect();

        assert_eq!(tracks, [0, upper]);
        assert_ne!(request.layers[0].decoder, request.layers[1].decoder);
    }

    #[test]
    fn media_that_fails_to_open_is_remembered_and_the_other_layers_still_show() {
        let key = |path: &str, track| DecoderKey {
            path: path.into(),
            stream: 0,
            track,
            bounds: (8, 4),
        };
        let layers = [
            LayerRequest {
                decoder: key("/missing/first.mkv", 0),
                time: Time::ZERO,
            },
            LayerRequest {
                decoder: key("/missing/second.mkv", 1),
                time: Time::ZERO,
            },
        ];
        let mut decoders = Decoders::new();
        let mut failed = FailedMedia::new();

        let decoded = decode_layers(&mut decoders, &mut failed, &layers);

        assert!(decoded.frames.is_empty());
        assert!(decoded.first_failure.is_some());
        assert!(decoded.decoded_anew);
        assert_eq!(failed.len(), 2);
        assert!(decoders.is_empty());

        let again = decode_layers(&mut decoders, &mut failed, &layers[..1]);

        assert_eq!(again.first_failure, decoded.first_failure);
        assert!(!again.decoded_anew);
        assert_eq!(failed.len(), 2);
    }

    #[test]
    fn a_panicking_render_gives_back_a_fresh_renderer_and_a_failure() {
        let black = |_: &mut u32| Rendering {
            picture: Picture::Black,
            decoded_anew: false,
        };

        let (renderer, rendering) =
            render_guarded(1_u32, |_| panic!("the device was lost"), || 2_u32);

        assert_eq!(renderer, 2);
        assert!(
            matches!(rendering.picture, Picture::Failed(reason) if reason == RENDERER_PANICKED)
        );

        let (renderer, rendering) = render_guarded(1_u32, black, || 2_u32);

        assert_eq!(renderer, 1);
        assert!(matches!(rendering.picture, Picture::Black));
    }

    #[gpui::test]
    fn a_deleted_clip_leaves_black_at_once(cx: &mut TestAppContext) {
        let project = cx.new(|_| project_showing("/missing/only.mkv"));
        let (viewer, cx) = cx.add_window_view(|window, cx| {
            let playhead = cx.new(|cx| Playhead::new(project.clone(), cx));
            Viewer::new(project.clone(), playhead, window, cx)
        });
        cx.run_until_parked();

        assert!(cx.read(|cx| viewer.read(cx).shown.is_some()));

        cx.update(|_, cx| {
            let editor = ProjectEditor::new(project.clone(), cx);
            let id = project.read(cx).timeline.tracks[0].clips()[0].id;
            editor
                .apply(tessera_timeline::Command::DeleteClip, cx, |project| {
                    project.delete_clip(id)
                })
                .unwrap();
        });

        assert!(cx.read(|cx| viewer.read(cx).shown.is_none()));
        assert!(cx.read(|cx| matches!(viewer.read(cx).picture, Picture::Black)));
    }
}
