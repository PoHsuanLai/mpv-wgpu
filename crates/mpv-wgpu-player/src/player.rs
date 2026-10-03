//! libmpv core, software frames, and the wgpu picture.

use std::path::Path;
use std::sync::{Arc, Mutex};

use mpv_wgpu::{Coefficients, Equalizer, Hue, UnitBias, bake};

use crate::chapters::{Chapter, ChapterIndex};
use crate::controls::{Direction, ScreenshotContent, VideoPresence};
use crate::core::{Core, CoreEvent, PropValue, Pulled};
use crate::media::MediaState;
use crate::notify::{Notify, lock};
use crate::options::{Host, PlayerOptions};
use crate::pipeline::{Gpu, Pipeline};
use crate::quantities::{Speed, Volume};
use crate::stats::Stats;
use crate::tracks::{TrackChoice, TrackKind, TrackList};
use crate::types::{
    Adjust, Deinterlace, Error, Event, Finite, Mute, Outcome, Picture, Playback, Presentation,
    Slot, SlotSize,
};
use crate::value::PropertyData;

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
    media: MediaState,
    coefficients: Coefficients,
    transfer: TransferNote,
    hwdec: HwdecLog,
    grade: Freshness,
    slot_repaint: Freshness,
}

/// One mpv player and the texture a host samples.
///
/// The mpv core runs in this process through libmpv, or in a child `mpv`
/// process; see [`Host`]. Everything below behaves the same either way unless a
/// method says otherwise.
///
/// `Player` is [`Send`] and not [`Sync`]. Call [`Player::poll`] on the thread
/// that presents. The notify closure may run on an mpv thread and must only
/// wake the host.
pub struct Player {
    core: Box<dyn Core>,
    notify: Arc<Notify>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: Pipeline,
    stage: Stage,
    shared: Mutex<Shared>,
    events: Vec<Event>,
    stats: Stats,
}

