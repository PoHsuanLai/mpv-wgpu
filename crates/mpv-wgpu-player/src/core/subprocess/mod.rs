//! The user's `mpv` as a child process, with `mpv-wgpu-cplugin` loaded into it.
//!
//! The child runs `--vo=libmpv` with the plugin, which renders `rgb0` frames
//! into a memfd ring this side created. [`Core::pull`] uploads the newest
//! finished slot straight from the mapping to the GPU texture, then
//! [`Core::finish`] hands the slot back, which is what lets mpv report the swap.

mod link;
mod spawn;

use std::process::Child;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use mpv_wgpu_protocol::{
    Message, Ring, RingLayout, SLOTS, SlotRef, Value, monotonic_ns, row_stride,
};

use self::link::Link;
use super::{Core, CoreEvent, OBSERVED, ObserveAs, PropValue, Pulled};
use crate::notify::Notify;
use crate::options::{PlayerOptions, SubprocessOptions};
use crate::pipeline::Gpu;
use crate::stats::Stats;
use crate::types::{Error, MpvError, SlotSize};

/// How long a polite quit gets before the child is killed.
const QUIT_GRACE: Duration = Duration::from_millis(1500);

pub(crate) struct Subprocess {
    link: Arc<Link>,
    child: Child,
    reader: Option<JoinHandle<()>>,
    logs: Vec<JoinHandle<()>>,
    ring: Option<Ring>,
    generation: u32,
    /// The slot uploaded by the last pull, owed a `Presented`.
    uploaded: Option<SlotRef>,
    delivery: Delivery,
}

/// How long frames sat between the plugin's render and the upload.
#[derive(Default)]
struct Delivery {
    frames: u64,
    total_ns: u64,
    max_ns: u64,
}

impl Subprocess {
    pub(crate) fn start(
        player: PlayerOptions,
        options: &SubprocessOptions,
        notify: &Arc<Notify>,
    ) -> Result<Self, Error> {
        let spawned = spawn::spawn(player, options)?;
        log::info!(
            "mpv child: {} (client API {}.{}, plugin {})",
            spawned.hello.mpv_version,
            spawned.hello.client_api >> 16,
            spawned.hello.client_api & 0xffff,
            spawned.hello.plugin_version
        );
        let link = Link::new(spawned.channel, Arc::clone(notify));
        let mut this = Self {
            link: Arc::clone(&link),
            child: spawned.child,
            reader: None,
            logs: spawned.logs,
            ring: None,
            generation: 0,
            uploaded: None,
            delivery: Delivery::default(),
        };
        // From here `Drop` cleans the child up, whatever fails.
        this.reader = Some(
            link.spawn_reader()
                .map_err(|error| Error::HostStart(format!("cannot start a thread: {error}")))?,
        );
        for (name, how) in OBSERVED {
            link.send(
                &Message::Observe {
                    name: (*name).to_string(),
                    format: protocol_format(*how),
                },
                None,
            )?;
        }
        Ok(this)
    }
}

fn protocol_format(how: ObserveAs) -> mpv_wgpu_protocol::Format {
    use mpv_wgpu_protocol::Format;
    match how {
        ObserveAs::Flag => Format::Flag,
        ObserveAs::Int => Format::Int64,
        ObserveAs::Double => Format::Double,
        ObserveAs::Text => Format::String,
        ObserveAs::Node => Format::Node,
    }
}

fn mpv_result(error: i32) -> Result<(), Error> {
    if error < 0 {
        Err(Error::Mpv(MpvError { code: error }))
    } else {
        Ok(())
    }
}

impl Core for Subprocess {
    fn command(&self, args: &[&str]) -> Result<(), Error> {
        let (error, _) = self.link.request(|id| Message::Command {
            id,
            args: args.iter().map(|arg| (*arg).to_string()).collect(),
        })?;
        mpv_result(error)
    }

    fn set(&self, name: &str, value: PropValue<'_>) -> Result<(), Error> {
        let value = match value {
            PropValue::Flag(flag) => Value::Flag(flag),
            PropValue::Int(number) => Value::Int64(number),
            PropValue::Double(number) => Value::Double(number),
            PropValue::Text(text) => Value::String(text.to_string()),
        };
        let (error, _) = self.link.request(|id| Message::Set {
            id,
            name: name.to_string(),
            value,
        })?;
        mpv_result(error)
    }

