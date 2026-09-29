//! Separable 4-tap cubic. Catmull-Rom (B = 0, C = 0.5) when an axis grows,
//! Hermite (B = 0, C = 0) when it shrinks. Weights are the Keys closed form.

/// Which cubic reconstructs one axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CubicKind {
    /// Sharp cubic, used when the destination is longer than the source.
    CatmullRom,
    /// Positive-weight cubic, used when the destination is shorter or equal.
    Hermite,
}

/// `dst_len > src_len` selects Catmull-Rom. Every other ratio selects Hermite.
pub fn kind_for_axis(src_len: u32, dst_len: u32) -> CubicKind {
    if dst_len > src_len {
        CubicKind::CatmullRom
    } else {
        CubicKind::Hermite
    }
}

/// Keys cubic at `distance` from the sample. Zero outside the radius of 2.
pub fn cubic_weight(distance: f32, kind: CubicKind) -> f32 {
    let ax = distance.abs();
    match kind {
        CubicKind::CatmullRom => {
            if ax < 1.0 {
                (1.5 * ax - 2.5) * ax * ax + 1.0
            } else if ax < 2.0 {
                ((-0.5 * ax + 2.5) * ax - 4.0) * ax + 2.0
            } else {
                0.0
            }
        }
        CubicKind::Hermite => {
            if ax < 1.0 {
                (2.0 * ax - 3.0) * ax * ax + 1.0
            } else {
                0.0
            }
        }
    }
}

/// First tap index and the four weights for a continuous source position.
///
/// Position `0` is the center of texel 0. The taps are `floor(pos)-1` through
/// `floor(pos)+2`.
pub fn weights_at(pos: f32, kind: CubicKind) -> (i32, [f32; 4]) {
    let base = pos.floor();
    let phase = pos - base;
    let weights = [
        cubic_weight(phase + 1.0, kind),
        cubic_weight(phase, kind),
        cubic_weight(1.0 - phase, kind),
        cubic_weight(2.0 - phase, kind),
    ];
    (base as i32 - 1, weights)
}

/// Source position of a destination pixel center. `1:1` maps index `i` to `i`.
pub fn source_pos(dest_index: f32, src_len: u32, dst_len: u32) -> f32 {
    if dst_len == 0 {
        return 0.0;
    }
    (dest_index + 0.5) * (src_len as f32) / (dst_len as f32) - 0.5
}

/// Clockwise quarter turn of the source inside the destination rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuarterTurn {
    /// Source x follows destination x.
    D0,
    /// Source rows become destination columns, clockwise.
    D90,
}

/// Continuous source coordinate for one destination pixel, before the cubic taps.
pub fn source_xy(
    local_x: u32,
    local_y: u32,
    src_w: u32,
    src_h: u32,
    dst_w: u32,
    dst_h: u32,
    turn: QuarterTurn,
) -> (f32, f32) {
    match turn {
        QuarterTurn::D0 => (
            source_pos(local_x as f32, src_w, dst_w),
            source_pos(local_y as f32, src_h, dst_h),
        ),
        QuarterTurn::D90 => {
            let rx = source_pos(local_x as f32, src_h, dst_w);
            let ry = source_pos(local_y as f32, src_w, dst_h);
            (ry, (src_h as f32 - 1.0) - rx)
        }
    }
}

/// Cubic kind for the source x axis and the source y axis.
pub fn axis_kinds(
    src_w: u32,
    src_h: u32,
    dst_w: u32,
    dst_h: u32,
    turn: QuarterTurn,
) -> (CubicKind, CubicKind) {
    match turn {
        QuarterTurn::D0 => (kind_for_axis(src_w, dst_w), kind_for_axis(src_h, dst_h)),
        QuarterTurn::D90 => (kind_for_axis(src_w, dst_h), kind_for_axis(src_h, dst_w)),
    }
}

fn clamp_index(index: i32, len: u32) -> u32 {
    if len == 0 {
        return 0;
    }
    index.clamp(0, len as i32 - 1) as u32
}

/// Separable 4-tap sample of an RGB image stored row-major.
pub fn sample_image(
    texels: &[[f32; 3]],
    width: u32,
    height: u32,
    sx: f32,
    sy: f32,
    kind_x: CubicKind,
    kind_y: CubicKind,
) -> [f32; 3] {
    if width == 0 || height == 0 || texels.is_empty() {
        return [0.0; 3];
    }
    let (x0, wx) = weights_at(sx, kind_x);
    let (y0, wy) = weights_at(sy, kind_y);
    let mut acc = [0.0; 3];
    for row in 0..4 {
        let y = clamp_index(y0 + row, height);
        for col in 0..4 {
            let x = clamp_index(x0 + col, width);
            let texel = texels[(y * width + x) as usize];
            let weight = wx[col as usize] * wy[row as usize];
            acc[0] += texel[0] * weight;
            acc[1] += texel[1] * weight;
            acc[2] += texel[2] * weight;
        }
    }
    acc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phases_use_the_closed_form() {
        for kind in [CubicKind::CatmullRom, CubicKind::Hermite] {
            for phase in [0.0_f32, 0.25, 0.5, 0.75] {
                let (first, weights) = weights_at(phase, kind);
                assert_eq!(first, -1);
                let distances = [phase + 1.0, phase, 1.0 - phase, 2.0 - phase];
                for (weight, distance) in weights.into_iter().zip(distances) {
                    let expect = cubic_weight(distance, kind);
                    assert!(
                        (weight - expect).abs() <= 1.0e-6,
                        "{weight} vs {expect} at {phase:?} {kind:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn integer_phase_is_the_center_texel() {
        for kind in [CubicKind::CatmullRom, CubicKind::Hermite] {
            let (_first, weights) = weights_at(3.0, kind);
            assert!((weights[0]).abs() <= 1.0e-6);
            assert!((weights[1] - 1.0).abs() <= 1.0e-6);
            assert!((weights[2]).abs() <= 1.0e-6);
            assert!((weights[3]).abs() <= 1.0e-6);
        }
    }

    #[test]
    fn growing_axis_is_catmull_and_shrinking_axis_is_hermite() {
        assert_eq!(kind_for_axis(2, 4), CubicKind::CatmullRom);
        assert_eq!(kind_for_axis(4, 2), CubicKind::Hermite);
        assert_eq!(kind_for_axis(4, 4), CubicKind::Hermite);
    }

    #[test]
    fn clockwise_quarter_turn_of_a_two_by_two() {
        // Source rows `a b / c d` land on `c a / d b`.
        let samples = [
            (0, 0, 0.0, 1.0),
            (1, 0, 0.0, 0.0),
            (0, 1, 1.0, 1.0),
            (1, 1, 1.0, 0.0),
        ];
        for (x, y, sx, sy) in samples {
            let (got_x, got_y) = source_xy(x, y, 2, 2, 2, 2, QuarterTurn::D90);
            assert!((got_x - sx).abs() <= 1.0e-5 && (got_y - sy).abs() <= 1.0e-5);
            let (kind_x, kind_y) = axis_kinds(2, 2, 2, 2, QuarterTurn::D90);
            assert_eq!(kind_x, CubicKind::Hermite);
            assert_eq!(kind_y, CubicKind::Hermite);
        }
    }
}
