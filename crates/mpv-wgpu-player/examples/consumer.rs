//! Fresh consumer of the public control surface. Prints read-backs, no window.

use std::num::NonZeroU32;

use mpv_wgpu_player::{
    Adjust, Deinterlace, Equalizer, Finite, Hue, Mute, Playback, Slot, SlotSize, UnitBias,
};

fn main() {
    let result = run();
    if let Err(err) = result {
        eprintln!("consumer-error={err}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: true,
    }))?;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("consumer"),
        required_features: wgpu::Features::empty(),
        required_limits: wgpu::Limits::default(),
        experimental_features: wgpu::ExperimentalFeatures::disabled(),
        memory_hints: wgpu::MemoryHints::Performance,
        trace: wgpu::Trace::Off,
    }))?;

    let mut player = mpv_wgpu_player::Player::new(&device, &queue)?;
    player.set_playback(Playback::Paused)?;
    println!("playback={}", player.playback());

    player.set_mute(Mute::On)?;
    println!("mute={}", player.mute());

    player.set_deinterlace(Deinterlace::Yes)?;
    println!("deinterlace={}", player.deinterlace());

    let width = NonZeroU32::new(320).ok_or("width")?;
    let height = NonZeroU32::new(240).ok_or("height")?;
    player.set_slot(Slot::Sized(SlotSize { width, height }))?;
    match player.slot() {
        Slot::Sized(size) => println!("slot=sized:{}x{}", size.width, size.height),
        Slot::Empty => println!("slot=empty"),
    }
    player.set_slot(Slot::Empty)?;
    match player.slot() {
        Slot::Sized(size) => println!("slot=sized:{}x{}", size.width, size.height),
        Slot::Empty => println!("slot=empty"),
    }

    let equalizer = Equalizer {
        brightness: UnitBias::new(10_000),
        contrast: UnitBias::new(-10_000),
        saturation: UnitBias::new(0),
        gamma: UnitBias::new(0),
        hue: Hue::new(10_000),
    };
    player.set_equalizer(equalizer)?;
    println!("equalizer={}", player.equalizer());

    match Finite::new(f64::NAN) {
        Some(_) => println!("seek_nonfinite=accepted"),
        None => println!("seek_nonfinite=rejected"),
    }

    let Some(step) = Finite::new(1.0) else {
        return Err("1.0 was not finite".into());
    };
    if let Some(path) = std::env::args().nth(1) {
        player.load(&path)?;
        for _ in 0..100 {
            let _ = player.poll()?;
            let loaded = player
                .events()
                .iter()
                .any(|event| matches!(event, mpv_wgpu_player::Event::Loaded));
            if loaded {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }
    player.seek(mpv_wgpu_player::Seek::Relative(step))?;
    println!("seek=ok");
    let _ = Adjust::Volume(step);
    Ok(())
}
