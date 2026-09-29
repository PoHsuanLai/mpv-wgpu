# mpv-wgpu

The picture chain of `vo=gpu` and `vo=gpu-next`, as static WGSL on a caller-owned wgpu device.

`vo=gpu` generates GLSL per frame and draws through an `ra` backend. `vo=gpu-next` hands the same job to libplacebo. This crate replaces that image path: csp, chroma location, EOTF, a cubic scale, one spline tone map, video-eq once in linear light, ordered dither, and premultiplied bitmap overlays. The caller supplies Y, U, V or RGBA, the way a decoder's `mp_image` would. There is no libplacebo, no `ra` backend, and no shader generator. The result is not bit-exact with libplacebo. The spline is a Hermite in PQ, not `tone-mapping=bt.2390`.

The host owns the device, the queue, and the swapchain. File playback, libass, and `vo=libmpv` are [`mpv-wgpu-player`](https://github.com/PoHsuanLai/mpv-wgpu/tree/master/crates/mpv-wgpu-player). That player does not run mpv's composited `rgb0` frame through this renderer.

```toml
mpv-wgpu = "0.1"
```

```sh
cargo doc --open -p mpv-wgpu
```

## Draw

`Renderer::new` compiles the static shaders. `Renderer::draw` uploads or binds the planes, decodes them to linear light at luma resolution, scales into `Draw::dest`, and writes `Draw::target`.

On `Encoding::Gamma8` the present order is:

1. Spline tone map, per channel, when the frame peak is above the target peak.
2. The five equalizer knobs, once, in linear light.
3. Inverse transfer into the display signal.
4. Ordered 8×8 Bayer dither. The coordinate is the framebuffer pixel.

`Encoding::Linear` skips the spline and the inverse transfer. The equalizer still runs. Contrast −100 therefore stays mid-gray (`0.5`) on a linear target. On `Gamma8` that mid-gray is encoded before dither: BT.1886 of `0.5` is about `0.749`.

```rust
use mpv_wgpu::{
    ChromaSiting, Coefficients, Draw, Encoding, Equalizer, Levels, Picture, PixelRect, Plane,
    PlaneBits, PlaneSource, QuarterTurn, Renderer, Transfer,
};

fn draw_limited(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    target: &wgpu::Texture,
) -> Result<(), mpv_wgpu::Error> {
    let mut renderer = Renderer::new(device)?;
    let y_bytes = [16u8, 16, 235, 235];
    let chroma = [128u8];
    let y = Plane {
        width: 2,
        height: 2,
        bits: PlaneBits::Eight,
        source: PlaneSource::Bytes(&y_bytes),
    };
    let u = Plane {
        width: 1,
        height: 1,
        bits: PlaneBits::Eight,
        source: PlaneSource::Bytes(&chroma),
    };
    let v = Plane {
        width: 1,
        height: 1,
        bits: PlaneBits::Eight,
        source: PlaneSource::Bytes(&chroma),
    };
    renderer.draw(
        device,
        queue,
        Draw {
            picture: Picture::Yuv { y, u, v },
            matrix: Coefficients::Bt709,
            levels: Levels::Limited,
            transfer: Transfer::Bt1886,
            siting: ChromaSiting::Center,
            peak_nits: 100.0,
            dest: PixelRect { x: 0, y: 0, width: 2, height: 2 },
            rotation: QuarterTurn::D0,
            equalizer: Equalizer::default(),
            overlays: &[],
            target,
            encoding: Encoding::Gamma8,
            target_peak_nits: 100.0,
        },
    )
}
```

Byte planes are tightly packed. The upload pads rows to wgpu's 256-byte alignment. `PlaneSource::Texture` is a view the caller already owns. Its `bits` field is ignored.

## Planes and color

`Picture::Yuv` is one luma plane plus U and V. U and V must be the same size, and each may be smaller than Y. `Picture::Rgba` is one 8-bit RGBA plane. RGB is the display-referred signal. Alpha in that plane is ignored. 16-bit RGBA returns `Error::InvalidSize`.

| Input | Values |
| --- | --- |
| `PlaneBits::Eight` | One byte per sample. Peak code 255. YUV is stored as `R8Unorm`. |
| `PlaneBits::Sixteen` | Little-endian `u16`. Peak code 65535. Stored as `R16Unorm`. |
| `Coefficients` | BT.601, BT.709, BT.2020. |
| `Levels::Limited` | Code 16 maps to 0 and code 235 maps to 1, as 8-bit fractions of the normalized signal, including for 16-bit planes. |
| `Levels::Full` | Code 0 maps to 0 and the peak code maps to 1. |
| `ChromaSiting` | `TopLeft` `[0, 0]`, `Left` `[0, 0.5]`, `Center` `[0.5, 0.5]`, in luma pixels. |

`Transfer::Bt1886` is a 2.4 power. `Transfer::Srgb` is a pure 2.2 power. `Transfer::Pq` maps signal 0 to 0 nits and signal 1 to 10000 nits. `Transfer::Hlg` maps signal 0 to 0. At a 1000-nit peak, signal 0.75 is the BT.2408 reference white, about 203 nits. Linear light uses 1.0 = 100 nits, so a PQ peak is the linear value 100.

16-bit planes need the device feature `TEXTURE_FORMAT_16BIT_NORM`. A device without it makes `draw` return `Error::Gpu`.

A neutral chroma sample is code 128 in 8-bit. In 16-bit, code 32768 is not exactly 0.5 and can tint green slightly. After the range expansion and clamp, a full-range peak luma is 1.

## Scale, rotation, overlays

Each axis is a separable 4-tap cubic. The destination pixel `i` samples source position `(i + 0.5) * src_len / dst_len - 0.5`. A 1:1 axis lands on texel centers. An axis that grows uses Catmull-Rom (B = 0, C = 0.5). An axis that shrinks, or stays the same length, uses Hermite.

`QuarterTurn::D90` turns the source clockwise inside `Draw::dest`, with y pointing down. There is no letterbox helper. Place the rectangle yourself. A destination that does not fit in the target, or a zero-size plane, returns `Error::InvalidSize`.

`Draw::overlays` are premultiplied RGBA8 bitmaps, drawn after the picture with blend factors one and one-minus-source-alpha. An empty slice draws none. An overlay outside the video rectangle keeps its color when the grade is not neutral.

## Targets

| `Encoding` | Target format | Stored values |
| --- | --- | --- |
| `Gamma8` | `Rgba8Unorm` | Display code / 255, after dither. Alpha is 1. |
| `Linear` | `Rgba16Float` | Linear light, 1.0 = 100 nits. Alpha is 1. |

A mismatched format returns `Error::Target`. The target is cleared to opaque black once per draw, then the destination rectangle is written. Pixels outside `Draw::dest` stay black unless an overlay covers them.

`decode_linear` decodes one sample. `renderer::placed_sample` scales and presents one pixel. The shader matches those functions, so a host can evaluate one pixel on the CPU and compare it with the texture.

## Left in mpv

Demux, decode, AO, the playloop, and libass stay in the mpv core. So do hwdec imports (`vaapi`, `nvdec`, `d3d11va`, `videotoolbox`, drmprime), `--glsl-shader`, Lanczos and EWA, deband, ICC, Dolby Vision, ST 2094, film grain, interpolation, and `display-resample`. `video-zoom` and `panscan` are the host's dest rect here. The player crate still lets mpv compute that rect. The shaders are source files. The draw path does not assemble them at runtime.

## Example

`examples/planes.rs` draws limited black and white, a 16-bit full-range peak, and an RGBA black, then prints:

```text
black=0
white=255
full16=255
rgba=0
```

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.
