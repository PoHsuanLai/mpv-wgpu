//! Draws planar frames into a caller texture and prints Gamma8 codes.

use mpv_wgpu::{
    ChromaSiting, Coefficients, Draw, Encoding, Equalizer, Levels, Picture, PixelRect, Plane,
    PlaneBits, PlaneSource, QuarterTurn, Renderer, Transfer,
};

fn main() {
    if let Err(err) = run() {
        eprintln!("planes-error={err}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let (device, queue) = open_device()?;
    let mut renderer = Renderer::new(&device)?;
    let neutral8 = [128u8];
    let black = yuv_code(
        &mut renderer,
        &device,
        &queue,
        &[16, 16, 16, 16],
        &neutral8,
        PlaneBits::Eight,
        Levels::Limited,
    )?;
    let white = yuv_code(
        &mut renderer,
        &device,
        &queue,
        &[235, 235, 235, 235],
        &neutral8,
        PlaneBits::Eight,
        Levels::Limited,
    )?;
    let full16 = yuv_code(
        &mut renderer,
        &device,
        &queue,
        &le_codes(&[65535, 65535, 65535, 65535]),
        &32768u16.to_le_bytes(),
        PlaneBits::Sixteen,
        Levels::Full,
    )?;
    let rgba = rgba_code(
        &mut renderer,
        &device,
        &queue,
        &[0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255],
    )?;
    println!("black={black}");
    println!("white={white}");
    println!("full16={full16}");
    println!("rgba={rgba}");
    Ok(())
}

fn yuv_code(
    renderer: &mut Renderer,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    y: &[u8],
    chroma: &[u8],
    bits: PlaneBits,
    levels: Levels,
) -> Result<u8, Box<dyn std::error::Error>> {
    let y_plane = Plane {
        width: 2,
        height: 2,
        bits,
        source: PlaneSource::Bytes(y),
    };
    let u_plane = Plane {
        width: 1,
        height: 1,
        bits,
        source: PlaneSource::Bytes(chroma),
    };
    let v_plane = Plane {
        width: 1,
        height: 1,
        bits,
        source: PlaneSource::Bytes(chroma),
    };
    let target = target(device);
    renderer.draw(
        device,
        queue,
        Draw {
            picture: Picture::Yuv {
                y: y_plane,
                u: u_plane,
                v: v_plane,
            },
            matrix: Coefficients::Bt709,
            levels,
            transfer: Transfer::Bt1886,
            siting: ChromaSiting::TopLeft,
            peak_nits: 100.0,
            dest: full_rect(),
            rotation: QuarterTurn::D0,
            equalizer: Equalizer::default(),
            overlays: &[],
            target: &target,
            encoding: Encoding::Gamma8,
            target_peak_nits: 100.0,
        },
    )?;
    Ok(first_code(device, queue, &target)?)
}

fn rgba_code(
    renderer: &mut Renderer,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    packed: &[u8],
) -> Result<u8, Box<dyn std::error::Error>> {
    let target = target(device);
    renderer.draw(
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
            dest: full_rect(),
            rotation: QuarterTurn::D0,
            equalizer: Equalizer::default(),
            overlays: &[],
            target: &target,
            encoding: Encoding::Gamma8,
            target_peak_nits: 100.0,
        },
    )?;
    Ok(first_code(device, queue, &target)?)
}

fn full_rect() -> PixelRect {
    PixelRect {
        x: 0,
        y: 0,
        width: 2,
        height: 2,
    }
}

fn target(device: &wgpu::Device) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("planes-target"),
        size: wgpu::Extent3d {
            width: 2,
            height: 2,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    })
}

fn first_code(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
) -> Result<u8, Box<dyn std::error::Error>> {
    let stride = 256u32;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("planes-readback"),
        size: u64::from(stride) * 2,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("planes-readback"),
    });
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(stride),
                rows_per_image: Some(2),
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
    device.poll(wgpu::PollType::wait_indefinitely())?;
    recv.recv()??;
    let view = buffer.slice(..).get_mapped_range()?;
    let code = view[0];
    drop(view);
    buffer.unmap();
    Ok(code)
}

fn le_codes(codes: &[u16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(codes.len() * 2);
    for code in codes {
        out.extend_from_slice(&code.to_le_bytes());
    }
    out
}

fn open_device() -> Result<(wgpu::Device, wgpu::Queue), Box<dyn std::error::Error>> {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let mut errors = Vec::new();
    let mut opened = None;
    for fallback in [true, false] {
        let adapter =
            match pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: None,
                force_fallback_adapter: fallback,
                apply_limit_buckets: false,
            })) {
                Ok(adapter) => adapter,
                Err(err) => {
                    errors.push(format!("fallback={fallback}: {err}"));
                    continue;
                }
            };
        let features = adapter.features() & wgpu::Features::TEXTURE_FORMAT_16BIT_NORM;
        let device = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("planes"),
            required_features: features,
            required_limits: wgpu::Limits::default(),
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            memory_hints: wgpu::MemoryHints::Performance,
            trace: wgpu::Trace::Off,
        }));
        match device {
            Ok(gpu) if features.contains(wgpu::Features::TEXTURE_FORMAT_16BIT_NORM) => {
                return Ok(gpu);
            }
            Ok(gpu) => opened = Some(gpu),
            Err(err) => errors.push(format!("fallback={fallback}: {err}")),
        }
    }
    opened.ok_or_else(|| errors.join("; ").into())
}
