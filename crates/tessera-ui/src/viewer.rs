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
use tessera_timeline::{AssetId, Project, Time, Timecode};

use crate::{frame_image::render_image, playhead::Playhead, theme};

type Decoders = HashMap<AssetId, VideoDecoder>;

#[derive(Clone, Debug, PartialEq, Eq)]
struct FrameRequest {
    asset: AssetId,
    path: PathBuf,
    time: Time,
    bounds: (u32, u32),
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
        let wanted = self.frame_request(cx);
        if wanted != self.wanted {
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
            asset: asset.id,
            path: asset.path.clone(),
            time: clip.source_time_at(time)?,
            bounds: (project.settings.width, project.settings.height),
        })
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
                viewer.show(Some(request), picture, cx);
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
        let timecode = Timecode::new(self.playhead.read(cx).time(), settings.frame_rate);
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
                    .child(timecode.to_string()),
            )
    }
}

fn decode_picture(decoders: &mut Decoders, request: &FrameRequest) -> Picture {
    match decode_frame(decoders, request) {
        Ok(Some(image)) => Picture::Frame(image),
        Ok(None) => Picture::Failed("The decoded frame has an unexpected size".into()),
        Err(error) => {
            tracing::warn!(path = %request.path.display(), %error, "viewer decode failed");
            Picture::Failed(error.to_string().into())
        }
    }
}

fn decode_frame(
    decoders: &mut Decoders,
    request: &FrameRequest,
) -> Result<Option<Arc<RenderImage>>, tessera_media::Error> {
    let decoder = match decoders.entry(request.asset) {
        Entry::Occupied(entry) => entry.into_mut(),
        Entry::Vacant(entry) => {
            let (width, height) = request.bounds;
            entry.insert(VideoDecoder::open(&request.path)?.fit_within(width, height))
        }
    };
    Ok(render_image(decoder.frame_at(request.time)?))
}
