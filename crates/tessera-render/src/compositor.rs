use std::{
    collections::HashMap,
    sync::mpsc::{self, TryRecvError},
};

use crate::{
    Error, Frame, Layer, Placement,
    fit::{Rect, Size, fit_rect},
};

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Bgra8Unorm;
const BYTES_PER_PIXEL: u32 = 4;
const QUAD_CORNERS: u32 = 4;
const PLACEMENT_FLOATS: usize = 16;
type PlacementBytes = [[u8; size_of::<f32>()]; PLACEMENT_FLOATS];
const PLACEMENT_BYTES: u64 = size_of::<PlacementBytes>() as u64;

pub struct Compositor {
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::RenderPipeline,
    layer_bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    target: Option<Target>,
    layer_pool: HashMap<Size, Vec<LayerTexture>>,
}

struct Target {
    size: Size,
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    readback: wgpu::Buffer,
    padded_bytes_per_row: u32,
}

struct LayerTexture {
    texture: wgpu::Texture,
    placement: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
}

impl Compositor {
    pub fn new() -> Result<Self, Error> {
        pollster::block_on(Self::create())
    }

    async fn create() -> Result<Self, Error> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN,
            ..wgpu::InstanceDescriptor::new_without_display_handle_from_env()
        });
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                ..Default::default()
            })
            .await?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("tessera-compositor"),
                ..Default::default()
            })
            .await?;
        let layer_bind_group_layout = create_layer_bind_group_layout(&device);
        let pipeline = create_pipeline(&device, &layer_bind_group_layout);
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("tessera-layer-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Ok(Self {
            adapter,
            device,
            queue,
            pipeline,
            layer_bind_group_layout,
            sampler,
            target: None,
            layer_pool: HashMap::new(),
        })
    }

    pub fn adapter_info(&self) -> wgpu::AdapterInfo {
        self.adapter.get_info()
    }

    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    pub fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }

    pub fn composite(
        &mut self,
        width: u32,
        height: u32,
        layers: &[Layer<'_>],
    ) -> Result<Frame, Error> {
        let sequence = Size { width, height };
        self.validate(sequence, layers)?;
        let target = match self.target.take() {
            Some(target) if target.size == sequence => target,
            _ => self.create_target(sequence),
        };
        let frame = self.render(&target, layers);
        self.target = Some(target);
        frame
    }

    fn validate(&self, sequence: Size, layers: &[Layer<'_>]) -> Result<(), Error> {
        let max_side = self.device.limits().max_texture_dimension_2d;
        if sequence.width == 0 || sequence.height == 0 {
            return Err(Error::EmptySequence);
        }
        check_side(sequence, max_side)?;
        for (index, layer) in layers.iter().enumerate() {
            if layer.width == 0 || layer.height == 0 {
                return Err(Error::EmptyLayer { index });
            }
            check_side(layer.size(), max_side)?;
            let expected = packed_len(layer.size());
            if layer.bgra.len() != expected {
                return Err(Error::LayerLength {
                    index,
                    expected,
                    actual: layer.bgra.len(),
                });
            }
        }
        Ok(())
    }

    fn render(&mut self, target: &Target, layers: &[Layer<'_>]) -> Result<Frame, Error> {
        let mut spare = std::mem::take(&mut self.layer_pool);
        let uploaded: Vec<(Size, LayerTexture)> = layers
            .iter()
            .map(|layer| {
                let size = layer.size();
                let texture = spare
                    .get_mut(&size)
                    .and_then(Vec::pop)
                    .unwrap_or_else(|| self.create_layer_texture(size));
                self.upload(&texture, layer, target.size);
                (size, texture)
            })
            .collect();

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("tessera-composite"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("tessera-composite"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target.view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            pass.set_pipeline(&self.pipeline);
            for (_, texture) in &uploaded {
                pass.set_bind_group(0, &texture.bind_group, &[]);
                pass.draw(0..QUAD_CORNERS, 0..1);
            }
        }
        encoder.copy_texture_to_buffer(
            target.texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &target.readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(target.padded_bytes_per_row),
                    rows_per_image: None,
                },
            },
            extent(target.size),
        );
        self.queue.submit([encoder.finish()]);

        for (size, texture) in uploaded {
            self.layer_pool.entry(size).or_default().push(texture);
        }
        self.read_back(target)
    }

    fn upload(&self, texture: &LayerTexture, layer: &Layer<'_>, sequence: Size) {
        self.queue.write_texture(
            texture.texture.as_image_copy(),
            layer.bgra,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(layer.width * BYTES_PER_PIXEL),
                rows_per_image: None,
            },
            extent(layer.size()),
        );
        let placement =
            placement_bytes(fit_rect(layer.size(), sequence), sequence, &layer.placement);
        self.queue
            .write_buffer(&texture.placement, 0, placement.as_flattened());
    }

    fn read_back(&self, target: &Target) -> Result<Frame, Error> {
        let (sender, receiver) = mpsc::channel();
        target
            .readback
            .map_async(wgpu::MapMode::Read, .., move |mapped| {
                sender.send(mapped).ok();
            });
        let mapped = loop {
            self.device.poll(wgpu::PollType::wait_indefinitely())?;
            match receiver.try_recv() {
                Ok(mapped) => break mapped,
                Err(TryRecvError::Empty) => continue,
                Err(TryRecvError::Disconnected) => break Err(wgpu::BufferAsyncError),
            }
        };
        mapped?;
        let row_bytes = target.size.width * BYTES_PER_PIXEL;
        let bgra = target
            .readback
            .get_mapped_range(..)
            .map(|padded| strip_row_padding(&padded, row_bytes, target.padded_bytes_per_row));
        target.readback.unmap();
        Ok(Frame {
            width: target.size.width,
            height: target.size.height,
            bgra: bgra?,
        })
    }

    fn create_target(&self, size: Size) -> Target {
        let texture = self.create_texture(
            "tessera-composite-target",
            size,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        );
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let padded_bytes_per_row =
            (size.width * BYTES_PER_PIXEL).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let readback = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("tessera-composite-readback"),
            size: u64::from(padded_bytes_per_row) * u64::from(size.height),
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Target {
            size,
            texture,
            view,
            readback,
            padded_bytes_per_row,
        }
    }

    fn create_layer_texture(&self, size: Size) -> LayerTexture {
        let texture = self.create_texture(
            "tessera-layer",
            size,
            wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        );
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let placement = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("tessera-layer-placement"),
            size: PLACEMENT_BYTES,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("tessera-layer"),
            layout: &self.layer_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: placement.as_entire_binding(),
                },
            ],
        });
        LayerTexture {
            texture,
            placement,
            bind_group,
        }
    }

    fn create_texture(&self, label: &str, size: Size, usage: wgpu::TextureUsages) -> wgpu::Texture {
        self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: extent(size),
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: FORMAT,
            usage,
            view_formats: &[],
        })
    }
}

