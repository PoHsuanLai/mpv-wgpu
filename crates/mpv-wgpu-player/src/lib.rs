//! libmpv with the VO window removed.
//!
//! [`Player`] is a libmpv client on `vo=libmpv` and `MPV_RENDER_API_TYPE_SW`.
//! lavf, lavc, the AO, libass, and the playloop stay in mpv. The software
//! target replaces `vo=gpu` / `vo=gpu-next` and mpv's window: mpv letterboxes
//! into the slot, burns OSD, and [`Player::poll`] uploads the `rgb0` image
//! 1:1. [`Picture::Shown`] is gamma-encoded `Rgba8Unorm`, alpha 1, top-left.
//! Sample it as non-sRGB data. An sRGB swapchain encodes it again.
//!
//! The `mpv-wgpu` crate is the gpu-next picture chain for planes the caller
//! already has. This player does not pass the composited frame through it.
//!
//! # Requirements
//!
//! The build links libmpv. `pkg-config` must resolve the `mpv` module. The core
//! starts with `vo=libmpv`, `hwdec=auto-safe`, `video-sync=audio`, `idle=yes`,
//! `keep-open=yes`, subtitles visible, and `deinterlace=auto`. The audio driver
//! comes from [`PlayerOptions::audio_output`]; [`AudioOutput::Auto`] leaves mpv
//! to probe.
//! The on-screen controller and the default key bindings are off.
//! [`Player::command`] forwards any other `mpv_command`.
//!
//! Hardware decode may run inside libmpv. The sampled texture is still the
//! software RGB frame, already scaled and letterboxed to the slot.
//!
//! # Example
//!
//! ```ignore
//! use std::num::NonZeroU32;
//!
//! use mpv_wgpu_player::{Picture, Player, PlayerOptions, Slot, SlotSize};
//!
//! fn start(device: &wgpu::Device, queue: &wgpu::Queue, path: &str) -> Result<(), mpv_wgpu_player::Error> {
//!     let mut player = Player::new(device, queue, PlayerOptions::default())?;
//!     player.set_slot(Slot::Sized(SlotSize {
//!         width: NonZeroU32::new(1280).expect("non-zero"),
//!         height: NonZeroU32::new(720).expect("non-zero"),
//!     }))?;
//!     player.load(path)?;
//!     player.poll()?;
//!     if let Picture::Shown(view) = player.picture() {
//!         let _sampled = view;
//!     }
//!     Ok(())
//! }
//! ```
//!
//! # State and audio-only files
//!
//! [`Player::tracks`], [`Player::chapters`], [`Player::volume`] and the seek and
//! cache state follow observed mpv properties and surface as [`Event`]s.
//! Audio-only files keep [`Player::picture`] at [`Picture::Waiting`];
//! embedded cover art plays as a video track and is shown
//! ([`Player::has_video`] is [`VideoPresence::CoverArt`]).
//!
//! # Threading
//!
//! [`Player`] is [`Send`] and not [`Sync`]. Call [`Player::poll`] on the thread
//! that presents. [`Player::set_notify`] may run on an mpv thread and must only
//! wake the host. `report_swap` runs only after a poll that consumed a new frame.
//!
//! # Equalizer
//!
//! [`Equalizer`] is brightness, contrast, saturation, and gamma in −100..=100,
//! and hue in −180..=180 degrees. mpv's hue property is −100..=100. The player
//! sends `degrees * 100 / 180` and inverts an echo with `round(raw * 180 / 100)`
//! before rounding the mpv value to an integer.
//!
//! [`Player::set_equalizer`] writes the knobs into libmpv and the blit applies
//! those same knobs again. A non-zero grade is applied twice. All zeros stay
//! identity on both stages.

#![forbid(unsafe_code)]
#![cfg_attr(
    not(any(feature = "in-process", feature = "subprocess")),
    allow(unused)
)]
#![deny(clippy::unwrap_used)]

#[cfg(not(any(feature = "in-process", feature = "subprocess")))]
compile_error!("enable the `in-process` or the `subprocess` feature of mpv-wgpu-player");

mod chapters;
mod controls;
mod core;
#[cfg(feature = "in-process")]
mod frame_buffer;
mod media;
mod nodes;
mod notify;
mod options;
mod pipeline;
mod player;
mod quantities;
mod stats;
mod tracks;
mod types;
mod value;

pub use chapters::{Chapter, ChapterIndex};
pub use controls::{Direction, ScreenshotContent, VideoPresence};
pub use mpv_wgpu::{Equalizer, Hue, UnitBias};
pub use options::{AudioOutput, Host, PlayerOptions, SubprocessOptions};
pub use player::Player;
pub use quantities::{Percent, Speed, Volume};
pub use tracks::{
    Track, TrackArt, TrackChoice, TrackDefault, TrackId, TrackKind, TrackList, TrackOrigin,
    TrackSelection,
};
pub use types::{
    Adjust, Deinterlace, EndReason, Error, Event, Finite, MpvError, Mute, Outcome, Picture,
    Playback, Presentation, Seek, Slot, SlotSize,
};
