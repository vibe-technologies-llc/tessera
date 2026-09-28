mod compositor;
mod fit;

pub use compositor::Compositor;
use fit::Size;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("no suitable GPU adapter: {0}")]
    Adapter(#[from] wgpu::RequestAdapterError),
    #[error("failed to create GPU device: {0}")]
    Device(#[from] wgpu::RequestDeviceError),
    #[error("the sequence has no pixels")]
    EmptySequence,
    #[error("layer {index} has no pixels")]
    EmptyLayer { index: usize },
    #[error("layer {index} holds {actual} bytes where {expected} were expected")]
    LayerLength {
        index: usize,
        expected: usize,
        actual: usize,
    },
    #[error("{width}×{height} exceeds the GPU's largest texture side of {max}")]
    TooLarge { width: u32, height: u32, max: u32 },
    #[error("failed to wait for the GPU: {0}")]
    Poll(#[from] wgpu::PollError),
    #[error("failed to map the composited frame: {0}")]
    Readback(#[from] wgpu::BufferAsyncError),
    #[error("failed to read the composited frame: {0}")]
    MappedRange(#[from] wgpu::MapRangeError),
}

#[derive(Clone, Copy, Debug)]
pub struct Layer<'a> {
    pub width: u32,
    pub height: u32,
    pub bgra: &'a [u8],
}

impl Layer<'_> {
    fn size(&self) -> Size {
        Size {
            width: self.width,
            height: self.height,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}
