//! Spline tone map, one equalizer grade, and the display encode.

use crate::color::{self, Coefficients, Transfer};
use crate::types::Equalizer;

/// Where the presented sample is stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    /// Display-encoded 8-bit unorm, after ordered dither. Values are code / 255.
    Gamma8,
    /// Linear light, 1.0 = 100 nits. The tone-map knee and the inverse transfer are skipped.
    Linear,
}

/// Inputs that are not the sample itself.
#[derive(Debug, Clone, Copy)]
pub struct PresentJob {
    /// Nominal peak of the frame, in nits.
    pub source_peak_nits: f32,
    /// Peak of the target, in nits.
    pub target_peak_nits: f32,
    /// Transfer used when encoding a [`Encoding::Gamma8`] target.
    pub transfer: Transfer,
    /// Matrix whose luma weights the hue rotation uses.
    pub matrix: Coefficients,
    /// Applied once, after the tone map and before the inverse transfer.
    pub equalizer: Equalizer,
    /// [`Encoding::Linear`] skips the spline and the inverse transfer.
    pub encoding: Encoding,
}

/// 8×8 Bayer matrix, entries `0..64`.
const BAYER8: [u8; 64] = [
    0, 32, 8, 40, 2, 34, 10, 42, 48, 16, 56, 24, 50, 18, 58, 26, 12, 44, 4, 36, 14, 46, 6, 38, 60,
    28, 52, 20, 62, 30, 54, 22, 3, 35, 11, 43, 1, 33, 9, 41, 51, 19, 59, 27, 49, 17, 57, 25, 15, 47,
    7, 39, 13, 45, 5, 37, 63, 31, 55, 23, 61, 29, 53, 21,
];

/// Cubic Hermite in the ST 2084 domain.
///
/// `f(0) = 0`, `f'(0) = 1`, and the source peak lands on the target peak.
/// When the target already holds the source peak, the sample is returned unchanged.
pub fn spline_tone_map(linear: f32, source_peak_nits: f32, target_peak_nits: f32) -> f32 {
    if linear <= 0.0 {
        return 0.0;
    }
    if !(source_peak_nits > target_peak_nits) {
        return linear;
    }
    let x = color::pq_nits_to_signal(linear * 100.0);
    let x_s = color::pq_nits_to_signal(source_peak_nits).max(1.0e-6);
    let y_s = color::pq_nits_to_signal(target_peak_nits);
    let t = (x / x_s).clamp(0.0, 1.0);
    let t2 = t * t;
    let t3 = t2 * t;
    let h10 = t3 - 2.0 * t2 + t;
    let h01 = -2.0 * t3 + 3.0 * t2;
    let h11 = t3 - t2;
    let y = (h10 * x_s + h01 * y_s + h11 * y_s).clamp(0.0, 1.0);
    color::pq_nits(y) / 100.0
}

/// Equalizer on a 0..1 signal. Neutral is the identity. Contrast `-100` is mid-gray.
pub fn grade_rgb(rgb: [f32; 3], equalizer: Equalizer, matrix: Coefficients) -> [f32; 3] {
    let grade = crate::equalizer::bake(equalizer, matrix);
    let biased = color::mul_vec(grade.matrix, rgb);
    let exp = grade.gamma_exp;
    [
        (biased[0] + grade.bias[0]).max(0.0).powf(exp),
        (biased[1] + grade.bias[1]).max(0.0).powf(exp),
        (biased[2] + grade.bias[2]).max(0.0).powf(exp),
    ]
}

/// Ordered dither of a display signal into an 8-bit code, returned as code / 255.
pub fn dither_unit(display: f32, x: u32, y: u32) -> f32 {
    let threshold = BAYER8[((y % 8) * 8 + (x % 8)) as usize] as f32 / 64.0;
    let code = (display.clamp(0.0, 1.0) * 255.0 + threshold - 0.5).round();
    code.clamp(0.0, 255.0) / 255.0
}

