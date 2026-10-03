//! Shared harness: a headless player on `ao=null`, driven by polling.
#![allow(dead_code)]

use std::num::NonZeroU32;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use mpv_wgpu_player::{
    AudioOutput, Event, Host, Player, PlayerOptions, Slot, SlotSize, SubprocessOptions,
};

const TIMEOUT: Duration = Duration::from_secs(20);

/// Where the player's mpv core runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// libmpv linked into the test process.
    InProcess,
    /// The stock `mpv` binary with the plugin loaded.
    Subprocess,
}

/// Run the test function `$name(Mode)` once per compiled-in mode, as `$name::in_process`
/// and `$name::subprocess`.
macro_rules! in_each_mode {
    ($($name:ident),+ $(,)?) => {
        $(
            mod $name {
                #[cfg(feature = "in-process")]
                #[test]
                fn in_process() {
                    super::$name($crate::support::Mode::InProcess);
                }

                #[cfg(feature = "subprocess")]
                #[test]
                fn subprocess() {
                    super::$name($crate::support::Mode::Subprocess);
                }
            }
        )+
    };
}

/// The `mpv` the subprocess tests run: `MPV_WGPU_MPV`, else `mpv` on `PATH`.
pub fn find_mpv() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("MPV_WGPU_MPV").filter(|p| !p.is_empty()) {
        let path = PathBuf::from(path);
        return path.is_file().then_some(path);
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join("mpv"))
        .find(|candidate| candidate.is_file())
}

/// Subprocess options for the tests, or `None` (after saying so) when there is no mpv.
pub fn subprocess_host() -> Option<Host> {
    let Some(mpv) = find_mpv() else {
        eprintln!("skipped: no mpv (set MPV_WGPU_MPV or put mpv on PATH)");
        return None;
    };
    Some(Host::Subprocess(SubprocessOptions {
        mpv: Some(mpv),
        ..SubprocessOptions::default()
    }))
}

/// A player plus every event it has produced since the harness was built.
pub struct Harness {
    pub player: Player,
    pub events: Vec<Event>,
}

/// Fixture path under `tests/fixtures`.
pub fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

/// A scratch directory unique to this process and `name`.
pub fn scratch_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mpv-wgpu-player-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

/// One wgpu device for the whole test process. The Vulkan loader is not safe to
/// start from several threads at once, and the tests run in parallel.
pub fn open_device() -> Result<(wgpu::Device, wgpu::Queue), String> {
    static GPU: OnceLock<Result<(wgpu::Device, wgpu::Queue), String>> = OnceLock::new();
    GPU.get_or_init(create_device).clone()
}

fn create_device() -> Result<(wgpu::Device, wgpu::Queue), String> {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let mut errors = Vec::new();
    for fallback in [true, false] {
        let adapter =
            match pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: None,
                force_fallback_adapter: fallback,
            })) {
                Ok(adapter) => adapter,
                Err(err) => {
                    errors.push(format!("fallback={fallback}: {err}"));
                    continue;
                }
            };
        match pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("mpv-wgpu-player-test"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            memory_hints: wgpu::MemoryHints::Performance,
            trace: wgpu::Trace::Off,
        })) {
            Ok(gpu) => return Ok(gpu),
            Err(err) => errors.push(format!("fallback={fallback}: {err}")),
        }
    }
    Err(errors.join("; "))
}

impl Harness {
    /// A player on the null audio output, or `None` when no wgpu adapter exists
    /// (or, in subprocess mode, no mpv).
    ///
    /// The tests return early on `None`, so a machine without any adapter
    /// (no Vulkan or GL driver) passes without exercising them.
    pub fn open(mode: Mode) -> Option<Self> {
        let host = match mode {
            Mode::InProcess => Host::InProcess,
            Mode::Subprocess => subprocess_host()?,
        };
        let (device, queue) = match open_device() {
            Ok(gpu) => gpu,
            Err(err) => {
                eprintln!("gpu-device-unavailable: {err}");
                return None;
            }
        };
        let player = Player::with_host(
            &device,
            &queue,
            PlayerOptions {
                audio_output: AudioOutput::Null,
            },
            host,
        )
        .expect("player");
        Some(Self {
            player,
            events: Vec::new(),
        })
    }

    /// Give mpv a 64x48 slot to render into.
    pub fn with_slot(self) -> Self {
        self.with_slot_of(64, 48)
    }

    /// Give mpv a `width` x `height` slot to render into.
    pub fn with_slot_of(mut self, width: u32, height: u32) -> Self {
        self.player
            .set_slot(Slot::Sized(SlotSize {
                width: NonZeroU32::new(width).expect("non-zero"),
                height: NonZeroU32::new(height).expect("non-zero"),
            }))
            .expect("slot");
        self
    }

    /// Poll once and keep the events.
    pub fn step(&mut self) {
        let _ = self.player.poll().expect("poll");
        self.events.extend_from_slice(self.player.events());
    }

    /// Poll until `done` holds, or panic naming `what` after the timeout.
    pub fn wait_for(&mut self, what: &str, done: impl Fn(&Harness) -> bool) {
        let started = Instant::now();
        loop {
            self.step();
            if done(self) {
                return;
            }
            assert!(
                started.elapsed() < TIMEOUT,
                "timed out waiting for {what}; events so far: {:?}",
                self.events
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Poll for `span`, whatever happens.
    pub fn run_for(&mut self, span: Duration) {
        let started = Instant::now();
        while started.elapsed() < span {
            self.step();
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Whether any collected event equals `event`.
    pub fn saw(&self, event: &Event) -> bool {
        self.events.contains(event)
    }

    /// Forget the events collected so far.
    pub fn clear_events(&mut self) {
        self.events.clear();
    }

    /// Load `name` and poll until mpv has opened it.
    pub fn load(&mut self, name: &str) {
        let path = fixture(name);
        let text = path.to_str().expect("utf-8 fixture path");
        self.player.load(text).expect("loadfile");
        self.wait_for("Event::Loaded", |h| h.saw(&Event::Loaded));
    }
}
