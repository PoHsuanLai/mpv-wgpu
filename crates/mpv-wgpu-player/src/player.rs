//! libmpv core, software frames, and the wgpu picture.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use rsmpv::render::{FrameInfo, OwnedRenderContext, SwPixelFormat};
use rsmpv::{EndFileReason, Event as MpvEvent, Format, Mpv, PropertyData};

use mpv_wgpu::{Coefficients, Equalizer, Hue, UnitBias, bake};

use crate::frame_buffer::FrameBuffer;
use crate::options::PlayerOptions;
use crate::pipeline::{Gpu, Pipeline};
use crate::types::{
    Adjust, Deinterlace, EndReason, Error, Event, Finite, Mute, Outcome, Picture, Playback,
    Presentation, Slot, SlotSize, map_mpv,
};

const WAKE_IDLE: u8 = 0;
const WAKE_PENDING: u8 = 1;

struct Notify {
    wake: AtomicU8,
    callback: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

impl Notify {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            wake: AtomicU8::new(WAKE_IDLE),
            callback: Mutex::new(None),
        })
    }

    fn signal(&self) {
        self.wake.store(WAKE_PENDING, Ordering::Release);
        let callback = lock(&self.callback).clone();
        if let Some(callback) = callback {
            callback();
        }
    }
}

enum Shown {
    Waiting,
    Current,
}

enum Stage {
    NoSlot,
    Live {
        size: SlotSize,
        gpu: Box<Gpu>,
        shown: Shown,
    },
}

enum NextFrame {
    Absent,
    Repeat,
    Redraw,
    New,
}

enum TransferNote {
    Unseen,
    Sdr,
    Hdr,
}

enum HwdecLog {
    Unseen,
    Logged,
}

enum Freshness {
    Clean,
    Dirty,
}

struct Shared {
    playback: Playback,
    mute: Mute,
    deinterlace: Deinterlace,
    equalizer: Equalizer,
    position: Option<Finite>,
    duration: Option<Finite>,
    coefficients: Coefficients,
    transfer: TransferNote,
    hwdec: HwdecLog,
    grade: Freshness,
    slot_repaint: Freshness,
}

struct Stats {
    frames: u64,
    repeats: u64,
    bytes: u64,
    software: std::time::Duration,
    upload: std::time::Duration,
    window: Instant,
}

/// One libmpv player and the texture a host samples.
///
/// `Player` is [`Send`] and not [`Sync`]. Call [`Player::poll`] on the thread
/// that presents. The notify closure may run on an mpv thread and must only
/// wake the host.
pub struct Player {
    core: Arc<Mpv>,
    render: OwnedRenderContext,
    notify: Arc<Notify>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    frames: FrameBuffer,
    pipeline: Pipeline,
    stage: Stage,
    shared: Mutex<Shared>,
    events: Vec<Event>,
    stats: Stats,
}

