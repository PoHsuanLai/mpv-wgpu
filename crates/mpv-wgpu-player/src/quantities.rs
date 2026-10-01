//! Bounded numbers mpv exposes as properties.

use std::fmt;

use crate::types::Finite;

/// Output volume as a percent of the source level, `0..=150`.
///
/// mpv's own `volume` has no hard top; the player raises `volume-max` to
/// [`Volume::MAX`] so every value here is accepted. Above 100 the audio filter
/// amplifies and can clip. Construction saturates, so no value is out of range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Volume(u8);

impl Volume {
    /// Silence.
    pub const MIN: Volume = Volume(0);
    /// mpv's default level, the source's own.
    pub const DEFAULT: Volume = Volume(100);
    /// The loudest value, 150 percent.
    pub const MAX: Volume = Volume(150);

    /// `percent`, saturated at [`Volume::MAX`].
    pub fn new(percent: u32) -> Self {
        Self(percent.min(u32::from(Self::MAX.0)) as u8)
    }

    /// The percent, `0..=150`.
    pub const fn percent(self) -> u8 {
        self.0
    }

    /// Round and clamp an mpv `volume` reading. Non-finite readings are `None`.
    pub(crate) fn from_mpv(value: f64) -> Option<Self> {
        value
            .is_finite()
            .then(|| Self(value.round().clamp(0.0, f64::from(Self::MAX.0)) as u8))
    }

    pub(crate) fn to_mpv(self) -> f64 {
        f64::from(self.0)
    }
}

impl fmt::Display for Volume {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}%", self.0)
    }
}

/// Playback rate as a multiple of normal speed, `0.01..=100`, in thousandths.
///
/// Stored as an integer so the value is `Eq`. mpv's own range is `0.01..=100`;
/// construction saturates into it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Speed(u32);

impl Speed {
    /// Normal speed, 1.0x.
    pub const NORMAL: Speed = Speed(1000);
    const MIN_THOUSANDTHS: u32 = 10;
    const MAX_THOUSANDTHS: u32 = 100_000;

    /// `ratio` as a speed, clamped to `0.01..=100` and rounded to thousandths.
    pub fn from_ratio(ratio: Finite) -> Self {
        let thousandths = (ratio.get() * 1000.0).round().clamp(
            f64::from(Self::MIN_THOUSANDTHS),
            f64::from(Self::MAX_THOUSANDTHS),
        );
        Self(thousandths as u32)
    }

    /// The multiple of normal speed.
    pub fn ratio(self) -> f64 {
        f64::from(self.0) / 1000.0
    }

    pub(crate) fn from_mpv(value: f64) -> Option<Self> {
        Finite::new(value).map(Self::from_ratio)
    }
}

impl fmt::Display for Speed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}x", self.ratio())
    }
}

/// A fill level, `0..=100`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Percent(u8);

impl Percent {
    /// Nothing buffered.
    pub const EMPTY: Percent = Percent(0);
    /// Fully buffered.
    pub const FULL: Percent = Percent(100);

    /// `value`, saturated at 100.
    pub fn new(value: u32) -> Self {
        Self(value.min(100) as u8)
    }

    /// The percent, `0..=100`.
    pub const fn get(self) -> u8 {
        self.0
    }

    pub(crate) fn from_mpv(value: i64) -> Self {
        Self::new(u32::try_from(value.max(0)).unwrap_or(u32::MAX))
    }
}

impl fmt::Display for Percent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}%", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::{Percent, Speed, Volume};
    use crate::types::Finite;

    #[test]
    fn volume_new_saturates_at_the_maximum() {
        const CASES: &[(&str, u32, u8)] = &[
            ("zero", 0, 0),
            ("default", 100, 100),
            ("top", 150, 150),
            ("one over", 151, 150),
            ("far over", u32::MAX, 150),
        ];
        for (name, input, expected) in CASES {
            assert_eq!(Volume::new(*input).percent(), *expected, "{name}");
        }
    }

    #[test]
    fn volume_from_mpv_rounds_clamps_and_rejects_non_finite() {
        const CASES: &[(&str, f64, Option<u8>)] = &[
            ("exact", 100.0, Some(100)),
            ("rounds down", 42.4, Some(42)),
            ("rounds up", 42.5, Some(43)),
            ("negative clamps to silence", -5.0, Some(0)),
            ("above the top clamps", 400.0, Some(150)),
            ("nan", f64::NAN, None),
            ("infinity", f64::INFINITY, None),
        ];
        for (name, input, expected) in CASES {
            assert_eq!(
                Volume::from_mpv(*input).map(Volume::percent),
                *expected,
                "{name}"
            );
        }
    }

    #[test]
    fn volume_constants_hold_the_documented_range() {
        assert_eq!(Volume::MIN.percent(), 0);
        assert_eq!(Volume::DEFAULT.percent(), 100);
        assert_eq!(Volume::MAX.percent(), 150);
        assert_eq!(Volume::DEFAULT.to_string(), "100%");
    }

    #[test]
    fn speed_from_ratio_clamps_and_rounds_to_thousandths() {
        const CASES: &[(&str, f64, f64)] = &[
            ("normal", 1.0, 1.0),
            ("double", 2.0, 2.0),
            ("rounds", 1.2345, 1.235),
            ("below the floor", 0.0, 0.01),
            ("negative", -3.0, 0.01),
            ("above the ceiling", 500.0, 100.0),
        ];
        for (name, input, expected) in CASES {
            let finite = Finite::new(*input).expect("finite case");
            assert_eq!(Speed::from_ratio(finite).ratio(), *expected, "{name}");
        }
        assert_eq!(Speed::NORMAL.ratio(), 1.0);
    }

    #[test]
    fn speed_from_mpv_rejects_non_finite() {
        assert_eq!(Speed::from_mpv(1.5).map(Speed::ratio), Some(1.5));
        assert_eq!(Speed::from_mpv(f64::NAN), None);
    }

    #[test]
    fn percent_saturates_and_maps_negative_to_empty() {
        const CASES: &[(&str, i64, u8)] = &[
            ("empty", 0, 0),
            ("half", 50, 50),
            ("full", 100, 100),
            ("over", 250, 100),
            ("negative", -7, 0),
        ];
        for (name, input, expected) in CASES {
            assert_eq!(Percent::from_mpv(*input).get(), *expected, "{name}");
        }
    }
}
