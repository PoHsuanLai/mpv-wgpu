# mpv-wgpu

[![ci](https://github.com/PoHsuanLai/mpv-wgpu/actions/workflows/ci.yml/badge.svg?branch=master)](https://github.com/PoHsuanLai/mpv-wgpu/actions/workflows/ci.yml)
[![mpv-wgpu on crates.io](https://img.shields.io/crates/v/mpv-wgpu.svg?label=mpv-wgpu)](https://crates.io/crates/mpv-wgpu)
[![mpv-wgpu on docs.rs](https://img.shields.io/docsrs/mpv-wgpu.svg?label=mpv-wgpu%20docs)](https://docs.rs/mpv-wgpu)
[![mpv-wgpu-player on crates.io](https://img.shields.io/crates/v/mpv-wgpu-player.svg?label=mpv-wgpu-player)](https://crates.io/crates/mpv-wgpu-player)
[![MIT license](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE-MIT)
[![Apache 2.0 license](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE-APACHE)
![rust 1.87+](https://img.shields.io/badge/rust-1.87%2B-orange.svg)

A wgpu stand-in for mpv's video output. The mpv core stays where a file is being played. The GPU picture chain is replaced.

mpv draws frames in one of two VOs. `vo=gpu` is `video.c`, the GLSL generator, and an `ra` backend. `vo=gpu-next` (the current default) is libplacebo. `vo=libmpv` skips mpv's window and waits for the host's render context, either OpenGL or the software target. OSC, `input.conf`, and the window sit on top of that.

This repository splits those jobs into two crates. Neither one opens a window or a swapchain. The host already has the `wgpu` device.

| mpv | this repo |
| --- | --- |
| The picture chain in `vo=gpu` / `vo=gpu-next`: csp, chroma location, EOTF, scale, tone map, video-eq, dither, bitmap overlay | [`mpv-wgpu`](crates/mpv-wgpu). Static WGSL on the caller's device. The caller passes Y, U, V or RGBA. No libplacebo, no `ra` backend, no generated shader. |
| `vo=libmpv` plus the window mpv would have opened | [`mpv-wgpu-player`](crates/mpv-wgpu-player). The mpv core is unchanged: lavf, lavc, AO, libass, properties, commands, events. The render context is `MPV_RENDER_API_TYPE_SW`. The `rgb0` image is uploaded 1:1 into a host texture. |

```toml
mpv-wgpu = "0.1"          # planes, in place of gpu / gpu-next
mpv-wgpu-player = "0.1"   # libmpv, with the VO window removed
```

The player crate depends on `mpv-wgpu` for `Equalizer`. It does not run libmpv's `rgb0` frame through `Renderer`. That frame has already been through mpv's software VO: dst rect, panscan, rotation, and `osd_draw_on_image`.

```sh
cargo doc --open -p mpv-wgpu
cargo doc --open -p mpv-wgpu-player
```

## What the picture crate replaces

`Renderer` is the image path of `gpu-next`, written as three fixed shaders: decode, present, overlay. It is not bit-exact with libplacebo. The spline is a Hermite in PQ, not `tone-mapping=bt.2390`.

Taken from that chain, in simpler form:

| mpv | here |
| --- | --- |
| `colormatrix`, `video-range` | BT.601, BT.709, BT.2020. Limited range maps code 16 to 0 and 235 to 1. Full range maps 0 and the peak code. |
| `chroma-location` | `TopLeft`, `Left`, `Center`, as luma-pixel offsets. |
| `gamma` / transfer: bt.1886, the 2.2 reading of srgb, pq, hlg | Same four. Linear light is 1.0 = 100 nits, so PQ signal 1 is linear 100. HLG signal 0.75 at a 1000-nit peak is the BT.2408 reference white, about 203 nits. |
| `scale` | Separable 4-tap cubic. Growing axes are Catmull-Rom. Shrinking axes are Hermite. `lanczos`, EWA, and `dscale` are absent. |
| `video-rotate=90` | `QuarterTurn::D90`, clockwise, y down, inside the dest rect the host computed. |
| `tone-mapping` | One spline. Identity when the target peak already holds the source peak. `Encoding::Linear` skips it, the way a linear export skips the display OETF. |
| `brightness`, `contrast`, `saturation`, `gamma`, `hue` | Once, in linear light, after the spline and before the inverse transfer. Contrast −100 is linear 0.5, then BT.1886-encoded to about 0.749 on `Gamma8`. On `Linear` it stays 0.5. |
| `dither=ordered` | 8×8 Bayer at the framebuffer pixel. Error diffusion is absent. |
| Bitmap OSD / sub images | Premultiplied RGBA overlays after the grade. libass and `blend-subs=video` are absent. |

Still mpv's, or still the host's: demux, decode, AO, the playloop, hwdec surface import (`vaapi`, `nvdec`, `d3d11va`, `videotoolbox`, drmprime), `--glsl-shader`, deband, ICC, Dolby Vision, ST 2094, film grain, interpolation, and `display-resample`. The shaders are source files. The draw path does not concatenate GLSL the way `vo=gpu` does.

`Encoding::Gamma8` writes `Rgba8Unorm` (code / 255). `Encoding::Linear` writes `Rgba16Float`. A 16-bit scalar plane needs `TEXTURE_FORMAT_16BIT_NORM`.

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

The pixel contract is in the [picture crate README](crates/mpv-wgpu/README.md). `decode_linear` and `renderer::placed_sample` are the CPU copy of the shaders. `examples/planes` prints `black=0`, `white=255`, `full16=255`, `rgba=0`.

## What the player crate replaces

`Player` is a libmpv client with `vo=libmpv` and a software render context created before `loadfile`. Demux, decode, audio, subtitle rendering, and the playloop stay inside mpv. What mpv's window, OSC, and `vo_gpu_next` flip used to do is now the host's pass over `Picture::Shown`.

`set_slot` is the render size, in physical pixels. mpv runs `mp_get_src_dst_rects` into that slot, so letterbox, panscan, zoom, and rotation happen before the upload. `osd_draw_on_image` has already burned libass and OSD into the `rgb0` buffer. The wgpu pass is a 1:1 blit of that buffer into `Rgba8Unorm`. Sample it as non-sRGB. An sRGB swapchain encodes it again.

The core starts at `hwdec=auto-safe`, `video-sync=audio`, `video-timing-offset=0`, `idle=yes`, `keep-open=yes`, `sub-visibility=yes`, `deinterlace=auto`. `osc`, `input-default-bindings`, and `input-vo-keyboard` are off. The audio driver is `PlayerOptions::audio_output` (`AudioOutput::Auto` by default, which lets mpv probe; the literal `ao=auto` is not a driver name). `Player::command` is `mpv_command`. `set_notify` is the wakeup callback and must only wake the host. `report_swap` runs only after a poll that consumed a frame.

The player also exposes typed tracks, chapters, absolute volume, speed, frame step, and full-resolution screenshots, with events for seeks, cache level, and track, chapter, and volume changes. Audio-only files keep `picture()` at `Waiting`, and embedded cover art is shown. See the [player crate README](crates/mpv-wgpu-player/README.md).

`set_equalizer` sets mpv's `brightness`, `contrast`, `saturation`, `gamma`, and `hue`, and the blit bakes the same values again. A non-zero grade is applied twice. Zeros stay identity. Hue on the public type is degrees, −180..=180. The mpv property stays −100..=100, and the player scales by 100/180.

```rust
use std::num::NonZeroU32;

use mpv_wgpu_player::{Picture, Player, PlayerOptions, Slot, SlotSize};

fn start(device: &wgpu::Device, queue: &wgpu::Queue, path: &str) -> Result<(), mpv_wgpu_player::Error> {
    let mut player = Player::new(device, queue, PlayerOptions::default())?;
    player.set_slot(Slot::Sized(SlotSize {
        width: NonZeroU32::new(1280).expect("non-zero"),
        height: NonZeroU32::new(720).expect("non-zero"),
    }))?;
    player.load(path)?;
    player.poll()?;
    if let Picture::Shown(view) = player.picture() {
        let _sampled = view;
    }
    Ok(())
}
```

`pkg-config` must find `mpv`. The rest of the client surface is in the [player crate README](crates/mpv-wgpu-player/README.md).

## Building

Rust 1.87 or newer. Both crates build against wgpu 29.

```sh
cargo test -p mpv-wgpu            # picture crate, no libmpv
cargo test -p mpv-wgpu-player     # links libmpv
cargo test --workspace --locked
```

CI on Ubuntu installs `libmpv-dev` and a software Vulkan driver, then runs the workspace tests and clippy with `unwrap` denied.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.
