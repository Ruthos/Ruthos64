//! Volume rendering: the 3D textures, the camera, transfer functions and the
//! egui callback for the ray marcher in `raymarch.wgsl`.
//!
//! All the geometry is here in plain numbers, so it can be tested without a
//! GPU and the interface can map a click to a point in the volume.

use std::collections::HashMap;
use std::num::NonZeroU64;
use std::sync::Arc;

use egui_wgpu::wgpu;
use egui_wgpu::{CallbackResources, CallbackTrait, ScreenDescriptor};

/// The transfer function's and the segment table's widths.
pub const TRANSFER_WIDTH: usize = 1024;
pub const SEGMENT_WIDTH: usize = 256;

/// A point of a transfer function: at this value, this colour and opacity.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Stop {
    pub value: f32,
    pub rgba: [f32; 4],
}

/// Maps each value to a colour and an opacity.
#[derive(Debug, Clone, PartialEq)]
pub struct TransferFunction {
    pub name: String,
    pub stops: Vec<Stop>,
}

impl TransferFunction {
    /// The function moved by `shift` and stretched by `scale` about its
    /// middle, which is what dragging the window does.
    pub fn windowed(&self, shift: f32, scale: f32) -> Self {
        let middle = match (self.stops.first(), self.stops.last()) {
            (Some(a), Some(b)) => (a.value + b.value) / 2.0,
            _ => 0.0,
        };
        Self {
            name: self.name.clone(),
            stops: self
                .stops
                .iter()
                .map(|s| Stop {
                    value: middle + (s.value - middle) * scale + shift,
                    rgba: s.rgba,
                })
                .collect(),
        }
    }

    pub fn new(name: &str, stops: &[(f32, [f32; 4])]) -> Self {
        Self {
            name: name.to_string(),
            stops: stops
                .iter()
                .map(|(value, rgba)| Stop {
                    value: *value,
                    rgba: *rgba,
                })
                .collect(),
        }
    }

    pub fn at(&self, value: f32) -> [f32; 4] {
        let Some(first) = self.stops.first() else {
            return [0.0; 4];
        };
        if value <= first.value {
            return first.rgba;
        }
        for pair in self.stops.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            if value <= b.value {
                let t = (value - a.value) / (b.value - a.value).max(1e-6);
                return std::array::from_fn(|i| a.rgba[i] + (b.rgba[i] - a.rgba[i]) * t);
            }
        }
        self.stops.last().map_or([0.0; 4], |s| s.rgba)
    }

    /// The function over a volume's range, as the shader reads it.
    pub fn bake(&self, min: f32, max: f32) -> Vec<u8> {
        let mut out = Vec::with_capacity(TRANSFER_WIDTH * 4);
        for i in 0..TRANSFER_WIDTH {
            let value = min + (max - min) * i as f32 / (TRANSFER_WIDTH - 1) as f32;
            for c in self.at(value) {
                out.push((c.clamp(0.0, 1.0) * 255.0).round() as u8);
            }
        }
        out
    }

    /// The presets, in Hounsfield units. Opacity ramps follow the attenuation
    /// of tissue, so they suit CT; a user adjusts them on the histogram.
    pub fn presets() -> Vec<TransferFunction> {
        const NONE: [f32; 4] = [0.0, 0.0, 0.0, 0.0];
        let bone = [0.93, 0.89, 0.80];
        vec![
            TransferFunction::new(
                "Bones",
                &[
                    (120.0, NONE),
                    (250.0, [0.72, 0.52, 0.36, 0.12]),
                    (500.0, [bone[0], bone[1], bone[2], 0.75]),
                    (1500.0, [1.0, 1.0, 0.97, 0.95]),
                ],
            ),
            TransferFunction::new(
                "Muscles and bones",
                &[
                    (-30.0, NONE),
                    (40.0, [0.68, 0.24, 0.20, 0.10]),
                    (120.0, [0.80, 0.42, 0.32, 0.14]),
                    (250.0, [0.85, 0.72, 0.58, 0.40]),
                    (500.0, [bone[0], bone[1], bone[2], 0.85]),
                ],
            ),
            TransferFunction::new(
                "Skin",
                &[
                    (-500.0, NONE),
                    (-200.0, [0.86, 0.66, 0.54, 0.45]),
                    (100.0, [0.90, 0.72, 0.60, 0.60]),
                    (500.0, [bone[0], bone[1], bone[2], 0.90]),
                ],
            ),
            TransferFunction::new(
                "Contrast",
                &[
                    (130.0, NONE),
                    (200.0, [0.75, 0.15, 0.12, 0.30]),
                    (400.0, [0.95, 0.35, 0.22, 0.80]),
                    (900.0, [1.0, 0.95, 0.85, 0.95]),
                ],
            ),
            // Greys for the projections, which use no opacity.
            TransferFunction::new(
                "Grey",
                &[(-1000.0, [0.0, 0.0, 0.0, 1.0]), (2000.0, [1.0, 1.0, 1.0, 1.0])],
            ),
        ]
    }
}

