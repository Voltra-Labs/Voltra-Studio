//! Compositing: a scene in, a frame out.
//!
//! ADR 0001 makes this the reference path — correct, portable and verifiable in
//! a headless container — with the `wgpu` backend arriving behind the same
//! [`Compositor`] trait. The order of operations follows libobs's
//! `obs_graphics_thread`: sources are ticked, then the canvas is cleared, then
//! items are drawn back to front.

// Panicking helpers stay available inside tests (CLAUDE.md §3).
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod blend;
mod compositor;
#[cfg(feature = "gpu")]
mod gpu;
mod sample;

pub use compositor::{CompositeStats, Compositor, CpuCompositor, FrameProvider};
#[cfg(feature = "gpu")]
pub use gpu::{GpuCompositor, GpuContext, GpuInfo};
