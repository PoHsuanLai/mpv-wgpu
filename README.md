# mpv-wgpu

Libraries for drawing video inside an application that already owns a [wgpu](https://wgpu.rs/) device and a window. Nothing in this repository opens a window or a swapchain.

| Crate | When to use it |
| --- | --- |
| [`mpv-wgpu`](crates/mpv-wgpu) | The host already has YUV or RGBA planes and wants them drawn into its own texture. No libmpv. |
| [`mpv-wgpu-player`](crates/mpv-wgpu-player) | The host wants libmpv to open a file, play audio, and hand back one gamma-encoded RGB texture. |

```toml
# planes you already decoded
mpv-wgpu = "0.1"

# file playback
mpv-wgpu-player = "0.1"
```

`mpv-wgpu-player` depends on `mpv-wgpu` for [`Equalizer`](crates/mpv-wgpu/src/types.rs). The player does not send its RGB frame through `Renderer`. That frame is already composited.

API documentation is the rustdoc on each crate:

```sh
cargo doc --open -p mpv-wgpu
cargo doc --open -p mpv-wgpu-player
```

## Picture renderer

`Renderer` uploads or samples the caller's planes, decodes them to linear light, scales into a destination rectangle, tone-maps, grades once, and writes the caller's target.

The order on a `Gamma8` target is spline, then the five equalizer knobs in linear light, then the inverse transfer, then an 8×8 ordered dither. A `Linear` target skips the spline and the inverse transfer, so contrast −100 stays mid-gray (`0.5`) there. On `Gamma8` that same mid-gray is encoded (BT.1886 of `0.5` is about `0.749`) before dither.

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

`target` for `Encoding::Gamma8` is `Rgba8Unorm`. For `Encoding::Linear` it is `Rgba16Float`. The full contract, including 16-bit planes, chroma siting, and overlays, is in the [picture crate README](crates/mpv-wgpu/README.md).

`examples/planes` in that crate prints the first-pixel code for black, white, a 16-bit peak, and an RGBA black:

```text
black=0
white=255
full16=255
rgba=0
```

## File playback

`Player` starts a headless libmpv core (`vo=libmpv`). libmpv demuxes, decodes, plays audio, and burns subtitles into a packed RGB frame the size of the slot. `poll` uploads that frame. `picture` is either `Picture::Waiting` or `Picture::Shown`, a gamma-encoded `Rgba8Unorm` view. Sample it as non-sRGB data. An sRGB swapchain encodes those texels a second time.

```rust
use std::num::NonZeroU32;

use mpv_wgpu_player::{Picture, Player, Slot, SlotSize};

fn start(device: &wgpu::Device, queue: &wgpu::Queue, path: &str) -> Result<(), mpv_wgpu_player::Error> {
    let mut player = Player::new(device, queue)?;
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

The build links libmpv. `pkg-config` must find the `mpv` module. Audio starts on PulseAudio (`ao=pulse`). `Player::command` forwards a string list to `mpv_command`. Details and the equalizer behavior are in the [player crate README](crates/mpv-wgpu-player/README.md).

## Building

Rust 1.87 or newer. From this repository:

```sh
cargo test --workspace --locked
cargo test -p mpv-wgpu          # no libmpv required
cargo test -p mpv-wgpu-player   # requires libmpv
```

Continuous integration on Ubuntu installs `libmpv-dev` and a software Vulkan driver, then runs the workspace tests and clippy with `unwrap` denied.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.
