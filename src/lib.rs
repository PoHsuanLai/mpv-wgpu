//! Embed libmpv as a headless software decoder, and draw caller-owned planes with wgpu.
//!
//! The host owns the [`wgpu::Device`], the window, and the swapchain. [`Player`]
//! uploads libmpv's packed RGB software frame into a gamma-encoded
//! `Rgba8Unorm` texture the host samples from its own pass. [`Renderer`] draws
//! a caller-supplied YUV or RGBA picture into a caller-owned texture and does
//! not open a window.
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
pub mod renderer;
mod types;

pub use color::{
    ChromaSiting, Coefficients, Levels, Transfer, chroma_coord, decode_linear, normalize_code,
    sample_bilinear,
};
pub use cubic::QuarterTurn;
pub use player::Player;
pub use present::Encoding;
pub use renderer::Renderer;
pub use types::{
    Adjust, Deinterlace, EndReason, Equalizer, Error, Event, Finite, Hue, MpvError, Mute, Outcome,
    Picture, Playback, Presentation, Seek, Slot, SlotSize, UnitBias,
};
