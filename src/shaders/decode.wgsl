struct DecodeParams {
    yuv: mat3x3<f32>,
    range: vec4<f32>,
    luma_size: vec2<f32>,
    chroma_size: vec2<f32>,
    chroma_offset: vec2<f32>,
    transfer: u32,
    rgba_mode: u32,
    peak_nits: f32,
    pad: f32,
}

@group(0) @binding(0) var t_y: texture_2d<f32>;
@group(0) @binding(1) var t_u: texture_2d<f32>;
@group(0) @binding(2) var t_v: texture_2d<f32>;
@group(0) @binding(3) var<uniform> params: DecodeParams;

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
const HLG_A: f32 = 0.17883277;
const HLG_B: f32 = 0.28466892;
const HLG_C: f32 = 0.55991073;

fn pq_nits(signal: f32) -> f32 {
    let e = clamp(signal, 0.0, 1.0);
    let p = pow(e, 1.0 / PQ_M2);
    let denom = max(PQ_C2 - PQ_C3 * p, 1.0e-6);
    let n = pow(max(p - PQ_C1, 0.0) / denom, 1.0 / PQ_M1);
    return 10000.0 * n;
}

fn hlg_scene(signal: f32) -> f32 {
    let e = clamp(signal, 0.0, 1.0);
    if e <= 0.5 {
        return (e * e) / 3.0;
    }
    return (exp((e - HLG_C) / HLG_A) + HLG_B) / 12.0;
}

fn hlg_nits(signal: f32, peak_nits: f32) -> f32 {
    var peak = peak_nits;
    if peak <= 0.0 {
        peak = 1000.0;
    }
    let scene = max(hlg_scene(signal), 0.0);
    let gamma = 1.2 + 0.42 * log(peak / 1000.0) / log(10.0);
    return peak * pow(scene, gamma);
}

fn eotf(signal: f32) -> f32 {
    let s = clamp(signal, 0.0, 1.0);
    if params.transfer == 1u {
        return pow(s, 2.2);
    }
    if params.transfer == 2u {
        return pq_nits(s) / 100.0;
    }
    if params.transfer == 3u {
        return hlg_nits(s, params.peak_nits) / 100.0;
    }
    return pow(s, 2.4);
}

fn load_clamped(tex: texture_2d<f32>, ix: i32, iy: i32) -> f32 {
    let dims = textureDimensions(tex);
    let x = clamp(ix, 0, i32(dims.x) - 1);
    let y = clamp(iy, 0, i32(dims.y) - 1);
    return textureLoad(tex, vec2<i32>(x, y), 0).r;
}

fn bilinear(tex: texture_2d<f32>, c: vec2<f32>) -> f32 {
    let x0 = i32(floor(c.x));
    let y0 = i32(floor(c.y));
    let fx = c.x - floor(c.x);
    let fy = c.y - floor(c.y);
    let a = load_clamped(tex, x0, y0);
    let b = load_clamped(tex, x0 + 1, y0);
    let c0 = load_clamped(tex, x0, y0 + 1);
    let d = load_clamped(tex, x0 + 1, y0 + 1);
    return a * (1.0 - fx) * (1.0 - fy) + b * fx * (1.0 - fy) + c0 * (1.0 - fx) * fy + d * fx * fy;
}

fn chroma_coord(luma_index: f32, luma_len: f32, chroma_len: f32, offset: f32) -> f32 {
    return (luma_index - offset) * chroma_len / luma_len;
}

@fragment
fn fs_main(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let pix = vec2<i32>(i32(position.x), i32(position.y));
    if params.rgba_mode != 0u {
        let rgb = textureLoad(t_y, pix, 0).rgb;
        return vec4<f32>(eotf(rgb.r), eotf(rgb.g), eotf(rgb.b), 1.0);
    }
    let y = textureLoad(t_y, pix, 0).r;
    let luma = position.xy - vec2<f32>(0.5);
    let cu = chroma_coord(luma.x, params.luma_size.x, params.chroma_size.x, params.chroma_offset.x);
    let cv = chroma_coord(luma.y, params.luma_size.y, params.chroma_size.y, params.chroma_offset.y);
    let u = bilinear(t_u, vec2<f32>(cu, cv));
    let v = bilinear(t_v, vec2<f32>(cu, cv));
    let yy = (y - params.range.x) * params.range.y;
    let uu = (u - params.range.z) * params.range.w;
    let vv = (v - params.range.z) * params.range.w;
    let rgb = params.yuv * vec3<f32>(yy, uu, vv);
    return vec4<f32>(eotf(rgb.r), eotf(rgb.g), eotf(rgb.b), 1.0);
}
