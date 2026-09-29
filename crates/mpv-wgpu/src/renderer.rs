//! Draw a caller-owned picture into a caller-owned texture.

use crate::color::{self, ChromaSiting, Coefficients, Levels, Transfer};
use crate::cubic::{self, CubicKind, QuarterTurn};
use crate::present::{self, Encoding, PresentJob};
use crate::types::{Equalizer, Error};

/// Pixel rectangle inside the target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PixelRect {
    /// Left edge, in target pixels.
    pub x: u32,
    /// Top edge, in target pixels.
    pub y: u32,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

/// Stored depth of a byte plane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaneBits {
    /// One byte per sample, peak code 255.
    Eight,
    /// Little-endian `u16` samples, peak code 65535.
    Sixteen,
}

/// Where a plane's samples live.
pub enum PlaneSource<'a> {
    /// Tightly packed samples. 16-bit planes are little-endian `u16` bytes.
    Bytes(&'a [u8]),
    /// A texture the caller already owns. The draw samples this view.
    Texture(&'a wgpu::TextureView),
}

/// One image plane.
pub struct Plane<'a> {
    /// Width in samples.
    pub width: u32,
    /// Height in samples.
    pub height: u32,
    /// Sample depth. Ignored for [`PlaneSource::Texture`], which is already normalized.
    pub bits: PlaneBits,
    /// Bytes or an existing texture.
    pub source: PlaneSource<'a>,
}

/// A planar picture or one RGBA plane.
pub enum Picture<'a> {
    /// 8-bit RGBA. RGB is the display-referred signal and alpha is ignored.
    Rgba(Plane<'a>),
    /// Y, then U, then V. U and V may be smaller than Y.
    Yuv {
        /// Luma.
        y: Plane<'a>,
        /// Cb.
        u: Plane<'a>,
        /// Cr.
        v: Plane<'a>,
    },
}

/// Premultiplied RGBA8 bitmap drawn after the grade.
pub struct Overlay<'a> {
    /// Tightly packed premultiplied RGBA8.
    pub pixels: &'a [u8],
    /// Bitmap width.
    pub width: u32,
    /// Bitmap height.
    pub height: u32,
    /// Destination rectangle in target pixels.
    pub dest: PixelRect,
}

/// One draw into a caller texture.
pub struct Draw<'a> {
    /// Planes to sample.
    pub picture: Picture<'a>,
    /// YUV matrix. RGBA pictures ignore it except for the equalizer's luma weights.
    pub matrix: Coefficients,
    /// Studio or full range.
    pub levels: Levels,
    /// Transfer stored in the planes.
    pub transfer: Transfer,
    /// Chroma sample position. RGBA pictures ignore it.
    pub siting: ChromaSiting,
    /// Nominal frame peak, in nits. HLG uses this as the display peak.
    pub peak_nits: f32,
    /// Video rectangle inside the target.
    pub dest: PixelRect,
    /// Quarter turn applied while scaling into [`Draw::dest`].
    pub rotation: QuarterTurn,
    /// Applied once in linear light, before the inverse transfer.
    pub equalizer: Equalizer,
    /// Premultiplied bitmaps. An empty slice draws none.
    pub overlays: &'a [Overlay<'a>],
    /// Caller-owned target. `Gamma8` requires `Rgba8Unorm`. `Linear` requires `Rgba16Float`.
    pub target: &'a wgpu::Texture,
    /// `Linear` skips the tone-map knee and the inverse transfer.
    pub encoding: Encoding,
    /// Target peak in nits. The spline maps the frame peak onto this peak.
    pub target_peak_nits: f32,
}

/// Source-pixel coordinate and cubic kinds for one destination pixel.
pub fn placed_sample(
    decoded: &[[f32; 3]],
    src_w: u32,
    src_h: u32,
    framebuffer_x: u32,
    framebuffer_y: u32,
    dest: PixelRect,
    rotation: QuarterTurn,
    job: &PresentJob,
) -> [f32; 3] {
    let local_x = framebuffer_x.saturating_sub(dest.x);
    let local_y = framebuffer_y.saturating_sub(dest.y);
    let (sx, sy) = cubic::source_xy(
        local_x,
        local_y,
        src_w,
        src_h,
        dest.width,
        dest.height,
        rotation,
    );
    let (kind_x, kind_y) = cubic::axis_kinds(src_w, src_h, dest.width, dest.height, rotation);
    let linear = cubic::sample_image(decoded, src_w, src_h, sx, sy, kind_x, kind_y);
    present::present_texel(linear, framebuffer_x, framebuffer_y, job)
}

#[repr(C, align(16))]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DecodeParams {
    yuv: [[f32; 4]; 3],
    range: [f32; 4],
    luma_size: [f32; 2],
    chroma_size: [f32; 2],
    chroma_offset: [f32; 2],
    transfer: u32,
    rgba_mode: u32,
    peak_nits: f32,
    pad: f32,
    pad_tail: [f32; 2],
}

#[repr(C, align(16))]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct PresentParams {
    dest_origin: [f32; 2],
    dest_size: [f32; 2],
    src_size: [f32; 2],
    kind: [u32; 2],
    rotation: u32,
    encoding: u32,
    transfer: u32,
    pad0: u32,
    peaks: [f32; 2],
    pad1: [f32; 2],
    grade: [[f32; 4]; 3],
    bias: [f32; 3],
    gamma_exp: f32,
}

struct GpuTex {
    width: u32,
    height: u32,
    format: wgpu::TextureFormat,
    texture: wgpu::Texture,
    view: wgpu::TextureView,
}

struct Pipe {
    layout: wgpu::BindGroupLayout,
    gamma: wgpu::RenderPipeline,
    linear: wgpu::RenderPipeline,
}

/// Pipelines for the planar draw. The device and the target stay with the caller.
pub struct Renderer {
    decode_layout: wgpu::BindGroupLayout,
    decode: wgpu::RenderPipeline,
    present: Pipe,
    overlay_layout: wgpu::BindGroupLayout,
    overlay_gamma: wgpu::RenderPipeline,
    overlay_linear: wgpu::RenderPipeline,
    decode_uniform: wgpu::Buffer,
    present_uniform: wgpu::Buffer,
    scratch: Option<GpuTex>,
    slots: [Option<GpuTex>; 3],
    dummy: GpuTex,
}

