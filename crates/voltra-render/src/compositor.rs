//! Turning a scene into a frame.

use std::time::{Duration, Instant};

use voltra_core::{
    Affine, BlendMode, FrameSize, PixelFormat, Placement, Rect, Result, ScaleFilter, Scene,
    SourceId, Timestamp, Vec2, VideoFrame,
};

use crate::blend::{
    Additive, Blend, Darken, Lighten, Multiply, Normal, Screen, Subtract, premultiply,
};
use crate::sample::{Bilinear, Point, Sampler, SourceView, to_fixed};

/// Where the compositor gets the pixels for a scene item.
///
/// The compositor resolves a [`SourceId`] to a *frame*, never to a source. That
/// is deliberate: in libobs a scene is a source, so drawing a nested scene is a
/// recursive call and adding a child needs a cycle check. Here a nested scene is
/// composited first, to its own frame, and arrives as one more frame. The
/// compositor cannot recurse because it never recurses — the risk plan 007 left
/// open disappears by construction rather than by a check someone can forget.
pub trait FrameProvider {
    /// The current frame of `source`, or `None` when it has none.
    fn frame(&self, source: SourceId) -> Option<&VideoFrame>;
}

/// What happened during the last composition.
///
/// Always on, per CLAUDE.md §4.10: a pipeline whose cost nobody watches is one
/// that quietly degrades.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CompositeStats {
    /// Items actually drawn.
    pub drawn: u32,
    /// Items with no frame, nothing visible, or entirely off canvas.
    pub skipped: u32,
    /// Items whose pixel format the CPU path cannot read yet.
    pub unsupported: u32,
    /// How long the composition took.
    pub duration: Duration,
}

/// Composites a scene onto a canvas.
///
/// ADR 0001 puts the accelerated backend behind this same trait, so callers do
/// not change when the GPU path arrives.
pub trait Compositor {
    /// Draw `scene` and return the finished canvas.
    ///
    /// # Errors
    ///
    /// When the canvas cannot be prepared for the scene's size.
    fn composite(
        &mut self,
        scene: &Scene,
        sources: &dyn FrameProvider,
        pts: Timestamp,
    ) -> Result<&VideoFrame>;
}

/// The reference compositor: correct, portable, no GPU.
///
/// # Examples
///
/// ```
/// use voltra_core::{FrameSize, Scene, SourceId, Timestamp};
/// use voltra_render::{Compositor, CpuCompositor};
///
/// let size = FrameSize::new(64, 64)?;
/// let scene = Scene::new("Empty", size);
///
/// let mut compositor = CpuCompositor::new(size)?;
/// compositor.set_background([0, 0, 255, 255]);
/// let canvas = compositor.composite(&scene, &(), Timestamp::ZERO)?;
///
/// // An empty scene is the background, stored as BGRA.
/// assert_eq!(&canvas.plane(0).expect("pixels")[..4], &[255, 0, 0, 255]);
/// # Ok::<(), voltra_core::Error>(())
/// ```
#[derive(Debug)]
pub struct CpuCompositor {
    canvas: VideoFrame,
    background: [u8; 4],
    stats: CompositeStats,
}

/// Which sampler an item's scale filter resolves to.
#[derive(Debug, Clone, Copy)]
enum SamplerKind {
    Point,
    Bilinear,
}