fn create_layer_bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("tessera-layer"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(PLACEMENT_BYTES),
                },
                count: None,
            },
        ],
    })
}

fn create_pipeline(
    device: &wgpu::Device,
    layer_bind_group_layout: &wgpu::BindGroupLayout,
) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::include_wgsl!("composite.wgsl"));
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("tessera-composite"),
        bind_group_layouts: &[Some(layer_bind_group_layout)],
        immediate_size: 0,
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("tessera-composite"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vertex_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            buffers: &[],
        },
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleStrip,
            ..Default::default()
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fragment_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: FORMAT,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    })
}

fn check_side(size: Size, max_side: u32) -> Result<(), Error> {
    if size.width > max_side || size.height > max_side {
        return Err(Error::TooLarge {
            width: size.width,
            height: size.height,
            max: max_side,
        });
    }
    Ok(())
}

fn packed_len(size: Size) -> usize {
    size.width as usize * size.height as usize * BYTES_PER_PIXEL as usize
}

fn extent(size: Size) -> wgpu::Extent3d {
    wgpu::Extent3d {
        width: size.width,
        height: size.height,
        depth_or_array_layers: 1,
    }
}

fn placement_bytes(rect: Rect, sequence: Size, placement: &Placement) -> PlacementBytes {
    let [left, top, right, bottom] = placement.crop.map(|side| side.clamp(0.0, 1.0));
    let (width, height) = (rect.width as f32, rect.height as f32);
    let centre = [
        rect.x as f32 + width / 2.0 + placement.offset[0] * sequence.width as f32,
        rect.y as f32 + height / 2.0 + placement.offset[1] * sequence.height as f32,
    ];
    let (sin, cos) = placement.rotation_degrees.to_radians().sin_cos();
    let corner = |u: f32, v: f32| {
        let local = [
            (u - 0.5) * width * placement.scale,
            (v - 0.5) * height * placement.scale,
        ];
        let turned = [
            local[0] * cos - local[1] * sin,
            local[0] * sin + local[1] * cos,
        ];
        [
            (centre[0] + turned[0]) / sequence.width as f32 * 2.0 - 1.0,
            1.0 - (centre[1] + turned[1]) / sequence.height as f32 * 2.0,
        ]
    };
    let (u0, v0, u1, v1) = (left, top, 1.0 - right, 1.0 - bottom);
    let floats: [f32; PLACEMENT_FLOATS] = [
        corner(u0, v0),
        corner(u1, v0),
        corner(u0, v1),
        corner(u1, v1),
        [u0, v0],
        [u1, v1],
        [placement.opacity.clamp(0.0, 1.0), 0.0],
        [0.0, 0.0],
    ]
    .as_flattened()
    .try_into()
    .expect("a placement is sixteen floats");
    floats.map(f32::to_le_bytes)
}

