//! Upload texture, equalizer pass, and the texture the host samples.

use mpv_wgpu::Grade;

use crate::types::{Error, SlotSize};

#[repr(C, align(16))]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct VideoParams {
    /// Three columns of a mat3, each padded to 16 bytes (WGSL uniform layout).
    grade: [[f32; 4]; 3],
    /// Packed against `gamma_exp`. A `vec3` is 12 bytes; the following `f32` sits at byte 60.
    bias: [f32; 3],
    gamma_exp: f32,
}

impl VideoParams {
    fn from_grade(grade: Grade) -> Self {
        let m = grade.matrix;
        Self {
            grade: [
                [m[0], m[1], m[2], 0.0],
                [m[3], m[4], m[5], 0.0],
                [m[6], m[7], m[8], 0.0],
            ],
            bias: grade.bias,
            gamma_exp: grade.gamma_exp,
        }
    }
}

/// Which upload texture the next `write_texture` fills.
#[derive(Clone, Copy)]
pub enum Upload {
    Front,
    Back,
}

impl Upload {
    fn index(self) -> usize {
        match self {
            Upload::Front => 0,
            Upload::Back => 1,
        }
    }

    pub fn flip(self) -> Self {
        match self {
            Upload::Front => Upload::Back,
            Upload::Back => Upload::Front,
        }
    }
}

struct Plane {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
}

/// GPU resources for one slot size. The render pipeline itself lives for the player.
pub struct Gpu {
    uploads: [Plane; 2],
    pub upload: Upload,
    // The view keeps the allocation alive; this handle is retained so resize drops it explicitly.
    output: wgpu::Texture,
    output_view: wgpu::TextureView,
    bind_group: wgpu::BindGroup,
    uniform: wgpu::Buffer,
    size: SlotSize,
}

pub struct Pipeline {
    sampler: wgpu::Sampler,
    layout: wgpu::BindGroupLayout,
    pipeline: wgpu::RenderPipeline,
}

impl Pipeline {
    pub fn new(device: &wgpu::Device) -> Result<Self, Error> {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("mpv-wgpu-video"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/video.wgsl").into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("mpv-wgpu-bg"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::NonFiltering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("mpv-wgpu-pl"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mpv-wgpu-rp"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("mpv-wgpu-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });
        Ok(Self {
            sampler,
            layout,
            pipeline,
        })
    }
}

impl Gpu {
    pub fn new(device: &wgpu::Device, pipeline: &Pipeline, size: SlotSize) -> Result<Self, Error> {
        let uploads = [
            plane(
                device,
                size,
                wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
            )?,
            plane(
                device,
                size,
                wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
            )?,
        ];
        let output = plane(
            device,
            size,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        )?;
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mpv-wgpu-uniform"),
            size: std::mem::size_of::<VideoParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group = bind_group(device, pipeline, &uploads[0].view, &uniform);
        Ok(Self {
            uploads,
            upload: Upload::Front,
            output: output.texture,
            output_view: output.view,
            bind_group,
            uniform,
            size,
        })
    }

    pub fn view(&self) -> &wgpu::TextureView {
        let _texture = &self.output;
        &self.output_view
    }

    /// Copy `bytes`, `height` rows of `stride` bytes, into the texture the next draw reads.
    pub fn upload(
        &self,
        queue: &wgpu::Queue,
        bytes: &[u8],
        stride: usize,
        width: u32,
        height: u32,
    ) -> Result<(), Error> {
        if width != self.size.width.get() || height != self.size.height.get() {
            return Err(Error::InvalidSize);
        }
        let needed = stride
            .checked_mul(height as usize)
            .ok_or(Error::InvalidSize)?;
        if bytes.len() < needed || stride < width as usize * 4 {
            return Err(Error::InvalidSize);
        }
        let texture = &self.uploads[self.upload.index()].texture;
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
                bytes_per_row: Some(u32::try_from(stride).map_err(|_| Error::InvalidSize)?),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        Ok(())
    }

    pub fn write_grade(&self, queue: &wgpu::Queue, grade: Grade) {
        let params = VideoParams::from_grade(grade);
        queue.write_buffer(&self.uniform, 0, bytemuck::bytes_of(&params));
    }

    /// Point the equalizer pass at `index`. Call this with the plane just written,
    /// then advance [`Gpu::upload`] to the other plane for the next write.
    pub fn rebind(&mut self, device: &wgpu::Device, pipeline: &Pipeline, index: Upload) {
        self.bind_group = bind_group(
            device,
            pipeline,
            &self.uploads[index.index()].view,
            &self.uniform,
        );
    }

    pub fn draw(&self, device: &wgpu::Device, queue: &wgpu::Queue, pipeline: &Pipeline) {
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("mpv-wgpu-eq"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("mpv-wgpu-eq"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.output_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&pipeline.pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        queue.submit(std::iter::once(encoder.finish()));
    }
}

fn plane(
    device: &wgpu::Device,
    size: SlotSize,
    usage: wgpu::TextureUsages,
) -> Result<Plane, Error> {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("mpv-wgpu-plane"),
        size: wgpu::Extent3d {
            width: size.width.get(),
            height: size.height.get(),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    Ok(Plane { texture, view })
}

fn bind_group(
    device: &wgpu::Device,
    pipeline: &Pipeline,
    view: &wgpu::TextureView,
    uniform: &wgpu::Buffer,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("mpv-wgpu-bind"),
        layout: &pipeline.layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(&pipeline.sampler),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: uniform.as_entire_binding(),
            },
        ],
    })
}

#[cfg(test)]
mod tests {
    use super::VideoParams;

    #[test]
    fn uniform_matches_wgsl_layout() {
        let source = include_str!("shaders/video.wgsl");
        let module = naga::front::wgsl::parse_str(source).expect("shader parses");
        let info = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::default(),
        )
        .validate(&module)
        .expect("shader validates");
        let params = module
            .types
            .iter()
            .find_map(|(_, ty)| match &ty.inner {
                naga::TypeInner::Struct { members, span }
                    if ty.name.as_deref() == Some("VideoParams") =>
                {
                    Some((members, *span))
                }
                _ => None,
            })
            .expect("VideoParams struct");
        let (members, span) = params;
        let _ = info;
        let gamma = members
            .iter()
            .find(|member| member.name.as_deref() == Some("gamma_exp"))
            .expect("gamma_exp");
        let bias = members
            .iter()
            .find(|member| member.name.as_deref() == Some("bias"))
            .expect("bias");
        assert_eq!(bias.offset, 48);
        assert_eq!(gamma.offset, 60);
        assert_eq!(span, 64);
        assert_eq!(std::mem::size_of::<VideoParams>(), span as usize);
        assert_eq!(
            std::mem::offset_of!(VideoParams, gamma_exp),
            gamma.offset as usize
        );
        assert_eq!(
            std::mem::offset_of!(VideoParams, bias),
            bias.offset as usize
        );
        assert_eq!(std::mem::align_of::<VideoParams>(), 16);
    }

    #[test]
    fn next_upload_is_the_plane_not_being_sampled() {
        use super::Upload;
        assert!(matches!(Upload::Front.flip(), Upload::Back));
        assert!(matches!(Upload::Back.flip(), Upload::Front));
    }
}
