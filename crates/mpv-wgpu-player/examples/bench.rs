//! Headless throughput check: play a file for a few seconds and count the frames
//! the player uploads, in either host mode.
//!
//! ```sh
//! RUST_LOG=info cargo run --release --example bench -- clip.mkv 1920 1080 8 subprocess
//! ```
//!
//! Arguments: file, slot width, slot height, seconds, `in-process` or `subprocess`.
//! In subprocess mode the log lines carry `dropped` and `delivery_*_us`, the time a
//! frame waited between the plugin finishing its render and the upload.

use std::num::NonZeroU32;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use mpv_wgpu_player::{
    AudioOutput, Host, Picture, Player, PlayerOptions, Presentation, Slot, SlotSize,
};

fn main() {
    if let Err(error) = run() {
        eprintln!("bench-error={error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();
    let mut args = std::env::args().skip(1);
    let path = args.next().ok_or("usage: bench FILE W H SECONDS MODE")?;
    let width: u32 = args.next().ok_or("width")?.parse()?;
    let height: u32 = args.next().ok_or("height")?.parse()?;
    let seconds: u64 = args.next().ok_or("seconds")?.parse()?;
    let host = match args.next().as_deref() {
        Some("in-process") => Host::InProcess,
        _ => Host::Subprocess(Default::default()),
    };

    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
    }))?;
    println!("adapter={}", adapter.get_info().name);
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("bench"),
        required_features: wgpu::Features::empty(),
        required_limits: wgpu::Limits::default(),
        experimental_features: wgpu::ExperimentalFeatures::disabled(),
        memory_hints: wgpu::MemoryHints::Performance,
        trace: wgpu::Trace::Off,
    }))?;

    let mut player = Player::with_host(
        &device,
        &queue,
        PlayerOptions {
            audio_output: AudioOutput::Null,
        },
        host,
    )?;
    let (wake, woken) = mpsc::channel::<()>();
    player.set_notify(move || {
        let _ = wake.send(());
    });
    player.set_slot(Slot::Sized(SlotSize {
        width: NonZeroU32::new(width).ok_or("width")?,
        height: NonZeroU32::new(height).ok_or("height")?,
    }))?;
    let started = Instant::now();
    player.load(&path)?;

    let mut updates = 0u64;
    let mut first = None;
    let mut window_start = Instant::now();
    let mut window_updates = 0u64;
    while started.elapsed() < Duration::from_secs(seconds) {
        let _ = woken.recv_timeout(Duration::from_millis(100));
        let outcome = player.poll()?;
        if outcome.presentation == Presentation::Updated
            && matches!(player.picture(), Picture::Shown(_))
        {
            first.get_or_insert_with(|| started.elapsed());
            updates += 1;
            window_updates += 1;
        }
        if window_start.elapsed() >= Duration::from_secs(1) {
            println!(
                "fps={:.1} position={:?}",
                window_updates as f64 / window_start.elapsed().as_secs_f64(),
                player.position().map(|p| p.get())
            );
            window_start = Instant::now();
            window_updates = 0;
        }
    }
    let wall = started.elapsed() - first.unwrap_or_default();
    println!(
        "first_frame_ms={} updates={updates} mean_fps={:.1}",
        first.map_or(0, |d| d.as_millis()),
        updates as f64 / wall.as_secs_f64()
    );
    Ok(())
}
