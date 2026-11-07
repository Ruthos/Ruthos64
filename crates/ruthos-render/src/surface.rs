//! Segment surfaces in the 3D pane: triangle meshes drawn with depth into a
//! picture of their own, then shown as the volume's picture is.

use std::collections::HashMap;
use std::sync::Arc;

use egui_wgpu::{CallbackResources, CallbackTrait, ScreenDescriptor};

use crate::volume3d::{Camera, Placement};

type V3 = [f64; 3];

/// One corner of a triangle, ready for the GPU.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct SurfaceVertex {
    /// Patient millimetres.
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub colour: [u8; 4],
}

/// Every surface shown, in one buffer.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Surfaces {
    pub vertices: Vec<SurfaceVertex>,
    pub indices: Vec<u32>,
}

impl Surfaces {
    /// Add a mesh in patient millimetres, with its triangles facing out, in a
    /// colour. Each vertex's normal is the mean of its triangles'.
    pub fn add(&mut self, vertices: &[V3], triangles: &[[u32; 3]], colour: [u8; 3]) {
        let first = self.vertices.len() as u32;
        let mut normals = vec![[0.0f64; 3]; vertices.len()];
        for t in triangles {
            let [a, b, c] = t.map(|i| vertices[i as usize]);
            let (u, v) = (sub(b, a), sub(c, a));
            // Larger triangles count for more.
            let n = [
                u[1] * v[2] - u[2] * v[1],
                u[2] * v[0] - u[0] * v[2],
                u[0] * v[1] - u[1] * v[0],
            ];
            for &i in t {
                for (k, x) in normals[i as usize].iter_mut().enumerate() {
                    *x += n[k];
                }
            }
        }
        for (p, n) in vertices.iter().zip(normals) {
            let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt().max(1e-12);
            self.vertices.push(SurfaceVertex {
                position: p.map(|x| x as f32),
                normal: n.map(|x| (x / len) as f32),
                colour: [colour[0], colour[1], colour[2], 255],
            });
        }
        self.indices.extend(triangles.iter().flat_map(|t| t.map(|i| i + first)));
    }

    pub fn is_empty(&self) -> bool {
        self.indices.is_empty()
    }
}

fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

/// From patient millimetres to clip space, in columns: the view's frame,
/// scaled to its height and aspect, with depth over the volume's diagonal.
pub fn view_matrix(camera: &Camera, placement: &Placement, aspect: f64) -> [[f32; 4]; 4] {
    let forward = camera.forward();
    let depth = 2.0 * placement.diagonal();
    let near: V3 = std::array::from_fn(|i| camera.target[i] - forward[i] * placement.diagonal());
    let x = camera.right.map(|v| v / (camera.half_height * aspect));
    let y = camera.up.map(|v| v / camera.half_height);
    let z = forward.map(|v| v / depth);
    let dot = |a: V3, b: V3| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
    let row = |axis: V3, from: V3| [axis[0], axis[1], axis[2], -dot(axis, from)];
    let rows = [row(x, camera.target), row(y, camera.target), row(z, near)];
    std::array::from_fn(|c| {
        let mut col = [0.0f32; 4];
        for (r, values) in rows.iter().enumerate() {
            col[r] = values[c] as f32;
        }
        col[3] = if c == 3 { 1.0 } else { 0.0 };
        col
    })
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    m: [[f32; 4]; 4],
    forward: [f32; 4],
}

struct Target {
    size: [u32; 2],
    view: wgpu::TextureView,
    depth: wgpu::TextureView,
    bind_group: wgpu::BindGroup,
}

struct Draw {
    uniform: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    surfaces_ptr: usize,
    vertices: Option<wgpu::Buffer>,
    indices: Option<wgpu::Buffer>,
    count: u32,
    target: Option<Target>,
}

/// The pipelines for surfaces, kept in egui's callback resources.
pub struct SurfaceRenderer {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    blit: wgpu::RenderPipeline,
    blit_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    format: wgpu::TextureFormat,
    draws: HashMap<u64, Draw>,
}

const DEPTH: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

