//! Draw caller-owned YUV or RGBA pictures into a caller-owned wgpu texture.
//!
//! The host owns the [`wgpu::Device`], the window, and the swapchain. [`Renderer`]
//! samples the caller's planes and writes a caller-owned target. It does not
//! open a window.

#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used)]

mod color;
mod cubic;
mod equalizer;
mod present;
pub mod renderer;
mod types;

pub use color::{
    ChromaSiting, Coefficients, Levels, Transfer, chroma_coord, decode_linear, normalize_code,
    sample_bilinear,
};
pub use cubic::QuarterTurn;
pub use equalizer::{Grade, bake};
pub use present::Encoding;
pub use renderer::{
    Draw, Overlay, Picture, PixelRect, Plane, PlaneBits, PlaneSource, Renderer,
};
pub use types::{Equalizer, Error, Hue, UnitBias};
