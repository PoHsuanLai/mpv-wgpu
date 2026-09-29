//! Shared picture controls and renderer errors.

use std::fmt;

/// Equalizer channel in the `-100..=100` range. The constructor saturates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnitBias(i32);

impl UnitBias {
    /// Clamp `value` into `-100..=100`.
    pub const fn new(value: i32) -> Self {
        let value = if value < -100 {
            -100
        } else if value > 100 {
            100
        } else {
            value
        };
        Self(value)
    }

    /// The clamped channel.
    pub const fn get(self) -> i32 {
        self.0
    }
}

impl fmt::Display for UnitBias {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Hue in degrees, saturating into `-180..=180`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hue(i32);

impl Hue {
    /// Clamp `value` into `-180..=180`.
    pub const fn new(value: i32) -> Self {
        let value = if value < -180 {
            -180
        } else if value > 180 {
            180
        } else {
            value
        };
        Self(value)
    }

    /// The clamped hue in degrees.
    pub const fn get(self) -> i32 {
        self.0
    }
}

impl fmt::Display for Hue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Brightness, contrast, saturation, gamma, and hue.
///
/// The picture renderer applies this once, in linear light. The libmpv player
/// sends the same values to mpv and also bakes them into its blit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Equalizer {
    /// `-100..=100`.
    pub brightness: UnitBias,
    /// `-100..=100`.
    pub contrast: UnitBias,
    /// `-100..=100`.
    pub saturation: UnitBias,
    /// `-100..=100`.
    pub gamma: UnitBias,
    /// `-180..=180` degrees.
    pub hue: Hue,
}

impl Default for Equalizer {
    fn default() -> Self {
        Self {
            brightness: UnitBias::new(0),
            contrast: UnitBias::new(0),
            saturation: UnitBias::new(0),
            gamma: UnitBias::new(0),
            hue: Hue::new(0),
        }
    }
}

impl fmt::Display for Equalizer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "brightness:{},contrast:{},saturation:{},gamma:{},hue:{}",
            self.brightness, self.contrast, self.saturation, self.gamma, self.hue
        )
    }
}

/// Failure from a picture draw.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// A plane or rectangle does not fit in `usize`.
    #[error("picture size overflows")]
    InvalidSize,
    /// Creating a texture, buffer, or pipeline failed.
    #[error("gpu allocation failed")]
    Gpu,
    /// The target texture format does not match the requested encoding.
    #[error("target format does not match the encoding")]
    Target,
}
