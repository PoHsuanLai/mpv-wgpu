//! Embed libmpv as a headless software decoder and sample the picture from wgpu.
//!
//! The host owns the [`wgpu::Device`], the window, and the swapchain. [`Player`]
//! uploads libmpv's packed RGB software frame into a gamma-encoded
//! `Rgba8Unorm` texture the host samples from its own pass.
//!
//! [`Picture::Shown`] texels are already gamma-encoded. Sample the view as
//! non-sRGB data. Writing those values into an sRGB swapchain encodes them again.

#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used)]

mod color;
mod cubic;
mod equalizer;
mod frame_buffer;
mod pipeline;
mod player;
mod present;
mod types;

pub use player::Player;
pub use types::{
    Adjust, Deinterlace, EndReason, Equalizer, Error, Event, Finite, Hue, MpvError, Mute, Outcome,
    Picture, Playback, Presentation, Seek, Slot, SlotSize, UnitBias,
};