impl Renderer {
    /// Compile the static shaders on `device`.
    pub fn new(device: &wgpu::Device) -> Result<Self, Error> {
        let decode_shader = shader(
            device,
            "mpv-wgpu-decode",
            include_str!("shaders/decode.wgsl"),
        );
        let present_shader = shader(
            device,
            "mpv-wgpu-present",
            include_str!("shaders/present.wgsl"),
        );
        let overlay_shader = shader(
            device,
            "mpv-wgpu-overlay",
            include_str!("shaders/overlay.wgsl"),
        );

        let decode_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("mpv-wgpu-decode-bg"),
            entries: &[tex_entry(0), tex_entry(1), tex_entry(2), uniform_entry(3)],
        });
        let present_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("mpv-wgpu-present-bg"),
            entries: &[tex_entry(0), uniform_entry(1)],
        });
        let overlay_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("mpv-wgpu-overlay-bg"),
            entries: &[tex_entry(0)],
        });

        let decode = pipeline(
            device,
            &decode_shader,
            &decode_layout,
            wgpu::TextureFormat::Rgba16Float,
            None,
            "mpv-wgpu-decode",
        );
        let present = Pipe {
            gamma: pipeline(
                device,
                &present_shader,
                &present_layout,
                wgpu::TextureFormat::Rgba8Unorm,
                None,
                "mpv-wgpu-present-gamma",
            ),
            linear: pipeline(
                device,
                &present_shader,
                &present_layout,
                wgpu::TextureFormat::Rgba16Float,
                None,
                "mpv-wgpu-present-linear",
            ),
            layout: present_layout,
        };
        let blend = premul_blend();
        let overlay_gamma = pipeline(
            device,
            &overlay_shader,
            &overlay_layout,
            wgpu::TextureFormat::Rgba8Unorm,
            Some(blend),
            "mpv-wgpu-overlay-gamma",
        );
        let overlay_linear = pipeline(
            device,
            &overlay_shader,
            &overlay_layout,
            wgpu::TextureFormat::Rgba16Float,
            Some(blend),
            "mpv-wgpu-overlay-linear",
        );
        let dummy = alloc_tex(device, 1, 1, wgpu::TextureFormat::R8Unorm, false)?;
        Ok(Self {
            decode_layout,
            decode,
            present,
            overlay_layout,
            overlay_gamma,
            overlay_linear,
            decode_uniform: uniform_buffer(device, std::mem::size_of::<DecodeParams>()),
            present_uniform: uniform_buffer(device, std::mem::size_of::<PresentParams>()),
            scratch: None,
            slots: [None, None, None],
            dummy,
        })
    }

    /// Upload or sample `draw.picture`, scale it into `draw.dest`, and store the encoding.
    pub fn draw(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        draw: Draw<'_>,
    ) -> Result<(), Error> {
        let (src_w, src_h, rgba) = match &draw.picture {
            Picture::Rgba(plane) => {
                nonempty(plane)?;
                (plane.width, plane.height, true)
            }
            Picture::Yuv { y, u, v } => {
                nonempty(y)?;
                nonempty(u)?;
                nonempty(v)?;
                if u.width != v.width || u.height != v.height {
                    return Err(Error::InvalidSize);
                }
                (y.width, y.height, false)
            }
        };
        if draw.dest.width == 0 || draw.dest.height == 0 {
            return Err(Error::InvalidSize);
        }
        let target_size = draw.target.size();
        if !fits(draw.dest, target_size.width, target_size.height) {
            return Err(Error::InvalidSize);
        }
        for overlay in draw.overlays {
            if !fits(overlay.dest, target_size.width, target_size.height) {
                return Err(Error::InvalidSize);
            }
        }
        let expected = match draw.encoding {
            Encoding::Gamma8 => wgpu::TextureFormat::Rgba8Unorm,
            Encoding::Linear => wgpu::TextureFormat::Rgba16Float,
        };
        if draw.target.format() != expected {
            return Err(Error::Target);
        }

        self.ensure_scratch(device, src_w, src_h)?;
        self.clear_dummy(queue);
        match &draw.picture {
            Picture::Rgba(plane) => {
                self.stage_plane(device, queue, 0, plane, true)?;
                self.clear_slot(1);
                self.clear_slot(2);
            }
            Picture::Yuv { y, u, v } => {
                self.stage_plane(device, queue, 0, y, false)?;
                self.stage_plane(device, queue, 1, u, false)?;
                self.stage_plane(device, queue, 2, v, false)?;
            }
        }

        let (y_view, u_view, v_view) = match &draw.picture {
            Picture::Rgba(plane) => (
                self.bound_view(plane, 0),
                self.plane_view(1),
                self.plane_view(2),
            ),
            Picture::Yuv { y, u, v } => (
                self.bound_view(y, 0),
                self.bound_view(u, 1),
                self.bound_view(v, 2),
            ),
        };
        let (chroma_w, chroma_h) = match &draw.picture {
            Picture::Rgba(_) => (1.0, 1.0),
            Picture::Yuv { u, .. } => (u.width as f32, u.height as f32),
        };
        let offset = color::chroma_siting_offset(draw.siting);
        let decode_params = DecodeParams {
            yuv: matrix_columns(color::yuv_to_rgb(draw.matrix)),
            range: range_uniform(draw.levels),
            luma_size: [src_w as f32, src_h as f32],
            chroma_size: [chroma_w, chroma_h],
            chroma_offset: offset,
            transfer: transfer_id(draw.transfer),
            rgba_mode: u32::from(rgba),
            peak_nits: draw.peak_nits,
            pad: 0.0,
            pad_tail: [0.0; 2],
        };
        queue.write_buffer(&self.decode_uniform, 0, bytemuck::bytes_of(&decode_params));

        let (kind_x, kind_y) = cubic::axis_kinds(
            src_w,
            src_h,
            draw.dest.width,
            draw.dest.height,
            draw.rotation,
        );
        let grade = crate::equalizer::bake(draw.equalizer, draw.matrix);
        let present_params = PresentParams {
            dest_origin: [draw.dest.x as f32, draw.dest.y as f32],
            dest_size: [draw.dest.width as f32, draw.dest.height as f32],
            src_size: [src_w as f32, src_h as f32],
            kind: [kind_id(kind_x), kind_id(kind_y)],
            rotation: match draw.rotation {
                QuarterTurn::D0 => 0,
                QuarterTurn::D90 => 1,
            },
            encoding: match draw.encoding {
                Encoding::Gamma8 => 0,
                Encoding::Linear => 1,
            },
            transfer: transfer_id(draw.transfer),
            pad0: 0,
            peaks: [draw.peak_nits, draw.target_peak_nits],
            pad1: [0.0; 2],
            grade: matrix_columns(grade.matrix),
            bias: grade.bias,
            gamma_exp: grade.gamma_exp,
        };
        queue.write_buffer(
            &self.present_uniform,
            0,
            bytemuck::bytes_of(&present_params),
        );

        let scratch_view = self
            .scratch
            .as_ref()
            .map(|tex| &tex.view)
            .ok_or(Error::Gpu)?;
        let decode_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("mpv-wgpu-decode-bind"),
            layout: &self.decode_layout,
            entries: &[
                view_bind(0, y_view),
                view_bind(1, u_view),
                view_bind(2, v_view),
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: self.decode_uniform.as_entire_binding(),
                },
            ],
        });
        let present_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("mpv-wgpu-present-bind"),
            layout: &self.present.layout,
            entries: &[
                view_bind(0, scratch_view),
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: self.present_uniform.as_entire_binding(),
                },
            ],
        });
        let target_view = draw
            .target
            .create_view(&wgpu::TextureViewDescriptor::default());
        let present_pipe = match draw.encoding {
            Encoding::Gamma8 => &self.present.gamma,
            Encoding::Linear => &self.present.linear,
        };
        let overlay_pipe = match draw.encoding {
            Encoding::Gamma8 => &self.overlay_gamma,
            Encoding::Linear => &self.overlay_linear,
        };

        let mut overlay_tex = Vec::with_capacity(draw.overlays.len());
        for overlay in draw.overlays {
            overlay_tex.push(upload_overlay(device, queue, overlay)?);
        }

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("mpv-wgpu-picture"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("mpv-wgpu-decode"),
                color_attachments: &[Some(color_att(scratch_view, wgpu::LoadOp::Clear(black())))],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.decode);
            pass.set_bind_group(0, &decode_bind, &[]);
            pass.draw(0..3, 0..1);
        }
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("mpv-wgpu-present"),
                color_attachments: &[Some(color_att(&target_view, wgpu::LoadOp::Clear(black())))],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(present_pipe);
            pass.set_bind_group(0, &present_bind, &[]);
            pass.set_viewport(
                draw.dest.x as f32,
                draw.dest.y as f32,
                draw.dest.width as f32,
                draw.dest.height as f32,
                0.0,
                1.0,
            );
            pass.set_scissor_rect(draw.dest.x, draw.dest.y, draw.dest.width, draw.dest.height);
            pass.draw(0..3, 0..1);
        }
        for (overlay, tex) in draw.overlays.iter().zip(overlay_tex.iter()) {
            if overlay.dest.width == 0 || overlay.dest.height == 0 {
                continue;
            }
            let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("mpv-wgpu-overlay-bind"),
                layout: &self.overlay_layout,
                entries: &[view_bind(0, &tex.view)],
            });
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("mpv-wgpu-overlay"),
                color_attachments: &[Some(color_att(&target_view, wgpu::LoadOp::Load))],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(overlay_pipe);
            pass.set_bind_group(0, &bind, &[]);
            pass.set_viewport(
                overlay.dest.x as f32,
                overlay.dest.y as f32,
                overlay.dest.width as f32,
                overlay.dest.height as f32,
                0.0,
                1.0,
            );
            pass.set_scissor_rect(
                overlay.dest.x,
                overlay.dest.y,
                overlay.dest.width,
                overlay.dest.height,
            );
            pass.draw(0..3, 0..1);
        }
        queue.submit([encoder.finish()]);
        Ok(())
    }

    fn ensure_scratch(
        &mut self,
        device: &wgpu::Device,
        width: u32,
        height: u32,
    ) -> Result<(), Error> {
        let replace = self
            .scratch
            .as_ref()
            .is_none_or(|tex| tex.width != width || tex.height != height);
        if replace {
            self.scratch = Some(alloc_tex(
                device,
                width,
                height,
                wgpu::TextureFormat::Rgba16Float,
                true,
            )?);
        }
        Ok(())
    }

    fn clear_dummy(&self, queue: &wgpu::Queue) {
        let bytes = [0u8; 256];
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.dummy.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &bytes,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(256),
                rows_per_image: Some(1),
            },
            wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
    }

    fn stage_plane(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        index: usize,
        plane: &Plane<'_>,
        rgba: bool,
    ) -> Result<(), Error> {
        match &plane.source {
            PlaneSource::Texture(_) => {
                self.slots[index] = None;
                Ok(())
            }
            PlaneSource::Bytes(bytes) => {
                let format = plane_format(plane, rgba)?;
                let byte_width = row_bytes(plane, rgba)?;
                let needed = byte_width
                    .checked_mul(plane.height as usize)
                    .ok_or(Error::InvalidSize)?;
                if bytes.len() < needed {
                    return Err(Error::InvalidSize);
                }
                let stale = self.slots[index].as_ref().is_none_or(|tex| {
                    tex.width != plane.width || tex.height != plane.height || tex.format != format
                });
                if stale {
                    if format == wgpu::TextureFormat::R16Unorm
                        && !device
                            .features()
                            .contains(wgpu::Features::TEXTURE_FORMAT_16BIT_NORM)
                    {
                        return Err(Error::Gpu);
                    }
                    self.slots[index] =
                        Some(alloc_tex(device, plane.width, plane.height, format, false)?);
                }
                let texture = &self.slots[index].as_ref().ok_or(Error::Gpu)?.texture;
                let (padded, stride) = pad_rows(bytes, byte_width, plane.height)?;
                queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture,
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    &padded,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(stride),
                        rows_per_image: Some(plane.height),
                    },
                    wgpu::Extent3d {
                        width: plane.width,
                        height: plane.height,
                        depth_or_array_layers: 1,
                    },
                );
                Ok(())
            }
        }
    }

    fn clear_slot(&mut self, index: usize) {
        self.slots[index] = None;
    }

    fn plane_view(&self, index: usize) -> &wgpu::TextureView {
        self.slots[index]
            .as_ref()
            .map(|tex| &tex.view)
            .unwrap_or(&self.dummy.view)
    }

    fn bound_view<'a>(&'a self, plane: &'a Plane<'_>, index: usize) -> &'a wgpu::TextureView {
        match &plane.source {
            PlaneSource::Texture(view) => view,
            PlaneSource::Bytes(_) => self.plane_view(index),
        }
    }
}

