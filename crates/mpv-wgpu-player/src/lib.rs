//! Headless libmpv playback into a caller-owned wgpu texture.
//!
//! The host owns the [`wgpu::Device`], the window, and the swapchain. [`Player`]
//! uploads libmpv's packed RGB software frame into a gamma-encoded
//! `Rgba8Unorm` texture the host samples from its own pass.
//!
//! [`Picture::Shown`] texels are already gamma-encoded. Sample the view as
//! non-sRGB data. Writing those values into an sRGB swapchain encodes them again.

#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used)]

mod frame_buffer;
mod pipeline;
mod player;
mod types;

pub use mpv_wgpu::{Equalizer, Hue, UnitBias};
pub use player::Player;
pub use types::{
    Adjust, Deinterlace, EndReason, Error, Event, Finite, MpvError, Mute, Outcome, Picture,
    Playback, Presentation, Seek, Slot, SlotSize,
};
