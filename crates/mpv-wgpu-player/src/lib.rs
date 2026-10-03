//! mpv with the VO window removed.
//!
//! [`Player`] runs mpv on `vo=libmpv` with `MPV_RENDER_API_TYPE_SW`. lavf, lavc,
//! the AO, libass, and the playloop stay in mpv. The software target replaces
//! `vo=gpu` / `vo=gpu-next` and mpv's window: mpv letterboxes into the slot,
//! burns OSD, and [`Player::poll`] uploads the `rgb0` image 1:1.
//! [`Picture::Shown`] is gamma-encoded `Rgba8Unorm`, alpha 1, top-left.
//! Sample it as non-sRGB. An sRGB swapchain encodes it again.
//!
//! The `mpv-wgpu` crate is the gpu-next picture chain for planes the caller
//! already has. This player does not pass the composited frame through it.
//!
//! # Two hosts for mpv
//!
//! Where mpv runs is a [`Host`], chosen with [`Player::with_host`]. Everything
//! else in the API is the same in both.
//!
//! | | [`Host::InProcess`] | [`Host::Subprocess`] |
//! | --- | --- | --- |
//! | cargo feature | `in-process` (default) | `subprocess` |
//! | mpv | libmpv linked into your binary (`pkg-config` must find `mpv`) | the user's own `mpv` executable, run as a child |
//! | links | libmpv, so GPL or LGPL code | nothing from mpv |
//! | frames | rendered into a CPU buffer, uploaded | rendered by mpv straight into a shared-memory ring, uploaded from the mapping |
//! | needs at run time | libmpv | `mpv` on `PATH` (or `MPV_WGPU_MPV`) and `libmpv_wgpu_cplugin.so` |
//!
//! Build with `default-features = false, features = ["subprocess"]` and no
//! libmpv, `rsmpv` or `libmpv-sys` is anywhere in the dependency tree. Subprocess
//! mode is Linux only for now.
//!
//! In subprocess mode the player starts `mpv --no-config --idle=yes --vo=libmpv
//! --video-timing-offset=0 --script=<plugin>` and talks to the
//! `mpv-wgpu-cplugin` library inside it over a private socket. mpv must be built
//! with C plugin support, as Fedora's and Debian's are. See [`SubprocessOptions`]
//! for how the executable and the plugin are found, and `mpv-wgpu-protocol` for the
//! wire format.
//!
//! What differs in subprocess mode:
//!
//! - Every call that talks to mpv (`load`, `seek`, `set_*`, `select_track`,
//!   [`Player::command`], and the reads of [`Player::volume`] and
//!   [`Player::speed`], which ask the core) is a round trip over the socket, tens
//!   of microseconds when mpv is idle. The call returns mpv's own error as
//!   [`Error::Mpv`]. If mpv does not answer for 10 seconds the call returns
//!   [`Error::HostTimeout`]. The typed getters ([`Player::tracks`],
//!   [`Player::position`], ...) read observed-property caches in both modes and
//!   never block.
//! - Events, observed properties and mpv's log lines arrive on a reader thread
//!   and are applied by the next [`Player::poll`], which [`Player::set_notify`]
//!   wakes. The wake closure runs on that thread.
//! - Frames are written by mpv into a three-slot ring. If the host polls slower
//!   than mpv renders, the newest frame wins and older ones are skipped.
//! - Paths and relative file names in [`Player::command`] and
//!   [`Player::screenshot_to_file`] are resolved by the mpv process, which has this
//!   process's working directory.
//! - If mpv exits or crashes, the next [`Player::poll`] returns
//!   [`Error::HostGone`] and so does every later call. Make a new [`Player`] to
//!   play again. Dropping a [`Player`] asks mpv to quit and kills it after a
//!   short grace period. mpv's own output goes to the `log` crate.
//! - A [`Player`] that cannot start mpv fails with [`Error::HostStart`], whose
//!   text names what was missing.
//!
//! # Requirements
//!
//! The `in-process` feature links libmpv. `pkg-config` must resolve the `mpv`
//! module. Either host starts mpv with `vo=libmpv`, `hwdec=auto-safe`,
//! `video-sync=audio`, `idle=yes`, `keep-open=yes`, subtitles visible, and
//! `deinterlace=auto`. The audio driver
//! comes from [`PlayerOptions::audio_output`]; [`AudioOutput::Auto`] leaves mpv
//! to probe.
//! The on-screen controller and the default key bindings are off.
//! [`Player::command`] forwards any other `mpv_command`.
//!
//! Hardware decode may run inside mpv. The sampled texture is still the
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
//! wake the host. `report_swap` runs only after a poll that consumed a new frame;
//! in subprocess mode the player tells the plugin the frame was presented and the
//! plugin makes the call.
//!
//! # Equalizer
//!
//! [`Equalizer`] is brightness, contrast, saturation, and gamma in −100..=100,
//! and hue in −180..=180 degrees. mpv's hue property is −100..=100. The player
//! sends `degrees * 100 / 180` and inverts an echo with `round(raw * 180 / 100)`
//! before rounding the mpv value to an integer.
//!
//! [`Player::set_equalizer`] writes the knobs into mpv and the blit applies
//! those same knobs again. A non-zero grade is applied twice. All zeros stay
//! identity on both stages.

#![forbid(unsafe_code)]
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
