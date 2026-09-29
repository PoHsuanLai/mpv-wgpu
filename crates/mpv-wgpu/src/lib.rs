//! Draw caller-owned YUV or RGBA pictures into a caller-owned [`wgpu::Texture`].
//!
//! The host owns the [`wgpu::Device`], the queue, the window, and the swapchain.
//! [`Renderer`] samples the caller's planes and writes a caller-owned target.
//! It does not open a window and it does not link libmpv. File playback lives
//! in the `mpv-wgpu-player` crate, which does not feed its composited RGB frame
//! through this renderer.
//!
//! # Pipeline
//!
//! [`Renderer::draw`] uploads or binds each plane, decodes to linear light at
//! luma resolution, and scales into [`Draw::dest`].
//!
//! On [`Encoding::Gamma8`] the present order is the spline, then the five
//! equalizer knobs once in linear light, then the inverse transfer, then an
//! 8×8 ordered dither. [`Encoding::Linear`] skips the spline and the inverse
//! transfer. Contrast −100 is linear `0.5`. That value stays `0.5` on a linear
//! target. On `Gamma8` it is encoded first. BT.1886 of `0.5` is about `0.749`.
//!
//! # Example
//!
//! ```no_run
//! use mpv_wgpu::{
//!     ChromaSiting, Coefficients, Draw, Encoding, Equalizer, Levels, Picture, PixelRect, Plane,
//!     PlaneBits, PlaneSource, QuarterTurn, Renderer, Transfer,
//! };
//!
//! fn draw_limited(
//!     device: &wgpu::Device,
//!     queue: &wgpu::Queue,
//!     target: &wgpu::Texture,
//! ) -> Result<(), mpv_wgpu::Error> {
//!     let mut renderer = Renderer::new(device)?;
//!     let y_bytes = [16u8, 16, 235, 235];
//!     let chroma = [128u8];
//!     let y = Plane {
//!         width: 2,
//!         height: 2,
//!         bits: PlaneBits::Eight,
//!         source: PlaneSource::Bytes(&y_bytes),
//!     };
//!     let u = Plane {
//!         width: 1,
//!         height: 1,
//!         bits: PlaneBits::Eight,
//!         source: PlaneSource::Bytes(&chroma),
//!     };
//!     let v = Plane {
//!         width: 1,
//!         height: 1,
//!         bits: PlaneBits::Eight,
//!         source: PlaneSource::Bytes(&chroma),
//!     };
//!     renderer.draw(
//!         device,
//!         queue,
//!         Draw {
//!             picture: Picture::Yuv { y, u, v },
//!             matrix: Coefficients::Bt709,
//!             levels: Levels::Limited,
//!             transfer: Transfer::Bt1886,
//!             siting: ChromaSiting::Center,
//!             peak_nits: 100.0,
//!             dest: PixelRect { x: 0, y: 0, width: 2, height: 2 },
//!             rotation: QuarterTurn::D0,
//!             equalizer: Equalizer::default(),
//!             overlays: &[],
//!             target,
//!             encoding: Encoding::Gamma8,
//!             target_peak_nits: 100.0,
//!         },
//!     )
//! }
//! ```
//!
//! # Planes
//!
//! [`Picture::Yuv`] is Y plus U and V. U and V must share a size and may be
//! smaller than Y. [`Picture::Rgba`] is one 8-bit plane whose RGB is the
//! display-referred signal. 16-bit RGBA returns [`Error::InvalidSize`].
//!
//! [`PlaneBits::Eight`] is one byte per sample, stored as `R8Unorm` (or
//! `Rgba8Unorm` for RGBA). [`PlaneBits::Sixteen`] is a little-endian `u16`,
//! stored as `R16Unorm`, and needs the device feature `TEXTURE_FORMAT_16BIT_NORM`.
//! Without that feature [`Error::Gpu`] is returned. [`PlaneSource::Bytes`] is
//! tightly packed. The upload pads rows to 256 bytes. [`PlaneSource::Texture`]
//! is a view the caller already owns, and its `bits` field is ignored.
//!
//! # Color
//!
//! [`Coefficients`] is BT.601, BT.709, or BT.2020. [`Levels::Limited`] maps
//! code 16 to 0 and code 235 to 1, as 8-bit fractions of the normalized signal,
//! including for 16-bit planes. [`Levels::Full`] maps 0 to 0 and the peak code
//! to 1. [`ChromaSiting`] is an offset in luma pixels: `TopLeft` `[0, 0]`,
//! `Left` `[0, 0.5]`, `Center` `[0.5, 0.5]`.
//!
//! [`Transfer::Bt1886`] is a 2.4 power. [`Transfer::Srgb`] is a pure 2.2 power.
//! [`Transfer::Pq`] maps signal 0 to 0 nits and signal 1 to 10000 nits.
//! [`Transfer::Hlg`] maps signal 0 to 0. At a 1000-nit peak, signal 0.75 is the
//! BT.2408 reference white, about 203 nits. Linear light uses 1.0 = 100 nits,
//! so a PQ peak is the linear value 100.
//!
//! # Scale and overlays
//!
//! Each axis is a separable 4-tap cubic. Destination pixel `i` samples
//! `(i + 0.5) * src_len / dst_len - 0.5`. A longer destination axis uses
//! Catmull-Rom. A shorter or equal axis uses Hermite. [`QuarterTurn::D90`]
//! turns the source clockwise inside the destination, with y pointing down.
//!
//! [`Draw::overlays`] are premultiplied RGBA8 bitmaps composited after the
//! picture. An empty slice draws none.
//!
//! # Targets
//!
//! [`Encoding::Gamma8`] requires `Rgba8Unorm` and stores code / 255.
//! [`Encoding::Linear`] requires `Rgba16Float`. Any other format is
//! [`Error::Target`]. The target is cleared to opaque black, then the
//! destination rectangle is written.
//!
//! [`decode_linear`] decodes one sample. [`renderer::placed_sample`] scales and
//! presents one pixel. The shader matches those functions.
//!
//! # What stays with the host
//!
//! The window, the swapchain, demux, decode, audio, the playback clock,
//! subtitle shaping, and import of hardware-decoder surfaces. Shader text
//! shipped with the crate is static.

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
pub use renderer::{Draw, Overlay, Picture, PixelRect, Plane, PlaneBits, PlaneSource, Renderer};
pub use types::{Equalizer, Error, Hue, UnitBias};
