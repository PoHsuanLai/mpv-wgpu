//! Public values. Invalid combinations are separate variants or newtypes.

use std::fmt;
use std::num::NonZeroU32;

use crate::quantities::Percent;

/// Video slot the host wants mpv to fill, in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    /// No picture target. [`crate::Player::poll`] skips GPU work.
    Empty,
    /// A non-zero rectangle.
    Sized(SlotSize),
}

/// Non-zero physical size of the video slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlotSize {
    /// Width in physical pixels.
    pub width: NonZeroU32,
    /// Height in physical pixels.
    pub height: NonZeroU32,
}

impl SlotSize {
    /// Byte length of one tightly described row before stride padding, times height.
    #[cfg(feature = "in-process")]
    pub(crate) fn byte_len(self, stride: usize) -> Result<usize, Error> {
        stride
            .checked_mul(self.height.get() as usize)
            .ok_or(Error::InvalidSize)
    }
}

/// A finite `f64`. NaN and infinities cannot be constructed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Finite(f64);

impl Finite {
    /// Returns `None` when `value` is NaN or infinite.
    pub fn new(value: f64) -> Option<Self> {
        if value.is_finite() {
            Some(Self(value))
        } else {
            None
        }
    }

    /// The contained seconds or delta.
    pub const fn get(self) -> f64 {
        self.0
    }
}

impl fmt::Display for Finite {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Where a seek lands.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Seek {
    /// Seconds from the current position. Negative seeks backward.
    Relative(Finite),
    /// Seconds from the start of the file.
    Absolute(Finite),
}

/// Relative adjustments of picture zoom and volume.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Adjust {
    /// Add to mpv's `panscan` (`0.0..=1.0` in the player; this is the delta).
    Panscan(Finite),
    /// Add to mpv's `video-zoom` (log2 scale).
    Zoom(Finite),
    /// Add to mpv's `volume`.
    Volume(Finite),
}

/// Whether the file is advancing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Playback {
    /// Frames and audio advance.
    Playing,
    /// Picture and audio are held.
    Paused,
}

impl fmt::Display for Playback {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Playback::Playing => "playing",
            Playback::Paused => "paused",
        })
    }
}

/// mpv's `deinterlace` choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Deinterlace {
    /// Never deinterlace.
    No,
    /// Always deinterlace.
    Yes,
    /// Deinterlace when the frame is tagged interlaced.
    ///
    /// libmpv 0.37 only knows `yes` and `no`; there [`crate::Player::set_deinterlace`]
    /// returns an mpv error for this mode and the player starts on [`Deinterlace::No`].
    Auto,
}

impl Deinterlace {
    pub(crate) fn as_mpv(self) -> &'static str {
        match self {
            Deinterlace::No => "no",
            Deinterlace::Yes => "yes",
            Deinterlace::Auto => "auto",
        }
    }

    pub(crate) fn parse(text: &str) -> Option<Self> {
        match text {
            "no" => Some(Deinterlace::No),
            "yes" => Some(Deinterlace::Yes),
            "auto" => Some(Deinterlace::Auto),
            _ => None,
        }
    }
}

impl fmt::Display for Deinterlace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_mpv())
    }
}

/// mpv's `mute` flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mute {
    /// Audio is audible.
    Off,
    /// Audio is silenced.
    On,
}

impl fmt::Display for Mute {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Mute::Off => "off",
            Mute::On => "on",
        })
    }
}

/// What [`crate::Player::poll`] did to the public texture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub struct Outcome {
    /// Whether the texels a host would sample changed.
    pub presentation: Presentation,
}

/// Whether the public texture changed on this poll.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Presentation {
    /// Same texels as before this poll.
    Unchanged,
    /// The public texture was rewritten.
    Updated,
}

impl fmt::Display for Presentation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Presentation::Unchanged => "unchanged",
            Presentation::Updated => "updated",
        })
    }
}

/// Sampleable picture. [`Picture::Shown`] borrows the player.
///
/// `Shown` texels are gamma-encoded `Rgba8Unorm` with alpha 1 and a top-left
/// origin. Do not view them as sRGB.
#[derive(Debug)]
pub enum Picture<'a> {
    /// No frame has been submitted for the current slot.
    Waiting,
    /// Gamma-encoded RGBA the host samples in its own pass.
    Shown(&'a wgpu::TextureView),
}

/// Playback notifications drained by the latest [`crate::Player::poll`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// mpv finished opening the file.
    Loaded,
    /// The file stopped. With `keep-open`, the last picture remains.
    Ended(EndReason),
    /// Pause state changed.
    Playback(Playback),
    /// A seek landed and playback restarted from the new position.
    SeekDone,
    /// The demuxer cache level moved. Local files stay at [`Percent::FULL`].
    Buffering(Percent),
    /// The track list or the selected tracks changed.
    TracksChanged,
    /// The chapter list changed.
    ChaptersChanged,
    /// The volume changed, whoever asked for it. Read it with [`crate::Player::volume`].
    VolumeChanged,
}

/// Why mpv stopped a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndReason {
    /// Reached the end.
    Eof,
    /// Stopped by command.
    Stop,
    /// The player is quitting.
    Quit,
    /// The playlist entry was redirected.
    Redirect,
    /// mpv reported an error reason.
    Error,
}

/// Failure from the player.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// libmpv rejected a call.
    #[error("mpv error {0}")]
    Mpv(MpvError),
    /// The slot's byte size does not fit in `usize`.
    #[error("slot size overflows")]
    InvalidSize,
    /// Creating a texture, buffer, or pipeline failed.
    #[error("gpu allocation failed")]
    Gpu,
    /// A file path is not valid UTF-8, which mpv commands need.
    #[error("path is not valid utf-8")]
    PathNotUtf8,
    /// The chapter index is past the last chapter of the file.
    #[error("no chapter at index {0}")]
    NoSuchChapter(u32),
    /// The mpv child process exited or crashed, or its plugin closed the socket.
    ///
    /// Subprocess mode only. Every later call fails the same way; make a new
    /// [`crate::Player`] to play again.
    #[error("the mpv process is gone")]
    HostGone,
    /// The mpv child process did not answer in time. It may be hung.
    ///
    /// Subprocess mode only.
    #[error("the mpv process did not answer")]
    HostTimeout,
    /// The mpv core could not be started: mpv or its plugin was not found, the
    /// versions do not match, or this host mode was compiled out.
    #[error("cannot start mpv: {0}")]
    HostStart(String),
}

/// Integer code from libmpv.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MpvError {
    /// `mpv_error` value.
    pub code: i32,
}

impl fmt::Display for MpvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.code)
    }
}

impl std::error::Error for MpvError {}

#[cfg(feature = "in-process")]
pub(crate) fn map_mpv(err: rsmpv::Error) -> Error {
    Error::Mpv(MpvError {
        code: err.raw_code().unwrap_or(-1),
    })
}