impl Player {
    /// Start a headless libmpv core on `device` / `queue`.
    ///
    /// `options` picks the audio driver; [`PlayerOptions::default`] lets mpv probe.
    ///
    /// The core is idle until [`Player::load`]. Register [`Player::set_notify`]
    /// before relying on wakes; a wake that arrives first is remembered.
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        options: PlayerOptions,
    ) -> Result<Self, Error> {
        let core = build_core(options)?;
        let core = Arc::new(core);
        let mut render = OwnedRenderContext::new_software(Arc::clone(&core)).map_err(map_mpv)?;
        let notify = Notify::new();
        let signal_target = Arc::clone(&notify);
        render.set_update_callback(move || signal_target.signal());
        let wake_target = Arc::clone(&notify);
        core.set_wakeup_callback(move || wake_target.signal());
        let _ = core.request_log_messages("warn");
        observe(&core)?;
        let pipeline = Pipeline::new(device)?;
        Ok(Self {
            core,
            render,
            notify,
            device: device.clone(),
            queue: queue.clone(),
            frames: FrameBuffer::new(),
            pipeline,
            stage: Stage::NoSlot,
            shared: Mutex::new(Shared {
                playback: Playback::Playing,
                mute: Mute::Off,
                deinterlace: Deinterlace::Auto,
                equalizer: Equalizer::default(),
                position: None,
                duration: None,
                coefficients: Coefficients::Bt709,
                transfer: TransferNote::Unseen,
                hwdec: HwdecLog::Unseen,
                grade: Freshness::Clean,
                slot_repaint: Freshness::Clean,
            }),
            events: Vec::new(),
            stats: Stats {
                frames: 0,
                repeats: 0,
                bytes: 0,
                software: std::time::Duration::ZERO,
                upload: std::time::Duration::ZERO,
                window: Instant::now(),
            },
        })
    }

    /// Replace the wake closure. It may run on an mpv thread, including inside this call.
    pub fn set_notify<F>(&mut self, notify: F)
    where
        F: Fn() + Send + Sync + 'static,
    {
        *lock(&self.notify.callback) = Some(Arc::new(notify));
    }

    /// Drop the wake closure. A pending wake stays pending.
    pub fn clear_notify(&mut self) {
        *lock(&self.notify.callback) = None;
    }

    /// Set the physical rectangle mpv scales and letterboxes into.
    pub fn set_slot(&mut self, slot: Slot) -> Result<(), Error> {
        match slot {
            Slot::Empty => {
                self.frames.clear_len();
                self.stage = Stage::NoSlot;
                Ok(())
            }
            Slot::Sized(size) => {
                let same = matches!(
                    &self.stage,
                    Stage::Live { size: current, .. } if *current == size
                );
                if !same {
                    self.frames.ensure(size)?;
                    let gpu = Box::new(Gpu::new(&self.device, &self.pipeline, size)?);
                    self.stage = Stage::Live {
                        size,
                        gpu,
                        shown: Shown::Waiting,
                    };
                }
                lock(&self.shared).slot_repaint = Freshness::Dirty;
                Ok(())
            }
        }
    }

    /// The slot last passed to [`Player::set_slot`].
    pub fn slot(&self) -> Slot {
        match &self.stage {
            Stage::NoSlot => Slot::Empty,
            Stage::Live { size, .. } => Slot::Sized(*size),
        }
    }

    /// Replace the current file. `url` is a path or a URL mpv understands.
    pub fn load(&self, url: &str) -> Result<(), Error> {
        self.core.command(&["loadfile", url]).map_err(map_mpv)
    }

    /// Drain mpv, upload a software frame when there is one, and submit the equalizer pass.
    pub fn poll(&mut self) -> Result<Outcome, Error> {
        self.notify.wake.store(WAKE_IDLE, Ordering::Release);
        self.events.clear();
        self.drain_events();
        let presentation = self.present()?;
        self.flush_stats();
        Ok(Outcome { presentation })
    }

    /// The picture written by the last successful pass.
    pub fn picture(&self) -> Picture<'_> {
        match &self.stage {
            Stage::Live {
                shown: Shown::Current,
                gpu,
                ..
            } => Picture::Shown(gpu.view()),
            Stage::NoSlot | Stage::Live { .. } => Picture::Waiting,
        }
    }

    /// Pause or resume. The getter updates immediately.
    pub fn set_playback(&self, playback: Playback) -> Result<(), Error> {
        let paused = matches!(playback, Playback::Paused);
        self.core.set_property("pause", paused).map_err(map_mpv)?;
        let mut shared = lock(&self.shared);
        if shared.playback != playback {
            shared.playback = playback;
        }
        Ok(())
    }

    /// Last playback state. Default is [`Playback::Playing`].
    pub fn playback(&self) -> Playback {
        lock(&self.shared).playback
    }

    /// Seek by a finite amount. Non-finite values cannot be built as [`Finite`].
    pub fn seek(&self, to: crate::types::Seek) -> Result<(), Error> {
        let (seconds, mode) = match to {
            crate::types::Seek::Relative(seconds) => (seconds, "relative"),
            crate::types::Seek::Absolute(seconds) => (seconds, "absolute"),
        };
        let text = format_finite(seconds);
        self.core
            .command(&["seek", text.as_str(), mode])
            .map_err(map_mpv)
    }

    /// Playback position in seconds, once mpv has reported a finite value.
    pub fn position(&self) -> Option<Finite> {
        lock(&self.shared).position
    }

    /// File duration in seconds, once mpv has reported a finite value.
    pub fn duration(&self) -> Option<Finite> {
        lock(&self.shared).duration
    }

    /// Store the equalizer and send it to mpv. The next [`Player::poll`] rebakes the pass.
    pub fn set_equalizer(&self, equalizer: Equalizer) -> Result<(), Error> {
        for (name, value) in [
            ("brightness", equalizer.brightness.get()),
            ("contrast", equalizer.contrast.get()),
            ("saturation", equalizer.saturation.get()),
            ("gamma", equalizer.gamma.get()),
            ("hue", equalizer.hue.get()),
        ] {
            let numeric = if name == "hue" {
                hue_to_mpv(value)
            } else {
                f64::from(value)
            };
            self.core.set_property(name, numeric).map_err(map_mpv)?;
        }
        let mut shared = lock(&self.shared);
        shared.equalizer = equalizer;
        shared.grade = Freshness::Dirty;
        Ok(())
    }

    /// Last equalizer. Default is all zeros.
    pub fn equalizer(&self) -> Equalizer {
        lock(&self.shared).equalizer
    }

    /// Add a delta to panscan, zoom, or volume.
    pub fn adjust(&self, adjust: Adjust) -> Result<(), Error> {
        let (name, delta) = match adjust {
            Adjust::Panscan(delta) => ("panscan", delta),
            Adjust::Zoom(delta) => ("video-zoom", delta),
            Adjust::Volume(delta) => ("volume", delta),
        };
        let text = format_finite(delta);
        self.core
            .command(&["add", name, text.as_str()])
            .map_err(map_mpv)
    }

    /// Set mpv's deinterlace mode.
    pub fn set_deinterlace(&self, mode: Deinterlace) -> Result<(), Error> {
        self.core
            .set_property("deinterlace", mode.as_mpv())
            .map_err(map_mpv)?;
        lock(&self.shared).deinterlace = mode;
        Ok(())
    }

    /// Last deinterlace mode. Default is [`Deinterlace::Auto`].
    pub fn deinterlace(&self) -> Deinterlace {
        lock(&self.shared).deinterlace
    }

    /// Mute or unmute.
    pub fn set_mute(&self, mute: Mute) -> Result<(), Error> {
        let on = matches!(mute, Mute::On);
        self.core.set_property("mute", on).map_err(map_mpv)?;
        lock(&self.shared).mute = mute;
        Ok(())
    }

    /// Last mute state. Default is [`Mute::Off`].
    pub fn mute(&self) -> Mute {
        lock(&self.shared).mute
    }

    /// Escape hatch to `mpv_command`. The slice is forwarded unchanged.
    pub fn command(&self, args: &[&str]) -> Result<(), Error> {
        self.core.command(args).map_err(map_mpv)
    }

    /// Events collected by the most recent [`Player::poll`].
    pub fn events(&self) -> &[Event] {
        &self.events
    }

    fn drain_events(&mut self) {
        while let Some(event) = self.core.poll_event() {
            self.ingest(event);
        }
    }

    fn ingest(&mut self, event: MpvEvent) {
        match event {
            MpvEvent::FileLoaded => {
                match self.core.get_property::<String>("current-ao") {
                    Ok(ao) => log::info!("current-ao={ao}"),
                    Err(err) => log::info!("current-ao-error={}", err.raw_code().unwrap_or(-1)),
                }
                if let Ok(ao) = self.core.get_property::<String>("ao") {
                    log::info!("ao={ao}");
                }
                if let Ok(codec) = self.core.get_property::<String>("audio-codec-name") {
                    log::info!("audio-codec={codec}");
                }
                self.events.push(Event::Loaded);
            }
            MpvEvent::EndFile { reason, .. } => {
                self.events.push(Event::Ended(end_reason(reason)));
            }
            MpvEvent::PropertyChange { name, data, .. } => self.ingest_property(&name, data),
            MpvEvent::LogMessage(message) => {
                log::warn!("mpv {}: {}", message.prefix, message.text.trim_end());
            }
            _ => {}
        }
    }

    fn ingest_property(&mut self, name: &str, data: PropertyData) {
        let mut shared = lock(&self.shared);
        match name {
            "pause" => {
                if let PropertyData::Flag(paused) = data {
                    let playback = if paused {
                        Playback::Paused
                    } else {
                        Playback::Playing
                    };
                    if shared.playback != playback {
                        shared.playback = playback;
                        self.events.push(Event::Playback(playback));
                    }
                }
            }
            "time-pos" => shared.position = finite_data(&data),
            "duration" => shared.duration = finite_data(&data),
            "brightness" => {
                if let Some(value) = rounded(&data) {
                    let next = UnitBias::new(value);
                    if shared.equalizer.brightness != next {
                        shared.equalizer.brightness = next;
                        shared.grade = Freshness::Dirty;
                    }
                }
            }
            "contrast" => {
                if let Some(value) = rounded(&data) {
                    let next = UnitBias::new(value);
                    if shared.equalizer.contrast != next {
                        shared.equalizer.contrast = next;
                        shared.grade = Freshness::Dirty;
                    }
                }
            }
            "saturation" => {
                if let Some(value) = rounded(&data) {
                    let next = UnitBias::new(value);
                    if shared.equalizer.saturation != next {
                        shared.equalizer.saturation = next;
                        shared.grade = Freshness::Dirty;
                    }
                }
            }
            "gamma" => {
                if let Some(value) = rounded(&data) {
                    let next = UnitBias::new(value);
                    if shared.equalizer.gamma != next {
                        shared.equalizer.gamma = next;
                        shared.grade = Freshness::Dirty;
                    }
                }
            }
            "hue" => {
                if let Some(value) = property_f64(&data) {
                    let hue = hue_degrees_from_mpv(value);
                    if shared.equalizer.hue != hue {
                        shared.equalizer.hue = hue;
                        shared.grade = Freshness::Dirty;
                    }
                }
            }
            "deinterlace" => {
                if let PropertyData::String(text) = &data
                    && let Some(mode) = Deinterlace::parse(text)
                {
                    shared.deinterlace = mode;
                }
            }
            "mute" => {
                if let PropertyData::Flag(on) = data {
                    shared.mute = if on { Mute::On } else { Mute::Off };
                }
            }
            "video-params/colormatrix" => {
                if let PropertyData::String(text) = &data {
                    let coefficients = Coefficients::parse(text);
                    if shared.coefficients != coefficients {
                        shared.coefficients = coefficients;
                        shared.grade = Freshness::Dirty;
                    }
                }
            }
            "video-params/gamma" => note_transfer(&mut shared.transfer, &data),
            "hwdec-current" => {
                if let (HwdecLog::Unseen, PropertyData::String(text)) = (&shared.hwdec, &data) {
                    log::info!("hwdec-current={text}");
                    shared.hwdec = HwdecLog::Logged;
                }
            }
            _ => {}
        }
    }

    fn present(&mut self) -> Result<Presentation, Error> {
        let Stage::Live { .. } = &self.stage else {
            return Ok(Presentation::Unchanged);
        };
        let frame_flag = self.render.update();
        let info = if frame_flag {
            self.render.next_frame_info().map_err(map_mpv)?
        } else {
            FrameInfo::default()
        };
        let next = next_frame(frame_flag, info);
        let (grade_dirty, slot_dirty, coefficients, equalizer) = {
            let shared = lock(&self.shared);
            (
                matches!(shared.grade, Freshness::Dirty),
                matches!(shared.slot_repaint, Freshness::Dirty),
                shared.coefficients,
                shared.equalizer,
            )
        };

        let software = matches!(next, NextFrame::Redraw | NextFrame::New) || slot_dirty;
        let report = matches!(next, NextFrame::Repeat | NextFrame::Redraw | NextFrame::New);
        if matches!(next, NextFrame::Repeat) && !software && !grade_dirty {
            self.render.report_swap();
            self.stats.repeats = self.stats.repeats.saturating_add(1);
            return Ok(Presentation::Unchanged);
        }
        if !software && !grade_dirty {
            return Ok(Presentation::Unchanged);
        }
        self.paint(software, coefficients, equalizer, report)
    }

    fn paint(
        &mut self,
        software: bool,
        coefficients: Coefficients,
        equalizer: Equalizer,
        report: bool,
    ) -> Result<Presentation, Error> {
        let size = match &self.stage {
            Stage::Live { size, .. } => *size,
            Stage::NoSlot => return Ok(Presentation::Unchanged),
        };
        if software {
            self.frames.ensure(size)?;
            let width = i32::try_from(size.width.get()).map_err(|_| Error::InvalidSize)?;
            let height = i32::try_from(size.height.get()).map_err(|_| Error::InvalidSize)?;
            let started = Instant::now();
            self.render
                .render_software(
                    width,
                    height,
                    SwPixelFormat::Rgb0,
                    self.frames.stride(),
                    self.frames.pixels_mut(),
                )
                .map_err(map_mpv)?;
            self.stats.software += started.elapsed();
            let started = Instant::now();
            if let Stage::Live { gpu, .. } = &self.stage {
                gpu.upload_bytes(&self.queue, &self.frames)?;
            }
            self.stats.upload += started.elapsed();
            self.stats.bytes = self
                .stats
                .bytes
                .saturating_add(self.frames.pixels().len() as u64);
            self.stats.frames = self.stats.frames.saturating_add(1);
        }
        let grade = bake(equalizer, coefficients);
        if software && let Stage::Live { gpu, .. } = &mut self.stage {
            let written = gpu.upload;
            gpu.rebind(&self.device, &self.pipeline, written);
            gpu.upload = written.flip();
        }
        if let Stage::Live { gpu, .. } = &self.stage {
            gpu.write_grade(&self.queue, grade);
            gpu.draw(&self.device, &self.queue, &self.pipeline);
        }
        if let Stage::Live { shown, .. } = &mut self.stage {
            *shown = Shown::Current;
        }
        if report {
            self.render.report_swap();
        }
        let mut shared = lock(&self.shared);
        shared.grade = Freshness::Clean;
        shared.slot_repaint = Freshness::Clean;
        Ok(Presentation::Updated)
    }

    fn flush_stats(&mut self) {
        if self.stats.window.elapsed() < std::time::Duration::from_secs(1) {
            return;
        }
        let ao = self
            .core
            .get_property::<String>("current-ao")
            .unwrap_or_else(|_| "unavailable".to_string());
        log::info!(
            "frames={} repeats={} bytes={} software_us={} upload_us={} current-ao={ao}",
            self.stats.frames,
            self.stats.repeats,
            self.stats.bytes,
            self.stats.software.as_micros(),
            self.stats.upload.as_micros()
        );
        self.stats.frames = 0;
        self.stats.repeats = 0;
        self.stats.bytes = 0;
        self.stats.software = std::time::Duration::ZERO;
        self.stats.upload = std::time::Duration::ZERO;
        self.stats.window = Instant::now();
    }
}

