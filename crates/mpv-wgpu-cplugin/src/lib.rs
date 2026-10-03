//! The mpv C plugin for `mpv-wgpu-player`'s subprocess mode.
//!
//! mpv loads this library with `--script=` and calls [`mpv_open_cplugin`]. The
//! plugin makes a software render context, connects to the player over the
//! socket named in `MPV_WGPU_SOCKET`, and then:
//!
//! - renders each frame as `rgb0` into a free slot of the memfd ring the player
//!   sent, and announces it with a `Frame` message;
//! - calls `mpv_render_context_report_swap` only after the player answers
//!   `Presented`, so mpv's frame timing follows the real presentation;
//! - runs the player's commands and property writes and reads, observes the
//!   properties it asks for, and forwards events and log lines.
//!
//! It declares the mpv functions it uses with no `#[link]`; they bind to the
//! running `mpv` executable, so the library never links libmpv.
//!
//! The crate has no public API besides the exported `mpv_open_cplugin` symbol.

#![deny(unsafe_op_in_unsafe_fn)]

mod convert;
mod ffi;
mod schedule;

#[cfg(not(test))]
mod plugin;

#[cfg(not(test))]
pub use plugin::mpv_open_cplugin;
