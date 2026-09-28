//! YUV to RGB matrices. The live picture is already RGB; hue baking and tests use these.

/// Which ITU matrix the coefficients come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Coefficients {
    /// ITU-R BT.601.
    Bt601,
    /// ITU-R BT.709.
    Bt709,
}

impl Coefficients {
    pub fn parse(text: &str) -> Self {
        match text {
            "bt.601" => Coefficients::Bt601,
            "bt.709" => Coefficients::Bt709,
            _ => Coefficients::Bt709,
        }
    }

    fn luma(self) -> [f32; 3] {
        match self {
            Coefficients::Bt601 => [0.299, 0.587, 0.114],
            Coefficients::Bt709 => [0.2126, 0.7152, 0.0722],
        }
    }

    /// R-from-V, G-from-U, G-from-V, B-from-U.
    fn chroma(self) -> (f32, f32, f32, f32) {
        match self {
            Coefficients::Bt601 => (1.4020, -0.3441, -0.7141, 1.7720),
            Coefficients::Bt709 => (1.5748, -0.1873, -0.4681, 1.8556),
        }
    }
}

/// Studio swing versus full swing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))]
pub enum Levels {
    /// Y 16..235, chroma 16..240.
    Limited,
    /// 0..255.
    Full,
}

/// Column-major matrix mapping expanded, centered `(Y, U, V)` to RGB.
///
/// `U` and `V` are zero at neutral chroma. Limited-range expansion is applied
/// by [`to_rgb`] before this matrix.
pub fn yuv_to_rgb(space: Coefficients) -> [f32; 9] {
    let (rv, gu, gv, bu) = space.chroma();
    // columns: Y, U, V
    [1.0, 1.0, 1.0, 0.0, gu, bu, rv, gv, 0.0]
}

/// Inverse of [`yuv_to_rgb`], mapping RGB to centered `(Y, U, V)`.
pub fn rgb_to_yuv(space: Coefficients) -> [f32; 9] {
    invert(yuv_to_rgb(space)).unwrap_or([1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0])
}

/// Luma weights for `space`, used by saturation.
pub fn luma_weights(space: Coefficients) -> [f32; 3] {
    space.luma()
}

/// Convert one 0..1 sample. `cb` and `cr` are the raw stored chroma, not centered.
#[cfg_attr(not(test), allow(dead_code))]
pub fn to_rgb(space: Coefficients, levels: Levels, y: f32, cb: f32, cr: f32) -> [f32; 3] {
    let (yy, uu, vv) = expand(levels, y, cb, cr);
    mul_vec(yuv_to_rgb(space), [yy, uu, vv])
}

fn expand(levels: Levels, y: f32, cb: f32, cr: f32) -> (f32, f32, f32) {
    match levels {
        Levels::Full => (y, cb - 0.5, cr - 0.5),
        Levels::Limited => {
            let yy = (y - 16.0 / 255.0) * (255.0 / 219.0);
            let uu = (cb - 128.0 / 255.0) * (255.0 / 224.0);
            let vv = (cr - 128.0 / 255.0) * (255.0 / 224.0);
            (yy, uu, vv)
        }
    }
}

pub(crate) fn mul_vec(m: [f32; 9], v: [f32; 3]) -> [f32; 3] {
    [
        m[0] * v[0] + m[3] * v[1] + m[6] * v[2],
        m[1] * v[0] + m[4] * v[1] + m[7] * v[2],
        m[2] * v[0] + m[5] * v[1] + m[8] * v[2],
    ]
}

pub(crate) fn invert(m: [f32; 9]) -> Option<[f32; 9]> {
    let a = m[0];
    let d = m[1];
    let g = m[2];
    let b = m[3];
    let e = m[4];
    let h = m[5];
    let c = m[6];
    let f = m[7];
    let i = m[8];
    let det = a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g);
    if det.abs() < 1e-8 {
        return None;
    }
    let inv = 1.0 / det;
    Some([
        (e * i - f * h) * inv,
        (f * g - d * i) * inv,
        (d * h - e * g) * inv,
        (c * h - b * i) * inv,
        (a * i - c * g) * inv,
        (b * g - a * h) * inv,
        (b * f - c * e) * inv,
        (c * d - a * f) * inv,
        (a * e - b * d) * inv,
    ])
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn near(value: f32, expected: f32) {
        assert!(
            (value - expected).abs() <= 1.0 / 255.0,
            "{value} vs {expected}"
        );
    }

    #[test]
    fn limited_bt709_endpoints() {
        let black = to_rgb(
            Coefficients::Bt709,
            Levels::Limited,
            16.0 / 255.0,
            128.0 / 255.0,
            128.0 / 255.0,
        );
        let white = to_rgb(
            Coefficients::Bt709,
            Levels::Limited,
            235.0 / 255.0,
            128.0 / 255.0,
            128.0 / 255.0,
        );
        for channel in black {
            near(channel, 0.0);
        }
        for channel in white {
            near(channel, 1.0);
        }
    }

    #[test]
    fn limited_bt601_neutral_tracks_luma() {
        let black = to_rgb(
            Coefficients::Bt601,
            Levels::Limited,
            16.0 / 255.0,
            128.0 / 255.0,
            128.0 / 255.0,
        );
        let white = to_rgb(
            Coefficients::Bt601,
            Levels::Limited,
            235.0 / 255.0,
            128.0 / 255.0,
            128.0 / 255.0,
        );
        for channel in black {
            near(channel, 0.0);
        }
        for channel in white {
            near(channel, 1.0);
        }
    }

    #[test]
    fn full_range_endpoints_for_both_matrices() {
        for space in [Coefficients::Bt601, Coefficients::Bt709] {
            let black = to_rgb(space, Levels::Full, 0.0, 0.5, 0.5);
            let white = to_rgb(space, Levels::Full, 1.0, 0.5, 0.5);
            for channel in black {
                near(channel, 0.0);
            }
            for channel in white {
                near(channel, 1.0);
            }
        }
    }

    #[test]
    fn matrices_round_trip_a_neutral_pixel() {
        for space in [Coefficients::Bt601, Coefficients::Bt709] {
            let forward = yuv_to_rgb(space);
            let back = rgb_to_yuv(space);
            let rgb = mul_vec(forward, [0.4, 0.1, -0.2]);
            let yuv = mul_vec(back, rgb);
            near(yuv[0], 0.4);
            near(yuv[1], 0.1);
            near(yuv[2], -0.2);
        }
    }
}
