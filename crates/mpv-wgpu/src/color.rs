//! YUV to RGB matrices. The live picture is already RGB; hue baking and tests use these.

/// Which ITU matrix the coefficients come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Coefficients {
    /// ITU-R BT.601.
    Bt601,
    /// ITU-R BT.709.
    Bt709,
    /// ITU-R BT.2020.
    Bt2020,
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
            Coefficients::Bt2020 => [0.2627, 0.6780, 0.0593],
        }
    }

    /// R-from-V, G-from-U, G-from-V, B-from-U.
    fn chroma(self) -> (f32, f32, f32, f32) {
        match self {
            Coefficients::Bt601 => (1.4020, -0.3441, -0.7141, 1.7720),
            Coefficients::Bt709 => (1.5748, -0.1873, -0.4681, 1.8556),
            // 2*(1-Kr), -Kb/Kg*2*(1-Kb), -Kr/Kg*2*(1-Kr), 2*(1-Kb).
            Coefficients::Bt2020 => (1.4746, -0.1646, -0.5714, 1.8814),
        }
    }
}

/// Where the chroma sample sits inside its luma block, in luma pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChromaSiting {
    /// Sample on the top-left luma.
    TopLeft,
    /// Horizontally on the left luma, vertically centered.
    Left,
    /// Center of the 2×2 block.
    Center,
}

/// Offset of the chroma sample from the top-left luma of its block.
pub fn chroma_siting_offset(siting: ChromaSiting) -> [f32; 2] {
    match siting {
        ChromaSiting::TopLeft => [0.0, 0.0],
        ChromaSiting::Left => [0.0, 0.5],
        ChromaSiting::Center => [0.5, 0.5],
    }
}

/// Continuous chroma-texel index for one luma pixel. Integer `n` is the center of texel `n`.
pub fn chroma_coord(luma_index: f32, luma_len: u32, chroma_len: u32, offset_luma: f32) -> f32 {
    if luma_len == 0 || chroma_len == 0 {
        return 0.0;
    }
    (luma_index - offset_luma) * (chroma_len as f32) / (luma_len as f32)
}

/// Electro-optical transfer stored in the picture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transfer {
    /// ITU-R BT.1886, a 2.4 power. `1` is SDR white (100 nits).
    Bt1886,
    /// Pure 2.2 power, the gpu-next reading of sRGB. `1` is SDR white.
    Srgb,
    /// ST 2084 PQ. Linear results are nits / 100, so signal `1` is 10000 nits.
    Pq,
    /// ITU-R BT.2100 HLG. `peak_nits` is the display peak used by the OOTF.
    Hlg,
}

const PQ_M1: f32 = 2610.0 / 16384.0;
const PQ_M2: f32 = 2523.0 / 32.0;
const PQ_C1: f32 = 3424.0 / 4096.0;
const PQ_C2: f32 = 2413.0 / 128.0;
const PQ_C3: f32 = 2392.0 / 128.0;

const HLG_A: f32 = 0.178_832_77;
const HLG_B: f32 = 0.284_668_92;
const HLG_C: f32 = 0.559_910_7;

/// ST 2084 forward OETF. Nits map into a 0..1 signal.
pub fn pq_nits_to_signal(nits: f32) -> f32 {
    let y = (nits / 10_000.0).clamp(0.0, 1.0);
    let y_m = y.powf(PQ_M1);
    let num = PQ_C1 + PQ_C2 * y_m;
    let den = 1.0 + PQ_C3 * y_m;
    (num / den).powf(PQ_M2)
}

/// ST 2084 inverse EOTF. Signal `0` is 0 nits and signal `1` is 10000 nits.
pub fn pq_nits(signal: f32) -> f32 {
    let e = signal.clamp(0.0, 1.0);
    let p = e.powf(1.0 / PQ_M2);
    let denom = (PQ_C2 - PQ_C3 * p).max(1.0e-6);
    let n = ((p - PQ_C1).max(0.0) / denom).powf(1.0 / PQ_M1);
    10_000.0 * n
}

