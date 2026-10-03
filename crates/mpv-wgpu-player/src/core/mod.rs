//! The mpv core behind a [`crate::Player`]: libmpv in this process, or the
//! user's `mpv` as a child process. Both give the player the same operations.

use crate::pipeline::Gpu;
use crate::stats::Stats;
use crate::types::{EndReason, Error, SlotSize};
use crate::value::PropertyData;

#[cfg(feature = "in-process")]
pub(crate) mod in_process;
#[cfg(feature = "subprocess")]
pub(crate) mod subprocess;

/// A value for [`Core::set`].
#[derive(Debug, Clone, Copy)]
pub(crate) enum PropValue<'a> {
    Flag(bool),
    Int(i64),
    Double(f64),
    Text(&'a str),
}

/// How an observed property is delivered.
#[derive(Debug, Clone, Copy)]
pub(crate) enum ObserveAs {
    Flag,
    Int,
    Double,
    Text,
    Node,
}

/// The properties the player mirrors, and the format each is observed in.
pub(crate) const OBSERVED: &[(&str, ObserveAs)] = &[
    ("pause", ObserveAs::Flag),
    ("mute", ObserveAs::Flag),
    ("time-pos", ObserveAs::Double),
    ("duration", ObserveAs::Double),
    ("brightness", ObserveAs::Double),
    ("contrast", ObserveAs::Double),
    ("saturation", ObserveAs::Double),
    ("gamma", ObserveAs::Double),
    ("hue", ObserveAs::Double),
    ("volume", ObserveAs::Double),
    ("seeking", ObserveAs::Flag),
    ("chapter", ObserveAs::Int),
    ("cache-buffering-state", ObserveAs::Int),
    ("track-list", ObserveAs::Node),
    ("chapter-list", ObserveAs::Node),
    ("deinterlace", ObserveAs::Text),
    ("hwdec-current", ObserveAs::Text),
    ("video-params/colormatrix", ObserveAs::Text),
    ("video-params/gamma", ObserveAs::Text),
];

/// Something mpv reported.
#[derive(Debug)]
pub(crate) enum CoreEvent {
    FileLoaded,
    Ended(EndReason),
    Property(String, PropertyData),
    Log { prefix: String, text: String },
}

/// What [`Core::pull`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Pulled {
    /// Nothing new to show.
    Nothing,
    /// mpv repeated its last frame. Only the swap needs reporting.
    #[cfg_attr(not(feature = "in-process"), allow(dead_code))]
    Repeat,
    /// A frame is in the upload texture. `swap` says whether mpv owes a swap report.
    Frame { swap: bool },
}

/// The operations a [`crate::Player`] needs from an mpv core.
pub(crate) trait Core: Send {
    /// `mpv_command`. May fail with [`Error::Mpv`] or, for a child process, [`Error::HostGone`].
    fn command(&self, args: &[&str]) -> Result<(), Error>;

    /// `mpv_set_property`.
    fn set(&self, name: &str, value: PropValue<'_>) -> Result<(), Error>;

    /// Read a numeric property from the core itself, not from a cache.
    fn get_double(&self, name: &str) -> Option<f64>;

    /// The next event mpv has queued, if any. Never blocks.
    fn next_event(&mut self) -> Option<CoreEvent>;

    /// The slot size the picture is rendered at, or none.
    fn set_slot(&mut self, size: Option<SlotSize>) -> Result<(), Error>;

    /// Look for a frame to show and, when there is one, upload it into the
    /// texture `gpu` is about to be drawn from. `repaint` asks for the current
    /// picture to be drawn again even if mpv has no new frame, as after a resize.
    fn pull(
        &mut self,
        repaint: bool,
        gpu: &Gpu,
        queue: &wgpu::Queue,
        stats: &mut Stats,
    ) -> Result<Pulled, Error>;

    /// The picture from the last [`Core::pull`] has been drawn. `swap` is
    /// whether mpv is told, which is what paces its video timing.
    fn finish(&mut self, swap: bool);

    /// The file finished loading. A chance to log what mpv chose.
    fn on_file_loaded(&self) {}

    /// Extra text for the once-a-second statistics line.
    fn stats_note(&mut self) -> String {
        String::new()
    }

    /// Fails once the core is gone and nothing more will arrive.
    fn check(&self) -> Result<(), Error> {
        Ok(())
    }
}