/// How rays are combined.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderMode {
    Shaded,
    Maximum,
    Minimum,
}

impl RenderMode {
    fn code(self) -> f32 {
        match self {
            RenderMode::Shaded => 0.0,
            RenderMode::Maximum => 1.0,
            RenderMode::Minimum => 2.0,
        }
    }
}

type V3 = [f64; 3];

fn dot(a: V3, b: V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: V3, b: V3) -> V3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn normalised(a: V3) -> V3 {
    let n = dot(a, a).sqrt().max(1e-12);
    [a[0] / n, a[1] / n, a[2] / n]
}

fn turn(v: V3, axis: V3, angle: f64) -> V3 {
    let (s, c) = angle.sin_cos();
    let k = dot(axis, v) * (1.0 - c);
    let x = cross(axis, v);
    std::array::from_fn(|i| v[i] * c + x[i] * s + axis[i] * k)
}

/// A volume's place in patient space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    pub dims: [usize; 3],
    pub spacing: V3,
    pub origin: V3,
    pub axes: [V3; 3],
}

impl Placement {
    pub fn centre(&self) -> V3 {
        let mut c = self.origin;
        for a in 0..3 {
            let d = (self.dims[a] as f64 - 1.0) / 2.0 * self.spacing[a];
            for (out, axis) in c.iter_mut().zip(self.axes[a]) {
                *out += axis * d;
            }
        }
        c
    }

    pub fn diagonal(&self) -> f64 {
        (0..3)
            .map(|a| (self.dims[a] as f64 * self.spacing[a]).powi(2))
            .sum::<f64>()
            .sqrt()
    }

    /// Texture coordinates of a patient point.
    fn texture_of(&self, p: V3) -> V3 {
        let d: V3 = std::array::from_fn(|i| p[i] - self.origin[i]);
        std::array::from_fn(|a| (dot(d, self.axes[a]) / self.spacing[a] + 0.5) / self.dims[a] as f64)
    }

    /// Texture coordinates per unit along a patient direction.
    fn texture_direction(&self, v: V3) -> V3 {
        std::array::from_fn(|a| dot(v, self.axes[a]) / self.spacing[a] / self.dims[a] as f64)
    }
}

/// An orbit around a point, in patient space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Camera {
    pub target: V3,
    pub right: V3,
    pub up: V3,
    /// Millimetres from the middle of the view to its top.
    pub half_height: f64,
}

impl Camera {
    /// Looking at the whole volume from the front, patient head up.
    pub fn framing(p: &Placement) -> Self {
        Self {
            target: p.centre(),
            // DICOM's axes: x to the patient's left, y to the back, z to the head.
            right: [-1.0, 0.0, 0.0],
            up: [0.0, 0.0, 1.0],
            half_height: p.diagonal() / 2.0,
        }
    }

    pub fn forward(&self) -> V3 {
        normalised(cross(self.right, self.up))
    }

    /// Orbit by a drag, in radians across and up the view.
    pub fn orbit(&mut self, across: f64, up: f64) {
        let (right, top) = (self.right, self.up);
        self.right = normalised(turn(turn(right, top, across), right, up));
        self.up = normalised(turn(turn(top, top, across), right, up));
        // Keep the frame square against rounding.
        self.up = normalised(cross(cross(self.right, self.up), self.right));
    }

    /// Move the view by a drag, as a share of its height.
    pub fn slide(&mut self, across: f64, up: f64) {
        for i in 0..3 {
            self.target[i] -= (self.right[i] * across + self.up[i] * up) * self.half_height;
        }
    }

    /// Where a patient point falls on screen, in -1 to 1 across the view.
    pub fn project(&self, p: V3, aspect: f64) -> [f64; 2] {
        let d: V3 = std::array::from_fn(|i| p[i] - self.target[i]);
        [
            dot(d, self.right) / (self.half_height * aspect),
            dot(d, self.up) / self.half_height,
        ]
    }