fn nonempty(plane: &Plane<'_>) -> Result<(), Error> {
    if plane.width == 0 || plane.height == 0 {
        Err(Error::InvalidSize)
    } else {
        Ok(())
    }
}

fn plane_format(plane: &Plane<'_>, rgba: bool) -> Result<wgpu::TextureFormat, Error> {
    if rgba {
        return match plane.bits {
            PlaneBits::Eight => Ok(wgpu::TextureFormat::Rgba8Unorm),
            PlaneBits::Sixteen => Err(Error::InvalidSize),
        };
    }
    match plane.bits {
        PlaneBits::Eight => Ok(wgpu::TextureFormat::R8Unorm),
        PlaneBits::Sixteen => Ok(wgpu::TextureFormat::R16Unorm),
    }
}

fn row_bytes(plane: &Plane<'_>, rgba: bool) -> Result<usize, Error> {
    let sample = if rgba {
        match plane.bits {
            PlaneBits::Eight => 4usize,
            PlaneBits::Sixteen => return Err(Error::InvalidSize),
        }
    } else {
        match plane.bits {
            PlaneBits::Eight => 1usize,
            PlaneBits::Sixteen => 2usize,
        }
    };
    (plane.width as usize)
        .checked_mul(sample)
        .ok_or(Error::InvalidSize)
}

fn shader(device: &wgpu::Device, label: &str, source: &'static str) -> wgpu::ShaderModule {
    device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(source.into()),
    })
}

fn tex_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: false },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

fn uniform_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn pipeline(
    device: &wgpu::Device,
    module: &wgpu::ShaderModule,
    layout: &wgpu::BindGroupLayout,
    format: wgpu::TextureFormat,
    blend: Option<wgpu::BlendState>,
    label: &str,
) -> wgpu::RenderPipeline {
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(label),
        bind_group_layouts: &[Some(layout)],
        immediate_size: 0,
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module,
            entry_point: Some("vs_main"),
            buffers: &[],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend,
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    })
}

fn premul_blend() -> wgpu::BlendState {
    let component = wgpu::BlendComponent {
        src_factor: wgpu::BlendFactor::One,
        dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
        operation: wgpu::BlendOperation::Add,
    };
    wgpu::BlendState {
        color: component,
        alpha: component,
    }
}

fn uniform_buffer(device: &wgpu::Device, size: usize) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("mpv-wgpu-picture-uniform"),
        size: size as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn alloc_tex(
    device: &wgpu::Device,
    width: u32,
    height: u32,
    format: wgpu::TextureFormat,
    render: bool,
) -> Result<GpuTex, Error> {
    let usage = if render {
        wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING
    } else {
        wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING
    };
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("mpv-wgpu-picture-tex"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    Ok(GpuTex {
        width,
        height,
        format,
        texture,
        view,
    })
}

fn upload_overlay(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    overlay: &Overlay<'_>,
) -> Result<GpuTex, Error> {
    let needed = (overlay.width as usize)
        .checked_mul(overlay.height as usize)
        .and_then(|n| n.checked_mul(4))
        .ok_or(Error::InvalidSize)?;
    if overlay.pixels.len() < needed || overlay.width == 0 || overlay.height == 0 {
        return Err(Error::InvalidSize);
    }
    let tex = alloc_tex(
        device,
        overlay.width,
        overlay.height,
        wgpu::TextureFormat::Rgba8Unorm,
        false,
    )?;
    let row = overlay.width as usize * 4;
    let (padded, stride) = pad_rows(overlay.pixels, row, overlay.height)?;
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &tex.texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &padded,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(stride),
            rows_per_image: Some(overlay.height),
        },
        wgpu::Extent3d {
            width: overlay.width,
            height: overlay.height,
            depth_or_array_layers: 1,
        },
    );
    Ok(tex)
}

fn pad_rows(bytes: &[u8], row_bytes: usize, height: u32) -> Result<(Vec<u8>, u32), Error> {
    let stride = row_bytes.div_ceil(256) * 256;
    let stride_u = u32::try_from(stride).map_err(|_| Error::InvalidSize)?;
    let mut padded = vec![0u8; stride * height as usize];
    for y in 0..height as usize {
        let src = y * row_bytes;
        let dst = y * stride;
        padded[dst..dst + row_bytes].copy_from_slice(&bytes[src..src + row_bytes]);
    }
    Ok((padded, stride_u))
}

fn fits(rect: PixelRect, width: u32, height: u32) -> bool {
    rect.x
        .checked_add(rect.width)
        .is_some_and(|right| right <= width)
        && rect
            .y
            .checked_add(rect.height)
            .is_some_and(|bottom| bottom <= height)
}

fn range_uniform(levels: Levels) -> [f32; 4] {
    match levels {
        Levels::Full => [0.0, 1.0, 0.5, 1.0],
        Levels::Limited => [16.0 / 255.0, 255.0 / 219.0, 128.0 / 255.0, 255.0 / 224.0],
    }
}

fn transfer_id(transfer: Transfer) -> u32 {
    match transfer {
        Transfer::Bt1886 => 0,
        Transfer::Srgb => 1,
        Transfer::Pq => 2,
        Transfer::Hlg => 3,
    }
}

fn kind_id(kind: CubicKind) -> u32 {
    match kind {
        CubicKind::Hermite => 0,
        CubicKind::CatmullRom => 1,
    }
}

fn matrix_columns(m: [f32; 9]) -> [[f32; 4]; 3] {
    [
        [m[0], m[1], m[2], 0.0],
        [m[3], m[4], m[5], 0.0],
        [m[6], m[7], m[8], 0.0],
    ]
}

fn view_bind(binding: u32, view: &wgpu::TextureView) -> wgpu::BindGroupEntry<'_> {
    wgpu::BindGroupEntry {
        binding,
        resource: wgpu::BindingResource::TextureView(view),
    }
}