impl CpuCompositor {
    /// A compositor drawing onto a canvas of `size`, on an opaque black
    /// background.
    ///
    /// # Errors
    ///
    /// Propagates from [`VideoFrame::new`].
    pub fn new(size: FrameSize) -> Result<Self> {
        Ok(Self {
            canvas: VideoFrame::new(PixelFormat::Bgra8, size, Timestamp::ZERO)?,
            background: [0, 0, 0, 255],
            stats: CompositeStats::default(),
        })
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

    /// The canvas as it stands.
    #[must_use]
    pub const fn canvas(&self) -> &VideoFrame {
        &self.canvas
    }

    /// What the last composition did.
    #[must_use]
    pub const fn stats(&self) -> CompositeStats {
        self.stats
    }

    /// Fill the canvas with the background colour.
    fn clear(&mut self) {
        let [red, green, blue, alpha] = self.background;
        let pixel = [blue, green, red, alpha];
        if let Some(rows) = self.canvas.rows_mut(0) {
            for row in rows {
                for target in row.chunks_exact_mut(4) {
                    target.copy_from_slice(&pixel);
                }
            }
        }
    }

    /// Make sure the canvas matches the scene, reallocating only on a change.
    fn prepare(&mut self, size: FrameSize, pts: Timestamp) -> Result<()> {
        if self.canvas.size() == size {
            self.canvas.set_pts(pts);
        } else {
            self.canvas = VideoFrame::new(PixelFormat::Bgra8, size, pts)?;
        }
        Ok(())
    }
}

impl Compositor for CpuCompositor {
    fn composite(
        &mut self,
        scene: &Scene,
        sources: &dyn FrameProvider,
        pts: Timestamp,
    ) -> Result<&VideoFrame> {
        let started = Instant::now();
        self.prepare(scene.size(), pts)?;
        self.clear();

        let mut stats = CompositeStats::default();
        let canvas_size = (
            i64::from(self.canvas.width()),
            i64::from(self.canvas.height()),
        );

        for item in scene.visible_items() {
            let Some(frame) = sources.frame(item.source()) else {
                stats.skipped += 1;
                continue;
            };

            // The CPU path reads packed RGB. A camera handing over I420 needs
            // the YUV to RGB direction, which does not exist yet (plan 004 did
            // only RGB to YUV), so it is reported rather than drawn wrong.
            let (red, blue) = match frame.format() {
                PixelFormat::Bgra8 => (2, 0),
                PixelFormat::Rgba8 => (0, 2),
                _ => {
                    stats.unsupported += 1;
                    continue;
                }
            };

            #[allow(
                clippy::cast_precision_loss,
                reason = "frame dimensions are far below 2^24, exact in f32"
            )]
            let source_size = Vec2::new(frame.width() as f32, frame.height() as f32);
            let Some(placement) = item.transform.place(source_size) else {
                stats.skipped += 1;
                continue;
            };
            let Some(bounds) = clip(placement.bbox, canvas_size) else {
                stats.skipped += 1;
                continue;
            };
            let (Some(layout), Some(pixels)) = (frame.plane_layout(0), frame.plane(0)) else {
                stats.skipped += 1;
                continue;
            };

            #[allow(
                clippy::cast_possible_truncation,
                reason = "frame coordinates are far inside i32"
            )]
            let view = SourceView {
                pixels,
                stride: layout.stride(),
                min_x: placement.src.x as i32,
                min_y: placement.src.y as i32,
                max_x: (placement.src.right() as i32) - 1,
                max_y: (placement.src.bottom() as i32) - 1,
                red,
                blue,
            };
            draw(
                &mut self.canvas,
                view,
                &placement,
                bounds,
                item.blend,
                sampler_for(item.scale_filter),
            );
            stats.drawn += 1;
        }

        stats.duration = started.elapsed();
        self.stats = stats;
        Ok(&self.canvas)
    }
}

/// Nothing to draw — useful for an empty scene and in tests.
impl FrameProvider for () {
    fn frame(&self, _source: SourceId) -> Option<&VideoFrame> {
        None
    }
}

/// Which sampler a scale filter uses.
///
/// `Bicubic`, `Lanczos` and `Area` fall back to bilinear for now. `Area` is the
/// one that will matter, on large downscales, and it will arrive with its own
/// measurement rather than as a guess.
const fn sampler_for(filter: ScaleFilter) -> SamplerKind {
    match filter {
        ScaleFilter::Disable | ScaleFilter::Point => SamplerKind::Point,
        _ => SamplerKind::Bilinear,
    }
}

/// The canvas pixels an item can touch, or `None` when it lands entirely off.
fn clip(bbox: Rect, canvas: (i64, i64)) -> Option<(usize, usize, usize, usize)> {
    let (left, top, right, bottom) = bbox.outer_pixels();
    let left = left.max(0);
    let top = top.max(0);
    let right = right.min(canvas.0);
    let bottom = bottom.min(canvas.1);
    if left >= right || top >= bottom {
        return None;
    }
    #[allow(
        clippy::cast_sign_loss,
        clippy::cast_possible_truncation,
        reason = "clamped to zero and to the canvas size, which came from usize dimensions"
    )]
    Some((left as usize, top as usize, right as usize, bottom as usize))
}

