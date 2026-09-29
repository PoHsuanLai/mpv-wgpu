struct VideoParams {
    grade: mat3x3<f32>,
    bias: vec3<f32>,
    gamma_exp: f32,
}

@group(0) @binding(0) var t_frame: texture_2d<f32>;
@group(0) @binding(1) var s_frame: sampler;
@group(0) @binding(2) var<uniform> params: VideoParams;

struct VertexOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> VertexOut {
    // One triangle covering the slot. uv (0, 0) is the top-left of the texture.
    let uv = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    var out: VertexOut;
    out.position = vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.0, 1.0);
    out.uv = uv;
    return out;
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    let rgb = textureSample(t_frame, s_frame, in.uv).rgb;
    let graded = params.grade * rgb + params.bias;
    let color = pow(max(graded, vec3<f32>(0.0)), vec3<f32>(params.gamma_exp));
    return vec4<f32>(saturate(color), 1.0);
}
