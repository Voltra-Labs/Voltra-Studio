//! Getting hold of a device.
//!
//! Absence of a GPU is a *state*, not a crash (CLAUDE.md §5): every entry point
//! here reports [`Error::Unsupported`] and lets the caller decide whether to
//! fall back to the CPU path or give up.

use std::sync::Arc;

use voltra_core::{Error, Result};

/// What adapter the backend ended up on.
///
/// Worth surfacing because "it works" and "it works on a software rasteriser"
/// are very different answers, and only one of them says anything about
/// performance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuInfo {
    /// The adapter's reported name, e.g. `llvmpipe (LLVM 20.1.2, 256 bits)`.
    pub name: String,
    /// The graphics API in use, e.g. `Vulkan`.
    pub backend: String,
    /// Whether this adapter is real hardware.
    ///
    /// `false` for a software rasteriser such as Mesa's lavapipe. Any timing
    /// taken on one of those says nothing about a GPU, so this flag exists to
    /// stop such a number being reported as if it did.
    pub hardware: bool,
}

impl std::fmt::Display for GpuInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let kind = if self.hardware {
            "hardware"
        } else {
            "software"
        };
        write!(f, "{} ({}, {kind})", self.name, self.backend)
    }
}

/// A device and its queue, shared by everything that draws.
///
/// Cloning shares the underlying device rather than opening a second one:
/// adapter enumeration takes milliseconds and a process wants exactly one
/// device.
#[derive(Debug, Clone)]
pub struct GpuContext {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    device: wgpu::Device,
    queue: wgpu::Queue,
    info: GpuInfo,
}

impl GpuContext {
    /// Open a device on the best available adapter.
    ///
    /// # Errors
    ///
    /// [`Error::Unsupported`] when no adapter is present — a headless container
    /// with no GPU and no software rasteriser — or when the adapter refuses a
    /// device.
    pub fn new() -> Result<Self> {
        let instance = wgpu::Instance::default();
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            // No surface: this backend renders to a texture and reads it back,
            // which is what lets it run in CI with no display.
            compatible_surface: None,
            force_fallback_adapter: false,
            ..wgpu::RequestAdapterOptions::default()
        }))
        .map_err(|error| Error::unsupported(format!("no graphics adapter available: {error}")))?;

        let details = adapter.get_info();
        let info = GpuInfo {
            name: details.name.clone(),
            backend: details.backend.to_string(),
            hardware: !matches!(
                details.device_type,
                wgpu::DeviceType::Cpu | wgpu::DeviceType::Other
            ),
        };

        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("voltra"),
            required_features: wgpu::Features::empty(),
            // The defaults every WebGPU implementation must meet. Asking for
            // more would refuse adapters that can do everything we need.
            required_limits: wgpu::Limits::downlevel_defaults(),
            memory_hints: wgpu::MemoryHints::Performance,
            trace: wgpu::Trace::Off,
            ..wgpu::DeviceDescriptor::default()
        }))
        .map_err(|error| Error::unsupported(format!("no usable graphics device: {error}")))?;

        Ok(Self {
            inner: Arc::new(Inner {
                device,
                queue,
                info,
            }),
        })
    }

    /// What adapter this context is running on.
    #[must_use]
    pub fn info(&self) -> &GpuInfo {
        &self.inner.info
    }

    pub(crate) fn device(&self) -> &wgpu::Device {
        &self.inner.device
    }

    pub(crate) fn queue(&self) -> &wgpu::Queue {
        &self.inner.queue
    }

    /// The largest canvas this adapter can render to, in pixels.
    #[must_use]
    pub fn max_dimension(&self) -> u32 {
        self.inner.device.limits().max_texture_dimension_2d
    }
}
