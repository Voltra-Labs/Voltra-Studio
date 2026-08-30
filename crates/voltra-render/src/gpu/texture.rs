//! Getting source frames onto the GPU.
//!
//! One texture per source, reused across frames and only reallocated when the
//! source changes size. Uploading is unavoidable while sources live in system
//! memory — a capture API that hands over a GPU buffer is the zero-copy path of
//! ADR 0003, and it arrives with the capture phase — but allocating a texture
//! per frame is not, and that is what this avoids.

use std::collections::HashMap;

use voltra_core::{PixelFormat, SourceId, VideoFrame};

use super::context::GpuContext;
use super::readback::aligned_bytes_per_row;

/// The GPU texture format for a source frame, or `None` when this backend
/// cannot read that pixel format.
///
/// Only the packed RGB formats. A YUV source needs the inverse of the plan 004
/// conversion, which does not exist yet; the CPU path reports the same gap the
/// same way, so both backends agree on what they cannot do.
const fn texture_format(format: PixelFormat) -> Option<wgpu::TextureFormat> {
    match format {
        PixelFormat::Bgra8 => Some(wgpu::TextureFormat::Bgra8Unorm),
        PixelFormat::Rgba8 => Some(wgpu::TextureFormat::Rgba8Unorm),
        _ => None,
    }
}

/// Whether a source in this format can be drawn.
pub(crate) const fn supports(format: PixelFormat) -> bool {
    texture_format(format).is_some()
}

/// One source's texture, and the shape it was built for.
#[derive(Debug)]
struct Entry {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    width: u32,
    height: u32,
    format: wgpu::TextureFormat,
}

/// Source textures, keyed by the source they belong to.
#[derive(Debug, Default)]
pub(crate) struct TextureCache {
    entries: HashMap<u64, Entry>,
}

impl TextureCache {
    /// Upload `frame` for `source` and return its view.
    ///
    /// Returns `None` when the frame's pixel format has no GPU equivalent.
    pub(crate) fn upload(
        &mut self,
        context: &GpuContext,
        source: SourceId,
        frame: &VideoFrame,
    ) -> Option<&wgpu::TextureView> {
        let format = texture_format(frame.format())?;
        let width = frame.width();
        let height = frame.height();
        let layout = frame.plane_layout(0)?;
        let pixels = frame.plane(0)?;

        let entry = self.entries.entry(source.get());
        let entry = match entry {
            std::collections::hash_map::Entry::Occupied(slot) => {
                let existing = slot.into_mut();
                if existing.width != width || existing.height != height || existing.format != format
                {
                    *existing = Entry::new(context, width, height, format);
                }
                existing
            }
            std::collections::hash_map::Entry::Vacant(slot) => {
                slot.insert(Entry::new(context, width, height, format))
            }
        };

        // `write_texture` takes the frame's own stride, so a padded source is
        // uploaded correctly without a repacking copy.
        context.queue().write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &entry.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(u32::try_from(layout.stride()).ok()?),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );

        Some(&entry.view)
    }

    /// Forget every cached texture.
    ///
    /// Sources come and go across a session; nothing here should outlive the
    /// scene that referenced it.
    pub(crate) fn clear(&mut self) {
        self.entries.clear();
    }

    /// How many textures are being held.
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }
}

impl Entry {
    fn new(context: &GpuContext, width: u32, height: u32, format: wgpu::TextureFormat) -> Self {
        let texture = context.device().create_texture(&wgpu::TextureDescriptor {
            label: Some("voltra source"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Self {
            texture,
            view,
            width,
            height,
            format,
        }
    }
}

/// A canvas texture and the staging buffer its readback lands in.
#[derive(Debug)]
pub(crate) struct Canvas {
    pub(crate) texture: wgpu::Texture,
    pub(crate) view: wgpu::TextureView,
    pub(crate) staging: wgpu::Buffer,
}

impl Canvas {
    /// Build a canvas of `width`×`height` and the buffer to read it back into.
    pub(crate) fn new(context: &GpuContext, width: u32, height: u32) -> Self {
        let texture = context.device().create_texture(&wgpu::TextureDescriptor {
            label: Some("voltra canvas"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: super::pipeline::CANVAS_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let staging = context.device().create_buffer(&wgpu::BufferDescriptor {
            label: Some("voltra readback"),
            size: u64::from(aligned_bytes_per_row(width * 4)) * u64::from(height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        Self {
            texture,
            view,
            staging,
        }
    }
}