/// Tone map, grade once, then encode.
///
/// [`Encoding::Gamma8`] runs the spline, the equalizer on that linear sample,
/// the inverse transfer, and ordered dither. [`Encoding::Linear`] skips the
/// spline and the inverse transfer, then runs the equalizer on the linear sample.
pub fn present_texel(linear: [f32; 3], x: u32, y: u32, job: &PresentJob) -> [f32; 3] {
    let mapped = match job.encoding {
        Encoding::Linear => linear,
        Encoding::Gamma8 => [
            spline_tone_map(linear[0], job.source_peak_nits, job.target_peak_nits),
            spline_tone_map(linear[1], job.source_peak_nits, job.target_peak_nits),
            spline_tone_map(linear[2], job.source_peak_nits, job.target_peak_nits),
        ],
    };
    let graded = grade_rgb(mapped, job.equalizer, job.matrix);
    match job.encoding {
        Encoding::Linear => graded,
        Encoding::Gamma8 => [
            dither_unit(
                color::encode_display(graded[0], job.target_peak_nits, job.transfer),
                x,
                y,
            ),
            dither_unit(
                color::encode_display(graded[1], job.target_peak_nits, job.transfer),
                x,
                y,
            ),
            dither_unit(
                color::encode_display(graded[2], job.target_peak_nits, job.transfer),
                x,
                y,
            ),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::UnitBias;

    fn near(value: f32, expected: f32) {
        assert!(
            (value - expected).abs() <= 1.0 / 255.0,
            "{value} vs {expected}"
        );
    }

    fn sdr_job(encoding: Encoding, equalizer: Equalizer) -> PresentJob {
        PresentJob {
            source_peak_nits: 100.0,
            target_peak_nits: 100.0,
            transfer: Transfer::Bt1886,
            matrix: Coefficients::Bt709,
            equalizer,
            encoding,
        }
    }

    #[test]
    fn neutral_equalizer_leaves_a_ramp_unchanged() {
        let ramp = [0.0, 0.25, 0.5, 0.75, 1.0];
        for value in ramp {
            let out = grade_rgb([value, value, value], Equalizer::default(), Coefficients::Bt709);
            near(out[0], value);
            near(out[1], value);
            near(out[2], value);
        }
    }

    #[test]
    fn contrast_minus_100_on_a_linear_ramp_is_mid_gray() {
        let equalizer = Equalizer {
            contrast: UnitBias::new(-100),
            ..Equalizer::default()
        };
        let job = sdr_job(Encoding::Linear, equalizer);
        for value in [0.0, 0.2, 0.5, 0.8, 1.0] {
            let out = present_texel([value, value * 0.5, 1.0 - value], 3, 5, &job);
            near(out[0], 0.5);
            near(out[1], 0.5);
            near(out[2], 0.5);
        }
    }

    #[test]
    fn spline_keeps_black_and_maps_the_source_peak() {
        assert_eq!(spline_tone_map(0.0, 1000.0, 100.0), 0.0);
        assert_eq!(spline_tone_map(0.4, 100.0, 100.0), 0.4);
        assert_eq!(spline_tone_map(1.5, 100.0, 200.0), 1.5);
        let mapped = spline_tone_map(10.0, 1000.0, 100.0);
        near(mapped, 1.0);
    }

    #[test]
    fn linear_encoding_skips_the_knee() {
        let mut job = sdr_job(Encoding::Linear, Equalizer::default());
        job.source_peak_nits = 10_000.0;
        job.target_peak_nits = 100.0;
        let out = present_texel([40.0, 40.0, 40.0], 0, 0, &job);
        near(out[0], 40.0);
    }

    #[test]
    fn gamma8_contrast_minus_100_encodes_after_the_grade() {
        let equalizer = Equalizer {
            contrast: UnitBias::new(-100),
            ..Equalizer::default()
        };
        let job = sdr_job(Encoding::Gamma8, equalizer);
        let out = present_texel([0.2, 0.9, 0.4], 1, 2, &job);
        let encoded = color::encode_display(0.5, job.target_peak_nits, job.transfer);
        let expect = dither_unit(encoded, 1, 2);
        near(out[0], expect);
        near(out[1], expect);
        near(out[2], expect);
        assert!(
            (out[0] - 0.5).abs() > 0.1,
            "Gamma8 stored {out:?} instead of the encoded mid-gray {expect}"
        );
    }
}
