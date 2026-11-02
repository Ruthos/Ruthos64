//! wgpu resources: frame textures, the image pipeline and the egui callback.

use std::collections::HashMap;
use std::num::NonZeroU64;
use std::sync::Arc;

use egui_wgpu::wgpu;
use egui_wgpu::{CallbackResources, CallbackTrait, ScreenDescriptor};
use ruthos_dicom::clut::Clut;
use ruthos_dicom::pixels::{Frame, FramePixels};

use crate::view::ViewTransform;

/// Texture sampling mode for the image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Interpolation {
    Nearest,
    Bilinear,
}

/// One frame uploaded to the GPU.
pub struct GpuImage {
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    pub width: u32,
    pub height: u32,
    pub is_color: bool,
    /// Downsampling factor applied when the frame exceeded the device limit.
    pub downsample: u32,
}

impl GpuImage {
    /// Upload a decoded frame. Frames larger than the device's maximum
    /// texture size are box-downsampled by an integer factor.
    pub fn upload(device: &wgpu::Device, queue: &wgpu::Queue, frame: &Frame) -> Self {
        let max_dim = device.limits().max_texture_dimension_2d.max(1024);
        let mut factor = 1u32;
        while frame.width / factor > max_dim || frame.height / factor > max_dim {
            factor *= 2;
        }
        let w = (frame.width / factor).max(1);
        let h = (frame.height / factor).max(1);

        let (format, bytes, bpp, is_color) = match &frame.pixels {
            FramePixels::Gray(values) => {
                let data: Vec<f32> = if factor == 1 {
                    values.clone()
                } else {
                    downsample_f32(values, frame.width, frame.height, factor, w, h)
                };
                (wgpu::TextureFormat::R32Float, bytemuck::cast_slice::<f32, u8>(&data).to_vec(), 4u32, false)
            }
            FramePixels::Rgba(rgba) => {
                let data = if factor == 1 {
                    rgba.clone()
                } else {
                    downsample_rgba(rgba, frame.width, frame.height, factor, w, h)
                };
                (wgpu::TextureFormat::Rgba8Unorm, data, 4u32, true)
            }
        };

        let size = wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        };
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("ruthos frame"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &bytes,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(w * bpp),
                rows_per_image: Some(h),
            },
            size,
        );
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Self {
            texture,
            view,
            width: w,
            height: h,
            is_color,
            downsample: factor,
        }
    }
}

fn downsample_f32(src: &[f32], sw: u32, sh: u32, f: u32, dw: u32, dh: u32) -> Vec<f32> {
    let mut out = vec![0.0f32; (dw * dh) as usize];
    for y in 0..dh {
        for x in 0..dw {
            let mut acc = 0.0f32;
            let mut n = 0u32;
            for dy in 0..f {
                for dx in 0..f {
                    let sx = x * f + dx;
                    let sy = y * f + dy;
                    if sx < sw && sy < sh {
                        acc += src[(sy * sw + sx) as usize];
                        n += 1;
                    }
                }
            }
            out[(y * dw + x) as usize] = if n > 0 { acc / n as f32 } else { 0.0 };
        }
    }
    out
}

fn downsample_rgba(src: &[u8], sw: u32, sh: u32, f: u32, dw: u32, dh: u32) -> Vec<u8> {
    let mut out = vec![0u8; (dw * dh * 4) as usize];
    for y in 0..dh {
        for x in 0..dw {
            let mut acc = [0u32; 4];
            let mut n = 0u32;
            for dy in 0..f {
                for dx in 0..f {
                    let sx = x * f + dx;
                    let sy = y * f + dy;
                    if sx < sw && sy < sh {
                        let i = ((sy * sw + sx) * 4) as usize;
                        for c in 0..4 {
                            acc[c] += src[i + c] as u32;
                        }
                        n += 1;
                    }
                }
            }
            let o = ((y * dw + x) * 4) as usize;
            for c in 0..4 {
                out[o + c] = if n > 0 { (acc[c] / n) as u8 } else { 0 };
            }
        }
    }
    out
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    m: [[f32; 4]; 4],
    wl: [f32; 4],
    size: [f32; 4],
}

struct ClutTexture {
    _texture: wgpu::Texture,
    view: wgpu::TextureView,
}

struct DrawResources {
    uniform: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    image_ptr: usize,
    clut_name: String,
}

/// Shared pipeline state, stored once in egui-wgpu's callback resources.
pub struct ImageRenderer {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    cluts: HashMap<String, ClutTexture>,
    draws: HashMap<u64, DrawResources>,
}