fn build_core(options: PlayerOptions) -> Result<Mpv, Error> {
    let mut builder = Mpv::builder().map_err(map_mpv)?;
    let mut settings: Vec<(&str, &str)> = vec![
        ("vo", "libmpv"),
        ("hwdec", "auto-safe"),
        ("vid", "auto"),
        ("idle", "yes"),
        ("keep-open", "yes"),
        ("video-sync", "audio"),
        ("sub-visibility", "yes"),
        ("deinterlace", "auto"),
        ("osc", "no"),
        ("input-default-bindings", "no"),
        ("input-vo-keyboard", "no"),
    ];
    if let Some(driver) = options.audio_output.as_mpv() {
        settings.push(("ao", driver));
    }
    for (name, value) in settings {
        builder = builder.set_property(name, value).map_err(map_mpv)?;
    }
    builder
        .set_property("video-timing-offset", 0.0_f64)
        .map_err(map_mpv)?
        .build()
        .map_err(map_mpv)
}

fn next_frame(ready: bool, info: FrameInfo) -> NextFrame {
    if !ready || !info.present {
        NextFrame::Absent
    } else if info.repeat {
        NextFrame::Repeat
    } else if info.redraw {
        NextFrame::Redraw
    } else {
        NextFrame::New
    }
}

fn end_reason(reason: EndFileReason) -> EndReason {
    match reason {
        EndFileReason::Eof => EndReason::Eof,
        EndFileReason::Stop => EndReason::Stop,
        EndFileReason::Quit => EndReason::Quit,
        EndFileReason::Redirect => EndReason::Redirect,
        EndFileReason::Error => EndReason::Error,
        _ => EndReason::Error,
    }
}

