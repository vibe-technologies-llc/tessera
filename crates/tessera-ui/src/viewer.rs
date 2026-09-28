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
use tessera_media::VideoDecoder;
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
struct FrameRequest {
    decoder: DecoderKey,
    time: Time,
}

enum Picture {
    Empty,
    Frame(Arc<RenderImage>),
    Failed(SharedString),
}

pub struct Viewer {
    project: Entity<Project>,
    playhead: Entity<Playhead>,
    idle_decoders: Option<Decoders>,
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
            idle_decoders: Some(Decoders::new()),
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
            .is_some_and(|shown| !self.is_live(&shown.decoder, cx));
        if stale {
            self.show(None, Picture::Empty, cx);
        }
        let wanted = self.frame_request(cx);
        if stale || wanted != self.wanted {
            self.wanted = wanted;
            self.decode_wanted(cx);
        }
    }

    fn frame_request(&self, cx: &Context<Self>) -> Option<FrameRequest> {
        let project = self.project.read(cx);
        let time = self.playhead.read(cx).time();
        let clip = project.timeline.top_video_clip_at(time)?;
        let asset = project.asset(clip.asset)?;
        Some(FrameRequest {
            decoder: DecoderKey {
                path: asset.path.clone(),
                bounds: sequence_bounds(project),
            },
            time: clip.source_time_at(time)?,
        })
    }

    fn is_live(&self, decoder: &DecoderKey, cx: &Context<Self>) -> bool {
        let project = self.project.read(cx);
        decoder.bounds == sequence_bounds(project)
            && project
                .assets
                .iter()
                .any(|asset| asset.path == decoder.path)
    }

    fn decode_wanted(&mut self, cx: &mut Context<Self>) {
        if self.wanted == self.shown {
            return;
        }
        let Some(request) = self.wanted.clone() else {
            self.show(None, Picture::Empty, cx);
            return;
        };
        let Some(mut decoders) = self.idle_decoders.take() else {
            return;
        };
        decoders.retain(|key, _| self.is_live(key, cx));
        let decoded = cx.background_spawn({
            let request = request.clone();
            async move {
                let picture = decode_picture(&mut decoders, &request);
                (decoders, picture)
            }
        });
        cx.spawn(async move |this, cx| {
            let (decoders, picture) = decoded.await;
            this.update(cx, |viewer, cx| {
                viewer.idle_decoders = Some(decoders);
                if viewer.is_live(&request.decoder, cx) {
                    viewer.show(Some(request), picture, cx);
                }
                viewer.decode_wanted(cx);
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

fn speed_label(speed: Speed) -> Option<String> {
    let factor = speed.factor();
    match factor {
        0 => None,
        1.. => Some(format!("▶ {factor}×")),
        _ => Some(format!("◀ {}×", factor.unsigned_abs())),
    }
}

fn decode_picture(decoders: &mut Decoders, request: &FrameRequest) -> Picture {
    match decode_frame(decoders, request) {
        Ok(Some(image)) => Picture::Frame(image),
        Ok(None) => Picture::Failed("The decoded frame has an unexpected size".into()),
        Err(error) => {
            tracing::warn!(path = %request.decoder.path.display(), %error, "viewer decode failed");
            Picture::Failed(error.to_string().into())
        }
    }
}

fn decode_frame(
    decoders: &mut Decoders,
    request: &FrameRequest,
) -> Result<Option<Arc<RenderImage>>, tessera_media::Error> {
    let decoder = match decoders.entry(request.decoder.clone()) {
        Entry::Occupied(entry) => entry.into_mut(),
        Entry::Vacant(entry) => {
            let DecoderKey { path, bounds } = entry.key();
            let (width, height) = *bounds;
            let opened = VideoDecoder::open(path)?.fit_within(width, height);
            entry.insert(opened)
        }
    };
    Ok(render_image(decoder.frame_at(request.time)?))
}

#[cfg(test)]
mod tests {
    use gpui::{TestAppContext, VisualTestContext};
    use tessera_timeline::{MediaInfo, Stream, VideoStream};

    use super::*;
    use crate::editor::ProjectEditor;

    fn project_showing(media: &str) -> Project {
        let mut project = Project::new(media);
        let info = MediaInfo {
            duration: Some(Time::from_seconds(4)),
            streams: vec![Stream::Video(VideoStream {
                index: 0,
                codec: "h264".into(),
                width: 640,
                height: 360,
                frame_rate: None,
            })],
        };
        let asset = project.add_asset(media.into(), info);
        project.place_clip(asset, 0, Time::ZERO).unwrap();
        project
    }

    fn wanted_path(viewer: &Entity<Viewer>, cx: &mut VisualTestContext) -> Option<PathBuf> {
        cx.read(|cx| {
            viewer
                .read(cx)
                .wanted
                .as_ref()
                .map(|request| request.decoder.path.clone())
        })
    }

    #[gpui::test]
    fn a_replaced_project_asks_for_frames_of_its_own_media(cx: &mut TestAppContext) {
        let project = cx.new(|_| project_showing("/missing/first.mkv"));
        let (viewer, cx) = cx.add_window_view(|_, cx| {
            let playhead = cx.new(|_| Playhead::new(project.clone()));
            Viewer::new(project.clone(), playhead, cx)
        });
        cx.run_until_parked();
        assert_eq!(wanted_path(&viewer, cx), Some("/missing/first.mkv".into()));

        let second = project_showing("/missing/second.mkv");
        assert_eq!(
            second.assets[0].id,
            project.read_with(cx, |project, _| project.assets[0].id)
        );
        cx.update(|_, cx| ProjectEditor::new(project.clone(), cx).replace(second, cx));
        assert!(cx.read(|cx| viewer.read(cx).shown.is_none()));
        assert_eq!(wanted_path(&viewer, cx), Some("/missing/second.mkv".into()));
        cx.run_until_parked();
        let shown = cx.read(|cx| viewer.read(cx).shown.clone());
        assert_eq!(
            shown.map(|request| request.decoder.path),
            Some("/missing/second.mkv".into())
        );
    }
}
