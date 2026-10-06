//! wasm-env's GPU service (PLAN.md, M9): compute kernels compiled to WGSL
//! and run on wgpu, so the environment's GPU APIs work on every host (the
//! host's GPU through Vulkan, Metal or DirectX 12; lavapipe, software
//! Vulkan, where there is none).
//!
//! OpenCL comes first: OpenCL C goes through clang to LLVM IR (ir.rs),
//! whose control flow is made structured (structure.rs) and whose code
//! becomes WGSL (wgsl.rs). Vulkan's SPIR-V shaders go through naga, and its
//! command buffers are replayed (vulkan.rs).

pub mod ir;
pub mod structure;
pub mod wgsl;
mod f64;
pub mod opencl;
pub mod device;
pub mod vulkan;
pub use wgpu;