fn observe(core: &Mpv) -> Result<(), Error> {
    let flags = [("pause", Format::Flag), ("mute", Format::Flag)];
    for (name, format) in flags {
        core.observe_property(1, name, format).map_err(map_mpv)?;
    }
    for name in [
        "time-pos",
        "duration",
        "brightness",
        "contrast",
        "saturation",
        "gamma",
        "hue",
    ] {
        core.observe_property(1, name, Format::Double)
            .map_err(map_mpv)?;
    }
    for name in [
        "deinterlace",
        "hwdec-current",
        "video-params/colormatrix",
        "video-params/gamma",
    ] {
        core.observe_property(1, name, Format::String)
            .map_err(map_mpv)?;
    }
    Ok(())
}

fn finite_data(data: &PropertyData) -> Option<Finite> {
    match data {
        PropertyData::Double(value) => Finite::new(*value),
        PropertyData::Int64(value) => Finite::new(*value as f64),
        _ => None,
    }
}

/// mpv's `hue` property is −100..=100. The public value is degrees, −180..=180.
pub(crate) fn hue_to_mpv(degrees: i32) -> f64 {
    f64::from(degrees) * 100.0 / 180.0
}

/// Invert a raw mpv hue echo. Round only after scaling back to degrees.
pub(crate) fn hue_degrees_from_mpv(value: f64) -> Hue {
    Hue::new((value * 180.0 / 100.0).round() as i32)
}