    pub fn zoom(&mut self, factor: f64) {
        self.half_height = (self.half_height * factor).clamp(1.0, 1e5);
    }

    /// The point at a place on screen, at the depth of the target.
    pub fn point_at(&self, ndc: [f64; 2], aspect: f64) -> V3 {
        std::array::from_fn(|i| {
            self.target[i] + self.right[i] * ndc[0] * self.half_height * aspect + self.up[i] * ndc[1] * self.half_height
        })
    }

    /// The matrix `raymarch.wgsl` uses, in columns. The ray's depth starts
    /// behind the volume's whole diagonal, so depth is never negative there.
    pub fn matrix(&self, p: &Placement, aspect: f64) -> [[f32; 4]; 4] {
        let forward = self.forward();
        let scale = |v: V3, s: f64| -> V3 { std::array::from_fn(|i| v[i] * s) };
        let origin: V3 = std::array::from_fn(|i| self.target[i] - forward[i] * p.diagonal());
        let x = p.texture_direction(scale(self.right, self.half_height * aspect));
        let y = p.texture_direction(scale(self.up, self.half_height));
        let z = p.texture_direction(forward);
        let o = p.texture_of(origin);
        let column = |v: V3, w: f32| [v[0] as f32, v[1] as f32, v[2] as f32, w];
        [column(x, 0.0), column(y, 0.0), column(z, 0.0), column(o, 1.0)]
    }
}

/// The largest texture a volume is uploaded at.
fn stride_for(dims: [usize; 3], limit: u32, max_bytes: u64) -> usize {
    let mut stride = 1;
    loop {
        let side = dims.iter().map(|d| d / stride).max().unwrap_or(0);
        let bytes: u64 = dims.iter().map(|d| (d / stride).max(1) as u64).product::<u64>() * 2;
        if side as u32 <= limit && bytes <= max_bytes {
            return stride;
        }
        stride += 1;
    }
}

fn create(
    device: &wgpu::Device,
    label: &str,
    size: wgpu::Extent3d,
    dim: wgpu::TextureDimension,
    format: wgpu::TextureFormat,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: dim,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

/// A volume and its labels on the GPU.
pub struct GpuVolume {
    pub view: wgpu::TextureView,
    label_texture: wgpu::Texture,
    pub label_view: wgpu::TextureView,
    /// Voxels on the GPU, which a very large volume may reduce.
    pub dims: [u32; 3],
    /// How many source voxels to one on the GPU.
    pub stride: usize,
    pub spacing: V3,
    /// The value range the texture's 0 to 1 stands for.
    pub min: f32,
    pub max: f32,
}

impl GpuVolume {
    /// Upload values, read through `at(x, y, z)`. A volume beyond the device's
    /// limits or `max_bytes` is reduced by a whole number of voxels.
    pub fn upload(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        placement: &Placement,
        range: (f32, f32),
        max_bytes: u64,
        at: impl Fn(usize, usize, usize) -> f32,
    ) -> Self {
        let limit = device.limits().max_texture_dimension_3d;
        let stride = stride_for(placement.dims, limit, max_bytes);
        let dims: [u32; 3] = std::array::from_fn(|a| (placement.dims[a] / stride).max(1) as u32);
        let size = wgpu::Extent3d {
            width: dims[0],
            height: dims[1],
            depth_or_array_layers: dims[2],
        };
        let texture = create(
            device,
            "ruthos volume",
            size,
            wgpu::TextureDimension::D3,
            wgpu::TextureFormat::R16Float,
        );
        let (low, span) = (range.0, (range.1 - range.0).max(1e-6));
        let mut row: Vec<u16> = Vec::with_capacity((dims[0] * dims[1]) as usize);
        let layer = wgpu::Extent3d {
            width: dims[0],
            height: dims[1],
            depth_or_array_layers: 1,
        };
        for z in 0..dims[2] {
            row.clear();
            for y in 0..dims[1] {
                for x in 0..dims[0] {
                    let v = at(x as usize * stride, y as usize * stride, z as usize * stride);
                    row.push(half(((v - low) / span).clamp(0.0, 1.0)));
                }
            }
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d { x: 0, y: 0, z },
                    aspect: wgpu::TextureAspect::All,
                },
                bytemuck::cast_slice(&row),
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(dims[0] * 2),
                    rows_per_image: Some(dims[1]),
                },
                layer,
            );
        }
        let label_texture = create(
            device,
            "ruthos labels",
            size,
            wgpu::TextureDimension::D3,
            wgpu::TextureFormat::R8Uint,
        );
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let label_view = label_texture.create_view(&wgpu::TextureViewDescriptor::default());
        Self {
            view,
            label_texture,
            label_view,
            dims,
            stride,
            spacing: std::array::from_fn(|a| placement.spacing[a] * stride as f64),
            min: range.0,
            max: range.1,
        }
    }

    /// Replace the labels, given on the source grid.
    pub fn write_labels(&self, queue: &wgpu::Queue, source_dims: [usize; 3], labels: &[u8]) {
        let [w, h, d] = self.dims;
        let mut layer = Vec::with_capacity((w * h) as usize);
        for z in 0..d as usize {
            layer.clear();
            for y in 0..h as usize {
                for x in 0..w as usize {
                    let s = self.stride;
                    layer.push(labels[x * s + source_dims[0] * (y * s + source_dims[1] * z * s)]);
                }
            }
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &self.label_texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: 0,
                        y: 0,
                        z: z as u32,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                &layer,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(w),
                    rows_per_image: Some(h),
                },
                wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
            );
        }
    }
}