impl Player {
    /// Start an mpv core on `device` / `queue`, in the way [`Host::default`] says.
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
        Self::with_host(device, queue, options, Host::default())
    }

    /// Like [`Player::new`], choosing where the mpv core runs.
    ///
    /// [`Host::InProcess`] needs the `in-process` cargo feature and
    /// [`Host::Subprocess`] needs the `subprocess` feature; asking for a mode that
    /// was compiled out fails with [`Error::HostStart`].
    pub fn with_host(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        options: PlayerOptions,
        host: Host,
    ) -> Result<Self, Error> {
        let notify = Notify::new();
        let core = start_core(options, host, &notify)?;
        let pipeline = Pipeline::new(device)?;
        let player = Self {
            core,
            notify,
            device: device.clone(),
            queue: queue.clone(),
            pipeline,
            stage: Stage::NoSlot,
            shared: Mutex::new(Shared {
                playback: Playback::Playing,
                mute: Mute::Off,
                deinterlace: Deinterlace::Auto,
                equalizer: Equalizer::default(),
                position: None,
                duration: None,
                media: MediaState::new(),
                coefficients: Coefficients::Bt709,
                transfer: TransferNote::Unseen,
                hwdec: HwdecLog::Unseen,
                grade: Freshness::Clean,
                slot_repaint: Freshness::Clean,
            }),
            events: Vec::new(),
            stats: Stats::new(),
        };
        if player
            .core
            .set("deinterlace", PropValue::Text("auto"))
            .is_err()
        {
            log::info!("this mpv rejects deinterlace=auto; keeping its default");
        }
        Ok(player)
    }

    /// Replace the wake closure. It may run on an mpv thread, including inside this call.
    pub fn set_notify<F>(&mut self, notify: F)
    where
        F: Fn() + Send + Sync + 'static,
    {
        self.notify.set(Some(Arc::new(notify)));
    }

    /// Drop the wake closure. A pending wake stays pending.
    pub fn clear_notify(&mut self) {
        self.notify.set(None);
    }

    /// Set the physical rectangle mpv scales and letterboxes into.
    pub fn set_slot(&mut self, slot: Slot) -> Result<(), Error> {
        match slot {
            Slot::Empty => {
                self.core.set_slot(None)?;
                self.stage = Stage::NoSlot;
                Ok(())
            }
            Slot::Sized(size) => {
                let same = matches!(
                    &self.stage,
                    Stage::Live { size: current, .. } if *current == size
                );
                if !same {
                    self.core.set_slot(Some(size))?;
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
        self.core.command(&["loadfile", url])
    }

    /// Drain mpv, upload a software frame when there is one, and submit the equalizer pass.
    pub fn poll(&mut self) -> Result<Outcome, Error> {
        self.notify.clear_pending();
        self.events.clear();
        self.drain_events();
        self.core.check()?;
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
        self.core.set("pause", PropValue::Flag(paused))?;
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
        self.core.command(&["seek", text.as_str(), mode])
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
            self.core.set(name, PropValue::Double(numeric))?;
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
        self.core.command(&["add", name, text.as_str()])
    }

    /// Set mpv's deinterlace mode.
    pub fn set_deinterlace(&self, mode: Deinterlace) -> Result<(), Error> {
        self.core
            .set("deinterlace", PropValue::Text(mode.as_mpv()))?;
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
        self.core.set("mute", PropValue::Flag(on))?;
        lock(&self.shared).mute = mute;
        Ok(())
    }

    /// Last mute state. Default is [`Mute::Off`].
    pub fn mute(&self) -> Mute {
        lock(&self.shared).mute
    }

    /// Every track of the current file. Empty until mpv has read the file.
    ///
    /// Changes arrive as [`Event::TracksChanged`].
    pub fn tracks(&self) -> TrackList {
        lock(&self.shared).media.tracks.clone()
    }

    /// Choose the track of `kind` mpv plays, or turn the kind off.
    ///
    /// The selection shows in [`Player::tracks`] after the next [`Player::poll`].
    pub fn select_track(&self, kind: TrackKind, choice: TrackChoice) -> Result<(), Error> {
        self.core
            .set(kind.option(), PropValue::Text(choice.as_mpv().as_str()))
    }

    /// Whether the current file shows a picture, from the selected video track.
    ///
    /// A cover image attached to an audio file is [`VideoPresence::CoverArt`].
    pub fn has_video(&self) -> VideoPresence {
        VideoPresence::of(&lock(&self.shared).media.tracks)
    }

    /// The chapters of the current file, in order. Empty without chapters.
    ///
    /// Changes arrive as [`Event::ChaptersChanged`].
    pub fn chapters(&self) -> Vec<Chapter> {
        lock(&self.shared).media.chapters.clone()
    }

    /// The chapter playback is in, once mpv has reported one.
    pub fn chapter(&self) -> Option<ChapterIndex> {
        lock(&self.shared).media.chapter
    }

    /// Seek to the start of a chapter.
    ///
    /// Fails with [`Error::NoSuchChapter`] when `index` is past the last chapter.
    pub fn set_chapter(&self, index: ChapterIndex) -> Result<(), Error> {
        if index.get() as usize >= lock(&self.shared).media.chapters.len() {
            return Err(Error::NoSuchChapter(index.get()));
        }
        self.core
            .set("chapter", PropValue::Int(i64::from(index.get())))
    }

    /// Set the absolute volume. [`Player::adjust`] with [`Adjust::Volume`] adds a delta.
    pub fn set_volume(&self, volume: Volume) -> Result<(), Error> {
        self.core.set("volume", PropValue::Double(volume.to_mpv()))
    }

    /// The volume mpv holds right now, read from the core.
    pub fn volume(&self) -> Volume {
        self.core
            .get_double("volume")
            .and_then(Volume::from_mpv)
            .unwrap_or(lock(&self.shared).media.volume)
    }

    /// Set the playback speed.
    pub fn set_speed(&self, speed: Speed) -> Result<(), Error> {
        self.core.set("speed", PropValue::Double(speed.ratio()))
    }

    /// The playback speed mpv holds right now, read from the core.
    pub fn speed(&self) -> Speed {
        self.core
            .get_double("speed")
            .and_then(Speed::from_mpv)
            .unwrap_or(Speed::NORMAL)
    }

    /// Step one frame and pause. Needs a video track.
    pub fn frame_step(&self, direction: Direction) -> Result<(), Error> {
        self.core.command(&[direction.command()])
    }

    /// Write the current frame to `path`; the extension picks the image format.
    ///
    /// [`ScreenshotContent::Video`] and [`ScreenshotContent::Subtitles`] are at
    /// source resolution, whatever the slot size.
    pub fn screenshot_to_file(&self, path: &Path, content: ScreenshotContent) -> Result<(), Error> {
        let path = path.to_str().ok_or(Error::PathNotUtf8)?;
        self.core
            .command(&["screenshot-to-file", path, content.as_mpv()])
    }

    /// Escape hatch to `mpv_command`. The slice is forwarded unchanged.
    pub fn command(&self, args: &[&str]) -> Result<(), Error> {
        self.core.command(args)
    }

    /// Events collected by the most recent [`Player::poll`].
    pub fn events(&self) -> &[Event] {
        &self.events
    }

    fn drain_events(&mut self) {
        while let Some(event) = self.core.next_event() {
            self.ingest(event);
        }
    }

    fn ingest(&mut self, event: CoreEvent) {
        match event {
            CoreEvent::FileLoaded => {
                self.core.on_file_loaded();
                self.events.push(Event::Loaded);
            }
            CoreEvent::Ended(reason) => self.events.push(Event::Ended(reason)),
            CoreEvent::Property(name, data) => self.ingest_property(&name, data),
            CoreEvent::Log { prefix, text } => {
                log::warn!("mpv {}: {}", prefix, text.trim_end());
            }
        }
    }

    fn ingest_property(&mut self, name: &str, data: PropertyData) {
        let mut shared = lock(&self.shared);
        if let Some(event) = shared.media.apply(name, &data) {
            self.events.push(event);
        }
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
        if !matches!(&self.stage, Stage::Live { .. }) {
            return Ok(Presentation::Unchanged);
        }
        if self.has_video() == VideoPresence::Absent {
            return Ok(self.withdraw_picture());
        }
        let (grade_dirty, slot_dirty, coefficients, equalizer) = {
            let shared = lock(&self.shared);
            (
                matches!(shared.grade, Freshness::Dirty),
                matches!(shared.slot_repaint, Freshness::Dirty),
                shared.coefficients,
                shared.equalizer,
            )
        };
        let Stage::Live { gpu, .. } = &self.stage else {
            return Ok(Presentation::Unchanged);
        };
        let pulled = self
            .core
            .pull(slot_dirty, gpu, &self.queue, &mut self.stats)?;
        match pulled {
            Pulled::Repeat if !grade_dirty => {
                self.core.finish(true);
                self.stats.repeats = self.stats.repeats.saturating_add(1);
                Ok(Presentation::Unchanged)
            }
            Pulled::Nothing if !grade_dirty => Ok(Presentation::Unchanged),
            Pulled::Nothing => self.paint(false, coefficients, equalizer, false),
            Pulled::Repeat => self.paint(false, coefficients, equalizer, true),
            Pulled::Frame { swap } => self.paint(true, coefficients, equalizer, swap),
        }
    }

    /// Audio-only or no file: there is nothing to sample, so the picture goes
    /// back to waiting instead of showing a black frame.
    fn withdraw_picture(&mut self) -> Presentation {
        match &mut self.stage {
            Stage::Live { shown, .. } if matches!(shown, Shown::Current) => {
                *shown = Shown::Waiting;
                Presentation::Updated
            }
            Stage::Live { .. } | Stage::NoSlot => Presentation::Unchanged,
        }
    }

    /// Draw the equalizer pass. `uploaded` says a new frame was just written
    /// into the upload texture; `swap` is whether mpv is told afterwards.
    fn paint(
        &mut self,
        uploaded: bool,
        coefficients: Coefficients,
        equalizer: Equalizer,
        swap: bool,
    ) -> Result<Presentation, Error> {
        let grade = bake(equalizer, coefficients);
        if uploaded && let Stage::Live { gpu, .. } = &mut self.stage {
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
        self.core.finish(swap);
        let mut shared = lock(&self.shared);
        shared.grade = Freshness::Clean;
        shared.slot_repaint = Freshness::Clean;
        Ok(Presentation::Updated)
    }

    fn flush_stats(&mut self) {
        if self.stats.window.elapsed() < std::time::Duration::from_secs(1) {
            return;
        }
        log::info!(
            "frames={} repeats={} bytes={} software_us={} upload_us={} {}",
            self.stats.frames,
            self.stats.repeats,
            self.stats.bytes,
            self.stats.software.as_micros(),
            self.stats.upload.as_micros(),
            self.core.stats_note()
        );
        self.stats.reset();
    }
}

fn start_core(
    options: PlayerOptions,
    host: Host,
    notify: &Arc<Notify>,
) -> Result<Box<dyn Core>, Error> {
    match host {
        #[cfg(feature = "in-process")]
        Host::InProcess => Ok(Box::new(crate::core::in_process::InProcess::new(
            options, notify,
        )?)),
        #[cfg(feature = "subprocess")]
        Host::Subprocess(host) => Ok(Box::new(crate::core::subprocess::Subprocess::start(
            options, &host, notify,
        )?)),
        #[allow(unreachable_patterns)]
        _ => {
            let _ = (options, notify);
            Err(Error::HostStart(
                "this host mode was compiled out; enable the `in-process` or `subprocess` feature"
                    .to_string(),
            ))
        }
    }
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
