use std::sync::Arc;

use gpui::RenderImage;
use tessera_media::VideoFrame;

pub fn render_image(frame: Arc<VideoFrame>) -> Option<Arc<RenderImage>> {
    let frame = Arc::unwrap_or_clone(frame);
    let buffer = image::RgbaImage::from_raw(frame.width, frame.height, frame.bgra)?;
    Some(Arc::new(RenderImage::new([image::Frame::new(buffer)])))
}
