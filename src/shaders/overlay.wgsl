@group(0) @binding(0) var t_overlay: texture_2d<f32>;

struct VertexOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> VertexOut {
    let uv = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    var out: VertexOut;
    out.position = vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.0, 1.0);
    out.uv = uv;
    return out;
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    let dims = vec2<i32>(textureDimensions(t_overlay));
    let x = clamp(i32(floor(in.uv.x * f32(dims.x))), 0, dims.x - 1);
    let y = clamp(i32(floor(in.uv.y * f32(dims.y))), 0, dims.y - 1);
    return textureLoad(t_overlay, vec2<i32>(x, y), 0);
}
