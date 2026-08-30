//! The seven blend modes, as pipeline state.
//!
//! `blend.rs` implements these same equations by hand for the CPU. Both come
//! from the one table at the top of libobs's `obs-scene.c`
//! (`docs/references/gpu-compositing.md` §2), which is what makes comparing the
//! two backends honest: it is the same equation on different hardware, not a
//! reimplementation that happens to look similar.
//!
//! All seven are built once, at construction. Choosing a mode per item is then
//! an array index, not a pipeline compile.

use voltra_core::BlendMode;

use super::context::GpuContext;

/// The canvas format. BGRA to match [`PixelFormat::Bgra8`], so the readback is
/// a straight copy with no channel swizzle.
///
/// [`PixelFormat::Bgra8`]: voltra_core::PixelFormat::Bgra8
pub(crate) const CANVAS_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Bgra8Unorm;

/// How many blend modes there are, and therefore how many pipelines.
const MODES: usize = 7;

/// Every blend mode in a fixed order, so a mode maps to an array index.
const ORDER: [BlendMode; MODES] = [
    BlendMode::Normal,
    BlendMode::Additive,
    BlendMode::Subtract,
    BlendMode::Screen,
    BlendMode::Multiply,
    BlendMode::Lighten,
    BlendMode::Darken,
];

/// The index of `mode` in [`ORDER`].
const fn index_of(mode: BlendMode) -> usize {
    match mode {
        BlendMode::Normal => 0,
        BlendMode::Additive => 1,
        BlendMode::Subtract => 2,
        BlendMode::Screen => 3,
        BlendMode::Multiply => 4,
        BlendMode::Lighten => 5,
        BlendMode::Darken => 6,
        // A mode added later would silently draw as Normal if this fell
        // through, so it is handled explicitly at the call site instead.
        _ => usize::MAX,
    }
}

/// Whether this backend can draw `mode`.
pub(crate) const fn supports(mode: BlendMode) -> bool {
    index_of(mode) != usize::MAX
}

/// The blend state for one mode, straight from the libobs table.
fn blend_state(mode: BlendMode) -> wgpu::BlendState {
    use wgpu::{BlendComponent, BlendFactor, BlendOperation, BlendState};

    /// `src + dst·(1−srcA)`.
    const OVER: BlendComponent = BlendComponent {
        src_factor: BlendFactor::One,
        dst_factor: BlendFactor::OneMinusSrcAlpha,
        operation: BlendOperation::Add,
    };
    /// `src + dst`.
    const ADD: BlendComponent = BlendComponent {
        src_factor: BlendFactor::One,
        dst_factor: BlendFactor::One,
        operation: BlendOperation::Add,
    };

    match mode {
        BlendMode::Additive => BlendState {
            color: ADD,
            alpha: ADD,
        },
        BlendMode::Subtract => {
            // `dst − src`: reverse subtract, because the equation subtracts the
            // source from the destination and not the other way round.
            const SUB: BlendComponent = BlendComponent {
                src_factor: BlendFactor::One,
                dst_factor: BlendFactor::One,
                operation: BlendOperation::ReverseSubtract,
            };
            BlendState {
                color: SUB,
                alpha: SUB,
            }
        }
        BlendMode::Screen => BlendState {
            // `src + dst·(1−src)`: the colour factor is one minus the source
            // *colour*, not its alpha, which is what makes screen never darken.
            color: BlendComponent {
                src_factor: BlendFactor::One,
                dst_factor: BlendFactor::OneMinusSrc,
                operation: BlendOperation::Add,
            },
            alpha: OVER,
        },
        BlendMode::Multiply => BlendState {
            color: BlendComponent {
                src_factor: BlendFactor::Dst,
                dst_factor: BlendFactor::OneMinusSrcAlpha,
                operation: BlendOperation::Add,
            },
            alpha: BlendComponent {
                src_factor: BlendFactor::DstAlpha,
                dst_factor: BlendFactor::OneMinusSrcAlpha,
                operation: BlendOperation::Add,
            },
        },
        BlendMode::Lighten => {
            const MAX: BlendComponent = BlendComponent {
                src_factor: BlendFactor::One,
                dst_factor: BlendFactor::One,
                operation: BlendOperation::Max,
            };
            BlendState {
                color: MAX,
                alpha: MAX,
            }
        }
        BlendMode::Darken => {
            const MIN: BlendComponent = BlendComponent {
                src_factor: BlendFactor::One,
                dst_factor: BlendFactor::One,
                operation: BlendOperation::Min,
            };
            BlendState {
                color: MIN,
                alpha: MIN,
            }
        }
        // Normal, and anything added later, composites source-over.
        _ => BlendState {
            color: OVER,
            alpha: OVER,
        },
    }
}

/// The seven pipelines, their shared layout, and the two samplers.
#[derive(Debug)]
pub(crate) struct Pipelines {
    /// One per blend mode, indexed by [`index_of`].
    by_mode: [wgpu::RenderPipeline; MODES],
    pub(crate) layout: wgpu::BindGroupLayout,
    pub(crate) point: wgpu::Sampler,
    pub(crate) linear: wgpu::Sampler,
}

impl Pipelines {
    /// Build every pipeline and sampler once.
    pub(crate) fn new(context: &GpuContext) -> Self {
        let device = context.device();
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("voltra composite"),
            source: wgpu::ShaderSource::Wgsl(include_str!("composite.wgsl").into()),
        });

        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("voltra item"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("voltra composite"),
            bind_group_layouts: &[Some(&layout)],
            ..wgpu::PipelineLayoutDescriptor::default()
        });

        let by_mode = ORDER.map(|mode| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("voltra composite"),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vertex_main"),
                    // The quad's corners come from the uniform, so there is no
                    // vertex buffer to bind or to keep in sync.
                    buffers: &[],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fragment_main"),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: CANVAS_FORMAT,
                        blend: Some(blend_state(mode)),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleStrip,
                    // No culling: a rotation past 180° or a negative scale flips
                    // the winding, and an item that vanishes when mirrored would
                    // be a bug nobody could explain.
                    cull_mode: None,
                    ..wgpu::PrimitiveState::default()
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            })
        });

        let point = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("voltra point"),
            // Clamp to edge, matching the CPU sampler: an edge texel repeated,
            // never a wrap-around from the far side of the source.
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..wgpu::SamplerDescriptor::default()
        });
        let linear = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("voltra linear"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..wgpu::SamplerDescriptor::default()
        });

        Self {
            by_mode,
            layout,
            point,
            linear,
        }
    }

    /// The pipeline for `mode`, or `None` when this backend cannot draw it.
    pub(crate) fn for_mode(&self, mode: BlendMode) -> Option<&wgpu::RenderPipeline> {
        self.by_mode.get(index_of(mode))
    }
}

#[cfg(test)]
mod tests {
    use super::{ORDER, index_of, supports};
    use voltra_core::BlendMode;

    /// Every mode maps to its own slot. A duplicate index would silently draw
    /// one mode with another's equation.
    #[test]
    fn every_mode_has_its_own_pipeline_slot() {
        let mut seen = Vec::new();
        for mode in ORDER {
            let index = index_of(mode);
            assert!(supports(mode), "{mode:?} unsupported");
            assert!(!seen.contains(&index), "{mode:?} collides at {index}");
            seen.push(index);
        }
        assert_eq!(seen.len(), ORDER.len());
    }

    #[test]
    fn the_order_covers_every_mode_exactly_once() {
        for (slot, mode) in ORDER.iter().enumerate() {
            assert_eq!(index_of(*mode), slot);
        }
        assert_eq!(index_of(BlendMode::Normal), 0);
    }
}