/// A float in 0 to 1 as a 16-bit float, which is filterable on every GPU,
/// unlike the 16-bit integer formats.
fn half(v: f32) -> u16 {
    let bits = v.to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    let exponent = ((bits >> 23) & 0xff) as i32 - 127 + 15;
    let mantissa = bits & 0x7f_ffff;
    if exponent <= 0 {
        if exponent < -10 {
            return sign;
        }
        let m = (mantissa | 0x80_0000) >> (1 - exponent);
        return sign | (m >> 13) as u16;
    }
    if exponent >= 31 {
        return sign | 0x7c00;
    }
    sign | ((exponent as u16) << 10) | (mantissa >> 13) as u16
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    m: [[f32; 4]; 4],
    params: [f32; 4],
    dims: [f32; 4],
    spacing: [f32; 4],
}

struct Draw {
    uniform: wgpu::Buffer,
    transfer: wgpu::Texture,
    segments: wgpu::Texture,
    bind_group: wgpu::BindGroup,
    volume_ptr: usize,
    transfer_ptr: usize,
    segments_ptr: usize,
}

/// The ray marcher's pipeline, stored once in egui-wgpu's callback resources.
pub struct VolumeRenderer {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    draws: HashMap<u64, Draw>,
}

impl VolumeRenderer {
    pub fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("ruthos ray marcher"),
            source: wgpu::ShaderSource::Wgsl(include_str!("raymarch.wgsl").into()),
        });
        let texture = |binding, sample_type, view_dimension| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type,
                view_dimension,
                multisampled: false,
            },
            count: None,
        };
        let filterable = wgpu::TextureSampleType::Float { filterable: true };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ruthos ray marcher layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: NonZeroU64::new(std::mem::size_of::<Uniforms>() as u64),
                    },
                    count: None,
                },
                texture(1, filterable, wgpu::TextureViewDimension::D3),
                texture(2, wgpu::TextureSampleType::Uint, wgpu::TextureViewDimension::D3),
                texture(3, filterable, wgpu::TextureViewDimension::D2),
                texture(4, filterable, wgpu::TextureViewDimension::D2),
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("ruthos ray marcher pipeline layout"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("ruthos ray marcher"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: target_format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("ruthos volume sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Self {
            pipeline,
            layout,
            sampler,
            draws: HashMap::new(),
        }
    }

    pub fn forget(&mut self, key: u64) {
        self.draws.remove(&key);
    }
}

fn write_row(queue: &wgpu::Queue, texture: &wgpu::Texture, bytes: &[u8], width: u32) {
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        bytes,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(width * 4),
            rows_per_image: Some(1),
        },
        wgpu::Extent3d {
            width,
            height: 1,
            depth_or_array_layers: 1,
        },
    );
}

/// The paint callback for one 3D view.
pub struct VolumeCallback {
    pub key: u64,
    pub volume: Arc<GpuVolume>,
    /// From `TransferFunction::bake`.
    pub transfer: Arc<Vec<u8>>,
    /// 256 RGBA entries by label: colour, and alpha 0 to hide.
    pub segments: Arc<Vec<u8>>,
    pub use_segments: bool,
    pub placement: Placement,
    pub camera: Camera,
    pub aspect: f64,
    pub mode: RenderMode,
    /// A step in millimetres: coarser while the user rotates.
    pub step_mm: f32,
}

