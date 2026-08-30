//! The GPU compositor: the per-item loop.

use std::time::Instant;

use voltra_core::{
    BlendMode, Error, FrameSize, PixelFormat, Placement, Rect, Result, ScaleFilter, Scene,
    Timestamp, Vec2, VideoFrame,
};

use crate::compositor::{CompositeStats, Compositor, FrameProvider};

use super::context::{GpuContext, GpuInfo};
use super::pipeline::Pipelines;
use super::readback::read_texture_into;
use super::texture::{Canvas, TextureCache};

/// One item's uniform block, matching `Item` in `composite.wgsl`.
///
/// Four corners as `(x, y, u, v)`, then the canvas size and the opacity. The
/// corners are canvas-space pixels, already transformed on the CPU, so the
/// shader does no matrix work at all.
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ItemUniform {
    corners: [[f32; 4]; 4],
    canvas: [f32; 2],
    opacity: f32,
    padding: f32,
}

/// Everything one item needs, resolved before the render pass opens.
///
/// A pass borrows the encoder and forbids uploading a texture while it is open,
/// so all the CPU-side work — placement, clipping, texture upload — happens
/// first and the pass itself is nothing but draws.
#[derive(Debug)]
struct Draw {
    uniform: ItemUniform,
    /// Index into the views collected alongside.
    view: usize,
    blend: BlendMode,
    filter: ScaleFilter,
}

/// `u32` to `f32` for canvas and source dimensions.
///
/// Exact for every size that can exist: a canvas wider than 2^24 pixels would
/// need 64 MB for one row, and no adapter allows anything close.
#[allow(clippy::cast_precision_loss)]
fn as_f32(value: u32) -> f32 {
    value as f32
}

/// Composites a scene on the GPU.
///
/// The accelerated backend of ADR 0001, behind the same [`Compositor`] trait as
/// [`CpuCompositor`](crate::CpuCompositor) so callers do not change.
///
/// # Examples
///
/// ```no_run
/// use voltra_core::{FrameSize, Scene, Timestamp};
/// use voltra_render::{Compositor, GpuCompositor};
///
/// let size = FrameSize::new(64, 64)?;
/// // `Unsupported` when the machine has no adapter, which is a state to handle
/// // and not a crash: fall back to the CPU compositor.
/// let mut compositor = GpuCompositor::new(size)?;
/// let canvas = compositor.composite(&Scene::new("Empty", size), &(), Timestamp::ZERO)?;
/// assert_eq!(canvas.width(), 64);
/// # Ok::<(), voltra_core::Error>(())
/// ```
#[derive(Debug)]
pub struct GpuCompositor {
    context: GpuContext,
    pipelines: Pipelines,
    canvas: Canvas,
    textures: TextureCache,
    frame: VideoFrame,
    background: [u8; 4],
    stats: CompositeStats,
}

impl GpuCompositor {
    /// Open a device and build everything a frame needs.
    ///
    /// # Errors
    ///
    /// [`Error::Unsupported`] when no adapter is available, or when the canvas
    /// is larger than the adapter can render to. [`Error::Config`] from
    /// [`VideoFrame::new`].
    pub fn new(size: FrameSize) -> Result<Self> {
        Self::with_context(GpuContext::new()?, size)
    }

    /// Build on an already-open device, so several compositors share one.
    ///
    /// # Errors
    ///
    /// As [`GpuCompositor::new`], minus the adapter search.
    pub fn with_context(context: GpuContext, size: FrameSize) -> Result<Self> {
        let limit = context.max_dimension();
        if size.width() > limit || size.height() > limit {
            return Err(Error::unsupported(format!(
                "canvas {size} exceeds the adapter's {limit}x{limit} limit"
            )));
        }

        let pipelines = Pipelines::new(&context);
        let canvas = Canvas::new(&context, size.width(), size.height());
        let frame = VideoFrame::new(PixelFormat::Bgra8, size, Timestamp::ZERO)?;

        Ok(Self {
            context,
            pipelines,
            canvas,
            textures: TextureCache::default(),
            frame,
            background: [0, 0, 0, 255],
            stats: CompositeStats::default(),
        })
    }

