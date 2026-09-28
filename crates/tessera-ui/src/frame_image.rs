use std::sync::Arc;

use gpui::RenderImage;
use tessera_media::VideoFrame;
use tessera_render::Frame;

pub trait PackedBgra {
    fn into_packed_bgra(self) -> (u32, u32, Vec<u8>);
}

impl PackedBgra for Arc<VideoFrame> {
    fn into_packed_bgra(self) -> (u32, u32, Vec<u8>) {
        let frame = Arc::unwrap_or_clone(self);
        (frame.width, frame.height, frame.bgra)
    }
}

impl PackedBgra for Frame {
    fn into_packed_bgra(self) -> (u32, u32, Vec<u8>) {
        (self.width, self.height, self.bgra)
    }
}

pub fn render_image(pixels: impl PackedBgra) -> Option<Arc<RenderImage>> {
    let (width, height, bgra) = pixels.into_packed_bgra();
    let buffer = image::RgbaImage::from_raw(width, height, bgra)?;
    Some(Arc::new(RenderImage::new([image::Frame::new(buffer)])))
}
