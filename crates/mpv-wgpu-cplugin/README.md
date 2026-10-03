# mpv-wgpu-cplugin

The mpv side of `mpv-wgpu-player`'s subprocess mode: a C plugin, written in Rust, that the user's stock `mpv`
loads with `--script=libmpv_wgpu_cplugin.so`.

Inside mpv it makes a software render context (`MPV_RENDER_API_TYPE_SW`, `vo=libmpv`), renders `rgb0` frames
straight into a three-slot memfd ring shared with the player, and forwards commands, property reads and writes,
observed properties, events and log lines over a `SOCK_SEQPACKET` socket. The player passes the socket's name in
`MPV_WGPU_SOCKET`. The wire format is [`mpv-wgpu-protocol`](../mpv-wgpu-protocol).

You do not run this by hand. `mpv-wgpu-player` starts `mpv --no-config --idle=yes --vo=libmpv
--video-timing-offset=0 --script=<this library>` and finds the library next to the executable, through
`MPV_WGPU_CPLUGIN`, or through `SubprocessOptions`.

## No libmpv at link time

The library declares the dozen `mpv_*` client and render functions it calls as `extern "C"` with no `#[link]`.
They resolve against the symbols the running `mpv` executable exports, so `ldd libmpv_wgpu_cplugin.so` shows no
libmpv. Fedora's mpv 0.41 and Debian's 0.35 and 0.40 export them. An mpv built without C plugin support
(`-Dcplugins=disabled`) cannot load it.

## Licence

This crate is MIT OR Apache-2.0, like the rest of the repository. It runs inside mpv, which is GPL-2.0-or-later
(or LGPL-2.1-or-later when built with `-Dgpl=false`), as a separately built plugin. It contains no mpv source and
links no mpv library: it calls mpv's public client API (`mpv/client.h`, `mpv/render.h`, ISC licensed) through
symbols resolved at load time, in the user's own mpv process. The player that loads it never links mpv at all.
That is the point of the mode: an application ships MIT/Apache code and uses the mpv the user already has.