    /// What adapter this compositor is running on.
    ///
    /// Check [`GpuInfo::hardware`] before reporting any timing: a software
    /// rasteriser runs the same code and says nothing about a GPU.
    #[must_use]
    pub fn info(&self) -> &GpuInfo {
        self.context.info()
    }

    /// The colour the canvas is cleared to, as `[r, g, b, a]`.
    #[must_use]
    pub const fn background(&self) -> [u8; 4] {
        self.background
    }

    /// Set the clear colour, as `[r, g, b, a]`.
    pub const fn set_background(&mut self, color: [u8; 4]) {
        self.background = color;
    }

    /// What the last composition did.
    #[must_use]
    pub const fn stats(&self) -> CompositeStats {
        self.stats
    }

    /// How many source textures are currently cached.
    #[must_use]
    pub fn cached_textures(&self) -> usize {
        self.textures.len()
    }

    /// Drop every cached source texture.
    ///
    /// Textures are keyed by [`SourceId`](voltra_core::SourceId) and live until
    /// dropped, so a long session that adds and removes sources would otherwise
    /// hold video memory for content nobody can see any more. The engine calls
    /// this when sources go away; a caller that never removes any never needs
    /// it.
    pub fn clear_source_cache(&mut self) {
        self.textures.clear();
    }

    /// Resize the canvas, reallocating only when it actually changed.
    fn resize(&mut self, size: FrameSize) -> Result<()> {
        if size == self.frame.size() {
            return Ok(());
        }
        let limit = self.context.max_dimension();
        if size.width() > limit || size.height() > limit {
            return Err(Error::unsupported(format!(
                "canvas {size} exceeds the adapter's {limit}x{limit} limit"
            )));
        }
        self.canvas = Canvas::new(&self.context, size.width(), size.height());
        self.frame = VideoFrame::new(PixelFormat::Bgra8, size, self.frame.pts())?;
        Ok(())
    }

    /// The four `(x, y, u, v)` corners of an item's quad.
    ///
    /// `Placement::forward` maps source pixels to canvas pixels, so the corners
    /// of `placement.src` are the corners of the quad — the same geometry the
    /// CPU path uses, read forwards instead of backwards.
    fn corners(placement: &Placement, source: Vec2) -> [[f32; 4]; 4] {
        let rect = placement.src;
        let points = [
            Vec2::new(rect.x, rect.y),
            Vec2::new(rect.right(), rect.y),
            Vec2::new(rect.right(), rect.bottom()),
            Vec2::new(rect.x, rect.bottom()),
        ];
        points.map(|point| {
            let canvas = placement.forward.apply(point);
            [canvas.x, canvas.y, point.x / source.x, point.y / source.y]
        })
    }
}

impl GpuCompositor {
    /// Resolve every visible item into a draw, uploading its texture.
    ///
    /// Returns the draws, the texture views they refer to, and the statistics
    /// for the items that never became draws.
    fn plan(
        &mut self,
        scene: &Scene,
        sources: &dyn FrameProvider,
        canvas: Vec2,
    ) -> (Vec<Draw>, Vec<wgpu::TextureView>, CompositeStats) {
        let mut stats = CompositeStats::default();
        let mut draws = Vec::with_capacity(scene.len());
        let mut views = Vec::with_capacity(scene.len());

        for item in scene.visible_items() {
            let Some(frame) = sources.frame(item.source()) else {
                stats.skipped += 1;
                continue;
            };
            if !super::texture::supports(frame.format()) || !super::pipeline::supports(item.blend) {
                stats.unsupported += 1;
                continue;
            }
            let source = Vec2::new(as_f32(frame.width()), as_f32(frame.height()));
            let Some(placement) = item.transform.place(source) else {
                stats.skipped += 1;
                continue;
            };
            // The rasteriser would clip an off-canvas item anyway, but counting
            // it here keeps both backends' statistics identical, which is what
            // the parity suite checks.
            if placement
                .bbox
                .intersect(Rect::new(0.0, 0.0, canvas.x, canvas.y))
                .is_empty()
            {
                stats.skipped += 1;
                continue;
            }
            let Some(view) = self.textures.upload(&self.context, item.source(), frame) else {
                stats.unsupported += 1;
                continue;
            };

            views.push(view.clone());
            draws.push(Draw {
                uniform: ItemUniform {
                    corners: Self::corners(&placement, source),
                    canvas: [canvas.x, canvas.y],
                    opacity: 1.0,
                    padding: 0.0,
                },
                view: views.len() - 1,
                blend: item.blend,
                filter: item.scale_filter,
            });
            stats.drawn += 1;
        }

        (draws, views, stats)
    }