fn property_f64(data: &PropertyData) -> Option<f64> {
    match data {
        PropertyData::Double(value) if value.is_finite() => Some(*value),
        PropertyData::Int64(value) => Some(*value as f64),
        _ => None,
    }
}

fn rounded(data: &PropertyData) -> Option<i32> {
    match data {
        PropertyData::Double(value) if value.is_finite() => Some(value.round() as i32),
        PropertyData::Int64(value) => i32::try_from(*value).ok(),
        _ => None,
    }
}

fn note_transfer(note: &mut TransferNote, data: &PropertyData) {
    let PropertyData::String(text) = data else {
        return;
    };
    let hdr = text == "pq" || text == "hlg";
    match (&*note, hdr) {
        (TransferNote::Unseen, false) => *note = TransferNote::Sdr,
        (TransferNote::Unseen, true) => {
            log::warn!("hdr transfer {text} is stored as 8-bit rgb by the software renderer");
            *note = TransferNote::Hdr;
        }
        _ => {}
    }
}

fn format_finite(value: Finite) -> String {
    let mut text = ryu_like(value.get());
    if text.is_empty() {
        text = "0".to_string();
    }
    text
}

/// Format a finite float without pulling an extra crate. Not used on the frame path.
fn ryu_like(value: f64) -> String {
    format!("{value}")
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poison| poison.into_inner())
}

#[cfg(test)]
mod tests {
    use super::{hue_degrees_from_mpv, hue_to_mpv};

    #[test]
    fn hue_echo_round_trips_degrees() {
        for degrees in [0, 1, 10, -10, 90, -90, 180, -180] {
            let echoed = hue_to_mpv(degrees);
            assert_eq!(
                hue_degrees_from_mpv(echoed).get(),
                degrees,
                "mpv echo {echoed} for {degrees} degrees"
            );
        }
    }
}
