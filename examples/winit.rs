//! Dev-only window that samples [`mpv_wgpu::Player`] and binds keys.

use std::num::NonZeroU32;
use std::sync::Arc;

use mpv_wgpu::{
    Adjust, Deinterlace, Equalizer, Finite, Mute, Picture, Playback, Presentation, Slot, SlotSize,
    UnitBias,
};
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{Window, WindowId};

enum Wake {
    Poll,
}

struct Host {
    path: String,
    proxy: EventLoopProxy<Wake>,
    modifiers: ModifiersState,
    window: Option<Arc<Window>>,
    gpu: Option<Gpu>,
    player: Option<mpv_wgpu::Player>,
    logged_shown: bool,
    started: std::time::Instant,
}

struct Gpu {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    blit: Blit,
}

struct Blit {
    layout: wgpu::BindGroupLayout,
    pipeline: wgpu::RenderPipeline,
    sampler: wgpu::Sampler,
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let path = std::env::args().nth(1).unwrap_or_else(|| {
        eprintln!("usage: winit <media>");
        std::process::exit(2);
    });
    let event_loop = EventLoop::<Wake>::with_user_event()
        .build()
        .unwrap_or_else(|err| {
            eprintln!("event-loop={err}");
            std::process::exit(1);
        });
    let proxy = event_loop.create_proxy();
    let mut host = Host {
        path,
        proxy,
        modifiers: ModifiersState::empty(),
        window: None,
        gpu: None,
        player: None,
        logged_shown: false,
        started: std::time::Instant::now(),
    };
    if let Err(err) = event_loop.run_app(&mut host) {
        eprintln!("run={err}");
        std::process::exit(1);
    }
}

impl ApplicationHandler<Wake> for Host {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let window = match event_loop.create_window(
            Window::default_attributes().with_title("mpv-wgpu").with_inner_size(PhysicalSize::new(960, 540)),
        ) {
            Ok(window) => Arc::new(window),
            Err(err) => {
                eprintln!("window={err}");
                event_loop.exit();
                return;
            }
        };
        let gpu = match Gpu::new(Arc::clone(&window)) {
            Ok(gpu) => gpu,
            Err(err) => {
                eprintln!("gpu={err}");
                event_loop.exit();
                return;
            }
        };
        let mut player = match mpv_wgpu::Player::new(&gpu.device, &gpu.queue) {
            Ok(player) => player,
            Err(err) => {
                eprintln!("player={err}");
                event_loop.exit();
                return;
            }
        };
        let proxy = self.proxy.clone();
        player.set_notify(move || {
            let _ = proxy.send_event(Wake::Poll);
        });
        if let Err(err) = apply_slot(&mut player, window.inner_size()) {
            eprintln!("slot={err}");
            event_loop.exit();
            return;
        }
        if let Err(err) = player.load(&self.path) {
            eprintln!("load={err}");
            event_loop.exit();
            return;
        }
        self.window = Some(window);
        self.gpu = Some(gpu);
        self.player = Some(player);
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, _event: Wake) {
        if self.poll_player() {
            event_loop.exit();
            return;
        }
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::ModifiersChanged(modifiers) => {
                self.modifiers = modifiers.state();
            }
            WindowEvent::Resized(size) => {
                if let Some(gpu) = &mut self.gpu {
                    gpu.resize(size);
                }
                if let Some(player) = &mut self.player
                    && let Err(err) = apply_slot(player, size)
                {
                    eprintln!("slot={err}");
                }
                if self.poll_player() {
                    event_loop.exit();
                }
            }
            WindowEvent::KeyboardInput {
                event: key,
                ..
            } => self.key(event_loop, key),
            WindowEvent::RedrawRequested => self.redraw(),
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let played_long_enough =
            self.logged_shown && self.started.elapsed() > std::time::Duration::from_secs(8);
        if self.poll_player() || played_long_enough {
            event_loop.exit();
            return;
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(
            std::time::Instant::now() + std::time::Duration::from_millis(50),
        ));
    }
}

