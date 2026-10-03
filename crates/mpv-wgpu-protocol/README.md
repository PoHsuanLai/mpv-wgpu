# mpv-wgpu-protocol

The wire protocol between [`mpv-wgpu-player`](https://github.com/PoHsuanLai/mpv-wgpu/tree/master/crates/mpv-wgpu-player)
in subprocess mode and the [`mpv-wgpu-cplugin`](https://github.com/PoHsuanLai/mpv-wgpu/tree/master/crates/mpv-wgpu-cplugin)
that runs inside the user's `mpv`. Both sides depend on this crate. It does not link or mention libmpv.

- `Message`: a versioned, compact binary encoding (little endian, length-prefixed strings) of the handshake,
  frames, presentation acknowledgements, commands, properties and events. Malformed input is an error, never a panic.
- `Channel`: a `SOCK_SEQPACKET` Unix socket that carries one message per packet and, on `Resize`, a memfd by `SCM_RIGHTS`.
- `Ring`: the three-slot `memfd` frame ring, mapped by both processes. mpv's software renderer writes `rgb0` pixels
  straight into a slot and the player uploads straight from it.

Linux only. Licensed under either of Apache-2.0 or MIT at your option.
