//! The wire protocol between `mpv-wgpu-player` and the plugin in the user's mpv.
//!
//! The player runs the stock `mpv` binary with `mpv-wgpu-cplugin` loaded as a C
//! plugin. The two talk over one `SOCK_SEQPACKET` Unix socket:
//!
//! 1. The player listens on a random abstract socket name and starts mpv with the
//!    name in `MPV_WGPU_SOCKET`. The plugin connects and sends [`Message::Hello`].
//! 2. The player creates a [`Ring`] for the video slot and sends
//!    [`Message::Resize`] with the memfd attached.
//! 3. For each frame mpv's software renderer writes `rgb0` pixels into a free slot
//!    and the plugin sends [`Message::Frame`]. The player uploads the slot to the
//!    GPU and answers [`Message::Presented`], which the plugin turns into
//!    `mpv_render_context_report_swap`. A frame the player skipped is answered
//!    with [`Message::Released`] instead.
//! 4. Commands, property writes and reads carry an id and are answered by
//!    [`Message::Reply`]. Observed properties and mpv's events flow the other way.
//!
//! This crate has no libmpv in it: the encoding, the socket and the ring only.

#![deny(clippy::unwrap_used)]
#![deny(unsafe_op_in_unsafe_fn)]

mod channel;
mod message;
mod ring;

pub use channel::{Channel, Listener, monotonic_ns};
pub use message::{
    DecodeError, EncodeError, Format, Frame, Hello, MAX_MESSAGE, Message, PROTOCOL_VERSION, Resize,
    SlotRef, Value,
};
pub use ring::{Ring, RingError, RingLayout, SLOTS, row_stride};