    fn get_double(&self, name: &str) -> Option<f64> {
        let (error, value) = self
            .link
            .request(|id| Message::Get {
                id,
                name: name.to_string(),
            })
            .ok()?;
        match value {
            Value::Double(number) if error >= 0 => Some(number),
            Value::Int64(number) if error >= 0 => Some(number as f64),
            _ => None,
        }
    }

    fn next_event(&mut self) -> Option<CoreEvent> {
        self.link.next_event()
    }

    fn set_slot(&mut self, size: Option<SlotSize>) -> Result<(), Error> {
        self.uploaded = None;
        let Some(size) = size else {
            self.ring = None;
            return Ok(());
        };
        let stride = row_stride(size.width.get()).ok_or(Error::InvalidSize)?;
        self.generation = self.generation.wrapping_add(1);
        let layout = RingLayout::new(
            self.generation,
            size.width.get(),
            size.height.get(),
            stride,
            SLOTS,
        )
        .map_err(|_| Error::InvalidSize)?;
        let ring = Ring::create(layout)
            .map_err(|error| Error::HostStart(format!("cannot create the frame ring: {error}")))?;
        // Frames of the old ring are stale from here on.
        let _ = self.link.take_frame();
        self.link.send(
            &Message::Resize(mpv_wgpu_protocol::Resize {
                generation: self.generation,
                width: size.width.get(),
                height: size.height.get(),
                stride,
                slots: SLOTS,
            }),
            Some(ring.fd()),
        )?;
        self.ring = Some(ring);
        Ok(())
    }

    fn pull(
        &mut self,
        _repaint: bool,
        gpu: &Gpu,
        queue: &wgpu::Queue,
        stats: &mut Stats,
    ) -> Result<Pulled, Error> {
        let Some(frame) = self.link.take_frame() else {
            return Ok(Pulled::Nothing);
        };
        let slot = SlotRef {
            generation: frame.generation,
            slot: frame.slot,
        };
        let Some(ring) = self.ring.as_ref().filter(|ring| {
            let layout = ring.layout();
            layout.generation() == frame.generation
                && layout.width() == frame.width
                && layout.height() == frame.height
                && layout.stride() == frame.stride
        }) else {
            // A frame for a ring that is gone. The plugin dropped its slots with it.
            return Ok(Pulled::Nothing);
        };
        let Some(bytes) = ring.slot(frame.slot) else {
            let _ = self.link.send(&Message::Released(slot), None);
            return Ok(Pulled::Nothing);
        };
        let now = monotonic_ns();
        let started = Instant::now();
        let uploaded = gpu.upload(
            queue,
            bytes,
            frame.stride as usize,
            frame.width,
            frame.height,
        );
        if let Err(error) = uploaded {
            let _ = self.link.send(&Message::Released(slot), None);
            return Err(error);
        }
        stats.upload += started.elapsed();
        stats.bytes = stats.bytes.saturating_add(bytes.len() as u64);
        stats.frames = stats.frames.saturating_add(1);
        let waited = now.saturating_sub(frame.sent_ns);
        self.delivery.frames += 1;
        self.delivery.total_ns += waited;
        self.delivery.max_ns = self.delivery.max_ns.max(waited);
        self.uploaded = Some(slot);
        Ok(Pulled::Frame { swap: true })
    }

    fn finish(&mut self, swap: bool) {
        let Some(slot) = self.uploaded.take() else {
            return;
        };
        // `write_texture` has copied the pixels, so the slot is free either way.
        let message = if swap {
            Message::Presented(slot)
        } else {
            Message::Released(slot)
        };
        let _ = self.link.send(&message, None);
    }

    fn stats_note(&mut self) -> String {
        let delivery = std::mem::take(&mut self.delivery);
        let mean_us = delivery
            .total_ns
            .checked_div(delivery.frames)
            .map_or(0, |ns| ns / 1000);
        format!(
            "dropped={} delivery_mean_us={mean_us} delivery_max_us={}",
            self.link.dropped(),
            delivery.max_ns / 1000
        )
    }

    fn check(&self) -> Result<(), Error> {
        if self.link.is_gone() && !self.link.has_events() {
            Err(Error::HostGone)
        } else {
            Ok(())
        }
    }
}

impl Drop for Subprocess {
    fn drop(&mut self) {
        if !self.link.is_gone() {
            let _ = self.link.send(&Message::Bye, None);
        }
        let started = Instant::now();
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if started.elapsed() < QUIT_GRACE => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                _ => {
                    spawn::kill(&mut self.child);
                    break;
                }
            }
        }
        self.link.shutdown();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
        for log in self.logs.drain(..) {
            let _ = log.join();
        }
    }
}
