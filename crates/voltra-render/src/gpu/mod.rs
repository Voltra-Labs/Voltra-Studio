//! The accelerated backend of ADR 0001.
//!
//! Same [`Compositor`](crate::Compositor) trait as the CPU path, so callers do
//! not change. What changes is the direction of the work: the CPU compositor
//! walks destination pixels and maps each one *back* into the source, because
//! there is no rasteriser; this one transforms the four corners of a quad and
//! lets the rasteriser interpolate. See `docs/references/gpu-compositing.md`.
//!
//! The CPU path stays: it is correct, portable, runs in CI without a GPU, and
//! it is the oracle this backend is verified against.

mod compositor;
mod context;
mod pipeline;
mod readback;
mod texture;

pub use compositor::GpuCompositor;
pub use context::{GpuContext, GpuInfo};