fn color_att(
    view: &wgpu::TextureView,
    load: wgpu::LoadOp<wgpu::Color>,
) -> wgpu::RenderPassColorAttachment<'_> {
    wgpu::RenderPassColorAttachment {
        view,
        resolve_target: None,
        depth_slice: None,
        ops: wgpu::Operations {
            load,
            store: wgpu::StoreOp::Store,
        },
    }
}

fn black() -> wgpu::Color {
    wgpu::Color {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 1.0,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::{
        Draw, Overlay, Picture, PixelRect, Plane, PlaneBits, PlaneSource, Renderer, pad_rows,
        placed_sample,
    };
    use crate::color::{self, ChromaSiting, Coefficients, Levels, Transfer};
    use crate::cubic::QuarterTurn;
    use crate::present::{Encoding, PresentJob};
    use crate::types::{Equalizer, Error, Hue, UnitBias};

    fn close(got: f32, want: f32, tol: f32) {
        assert!((got - want).abs() <= tol, "{got} vs {want} (tol {tol})");
    }

    fn norm8(codes: &[u8]) -> Vec<f32> {
        codes
            .iter()
            .map(|code| color::normalize_code(u32::from(*code), 8))
            .collect()
    }

    fn norm16(codes: &[u16]) -> Vec<f32> {
        codes
            .iter()
            .map(|code| color::normalize_code(u32::from(*code), 16))
            .collect()
    }

    fn job(
        matrix: Coefficients,
        transfer: Transfer,
        equalizer: Equalizer,
        encoding: Encoding,
        peak: f32,
        target_peak: f32,
    ) -> PresentJob {
        PresentJob {
            source_peak_nits: peak,
            target_peak_nits: target_peak,
            transfer,
            matrix,
            equalizer,
            encoding,
        }
    }

    fn decode_yuv(
        y: &[f32],
        yw: u32,
        yh: u32,
        u: &[f32],
        uw: u32,
        uh: u32,
        v: &[f32],
        matrix: Coefficients,
        levels: Levels,
        transfer: Transfer,
        peak: f32,
        siting: ChromaSiting,
    ) -> Vec<[f32; 3]> {
        let offset = color::chroma_siting_offset(siting);
        let mut out = Vec::with_capacity((yw * yh) as usize);
        for row in 0..yh {
            for col in 0..yw {
                let sample = y[(row * yw + col) as usize];
                let cu = color::chroma_coord(col as f32, yw, uw, offset[0]);
                let cv = color::chroma_coord(row as f32, yh, uh, offset[1]);
                let cb = color::sample_bilinear(u, uw, uh, cu, cv);
                let cr = color::sample_bilinear(v, uw, uh, cu, cv);
                out.push(color::decode_linear(
                    matrix, levels, transfer, peak, sample, cb, cr,
                ));
            }
        }
        out
    }

    fn assert_placed(
        pixels: &[[f32; 4]],
        target_w: u32,
        decoded: &[[f32; 3]],
        src_w: u32,
        src_h: u32,
        dest: PixelRect,
        rotation: QuarterTurn,
        present: &PresentJob,
        tol: f32,
    ) {
        for y in 0..dest.height {
            for x in 0..dest.width {
                let fx = dest.x + x;
                let fy = dest.y + y;
                let expect = placed_sample(decoded, src_w, src_h, fx, fy, dest, rotation, present);
                let got = pixels[(fy * target_w + fx) as usize];
                for channel in 0..3 {
                    close(got[channel], expect[channel], tol);
                }
            }
        }
    }

    fn rect(width: u32, height: u32) -> PixelRect {
        PixelRect {
            x: 0,
            y: 0,
            width,
            height,
        }
    }

    #[test]
    fn uniform_matches_wgsl_layout() {
        assert_layout(
            include_str!("shaders/decode.wgsl"),
            "DecodeParams",
            &[
                ("yuv", 0),
                ("range", 48),
                ("luma_size", 64),
                ("chroma_size", 72),
                ("chroma_offset", 80),
                ("transfer", 88),
                ("rgba_mode", 92),
                ("peak_nits", 96),
                ("pad", 100),
            ],
            112,
            std::mem::size_of::<super::DecodeParams>(),
        );
        assert_layout(
            include_str!("shaders/present.wgsl"),
            "PresentParams",
            &[
                ("dest_origin", 0),
                ("dest_size", 8),
                ("src_size", 16),
                ("kind", 24),
                ("rotation", 32),
                ("encoding", 36),
                ("transfer", 40),
                ("pad0", 44),
                ("peaks", 48),
                ("pad1", 56),
                ("grade", 64),
                ("bias", 112),
                ("gamma_exp", 124),
            ],
            128,
            std::mem::size_of::<super::PresentParams>(),
        );
        let overlay = naga::front::wgsl::parse_str(include_str!("shaders/overlay.wgsl"))
            .expect("overlay shader parses");
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::default(),
        )
        .validate(&overlay)
        .expect("overlay shader validates");
        assert_eq!(std::mem::offset_of!(super::PresentParams, gamma_exp), 124);
        assert_eq!(std::mem::offset_of!(super::PresentParams, bias), 112);
        assert_eq!(std::mem::offset_of!(super::DecodeParams, peak_nits), 96);
    }

    fn assert_layout(
        source: &str,
        name: &str,
        fields: &[(&str, u32)],
        span_bytes: u32,
        rust_size: usize,
    ) {
        let module = naga::front::wgsl::parse_str(source).expect("shader parses");
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::default(),
        )
        .validate(&module)
        .expect("shader validates");
        let (members, span) = module
            .types
            .iter()
            .find_map(|(_, ty)| match &ty.inner {
                naga::TypeInner::Struct { members, span } if ty.name.as_deref() == Some(name) => {
                    Some((members, *span))
                }
                _ => None,
            })
            .expect(name);
        assert_eq!(span, span_bytes);
        assert_eq!(rust_size, span as usize);
        for (field, offset) in fields {
            let member = members
                .iter()
                .find(|member| member.name.as_deref() == Some(*field))
                .expect(field);
            assert_eq!(member.offset, *offset, "{field}");
        }
    }

    #[test]
    fn half_float_round_trips_the_values_the_target_stores() {
        assert_eq!(f16_to_f32(0), 0.0);
        assert_eq!(f16_to_f32(0x3c00), 1.0);
        assert_eq!(f16_to_f32(0xbc00), -1.0);
        assert_eq!(f16_to_f32(0x3800), 0.5);
        assert_eq!(f16_to_f32(0x5640), 100.0);
    }

    #[test]
    fn limited_endpoints_survive_a_one_to_one_present() {
        let y = norm8(&[16, 235]);
        let chroma = norm8(&[128]);
        let dest = rect(2, 1);
        for matrix in [Coefficients::Bt709, Coefficients::Bt601] {
            let decoded = decode_yuv(
                &y,
                2,
                1,
                &chroma,
                1,
                1,
                &chroma,
                matrix,
                Levels::Limited,
                Transfer::Bt1886,
                100.0,
                ChromaSiting::Center,
            );
            let present = job(
                matrix,
                Transfer::Bt1886,
                Equalizer::default(),
                Encoding::Gamma8,
                100.0,
                100.0,
            );
            let black = placed_sample(&decoded, 2, 1, 0, 0, dest, QuarterTurn::D0, &present);
            let white = placed_sample(&decoded, 2, 1, 1, 0, dest, QuarterTurn::D0, &present);
            for channel in 0..3 {
                close(black[channel], 0.0, 1.0 / 255.0);
                close(white[channel], 1.0, 1.0 / 255.0);
            }
        }
    }

    #[test]
    fn contrast_minus_100_placed_sample_is_mid_gray() {
        let decoded = [
            [0.1, 0.8, 0.3],
            [1.0, 0.0, 0.4],
            [0.2, 0.2, 0.9],
            [0.7, 0.1, 0.5],
        ];
        let equalizer = Equalizer {
            contrast: UnitBias::new(-100),
            ..Equalizer::default()
        };
        let present = job(
            Coefficients::Bt709,
            Transfer::Bt1886,
            equalizer,
            Encoding::Linear,
            100.0,
            100.0,
        );
        let dest = rect(2, 2);
        for y in 0..2 {
            for x in 0..2 {
                let sample = placed_sample(&decoded, 2, 2, x, y, dest, QuarterTurn::D0, &present);
                for channel in sample {
                    close(channel, 0.5, 1.0e-5);
                }
            }
        }
    }

    #[test]
    fn draw_readback_matches_the_shipped_functions() {
        let (device, queue) = match open_device() {
            Ok(gpu) => gpu,
            Err(err) => {
                eprintln!("gpu-device-unavailable: {err}");
                return;
            }
        };
        let mut renderer = Renderer::new(&device).expect("renderer");
        let tol = 1.0 / 255.0;

        let y4 = [
            16, 235, 16, 235, 235, 16, 235, 16, 16, 16, 235, 235, 235, 235, 16, 16,
        ];
        let u4 = [128, 128, 128, 200];
        let v4 = [128, 160, 128, 128];
        let pattern = paint_yuv(
            &mut renderer,
            &device,
            &queue,
            &y4,
            4,
            4,
            &u4,
            2,
            2,
            &v4,
            PlaneBits::Eight,
            Coefficients::Bt709,
            Levels::Limited,
            Transfer::Bt1886,
            ChromaSiting::Center,
            100.0,
            100.0,
            QuarterTurn::D0,
            Equalizer::default(),
            Encoding::Gamma8,
            rect(4, 4),
            &[],
            tol,
        );
        assert!(pattern.cpu.iter().any(|px| px[0] < 0.05));
        assert!(pattern.cpu.iter().any(|px| px[0] > 0.95));

        let y601 = [16, 235];
        let neutral = [128];
        paint_yuv(
            &mut renderer,
            &device,
            &queue,
            &y601,
            2,
            1,
            &neutral,
            1,
            1,
            &neutral,
            PlaneBits::Eight,
            Coefficients::Bt601,
            Levels::Limited,
            Transfer::Bt1886,
            ChromaSiting::TopLeft,
            100.0,
            100.0,
            QuarterTurn::D0,
            Equalizer::default(),
            Encoding::Gamma8,
            rect(2, 1),
            &[],
            tol,
        );

        let full = [0, 255];
        paint_yuv(
            &mut renderer,
            &device,
            &queue,
            &full,
            2,
            1,
            &neutral,
            1,
            1,
            &neutral,
            PlaneBits::Eight,
            Coefficients::Bt709,
            Levels::Full,
            Transfer::Bt1886,
            ChromaSiting::TopLeft,
            100.0,
            100.0,
            QuarterTurn::D0,
            Equalizer::default(),
            Encoding::Gamma8,
            rect(2, 1),
            &[],
            tol,
        );

        let y16 = [0, 65535];
        let chroma16 = [32768];
        paint16(&mut renderer, &device, &queue, &y16, &chroma16, &chroma16);

        let y_up = [16, 235, 235, 16];
        paint_yuv(
            &mut renderer,
            &device,
            &queue,
            &y_up,
            2,
            2,
            &neutral,
            1,
            1,
            &neutral,
            PlaneBits::Eight,
            Coefficients::Bt709,
            Levels::Limited,
            Transfer::Bt1886,
            ChromaSiting::TopLeft,
            100.0,
            100.0,
            QuarterTurn::D0,
            Equalizer::default(),
            Encoding::Linear,
            rect(4, 2),
            &[],
            tol,
        );

        let y_down = [16, 235, 16, 235, 235, 16, 235, 16];
        let chroma_down = [128, 128];
        paint_yuv(
            &mut renderer,
            &device,
            &queue,
            &y_down,
            4,
            2,
            &chroma_down,
            2,
            1,
            &chroma_down,
            PlaneBits::Eight,
            Coefficients::Bt709,
            Levels::Limited,
            Transfer::Bt1886,
            ChromaSiting::Left,
            100.0,
            100.0,
            QuarterTurn::D0,
            Equalizer::default(),
            Encoding::Linear,
            rect(2, 2),
            &[],
            tol,
        );

        let y_turn = [16, 80, 160, 235];
        let flat = [128, 128, 128, 128];
        let turned = paint_yuv(
            &mut renderer,
            &device,
            &queue,
            &y_turn,
            2,
            2,
            &flat,
            2,
            2,
            &flat,
            PlaneBits::Eight,
            Coefficients::Bt709,
            Levels::Limited,
            Transfer::Bt1886,
            ChromaSiting::TopLeft,
            100.0,
            100.0,
            QuarterTurn::D90,
            Equalizer::default(),
            Encoding::Linear,
            rect(2, 2),
            &[],
            tol,
        );
        assert_distinct(&turned.cpu);

        let graded = Equalizer {
            contrast: UnitBias::new(-100),
            ..Equalizer::default()
        };
        let gray = paint_yuv(
            &mut renderer,
            &device,
            &queue,
            &y_up,
            2,
            2,
            &neutral,
            1,
            1,
            &neutral,
            PlaneBits::Eight,
            Coefficients::Bt709,
            Levels::Limited,
            Transfer::Bt1886,
            ChromaSiting::TopLeft,
            100.0,
            100.0,
            QuarterTurn::D0,
            graded,
            Encoding::Linear,
            rect(2, 2),
            &[],
            tol,
        );
        for px in &gray.cpu {
            for channel in px {
                close(*channel, 0.5, 1.0e-4);
            }
        }

        let white = [255, 255, 255, 255];
        let pq = paint_yuv(
            &mut renderer,
            &device,
            &queue,
            &white,
            2,
            2,
            &neutral,
            1,
            1,
            &neutral,
            PlaneBits::Eight,
            Coefficients::Bt709,
            Levels::Full,
            Transfer::Pq,
            ChromaSiting::TopLeft,
            10_000.0,
            100.0,
            QuarterTurn::D0,
            Equalizer::default(),
            Encoding::Linear,
            rect(2, 2),
            &[],
            0.05,
        );
        assert!(pq.gpu[0][0] > 1.0, "pq peak {}", pq.gpu[0][0]);
        assert!(pq.cpu[0][0] > 1.0, "cpu pq {}", pq.cpu[0][0]);

        let red = [255, 0, 0, 255];
        let overlays = [Overlay {
            pixels: &red,
            width: 1,
            height: 1,
            dest: PixelRect {
                x: 4,
                y: 0,
                width: 1,
                height: 1,
            },
        }];
        let composited = paint_yuv(
            &mut renderer,
            &device,
            &queue,
            &y_up,
            2,
            2,
            &neutral,
            1,
            1,
            &neutral,
            PlaneBits::Eight,
            Coefficients::Bt709,
            Levels::Limited,
            Transfer::Bt1886,
            ChromaSiting::TopLeft,
            100.0,
            100.0,
            QuarterTurn::D0,
            graded,
            Encoding::Gamma8,
            rect(2, 2),
            &overlays,
            tol,
        );
        let overlay_px = composited.gpu[4];
        close(overlay_px[0], 1.0, tol);
        close(overlay_px[1], 0.0, tol);
        close(overlay_px[2], 0.0, tol);

        let rgba = [0, 0, 0, 255, 255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255];
        paint_rgba(&mut renderer, &device, &queue, &rgba);

        let again = paint_yuv_texture(&mut renderer, &device, &queue, &y4, &u4, &v4);
        for (got, want) in again.iter().zip(pattern.gpu.iter()) {
            for channel in 0..3 {
                close(got[channel], want[channel], tol);
            }
        }

        let y_bt2020 = [200u8];
        let u_bt2020 = [64u8];
        let v_bt2020 = [220u8];
        paint_yuv(
            &mut renderer,
            &device,
            &queue,
            &y_bt2020,
            1,
            1,
            &u_bt2020,
            1,
            1,
            &v_bt2020,
            PlaneBits::Eight,
            Coefficients::Bt2020,
            Levels::Full,
            Transfer::Bt1886,
            ChromaSiting::TopLeft,
            100.0,
            100.0,
            QuarterTurn::D0,
            Equalizer::default(),
            Encoding::Linear,
            rect(1, 1),
            &[],
            tol,
        );
        let decoded_2020 = decode_yuv(
            &norm8(&y_bt2020),
            1,
            1,
            &norm8(&u_bt2020),
            1,
            1,
            &norm8(&v_bt2020),
            Coefficients::Bt2020,
            Levels::Full,
            Transfer::Bt1886,
            100.0,
            ChromaSiting::TopLeft,
        );
        let decoded_709 = decode_yuv(
            &norm8(&y_bt2020),
            1,
            1,
            &norm8(&u_bt2020),
            1,
            1,
            &norm8(&v_bt2020),
            Coefficients::Bt709,
            Levels::Full,
            Transfer::Bt1886,
            100.0,
            ChromaSiting::TopLeft,
        );
        let matrix_delta: f32 = (0..3)
            .map(|channel| (decoded_2020[0][channel] - decoded_709[0][channel]).abs())
            .sum();
        assert!(
            matrix_delta > 0.02,
            "bt.2020 {decoded_2020:?} matched bt.709 {decoded_709:?}"
        );

        let y_srgb = [0u8, 128];
        let srgb = paint_yuv(
            &mut renderer,
            &device,
            &queue,
            &y_srgb,
            2,
            1,
            &neutral,
            1,
            1,
            &neutral,
            PlaneBits::Eight,
            Coefficients::Bt709,
            Levels::Full,
            Transfer::Srgb,
            ChromaSiting::TopLeft,
            100.0,
            100.0,
            QuarterTurn::D0,
            Equalizer::default(),
            Encoding::Linear,
            rect(2, 1),
            &[],
            tol,
        );
        close(srgb.gpu[0][0], 0.0, tol);
        let signal: f32 = 128.0 / 255.0;
        let as_22 = signal.powf(2.2);
        let as_24 = signal.powf(2.4);
        let mid = srgb.gpu[1][0];
        assert!(
            (mid - as_22).abs() + 0.01 < (mid - as_24).abs(),
            "srgb linear {mid} is not nearer 2.2 ({as_22}) than 2.4 ({as_24})"
        );

        let y_hlg = [0u8, 255];
        let hlg = paint_yuv(
            &mut renderer,
            &device,
            &queue,
            &y_hlg,
            2,
            1,
            &neutral,
            1,
            1,
            &neutral,
            PlaneBits::Eight,
            Coefficients::Bt709,
            Levels::Full,
            Transfer::Hlg,
            ChromaSiting::TopLeft,
            1000.0,
            1000.0,
            QuarterTurn::D0,
            Equalizer::default(),
            Encoding::Linear,
            rect(2, 1),
            &[],
            0.05,
        );
        close(hlg.cpu[0][0], 0.0, 0.02);
        close(hlg.gpu[0][0], 0.0, 0.05);
        close(hlg.cpu[1][0], 10.0, 0.05);
        for channel in 0..3 {
            assert!(
                hlg.gpu[1][channel] > 1.0,
                "hlg white channel {channel} {}",
                hlg.gpu[1][channel]
            );
        }

        let knobs = Equalizer {
            brightness: UnitBias::new(20),
            saturation: UnitBias::new(-40),
            gamma: UnitBias::new(50),
            hue: Hue::new(45),
            ..Equalizer::default()
        };
        let y_grade = [96u8, 180];
        let u_grade = [64u8];
        let v_grade = [200u8];
        let graded = paint_yuv(
            &mut renderer,
            &device,
            &queue,
            &y_grade,
            2,
            1,
            &u_grade,
            1,
            1,
            &v_grade,
            PlaneBits::Eight,
            Coefficients::Bt709,
            Levels::Full,
            Transfer::Bt1886,
            ChromaSiting::TopLeft,
            100.0,
            100.0,
            QuarterTurn::D0,
            knobs,
            Encoding::Linear,
            rect(2, 1),
            &[],
            tol,
        );
        let decoded_grade = decode_yuv(
            &norm8(&y_grade),
            2,
            1,
            &norm8(&u_grade),
            1,
            1,
            &norm8(&v_grade),
            Coefficients::Bt709,
            Levels::Full,
            Transfer::Bt1886,
            100.0,
            ChromaSiting::TopLeft,
        );
        let plain = job(
            Coefficients::Bt709,
            Transfer::Bt1886,
            Equalizer::default(),
            Encoding::Linear,
            100.0,
            100.0,
        );
        let mut moved = 0.0;
        for x in 0..2 {
            let sample = placed_sample(
                &decoded_grade,
                2,
                1,
                x,
                0,
                rect(2, 1),
                QuarterTurn::D0,
                &plain,
            );
            for (graded_channel, sample_channel) in graded.cpu[x as usize].iter().zip(sample) {
                moved += (graded_channel - sample_channel).abs();
            }
        }
        assert!(moved > 0.05, "grade was a no-op on {:?}", graded.cpu);
    }

    #[test]
    fn draw_rejects_bad_size_format_and_16_bit_without_the_feature() {
        let (device, queue) = match open_without_16bit() {
            Ok(gpu) => gpu,
            Err(err) => {
                eprintln!("gpu-device-unavailable: {err}");
                return;
            }
        };
        assert!(
            !device
                .features()
                .contains(wgpu::Features::TEXTURE_FORMAT_16BIT_NORM),
            "a device requested with no features still exposes 16-bit unorm"
        );
        let mut renderer = Renderer::new(&device).expect("renderer");
        let y = [128u8, 128];
        let chroma = [128u8];
        let short = [128u8];
        let rgba = [0u8, 0, 0, 255];
        let y16 = [0u8, 0, 255, 255];
        let c16 = [0u8, 128];
        let red = [255u8, 0, 0, 255];
        let target = target_tex(&device, 2, 1, wgpu::TextureFormat::Rgba8Unorm);

        let err = draw_error(
            &mut renderer,
            &device,
            &queue,
            Picture::Yuv {
                y: byte_plane(&y, 0, 1, PlaneBits::Eight),
                u: byte_plane(&chroma, 1, 1, PlaneBits::Eight),
                v: byte_plane(&chroma, 1, 1, PlaneBits::Eight),
            },
            rect(2, 1),
            &[],
            &target,
            Encoding::Gamma8,
        );
        assert!(matches!(err, Error::InvalidSize));

        let err = draw_error(
            &mut renderer,
            &device,
            &queue,
            Picture::Yuv {
                y: byte_plane(&y, 2, 1, PlaneBits::Eight),
                u: byte_plane(&chroma, 1, 1, PlaneBits::Eight),
                v: byte_plane(&y, 2, 1, PlaneBits::Eight),
            },
            rect(2, 1),
            &[],
            &target,
            Encoding::Gamma8,
        );
        assert!(matches!(err, Error::InvalidSize));

        let err = draw_error(
            &mut renderer,
            &device,
            &queue,
            Picture::Yuv {
                y: byte_plane(&y, 2, 1, PlaneBits::Eight),
                u: byte_plane(&chroma, 1, 1, PlaneBits::Eight),
                v: byte_plane(&chroma, 1, 1, PlaneBits::Eight),
            },
            rect(0, 1),
            &[],
            &target,
            Encoding::Gamma8,
        );
        assert!(matches!(err, Error::InvalidSize));

        let err = draw_error(
            &mut renderer,
            &device,
            &queue,
            Picture::Yuv {
                y: byte_plane(&y, 2, 1, PlaneBits::Eight),
                u: byte_plane(&chroma, 1, 1, PlaneBits::Eight),
                v: byte_plane(&chroma, 1, 1, PlaneBits::Eight),
            },
            rect(4, 1),
            &[],
            &target,
            Encoding::Gamma8,
        );
        assert!(matches!(err, Error::InvalidSize));

        let err = draw_error(
            &mut renderer,
            &device,
            &queue,
            Picture::Yuv {
                y: byte_plane(&short, 2, 1, PlaneBits::Eight),
                u: byte_plane(&chroma, 1, 1, PlaneBits::Eight),
                v: byte_plane(&chroma, 1, 1, PlaneBits::Eight),
            },
            rect(2, 1),
            &[],
            &target,
            Encoding::Gamma8,
        );
        assert!(matches!(err, Error::InvalidSize));

        let err = draw_error(
            &mut renderer,
            &device,
            &queue,
            Picture::Rgba(Plane {
                width: 1,
                height: 1,
                bits: PlaneBits::Sixteen,
                source: PlaneSource::Bytes(&rgba),
            }),
            rect(1, 1),
            &[],
            &target,
            Encoding::Gamma8,
        );
        assert!(matches!(err, Error::InvalidSize));

        let overlays = [Overlay {
            pixels: &red,
            width: 1,
            height: 1,
            dest: PixelRect {
                x: 2,
                y: 0,
                width: 1,
                height: 1,
            },
        }];
        let err = draw_error(
            &mut renderer,
            &device,
            &queue,
            Picture::Yuv {
                y: byte_plane(&y, 2, 1, PlaneBits::Eight),
                u: byte_plane(&chroma, 1, 1, PlaneBits::Eight),
                v: byte_plane(&chroma, 1, 1, PlaneBits::Eight),
            },
            rect(2, 1),
            &overlays,
            &target,
            Encoding::Gamma8,
        );
        assert!(matches!(err, Error::InvalidSize));

        let err = draw_error(
            &mut renderer,
            &device,
            &queue,
            Picture::Yuv {
                y: byte_plane(&y, 2, 1, PlaneBits::Eight),
                u: byte_plane(&chroma, 1, 1, PlaneBits::Eight),
                v: byte_plane(&chroma, 1, 1, PlaneBits::Eight),
            },
            rect(2, 1),
            &[],
            &target,
            Encoding::Linear,
        );
        assert!(matches!(err, Error::Target));

        let err = draw_error(
            &mut renderer,
            &device,
            &queue,
            Picture::Yuv {
                y: byte_plane(&y16, 2, 1, PlaneBits::Sixteen),
                u: byte_plane(&c16, 1, 1, PlaneBits::Sixteen),
                v: byte_plane(&c16, 1, 1, PlaneBits::Sixteen),
            },
            rect(2, 1),
            &[],
            &target,
            Encoding::Gamma8,
        );
        assert!(matches!(err, Error::Gpu));
    }

    fn draw_error(
        renderer: &mut Renderer,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        picture: Picture<'_>,
        dest: PixelRect,
        overlays: &[Overlay<'_>],
        target: &wgpu::Texture,
        encoding: Encoding,
    ) -> Error {
        renderer
            .draw(
                device,
                queue,
                Draw {
                    picture,
                    matrix: Coefficients::Bt709,
                    levels: Levels::Limited,
                    transfer: Transfer::Bt1886,
                    siting: ChromaSiting::TopLeft,
                    peak_nits: 100.0,
                    dest,
                    rotation: QuarterTurn::D0,
                    equalizer: Equalizer::default(),
                    overlays,
                    target,
                    encoding,
                    target_peak_nits: 100.0,
                },
            )
            .expect_err("draw should fail")
    }

    struct Painted {
        cpu: Vec<[f32; 3]>,
        gpu: Vec<[f32; 4]>,
    }

    fn paint_yuv(
        renderer: &mut Renderer,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        y: &[u8],
        yw: u32,
        yh: u32,
        u: &[u8],
        uw: u32,
        uh: u32,
        v: &[u8],
        bits: PlaneBits,
        matrix: Coefficients,
        levels: Levels,
        transfer: Transfer,
        siting: ChromaSiting,
        peak: f32,
        target_peak: f32,
        rotation: QuarterTurn,
        equalizer: Equalizer,
        encoding: Encoding,
        dest: PixelRect,
        overlays: &[Overlay<'_>],
        tol: f32,
    ) -> Painted {
        let decoded = decode_yuv(
            &norm8(y),
            yw,
            yh,
            &norm8(u),
            uw,
            uh,
            &norm8(v),
            matrix,
            levels,
            transfer,
            peak,
            siting,
        );
        let present = job(matrix, transfer, equalizer, encoding, peak, target_peak);
        let width = dest.x.checked_add(dest.width).expect("dest").max(
            overlays
                .iter()
                .map(|item| item.dest.x + item.dest.width)
                .max()
                .unwrap_or(0),
        );
        let height = dest.y.checked_add(dest.height).expect("dest").max(
            overlays
                .iter()
                .map(|item| item.dest.y + item.dest.height)
                .max()
                .unwrap_or(0),
        );
        let texture = target_tex(device, width, height, format_of(encoding));
        let picture = Picture::Yuv {
            y: byte_plane(y, yw, yh, bits),
            u: byte_plane(u, uw, uh, bits),
            v: byte_plane(v, uw, uh, bits),
        };
        renderer
            .draw(
                device,
                queue,
                Draw {
                    picture,
                    matrix,
                    levels,
                    transfer,
                    siting,
                    peak_nits: peak,
                    dest,
                    rotation,
                    equalizer,
                    overlays,
                    target: &texture,
                    encoding,
                    target_peak_nits: target_peak,
                },
            )
            .expect("draw");
        let gpu = read_target(device, queue, &texture);
        assert_placed(&gpu, width, &decoded, yw, yh, dest, rotation, &present, tol);
        let mut cpu = Vec::new();
        for y in 0..dest.height {
            for x in 0..dest.width {
                cpu.push(placed_sample(
                    &decoded,
                    yw,
                    yh,
                    dest.x + x,
                    dest.y + y,
                    dest,
                    rotation,
                    &present,
                ));
            }
        }
        Painted { cpu, gpu }
    }

    fn paint16(
        renderer: &mut Renderer,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        y: &[u16],
        u: &[u16],
        v: &[u16],
    ) {
        let yb = le_bytes(y);
        let ub = le_bytes(u);
        let vb = le_bytes(v);
        let decoded = decode_yuv(
            &norm16(y),
            2,
            1,
            &norm16(u),
            1,
            1,
            &norm16(v),
            Coefficients::Bt709,
            Levels::Full,
            Transfer::Bt1886,
            100.0,
            ChromaSiting::TopLeft,
        );
        let present = job(
            Coefficients::Bt709,
            Transfer::Bt1886,
            Equalizer::default(),
            Encoding::Gamma8,
            100.0,
            100.0,
        );
        let dest = rect(2, 1);
        let texture = target_tex(device, 2, 1, wgpu::TextureFormat::Rgba8Unorm);
        renderer
            .draw(
                device,
                queue,
                Draw {
                    picture: Picture::Yuv {
                        y: byte_plane(&yb, 2, 1, PlaneBits::Sixteen),
                        u: byte_plane(&ub, 1, 1, PlaneBits::Sixteen),
                        v: byte_plane(&vb, 1, 1, PlaneBits::Sixteen),
                    },
                    matrix: Coefficients::Bt709,
                    levels: Levels::Full,
                    transfer: Transfer::Bt1886,
                    siting: ChromaSiting::TopLeft,
                    peak_nits: 100.0,
                    dest,
                    rotation: QuarterTurn::D0,
                    equalizer: Equalizer::default(),
                    overlays: &[],
                    target: &texture,
                    encoding: Encoding::Gamma8,
                    target_peak_nits: 100.0,
                },
            )
            .expect("draw 16");
        let gpu = read_target(device, queue, &texture);
        assert_placed(
            &gpu,
            2,
            &decoded,
            2,
            1,
            dest,
            QuarterTurn::D0,
            &present,
            1.0 / 255.0,
        );
        close(gpu[1][0], 1.0, 1.0 / 255.0);
    }

    fn paint_rgba(
        renderer: &mut Renderer,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        packed: &[u8],
    ) {
        let mut linear = Vec::new();
        for px in packed.chunks(4) {
            linear.push([
                color::eotf(
                    Transfer::Bt1886,
                    color::normalize_code(u32::from(px[0]), 8),
                    100.0,
                ),
                color::eotf(
                    Transfer::Bt1886,
                    color::normalize_code(u32::from(px[1]), 8),
                    100.0,
                ),
                color::eotf(
                    Transfer::Bt1886,
                    color::normalize_code(u32::from(px[2]), 8),
                    100.0,
                ),
            ]);
        }
        let present = job(
            Coefficients::Bt709,
            Transfer::Bt1886,
            Equalizer::default(),
            Encoding::Gamma8,
            100.0,
            100.0,
        );
        let dest = rect(2, 2);
        let texture = target_tex(device, 2, 2, wgpu::TextureFormat::Rgba8Unorm);
        renderer
            .draw(
                device,
                queue,
                Draw {
                    picture: Picture::Rgba(Plane {
                        width: 2,
                        height: 2,
                        bits: PlaneBits::Eight,
                        source: PlaneSource::Bytes(packed),
                    }),
                    matrix: Coefficients::Bt709,
                    levels: Levels::Full,
                    transfer: Transfer::Bt1886,
                    siting: ChromaSiting::TopLeft,
                    peak_nits: 100.0,
                    dest,
                    rotation: QuarterTurn::D0,
                    equalizer: Equalizer::default(),
                    overlays: &[],
                    target: &texture,
                    encoding: Encoding::Gamma8,
                    target_peak_nits: 100.0,
                },
            )
            .expect("draw rgba");
        let gpu = read_target(device, queue, &texture);
        assert_placed(
            &gpu,
            2,
            &linear,
            2,
            2,
            dest,
            QuarterTurn::D0,
            &present,
            1.0 / 255.0,
        );
        close(gpu[0][0], 0.0, 1.0 / 255.0);
    }

    fn paint_yuv_texture(
        renderer: &mut Renderer,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        y: &[u8],
        u: &[u8],
        v: &[u8],
    ) -> Vec<[f32; 4]> {
        let uploaded =
            super::alloc_tex(device, 4, 4, wgpu::TextureFormat::R8Unorm, false).expect("y texture");
        let (padded, stride) = pad_rows(y, 4, 4).expect("pad");
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &uploaded.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &padded,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(stride),
                rows_per_image: Some(4),
            },
            wgpu::Extent3d {
                width: 4,
                height: 4,
                depth_or_array_layers: 1,
            },
        );
        let texture = target_tex(device, 4, 4, wgpu::TextureFormat::Rgba8Unorm);
        renderer
            .draw(
                device,
                queue,
                Draw {
                    picture: Picture::Yuv {
                        y: Plane {
                            width: 4,
                            height: 4,
                            bits: PlaneBits::Eight,
                            source: PlaneSource::Texture(&uploaded.view),
                        },
                        u: byte_plane(u, 2, 2, PlaneBits::Eight),
                        v: byte_plane(v, 2, 2, PlaneBits::Eight),
                    },
                    matrix: Coefficients::Bt709,
                    levels: Levels::Limited,
                    transfer: Transfer::Bt1886,
                    siting: ChromaSiting::Center,
                    peak_nits: 100.0,
                    dest: rect(4, 4),
                    rotation: QuarterTurn::D0,
                    equalizer: Equalizer::default(),
                    overlays: &[],
                    target: &texture,
                    encoding: Encoding::Gamma8,
                    target_peak_nits: 100.0,
                },
            )
            .expect("draw texture plane");
        read_target(device, queue, &texture)
    }

    fn assert_distinct(pixels: &[[f32; 3]]) {
        for (index, px) in pixels.iter().enumerate() {
            for other in pixels.iter().skip(index + 1) {
                let delta =
                    (px[0] - other[0]).abs() + (px[1] - other[1]).abs() + (px[2] - other[2]).abs();
                assert!(delta > 0.02, "{px:?} vs {other:?}");
            }
        }
    }

    fn byte_plane(bytes: &[u8], width: u32, height: u32, bits: PlaneBits) -> Plane<'_> {
        Plane {
            width,
            height,
            bits,
            source: PlaneSource::Bytes(bytes),
        }
    }

    fn le_bytes(codes: &[u16]) -> Vec<u8> {
        let mut out = Vec::with_capacity(codes.len() * 2);
        for code in codes {
            out.extend_from_slice(&code.to_le_bytes());
        }
        out
    }

    fn format_of(encoding: Encoding) -> wgpu::TextureFormat {
        match encoding {
            Encoding::Gamma8 => wgpu::TextureFormat::Rgba8Unorm,
            Encoding::Linear => wgpu::TextureFormat::Rgba16Float,
        }
    }

    fn target_tex(
        device: &wgpu::Device,
        width: u32,
        height: u32,
        format: wgpu::TextureFormat,
    ) -> wgpu::Texture {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some("mpv-wgpu-readback-target"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        })
    }

    fn open_device() -> Result<(wgpu::Device, wgpu::Queue), String> {
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let mut errors = Vec::new();
        let mut opened = None;
        for fallback in [true, false] {
            let adapter = match request_adapter(&instance, fallback) {
                Ok(adapter) => adapter,
                Err(err) => {
                    errors.push(format!("fallback={fallback}: {err}"));
                    continue;
                }
            };
            let features = adapter.features() & wgpu::Features::TEXTURE_FORMAT_16BIT_NORM;
            match open_with(&adapter, features) {
                Ok(gpu) if features.contains(wgpu::Features::TEXTURE_FORMAT_16BIT_NORM) => {
                    return Ok(gpu);
                }
                Ok(gpu) => opened = Some(gpu),
                Err(err) => errors.push(format!("fallback={fallback}: {err}")),
            }
        }
        opened.ok_or_else(|| errors.join("; "))
    }

    fn open_without_16bit() -> Result<(wgpu::Device, wgpu::Queue), String> {
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let mut errors = Vec::new();
        for fallback in [true, false] {
            let adapter = match request_adapter(&instance, fallback) {
                Ok(adapter) => adapter,
                Err(err) => {
                    errors.push(format!("fallback={fallback}: {err}"));
                    continue;
                }
            };
            match open_with(&adapter, wgpu::Features::empty()) {
                Ok(gpu) => return Ok(gpu),
                Err(err) => errors.push(format!("fallback={fallback}: {err}")),
            }
        }
        Err(errors.join("; "))
    }

    fn open_with(
        adapter: &wgpu::Adapter,
        features: wgpu::Features,
    ) -> Result<(wgpu::Device, wgpu::Queue), String> {
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("mpv-wgpu-picture-test"),
            required_features: features,
            required_limits: wgpu::Limits::default(),
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            memory_hints: wgpu::MemoryHints::Performance,
            trace: wgpu::Trace::Off,
        }))
        .map_err(|err| err.to_string())
    }

    fn request_adapter(instance: &wgpu::Instance, fallback: bool) -> Result<wgpu::Adapter, String> {
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: fallback,
            apply_limit_buckets: false,
        }))
        .map_err(|err| err.to_string())
    }

    fn read_target(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        texture: &wgpu::Texture,
    ) -> Vec<[f32; 4]> {
        let width = texture.size().width;
        let height = texture.size().height;
        let bpp = match texture.format() {
            wgpu::TextureFormat::Rgba8Unorm => 4,
            wgpu::TextureFormat::Rgba16Float => 8,
            _ => panic!("unsupported readback format"),
        };
        let tight = width * bpp;
        let stride = tight.div_ceil(256) * 256;
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mpv-wgpu-readback"),
            size: u64::from(stride) * u64::from(height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("mpv-wgpu-readback"),
        });
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(stride),
                    rows_per_image: Some(height),
                },
            },
            texture.size(),
        );
        queue.submit([encoder.finish()]);
        let (send, recv) = std::sync::mpsc::channel();
        buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = send.send(result);
            });
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("poll readback");
        recv.recv().expect("map callback").expect("map buffer");
        let view = buffer.slice(..).get_mapped_range().expect("mapped range");
        let mut pixels = Vec::with_capacity((width * height) as usize);
        for row in 0..height as usize {
            let start = row * stride as usize;
            let row_bytes = &view[start..start + tight as usize];
            if bpp == 4 {
                for px in row_bytes.chunks(4) {
                    pixels.push([
                        px[0] as f32 / 255.0,
                        px[1] as f32 / 255.0,
                        px[2] as f32 / 255.0,
                        px[3] as f32 / 255.0,
                    ]);
                }
            } else {
                for px in row_bytes.chunks(8) {
                    pixels.push([
                        f16_to_f32(u16::from_le_bytes([px[0], px[1]])),
                        f16_to_f32(u16::from_le_bytes([px[2], px[3]])),
                        f16_to_f32(u16::from_le_bytes([px[4], px[5]])),
                        f16_to_f32(u16::from_le_bytes([px[6], px[7]])),
                    ]);
                }
            }
        }
        drop(view);
        buffer.unmap();
        pixels
    }

    fn f16_to_f32(bits: u16) -> f32 {
        let sign = (bits >> 15) & 1;
        let exp = (bits >> 10) & 0x1f;
        let frac = bits & 0x3ff;
        if exp == 0 {
            let mag = f32::from(frac) / 1024.0 * 2.0_f32.powi(-14);
            return if sign == 1 { -mag } else { mag };
        }
        if exp == 31 {
            if frac == 0 {
                return if sign == 1 {
                    f32::NEG_INFINITY
                } else {
                    f32::INFINITY
                };
            }
            return f32::NAN;
        }
        let exp_f32 = u32::from(exp) + 127 - 15;
        f32::from_bits((u32::from(sign) << 31) | (exp_f32 << 23) | (u32::from(frac) << 13))
    }
}
