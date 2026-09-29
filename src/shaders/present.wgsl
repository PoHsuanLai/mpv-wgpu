struct PresentParams {
    dest_origin: vec2<f32>,
    dest_size: vec2<f32>,
    src_size: vec2<f32>,
    kind: vec2<u32>,
    rotation: u32,
    encoding: u32,
    transfer: u32,
    pad0: u32,
    peaks: vec2<f32>,
    pad1: vec2<f32>,
    grade: mat3x3<f32>,
    bias: vec3<f32>,
    gamma_exp: f32,
}

@group(0) @binding(0) var t_linear: texture_2d<f32>;
@group(0) @binding(1) var<uniform> params: PresentParams;

struct VertexOut {
    @builtin(position) position: vec4<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> VertexOut {
    let uv = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    var out: VertexOut;
    out.position = vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.0, 1.0);
    return out;
}

const PQ_M1: f32 = 2610.0 / 16384.0;
const PQ_M2: f32 = 2523.0 / 32.0;
const PQ_C1: f32 = 3424.0 / 4096.0;
const PQ_C2: f32 = 2413.0 / 128.0;
const PQ_C3: f32 = 2392.0 / 128.0;

const BAYER: array<f32, 64> = array<f32, 64>(
    0.0, 32.0, 8.0, 40.0, 2.0, 34.0, 10.0, 42.0,
    48.0, 16.0, 56.0, 24.0, 50.0, 18.0, 58.0, 26.0,
    12.0, 44.0, 4.0, 36.0, 14.0, 46.0, 6.0, 38.0,
    60.0, 28.0, 52.0, 20.0, 62.0, 30.0, 54.0, 22.0,
    3.0, 35.0, 11.0, 43.0, 1.0, 33.0, 9.0, 41.0,
    51.0, 19.0, 59.0, 27.0, 49.0, 17.0, 57.0, 25.0,
    15.0, 47.0, 7.0, 39.0, 13.0, 45.0, 5.0, 37.0,
    63.0, 31.0, 55.0, 23.0, 61.0, 29.0, 53.0, 21.0,
);

fn pq_signal(nits: f32) -> f32 {
    let y = clamp(nits / 10000.0, 0.0, 1.0);
    let y_m = pow(y, PQ_M1);
    let num = PQ_C1 + PQ_C2 * y_m;
    let den = 1.0 + PQ_C3 * y_m;
    return pow(num / den, PQ_M2);
}

fn pq_nits(signal: f32) -> f32 {
    let e = clamp(signal, 0.0, 1.0);
    let p = pow(e, 1.0 / PQ_M2);
    let denom = max(PQ_C2 - PQ_C3 * p, 1.0e-6);
    let n = pow(max(p - PQ_C1, 0.0) / denom, 1.0 / PQ_M1);
    return 10000.0 * n;
}

fn spline(linear: f32) -> f32 {
    let source_peak = params.peaks.x;
    let target_peak = params.peaks.y;
    if linear <= 0.0 {
        return 0.0;
    }
    if !(source_peak > target_peak) {
        return linear;
    }
    let x = pq_signal(linear * 100.0);
    let x_s = max(pq_signal(source_peak), 1.0e-6);
    let y_s = pq_signal(target_peak);
    let t = clamp(x / x_s, 0.0, 1.0);
    let t2 = t * t;
    let t3 = t2 * t;
    let h10 = t3 - 2.0 * t2 + t;
    let h01 = -2.0 * t3 + 3.0 * t2;
    let h11 = t3 - t2;
    let y = clamp(h10 * x_s + h01 * y_s + h11 * y_s, 0.0, 1.0);
    return pq_nits(y) / 100.0;
}

fn encode_display(linear: f32) -> f32 {
    var peak = params.peaks.y;
    if peak <= 0.0 {
        peak = 100.0;
    }
    let relative = clamp(linear * 100.0 / peak, 0.0, 1.0);
    var gamma = 2.4;
    if params.transfer == 1u {
        gamma = 2.2;
    }
    return pow(relative, 1.0 / gamma);
}

