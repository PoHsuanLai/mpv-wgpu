# mpv-wgpu

Draw caller-owned YUV or RGBA pictures into a caller-owned wgpu texture.

The host owns the `wgpu` device, the queue, the window, and the swapchain. This crate does not open a window and does not link libmpv. File playback lives in [`mpv-wgpu-player`](https://crates.io/crates/mpv-wgpu-player).

A `Gamma8` target is `Rgba8Unorm` and is display-encoded. A `Linear` target is `Rgba16Float`; that path skips the tone-map knee and the inverse transfer. The equalizer runs once, in linear light, before the inverse transfer.

```rust
use mpv_wgpu::{
    ChromaSiting, Coefficients, Draw, Encoding, Equalizer, Levels, Picture, PixelRect, Plane,
    PlaneBits, PlaneSource, QuarterTurn, Renderer, Transfer,
};

let mut renderer = Renderer::new(&device)?;
renderer.draw(
    &device,
    &queue,
    Draw {
        picture: Picture::Yuv { y, u, v },
        matrix: Coefficients::Bt709,
        levels: Levels::Limited,
        transfer: Transfer::Bt1886,
        siting: ChromaSiting::Left,
        peak_nits: 100.0,
        dest: PixelRect { x: 0, y: 0, width: 2, height: 2 },
        rotation: QuarterTurn::D0,
        equalizer: Equalizer::default(),
        overlays: &[],
        target: &target,
        encoding: Encoding::Gamma8,
        target_peak_nits: 100.0,
    },
)?;
```

`y`, `u`, and `v` are [`Plane`](https://docs.rs/mpv-wgpu) values. Each plane is tightly packed bytes or a texture view the caller already owns. `target` is the caller's `Rgba8Unorm` texture.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.