fn strip_row_padding(padded: &[u8], row_bytes: u32, padded_bytes_per_row: u32) -> Vec<u8> {
    let rows = padded.chunks_exact(padded_bytes_per_row as usize);
    let mut packed = Vec::with_capacity(rows.len() * row_bytes as usize);
    for row in rows {
        packed.extend_from_slice(&row[..row_bytes as usize]);
    }
    packed
}

#[cfg(test)]
mod tests {
    use super::*;

    const OPAQUE_BLACK: [u8; 4] = [0, 0, 0, 255];
    const RED: [u8; 4] = [0, 0, 255, 255];
    const GREEN: [u8; 4] = [0, 255, 0, 255];
    const BLUE: [u8; 4] = [255, 0, 0, 255];

    fn compositor() -> Compositor {
        Compositor::new().expect("a Vulkan adapter")
    }

    fn solid(width: u32, height: u32, bgra: [u8; 4]) -> Vec<u8> {
        bgra.repeat(packed_len(Size { width, height }) / 4)
    }

    fn layer(width: u32, height: u32, bgra: &[u8]) -> Layer<'_> {
        Layer {
            width,
            height,
            bgra,
            placement: Placement::FIT,
        }
    }

    fn placed(layer: Layer<'_>, placement: Placement) -> Layer<'_> {
        Layer { placement, ..layer }
    }

    fn pixel(frame: &Frame, x: u32, y: u32) -> [u8; 4] {
        let start = ((y * frame.width + x) * BYTES_PER_PIXEL) as usize;
        frame.bgra[start..start + 4].try_into().unwrap()
    }

    fn assert_near(actual: [u8; 4], expected: [u8; 4]) {
        let near = actual
            .iter()
            .zip(expected)
            .all(|(&actual, expected)| actual.abs_diff(expected) <= 2);
        assert!(near, "{actual:?} is not within 2 of {expected:?}");
    }

    #[test]
    fn empty_layer_list_gives_opaque_black() {
        let frame = compositor().composite(6, 3, &[]).unwrap();
        assert_eq!((frame.width, frame.height), (6, 3));
        assert_eq!(frame.bgra, solid(6, 3, OPAQUE_BLACK));
    }

    #[test]
    fn full_size_layer_comes_back_unchanged() {
        let (width, height) = (37, 5);
        let bgra: Vec<u8> = (0..width * height)
            .flat_map(|index| {
                let index = index as u8;
                [index, index.wrapping_mul(7), index.wrapping_mul(29), 255]
            })
            .collect();
        let frame = compositor()
            .composite(width, height, &[layer(width, height, &bgra)])
            .unwrap();
        assert_eq!(frame.bgra, bgra);
    }

    #[test]
    fn opaque_top_layer_hides_the_one_below() {
        let red = solid(8, 4, RED);
        let blue = solid(8, 4, BLUE);
        let mut compositor = compositor();
        let frame = compositor
            .composite(8, 4, &[layer(8, 4, &red), layer(8, 4, &blue)])
            .unwrap();
        assert_eq!(frame.bgra, blue);
        let frame = compositor
            .composite(8, 4, &[layer(8, 4, &blue), layer(8, 4, &red)])
            .unwrap();
        assert_eq!(frame.bgra, red);
    }

    #[test]
    fn half_transparent_layer_blends_over_the_one_below() {
        let bottom = solid(4, 4, [0, 0, 200, 255]);
        let top = solid(4, 4, [100, 0, 0, 128]);
        let frame = compositor()
            .composite(4, 4, &[layer(4, 4, &bottom), layer(4, 4, &top)])
            .unwrap();
        assert_near(pixel(&frame, 1, 2), [50, 0, 100, 255]);
    }

    #[test]
    fn narrower_layer_is_pillarboxed() {
        let green = solid(32, 32, GREEN);
        let frame = compositor()
            .composite(64, 32, &[layer(32, 32, &green)])
            .unwrap();
        assert_eq!(pixel(&frame, 32, 16), GREEN);
        assert_eq!(pixel(&frame, 4, 16), OPAQUE_BLACK);
        assert_eq!(pixel(&frame, 59, 16), OPAQUE_BLACK);
    }

    #[test]
    fn wider_layer_is_letterboxed() {
        let green = solid(64, 16, GREEN);
        let frame = compositor()
            .composite(32, 32, &[layer(64, 16, &green)])
            .unwrap();
        assert_eq!(pixel(&frame, 16, 16), GREEN);
        assert_eq!(pixel(&frame, 16, 4), OPAQUE_BLACK);
        assert_eq!(pixel(&frame, 16, 27), OPAQUE_BLACK);
    }

    #[test]
    fn letterbox_bars_show_the_layer_below() {
        let red = solid(32, 32, RED);
        let green = solid(64, 16, GREEN);
        let frame = compositor()
            .composite(32, 32, &[layer(32, 32, &red), layer(64, 16, &green)])
            .unwrap();
        assert_eq!(pixel(&frame, 16, 16), GREEN);
        assert_eq!(pixel(&frame, 16, 4), RED);
    }

    #[test]
    fn smaller_layer_scales_up_to_fit() {
        let halves: Vec<u8> = (0..2)
            .flat_map(|_| [RED, RED, BLUE, BLUE])
            .flatten()
            .collect();
        let frame = compositor()
            .composite(40, 20, &[layer(4, 2, &halves)])
            .unwrap();
        assert_eq!(pixel(&frame, 5, 10), RED);
        assert_eq!(pixel(&frame, 34, 10), BLUE);
        assert_eq!(pixel(&frame, 0, 0), RED);
        assert_eq!(pixel(&frame, 39, 19), BLUE);
    }

    #[test]
    fn changing_the_sequence_size_rebuilds_the_target() {
        let mut compositor = compositor();
        let frame = compositor.composite(8, 4, &[]).unwrap();
        assert_eq!(frame.bgra.len(), 8 * 4 * 4);
        let green = solid(5, 9, GREEN);
        let frame = compositor.composite(5, 9, &[layer(5, 9, &green)]).unwrap();
        assert_eq!((frame.width, frame.height), (5, 9));
        assert_eq!(frame.bgra, green);
        let target = compositor.target.as_ref().unwrap();
        assert_eq!(
            target.size,
            Size {
                width: 5,
                height: 9
            }
        );
        let frame = compositor.composite(8, 4, &[]).unwrap();
        assert_eq!(frame.bgra, solid(8, 4, OPAQUE_BLACK));
    }

    #[test]
    fn same_size_target_is_kept() {
        let mut compositor = compositor();
        compositor.composite(8, 4, &[]).unwrap();
        let first = compositor.target.as_ref().unwrap().texture.clone();
        compositor.composite(8, 4, &[]).unwrap();
        assert_eq!(compositor.target.as_ref().unwrap().texture, first);
    }

    #[test]
    fn same_size_layers_reuse_pooled_textures() {
        let size = Size {
            width: 16,
            height: 16,
        };
        let red = solid(16, 16, RED);
        let blue = solid(16, 16, BLUE);
        let mut compositor = compositor();
        compositor
            .composite(16, 16, &[layer(16, 16, &red)])
            .unwrap();
        let pooled = compositor.layer_pool[&size][0].texture.clone();
        let frame = compositor
            .composite(16, 16, &[layer(16, 16, &blue)])
            .unwrap();
        assert_eq!(frame.bgra, blue);
        assert_eq!(compositor.layer_pool[&size].len(), 1);
        assert_eq!(compositor.layer_pool[&size][0].texture, pooled);

        compositor
            .composite(16, 16, &[layer(16, 16, &red), layer(16, 16, &blue)])
            .unwrap();
        assert_eq!(compositor.layer_pool[&size].len(), 2);
        assert!(
            compositor.layer_pool[&size]
                .iter()
                .any(|texture| texture.texture == pooled)
        );

        let green = solid(8, 8, GREEN);
        compositor
            .composite(16, 16, &[layer(8, 8, &green)])
            .unwrap();
        assert!(!compositor.layer_pool.contains_key(&size));
        assert_eq!(compositor.layer_pool.len(), 1);
    }

    #[test]
    fn invalid_sizes_and_lengths_are_errors() {
        let mut compositor = compositor();
        let green = solid(4, 4, GREEN);
        assert!(matches!(
            compositor.composite(0, 4, &[]),
            Err(Error::EmptySequence)
        ));
        assert!(matches!(
            compositor.composite(4, 0, &[]),
            Err(Error::EmptySequence)
        ));
        assert!(matches!(
            compositor.composite(4, 4, &[layer(4, 4, &green), layer(0, 4, &[])]),
            Err(Error::EmptyLayer { index: 1 })
        ));
        assert!(matches!(
            compositor.composite(4, 4, &[layer(4, 3, &green)]),
            Err(Error::LayerLength {
                index: 0,
                expected: 48,
                actual: 64,
            })
        ));
        assert!(matches!(
            compositor.composite(u32::MAX, 4, &[]),
            Err(Error::TooLarge { .. })
        ));
    }

    #[test]
    fn a_layer_moves_scales_and_fades_by_its_placement() {
        let green = solid(32, 32, GREEN);
        let mut compositor = compositor();
        let moved = Placement {
            offset: [0.25, 0.0],
            scale: 0.5,
            ..Placement::FIT
        };

        let frame = compositor
            .composite(32, 32, &[placed(layer(32, 32, &green), moved)])
            .unwrap();

        assert_eq!(pixel(&frame, 24, 16), GREEN);
        assert_eq!(pixel(&frame, 8, 16), OPAQUE_BLACK);
        assert_eq!(pixel(&frame, 24, 4), OPAQUE_BLACK);

        let faded = Placement {
            opacity: 0.5,
            ..Placement::FIT
        };
        let frame = compositor
            .composite(32, 32, &[placed(layer(32, 32, &green), faded)])
            .unwrap();

        assert_near(pixel(&frame, 16, 16), [0, 128, 0, 255]);
    }

    #[test]
    fn a_crop_removes_the_edges_where_they_were() {
        let halves: Vec<u8> = (0..2)
            .flat_map(|_| [RED, RED, BLUE, BLUE])
            .flatten()
            .collect();
        let cropped = Placement {
            crop: [0.5, 0.0, 0.0, 0.0],
            ..Placement::FIT
        };

        let frame = compositor()
            .composite(40, 20, &[placed(layer(4, 2, &halves), cropped)])
            .unwrap();

        assert_eq!(pixel(&frame, 5, 10), OPAQUE_BLACK);
        assert_eq!(pixel(&frame, 34, 10), BLUE);
    }

    #[test]
    fn a_quarter_turn_stands_a_wide_layer_on_its_end() {
        let green = solid(64, 16, GREEN);
        let turned = Placement {
            rotation_degrees: 90.0,
            ..Placement::FIT
        };

        let frame = compositor()
            .composite(64, 64, &[placed(layer(64, 16, &green), turned)])
            .unwrap();

        assert_eq!(pixel(&frame, 32, 4), GREEN);
        assert_eq!(pixel(&frame, 32, 60), GREEN);
        assert_eq!(pixel(&frame, 4, 32), OPAQUE_BLACK);
        assert_eq!(pixel(&frame, 60, 32), OPAQUE_BLACK);
    }
}
