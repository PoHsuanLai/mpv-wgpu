//! Bake mpv's equalizer into one affine RGB transform plus a gamma exponent.

use crate::color::{self, Coefficients};
use crate::types::Equalizer;

/// GPU uniform payload, column-major, before WGSL padding.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Grade {
    /// Column-major 3×3.
    pub matrix: [f32; 9],
    /// Added after the matrix.
    pub bias: [f32; 3],
    /// `pow` exponent. `1.0` leaves the sample unchanged.
    pub gamma_exp: f32,
}

impl Grade {
    #[cfg_attr(not(test), allow(dead_code))]
    pub const IDENTITY: Self = Self {
        matrix: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
        bias: [0.0, 0.0, 0.0],
        gamma_exp: 1.0,
    };
}

/// Contrast, brightness, saturation, and hue become `matrix` and `bias`.
/// Gamma stays in [`Grade::gamma_exp`].
pub fn bake(eq: Equalizer, space: Coefficients) -> Grade {
    let contrast = (eq.contrast.get() as f32 + 100.0) / 100.0;
    let brightness = eq.brightness.get() as f32 / 100.0;
    let saturation = (eq.saturation.get() as f32 + 100.0) / 100.0;
    let hue = (eq.hue.get() as f32).to_radians();

    let contrast_m = scale(contrast);
    let bias0 = 0.5 + brightness - 0.5 * contrast;
    let sat_m = saturation_matrix(saturation, color::luma_weights(space));
    let hue_m = hue_matrix(hue, space);

    let hue_sat = mul(hue_m, sat_m);
    let linear = mul(hue_sat, contrast_m);
    let bias = color::mul_vec(hue_sat, [bias0, bias0, bias0]);
    let gamma_exp = 1.0 / 2.0_f32.powf(eq.gamma.get() as f32 / 100.0);
    Grade {
        matrix: linear,
        bias,
        gamma_exp,
    }
}

fn scale(k: f32) -> [f32; 9] {
    [k, 0.0, 0.0, 0.0, k, 0.0, 0.0, 0.0, k]
}

fn saturation_matrix(saturation: f32, luma: [f32; 3]) -> [f32; 9] {
    let mut m = [0.0; 9];
    let keep = 1.0 - saturation;
    for col in 0..3 {
        for row in 0..3 {
            let diag = if row == col { saturation } else { 0.0 };
            m[col * 3 + row] = diag + keep * luma[row] * luma[col];
        }
    }
    m
}

fn hue_matrix(radians: f32, space: Coefficients) -> [f32; 9] {
    let cos = radians.cos();
    let sin = radians.sin();
    // Rotate centered U,V. Column-major.
    let rotate = [1.0, 0.0, 0.0, 0.0, cos, sin, 0.0, -sin, cos];
    let to_rgb = color::yuv_to_rgb(space);
    let to_yuv = color::rgb_to_yuv(space);
    mul(to_rgb, mul(rotate, to_yuv))
}

fn mul(a: [f32; 9], b: [f32; 9]) -> [f32; 9] {
    let mut c = [0.0; 9];
    for col in 0..3 {
        for row in 0..3 {
            c[col * 3 + row] =
                a[row] * b[col * 3] + a[3 + row] * b[col * 3 + 1] + a[6 + row] * b[col * 3 + 2];
        }
    }
    c
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn near(value: f32, expected: f32) {
        assert!((value - expected).abs() <= 1e-5, "{value} vs {expected}");
    }

    #[test]
    fn zero_equalizer_is_identity() {
        let grade = bake(Equalizer::default(), Coefficients::Bt709);
        let expected = Grade::IDENTITY;
        for (got, want) in grade.matrix.into_iter().zip(expected.matrix) {
            near(got, want);
        }
        for (got, want) in grade.bias.into_iter().zip(expected.bias) {
            near(got, want);
        }
        near(grade.gamma_exp, 1.0);

        let grade601 = bake(Equalizer::default(), Coefficients::Bt601);
        for (got, want) in grade601.matrix.into_iter().zip(expected.matrix) {
            near(got, want);
        }
        near(grade601.gamma_exp, 1.0);
    }

    #[test]
    fn contrast_minus_100_is_mid_gray() {
        let eq = Equalizer {
            contrast: crate::types::UnitBias::new(-100),
            ..Equalizer::default()
        };
        let grade = bake(eq, Coefficients::Bt709);
        let sample = crate::color::mul_vec(grade.matrix, [0.2, 0.8, 0.1]);
        for (channel, bias) in sample.iter().zip(grade.bias) {
            near(channel + bias, 0.5);
        }
    }
}
