# mpv-wgpu-player

[![ci](https://github.com/PoHsuanLai/mpv-wgpu/actions/workflows/ci.yml/badge.svg?branch=master)](https://github.com/PoHsuanLai/mpv-wgpu/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/mpv-wgpu-player.svg)](https://crates.io/crates/mpv-wgpu-player)
[![MIT license](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE-MIT)
[![Apache 2.0 license](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE-APACHE)
![rust 1.87+](https://img.shields.io/badge/rust-1.87%2B-orange.svg)

mpv with the VO window removed. The core is the same one the `mpv` binary uses. The presentation is a wgpu texture. mpv runs either as libmpv inside your process or as the user's own `mpv` executable in a child process, so an application can ship without linking any mpv code.

`vo=libmpv` and a software render context (`MPV_RENDER_API_TYPE_SW`, `rgb0`) replace `vo=gpu` / `vo=gpu-next` and mpv's own window. lavf, lavc, the AO, libass, and the playloop stay inside mpv. mpv still runs the dst rect, panscan, zoom, rotation, and `osd_draw_on_image` into the slot. `poll` uploads that buffer 1:1. `Picture::Shown` is gamma-encoded `Rgba8Unorm`, alpha 1, top-left. Sample it as non-sRGB. An sRGB swapchain encodes it again.

The OpenGL render API and `ra_*` are not used. Hwdec may decode inside mpv. The texture is still the software frame. OSC, `input-default-bindings`, and `input-vo-keyboard` are off. The host is the UI.

Planes the caller already has, in place of the gpu-next picture chain, are [`mpv-wgpu`](https://github.com/PoHsuanLai/mpv-wgpu/tree/master/crates/mpv-wgpu). This player does not pass the composited `rgb0` frame through that renderer.

```toml
mpv-wgpu-player = "0.1"                                                          # libmpv linked in
mpv-wgpu-player = { version = "0.1", default-features = false, features = ["subprocess"] }  # no libmpv
```

```sh
cargo doc --open -p mpv-wgpu-player
```

## Two hosts for mpv

`Player::new` uses `Host::default()`: libmpv in this process when the `in-process` feature is on (the default), the child process otherwise. `Player::with_host(device, queue, options, host)` picks one. Everything else in the API is the same in both modes.

| | `Host::InProcess` | `Host::Subprocess(SubprocessOptions)` |
| --- | --- | --- |
| cargo feature | `in-process` (default) | `subprocess` |
| mpv | libmpv linked into your binary | the user's `mpv` executable as a child process, with the [`mpv-wgpu-cplugin`](../mpv-wgpu-cplugin) library loaded into it as a C plugin |
| links | libmpv | nothing from mpv: no `rsmpv`, no `libmpv-sys`, no libmpv in `ldd` |
| frames | mpv renders into a CPU buffer, `poll` uploads it | mpv renders `rgb0` straight into a three-slot memfd ring shared with the player, `poll` uploads from the mapping |
| needs at run time | libmpv | `mpv` on `PATH` or `MPV_WGPU_MPV`, and `libmpv_wgpu_cplugin.so` |

### Licence model

Distro builds of libmpv are GPL-2.0-or-later (LGPL with `-Dgpl=false`), and so are the libav libraries behind it. With `in-process`, an application that links libmpv takes on that licence. With `subprocess` the application is only MIT or Apache code: it runs the user's own mpv, which the user installed from their distro, and talks to it over a socket. The plugin that runs inside mpv is also MIT or Apache and contains no mpv code; it calls mpv's public ISC-licensed client API through symbols that mpv exports. Patent-encumbered codecs (H.264, HEVC, AAC) come from the user's mpv and FFmpeg, not from your package.

### Subprocess mode

```rust
use mpv_wgpu_player::{Host, Player, PlayerOptions, SubprocessOptions};

fn start(device: &wgpu::Device, queue: &wgpu::Queue) -> Result<Player, mpv_wgpu_player::Error> {
    Player::with_host(
        device,
        queue,
        PlayerOptions::default(),
        Host::Subprocess(SubprocessOptions::default()),
    )
}
```

`mpv` comes from `SubprocessOptions::mpv`, else `MPV_WGPU_MPV`, else `mpv` on `PATH`. The plugin comes from `SubprocessOptions::cplugin`, else `MPV_WGPU_CPLUGIN`, else the directory of the running executable or its parent, where `cargo build -p mpv-wgpu-cplugin` puts `libmpv_wgpu_cplugin.so`. `extra_args` adds mpv options after the player's own.

The player starts `mpv --no-config --load-scripts=no --idle=yes --vo=libmpv --video-timing-offset=0 --script=<plugin>` plus the options in the table below, connects over a private abstract `SOCK_SEQPACKET` socket, and checks the plugin's hello: the protocol version must match exactly and mpv's client API must be 2.0 or newer. mpv has to be built with C plugin support (`-Dcplugins=enabled`); Fedora's 0.41 and Debian's 0.35 and 0.40 export what the plugin needs.

What differs from in-process mode:

- Calls that talk to mpv (`load`, `seek`, `set_*`, `select_track`, `command`, and the reads of `volume` and `speed`) are round trips over the socket, tens of microseconds when mpv is idle. They return mpv's own error as `Error::Mpv`, and `Error::HostTimeout` if mpv does not answer for 10 seconds. The typed getters (`tracks`, `chapters`, `position`, `duration`, ...) read observed-property caches in both modes and never block.
- Events, observed properties and mpv's log lines arrive on a reader thread. The next `poll` applies them, and `set_notify` wakes you from that thread.
- If `poll` is slower than mpv renders, the newest of the three ring slots wins and older frames are skipped. `poll` never copies a frame twice: it uploads straight from the mapping and then tells the plugin the slot was presented, which is when the plugin calls `mpv_render_context_report_swap`.
- File names in `command` and `screenshot_to_file` are resolved by the mpv process, which inherits your working directory.
- If mpv exits or crashes, `poll` returns `Error::HostGone` and so does every later call. Create a new `Player` to play again. Dropping a `Player` asks mpv to quit and kills it after 1.5 seconds. mpv's output goes to the `log` crate at `warn`.
- A start failure is `Error::HostStart`, with text naming what was missing (mpv, the plugin, a version, or a build without C plugins).
- Linux only for now.

`cargo run --release --example bench -- FILE 3840 2160 8 subprocess` plays a file headless for a few seconds and prints the frame rate, with `RUST_LOG=info` adding dropped frames and how long each frame waited between the plugin's render and the upload.

## System requirement

With the `in-process` feature the build links libmpv. `pkg-config` must find the `mpv` module, so the libmpv development files have to be installed. This crate does not bundle mpv. With only the `subprocess` feature there is nothing to install at build time, and the user needs an `mpv` at run time.

```sh
# Debian or Ubuntu
sudo apt-get install pkg-config libmpv-dev mpv
```

docs.rs images do not ship libmpv, so rendered docs for this crate can fail there with the default features. `mpv-wgpu` documents without that library, and so does this crate with `--no-default-features --features subprocess`.

## Playback

`Player::new` starts an idle core. `set_slot` is the physical pixel rectangle libmpv scales and letterboxes into. `load` takes a path or any URL mpv accepts. `poll` drains events, uploads a new software frame when there is one, and submits the blit. Call `poll` on the thread that presents.

`set_notify` registers a `Fn() + Send + Sync` wake. It may run on an mpv thread (the reader thread in subprocess mode) and must only wake the host. A wake that arrives before the closure is registered is remembered. `Player` is `Send` and not `Sync`.

```rust
use std::num::NonZeroU32;

use mpv_wgpu_player::{Event, Picture, Player, PlayerOptions, Slot, SlotSize};

fn start(device: &wgpu::Device, queue: &wgpu::Queue, path: &str) -> Result<(), mpv_wgpu_player::Error> {
    let mut player = Player::new(device, queue, PlayerOptions::default())?;
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
| `ao` | from `PlayerOptions::audio_output`; unset for `AudioOutput::Auto` |
| `idle` | `yes` |
| `keep-open` | `yes` |
| `video-sync` | `audio` |
| `video-timing-offset` | `0` |
| `sub-visibility` | `yes` |
| `deinterlace` | `auto`, kept at mpv's default when this libmpv rejects it (0.37 does) |
| `volume-max` | `150` |
| `audio-display` | `embedded-first` |
| `osc` | `no` |
| `input-default-bindings`, `input-vo-keyboard` | `no` |

Hardware decode may run inside mpv. The texture the host samples is still the software RGB frame. `Player::command` forwards a string list to `mpv_command` for everything else. In subprocess mode these are `mpv` command-line options with the same names, plus `--no-config`.

`report_swap` runs only after a poll that consumed a new frame.

## Audio output

`PlayerOptions::audio_output` takes an `AudioOutput`: `Auto` (the default), `Pulse`, `PipeWire`, `Alsa`, `CoreAudio`, `Wasapi`, or `Null`.

`Auto` does not write `ao`. mpv then probes every driver it was built with, in its own order, and falls through to the next one when a driver cannot open a device. `auto` is not a driver name for `--ao`: setting it is accepted at startup but fails when playback begins with `Audio output auto not found!`, and an audio-only file then ends with `EndReason::Error`. The named variants pin one driver and do not fall back, so a missing server ends an audio-only file with an error. `Null` decodes and clocks the audio and plays nothing, which suits headless runs.

## Controls

`set_playback`, `seek`, `set_mute`, `set_deinterlace`, and `adjust` (panscan, zoom, volume) go to libmpv. `position` and `duration` are `Option<Finite>` and stay empty until mpv has reported a finite number. `Finite` rejects NaN and infinities.

`Equalizer` is brightness, contrast, saturation, and gamma in −100..=100, plus hue in −180..=180 degrees. mpv's own hue property is −100..=100. The player sends `degrees * 100 / 180` and reads the echo back with `round(raw * 180 / 100)` before any integer rounding of the mpv value.

`set_equalizer` writes those knobs into libmpv and the blit applies the same knobs again. A non-zero grade is applied twice. All zeros stay identity on both stages.

## State, tracks, and chapters

Tracks, chapters, volume, and the seek and cache state are mirrored from observed mpv properties. Each change shows up in `events()` after the `poll` that saw it. Reads never block on mpv, except `volume` and `speed`, which ask the core directly and so reflect a `set_*` made a moment ago.

| Call | Meaning |
| --- | --- |
| `tracks() -> TrackList` | Every track: `id`, `kind` (`Video`, `Audio`, `Subtitle`), `title`, `lang`, `codec`, `default` (`TrackDefault`), `selected` (`TrackSelection`), `origin` (`TrackOrigin::External` for a sidecar file), `art` (`TrackArt::Cover` for attached images). Ids count from 1 within each kind. |
| `select_track(TrackKind, TrackChoice)` | `Off`, `Auto`, or `Id(TrackId)`; sets mpv's `vid` / `aid` / `sid`. |
| `chapters() -> Vec<Chapter>`, `chapter()`, `set_chapter(ChapterIndex)` | Titles and start seconds. `set_chapter` seeks and fails with `Error::NoSuchChapter` past the end. |
| `set_volume(Volume)`, `volume()` | Absolute percent. `Volume` is clamped to `0..=150`; above 100 mpv amplifies and can clip. `adjust(Adjust::Volume)` still adds a delta. |
| `set_speed(Speed)`, `speed()` | Multiples of normal, `0.01..=100`, stored in thousandths. |
| `frame_step(Direction)` | `frame-step` or `frame-back-step`. mpv pauses. |
| `screenshot_to_file(&Path, ScreenshotContent)` | mpv's `screenshot-to-file`; the extension picks the format. `Video` and `Subtitles` are at source resolution, `Window` is the slot size with OSD. This is the full-resolution "save current frame". |
| `has_video() -> VideoPresence` | `Absent`, `CoverArt`, or `Present`, from the selected video track. |

New events: `SeekDone`, `Buffering(Percent)`, `TracksChanged`, `ChaptersChanged`, `VolumeChanged`. `Buffering` fires when mpv's `cache-buffering-state` moves; local files stay at 100 and never fire it. `VolumeChanged` fires for any change, whoever asked, and carries no value: read `volume()`. `command(&[&str])` stays the escape hatch.

## Audio-only files

An audio file loads and fires `Event::Loaded` like any other. With no video track `has_video()` is `Absent` and `picture()` is `Picture::Waiting`: the player does not paint a black frame, and a picture that was showing from the previous file goes back to `Waiting` with `Presentation::Updated`. With `keep-open=yes` the file does not end with `Event::Ended` at the last sample. mpv pauses, which arrives as `Event::Playback(Playback::Paused)`.

Embedded cover art (the tests use an MP3 with an attached PNG) is a video track with the `albumart` flag under `audio-display=embedded-first`. The player renders it as a still: `has_video()` is `CoverArt`, `Track::art` is `TrackArt::Cover`, and `picture()` is `Shown` like any frame, letterboxed into the slot. Hosts that want no artwork can call `select_track(TrackKind::Video, TrackChoice::Off)`.

## Tests

`cargo test -p mpv-wgpu-player` also runs `tests/playback.rs` and `tests/audio_only.rs`. They open a wgpu adapter (the fallback adapter first, so a software Vulkan driver is enough), start a `Player` on `AudioOutput::Null`, and play the small files in `tests/fixtures`. When no adapter can be opened they print `gpu-device-unavailable` and pass without running.

Each of those tests runs once per compiled-in host, as `name::in_process` and `name::subprocess`. Add the feature to run both: `cargo test -p mpv-wgpu-player --features subprocess`. The subprocess tests spawn the `mpv` from `MPV_WGPU_MPV`, else from `PATH`, and print `skipped: no mpv` and pass when there is none. They load the plugin library the dev-dependency on `mpv-wgpu-cplugin` builds. `tests/subprocess.rs` covers the child's lifecycle: a killed mpv is `HostGone`, dropping the player ends mpv, and a missing mpv, a missing plugin or a bad mpv option is a readable `HostStart`.

## Examples

`examples/consumer.rs` drives the control surface and prints read-backs. `examples/bench.rs` measures throughput in either host mode. `examples/winit.rs` is a dev window that samples `Picture::Shown` and binds keys. winit is a dev-dependency. The library does not depend on it.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.