/// Inverse HLG OETF, scene light relative to the nominal peak.
pub fn hlg_scene(signal: f32) -> f32 {
    let e = signal.clamp(0.0, 1.0);
    if e <= 0.5 {
        (e * e) / 3.0
    } else {
        (((e - HLG_C) / HLG_A).exp() + HLG_B) / 12.0
    }
}

fn hlg_gamma(peak_nits: f32) -> f32 {
    let peak = if peak_nits > 0.0 { peak_nits } else { 1000.0 };
    1.2 + 0.42 * (peak / 1000.0).log10()
}

/// HLG display light in nits. Signal `0` is 0. Signal `0.75` at a 1000-nit peak
/// is the BT.2408 reference white, about 203 nits.
pub fn hlg_nits(signal: f32, peak_nits: f32) -> f32 {
    let peak = if peak_nits > 0.0 { peak_nits } else { 1000.0 };
    let scene = hlg_scene(signal).max(0.0);
    peak * scene.powf(hlg_gamma(peak))
}

/// Linear light where `1.0` means 100 nits.
///
/// BT.1886 and the 2.2 power map signal `1` to `1`. PQ and HLG return nits / 100,
/// so a PQ peak is `100` and an HLG reference white is above `1`.
pub fn eotf(transfer: Transfer, signal: f32, peak_nits: f32) -> f32 {
    let s = signal.clamp(0.0, 1.0);
    match transfer {
        Transfer::Bt1886 => s.powf(2.4),
        Transfer::Srgb => s.powf(2.2),
        Transfer::Pq => pq_nits(s) / 100.0,
        Transfer::Hlg => hlg_nits(s, peak_nits) / 100.0,
    }
}

/// Inverse of the SDR power, after scaling `linear` so `target_peak_nits` is signal `1`.
/// PQ and HLG pictures encode into a BT.1886 display. sRGB stays a 2.2 power.
pub fn encode_display(linear: f32, target_peak_nits: f32, transfer: Transfer) -> f32 {
    let peak = if target_peak_nits > 0.0 {
        target_peak_nits
    } else {
        100.0
    };
    let relative = (linear * 100.0 / peak).clamp(0.0, 1.0);
    let gamma = match transfer {
        Transfer::Srgb => 2.2,
        Transfer::Bt1886 | Transfer::Pq | Transfer::Hlg => 2.4,
    };
    relative.powf(1.0 / gamma)
}

/// Studio swing versus full swing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

/// Stored code as a 0..1 signal. The 8-bit peak is 255 and the 16-bit peak is 65535.
pub fn normalize_code(code: u32, bits: u32) -> f32 {
    let peak = if bits >= 16 { 65535.0 } else { 255.0 };
    code as f32 / peak
}

/// Bilinear sample. Integer `n` is the center of texel `n`. Out-of-range taps clamp.
pub fn sample_bilinear(texels: &[f32], width: u32, height: u32, x: f32, y: f32) -> f32 {
    if width == 0 || height == 0 || texels.is_empty() {
        return 0.0;
    }
    let x0 = x.floor() as i32;
    let y0 = y.floor() as i32;
    let fx = x - x0 as f32;
    let fy = y - y0 as f32;
    let at = |ix: i32, iy: i32| -> f32 {
        let ix = ix.clamp(0, width as i32 - 1) as u32;
        let iy = iy.clamp(0, height as i32 - 1) as u32;
        texels[(iy * width + ix) as usize]
    };
    let a = at(x0, y0);
    let b = at(x0 + 1, y0);
    let c = at(x0, y0 + 1);
    let d = at(x0 + 1, y0 + 1);
    a * (1.0 - fx) * (1.0 - fy) + b * fx * (1.0 - fy) + c * (1.0 - fx) * fy + d * fx * fy
}

