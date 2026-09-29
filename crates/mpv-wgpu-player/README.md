# mpv-wgpu-player

Headless libmpv playback into a caller-owned wgpu texture.

The host owns the `wgpu` device, the queue, the window, and the swapchain. libmpv decodes, plays audio, and burns subtitles into a packed RGB frame. [`Player::poll`](https://docs.rs/mpv-wgpu-player) uploads that frame. [`Player::picture`](https://docs.rs/mpv-wgpu-player) is a gamma-encoded `Rgba8Unorm` view with alpha 1 and a top-left origin. Sample it as non-sRGB data.

Picture-only drawing, with no libmpv dependency, is [`mpv-wgpu`](https://crates.io/crates/mpv-wgpu).

## System requirement

The build links libmpv. `pkg-config` must find the `mpv` module, which means the libmpv development files are installed. This crate does not bundle mpv.

docs.rs images do not ship libmpv, so the rendered docs for this crate can fail there. `mpv-wgpu` documents without that library.

## Example

```rust
use std::num::NonZeroU32;

use mpv_wgpu_player::{Picture, Player, Slot, SlotSize};

let mut player = Player::new(&device, &queue)?;
player.set_slot(Slot::Sized(SlotSize {
    width: NonZeroU32::new(1280).unwrap(),
    height: NonZeroU32::new(720).unwrap(),
}))?;
player.load(path)?;
player.poll()?;
if let Picture::Shown(view) = player.picture() {
    let _view = view;
}
```

A non-zero equalizer is written into libmpv and also baked into the blit, so the grade is applied twice. All zeros stay identity on both stages.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.
