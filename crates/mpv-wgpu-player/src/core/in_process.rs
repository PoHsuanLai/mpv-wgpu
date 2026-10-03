//! libmpv linked into this process, with a software render context.

use std::sync::Arc;
use std::time::Instant;

use rsmpv::render::{FrameInfo, OwnedRenderContext, SwPixelFormat};
use rsmpv::{
    EndFileReason, Event as MpvEvent, Format, Mpv, Node as MpvNode, PropertyData as MpvData,
};

use super::{Core, CoreEvent, OBSERVED, ObserveAs, PropValue, Pulled};
use crate::frame_buffer::FrameBuffer;
use crate::notify::Notify;
use crate::options::PlayerOptions;
use crate::pipeline::Gpu;
use crate::stats::Stats;
use crate::types::{EndReason, Error, SlotSize, map_mpv};
use crate::value::{Node, PropertyData};

enum NextFrame {
    Absent,
    Repeat,
    Redraw,
    New,
}

pub(crate) struct InProcess {
    core: Arc<Mpv>,
    render: OwnedRenderContext,
    frames: FrameBuffer,
    size: Option<SlotSize>,
}

impl InProcess {
    pub(crate) fn new(options: PlayerOptions, notify: &Arc<Notify>) -> Result<Self, Error> {
        let core = build_core(options)?;
        let core = Arc::new(core);
        let mut render = OwnedRenderContext::new_software(Arc::clone(&core)).map_err(map_mpv)?;
        let signal_target = Arc::clone(notify);
        render.set_update_callback(move || signal_target.signal());
        let wake_target = Arc::clone(notify);
        core.set_wakeup_callback(move || wake_target.signal());
        let _ = core.request_log_messages("warn");
        observe(&core)?;
        Ok(Self {
            core,
            render,
            frames: FrameBuffer::new(),
            size: None,
        })
    }
}

impl Core for InProcess {
    fn command(&self, args: &[&str]) -> Result<(), Error> {
        self.core.command(args).map_err(map_mpv)
    }

    fn set(&self, name: &str, value: PropValue<'_>) -> Result<(), Error> {
        match value {
            PropValue::Flag(flag) => self.core.set_property(name, flag),
            PropValue::Int(number) => self.core.set_property(name, number),
            PropValue::Double(number) => self.core.set_property(name, number),
            PropValue::Text(text) => self.core.set_property(name, text),
        }
        .map_err(map_mpv)
    }

    fn get_double(&self, name: &str) -> Option<f64> {
        self.core.get_property::<f64>(name).ok()
    }

    fn next_event(&mut self) -> Option<CoreEvent> {
        loop {
            let event = self.core.poll_event()?;
            if let Some(event) = convert_event(event) {
                return Some(event);
            }
        }
    }

    fn set_slot(&mut self, size: Option<SlotSize>) -> Result<(), Error> {
        match size {
            Some(size) => self.frames.ensure(size)?,
            None => self.frames.clear_len(),
        }
        self.size = size;
        Ok(())
    }

    fn pull(
        &mut self,
        repaint: bool,
        gpu: &Gpu,
        queue: &wgpu::Queue,
        stats: &mut Stats,
    ) -> Result<Pulled, Error> {
        let frame_flag = self.render.update();
        let info = if frame_flag {
            self.render.next_frame_info().map_err(map_mpv)?
        } else {
            FrameInfo::default()
        };
        let next = next_frame(frame_flag, info);
        let software = matches!(next, NextFrame::Redraw | NextFrame::New) || repaint;
        let swap = matches!(next, NextFrame::Repeat | NextFrame::Redraw | NextFrame::New);
        if !software {
            return Ok(if matches!(next, NextFrame::Repeat) {
                Pulled::Repeat
            } else {
                Pulled::Nothing
            });
        }
        let Some(size) = self.size else {
            return Ok(Pulled::Nothing);
        };
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
        stats.software += started.elapsed();
        let started = Instant::now();
        gpu.upload(
            queue,
            self.frames.pixels(),
            self.frames.stride(),
            self.frames.width(),
            self.frames.height(),
        )?;
        stats.upload += started.elapsed();
        stats.bytes = stats
            .bytes
            .saturating_add(self.frames.pixels().len() as u64);
        stats.frames = stats.frames.saturating_add(1);
        Ok(Pulled::Frame { swap })
    }

    fn finish(&mut self, swap: bool) {
        if swap {
            self.render.report_swap();
        }
    }

    fn on_file_loaded(&self) {
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
    }

    fn stats_note(&mut self) -> String {
        let ao = self
            .core
            .get_property::<String>("current-ao")
            .unwrap_or_else(|_| "unavailable".to_string());
        format!("current-ao={ao}")
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
        ("volume-max", "150"),
        ("audio-display", "embedded-first"),
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

fn observe(core: &Mpv) -> Result<(), Error> {
    for (name, how) in OBSERVED {
        let format = match how {
            ObserveAs::Flag => Format::Flag,
            ObserveAs::Int => Format::Int64,
            ObserveAs::Double => Format::Double,
            ObserveAs::Text => Format::String,
            ObserveAs::Node => Format::Node,
        };
        core.observe_property(1, name, format).map_err(map_mpv)?;
    }
    Ok(())
}

fn convert_event(event: MpvEvent) -> Option<CoreEvent> {
    match event {
        MpvEvent::FileLoaded => Some(CoreEvent::FileLoaded),
        MpvEvent::EndFile { reason, .. } => Some(CoreEvent::Ended(end_reason(reason))),
        MpvEvent::PropertyChange { name, data, .. } => {
            Some(CoreEvent::Property(name, convert_data(data)))
        }
        MpvEvent::LogMessage(message) => Some(CoreEvent::Log {
            prefix: message.prefix,
            text: message.text,
        }),
        _ => None,
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

fn convert_data(data: MpvData) -> PropertyData {
    match data {
        MpvData::String(text) | MpvData::OsdString(text) => PropertyData::String(text),
        MpvData::Flag(flag) => PropertyData::Flag(flag),
        MpvData::Int64(number) => PropertyData::Int64(number),
        MpvData::Double(number) => PropertyData::Double(number),
        MpvData::Node(node) => PropertyData::Node(convert_node(node)),
        _ => PropertyData::None,
    }
}

fn convert_node(node: MpvNode) -> Node {
    match node {
        MpvNode::Flag(flag) => Node::Flag(flag),
        MpvNode::Int64(number) => Node::Int64(number),
        MpvNode::Double(number) => Node::Double(number),
        MpvNode::String(text) => Node::String(text),
        MpvNode::Array(items) => Node::Array(items.into_iter().map(convert_node).collect()),
        MpvNode::Map(entries) => Node::Map(
            entries
                .into_iter()
                .map(|(key, value)| (key, convert_node(value)))
                .collect(),
        ),
        _ => Node::None,
    }
}
