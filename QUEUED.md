# Queued decisions

## Transitive `wgpu-hal` edge

`cargo tree -e normal` lists `wgpu-hal` even though this crate does not name it and does not call it.

wgpu 30.0.1 depends on `wgpu-hal` unconditionally for every non-wasm target (`[target.'cfg(not(target_arch = "wasm32"))'.dependencies.wgpu-hal]`). `wgpu-core` does the same. The dependency is not behind the `vulkan`, `metal`, `dx12`, or `gles` features. Setting `default-features = false` and keeping only `std` and `wgsl` still leaves `wgpu-hal v30.0.1` in the normal tree.

The library keeps wgpu's default features so the example can create a real device. Removing the edge would mean not depending on wgpu.