impl SurfaceRenderer {
    pub fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("ruthos surfaces"),
            source: wgpu::ShaderSource::Wgsl(include_str!("surface.wgsl").into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ruthos surfaces layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: std::num::NonZeroU64::new(std::mem::size_of::<Uniforms>() as u64),
                },
                count: None,
            }],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("ruthos surfaces pipeline layout"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("ruthos surfaces"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<SurfaceVertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Unorm8x4],
                })],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::Less),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
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
        let blit_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("ruthos surfaces picture"),
            source: wgpu::ShaderSource::Wgsl(include_str!("blit.wgsl").into()),
        });
        let blit_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ruthos surfaces picture layout"),
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
            ],
        });
        let blit_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("ruthos surfaces picture pipeline layout"),
            bind_group_layouts: &[Some(&blit_layout)],
            immediate_size: 0,
        });
        let blit = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("ruthos surfaces picture"),
            layout: Some(&blit_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &blit_shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &blit_shader,
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
            label: Some("ruthos surfaces sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Self {
            pipeline,
            layout,
            blit,
            blit_layout,
            sampler,
            format: target_format,
            draws: HashMap::new(),
        }
    }

    pub fn forget(&mut self, key: u64) {
        self.draws.remove(&key);
    }
}

/// The paint callback for surfaces in one 3D pane.
pub struct SurfaceCallback {
    pub key: u64,
    pub surfaces: Arc<Surfaces>,
    pub placement: Placement,
    pub camera: Camera,
    pub aspect: f64,
    /// The pane's size in physical pixels.
    pub size_px: [u32; 2],
}