fn cubic(distance: f32, kind: u32) -> f32 {
    let ax = abs(distance);
    if kind == 1u {
        if ax < 1.0 {
            return (1.5 * ax - 2.5) * ax * ax + 1.0;
        }
        if ax < 2.0 {
            return ((-0.5 * ax + 2.5) * ax - 4.0) * ax + 2.0;
        }
        return 0.0;
    }
    if ax < 1.0 {
        return (2.0 * ax - 3.0) * ax * ax + 1.0;
    }
    return 0.0;
}

struct Taps {
    first: i32,
    w0: f32,
    w1: f32,
    w2: f32,
    w3: f32,
}

fn weights_at(pos: f32, kind: u32) -> Taps {
    let base = floor(pos);
    let phase = pos - base;
    var taps: Taps;
    taps.first = i32(base) - 1;
    taps.w0 = cubic(phase + 1.0, kind);
    taps.w1 = cubic(phase, kind);
    taps.w2 = cubic(1.0 - phase, kind);
    taps.w3 = cubic(2.0 - phase, kind);
    return taps;
}

fn tap_weight(taps: Taps, index: i32) -> f32 {
    if index == 0 { return taps.w0; }
    if index == 1 { return taps.w1; }
    if index == 2 { return taps.w2; }
    return taps.w3;
}

fn source_pos(dest_index: f32, src_len: f32, dst_len: f32) -> f32 {
    return (dest_index + 0.5) * src_len / dst_len - 0.5;
}

fn source_xy(local: vec2<f32>) -> vec2<f32> {
    if params.rotation == 0u {
        return vec2<f32>(
            source_pos(local.x, params.src_size.x, params.dest_size.x),
            source_pos(local.y, params.src_size.y, params.dest_size.y),
        );
    }
    let rx = source_pos(local.x, params.src_size.y, params.dest_size.x);
    let ry = source_pos(local.y, params.src_size.x, params.dest_size.y);
    return vec2<f32>(ry, (params.src_size.y - 1.0) - rx);
}

fn sample_linear(sx: f32, sy: f32) -> vec3<f32> {
    let wx = weights_at(sx, params.kind.x);
    let wy = weights_at(sy, params.kind.y);
    let max_x = i32(params.src_size.x) - 1;
    let max_y = i32(params.src_size.y) - 1;
    var acc = vec3<f32>(0.0);
    for (var row = 0; row < 4; row++) {
        let y = clamp(wy.first + row, 0, max_y);
        let wy_w = tap_weight(wy, row);
        for (var col = 0; col < 4; col++) {
            let x = clamp(wx.first + col, 0, max_x);
            let texel = textureLoad(t_linear, vec2<i32>(x, y), 0).rgb;
            acc += texel * (tap_weight(wx, col) * wy_w);
        }
    }
    return acc;
}

fn round_away(value: f32) -> f32 {
    if value >= 0.0 {
        return floor(value + 0.5);
    }
    return ceil(value - 0.5);
}

fn dither(display: f32, x: u32, y: u32) -> f32 {
    let threshold = BAYER[(y % 8u) * 8u + (x % 8u)] / 64.0;
    let code = round_away(clamp(display, 0.0, 1.0) * 255.0 + threshold - 0.5);
    return clamp(code, 0.0, 255.0) / 255.0;
}

@fragment
fn fs_main(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let pix = vec2<u32>(u32(position.x), u32(position.y));
    let local = vec2<f32>(position.xy) - params.dest_origin - vec2<f32>(0.5);
    let src = source_xy(local);
    var linear = sample_linear(src.x, src.y);
    if params.encoding == 0u {
        linear = vec3<f32>(spline(linear.r), spline(linear.g), spline(linear.b));
        linear = vec3<f32>(encode_display(linear.r), encode_display(linear.g), encode_display(linear.b));
    }
    let graded = pow(max(params.grade * linear + params.bias, vec3<f32>(0.0)), vec3<f32>(params.gamma_exp));
    if params.encoding == 0u {
        return vec4<f32>(dither(graded.r, pix.x, pix.y), dither(graded.g, pix.x, pix.y), dither(graded.b, pix.x, pix.y), 1.0);
    }
    return vec4<f32>(graded, 1.0);
}