/// Whether the placement maps destination pixels onto source pixels one to one.
///
/// When it does, bilinear sampling reads four texels whose weights resolve to a
/// single one, so substituting nearest neighbour is **bit-identical** — not a
/// quality trade. It is also the most common case there is: a full-screen
/// capture at canvas resolution.
fn is_pixel_aligned(placement: &Placement) -> bool {
    const EPSILON: f32 = 1e-4;
    let inverse = placement.inverse;
    inverse.b.abs() < EPSILON
        && inverse.c.abs() < EPSILON
        && (inverse.a - 1.0).abs() < EPSILON
        && (inverse.d - 1.0).abs() < EPSILON
        && (inverse.e - inverse.e.round()).abs() < EPSILON
        && (inverse.f - inverse.f.round()).abs() < EPSILON
}

/// Resolve blend mode and sampler once, then run a loop with neither branch.
fn draw(
    canvas: &mut VideoFrame,
    view: SourceView<'_>,
    placement: &Placement,
    bounds: (usize, usize, usize, usize),
    blend: BlendMode,
    requested: SamplerKind,
) {
    let sampler = if is_pixel_aligned(placement) {
        SamplerKind::Point
    } else {
        requested
    };
    match blend {
        BlendMode::Additive => draw_sampled::<Additive>(canvas, view, placement, bounds, sampler),
        BlendMode::Subtract => draw_sampled::<Subtract>(canvas, view, placement, bounds, sampler),
        BlendMode::Screen => draw_sampled::<Screen>(canvas, view, placement, bounds, sampler),
        BlendMode::Multiply => draw_sampled::<Multiply>(canvas, view, placement, bounds, sampler),
        BlendMode::Lighten => draw_sampled::<Lighten>(canvas, view, placement, bounds, sampler),
        BlendMode::Darken => draw_sampled::<Darken>(canvas, view, placement, bounds, sampler),
        // Normal, and any mode added to the enum later, composite source-over.
        _ => draw_sampled::<Normal>(canvas, view, placement, bounds, sampler),
    }
}

/// The second half of the dispatch.
fn draw_sampled<B: Blend>(
    canvas: &mut VideoFrame,
    view: SourceView<'_>,
    placement: &Placement,
    bounds: (usize, usize, usize, usize),
    sampler: SamplerKind,
) {
    match sampler {
        SamplerKind::Point => draw_item::<B, Point>(canvas, view, placement, bounds),
        SamplerKind::Bilinear => draw_item::<B, Bilinear>(canvas, view, placement, bounds),
    }
}

/// The pixel loop. No branch here decides how to sample or how to blend.
fn draw_item<B: Blend, S: Sampler>(
    canvas: &mut VideoFrame,
    view: SourceView<'_>,
    placement: &Placement,
    (left, top, right, bottom): (usize, usize, usize, usize),
) {
    let inverse: Affine = placement.inverse;
    let source = placement.src;
    let Some(rows) = canvas.rows_mut(0) else {
        return;
    };

    // Stepping one pixel to the right moves the source position by a constant,
    // so the transform is applied once per row rather than once per pixel — and
    // in fixed point, so the step accumulates no error and the sampler needs no
    // float-to-integer conversion.
    let step_x = to_fixed(inverse.a);
    let step_y = to_fixed(inverse.b);
    let (limit_left, limit_right) = (to_fixed(source.x), to_fixed(source.right()));
    let (limit_top, limit_bottom) = (to_fixed(source.y), to_fixed(source.bottom()));

    for (y, row) in rows.enumerate().skip(top).take(bottom - top) {
        let Some(span) = row.get_mut(left * 4..right * 4) else {
            continue;
        };

        #[allow(
            clippy::cast_precision_loss,
            reason = "canvas coordinates are small integers, exact in f32"
        )]
        let (start_x, start_y) = (left as f32 + 0.5, y as f32 + 0.5);
        let mut source_x = to_fixed(
            inverse
                .a
                .mul_add(start_x, inverse.c.mul_add(start_y, inverse.e)),
        );
        let mut source_y = to_fixed(
            inverse
                .b
                .mul_add(start_x, inverse.d.mul_add(start_y, inverse.f)),
        );

        for target in span.chunks_exact_mut(4) {
            let inside = source_x >= limit_left
                && source_x < limit_right
                && source_y >= limit_top
                && source_y < limit_bottom;

            if inside {
                let sampled = premultiply(S::sample(view, source_x, source_y));
                let destination = [target[0], target[1], target[2], target[3]];
                target.copy_from_slice(&B::apply(sampled, destination));
            }

            source_x += step_x;
            source_y += step_y;
        }
    }
}