impl Host {
    fn poll_player(&mut self) -> bool {
        let Some(player) = &mut self.player else {
            return false;
        };
        match player.poll() {
            Ok(outcome) => {
                if matches!(outcome.presentation, Presentation::Updated)
                    && matches!(player.picture(), Picture::Shown(_))
                    && !self.logged_shown
                {
                    let slot = match player.slot() {
                        Slot::Sized(size) => format!("{}x{}", size.width, size.height),
                        Slot::Empty => "empty".to_string(),
                    };
                    println!("slot={slot} presentation=updated picture=shown");
                    self.logged_shown = true;
                }
                player
                    .events()
                    .iter()
                    .any(|event| matches!(event, mpv_wgpu::Event::Ended(_)))
            }
            Err(err) => {
                eprintln!("poll={err}");
                false
            }
        }
    }

    fn key(&mut self, event_loop: &ActiveEventLoop, event: KeyEvent) {
        if event.state != ElementState::Pressed || event.repeat {
            return;
        }
        let Some(player) = &self.player else {
            return;
        };
        let alt = self.modifiers.alt_key();
        let result = match &event.logical_key {
            Key::Named(NamedKey::Space) => {
                let next = match player.playback() {
                    Playback::Playing => Playback::Paused,
                    Playback::Paused => Playback::Playing,
                };
                player.set_playback(next)
            }
            Key::Named(NamedKey::ArrowLeft) => seek(player, -5.0),
            Key::Named(NamedKey::ArrowRight) => seek(player, 5.0),
            Key::Named(NamedKey::Escape) => {
                event_loop.exit();
                return;
            }
            Key::Character(text) if text == "q" => {
                event_loop.exit();
                return;
            }
            Key::Character(text) if text == "f" => {
                if let Some(window) = &self.window {
                    let next = if window.fullscreen().is_some() {
                        None
                    } else {
                        Some(winit::window::Fullscreen::Borderless(None))
                    };
                    window.set_fullscreen(next);
                }
                return;
            }
            Key::Character(text) if text == "w" => adjust(player, Adjust::Panscan, -0.1),
            Key::Character(text) if text == "e" => adjust(player, Adjust::Panscan, 0.1),
            Key::Character(text) if text == "=" && alt => adjust(player, Adjust::Zoom, 0.1),
            Key::Character(text) if text == "-" && alt => adjust(player, Adjust::Zoom, -0.1),
            Key::Character(text) if text == "1" => bump_eq(player, EqField::Contrast, -1),
            Key::Character(text) if text == "2" => bump_eq(player, EqField::Contrast, 1),
            Key::Character(text) if text == "3" => bump_eq(player, EqField::Brightness, -1),
            Key::Character(text) if text == "4" => bump_eq(player, EqField::Brightness, 1),
            Key::Character(text) if text == "5" => bump_eq(player, EqField::Gamma, -1),
            Key::Character(text) if text == "6" => bump_eq(player, EqField::Gamma, 1),
            Key::Character(text) if text == "7" => bump_eq(player, EqField::Saturation, -1),
            Key::Character(text) if text == "8" => bump_eq(player, EqField::Saturation, 1),
            Key::Character(text) if text == "d" => {
                let next = match player.deinterlace() {
                    Deinterlace::No => Deinterlace::Yes,
                    Deinterlace::Yes => Deinterlace::Auto,
                    Deinterlace::Auto => Deinterlace::No,
                };
                player.set_deinterlace(next)
            }
            Key::Character(text) if text == "9" => adjust(player, Adjust::Volume, -2.0),
            Key::Character(text) if text == "0" => adjust(player, Adjust::Volume, 2.0),
            Key::Character(text) if text == "m" => {
                let next = match player.mute() {
                    Mute::Off => Mute::On,
                    Mute::On => Mute::Off,
                };
                player.set_mute(next)
            }
            _ => return,
        };
        if let Err(err) = result {
            eprintln!("key={err}");
        }
    }

    fn redraw(&mut self) {
        let (Some(gpu), Some(player)) = (&mut self.gpu, &self.player) else {
            return;
        };
        let Picture::Shown(frame) = player.picture() else {
            return;
        };
        match gpu.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(surface)
            | wgpu::CurrentSurfaceTexture::Suboptimal(surface) => {
                let view = surface.texture.create_view(&wgpu::TextureViewDescriptor {
                    format: Some(gpu.config.format),
                    ..Default::default()
                });
                gpu.blit.draw(&gpu.device, &gpu.queue, frame, &view);
                gpu.queue.present(surface);
            }
            other => eprintln!("surface={other:?}"),
        }
    }
}