impl CallbackTrait for SurfaceCallback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen: &ScreenDescriptor,
        encoder: &mut wgpu::CommandEncoder,
        resources: &mut CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let Some(r) = resources.get_mut::<SurfaceRenderer>() else {
            return Vec::new();
        };
        let d = r.draws.entry(self.key).or_insert_with(|| {
            let uniform = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("ruthos surfaces uniforms"),
                size: std::mem::size_of::<Uniforms>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("ruthos surfaces bind group"),
                layout: &r.layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform.as_entire_binding(),
                }],
            });
            Draw {
                uniform,
                bind_group,
                surfaces_ptr: 0,
                vertices: None,
                indices: None,
                count: 0,
                target: None,
            }
        });
        let ptr = Arc::as_ptr(&self.surfaces) as usize;
        if d.surfaces_ptr != ptr {
            d.surfaces_ptr = ptr;
            d.count = self.surfaces.indices.len() as u32;
            let buffer = |label, bytes: &[u8], usage| {
                (!bytes.is_empty()).then(|| {
                    let b = device.create_buffer(&wgpu::BufferDescriptor {
                        label: Some(label),
                        size: bytes.len() as u64,
                        usage: usage | wgpu::BufferUsages::COPY_DST,
                        mapped_at_creation: false,
                    });
                    queue.write_buffer(&b, 0, bytes);
                    b
                })
            };
            d.vertices = buffer(
                "ruthos surface vertices",
                bytemuck::cast_slice(&self.surfaces.vertices),
                wgpu::BufferUsages::VERTEX,
            );
            d.indices = buffer(
                "ruthos surface indices",
                bytemuck::cast_slice(&self.surfaces.indices),
                wgpu::BufferUsages::INDEX,
            );
        }
        let limit = device.limits().max_texture_dimension_2d;
        let size = self.size_px.map(|side| side.clamp(1, limit));
        if d.target.as_ref().is_none_or(|t| t.size != size) {
            let extent = wgpu::Extent3d {
                width: size[0],
                height: size[1],
                depth_or_array_layers: 1,
            };
            let texture = |label, format, usage| {
                device
                    .create_texture(&wgpu::TextureDescriptor {
                        label: Some(label),
                        size: extent,
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format,
                        usage,
                        view_formats: &[],
                    })
                    .create_view(&wgpu::TextureViewDescriptor::default())
            };
            let view = texture(
                "ruthos surfaces picture",
                r.format,
                wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            );
            let depth = texture("ruthos surfaces depth", DEPTH, wgpu::TextureUsages::RENDER_ATTACHMENT);
            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("ruthos surfaces picture bind group"),
                layout: &r.blit_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&r.sampler),
                    },
                ],
            });
            d.target = Some(Target {
                size,
                view,
                depth,
                bind_group,
            });
        }
        let forward = self.camera.forward();
        let uniforms = Uniforms {
            m: view_matrix(&self.camera, &self.placement, self.aspect),
            forward: [forward[0] as f32, forward[1] as f32, forward[2] as f32, 0.0],
        };
        queue.write_buffer(&d.uniform, 0, bytemuck::bytes_of(&uniforms));
        let Some(target) = d.target.as_ref() else {
            return Vec::new();
        };
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("ruthos surfaces"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &target.view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &target.depth,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(1.0),
                    store: wgpu::StoreOp::Discard,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        if let (Some(vertices), Some(indices)) = (&d.vertices, &d.indices) {
            pass.set_pipeline(&r.pipeline);
            pass.set_bind_group(0, &d.bind_group, &[]);
            pass.set_vertex_buffer(0, vertices.slice(..));
            pass.set_index_buffer(indices.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..d.count, 0, 0..1);
        }
        Vec::new()
    }

    fn paint(
        &self,
        _info: egui::PaintCallbackInfo,
        render_pass: &mut wgpu::RenderPass<'static>,
        resources: &CallbackResources,
    ) {
        let Some(r) = resources.get::<SurfaceRenderer>() else {
            return;
        };
        let Some(target) = r.draws.get(&self.key).and_then(|d| d.target.as_ref()) else {
            return;
        };
        render_pass.set_pipeline(&r.blit);
        render_pass.set_bind_group(0, &target.bind_group, &[]);
        render_pass.draw(0..3, 0..1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shader_is_valid() {
        let module = naga::front::wgsl::parse_str(include_str!("surface.wgsl")).expect("parses");
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
            .validate(&module)
            .expect("validates");
    }

    #[test]
    fn the_view_puts_the_target_in_the_middle_and_the_volume_in_depth() {
        let placement = Placement {
            dims: [100, 100, 100],
            spacing: [1.0; 3],
            origin: [0.0; 3],
            axes: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        };
        let camera = Camera::framing(&placement);
        let m = view_matrix(&camera, &placement, 1.5);
        let apply = |p: V3| -> [f32; 4] {
            std::array::from_fn(|r| (0..4).map(|c| m[c][r] * if c == 3 { 1.0 } else { p[c] as f32 }).sum())
        };
        let centre = apply(camera.target);
        assert!(centre[0].abs() < 1e-5 && centre[1].abs() < 1e-5, "{centre:?}");
        assert!(
            (centre[2] - 0.5).abs() < 1e-5,
            "the target is half way through: {centre:?}"
        );
        // A point to the right and up lands where `project` says.
        let p: V3 = std::array::from_fn(|i| camera.target[i] + camera.right[i] * 10.0 + camera.up[i] * 5.0);
        let on_screen = camera.project(p, 1.5);
        let clip = apply(p);
        assert!((clip[0] as f64 - on_screen[0]).abs() < 1e-5 && (clip[1] as f64 - on_screen[1]).abs() < 1e-5);
        // Nearer the viewer is less deep.
        let nearer: V3 = std::array::from_fn(|i| camera.target[i] - camera.forward()[i] * 20.0);
        assert!(apply(nearer)[2] < centre[2]);
    }

    #[test]
    fn a_surface_gets_normals_that_point_out() {
        let mut s = Surfaces::default();
        // A triangle in the z = 0 plane, anticlockwise from above.
        s.add(
            &[[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            &[[0, 1, 2]],
            [10, 20, 30],
        );
        s.add(
            &[[0.0, 0.0, 1.0], [1.0, 0.0, 1.0], [0.0, 1.0, 1.0]],
            &[[0, 1, 2]],
            [1, 2, 3],
        );
        assert_eq!(s.vertices[0].normal, [0.0, 0.0, 1.0]);
        assert_eq!(s.indices, vec![0, 1, 2, 3, 4, 5]);
        assert_eq!(s.vertices[4].colour, [1, 2, 3, 255]);
    }
}