impl ImageRenderer {
    pub fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("ruthos image shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("image.wgsl").into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ruthos image bind group layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: NonZeroU64::new(std::mem::size_of::<Uniforms>() as u64),
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("ruthos image pipeline layout"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("ruthos image pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                unclipped_depth: false,
                polygon_mode: wgpu::PolygonMode::Fill,
                conservative: false,
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState {
                count: 1,
                mask: !0,
                alpha_to_coverage_enabled: false,
            },
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
            label: Some("ruthos clut sampler"),
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
            cluts: HashMap::new(),
            draws: HashMap::new(),
        }
    }

    fn clut_view(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, clut: &Clut) -> &wgpu::TextureView {
        if !self.cluts.contains_key(&clut.name) {
            let size = wgpu::Extent3d {
                width: 256,
                height: 1,
                depth_or_array_layers: 1,
            };
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("ruthos clut"),
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &clut.to_rgba_bytes(),
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(256 * 4),
                    rows_per_image: Some(1),
                },
                size,
            );
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            self.cluts.insert(clut.name.clone(), ClutTexture { _texture: texture, view });
        }
        &self.cluts[&clut.name].view
    }

    /// Drop per-view resources for a view that no longer exists.
    pub fn forget(&mut self, key: u64) {
        self.draws.remove(&key);
    }
}

/// The paint callback for one image view. Create one per frame draw.
pub struct ImageCallback {
    /// Stable identifier of the view (so GPU resources are reused frame to frame).
    pub key: u64,
    pub image: Arc<GpuImage>,
    pub clut: Arc<Clut>,
    pub transform: ViewTransform,
    pub inverted: bool,
    pub interpolation: Interpolation,
}

impl ImageCallback {
    fn uniforms(&self) -> Uniforms {
        // The GPU texture may be downsampled relative to the frame the
        // transform was built for; scale pixel coordinates accordingly.
        let mut t = self.transform;
        let f = self.image.downsample as f32;
        if f > 1.0 {
            t.image_w = self.image.width;
            t.image_h = self.image.height;
            t.state.zoom *= f;
        }
        Uniforms {
            m: t.image_to_clip_matrix(),
            wl: [
                self.transform.state.window_center,
                self.transform.state.window_width,
                if self.inverted { 1.0 } else { 0.0 },
                if self.image.is_color { 1.0 } else { 0.0 },
            ],
            size: [
                self.image.width as f32,
                self.image.height as f32,
                if self.interpolation == Interpolation::Bilinear { 1.0 } else { 0.0 },
                0.0,
            ],
        }
    }
}

impl CallbackTrait for ImageCallback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen: &ScreenDescriptor,
        _encoder: &mut wgpu::CommandEncoder,
        resources: &mut CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let Some(renderer) = resources.get_mut::<ImageRenderer>() else {
            return Vec::new();
        };
        let uniforms = self.uniforms();
        let image_ptr = Arc::as_ptr(&self.image) as usize;

        let needs_bind_group = match renderer.draws.get(&self.key) {
            Some(d) => d.image_ptr != image_ptr || d.clut_name != self.clut.name,
            None => true,
        };

        if needs_bind_group {
            let uniform = match renderer.draws.remove(&self.key) {
                Some(d) => d.uniform,
                None => device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("ruthos image uniforms"),
                    size: std::mem::size_of::<Uniforms>() as u64,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }),
            };
            let clut_view = renderer.clut_view(device, queue, &self.clut).clone();
            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("ruthos image bind group"),
                layout: &renderer.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: uniform.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&self.image.view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(&clut_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::Sampler(&renderer.sampler),
                    },
                ],
            });
            renderer.draws.insert(
                self.key,
                DrawResources {
                    uniform,
                    bind_group,
                    image_ptr,
                    clut_name: self.clut.name.clone(),
                },
            );
        }

        if let Some(d) = renderer.draws.get(&self.key) {
            queue.write_buffer(&d.uniform, 0, bytemuck::bytes_of(&uniforms));
        }
        Vec::new()
    }

    fn paint(
        &self,
        _info: egui::PaintCallbackInfo,
        render_pass: &mut wgpu::RenderPass<'static>,
        resources: &CallbackResources,
    ) {
        let Some(renderer) = resources.get::<ImageRenderer>() else {
            return;
        };
        let Some(d) = renderer.draws.get(&self.key) else {
            return;
        };
        render_pass.set_pipeline(&renderer.pipeline);
        render_pass.set_bind_group(0, &d.bind_group, &[]);
        render_pass.draw(0..6, 0..1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downsample_averages() {
        let src = vec![1.0, 3.0, 5.0, 7.0];
        let out = downsample_f32(&src, 2, 2, 2, 1, 1);
        assert_eq!(out, vec![4.0]);
        let rgba: Vec<u8> = vec![0, 0, 0, 255, 200, 100, 50, 255, 0, 0, 0, 255, 200, 100, 50, 255];
        let out = downsample_rgba(&rgba, 2, 2, 2, 1, 1);
        assert_eq!(out, vec![100, 50, 25, 255]);
    }
}
