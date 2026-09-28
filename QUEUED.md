# Queued decisions

## Transitive `wgpu-hal` edge

Resolved in the plan, not by this file alone: the normal dependency tree may contain `wgpu-hal` because wgpu 30 depends on it. This crate still must not name or call it.

wgpu 30.0.1 depends on `wgpu-hal` unconditionally for every non-wasm target (`[target.'cfg(not(target_arch = "wasm32"))'.dependencies.wgpu-hal]`). `wgpu-core` does the same. The dependency is not behind the `vulkan`, `metal`, `dx12`, or `gles` features. Setting `default-features = false` and keeping only `std` and `wgsl` still leaves `wgpu-hal v30.0.1` in the normal tree.

The library keeps wgpu's default features so the example can create a real device. Removing the edge would mean not depending on wgpu.