impl CallbackTrait for VolumeCallback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen: &ScreenDescriptor,
        _encoder: &mut wgpu::CommandEncoder,
        resources: &mut CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let Some(r) = resources.get_mut::<VolumeRenderer>() else {
            return Vec::new();
        };
        let volume_ptr = Arc::as_ptr(&self.volume) as usize;
        if r.draws.get(&self.key).is_none_or(|d| d.volume_ptr != volume_ptr) {
            let row = |width: usize| wgpu::Extent3d {
                width: width as u32,
                height: 1,
                depth_or_array_layers: 1,
            };
            let transfer = create(
                device,
                "ruthos transfer",
                row(TRANSFER_WIDTH),
                wgpu::TextureDimension::D2,
                wgpu::TextureFormat::Rgba8Unorm,
            );
            let segments = create(
                device,
                "ruthos segments",
                row(SEGMENT_WIDTH),
                wgpu::TextureDimension::D2,
                wgpu::TextureFormat::Rgba8Unorm,
            );
            let uniform = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("ruthos ray marcher uniforms"),
                size: std::mem::size_of::<Uniforms>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let transfer_view = transfer.create_view(&wgpu::TextureViewDescriptor::default());
            let segments_view = segments.create_view(&wgpu::TextureViewDescriptor::default());
            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("ruthos ray marcher bind group"),
                layout: &r.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: uniform.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&self.volume.view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(&self.volume.label_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::TextureView(&transfer_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: wgpu::BindingResource::TextureView(&segments_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 5,
                        resource: wgpu::BindingResource::Sampler(&r.sampler),
                    },
                ],
            });
            r.draws.insert(
                self.key,
                Draw {
                    uniform,
                    transfer,
                    segments,
                    bind_group,
                    volume_ptr,
                    transfer_ptr: 0,
                    segments_ptr: 0,
                },
            );
        }
        let Some(d) = r.draws.get_mut(&self.key) else {
            return Vec::new();
        };
        let transfer_ptr = Arc::as_ptr(&self.transfer) as usize;
        if d.transfer_ptr != transfer_ptr {
            write_row(queue, &d.transfer, &self.transfer, TRANSFER_WIDTH as u32);
            d.transfer_ptr = transfer_ptr;
        }
        let segments_ptr = Arc::as_ptr(&self.segments) as usize;
        if d.segments_ptr != segments_ptr {
            write_row(queue, &d.segments, &self.segments, SEGMENT_WIDTH as u32);
            d.segments_ptr = segments_ptr;
        }
        let dims = self.volume.dims;
        let spacing = self.volume.spacing;
        let uniforms = Uniforms {
            m: self.camera.matrix(&self.placement, self.aspect),
            params: [
                self.step_mm,
                self.mode.code(),
                if self.use_segments { 1.0 } else { 0.0 },
                // Opacities are as seen at about half a voxel.
                spacing[0].min(spacing[1]).min(spacing[2]) as f32 * 0.5,
            ],
            dims: [dims[0] as f32, dims[1] as f32, dims[2] as f32, 0.0],
            spacing: [spacing[0] as f32, spacing[1] as f32, spacing[2] as f32, 0.0],
        };
        queue.write_buffer(&d.uniform, 0, bytemuck::bytes_of(&uniforms));
        Vec::new()
    }

    fn paint(
        &self,
        _info: egui::PaintCallbackInfo,
        render_pass: &mut wgpu::RenderPass<'static>,
        resources: &CallbackResources,
    ) {
        let Some(r) = resources.get::<VolumeRenderer>() else {
            return;
        };
        let Some(d) = r.draws.get(&self.key) else {
            return;
        };
        render_pass.set_pipeline(&r.pipeline);
        render_pass.set_bind_group(0, &d.bind_group, &[]);
        render_pass.draw(0..3, 0..1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn placement() -> Placement {
        Placement {
            dims: [100, 80, 50],
            spacing: [0.5, 0.5, 2.0],
            origin: [-20.0, 10.0, 5.0],
            axes: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        }
    }

    fn apply(m: [[f32; 4]; 4], v: [f32; 4]) -> [f32; 3] {
        std::array::from_fn(|r| (0..4).map(|c| m[c][r] * v[c]).sum())
    }

    #[test]
    fn the_middle_ray_goes_through_the_middle_of_the_volume() {
        let p = placement();
        let cam = Camera::framing(&p);
        let m = cam.matrix(&p, 1.5);
        // At a depth of one diagonal the ray reaches the target.
        let at = apply(m, [0.0, 0.0, p.diagonal() as f32, 1.0]);
        for c in at {
            assert!((c - 0.5).abs() < 1e-3, "{at:?}");
        }
    }

    #[test]
    fn the_framing_shows_the_whole_volume_from_any_side() {
        let p = placement();
        let mut cam = Camera::framing(&p);
        cam.orbit(0.7, 0.4);
        let m = cam.matrix(&p, 1.0);
        // A corner of the volume projects inside the view.
        let c = p.centre();
        let half: V3 = std::array::from_fn(|a| p.dims[a] as f64 * p.spacing[a] / 2.0);
        let corner: V3 = std::array::from_fn(|i| c[i] + half[i]);
        let d: V3 = std::array::from_fn(|i| corner[i] - c[i]);
        assert!(dot(d, cam.right).abs() <= cam.half_height);
        assert!(dot(d, cam.up).abs() <= cam.half_height);
        let _ = m;
    }

    #[test]
    fn a_point_projects_back_to_where_a_click_found_it() {
        let p = placement();
        let mut cam = Camera::framing(&p);
        cam.orbit(0.5, -0.3);
        cam.slide(0.1, 0.2);
        let aspect = 1.6;
        let back = cam.project(cam.point_at([0.25, -0.4], aspect), aspect);
        assert!(
            (back[0] - 0.25).abs() < 1e-9 && (back[1] + 0.4).abs() < 1e-9,
            "{back:?}"
        );
    }

    #[test]
    fn windowing_moves_and_stretches_the_stops() {
        let t = TransferFunction::new("t", &[(0.0, [0.0; 4]), (100.0, [1.0; 4])]);
        let w = t.windowed(10.0, 2.0);
        assert_eq!((w.stops[0].value, w.stops[1].value), (-40.0, 160.0));
        assert_eq!(w.stops[1].rgba, [1.0; 4]);
    }

    #[test]
    fn orbiting_keeps_the_frame_square() {
        let p = placement();
        let mut cam = Camera::framing(&p);
        for i in 0..200 {
            cam.orbit(0.13, if i % 2 == 0 { 0.07 } else { -0.05 });
        }
        assert!((dot(cam.right, cam.right) - 1.0).abs() < 1e-9);
        assert!((dot(cam.up, cam.up) - 1.0).abs() < 1e-9);
        assert!(dot(cam.right, cam.up).abs() < 1e-9);
    }

    #[test]
    fn a_click_maps_back_to_the_patient() {
        let p = placement();
        let cam = Camera::framing(&p);
        let up = cam.point_at([0.0, 1.0], 1.0);
        assert!(
            (up[2] - cam.target[2] - cam.half_height).abs() < 1e-9,
            "the top of the view is towards the head"
        );
    }

    #[test]
    fn transfer_functions_interpolate_and_clamp() {
        let t = TransferFunction::new("t", &[(0.0, [0.0; 4]), (100.0, [1.0; 4])]);
        assert_eq!(t.at(-5.0), [0.0; 4]);
        assert_eq!(t.at(50.0), [0.5; 4]);
        assert_eq!(t.at(500.0), [1.0; 4]);
        let baked = t.bake(0.0, 100.0);
        assert_eq!(baked.len(), TRANSFER_WIDTH * 4);
        assert_eq!((baked[0], baked[TRANSFER_WIDTH * 4 - 1]), (0, 255));
    }

    #[test]
    fn half_floats_are_right() {
        assert_eq!(half(0.0), 0);
        assert_eq!(half(1.0), 0x3c00);
        assert_eq!(half(0.5), 0x3800);
        assert_eq!(half(0.25), 0x3400);
    }

    #[test]
    fn a_large_volume_is_reduced_to_fit() {
        assert_eq!(stride_for([512, 512, 600], 2048, 1 << 30), 1);
        assert_eq!(stride_for([512, 512, 600], 2048, 100 << 20), 2);
        assert_eq!(stride_for([4096, 100, 100], 2048, 1 << 40), 2);
    }
}