    /// Clear the canvas and draw every planned item, back to front.
    fn encode(&self, draws: &[Draw], views: &[wgpu::TextureView]) {
        let device = self.context.device();
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("voltra composite"),
        });

        {
            let [red, green, blue, alpha] = self.background;
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("voltra composite"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.canvas.view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        // Clearing through the load op rather than by drawing a
                        // full-screen quad: it is what the hardware is for.
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: channel(red),
                            g: channel(green),
                            b: channel(blue),
                            a: channel(alpha),
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });

            for draw in draws {
                let Some(pipeline) = self.pipelines.for_mode(draw.blend) else {
                    continue;
                };
                let uniform =
                    device.create_uniform("voltra item", bytemuck::bytes_of(&draw.uniform));
                let sampler = match draw.filter {
                    // Point is nearest neighbour. Everything else resolves to
                    // linear, exactly as the CPU path documents: Bicubic,
                    // Lanczos and Area arrive with their own measurements.
                    ScaleFilter::Point => &self.pipelines.point,
                    _ => &self.pipelines.linear,
                };
                let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("voltra item"),
                    layout: &self.pipelines.layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: uniform.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::TextureView(&views[draw.view]),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: wgpu::BindingResource::Sampler(sampler),
                        },
                    ],
                });
                pass.set_pipeline(pipeline);
                pass.set_bind_group(0, &bind, &[]);
                pass.draw(0..4, 0..1);
            }
        }

        self.context.queue().submit(Some(encoder.finish()));
    }
}

impl Compositor for GpuCompositor {
    fn composite(
        &mut self,
        scene: &Scene,
        sources: &dyn FrameProvider,
        pts: Timestamp,
    ) -> Result<&VideoFrame> {
        let started = Instant::now();
        self.resize(scene.size())?;
        self.frame.set_pts(pts);

        let size = self.frame.size();
        let canvas = Vec2::new(as_f32(size.width()), as_f32(size.height()));

        let (draws, views, mut stats) = self.plan(scene, sources, canvas);
        self.encode(&draws, &views);
        read_texture_into(
            &self.context,
            &self.canvas.texture,
            &self.canvas.staging,
            &mut self.frame,
        )?;

        stats.duration = started.elapsed();
        self.stats = stats;
        Ok(&self.frame)
    }
}

/// An 8-bit channel as the 0..=1 the clear operation wants.
///
/// The canvas target is not sRGB, so the value passes through with no gamma
/// curve. Named rather than inlined so the next person does not "fix" it by
/// adding one, which would put the clear colour out of step with every pixel
/// drawn over it.
fn channel(value: u8) -> f64 {
    f64::from(value) / 255.0
}

/// Uploading a uniform block, so the call site reads the same everywhere.
trait UniformExt {
    fn create_uniform(&self, label: &str, contents: &[u8]) -> wgpu::Buffer;
}

impl UniformExt for wgpu::Device {
    fn create_uniform(&self, label: &str, contents: &[u8]) -> wgpu::Buffer {
        let buffer = self.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: contents.len() as u64,
            usage: wgpu::BufferUsages::UNIFORM,
            mapped_at_creation: true,
        });
        if let Ok(mut mapped) = buffer.slice(..).get_mapped_range_mut() {
            mapped.copy_from_slice(contents);
        }
        buffer.unmap();
        buffer
    }
}
