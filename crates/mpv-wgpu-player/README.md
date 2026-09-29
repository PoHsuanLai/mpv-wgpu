# mpv-wgpu-player

[![ci](https://github.com/PoHsuanLai/mpv-wgpu/actions/workflows/ci.yml/badge.svg?branch=master)](https://github.com/PoHsuanLai/mpv-wgpu/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/mpv-wgpu-player.svg)](https://crates.io/crates/mpv-wgpu-player)
[![MIT license](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE-MIT)
[![Apache 2.0 license](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE-APACHE)
![rust 1.87+](https://img.shields.io/badge/rust-1.87%2B-orange.svg)

libmpv with the VO window removed. The core is the same one the `mpv` binary uses. The presentation is a wgpu texture.

`vo=libmpv` and a software render context (`MPV_RENDER_API_TYPE_SW`, `rgb0`) replace `vo=gpu` / `vo=gpu-next` and mpv's own window. lavf, lavc, the AO, libass, and the playloop stay inside mpv. mpv still runs the dst rect, panscan, zoom, rotation, and `osd_draw_on_image` into the slot. `poll` uploads that buffer 1:1. `Picture::Shown` is gamma-encoded `Rgba8Unorm`, alpha 1, top-left. Sample it as non-sRGB. An sRGB swapchain encodes it again.

The OpenGL render API and `ra_*` are not used. Hwdec may decode inside mpv. The texture is still the software frame. OSC, `input-default-bindings`, and `input-vo-keyboard` are off. The host is the UI.

Planes the caller already has, in place of the gpu-next picture chain, are [`mpv-wgpu`](https://github.com/PoHsuanLai/mpv-wgpu/tree/master/crates/mpv-wgpu). This player does not pass the composited `rgb0` frame through that renderer.

```toml
mpv-wgpu-player = "0.1"
```

```sh
cargo doc --open -p mpv-wgpu-player
```

## System requirement

The build links libmpv. `pkg-config` must find the `mpv` module, so the libmpv development files have to be installed. This crate does not bundle mpv.

```sh
# Debian or Ubuntu
sudo apt-get install pkg-config libmpv-dev
```

docs.rs images do not ship libmpv, so rendered docs for this crate can fail there. `mpv-wgpu` documents without that library.

## Playback

`Player::new` starts an idle core. `set_slot` is the physical pixel rectangle libmpv scales and letterboxes into. `load` takes a path or any URL mpv accepts. `poll` drains events, uploads a new software frame when there is one, and submits the blit. Call `poll` on the thread that presents.

`set_notify` registers a `Fn() + Send + Sync` wake. It may run on an mpv thread and must only wake the host. A wake that arrives before the closure is registered is remembered. `Player` is `Send` and not `Sync`.

```rust
use std::num::NonZeroU32;

use mpv_wgpu_player::{Event, Picture, Player, Slot, SlotSize};

fn start(device: &wgpu::Device, queue: &wgpu::Queue, path: &str) -> Result<(), mpv_wgpu_player::Error> {
    let mut player = Player::new(device, queue)?;
    player.set_notify(|| {
        // Wake the host thread. Do not call into the player from here.
    });
    player.set_slot(Slot::Sized(SlotSize {
        width: NonZeroU32::new(1280).expect("non-zero"),
        height: NonZeroU32::new(720).expect("non-zero"),
    }))?;
    player.load(path)?;
    let outcome = player.poll()?;
    let _presentation = outcome.presentation;
    for event in player.events() {
        if let Event::Loaded = event {
            let _duration = player.duration();
        }
    }
    if let Picture::Shown(view) = player.picture() {
        let _sampled = view;
    }
    Ok(())
}
```

`Slot::Empty` skips the GPU work. Until a frame has been uploaded, `picture` is `Picture::Waiting`.

The core is created with:

| Property | Value |
| --- | --- |
| `vo` | `libmpv` |
| `hwdec` | `auto-safe` |
| `ao` | `pulse` |
| `idle` | `yes` |
| `keep-open` | `yes` |
| `video-sync` | `audio` |
| `video-timing-offset` | `0` |
| `sub-visibility` | `yes` |
| `deinterlace` | `auto` |
| `osc` | `no` |
| `input-default-bindings`, `input-vo-keyboard` | `no` |

Hardware decode may run inside libmpv. The texture the host samples is still the software RGB frame. `Player::command` forwards a string list to `mpv_command` for everything else, including another audio output.

`report_swap` runs only after a poll that consumed a new frame.

## Controls

`set_playback`, `seek`, `set_mute`, `set_deinterlace`, and `adjust` (panscan, zoom, volume) go to libmpv. `position` and `duration` are `Option<Finite>` and stay empty until mpv has reported a finite number. `Finite` rejects NaN and infinities.

`Equalizer` is brightness, contrast, saturation, and gamma in −100..=100, plus hue in −180..=180 degrees. mpv's own hue property is −100..=100. The player sends `degrees * 100 / 180` and reads the echo back with `round(raw * 180 / 100)` before any integer rounding of the mpv value.

`set_equalizer` writes those knobs into libmpv and the blit applies the same knobs again. A non-zero grade is applied twice. All zeros stay identity on both stages.

## Examples

`examples/consumer.rs` drives the control surface and prints read-backs. `examples/winit.rs` is a dev window that samples `Picture::Shown` and binds keys. winit is a dev-dependency. The library does not depend on it.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.