/// Display-referred YUV sample, then the transfer, as linear light (1 = 100 nits).
pub fn decode_linear(
    space: Coefficients,
    levels: Levels,
    transfer: Transfer,
    peak_nits: f32,
    y: f32,
    cb: f32,
    cr: f32,
) -> [f32; 3] {
    let display = to_rgb(space, levels, y, cb, cr);
    [
        eotf(transfer, display[0], peak_nits),
        eotf(transfer, display[1], peak_nits),
        eotf(transfer, display[2], peak_nits),
    ]
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
        for space in [
            Coefficients::Bt601,
            Coefficients::Bt709,
            Coefficients::Bt2020,
        ] {
            let forward = yuv_to_rgb(space);
            let back = rgb_to_yuv(space);
            let rgb = mul_vec(forward, [0.4, 0.1, -0.2]);
            let yuv = mul_vec(back, rgb);
            near(yuv[0], 0.4);
            near(yuv[1], 0.1);
            near(yuv[2], -0.2);
        }
    }

    #[test]
    fn full_range_peak_codes_reach_one() {
        for bits in [8, 16] {
            let peak = if bits == 16 { 65535 } else { 255 };
            let black = to_rgb(
                Coefficients::Bt709,
                Levels::Full,
                normalize_code(0, bits),
                0.5,
                0.5,
            );
            let white = to_rgb(
                Coefficients::Bt709,
                Levels::Full,
                normalize_code(peak, bits),
                0.5,
                0.5,
            );
            for channel in black {
                near(channel, 0.0);
            }
            for channel in white {
                near(channel, 1.0);
            }
        }
    }

    #[test]
    fn bt2020_limited_neutral_matches_luma() {
        let black = to_rgb(
            Coefficients::Bt2020,
            Levels::Limited,
            16.0 / 255.0,
            128.0 / 255.0,
            128.0 / 255.0,
        );
        let white = to_rgb(
            Coefficients::Bt2020,
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
    fn bilinear_sample_blends_the_four_texels() {
        let texels = [0.0, 1.0, 0.0, 0.0];
        assert!((sample_bilinear(&texels, 2, 2, 0.0, 0.0) - 0.0).abs() <= 1.0e-6);
        assert!((sample_bilinear(&texels, 2, 2, 1.0, 0.0) - 1.0).abs() <= 1.0e-6);
        assert!((sample_bilinear(&texels, 2, 2, 0.5, 0.0) - 0.5).abs() <= 1.0e-6);
    }

    #[test]
    fn chroma_siting_changes_the_sample_index() {
        assert_eq!(chroma_siting_offset(ChromaSiting::TopLeft), [0.0, 0.0]);
        assert_eq!(chroma_siting_offset(ChromaSiting::Left), [0.0, 0.5]);
        assert_eq!(chroma_siting_offset(ChromaSiting::Center), [0.5, 0.5]);
        let top_left = chroma_coord(1.0, 4, 2, chroma_siting_offset(ChromaSiting::TopLeft)[0]);
        let center = chroma_coord(1.0, 4, 2, chroma_siting_offset(ChromaSiting::Center)[0]);
        assert!((top_left - center).abs() > 0.1);
    }

    #[test]
    fn transfers_hit_their_documented_endpoints() {
        near(eotf(Transfer::Bt1886, 0.0, 100.0), 0.0);
        near(eotf(Transfer::Bt1886, 1.0, 100.0), 1.0);
        near(eotf(Transfer::Bt1886, 0.5, 100.0), 0.5_f32.powf(2.4));
        near(eotf(Transfer::Srgb, 0.0, 100.0), 0.0);
        near(eotf(Transfer::Srgb, 1.0, 100.0), 1.0);
        near(eotf(Transfer::Srgb, 0.5, 100.0), 0.5_f32.powf(2.2));
        assert!(pq_nits(0.0).abs() < 1.0e-3);
        assert!((pq_nits(1.0) - 10_000.0).abs() < 1.0);
        assert!(hlg_nits(0.0, 1000.0).abs() < 1.0e-3);
        let reference_white = hlg_nits(0.75, 1000.0);
        assert!(
            (reference_white - 203.0).abs() < 1.0,
            "BT.2408 reference white at signal 0.75, got {reference_white}"
        );
    }
}