enum EqField {
    Contrast,
    Brightness,
    Gamma,
    Saturation,
}

fn seek(player: &mpv_wgpu::Player, seconds: f64) -> Result<(), mpv_wgpu::Error> {
    let Some(amount) = Finite::new(seconds) else {
        return Ok(());
    };
    player.seek(mpv_wgpu::Seek::Relative(amount))
}

fn adjust(
    player: &mpv_wgpu::Player,
    kind: impl FnOnce(Finite) -> Adjust,
    delta: f64,
) -> Result<(), mpv_wgpu::Error> {
    let Some(amount) = Finite::new(delta) else {
        return Ok(());
    };
    player.adjust(kind(amount))
}

fn bump_eq(player: &mpv_wgpu::Player, field: EqField, delta: i32) -> Result<(), mpv_wgpu::Error> {
    let mut eq = player.equalizer();
    match field {
        EqField::Contrast => eq.contrast = UnitBias::new(eq.contrast.get().saturating_add(delta)),
        EqField::Brightness => {
            eq.brightness = UnitBias::new(eq.brightness.get().saturating_add(delta))
        }
        EqField::Gamma => eq.gamma = UnitBias::new(eq.gamma.get().saturating_add(delta)),
        EqField::Saturation => {
            eq.saturation = UnitBias::new(eq.saturation.get().saturating_add(delta))
        }
    }
    let _ = Equalizer::default();
    player.set_equalizer(eq)
}

fn apply_slot(player: &mut mpv_wgpu::Player, size: PhysicalSize<u32>) -> Result<(), mpv_wgpu::Error> {
    let slot = match (NonZeroU32::new(size.width), NonZeroU32::new(size.height)) {
        (Some(width), Some(height)) => Slot::Sized(SlotSize { width, height }),
        _ => Slot::Empty,
    };
    player.set_slot(slot)
}

impl Gpu {
    fn new(window: Arc<Window>) -> Result<Self, Box<dyn std::error::Error>> {
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let surface = instance.create_surface(window.clone())?;
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
            apply_limit_buckets: false,
        }))?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("example"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            memory_hints: wgpu::MemoryHints::Performance,
            trace: wgpu::Trace::Off,
        }))?;
        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|format| {
                matches!(
                    format,
                    wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Rgba8Unorm
                )
            })
            .unwrap_or(caps.formats[0]);
        let size = window.inner_size();
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::Fifo,
            desired_maximum_frame_latency: 2,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            color_space: wgpu::SurfaceColorSpace::Auto,
        };
        surface.configure(&device, &config);
        let blit = Blit::new(&device, format);
        Ok(Self {
            surface,
            device,
            queue,
            config,
            blit,
        })
    }

    fn resize(&mut self, size: PhysicalSize<u32>) {
        if size.width == 0 || size.height == 0 {
            return;
        }
        self.config.width = size.width;
        self.config.height = size.height;
        self.surface.configure(&self.device, &self.config);
    }
}

impl Blit {
    fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("blit"),
            source: wgpu::ShaderSource::Wgsl(
                r#"
@group(0) @binding(0) var t: texture_2d<f32>;
@group(0) @binding(1) var s: sampler;

struct Out {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs(@builtin(vertex_index) i: u32) -> Out {
    let uv = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    var o: Out;
    o.pos = vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.0, 1.0);
    o.uv = uv;
    return o;
}

@fragment
fn fs(in: Out) -> @location(0) vec4<f32> {
    return textureSample(t, s, in.uv);
}
"#
                .into(),
            ),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("blit"),
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
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("blit"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("blit"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
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
            label: Some("blit"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });
        Self {
            layout,
            pipeline,
            sampler,
        }
    }

    fn draw(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        frame: &wgpu::TextureView,
        target: &wgpu::TextureView,
    ) {
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("blit"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(frame),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("blit"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("blit"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
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
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.draw(0..3, 0..1);
        }
        queue.submit(std::iter::once(encoder.finish()));
    }
}
